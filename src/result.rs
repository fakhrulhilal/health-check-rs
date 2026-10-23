use std::error::Error as StdError;
use std::fmt::Write as _;

use indexmap::IndexMap;
use serde::Serialize;
use serde_json::Value;

use crate::HealthStatus;

/// Boxed error a check may return. Any error is mapped to the registration's
/// failure status, the way ASP.NET Core maps a thrown exception.
pub type BoxError = Box<dyn StdError + Send + Sync + 'static>;

/// Outcome of a single check. Equivalent of `HealthCheckResult`.
#[derive(Debug, Clone, PartialEq)]
pub struct HealthCheckResult {
    pub status: HealthStatus,
    pub description: Option<String>,
    /// Error details (message plus source chain). `Exception` in .NET.
    pub exception: Option<String>,
    pub data: IndexMap<String, Value>,
}

impl HealthCheckResult {
    pub fn new(status: HealthStatus) -> Self {
        Self {
            status,
            description: None,
            exception: None,
            data: IndexMap::new(),
        }
    }

    pub fn healthy() -> Self {
        Self::new(HealthStatus::Healthy)
    }

    pub fn degraded() -> Self {
        Self::new(HealthStatus::Degraded)
    }

    pub fn unhealthy() -> Self {
        Self::new(HealthStatus::Unhealthy)
    }

    /// Build a failed result from an error: description is the top-level message,
    /// `exception` holds the full source chain.
    pub fn from_error(status: HealthStatus, error: &(dyn StdError + 'static)) -> Self {
        Self::new(status)
            .with_description(error.to_string())
            .with_exception(error_chain(error))
    }

    pub fn with_description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }

    pub fn with_exception(mut self, exception: impl Into<String>) -> Self {
        self.exception = Some(exception.into());
        self
    }

    /// Attach a data item. Values that fail to serialize are stored as `null`.
    pub fn with_data(mut self, key: impl Into<String>, value: impl Serialize) -> Self {
        self.data.insert(
            key.into(),
            serde_json::to_value(value).unwrap_or(Value::Null),
        );
        self
    }
}

impl From<HealthStatus> for HealthCheckResult {
    fn from(status: HealthStatus) -> Self {
        Self::new(status)
    }
}

/// `"outer: inner: root"` from an error and its sources.
pub(crate) fn error_chain(error: &(dyn StdError + 'static)) -> String {
    let mut out = error.to_string();
    let mut source = error.source();
    while let Some(e) = source {
        let msg = e.to_string();
        // Many errors already embed their source's message; don't repeat it.
        if !out.contains(&msg) {
            let _ = write!(out, ": {msg}");
        }
        source = e.source();
    }
    out
}
