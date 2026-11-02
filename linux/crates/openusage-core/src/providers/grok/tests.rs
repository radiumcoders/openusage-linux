use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use base64::Engine;

use chrono::Duration as ChronoDuration;
use serde_json::json;

use super::*;
use crate::account::Account;
use crate::login::LoginPrompter;
use crate::provider::ProviderState;
use crate::vault::{MemoryVault, Vault};

fn jwt(payload: &str) -> String {
    format!(
        "e30.{}",
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(payload.as_bytes())
    )
}

fn form(body: &[u8]) -> BTreeMap<String, String> {
    url::form_urlencoded::parse(body).into_owned().collect()
}

struct Prompt {
    shown: Mutex<Vec<LoginPrompt>>,
}

impl LoginPrompter for Prompt {
    fn show(&self, prompt: &LoginPrompt) {
        self.shown.lock().unwrap().push(prompt.clone());
    }
    fn read_line(&self, _secret: bool) -> Option<String> {
        None
    }
    fn pasted_codes(&self) -> std::sync::mpsc::Receiver<String> {
        std::sync::mpsc::channel().1
    }
}

struct Script {
    access: String,
    seen: Mutex<Vec<HttpRequest>>,
}

impl HttpClient for Script {
    fn send(&self, request: HttpRequest) -> Result<HttpResponse, ProviderError> {
        self.seen.lock().unwrap().push(request.clone());
        let body = if request.url.ends_with("/device/code") {
            json!({"device_code":"device","user_code":"GROK-CODE","verification_uri":"https://auth.x.ai/activate","expires_in":600,"interval":1})
        } else {
            json!({"access_token": self.access, "refresh_token": "grok-refresh", "expires_in": 3600})
        };
        Ok(HttpResponse {
            status: 200,
            headers: Default::default(),
            body: serde_json::to_vec(&body).unwrap(),
        })
    }
}

