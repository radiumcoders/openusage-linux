//! Total Spend: the dollars and tokens each provider reports for a period, summed across its
//! accounts. Port of 0.7.x's `TotalSpendAggregator`, reading only the history rows providers report
//! (Today, Yesterday, Last 30 Days). Nothing is estimated or priced here.

use std::collections::BTreeMap;

use crate::account::Account;
use crate::descriptor::{self, Style};
use crate::kind::ProviderKind;
use crate::layout::LayoutState;
use crate::metric::{MetricKind, MetricLine, ProviderSnapshot};
use crate::widget::{format_number, FormatStyle};

/// Shared with 0.7.x on the Mac, so a saved choice reads the same on both.
pub const SHOW_KEY: &str = "showTotalSpend";
pub const PERIOD_KEY: &str = "openusage.totalSpend.period";
pub const METRIC_KEY: &str = "openusage.totalSpend.metric";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpendPeriod {
    Today,
    Yesterday,
    Last30,
}

impl SpendPeriod {
    pub const ALL: [Self; 3] = [Self::Today, Self::Yesterday, Self::Last30];

    /// The stored value, which is also the history row's label.
    pub fn raw(self) -> &'static str {
        match self {
            Self::Today => "Today",
            Self::Yesterday => "Yesterday",
            Self::Last30 => "Last 30 Days",
        }
    }

    pub fn from_raw(raw: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|period| period.raw() == raw)
    }

    /// The switcher's segment title: "Last 30 Days" doesn't fit three across.
    pub fn short_label(self) -> &'static str {
        match self {
            Self::Today => "Today",
            Self::Yesterday => "Yesterday",
            Self::Last30 => "30 Days",
        }
    }

    /// The history descriptor's key, for its hidden switch.
    fn key(self) -> &'static str {
        match self {
            Self::Today => "today",
            Self::Yesterday => "yesterday",
            Self::Last30 => "last30",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpendMetric {
    Cost,
    CostPerMtok,
    Tokens,
}

impl SpendMetric {
    pub const ALL: [Self; 3] = [Self::Cost, Self::CostPerMtok, Self::Tokens];

    pub fn raw(self) -> &'static str {
        match self {
            Self::Cost => "apiSpend",
            Self::CostPerMtok => "costPerMtok",
            Self::Tokens => "tokens",
        }
    }

    pub fn from_raw(raw: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|metric| metric.raw() == raw)
    }

    pub fn title(self) -> &'static str {
        match self {
            Self::Cost => "Cost",
            Self::CostPerMtok => "Cost/MTok",
            Self::Tokens => "Tokens",
        }
    }

    pub fn empty_message(self) -> &'static str {
        match self {
            Self::Cost => "No cost data for this period",
            Self::CostPerMtok => "No cost-per-token data for this period",
            Self::Tokens => "No token data for this period",
        }
    }
}

/// One provider's share of a period, summed over its accounts.
#[derive(Debug, Clone, PartialEq)]
pub struct SpendSlice {
    pub kind: ProviderKind,
    pub dollars: f64,
    pub tokens: f64,
}

impl SpendSlice {
    fn cost_per_mtok(&self) -> Option<f64> {
        (self.dollars > 0.0 && self.tokens > 0.0).then(|| self.dollars / self.tokens * 1_000_000.0)
    }
}

/// A period under one metric: slices largest first, and the number in the ring's center.
#[derive(Debug, Clone, PartialEq)]
pub struct Projection {
    pub metric: SpendMetric,
    pub slices: Vec<(ProviderKind, f64)>,
    pub center: f64,
}

/// The account's provider reports history rows, so it can add to Total Spend.
pub fn tracks_spend(account: &Account) -> bool {
    descriptor::descriptors(account)
        .iter()
        .any(|descriptor| descriptor.style == Style::History)
}

/// Each provider's dollars and tokens for `period`, in the order `accounts` first names it. A history
/// row hidden on the account is left out, as it is everywhere else on the dashboard. A provider that
/// reported the period with nothing in it keeps a zero slice; one that reported no row has none.
pub fn total(
    period: SpendPeriod,
    accounts: &[Account],
    snapshots: &BTreeMap<String, ProviderSnapshot>,
    layout: &LayoutState,
) -> Vec<SpendSlice> {
    let mut slices: Vec<SpendSlice> = Vec::new();
    for account in accounts.iter().filter(|account| tracks_spend(account)) {
        let id = account.card_id();
        if layout.is_hidden(&format!("{id}.{}", period.key())) {
            continue;
        }
        let Some(MetricLine::Values { values, .. }) = snapshots
            .get(&id)
            .and_then(|snapshot| snapshot.line(period.raw()))
        else {
            continue;
        };
        let dollars: f64 = values
            .iter()
            .filter(|value| value.kind == MetricKind::Dollars)
            .map(|value| value.number.max(0.0))
            .sum();
        let tokens: f64 = values
            .iter()
            .filter(|value| value.kind == MetricKind::Count && value.label.as_deref() == Some("tokens"))
            .map(|value| value.number.max(0.0))
            .sum();
        match slices.iter_mut().find(|slice| slice.kind == account.kind) {
            Some(slice) => {
                slice.dollars += dollars;
                slice.tokens += tokens;
            }
            None => slices.push(SpendSlice {
                kind: account.kind,
                dollars,
                tokens,
            }),
        }
    }
    slices
}

