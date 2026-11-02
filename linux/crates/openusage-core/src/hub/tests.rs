//! Ports of `CLIProxyHubTests`, `CLIProxyHubTunnelTests`, and the security property in
//! `CLIProxyHubOffTests`. The Swift feature flag stays off; this edition connects hubs.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;

use chrono::{Duration as ChronoDuration, Utc};
use serde_json::{json, Value};
use uuid::Uuid;

use super::*;
use crate::account::{Account, Credential, OAuthTokens};
use crate::accounts::duplicate_message;
use crate::error::ProviderError;
use crate::http::{HttpClient, HttpRequest, HttpResponse};
use crate::kind::ProviderKind;
use crate::metric::ProviderUsage;
use crate::provider::{FetchContext, ProviderState};
use crate::store::Settings;
use crate::vault::{MemoryVault, Vault};

const LISTING: &str = r#"{"files":[
  {"id":"claude-alice@example.com.json","auth_index":"b799da203df01af5","provider":"claude","email":"alice@example.com","account_type":"oauth","status":"ok"},
  {"id":"apikey-file.json","auth_index":"c1","provider":"claude","label":"claude","account_type":"oauth"},
  {"id":"disabled-claude.json","auth_index":"c2","provider":"claude","email":"gone@example.com","disabled":true,"account_type":"oauth"},
  {"id":"notoken-claude.json","auth_index":"c3","provider":"claude","email":"notoken@example.com","account_type":"oauth","unavailable":true},
  {"id":"codex-codex-user@example.com-plus.json","auth_index":"d4","provider":"codex","email":"codex-user@example.com","account_type":"oauth","id_token":{"chatgpt_account_id":"acct-dummy-0001","plan_type":"plus"}},
  {"id":"kimi-user.json","auth_index":"e5","provider":"kimi","email":"kimi@example.com","account_type":"oauth"}
]}"#;

const CLAUDE_USAGE: &str =
    r#"{"five_hour":{"utilization":42,"resets_at":"2099-01-01T00:00:00.000Z"}}"#;
const CLAUDE_PROFILE: &str = r#"{"account":{"uuid":"acc-1"},"organization":{"uuid":"6a9f1c2e-1b4d-4c3a-9e8f-0123456789ab","organization_type":"claude_max","rate_limit_tier":"default_claude_max_20x"}}"#;
const CODEX_USAGE: &str = r#"{"rate_limit":{}}"#;
const DEVIN_STATUS: &str = r#"{"userStatus":{"planStatus":{"planInfo":{"planName":"Max"},"weeklyQuotaRemainingPercent":40}}}"#;
const GROK_CREDITS: &str = r#"{"config":{"currentPeriod":{"type":"USAGE_PERIOD_TYPE_WEEKLY","start":"2099-01-01T00:00:00Z","end":"2099-01-08T00:00:00Z"},"creditUsagePercent":25}}"#;
const ANTIGRAVITY_LOAD: &str =
    r#"{"cloudaicompanionProject":"p-1","currentTier":{"id":"free-tier","name":"Free"}}"#;
const ANTIGRAVITY_QUOTA: &str = r#"{"groups":[{"displayName":"Gemini","buckets":[{"bucketId":"gemini-weekly","remainingFraction":0.75,"resetTime":"2099-01-01T00:00:00Z","window":"weekly"}]}]}"#;

struct Temp(std::path::PathBuf);
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

struct Fake {
    requests: Mutex<Vec<HttpRequest>>,
    handler: Mutex<Box<dyn FnMut(&HttpRequest) -> Result<HttpResponse, ProviderError> + Send>>,
    /// Pauses the first api-call that presents this bearer key, so a test can replace the key mid-flight.
    pause: Mutex<Option<(String, mpsc::Sender<()>, mpsc::Receiver<()>)>>,
}

impl Fake {
    fn new(
        handler: impl FnMut(&HttpRequest) -> Result<HttpResponse, ProviderError> + Send + 'static,
    ) -> Arc<Self> {
        Arc::new(Self {
            requests: Mutex::new(Vec::new()),
            handler: Mutex::new(Box::new(handler)),
            pause: Mutex::new(None),
        })
    }

    fn len(&self) -> usize {
        self.requests.lock().unwrap().len()
    }

    fn requests(&self) -> Vec<HttpRequest> {
        self.requests.lock().unwrap().clone()
    }
}

impl HttpClient for Fake {
    fn send(&self, request: HttpRequest) -> Result<HttpResponse, ProviderError> {
        self.requests.lock().unwrap().push(request.clone());
        let auth = request.headers.get("Authorization").cloned();
        let pause = {
            let mut slot = self.pause.lock().unwrap();
            match slot.as_ref() {
                Some((key, _, _))
                    if auth.as_deref() == Some(key.as_str())
                        && request.url.contains("api-call") =>
                {
                    slot.take()
                }
                _ => None,
            }
        };
        if let Some((_, entered, release)) = pause {
            let _ = entered.send(());
            let _ = release.recv_timeout(Duration::from_secs(5));
        }
        (self.handler.lock().unwrap())(&request)
    }
}

fn response(status: u16, body: &str) -> HttpResponse {
    HttpResponse {
        status,
        headers: Default::default(),
        body: body.as_bytes().to_vec(),
    }
}

fn envelope(status: u16, body: &str, extra: &[(&str, &str)]) -> HttpResponse {
    let mut header = serde_json::Map::new();
    header.insert("Content-Type".into(), json!(["application/json"]));
    for (name, value) in extra {
        header.insert((*name).to_string(), json!([value]));
    }
    response(
        200,
        &json!({"status_code": status, "header": header, "body": body}).to_string(),
    )
}

