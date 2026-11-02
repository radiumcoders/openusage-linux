//! UTC-day token history. Port of `CodexHistorySource`, `UsageLedger`, and the token rows.
//!
//! Token counts and workspace credits have separate ledgers, so a refused credits report cannot hide
//! the counts. `FetchContext::force` forgets the hourly rest (the Mac app's user refresh passes
//! `rereadHourly: force`). A 429 hold is not cleared by that. Model breakdowns and credit dollar rows
//! are not shown: the descriptors' history rows are token counts.

use std::collections::BTreeMap;

use chrono::{DateTime, Days, Utc};
use serde::{Deserialize, Serialize};

use crate::log;
use crate::metric::{MetricKind, MetricLine, MetricValue};
use crate::provider::FetchContext;

use super::client;
use super::mapper::raw_plan_type;
use super::reports;

const REREAD_MS: i64 = 3_600_000;
const COUNTS_KEY: &str = "codex.counts";
const CREDITS_KEY: &str = "codex.credits";

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Ledger {
    #[serde(default)]
    days: BTreeMap<String, Option<f64>>,
    #[serde(default)]
    last_read_ms: Option<i64>,
    #[serde(default)]
    last_today: Option<String>,
    #[serde(default)]
    hold_until_ms: Option<i64>,
    #[serde(default)]
    partial_day: Option<String>,
}

enum ReportKind {
    Counts,
    Team,
    Enterprise,
}

pub(super) fn attach(
    ctx: &mut FetchContext,
    access_token: &str,
    account_id: Option<&str>,
    usage_body: &[u8],
    lines: &mut Vec<MetricLine>,
) {
    let mut counts: Ledger = ctx.state.get(COUNTS_KEY).unwrap_or_default();
    refresh(
        ctx,
        access_token,
        account_id,
        &mut counts,
        ReportKind::Counts,
    );
    ctx.state.set(COUNTS_KEY, &counts);
    if let Some(kind) = credits_kind(raw_plan_type(usage_body).as_deref()) {
        let mut credits: Ledger = ctx.state.get(CREDITS_KEY).unwrap_or_default();
        refresh(ctx, access_token, account_id, &mut credits, kind);
        ctx.state.set(CREDITS_KEY, &credits);
    }
    lines.extend(token_lines(&counts, ctx.now));
}

fn credits_kind(plan: Option<&str>) -> Option<ReportKind> {
    match plan? {
        "team"
        | "self_serve_business_prolite"
        | "self_serve_business_usage_based"
        | "sci"
        | "sci_plus" => Some(ReportKind::Team),
        "business"
        | "ent26"
        | "enterprise_cbp_automation"
        | "enterprise_cbp_usage_based"
        | "enterprise"
        | "edu"
        | "edu_plus"
        | "edu_pro" => Some(ReportKind::Enterprise),
        _ => None,
    }
}

fn refresh(
    ctx: &mut FetchContext,
    access_token: &str,
    account_id: Option<&str>,
    ledger: &mut Ledger,
    kind: ReportKind,
) {
    let span = span(ctx.now);
    if span.is_empty() {
        return;
    }
    if ctx.force {
        ledger.last_read_ms = None;
        ledger.last_today = None;
    }
    if ledger.is_resting(&span[0], ctx.now) {
        return;
    }
    let due = ledger.due(&span);
    if ledger.is_held(ctx.now) {
        ledger.record(&BTreeMap::new(), &due, &span);
        return;
    }
    ledger.note_read(ctx.now, &span[0]);
    let (read, wait) = read_report(ctx, access_token, account_id, &due, &kind);
    ledger.record(&read, &due, &span);
    if let Some(wait) = wait {
        ledger.hold_until_ms = Some(ctx.now.timestamp_millis() + (wait * 1000.0).round() as i64);
    }
}

