//! Fetch cadence, renewal, and sign-in. Mapper cases live in `mapper.rs`.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use chrono::{TimeZone, Utc};

use crate::account::{Account, Credential, OAuthTokens};
use crate::error::{LoginError, ProviderError};
use crate::http::{HttpClient, HttpRequest, HttpResponse};
use crate::kind::ProviderKind;
use crate::login::{LoginPrompt, LoginPrompter};
use crate::metric::MetricLine;
use crate::provider::{FetchContext, LoginMethod, ProviderState};
use crate::vault::Vault;

use super::client::{self, CLIENT_ID};
use super::provider;

fn at(seconds: i64) -> chrono::DateTime<Utc> {
    Utc.timestamp_opt(seconds, 0).single().unwrap()
}

fn tokens(access: &str, refresh: &str, expires: Option<chrono::DateTime<Utc>>) -> OAuthTokens {
    let mut tokens = OAuthTokens::new(access);
    tokens.refresh_token = Some(refresh.into());
    tokens.expires_at = expires;
    tokens
}

fn response(status: u16, body: &str) -> HttpResponse {
    let mut headers = std::collections::BTreeMap::new();
    if status == 429 {
        headers.insert("retry-after".into(), "0".into());
    }
    HttpResponse {
        status,
        headers,
        body: body.as_bytes().to_vec(),
    }
}

fn with_retry(status: u16, body: &str, retry_after: &str) -> HttpResponse {
    let mut response = response(status, body);
    response
        .headers
        .insert("retry-after".into(), retry_after.into());
    response
}

struct Fake {
    inner: Mutex<Box<dyn FnMut(&HttpRequest) -> Result<HttpResponse, ProviderError> + Send>>,
}

impl Fake {
    fn new(
        handler: impl FnMut(&HttpRequest) -> Result<HttpResponse, ProviderError> + Send + 'static,
    ) -> Self {
        Self {
            inner: Mutex::new(Box::new(handler)),
        }
    }
}

impl HttpClient for Fake {
    fn send(&self, request: HttpRequest) -> Result<HttpResponse, ProviderError> {
        (self.inner.lock().unwrap())(&request)
    }
}

struct SharedVault(Arc<Mutex<Credential>>);

impl SharedVault {
    fn new(credential: Credential) -> Self {
        Self(Arc::new(Mutex::new(credential)))
    }
}

impl Vault for SharedVault {
    fn load(&self, _id: uuid::Uuid) -> Result<Option<Credential>, ProviderError> {
        Ok(Some(self.0.lock().unwrap().clone()))
    }
    fn save(&self, _id: uuid::Uuid, credential: &Credential) -> Result<(), ProviderError> {
        *self.0.lock().unwrap() = credential.clone();
        Ok(())
    }
    fn delete(&self, _id: uuid::Uuid) -> Result<(), ProviderError> {
        Ok(())
    }
}

fn access_of(credential: &Credential) -> String {
    match credential {
        Credential::OAuth(tokens) => tokens.access_token.clone(),
        Credential::ApiKey(key) => key.clone(),
    }
}

struct Harness {
    account: Account,
    vault: SharedVault,
    state: ProviderState,
}

impl Harness {
    fn new(tokens: OAuthTokens) -> Self {
        let account = Account::new(ProviderKind::Claude, "Claude", None);
        Self {
            account,
            vault: SharedVault::new(Credential::OAuth(tokens)),
            state: ProviderState::default(),
        }
    }

    fn fetch(
        &mut self,
        http: &Fake,
        now: chrono::DateTime<Utc>,
        force: bool,
    ) -> Result<crate::metric::ProviderUsage, ProviderError> {
        let mut ctx = FetchContext {
            account: &self.account,
            http,
            vault: &self.vault,
            state: &mut self.state,
            now,
            force,
        };
        provider().fetch(&mut ctx)
    }

    fn stored_access(&self) -> String {
        access_of(&self.vault.0.lock().unwrap())
    }
}

fn routes(request: &HttpRequest) -> &'static str {
    if request.url.contains("cedar_ember") {
        "grants"
    } else if request.url.contains("/api/oauth/usage") {
        "usage"
    } else if request.url.contains("/api/oauth/profile") {
        "profile"
    } else if request.url.contains("/v1/oauth/token") {
        "token"
    } else {
        "other"
    }
}

