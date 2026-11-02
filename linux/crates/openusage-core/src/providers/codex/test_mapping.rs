#[test]
fn windows_are_read_by_duration_and_a_header_fills_a_missing_slot() {
    let now = at(1_800_000_000);
    let weekly = map(
        r#"{"rate_limit":{"primary_window":{"used_percent":30,"limit_window_seconds":604800,"reset_after_seconds":100}}}"#,
    );
    assert_eq!(
        meters(&weekly.lines),
        vec![("Weekly".into(), 30.0, Some(604_800_000))]
    );
    match &weekly.lines[0] {
        MetricLine::Progress { resets_at, .. } => {
            assert_eq!(*resets_at, Some(now + chrono::Duration::seconds(100)))
        }
        other => panic!("{other:?}"),
    }

    let swapped = map(
        r#"{"rate_limit":{"primary_window":{"used_percent":70,"limit_window_seconds":604800},"secondary_window":{"used_percent":20,"limit_window_seconds":18000}}}"#,
    );
    assert_eq!(
        meters(&swapped.lines),
        vec![
            ("Session".into(), 20.0, Some(18_000_000)),
            ("Weekly".into(), 70.0, Some(604_800_000))
        ]
    );

    let unfamiliar = map(
        r#"{"rate_limit":{"primary_window":{"used_percent":10,"limit_window_seconds":3600},"secondary_window":{"used_percent":40,"limit_window_seconds":2592000}}}"#,
    );
    assert_eq!(
        meters(&unfamiliar.lines),
        vec![
            ("Session".into(), 10.0, Some(3_600_000)),
            ("Weekly".into(), 40.0, Some(2_592_000_000))
        ]
    );

    let borrowed = map(
        r#"{"rate_limit":{"primary_window":{"limit_window_seconds":604800},"secondary_window":{"used_percent":40,"limit_window_seconds":86400}}}"#,
    );
    assert!(meters(&borrowed.lines).is_empty());

    let header = mapper::map_usage(
        &with_header(
            response(200, r#"{"rate_limit":{}}"#),
            "x-codex-primary-used-percent",
            "55",
        ),
        None,
        now,
    )
    .unwrap();
    assert_eq!(
        meters(&header.lines),
        vec![("Session".into(), 55.0, Some(18_000_000))]
    );

    let huge = map(
        r#"{"rate_limit":{"primary_window":{"used_percent":42,"limit_window_seconds":1e300}},"rate_limit_reset_credits":{"available_count":1e300},"credits":{"balance":1e300}}"#,
    );
    assert_eq!(
        huge.lines.iter().map(MetricLine::label).collect::<Vec<_>>(),
        ["Session"]
    );
    assert_eq!(
        meters(&huge.lines),
        vec![("Session".into(), 42.0, Some(18_000_000))]
    );
}

#[test]
fn resets_credits_spark_and_the_credit_limit_use_the_descriptor_labels() {
    let now = at(1_800_000_000);
    let mapped = mapper::map_usage(
        &response(200, r#"{"plan_type":"pro","rate_limit_reset_credits":{"available_count":1},"credits":{"balance":100},"additional_rate_limits":[{"metered_feature":"Spark","rate_limit":{"primary_window":{"used_percent":15,"limit_window_seconds":18000}}}]}"#),
        None,
        now,
    )
    .unwrap();
    assert_eq!(mapped.plan.as_deref(), Some("Pro 200"));
    assert_eq!(
        mapped
            .lines
            .iter()
            .map(MetricLine::label)
            .collect::<Vec<_>>(),
        ["Spark", "Rate Limit Resets", "Credits"]
    );
    match &mapped.lines[2] {
        MetricLine::Values { values, .. } => {
            assert!((values[0].number - 4.0).abs() < 1e-9);
            assert_eq!(values[0].kind, MetricKind::Dollars);
            assert_eq!(
                values[1],
                MetricValue::labeled(100.0, MetricKind::Count, "credits")
            );
        }
        other => panic!("{other:?}"),
    }

    let zero = map(
        r#"{"rate_limit_reset_credits":{"available_count":0},"credits":{"has_credits":false}}"#,
    );
    assert_eq!(
        zero.lines.iter().map(MetricLine::label).collect::<Vec<_>>(),
        ["Rate Limit Resets", "Credits"]
    );
    assert!(
        map(r#"{"rate_limit_reset_credits":{"available_count":null}}"#)
            .lines
            .is_empty()
    );

    let dedicated = response(
        200,
        r#"{"available_count":2,"credits":[
            {"status":"available","expires_at":"2026-02-20T19:00:00.000Z"},
            {"expires_at":"2026-02-20T17:30:00.000Z"},
            {"status":"consumed","expires_at":"2026-02-20T16:10:00.000Z"}
        ]}"#,
    );
    let mapped =
        mapper::map_usage(&response(200, "{}"), Some(&dedicated), at(1_771_600_000)).unwrap();
    match &mapped.lines[0] {
        MetricLine::Values {
            values,
            expiries_at,
            ..
        } => {
            assert_eq!(
                *values,
                vec![MetricValue::labeled(2.0, MetricKind::Count, "available")]
            );
            assert_eq!(expiries_at.len(), 2);
            assert!(expiries_at[0] < expiries_at[1]);
        }
        other => panic!("{other:?}"),
    }

    let limit = map(
        r#"{"spend_control":{"individual_limit":{"limit":"25000","used":"8000","reset_at":1793520000}},"plan_type":"self_serve_business_prolite"}"#,
    );
    assert_eq!(limit.plan.as_deref(), Some("Business Premium"));
    match &limit.lines[0] {
        MetricLine::Progress {
            label,
            used,
            limit,
            format: ProgressFormat::Count { suffix },
            resets_at,
            ..
        } => {
            assert_eq!(label, "Credit Limit");
            assert_eq!(*used, 8000.0);
            assert_eq!(*limit, 25000.0);
            assert_eq!(suffix, "credits");
            assert_eq!(*resets_at, Some(at(1_793_520_000)));
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(
        mapper::format_plan(Some("prolite")).as_deref(),
        Some("Pro 100")
    );
    assert_eq!(
        mapper::format_plan(Some("promax")).as_deref(),
        Some("Pro 500")
    );
    assert_eq!(
        mapper::format_plan(Some("plus_ultra")).as_deref(),
        Some("Plus Ultra")
    );
}

#[test]
fn rejected_usage_reads_fail_the_mapper() {
    assert_eq!(
        mapper::map_usage(&response(401, ""), None, at(0)).unwrap_err(),
        ProviderError::SignInExpired(ProviderKind::Codex)
    );
    assert_eq!(
        mapper::map_usage(
            &with_header(response(429, ""), "retry-after", "12"),
            None,
            at(0)
        )
        .unwrap_err(),
        ProviderError::RateLimited {
            retry_after_seconds: Some(12.0)
        }
    );
}

#[test]
fn refresh_percent_encodes_the_body_and_takes_expiry_from_the_access_token() {
    let http = Fake::new(|request| {
        assert_eq!(request.url, client::TOKEN_URL);
        assert_eq!(request.method, "POST");
        assert_eq!(
            std::str::from_utf8(request.body.as_deref().unwrap()).unwrap(),
            &format!("grant_type=refresh_token&client_id={CLIENT_ID}&refresh_token=refresh%20token%26%3D%2B%2F%3F%25")
        );
        Ok(response(
            200,
            &format!(
                r#"{{"access_token":"{}","expires_in":10}}"#,
                jwt(r#"{"exp":1800000000}"#)
            ),
        ))
    });
    let grant = client::refresh("refresh token&=+/?%", &http).unwrap();
    assert_eq!(grant.expires_at, Some(at(1_800_000_000)));

    let refused = Fake::new(|_| {
        Ok(response(
            400,
            r#"{"error":{"code":"refresh_token_expired"}}"#,
        ))
    });
    assert_eq!(
        client::refresh("refresh", &refused).unwrap_err(),
        ProviderError::SignInExpired(ProviderKind::Codex)
    );

    let plain = Fake::new(|_| Ok(response(200, r#"{"access_token":"new-token"}"#)));
    assert_eq!(client::refresh("refresh", &plain).unwrap().expires_at, None);
}
