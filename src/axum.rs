//! axum integration (feature `axum`).
//!
//! ```no_run
//! use axum::Router;
//! use health_checks::{HealthChecks, HealthCheckOptions, HealthCheckResult};
//! use health_checks::axum::RouterExt;
//!
//! let health = HealthChecks::builder().add_fn("self", HealthCheckResult::healthy).build();
//! let app: Router = Router::new()
//!     .map_health_checks("/health", health.clone(), HealthCheckOptions::json())
//!     .map_health_checks("/health/live", health, HealthCheckOptions::liveness());
//! ```

use ::axum::Router;
use ::axum::http::{HeaderName, HeaderValue, StatusCode};
use ::axum::response::{IntoResponse, Response};
use ::axum::routing::{MethodRouter, get};

use crate::{HealthCheckOptions, HealthCheckService, HealthResponse};

impl IntoResponse for HealthResponse {
    fn into_response(self) -> Response {
        let mut response = self.body.into_response();
        *response.status_mut() =
            StatusCode::from_u16(self.status_code).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
        let headers = response.headers_mut();
        for (name, value) in self.headers {
            if let Ok(value) = HeaderValue::from_str(&value) {
                headers.insert(HeaderName::from_static(name), value);
            }
        }
        response
    }
}

/// GET (and HEAD) handler running the checks selected by `options`.
pub fn endpoint<S>(service: HealthCheckService, options: HealthCheckOptions) -> MethodRouter<S>
where
    S: Clone + Send + Sync + 'static,
{
    get(move || async move { options.execute(&service).await })
}

/// `app.MapHealthChecks(path, options)` for axum routers.
pub trait RouterExt {
    fn map_health_checks(
        self,
        path: &str,
        service: HealthCheckService,
        options: HealthCheckOptions,
    ) -> Self;
}

impl<S> RouterExt for Router<S>
where
    S: Clone + Send + Sync + 'static,
{
    fn map_health_checks(
        self,
        path: &str,
        service: HealthCheckService,
        options: HealthCheckOptions,
    ) -> Self {
        self.route(path, endpoint(service, options))
    }
}
