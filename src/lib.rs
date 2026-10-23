//! Health checks modelled on ASP.NET Core's `Microsoft.Extensions.Diagnostics.HealthChecks`.
//!
//! | ASP.NET Core                         | here                                  |
//! |--------------------------------------|---------------------------------------|
//! | `IHealthCheck`                       | [`HealthCheck`]                       |
//! | `HealthCheckResult`                  | [`HealthCheckResult`]                 |
//! | `HealthStatus`                       | [`HealthStatus`]                      |
//! | `HealthCheckRegistration`            | [`HealthCheckRegistration`]           |
//! | `services.AddHealthChecks()`         | [`HealthChecks::builder()`]           |
//! | `HealthCheckService.CheckHealthAsync`| [`HealthCheckService::check_health`]  |
//! | `HealthReport` / `HealthReportEntry` | [`HealthReport`] / [`HealthReportEntry`] |
//! | `HealthCheckOptions`                 | [`HealthCheckOptions`]                |
//! | `app.MapHealthChecks(...)`           | `axum::RouterExt::map_health_checks` (feature `axum`) |
//!
//! Output is either the bare status as `text/plain` (`Healthy`) or JSON in the
//! Xabaril `UIResponseWriter` shape.
//!
//! Ready-made checks live in [`checks`], each behind its own feature:
//! `sqlserver`, `postgres` (+ `postgres-tls`), `redis`, `elasticsearch` (+ `elasticsearch-tls`), `kafka`.
//!
//! Requires a Tokio runtime (timeouts use `tokio::time`).

mod check;
mod options;
mod report;
mod result;
mod service;
mod status;

#[cfg(feature = "axum")]
pub mod axum;
pub mod checks;
#[cfg(feature = "tls")]
pub mod tls;

pub use async_trait::async_trait;
pub use check::{AsyncFnCheck, FnCheck, HealthCheck, HealthCheckContext, HealthCheckRegistration};
pub use options::{HealthCheckOptions, HealthResponse, ResponseWriter, ResultStatusCodes};
pub use report::{HealthReport, HealthReportEntry, format_timespan};
pub use result::{BoxError, HealthCheckResult};
pub use service::{HealthCheckService, HealthChecks, HealthChecksBuilder};
pub use status::HealthStatus;
