//! Port of `OllamaUsageMapper`. Monthly is a dollar meter; session and weekly are percents.

use chrono::{DateTime, Utc};
use serde_json::{Map, Value};

use crate::error::ProviderError;
use crate::metric::{MetricKind, MetricLine, MetricValue, ProgressFormat, ProviderUsage};
use crate::widget::clamp_percent;

const SESSION_MS: i64 = 5 * 60 * 60 * 1000;
const WEEK_MS: i64 = 7 * 24 * 60 * 60 * 1000;

pub fn map(balance: &[u8], account: Option<&Value>) -> Result<ProviderUsage, ProviderError> {
    Ok(ProviderUsage {
        breakdowns: Default::default(),
        plan: account.and_then(plan),
        lines: balance_lines(balance)?,
        notice: None,
    })
}

pub fn balance_lines(body: &[u8]) -> Result<Vec<MetricLine>, ProviderError> {
    let root: Value = serde_json::from_slice(body).map_err(|_| ProviderError::InvalidResponse)?;
    let Some(included) = root.get("included").and_then(Value::as_object) else {
        return Err(ProviderError::InvalidResponse);
    };
    let mut lines = Vec::new();
    if let Some(line) = legacy_line(included.get("session"), "Session", SESSION_MS) {
        lines.push(line);
    }
    if let Some(line) = legacy_line(included.get("weekly"), "Weekly", WEEK_MS) {
        lines.push(line);
    }
    if let Some(line) = monthly_line(included) {
        lines.push(line);
    }
    if lines.is_empty() {
        return Err(ProviderError::InvalidResponse);
    }
    if let Some(line) = purchased_line(root.get("purchased")) {
        lines.push(line);
    }
    Ok(lines)
}

/// ollama.com capitalizes JSON keys (`Plan`); the local server lowercases them when it proxies.
pub fn plan(body: &Value) -> Option<String> {
    field(body, "Plan")
        .or_else(|| field(body, "plan"))
        .map(|name| capitalize(&name))
}

pub fn email(body: &Value) -> Option<String> {
    field(body, "Email").or_else(|| field(body, "email"))
}

fn legacy_line(entry: Option<&Value>, label: &str, period_ms: i64) -> Option<MetricLine> {
    let entry = entry?.as_object()?;
    let remaining = number_at(entry, "remaining_percent")?;
    let resets_at = entry
        .get("resets_at")
        .and_then(Value::as_str)
        .and_then(parse_time);
    Some(MetricLine::percent(
        label,
        clamp_percent(100.0 - remaining),
        resets_at,
        resets_at.map(|_| period_ms),
    ))
}

fn monthly_line(included: &Map<String, Value>) -> Option<MetricLine> {
    let allowance = number_at(included, "allowance_usd").filter(|allowance| *allowance > 0.0)?;
    let balance = number_at(included, "balance_usd")?;
    let period = included.get("period").and_then(Value::as_object);
    let from = period
        .and_then(|period| period.get("from"))
        .and_then(Value::as_str)
        .and_then(parse_time);
    let until = period
        .and_then(|period| period.get("until"))
        .and_then(Value::as_str)
        .and_then(parse_time);
    let period_ms = match (from, until) {
        (Some(from), Some(until)) if until > from => Some((until - from).num_milliseconds()),
        _ => None,
    };
    Some(MetricLine::Progress {
        label: "Monthly".into(),
        used: allowance.min((allowance - balance).max(0.0)),
        limit: allowance,
        format: ProgressFormat::Dollars,
        resets_at: until,
        period_duration_ms: period_ms,
    })
}

fn purchased_line(purchased: Option<&Value>) -> Option<MetricLine> {
    let balance = number_at(purchased?.as_object()?, "balance_usd")?;
    Some(MetricLine::values(
        "Purchased Credits",
        vec![MetricValue::new(balance.max(0.0), MetricKind::Dollars)],
    ))
}

fn field(body: &Value, key: &str) -> Option<String> {
    let text = body.get(key)?.as_str()?.trim();
    (!text.is_empty()).then(|| text.to_string())
}

fn capitalize(text: &str) -> String {
    let mut out = String::new();
    let mut new_word = true;
    for ch in text.chars() {
        if ch.is_alphanumeric() {
            if new_word {
                out.extend(ch.to_uppercase());
                new_word = false;
            } else {
                out.extend(ch.to_lowercase());
            }
        } else {
            out.push(ch);
            new_word = true;
        }
    }
    out
}

fn parse_time(value: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value.trim())
        .ok()
        .map(|date| date.with_timezone(&Utc))
}

fn number(value: &Value) -> Option<f64> {
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

fn number_at(object: &Map<String, Value>, key: &str) -> Option<f64> {
    object.get(key).and_then(number)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn balance_lines_follow_the_dashboard_labels() {
        let body = serde_json::to_vec(&json!({
            "included": {
                "session": {"remaining_percent": 40, "resets_at": "2026-10-30T22:00:00Z"},
                "weekly": {"remaining_percent": 100},
                "allowance_usd": 100,
                "balance_usd": 72.5,
                "period": {"from": "2026-10-01T00:00:00Z", "until": "2026-11-01T00:00:00Z"}
            },
            "purchased": {"balance_usd": -3}
        }))
        .unwrap();
        let lines = balance_lines(&body).unwrap();
        assert_eq!(
            lines.iter().map(MetricLine::label).collect::<Vec<_>>(),
            ["Session", "Weekly", "Monthly", "Purchased Credits"]
        );
        assert_eq!(
            lines[0],
            MetricLine::percent(
                "Session",
                60.0,
                parse_time("2026-10-30T22:00:00Z"),
                Some(SESSION_MS)
            )
        );
        assert_eq!(lines[1], MetricLine::percent("Weekly", 0.0, None, None));
        match &lines[2] {
            MetricLine::Progress {
                used,
                limit,
                format,
                resets_at,
                period_duration_ms,
                ..
            } => {
                assert_eq!(*used, 27.5);
                assert_eq!(*limit, 100.0);
                assert_eq!(*format, ProgressFormat::Dollars);
                assert_eq!(*resets_at, parse_time("2026-11-01T00:00:00Z"));
                assert_eq!(*period_duration_ms, Some(31 * 24 * 60 * 60 * 1000));
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(
            lines[3],
            MetricLine::values(
                "Purchased Credits",
                vec![MetricValue::new(0.0, MetricKind::Dollars)]
            )
        );
    }

    #[test]
    fn a_balance_without_a_window_is_invalid_and_plans_are_title_cased() {
        assert_eq!(
            balance_lines(br#"{"purchased":{"balance_usd":4}}"#),
            Err(ProviderError::InvalidResponse)
        );
        assert_eq!(
            balance_lines(br#"{"included":{"allowance_usd":0,"balance_usd":0}}"#),
            Err(ProviderError::InvalidResponse)
        );
        assert_eq!(plan(&json!({"Plan":"pro"})).as_deref(), Some("Pro"));
        assert_eq!(plan(&json!({"plan":"MAX"})).as_deref(), Some("Max"));
        assert_eq!(
            email(&json!({"Email":" dev@example.com "})).as_deref(),
            Some("dev@example.com")
        );
        assert_eq!(email(&json!({"email":""})), None);
    }
}