fn read_report(
    ctx: &FetchContext,
    access_token: &str,
    account_id: Option<&str>,
    due: &[String],
    kind: &ReportKind,
) -> (BTreeMap<String, Option<f64>>, Option<f64>) {
    let Some((start, end)) = range(due) else {
        return (BTreeMap::new(), None);
    };
    let (path, query) = match kind {
        ReportKind::Counts => (
            "analytics/daily-workspace-usage-counts",
            vec![
                ("start_date", start),
                ("end_date", end),
                ("group_by", "day".into()),
                ("workspace_user", "true".into()),
            ],
        ),
        ReportKind::Team => (
            "usage/daily-workspace-user-token-usage-breakdown",
            vec![
                ("start_date", start),
                ("end_date", end),
                ("group_by", "day".into()),
            ],
        ),
        ReportKind::Enterprise => (
            "usage/daily-workspace-user-credit-usage",
            vec![
                ("start_date", start),
                ("end_date", end),
                ("breakdown", "model".into()),
            ],
        ),
    };
    let query: Vec<(&str, &str)> = query
        .iter()
        .map(|(key, value)| (*key, value.as_str()))
        .collect();
    let url = report_url(path, &query);
    let card = ctx.account.card_id();
    let response = match ctx
        .http
        .send(client::report_request(&url, access_token, account_id))
    {
        Ok(response) => response,
        Err(_) => {
            log::warn(
                "plugin:codex",
                &format!("{card} usage history: {path} request failed"),
            );
            return (BTreeMap::new(), None);
        }
    };
    if response.status == 429 {
        let wait = response.retry_after_seconds().unwrap_or(3600.0);
        log::warn(
            "plugin:codex",
            &format!(
                "{card} usage history: {path} rate limited; next read in {}s",
                wait.round() as i64
            ),
        );
        return (BTreeMap::new(), Some(wait));
    }
    if !response.is_success() {
        log::warn(
            "plugin:codex",
            &format!("{card} usage history: {path} HTTP {}", response.status),
        );
        return (BTreeMap::new(), None);
    }
    // A day the counts report leaves out is quiet: recorded, with no token total. A credits report
    // only needs the day marked read; its amounts are not a history row.
    let tokens = match kind {
        ReportKind::Counts => reports::counts(&response.body),
        ReportKind::Team => reports::workspace_credits(&response.body).map(|_| BTreeMap::new()),
        ReportKind::Enterprise => {
            reports::enterprise_credits(&response.body).map(|_| BTreeMap::new())
        }
    };
    let Some(tokens) = tokens else {
        log::warn(
            "plugin:codex",
            &format!("{card} usage history: {path} invalid response"),
        );
        return (BTreeMap::new(), None);
    };
    let read = due
        .iter()
        .map(|day| (day.clone(), tokens.get(day).copied().unwrap_or(None)))
        .collect();
    (read, None)
}

fn report_url(path: &str, query: &[(&str, &str)]) -> String {
    let mut url = url::Url::parse(&format!("{}{path}", client::REPORTS_URL))
        .unwrap_or_else(|_| url::Url::parse(client::REPORTS_URL).unwrap());
    {
        let mut pairs = url.query_pairs_mut();
        for (key, value) in query {
            pairs.append_pair(key, value);
        }
    }
    url.to_string()
}

fn range(due: &[String]) -> Option<(String, String)> {
    let mut dates: Vec<&str> = due.iter().map(String::as_str).collect();
    dates.sort_unstable();
    Some((dates.first()?.to_string(), dates.last()?.to_string()))
}

fn span(now: DateTime<Utc>) -> Vec<String> {
    let today = now.date_naive();
    (0..30)
        .filter_map(|offset| today.checked_sub_days(Days::new(offset)))
        .map(|day| day.format("%Y-%m-%d").to_string())
        .collect()
}

fn token_lines(ledger: &Ledger, now: DateTime<Utc>) -> Vec<MetricLine> {
    let span = span(now);
    if span.len() < 30 {
        return Vec::new();
    }
    let mut lines = Vec::new();
    push_window(&mut lines, "Today", &span[..1], ledger);
    push_window(&mut lines, "Yesterday", &span[1..2], ledger);
    push_window(&mut lines, "Last 30 Days", &span, ledger);
    lines
}

fn push_window(lines: &mut Vec<MetricLine>, label: &str, days: &[String], ledger: &Ledger) {
    let mut sum = 0.0;
    let mut any = false;
    for day in days {
        if let Some(Some(tokens)) = ledger.days.get(day) {
            sum += *tokens;
            any = true;
        }
    }
    if any {
        lines.push(MetricLine::Values {
            label: label.into(),
            values: vec![MetricValue::labeled(sum, MetricKind::Count, "tokens")],
            expiries_at: vec![],
        });
    }
}

impl Ledger {
    fn due(&self, span: &[String]) -> Vec<String> {
        span.iter()
            .enumerate()
            .filter(|(index, day)| {
                *index < 2
                    || !self.days.contains_key(*day)
                    || self.partial_day.as_deref() == Some(day.as_str())
            })
            .map(|(_, day)| day.clone())
            .collect()
    }

    fn record(&mut self, read: &BTreeMap<String, Option<f64>>, due: &[String], span: &[String]) {
        let due: Vec<&str> = due.iter().map(String::as_str).collect();
        self.days.retain(|day, _| {
            span.iter().any(|kept| kept == day) && !due.iter().any(|dropped| *dropped == day)
        });
        for (day, value) in read {
            if span.iter().any(|kept| kept == day) {
                self.days.insert(day.clone(), *value);
            }
        }
        self.partial_day = span.first().cloned();
    }

    fn is_held(&self, now: DateTime<Utc>) -> bool {
        self.hold_until_ms
            .is_some_and(|until| now.timestamp_millis() < until)
    }

    fn is_resting(&self, today: &str, now: DateTime<Utc>) -> bool {
        match (self.last_today.as_deref(), self.last_read_ms) {
            (Some(last_today), Some(at)) if last_today == today => {
                now.timestamp_millis() < at + REREAD_MS
            }
            _ => false,
        }
    }

    fn note_read(&mut self, now: DateTime<Utc>, today: &str) {
        self.last_read_ms = Some(now.timestamp_millis());
        self.last_today = Some(today.to_string());
    }
}