#[test]
fn device_login_maps_the_email_from_the_token() {
    let access = jwt(r#"{"email":"grok@example.com","sub":"user-1"}"#);
    let http = Script {
        access: access.clone(),
        seen: Mutex::new(vec![]),
    };
    let prompt = Prompt {
        shown: Mutex::new(vec![]),
    };
    let started = Utc::now();
    let (label, credential) = login(&LoginMethod::Default).login(&prompt, &http).unwrap();
    assert_eq!(label, "grok@example.com");
    match &prompt.shown.lock().unwrap()[0] {
        LoginPrompt::DeviceCode {
            user_code,
            verification_url,
        } => {
            assert_eq!(user_code, "GROK-CODE");
            assert_eq!(verification_url, "https://auth.x.ai/activate");
        }
        other => panic!("{other:?}"),
    }
    let seen = http.seen.lock().unwrap();
    let start = form(seen[0].body.as_deref().unwrap());
    assert_eq!(start.get("client_id").map(String::as_str), Some(CLIENT_ID));
    assert_eq!(start.get("scope").map(String::as_str), Some(SCOPES));
    assert_eq!(
        seen[0].headers.get("Accept").map(String::as_str),
        Some("application/json")
    );
    let token = form(seen[1].body.as_deref().unwrap());
    assert_eq!(
        token.get("grant_type").map(String::as_str),
        Some("urn:ietf:params:oauth:grant-type:device_code")
    );
    assert_eq!(token.get("device_code").map(String::as_str), Some("device"));
    let Credential::OAuth(tokens) = credential else {
        panic!("oauth")
    };
    assert_eq!(tokens.access_token, access);
    assert_eq!(tokens.refresh_token.as_deref(), Some("grok-refresh"));
    assert_eq!(
        tokens
            .extras
            .get(OAuthTokens::IDENTITY_KEY)
            .map(String::as_str),
        Some("user-1")
    );
    let expires = tokens.expires_at.unwrap();
    assert!(
        expires >= started + ChronoDuration::seconds(3590)
            && expires <= Utc::now() + ChronoDuration::seconds(3600)
    );
}

const CREDITS: &str = r#"{"config":{"currentPeriod":{"type":"USAGE_PERIOD_TYPE_WEEKLY","start":"2026-06-30T21:36:52Z","end":"2026-07-07T21:36:52Z"},"creditUsagePercent":40,"onDemandCap":{"val":2500}}}"#;

struct RenewalHttp {
    events: Arc<Mutex<Vec<String>>>,
    mode: &'static str,
    refreshed: std::sync::atomic::AtomicBool,
}

impl HttpClient for RenewalHttp {
    fn send(&self, request: HttpRequest) -> Result<HttpResponse, ProviderError> {
        let kind = if request.url.contains("/oauth2/token") {
            "refresh"
        } else if request.url.contains("/v1/billing") {
            "credits"
        } else {
            "settings"
        };
        self.events.lock().unwrap().push(kind.into());
        let body = match kind {
            "refresh" if self.mode == "refuse" => {
                return Ok(HttpResponse {
                    status: 400,
                    headers: Default::default(),
                    body: br#"{"error":"invalid_grant"}"#.to_vec(),
                })
            }
            "refresh" if self.mode == "rate" => {
                let mut headers = BTreeMap::new();
                headers.insert("retry-after".into(), "30".into());
                return Ok(HttpResponse {
                    status: 429,
                    headers,
                    body: br#"{"error":"slow_down"}"#.to_vec(),
                });
            }
            "refresh" => {
                self.refreshed
                    .store(true, std::sync::atomic::Ordering::SeqCst);
                let raw = String::from_utf8(request.body.clone().unwrap()).unwrap();
                assert!(
                    raw.starts_with("grant_type=refresh_token&client_id="),
                    "{raw}"
                );
                json!({"access_token":"new-token","refresh_token":"new-refresh","expires_in":3600})
            }
            "credits"
                if self.mode == "unauthorized"
                    && !self.refreshed.load(std::sync::atomic::Ordering::SeqCst) =>
            {
                return Ok(HttpResponse {
                    status: 401,
                    headers: Default::default(),
                    body: br#"{"error":"unauthorized"}"#.to_vec(),
                })
            }
            "credits" => {
                let bearer = if matches!(self.mode, "rate" | "keep") {
                    "Bearer old-token"
                } else {
                    "Bearer new-token"
                };
                assert_eq!(
                    request.headers.get("Authorization").map(String::as_str),
                    Some(bearer)
                );
                assert_eq!(
                    request.headers.get("X-XAI-Token-Auth").map(String::as_str),
                    Some(TOKEN_AUTH)
                );
                return Ok(HttpResponse {
                    status: 200,
                    headers: Default::default(),
                    body: CREDITS.as_bytes().to_vec(),
                });
            }
            _ => json!({"subscription_tier_display":"SuperGrok"}),
        };
        Ok(HttpResponse {
            status: 200,
            headers: Default::default(),
            body: serde_json::to_vec(&body).unwrap(),
        })
    }
}

struct RecordingVault {
    inner: MemoryVault,
    events: Arc<Mutex<Vec<String>>>,
}

impl Vault for RecordingVault {
    fn load(&self, id: uuid::Uuid) -> Result<Option<Credential>, ProviderError> {
        self.inner.load(id)
    }
    fn save(&self, id: uuid::Uuid, credential: &Credential) -> Result<(), ProviderError> {
        self.events.lock().unwrap().push("save".into());
        self.inner.save(id, credential)
    }
    fn delete(&self, id: uuid::Uuid) -> Result<(), ProviderError> {
        self.inner.delete(id)
    }
}

fn tokens(access: &str, expires_at: DateTime<Utc>) -> OAuthTokens {
    let mut tokens = OAuthTokens::new(access);
    tokens.refresh_token = Some("old-refresh".into());
    tokens.expires_at = Some(expires_at);
    tokens.extras.insert("kept".into(), "yes".into());
    tokens
}

fn run(
    mode: &'static str,
    expires_at: DateTime<Utc>,
    now: DateTime<Utc>,
) -> (
    ProviderUsage,
    Arc<Mutex<Vec<String>>>,
    RecordingVault,
    Account,
) {
    let account = Account::new(ProviderKind::Grok, "Grok", None);
    let events = Arc::new(Mutex::new(Vec::new()));
    let vault = RecordingVault {
        inner: MemoryVault::with(
            account.id,
            Credential::OAuth(tokens("old-token", expires_at)),
        ),
        events: events.clone(),
    };
    let http = RenewalHttp {
        events: events.clone(),
        mode,
        refreshed: std::sync::atomic::AtomicBool::new(false),
    };
    let mut state = ProviderState::default();
    let usage = {
        let mut ctx = FetchContext {
            account: &account,
            http: &http,
            vault: &vault,
            state: &mut state,
            now,
            force: true,
        };
        provider().fetch(&mut ctx).unwrap()
    };
    (usage, events, vault, account)
}

#[test]
fn an_expired_sign_in_is_saved_before_the_new_token_is_used() {
    let now = Utc::now();
    let (usage, events, vault, account) = run("refresh", now - ChronoDuration::minutes(1), now);
    assert_eq!(usage.plan.as_deref(), Some("SuperGrok"));
    assert_eq!(usage.lines[0].label(), "Weekly limit");
    assert_eq!(
        events.lock().unwrap().as_slice(),
        ["refresh", "save", "credits", "settings"]
    );
    let Credential::OAuth(saved) = vault.load(account.id).unwrap().unwrap() else {
        panic!("oauth")
    };
    assert_eq!(saved.access_token, "new-token");
    assert_eq!(saved.refresh_token.as_deref(), Some("new-refresh"));
    assert_eq!(saved.extras.get("kept").map(String::as_str), Some("yes"));
}

#[test]
fn a_refused_access_token_renews_once() {
    let now = Utc::now();
    let (_, events, vault, account) = run("unauthorized", now + ChronoDuration::hours(2), now);
    assert_eq!(
        events.lock().unwrap().as_slice(),
        ["credits", "refresh", "save", "credits", "settings"]
    );
    let Credential::OAuth(saved) = vault.load(account.id).unwrap().unwrap() else {
        panic!("oauth")
    };
    assert_eq!(saved.access_token, "new-token");
}

#[test]
fn a_refused_refresh_token_is_an_expired_sign_in() {
    let account = Account::new(ProviderKind::Grok, "Grok", None);
    let events = Arc::new(Mutex::new(Vec::new()));
    let now = Utc::now();
    let vault = RecordingVault {
        inner: MemoryVault::with(
            account.id,
            Credential::OAuth(tokens("old-token", now - ChronoDuration::minutes(1))),
        ),
        events: events.clone(),
    };
    let http = RenewalHttp {
        events,
        mode: "refuse",
        refreshed: Default::default(),
    };
    let mut state = ProviderState::default();
    let mut ctx = FetchContext {
        account: &account,
        http: &http,
        vault: &vault,
        state: &mut state,
        now,
        force: false,
    };
    assert_eq!(
        provider().fetch(&mut ctx),
        Err(ProviderError::SignInExpired(ProviderKind::Grok))
    );
}

#[test]
fn an_early_rate_limit_keeps_the_current_token_until_the_hold_passes() {
    let now = Utc::now();
    let account = Account::new(ProviderKind::Grok, "Grok", None);
    let events = Arc::new(Mutex::new(Vec::new()));
    let vault = RecordingVault {
        inner: MemoryVault::with(
            account.id,
            Credential::OAuth(tokens("old-token", now + ChronoDuration::minutes(2))),
        ),
        events: events.clone(),
    };
    let http = RenewalHttp {
        events: events.clone(),
        mode: "rate",
        refreshed: Default::default(),
    };
    let mut state = ProviderState::default();
    let usage = {
        let mut ctx = FetchContext {
            account: &account,
            http: &http,
            vault: &vault,
            state: &mut state,
            now,
            force: false,
        };
        provider().fetch(&mut ctx).unwrap()
    };
    assert_eq!(
        usage.lines[1],
        crate::metric::MetricLine::Badge {
            label: "Pay as you go".into(),
            text: "2500 cap".into()
        }
    );
    {
        let mut ctx = FetchContext {
            account: &account,
            http: &http,
            vault: &vault,
            state: &mut state,
            now: now + ChronoDuration::seconds(10),
            force: false,
        };
        provider().fetch(&mut ctx).unwrap();
    }
    let refresh_calls = events
        .lock()
        .unwrap()
        .iter()
        .filter(|event| event.as_str() == "refresh")
        .count();
    assert_eq!(
        refresh_calls, 1,
        "the rate-limited early renewal is not retried inside the hold"
    );
    let Credential::OAuth(saved) = vault.load(account.id).unwrap().unwrap() else {
        panic!("oauth")
    };
    assert_eq!(saved.access_token, "old-token");
}
