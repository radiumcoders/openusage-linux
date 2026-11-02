use super::*;

fn thursday() -> DateTime<Utc> {
    DateTime::parse_from_rfc3339("2026-10-29T11:00:00Z")
        .unwrap()
        .with_timezone(&Utc)
}

fn at(text: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(text)
        .unwrap()
        .with_timezone(&Utc)
}

#[test]
fn a_key_response_missing_what_the_card_needs_is_invalid() {
    for text in [
        r#"{"data":{"usage_daily":1.5,"usage_weekly":4,"usage_monthly":12}}"#,
        r#"{"data":{"is_management_key":false,"usage_daily":1.5,"usage_weekly":4}}"#,
        r#"{"data":{"is_management_key":false,"usage_daily":1.5,"usage_weekly":4,"usage_monthly":12,"limit":10,"limit_remaining":null}}"#,
        r#"{"error":{"code":500,"message":"Internal Server Error"}}"#,
    ] {
        assert_eq!(
            key(text.as_bytes(), thursday()).err(),
            Some(ProviderError::InvalidResponse),
            "{text}"
        );
    }
}

#[test]
fn the_key_limit_resets_at_midnight_utc_with_weeks_starting_monday() {
    let cases = [
        (r#""daily""#, Some("2026-10-30T00:00:00Z"), Some(DAY_MS)),
        (
            r#""weekly""#,
            Some("2026-11-02T00:00:00Z"),
            Some(7 * DAY_MS),
        ),
        (
            r#""monthly""#,
            Some("2026-11-01T00:00:00Z"),
            Some(31 * DAY_MS),
        ),
        ("null", None, None),
    ];
    for (reset, resets_at, period) in cases {
        let body = format!(
            r#"{{"data":{{"is_management_key":false,"usage_daily":0,"usage_weekly":0,"usage_monthly":0,"limit":10,"limit_remaining":7.5,"limit_reset":{reset}}}}}"#
        );
        let line = key(body.as_bytes(), thursday())
            .unwrap()
            .lines
            .into_iter()
            .find(|line| line.label() == "Key Limit")
            .unwrap();
        assert_eq!(
            line,
            MetricLine::Progress {
                label: "Key Limit".into(),
                used: 2.5,
                limit: 10.0,
                format: ProgressFormat::Dollars,
                resets_at: resets_at.map(at),
                period_duration_ms: period,
            },
            "{reset}"
        );
    }
    let absent = key(
            br#"{"data":{"is_management_key":false,"usage_daily":0,"usage_weekly":0,"usage_monthly":0,"limit":null,"limit_remaining":null,"limit_reset":null}}"#,
            thursday(),
        )
        .unwrap();
    assert!(absent.lines.iter().all(|line| line.label() != "Key Limit"));
}

#[test]
fn credits_show_only_the_balance() {
    assert_eq!(
        credits_lines(br#"{"data":{"total_credits":100,"total_usage":22.25}}"#).unwrap(),
        vec![MetricLine::values(
            "Balance",
            vec![MetricValue::new(77.75, MetricKind::Dollars)]
        )]
    );
    assert_eq!(
        credits_lines(br#"{"data":{"total_credits":0,"total_usage":0}}"#).unwrap(),
        vec![MetricLine::values(
            "Balance",
            vec![MetricValue::new(0.0, MetricKind::Dollars)]
        )]
    );
    assert_eq!(
        credits_lines(br#"{"data":{"total_credits":10,"total_usage":12.5}}"#).unwrap(),
        vec![MetricLine::values(
            "Balance",
            vec![MetricValue::new(0.0, MetricKind::Dollars)]
        )]
    );
    assert!(credits_lines(br#"{"data":{"total_credits":100}}"#).is_err());
}

#[test]
fn a_keys_page_must_be_a_list_of_keys_with_their_spend() {
    let page = keys_page(super::keys_page_body(2, 0.25, 0.5, 1.0).as_bytes()).unwrap();
    let spend = Spend {
        today: WindowSpend {
            spend: 0.25,
            byok: Some(0.0),
        },
        week: WindowSpend {
            spend: 0.5,
            byok: Some(0.0),
        },
        month: WindowSpend {
            spend: 1.0,
            byok: Some(0.0),
        },
    };
    assert_eq!(page, vec![spend.clone(), spend]);
    assert!(keys_page(br#"{"data":[]}"#).unwrap().is_empty());
    assert!(keys_page(br#"{"data":{"usage_daily":1}}"#).is_err());
    assert!(keys_page(br#"{"data":[{"hash":"abc","usage_daily":1,"usage_weekly":2}]}"#).is_err());
}

#[test]
fn activity_sums_rows_per_utc_day_and_a_malformed_row_rejects_the_read() {
    let rows = r#"{"data":[
            {"date":"2026-10-28","model":"a","usage":1,"requests":2,"prompt_tokens":10,"completion_tokens":5},
            {"date":"2026-10-28","model":"a","usage":0.5,"requests":1,"prompt_tokens":4,"completion_tokens":1,"cached_tokens":2,"reasoning_tokens":null}
        ]}"#;
    let day = activity_days(rows.as_bytes())
        .unwrap()
        .remove(&at("2026-10-28T00:00:00Z").timestamp())
        .unwrap();
    assert_eq!(day.totals.get("spend").copied(), Some(1.5));
    assert_eq!(day.totals.get("tokens").copied(), Some(20.0));
    assert_eq!(day.totals.get("input").copied(), Some(12.0));
    assert_eq!(day.totals.get("output").copied(), Some(6.0));
    assert_eq!(day.totals.get("cache").copied(), Some(2.0));
    assert_eq!(day.totals.get("reasoning").copied(), Some(0.0));
    assert_eq!(day.totals.get("byok").copied(), Some(0.0));
    assert_eq!(
        day.by_model.get("a").unwrap().get("tokens").copied(),
        Some(20.0)
    );
    for text in [
        r#"{"data":{"date":"2026-10-28"}}"#,
        r#"{"data":[{"date":"2026-10-28","model":"a","usage":1,"prompt_tokens":10}]}"#,
        r#"{"data":[{"date":"2026-10-28","model":"a","usage":-1,"requests":1,"prompt_tokens":10,"completion_tokens":5}]}"#,
        r#"{"data":[{"date":"yesterday","model":"a","usage":1,"requests":1,"prompt_tokens":10,"completion_tokens":5}]}"#,
    ] {
        assert!(activity_days(text.as_bytes()).is_none(), "{text}");
    }
}
