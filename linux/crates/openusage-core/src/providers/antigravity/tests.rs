use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::{Arc, Mutex};
use std::thread;

use chrono::{DateTime, Duration, Utc};

use super::client::CLIENT_ID;
use super::mapper;
use super::*;
use crate::account::{Account, Credential, OAuthTokens};
use crate::error::ProviderError;
use crate::http::{HttpClient, HttpRequest, HttpResponse};
use crate::kind::ProviderKind;
use crate::login::{LoginPrompt, LoginPrompter};
use crate::metric::MetricLine;
use crate::provider::{FetchContext, LoginMethod, ProviderState};
use crate::vault::{MemoryVault, Vault};

fn used(line: &MetricLine) -> f64 {
    match line {
        MetricLine::Progress { used, .. } => *used,
        other => panic!("{other:?}"),
    }
}

fn labels(lines: &[MetricLine]) -> Vec<&str> {
    lines.iter().map(MetricLine::label).collect()
}

#[test]
fn merged_summaries_take_the_first_usable_bucket() {
    let first = br#"{"groups":[{"buckets":[{"bucketId":"gemini-5h"},{"bucketId":"3p-5h","remainingFraction":0.5}]}]}"#;
    let second = br#"{"groups":[{"buckets":[{"bucketId":"gemini-5h","remainingFraction":0.75},{"bucketId":"3p-5h","remainingFraction":0.1}]}]}"#;
    let lines =
        mapper::parse_quota_summaries(&[first.to_vec(), b"not json".to_vec(), second.to_vec()])
            .unwrap();
    assert_eq!(labels(&lines), vec!["Session", "Claude"]);
    assert_eq!(lines.iter().map(used).collect::<Vec<_>>(), vec![25.0, 50.0]);
    assert!(mapper::parse_quota_summaries(&[b"{}".to_vec()]).is_none());
}

#[test]
fn merged_summaries_take_argon_from_the_first_body() {
    let argon = |fraction: f64| {
        format!(
            r#"{{"groups":[{{"displayName":"Gemini 4 Argon","buckets":[{{"bucketId":"gemini-argon-wtu-weekly","window":"weekly","remainingFraction":{fraction}}}]}}]}}"#
        )
    };
    let lines =
        mapper::parse_quota_summaries(&[argon(0.6).into_bytes(), argon(0.1).into_bytes()]).unwrap();
    assert_eq!(labels(&lines), vec!["Argon Weekly"]);
    assert_eq!(used(&lines[0]), 40.0);
}

#[test]
fn a_complete_summary_needs_all_four_usable_buckets() {
    let complete = br#"{"response":{"groups":[{"buckets":[{"bucketId":"gemini-5h","remainingFraction":1},{"bucketId":"gemini-weekly","remainingFraction":1},{"bucketId":"3p-5h","remainingFraction":1},{"bucketId":"3p-weekly","remainingFraction":1}]}]}}"#;
    assert!(mapper::is_complete_summary(complete));
    let missing = br#"{"groups":[{"buckets":[{"bucketId":"gemini-5h","remainingFraction":1},{"bucketId":"3p-5h","remainingFraction":1},{"bucketId":"3p-weekly","remainingFraction":1}]}]}"#;
    assert!(!mapper::is_complete_summary(missing));
    let no_fraction = br#"{"groups":[{"buckets":[{"bucketId":"gemini-5h","remainingFraction":1},{"bucketId":"gemini-weekly"},{"bucketId":"3p-5h","remainingFraction":1},{"bucketId":"3p-weekly","remainingFraction":1}]}]}"#;
    assert!(!mapper::is_complete_summary(no_fraction));
    assert!(!mapper::is_complete_summary(b"not json"));
    let weekly_in_argon = br#"{"groups":[{"buckets":[{"bucketId":"gemini-5h","remainingFraction":1},{"bucketId":"3p-5h","remainingFraction":1},{"bucketId":"3p-weekly","remainingFraction":1}]},{"displayName":"Gemini 4 Argon","buckets":[{"bucketId":"gemini-weekly","window":"weekly","remainingFraction":1}]}]}"#;
    assert!(!mapper::is_complete_summary(weekly_in_argon));
}

