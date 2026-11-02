//! Port of `OllamaHistorySource` plus the history rows it feeds: Today, Yesterday, Last 30 Days, Requests.
//! A failed history read hides those rows and never fails the card. Today and yesterday are read again
//! after an hour, or immediately on a forced refresh or a new UTC day; older days stay once read.

use std::collections::BTreeMap;

use chrono::{DateTime, TimeZone, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::http::{HttpClient, HttpRequest};
use crate::log;
use crate::metric::{MetricKind, MetricLine, MetricValue};
use crate::provider::FetchContext;

const HISTORY: &str = "history";
const HOST: &str = "https://ollama.com";
const DAY: i64 = 86_400;
const REREAD_SECONDS: i64 = 3600;

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
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

#[derive(Clone, Debug, Serialize, Deserialize)]
struct StoredDay {
    day: i64,
    measures: BTreeMap<String, f64>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ParsedDay {
    pub totals: BTreeMap<String, f64>,
    pub by_model: Option<BTreeMap<String, BTreeMap<String, f64>>>,
}

pub fn append(ctx: &mut FetchContext, api_key: &str, lines: &mut Vec<MetricLine>) {
    let ledger = refresh(ctx, api_key);
    lines.extend(history_lines(&ledger, ctx.now));
}

fn refresh(ctx: &mut FetchContext, api_key: &str) -> Ledger {
    let mut ledger = ctx.state.get::<Ledger>(HISTORY).unwrap_or_default();
    let today = midnight(ctx.now).timestamp();
    let span: Vec<i64> = (0..30).map(|offset| today - offset * DAY).collect();
    if resting(&ledger, today, ctx.now, ctx.force) {
        return ledger;
    }
    ledger.last_read_ms = Some(ctx.now.timestamp_millis());
    ledger.last_today = Some(today);
    let due = due_days(&ledger, &span);
    let read = read_usage(ctx.http, api_key, &due, today, &ctx.account.card_id());
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

fn record(
    ledger: &mut Ledger,
    read: &BTreeMap<i64, BTreeMap<String, f64>>,
    due: &[i64],
    span: &[i64],
) {
    ledger
        .days
        .retain(|stored| span.contains(&stored.day) && !due.contains(&stored.day));
    for (day, measures) in read {
        ledger.days.push(StoredDay {
            day: *day,
            measures: measures.clone(),
        });
    }
    ledger.partial_day = span.first().copied();
}

fn read_usage(
    http: &dyn HttpClient,
    api_key: &str,
    due: &[i64],
    today: i64,
    card: &str,
) -> BTreeMap<i64, BTreeMap<String, f64>> {
    let tag = "plugin:ollama";
    if due.is_empty() {
        return BTreeMap::new();
    }
    let week_ago = today - 7 * DAY;
    let range = if due.iter().all(|day| *day >= week_ago) {
        "7d"
    } else {
        "30d"
    };
    let url = format!("{HOST}/api/usage?range={range}&group_by=model");
    let response = match http.send(
        HttpRequest::get(url)
            .bearer(api_key)
            .header("Accept", "application/json"),
    ) {
        Ok(response) => response,
        Err(_) => {
            log::warn(tag, &format!("{card} usage history: request failed"));
            return BTreeMap::new();
        }
    };
    if !response.is_success() {
        log::warn(
            tag,
            &format!("{card} usage history: HTTP {}", response.status),
        );
        return BTreeMap::new();
    }
    let Some(parsed) = days(&response.body) else {
        log::warn(tag, &format!("{card} usage history: invalid response"));
        return BTreeMap::new();
    };
    due.iter()
        .map(|day| {
            (
                *day,
                parsed
                    .get(day)
                    .map(|parsed| parsed.totals.clone())
                    .unwrap_or_default(),
            )
        })
        .collect()
}

fn history_lines(ledger: &Ledger, now: DateTime<Utc>) -> Vec<MetricLine> {
    let today = midnight(now).timestamp();
    let mut lines = Vec::new();
    if let Some(line) = window_line("Today", sum(ledger, today, 0, 0)) {
        lines.push(line);
    }
    if let Some(line) = window_line("Yesterday", sum(ledger, today, 1, 1)) {
        lines.push(line);
    }
    if let Some(line) = window_line("Last 30 Days", sum(ledger, today, 0, 29)) {
        lines.push(line);
    }
    if let Some(line) = requests_line(sum(ledger, today, 0, 29)) {
        lines.push(line);
    }
    lines
}

fn sum(ledger: &Ledger, today: i64, first: i64, last: i64) -> Option<BTreeMap<String, f64>> {
    let present: Vec<&BTreeMap<String, f64>> = (first..=last)
        .filter_map(|offset| {
            let day = today - offset * DAY;
            ledger
                .days
                .iter()
                .find(|stored| stored.day == day)
                .map(|stored| &stored.measures)
        })
        .collect();
    let (first, rest) = present.split_first()?;
    let mut total = (*first).clone();
    for measures in rest {
        for (key, value) in *measures {
            *total.entry(key.clone()).or_insert(0.0) += *value;
        }
    }
    Some(total)
}

fn window_line(label: &str, totals: Option<BTreeMap<String, f64>>) -> Option<MetricLine> {
    let totals = totals?;
    let mut values = Vec::new();
    if let Some(dollars) = totals.get("usage") {
        values.push(MetricValue::new(*dollars, MetricKind::Dollars));
    }
    if let Some(tokens) = totals.get("tokens") {
        values.push(MetricValue::labeled(*tokens, MetricKind::Count, "tokens"));
    }
    (!values.is_empty()).then(|| MetricLine::values(label, values))
}

fn requests_line(totals: Option<BTreeMap<String, f64>>) -> Option<MetricLine> {
    let requests = totals?.get("requests").copied()?;
    Some(MetricLine::values(
        "Requests",
        vec![MetricValue::labeled(
            requests,
            MetricKind::Count,
            "in 30 days",
        )],
    ))
}

/// Every bucket keyed by its UTC day, or `None` when any bucket is malformed.
pub fn days(body: &[u8]) -> Option<BTreeMap<i64, ParsedDay>> {
    let root: Value = serde_json::from_slice(body).ok()?;
    let root = root.as_object()?;
    if root.get("granularity").and_then(Value::as_str) != Some("day") {
        return None;
    }
    let buckets = root.get("buckets")?.as_array()?;
    let mut days = BTreeMap::new();
    for raw in buckets {
        let bucket = raw.as_object()?;
        let (day, parsed) = parse_bucket(bucket)?;
        if days.contains_key(&day) {
            return None;
        }
        days.insert(day, parsed);
    }
    Some(days)
}

fn parse_bucket(bucket: &Map<String, Value>) -> Option<(i64, ParsedDay)> {
    let from = parse_time(bucket.get("from")?.as_str()?)?;
    let until = parse_time(bucket.get("until")?.as_str()?)?;
    let day = midnight(from);
    if from != day {
        return None;
    }
    let end = day + chrono::Duration::seconds(DAY);
    if from >= until || until > end {
        return None;
    }
    let totals = measures(bucket)?;
    let by_model = models(bucket)?;
    Some((day.timestamp(), ParsedDay { totals, by_model }))
}

fn models(bucket: &Map<String, Value>) -> Option<Option<BTreeMap<String, BTreeMap<String, f64>>>> {
    let Some(raw) = bucket.get("groups") else {
        return Some(None);
    };
    let mut models = BTreeMap::new();
    for raw in raw.as_array()? {
        let group = raw.as_object()?;
        let model = group
            .get("model")
            .and_then(Value::as_str)
            .filter(|model| !model.is_empty())?;
        if models.contains_key(model) {
            return None;
        }
        models.insert(model.to_string(), measures(group)?);
    }
    if bucket.contains_key("remainder") {
        Some(None)
    } else {
        Some(Some(models))
    }
}

/// Cached input is part of input, so tokens are input plus output. `None` without a request count.
fn measures(object: &Map<String, Value>) -> Option<BTreeMap<String, f64>> {
    let mut totals = BTreeMap::new();
    for (field, key) in [
        ("request_count", "requests"),
        ("usage_usd", "usage"),
        ("input_tokens", "input"),
        ("cached_input_tokens", "cache"),
        ("output_tokens", "output"),
    ] {
        match object.get(field) {
            None | Some(Value::Null) => {}
            Some(value) => {
                let number = json_number(value).filter(|number| *number >= 0.0)?;
                totals.insert(key.to_string(), number);
            }
        }
    }
    totals.get("requests")?;
    if let (Some(input), Some(output)) =
        (totals.get("input").copied(), totals.get("output").copied())
    {
        totals.insert("tokens".into(), input + output);
    }
    Some(totals)
}

fn midnight(time: DateTime<Utc>) -> DateTime<Utc> {
    Utc.from_utc_datetime(&time.date_naive().and_hms_opt(0, 0, 0).unwrap())
}

fn parse_time(value: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value.trim())
        .ok()
        .map(|date| date.with_timezone(&Utc))
}

fn json_number(value: &Value) -> Option<f64> {
    match value {
        Value::Number(number) => number.as_f64().filter(|number| number.is_finite()),
        Value::String(text) => text
            .trim()
            .parse::<f64>()
            .ok()
            .filter(|number| number.is_finite()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn measure(day: &ParsedDay, key: &str) -> f64 {
        day.totals.get(key).copied().unwrap_or(f64::NAN)
    }

    #[test]
    fn days_parse_buckets_and_any_malformed_bucket_rejects_the_read() {
        let documented = r#"{"granularity":"day","buckets":[{"from":"2026-09-30T00:00:00Z","until":"2026-10-01T00:00:00Z","request_count":8,"usage_usd":0.0166,"input_tokens":52000,"cached_input_tokens":30000,"output_tokens":7000,"groups":[
            {"model":"glm-5.3-flash","request_count":6,"usage_usd":0.0066,"input_tokens":48000,"cached_input_tokens":30000,"output_tokens":6000},
            {"model":"glm-5.3","request_count":2,"usage_usd":0.01,"input_tokens":4000,"cached_input_tokens":0,"output_tokens":1000}
        ]}]}"#;
        let day_key = parse_time("2026-09-30T00:00:00Z").unwrap().timestamp();
        let parsed = days(documented.as_bytes())
            .unwrap()
            .remove(&day_key)
            .unwrap();
        assert_eq!(measure(&parsed, "requests"), 8.0);
        assert!((measure(&parsed, "usage") - 0.0166).abs() < 1e-9);
        assert_eq!(measure(&parsed, "tokens"), 59_000.0);
        assert_eq!(measure(&parsed, "input"), 52_000.0);
        assert_eq!(measure(&parsed, "cache"), 30_000.0);
        assert_eq!(measure(&parsed, "output"), 7_000.0);
        assert_eq!(
            parsed
                .by_model
                .as_ref()
                .unwrap()
                .get("glm-5.3")
                .unwrap()
                .get("tokens")
                .copied(),
            Some(5_000.0)
        );

        let legacy = r#"{"granularity":"day","buckets":[{"from":"2026-09-30T00:00:00Z","until":"2026-10-01T00:00:00Z","request_count":3}]}"#;
        let legacy = days(legacy.as_bytes()).unwrap().remove(&day_key).unwrap();
        assert_eq!(legacy.totals, BTreeMap::from([("requests".into(), 3.0)]));

        let unsplit = r#"{"granularity":"day","buckets":[{"from":"2026-09-30T00:00:00Z","until":"2026-10-01T00:00:00Z","request_count":9,"groups":[{"model":"a","request_count":1}],"remainder":{"request_count":8}}]}"#;
        assert!(days(unsplit.as_bytes())
            .unwrap()
            .remove(&day_key)
            .unwrap()
            .by_model
            .is_none());

        let empty_groups = r#"{"granularity":"day","buckets":[{"from":"2026-09-30T00:00:00Z","until":"2026-10-01T00:00:00Z","request_count":0,"groups":[]}]}"#;
        assert_eq!(
            days(empty_groups.as_bytes())
                .unwrap()
                .remove(&day_key)
                .unwrap()
                .by_model,
            Some(BTreeMap::new())
        );

        let day = r#""from":"2026-09-30T00:00:00Z","until":"2026-10-01T00:00:00Z""#;
        for bucket in [
            format!(r#"{{{day}}}"#),
            format!(r#"{{{day},"request_count":-1}}"#),
            format!(r#"{{{day},"request_count":1,"usage_usd":"lots"}}"#),
            format!(r#"{{{day},"request_count":1,"groups":[{{"request_count":1}}]}}"#),
            format!(
                r#"{{{day},"request_count":2,"groups":[{{"model":"a","request_count":1}},{{"model":"a","request_count":1}}]}}"#
            ),
            r#"{"from":"2026-09-30T02:00:00Z","until":"2026-09-30T03:00:00Z","request_count":1}"#
                .into(),
            r#"{"from":"2026-09-30T00:00:00Z","until":"2026-10-02T00:00:00Z","request_count":1}"#
                .into(),
            format!(r#"{{{day},"request_count":1}},{{{day},"request_count":1}}"#),
        ] {
            assert!(
                days(format!(r#"{{"granularity":"day","buckets":[{bucket}]}}"#).as_bytes())
                    .is_none(),
                "{bucket}"
            );
        }
        assert!(days(br#"{"granularity":"hour","buckets":[{"from":"2026-09-30T00:00:00Z","until":"2026-10-01T00:00:00Z","request_count":1}]}"#).is_none());
    }

    #[test]
    fn a_successful_read_covers_omitted_days_without_inventing_figures() {
        let now = DateTime::from_timestamp(1_793_392_200, 0).unwrap();
        let today = midnight(now).timestamp();
        let yesterday = today - DAY;
        let body = format!(
            r#"{{"granularity":"day","buckets":[{{"from":"{}","until":"{}","request_count":3}}]}}"#,
            stamp(yesterday),
            stamp(yesterday + DAY)
        );
        let parsed = days(body.as_bytes()).unwrap();
        let covered: BTreeMap<_, _> = [today, yesterday]
            .into_iter()
            .map(|day| {
                (
                    day,
                    parsed
                        .get(&day)
                        .map(|parsed| parsed.totals.clone())
                        .unwrap_or_default(),
                )
            })
            .collect();
        assert!(covered[&today].is_empty());
        assert_eq!(covered[&yesterday].get("requests").copied(), Some(3.0));
    }

    fn stamp(seconds: i64) -> String {
        DateTime::from_timestamp(seconds, 0)
            .unwrap()
            .format("%Y-%m-%dT%H:%M:%SZ")
            .to_string()
    }
}
