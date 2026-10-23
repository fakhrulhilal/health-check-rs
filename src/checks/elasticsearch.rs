//! Elasticsearch / OpenSearch over HTTP via `hyper` (feature `elasticsearch`,
//! `https://` with `elasticsearch-tls`). Equivalent of `AddElasticsearch`.
//!
//! Default mode calls `GET /_cluster/health` and maps the cluster colour:
//! green → Healthy, yellow → Degraded, red → the registration's failure status.
//! [`ElasticsearchCheck::ping_only`] just requires `GET /` to succeed.
//!
//! The URL scheme picks the transport, so `http://localhost:9200` in dev and
//! `https://es.prod:9200` in production work from the same build (`https` needs
//! `elasticsearch-tls`). Certificates are verified against the OS trust store by default;
//! for self-signed clusters use one of:
//! - [`ca_fingerprint`](ElasticsearchCheck::ca_fingerprint): the SHA-256 Elasticsearch prints at setup
//! - [`ca_certificate_pem`](ElasticsearchCheck::ca_certificate_pem): e.g. `config/certs/http_ca.crt`
//! - [`danger_accept_invalid_certs`](ElasticsearchCheck::danger_accept_invalid_certs): encryption only

use async_trait::async_trait;
use http_body_util::{BodyExt, Empty};
use hyper::body::Bytes;
use hyper::header::{AUTHORIZATION, HeaderValue};
use hyper::{Request, Uri};
use hyper_util::client::legacy::Client;
use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::rt::TokioExecutor;
use serde::Deserialize;

use crate::{BoxError, HealthCheck, HealthCheckContext, HealthCheckResult, HealthStatus};

#[cfg(feature = "elasticsearch-tls")]
type Connector = hyper_rustls::HttpsConnector<HttpConnector>;
#[cfg(not(feature = "elasticsearch-tls"))]
type Connector = HttpConnector;

pub struct ElasticsearchCheck {
    client: Client<Connector, Empty<Bytes>>,
    base_url: String,
    authorization: Option<HeaderValue>,
    cluster_health: bool,
}

#[derive(Deserialize)]
struct ClusterHealth {
    cluster_name: String,
    status: String,
    number_of_nodes: u64,
    number_of_data_nodes: u64,
    active_shards: u64,
    unassigned_shards: u64,
}

impl ElasticsearchCheck {
    /// `url` like `http://localhost:9200` or `https://es.example.com`.
    /// Credentials go through [`basic_auth`](Self::basic_auth) / [`api_key`](Self::api_key),
    /// not the URL.
    pub fn new(url: impl Into<String>) -> Result<Self, BoxError> {
        let base_url = url.into().trim_end_matches('/').to_owned();
        let uri: Uri = base_url.parse()?;
        match uri.scheme_str() {
            Some("http") => {}
            Some("https") if cfg!(feature = "elasticsearch-tls") => {}
            Some("https") => return Err("https:// needs the `elasticsearch-tls` feature".into()),
            other => return Err(format!("unsupported URL scheme: {other:?}").into()),
        }
        if uri.authority().is_some_and(|a| a.as_str().contains('@')) {
            return Err("put credentials in basic_auth()/api_key(), not in the URL".into());
        }
        Ok(Self {
            client: build_client(None)?,
            base_url,
            authorization: None,
            cluster_health: true,
        })
    }

    /// Use your own rustls config (see [`crate::tls`]).
    #[cfg(feature = "elasticsearch-tls")]
    pub fn with_tls_config(mut self, config: crate::tls::ClientConfig) -> Result<Self, BoxError> {
        self.client = build_client(Some(config))?;
        Ok(self)
    }

    /// Trust a cluster whose chain contains a certificate with this SHA-256 fingerprint
    /// (hex, `:` allowed). Same as the official clients' `ca_trusted_fingerprint`.
    #[cfg(feature = "elasticsearch-tls")]
    pub fn ca_fingerprint(self, sha256_hex: &str) -> Result<Self, BoxError> {
        self.with_tls_config(crate::tls::pinned_config([sha256_hex])?)
    }

    /// Trust this CA (PEM) instead of the OS store; hostname is still checked.
    #[cfg(feature = "elasticsearch-tls")]
    pub fn ca_certificate_pem(self, pem: &[u8]) -> Result<Self, BoxError> {
        self.with_tls_config(crate::tls::config_with_ca_pem(pem)?)
    }