/// Keeps the slices that have the metric, largest first (ties by name), with the center value.
pub fn project(slices: &[SpendSlice], metric: SpendMetric) -> Projection {
    let mut included: Vec<(&SpendSlice, f64)> = slices
        .iter()
        .filter_map(|slice| {
            let amount = match metric {
                SpendMetric::Cost => Some(slice.dollars).filter(|dollars| *dollars > 0.0),
                SpendMetric::Tokens => Some(slice.tokens).filter(|tokens| *tokens > 0.0),
                SpendMetric::CostPerMtok => slice.cost_per_mtok(),
            };
            amount.map(|amount| (slice, amount))
        })
        .collect();
    included.sort_by(|a, b| {
        b.1.total_cmp(&a.1)
            .then_with(|| a.0.kind.display_name().cmp(b.0.kind.display_name()))
    });
    let center = match metric {
        SpendMetric::Cost => included.iter().map(|(slice, _)| slice.dollars).sum(),
        SpendMetric::Tokens => included.iter().map(|(slice, _)| slice.tokens).sum(),
        SpendMetric::CostPerMtok => {
            let dollars: f64 = included.iter().map(|(slice, _)| slice.dollars).sum();
            let tokens: f64 = included.iter().map(|(slice, _)| slice.tokens).sum();
            if tokens > 0.0 {
                dollars / tokens * 1_000_000.0
            } else {
                0.0
            }
        }
    };
    Projection {
        metric,
        slices: included
            .into_iter()
            .map(|(slice, amount)| (slice.kind, amount))
            .collect(),
        center,
    }
}

/// The legend's figure: exact cents for dollars ("$7,121.12"), abbreviated tokens ("214.4M").
pub fn legend_text(value: f64, metric: SpendMetric) -> String {
    match metric {
        SpendMetric::Cost => full_dollars(value),
        SpendMetric::CostPerMtok => format!("{}/MTok", full_dollars(value)),
        SpendMetric::Tokens => format_number(value, MetricKind::Count, FormatStyle::Row),
    }
}

/// The ring's two-line center: a short figure over a quiet unit ("$13.4K" over "dollars").
pub fn ring_center(value: f64, metric: SpendMetric) -> (String, &'static str) {
    match metric {
        SpendMetric::Cost => (
            format_number(value, MetricKind::Dollars, FormatStyle::Tray),
            "dollars",
        ),
        SpendMetric::CostPerMtok => {
            let figure = if value.abs() >= 1000.0 {
                format_number(value, MetricKind::Dollars, FormatStyle::Tray)
            } else {
                full_dollars(value)
            };
            (figure, "MTok")
        }
        SpendMetric::Tokens => {
            let magnitude = value.abs();
            let (scaled, unit) = if magnitude >= 1e9 {
                (value / 1e9, "billion")
            } else if magnitude >= 1e6 {
                (value / 1e6, "million")
            } else if magnitude >= 1e3 {
                (value / 1e3, "thousand")
            } else {
                (value, "tokens")
            };
            (
                format_number(scaled, MetricKind::Count, FormatStyle::Row),
                unit,
            )
        }
    }
}

