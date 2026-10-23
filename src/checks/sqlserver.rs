//! SQL Server via `tiberius-ng` (feature `sqlserver`). Equivalent of `AddSqlServer`.
//!
//! Takes an ADO.NET connection string, so the one from `appsettings.json` works as-is:
//! `Server=tcp:host,1433;Database=app;User Id=sa;Password=...;TrustServerCertificate=true`.
//! Opens a new connection per check (like Xabaril's check) and follows Azure SQL
//! gateway redirects. TLS is rustls.

use std::borrow::Cow;

use async_trait::async_trait;
use tiberius::error::Error as TdsError;
use tiberius::{Client, Config};
use tokio::net::TcpStream;
use tokio_util::compat::{Compat, TokioAsyncWriteCompatExt};

use crate::{BoxError, HealthCheck, HealthCheckContext, HealthCheckResult};

pub struct SqlServerCheck {
    config: Config,
    query: Cow<'static, str>,
}

impl SqlServerCheck {
    /// Parse an ADO.NET connection string.
    pub fn new(ado_connection_string: &str) -> Result<Self, BoxError> {
        Ok(Self::from_config(Config::from_ado_string(
            ado_connection_string,
        )?))
    }

    pub fn from_config(config: Config) -> Self {
        Self {
            config,
            query: Cow::Borrowed("SELECT 1"),
        }
    }

    /// Replace the default `SELECT 1`.
    pub fn with_query(mut self, query: impl Into<Cow<'static, str>>) -> Self {
        self.query = query.into();
        self
    }

    async fn connect(config: Config) -> Result<Client<Compat<TcpStream>>, BoxError> {
        let tcp = TcpStream::connect(config.get_addr()).await?;
        tcp.set_nodelay(true)?;
        match Client::connect(config.clone(), tcp.compat_write()).await {
            Ok(client) => Ok(client),
            // Azure SQL redirect policy: reconnect once to the address the gateway gave us.
            Err(TdsError::Routing { host, port }) => {
                let mut config = config;
                config.host(&host);
                config.port(port);
                let tcp = TcpStream::connect(config.get_addr()).await?;
                tcp.set_nodelay(true)?;
                Ok(Client::connect(config, tcp.compat_write()).await?)
            }
            Err(e) => Err(e.into()),
        }
    }
}

#[async_trait]
impl HealthCheck for SqlServerCheck {
    async fn check(&self, _ctx: &HealthCheckContext) -> Result<HealthCheckResult, BoxError> {
        let mut client = Self::connect(self.config.clone()).await?;
        client
            .simple_query(self.query.as_ref())
            .await?
            .into_results()
            .await?;
        client.close().await?;
        Ok(HealthCheckResult::healthy())
    }
}
