//! Port of `GrokCreditsConfigDecoder` and `GrokUsageMapper`.

use chrono::{DateTime, Utc};
use serde_json::Value;

use crate::error::ProviderError;
use crate::http::HttpResponse;
use crate::kind::ProviderKind;
use crate::metric::{MetricLine, ProviderUsage};
use crate::widget::clamp_percent;

const WEEKLY: &str = "USAGE_PERIOD_TYPE_WEEKLY";

#[derive(Debug, PartialEq)]
pub struct CreditsConfig {
    pub period_type: String,
    pub used_percent: f64,
    pub period_start: DateTime<Utc>,
    pub period_end: DateTime<Utc>,
    pub on_demand_cap: f64,
}

impl CreditsConfig {
    pub fn period_duration_ms(&self) -> i64 {
        let micros = (self.period_end - self.period_start)
            .num_microseconds()
            .unwrap_or(0);
        (micros as f64 / 1000.0).round() as i64
    }
}

pub fn map_credits(response: &HttpResponse) -> Result<ProviderUsage, ProviderError> {
    if response.status == 412
        && response
            .json()
            .and_then(|body| {
                body.get("error")
                    .and_then(Value::as_str)
                    .map(|error| error.to_ascii_lowercase().contains("no personal team"))
            })
            .unwrap_or(false)
    {
        return Ok(ProviderUsage {
            breakdowns: Default::default(),
            plan: None,
            lines: vec![],
            notice: Some("Grok team accounts have no personal quota.".into()),
        });
    }
    ProviderError::require_success(response, ProviderKind::Grok)?;
    let config = decode(&response.body)?;
    let mut lines = Vec::new();
    if config.period_type == WEEKLY {
        lines.push(MetricLine::percent(
            "Weekly limit",
            clamp_percent(config.used_percent),
            Some(config.period_end),
            Some(config.period_duration_ms()),
        ));
    }
    let text = if config.on_demand_cap > 0.0 {
        format!("{} cap", format_units(config.on_demand_cap))
    } else {
        "Disabled".into()
    };
    lines.push(MetricLine::Badge {
        label: "Pay as you go".into(),
        text,
    });
    Ok(ProviderUsage {
        breakdowns: Default::default(),
        plan: None,
        lines,
        notice: None,
    })
}

pub fn plan_name(response: &HttpResponse) -> Option<String> {
    if !response.is_success() {
        return None;
    }
    let body = response.json()?;
    let plan = body.get("subscription_tier_display")?.as_str()?.trim();
    (!plan.is_empty()).then(|| plan.to_string())
}

pub fn decode(body: &[u8]) -> Result<CreditsConfig, ProviderError> {
    let root: Value = serde_json::from_slice(body).map_err(|_| ProviderError::InvalidResponse)?;
    let config = root
        .get("config")
        .and_then(Value::as_object)
        .ok_or(ProviderError::InvalidResponse)?;
    let period = config
        .get("currentPeriod")
        .and_then(Value::as_object)
        .ok_or(ProviderError::InvalidResponse)?;
    let period_type = period
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string();
    if period_type.is_empty() {
        return Err(ProviderError::InvalidResponse);
    }
    let start = date_field(period.get("start")).ok_or(ProviderError::InvalidResponse)?;
    let end = date_field(period.get("end")).ok_or(ProviderError::InvalidResponse)?;
    if end <= start {
        return Err(ProviderError::InvalidResponse);
    }
    let used_percent = match config.get("creditUsagePercent") {
        Some(raw) => number(raw).ok_or(ProviderError::InvalidResponse)?,
        None => 0.0,
    };
    let on_demand_cap = match config.get("onDemandCap") {
        Some(raw) => {
            let object = raw.as_object().ok_or(ProviderError::InvalidResponse)?;
            let cap = match object.get("val") {
                Some(raw) => number(raw).ok_or(ProviderError::InvalidResponse)?,
                None => 0.0,
            };
            if !fits_int(cap) {
                return Err(ProviderError::InvalidResponse);
            }
            cap
        }
        None => 0.0,
    };
    Ok(CreditsConfig {
        period_type,
        used_percent,
        period_start: start,
        period_end: end,
        on_demand_cap,
    })
}

fn format_units(value: f64) -> String {
    if value.is_finite() && value == value.round() && fits_int(value) {
        format!("{}", value.round() as i64)
    } else {
        format!("{value}")
    }
}