    /// Accept any certificate (encryption only).
    #[cfg(feature = "elasticsearch-tls")]
    pub fn danger_accept_invalid_certs(self) -> Result<Self, BoxError> {
        self.with_tls_config(crate::tls::insecure_config()?)
    }

    pub fn basic_auth(mut self, user: &str, password: &str) -> Self {
        let token = base64(format!("{user}:{password}").as_bytes());
        self.authorization = header(format!("Basic {token}"));
        self
    }

    /// Base64 `id:key` value as issued by Elasticsearch (sent as `Authorization: ApiKey ...`).
    pub fn api_key(mut self, encoded: &str) -> Self {
        self.authorization = header(format!("ApiKey {encoded}"));
        self
    }

    /// Only check that the node answers `GET /`.
    pub fn ping_only(mut self) -> Self {
        self.cluster_health = false;
        self
    }

    async fn get(&self, path: &str) -> Result<Bytes, BoxError> {
        let uri = format!("{}{path}", self.base_url);
        let mut req = Request::get(&uri).body(Empty::new())?;
        if let Some(auth) = &self.authorization {
            req.headers_mut().insert(AUTHORIZATION, auth.clone());
        }
        let res = self.client.request(req).await?;
        let status = res.status();
        let body = res.into_body().collect().await?.to_bytes();
        if !status.is_success() {
            let snippet = String::from_utf8_lossy(&body[..body.len().min(200)]).into_owned();
            return Err(format!("HTTP {status} from {uri}: {snippet}").into());
        }
        Ok(body)
    }
}

#[async_trait]
impl HealthCheck for ElasticsearchCheck {
    async fn check(&self, ctx: &HealthCheckContext) -> Result<HealthCheckResult, BoxError> {
        if !self.cluster_health {
            self.get("/").await?;
            return Ok(HealthCheckResult::healthy());
        }

        let health: ClusterHealth = serde_json::from_slice(&self.get("/_cluster/health").await?)?;
        let status = match health.status.as_str() {
            "green" => HealthStatus::Healthy,
            "yellow" => HealthStatus::Degraded,
            _ => ctx.failure_status(),
        };
        Ok(HealthCheckResult::new(status)
            .with_description(format!("cluster status is {}", health.status))
            .with_data("cluster_name", health.cluster_name)
            .with_data("status", health.status)
            .with_data("number_of_nodes", health.number_of_nodes)
            .with_data("number_of_data_nodes", health.number_of_data_nodes)
            .with_data("active_shards", health.active_shards)
            .with_data("unassigned_shards", health.unassigned_shards))
    }
}

#[cfg(feature = "elasticsearch-tls")]
fn build_client(
    tls: Option<crate::tls::ClientConfig>,
) -> Result<Client<Connector, Empty<Bytes>>, BoxError> {
    // A missing OS trust store only matters for https URLs; keep plain http working.
    let config = match tls {
        Some(c) => c,
        None => crate::tls::native_roots_config()
            .or_else(|_| crate::tls::config_with_roots(rustls::RootCertStore::empty()))?,
    };
    let connector = hyper_rustls::HttpsConnectorBuilder::new()
        .with_tls_config(config)
        .https_or_http()
        .enable_http1()
        .build();
    Ok(Client::builder(TokioExecutor::new()).build(connector))
}

#[cfg(not(feature = "elasticsearch-tls"))]
fn build_client(_: Option<()>) -> Result<Client<Connector, Empty<Bytes>>, BoxError> {
    Ok(Client::builder(TokioExecutor::new()).build(HttpConnector::new()))
}

fn header(value: String) -> Option<HeaderValue> {
    HeaderValue::from_str(&value).ok().map(|mut v| {
        v.set_sensitive(true);
        v
    })
}

/// Standard base64 with padding (enough for a Basic auth header; avoids a dependency).
fn base64(input: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(T[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_rfc4648_vectors() {
        for (i, o) in [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="),
            ("elastic:changeme", "ZWxhc3RpYzpjaGFuZ2VtZQ=="),
        ] {
            assert_eq!(base64(i.as_bytes()), o);
        }
    }

    #[test]
    fn rejects_bad_urls() {
        assert!(ElasticsearchCheck::new("ftp://x").is_err());
        assert!(ElasticsearchCheck::new("http://u:p@x:9200").is_err());
        #[cfg(not(feature = "elasticsearch-tls"))]
        assert!(ElasticsearchCheck::new("https://x").is_err());
        #[cfg(feature = "elasticsearch-tls")]
        assert!(ElasticsearchCheck::new("https://x").is_ok());
    }
}
