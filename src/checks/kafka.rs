//! Kafka via pure-Rust `rskafka` (feature `kafka`). Equivalent of `AddKafka`.
//!
//! Fetches cluster metadata (no message is produced, unlike Xabaril's check, so no
//! test topic is needed). Optionally requires specific topics to exist.
//! No librdkafka / C toolchain involved.
//!
//! TLS / SASL: enable `rskafka`'s `transport-tls` in your own manifest and pass a
//! [`KafkaCheck::configure`] closure that sets `tls_config` / `sasl_config`.

use std::sync::Arc;

use async_trait::async_trait;
use rskafka::client::{Client, ClientBuilder};

use crate::{BoxError, HealthCheck, HealthCheckContext, HealthCheckResult};

type Configure = Arc<dyn Fn(ClientBuilder) -> ClientBuilder + Send + Sync>;

enum Source {
    Brokers {
        brokers: Vec<String>,
        configure: Option<Configure>,
    },
    Client(Arc<Client>),
}

pub struct KafkaCheck {
    source: Source,
    required_topics: Vec<String>,
}

impl KafkaCheck {
    /// Bootstrap brokers, e.g. `["localhost:9092"]`. A new client is built on every check.
    pub fn new<I, S>(brokers: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self {
            source: Source::Brokers {
                brokers: brokers.into_iter().map(Into::into).collect(),
                configure: None,
            },
            required_topics: Vec::new(),
        }
    }

    /// Reuse an existing client.
    pub fn from_client(client: Arc<Client>) -> Self {
        Self {
            source: Source::Client(client),
            required_topics: Vec::new(),
        }
    }

    /// Customise the per-check `ClientBuilder` (TLS, SASL, client id...).
    /// Ignored when built with [`KafkaCheck::from_client`].
    pub fn configure<F>(mut self, f: F) -> Self
    where
        F: Fn(ClientBuilder) -> ClientBuilder + Send + Sync + 'static,
    {
        if let Source::Brokers { configure, .. } = &mut self.source {
            *configure = Some(Arc::new(f));
        }
        self
    }

    /// Report the failure status if any of these topics is missing.
    pub fn require_topics<I, S>(mut self, topics: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.required_topics
            .extend(topics.into_iter().map(Into::into));
        self
    }
}

#[async_trait]
impl HealthCheck for KafkaCheck {
    async fn check(&self, ctx: &HealthCheckContext) -> Result<HealthCheckResult, BoxError> {
        let topics = match &self.source {
            Source::Client(client) => client.list_topics().await?,
            Source::Brokers { brokers, configure } => {
                let mut builder = ClientBuilder::new(brokers.clone()).client_id("health-check");
                if let Some(f) = configure {
                    builder = f(builder);
                }
                builder.build().await?.list_topics().await?
            }
        };

        let missing: Vec<&str> = self
            .required_topics
            .iter()
            .filter(|t| !topics.iter().any(|x| &x.name == *t))
            .map(String::as_str)
            .collect();

        let result = if missing.is_empty() {
            HealthCheckResult::healthy()
        } else {
            HealthCheckResult::new(ctx.failure_status())
                .with_description(format!("missing topics: {}", missing.join(", ")))
                .with_data("missing_topics", &missing)
        };
        Ok(result.with_data("topic_count", topics.len()))
    }
}
