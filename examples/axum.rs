//! Scenario 3: axum with Redis, Elasticsearch and a "self" check.
//!
//!   cargo run --example axum --features axum,redis,elasticsearch
//!
//!   REDIS_URL    default: redis://localhost:6379
//!   ELASTIC_URL  default: http://localhost:9200
//!
//!   curl -i localhost:3000/health         # plain text: Healthy | Degraded | Unhealthy
//!   curl -s localhost:3000/health/json    # every check, full JSON
//!   curl -i localhost:3000/health/ready   # only dependencies (everything but "self")

use std::time::Duration;

use axum::Router;
use health_checks::axum::RouterExt;
use health_checks::checks::{ElasticsearchCheck, RedisCheck};
use health_checks::{
    BoxError, HealthCheckOptions, HealthCheckRegistration, HealthCheckResult, HealthChecks,
};

#[tokio::main]
async fn main() -> Result<(), BoxError> {
    let redis_url = std::env::var("REDIS_URL").unwrap_or_else(|_| "redis://localhost:6379".into());
    let elastic_url =
        std::env::var("ELASTIC_URL").unwrap_or_else(|_| "http://localhost:9200".into());

    let health = HealthChecks::builder()
        .default_timeout(Duration::from_secs(3))
        // The process is up and serving requests.
        .add_fn("self", HealthCheckResult::healthy)
        .add(
            HealthCheckRegistration::new("redis", RedisCheck::new(redis_url.as_str())?)
                .tags(["ready"]),
        )
        .add(
            HealthCheckRegistration::new("elasticsearch", ElasticsearchCheck::new(elastic_url)?)
                .tags(["ready"]),
        )
        .build();

    let app = Router::new()
        .map_health_checks("/health", health.clone(), HealthCheckOptions::new())
        .map_health_checks("/health/json", health.clone(), HealthCheckOptions::json())
        .map_health_checks(
            "/health/ready",
            health,
            HealthCheckOptions::json().predicate(|r| r.name != "self"),
        );

    let listener = tokio::net::TcpListener::bind("0.0.0.0:3000").await?;
    println!("listening on http://{}", listener.local_addr()?);
    axum::serve(listener, app).await?;
    Ok(())
}
