use std::collections::BTreeSet;
use std::time::Duration;

use indexmap::IndexMap;
use serde::{Serialize, Serializer};
use serde_json::Value;

use crate::{HealthCheckResult, HealthStatus};

/// Result of one registration inside a report. Equivalent of `HealthReportEntry`.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HealthReportEntry {
    pub data: IndexMap<String, Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(serialize_with = "serialize_timespan")]
    pub duration: Duration,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exception: Option<String>,
    pub status: HealthStatus,
    pub tags: BTreeSet<String>,
}

impl HealthReportEntry {
    pub(crate) fn new(
        result: HealthCheckResult,
        duration: Duration,
        tags: BTreeSet<String>,
    ) -> Self {
        Self {
            data: result.data,
            description: result.description,
            duration,
            exception: result.exception,
            status: result.status,
            tags,
        }
    }
}

/// Aggregated result. Equivalent of `HealthReport`.
///
/// Serializes to the same shape as Xabaril's `UIResponseWriter.WriteHealthCheckUIResponse`,
/// so HealthChecks.UI and existing dashboards can consume it unchanged.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HealthReport {
    pub status: HealthStatus,
    #[serde(serialize_with = "serialize_timespan")]
    pub total_duration: Duration,
    /// Registration order is preserved.
    pub entries: IndexMap<String, HealthReportEntry>,
}

impl HealthReport {
    /// Overall status is the worst entry; an empty report is `Healthy` (as in .NET).
    pub fn new(entries: IndexMap<String, HealthReportEntry>, total_duration: Duration) -> Self {
        let status = entries
            .values()
            .map(|e| e.status)
            .min()
            .unwrap_or(HealthStatus::Healthy);
        Self {
            status,
            total_duration,
            entries,
        }
    }

    pub fn to_json(&self) -> String {
        // Cannot fail: all keys are strings and all values are plain data.
        serde_json::to_string(self).expect("HealthReport serialization")
    }

    pub fn to_json_pretty(&self) -> String {
        serde_json::to_string_pretty(self).expect("HealthReport serialization")
    }
}

/// .NET `TimeSpan.ToString("c")`: `[d.]hh:mm:ss[.fffffff]`, 100 ns resolution.
pub fn format_timespan(d: Duration) -> String {
    let ticks = d.as_nanos() / 100;
    let frac = ticks % 10_000_000;
    let total_secs = ticks / 10_000_000;
    let (secs, mins) = (total_secs % 60, total_secs / 60);
    let (mins, hours) = (mins % 60, mins / 60);
    let (hours, days) = (hours % 24, hours / 24);

    let mut s = String::with_capacity(20);
    if days > 0 {
        s.push_str(&format!("{days}."));
    }
    s.push_str(&format!("{hours:02}:{mins:02}:{secs:02}"));
    if frac > 0 {
        s.push_str(&format!(".{frac:07}"));
    }
    s
}

fn serialize_timespan<S: Serializer>(d: &Duration, s: S) -> Result<S::Ok, S::Error> {
    s.serialize_str(&format_timespan(*d))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timespan_matches_dotnet_constant_format() {
        assert_eq!(format_timespan(Duration::ZERO), "00:00:00");
        assert_eq!(
            format_timespan(Duration::from_micros(12_345)),
            "00:00:00.0123450"
        );
        assert_eq!(
            format_timespan(Duration::from_nanos(1_234_567_899)),
            "00:00:01.2345678"
        );
        assert_eq!(format_timespan(Duration::from_secs(3_723)), "01:02:03");
        assert_eq!(format_timespan(Duration::from_secs(90_061)), "1.01:01:01");
    }
}