fn summary(group: &str, buckets: &str) -> Vec<u8> {
    format!(
        r#"{{"groups":[{{"displayName":"{group}","buckets":[{buckets}]}},{{"displayName":"Gemini Models","buckets":[{{"bucketId":"gemini-5h","remainingFraction":0.8}},{{"bucketId":"gemini-weekly","remainingFraction":0.7}}]}},{{"displayName":"Claude and GPT models","buckets":[{{"bucketId":"3p-5h","remainingFraction":0.6}},{{"bucketId":"3p-weekly","remainingFraction":0.5}}]}}]}}"#
    )
    .into_bytes()
}

fn shared_only(body: Vec<u8>) {
    let lines = mapper::parse_quota_summaries(&[body]).unwrap();
    assert_eq!(
        labels(&lines),
        vec!["Session", "Weekly", "Claude", "Claude Weekly"]
    );
    assert_eq!(
        lines.iter().map(used).collect::<Vec<_>>(),
        vec![20.0, 30.0, 40.0, 50.0]
    );
}

#[test]
fn unavailable_argon_preserves_the_shared_pools() {
    shared_only(summary("Unrelated group", ""));
    for buckets in [
        "",
        r#"{"bucketId":"test-id","window":"weekly"}"#,
        r#"{"bucketId":"test-id","window":"weekly","remainingFraction":"unknown"}"#,
        r#"{"bucketId":"test-id","window":"weekly","remainingFraction":1,"disabled":true}"#,
        r#"{"bucketId":"test-id","window":"weekly","remainingAmount":100}"#,
    ] {
        shared_only(summary("Gemini 4 Argon", buckets));
    }
    for group in ["Gemini Models", "Gemini 4 Argon Images", "Future Models"] {
        shared_only(summary(
            group,
            r#"{"bucketId":"test-id","window":"weekly","remainingFraction":0.1}"#,
        ));
    }
    for buckets in [
        r#"{"bucketId":"test-id","remainingFraction":0.1}"#,
        r#"{"bucketId":"test-id","window":"5h","remainingFraction":0.1}"#,
        r#"{"window":"weekly","remainingFraction":0.1}"#,
        r#"{"bucketId":"","window":"weekly","remainingFraction":0.1}"#,
        r#"{"bucketId":"gemini-weekly","window":"weekly","remainingFraction":0.1}"#,
    ] {
        shared_only(summary("Gemini 4 Argon", buckets));
    }
}

#[test]
fn the_disabled_flag_rejects_malformed_values_and_keeps_omission_null_and_false() {
    for value in ["true", r#""true""#, r#""false""#, "1", "0"] {
        shared_only(summary(
            "Gemini 4 Argon",
            &format!(
                r#"{{"bucketId":"test-id","window":"weekly","remainingFraction":0.5,"disabled":{value}}}"#
            ),
        ));
    }
    for field in ["", r#", "disabled":null"#, r#", "disabled":false"#] {
        let lines = mapper::parse_quota_summaries(&[summary(
            "Gemini 4 Argon",
            &format!(
                r#"{{"bucketId":"test-id","window":"weekly","remainingFraction":0.5{field}}}"#
            ),
        )])
        .unwrap();
        let argon = lines
            .iter()
            .find(|line| line.label() == "Argon Weekly")
            .unwrap();
        assert_eq!(
            argon,
            &MetricLine::percent("Argon Weekly", 50.0, None, Some(7 * 86_400_000))
        );
    }
}

#[test]
fn ambiguous_weekly_buckets_do_not_merge_and_a_five_hour_bucket_does_not_override_weekly() {
    shared_only(summary(
        "Gemini 4 Argon",
        r#"{"bucketId":"test-first","window":"weekly","remainingFraction":0.9},{"bucketId":"test-second","window":"weekly","remainingFraction":0.1}"#,
    ));
    let lines = mapper::parse_quota_summaries(&[summary(
        "Gemini 4 Argon",
        r#"{"bucketId":"test-session","window":"5h","remainingFraction":0},{"bucketId":"test-weekly","window":"weekly","remainingFraction":0.5}"#,
    )])
    .unwrap();
    assert_eq!(
        labels(&lines),
        vec![
            "Session",
            "Weekly",
            "Argon Weekly",
            "Claude",
            "Claude Weekly"
        ]
    );
    assert_eq!(
        lines
            .iter()
            .find(|line| line.label() == "Argon Weekly")
            .unwrap(),
        &MetricLine::percent("Argon Weekly", 50.0, None, Some(7 * 86_400_000))
    );
}

