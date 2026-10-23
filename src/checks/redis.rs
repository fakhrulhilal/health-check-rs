//! Redis via the `redis` crate (feature `redis`). Equivalent of `AddRedis`.
//!
//! Sends `PING` and expects `PONG`. For TLS (`rediss://`) enable `tokio-rustls-comp`
//! (or `tokio-native-tls-comp`) on your own `redis` dependency.

use async_trait::async_trait;
use redis::aio::ConnectionManager;
use redis::{Client, IntoConnectionInfo};

use crate::{BoxError, HealthCheck, HealthCheckContext, HealthCheckResult};

enum Source {
    Client(Client),
    Manager(ConnectionManager),
}

pub struct RedisCheck {
    source: Source,
}

impl RedisCheck {
    /// Opens a fresh multiplexed connection per check, so a dead server is always noticed.
    pub fn new(info: impl IntoConnectionInfo) -> Result<Self, BoxError> {
        Ok(Self::from_client(Client::open(info)?))
    }

    pub fn from_client(client: Client) -> Self {
        Self {
            source: Source::Client(client),
        }
    }

    /// Reuse the application's auto-reconnecting `ConnectionManager`.
    pub fn from_connection_manager(manager: ConnectionManager) -> Self {
        Self {
            source: Source::Manager(manager),
        }
    }
}

#[async_trait]
impl HealthCheck for RedisCheck {
    async fn check(&self, _ctx: &HealthCheckContext) -> Result<HealthCheckResult, BoxError> {
        let reply: String = match &self.source {
            Source::Client(client) => {
                let mut conn = client.get_multiplexed_async_connection().await?;
                redis::cmd("PING").query_async(&mut conn).await?
            }
            Source::Manager(manager) => {
                let mut conn = manager.clone();
                redis::cmd("PING").query_async(&mut conn).await?
            }
        };
        if reply != "PONG" {
            return Err(format!("unexpected PING reply: {reply:?}").into());
        }
        Ok(HealthCheckResult::healthy())
    }
}
