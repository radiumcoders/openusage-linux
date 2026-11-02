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
        /// Claude's claim confirmation, kept off the figures. Empty means a claim is offered with no
        /// extra sentence. Absent means this line is not a claimable Claude grant.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
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
        Self::Values { label: label.into(), values, expiries_at: vec![], detail: None }
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
    /// Per-model detail behind a history row, keyed by descriptor key ("today", "last30", …): the Mac's
    /// usage-breakdown hover. Providers fill it where their API reports models.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub breakdowns: std::collections::BTreeMap<String, UsageBreakdown>,
}

/// One model's part of a history row's range (a port of `UsageBreakdown.Model`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BreakdownModel {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dollars: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tokens: Option<f64>,
    /// A share of credits or turns when neither dollars nor tokens are reported by model (0–1).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub share: Option<f64>,
}

/// The models behind one history row, most used first (the Mac shows the top five).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct UsageBreakdown {
    pub models: Vec<BreakdownModel>,
}

impl UsageBreakdown {
    pub const SHOWN_MODELS: usize = 5;

    /// The models the popover shows: ranked by cost, then tokens, then share, with the rest as Other.
    /// A figure is available only when some model reports it above zero. Dollars and tokens that were
    /// not reported are cleared so a zero-spend model is not treated as a cost.
    pub fn ranked(mut models: Vec<BreakdownModel>) -> Option<Self> {
        let cost = models.iter().any(|model| model.dollars.is_some_and(|number| number > 0.0));
        let tokens = models.iter().any(|model| model.tokens.is_some_and(|number| number > 0.0));
        let share = models.iter().any(|model| model.share.is_some_and(|number| number > 0.0));
        let figure = if cost {
            0
        } else if tokens {
            1
        } else if share {
            2
        } else {
            return None;
        };
        if !cost {
            for model in &mut models {
                model.dollars = None;
            }
        }
        if !tokens {
            for model in &mut models {
                model.tokens = None;
            }
        }
        let number = |model: &BreakdownModel| match figure {
            0 => model.dollars,
            1 => model.tokens,
            _ => model.share,
        };
        models.retain(|model| number(model).is_some());
        models.sort_by(|left, right| {
            let left_number = number(left).unwrap_or(0.0);
            let right_number = number(right).unwrap_or(0.0);
            right_number.total_cmp(&left_number).then_with(|| left.name.cmp(&right.name))
        });
        if models.is_empty() {
            return None;
        }
        if models.len() > Self::SHOWN_MODELS {
            let rest = models.split_off(Self::SHOWN_MODELS);
            models.push(BreakdownModel {
                name: "Other".into(),
                dollars: sum_optional(rest.iter().map(|model| model.dollars)),
                tokens: sum_optional(rest.iter().map(|model| model.tokens)),
                share: sum_optional(rest.iter().map(|model| model.share)),
            });
        }
        Some(Self { models })
    }
}

fn sum_optional(values: impl Iterator<Item = Option<f64>>) -> Option<f64> {
    let mut total = None;
    for value in values.flatten() {
        total = Some(total.unwrap_or(0.0) + value);
    }
    total
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
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub breakdowns: std::collections::BTreeMap<String, UsageBreakdown>,
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
