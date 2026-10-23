use std::collections::BTreeSet;
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;

use crate::{BoxError, HealthCheckResult, HealthStatus};

/// A health check. Equivalent of `IHealthCheck`.
///
/// Return `Err` (or panic) and the runner maps it to the registration's
/// [`failure_status`](HealthCheckRegistration::failure_status), like a thrown exception in .NET.
/// To report a failure yourself, use `ctx.failure_status()` rather than hard-coding
/// `Unhealthy`, so registrations configured as `Degraded` behave as expected.
#[async_trait]
pub trait HealthCheck: Send + Sync + 'static {
    async fn check(&self, ctx: &HealthCheckContext) -> Result<HealthCheckResult, BoxError>;
}

/// Passed to every check. Equivalent of `HealthCheckContext`.
#[derive(Clone)]
pub struct HealthCheckContext {
    registration: Arc<HealthCheckRegistration>,
}

impl HealthCheckContext {
    pub(crate) fn new(registration: Arc<HealthCheckRegistration>) -> Self {
        Self { registration }
    }

    pub fn registration(&self) -> &HealthCheckRegistration {
        &self.registration
    }

    pub fn name(&self) -> &str {
        &self.registration.name
    }

    pub fn failure_status(&self) -> HealthStatus {
        self.registration.failure_status
    }
}

/// A named check plus its options. Equivalent of `HealthCheckRegistration`.
#[derive(Clone)]
pub struct HealthCheckRegistration {
    pub name: String,
    pub check: Arc<dyn HealthCheck>,
    /// Status reported when the check errors, panics or times out. Default `Unhealthy`.
    pub failure_status: HealthStatus,
    pub tags: BTreeSet<String>,
    /// `None` = no per-check timeout (falls back to the builder's default, if any).
    pub timeout: Option<Duration>,
}

impl HealthCheckRegistration {
    pub fn new(name: impl Into<String>, check: impl HealthCheck) -> Self {
        Self::from_arc(name, Arc::new(check))
    }

    pub fn from_arc(name: impl Into<String>, check: Arc<dyn HealthCheck>) -> Self {
        Self {
            name: name.into(),
            check,
            failure_status: HealthStatus::Unhealthy,
            tags: BTreeSet::new(),
            timeout: None,
        }
    }

    pub fn failure_status(mut self, status: HealthStatus) -> Self {
        self.failure_status = status;
        self
    }

    pub fn tags<I, S>(mut self, tags: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.tags.extend(tags.into_iter().map(Into::into));
        self
    }

    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }

    pub fn has_tag(&self, tag: &str) -> bool {
        self.tags.contains(tag)
    }
}

impl std::fmt::Debug for HealthCheckRegistration {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HealthCheckRegistration")
            .field("name", &self.name)
            .field("failure_status", &self.failure_status)
            .field("tags", &self.tags)
            .field("timeout", &self.timeout)
            .finish_non_exhaustive()
    }
}

/// Adapter for a synchronous closure, e.g. `|| HealthCheckResult::healthy()`.
pub struct FnCheck<F>(pub F);

#[async_trait]
impl<F, R> HealthCheck for FnCheck<F>
where
    F: Fn() -> R + Send + Sync + 'static,
    R: Into<HealthCheckResult>,
{
    async fn check(&self, _ctx: &HealthCheckContext) -> Result<HealthCheckResult, BoxError> {
        Ok((self.0)().into())
    }
}

/// Adapter for an async closure returning `Result<HealthCheckResult, BoxError>`.
pub struct AsyncFnCheck<F>(pub F);

#[async_trait]
impl<F, Fut> HealthCheck for AsyncFnCheck<F>
where
    F: Fn() -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<HealthCheckResult, BoxError>> + Send,
{
    async fn check(&self, _ctx: &HealthCheckContext) -> Result<HealthCheckResult, BoxError> {
        (self.0)().await
    }
}

#[async_trait]
impl<T: HealthCheck + ?Sized> HealthCheck for Arc<T> {
    async fn check(&self, ctx: &HealthCheckContext) -> Result<HealthCheckResult, BoxError> {
        (**self).check(ctx).await
    }
}
