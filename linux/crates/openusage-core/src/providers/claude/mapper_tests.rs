use super::*;

fn response(status: u16, body: &str) -> HttpResponse {
    HttpResponse {
        status,
        headers: Default::default(),
        body: body.as_bytes().to_vec(),
    }
}

fn at(seconds: i64) -> DateTime<Utc> {
    Utc.timestamp_opt(seconds, 0).single().unwrap()
}

#[test]
fn plans_follow_the_october_2026_names() {
    assert_eq!(
        format_live_plan(Some("claude_max"), Some("default_claude_max_5x")).as_deref(),
        Some("Max")
    );
    assert_eq!(
        format_live_plan(Some("claude_max"), Some("default_claude_max_20x")).as_deref(),
        Some("Ultra")
    );
    assert_eq!(
        format_live_plan(Some("claude_max"), None).as_deref(),
        Some("Max")
    );
    assert_eq!(
        format_live_plan(Some("claude_max"), Some("default_claude_ultra")).as_deref(),
        Some("Ultra")
    );
    assert_eq!(
        format_live_plan(Some("claude_ultra"), Some("default_claude_ultra")).as_deref(),
        Some("Ultra")
    );
    assert_eq!(
        format_live_plan(Some("claude_ultra"), Some("default_claude_max_20x")).as_deref(),
        Some("Ultra")
    );
    assert_eq!(
        format_live_plan(Some("claude_pro"), Some("default_claude_ai")).as_deref(),
        Some("Pro")
    );
    assert_eq!(
        format_live_plan(Some("claude_team"), Some("default_claude_max_5x")).as_deref(),
        Some("Team 5x")
    );
    assert_eq!(
        format_live_plan(Some("claude_enterprise"), None).as_deref(),
        Some("Enterprise")
    );
    assert_eq!(format_live_plan(None, Some("default_claude_max_5x")), None);
}

#[test]
fn maps_windows_fable_and_extra_usage() {
    let body = r#"{
            "five_hour": {"utilization": 18, "resets_at": "2099-01-01T00:00:00Z"},
            "seven_day": {"utilization": 40, "resets_at": "2099-01-08T00:00:00Z"},
            "seven_day_sonnet": {"utilization": "7"},
            "limits": [
                {"kind": "weekly_scoped", "scope": {"model": {"display_name": "Opus"}}, "percent": 80},
                {"kind": "weekly_scoped", "scope": {"model": {"display_name": "Fable"}}, "percent": 12, "resets_at": "2099-06-01T00:00:00Z"}
            ],
            "extra_usage": {"is_enabled": true, "used_credits": 123456, "monthly_limit": 500000}
        }"#;
    let lines = map_usage(&response(200, body)).unwrap();
    assert_eq!(
        lines.iter().map(MetricLine::label).collect::<Vec<_>>(),
        ["Session", "Weekly", "Sonnet", "Fable", "Extra usage spent"]
    );
    match &lines[0] {
        MetricLine::Progress {
            used,
            limit,
            period_duration_ms,
            resets_at,
            ..
        } => {
            assert_eq!(*used, 18.0);
            assert_eq!(*limit, 100.0);
            assert_eq!(*period_duration_ms, Some(SESSION_MS));
            assert_eq!(
                resets_at.unwrap(),
                DateTime::parse_from_rfc3339("2099-01-01T00:00:00Z").unwrap()
            );
        }
        other => panic!("session {other:?}"),
    }
    match &lines[3] {
        MetricLine::Progress {
            used,
            period_duration_ms,
            ..
        } => {
            assert_eq!(*used, 12.0);
            assert_eq!(*period_duration_ms, Some(WEEK_MS));
        }
        other => panic!("fable {other:?}"),
    }
    match &lines[4] {
        MetricLine::Progress {
            used,
            limit,
            format: ProgressFormat::Dollars,
            ..
        } => {
            assert!((*used - 1234.56).abs() < 1e-9);
            assert!((*limit - 5000.0).abs() < 1e-9);
        }
        other => panic!("extra {other:?}"),
    }
}

