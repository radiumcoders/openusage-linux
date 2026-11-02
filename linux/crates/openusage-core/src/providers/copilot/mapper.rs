//! Port of `CopilotUsageMapper.swift` and `CopilotOrgBillingMapper.swift`.

use chrono::{DateTime, NaiveDate, Utc};
use serde_json::{Map, Value};

use crate::error::ProviderError;
use crate::metric::{MetricKind, MetricLine, MetricValue, ProviderUsage};

const MONTH_MS: i64 = 30 * 24 * 60 * 60 * 1000;

pub(crate) fn map_response(body: &[u8]) -> Result<ProviderUsage, ProviderError> {
    let value: Value = serde_json::from_slice(body).map_err(|_| ProviderError::InvalidResponse)?;
    let body = value.as_object().ok_or(ProviderError::InvalidResponse)?;
    map_body(body)
}

pub(crate) fn map_body(body: &Map<String, Value>) -> Result<ProviderUsage, ProviderError> {
    let plan = plan_label(body.get("copilot_plan"));
    let resets_at = reset_date(body.get("quota_reset_date"))
        .or_else(|| reset_date(body.get("limited_user_reset_date")));
    let snapshots = body.get("quota_snapshots").and_then(Value::as_object);
    let premium = snapshots.and_then(|snapshots| snapshots.get("premium_interactions"));
    let mut lines = Vec::new();
    let credits = snapshot_line("Credits", premium, resets_at);
    if let Some(line) = credits.clone() {
        lines.push(line);
        if let Some(line) = overage_line(premium) {
            lines.push(line);
        }
    }
    if let Some(line) = snapshot_line(
        "Chat",
        snapshots.and_then(|snapshots| snapshots.get("chat")),
        resets_at,
    ) {
        lines.push(line);
    }
    if let Some(line) = snapshot_line(
        "Completions",
        snapshots.and_then(|snapshots| snapshots.get("completions")),
        resets_at,
    ) {
        lines.push(line);
    }
    if lines.is_empty() {
        let limited = body.get("limited_user_quotas").and_then(Value::as_object);
        let monthly = body.get("monthly_quotas").and_then(Value::as_object);
        if let Some(line) = limited_line(
            "Chat",
            limited.and_then(|q| q.get("chat")),
            monthly.and_then(|q| q.get("chat")),
            resets_at,
        ) {
            lines.push(line);
        }
        if let Some(line) = limited_line(
            "Completions",
            limited.and_then(|q| q.get("completions")),
            monthly.and_then(|q| q.get("completions")),
            resets_at,
        ) {
            lines.push(line);
        }
    }
    if lines.is_empty() {
        if body.get("token_based_billing").and_then(loose_bool) == Some(true) {
            return Ok(ProviderUsage {
                breakdowns: Default::default(),
                plan,
                lines: personal_credits(premium),
                notice: None,
            });
        }
        return Err(ProviderError::Unavailable(
            "Copilot usage data is unavailable for this account.".into(),
        ));
    }
    Ok(ProviderUsage {
        breakdowns: Default::default(),
        plan,
        lines,
        notice: None,
    })
}

fn snapshot_line(
    label: &str,
    raw: Option<&Value>,
    resets_at: Option<DateTime<Utc>>,
) -> Option<MetricLine> {
    let snapshot = raw?.as_object()?;
    let entitlement = snapshot.get("entitlement").and_then(number);
    let remaining = snapshot.get("remaining").and_then(number);
    if snapshot.get("unlimited").and_then(loose_bool) == Some(true)
        || entitlement == Some(-1.0)
        || remaining == Some(-1.0)
    {
        return None;
    }
    if entitlement == Some(0.0) {
        return None;
    }
    let used = if let Some(percent_remaining) = snapshot.get("percent_remaining").and_then(number) {
        clamp_percent(100.0 - percent_remaining)
    } else if let (Some(entitlement), Some(remaining)) = (entitlement, remaining) {
        if entitlement <= 0.0 {
            return None;
        }
        clamp_percent(100.0 - (remaining / entitlement) * 100.0)
    } else {
        return None;
    };
    Some(MetricLine::percent(label, used, resets_at, Some(MONTH_MS)))
}

