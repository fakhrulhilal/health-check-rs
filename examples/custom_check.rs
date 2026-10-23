//! Scenario 1: your own backend. A check that probes an HTTP endpoint, run from a
//! console program (no web server).
//!
//!   cargo run --example custom_check -- http://localhost:8080/ping http://localhost:9999/
//!
//! Prints the JSON report and exits 0 when Healthy/Degraded, 1 when Unhealthy,
//! so it also works as a Docker HEALTHCHECK or cron probe.

use std::time::{Duration, Instant};

use health_checks::{
    BoxError, HealthCheck, HealthCheckContext, HealthCheckRegistration, HealthCheckResult,
    HealthChecks, HealthStatus, async_trait,
};
use http_body_util::Empty;
use hyper::body::Bytes;
use hyper_util::client::legacy::Client;
use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::rt::TokioExecutor;

/// Healthy on 2xx within `slow_after`, Degraded on 2xx but slow,
/// failure status on non-2xx. Connection errors are returned as `Err`.
struct HttpProbe {
    client: Client<HttpConnector, Empty<Bytes>>,
    url: hyper::Uri,
    slow_after: Duration,
}

impl HttpProbe {
    fn new(url: &str, slow_after: Duration) -> Result<Self, BoxError> {
        Ok(Self {
            client: Client::builder(TokioExecutor::new()).build_http(),
            url: url.parse()?,
            slow_after,
        })
    }
}

#[async_trait]
impl HealthCheck for HttpProbe {
    async fn check(&self, ctx: &HealthCheckContext) -> Result<HealthCheckResult, BoxError> {
        let started = Instant::now();
        let response = self.client.get(self.url.clone()).await?; // Err → failure status
        let elapsed = started.elapsed();
        let code = response.status();

        let result = if !code.is_success() {
            HealthCheckResult::new(ctx.failure_status())
                .with_description(format!("{} returned {code}", self.url))
        } else if elapsed > self.slow_after {
            HealthCheckResult::degraded().with_description(format!("slow response: {elapsed:?}"))
        } else {
            HealthCheckResult::healthy()
        };
        Ok(result
            .with_data("url", self.url.to_string())
            .with_data("status_code", code.as_u16())
            .with_data("latency_ms", elapsed.as_millis() as u64))
    }
}

#[tokio::main]
async fn main() -> Result<(), BoxError> {
    let urls: Vec<String> = std::env::args().skip(1).collect();
    if urls.is_empty() {
        eprintln!("usage: custom_check <url> [url...]");
        std::process::exit(2);
    }

    let mut builder = HealthChecks::builder().default_timeout(Duration::from_secs(5));
    for (i, url) in urls.iter().enumerate() {
        builder = builder.add(
            HealthCheckRegistration::new(
                format!("http-{i}"),
                HttpProbe::new(url, Duration::from_millis(500))?,
            )
            .tags(["http"]),
        );
    }

    let report = builder.build().check_health().await;
    println!("{}", report.to_json_pretty());
    std::process::exit(if report.status == HealthStatus::Unhealthy {
        1
    } else {
        0
    });
}