fn upstream_body(url: &str) -> &'static str {
    if url.contains("/api/oauth/usage") && !url.contains("cedar") {
        CLAUDE_USAGE
    } else if url.contains("/api/oauth/profile") {
        CLAUDE_PROFILE
    } else if url.contains("wham/usage") {
        CODEX_USAGE
    } else if url.contains("loadCodeAssist") {
        ANTIGRAVITY_LOAD
    } else if url.contains("retrieveUserQuotaSummary") {
        ANTIGRAVITY_QUOTA
    } else if url.contains("GetUserStatus") {
        DEVIN_STATUS
    } else if url.contains("/v1/billing") {
        GROK_CREDITS
    } else {
        "{}"
    }
}

/// A hub that answers auth-files with `listing` and wraps every other provider URL.
fn routing(listing: &'static str) -> Arc<Fake> {
    Fake::new(move |request| {
        let auth = request.headers.get("Authorization").map(String::as_str);
        if auth != Some("Bearer mgmt-key") {
            return Ok(response(401, r#"{"error":"invalid management key"}"#));
        }
        if request.url.ends_with("/auth-files") {
            return Ok(response(200, listing));
        }
        if request.url.ends_with("/api-call") {
            let payload: Value =
                serde_json::from_slice(request.body.as_deref().unwrap_or_default()).unwrap();
            let url = payload.get("url").and_then(Value::as_str).unwrap_or("");
            return Ok(envelope(200, upstream_body(url), &[]));
        }
        Ok(response(404, r#"{"error":"no"}"#))
    })
}

struct World {
    settings: Settings,
    vault: Arc<MemoryVault>,
    http: Arc<Fake>,
    gate: Arc<HubGate>,
    _dir: Temp,
}

impl World {
    fn new(http: Arc<Fake>) -> Self {
        let dir = std::env::temp_dir().join(format!("openusage-hub-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        Self {
            settings: Settings::at(dir.join("settings.plist")),
            vault: Arc::new(MemoryVault::default()),
            http,
            gate: Arc::new(HubGate::default()),
            _dir: Temp(dir),
        }
    }

    fn connect(&self, url: &str, key: &str) -> Result<Hub, ProviderError> {
        connect(
            url,
            key,
            self.http.as_ref(),
            self.vault.as_ref(),
            &self.settings,
        )
    }

    fn client(&self, account: &Account) -> Arc<dyn HttpClient> {
        let vault: Arc<dyn Vault> = self.vault.clone();
        let http: Arc<dyn HttpClient> = self.http.clone();
        http_for(
            account,
            &list(&self.settings),
            http,
            vault,
            self.gate.clone(),
        )
    }
}

fn login(id: &str, provider: &str, index: Option<&str>, disabled: bool) -> HubLogin {
    HubLogin {
        id: id.into(),
        auth_index: index.unwrap_or("").into(),
        provider: provider.into(),
        label: id.into(),
        is_oauth: true,
        is_disabled: disabled,
        chatgpt_account_id: (provider == "codex").then(|| format!("acct-{id}")),
    }
}

fn fetch(
    account: &Account,
    http: &dyn HttpClient,
    vault: &dyn Vault,
    state: &mut ProviderState,
) -> Result<ProviderUsage, ProviderError> {
    let mut ctx = FetchContext {
        account,
        http,
        vault,
        state,
        now: Utc::now(),
        force: false,
    };
    crate::providers::provider(account.kind)
        .fetch(&mut ctx)
        .map_err(|error| account_failure(account, error))
}

fn api_calls(requests: &[HttpRequest]) -> Vec<Value> {
    requests
        .iter()
        .filter(|request| request.url.ends_with("/api-call"))
        .map(|request| serde_json::from_slice(request.body.as_deref().unwrap()).unwrap())
        .collect()
}

#[test]
fn base_url_drops_the_pasted_panel_and_keeps_a_proxy_prefix() {
    let cases = [
        ("127.0.0.1:8317", Some("http://127.0.0.1:8317")),
        (" http://mini.local:8317/ ", Some("http://mini.local:8317")),
        (
            "http://192.168.1.20:8317/management.html#/usage",
            Some("http://192.168.1.20:8317"),
        ),
        (
            "https://hub.example.com/v0/management?x=1",
            Some("https://hub.example.com"),
        ),
        (
            "https://hub.example.com/cpa/v1",
            Some("https://hub.example.com/cpa"),
        ),
        ("ftp://hub", None),
        ("", None),
    ];
    for (input, expected) in cases {
        assert_eq!(base_url(input).as_deref(), expected, "{input}");
    }
    assert_eq!(host_of("http://mini.local:8317"), "mini.local:8317");
}

#[test]
fn a_listing_keeps_hub_order_and_skips_what_cannot_be_mirrored() {
    let logins = parse_logins(LISTING.as_bytes()).unwrap();
    assert_eq!(logins.len(), 6);
    assert_eq!(logins[0].auth_index, "b799da203df01af5");
    assert_eq!(
        logins.iter().map(|login| login.kind()).collect::<Vec<_>>(),
        vec![
            Some(ProviderKind::Claude),
            Some(ProviderKind::Claude),
            Some(ProviderKind::Claude),
            Some(ProviderKind::Claude),
            Some(ProviderKind::Codex),
            None,
        ]
    );
    assert_eq!(
        logins[4].chatgpt_account_id.as_deref(),
        Some("acct-dummy-0001")
    );
    assert!(logins[2].is_disabled);

    let every = parse_logins(
        br#"{"files":[
            {"id":"a","auth_index":"1","provider":"claude"},
            {"id":"b","auth_index":"2","provider":"codex"},
            {"id":"c","auth_index":"3","provider":"antigravity"},
            {"id":"d","auth_index":"4","provider":"devin"},
            {"id":"e","auth_index":"5","provider":"xai"},
            {"id":"f","auth_index":"6","provider":"cursor"},
            {"id":"g","auth_index":"7","provider":"gemini"}
        ]}"#,
    )
    .unwrap();
    assert_eq!(
        every
            .iter()
            .filter_map(|login| login.kind())
            .collect::<Vec<_>>(),
        vec![
            ProviderKind::Claude,
            ProviderKind::Codex,
            ProviderKind::Antigravity,
            ProviderKind::Devin,
            ProviderKind::Grok,
        ]
    );

    let skipped = parse_logins(
        br#"{"files":[
            {"id":"a","auth_index":"1","provider":"claude","account_type":"api_key"},
            {"id":"b","provider":"codex"},
            {"id":"c","auth_index":"3","provider":"codex"}
        ]}"#,
    )
    .unwrap();
    assert_eq!(
        skipped
            .iter()
            .map(|login| login.id.as_str())
            .collect::<Vec<_>>(),
        vec!["a", "c"]
    );
    assert_eq!(skipped[0].kind(), None);
    assert_eq!(skipped[1].kind(), Some(ProviderKind::Codex));
    assert!(parse_logins(b"<html>").is_err());
}

#[test]
fn connecting_mirrors_supported_logins_and_a_second_pass_changes_nothing() {
    let world = World::new(routing(LISTING));
    let hub = world
        .connect("http://mini.local:8317/management.html", " mgmt-key ")
        .unwrap();
    assert_eq!(hub.url, "http://mini.local:8317");
    assert_eq!(hub.host, "mini.local:8317");
    assert_eq!(
        world.vault.load(hub.id).unwrap(),
        Some(Credential::ApiKey("mgmt-key".into()))
    );

    let accounts = world.settings.accounts();
    assert_eq!(
        accounts
            .iter()
            .map(|account| account.kind)
            .collect::<Vec<_>>(),
        vec![
            ProviderKind::Claude,
            ProviderKind::Claude,
            ProviderKind::Claude,
            ProviderKind::Codex
        ]
    );
    assert_eq!(
        accounts
            .iter()
            .map(|account| account.label.as_str())
            .collect::<Vec<_>>(),
        vec![
            "alice@example.com",
            "claude",
            "notoken@example.com",
            "codex-user@example.com"
        ]
    );
    assert!(accounts
        .iter()
        .all(|account| account.hub_link().unwrap().hub_id == hub.id));
    assert_eq!(
        accounts[0].hub_link().unwrap().auth_index,
        "b799da203df01af5"
    );
    assert_eq!(accounts[0].full_name(), "Claude · alice@example.com");
    assert_eq!(
        accounts[0].accounts_label(),
        "alice@example.com (mini.local:8317)"
    );
    for account in &accounts {
        let credential = world.vault.load(account.id).unwrap().unwrap();
        let Credential::OAuth(tokens) = credential else {
            panic!("mirrored sign-in is oauth")
        };
        assert_eq!(tokens.access_token, TOKEN_PLACEHOLDER);
        assert!(tokens.refresh_token.is_none());
        if account.kind == ProviderKind::Codex {
            assert_eq!(
                tokens.extras.get("chatgpt_account_id").map(String::as_str),
                Some("acct-dummy-0001")
            );
        } else {
            assert!(tokens.extras.is_empty());
        }
    }

    let before = world.settings.accounts();
    let logins = parse_logins(LISTING.as_bytes()).unwrap();
    assert_eq!(
        mirror(&world.settings, world.vault.as_ref(), hub.id, &logins).unwrap(),
        false
    );
    assert_eq!(world.settings.accounts(), before);

    let raw =
        String::from_utf8(world.settings.get(HUBS_KEY).unwrap().into_data().unwrap()).unwrap();
    assert!(raw.contains(&hub.id.to_string().to_ascii_uppercase()));
    assert!(raw.contains("\"url\":\"http://mini.local:8317\""));
    assert!(raw.contains("addedAt"));
    let accounts_raw = String::from_utf8(
        world
            .settings
            .get(crate::store::ACCOUNTS_KEY)
            .unwrap()
            .into_data()
            .unwrap(),
    )
    .unwrap();
    for key in ["hubID", "hubURL", "loginID", "authIndex"] {
        assert!(accounts_raw.contains(key), "{key}");
    }

    let again = Settings::at(world.settings.path().to_path_buf());
    assert_eq!(list(&again), vec![hub]);
    let links: Vec<_> = again
        .accounts()
        .iter()
        .filter_map(Account::hub_link)
        .collect();
    assert_eq!(links.len(), 4);
    assert!(links
        .iter()
        .all(|link| link.hub_url == "http://mini.local:8317"));
}

#[test]
fn a_mac_written_hub_list_and_link_read_back() {
    let dir = std::env::temp_dir().join(format!("openusage-hub-mac-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let settings = Settings::at(dir.join("settings.plist"));
    let hubs = r#"[{"id":"8AD3B217-0000-4000-8000-0000000000AA","url":"http://mini.local:8317","addedAt":0}]"#;
    let accounts = r#"[{"id":"8AD3B217-0000-4000-8000-0000000000BB","kind":"claude","label":"alice@example.com","hub":{"hubID":"8AD3B217-0000-4000-8000-0000000000AA","hubURL":"http://mini.local:8317","loginID":"claude-alice@example.com.json","authIndex":"b799da203df01af5"},"addedAt":0}]"#;
    settings
        .update(|values| {
            values.insert(
                HUBS_KEY.into(),
                plist::Value::Data(hubs.as_bytes().to_vec()),
            );
            values.insert(
                crate::store::ACCOUNTS_KEY.into(),
                plist::Value::Data(accounts.as_bytes().to_vec()),
            );
        })
        .unwrap();
    let loaded = list(&settings);
    assert_eq!(
        loaded[0].id,
        Uuid::parse_str("8AD3B217-0000-4000-8000-0000000000AA").unwrap()
    );
    assert_eq!(loaded[0].host, "mini.local:8317");
    let account = settings.accounts().remove(0);
    assert_eq!(account.full_name(), "Claude · alice@example.com");
    assert_eq!(account.hub_host().as_deref(), Some("mini.local:8317"));
    assert_eq!(account.hub_link().unwrap().auth_index, "b799da203df01af5");
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn a_refused_key_or_a_bad_address_saves_nothing() {
    let calls = Arc::new(AtomicBool::new(false));
    let flag = calls.clone();
    let http = Fake::new(move |_| {
        flag.store(true, Ordering::SeqCst);
        Ok(response(401, r#"{"error":"invalid management key"}"#))
    });
    let world = World::new(http);
    let error = world
        .connect("http://mini.local:8317", "wrong-key")
        .unwrap_err();
    assert_eq!(
        error.to_string(),
        "The CLIProxyAPI hub at mini.local:8317 rejected the management key. Connect it again with the right key (`openusage hub remove`, then `openusage hub add`)."
    );
    assert!(list(&world.settings).is_empty());
    assert!(world.settings.accounts().is_empty());
    assert!(calls.load(Ordering::SeqCst));

    let quiet = World::new(Fake::new(|_| panic!("a bad address is not requested")));
    let error = quiet.connect("ftp://hub", "key").unwrap_err();
    assert_eq!(
        error.to_string(),
        "Enter the hub\u{2019}s address, like http://192.168.1.20:8317."
    );
    let error = quiet.connect("http://mini.local:8317", "  ").unwrap_err();
    assert_eq!(error.to_string(), "Enter the hub\u{2019}s management key.");
    assert!(list(&quiet.settings).is_empty());
}

#[test]
fn the_same_hub_is_not_connected_twice() {
    let world = World::new(routing(LISTING));
    world.connect("http://mini.local:8317", "mgmt-key").unwrap();
    let calls = world.http.len();
    let error = world
        .connect(" http://mini.local:8317/ ", "mgmt-key")
        .unwrap_err();
    assert_eq!(
        error.to_string(),
        "The hub at mini.local:8317 is already connected."
    );
    assert_eq!(world.http.len(), calls);
    assert_eq!(list(&world.settings).len(), 1);
}

#[test]
fn mirroring_follows_added_removed_renamed_and_disabled_logins() {
    let world = World::new(routing(r#"{"files":[]}"#));
    let hub = world.connect("http://mini.local:8317", "mgmt-key").unwrap();
    let direct = Account::new(ProviderKind::OpenRouter, "Direct", None);
    world
        .vault
        .save(direct.id, &Credential::ApiKey("direct-key".into()))
        .unwrap();
    world
        .settings
        .update_accounts(|accounts| accounts.insert(0, direct.clone()))
        .unwrap();

    let apply = |logins: Vec<HubLogin>| {
        mirror(&world.settings, world.vault.as_ref(), hub.id, &logins).unwrap();
    };
    apply(vec![
        login("a", "claude", Some("1"), false),
        login("b", "codex", Some("2"), false),
        login("c", "claude", Some("3"), false),
    ]);
    assert_eq!(
        world
            .settings
            .accounts()
            .iter()
            .map(|account| account.label.as_str())
            .collect::<Vec<_>>(),
        vec!["Direct", "a", "b", "c"]
    );
    let first = world.settings.accounts();
    apply(vec![
        login("a", "claude", Some("1"), false),
        login("b", "codex", Some("2"), false),
        login("c", "claude", Some("3"), false),
    ]);
    assert_eq!(world.settings.accounts(), first);

    let renamed = login("b", "codex", Some("renamed"), false);
    apply(vec![
        renamed,
        login("c", "claude", Some("3"), true),
        login("d", "claude", Some("4"), false),
    ]);
    let accounts = world.settings.accounts();
    assert_eq!(
        accounts
            .iter()
            .map(|account| account.label.as_str())
            .collect::<Vec<_>>(),
        vec!["Direct", "b", "d"]
    );
    assert_eq!(
        accounts[1].id,
        first
            .iter()
            .find(|account| account.label == "b")
            .unwrap()
            .id
    );
    assert_eq!(accounts[1].hub_link().unwrap().auth_index, "renamed");
    assert_ne!(
        accounts[2].id,
        first
            .iter()
            .find(|account| account.label == "c")
            .unwrap()
            .id
    );
    assert!(world
        .vault
        .load(
            first
                .iter()
                .find(|account| account.label == "a")
                .unwrap()
                .id
        )
        .unwrap()
        .is_none());

    apply(vec![
        login("b", "codex", Some("renamed"), false),
        login("c", "claude", Some("5"), false),
        login("d", "claude", Some("4"), false),
    ]);
    let accounts = world.settings.accounts();
    assert_eq!(
        accounts
            .iter()
            .map(|account| account.label.as_str())
            .collect::<Vec<_>>(),
        vec!["Direct", "b", "d", "c"]
    );
    assert_ne!(
        accounts[3].id,
        first
            .iter()
            .find(|account| account.label == "c")
            .unwrap()
            .id
    );

    let codex = accounts
        .iter()
        .find(|account| account.label == "b")
        .unwrap();
    let Credential::OAuth(tokens) = world.vault.load(codex.id).unwrap().unwrap() else {
        panic!("oauth")
    };
    assert_eq!(
        tokens.extras.get("chatgpt_account_id").map(String::as_str),
        Some("acct-b")
    );

    disconnect(hub.id, world.vault.as_ref(), &world.settings).unwrap();
    assert!(list(&world.settings).is_empty());
    assert_eq!(world.settings.accounts().len(), 1);
    assert_eq!(world.settings.accounts()[0].label, "Direct");
    assert_eq!(
        world.vault.load(direct.id).unwrap(),
        Some(Credential::ApiKey("direct-key".into()))
    );
    assert!(world.vault.load(hub.id).unwrap().is_none());
    assert!(world.vault.load(codex.id).unwrap().is_none());

    let reloaded = Settings::at(world.settings.path().to_path_buf());
    assert!(list(&reloaded).is_empty());
    assert_eq!(reloaded.accounts().len(), 1);
}

#[test]
fn a_hub_login_is_not_a_duplicate_of_a_direct_sign_in() {
    let world = World::new(routing(LISTING));
    world.connect("http://mini.local:8317", "mgmt-key").unwrap();
    let mirrored = world
        .settings
        .accounts()
        .into_iter()
        .find(|account| account.kind == ProviderKind::Claude)
        .unwrap();
    let mut stored = OAuthTokens::new(TOKEN_PLACEHOLDER);
    stored
        .extras
        .insert(OAuthTokens::IDENTITY_KEY.into(), "acct/org".into());
    world
        .vault
        .save(mirrored.id, &Credential::OAuth(stored))
        .unwrap();
    let mut direct = OAuthTokens::new("real-token");
    direct
        .extras
        .insert(OAuthTokens::IDENTITY_KEY.into(), "acct/org".into());
    let result = duplicate_message(
        world.vault.as_ref(),
        &world.settings.accounts(),
        &Credential::OAuth(direct),
        ProviderKind::Claude,
        None,
        None,
    );
    assert!(result.is_ok(), "{result:?}");
}

#[test]
fn sync_skips_a_hub_read_within_the_hour_and_a_failure_still_counts() {
    let world = World::new(routing(LISTING));
    let hub = world.connect("http://mini.local:8317", "mgmt-key").unwrap();
    let mut synced = std::collections::HashMap::new();
    let now = Utc::now();
    synced.insert(hub.id, now);
    let calls = world.http.len();
    sync_due(
        world.settings(),
        world.http.as_ref(),
        world.vault.as_ref(),
        &world.gate,
        &mut synced,
        now,
        ChronoDuration::hours(1),
    );
    assert_eq!(world.http.len(), calls);
    sync_due(
        world.settings(),
        world.http.as_ref(),
        world.vault.as_ref(),
        &world.gate,
        &mut synced,
        now,
        ChronoDuration::zero(),
    );
    assert_eq!(world.http.len(), calls + 1);

    let fail = Arc::new(AtomicBool::new(true));
    let flag = fail.clone();
    let http = Fake::new(move |request| {
        if flag.load(Ordering::SeqCst) {
            return Ok(response(500, r#"{"error":"down"}"#));
        }
        if request.url.ends_with("/auth-files") {
            return Ok(response(200, r#"{"files":[]}"#));
        }
        Ok(response(404, "{}"))
    });
    let world = World::new(http);
    fail.store(false, Ordering::SeqCst);
    let hub = world.connect("http://mini.local:8317", "mgmt-key").unwrap();
    fail.store(true, Ordering::SeqCst);
    let mut synced = std::collections::HashMap::new();
    let calls = world.http.len();
    sync_due(
        &world.settings,
        world.http.as_ref(),
        world.vault.as_ref(),
        &world.gate,
        &mut synced,
        now,
        ChronoDuration::hours(1),
    );
    assert_eq!(world.http.len(), calls + 1);
    sync_due(
        &world.settings,
        world.http.as_ref(),
        world.vault.as_ref(),
        &world.gate,
        &mut synced,
        now,
        ChronoDuration::hours(1),
    );
    assert_eq!(world.http.len(), calls + 1);
    assert!(synced.contains_key(&hub.id));
}

#[test]
fn provider_reads_go_through_api_call_and_keep_their_own_requests() {
    let listing = r#"{"files":[
        {"id":"claude-a","auth_index":"index-claude","provider":"claude","email":"a@example.com"},
        {"id":"codex-b","auth_index":"index-codex","provider":"codex","email":"b@example.com","id_token":{"chatgpt_account_id":"acct-b"}},
        {"id":"anti-c","auth_index":"index-antigravity","provider":"antigravity","email":"c@example.com"},
        {"id":"devin-d","auth_index":"index-devin","provider":"devin","email":"d@example.com"},
        {"id":"grok-e","auth_index":"index-grok","provider":"xai","email":"e@example.com"}
    ]}"#;
    let world = World::new(routing(listing));
    world.connect("http://mini.local:8317", "mgmt-key").unwrap();
    let accounts = world.settings.accounts();
    let before = world.http.len();

    let claude = accounts
        .iter()
        .find(|account| account.kind == ProviderKind::Claude)
        .unwrap();
    let mut state = ProviderState::default();
    let usage = fetch(
        claude,
        world.client(claude).as_ref(),
        world.vault.as_ref(),
        &mut state,
    )
    .unwrap();
    assert!(!usage.lines.is_empty(), "{usage:?}");
    assert_eq!(usage.plan.as_deref(), Some("Ultra"));
    let profiles = api_calls(&world.http.requests())
        .iter()
        .filter(|call| call["url"].as_str().unwrap_or("").contains("/profile"))
        .count();
    assert_eq!(profiles, 1);
    fetch(
        claude,
        world.client(claude).as_ref(),
        world.vault.as_ref(),
        &mut state,
    )
    .unwrap();
    let profiles = api_calls(&world.http.requests())
        .iter()
        .filter(|call| call["url"].as_str().unwrap_or("").contains("/profile"))
        .count();
    assert_eq!(profiles, 1, "the plan is not read again within the hour");

    for kind in [
        ProviderKind::Codex,
        ProviderKind::Antigravity,
        ProviderKind::Devin,
        ProviderKind::Grok,
    ] {
        let account = accounts
            .iter()
            .find(|account| account.kind == kind)
            .unwrap();
        let mut state = ProviderState::default();
        let usage = fetch(
            account,
            world.client(account).as_ref(),
            world.vault.as_ref(),
            &mut state,
        );
        assert!(usage.is_ok(), "{kind:?}: {usage:?}");
    }

    let calls = api_calls(&world.http.requests()[before..]);
    let management: Vec<_> = world.http.requests()[before..]
        .iter()
        .filter(|request| request.url.ends_with("/api-call"))
        .cloned()
        .collect();
    assert!(management.iter().all(|request| {
        request.method == "POST"
            && request.headers.get("Authorization").map(String::as_str) == Some("Bearer mgmt-key")
            && request.headers.get("Content-Type").map(String::as_str) == Some("application/json")
    }));
    let find = |index: &str| {
        calls
            .iter()
            .find(|call| call["auth_index"] == index)
            .cloned()
            .unwrap()
    };
    let claude_call = find("index-claude");
    assert_eq!(
        claude_call["url"].as_str(),
        Some("https://api.anthropic.com/api/oauth/usage")
    );
    assert_eq!(
        claude_call["header"]["Authorization"].as_str(),
        Some("Bearer $TOKEN$")
    );
    let wrapped = management
        .iter()
        .find(|request| {
            request
                .body
                .as_ref()
                .is_some_and(|body| String::from_utf8_lossy(body).contains("index-claude"))
        })
        .unwrap();
    assert_eq!(wrapped.timeout, Duration::from_secs(20));

    let codex_call = find("index-codex");
    assert_eq!(
        codex_call["url"].as_str(),
        Some("https://chatgpt.com/backend-api/wham/usage")
    );
    assert_eq!(
        codex_call["header"]["ChatGPT-Account-Id"].as_str(),
        Some("acct-b")
    );
    assert_eq!(
        codex_call["header"]["Authorization"].as_str(),
        Some("Bearer $TOKEN$")
    );

    let devin_call = calls
        .iter()
        .find(|call| call["auth_index"] == "index-devin")
        .unwrap();
    assert!(devin_call["url"]
        .as_str()
        .unwrap()
        .ends_with("GetUserStatus"));
    assert!(devin_call["data"]
        .as_str()
        .unwrap()
        .contains(r#""apiKey":"$TOKEN$""#));

    let quota = calls
        .iter()
        .find(|call| {
            call["url"]
                .as_str()
                .unwrap_or("")
                .contains("retrieveUserQuotaSummary")
        })
        .unwrap();
    assert!(quota["data"].as_str().unwrap_or("").contains("p-1"));
    let load = calls
        .iter()
        .find(|call| {
            call["url"]
                .as_str()
                .unwrap_or("")
                .contains("loadCodeAssist")
        })
        .unwrap();
    assert!(load["header"]["User-Agent"]
        .as_str()
        .unwrap_or("")
        .contains("os_type=linux"));

    let grok_call = calls
        .iter()
        .find(|call| call["url"].as_str().unwrap_or("").contains("/v1/billing"))
        .unwrap();
    assert_eq!(
        grok_call["header"]["Authorization"].as_str(),
        Some("Bearer $TOKEN$")
    );
}

#[test]
fn one_rejected_key_or_an_ip_ban_stops_the_hub() {
    let listing = r#"{"files":[{"id":"a","auth_index":"index-a","provider":"claude","email":"a@example.com"}]}"#;
    for (status, body, expected) in [
        (
            401,
            r#"{"error":"invalid management key"}"#,
            "rejected the management key",
        ),
        (
            403,
            r#"{"error":"IP banned for 30 minutes"}"#,
            "blocked this computer",
        ),
    ] {
        let hits = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counted = hits.clone();
        let http = Fake::new(move |request| {
            if request.url.ends_with("/auth-files") {
                return Ok(response(200, listing));
            }
            counted.fetch_add(1, Ordering::SeqCst);
            Ok(response(status as u16, body))
        });
        let world = World::new(http);
        world
            .vault
            .save(Uuid::nil(), &Credential::ApiKey("unused".into()))
            .ok();
        world.connect("http://mini.local:8317", "mgmt-key").unwrap();
        let account = world.settings.accounts().remove(0);
        let client = world.client(&account);
        let mut errors = Vec::new();
        std::thread::scope(|scope| {
            let mut handles = Vec::new();
            for _ in 0..6 {
                let client = client.clone();
                handles.push(scope.spawn(move || {
                    client.send(HttpRequest::get(
                        "https://api.anthropic.com/api/oauth/usage",
                    ))
                }));
            }
            for handle in handles {
                errors.push(handle.join().unwrap().unwrap_err().to_string());
            }
        });
        assert!(
            errors.iter().all(|error| error.contains(expected)),
            "{errors:?}"
        );
        assert_eq!(hits.load(Ordering::SeqCst), 1, "{expected}");
    }
}

#[test]
fn a_late_rejection_of_the_replaced_key_does_not_stop_the_new_key() {
    let listing = r#"{"files":[{"id":"a","auth_index":"index-a","provider":"claude","email":"a@example.com"}]}"#;
    let http = Fake::new(move |request| {
        let auth = request.headers.get("Authorization").map(String::as_str);
        if auth != Some("Bearer right-key") {
            return Ok(response(401, r#"{"error":"invalid management key"}"#));
        }
        if request.url.ends_with("/auth-files") {
            return Ok(response(200, listing));
        }
        Ok(envelope(200, CLAUDE_USAGE, &[]))
    });
    let world = World::new(http);
    let hub = world
        .connect("http://mini.local:8317", "right-key")
        .unwrap();
    world
        .vault
        .save(hub.id, &Credential::ApiKey("wrong-key".into()))
        .unwrap();
    let account = world.settings.accounts().remove(0);
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    *world.http.pause.lock().unwrap() = Some(("Bearer wrong-key".into(), entered_tx, release_rx));
    let client = world.client(&account);
    let inflight = std::thread::spawn(move || {
        client.send(HttpRequest::get(
            "https://api.anthropic.com/api/oauth/usage",
        ))
    });
    entered_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("the rejected call did not start");
    let error = replace_key(
        hub.id,
        "still-wrong",
        world.http.as_ref(),
        world.vault.as_ref(),
        &world.settings,
        &world.gate,
    )
    .unwrap_err();
    assert!(error.to_string().contains("rejected the management key"));
    assert_eq!(
        world.vault.load(hub.id).unwrap(),
        Some(Credential::ApiKey("wrong-key".into()))
    );
    replace_key(
        hub.id,
        "right-key",
        world.http.as_ref(),
        world.vault.as_ref(),
        &world.settings,
        &world.gate,
    )
    .unwrap();
    assert_eq!(
        world.vault.load(hub.id).unwrap(),
        Some(Credential::ApiKey("right-key".into()))
    );
    release_tx.send(()).unwrap();
    let late = inflight.join().unwrap().unwrap_err();
    assert!(
        late.to_string().contains("rejected the management key"),
        "{late}"
    );
    let client = world.client(&account);
    let response = client
        .send(HttpRequest::get(
            "https://api.anthropic.com/api/oauth/usage",
        ))
        .unwrap();
    assert_eq!(response.status, 200);
    assert!(String::from_utf8_lossy(&response.body).contains("five_hour"));
}

#[test]
fn hub_answers_use_the_swift_wording() {
    let cases: &[(&str, u16, &str, &str)] = &[
        (
            "remote",
            403,
            r#"{"error":"remote management disabled"}"#,
            "only allows management from its own machine",
        ),
        ("off", 404, "", "has its management API turned off"),
        (
            "gone",
            400,
            r#"{"error":"auth credential not found"}"#,
            "no longer on the CLIProxyAPI hub",
        ),
        (
            "token",
            400,
            r#"{"error":"auth token not found"}"#,
            "has no working sign-in for this login",
        ),
        ("upstream", 502, "", "couldn\u{2019}t reach Claude"),
        (
            "other",
            418,
            r#"{"error":"teapot"}"#,
            "answered with an error (HTTP 418)",
        ),
    ];
    for (_name, status, body, expected) in cases {
        let http = Fake::new(move |request| {
            if request.url.ends_with("/auth-files") {
                return Ok(response(
                    200,
                    r#"{"files":[{"id":"a","auth_index":"index-a","provider":"claude","email":"a@example.com"}]}"#,
                ));
            }
            Ok(response(*status, body))
        });
        let world = World::new(http);
        world.connect("http://mini.local:8317", "mgmt-key").unwrap();
        let account = world.settings.accounts().remove(0);
        let error = world
            .client(&account)
            .send(HttpRequest::get(
                "https://api.anthropic.com/api/oauth/usage",
            ))
            .unwrap_err();
        assert!(error.to_string().contains(expected), "{}: {error}", error);
        if *status == 502 {
            assert!(matches!(error, ProviderError::Unreachable(_)));
        }
    }

    let http = Fake::new(|request| {
        if request.url.ends_with("/auth-files") {
            return Ok(response(
                200,
                r#"{"files":[{"id":"a","auth_index":"index-a","provider":"claude","email":"a@example.com"}]}"#,
            ));
        }
        Err(ProviderError::ConnectionFailed)
    });
    let world = World::new(http);
    world.connect("http://mini.local:8317", "mgmt-key").unwrap();
    let account = world.settings.accounts().remove(0);
    let error = world
        .client(&account)
        .send(HttpRequest::get("https://example.test/usage"))
        .unwrap_err();
    assert_eq!(
        error.to_string(),
        "Can\u{2019}t reach the CLIProxyAPI hub at mini.local:8317."
    );
    assert!(matches!(error, ProviderError::Unreachable(_)));

    let http = Fake::new(|request| {
        if request.url.ends_with("/auth-files") {
            return Ok(response(
                200,
                r#"{"files":[{"id":"b","auth_index":"index-b","provider":"codex","email":"b@example.com","id_token":{"chatgpt_account_id":"acct-b"}}]}"#,
            ));
        }
        let payload: Value = serde_json::from_slice(request.body.as_deref().unwrap()).unwrap();
        if payload["url"].as_str().unwrap().contains("wham/usage") {
            return Ok(envelope(
                429,
                r#"{"error":"slow down"}"#,
                &[("Retry-After", "120")],
            ));
        }
        Ok(envelope(200, "{}", &[]))
    });
    let world = World::new(http);
    world.connect("http://mini.local:8317", "mgmt-key").unwrap();
    let account = world.settings.accounts().remove(0);
    let mut state = ProviderState::default();
    let error = fetch(
        &account,
        world.client(&account).as_ref(),
        world.vault.as_ref(),
        &mut state,
    )
    .unwrap_err();
    assert!(
        matches!(error, ProviderError::RateLimited { retry_after_seconds: Some(seconds) } if seconds == 120.0),
        "{error:?}"
    );

    let http = Fake::new(|request| {
        if request.url.ends_with("/auth-files") {
            return Ok(response(
                200,
                r#"{"files":[{"id":"a","auth_index":"index-a","provider":"claude","email":"a@example.com"}]}"#,
            ));
        }
        Ok(envelope(401, r#"{"error":"expired"}"#, &[]))
    });
    let world = World::new(http);
    world.connect("http://mini.local:8317", "mgmt-key").unwrap();
    let account = world.settings.accounts().remove(0);
    let mut state = ProviderState::default();
    let error = fetch(
        &account,
        world.client(&account).as_ref(),
        world.vault.as_ref(),
        &mut state,
    )
    .unwrap_err();
    assert_eq!(
        error.to_string(),
        "The CLIProxyAPI hub at mini.local:8317 has no working sign-in for this login. Sign it in again on the hub."
    );

    let http = Fake::new(|request| {
        if request.url.ends_with("/auth-files") {
            return Ok(response(
                200,
                r#"{"files":[{"id":"a","auth_index":"index-a","provider":"claude","email":"a@example.com"}]}"#,
            ));
        }
        Ok(response(200, "not-an-envelope"))
    });
    let world = World::new(http);
    world.connect("http://mini.local:8317", "mgmt-key").unwrap();
    let account = world.settings.accounts().remove(0);
    let error = world
        .client(&account)
        .send(HttpRequest::get(
            "https://api.anthropic.com/api/oauth/usage",
        ))
        .unwrap_err();
    assert_eq!(
        error.to_string(),
        "The provider returned an invalid response."
    );
}

#[test]
fn a_hub_link_without_a_management_key_sends_nothing() {
    let hits = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counted = hits.clone();
    let http = Fake::new(move |_| {
        counted.fetch_add(1, Ordering::SeqCst);
        Ok(response(200, "{}"))
    });
    let world = World::new(http);
    let link = HubLink {
        hub_id: Uuid::new_v4(),
        hub_url: "http://evil.example/collect".into(),
        login_id: "a".into(),
        auth_index: "index-a".into(),
    };
    let mut account = Account::new(ProviderKind::Claude, "a@example.com", None);
    account.hub = Some(link.to_value());
    world
        .vault
        .save(account.id, &Credential::ApiKey("direct-secret".into()))
        .unwrap();
    let error = world
        .client(&account)
        .send(HttpRequest::get(
            "https://api.anthropic.com/api/oauth/usage",
        ))
        .unwrap_err();
    assert!(error.to_string().contains("management key"), "{error}");
    assert_eq!(hits.load(Ordering::SeqCst), 0);
}

impl World {
    fn settings(&self) -> &Settings {
        &self.settings
    }
}

#[test]
fn an_edited_link_never_sends_the_management_key_elsewhere() {
    let world = World::new(routing(LISTING));
    world.connect("http://mini.local:8317", "mgmt-key").unwrap();
    let mut account = world
        .settings
        .accounts()
        .into_iter()
        .find(|account| account.hub.is_some())
        .unwrap();

    // A link pointing the connected hub's id at another address still goes to the stored hub.
    let mut link = account.hub_link().unwrap();
    link.hub_url = "http://evil.example:9999".into();
    account.hub = Some(link.to_value());
    let before = world.http.len();
    let _ = world.client(&account).send(HttpRequest::get(
        "https://api.anthropic.com/api/oauth/usage",
    ));
    let sent = world.http.requests()[before..].to_vec();
    assert!(!sent.is_empty());
    assert!(
        sent.iter()
            .all(|request| request.url.starts_with("http://mini.local:8317/")),
        "{sent:?}"
    );

    // A link naming no connected hub sends nothing at all.
    let mut stray = account.hub_link().unwrap();
    stray.hub_id = Uuid::new_v4();
    account.hub = Some(stray.to_value());
    let before = world.http.len();
    assert!(world
        .client(&account)
        .send(HttpRequest::get(
            "https://api.anthropic.com/api/oauth/usage"
        ))
        .is_err());
    assert_eq!(world.http.len(), before);
}