fn personal_credits(raw: Option<&Value>) -> Vec<MetricLine> {
    let Some(used) = raw
        .and_then(Value::as_object)
        .and_then(|snapshot| snapshot.get("credits_used"))
        .and_then(number)
        .filter(|used| *used > 0.0)
    else {
        return Vec::new();
    };
    vec![MetricLine::values(
        "Credits",
        vec![MetricValue::new(used, MetricKind::Count)],
    )]
}

fn overage_line(raw: Option<&Value>) -> Option<MetricLine> {
    let snapshot = raw?.as_object()?;
    if snapshot.get("overage_permitted").and_then(loose_bool) != Some(true) {
        return None;
    }
    let overage = snapshot
        .get("overage_count")
        .and_then(number)
        .unwrap_or(0.0)
        .max(0.0);
    Some(MetricLine::values(
        "Extra Usage",
        vec![MetricValue::new(overage, MetricKind::Count)],
    ))
}

fn limited_line(
    label: &str,
    remaining: Option<&Value>,
    total: Option<&Value>,
    resets_at: Option<DateTime<Utc>>,
) -> Option<MetricLine> {
    let total = total.and_then(number).filter(|total| *total > 0.0)?;
    let remaining = remaining.and_then(number)?;
    let used = (total - remaining).max(0.0);
    Some(MetricLine::percent(
        label,
        clamp_percent((used / total) * 100.0),
        resets_at,
        Some(MONTH_MS),
    ))
}

fn plan_label(value: Option<&Value>) -> Option<String> {
    let raw = value
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())?;
    Some(title_case(raw))
}

fn title_case(value: &str) -> String {
    value
        .split(|character: char| character == '_' || character == ' ' || character == '-')
        .filter(|word| !word.is_empty())
        .map(|word| {
            let mut chars = word.chars();
            let head: String = chars
                .next()
                .map(|character| character.to_uppercase().collect())
                .unwrap_or_default();
            let tail: String = chars.collect();
            head + &tail.to_lowercase()
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn reset_date(value: Option<&Value>) -> Option<DateTime<Utc>> {
    let raw = value
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())?;
    parse_time(raw)
}

pub(crate) fn usage_lines(body: &[u8]) -> Option<Vec<MetricLine>> {
    let value: Value = serde_json::from_slice(body).ok()?;
    let items = value.get("usageItems")?.as_array()?;
    let mut credits = 0.0;
    let mut spend = 0.0;
    for item in items {
        let item = item.as_object()?;
        if item.get("product").and_then(Value::as_str) != Some("Copilot") {
            continue;
        }
        credits += number(item.get("grossQuantity")?)?;
        spend += number(item.get("netAmount")?)?;
    }
    Some(vec![
        MetricLine::values(
            "Org Credits",
            vec![MetricValue::labeled(credits, MetricKind::Count, "credits")],
        ),
        MetricLine::values(
            "Org Spend",
            vec![MetricValue::new(spend, MetricKind::Dollars)],
        ),
    ])
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

fn loose_bool(value: &Value) -> Option<bool> {
    match value {
        Value::Bool(value) => Some(*value),
        Value::Number(number) => number.as_f64().map(|number| number != 0.0),
        Value::String(text) => match text.trim().to_ascii_lowercase().as_str() {
            "true" | "1" => Some(true),
            "false" | "0" => Some(false),
            _ => None,
        },
        _ => None,
    }
}

fn clamp_percent(value: f64) -> f64 {
    if value.is_finite() {
        value.clamp(0.0, 100.0)
    } else {
        0.0
    }
}

fn parse_time(value: &str) -> Option<DateTime<Utc>> {
    let mut text = value.trim().to_string();
    if let Some(stripped) = text.strip_suffix(" UTC") {
        text = format!("{stripped}Z");
    }
    if let Ok(date) = DateTime::parse_from_rfc3339(&text) {
        return Some(date.with_timezone(&Utc));
    }
    NaiveDate::parse_from_str(&text, "%Y-%m-%d")
        .ok()
        .and_then(|date| date.and_hms_opt(0, 0, 0))
        .map(|date| date.and_utc())
}
