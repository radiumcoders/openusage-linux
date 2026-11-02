//! Port of `OpenCodeUsageMapper`, including the rolling-window placeholder reset.

use chrono::{DateTime, Utc};
use serde_json::{Map, Value};

use crate::error::ProviderError;
use crate::http::HttpResponse;
use crate::metric::MetricLine;
use crate::widget::clamp_percent;

pub const PLAN: &str = "Go";
/// OpenCode rounds `resetInSec` up to whole seconds and HTTP `Date` is whole seconds, so two seconds
/// covers both. The comparison is exact only at capture time, so it runs here and not at render.
pub const PLACEHOLDER_RESET_TOLERANCE_SECONDS: f64 = 2.0;

const SESSION_MS: i64 = 5 * 60 * 60 * 1000;
const WEEK_MS: i64 = 7 * 24 * 60 * 60 * 1000;
const MONTH_MS: i64 = 30 * 24 * 60 * 60 * 1000;

pub fn meter_lines(
    response: &HttpResponse,
    captured_at: DateTime<Utc>,
) -> Result<Vec<MetricLine>, ProviderError> {
    let body: Value =
        serde_json::from_slice(&response.body).map_err(|_| ProviderError::InvalidResponse)?;
    meter_lines_at(&body, http_date(response).unwrap_or(captured_at))
}

pub fn meter_lines_at(
    body: &Value,
    captured_at: DateTime<Utc>,
) -> Result<Vec<MetricLine>, ProviderError> {
    let Some(usage) = body.get("usage").and_then(Value::as_object) else {
        return Err(ProviderError::InvalidResponse);
    };
    Ok(vec![
        window(
            usage.get("rolling"),
            "Session",
            SESSION_MS,
            captured_at,
            true,
        )?,
        window(usage.get("weekly"), "Weekly", WEEK_MS, captured_at, false)?,
        window(
            usage.get("monthly"),
            "Monthly",
            MONTH_MS,
            captured_at,
            false,
        )?,
    ])
}

pub fn error_type(body: &[u8]) -> Option<String> {
    let root: Value = serde_json::from_slice(body).ok()?;
    let name = root.get("error")?.get("type")?.as_str()?.trim();
    (!name.is_empty()).then(|| name.to_string())
}

fn window(
    raw: Option<&Value>,
    label: &str,
    period_ms: i64,
    captured_at: DateTime<Utc>,
    drops_placeholder_reset: bool,
) -> Result<MetricLine, ProviderError> {
    let Some(object) = raw.and_then(Value::as_object) else {
        return Err(ProviderError::InvalidResponse);
    };
    let Some(percent) = num_at(object, "percent") else {
        return Err(ProviderError::InvalidResponse);
    };
    let Some(reported) = object
        .get("resetsAt")
        .and_then(Value::as_str)
        .and_then(parse_time)
    else {
        return Err(ProviderError::InvalidResponse);
    };
    let used = clamp_percent(percent);
    let mut resets_at = Some(reported);
    if drops_placeholder_reset && used == 0.0 {
        let period = period_ms as f64 / 1000.0;
        let distance = (reported - captured_at).num_milliseconds() as f64 / 1000.0 - period;
        if distance.abs() <= PLACEHOLDER_RESET_TOLERANCE_SECONDS {
            resets_at = None;
        }
    }
    Ok(MetricLine::percent(label, used, resets_at, Some(period_ms)))
}

fn http_date(response: &HttpResponse) -> Option<DateTime<Utc>> {
    let value = response.header("date")?.trim();
    DateTime::parse_from_rfc2822(value)
        .ok()
        .map(|date| date.with_timezone(&Utc))
}

fn parse_time(value: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value.trim())
        .ok()
        .map(|date| date.with_timezone(&Utc))
}

