use std::sync::Mutex;
use std::time::Duration;

use chrono::{DateTime, Utc};

use super::*;
use crate::account::Credential;
use crate::http::{HttpRequest, HttpResponse};
use crate::kind::ProviderKind;
use crate::metric::{MetricKind, MetricLine, MetricValue, ProviderSnapshot};
use crate::providers::claude::mapper as claude_mapper;
use crate::providers::codex::mapper as codex_mapper;
use crate::vault::MemoryVault;

fn at(seconds: i64) -> DateTime<Utc> {
    DateTime::from_timestamp(seconds, 0).unwrap()
}

fn response(status: u16, body: &str) -> HttpResponse {
    HttpResponse {
        status,
        headers: Default::default(),
        body: body.as_bytes().to_vec(),
    }
}

struct Api {
    requests: Mutex<Vec<HttpRequest>>,
    steps: Mutex<Vec<Result<HttpResponse, ProviderError>>>,
}

impl Api {
    fn new(steps: Vec<Result<HttpResponse, ProviderError>>) -> Self {
        Self {
            requests: Mutex::new(vec![]),
            steps: Mutex::new(steps),
        }
    }

    fn urls(&self) -> Vec<String> {
        self.requests
            .lock()
            .unwrap()
            .iter()
            .map(|request| request.url.clone())
            .collect()
    }
}

impl HttpClient for Api {
    fn send(&self, request: HttpRequest) -> Result<HttpResponse, ProviderError> {
        self.requests.lock().unwrap().push(request);
        self.steps.lock().unwrap().remove(0)
    }
}

fn oauth(access: &str, refresh: Option<&str>, expires: Option<DateTime<Utc>>) -> OAuthTokens {
    let mut tokens = OAuthTokens::new(access);
    tokens.refresh_token = refresh.map(str::to_string);
    tokens.expires_at = expires;
    tokens
}

fn claude_account() -> (Account, MemoryVault) {
    let account = Account::new(ProviderKind::Claude, "Claude", None);
    let mut tokens = oauth("access", Some("refresh"), Some(at(4_000_000_000)));
    tokens
        .extras
        .insert(OAuthTokens::IDENTITY_KEY.into(), "acct/org-1".into());
    let vault = MemoryVault::with(account.id, Credential::OAuth(tokens));
    (account, vault)
}

fn codex_account() -> (Account, MemoryVault) {
    let account = Account::new(ProviderKind::Codex, "ChatGPT", None);
    let mut tokens = oauth("access", Some("refresh"), Some(at(4_000_000_000)));
    tokens
        .extras
        .insert("chatgpt_account_id".into(), "acct-fake".into());
    let vault = MemoryVault::with(account.id, Credential::OAuth(tokens));
    (account, vault)
}

const GRANTS: &str = r#"{"cedar_ember":{"eligible":true,"next_grant_id":"grant_1","grants":[
    {"id":"grant_1","resets_left":1,"clears":["five_hour","seven_day"],"ends_at":"2026-12-01T00:00:00Z"}
]}}"#;

fn ok(status: u16, body: &'static str) -> Result<HttpResponse, ProviderError> {
    Ok(response(status, body))
}

