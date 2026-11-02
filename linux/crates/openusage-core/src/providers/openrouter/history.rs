//! Management-key activity (`OpenRouterActivitySource`) and the dollar history rows.
//! `/api/v1/activity` ends at yesterday, so today always comes from the keys' reported spend.
//! Activity is read again after an hour, on a forced refresh, and at the new UTC day.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::mapper::{self, Spend};
use crate::http::{HttpClient, HttpRequest};
use crate::log;
use crate::metric::{BreakdownModel, MetricKind, MetricLine, MetricValue, UsageBreakdown};
use crate::provider::FetchContext;

const HISTORY: &str = "activity";
const ACTIVITY: &str = "https://openrouter.ai/api/v1/activity";
const DAY: i64 = 86_400;
const REREAD_SECONDS: i64 = 3600;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct Ledger {
    #[serde(default)]
    days: Vec<StoredDay>,
    #[serde(default)]
    last_read_ms: Option<i64>,
    #[serde(default)]
    last_today: Option<i64>,
    #[serde(default)]
    partial_day: Option<i64>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct StoredModel {
    spend: f64,
    tokens: f64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct StoredDay {
    day: i64,
    measures: BTreeMap<String, f64>,
    #[serde(default)]
    models: BTreeMap<String, StoredModel>,
}

struct ReadDay {
    measures: BTreeMap<String, f64>,
    models: BTreeMap<String, StoredModel>,
}

pub fn lines(
    ctx: &mut FetchContext,
    api_key: &str,
    spend: &Spend,
    read_activity: bool,
) -> (Vec<MetricLine>, BTreeMap<String, UsageBreakdown>) {
    let ledger = if read_activity {
        Some(refresh(ctx, api_key))
    } else {
        None
    };
    spend_lines(spend, ledger.as_ref(), ctx.now)
}

fn refresh(ctx: &mut FetchContext, api_key: &str) -> Ledger {
    let mut ledger = ctx.state.get::<Ledger>(HISTORY).unwrap_or_default();
    let today = mapper::midnight(ctx.now).timestamp();
    let span: Vec<i64> = (0..31).map(|offset| today - offset * DAY).collect();
    if resting(&ledger, today, ctx.now, ctx.force) {
        return ledger;
    }
    ledger.last_read_ms = Some(ctx.now.timestamp_millis());
    ledger.last_today = Some(today);
    let due = due_days(&ledger, &span);
    let read = read_activity(ctx.http, api_key, &due, today, &ctx.account.card_id());
    record(&mut ledger, &read, &due, &span);
    ctx.state.set(HISTORY, &ledger);
    ledger
}

fn resting(ledger: &Ledger, today: i64, now: DateTime<Utc>, force: bool) -> bool {
    if force {
        return false;
    }
    match (ledger.last_today, ledger.last_read_ms) {
        (Some(day), Some(read_ms)) if day == today => DateTime::from_timestamp_millis(read_ms)
            .is_some_and(|read_at| now < read_at + chrono::Duration::seconds(REREAD_SECONDS)),
        _ => false,
    }
}

fn due_days(ledger: &Ledger, span: &[i64]) -> Vec<i64> {
    let mut due: Vec<i64> = span.iter().take(2).copied().collect();
    for day in span.iter().skip(2).copied() {
        let present = ledger.days.iter().any(|stored| stored.day == day);
        if !present || ledger.partial_day == Some(day) {
            due.push(day);
        }
    }
    due
}

fn record(ledger: &mut Ledger, read: &BTreeMap<i64, ReadDay>, due: &[i64], span: &[i64]) {
    ledger
        .days
        .retain(|stored| span.contains(&stored.day) && !due.contains(&stored.day));
    for (day, read) in read {
        ledger.days.push(StoredDay {
            day: *day,
            measures: read.measures.clone(),
            models: read.models.clone(),
        });
    }
    ledger.partial_day = span.first().copied();
}

fn read_activity(
    http: &dyn HttpClient,
    api_key: &str,
    due: &[i64],
    today: i64,
    card: &str,
) -> BTreeMap<i64, ReadDay> {
    let tag = "plugin:openrouter";
    let past: Vec<i64> = due.iter().copied().filter(|day| *day < today).collect();
    if past.is_empty() {
        return BTreeMap::new();
    }
    let url = match past.as_slice() {
        [day] => format!("{ACTIVITY}?date={}", ymd(*day)),
        _ => ACTIVITY.to_string(),
    };
    let response = match http.send(
        HttpRequest::get(url)
            .bearer(api_key)
            .header("Accept", "application/json"),
    ) {
        Ok(response) => response,
        Err(error) => {
            log::warn(tag, &format!("{card} activity request failed: {error}"));
            return BTreeMap::new();
        }
    };
    if !response.is_success() {
        log::warn(
            tag,
            &format!("{card} activity returned HTTP {}", response.status),
        );
        return BTreeMap::new();
    }
    let Some(parsed) = mapper::activity_days(&response.body) else {
        log::warn(tag, &format!("{card} activity response was invalid"));
        return BTreeMap::new();
    };
    past.into_iter()
        .map(|day| {
            let activity = parsed.get(&day);
            (
                day,
                ReadDay {
                    measures: activity
                        .map(|day| day.totals.clone())
                        .unwrap_or_else(mapper::quiet),
                    models: activity.map(stored_models).unwrap_or_default(),
                },
            )
        })
        .collect()
}

fn stored_models(day: &mapper::ActivityDay) -> BTreeMap<String, StoredModel> {
    day.by_model
        .iter()
        .map(|(name, measures)| {
            (
                name.clone(),
                StoredModel {
                    spend: measures.get("spend").copied().unwrap_or(0.0),
                    tokens: measures.get("tokens").copied().unwrap_or(0.0),
                },
            )
        })
        .collect()
}

fn spend_lines(
    spend: &Spend,
    ledger: Option<&Ledger>,
    now: DateTime<Utc>,
) -> (Vec<MetricLine>, BTreeMap<String, UsageBreakdown>) {
    let today = mapper::midnight(now).timestamp();
    let windows = [
        (
            "Today",
            "today",
            merge(window(ledger, today, 0, 0), Some(spend.today.spend)),
            model_window(ledger, today, 0, 0),
        ),
        (
            "Yesterday",
            "yesterday",
            merge(window(ledger, today, 1, 1), None),
            model_days(ledger, (1..=1).map(|offset| today - offset * DAY)),
        ),
        (
            "This Week",
            "week",
            merge(
                interval(ledger, mapper::reset_window("weekly", now)),
                Some(spend.week.spend),
            ),
            model_interval(ledger, mapper::reset_window("weekly", now)),
        ),
        (
            "This Month",
            "month",
            merge(
                interval(ledger, mapper::reset_window("monthly", now)),
                Some(spend.month.spend),
            ),
            model_interval(ledger, mapper::reset_window("monthly", now)),
        ),
        (
            "Prior 30 Days",
            "prior30",
            merge(window(ledger, today, 1, 30), None),
            model_days(ledger, (1..=30).map(|offset| today - offset * DAY)),
        ),
    ];
    let mut lines = Vec::new();
    let mut breakdowns = BTreeMap::new();
    for (label, key, totals, models) in windows {
        let Some(number) = totals.and_then(|totals| totals.get("spend").copied()) else {
            continue;
        };
        lines.push(MetricLine::values(
            label,
            vec![MetricValue::new(number, MetricKind::Dollars)],
        ));
        if let Some(breakdown) = UsageBreakdown::ranked(models) {
            breakdowns.insert(key.to_string(), breakdown);
        }
    }
    (lines, breakdowns)
}

fn model_window(ledger: Option<&Ledger>, today: i64, first: i64, last: i64) -> Vec<BreakdownModel> {
    model_days(ledger, (first..=last).map(|offset| today - offset * DAY))
}

fn model_interval(
    ledger: Option<&Ledger>,
    bounds: Option<(DateTime<Utc>, i64)>,
) -> Vec<BreakdownModel> {
    let Some((end, period_ms)) = bounds else {
        return Vec::new();
    };
    let start = end.timestamp() - period_ms / 1000;
    let mut day = start;
    let mut days = Vec::new();
    while day < end.timestamp() {
        days.push(day);
        day += DAY;
    }
    model_days(ledger, days.into_iter())
}

fn model_days(ledger: Option<&Ledger>, days: impl Iterator<Item = i64>) -> Vec<BreakdownModel> {
    let Some(ledger) = ledger else {
        return Vec::new();
    };
    let mut models: BTreeMap<String, (f64, f64)> = BTreeMap::new();
    for day in days {
        let Some(stored) = ledger.days.iter().find(|stored| stored.day == day) else {
            continue;
        };
        for (name, model) in &stored.models {
            let slot = models.entry(name.clone()).or_insert((0.0, 0.0));
            slot.0 += model.spend;
            slot.1 += model.tokens;
        }
    }
    models
        .into_iter()
        .map(|(name, (spend, tokens))| BreakdownModel {
            name,
            dollars: Some(spend),
            tokens: Some(tokens),
            share: None,
        })
        .collect()
}

/// A window with no reported figure appears when at least one of its days was read. A missing day is
/// left out of the sum, the same way `UsageHistory.totals` skips a day the ledger does not hold.
fn merge(
    summed: Option<BTreeMap<String, f64>>,
    reported: Option<f64>,
) -> Option<BTreeMap<String, f64>> {
    match reported {
        Some(spend) => {
            let mut values = summed.unwrap_or_default();
            values.insert("spend".into(), spend);
            Some(values)
        }
        None => summed,
    }
}

fn window(
    ledger: Option<&Ledger>,
    today: i64,
    first: i64,
    last: i64,
) -> Option<BTreeMap<String, f64>> {
    let days = (first..=last).map(|offset| today - offset * DAY);
    sum_days(ledger?, days)
}

fn interval(
    ledger: Option<&Ledger>,
    bounds: Option<(DateTime<Utc>, i64)>,
) -> Option<BTreeMap<String, f64>> {
    let (end, period_ms) = bounds?;
    let start = end.timestamp() - period_ms / 1000;
    let mut day = start;
    let mut days = Vec::new();
    while day < end.timestamp() {
        days.push(day);
        day += DAY;
    }
    sum_days(ledger?, days.into_iter())
}

fn sum_days(ledger: &Ledger, days: impl Iterator<Item = i64>) -> Option<BTreeMap<String, f64>> {
    let mut total: Option<BTreeMap<String, f64>> = None;
    for day in days {
        let Some(stored) = ledger.days.iter().find(|stored| stored.day == day) else {
            continue;
        };
        let slot = total.get_or_insert_with(BTreeMap::new);
        for (key, value) in &stored.measures {
            *slot.entry(key.clone()).or_insert(0.0) += *value;
        }
    }
    total
}

fn ymd(day: i64) -> String {
    DateTime::from_timestamp(day, 0)
        .unwrap()
        .format("%Y-%m-%d")
        .to_string()
}
