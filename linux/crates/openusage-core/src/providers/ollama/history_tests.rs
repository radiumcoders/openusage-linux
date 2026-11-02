use super::*;

fn measure(day: &ParsedDay, key: &str) -> f64 {
    day.totals.get(key).copied().unwrap_or(f64::NAN)
}

#[test]
fn days_parse_buckets_and_any_malformed_bucket_rejects_the_read() {
    let documented = r#"{"granularity":"day","buckets":[{"from":"2026-09-30T00:00:00Z","until":"2026-10-01T00:00:00Z","request_count":8,"usage_usd":0.0166,"input_tokens":52000,"cached_input_tokens":30000,"output_tokens":7000,"groups":[
        {"model":"glm-5.3-flash","request_count":6,"usage_usd":0.0066,"input_tokens":48000,"cached_input_tokens":30000,"output_tokens":6000},
        {"model":"glm-5.3","request_count":2,"usage_usd":0.01,"input_tokens":4000,"cached_input_tokens":0,"output_tokens":1000}
    ]}]}"#;
    let day_key = parse_time("2026-09-30T00:00:00Z").unwrap().timestamp();
    let parsed = days(documented.as_bytes())
        .unwrap()
        .remove(&day_key)
        .unwrap();
    assert_eq!(measure(&parsed, "requests"), 8.0);
    assert!((measure(&parsed, "usage") - 0.0166).abs() < 1e-9);
    assert_eq!(measure(&parsed, "tokens"), 59_000.0);
    assert_eq!(measure(&parsed, "input"), 52_000.0);
    assert_eq!(measure(&parsed, "cache"), 30_000.0);
    assert_eq!(measure(&parsed, "output"), 7_000.0);
    assert_eq!(
        parsed
            .by_model
            .as_ref()
            .unwrap()
            .get("glm-5.3")
            .unwrap()
            .get("tokens")
            .copied(),
        Some(5_000.0)
    );

    let legacy = r#"{"granularity":"day","buckets":[{"from":"2026-09-30T00:00:00Z","until":"2026-10-01T00:00:00Z","request_count":3}]}"#;
    let legacy = days(legacy.as_bytes()).unwrap().remove(&day_key).unwrap();
    assert_eq!(legacy.totals, BTreeMap::from([("requests".into(), 3.0)]));

    let unsplit = r#"{"granularity":"day","buckets":[{"from":"2026-09-30T00:00:00Z","until":"2026-10-01T00:00:00Z","request_count":9,"groups":[{"model":"a","request_count":1}],"remainder":{"request_count":8}}]}"#;
    assert!(days(unsplit.as_bytes())
        .unwrap()
        .remove(&day_key)
        .unwrap()
        .by_model
        .is_none());

    let empty_groups = r#"{"granularity":"day","buckets":[{"from":"2026-09-30T00:00:00Z","until":"2026-10-01T00:00:00Z","request_count":0,"groups":[]}]}"#;
    assert_eq!(
        days(empty_groups.as_bytes())
            .unwrap()
            .remove(&day_key)
            .unwrap()
            .by_model,
        Some(BTreeMap::new())
    );

    let day = r#""from":"2026-09-30T00:00:00Z","until":"2026-10-01T00:00:00Z""#;
    for bucket in [
        format!(r#"{{{day}}}"#),
        format!(r#"{{{day},"request_count":-1}}"#),
        format!(r#"{{{day},"request_count":1,"usage_usd":"lots"}}"#),
        format!(r#"{{{day},"request_count":1,"groups":[{{"request_count":1}}]}}"#),
        format!(
            r#"{{{day},"request_count":2,"groups":[{{"model":"a","request_count":1}},{{"model":"a","request_count":1}}]}}"#
        ),
        r#"{"from":"2026-09-30T02:00:00Z","until":"2026-09-30T03:00:00Z","request_count":1}"#
            .into(),
        r#"{"from":"2026-09-30T00:00:00Z","until":"2026-10-02T00:00:00Z","request_count":1}"#
            .into(),
        format!(r#"{{{day},"request_count":1}},{{{day},"request_count":1}}"#),
    ] {
        assert!(
            days(format!(r#"{{"granularity":"day","buckets":[{bucket}]}}"#).as_bytes()).is_none(),
            "{bucket}"
        );
    }
    assert!(days(br#"{"granularity":"hour","buckets":[{"from":"2026-09-30T00:00:00Z","until":"2026-10-01T00:00:00Z","request_count":1}]}"#).is_none());
}

#[test]
fn a_successful_read_covers_omitted_days_without_inventing_figures() {
    let now = DateTime::from_timestamp(1_793_392_200, 0).unwrap();
    let today = midnight(now).timestamp();
    let yesterday = today - DAY;
    let body = format!(
        r#"{{"granularity":"day","buckets":[{{"from":"{}","until":"{}","request_count":3}}]}}"#,
        stamp(yesterday),
        stamp(yesterday + DAY)
    );
    let parsed = days(body.as_bytes()).unwrap();
    let covered: BTreeMap<_, _> = [today, yesterday]
        .into_iter()
        .map(|day| {
            (
                day,
                parsed
                    .get(&day)
                    .map(|parsed| parsed.totals.clone())
                    .unwrap_or_default(),
            )
        })
        .collect();
    assert!(covered[&today].is_empty());
    assert_eq!(covered[&yesterday].get("requests").copied(), Some(3.0));
}

fn stamp(seconds: i64) -> String {
    DateTime::from_timestamp(seconds, 0)
        .unwrap()
        .format("%Y-%m-%dT%H:%M:%SZ")
        .to_string()
}
