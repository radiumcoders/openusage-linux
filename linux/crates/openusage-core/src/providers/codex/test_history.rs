#[test]
fn a_personal_plan_shows_token_rows_and_rereads_on_the_hour_a_new_day_and_a_forced_refresh() {
    let script = Script::personal();
    let mut state = ProviderState::default();
    let now = at(NOW);
    let usage = fetch(&script, now, false, &mut state);
    assert_eq!(usage.plan.as_deref(), Some("Pro 200"));
    assert_eq!(
        token_rows(&usage),
        vec![
            ("Today".into(), 0.0),
            ("Yesterday".into(), 8_139_475.0),
            ("Last 30 Days".into(), 2_014_940_056.0)
        ]
    );
    let models = &usage.breakdowns["last30"].models;
    assert_eq!(models.iter().map(|model| model.name.as_str()).collect::<Vec<_>>(), ["gpt-6.1-sol", "gpt-6-astra"]);
    assert!(models.iter().all(|model| model.dollars.is_none() && model.tokens.is_none()));
    assert!((models[0].share.unwrap() - 55.0 / 478.0).abs() < 1e-9);
    assert!((models[1].share.unwrap() - 31.0 / 478.0).abs() < 1e-9);
    assert_eq!(
        history_paths(&script),
        vec!["/backend-api/wham/analytics/daily-workspace-usage-counts".to_string()]
    );
    let counts = script
        .paths
        .lock()
        .unwrap()
        .iter()
        .find(|path| path.contains("analytics"))
        .cloned()
        .unwrap();
    // The stored path has no query; check the last request URL via a second look at send. Re-read from a captured URL.
    let _ = counts;
    assert!(script
        .paths
        .lock()
        .unwrap()
        .iter()
        .any(|path| path.ends_with("daily-workspace-usage-counts")));

    fetch(
        &script,
        now + chrono::Duration::minutes(30),
        false,
        &mut state,
    );
    assert_eq!(history_paths(&script).len(), 1, "an hour has not passed");

    fetch(
        &script,
        now + chrono::Duration::minutes(61),
        false,
        &mut state,
    );
    assert_eq!(history_paths(&script).len(), 2);

    fetch(
        &script,
        now + chrono::Duration::minutes(62),
        true,
        &mut state,
    );
    assert_eq!(
        history_paths(&script).len(),
        3,
        "a forced refresh reads at once"
    );

    let before = history_paths(&script).len();
    fetch(&script, at(1_793_491_500), false, &mut state);
    assert_eq!(
        history_paths(&script).len(),
        before + 1,
        "a new UTC day is read at once"
    );
}

#[test]
fn history_requests_use_the_due_range_and_a_team_plan_also_reads_credits() {
    let urls = Arc::new(Mutex::new(Vec::new()));
    let seen = Arc::clone(&urls);
    let http = Fake::new(move |request| {
        seen.lock().unwrap().push(request.url.clone());
        let path = path_of(&request.url);
        let body = if path.ends_with("/usage") {
            USAGE.replace("\"pro\"", "\"team\"")
        } else if path.contains("token-usage-breakdown") {
            r#"{"data":[{"date":"2026-10-30","premium_usage_values":{"credit_usage_credits":{"cli":12.5}},"models":[{"model":"gpt-6-astra","on_demand_credits":10.0},{"model":"gpt-6.1-sol","on_demand_credits":2.5}]}]}"#.into()
        } else if path.contains("analytics") {
            COUNTS.into()
        } else {
            "{}".into()
        };
        Ok(response(200, &body))
    });
    let account = Account::new(ProviderKind::Codex, "ChatGPT", None);
    let vault = SharedVault::new(Credential::OAuth(account_tokens()));
    let mut state = ProviderState::default();
    let mut ctx = FetchContext {
        account: &account,
        http: &http,
        vault: &vault,
        state: &mut state,
        now: at(NOW),
        force: false,
    };
    let usage = provider().fetch(&mut ctx).unwrap();
    assert_eq!(token_rows(&usage)[1], ("Yesterday".into(), 8_139_475.0));
    let yesterday = &usage.breakdowns["yesterday"].models;
    assert_eq!(yesterday.iter().map(|model| model.name.as_str()).collect::<Vec<_>>(), ["gpt-6-astra", "gpt-6.1-sol"]);
    assert!((yesterday[0].share.unwrap() - 0.8).abs() < 1e-9);
    assert!((yesterday[1].share.unwrap() - 0.2).abs() < 1e-9);
    let urls = urls.lock().unwrap();
    let counts = urls
        .iter()
        .find(|url| url.contains("daily-workspace-usage-counts"))
        .unwrap();
    assert_eq!(
        query_of(counts),
        vec![
            "start_date=2026-10-02".to_string(),
            "end_date=2026-10-31".into(),
            "group_by=day".into(),
            "workspace_user=true".into()
        ]
    );
    let credits = urls
        .iter()
        .find(|url| url.contains("token-usage-breakdown"))
        .unwrap();
    assert_eq!(
        query_of(credits),
        vec![
            "start_date=2026-10-02".to_string(),
            "end_date=2026-10-31".into(),
            "group_by=day".into()
        ]
    );
}