const USAGE: &str = r#"{"five_hour":{"utilization":18,"resets_at":"2099-01-01T00:00:00Z"},"seven_day":{"utilization":40}}"#;
const PROFILE: &str = r#"{"account":{"uuid":"acct-1","email":"ada@example.com"},"organization":{"uuid":"org-1","organization_type":"claude_pro","rate_limit_tier":"default_claude_ai"}}"#;
const GRANTS: &str = r#"{"cedar_ember":{"eligible":true,"grants":[{"id":"g","resets_left":1,"ends_at":"2099-01-01T00:00:00Z"}]}}"#;

#[test]
fn a_pass_reads_usage_then_the_hourly_plan_and_grants() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = Arc::clone(&seen);
    let http = Fake::new(move |request| {
        log.lock().unwrap().push(routes(request));
        Ok(match routes(request) {
            "profile" => response(200, PROFILE),
            "grants" => response(200, GRANTS),
            _ => response(200, USAGE),
        })
    });
    let mut harness = Harness::new(tokens("access", "refresh", Some(at(2_000_000_000))));
    let now = at(1_700_000_000);
    let usage = harness.fetch(&http, now, false).unwrap();
    assert_eq!(*seen.lock().unwrap(), ["usage", "profile", "grants"]);
    assert_eq!(usage.plan.as_deref(), Some("Pro"));
    assert_eq!(
        usage
            .lines
            .iter()
            .map(MetricLine::label)
            .collect::<Vec<_>>(),
        ["Session", "Weekly", "Rate Limit Resets"]
    );
    assert!(usage.lines[0].label() == "Session");
    match &usage.lines[0] {
        MetricLine::Progress { used, .. } => assert_eq!(*used, 18.0),
        other => panic!("{other:?}"),
    }

    seen.lock().unwrap().clear();
    harness
        .fetch(&http, now + chrono::Duration::minutes(10), false)
        .unwrap();
    assert_eq!(
        *seen.lock().unwrap(),
        ["usage"],
        "plan and grants stay read for the hour"
    );

    seen.lock().unwrap().clear();
    harness
        .fetch(&http, now + chrono::Duration::minutes(10), true)
        .unwrap();
    assert_eq!(
        *seen.lock().unwrap(),
        ["usage", "profile"],
        "a forced pass rereads the plan, and grants only after fifteen minutes"
    );

    seen.lock().unwrap().clear();
    harness
        .fetch(&http, now + chrono::Duration::minutes(16), true)
        .unwrap();
    assert_eq!(*seen.lock().unwrap(), ["usage", "grants", "profile"]);
}

#[test]
fn a_usage_limit_waits_out_the_saved_window_and_backs_off() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = Arc::clone(&seen);
    let http = Fake::new(move |request| {
        log.lock().unwrap().push(routes(request));
        assert_eq!(
            request.headers.get("User-Agent").map(String::as_str),
            Some("claude-cli/2.1.294 (external, cli)")
        );
        assert_eq!(
            request.headers.get("anthropic-beta").map(String::as_str),
            Some("oauth-2025-04-20")
        );
        assert_eq!(request.timeout, Duration::from_secs(10));
        Ok(response(429, ""))
    });
    let mut harness = Harness::new(tokens("access", "refresh", Some(at(2_000_000_000))));
    let now = at(1_700_000_000);
    let error = harness.fetch(&http, now, false).unwrap_err();
    assert_eq!(
        error,
        ProviderError::RateLimited {
            retry_after_seconds: Some(300.0)
        }
    );
    assert_eq!(
        *seen.lock().unwrap(),
        ["usage"],
        "a limited usage read does not spend the hourly plan or grants"
    );

    seen.lock().unwrap().clear();
    let held = harness
        .fetch(&http, now + chrono::Duration::seconds(299), false)
        .unwrap_err();
    assert_eq!(
        held,
        ProviderError::RateLimited {
            retry_after_seconds: Some(1.0)
        }
    );
    assert!(seen.lock().unwrap().is_empty());

    let again = harness
        .fetch(&http, now + chrono::Duration::seconds(300), false)
        .unwrap_err();
    assert_eq!(
        again,
        ProviderError::RateLimited {
            retry_after_seconds: Some(600.0)
        }
    );
}