#[test]
fn uncapped_extra_usage_is_a_dollar_figure_and_a_missing_window_is_omitted() {
    let lines = map_usage(&response(
            200,
            r#"{"five_hour": null, "seven_day": {"utilization": 40}, "extra_usage": {"is_enabled": true, "used_credits": 123456, "monthly_limit": null}}"#,
        ))
        .unwrap();
    assert_eq!(
        lines.iter().map(MetricLine::label).collect::<Vec<_>>(),
        ["Weekly", "Extra usage spent"]
    );
    match &lines[1] {
        MetricLine::Values { values, .. } => {
            assert_eq!(values.len(), 1);
            assert_eq!(values[0].kind, MetricKind::Dollars);
            assert!((values[0].number - 1234.56).abs() < 1e-9);
        }
        other => panic!("{other:?}"),
    }
    let disabled = map_usage(&response(
        200,
        r#"{"extra_usage": {"is_enabled": false, "used_credits": 100}}"#,
    ))
    .unwrap();
    assert!(disabled.is_empty());
}

#[test]
fn reset_grants_count_each_remaining_reset_and_drop_a_bad_block() {
    let now = at(1_790_000_000);
    let body = r#"{ "cedar_ember": {
            "eligible": true,
            "grants": [
              {"id": "opus", "resets_left": 1, "ends_at": "2026-10-22T16:00:00+00:00"},
              {"id": "other", "resets_left": 2, "ends_at": "2026-10-01T00:00:00+00:00"},
              {"id": "spent", "resets_left": 0, "ends_at": "2026-10-05T00:00:00+00:00"},
              {"id": "lapsed", "resets_left": 1, "ends_at": "2026-09-01T00:00:00+00:00"}
            ] } }"#;
    let status = map_reset_grants(&response(200, body)).unwrap().unwrap();
    match resets_line(&status, now) {
        MetricLine::Values {
            values,
            expiries_at,
            ..
        } => {
            assert_eq!(
                values,
                vec![MetricValue::labeled(3.0, MetricKind::Count, "available")]
            );
            assert_eq!(
                expiries_at,
                vec![
                    DateTime::parse_from_rfc3339("2026-10-01T00:00:00+00:00")
                        .unwrap()
                        .with_timezone(&Utc),
                    DateTime::parse_from_rfc3339("2026-10-01T00:00:00+00:00")
                        .unwrap()
                        .with_timezone(&Utc),
                    DateTime::parse_from_rfc3339("2026-10-22T16:00:00+00:00")
                        .unwrap()
                        .with_timezone(&Utc),
                ]
            );
        }
        other => panic!("{other:?}"),
    }

    let open = map_reset_grants(&response(
        200,
        r#"{"cedar_ember":{"eligible":true,"grants":[{"id":"g","resets_left":1,"ends_at":null}]}}"#,
    ))
    .unwrap()
    .unwrap();
    match resets_line(&open, now) {
        MetricLine::Values {
            values,
            expiries_at,
            ..
        } => {
            assert_eq!(values[0].number, 1.0);
            assert!(expiries_at.is_empty());
        }
        other => panic!("{other:?}"),
    }

    let ineligible = map_reset_grants(&response(
        200,
        r#"{"cedar_ember":{"eligible":false,"grants":[{"id":"g","resets_left":1}]}}"#,
    ))
    .unwrap()
    .unwrap();
    match resets_line(&ineligible, now) {
        MetricLine::Values {
            values,
            expiries_at,
            ..
        } => {
            assert_eq!(values[0].number, 0.0);
            assert!(expiries_at.is_empty());
        }
        other => panic!("{other:?}"),
    }

    assert!(map_reset_grants(&response(200, r#"{"cedar_ember":null}"#))
        .unwrap()
        .is_none());
    assert!(
        map_reset_grants(&response(200, r#"{"five_hour":{"utilization":3}}"#))
            .unwrap()
            .is_none()
    );
    assert!(map_reset_grants(&response(
        200,
        r#"{"cedar_ember":{"eligible":true,"grants":[{"id":"g","resets_left":1e300}]}}"#
    ))
    .unwrap()
    .is_none());
}

#[test]
fn rejected_grant_reads_fail_the_mapper() {
    assert_eq!(
        map_reset_grants(&response(401, "")).unwrap_err(),
        ProviderError::SignInExpired(ProviderKind::Claude)
    );
    assert_eq!(
        map_reset_grants(&response(429, "")).unwrap_err(),
        ProviderError::RateLimited {
            retry_after_seconds: None
        }
    );
    assert_eq!(
        map_reset_grants(&response(200, "not json")).unwrap_err(),
        ProviderError::InvalidResponse
    );
    assert_eq!(
        map_usage(&response(401, "")).unwrap_err(),
        ProviderError::SignInExpired(ProviderKind::Claude)
    );
}
