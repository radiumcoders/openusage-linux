//! ChatGPT usage mapping. Port of `CodexUsageMapper`.
//! Credits are priced at 4¢, the one estimate the app allows. Reset credits are read, not claimed.

use chrono::{DateTime, Duration, TimeZone, Utc};
use serde_json::Value;

use crate::error::ProviderError;
use crate::http::HttpResponse;
use crate::kind::ProviderKind;
use crate::metric::{MetricKind, MetricLine, MetricValue, ProgressFormat};

const SESSION_MS: i64 = 5 * 60 * 60 * 1000;
const WEEK_MS: i64 = 7 * 24 * 60 * 60 * 1000;
const CREDIT_USD: f64 = 0.04;

#[derive(Debug)]
pub(super) struct Mapped {
    pub plan: Option<String>,
    pub lines: Vec<MetricLine>,
}

pub(super) fn map_usage(
    response: &HttpResponse,
    reset_credits: Option<&HttpResponse>,
    now: DateTime<Utc>,
) -> Result<Mapped, ProviderError> {
    ProviderError::require_success(response, ProviderKind::Codex)?;
    let body = json_object(&response.body)?;
    let mut lines = Vec::new();
    let rate_limit = body.get("rate_limit");
    lines.extend(window_lines(
        rate_limit,
        ("Session", "Weekly"),
        (
            header_number(response, "x-codex-primary-used-percent"),
            header_number(response, "x-codex-secondary-used-percent"),
        ),
        now,
    ));
    lines.extend(spark_lines(&body, now));
    if let Some(resets) = read_reset_credits(&body, reset_credits) {
        lines.push(MetricLine::Values {
            label: "Rate Limit Resets".into(),
            values: vec![MetricValue::labeled(
                resets.count as f64,
                MetricKind::Count,
                "available",
            )],
            expiries_at: resets.expiries,
        });
    }
    if let Some(remaining) = read_credits_remaining(response, &body) {
        if int_trunc(remaining).is_some() {
            lines.push(credits_line(remaining));
        }
    }
    if let Some(limit) = credit_limit_line(&body) {
        lines.push(limit);
    }
    Ok(Mapped {
        plan: format_plan(body.get("plan_type").and_then(Value::as_str)),
        lines,
    })
}

/// The raw `plan_type`, which selects the credits report. The formatted name is the usage plan.
pub(super) fn raw_plan_type(body: &[u8]) -> Option<String> {
    json_object(body)
        .ok()?
        .get("plan_type")
        .and_then(Value::as_str)
        .map(|plan| plan.trim().to_ascii_lowercase())
}

fn window_lines(
    rate_limit: Option<&Value>,
    labels: (&str, &str),
    headers: (Option<f64>, Option<f64>),
    now: DateTime<Utc>,
) -> Vec<MetricLine> {
    let object = rate_limit.filter(|value| value.is_object());
    let slots = [
        slot(
            object.and_then(|value| value.get("primary_window")),
            headers.0,
        ),
        slot(
            object.and_then(|value| value.get("secondary_window")),
            headers.1,
        ),
    ];
    let meters = [(labels.0, SESSION_MS), (labels.1, WEEK_MS)];
    let familiar = [SESSION_MS, WEEK_MS];
    meters
        .into_iter()
        .enumerate()
        .filter_map(|(index, (label, period))| {
            let exact = slots
                .iter()
                .flatten()
                .find(|candidate| period_ms(&candidate.window) == Some(period));
            let fallback = slots[index].as_ref().filter(|candidate| {
                period_ms(&candidate.window).is_none_or(|period| !familiar.contains(&period))
            });
            let candidate = exact.or(fallback)?;
            let used = candidate.used?;
            let period = period_ms(&candidate.window).unwrap_or(period);
            Some(MetricLine::percent(
                label,
                used,
                reset_date(&candidate.window, now),
                Some(period),
            ))
        })
        .collect()
}

struct Slot {
    window: Value,
    used: Option<f64>,
}

fn slot(value: Option<&Value>, header: Option<f64>) -> Option<Slot> {
    let window = match value {
        Some(value) if value.is_object() => value.clone(),
        _ if header.is_some() => Value::Object(Default::default()),
        _ => return None,
    };
    let used = num(window.get("used_percent")).or(header);
    Some(Slot { window, used })
}

fn spark_lines(body: &Value, now: DateTime<Utc>) -> Vec<MetricLine> {
    let Some(entries) = body.get("additional_rate_limits").and_then(Value::as_array) else {
        return Vec::new();
    };
    let Some(spark) = entries.iter().find(|entry| is_spark(entry)) else {
        return Vec::new();
    };
    let Some(rate_limit) = spark.get("rate_limit") else {
        return Vec::new();
    };
    window_lines(
        Some(rate_limit),
        ("Spark", "Spark Weekly"),
        (None, None),
        now,
    )
}

fn is_spark(entry: &Value) -> bool {
    ["limit_name", "metered_feature"].iter().any(|key| {
        entry
            .get(*key)
            .and_then(Value::as_str)
            .is_some_and(|name| name.to_ascii_lowercase().contains("spark"))
    })
}

fn reset_date(window: &Value, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
    if let Some(reset_at) = num(window.get("reset_at")) {
        return from_unix(reset_at);
    }
    let after = num(window.get("reset_after_seconds"))?;
    now.checked_add_signed(Duration::milliseconds((after * 1000.0).round() as i64))
}

fn period_ms(window: &Value) -> Option<i64> {
    int_trunc(num(window.get("limit_window_seconds"))? * 1000.0)
}

struct Resets {
    count: i64,
    expiries: Vec<DateTime<Utc>>,
}