#[test]
fn claude_claim_matches_claude_code_and_spends_the_next_grant() {
    let (account, vault) = claude_account();
    let http = Api::new(vec![
        ok(200, GRANTS),
        ok(200, r#"{"result":"reset","resets_left":0}"#),
    ]);
    assert_eq!(claim(&account, &vault, &http, None), ClaimOutcome::Success);
    let requests = http.requests.lock().unwrap();
    assert!(requests[0].url.contains("cedar_ember=1"));
    assert_eq!(requests[0].timeout, Duration::from_secs(10));
    assert_eq!(
        requests[0].headers.get("User-Agent").map(String::as_str),
        Some("claude-cli/2.1.294 (external, cli)")
    );
    assert_eq!(
        requests[0]
            .headers
            .get("anthropic-beta")
            .map(String::as_str),
        Some("oauth-2025-04-20")
    );
    assert_eq!(
        requests[1].url,
        "https://api.anthropic.com/api/organizations/org-1/reset_rate_limits"
    );
    assert_eq!(requests[1].method, "POST");
    assert_eq!(requests[1].timeout, Duration::from_secs(25));
    let body =
        serde_json::from_slice::<serde_json::Value>(requests[1].body.as_ref().unwrap()).unwrap();
    assert_eq!(body["program"], "cedar_ember");
    assert_eq!(body["grant_id"], "grant_1");
    let request_id = body["request_id"].as_str().unwrap();
    assert!(request_id_ok(request_id));
    assert!(requests[1]
        .headers
        .get("Authorization")
        .unwrap()
        .ends_with("access"));
}

#[test]
fn claude_outcomes_match_the_protocol_table() {
    let cases = [
        (200, r#"{"result":"reset"}"#, ClaimOutcome::Success),
        (200, r#"{"result":"already_used"}"#, ClaimOutcome::Success),
        (
            200,
            r#"{"result":"not_limited"}"#,
            ClaimOutcome::NothingToReset,
        ),
        (200, r#"{"result":"ineligible"}"#, ClaimOutcome::NoCredit),
        (200, r#"{"result":"no_grant"}"#, ClaimOutcome::NoCredit),
        (200, r#"{"result":"unknown_grant"}"#, ClaimOutcome::NoCredit),
        (
            200,
            r#"{"result":"not_next_grant"}"#,
            ClaimOutcome::NoCredit,
        ),
        (
            200,
            r#"{"result":"reset_unconfirmed"}"#,
            ClaimOutcome::Unconfirmed,
        ),
        (
            200,
            r#"{"result":"stamp_indeterminate"}"#,
            ClaimOutcome::Unconfirmed,
        ),
        (
            200,
            r#"{"result":"cooldown","cooldown_until":"2026-12-01T00:00:00Z"}"#,
            ClaimOutcome::UnavailableNow {
                until: Some(at(1_796_083_200)),
            },
        ),
        (
            200,
            r#"{"result":"paused"}"#,
            ClaimOutcome::UnavailableNow { until: None },
        ),
        (
            200,
            r#"{"result":"low_priority_active"}"#,
            ClaimOutcome::UnavailableNow { until: None },
        ),
        (
            200,
            r#"{"result":"unavailable","reason":"cooldown"}"#,
            ClaimOutcome::UnavailableNow { until: None },
        ),
        (
            200,
            r#"{"result":"unavailable","reason":"nope"}"#,
            ClaimOutcome::Failed(ClaimFailure::Refused),
        ),
        (
            200,
            r#"{"result":"other"}"#,
            ClaimOutcome::Failed(ClaimFailure::Refused),
        ),
        (200, r#"{}"#, ClaimOutcome::Failed(ClaimFailure::Refused)),
        (500, r#"{}"#, ClaimOutcome::Failed(ClaimFailure::Refused)),
        (
            401,
            r#"{}"#,
            ClaimOutcome::Failed(ClaimFailure::SignInExpired),
        ),
        (
            403,
            r#"{"error":"OAuth token does not meet scope requirement"}"#,
            ClaimOutcome::Failed(ClaimFailure::NotPermitted),
        ),
    ];
    for (status, body, expected) in cases {
        assert_eq!(
            claude_mapper::reset_claim_outcome(&response(status, body)),
            expected,
            "{body}"
        );
    }
}

#[test]
fn claude_offer_uses_the_snapshot_confirm_detail_and_expiries() {
    let now = at(1_790_000_000);
    let status = claude_mapper::map_reset_grants(&response(200, GRANTS))
        .unwrap()
        .unwrap();
    assert_eq!(
        claude_mapper::confirm_detail(&status).as_deref(),
        Some("Refills your session and weekly limits now. This can't be undone.")
    );
    let account = Account::new(ProviderKind::Claude, "Claude", None);
    let snapshot = ProviderSnapshot {
        provider_id: account.card_id(),
        display_name: "Claude".into(),
        plan: None,
        lines: vec![claude_mapper::resets_line(&status, now)],
        breakdowns: Default::default(),
        notice: None,
        refreshed_at: now,
    };
    let offered = offer(&account, Some(&snapshot)).unwrap();
    assert_eq!(
        offered.confirm_detail.as_deref(),
        Some("Refills your session and weekly limits now. This can't be undone.")
    );
    assert_eq!(offered.expiries, vec![at(1_796_083_200)]);

    let ineligible = claude_mapper::map_reset_grants(&response(
        200,
        r#"{"cedar_ember":{"eligible":false,"next_grant_id":"grant_1","grants":[{"id":"grant_1","resets_left":1}]}}"#,
    ))
    .unwrap()
    .unwrap();
    let snapshot = ProviderSnapshot {
        lines: vec![claude_mapper::resets_line(&ineligible, now)],
        ..snapshot
    };
    assert!(offer(&account, Some(&snapshot)).is_none());
    assert!(offer(
        &Account::new(ProviderKind::Cursor, "Cursor", None),
        Some(&snapshot)
    )
    .is_none());
}

#[test]
fn claude_claim_refreshes_an_expired_token_and_saves_it_before_the_claim() {
    let account = Account::new(ProviderKind::Claude, "Claude", None);
    let mut tokens = oauth(
        "old",
        Some("refresh"),
        Some(Utc::now() - chrono::Duration::minutes(1)),
    );
    tokens
        .extras
        .insert(OAuthTokens::IDENTITY_KEY.into(), "acct/org-1".into());
    let vault = MemoryVault::with(account.id, Credential::OAuth(tokens));
    let http = Api::new(vec![
        ok(
            200,
            r#"{"access_token":"new","refresh_token":"rotated","expires_in":3600}"#,
        ),
        ok(200, GRANTS),
        ok(200, r#"{"result":"already_used"}"#),
    ]);
    assert_eq!(claim(&account, &vault, &http, None), ClaimOutcome::Success);
    let urls = http.urls();
    assert!(urls[0].contains("oauth/token"));
    assert!(urls[2].contains("reset_rate_limits"));
    let saved = vault.load(account.id).unwrap().unwrap();
    let Credential::OAuth(saved) = saved else {
        panic!("api key")
    };
    assert_eq!(saved.access_token, "new");
    assert_eq!(saved.refresh_token.as_deref(), Some("rotated"));
    let claim = &http.requests.lock().unwrap()[2];
    assert!(claim.headers.get("Authorization").unwrap().ends_with("new"));
}

#[test]
fn claude_claim_without_an_organization_reads_the_profile_once() {
    let account = Account::new(ProviderKind::Claude, "Claude", None);
    let vault = MemoryVault::with(
        account.id,
        Credential::OAuth(oauth("access", Some("refresh"), Some(at(4_000_000_000)))),
    );
    let http = Api::new(vec![
        ok(200, GRANTS),
        ok(
            200,
            r#"{"account":{"uuid":"acct"},"organization":{"uuid":"org-9","organization_type":"claude_max"}}"#,
        ),
        ok(200, r#"{"result":"reset"}"#),
    ]);
    assert_eq!(claim(&account, &vault, &http, None), ClaimOutcome::Success);
    assert!(http.urls()[1].ends_with("/api/oauth/profile"));
    assert!(http.urls()[2].contains("/organizations/org-9/"));
    let Credential::OAuth(saved) = vault.load(account.id).unwrap().unwrap() else {
        panic!("api key")
    };
    assert_eq!(
        saved
            .extras
            .get(OAuthTokens::IDENTITY_KEY)
            .map(String::as_str),
        Some("acct/org-9")
    );
}

#[test]
fn a_cut_short_claude_token_renews_once_and_retries_the_same_request() {
    let (account, vault) = claude_account();
    let http = Api::new(vec![
        ok(200, GRANTS),
        ok(401, ""),
        ok(200, r#"{"access_token":"renewed","expires_in":3600}"#),
        ok(200, r#"{"result":"reset"}"#),
    ]);
    assert_eq!(claim(&account, &vault, &http, None), ClaimOutcome::Success);
    let requests = http.requests.lock().unwrap();
    assert_eq!(requests.len(), 4);
    assert!(requests[1].url.contains("reset_rate_limits"));
    assert!(requests[2].url.contains("oauth/token"));
    assert!(requests[3].url.contains("reset_rate_limits"));
    let first =
        serde_json::from_slice::<serde_json::Value>(requests[1].body.as_ref().unwrap()).unwrap();
    let second =
        serde_json::from_slice::<serde_json::Value>(requests[3].body.as_ref().unwrap()).unwrap();
    assert_eq!(first["request_id"], second["request_id"]);
    assert!(requests[3]
        .headers
        .get("Authorization")
        .unwrap()
        .ends_with("renewed"));
}

#[test]
fn a_scope_refusal_says_the_sign_in_cant_use_resets() {
    let (account, vault) = claude_account();
    let http = Api::new(vec![
        ok(200, GRANTS),
        ok(403, "OAuth token does not meet scope requirement"),
    ]);
    assert_eq!(
        claim(&account, &vault, &http, None),
        ClaimOutcome::Failed(ClaimFailure::NotPermitted)
    );
    assert_eq!(http.urls().len(), 2);
}

#[test]
fn a_missing_credential_fails_without_a_request() {
    let account = Account::new(ProviderKind::Claude, "Claude", None);
    let vault = MemoryVault::default();
    let http = Api::new(vec![]);
    assert_eq!(
        claim(&account, &vault, &http, None),
        ClaimOutcome::Failed(ClaimFailure::SignInExpired)
    );
    assert!(http.urls().is_empty());
}

#[test]
fn an_unsettled_claude_claim_reuses_its_request_id() {
    let (account, vault) = claude_account();
    let http = Api::new(vec![ok(200, GRANTS), Err(ProviderError::ConnectionFailed)]);
    assert_eq!(
        claim(&account, &vault, &http, None),
        ClaimOutcome::Failed(ClaimFailure::Connection)
    );
    let first = serde_json::from_slice::<serde_json::Value>(
        http.requests.lock().unwrap()[1].body.as_ref().unwrap(),
    )
    .unwrap();
    let http = Api::new(vec![ok(200, GRANTS), ok(500, "")]);
    assert_eq!(
        claim(&account, &vault, &http, None),
        ClaimOutcome::Failed(ClaimFailure::Refused)
    );
    let second = serde_json::from_slice::<serde_json::Value>(
        http.requests.lock().unwrap()[1].body.as_ref().unwrap(),
    )
    .unwrap();
    assert_eq!(first["request_id"], second["request_id"]);
    let http = Api::new(vec![
        ok(200, GRANTS),
        ok(200, r#"{"result":"already_used"}"#),
    ]);
    assert_eq!(claim(&account, &vault, &http, None), ClaimOutcome::Success);
    let third = serde_json::from_slice::<serde_json::Value>(
        http.requests.lock().unwrap()[1].body.as_ref().unwrap(),
    )
    .unwrap();
    assert_eq!(first["request_id"], third["request_id"]);
    let http = Api::new(vec![ok(200, GRANTS), ok(200, r#"{"result":"reset"}"#)]);
    assert_eq!(claim(&account, &vault, &http, None), ClaimOutcome::Success);
    let fourth = serde_json::from_slice::<serde_json::Value>(
        http.requests.lock().unwrap()[1].body.as_ref().unwrap(),
    )
    .unwrap();
    assert_ne!(first["request_id"], fourth["request_id"]);
}

#[test]
fn no_claude_claim_without_a_claimable_grant_and_a_bad_grant_id_is_refused() {
    let (account, vault) = claude_account();
    let http = Api::new(vec![ok(
        200,
        r#"{"cedar_ember":{"eligible":true,"grants":[{"id":"grant_1","resets_left":1}]}}"#,
    )]);
    assert_eq!(claim(&account, &vault, &http, None), ClaimOutcome::NoCredit);
    assert_eq!(http.urls().len(), 1);

    let http = Api::new(vec![ok(
        200,
        r#"{"cedar_ember":{"eligible":true,"next_grant_id":"BAD","grants":[{"id":"BAD","resets_left":1}]}}"#,
    )]);
    assert_eq!(
        claim(&account, &vault, &http, None),
        ClaimOutcome::Failed(ClaimFailure::Refused)
    );
    assert_eq!(http.urls().len(), 1);
}

#[test]
fn codex_consume_matches_the_credit_and_the_protocol_codes() {
    let expiry = at(1_800_000_000);
    let list = format!(
        r#"{{"available_count":2,"credits":[
            {{"id":"credit-a","status":"available","expires_at":"{}"}},
            {{"id":"credit-b","expires_at":"{}"}},
            {{"id":"spent","status":"redeemed","expires_at":"{}"}}
        ]}}"#,
        expiry.to_rfc3339(),
        (expiry + chrono::Duration::seconds(2)).to_rfc3339(),
        expiry.to_rfc3339()
    );
    assert_eq!(
        codex_mapper::reset_credit_id(list.as_bytes(), expiry).as_deref(),
        Some("credit-a")
    );
    assert_eq!(
        codex_mapper::reset_credit_id(
            list.as_bytes(),
            expiry + chrono::Duration::milliseconds(400)
        )
        .as_deref(),
        Some("credit-a"),
        "the first credit within a second wins"
    );
    assert_eq!(
        codex_mapper::reset_credit_id(
            list.as_bytes(),
            expiry + chrono::Duration::milliseconds(1_600)
        )
        .as_deref(),
        Some("credit-b"),
        "a missing status still counts as available"
    );
    assert!(
        codex_mapper::reset_credit_id(list.as_bytes(), expiry + chrono::Duration::seconds(4))
            .is_none()
    );

    let cases = [
        (200, r#"{"code":"reset"}"#, ClaimOutcome::Success),
        (200, r#"{"code":"already_redeemed"}"#, ClaimOutcome::Success),
        (
            200,
            r#"{"code":"nothing_to_reset"}"#,
            ClaimOutcome::NothingToReset,
        ),
        (200, r#"{"code":"no_credit"}"#, ClaimOutcome::NoCredit),
        (
            200,
            r#"{"code":"nope"}"#,
            ClaimOutcome::Failed(ClaimFailure::Refused),
        ),
        (500, r#"{}"#, ClaimOutcome::Failed(ClaimFailure::Refused)),
        (
            401,
            r#"{}"#,
            ClaimOutcome::Failed(ClaimFailure::SignInExpired),
        ),
    ];
    for (status, body, expected) in cases {
        assert_eq!(
            codex_mapper::reset_claim_outcome(&response(status, body)),
            expected,
            "{body}"
        );
    }

    let (account, vault) = codex_account();
    let http = Api::new(vec![
        ok(200, Box::leak(list.into_boxed_str())),
        ok(200, r#"{"code":"reset"}"#),
    ]);
    assert_eq!(
        claim(&account, &vault, &http, Some(expiry)),
        ClaimOutcome::Success
    );
    let requests = http.requests.lock().unwrap();
    assert!(requests[0].url.ends_with("/rate-limit-reset-credits"));
    assert_eq!(requests[0].timeout, Duration::from_secs(10));
    assert_eq!(
        requests[0]
            .headers
            .get("ChatGPT-Account-Id")
            .map(String::as_str),
        Some("acct-fake")
    );
    assert_eq!(
        requests[0].headers.get("OpenAI-Beta").map(String::as_str),
        Some("codex-1")
    );
    assert_eq!(
        requests[0].headers.get("originator").map(String::as_str),
        Some("Codex Desktop")
    );
    assert_eq!(
        requests[0].headers.get("User-Agent").map(String::as_str),
        Some("OpenUsage")
    );
    assert!(requests[1]
        .url
        .ends_with("/rate-limit-reset-credits/consume"));
    assert_eq!(requests[1].timeout, Duration::from_secs(15));
    let body =
        serde_json::from_slice::<serde_json::Value>(requests[1].body.as_ref().unwrap()).unwrap();
    assert_eq!(body["credit_id"], "credit-a");
    assert!(request_id_ok(body["redeem_request_id"].as_str().unwrap()));
    assert_eq!(
        std::str::from_utf8(requests[1].body.as_ref().unwrap()).unwrap(),
        format!(
            r#"{{"credit_id":"credit-a","redeem_request_id":"{}"}}"#,
            body["redeem_request_id"].as_str().unwrap()
        )
    );
}

#[test]
fn codex_claim_failures_and_a_lost_consume_replay_the_same_credit() {
    let expiry = at(1_800_000_100);
    let list = format!(
        r#"{{"credits":[{{"id":"credit-a","status":"available","expires_at":"{}"}}]}}"#,
        expiry.to_rfc3339()
    );
    let leaked: &'static str = Box::leak(list.into_boxed_str());
    let (account, vault) = codex_account();

    let http = Api::new(vec![ok(200, leaked)]);
    assert_eq!(
        claim(
            &account,
            &vault,
            &http,
            Some(expiry + chrono::Duration::seconds(3))
        ),
        ClaimOutcome::NoCredit
    );
    assert_eq!(http.urls().len(), 1);

    let http = Api::new(vec![ok(503, "")]);
    assert_eq!(
        claim(&account, &vault, &http, Some(expiry)),
        ClaimOutcome::Failed(ClaimFailure::Refused)
    );
    let http = Api::new(vec![ok(401, ""), ok(401, r#"{"error":"invalid_grant"}"#)]);
    assert_eq!(
        claim(&account, &vault, &http, Some(expiry)),
        ClaimOutcome::Failed(ClaimFailure::SignInExpired)
    );

    let account = Account::new(ProviderKind::Codex, "ChatGPT", None);
    let vault = MemoryVault::default();
    let http = Api::new(vec![]);
    assert_eq!(
        claim(&account, &vault, &http, Some(expiry)),
        ClaimOutcome::Failed(ClaimFailure::SignInExpired)
    );

    let (account, vault) = codex_account();
    let http = Api::new(vec![ok(200, leaked), ok(500, "")]);
    assert_eq!(
        claim(&account, &vault, &http, Some(expiry)),
        ClaimOutcome::Failed(ClaimFailure::Refused)
    );
    let first = http.requests.lock().unwrap()[1].body.clone();
    let http = Api::new(vec![ok(200, r#"{"code":"already_redeemed"}"#)]);
    assert_eq!(
        claim(&account, &vault, &http, Some(expiry)),
        ClaimOutcome::Success
    );
    assert_eq!(http.urls().len(), 1, "a lost consume is not matched again");
    assert_eq!(http.requests.lock().unwrap()[0].body, first);

    let http = Api::new(vec![
        ok(200, leaked),
        ok(200, r#"{"code":"nothing_to_reset"}"#),
    ]);
    assert_eq!(
        claim(&account, &vault, &http, Some(expiry)),
        ClaimOutcome::NothingToReset
    );
    assert_eq!(
        http.urls().len(),
        2,
        "a settled claim matches the list again"
    );
}

#[test]
fn a_cut_short_codex_token_renews_once_and_lists_again() {
    let expiry = at(1_800_000_200);
    let list = format!(
        r#"{{"credits":[{{"id":"credit-a","status":"available","expires_at":"{}"}}]}}"#,
        expiry.to_rfc3339()
    );
    let leaked: &'static str = Box::leak(list.into_boxed_str());
    let (account, vault) = codex_account();
    let http = Api::new(vec![
        ok(401, ""),
        ok(200, r#"{"access_token":"renewed","refresh_token":"next"}"#),
        ok(200, leaked),
        ok(200, r#"{"code":"reset"}"#),
    ]);
    assert_eq!(
        claim(&account, &vault, &http, Some(expiry)),
        ClaimOutcome::Success
    );
    let urls = http.urls();
    assert!(urls[0].contains("rate-limit-reset-credits"));
    assert!(urls[1].contains("oauth/token"));
    assert!(urls[2].contains("rate-limit-reset-credits"));
    assert!(urls[3].ends_with("/consume"));
    assert!(http.requests.lock().unwrap()[2]
        .headers
        .get("Authorization")
        .unwrap()
        .ends_with("renewed"));
}

#[test]
fn codex_offer_uses_available_resets_without_a_confirm_detail() {
    let account = Account::new(ProviderKind::Codex, "ChatGPT", None);
    let later = at(1_800_000_000);
    let sooner = at(1_700_000_000);
    let snapshot = ProviderSnapshot {
        provider_id: account.card_id(),
        display_name: "ChatGPT".into(),
        plan: None,
        lines: vec![MetricLine::Values {
            label: "Rate Limit Resets".into(),
            values: vec![MetricValue::labeled(2.0, MetricKind::Count, "available")],
            expiries_at: vec![later, sooner],
            detail: None,
        }],
        breakdowns: Default::default(),
        notice: None,
        refreshed_at: sooner,
    };
    let offer = offer(&account, Some(&snapshot)).unwrap();
    assert_eq!(offer.confirm_detail, None);
    assert_eq!(offer.expiries, vec![sooner, later]);
}
