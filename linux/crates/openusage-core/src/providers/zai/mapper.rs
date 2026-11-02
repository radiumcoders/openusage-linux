//! Port of `ZAIUsageMapper`. Quota windows become Session, Weekly, and Web Searches.

use chrono::{DateTime, TimeZone, Utc};
use serde_json::{Map, Value};

use crate::error::ProviderError;
use crate::kind::ProviderKind;
use crate::metric::{MetricLine, ProgressFormat};
use crate::widget::clamp_percent;

const HOUR_MS: f64 = 60.0 * 60.0 * 1000.0;
const DAY_MS: f64 = 24.0 * HOUR_MS;
const MONTH_MS: i64 = 30 * 24 * 60 * 60 * 1000;

pub fn map(
    quota_body: &[u8],
    subscription: Option<&Value>,
) -> Result<crate::metric::ProviderUsage, ProviderError> {
    Ok(crate::metric::ProviderUsage {
        breakdowns: Default::default(),
        plan: subscription.and_then(plan_name),
        lines: map_quota(quota_body)?,
        notice: None,
    })
}

pub fn map_quota(body: &[u8]) -> Result<Vec<MetricLine>, ProviderError> {
    let root: Value = serde_json::from_slice(body).map_err(|_| ProviderError::InvalidResponse)?;
    let Some(root) = root.as_object() else {
        return Err(ProviderError::InvalidResponse);
    };
    if root.get("success") == Some(&Value::Bool(false)) {
        if root
            .get("msg")
            .and_then(Value::as_str)
            .is_some_and(|msg| msg.to_ascii_lowercase().contains("coding plan"))
        {
            return Err(ProviderError::Unavailable(
                "No active GLM Coding Plan. Subscribe at z.ai/subscribe to see usage.".into(),
            ));
        }
        if integer(root.get("code")) == Some(401) {
            return Err(ProviderError::SignInExpired(ProviderKind::Zai));
        }
    }

    let container = match root.get("data") {
        Some(data) => data.as_object().ok_or(ProviderError::InvalidResponse)?,
        None => root,
    };
    let limits = container
        .get("limits")
        .and_then(Value::as_array)
        .ok_or(ProviderError::InvalidResponse)?;
    if limits.is_empty() {
        return Ok(vec![]);
    }
    let mut entries = Vec::with_capacity(limits.len());
    for entry in limits {
        entries.push(entry.as_object().ok_or(ProviderError::InvalidResponse)?);
    }

    let mut lines = Vec::new();
    let mut saw_recognized = false;
    for entry in entries
        .iter()
        .copied()
        .filter(|entry| matches!(kind(entry), Some("CREDIT_LIMIT" | "TOKENS_LIMIT")))
    {
        let Some(window) = classify(entry)? else {
            continue;
        };
        saw_recognized = true;
        let (label, period_ms) = match window {
            Window::Session(period) => ("Session", period),
            Window::Weekly(period) => ("Weekly", period),
        };
        lines.push(percent_line(entry, label, period_ms)?);
    }
    if let Some(web) = entries
        .into_iter()
        .find(|entry| kind(entry) == Some("TIME_LIMIT"))
    {
        saw_recognized = true;
        lines.push(web_search_line(web)?);
    }
    if lines.is_empty() {
        if saw_recognized {
            return Err(ProviderError::InvalidResponse);
        }
        return Ok(vec![]);
    }
    Ok(lines)
}

pub fn plan_name(body: &Value) -> Option<String> {
    let name = body
        .get("data")?
        .as_array()?
        .first()?
        .as_object()?
        .get("productName")?
        .as_str()?
        .trim();
    (!name.is_empty()).then(|| name.to_string())
}

enum Window {
    Session(i64),
    Weekly(i64),
}

fn classify(entry: &Map<String, Value>) -> Result<Option<Window>, ProviderError> {
    let Some(unit) = num_at(entry, "unit") else {
        return Err(ProviderError::InvalidResponse);
    };
    let Some(number) = num_at(entry, "number").filter(|number| *number > 0.0) else {
        return Err(ProviderError::InvalidResponse);
    };
    let Some(unit_ms) = unit_ms(unit) else {
        return Ok(None);
    };
    let duration = unit_ms * number;
    if duration < 1.0 {
        return Err(ProviderError::InvalidResponse);
    }
    let Some(period_ms) = fit_i64(duration) else {
        return Err(ProviderError::InvalidResponse);
    };
    if period_ms < DAY_MS as i64 {
        Ok(Some(Window::Session(period_ms)))
    } else {
        Ok(Some(Window::Weekly(period_ms)))
    }
}