/// The count comes from this pass's usage body. The dedicated list, read hourly, supplies the count
/// only when the usage body has none, and its expiries only while it agrees with that count.
fn read_reset_credits(body: &Value, reset_credits: Option<&HttpResponse>) -> Option<Resets> {
    let embedded = body
        .get("rate_limit_reset_credits")
        .filter(|value| value.is_object());
    let dedicated = reset_credits.and_then(|response| {
        if !response.is_success() {
            return None;
        }
        let body = response.json().filter(|value| value.is_object())?;
        available_count(&body)?;
        Some(body)
    });
    let count = embedded
        .and_then(available_count)
        .or_else(|| dedicated.as_ref().and_then(available_count))?;
    if count == 0 {
        return Some(Resets {
            count: 0,
            expiries: Vec::new(),
        });
    }
    let source = if dedicated.as_ref().and_then(available_count) == Some(count) {
        dedicated.as_ref()
    } else {
        embedded
    };
    let expiries = source.map(available_expiries).unwrap_or_default();
    Some(Resets { count, expiries })
}

fn available_count(source: &Value) -> Option<i64> {
    let count = num(source.get("available_count")).filter(|count| *count >= 0.0)?;
    int_floor(count)
}

fn available_expiries(source: &Value) -> Vec<DateTime<Utc>> {
    let Some(credits) = source.get("credits").and_then(Value::as_array) else {
        return Vec::new();
    };
    let mut expiries: Vec<DateTime<Utc>> = credits
        .iter()
        .filter(
            |credit| match credit.get("status").and_then(Value::as_str) {
                None => true,
                Some(status) => status == "available",
            },
        )
        .filter_map(|credit| parse_expiry(credit.get("expires_at")))
        .collect();
    expiries.sort();
    expiries
}

fn parse_expiry(value: Option<&Value>) -> Option<DateTime<Utc>> {
    let value = value?;
    if let Some(text) = value.as_str() {
        return DateTime::parse_from_rfc3339(text.trim())
            .ok()
            .map(|date| date.with_timezone(&Utc));
    }
    from_unix(num(Some(value))?)
}

fn from_unix(seconds: f64) -> Option<DateTime<Utc>> {
    Utc.timestamp_millis_opt((seconds * 1000.0).round() as i64)
        .single()
}

fn credits_line(remaining: f64) -> MetricLine {
    let credits = int_floor(remaining).unwrap_or(0).max(0) as f64;
    MetricLine::values(
        "Credits",
        vec![
            MetricValue::new(credits * CREDIT_USD, MetricKind::Dollars),
            MetricValue::labeled(credits, MetricKind::Count, "credits"),
        ],
    )
}

fn credit_limit_line(body: &Value) -> Option<MetricLine> {
    let limit = body
        .pointer("/spend_control/individual_limit")
        .filter(|value| value.is_object())?;
    let used = num(limit.get("used")).filter(|used| *used >= 0.0)?;
    let cap = num(limit.get("limit")).filter(|cap| *cap >= 0.0)?;
    Some(MetricLine::Progress {
        label: "Credit Limit".into(),
        used,
        limit: cap,
        format: ProgressFormat::Count {
            suffix: "credits".into(),
        },
        resets_at: num(limit.get("reset_at")).and_then(from_unix),
        period_duration_ms: None,
    })
}

fn read_credits_remaining(response: &HttpResponse, body: &Value) -> Option<f64> {
    if let Some(credits) = body.get("credits").filter(|value| value.is_object()) {
        if let Some(balance) = num(credits.get("balance")) {
            return Some(balance);
        }
        if credits.get("has_credits").and_then(Value::as_bool) == Some(false) {
            return Some(0.0);
        }
    }
    header_number(response, "x-codex-credits-balance")
}

pub(super) fn format_plan(raw: Option<&str>) -> Option<String> {
    let raw = raw?.trim();
    if raw.is_empty() {
        return None;
    }
    Some(match raw.to_ascii_lowercase().as_str() {
        "prolite" => "Pro 100".into(),
        "pro" => "Pro 200".into(),
        "promax" => "Pro 500".into(),
        "self_serve_business_prolite" => "Business Premium".into(),
        _ => title_case(raw),
    })
}

fn title_case(raw: &str) -> String {
    raw.split('_')
        .filter(|word| !word.is_empty())
        .map(|word| {
            let mut chars = word.chars();
            let Some(first) = chars.next() else {
                return String::new();
            };
            let mut titled = String::new();
            titled.extend(first.to_uppercase());
            titled.push_str(chars.as_str());
            titled
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn json_object(body: &[u8]) -> Result<Value, ProviderError> {
    let value: Value = serde_json::from_slice(body).map_err(|_| ProviderError::InvalidResponse)?;
    value
        .is_object()
        .then_some(value)
        .ok_or(ProviderError::InvalidResponse)
}

fn header_number(response: &HttpResponse, name: &str) -> Option<f64> {
    let number: f64 = response.header(name)?.trim().parse().ok()?;
    number.is_finite().then_some(number)
}

fn num(value: Option<&Value>) -> Option<f64> {
    let number = match value? {
        Value::Number(number) => number.as_f64(),
        Value::String(text) => text.trim().parse().ok(),
        _ => None,
    }?;
    number.is_finite().then_some(number)
}

fn int_floor(value: f64) -> Option<i64> {
    int_trunc(value.floor())
}

fn int_trunc(value: f64) -> Option<i64> {
    if !value.is_finite() || value.abs() >= i64::MAX as f64 {
        None
    } else {
        Some(value as i64)
    }
}
