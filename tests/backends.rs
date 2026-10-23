//! Offline backend tests: a mock Elasticsearch, and unreachable endpoints for the rest
//! (asserting failures are reported, not hung or panicked).
//!
//! Live tests run when the env vars from `examples/axum.rs` are set:
//!   cargo test --all-features --test backends -- --nocapture

#![allow(unused_imports, dead_code)]

use std::time::Duration;

use health_checks::{
    HealthCheck, HealthCheckRegistration, HealthChecks, HealthReport, HealthStatus,
};

/// Port 1 on loopback: connection refused immediately.
const DEAD: &str = "127.0.0.1:1";

async fn run(name: &str, check: impl HealthCheck) -> HealthReport {
    HealthChecks::builder()
        .default_timeout(Duration::from_secs(5))
        .add(HealthCheckRegistration::new(name, check))
        .build()
        .check_health()
        .await
}

fn assert_failed(report: &HealthReport, name: &str) {
    let e = &report.entries[name];
    assert_eq!(e.status, HealthStatus::Unhealthy, "{name}: {e:?}");
    assert_ne!(
        e.description.as_deref(),
        Some("A timeout occurred while running check."),
        "{name} should fail fast, not time out"
    );
    assert!(e.exception.is_some(), "{name}: {e:?}");
    println!("{name}: {:?}", e.exception);
}

fn env(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|v| !v.is_empty())
}

#[cfg(feature = "elasticsearch")]
mod elasticsearch {
    use super::*;
    use axum::Router;
    use axum::routing::get;
    use health_checks::checks::ElasticsearchCheck;

    async fn mock(colour: &'static str) -> String {
        let app = Router::new().route(
            "/_cluster/health",
            get(move || async move {
                format!(
                    r#"{{"cluster_name":"docker-cluster","status":"{colour}","timed_out":false,
                        "number_of_nodes":1,"number_of_data_nodes":1,"active_primary_shards":3,
                        "active_shards":3,"unassigned_shards":1}}"#
                )
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        format!("http://{addr}/")
    }

    #[tokio::test]
    async fn maps_cluster_colours() {
        for (colour, expected) in [
            ("green", HealthStatus::Healthy),
            ("yellow", HealthStatus::Degraded),
            ("red", HealthStatus::Unhealthy),
        ] {
            let url = mock(colour).await;
            let report = run("es", ElasticsearchCheck::new(url).unwrap()).await;
            let e = &report.entries["es"];
            assert_eq!(e.status, expected, "{colour}");
            assert_eq!(e.data["cluster_name"], "docker-cluster");
            assert_eq!(e.data["unassigned_shards"], 1);
        }
    }

    /// Same mock behind TLS with a throwaway CA: trusted config passes, OS roots fail.
    #[cfg(feature = "elasticsearch-tls")]
    #[tokio::test]
    async fn https_with_private_ca() {
        use health_checks::tls::{RootCertStore, config_with_roots};
        use hyper_util::rt::{TokioExecutor, TokioIo};
        use hyper_util::server::conn::auto::Builder;
        use std::sync::Arc;
        use tokio_rustls::rustls;

        let cert = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
        let der = cert.cert.der().clone();
        let key = rustls::pki_types::PrivateKeyDer::Pkcs8(cert.signing_key.serialize_der().into());
        let server = rustls::ServerConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(vec![der.clone()], key)
        .unwrap();
        let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(server));

        let app = Router::new().route(
            "/_cluster/health",
            get(|| async { r#"{"cluster_name":"tls","status":"green","number_of_nodes":1,"number_of_data_nodes":1,"active_shards":1,"unassigned_shards":0}"# }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            loop {
                let (tcp, _) = listener.accept().await.unwrap();
                let (acceptor, app) = (acceptor.clone(), app.clone());
                tokio::spawn(async move {
                    let Ok(tls) = acceptor.accept(tcp).await else {
                        return;
                    };
                    let svc = hyper_util::service::TowerToHyperService::new(app);
                    let _ = Builder::new(TokioExecutor::new())
                        .serve_connection(TokioIo::new(tls), svc)
                        .await;
                });
            }
        });
        let url = format!("https://localhost:{port}");

        let mut roots = RootCertStore::empty();
        roots.add(der).unwrap();
        let trusted = ElasticsearchCheck::new(&url)
            .unwrap()
            .with_tls_config(config_with_roots(roots).unwrap())
            .unwrap();
        let report = run("es", trusted).await;
        assert_eq!(report.status, HealthStatus::Healthy, "{}", report.to_json());
        assert_eq!(report.entries["es"].data["cluster_name"], "tls");

        let report = run("es", ElasticsearchCheck::new(&url).unwrap()).await;
        assert_failed(&report, "es");

        // Self-signed options.
        let pem = cert.cert.pem();
        let fingerprint: String = {
            use std::fmt::Write;
            let d = ring_digest(cert.cert.der());
            d.iter().fold(String::new(), |mut s, b| {
                let _ = write!(s, "{b:02X}:");
                s
            })
        };
        for (name, check) in [
            (
                "pem",
                ElasticsearchCheck::new(&url)
                    .unwrap()
                    .ca_certificate_pem(pem.as_bytes())
                    .unwrap(),
            ),
            (
                "fingerprint",
                ElasticsearchCheck::new(&url)
                    .unwrap()
                    .ca_fingerprint(fingerprint.trim_end_matches(':'))
                    .unwrap(),
            ),
            (
                "insecure",
                ElasticsearchCheck::new(&url)
                    .unwrap()
                    .danger_accept_invalid_certs()
                    .unwrap(),
            ),
        ] {
            let report = run("es", check).await;
            assert_eq!(
                report.status,
                HealthStatus::Healthy,
                "{name}: {}",
                report.to_json()
            );
        }
        let wrong = ElasticsearchCheck::new(&url)
            .unwrap()
            .ca_fingerprint(&"00".repeat(32))
            .unwrap();
        assert_failed(&run("es", wrong).await, "es");

        // Hostname must match for the PEM option: connect via 127.0.0.1 instead of localhost.
        let by_ip = format!("https://127.0.0.1:{port}");
        let check = ElasticsearchCheck::new(&by_ip)
            .unwrap()
            .ca_certificate_pem(pem.as_bytes())
            .unwrap();
        assert_failed(&run("es", check).await, "es");
    }

