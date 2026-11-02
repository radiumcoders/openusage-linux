//! The dashboard's model: each account with its rows, the Priority board, and the bar strip. Ports of
//! `PriorityBoard.swift` and `MenuBarContent.swift`, so the bar and the panel agree with the Mac app.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};

use crate::account::Account;
use crate::descriptor::{Descriptor, LimitKind};
use crate::kind::ProviderKind;
use crate::layout::LayoutState;
use crate::metric::{MetricKind, ProviderSnapshot};
use crate::widget::{DisplayMode, MeterState, Severity, WidgetData};

pub struct DashboardLine {
    pub descriptor: Descriptor,
    pub data: WidgetData,
}

pub struct DashboardAccount {
    pub account: Account,
    pub plan: Option<String>,
    /// Lines with data that the layout has not hidden, in the user's metric order.
    pub lines: Vec<DashboardLine>,
    /// The refresh error or notice, or a staleness hint.
    pub problem: Option<String>,
    pub stale: bool,
    pub waiting: bool,
}

/// Each account, in the user's order, with what its card shows.
///
/// Account order and which metrics exist come from `layout`. A line is omitted when it has no
/// reading, when a Limit Resets row has none left, or when ChatGPT credits are exactly zero.
/// Those last two stay in the limits JSON; only the dashboard, the bar, and alerts drop them.
pub fn accounts(
    accounts: &[Account],
    layout: &LayoutState,
    snapshots: &BTreeMap<String, ProviderSnapshot>,
    problems: &BTreeMap<String, String>,
    mode: DisplayMode,
    now: DateTime<Utc>,
) -> Vec<DashboardAccount> {
    layout
        .ordered_accounts(accounts)
        .iter()
        .map(|account| {
            let id = account.card_id();
            let snapshot = snapshots.get(&id);
            let lines = layout
                .visible_descriptors(account)
                .into_iter()
                .filter_map(|descriptor| {
                    let data = WidgetData::resolve(&descriptor, snapshot, mode);
                    line_visible(&descriptor, &data).then_some(DashboardLine { descriptor, data })
                })
                .collect();
            let stale = snapshot.is_some_and(|snapshot| snapshot.is_stale(now));
            let problem = problems
                .get(&id)
                .cloned()
                .or_else(|| snapshot.and_then(|s| s.notice.clone()))
                .or_else(|| {
                    snapshot.filter(|s| s.is_stale(now)).and_then(|s| {
                        crate::widget::compact_duration((now - s.refreshed_at).num_seconds() as f64)
                            .map(|duration| format!("Last updated {duration} ago"))
                    })
                });
            DashboardAccount {
                account: account.clone(),
                plan: snapshot.and_then(|s| s.plan.clone()),
                lines,
                problem,
                stale,
                waiting: snapshot.is_none(),
            }
        })
        .collect()
}

/// `DashboardLine.isVisible`: no data is omitted, a spent reset grant is omitted, and an empty
/// ChatGPT credit balance is omitted. Any other balance, including Cursor credits at zero, stays.
fn line_visible(descriptor: &Descriptor, data: &WidgetData) -> bool {
    if !data.has_data {
        return false;
    }
    if data.shows_reset_expiries {
        let count = data
            .values
            .first()
            .map(|value| value.number.floor())
            .unwrap_or(0.0);
        return count > 0.0;
    }
    if descriptor.kind == ProviderKind::Codex && descriptor.key == "credits" {
        return balance_number(descriptor, data) != Some(0.0);
    }
    true
}

fn balance_number(descriptor: &Descriptor, data: &WidgetData) -> Option<f64> {
    if descriptor.shows_reset_expiries() || !data.has_data || data.limit.is_some() {
        return None;
    }
    let export = descriptor
        .export
        .as_ref()
        .filter(|export| export.kind == LimitKind::Balance)?;
    export.value(&data.values).map(|value| value.number)
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Reason {
    Low {
        severity: Severity,
        remaining: f64,
        runway: Option<f64>,
    },
    UsedUp {
        resets_at: Option<DateTime<Utc>>,
    },
    EmptyBalance,
}

impl Reason {
    fn new(line: &DashboardLine, now: DateTime<Utc>) -> Option<Reason> {
        let data = &line.data;
        let balance =
            (!line.descriptor.shows_reset_expiries() && data.has_data && data.limit.is_none())
                .then(|| {
                    line.descriptor
                        .export
                        .as_ref()
                        .filter(|export| export.kind == crate::descriptor::LimitKind::Balance)
                })
                .flatten()
                .and_then(|export| export.value(&data.values));
        if balance.is_some_and(|value| value.number == 0.0) {
            return Some(Reason::EmptyBalance);
        }
        if !data.has_data || !data.is_bounded() {
            return None;
        }
        if data.meter_state(now) == MeterState::Spent {
            return Some(Reason::UsedUp {
                resets_at: data.resets_at,
            });
        }
        let severity = data.severity();
        (severity != Severity::Normal).then(|| Reason::Low {
            severity,
            remaining: data.remaining_fraction(),
            runway: data.runway(now),
        })
    }

    pub fn severity(&self) -> Severity {
        match self {
            Reason::Low { severity, .. } => *severity,
            Reason::UsedUp { .. } | Reason::EmptyBalance => Severity::Critical,
        }
    }

    pub fn is_used_up(&self) -> bool {
        !matches!(self, Reason::Low { .. })
    }

    fn rank(&self) -> (u8, f64) {
        match self {
            Reason::Low {
                runway: Some(runway),
                ..
            } => (0, *runway),
            Reason::Low {
                remaining,
                runway: None,
                ..
            } => (1, *remaining),
            Reason::UsedUp { resets_at } => (
                2,
                resets_at.map_or(f64::INFINITY, |date| date.timestamp() as f64),
            ),
            Reason::EmptyBalance => (2, f64::INFINITY),
        }
    }
}

pub struct PriorityItem<'a> {
    pub account: &'a DashboardAccount,
    pub line: &'a DashboardLine,
    pub reason: Reason,
}

