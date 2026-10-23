use std::any::Any;
use std::future::Future;
use std::panic::AssertUnwindSafe;
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures_util::FutureExt;
use futures_util::future::join_all;
use indexmap::IndexMap;

use crate::check::{AsyncFnCheck, FnCheck, HealthCheckContext};
use crate::result::error_chain;
use crate::{
    BoxError, HealthCheck, HealthCheckRegistration, HealthCheckResult, HealthReport,
    HealthReportEntry,
};

/// Registers checks. Equivalent of `services.AddHealthChecks()` / `IHealthChecksBuilder`.
///
/// ```
/// use std::time::Duration;
/// use health_checks::{HealthChecks, HealthCheckRegistration, HealthCheckResult, HealthStatus};
///
/// let service = HealthChecks::builder()
///     .default_timeout(Duration::from_secs(5))
///     .add_fn("self", HealthCheckResult::healthy)
///     .add(
///         HealthCheckRegistration::new("cache", health_checks::FnCheck(HealthCheckResult::degraded))
///             .tags(["ready"])
///             .failure_status(HealthStatus::Degraded),
///     )
///     .build();
/// ```
#[derive(Default)]
pub struct HealthChecksBuilder {
    registrations: Vec<HealthCheckRegistration>,
    default_timeout: Option<Duration>,
}

impl HealthChecksBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a fully configured registration (tags, failure status, timeout).
    ///
    /// # Panics
    /// If a registration with the same name already exists (ASP.NET Core throws too).
    #[allow(clippy::should_implement_trait)] // mirrors IHealthChecksBuilder.Add
    pub fn add(mut self, registration: HealthCheckRegistration) -> Self {
        assert!(
            !self
                .registrations
                .iter()
                .any(|r| r.name == registration.name),
            "duplicate health check registration name: {:?}",
            registration.name
        );
        self.registrations.push(registration);
        self
    }

    /// `AddCheck(name, check)` with default options.
    pub fn add_check(self, name: impl Into<String>, check: impl HealthCheck) -> Self {
        self.add(HealthCheckRegistration::new(name, check))
    }

    /// `AddCheck(name, () => HealthCheckResult.Healthy())`.
    pub fn add_fn<F, R>(self, name: impl Into<String>, f: F) -> Self
    where
        F: Fn() -> R + Send + Sync + 'static,
        R: Into<HealthCheckResult> + 'static,
    {
        self.add_check(name, FnCheck(f))
    }

    /// `AddAsyncCheck(name, async () => ...)`.
    pub fn add_async_fn<F, Fut>(self, name: impl Into<String>, f: F) -> Self
    where
        F: Fn() -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<HealthCheckResult, BoxError>> + Send + 'static,
    {
        self.add_check(name, AsyncFnCheck(f))
    }

    /// Timeout applied to registrations that don't set their own. .NET has no default
    /// (infinite); setting one is strongly recommended so a hung dependency can't hang
    /// the endpoint.
    pub fn default_timeout(mut self, timeout: Duration) -> Self {
        self.default_timeout = Some(timeout);
        self
    }

    pub fn build(self) -> HealthCheckService {
        let registrations = self
            .registrations
            .into_iter()
            .map(|mut r| {
                r.timeout = r.timeout.or(self.default_timeout);
                Arc::new(r)
            })
            .collect();
        HealthCheckService {
            registrations: Arc::new(registrations),
        }
    }
}

/// Entry point, mirroring `services.AddHealthChecks()`.
pub struct HealthChecks;

impl HealthChecks {
    pub fn builder() -> HealthChecksBuilder {
        HealthChecksBuilder::new()
    }
}

/// Runs registered checks. Equivalent of `HealthCheckService`. Cheap to clone.
#[derive(Clone)]
pub struct HealthCheckService {
    registrations: Arc<Vec<Arc<HealthCheckRegistration>>>,
}

impl HealthCheckService {
    pub fn registrations(&self) -> impl Iterator<Item = &HealthCheckRegistration> {
        self.registrations.iter().map(|r| r.as_ref())
    }

    /// Run every check concurrently.
    pub async fn check_health(&self) -> HealthReport {
        self.check_health_filtered(|_| true).await
    }

    /// Run the checks matching `predicate` concurrently.
    /// `|_| false` gives a pure liveness probe: no checks, always `Healthy`.
    pub async fn check_health_filtered<P>(&self, predicate: P) -> HealthReport
    where
        P: Fn(&HealthCheckRegistration) -> bool,
    {
        let started = Instant::now();
        let runs = self
            .registrations
            .iter()
            .filter(|r| predicate(r))
            .map(|r| run_one(Arc::clone(r)));
        let entries: IndexMap<_, _> = join_all(runs).await.into_iter().collect();
        HealthReport::new(entries, started.elapsed())
    }
}

async fn run_one(registration: Arc<HealthCheckRegistration>) -> (String, HealthReportEntry) {
    let ctx = HealthCheckContext::new(Arc::clone(&registration));
    let failure = registration.failure_status;
    let started = Instant::now();

    let guarded = AssertUnwindSafe(registration.check.check(&ctx)).catch_unwind();
    let outcome = match registration.timeout {
        Some(t) => tokio::time::timeout(t, guarded).await.ok(),
        None => Some(guarded.await),
    };

    let result = match outcome {
        Some(Ok(Ok(result))) => result,
        Some(Ok(Err(error))) => HealthCheckResult::new(failure)
            .with_description(error.to_string())
            .with_exception(error_chain(error.as_ref())),
        Some(Err(panic)) => HealthCheckResult::new(failure)
            .with_description("The health check panicked.")
            .with_exception(panic_message(&panic)),
        None => HealthCheckResult::new(failure)
            .with_description("A timeout occurred while running check."),
    };

    let entry = HealthReportEntry::new(result, started.elapsed(), registration.tags.clone());
    (registration.name.clone(), entry)
}

fn panic_message(payload: &Box<dyn Any + Send>) -> String {
    payload
        .downcast_ref::<&str>()
        .map(|s| (*s).to_owned())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "panic with non-string payload".to_owned())
}