    fn ring_digest(der: &[u8]) -> Vec<u8> {
        ring::digest::digest(&ring::digest::SHA256, der)
            .as_ref()
            .to_vec()
    }

    #[tokio::test]
    async fn http_error_is_failure() {
        // mock has no `/` route → 404 → error_for_status
        let url = mock("green").await;
        let report = run("es", ElasticsearchCheck::new(url).unwrap().ping_only()).await;
        assert_failed(&report, "es");
    }

    #[tokio::test]
    async fn unreachable() {
        let report = run(
            "es",
            ElasticsearchCheck::new(format!("http://{DEAD}")).unwrap(),
        )
        .await;
        assert_failed(&report, "es");
    }

    #[tokio::test]
    async fn live() {
        let Some(url) = env("ELASTIC_URL") else {
            return;
        };
        let report = run("es", ElasticsearchCheck::new(url).unwrap()).await;
        println!("{}", report.to_json_pretty());
        assert_ne!(report.status, HealthStatus::Unhealthy);
    }
}

#[cfg(feature = "redis")]
mod redis {
    use super::*;
    use health_checks::checks::RedisCheck;

    #[tokio::test]
    async fn unreachable() {
        let report = run("redis", RedisCheck::new(format!("redis://{DEAD}")).unwrap()).await;
        assert_failed(&report, "redis");
    }

    #[tokio::test]
    async fn live() {
        let Some(url) = env("REDIS_URL") else { return };
        let report = run("redis", RedisCheck::new(url.as_str()).unwrap()).await;
        println!("{}", report.to_json_pretty());
        assert_eq!(report.status, HealthStatus::Healthy);
    }
}

#[cfg(feature = "postgres")]
mod postgres {
    use super::*;
    use health_checks::checks::PostgresCheck;

    #[tokio::test]
    async fn unreachable() {
        let check = PostgresCheck::new("Host=127.0.0.1;Port=1;Username=u;Password=p").unwrap();
        let report = run("pg", check).await;
        assert_failed(&report, "pg");
    }

