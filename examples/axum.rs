//! cargo run --example axum --features full
//!
//! Backends are registered only when their env var is set:
//!   SQLSERVER_CS="Server=tcp:localhost,1433;User Id=sa;Password=...;TrustServerCertificate=true"
//!   POSTGRES_URL="Host=localhost;Username=postgres;Password=...;SSL Mode=Disable"
//!   REDIS_URL="redis://localhost:6379"
//!   ELASTIC_URL="http://localhost:9200"
//!   KAFKA_BROKERS="localhost:9092"
//!
//! Then:
//!   curl -i localhost:3000/health          # plain: Healthy | Degraded | Unhealthy
//!   curl -s localhost:3000/health/json     # UIResponseWriter-shaped JSON
//!   curl -i localhost:3000/health/ready    # only checks tagged "ready"
//!   curl -i localhost:3000/health/live     # no checks, always 200 Healthy

use std::time::Duration;

use axum::Router;
use health_checks::axum::RouterExt;
use health_checks::{HealthCheckOptions, HealthCheckResult, HealthChecks};

#[tokio::main]
async fn main() -> Result<(), health_checks::BoxError> {
    #[allow(unused_mut)]
    let mut builder = HealthChecks::builder()
        .default_timeout(Duration::from_secs(5))
        .add_fn("self", HealthCheckResult::healthy)
        .add_async_fn("disk", || async {
            Ok(HealthCheckResult::healthy()
                .with_description("plenty of space")
                .with_data("free_gb", 412))
        });

    #[cfg(feature = "sqlserver")]
    if let Ok(cs) = std::env::var("SQLSERVER_CS") {
        use health_checks::checks::SqlServerCheck;
        builder = builder.add(
            health_checks::HealthCheckRegistration::new("sqlserver", SqlServerCheck::new(&cs)?)
                .tags(["ready", "db"]),
        );
    }
    #[cfg(feature = "postgres")]
    if let Ok(url) = std::env::var("POSTGRES_URL") {
        use health_checks::checks::PostgresCheck;
        builder = builder.add(
            health_checks::HealthCheckRegistration::new("postgres", PostgresCheck::new(&url)?)
                .tags(["ready", "db"]),
        );
    }
    #[cfg(feature = "redis")]
    if let Ok(url) = std::env::var("REDIS_URL") {
        use health_checks::checks::RedisCheck;
        builder = builder.add(
            health_checks::HealthCheckRegistration::new("redis", RedisCheck::new(url.as_str())?)
                .tags(["ready"])
                .failure_status(health_checks::HealthStatus::Degraded),
        );
    }
    #[cfg(feature = "elasticsearch")]
    if let Ok(url) = std::env::var("ELASTIC_URL") {
        use health_checks::checks::ElasticsearchCheck;
        builder = builder.add(
            health_checks::HealthCheckRegistration::new(
                "elasticsearch",
                ElasticsearchCheck::new(url)?,
            )
            .tags(["ready"]),
        );
    }
    #[cfg(feature = "kafka")]
    if let Ok(brokers) = std::env::var("KAFKA_BROKERS") {
        use health_checks::checks::KafkaCheck;
        builder = builder.add(
            health_checks::HealthCheckRegistration::new(
                "kafka",
                KafkaCheck::new(brokers.split(',').map(str::trim)),
            )
            .tags(["ready"]),
        );
    }

    let health = builder.build();

    let app = Router::new()
        .map_health_checks("/health", health.clone(), HealthCheckOptions::new())
        .map_health_checks("/health/json", health.clone(), HealthCheckOptions::json())
        .map_health_checks(
            "/health/ready",
            health.clone(),
            HealthCheckOptions::json().tags(["ready"]),
        )
        .map_health_checks("/health/live", health, HealthCheckOptions::liveness());

    let listener = tokio::net::TcpListener::bind("0.0.0.0:3000").await?;
    println!("listening on http://{}", listener.local_addr()?);
    axum::serve(listener, app).await?;
    Ok(())
}