fn num_at(object: &Map<String, Value>, key: &str) -> Option<f64> {
    match object.get(key)? {
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
    use crate::http::HttpResponse;
    use chrono::Duration;
    use serde_json::{json, Value};
    use std::collections::BTreeMap;

    fn at(seconds: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(seconds, 0).unwrap()
    }

    fn stamp(time: DateTime<Utc>) -> String {
        time.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
    }

    fn usage(
        rolling_percent: f64,
        rolling_reset: &str,
        weekly_percent: f64,
        weekly_reset: &str,
        monthly_percent: f64,
        monthly_reset: &str,
    ) -> Value {
        json!({
            "usage": {
                "rolling": {"percent": rolling_percent, "resetsAt": rolling_reset},
                "weekly": {"percent": weekly_percent, "resetsAt": weekly_reset},
                "monthly": {"percent": monthly_percent, "resetsAt": monthly_reset}
            }
        })
    }

    fn session_reset(lines: &[MetricLine]) -> Option<DateTime<Utc>> {
        match &lines[0] {
            MetricLine::Progress { resets_at, .. } => *resets_at,
            other => panic!("session is not a progress line: {other:?}"),
        }
    }

    #[test]
    fn meter_lines_match_dashboard_percents_and_resets() {
        let body = usage(
            12.0,
            "2026-07-12T13:30:00.662Z",
            8.0,
            "2026-07-13T00:00:00.662Z",
            100.0,
            "2026-08-04T11:18:32.662Z",
        );
        let lines = meter_lines_at(&body, Utc::now()).unwrap();
        assert_eq!(
            lines.iter().map(MetricLine::label).collect::<Vec<_>>(),
            ["Session", "Weekly", "Monthly"]
        );
        match &lines[0] {
            MetricLine::Progress {
                used,
                limit,
                format,
                resets_at,
                period_duration_ms,
                ..
            } => {
                assert_eq!(*used, 12.0);
                assert_eq!(*limit, 100.0);
                assert_eq!(*format, crate::metric::ProgressFormat::Percent);
                assert_eq!(*resets_at, parse_time("2026-07-12T13:30:00.662Z"));
                assert_eq!(*period_duration_ms, Some(SESSION_MS));
            }
            other => panic!("{other:?}"),
        }
        match &lines[1] {
            MetricLine::Progress {
                used,
                resets_at,
                period_duration_ms,
                ..
            } => {
                assert_eq!(*used, 8.0);
                assert_eq!(*resets_at, parse_time("2026-07-13T00:00:00.662Z"));
                assert_eq!(*period_duration_ms, Some(WEEK_MS));
            }
            other => panic!("{other:?}"),
        }
        match &lines[2] {
            MetricLine::Progress {
                used,
                period_duration_ms,
                ..
            } => {
                assert_eq!(*used, 100.0);
                assert_eq!(*period_duration_ms, Some(MONTH_MS));
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn zero_percent_is_a_real_meter_not_no_data() {
        let body = usage(
            0.0,
            "2026-07-12T17:00:00.000Z",
            0.0,
            "2026-07-13T00:00:00.000Z",
            0.0,
            "2026-08-04T00:00:00.000Z",
        );
        let lines = meter_lines_at(&body, Utc::now()).unwrap();
        match &lines[0] {
            MetricLine::Progress {
                used,
                limit,
                format,
                ..
            } => {
                assert_eq!(*used, 0.0);
                assert_eq!(*limit, 100.0);
                assert_eq!(*format, crate::metric::ProgressFormat::Percent);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn untouched_rolling_window_drops_placeholder_reset() {
        let captured = at(1_800_000_000);
        let placeholder = captured + Duration::milliseconds(5 * 3_600_000 + 800);
        let body = usage(
            0.0,
            &stamp(placeholder),
            1.0,
            &stamp(captured + Duration::days(3)),
            0.0,
            &stamp(captured + Duration::days(20)),
        );
        let lines = meter_lines_at(&body, captured).unwrap();
        match &lines[0] {
            MetricLine::Progress {
                used, resets_at, ..
            } => {
                assert_eq!(*used, 0.0);
                assert_eq!(*resets_at, None);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn sub_one_percent_rolling_window_keeps_anchored_reset() {
        let captured = at(1_800_000_000);
        let period = Duration::milliseconds(SESSION_MS);
        for age in [30_i64, 60] {
            let anchored = captured + period - Duration::seconds(age);
            let body = usage(
                0.0,
                &stamp(anchored),
                1.0,
                &stamp(captured + Duration::days(3)),
                0.0,
                &stamp(captured + Duration::days(20)),
            );
            let lines = meter_lines_at(&body, captured).unwrap();
            assert_eq!(
                session_reset(&lines),
                Some(anchored),
                "active session aged {age}s lost its reset"
            );
        }
    }

    #[test]
    fn placeholder_comparison_is_bounded_on_both_sides() {
        let captured = at(1_800_000_000);
        let period = Duration::milliseconds(SESSION_MS);
        let mapped = |offset_ms: i64| {
            let reset = captured + period + Duration::milliseconds(offset_ms);
            let body = usage(
                0.0,
                &stamp(reset),
                0.0,
                &stamp(captured + Duration::days(3)),
                0.0,
                &stamp(captured + Duration::days(20)),
            );
            session_reset(&meter_lines_at(&body, captured).unwrap())
        };
        let tolerance_ms = (PLACEHOLDER_RESET_TOLERANCE_SECONDS * 1000.0) as i64;
        assert_eq!(mapped(-tolerance_ms), None);
        assert_eq!(mapped(tolerance_ms), None);
        assert!(mapped(-tolerance_ms - 500).is_some());
        assert!(mapped(tolerance_ms + 500).is_some());
        assert!(
            mapped(5 * 60 * 1000).is_some(),
            "an implausibly distant reset is not a placeholder"
        );
    }

    #[test]
    fn http_date_header_wins_over_skewed_local_clock() {
        let server = parse_time("2027-01-15T08:00:00.000Z").unwrap();
        let placeholder = server + Duration::milliseconds(5 * 3_600_000 + 500);
        let body = usage(
            0.0,
            &stamp(placeholder),
            0.0,
            "2027-01-18T00:00:00.000Z",
            0.0,
            "2027-02-10T08:00:00.000Z",
        );
        let mut headers = BTreeMap::new();
        headers.insert("date".into(), "Fri, 15 Jan 2027 08:00:00 GMT".into());
        let response = HttpResponse {
            status: 200,
            headers,
            body: serde_json::to_vec(&body).unwrap(),
        };
        let lines = meter_lines(&response, server + Duration::minutes(2)).unwrap();
        assert_eq!(session_reset(&lines), None);
    }

    #[test]
    fn missing_or_malformed_http_date_falls_back_to_captured_at() {
        let captured = at(1_800_000_000);
        let placeholder = captured + Duration::milliseconds(5 * 3_600_000 + 500);
        let body = usage(
            0.0,
            &stamp(placeholder),
            0.0,
            &stamp(captured + Duration::days(3)),
            0.0,
            &stamp(captured + Duration::days(20)),
        );
        let bytes = serde_json::to_vec(&body).unwrap();
        for headers in [
            BTreeMap::new(),
            BTreeMap::from([("date".into(), "not-an-http-date".into())]),
        ] {
            let response = HttpResponse {
                status: 200,
                headers,
                body: bytes.clone(),
            };
            assert_eq!(
                session_reset(&meter_lines(&response, captured).unwrap()),
                None
            );
        }
    }

    #[test]
    fn full_period_weekly_and_monthly_resets_survive_at_zero_usage() {
        let captured = at(1_800_000_000);
        let weekly = captured + Duration::days(7);
        let monthly = captured + Duration::days(30);
        let body = usage(
            0.0,
            &stamp(captured + Duration::hours(2)),
            0.0,
            &stamp(weekly),
            0.0,
            &stamp(monthly),
        );
        let lines = meter_lines_at(&body, captured).unwrap();
        match (&lines[1], &lines[2]) {
            (
                MetricLine::Progress {
                    resets_at: weekly_reset,
                    ..
                },
                MetricLine::Progress {
                    resets_at: monthly_reset,
                    ..
                },
            ) => {
                assert_eq!(*weekly_reset, Some(weekly));
                assert_eq!(*monthly_reset, Some(monthly));
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn missing_or_malformed_reset_is_invalid() {
        let missing_weekly = json!({"usage": {
            "rolling": {"percent": 0, "resetsAt": "2027-01-15T13:00:00.000Z"},
            "weekly": {"percent": 0},
            "monthly": {"percent": 0, "resetsAt": "2027-02-10T08:00:00.000Z"}
        }});
        let malformed = json!({"usage": {
            "rolling": {"percent": 0, "resetsAt": "not-a-date"},
            "weekly": {"percent": 0, "resetsAt": "2027-01-18T00:00:00.000Z"},
            "monthly": {"percent": 0, "resetsAt": "2027-02-10T08:00:00.000Z"}
        }});
        assert_eq!(
            meter_lines_at(&missing_weekly, Utc::now()),
            Err(ProviderError::InvalidResponse)
        );
        assert_eq!(
            meter_lines_at(&malformed, Utc::now()),
            Err(ProviderError::InvalidResponse)
        );
    }

    #[test]
    fn percent_is_clamped() {
        let body = usage(
            150.0,
            "2026-07-12T13:30:00.662Z",
            -4.0,
            "2026-07-13T00:00:00.662Z",
            35.0,
            "2026-08-04T11:18:32.662Z",
        );
        let lines = meter_lines_at(&body, Utc::now()).unwrap();
        match (&lines[0], &lines[1]) {
            (
                MetricLine::Progress { used: rolling, .. },
                MetricLine::Progress { used: weekly, .. },
            ) => {
                assert_eq!(*rolling, 100.0);
                assert_eq!(*weekly, 0.0);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn missing_usage_or_window_is_invalid() {
        assert_eq!(
            meter_lines_at(&json!({}), Utc::now()),
            Err(ProviderError::InvalidResponse)
        );
        assert_eq!(
            meter_lines_at(&json!({"usage": {"weekly": {"percent": 1}}}), Utc::now()),
            Err(ProviderError::InvalidResponse)
        );
    }

    #[test]
    fn error_type_from_documented_error_body() {
        let entitlement = br#"{"type":"error","error":{"type":"EntitlementError","message":"OpenCode Go subscription required."}}"#;
        let auth = br#"{"type":"error","error":{"type":"AuthError","message":"Unauthorized"}}"#;
        assert_eq!(error_type(entitlement).as_deref(), Some("EntitlementError"));
        assert_eq!(error_type(auth).as_deref(), Some("AuthError"));
        assert_eq!(error_type(b"<html>"), None);
    }
}