#[test]
fn an_unauthorized_usage_read_saves_the_renewed_sign_in_before_retrying() {
    let vault_at_usage = Arc::new(Mutex::new(Vec::new()));
    let record = Arc::clone(&vault_at_usage);
    let stored = Arc::new(Mutex::new(Credential::OAuth(tokens(
        "old",
        "refresh",
        Some(at(2_000_000_000)),
    ))));
    let vault = SharedVault(Arc::clone(&stored));
    let account = Account::new(ProviderKind::Claude, "Claude", None);
    let mut state = ProviderState::default();
    let attempts = Arc::new(Mutex::new(0_i32));
    let count = Arc::clone(&attempts);
    let during = Arc::clone(&stored);
    let http = Fake::new(move |request| {
        Ok(match routes(request) {
            "token" => {
                let body = serde_json::from_slice::<serde_json::Value>(
                    request.body.as_deref().unwrap_or_default(),
                )
                .unwrap();
                assert_eq!(body["grant_type"], "refresh_token");
                assert_eq!(body["refresh_token"], "refresh");
                assert_eq!(body["client_id"], CLIENT_ID);
                assert_eq!(body["scope"], "user:profile");
                response(
                    200,
                    r#"{"access_token":"new","refresh_token":"rotated","expires_in":3600}"#,
                )
            }
            "usage" => {
                let n = {
                    let mut n = count.lock().unwrap();
                    *n += 1;
                    *n
                };
                record
                    .lock()
                    .unwrap()
                    .push(access_of(&during.lock().unwrap()));
                if n == 1 {
                    response(401, "")
                } else {
                    assert_eq!(
                        request.headers.get("Authorization").map(String::as_str),
                        Some("Bearer new")
                    );
                    response(200, USAGE)
                }
            }
            "profile" => response(200, PROFILE),
            "grants" => response(200, GRANTS),
            _ => response(404, ""),
        })
    });
    let mut ctx = FetchContext {
        account: &account,
        http: &http,
        vault: &vault,
        state: &mut state,
        now: at(1_700_000_000),
        force: false,
    };
    provider().fetch(&mut ctx).unwrap();
    assert_eq!(*vault_at_usage.lock().unwrap(), ["old", "new"]);
    let saved = stored.lock().unwrap().clone();
    match &saved {
        Credential::OAuth(tokens) => {
            assert_eq!(tokens.access_token, "new");
            assert_eq!(tokens.refresh_token.as_deref(), Some("rotated"));
            assert_eq!(tokens.expires_at, Some(at(1_700_000_000 + 3600)));
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn an_early_renewal_that_is_rate_limited_keeps_the_current_token() {
    let calls = Arc::new(Mutex::new(0_i32));
    let count = Arc::clone(&calls);
    let http = Fake::new(move |request| {
        Ok(match routes(request) {
            "token" => {
                *count.lock().unwrap() += 1;
                with_retry(429, r#"{"error":"slow_down"}"#, "120")
            }
            "usage" => response(200, USAGE),
            _ => response(200, "{}"),
        })
    });
    let now = at(1_700_000_000);
    let mut harness = Harness::new(tokens(
        "current",
        "refresh",
        Some(now + chrono::Duration::minutes(4)),
    ));
    harness.fetch(&http, now, false).unwrap();
    assert_eq!(*calls.lock().unwrap(), 1);
    assert_eq!(harness.stored_access(), "current");

    harness
        .fetch(&http, now + chrono::Duration::seconds(30), false)
        .unwrap();
    assert_eq!(
        *calls.lock().unwrap(),
        1,
        "the rate-limit hold skips the next early renewal"
    );
}

#[test]
fn a_renewal_of_an_expired_token_that_is_refused_fails_the_read() {
    let http = Fake::new(|request| {
        Ok(match routes(request) {
            "token" => response(400, r#"{"error":{"code":"invalid_grant"}}"#),
            _ => response(200, USAGE),
        })
    });
    let now = at(1_700_000_000);
    let mut harness = Harness::new(tokens(
        "old",
        "refresh",
        Some(now - chrono::Duration::seconds(1)),
    ));
    let error = harness.fetch(&http, now, false).unwrap_err();
    assert_eq!(error, ProviderError::SignInExpired(ProviderKind::Claude));
}

include!("test_login.rs");
