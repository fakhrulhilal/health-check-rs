#![cfg(feature = "axum")]

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use health_checks::axum::RouterExt;
use health_checks::{HealthCheckOptions, HealthCheckResult, HealthChecks};
use http_body_util::BodyExt;
use tower::ServiceExt;

async fn call(app: &Router, method: &str, uri: &str) -> (StatusCode, String, String) {
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(uri)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = res.status();
    let ct = res.headers()["content-type"].to_str().unwrap().to_owned();
    let body =
        String::from_utf8(res.into_body().collect().await.unwrap().to_bytes().to_vec()).unwrap();
    (status, ct, body)
}

#[tokio::test]
async fn endpoints() {
    let health = HealthChecks::builder()
        .add_fn("self", HealthCheckResult::healthy)
        .add_fn("cache", HealthCheckResult::degraded)
        .build();
    let app: Router = Router::new()
        .map_health_checks("/health", health.clone(), HealthCheckOptions::new())
        .map_health_checks("/health/json", health.clone(), HealthCheckOptions::json())
        .map_health_checks("/health/live", health, HealthCheckOptions::liveness());

    let (s, ct, body) = call(&app, "GET", "/health").await;
    assert_eq!((s, body.as_str()), (StatusCode::OK, "Degraded"));
    assert!(ct.starts_with("text/plain"));

    let (s, ct, body) = call(&app, "GET", "/health/json").await;
    assert_eq!(s, StatusCode::OK);
    assert!(ct.starts_with("application/json"));
    assert!(body.contains(r#""status":"Degraded""#));

    let (s, _, body) = call(&app, "GET", "/health/live").await;
    assert_eq!((s, body.as_str()), (StatusCode::OK, "Healthy"));

    let (s, _, body) = call(&app, "HEAD", "/health").await;
    assert_eq!((s, body.as_str()), (StatusCode::OK, ""));
}

#[tokio::test]
async fn unhealthy_is_503() {
    let health = HealthChecks::builder()
        .add_fn("db", HealthCheckResult::unhealthy)
        .build();
    let app: Router = Router::new().map_health_checks("/h", health, HealthCheckOptions::new());
    let (s, _, body) = call(&app, "GET", "/h").await;
    assert_eq!(
        (s, body.as_str()),
        (StatusCode::SERVICE_UNAVAILABLE, "Unhealthy")
    );
}
