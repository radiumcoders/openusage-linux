use super::*;

fn line(id: &str, request: &str, sidechain: bool, output: u64, extra: &str) -> String {
    format!(
        concat!(
            r#"{{"timestamp":"2026-11-03T05:00:00.000Z","sessionId":"s","version":"2.1.312","#,
            r#""requestId":"{request}","isSidechain":{sidechain},"message":{{"id":"{id}","#,
            r#""model":"claude-opus-5-5","content":[{{"type":"text","text":"hi"}}],"#,
            r#""usage":{{"input_tokens":1000000,"output_tokens":{output},"#,
            r#""cache_read_input_tokens":0,"cache_creation_input_tokens":0{extra}}}}}}}"#
        ),
        id = id,
        request = request,
        sidechain = sidechain,
        output = output,
        extra = extra,
    )
}

#[test]
fn a_usage_line_parses_into_priced_buckets_and_advisors_count_on_their_own() {
    let text = line(
        "m1",
        "r1",
        false,
        0,
        r#","speed":"fast","iterations":[{"type":"message","model":null,"input_tokens":5,"output_tokens":5},{"type":"advisor_message","model":"claude-fable-5-1","input_tokens":1000000,"output_tokens":0}]"#,
    );
    let entries = parse_entries(&text);
    assert_eq!(entries.len(), 2);
    assert!(entries[0].tokens.fast && entries[0].has_speed);
    assert_eq!(entries[1].model.as_deref(), Some("claude-fable-5-1"));
    assert_eq!(entries[1].message_id.as_deref(), Some("m1:advisor:0"));
    let since = entries[0].timestamp - Duration::days(1);
    let days = aggregate(entries, since, Pricing::bundled());
    let day = days.values().next().unwrap();
    // Opus 5.5 fast: 1M input at $4 x2 = $8; Fable 5.1 advisor: 1M input at $10.
    assert!((day.dollars - 18.0).abs() < 1e-9, "{}", day.dollars);
    assert_eq!(day.models.len(), 2);
}

#[test]
fn foreign_or_malformed_lines_are_skipped() {
    assert!(parse_entries(&line("m1", "r1", false, 1, r#","speed":"warp""#)).is_empty());
    assert!(parse_entries(&line("", "r1", false, 1, "")).is_empty());
    let null_cache = line("m1", "r1", false, 1, "").replace(
        r#""cache_read_input_tokens":0"#,
        r#""cache_read_input_tokens":null"#,
    );
    assert!(parse_entries(&null_cache).is_empty());
    let bad_version = line("m1", "r1", false, 1, "").replace("2.1.312", "unknown");
    assert!(parse_entries(&bad_version).is_empty());
    assert!(is_semver_prefix("1.0.24-beta"));
    assert!(!is_semver_prefix("1.0"));
}

#[test]
fn replayed_lines_count_once_and_the_parent_wins_over_a_sidechain() {
    let mut entries = parse_entries(&line("m1", "r1", false, 10, ""));
    entries.extend(parse_entries(&line("m1", "r1", false, 10, "")));
    entries.extend(parse_entries(&line("m1", "r2", true, 99, "")));
    entries.extend(parse_entries(&line("m2", "r3", false, 10, "")));
    let kept = dedup(entries);
    assert_eq!(kept.len(), 2);
    assert!(!kept[0].sidechain);
    assert_eq!(kept[0].tokens.output, 10);
}

#[test]
fn a_grown_file_is_read_from_where_the_last_read_stopped() {
    let dir = std::env::temp_dir().join(format!("openusage-claude-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("session.jsonl");
    let since = DateTime::parse_from_rfc3339("2026-11-01T00:00:00Z")
        .unwrap()
        .with_timezone(&Utc);
    // The second line is still being written (no newline yet).
    let first = line("m1", "r1", false, 1, "");
    let partial = line("m2", "r2", false, 1, "");
    std::fs::write(&path, format!("{first}\n{}", &partial[..40])).unwrap();
    let meta = std::fs::metadata(&path).unwrap();
    let file = refresh_file(&path, meta.len(), meta.modified().unwrap(), None, since);
    assert_eq!(file.entries.len(), 1);
    assert_eq!(file.offset, first.len() as u64 + 1);

    std::fs::write(&path, format!("{first}\n{partial}\n")).unwrap();
    let meta = std::fs::metadata(&path).unwrap();
    let file = refresh_file(&path, meta.len(), meta.modified().unwrap(), Some(file), since);
    assert_eq!(file.entries.len(), 2);
    assert_eq!(file.offset, meta.len());
}

#[test]
fn history_rows_sum_today_yesterday_and_the_window() {
    let now = Utc::now();
    let today = now.with_timezone(&Local).date_naive();
    let mut days = BTreeMap::new();
    for (back, dollars) in [(0, 2.0), (1, 3.0), (10, 5.0), (40, 100.0)] {
        let mut day = Day {
            dollars,
            tokens: dollars * 1000.0,
            ..Default::default()
        };
        day.models.insert("claude-opus-5-5".into(), (dollars, dollars * 1000.0));
        days.insert(today - Duration::days(back), day);
    }
    let (lines, breakdowns) = history_lines(&days, now);
    let dollars = |index: usize| match &lines[index] {
        MetricLine::Values { values, .. } => values[0].number,
        _ => panic!(),
    };
    assert_eq!(lines[0].label(), "Today");
    assert_eq!(dollars(0), 2.0);
    assert_eq!(dollars(1), 3.0);
    assert_eq!(dollars(2), 10.0);
    assert_eq!(breakdowns["last30"].models[0].dollars, Some(10.0));
}
