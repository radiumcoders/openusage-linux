//! Claude usage, plan, and reset-grant mapping. Port of `ClaudeUsageMapper` and `ClaudeResetGrantStatus`.

use chrono::{DateTime, TimeZone, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::ProviderError;
use crate::http::HttpResponse;
use crate::kind::ProviderKind;
use crate::log;
use crate::metric::{MetricKind, MetricLine, MetricValue, ProgressFormat};

const SESSION_MS: i64 = 5 * 60 * 60 * 1000;
const WEEK_MS: i64 = 7 * 24 * 60 * 60 * 1000;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub(super) struct GrantStatus {
    pub eligible: bool,
    pub grants: Vec<StoredGrant>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub(super) struct StoredGrant {
    pub id: String,
    pub resets_left: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ends_at: Option<DateTime<Utc>>,
}

pub(super) fn map_usage(response: &HttpResponse) -> Result<Vec<MetricLine>, ProviderError> {
    ProviderError::require_success(response, ProviderKind::Claude)?;
    let body = json_object(&response.body)?;
    let mut lines = Vec::new();
    append_window(&body, "five_hour", "Session", SESSION_MS, &mut lines);
    append_window(&body, "seven_day", "Weekly", WEEK_MS, &mut lines);
    append_window(&body, "seven_day_sonnet", "Sonnet", WEEK_MS, &mut lines);
    append_fable(&body, &mut lines);
    append_extra(&body, &mut lines);
    Ok(lines)
}

pub(super) fn map_reset_grants(
    response: &HttpResponse,
) -> Result<Option<GrantStatus>, ProviderError> {
    ProviderError::require_success(response, ProviderKind::Claude)?;
    let body = json_object(&response.body)?;
    Ok(parse_status(body.get("cedar_ember")))
}

pub(super) fn resets_line(status: &GrantStatus, now: DateTime<Utc>) -> MetricLine {
    let mut count = 0_i64;
    let mut expiries = Vec::new();
    if status.eligible {
        for grant in &status.grants {
            if grant.resets_left < 1 {
                continue;
            }
            if grant.ends_at.is_some_and(|ends| ends <= now) {
                continue;
            }
            count += grant.resets_left;
            if let Some(ends) = grant.ends_at {
                expiries.extend(std::iter::repeat_n(ends, grant.resets_left as usize));
            }
        }
    }
    expiries.sort();
    MetricLine::Values {
        label: "Rate Limit Resets".into(),
        values: vec![MetricValue::labeled(
            count as f64,
            MetricKind::Count,
            "available",
        )],
        expiries_at: expiries,
    }
}

/// `organization_type` drops a leading `claude_`. Max 20x and anything Ultra read as Ultra.
pub(super) fn format_live_plan(
    organization_type: Option<&str>,
    tier: Option<&str>,
) -> Option<String> {
    let organization_type = organization_type?;
    let subscription = organization_type
        .strip_prefix("claude_")
        .unwrap_or(organization_type);
    format_plan(subscription, tier)
}

fn format_plan(subscription_type: &str, tier: Option<&str>) -> Option<String> {
    let raw = subscription_type.trim();
    if raw.is_empty() {
        return None;
    }
    let base = title_case(raw, true, ' ');
    let tier = tier.unwrap_or("");
    if base == "Ultra" || (base == "Max" && (tier.contains("ultra") || tier.contains("20x"))) {
        return Some("Ultra".into());
    }
    if base == "Max" {
        return Some(base);
    }
    if let Some(multiplier) = first_multiplier(tier) {
        return Some(format!("{base} {multiplier}"));
    }
    Some(base)
}

fn append_window(
    body: &Value,
    key: &str,
    label: &str,
    period_ms: i64,
    lines: &mut Vec<MetricLine>,
) {
    let Some(window) = body.get(key).filter(|value| value.is_object()) else {
        return;
    };
    let Some(used) = num(window.get("utilization")) else {
        return;
    };
    lines.push(MetricLine::percent(
        label,
        used,
        claude_reset(window.get("resets_at")),
        Some(period_ms),
    ));
}

fn append_fable(body: &Value, lines: &mut Vec<MetricLine>) {
    let Some(entries) = body.get("limits").and_then(Value::as_array) else {
        return;
    };
    for entry in entries {
        if entry.get("kind").and_then(Value::as_str) != Some("weekly_scoped") {
            continue;
        }
        let display = entry
            .pointer("/scope/model/display_name")
            .and_then(Value::as_str);
        if display != Some("Fable") {
            continue;
        }
        let Some(used) = num(entry.get("percent")) else {
            continue;
        };
        lines.push(MetricLine::percent(
            "Fable",
            used,
            claude_reset(entry.get("resets_at")),
            Some(WEEK_MS),
        ));
        return;
    }
}

fn append_extra(body: &Value, lines: &mut Vec<MetricLine>) {
    let Some(extra) = body.get("extra_usage").filter(|value| value.is_object()) else {
        return;
    };
    if extra.get("is_enabled").and_then(Value::as_bool) != Some(true) {
        return;
    }
    let Some(used_cents) = num(extra.get("used_credits")) else {
        return;
    };
    let used = cents_to_dollars(used_cents);
    if let Some(limit_cents) = num(extra.get("monthly_limit")).filter(|cents| *cents > 0.0) {
        lines.push(MetricLine::Progress {
            label: "Extra usage spent".into(),
            used,
            limit: cents_to_dollars(limit_cents),
            format: ProgressFormat::Dollars,
            resets_at: None,
            period_duration_ms: None,
        });
    } else if used > 0.0 {
        lines.push(MetricLine::values(
            "Extra usage spent",
            vec![MetricValue::new(used, MetricKind::Dollars)],
        ));
    }
}

fn parse_status(value: Option<&Value>) -> Option<GrantStatus> {
    let object = value?.as_object()?;
    let mut grants = Vec::new();
    if let Some(entries) = object.get("grants").and_then(Value::as_array) {
        for entry in entries {
            let Some(grant) = parse_grant(entry) else {
                log::warn(
                    "plugin:claude",
                    "reset grants block has a malformed grant; ignoring the block",
                );
                return None;
            };
            grants.push(grant);
        }
    }
    Some(GrantStatus {
        eligible: object.get("eligible").and_then(Value::as_bool) == Some(true),
        grants,
    })
}

fn parse_grant(value: &Value) -> Option<StoredGrant> {
    let id = value
        .get("id")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())?;
    let left = int_floor(num(value.get("resets_left"))?)?;
    Some(StoredGrant {
        id: id.to_string(),
        resets_left: left.max(0),
        ends_at: claude_reset(value.get("ends_at")),
    })
}

fn json_object(body: &[u8]) -> Result<Value, ProviderError> {
    let value: Value = serde_json::from_slice(body).map_err(|_| ProviderError::InvalidResponse)?;
    if !value.is_object() {
        return Err(ProviderError::InvalidResponse);
    }
    Ok(value)
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

fn cents_to_dollars(cents: f64) -> f64 {
    cents.round() / 100.0
}

fn claude_reset(value: Option<&Value>) -> Option<DateTime<Utc>> {
    let value = value?;
    if let Some(text) = value.as_str() {
        let text = text.trim();
        if text.is_empty() {
            return None;
        }
        return parse_iso(text);
    }
    let number = num(Some(value))?;
    let millis = if number.abs() < 1e10 {
        number * 1000.0
    } else {
        number
    };
    Utc.timestamp_millis_opt(millis.round() as i64).single()
}

fn parse_iso(value: &str) -> Option<DateTime<Utc>> {
    let trimmed = value.trim();
    let owned = trimmed
        .strip_suffix(" UTC")
        .map(|prefix| format!("{prefix}Z"));
    let text = owned.as_deref().unwrap_or(trimmed);
    let normalized = match text.split_once(' ') {
        Some((date, time)) if time.starts_with(|c: char| c.is_ascii_digit()) => {
            format!("{date}T{time}")
        }
        _ => text.to_string(),
    };
    DateTime::parse_from_rfc3339(&normalized)
        .or_else(|_| DateTime::parse_from_rfc3339(&format!("{normalized}Z")))
        .ok()
        .map(|date| date.with_timezone(&Utc))
}

fn title_case(raw: &str, lower_tail: bool, separator: char) -> String {
    raw.split(separator)
        .filter(|word| !word.is_empty())
        .map(|word| {
            let mut chars = word.chars();
            let Some(first) = chars.next() else {
                return String::new();
            };
            let mut titled = String::new();
            titled.extend(first.to_uppercase());
            let tail: String = chars.collect();
            if lower_tail {
                titled.push_str(&tail.to_lowercase());
            } else {
                titled.push_str(&tail);
            }
            titled
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn first_multiplier(tier: &str) -> Option<&str> {
    let bytes = tier.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index].is_ascii_digit() {
            let start = index;
            while index < bytes.len() && bytes[index].is_ascii_digit() {
                index += 1;
            }
            if index < bytes.len() && bytes[index] == b'x' {
                return Some(&tier[start..=index]);
            }
        } else {
            index += 1;
        }
    }
    None
}

#[cfg(test)]
#[path = "mapper_tests.rs"]
mod tests;