#[test]
fn an_argon_only_summary_uses_the_weekly_period_and_zero_remaining_is_used_up() {
    let body = br#"{"groups":[{"displayName":"Gemini 4 Argon","buckets":[{"bucketId":"test-id","window":"weekly","remainingFraction":0,"disabled":false}]}]}"#;
    let lines = mapper::parse_quota_summaries(&[body.to_vec()]).unwrap();
    assert_eq!(
        lines,
        vec![MetricLine::percent(
            "Argon Weekly",
            100.0,
            None,
            Some(7 * 86_400_000)
        )]
    );
}

#[test]
fn an_argon_weekly_bucket_parses_bare_and_wrapped() {
    let group = r#"{"displayName":"Gemini 4 Argon","buckets":[{"bucketId":"gemini-argon-wtu-weekly","window":"weekly","remainingFraction":1.0,"resetTime":"2026-10-22T23:24:33Z"}]}"#;
    let reset = chrono::DateTime::parse_from_rfc3339("2026-10-22T23:24:33Z")
        .unwrap()
        .with_timezone(&Utc);
    let expected = MetricLine::percent("Argon Weekly", 0.0, Some(reset), Some(7 * 86_400_000));
    for json in [
        format!(r#"{{"groups":[{group}]}}"#),
        format!(r#"{{"response":{{"groups":[{group}]}}}}"#),
    ] {
        assert_eq!(
            mapper::parse_quota_summaries(&[json.into_bytes()]).unwrap(),
            vec![expected.clone()]
        );
    }
}

#[test]
fn a_malformed_group_name_does_not_invalidate_shared_buckets() {
    let body = br#"{"groups":[{"displayName":42,"buckets":[{"bucketId":"gemini-5h","remainingFraction":0.8}]}]}"#;
    let lines = mapper::parse_quota_summaries(&[body.to_vec()]).unwrap();
    assert_eq!(labels(&lines), vec!["Session"]);
}

const LOAD: &str = r#"{"cloudaicompanionProject":"fake-project-123","currentTier":{"id":"free-tier","name":"Antigravity Starter Quota"},"paidTier":{"id":"free-tier","name":"Antigravity Starter Quota"}}"#;
const QUOTA: &str = r#"{"groups":[{"displayName":"Gemini","buckets":[{"bucketId":"gemini-weekly","remainingFraction":0.75,"resetTime":"2026-10-27T00:00:00Z","window":"weekly"}]},{"displayName":"Claude","buckets":[{"bucketId":"3p-weekly","remainingFraction":0.5,"resetTime":"2026-10-27T00:00:00Z","window":"weekly"}]}]}"#;
const SUBSCRIPTION: &str = r#"{"error":{"code":403,"message":"fake","status":"PERMISSION_DENIED","details":[{"reason":"SUBSCRIPTION_REQUIRED"}]}}"#;

struct Api {
    requests: Mutex<Vec<HttpRequest>>,
    load_status: Mutex<u16>,
    quota: Mutex<QuotaMode>,
}

#[derive(Clone, Copy, PartialEq)]
enum QuotaMode {
    Project,
    Forbidden(&'static str),
    Rate,
    UnauthorizedThenOk,
    Transport,
}

impl HttpClient for Api {
    fn send(&self, request: HttpRequest) -> Result<HttpResponse, ProviderError> {
        self.requests.lock().unwrap().push(request.clone());
        if request.url.contains("/token") {
            return Ok(HttpResponse {
                status: 200,
                headers: Default::default(),
                body: br#"{"access_token":"new-access","expires_in":3600}"#.to_vec(),
            });
        }
        if *self.quota.lock().unwrap() == QuotaMode::Transport {
            return Err(ProviderError::ConnectionFailed);
        }
        let path = url::Url::parse(&request.url).unwrap().path().to_string();
        if path.contains("loadCodeAssist") {
            let status = *self.load_status.lock().unwrap();
            return Ok(HttpResponse {
                status,
                headers: Default::default(),
                body: if status == 200 {
                    LOAD.as_bytes().to_vec()
                } else {
                    Vec::new()
                },
            });
        }
        match *self.quota.lock().unwrap() {
            QuotaMode::Rate => {
                let mut headers = std::collections::BTreeMap::new();
                headers.insert("retry-after".into(), "120".into());
                Ok(HttpResponse {
                    status: 429,
                    headers,
                    body: Vec::new(),
                })
            }
            QuotaMode::Forbidden(body) => Ok(HttpResponse {
                status: 403,
                headers: Default::default(),
                body: body.as_bytes().to_vec(),
            }),
            QuotaMode::UnauthorizedThenOk => {
                let bearer = request.headers.get("Authorization").map(String::as_str);
                if bearer == Some("Bearer new-access") {
                    Ok(HttpResponse {
                        status: 200,
                        headers: Default::default(),
                        body: QUOTA.as_bytes().to_vec(),
                    })
                } else {
                    Ok(HttpResponse {
                        status: 401,
                        headers: Default::default(),
                        body: Vec::new(),
                    })
                }
            }
            QuotaMode::Project | QuotaMode::Transport => {
                let body: serde_json::Value =
                    serde_json::from_slice(request.body.as_deref().unwrap_or(b"{}"))
                        .unwrap_or(serde_json::Value::Null);
                if body.get("project").and_then(|project| project.as_str())
                    == Some("fake-project-123")
                {
                    Ok(HttpResponse {
                        status: 200,
                        headers: Default::default(),
                        body: QUOTA.as_bytes().to_vec(),
                    })
                } else {
                    Ok(HttpResponse {
                        status: 403,
                        headers: Default::default(),
                        body: SUBSCRIPTION.as_bytes().to_vec(),
                    })
                }
            }
        }
    }
}

fn tokens(access: &str) -> OAuthTokens {
    let mut tokens = OAuthTokens::new(access);
    tokens.refresh_token = Some("old-refresh".into());
    tokens.expires_at = Some(Utc::now() + Duration::days(30));
    tokens
}

fn fetch(
    http: &Api,
    access: &str,
    state: &mut ProviderState,
) -> Result<crate::metric::ProviderUsage, ProviderError> {
    let account = Account::new(ProviderKind::Antigravity, "Google Account", None);
    let vault = MemoryVault::with(account.id, Credential::OAuth(tokens(access)));
    let mut ctx = FetchContext {
        account: &account,
        http,
        vault: &vault,
        state,
        now: Utc::now(),
        force: false,
    };
    provider().fetch(&mut ctx)
}

#[test]
fn quota_summary_names_the_accounts_project() {
    let http = Api {
        requests: Mutex::new(vec![]),
        load_status: Mutex::new(200),
        quota: Mutex::new(QuotaMode::Project),
    };
    let usage = fetch(&http, "access", &mut ProviderState::default()).unwrap();
    assert_eq!(labels(&usage.lines), vec!["Weekly", "Claude Weekly"]);
    assert_eq!(usage.plan.as_deref(), Some("Antigravity Starter Quota"));
    let quota = http
        .requests
        .lock()
        .unwrap()
        .iter()
        .find(|request| request.url.contains("retrieveUserQuotaSummary"))
        .unwrap()
        .clone();
    let body: serde_json::Value = serde_json::from_slice(quota.body.as_deref().unwrap()).unwrap();
    assert_eq!(body["project"], "fake-project-123");
    assert_eq!(
        quota
            .headers
            .get("User-Agent")
            .map(|agent| agent.contains("os_type=linux")),
        Some(true)
    );
}

#[test]
fn quota_access_denial_reports_the_reason_google_supplied() {
    let http = Api {
        requests: Mutex::new(vec![]),
        load_status: Mutex::new(200),
        quota: Mutex::new(QuotaMode::Forbidden(SUBSCRIPTION)),
    };
    let error = fetch(&http, "access", &mut ProviderState::default()).unwrap_err();
    assert_eq!(
        error.to_string(),
        "Google reports no Antigravity quota subscription for this project."
    );
    let http = Api {
        requests: Mutex::new(vec![]),
        load_status: Mutex::new(200),
        quota: Mutex::new(QuotaMode::Forbidden(
            r#"{"error":{"status":"PERMISSION_DENIED"}}"#,
        )),
    };
    assert_eq!(
        fetch(&http, "access", &mut ProviderState::default()).unwrap_err(),
        ProviderError::SignInExpired(ProviderKind::Antigravity)
    );
}

#[test]
fn every_base_offline_is_a_connection_failure() {
    let http = Api {
        requests: Mutex::new(vec![]),
        load_status: Mutex::new(200),
        quota: Mutex::new(QuotaMode::Transport),
    };
    assert_eq!(
        fetch(&http, "access", &mut ProviderState::default()).unwrap_err(),
        ProviderError::ConnectionFailed
    );
    let quota = http
        .requests
        .lock()
        .unwrap()
        .iter()
        .filter(|request| request.url.contains("retrieveUserQuotaSummary"))
        .count();
    assert_eq!(quota, 2);
}

#[test]
fn both_bases_rate_limited_keep_their_retry_after() {
    let http = Api {
        requests: Mutex::new(vec![]),
        load_status: Mutex::new(200),
        quota: Mutex::new(QuotaMode::Rate),
    };
    assert_eq!(
        fetch(&http, "access", &mut ProviderState::default()).unwrap_err(),
        ProviderError::RateLimited {
            retry_after_seconds: Some(120.0)
        }
    );
}

#[test]
fn an_unauthorized_quota_read_renews_once_and_retries() {
    let account = Account::new(ProviderKind::Antigravity, "Google Account", None);
    let vault = MemoryVault::with(account.id, Credential::OAuth(tokens("old-access")));
    let http = Api {
        requests: Mutex::new(vec![]),
        load_status: Mutex::new(200),
        quota: Mutex::new(QuotaMode::UnauthorizedThenOk),
    };
    let mut state = ProviderState::default();
    let usage = {
        let mut ctx = FetchContext {
            account: &account,
            http: &http,
            vault: &vault,
            state: &mut state,
            now: Utc::now(),
            force: false,
        };
        provider().fetch(&mut ctx).unwrap()
    };
    assert_eq!(labels(&usage.lines), vec!["Weekly", "Claude Weekly"]);
    assert_eq!(
        http.requests
            .lock()
            .unwrap()
            .iter()
            .filter(|request| request.url.contains("/token"))
            .count(),
        1
    );
    let Credential::OAuth(saved) = vault.load(account.id).unwrap().unwrap() else {
        panic!("oauth")
    };
    assert_eq!(saved.access_token, "new-access");
    assert_eq!(saved.refresh_token.as_deref(), Some("old-refresh"));
}

#[test]
fn a_failed_project_lookup_is_read_again_on_the_next_refresh() {
    let http = Api {
        requests: Mutex::new(vec![]),
        load_status: Mutex::new(503),
        quota: Mutex::new(QuotaMode::Project),
    };
    let mut state = ProviderState::default();
    let error = fetch(&http, "access", &mut state).unwrap_err();
    assert_eq!(error.to_string(), "Antigravity couldn't find a project with quota access for this account. Try again shortly.");
    *http.load_status.lock().unwrap() = 200;
    let usage = fetch(&http, "access", &mut state).unwrap();
    assert_eq!(labels(&usage.lines), vec!["Weekly", "Claude Weekly"]);
    assert_eq!(usage.plan.as_deref(), Some("Antigravity Starter Quota"));
}

#[test]
fn paid_tiers_read_as_google_sells_them() {
    let plan = |id: &str, name: &str| {
        mapper::parse_plan(format!(r#"{{"paidTier":{{"id":"{id}","name":"{name}"}}}}"#).as_bytes())
    };
    assert_eq!(
        plan("g1-pro-tier", "fake").as_deref(),
        Some("Google AI Pro")
    );
    assert_eq!(
        plan("g1-ultra-tier", "fake").as_deref(),
        Some("Google AI Ultra")
    );
    assert_eq!(
        plan("g1-ultra-lite-tier", "fake").as_deref(),
        Some("Google AI Ultra Lite")
    );
    assert_eq!(
        plan("unknown-tier", "Google AI Pro").as_deref(),
        Some("Google AI Pro")
    );
    assert_eq!(
        plan("unknown-tier", "Gemini Code Assist in Google One AI Pro").as_deref(),
        Some("Google AI Pro")
    );
    assert_eq!(
        plan("unknown-tier", "Google AI Ultra Lite").as_deref(),
        Some("Google AI Ultra Lite")
    );
    assert_eq!(
        plan("free-tier", "Antigravity Starter Quota").as_deref(),
        Some("Antigravity Starter Quota")
    );
    assert!(plan("unknown-tier", "").is_none());
}

struct BrowserPrompt {
    shown: Mutex<Vec<LoginPrompt>>,
    callback: Mutex<Option<thread::JoinHandle<u16>>>,
}

impl LoginPrompter for BrowserPrompt {
    fn show(&self, prompt: &LoginPrompt) {
        self.shown.lock().unwrap().push(prompt.clone());
        let LoginPrompt::Browser { url, .. } = prompt else {
            return;
        };
        let url = url::Url::parse(url).unwrap();
        let query: std::collections::BTreeMap<_, _> = url.query_pairs().into_owned().collect();
        let state = query.get("state").cloned().unwrap_or_default();
        let handle = thread::spawn(move || {
            let mut stream = TcpStream::connect("127.0.0.1:51121").expect("callback");
            let request = format!("GET /oauth-callback?code=fake-authorization-code&state={state} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n");
            stream.write_all(request.as_bytes()).unwrap();
            let mut buffer = String::new();
            stream.read_to_string(&mut buffer).unwrap();
            buffer.split_whitespace().nth(1).unwrap().parse().unwrap()
        });
        *self.callback.lock().unwrap() = Some(handle);
    }
    fn read_line(&self, _secret: bool) -> Option<String> {
        None
    }
    fn pasted_codes(&self) -> std::sync::mpsc::Receiver<String> {
        std::sync::mpsc::channel().1
    }
}

#[test]
fn login_exchanges_the_code_and_maps_the_google_profile() {
    struct Token(Arc<Mutex<Vec<HttpRequest>>>);
    impl HttpClient for Token {
        fn send(&self, request: HttpRequest) -> Result<HttpResponse, ProviderError> {
            self.0.lock().unwrap().push(request.clone());
            let path = url::Url::parse(&request.url).unwrap().path().to_string();
            let body = if path == "/token" {
                br#"{"access_token":"google-access","refresh_token":"google-refresh","expires_in":3600}"#.to_vec()
            } else if path.contains("userinfo") {
                br#"{"email":"google@example.com"}"#.to_vec()
            } else {
                return Ok(HttpResponse {
                    status: 404,
                    headers: Default::default(),
                    body: b"{}".to_vec(),
                });
            };
            Ok(HttpResponse {
                status: 200,
                headers: Default::default(),
                body,
            })
        }
    }
    let seen = Arc::new(Mutex::new(Vec::new()));
    let http = Token(seen.clone());
    let prompt = BrowserPrompt {
        shown: Mutex::new(vec![]),
        callback: Mutex::new(None),
    };
    let started = Utc::now();
    let (label, credential) = login(&LoginMethod::Default).login(&prompt, &http).unwrap();
    let status = prompt
        .callback
        .lock()
        .unwrap()
        .take()
        .unwrap()
        .join()
        .unwrap();
    assert_eq!(status, 200);
    assert_eq!(label, "google@example.com");
    let LoginPrompt::Browser { url, .. } = &prompt.shown.lock().unwrap()[0] else {
        panic!("browser")
    };
    let authorize = url::Url::parse(url).unwrap();
    let query: std::collections::BTreeMap<_, _> = authorize.query_pairs().into_owned().collect();
    assert_eq!(
        query.get("scope").map(String::as_str),
        Some("https://www.googleapis.com/auth/cloud-platform https://www.googleapis.com/auth/userinfo.email")
    );
    let token = seen
        .lock()
        .unwrap()
        .iter()
        .find(|request| request.url.contains("/token"))
        .unwrap()
        .clone();
    let form: std::collections::BTreeMap<_, _> =
        url::form_urlencoded::parse(token.body.as_deref().unwrap())
            .into_owned()
            .collect();
    assert_eq!(
        form.get("grant_type").map(String::as_str),
        Some("authorization_code")
    );
    assert_eq!(form.get("client_id").map(String::as_str), Some(CLIENT_ID));
    assert_eq!(
        form.get("code").map(String::as_str),
        Some("fake-authorization-code")
    );
    assert_eq!(
        form.get("redirect_uri").map(String::as_str),
        Some("http://localhost:51121/oauth-callback")
    );
    let verifier = form.get("code_verifier").cloned().unwrap();
    assert!(!verifier.is_empty());
    assert_ne!(
        query.get("state").map(String::as_str),
        Some(verifier.as_str())
    );
    assert!(!url.contains(&verifier));
    let Credential::OAuth(tokens) = credential else {
        panic!("oauth")
    };
    assert_eq!(tokens.access_token, "google-access");
    assert_eq!(tokens.refresh_token.as_deref(), Some("google-refresh"));
    assert_eq!(
        tokens
            .extras
            .get(OAuthTokens::IDENTITY_KEY)
            .map(String::as_str),
        Some("google@example.com")
    );
    let expires = tokens.expires_at.unwrap();
    assert!(expires >= started + Duration::seconds(3590));
    let _ = DateTime::<Utc>::UNIX_EPOCH;
}