fn date_field(value: Option<&Value>) -> Option<DateTime<Utc>> {
    let text = value?.as_str()?.trim();
    if text.is_empty() {
        return None;
    }
    DateTime::parse_from_rfc3339(text)
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

fn fits_int(value: f64) -> bool {
    value.is_finite() && value.abs() < i64::MAX as f64
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Map};

    fn response(status: u16, body: &str) -> HttpResponse {
        HttpResponse {
            status,
            headers: Default::default(),
            body: body.as_bytes().to_vec(),
        }
    }

    fn body(percent: Option<Value>, cap: Option<Value>, start: &str, end: &str) -> Vec<u8> {
        let mut config = Map::new();
        config.insert(
            "currentPeriod".into(),
            json!({"type": "USAGE_PERIOD_TYPE_WEEKLY", "start": start, "end": end}),
        );
        if let Some(percent) = percent {
            config.insert("creditUsagePercent".into(), percent);
        }
        if let Some(cap) = cap {
            config.insert("onDemandCap".into(), json!({"val": cap}));
        }
        serde_json::to_vec(&json!({"config": config})).unwrap()
    }

    #[test]
    fn absent_zero_valued_fields_decode_as_zero() {
        let start = "2026-06-30T21:36:52.140114+00:00";
        let end = "2026-07-07T21:36:52.140114+00:00";
        assert_eq!(
            decode(&body(None, Some(json!(2500)), start, end))
                .unwrap()
                .used_percent,
            0.0
        );
        assert_eq!(
            decode(&body(Some(json!(99.0)), None, start, end))
                .unwrap()
                .on_demand_cap,
            0.0
        );
    }

    #[test]
    fn rejects_non_numeric_fields_a_reversed_period_and_a_missing_config() {
        let start = "2026-06-30T21:36:52.140114+00:00";
        let end = "2026-07-07T21:36:52.140114+00:00";
        assert!(decode(&body(Some(json!("high")), Some(json!(2500)), start, end)).is_err());
        assert!(decode(&body(Some(json!(99.0)), Some(json!("lots")), start, end)).is_err());
        assert!(decode(&body(Some(json!(99.0)), Some(json!(2500)), end, start)).is_err());
        for raw in [
            "{}",
            r#"{"config":{}}"#,
            r#"{"config":{"currentPeriod":{}}}"#,
            "not json",
        ] {
            assert_eq!(
                decode(raw.as_bytes()),
                Err(ProviderError::InvalidResponse),
                "{raw}"
            );
        }
    }

    #[test]
    fn a_team_principals_refusal_is_a_notice_and_any_other_412_is_a_request_failure() {
        let team = map_credits(&response(
            412,
            r#"{"error":"principal has no personal team"}"#,
        ))
        .unwrap();
        assert!(team.lines.is_empty());
        assert_eq!(
            team.notice.as_deref(),
            Some("Grok team accounts have no personal quota.")
        );
        assert_eq!(
            map_credits(&response(412, r#"{"error":"precondition failed"}"#)),
            Err(ProviderError::RequestFailed(412))
        );
    }

    #[test]
    fn a_pay_as_you_go_cap_too_large_for_an_int_is_an_invalid_response() {
        let body = r#"{"config":{"currentPeriod":{"type":"USAGE_PERIOD_TYPE_WEEKLY","start":"2026-06-30T21:36:52Z","end":"2026-07-07T21:36:52Z"},"onDemandCap":{"val":1e300}}}"#;
        assert_eq!(
            map_credits(&response(200, body)),
            Err(ProviderError::InvalidResponse)
        );
    }

    #[test]
    fn weekly_usage_and_the_cap_become_the_dashboard_lines() {
        let raw = body(
            Some(json!(99.0)),
            Some(json!(2500)),
            "2026-06-30T21:36:52Z",
            "2026-07-07T21:36:52Z",
        );
        let usage = map_credits(&response(200, &String::from_utf8(raw).unwrap())).unwrap();
        assert_eq!(
            usage.lines[0],
            MetricLine::percent(
                "Weekly limit",
                99.0,
                date_field(Some(&json!("2026-07-07T21:36:52Z"))),
                Some(7 * 24 * 60 * 60 * 1000)
            )
        );
        assert_eq!(
            usage.lines[1],
            MetricLine::Badge {
                label: "Pay as you go".into(),
                text: "2500 cap".into()
            }
        );

        let daily = r#"{"config":{"currentPeriod":{"type":"USAGE_PERIOD_TYPE_DAILY","start":"2026-06-30T00:00:00Z","end":"2026-07-01T00:00:00Z"}}}"#;
        let usage = map_credits(&response(200, daily)).unwrap();
        assert_eq!(
            usage.lines,
            vec![MetricLine::Badge {
                label: "Pay as you go".into(),
                text: "Disabled".into()
            }]
        );
    }

    #[test]
    fn plan_name_reads_the_settings_tier() {
        assert_eq!(
            plan_name(&response(
                200,
                r#"{"subscription_tier_display":" SuperGrok "}"#
            ))
            .as_deref(),
            Some("SuperGrok")
        );
        assert_eq!(
            plan_name(&response(200, r#"{"subscription_tier_display":"  "}"#)),
            None
        );
        assert_eq!(
            plan_name(&response(
                500,
                r#"{"subscription_tier_display":"SuperGrok"}"#
            )),
            None
        );
    }
}
