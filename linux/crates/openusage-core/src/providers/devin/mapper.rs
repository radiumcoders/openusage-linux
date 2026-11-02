//! Port of `DevinUsageMapper.swift`.

use chrono::{DateTime, Utc};
use serde_json::Value;

use crate::error::ProviderError;
use crate::metric::{MetricKind, MetricLine, MetricValue, ProviderUsage};

const DAY_MS: i64 = 24 * 60 * 60 * 1000;
const WEEK_MS: i64 = 7 * DAY_MS;

pub(crate) fn map(body: &[u8]) -> Result<ProviderUsage, ProviderError> {
    let value: Value = serde_json::from_slice(body).map_err(|_| ProviderError::InvalidResponse)?;
    let user = value
        .get("userStatus")
        .and_then(Value::as_object)
        .ok_or(ProviderError::InvalidResponse)?;
    let plan_status = user.get("planStatus").and_then(Value::as_object);
    let plan_info = plan_status
        .and_then(|status| status.get("planInfo"))
        .and_then(Value::as_object);
    let empty = serde_json::Map::new();
    let plan_status = plan_status.unwrap_or(&empty);
    let daily = quota(
        plan_status,
        "dailyQuotaRemainingPercent",
        "dailyQuotaResetAtUnix",
    )?;
    let weekly = quota(
        plan_status,
        "weeklyQuotaRemainingPercent",
        "weeklyQuotaResetAtUnix",
    )?;
    let mut lines = Vec::new();
    if plan_info
        .and_then(|info| info.get("hideDailyQuota"))
        .and_then(loose_bool)
        == Some(true)
    {
        let shown = weekly.or_else(|| daily.map(|(remaining, _)| (remaining, None)));
        if let Some(shown) = shown {
            lines.push(quota_line("Weekly quota", shown, WEEK_MS));
        }
    } else {
        if let Some(daily) = daily {
            lines.push(quota_line("Daily quota", daily, DAY_MS));
        }
        if let Some(weekly) = weekly {
            lines.push(quota_line("Weekly quota", weekly, WEEK_MS));
        }
    }
    if let Some(micros) = plan_status.get("overageBalanceMicros").and_then(number) {
        lines.push(MetricLine::values(
            "Extra usage balance",
            vec![MetricValue::new(
                micros.max(0.0) / 1_000_000.0,
                MetricKind::Dollars,
            )],
        ));
    }
    if lines.is_empty() {
        return Err(ProviderError::Unavailable(
            "Devin reports no quota for this account.".into(),
        ));
    }
    let plan = plan_info
        .and_then(|info| info.get("planName"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_string);
    Ok(ProviderUsage {
        breakdowns: Default::default(),
        plan,
        lines,
        notice: None,
    })
}

fn quota(
    plan: &serde_json::Map<String, Value>,
    key: &str,
    reset_key: &str,
) -> Result<Option<(f64, Option<DateTime<Utc>>)>, ProviderError> {
    let resets_at = plan
        .get(reset_key)
        .and_then(number)
        .and_then(|seconds| DateTime::from_timestamp(seconds as i64, 0));
    match plan.get(key) {
        None => Ok(resets_at.map(|resets_at| (0.0, Some(resets_at)))),
        Some(value) => {
            let Some(remaining) = number(value) else {
                return Err(ProviderError::InvalidResponse);
            };
            Ok(Some((remaining, resets_at)))
        }
    }
}

fn quota_line(label: &str, quota: (f64, Option<DateTime<Utc>>), period_ms: i64) -> MetricLine {
    MetricLine::percent(
        label,
        clamp_percent(100.0 - quota.0),
        quota.1,
        Some(period_ms),
    )
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
