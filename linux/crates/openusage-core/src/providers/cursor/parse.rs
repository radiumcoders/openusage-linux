//! JSON, JWT, and date helpers shared by the Cursor port. Numeric reads match `ProviderParse`:
//! JSON numbers and numeric strings, never booleans, never non-finite values.

use chrono::{DateTime, NaiveDate, Utc};
use serde_json::{Map, Value};

use base64::Engine;

pub(crate) const DAY_MS: i64 = 24 * 60 * 60 * 1000;
pub(crate) const WEEK_MS: i64 = 7 * DAY_MS;
pub(crate) const MONTH_MS: i64 = 30 * DAY_MS;

pub(crate) fn number(value: &Value) -> Option<f64> {
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

pub(crate) fn number_at<'a>(object: &'a Map<String, Value>, key: &str) -> Option<f64> {
    object.get(key).and_then(number)
}

/// `value as? Bool` — only a JSON boolean, so a missing or non-boolean value stays `None`.
pub(crate) fn strict_bool(value: &Value) -> Option<bool> {
    value.as_bool()
}

pub(crate) fn as_int(value: f64) -> Option<i64> {
    (value.is_finite() && value.abs() < i64::MAX as f64).then_some(value as i64)
}

pub(crate) fn clamp_percent(value: f64) -> f64 {
    if value.is_finite() {
        value.clamp(0.0, 100.0)
    } else {
        0.0
    }
}

pub(crate) fn cents_to_dollars(cents: f64) -> f64 {
    cents.round() / 100.0
}

pub(crate) fn millis_date(millis: f64) -> Option<DateTime<Utc>> {
    as_int(millis).and_then(|millis| DateTime::from_timestamp_millis(millis))
}

pub(crate) fn trimmed_nonempty(value: &str) -> Option<&str> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then_some(trimmed)
}

pub(crate) fn title_case(
    value: &str,
    separator: impl Fn(char) -> bool,
    lower_tail: bool,
) -> String {
    value
        .split(separator)
        .filter(|word| !word.is_empty())
        .map(|word| {
            let mut chars = word.chars();
            let head: String = chars
                .next()
                .map(|character| character.to_uppercase().collect())
                .unwrap_or_default();
            let tail: String = chars.collect();
            if lower_tail {
                head + &tail.to_lowercase()
            } else {
                head + &tail
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

pub(crate) fn jwt_payload(token: &str) -> Option<Value> {
    let segment = token.split('.').nth(1)?;
    let mut padded = segment.replace('-', "+").replace('_', "/");
    while padded.len() % 4 != 0 {
        padded.push('=');
    }
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(padded)
        .ok()?;
    serde_json::from_slice(&bytes).ok()
}

pub(crate) fn jwt_expiry(token: &str) -> Option<DateTime<Utc>> {
    let exp = number(jwt_payload(token)?.get("exp")?)?;
    as_int(exp).and_then(|seconds| DateTime::from_timestamp(seconds, 0))
}

pub(crate) fn jwt_subject(token: &str) -> Option<String> {
    jwt_payload(token)?
        .get("sub")
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// `OpenUsageISO8601.date(from:)`, plus a `yyyy-MM-dd` calendar day in UTC.
pub(crate) fn parse_iso(value: &str) -> Option<DateTime<Utc>> {
    let mut text = value.trim().to_string();
    if let Some(stripped) = text.strip_suffix(" UTC") {
        text = format!("{stripped}Z");
    }
    if let Ok(date) = DateTime::parse_from_rfc3339(&text) {
        return Some(date.with_timezone(&Utc));
    }
    let spaced = text.replacen(' ', "T", 1);
    if spaced != text {
        if let Ok(date) = DateTime::parse_from_rfc3339(&spaced) {
            return Some(date.with_timezone(&Utc));
        }
    }
    NaiveDate::parse_from_str(&text, "%Y-%m-%d")
        .ok()
        .and_then(|date| date.and_hms_opt(0, 0, 0))
        .map(|date| date.and_utc())
}
