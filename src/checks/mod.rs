//! Ready-made checks, one feature flag per backend.
//!
//! Failures (connection errors, query errors, unexpected replies) are returned as `Err`,
//! so the runner reports them with the registration's failure status, error message as
//! `description` and the full error chain as `exception`.

#[cfg(feature = "elasticsearch")]
pub mod elasticsearch;
#[cfg(feature = "kafka")]
pub mod kafka;
#[cfg(feature = "postgres")]
pub mod postgres;
#[cfg(feature = "redis")]
pub mod redis;
#[cfg(feature = "sqlserver")]
pub mod sqlserver;

#[cfg(feature = "elasticsearch")]
pub use elasticsearch::ElasticsearchCheck;
#[cfg(feature = "kafka")]
pub use kafka::KafkaCheck;
#[cfg(feature = "postgres")]
pub use postgres::PostgresCheck;
#[cfg(feature = "redis")]
pub use redis::RedisCheck;
#[cfg(feature = "sqlserver")]
pub use sqlserver::SqlServerCheck;