/// The limits that need the user, most urgent first; `accounts` order breaks ties.
pub struct PriorityBoard<'a> {
    pub attention: Vec<PriorityItem<'a>>,
    pub used_up: Vec<PriorityItem<'a>>,
}

impl<'a> PriorityBoard<'a> {
    pub fn make(accounts: &'a [DashboardAccount], now: DateTime<Utc>) -> Self {
        let mut items: Vec<(usize, PriorityItem<'a>)> = accounts
            .iter()
            .flat_map(|account| account.lines.iter().map(move |line| (account, line)))
            .filter_map(|(account, line)| {
                Reason::new(line, now).map(|reason| PriorityItem {
                    account,
                    line,
                    reason,
                })
            })
            .enumerate()
            .collect();
        items.sort_by(|(left_index, left), (right_index, right)| {
            let (left_tier, left_close) = left.reason.rank();
            let (right_tier, right_close) = right.reason.rank();
            (left_tier, left_close, left_index)
                .partial_cmp(&(right_tier, right_close, right_index))
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        let (used_up, attention): (Vec<_>, Vec<_>) = items
            .into_iter()
            .map(|(_, item)| item)
            .partition(|item| item.reason.is_used_up());
        PriorityBoard { attention, used_up }
    }
}

/// One item of the bar strip: "Claude Session 18%".
#[derive(Debug, Clone, PartialEq)]
pub struct StripItem {
    pub provider: String,
    pub badge: Option<String>,
    pub label: String,
    pub value: String,
    pub severity: Severity,
}

const GENERIC_WORDS: [&str; 3] = ["models", "bot", "usage"];

/// "Cursor Models" → "Cursor"; one-word labels stay.
pub fn short_label(label: &str) -> String {
    let words: Vec<&str> = label.split(' ').collect();
    match words.last() {
        Some(last) if words.len() > 1 && GENERIC_WORDS.contains(&last.to_lowercase().as_str()) => {
            words[..words.len() - 1].join(" ")
        }
        _ => label.to_string(),
    }
}

/// Initials marking accounts of a provider with several accounts.
pub fn badges(accounts: &[Account]) -> BTreeMap<String, String> {
    let mut counts: BTreeMap<_, usize> = BTreeMap::new();
    for account in accounts {
        *counts.entry(account.kind).or_default() += 1;
    }
    accounts
        .iter()
        .filter(|account| counts[&account.kind] > 1)
        .map(|account| {
            let initials: String = account
                .label
                .split(|c: char| !c.is_alphanumeric())
                .filter(|word| !word.is_empty())
                .take(2)
                .filter_map(|word| word.chars().next())
                .flat_map(char::to_uppercase)
                .collect();
            (
                account.card_id(),
                if initials.is_empty() {
                    "?".into()
                } else {
                    initials
                },
            )
        })
        .collect()
}

/// The bar's strip: up to `limit` of the most urgent limits, and how many more there are.
pub fn strip(
    board: &PriorityBoard,
    limit: usize,
    badges: &BTreeMap<String, String>,
) -> (Vec<StripItem>, usize) {
    let shown: Vec<StripItem> = board
        .attention
        .iter()
        .take(limit)
        .map(|item| StripItem {
            provider: item.account.account.kind.display_name().to_string(),
            badge: badges.get(&item.account.account.card_id()).cloned(),
            label: short_label(&item.line.data.title),
            value: item.line.data.menu_bar_value(),
            severity: item.reason.severity(),
        })
        .collect();
    let overflow = board.attention.len() - shown.len();
    (shown, overflow)
}

/// The user's "how many limits the bar shows" setting, stored under the Mac key `menuBarLimit`.
pub fn menu_bar_limit(raw: Option<&str>) -> usize {
    match raw {
        Some("iconOnly") => 0,
        Some("upToOne") => 1,
        Some("upToTwo") => 2,
        Some("upToFour") => 4,
        Some("upToFive") => 5,
        _ => 3,
    }
}

/// Whether a line is money (for totals).
pub fn is_dollars(line: &DashboardLine) -> bool {
    line.data.kind == MetricKind::Dollars
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metric::{MetricLine, MetricValue};

    fn account(kind: ProviderKind) -> Account {
        Account::new(kind, kind.display_name(), None)
    }

    fn snapshot(account: &Account, lines: Vec<MetricLine>) -> ProviderSnapshot {
        ProviderSnapshot {
            breakdowns: Default::default(),
            provider_id: account.card_id(),
            display_name: account.kind.display_name().into(),
            plan: None,
            lines,
            notice: None,
            refreshed_at: Utc::now(),
        }
    }

    fn line_keys(accounts: &[DashboardAccount]) -> Vec<Vec<&str>> {
        accounts
            .iter()
            .map(|account| {
                account
                    .lines
                    .iter()
                    .map(|line| line.descriptor.key)
                    .collect()
            })
            .collect()
    }

    #[test]
    fn accounts_follow_the_layout_and_omit_hidden_metrics_and_rows_without_data() {
        let first = account(ProviderKind::Claude);
        let second = account(ProviderKind::Claude);
        let codex = account(ProviderKind::Codex);
        let stored = vec![first.clone(), codex.clone(), second.clone()];
        let mut layout = LayoutState::default();
        assert!(layout.move_provider(&stored, &codex.card_id(), &first.card_id()));
        assert!(layout.set_hidden(&format!("{}.weekly", first.card_id()), true));
        let mut snapshots = BTreeMap::new();
        snapshots.insert(
            first.card_id(),
            snapshot(
                &first,
                vec![
                    MetricLine::percent("Session", 10.0, None, None),
                    MetricLine::percent("Weekly", 90.0, None, None),
                ],
            ),
        );
        snapshots.insert(
            second.card_id(),
            snapshot(&second, vec![MetricLine::percent("Fable", 5.0, None, None)]),
        );
        let view = accounts(
            &stored,
            &layout,
            &snapshots,
            &BTreeMap::new(),
            DisplayMode::Remaining,
            Utc::now(),
        );
        assert_eq!(
            view.iter()
                .map(|item| item.account.card_id())
                .collect::<Vec<_>>(),
            vec![codex.card_id(), first.card_id(), second.card_id()]
        );
        assert_eq!(
            line_keys(&view),
            vec![vec![], vec!["session"], vec!["fable"]]
        );
    }

    #[test]
    fn the_limit_resets_row_shows_only_while_a_reset_is_available() {
        let codex = account(ProviderKind::Codex);
        let layout = LayoutState::default();
        let view = |count: f64| {
            let mut snapshots = BTreeMap::new();
            snapshots.insert(
                codex.card_id(),
                snapshot(
                    &codex,
                    vec![
                        MetricLine::percent("Weekly", 10.0, None, None),
                        MetricLine::values(
                            "Rate Limit Resets",
                            vec![MetricValue::labeled(count, MetricKind::Count, "available")],
                        ),
                    ],
                ),
            );
            accounts(
                std::slice::from_ref(&codex),
                &layout,
                &snapshots,
                &BTreeMap::new(),
                DisplayMode::Remaining,
                Utc::now(),
            )
        };
        assert_eq!(
            line_keys(&view(2.0)),
            vec![vec!["weekly", "rateLimitResets"]]
        );
        assert_eq!(line_keys(&view(0.0)), vec![vec!["weekly"]]);
    }

    #[test]
    fn an_empty_chatgpt_credit_balance_stays_off_the_dashboard() {
        let codex = account(ProviderKind::Codex);
        let layout = LayoutState::default();
        let view = |credits: f64| {
            let mut snapshots = BTreeMap::new();
            snapshots.insert(
                codex.card_id(),
                snapshot(
                    &codex,
                    vec![
                        MetricLine::percent("Weekly", 10.0, None, None),
                        MetricLine::values(
                            "Credits",
                            vec![MetricValue::labeled(credits, MetricKind::Count, "credits")],
                        ),
                    ],
                ),
            );
            accounts(
                std::slice::from_ref(&codex),
                &layout,
                &snapshots,
                &BTreeMap::new(),
                DisplayMode::Remaining,
                Utc::now(),
            )
        };
        assert_eq!(line_keys(&view(0.0)), vec![vec!["weekly"]]);
        assert_eq!(line_keys(&view(12.0)), vec![vec!["weekly", "credits"]]);
    }
}