fn unit_ms(unit: f64) -> Option<f64> {
    if unit == 3.0 {
        Some(HOUR_MS)
    } else if unit == 4.0 {
        Some(DAY_MS)
    } else if unit == 6.0 {
        Some(7.0 * DAY_MS)
    } else if unit == 5.0 {
        Some(30.0 * DAY_MS)
    } else {
        None
    }
}

fn percent_line(
    entry: &Map<String, Value>,
    label: &str,
    period_ms: i64,
) -> Result<MetricLine, ProviderError> {
    let Some(percentage) = num_at(entry, "percentage") else {
        return Err(ProviderError::InvalidResponse);
    };
    Ok(MetricLine::percent(
        label,
        clamp_percent(percentage),
        epoch_ms(num_at(entry, "nextResetTime")),
        Some(period_ms),
    ))
}

/// `usage` is the allowance and `currentValue` is how many searches have been spent.
fn web_search_line(entry: &Map<String, Value>) -> Result<MetricLine, ProviderError> {
    let Some(used) = num_at(entry, "currentValue").filter(|used| *used >= 0.0) else {
        return Err(ProviderError::InvalidResponse);
    };
    let Some(limit) = num_at(entry, "usage").filter(|limit| *limit >= 0.0) else {
        return Err(ProviderError::InvalidResponse);
    };
    Ok(MetricLine::Progress {
        label: "Web Searches".into(),
        used,
        limit,
        format: ProgressFormat::Count {
            suffix: "searches".into(),
        },
        resets_at: epoch_ms(num_at(entry, "nextResetTime")),
        period_duration_ms: Some(MONTH_MS),
    })
}

fn kind(entry: &Map<String, Value>) -> Option<&str> {
    entry
        .get("type")
        .and_then(Value::as_str)
        .or_else(|| entry.get("name").and_then(Value::as_str))
}

fn epoch_ms(ms: Option<f64>) -> Option<DateTime<Utc>> {
    ms.filter(|ms| ms.is_finite())
        .and_then(|ms| Utc.timestamp_millis_opt(ms.round() as i64).single())
}