    /// POSTGRES_TLS_URL (Npgsql form, `Host=localhost`, no SSL Mode) + POSTGRES_TLS_CA (CA PEM path).
    #[cfg(feature = "postgres-tls")]
    #[tokio::test]
    async fn live_tls_with_private_ca() {
        let (Some(url), Some(ca)) = (env("POSTGRES_TLS_URL"), env("POSTGRES_TLS_CA")) else {
            return;
        };
        let ok = |cs: String| async move {
            let r = run("pg", PostgresCheck::new(&cs).unwrap()).await;
            (r.status, r.entries["pg"].exception.clone())
        };
        // Self-signed server, Npgsql semantics:
        assert_eq!(
            ok(format!("{url};SSL Mode=Require")).await.0,
            HealthStatus::Healthy
        );
        assert_eq!(
            ok(format!("{url};SSL Mode=Prefer")).await.0,
            HealthStatus::Healthy
        );
        assert_eq!(
            ok(format!("{url};SSL Mode=VerifyFull;Root Certificate={ca}"))
                .await
                .0,
            HealthStatus::Healthy
        );
        assert_eq!(
            ok(format!("{url};SSL Mode=Require;Root Certificate={ca}"))
                .await
                .0,
            HealthStatus::Healthy
        );
        // OS store doesn't know the CA → rejected.
        let (s, e) = ok(format!("{url};SSL Mode=VerifyCA")).await;
        println!("verify-ca, OS roots: {e:?}");
        assert_eq!(s, HealthStatus::Unhealthy);
        // Cert is for "localhost": VerifyFull via 127.0.0.1 fails, VerifyCA passes.
        let ip = url.replace("Host=localhost", "Host=127.0.0.1");
        let (s, e) = ok(format!("{ip};SSL Mode=VerifyFull;Root Certificate={ca}")).await;
        println!("verify-full by IP: {e:?}");
        assert_eq!(s, HealthStatus::Unhealthy);
        assert_eq!(
            ok(format!("{ip};SSL Mode=VerifyCA;Root Certificate={ca}"))
                .await
                .0,
            HealthStatus::Healthy
        );
    }

    #[tokio::test]
    async fn live() {
        let Some(url) = env("POSTGRES_URL") else {
            return;
        };
        let report = run("pg", PostgresCheck::new(&url).unwrap()).await;
        println!("{}", report.to_json_pretty());
        assert_eq!(report.status, HealthStatus::Healthy);
    }
}

#[cfg(feature = "sqlserver")]
mod sqlserver {
    use super::*;
    use health_checks::checks::SqlServerCheck;

    #[test]
    fn parses_ado_connection_string() {
        SqlServerCheck::new("Server=tcp:db.example.com,1433;Database=app;User Id=sa;Password=x;TrustServerCertificate=true").unwrap();
    }

    #[tokio::test]
    async fn unreachable() {
        let check = SqlServerCheck::new(
            "Server=tcp:127.0.0.1,1;User Id=sa;Password=x;TrustServerCertificate=true",
        )
        .unwrap();
        let report = run("sql", check).await;
        assert_failed(&report, "sql");
    }

    #[tokio::test]
    async fn live() {
        let Some(cs) = env("SQLSERVER_CS") else {
            return;
        };
        let report = run("sql", SqlServerCheck::new(&cs).unwrap()).await;
        println!("{}", report.to_json_pretty());
        assert_eq!(report.status, HealthStatus::Healthy);
    }
}

#[cfg(feature = "kafka")]
mod kafka {
    use super::*;
    use health_checks::checks::KafkaCheck;

    #[tokio::test]
    async fn unreachable() {
        let report = run("kafka", KafkaCheck::new([DEAD])).await;
        // rskafka retries bootstrap with backoff; accept failure or timeout.
        assert_eq!(report.entries["kafka"].status, HealthStatus::Unhealthy);
    }

    #[tokio::test]
    async fn live() {
        let Some(brokers) = env("KAFKA_BROKERS") else {
            return;
        };
        let report = run("kafka", KafkaCheck::new(brokers.split(','))).await;
        println!("{}", report.to_json_pretty());
        assert_eq!(report.status, HealthStatus::Healthy);
    }
}
