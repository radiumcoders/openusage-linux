//! What each account's card can show, in order: a port of `AccountProviderRuntime+Descriptors.swift`.
//! A descriptor finds its reading by matching `metric_label` to a `MetricLine` label.

use crate::account::Account;
use crate::kind::ProviderKind;
use crate::metric::{MetricKind, MetricValue};

pub const MONTH_MS: i64 = 30 * 24 * 60 * 60 * 1000;

/// How a fresh window is told apart from an untouched one (see `WidgetData.SessionStartSignal`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionStartSignal {
    ZeroUsage,
    MissingResetDate,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Style {
    /// A 0–100% bar.
    Percent {
        session_start: Option<SessionStartSignal>,
    },
    /// A dollar bar whose limit comes from the line ("$12 spent" when unbounded).
    BoundedDollars {
        limit: f64,
        value_word: Option<&'static str>,
    },
    /// A count bar whose limit comes from the line.
    BoundedCount { limit: f64, period_ms: Option<i64> },
    /// Figures with no bar.
    Values {
        value_word: Option<&'static str>,
        shows_reset_expiries: bool,
    },
    /// Money held, e.g. "$12 left".
    DollarBalance { value_word: &'static str },
    /// A short text, e.g. Grok's "Pay as you go".
    Badge,
    /// A history row: "Today  $1.20 · 1.2M tokens".
    History,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LimitKind {
    Consumption,
    Balance,
}

impl LimitKind {
    pub fn raw(self) -> &'static str {
        match self {
            Self::Consumption => "consumption",
            Self::Balance => "balance",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum LimitSource {
    Progress,
    Value {
        kind: MetricKind,
        label: Option<&'static str>,
    },
    ProgressOrValue {
        kind: MetricKind,
        label: Option<&'static str>,
    },
}

/// A resource in the `openusage.limits.v2` JSON (see docs/cli.md).
#[derive(Debug, Clone, PartialEq)]
pub struct LimitExport {
    pub key: &'static str,
    pub kind: LimitKind,
    pub unit: &'static str,
    pub source: LimitSource,
}

impl LimitExport {
    /// The figure of a values line this resource reads.
    pub fn value<'a>(&self, values: &'a [MetricValue]) -> Option<&'a MetricValue> {
        match &self.source {
            LimitSource::Progress => None,
            LimitSource::Value { kind, label } | LimitSource::ProgressOrValue { kind, label } => {
                values.iter().find(|value| {
                    value.kind == *kind && (label.is_none() || value.label.as_deref() == *label)
                })
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Descriptor {
    /// `<card id>.<key>`
    pub id: String,
    pub provider_id: String,
    pub kind: ProviderKind,
    pub key: &'static str,
    pub title: &'static str,
    pub metric_label: &'static str,
    pub style: Style,
    pub export: Option<LimitExport>,
}

impl Descriptor {
    pub fn kind_scoped_key(&self) -> String {
        format!("{}.{}", self.kind.raw(), self.key)
    }

    pub fn shows_reset_expiries(&self) -> bool {
        matches!(
            self.style,
            Style::Values {
                shows_reset_expiries: true,
                ..
            }
        )
    }
}

struct Builder<'a> {
    account: &'a Account,
    list: Vec<Descriptor>,
}

impl Builder<'_> {
    fn add(
        &mut self,
        key: &'static str,
        title: &'static str,
        label: Option<&'static str>,
        style: Style,
    ) -> &mut Self {
        let provider_id = self.account.card_id();
        self.list.push(Descriptor {
            id: format!("{provider_id}.{key}"),
            provider_id,
            kind: self.account.kind,
            key,
            title,
            metric_label: label.unwrap_or(title),
            style,
            export: None,
        });
        self
    }

    fn export(&mut self, key: &'static str, unit: &'static str) -> &mut Self {
        self.export_as(key, LimitKind::Consumption, unit, LimitSource::Progress)
    }

    fn export_as(
        &mut self,
        key: &'static str,
        kind: LimitKind,
        unit: &'static str,
        source: LimitSource,
    ) -> &mut Self {
        if let Some(last) = self.list.last_mut() {
            last.export = Some(LimitExport {
                key,
                kind,
                unit,
                source,
            });
        }
        self
    }

    fn percent(&mut self, key: &'static str, title: &'static str) -> &mut Self {
        self.add(
            key,
            title,
            None,
            Style::Percent {
                session_start: None,
            },
        )
    }

    fn percent_labeled(
        &mut self,
        key: &'static str,
        title: &'static str,
        label: &'static str,
    ) -> &mut Self {
        self.add(
            key,
            title,
            Some(label),
            Style::Percent {
                session_start: None,
            },
        )
    }

    fn session(
        &mut self,
        key: &'static str,
        title: &'static str,
        label: Option<&'static str>,
        signal: SessionStartSignal,
    ) -> &mut Self {
        self.add(
            key,
            title,
            label,
            Style::Percent {
                session_start: Some(signal),
            },
        )
    }

    fn history(&mut self, key: &'static str, title: &'static str) -> &mut Self {
        self.add(key, title, None, Style::History)
    }

    fn rate_limit_resets(&mut self) -> &mut Self {
        self.add(
            "rateLimitResets",
            "Limit Resets",
            Some("Rate Limit Resets"),
            Style::Values {
                value_word: None,
                shows_reset_expiries: true,
            },
        )
        .export_as(
            "rateLimitResets",
            LimitKind::Balance,
            "resets",
            LimitSource::Value {
                kind: MetricKind::Count,
                label: Some("available"),
            },
        )
    }
}

pub fn descriptors(account: &Account) -> Vec<Descriptor> {
    use LimitKind::*;
    use MetricKind::*;
    use SessionStartSignal::*;
    let mut b = Builder {
        account,
        list: vec![],
    };
    let values = |value_word| Style::Values {
        value_word,
        shows_reset_expiries: false,
    };
    match account.kind {
        ProviderKind::Claude => {
            b.session("session", "Session", None, MissingResetDate)
                .export("session", "percent");
            b.percent("weekly", "Weekly").export("weekly", "percent");
            b.percent("sonnet", "Sonnet").export("sonnet", "percent");
            b.percent("fable", "Fable").export("fable", "percent");
            b.add(
                "extra",
                "Extra Usage",
                Some("Extra usage spent"),
                Style::BoundedDollars {
                    limit: 100.0,
                    value_word: Some("spent"),
                },
            )
            .export_as(
                "extraUsage",
                Consumption,
                "usd",
                LimitSource::ProgressOrValue {
                    kind: Dollars,
                    label: None,
                },
            );
            b.rate_limit_resets();
        }
        ProviderKind::Codex => {
            b.percent("session", "Session").export("session", "percent");
            b.percent("weekly", "Weekly").export("weekly", "percent");
            b.percent("spark", "Spark").export("spark", "percent");
            b.percent("sparkWeekly", "Spark Weekly")
                .export("sparkWeekly", "percent");
            b.add("credits", "Extra Usage", Some("Credits"), values(None))
                .export_as(
                    "credits",
                    Balance,
                    "credits",
                    LimitSource::Value {
                        kind: Count,
                        label: Some("credits"),
                    },
                );
            b.add(
                "creditLimit",
                "Credit Limit",
                None,
                Style::BoundedCount {
                    limit: 0.0,
                    period_ms: None,
                },
            );
            b.rate_limit_resets();
            b.history("today", "Today");
            b.history("yesterday", "Yesterday");
            b.history("last30", "Last 30 Days");
        }
        ProviderKind::Cursor => {
            b.percent_labeled("usage", "Total Usage", "Total usage")
                .export("totalUsage", "percent");
            b.percent("auto", "Cursor Models")
                .export("autoUsage", "percent");
            b.percent("api", "Other Models")
                .export("apiUsage", "percent");
            b.add(
                "onDemand",
                "Extra Usage",
                Some("On-demand"),
                Style::BoundedDollars {
                    limit: 100.0,
                    value_word: Some("spent"),
                },
            )
            .export_as(
                "onDemand",
                Consumption,
                "usd",
                LimitSource::ProgressOrValue {
                    kind: Dollars,
                    label: None,
                },
            );
            b.percent_labeled("grokBot", "Grok Bot", "Grok Bot usage")
                .export("grokBot", "percent");
            b.add(
                "requests",
                "Requests",
                None,
                Style::BoundedCount {
                    limit: 500.0,
                    period_ms: Some(MONTH_MS),
                },
            )
            .export("requests", "requests");
            b.add(
                "credits",
                "Credits",
                None,
                Style::DollarBalance { value_word: "left" },
            )
            .export_as(
                "credits",
                Balance,
                "usd",
                LimitSource::Value {
                    kind: Dollars,
                    label: None,
                },
            );
            b.history("today", "Today");
            b.history("yesterday", "Yesterday");
            b.history("last30", "Last 30 Days");
        }
        ProviderKind::Antigravity => {
            b.session("geminiPro", "Gemini 5h", Some("Session"), ZeroUsage)
                .export("geminiSession", "percent");
            b.percent_labeled("geminiWeekly", "Gemini", "Weekly")
                .export("geminiWeekly", "percent");
            b.percent_labeled("argonWeekly", "Argon", "Argon Weekly")
                .export("argonWeekly", "percent");
            b.session("claude", "Claude 5h", Some("Claude"), ZeroUsage)
                .export("nonGeminiSession", "percent");
            b.percent_labeled("claudeWeekly", "Claude", "Claude Weekly")
                .export("nonGeminiWeekly", "percent");
        }
        ProviderKind::Copilot if account.organization.is_some() => {
            b.add("orgCredits", "Org Credits", None, values(None))
                .export_as(
                    "orgCredits",
                    Consumption,
                    "credits",
                    LimitSource::Value {
                        kind: Count,
                        label: Some("credits"),
                    },
                );
            b.add("orgSpend", "Org Spend", None, values(Some("spent")))
                .export_as(
                    "orgSpend",
                    Consumption,
                    "usd",
                    LimitSource::Value {
                        kind: Dollars,
                        label: None,
                    },
                );
        }
        ProviderKind::Copilot => {
            b.percent_labeled("premium", "Credits", "Credits")
                .export_as(
                    "premiumCredits",
                    Consumption,
                    "credits",
                    LimitSource::ProgressOrValue {
                        kind: Count,
                        label: None,
                    },
                );
            b.add("extra", "Extra Usage", None, values(None)).export_as(
                "extraUsage",
                Consumption,
                "count",
                LimitSource::Value {
                    kind: Count,
                    label: None,
                },
            );
            b.add(
                "chat",
                "Chat",
                None,
                Style::BoundedCount {
                    limit: 0.0,
                    period_ms: None,
                },
            )
            .export("chat", "percent");
            b.add(
                "completions",
                "Completions",
                None,
                Style::BoundedCount {
                    limit: 0.0,
                    period_ms: None,
                },
            )
            .export("completions", "percent");
        }
        ProviderKind::Devin => {
            b.percent_labeled("daily", "Daily", "Daily quota")
                .export("daily", "percent");
            b.percent_labeled("weekly", "Weekly", "Weekly quota")
                .export("weekly", "percent");
            b.add(
                "extra",
                "Extra Usage",
                Some("Extra usage balance"),
                Style::DollarBalance { value_word: "left" },
            )
            .export_as(
                "extraUsageBalance",
                Balance,
                "usd",
                LimitSource::Value {
                    kind: Dollars,
                    label: None,
                },
            );
        }
        ProviderKind::Grok => {
            b.percent_labeled("weekly", "Weekly", "Weekly limit")
                .export("weekly", "percent");
            b.add(
                "payAsYouGo",
                "Extra Usage",
                Some("Pay as you go"),
                Style::Badge,
            );
        }
        ProviderKind::Ollama => {
            b.percent("session", "Session").export("session", "percent");
            b.percent("weekly", "Weekly").export("weekly", "percent");
            b.percent("monthly", "Monthly").export("monthly", "percent");
            b.add(
                "purchasedCredits",
                "Extra Usage",
                Some("Purchased Credits"),
                Style::DollarBalance { value_word: "left" },
            )
            .export_as(
                "purchasedCredits",
                Balance,
                "usd",
                LimitSource::Value {
                    kind: Dollars,
                    label: None,
                },
            );
            b.history("today", "Today");
            b.history("yesterday", "Yesterday");
            b.history("last30", "Last 30 Days");
            b.history("requests", "Requests");
        }
        ProviderKind::OpenCode => {
            b.session("session", "Session", None, MissingResetDate)
                .export("session", "percent");
            b.percent("weekly", "Weekly").export("weekly", "percent");
            b.percent("monthly", "Monthly").export("monthly", "percent");
        }
        ProviderKind::OpenRouter => {
            b.add(
                "balance",
                "Balance",
                None,
                Style::DollarBalance { value_word: "left" },
            )
            .export_as(
                "balance",
                Balance,
                "usd",
                LimitSource::Value {
                    kind: Dollars,
                    label: None,
                },
            );
            b.history("today", "Today");
            b.history("yesterday", "Yesterday");
            b.history("week", "This Week");
            b.history("month", "This Month");
            b.history("prior30", "Prior 30 Days");
            b.add(
                "keyLimit",
                "Key Limit",
                None,
                Style::BoundedDollars {
                    limit: 100.0,
                    value_word: None,
                },
            )
            .export("keyLimit", "usd");
            b.add(
                "budget",
                "Budget",
                None,
                Style::BoundedDollars {
                    limit: 100.0,
                    value_word: None,
                },
            );
            b.add(
                "freeRequests",
                "Free Requests",
                None,
                Style::BoundedCount {
                    limit: 50.0,
                    period_ms: None,
                },
            );
        }
        ProviderKind::Zai => {
            b.percent("session", "Session").export("session", "percent");
            b.percent("weekly", "Weekly").export("weekly", "percent");
            b.add(
                "webSearches",
                "Web Searches",
                None,
                Style::BoundedCount {
                    limit: 0.0,
                    period_ms: None,
                },
            )
            .export("webSearches", "searches");
        }
    }
    b.list
}
