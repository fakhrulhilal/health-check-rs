//! Scenario 2: a provided backend (PostgreSQL) without TLS, with TLS against a
//! self-signed server, and with TLS verified against that self-signed CA.
//!
//!   cargo run --example postgres --features postgres-tls
//!
//! Override any connection string through the environment:
//!   PG_LOCAL     default: Host=localhost;Username=postgres;Password=postgres;SSL Mode=Disable
//!   PG_TLS       default: Host=localhost;Username=postgres;Password=postgres;SSL Mode=Require
//!   PG_VERIFIED  default: Host=localhost;Username=postgres;Password=postgres;SSL Mode=VerifyFull;Root Certificate=ca.pem
//!
//! `Require` encrypts but accepts any certificate (Npgsql/libpq behaviour), so it works
//! with self-signed servers out of the box. `VerifyFull` + `Root Certificate` also proves
//! the server is the one your CA signed, with the hostname on its certificate.

use std::time::Duration;

use health_checks::checks::PostgresCheck;
use health_checks::{BoxError, HealthCheckRegistration, HealthChecks};

fn conn(var: &str, default: &str) -> String {
    std::env::var(var).unwrap_or_else(|_| default.to_owned())
}

#[tokio::main]
async fn main() -> Result<(), BoxError> {
    let local = conn(
        "PG_LOCAL",
        "Host=localhost;Username=postgres;Password=postgres;SSL Mode=Disable",
    );
    let tls = conn(
        "PG_TLS",
        "Host=localhost;Username=postgres;Password=postgres;SSL Mode=Require",
    );
    let verified = conn(
        "PG_VERIFIED",
        "Host=localhost;Username=postgres;Password=postgres;SSL Mode=VerifyFull;Root Certificate=ca.pem",
    );

    let health = HealthChecks::builder()
        .default_timeout(Duration::from_secs(5))
        .add(HealthCheckRegistration::new("pg-plain", PostgresCheck::new(&local)?).tags(["db"]))
        .add(HealthCheckRegistration::new("pg-tls", PostgresCheck::new(&tls)?).tags(["db"]))
        // Construction fails if the CA file is missing, so report that instead of panicking.
        .add_check_or_report("pg-tls-verified", PostgresCheck::new(&verified))
        .build();

    let report = health.check_health().await;
    println!("{}", report.to_json_pretty());
    for (name, entry) in &report.entries {
        println!(
            "{name:16} {:9} {}",
            entry.status.to_string(),
            entry.description.as_deref().unwrap_or("")
        );
    }
    Ok(())
}

/// Register a check whose construction may fail; a failure becomes an Unhealthy entry.
trait AddOrReport {
    fn add_check_or_report(self, name: &str, check: Result<PostgresCheck, BoxError>) -> Self;
}

impl AddOrReport for health_checks::HealthChecksBuilder {
    fn add_check_or_report(self, name: &str, check: Result<PostgresCheck, BoxError>) -> Self {
        match check {
            Ok(check) => self.add(HealthCheckRegistration::new(name, check).tags(["db"])),
            Err(e) => {
                let message = e.to_string();
                self.add_fn(name, move || {
                    health_checks::HealthCheckResult::unhealthy()
                        .with_description(format!("not configured: {message}"))
                })
            }
        }
    }
}
