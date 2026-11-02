use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MetricKind {
    Percent,
    Dollars,
    Count,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "type")]
pub enum ProgressFormat {
    Percent,
    Dollars,
    Count { suffix: String },
}

impl ProgressFormat {
    pub fn metric_kind(&self) -> MetricKind {
        match self {
            Self::Percent => MetricKind::Percent,
            Self::Dollars => MetricKind::Dollars,
            Self::Count { .. } => MetricKind::Count,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MetricValue {
    pub number: f64,
    pub kind: MetricKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

impl MetricValue {
    pub fn new(number: f64, kind: MetricKind) -> Self {
        Self { number, kind, label: None }
    }

    pub fn labeled(number: f64, kind: MetricKind, label: impl Into<String>) -> Self {
        Self { number, kind, label: Some(label.into()) }
    }
}

/// One reading a provider mapper produces. The `label` is matched against a descriptor's
/// `metric_label` (see `descriptor.rs`), exactly as the Mac app matches `MetricLine.label`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "type")]
pub enum MetricLine {
    Progress {
        label: String,
        used: f64,
        limit: f64,
        format: ProgressFormat,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        resets_at: Option<DateTime<Utc>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        period_duration_ms: Option<i64>,
    },
    Values {
        label: String,
        values: Vec<MetricValue>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        expiries_at: Vec<DateTime<Utc>>,
    },
    Badge {
        label: String,
        text: String,
    },
}

impl MetricLine {
    pub fn label(&self) -> &str {
        match self {
            Self::Progress { label, .. } | Self::Values { label, .. } | Self::Badge { label, .. } => label,
        }
    }

    /// `.percent(label, used, resetsAt:, periodMs:)` in the Swift code.
    pub fn percent(label: impl Into<String>, used: f64, resets_at: Option<DateTime<Utc>>, period_ms: Option<i64>) -> Self {
        Self::Progress {
            label: label.into(),
            used,
            limit: 100.0,
            format: ProgressFormat::Percent,
            resets_at,
            period_duration_ms: period_ms,
        }
    }

    pub fn values(label: impl Into<String>, values: Vec<MetricValue>) -> Self {
        Self::Values { label: label.into(), values, expiries_at: vec![] }
    }
}

/// What one provider read returns: the account's plan, its metric lines, and an optional notice.
/// History rows (Today, Yesterday, Last 30 Days, …) are `Values` lines labeled with the row's title, each
/// holding at most two figures: dollars and/or a count labeled "tokens" or "requests".
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ProviderUsage {
    pub plan: Option<String>,
    pub lines: Vec<MetricLine>,
    pub notice: Option<String>,
}

/// The last successful read of one account, as cached and shown.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderSnapshot {
    pub provider_id: String,
    pub display_name: String,
    #[serde(default)]
    pub plan: Option<String>,
    pub lines: Vec<MetricLine>,
    #[serde(default)]
    pub notice: Option<String>,
    pub refreshed_at: DateTime<Utc>,
}

impl ProviderSnapshot {
    pub fn line(&self, label: &str) -> Option<&MetricLine> {
        self.lines.iter().find(|line| line.label() == label)
    }

    pub fn stale_at(&self) -> DateTime<Utc> {
        self.refreshed_at + Duration::seconds(crate::refresh::STALENESS_THRESHOLD_SECONDS)
    }

    pub fn is_stale(&self, now: DateTime<Utc>) -> bool {
        now >= self.stale_at()
    }
}
