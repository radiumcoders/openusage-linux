//! Port of `CursorHistorySource.swift` and the in-memory `UsageLedger` it writes.
//! Today, yesterday, and the last 30 days become value lines: dollars, then a token count.
//! Up to four days are read at once, and the sweep stops after 20 seconds or the first rate limit.

use std::collections::{BTreeMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::thread;
use std::time::{Duration, Instant};

use chrono::{DateTime, Duration as ChronoDuration, Local, NaiveDate, NaiveTime, TimeZone, Utc};
use serde::{Deserialize, Serialize};

use crate::http::HttpClient;
use crate::log;
use crate::metric::{
    BreakdownModel, MetricKind, MetricLine, MetricValue, ProviderUsage, UsageBreakdown,
};
use crate::provider::FetchContext;

use super::client;
use super::events::{self, Day};

const BUDGET: Duration = Duration::from_secs(20);
const PAGE_SIZE: i64 = 1000;
const WORKERS: usize = 4;
const LEDGER_KEY: &str = "cursor_history_ledger";

#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoredModel {
    spend: f64,
    tokens: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoredDay {
    spend: f64,
    tokens: f64,
    #[serde(default)]
    models: BTreeMap<String, StoredModel>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Ledger {
    #[serde(default)]
    days: BTreeMap<i64, StoredDay>,
    #[serde(default)]
    partial: Option<i64>,
    #[serde(default)]
    resume_at: Option<DateTime<Utc>>,
}

impl Ledger {
    /// Today and yesterday are always due. An older day is due until one read of it after it ended.
    fn due(&self, span: &[i64]) -> Vec<i64> {
        let mut due: Vec<i64> = span.iter().take(2).copied().collect();
        due.extend(
            span.iter()
                .skip(2)
                .copied()
                .filter(|day| !self.days.contains_key(day) || self.partial == Some(*day)),
        );
        due
    }

    /// Days outside the span are dropped. A due day that did not come back is unread.
    fn record(&mut self, read: &BTreeMap<i64, StoredDay>, due: &[i64], span: &[i64]) {
        let due_set: std::collections::HashSet<i64> = due.iter().copied().collect();
        let span_set: std::collections::HashSet<i64> = span.iter().copied().collect();
        self.days
            .retain(|day, _| span_set.contains(day) && !due_set.contains(day));
        for (day, totals) in read {
            if span_set.contains(day) {
                self.days.insert(*day, totals.clone());
            }
        }
        self.partial = span.first().copied();
    }

    fn is_held(&self, now: DateTime<Utc>) -> bool {
        self.resume_at.is_some_and(|resume| now < resume)
    }

    fn hold(&mut self, now: DateTime<Utc>, seconds: f64) {
        let millis = (seconds * 1000.0) as i64;
        self.resume_at = now.checked_add_signed(ChronoDuration::milliseconds(millis.max(0)));
    }
}

pub(crate) fn append_lines(
    ctx: &mut FetchContext<'_>,
    access_token: &str,
    usage: &mut ProviderUsage,
) {
    let now = ctx.now;
    let span = day_starts(30, now);
    let keys: Vec<i64> = span.iter().map(|day| day.timestamp()).collect();
    let mut ledger = ctx.state.get::<Ledger>(LEDGER_KEY).unwrap_or_default();
    let due = ledger.due(&keys);
    if ledger.is_held(now) {
        ledger.record(&BTreeMap::new(), &due, &keys);
    } else {
        let due_keys: std::collections::HashSet<i64> = due.iter().copied().collect();
        let due_days: Vec<DateTime<Utc>> = span
            .iter()
            .copied()
            .filter(|day| due_keys.contains(&day.timestamp()))
            .collect();
        let read = read_days(
            ctx.http,
            access_token,
            &due_days,
            now,
            PAGE_SIZE,
            BUDGET,
            &ctx.account.card_id(),
        );
        let stored = read
            .days
            .into_iter()
            .map(|(day, totals)| {
                (
                    day.timestamp(),
                    StoredDay {
                        spend: totals.spend,
                        tokens: totals.tokens,
                        models: totals
                            .by_model
                            .iter()
                            .map(|(name, model)| {
                                (
                                    name.clone(),
                                    StoredModel {
                                        spend: model.spend,
                                        tokens: model.tokens,
                                    },
                                )
                            })
                            .collect(),
                    },
                )
            })
            .collect();
        ledger.record(&stored, &due, &keys);
        if let Some(wait) = read.rate_limited_for {
            ledger.hold(now, wait);
        }
    }
    ctx.state.set(LEDGER_KEY, &ledger);
    for (label, indexes) in [
        ("Today", 0..1),
        ("Yesterday", 1..2),
        ("Last 30 Days", 0..30),
    ] {
        if let Some((spend, tokens)) = sum(&keys, indexes.clone(), &ledger.days) {
            usage.lines.push(MetricLine::values(
                label,
                vec![
                    MetricValue::new(spend, MetricKind::Dollars),
                    MetricValue::labeled(tokens, MetricKind::Count, "tokens"),
                ],
            ));
            if let Some(breakdown) =
                UsageBreakdown::ranked(window_models(&keys, indexes, &ledger.days))
            {
                usage
                    .breakdowns
                    .insert(history_key(label).to_string(), breakdown);
            }
        }
    }
}

fn history_key(label: &str) -> &'static str {
    match label {
        "Today" => "today",
        "Yesterday" => "yesterday",
        _ => "last30",
    }
}

fn window_models(
    span: &[i64],
    indexes: std::ops::Range<usize>,
    days: &BTreeMap<i64, StoredDay>,
) -> Vec<BreakdownModel> {
    let mut models: BTreeMap<String, (f64, f64)> = BTreeMap::new();
    for index in indexes {
        let Some(day) = span.get(index).and_then(|key| days.get(key)) else {
            continue;
        };
        for (name, model) in &day.models {
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

fn sum(
    span: &[i64],
    indexes: std::ops::Range<usize>,
    days: &BTreeMap<i64, StoredDay>,
) -> Option<(f64, f64)> {
    let mut found = false;
    let mut spend = 0.0;
    let mut tokens = 0.0;
    for index in indexes {
        if let Some(day) = span.get(index).and_then(|key| days.get(key)) {
            found = true;
            spend += day.spend;
            tokens += day.tokens;
        }
    }
    found.then_some((spend, tokens))
}

struct Read {
    days: BTreeMap<DateTime<Utc>, Day>,
    rate_limited_for: Option<f64>,
}

fn read_days(
    http: &dyn HttpClient,
    access_token: &str,
    days: &[DateTime<Utc>],
    as_of: DateTime<Utc>,
    page_size: i64,
    budget: Duration,
    account_id: &str,
) -> Read {
    let queue = Mutex::new(VecDeque::from_iter(days.iter().copied()));
    let stop = AtomicBool::new(false);
    let deadline = Instant::now() + budget;
    let out = Mutex::new(Read {
        days: BTreeMap::new(),
        rate_limited_for: None,
    });
    let failures = Mutex::new(Vec::new());
    let deadline_hit = AtomicBool::new(false);
    let workers = WORKERS.min(days.len().max(1));
    thread::scope(|scope| {
        for _ in 0..workers {
            scope.spawn(|| loop {
                if stop.load(Ordering::SeqCst) {
                    break;
                }
                if Instant::now() >= deadline {
                    deadline_hit.store(true, Ordering::SeqCst);
                    break;
                }
                let day = queue.lock().expect("history queue").pop_front();
                let Some(day) = day else { break };
                match read_day(http, access_token, day, as_of, page_size) {
                    DayResult::Read(totals) => {
                        out.lock().expect("history days").days.insert(day, totals);
                    }
                    DayResult::Failed(reason) => {
                        failures.lock().expect("history failures").push(reason)
                    }
                    DayResult::RateLimited(wait) => {
                        stop.store(true, Ordering::SeqCst);
                        let mut out = out.lock().expect("history days");
                        if out.rate_limited_for.is_none() {
                            out.rate_limited_for = Some(wait.unwrap_or(3600.0));
                        }
                        break;
                    }
                }
            });
        }
    });
    let read = out
        .into_inner()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let failures = failures.into_inner().unwrap_or_default();
    if let Some(first) = failures.first() {
        log::warn(
            "plugin:cursor",
            &format!(
                "{account_id} usage history: {} of {} days failed ({first})",
                failures.len(),
                days.len()
            ),
        );
    }
    if deadline_hit.load(Ordering::SeqCst) {
        log::warn(
            "plugin:cursor",
            &format!(
                "{account_id} usage history: ran out of time with {} of {} days missing",
                days.len().saturating_sub(read.days.len()),
                days.len()
            ),
        );
    }
    if let Some(wait) = read.rate_limited_for {
        log::warn(
            "plugin:cursor",
            &format!(
                "{account_id} usage history: rate limited; next read in {}s",
                wait as i64
            ),
        );
    }
    read
}

enum DayResult {
    Read(Day),
    Failed(String),
    RateLimited(Option<f64>),
}

fn read_day(
    http: &dyn HttpClient,
    access_token: &str,
    start: DateTime<Utc>,
    as_of: DateTime<Utc>,
    page_size: i64,
) -> DayResult {
    let next = next_day_start(start);
    let end_ms = end_millis(next, as_of);
    let start_ms = start.timestamp_millis();
    let mut events = Vec::new();
    let mut expected: Option<usize> = None;
    let mut page = 1_i64;
    loop {
        let response = match client::fetch_events_page(
            http,
            access_token,
            start_ms,
            end_ms,
            page,
            page_size,
        ) {
            Ok(Some(response)) => response,
            Ok(None) => {
                return DayResult::Failed(
                    "request could not be prepared from the current session".into(),
                )
            }
            Err(_) => return DayResult::Failed("request failed".into()),
        };
        if response.status == 429 {
            return DayResult::RateLimited(response.retry_after_seconds());
        }
        if !response.is_success() {
            return DayResult::Failed(format!("HTTP {}", response.status));
        }
        let Some(parsed) = events::parse_page(&response.body) else {
            return DayResult::Failed("invalid response".into());
        };
        if parsed
            .events
            .iter()
            .any(|event| start_of_local_day(event.date).timestamp() != start.timestamp())
        {
            return DayResult::Failed("invalid response".into());
        }
        if expected.is_some_and(|total| total != parsed.total) {
            return DayResult::Failed("incomplete pages".into());
        }
        expected = Some(parsed.total);
        let total = parsed.total;
        let added = parsed.events.len();
        events.extend(parsed.events);
        if events.len() == total {
            return DayResult::Read(events::day(&events));
        }
        if events.len() > total || added == 0 {
            return DayResult::Failed("incomplete pages".into());
        }
        page += 1;
        if page > 10_000 {
            return DayResult::Failed("incomplete pages".into());
        }
    }
}

fn end_millis(next: DateTime<Utc>, as_of: DateTime<Utc>) -> i64 {
    let end = if next < as_of { next } else { as_of };
    end.timestamp_millis() - 1
}

fn day_starts(count: usize, now: DateTime<Utc>) -> Vec<DateTime<Utc>> {
    let today = now.with_timezone(&Local).date_naive();
    (0..count)
        .filter_map(|offset| shift_date(today, -(offset as i64)).map(start_of_local_date))
        .collect()
}

fn next_day_start(day_start: DateTime<Utc>) -> DateTime<Utc> {
    let date = day_start.with_timezone(&Local).date_naive();
    shift_date(date, 1)
        .map(start_of_local_date)
        .unwrap_or(day_start)
}

fn start_of_local_day(instant: DateTime<Utc>) -> DateTime<Utc> {
    start_of_local_date(instant.with_timezone(&Local).date_naive())
}

/// Midnight on that calendar date, or the first hour that exists when midnight falls in a DST gap.
fn start_of_local_date(date: NaiveDate) -> DateTime<Utc> {
    for hour in 0..24 {
        let naive = date.and_time(NaiveTime::from_hms_opt(hour, 0, 0).expect("hour"));
        if let Some(local) = Local.from_local_datetime(&naive).earliest() {
            return local.with_timezone(&Utc);
        }
    }
    date.and_hms_opt(0, 0, 0).expect("midnight").and_utc()
}

fn shift_date(date: NaiveDate, days: i64) -> Option<NaiveDate> {
    if days >= 0 {
        date.checked_add_days(chrono::Days::new(days as u64))
    } else {
        date.checked_sub_days(chrono::Days::new((-days) as u64))
    }
}

#[cfg(test)]
mod page_tests {
    use std::sync::Mutex;

    use chrono::DateTime;
    use serde_json::Value;

    use super::read_days;
    use crate::error::ProviderError;
    use crate::http::{HttpClient, HttpRequest, HttpResponse};

    #[test]
    fn a_short_page_fails_that_day_and_the_other_pages_add_up() {
        let now = DateTime::from_timestamp(1_793_000_000, 0).unwrap();
        let span = super::day_starts(30, now);
        let short = span[2].timestamp_millis();
        let http = Short {
            short,
            requests: Mutex::new(vec![]),
        };
        let read = read_days(
            &http,
            &token(),
            &span,
            now,
            2,
            std::time::Duration::from_secs(20),
            "cursor@1",
        );
        assert_eq!(read.days.len(), 29);
        assert!(!read.days.contains_key(&span[2]));
        assert!((read.days[&span[0]].spend - 2.255).abs() < 1e-9);
        assert!((read.days[&span[1]].spend - 2.255).abs() < 1e-9);
        let spend: f64 = read.days.values().map(|day| day.spend).sum();
        assert!((spend - 65.395).abs() < 1e-9);
    }

    struct Short {
        short: i64,
        requests: Mutex<Vec<HttpRequest>>,
    }

    impl HttpClient for Short {
        fn send(&self, request: HttpRequest) -> Result<HttpResponse, ProviderError> {
            self.requests.lock().unwrap().push(request.clone());
            let body: Value = serde_json::from_slice(request.body.as_deref().unwrap()).unwrap();
            let start: i64 = body["startDate"].as_str().unwrap().parse().unwrap();
            let page = body["page"].as_i64().unwrap();
            let cents: &[f64] = if start == self.short && page > 1 {
                &[]
            } else if page == 1 {
                &[100.0, 100.0]
            } else {
                &[25.5]
            };
            let events = cents.iter().map(|cents| format!(r#"{{"timestamp":"{}","kind":"USAGE_EVENT_KIND_USAGE_BASED","chargedCents":{cents}}}"#, start + 1)).collect::<Vec<_>>().join(",");
            let payload =
                format!(r#"{{"totalUsageEventsCount":3,"usageEventsDisplay":[{events}]}}"#);
            Ok(HttpResponse {
                status: 200,
                headers: Default::default(),
                body: payload.into_bytes(),
            })
        }
    }

    fn token() -> String {
        use base64::Engine;
        let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(br#"{"sub":"auth0|user_fake"}"#);
        format!("e30.{payload}.sig")
    }
}
