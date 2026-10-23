use std::collections::BTreeSet;
use std::fmt;
use std::sync::Arc;

use crate::{HealthCheckRegistration, HealthCheckService, HealthReport, HealthStatus};

type Predicate = Arc<dyn Fn(&HealthCheckRegistration) -> bool + Send + Sync>;
type WriterFn = Arc<dyn Fn(&HealthReport) -> (String, String) + Send + Sync>;

/// How a report becomes a response body.
#[derive(Clone, Default)]
pub enum ResponseWriter {
    /// `text/plain` body containing only the overall status (`Healthy`), like ASP.NET Core's
    /// default writer.
    #[default]
    Plain,
    /// `application/json` in the `UIResponseWriter` shape (status, totalDuration, entries).
    Json,
    /// Custom writer returning `(content_type, body)`.
    Custom(WriterFn),
}

impl ResponseWriter {
    pub fn custom<F>(f: F) -> Self
    where
        F: Fn(&HealthReport) -> (String, String) + Send + Sync + 'static,
    {
        Self::Custom(Arc::new(f))
    }

    pub fn write(&self, report: &HealthReport) -> (String, String) {
        match self {
            Self::Plain => (
                "text/plain; charset=utf-8".into(),
                report.status.to_string(),
            ),
            Self::Json => ("application/json; charset=utf-8".into(), report.to_json()),
            Self::Custom(f) => f(report),
        }
    }
}

impl fmt::Debug for ResponseWriter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Plain => "Plain",
            Self::Json => "Json",
            Self::Custom(_) => "Custom",
        })
    }
}

/// HTTP status per health status. Defaults match ASP.NET Core: 200 / 200 / 503.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResultStatusCodes {
    pub healthy: u16,
    pub degraded: u16,
    pub unhealthy: u16,
}

impl Default for ResultStatusCodes {
    fn default() -> Self {
        Self {
            healthy: 200,
            degraded: 200,
            unhealthy: 503,
        }
    }
}

impl ResultStatusCodes {
    pub fn get(&self, status: HealthStatus) -> u16 {
        match status {
            HealthStatus::Healthy => self.healthy,
            HealthStatus::Degraded => self.degraded,
            HealthStatus::Unhealthy => self.unhealthy,
        }
    }
}

/// Endpoint options. Equivalent of `HealthCheckOptions`.
#[derive(Clone, Default)]
pub struct HealthCheckOptions {
    pub predicate: Option<Predicate>,
    pub writer: ResponseWriter,
    pub result_status_codes: ResultStatusCodes,
    /// When `false` (default) the response carries no-cache headers, as in .NET.
    pub allow_caching: bool,
}

impl HealthCheckOptions {
    /// Plain-text writer, all checks.
    pub fn new() -> Self {
        Self::default()
    }

    pub fn json() -> Self {
        Self::new().writer(ResponseWriter::Json)
    }

    pub fn writer(mut self, writer: ResponseWriter) -> Self {
        self.writer = writer;
        self
    }

    pub fn predicate<F>(mut self, predicate: F) -> Self
    where
        F: Fn(&HealthCheckRegistration) -> bool + Send + Sync + 'static,
    {
        self.predicate = Some(Arc::new(predicate));
        self
    }

    /// Only run checks carrying at least one of `tags`.
    pub fn tags<I, S>(self, tags: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let wanted: BTreeSet<String> = tags.into_iter().map(Into::into).collect();
        self.predicate(move |r| r.tags.iter().any(|t| wanted.contains(t)))
    }

    /// Run no checks at all: the classic liveness probe (`Predicate = _ => false`).
    pub fn liveness() -> Self {
        Self::new().predicate(|_| false)
    }

    pub fn status_code(mut self, status: HealthStatus, code: u16) -> Self {
        match status {
            HealthStatus::Healthy => self.result_status_codes.healthy = code,
            HealthStatus::Degraded => self.result_status_codes.degraded = code,
            HealthStatus::Unhealthy => self.result_status_codes.unhealthy = code,
        }
        self
    }

    pub fn allow_caching(mut self, allow: bool) -> Self {
        self.allow_caching = allow;
        self
    }

    /// Run the checks and render a framework-agnostic response.
    pub async fn execute(&self, service: &HealthCheckService) -> HealthResponse {
        let report = match &self.predicate {
            Some(p) => service.check_health_filtered(|r| p(r)).await,
            None => service.check_health().await,
        };
        self.render(&report)
    }

    pub fn render(&self, report: &HealthReport) -> HealthResponse {
        let (content_type, body) = self.writer.write(report);
        let mut headers = vec![("content-type", content_type)];
        if !self.allow_caching {
            headers.extend([
                ("cache-control", "no-store, no-cache".to_owned()),
                ("pragma", "no-cache".to_owned()),
                ("expires", "Thu, 01 Jan 1970 00:00:00 GMT".to_owned()),
            ]);
        }
        HealthResponse {
            status: report.status,
            status_code: self.result_status_codes.get(report.status),
            headers,
            body,
        }
    }
}

impl fmt::Debug for HealthCheckOptions {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HealthCheckOptions")
            .field("predicate", &self.predicate.as_ref().map(|_| "<fn>"))
            .field("writer", &self.writer)
            .field("result_status_codes", &self.result_status_codes)
            .field("allow_caching", &self.allow_caching)
            .finish()
    }
}

/// Rendered response, usable from any HTTP framework.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HealthResponse {
    pub status: HealthStatus,
    pub status_code: u16,
    pub headers: Vec<(&'static str, String)>,
    pub body: String,
}