fn full_dollars(value: f64) -> String {
    let cents = (value.abs() * 100.0).round() as u64;
    let whole = (cents / 100).to_string();
    let mut grouped = String::new();
    for (index, digit) in whole.chars().enumerate() {
        if index > 0 && (whole.len() - index) % 3 == 0 {
            grouped.push(',');
        }
        grouped.push(digit);
    }
    let sign = if value < 0.0 && cents > 0 { "-" } else { "" };
    format!("{sign}${grouped}.{:02}", cents % 100)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metric::MetricValue;
    use chrono::Utc;

    fn history(account: &Account, rows: &[(&str, f64, f64)]) -> ProviderSnapshot {
        ProviderSnapshot {
            provider_id: account.card_id(),
            display_name: account.kind.display_name().into(),
            plan: None,
            lines: rows
                .iter()
                .map(|(label, dollars, tokens)| {
                    MetricLine::values(
                        *label,
                        vec![
                            MetricValue::new(*dollars, MetricKind::Dollars),
                            MetricValue::labeled(*tokens, MetricKind::Count, "tokens"),
                        ],
                    )
                })
                .collect(),
            breakdowns: Default::default(),
            notice: None,
            refreshed_at: Utc::now(),
        }
    }

    #[test]
    fn sums_each_providers_accounts_and_skips_hidden_rows_and_idle_providers() {
        let work = Account::new(ProviderKind::Cursor, "Work", None);
        let home = Account::new(ProviderKind::Cursor, "Home", None);
        let codex = Account::new(ProviderKind::Codex, "Jay", None);
        let claude = Account::new(ProviderKind::Claude, "Claude", None);
        let snapshots = BTreeMap::from([
            (work.card_id(), history(&work, &[("Today", 10.0, 1000.0)])),
            (home.card_id(), history(&home, &[("Today", 2.5, 500.0)])),
            (codex.card_id(), history(&codex, &[("Today", 0.0, 0.0)])),
        ]);
        let accounts = vec![claude.clone(), work.clone(), home.clone(), codex.clone()];
        let mut layout = LayoutState::default();
        let slices = total(SpendPeriod::Today, &accounts, &snapshots, &layout);
        assert_eq!(
            slices,
            vec![
                SpendSlice {
                    kind: ProviderKind::Cursor,
                    dollars: 12.5,
                    tokens: 1500.0
                },
                SpendSlice {
                    kind: ProviderKind::Codex,
                    dollars: 0.0,
                    tokens: 0.0
                },
            ]
        );
        assert!(project(&slices, SpendMetric::Cost).slices.len() == 1);
        layout.set_hidden(&format!("{}.today", home.card_id()), true);
        let slices = total(SpendPeriod::Today, &accounts, &snapshots, &layout);
        assert_eq!(slices[0].dollars, 10.0);
        assert!(total(SpendPeriod::Yesterday, &accounts, &snapshots, &layout).is_empty());
        assert!(tracks_spend(&claude));
        assert!(tracks_spend(&codex));
    }

    #[test]
    fn a_projection_ranks_largest_first_and_drops_slices_without_the_metric() {
        let slices = vec![
            SpendSlice {
                kind: ProviderKind::Codex,
                dollars: 1.0,
                tokens: 4_000_000.0,
            },
            SpendSlice {
                kind: ProviderKind::Cursor,
                dollars: 0.0,
                tokens: 9_000_000.0,
            },
            SpendSlice {
                kind: ProviderKind::Ollama,
                dollars: 3.0,
                tokens: 1_000_000.0,
            },
        ];
        let cost = project(&slices, SpendMetric::Cost);
        assert_eq!(
            cost.slices,
            vec![(ProviderKind::Ollama, 3.0), (ProviderKind::Codex, 1.0)]
        );
        assert_eq!(cost.center, 4.0);
        let tokens = project(&slices, SpendMetric::Tokens);
        assert_eq!(tokens.slices[0], (ProviderKind::Cursor, 9_000_000.0));
        assert_eq!(tokens.center, 14_000_000.0);
        let rate = project(&slices, SpendMetric::CostPerMtok);
        assert_eq!(rate.slices.len(), 2);
        assert!((rate.center - 0.8).abs() < 1e-9);
    }

    #[test]
    fn figures_read_like_the_mac_card() {
        assert_eq!(legend_text(7121.123, SpendMetric::Cost), "$7,121.12");
        assert_eq!(legend_text(0.5, SpendMetric::Cost), "$0.50");
        assert_eq!(legend_text(214_415_571.0, SpendMetric::Tokens), "214.4M");
        assert_eq!(legend_text(1.369, SpendMetric::CostPerMtok), "$1.37/MTok");
        assert_eq!(
            ring_center(13_369.24, SpendMetric::Cost),
            ("$13.4K".into(), "dollars")
        );
        assert_eq!(
            ring_center(461_800_000.0, SpendMetric::Tokens),
            ("461.8".into(), "million")
        );
        assert_eq!(
            ring_center(1.369, SpendMetric::CostPerMtok),
            ("$1.37".into(), "MTok")
        );
        assert_eq!(SpendPeriod::from_raw("Last 30 Days"), Some(SpendPeriod::Last30));
        assert_eq!(SpendMetric::from_raw("apiSpend"), Some(SpendMetric::Cost));
    }
}