#[test]
fn a_failed_or_limited_history_read_keeps_the_card_and_a_credits_failure_keeps_the_tokens() {
    let mode = Arc::new(Mutex::new("fail"));
    let flag = Arc::clone(&mode);
    let http = Fake::new(move |request| {
        let path = path_of(&request.url);
        if path.ends_with("/usage") {
            return Ok(response(200, USAGE));
        }
        if path.contains("analytics") {
            return Ok(match *flag.lock().unwrap() {
                "fail" => response(500, ""),
                "limit" => with_header(response(429, ""), "retry-after", "3600"),
                _ => response(200, COUNTS),
            });
        }
        Ok(response(200, "{}"))
    });
    let account = Account::new(ProviderKind::Codex, "ChatGPT", None);
    let vault = SharedVault::new(Credential::OAuth(account_tokens()));
    let mut state = ProviderState::default();
    let now = at(NOW);
    let mut ctx = FetchContext {
        account: &account,
        http: &http,
        vault: &vault,
        state: &mut state,
        now,
        force: false,
    };
    let usage = provider().fetch(&mut ctx).unwrap();
    assert!(token_rows(&usage).is_empty());
    assert!(usage.lines.iter().any(|line| line.label() == "Weekly"));

    let mut ctx = FetchContext {
        account: &account,
        http: &http,
        vault: &vault,
        state: &mut state,
        now: now + chrono::Duration::minutes(10),
        force: false,
    };
    provider().fetch(&mut ctx).unwrap();

    *mode.lock().unwrap() = "ok";
    let mut ctx = FetchContext {
        account: &account,
        http: &http,
        vault: &vault,
        state: &mut state,
        now: now + chrono::Duration::minutes(61),
        force: false,
    };
    let usage = provider().fetch(&mut ctx).unwrap();
    assert_eq!(
        token_rows(&usage)
            .iter()
            .find(|(label, _)| label == "Last 30 Days")
            .unwrap()
            .1,
        2_014_940_056.0
    );

    *mode.lock().unwrap() = "fail";
    let mut ctx = FetchContext {
        account: &account,
        http: &http,
        vault: &vault,
        state: &mut state,
        now: now + chrono::Duration::minutes(122),
        force: false,
    };
    let usage = provider().fetch(&mut ctx).unwrap();
    let rows = token_rows(&usage);
    assert!(rows.iter().all(|(label, _)| label == "Last 30 Days"));
    assert_eq!(rows[0].1, 2_006_800_581.0);

    *mode.lock().unwrap() = "limit";
    let mut limited = ProviderState::default();
    let mut ctx = FetchContext {
        account: &account,
        http: &http,
        vault: &vault,
        state: &mut limited,
        now,
        force: true,
    };
    assert!(provider()
        .fetch(&mut ctx)
        .unwrap()
        .lines
        .iter()
        .any(|line| line.label() == "Weekly"));
    *mode.lock().unwrap() = "ok";
    let mut ctx = FetchContext {
        account: &account,
        http: &http,
        vault: &vault,
        state: &mut limited,
        now: now + chrono::Duration::minutes(30),
        force: true,
    };
    assert!(
        token_rows(&provider().fetch(&mut ctx).unwrap()).is_empty(),
        "the 429 hold drops the due days and is not cleared by a forced refresh"
    );
}

#[test]
fn an_unauthorized_read_saves_the_renewed_token_before_retrying() {
    let stored = Arc::new(Mutex::new(Credential::OAuth(account_tokens())));
    let vault = SharedVault(Arc::clone(&stored));
    let seen = Arc::new(Mutex::new(Vec::new()));
    let record = Arc::clone(&seen);
    let slot = Arc::clone(&stored);
    let attempts = Arc::new(Mutex::new(0));
    let count = Arc::clone(&attempts);
    let http = Fake::new(move |request| {
        let path = path_of(&request.url);
        if path == "/oauth/token" {
            return Ok(response(
                200,
                r#"{"access_token":"new","refresh_token":"rotated"}"#,
            ));
        }
        if path.ends_with("/usage") {
            let n = {
                let mut n = count.lock().unwrap();
                *n += 1;
                *n
            };
            let Credential::OAuth(tokens) = slot.lock().unwrap().clone() else {
                unreachable!()
            };
            record.lock().unwrap().push(tokens.access_token);
            return Ok(if n == 1 {
                response(401, "")
            } else {
                response(200, USAGE)
            });
        }
        Ok(response(200, "{}"))
    });
    let account = Account::new(ProviderKind::Codex, "ChatGPT", None);
    let mut state = ProviderState::default();
    let mut ctx = FetchContext {
        account: &account,
        http: &http,
        vault: &vault,
        state: &mut state,
        now: at(NOW),
        force: false,
    };
    provider().fetch(&mut ctx).unwrap();
    assert_eq!(*seen.lock().unwrap(), ["access", "new"]);
    let Credential::OAuth(tokens) = stored.lock().unwrap().clone() else {
        unreachable!()
    };
    assert_eq!(tokens.refresh_token.as_deref(), Some("rotated"));
    assert!(
        tokens.expires_at.is_none(),
        "a renewed access token that is not a JWT has no expiry"
    );
}
