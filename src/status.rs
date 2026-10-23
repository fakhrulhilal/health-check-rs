use std::fmt;

use serde::{Deserialize, Serialize};

/// Health of a component or of the whole application.
///
/// Mirrors `Microsoft.Extensions.Diagnostics.HealthChecks.HealthStatus`, including its
/// ordering: `Unhealthy < Degraded < Healthy`. The aggregate status of a report is the
/// *minimum* of its entries, so the worst result wins.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum HealthStatus {
    Unhealthy = 0,
    Degraded = 1,
    Healthy = 2,
}

impl HealthStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Unhealthy => "Unhealthy",
            Self::Degraded => "Degraded",
            Self::Healthy => "Healthy",
        }
    }
}

impl fmt::Display for HealthStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}
