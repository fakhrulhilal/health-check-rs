# health-checks

[![CI](https://github.com/fakhrulhilal/health-check-rs/actions/workflows/ci.yml/badge.svg)](https://github.com/fakhrulhilal/health-check-rs/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/health-checks.svg)](https://crates.io/crates/health-checks)
[![downloads](https://img.shields.io/crates/d/health-checks.svg)](https://crates.io/crates/health-checks)
[![docs.rs](https://img.shields.io/docsrs/health-checks)](https://docs.rs/health-checks)
[![MSRV](https://img.shields.io/badge/MSRV-1.88-blue)](Cargo.toml)
[![license](https://img.shields.io/crates/l/health-checks.svg)](LICENSE)

ASP.NET Core-style health checks for Rust: `IHealthCheck`, `Healthy/Degraded/Unhealthy`,
tags, failure status, timeouts, and a `/health` endpoint that answers in plain text or JSON.

## Install

```toml
[dependencies]
health-checks = { version = "0.1", features = ["axum"] }
# or: { git = "https://github.com/fakhrulhilal/health-check-rs", features = ["axum"] }
```

Add backend features as needed (see [Backends](#backends)).

## Usage

```rust
use std::time::Duration;
use axum::Router;
use health_checks::axum::RouterExt;
use health_checks::{HealthCheckOptions, HealthCheckRegistration, HealthCheckResult, HealthChecks, HealthStatus};

let health = HealthChecks::builder()
    .default_timeout(Duration::from_secs(5))                 // per check
    .add_fn("self", HealthCheckResult::healthy)
    .add(
        HealthCheckRegistration::new("disk", DiskSpace { min_gb: 10 })
            .tags(["ready"])
            .failure_status(HealthStatus::Degraded),         // used on Err / panic / timeout
    )
    .build();

let app = Router::new()
    .map_health_checks("/health", health.clone(), HealthCheckOptions::new())        // text/plain: Healthy
    .map_health_checks("/health/ready", health.clone(), HealthCheckOptions::json().tags(["ready"]))
    .map_health_checks("/health/live", health, HealthCheckOptions::liveness());     // runs no checks
```

- HTTP status: Healthy 200, Degraded 200, Unhealthy 503 (change with `.status_code(..)`).
- Overall status is the worst check. Checks run concurrently.
- JSON matches HealthChecks.UI (`UIResponseWriter`):
  `{"status":"Healthy","totalDuration":"00:00:00.0051705","entries":{"db":{"data":{},"duration":"...","status":"Healthy","tags":["ready"]}}}`
- Not on axum: `HealthCheckOptions::json().execute(&health).await` returns status code, headers and body.

## Custom check

```rust
use health_checks::{async_trait, BoxError, HealthCheck, HealthCheckContext, HealthCheckResult};

struct DiskSpace { min_gb: u64 }

#[async_trait]
impl HealthCheck for DiskSpace {
    async fn check(&self, ctx: &HealthCheckContext) -> Result<HealthCheckResult, BoxError> {
        let free = free_gb()?;               // Err → failure status, message in "description"
        let result = if free >= self.min_gb {
            HealthCheckResult::healthy()
        } else {
            HealthCheckResult::new(ctx.failure_status()).with_description("low disk space")
        };
        Ok(result.with_data("free_gb", free))
    }
}
```

One-liners without a type: `.add_fn(name, || ...)` or `.add_async_fn(name, || async { ... })`.

## Backends

Each backend is a feature. Register it like any other check:
`.add(HealthCheckRegistration::new("name", <check>).tags(["ready"]))`.

TLS is chosen at runtime from the connection string or URL, so one build talks plain to
local dev and TLS to production. Every backend can connect to servers with self-signed
certificates; see each section.

### SQL Server — `sqlserver`

```rust
use health_checks::checks::SqlServerCheck;
SqlServerCheck::new("Server=tcp:db,1433;Database=app;User Id=sa;Password=...;TrustServerCertificate=true")?
```

ADO.NET connection string. New connection + `SELECT 1` per check.
Self-signed: `TrustServerCertificate=true`, as in .NET.

### PostgreSQL — `postgres` (TLS: `postgres-tls`)

```rust
use health_checks::checks::PostgresCheck;
PostgresCheck::new("Host=db;Port=5432;Database=app;Username=u;Password=p;SSL Mode=Require")?
// also: "postgres://u:p@db/app?sslmode=require" or "host=db user=u sslmode=disable"
```

New connection + `SELECT 1` per check. SSL modes behave like Npgsql/libpq:

| `SSL Mode`   | TLS                 | certificate check |
|--------------|---------------------|-------------------|
| `Disable`    | no                  | –                 |
| `Prefer`     | if server offers it | none              |
| `Require`    | yes                 | none (works with self-signed) |
| `VerifyCA`   | yes                 | chain             |
| `VerifyFull` | yes                 | chain + hostname  |

To verify a self-signed server add `Root Certificate=/path/ca.pem` (`sslrootcert=` in URL form);
otherwise the OS trust store is used. Anything other than `Disable`/`Prefer` needs `postgres-tls`.

### Redis — `redis`

```rust
use health_checks::checks::RedisCheck;
RedisCheck::new("redis://:password@cache:6379")?
// or reuse yours: RedisCheck::from_connection_manager(manager)
```

`PING` → `PONG`. For `rediss://`, enable `tokio-rustls-comp` on your own `redis` dependency.
Self-signed: also enable `tls-rustls-insecure` and use `rediss://:password@cache:6380/#insecure`.

### Elasticsearch — `elasticsearch` (TLS: `elasticsearch-tls`)

```rust
use health_checks::checks::ElasticsearchCheck;
ElasticsearchCheck::new("https://es:9200")?.basic_auth("elastic", "...")
// or .api_key("base64-id:key"), .ping_only()
```

`GET /_cluster/health`: green → Healthy, yellow → Degraded, red → failure status.
`https://` needs `elasticsearch-tls`. Certificates are checked against the OS trust store; for
self-signed clusters add one of:

```rust
.ca_fingerprint("AB:CD:...")?               // SHA-256 Elasticsearch prints at setup
.ca_certificate_pem(&std::fs::read("http_ca.crt")?)?
.danger_accept_invalid_certs()?             // encryption only
```

### Kafka — `kafka`

```rust
use health_checks::checks::KafkaCheck;
KafkaCheck::new(["broker1:9092", "broker2:9092"]).require_topics(["orders"])
```

Fetches cluster metadata (no message produced). For TLS/SASL, enable `rskafka/transport-tls`
in your manifest and pass a config, e.g. for self-signed (needs the `tls` feature):

```rust
let tls = std::sync::Arc::new(health_checks::tls::insecure_config()?);
KafkaCheck::new(["broker:9093"]).configure(move |b| b.tls_config(tls.clone()))
```

### TLS helpers — `tls`

`health_checks::tls` builds rustls configs (ring, no aws-lc-rs) usable with any check's
`with_tls_config(..)`: `native_roots_config()`, `config_with_ca_pem(pem)`, `ca_only_config(roots)`,
`pinned_config([sha256])`, `insecure_config()`. Enabled by `postgres-tls` and `elasticsearch-tls`.

## Examples

| file | scenario | run |
|------|----------|-----|
| [`custom_check.rs`](examples/custom_check.rs) | own backend: HTTP endpoint probe, console program, exit code 1 when unhealthy | `cargo run --example custom_check -- http://localhost:8080/ping` |
| [`postgres.rs`](examples/postgres.rs) | provided backend: Postgres without TLS, TLS to a self-signed server, TLS verified against its CA | `cargo run --example postgres --features postgres-tls` |
| [`axum.rs`](examples/axum.rs) | axum with Redis, Elasticsearch and `self`: `/health` plain, `/health/json` full, `/health/ready` everything but `self` | `cargo run --example axum --features axum,redis,elasticsearch` |

Connection strings and URLs are read from environment variables listed at the top of each file.

## License

MIT
