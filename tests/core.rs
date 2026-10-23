use std::time::Duration;

use health_checks::{
    HealthCheck, HealthCheckContext, HealthCheckOptions, HealthCheckRegistration,
    HealthCheckResult, HealthChecks, HealthStatus, async_trait,
};
use serde_json::Value;

struct Failing;

#[async_trait]
impl HealthCheck for Failing {
    async fn check(
        &self,
        _: &HealthCheckContext,
    ) -> Result<HealthCheckResult, health_checks::BoxError> {
        let io = std::io::Error::new(std::io::ErrorKind::ConnectionRefused, "refused");
        Err(Box::new(Wrapped(io)))
    }
}

#[derive(Debug)]
struct Wrapped(std::io::Error);
impl std::fmt::Display for Wrapped {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("could not connect")
    }
}
impl std::error::Error for Wrapped {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.0)
    }
}

struct Panicking;

#[async_trait]
impl HealthCheck for Panicking {
    async fn check(
        &self,
        _: &HealthCheckContext,
    ) -> Result<HealthCheckResult, health_checks::BoxError> {
        panic!("boom");
    }
}

#[tokio::test]
async fn empty_report_is_healthy() {
    let report = HealthChecks::builder().build().check_health().await;
    assert_eq!(report.status, HealthStatus::Healthy);
    assert!(report.entries.is_empty());
}

#[tokio::test]
async fn worst_status_wins_and_order_is_preserved() {
    let report = HealthChecks::builder()
        .add_fn("z", HealthCheckResult::healthy)
        .add_fn("a", HealthCheckResult::degraded)
        .add_fn("m", HealthCheckResult::healthy)
        .build()
        .check_health()
        .await;
    assert_eq!(report.status, HealthStatus::Degraded);
    assert_eq!(report.entries.keys().collect::<Vec<_>>(), ["z", "a", "m"]);
}

#[tokio::test]
async fn error_maps_to_failure_status_with_chain() {
    let report = HealthChecks::builder()
        .add(HealthCheckRegistration::new("db", Failing).failure_status(HealthStatus::Degraded))
        .build()
        .check_health()
        .await;
    let e = &report.entries["db"];
    assert_eq!(e.status, HealthStatus::Degraded);
    assert_eq!(e.description.as_deref(), Some("could not connect"));
    assert_eq!(e.exception.as_deref(), Some("could not connect: refused"));
    assert_eq!(report.status, HealthStatus::Degraded);
}

#[tokio::test]
async fn panic_is_contained() {
    let report = HealthChecks::builder()
        .add_check("bad", Panicking)
        .add_fn("ok", HealthCheckResult::healthy)
        .build()
        .check_health()
        .await;
    assert_eq!(report.entries["bad"].status, HealthStatus::Unhealthy);
    assert_eq!(report.entries["bad"].exception.as_deref(), Some("boom"));
    assert_eq!(report.entries["ok"].status, HealthStatus::Healthy);
}

#[tokio::test]
async fn timeout_uses_failure_status_and_checks_run_concurrently() {
    let started = std::time::Instant::now();
    let report = HealthChecks::builder()
        .default_timeout(Duration::from_millis(150))
        .add_async_fn("slow1", || async {
            tokio::time::sleep(Duration::from_secs(10)).await;
            Ok(HealthCheckResult::healthy())
        })
        .add_async_fn("slow2", || async {
            tokio::time::sleep(Duration::from_secs(10)).await;
            Ok(HealthCheckResult::healthy())
        })
        .add(
            HealthCheckRegistration::new(
                "slow3",
                health_checks::AsyncFnCheck(|| async {
                    tokio::time::sleep(Duration::from_secs(10)).await;
                    Ok(HealthCheckResult::healthy())
                }),
            )
            .timeout(Duration::from_millis(50))
            .failure_status(HealthStatus::Degraded),
        )
        .build()
        .check_health()
        .await;
    assert!(
        started.elapsed() < Duration::from_millis(500),
        "checks must run concurrently"
    );
    assert_eq!(report.entries["slow1"].status, HealthStatus::Unhealthy);
    assert_eq!(report.entries["slow3"].status, HealthStatus::Degraded);
    assert_eq!(
        report.entries["slow1"].description.as_deref(),
        Some("A timeout occurred while running check.")
    );
}

#[test]
#[should_panic(expected = "duplicate")]
fn duplicate_names_panic() {
    let _ = HealthChecks::builder()
        .add_fn("x", HealthCheckResult::healthy)
        .add_fn("x", HealthCheckResult::healthy);
}

fn sample() -> health_checks::HealthCheckService {
    HealthChecks::builder()
        .add(
            HealthCheckRegistration::new("db", health_checks::FnCheck(HealthCheckResult::healthy))
                .tags(["ready", "db"]),
        )
        .add(
            HealthCheckRegistration::new(
                "cache",
                health_checks::FnCheck(|| {
                    HealthCheckResult::unhealthy()
                        .with_description("down")
                        .with_data("node", "redis-1")
                }),
            )
            .tags(["ready"]),
        )
        .add_fn("self", HealthCheckResult::healthy)
        .build()
}

#[tokio::test]
async fn plain_writer_and_default_status_codes() {
    let res = HealthCheckOptions::new().execute(&sample()).await;
    assert_eq!(res.body, "Unhealthy");
    assert_eq!(res.status_code, 503);
    assert!(
        res.headers
            .iter()
            .any(|(k, v)| *k == "content-type" && v.starts_with("text/plain"))
    );
    assert!(
        res.headers
            .iter()
            .any(|(k, v)| *k == "cache-control" && v == "no-store, no-cache")
    );

    let live = HealthCheckOptions::liveness().execute(&sample()).await;
    assert_eq!((live.body.as_str(), live.status_code), ("Healthy", 200));

    let db_only = HealthCheckOptions::new()
        .tags(["db"])
        .execute(&sample())
        .await;
    assert_eq!(
        (db_only.body.as_str(), db_only.status_code),
        ("Healthy", 200)
    );

    let custom = HealthCheckOptions::new()
        .status_code(HealthStatus::Unhealthy, 500)
        .allow_caching(true)
        .execute(&sample())
        .await;
    assert_eq!(custom.status_code, 500);
    assert!(!custom.headers.iter().any(|(k, _)| *k == "cache-control"));
}

#[tokio::test]
async fn json_writer_matches_ui_response_shape() {
    let res = HealthCheckOptions::json()
        .tags(["ready"])
        .execute(&sample())
        .await;
    let v: Value = serde_json::from_str(&res.body).unwrap();

    assert_eq!(v["status"], "Unhealthy");
    assert!(v["totalDuration"].as_str().unwrap().starts_with("00:00:00"));
    let entries = v["entries"].as_object().unwrap();
    assert_eq!(entries.keys().collect::<Vec<_>>(), ["db", "cache"]);

    let cache = &entries["cache"];
    assert_eq!(cache["status"], "Unhealthy");
    assert_eq!(cache["description"], "down");
    assert_eq!(cache["data"]["node"], "redis-1");
    assert_eq!(cache["tags"], serde_json::json!(["ready"]));
    assert!(cache.get("exception").is_none(), "null fields are omitted");
    assert!(cache["duration"].is_string());
}