fn num(value: &Value) -> Option<f64> {
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

fn num_at(object: &Map<String, Value>, key: &str) -> Option<f64> {
    object.get(key).and_then(num)
}

fn integer(value: Option<&Value>) -> Option<i64> {
    let number = num(value?)?;
    (number.fract() == 0.0).then(|| number as i64)
}

fn fit_i64(value: f64) -> Option<i64> {
    (value.is_finite() && value.abs() < i64::MAX as f64).then_some(value as i64)
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIVE_QUOTA: &str = r#"{"code":200,"msg":"Operation successful","data":{"limits":[
      {"type":"TOKENS_LIMIT","unit":3,"number":5,"percentage":17,"nextResetTime":1782724971179},
      {"type":"TOKENS_LIMIT","unit":6,"number":1,"percentage":3,"nextResetTime":1783305486997},
      {"type":"TIME_LIMIT","unit":5,"number":1,"usage":1000,"currentValue":0,"remaining":1000,"percentage":0,"nextResetTime":1785292686976,"usageDetails":[{"modelCode":"search-prime","usage":0},{"modelCode":"web-reader","usage":0},{"modelCode":"zread","usage":0}]}
    ],"level":"pro"},"success":true}"#;

    const LIVE_CREDIT: &str = r#"{"code":200,"msg":"Operation successful","data":{"limits":[
      {"type":"CREDIT_LIMIT","unit":3,"number":5,"usage":2000,"currentValue":0,"remaining":2000,"percentage":0},
      {"type":"CREDIT_LIMIT","unit":6,"number":1,"usage":10000,"currentValue":9855,"remaining":145,"percentage":98,"nextResetTime":1786685679998}
    ],"level":"lite"},"success":true}"#;

    const LIVE_SUBSCRIPTION: &str = r#"{"code":200,"msg":"Operation successful","data":[{"productName":"GLM Coding Pro","status":"VALID","nextRenewTime":"2026-07-29","billingCycle":"monthly","inCurrentPeriod":true}],"success":true}"#;

    fn progress(lines: &[MetricLine], label: &str) -> (f64, f64, Option<i64>) {
        match lines.iter().find(|line| line.label() == label) {
            Some(MetricLine::Progress {
                used,
                limit,
                period_duration_ms,
                ..
            }) => (*used, *limit, *period_duration_ms),
            other => panic!("expected a progress line for {label}, got {other:?}"),
        }
    }

    #[test]
    fn maps_live_response_to_session_weekly_and_web_searches() {
        let subscription: Value = serde_json::from_str(LIVE_SUBSCRIPTION).unwrap();
        let mapped = map(LIVE_QUOTA.as_bytes(), Some(&subscription)).unwrap();
        assert_eq!(mapped.plan.as_deref(), Some("GLM Coding Pro"));
        assert_eq!(
            progress(&mapped.lines, "Session"),
            (17.0, 100.0, Some(5 * 60 * 60 * 1000))
        );
        assert_eq!(
            progress(&mapped.lines, "Weekly"),
            (3.0, 100.0, Some(7 * 24 * 60 * 60 * 1000))
        );
        assert_eq!(
            progress(&mapped.lines, "Web Searches"),
            (0.0, 1000.0, Some(MONTH_MS))
        );
        match mapped
            .lines
            .iter()
            .find(|line| line.label() == "Web Searches")
        {
            Some(MetricLine::Progress {
                format: ProgressFormat::Count { suffix },
                ..
            }) => assert_eq!(suffix, "searches"),
            other => panic!("web searches format, got {other:?}"),
        }
    }

    #[test]
    fn maps_credit_limit_response_to_session_and_weekly() {
        let mapped = map(LIVE_CREDIT.as_bytes(), None).unwrap();
        assert_eq!(mapped.plan, None);
        assert_eq!(
            progress(&mapped.lines, "Session"),
            (0.0, 100.0, Some(5 * 60 * 60 * 1000))
        );
        assert_eq!(
            progress(&mapped.lines, "Weekly"),
            (98.0, 100.0, Some(7 * 24 * 60 * 60 * 1000))
        );
        match mapped.lines.iter().find(|line| line.label() == "Session") {
            Some(MetricLine::Progress {
                resets_at: None, ..
            }) => {}
            other => panic!("the 5-hour credit window has no reset, got {other:?}"),
        }
    }

    #[test]
    fn missing_required_values_never_become_zero_usage() {
        for body in [
            r#"{"data":{"limits":[{"type":"TOKENS_LIMIT","unit":3,"number":5}]}}"#,
            r#"{"data":{"limits":[{"type":"TOKENS_LIMIT","unit":3,"number":5,"percentage":true}]}}"#,
            r#"{"data":{"limits":[{"type":"TIME_LIMIT","usage":1000}]}}"#,
            r#"{"data":{"limits":[{"type":"TIME_LIMIT","currentValue":10}]}}"#,
            r#"{"data":{"limits":[{"type":"TIME_LIMIT","currentValue":-1,"usage":1000}]}}"#,
        ] {
            assert_eq!(
                map_quota(body.as_bytes()),
                Err(ProviderError::InvalidResponse),
                "{body}"
            );
        }
    }

    #[test]
    fn malformed_envelope_is_rejected_but_explicit_empty_limits_remain_valid() {
        for body in [
            "not-json",
            r#"{"data":[]}"#,
            r#"{"data":{}}"#,
            r#"{"data":{"limits":{}}}"#,
        ] {
            assert_eq!(
                map_quota(body.as_bytes()),
                Err(ProviderError::InvalidResponse),
                "{body}"
            );
        }
        assert!(map_quota(br#"{"data":{"limits":[]}}"#).unwrap().is_empty());
    }

    #[test]
    fn a_key_with_no_coding_plan_says_so_and_any_other_failed_body_stays_invalid() {
        let no_plan = map_quota(
            r#"{"code":500,"msg":"当前用户不存在coding plan","success":false}"#.as_bytes(),
        );
        assert_eq!(
            no_plan,
            Err(ProviderError::Unavailable(
                "No active GLM Coding Plan. Subscribe at z.ai/subscribe to see usage.".into()
            ))
        );
        assert_eq!(
            map_quota(br#"{"code":500,"msg":"internal error","success":false}"#),
            Err(ProviderError::InvalidResponse)
        );
    }

    #[test]
    fn a_key_zai_refuses_inside_an_ok_response_is_rejected() {
        assert_eq!(
            map_quota(br#"{"code":401,"msg":"token expired or incorrect","success":false}"#),
            Err(ProviderError::SignInExpired(ProviderKind::Zai))
        );
    }

    #[test]
    fn unknown_entries_do_not_hide_known_meters() {
        let lines = map_quota(
            br#"{"data":{"limits":[{"type":"FUTURE_LIMIT"},{"type":"TOKENS_LIMIT","unit":99,"number":1,"percentage":70},{"name":"TOKENS_LIMIT","unit":3,"number":5,"percentage":25}]}}"#,
        )
        .unwrap();
        assert!(lines.iter().any(|line| line.label() == "Session"));
        assert!(lines.iter().all(|line| line.label() != "Weekly"));
    }

    #[test]
    fn only_unknown_limits_remain_forward_compatible_no_data() {
        let lines = map_quota(
            br#"{"data":{"limits":[{"type":"FUTURE_LIMIT"},{"type":"TOKENS_LIMIT","unit":99,"number":1,"percentage":70}]}}"#,
        )
        .unwrap();
        assert!(lines.is_empty());
    }
}
