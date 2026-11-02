use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use chrono::{DateTime, TimeZone, Utc};
use serde_json::{json, Value};

use super::*;
use crate::account::Account;
use crate::login::{LoginPrompt, LoginPrompter};
use crate::metric::{MetricKind, MetricLine};
use crate::provider::ProviderState;
use crate::vault::MemoryVault;

const NOW: i64 = 1_793_392_200;

fn now() -> DateTime<Utc> {
    DateTime::from_timestamp(NOW, 0).unwrap()
}

fn usage_body(at: DateTime<Utc>, range: &str, legacy: bool) -> Value {
    let today = Utc
        .from_utc_datetime(&at.date_naive().and_hms_opt(0, 0, 0).unwrap())
        .timestamp();
    let days = if range == "30d" { 30 } else { 7 };
    let buckets: Vec<Value> = (0..=days)
        .rev()
        .map(|days_ago| {
            let scale = match days_ago {
                0 => 2.0,
                1 => 0.0,
                30 => 400.0,
                _ => 1.0,
            };
            let from = today - days_ago * 86_400;
            let until = if days_ago == 0 {
                at.timestamp()
            } else {
                from + 86_400
            };
            let mut bucket = json!({
                "from": stamp(from),
                "until": stamp(until),
                "request_count": (4.0 * scale) as i64
            });
            if !legacy {
                bucket["usage_usd"] = json!(0.25 * scale);
                bucket["input_tokens"] = json!((3_000.0 * scale) as i64);
                bucket["cached_input_tokens"] = json!((1_000.0 * scale) as i64);
                bucket["output_tokens"] = json!((300.0 * scale) as i64);
            }
            bucket
        })
        .collect();
    json!({"granularity": "day", "buckets": buckets})
}

fn stamp(seconds: i64) -> String {
    DateTime::from_timestamp(seconds, 0)
        .unwrap()
        .format("%Y-%m-%dT%H:%M:%SZ")
        .to_string()
}

struct Api {
    seen: Mutex<Vec<HttpRequest>>,
    usage_status: Mutex<u16>,
    legacy: bool,
    account: String,
    account_status: u16,
}

impl HttpClient for Api {
    fn send(&self, request: HttpRequest) -> Result<HttpResponse, ProviderError> {
        self.seen.lock().unwrap().push(request.clone());
        let path = url::Url::parse(&request.url).unwrap().path().to_string();
        let (status, body) = if path == "/api/me" {
            (self.account_status, self.account.clone())
        } else if path == "/api/usage" {
            let status = *self.usage_status.lock().unwrap();
            if status != 200 {
                (status, "{}".into())
            } else {
                let range = url::Url::parse(&request.url)
                    .unwrap()
                    .query_pairs()
                    .find(|(key, _)| key == "range")
                    .map(|(_, value)| value.into_owned())
                    .unwrap_or_else(|| "7d".into());
                (200, usage_body(now(), &range, self.legacy).to_string())
            }
        } else {
            (
                200,
                r#"{"included":{"balance_usd":72.5,"allowance_usd":100}}"#.into(),
            )
        };
        Ok(HttpResponse {
            status,
            headers: Default::default(),
            body: body.into_bytes(),
        })
    }
}

fn values(line: &MetricLine) -> Vec<(f64, MetricKind, Option<String>)> {
    match line {
        MetricLine::Values { values, .. } => values
            .iter()
            .map(|value| (value.number, value.kind, value.label.clone()))
            .collect(),
        other => panic!("{other:?}"),
    }
}

#[test]
fn rows_show_each_utc_day_and_tokens_count_cached_input_once() {
    let account = Account::new(ProviderKind::Ollama, "Ollama", None);
    let vault = MemoryVault::with(account.id, Credential::ApiKey("ollama-key".into()));
    let http = Api {
        seen: Mutex::new(vec![]),
        usage_status: Mutex::new(200),
        legacy: false,
        account: r#"{"Plan":"pro","Email":"dev@example.com"}"#.into(),
        account_status: 200,
    };
    let provider = provider();
    let mut state = ProviderState::default();
    let usage = fetch(
        provider.as_ref(),
        &account,
        &vault,
        &http,
        &mut state,
        now(),
        false,
    );
    assert_eq!(usage.plan.as_deref(), Some("Pro"));
    assert!(usage.lines.iter().any(|line| line.label() == "Monthly"));
    assert_eq!(
        values(
            usage
                .lines
                .iter()
                .find(|line| line.label() == "Today")
                .unwrap()
        ),
        vec![
            (0.5, MetricKind::Dollars, None),
            (6_600.0, MetricKind::Count, Some("tokens".into()))
        ]
    );
    assert_eq!(
        values(
            usage
                .lines
                .iter()
                .find(|line| line.label() == "Yesterday")
                .unwrap()
        ),
        vec![
            (0.0, MetricKind::Dollars, None),
            (0.0, MetricKind::Count, Some("tokens".into()))
        ]
    );
    assert_eq!(
        values(
            usage
                .lines
                .iter()
                .find(|line| line.label() == "Last 30 Days")
                .unwrap()
        ),
        vec![
            (7.5, MetricKind::Dollars, None),
            (99_000.0, MetricKind::Count, Some("tokens".into()))
        ]
    );
    assert_eq!(
        values(
            usage
                .lines
                .iter()
                .find(|line| line.label() == "Requests")
                .unwrap()
        ),
        vec![(120.0, MetricKind::Count, Some("in 30 days".into()))]
    );
    let queries = usage_queries(&http);
    assert_eq!(queries, vec!["range=30d&group_by=model".to_string()]);
    let request = http
        .seen
        .lock()
        .unwrap()
        .iter()
        .find(|request| request.url.contains("/api/usage"))
        .unwrap()
        .clone();
    assert!(request.url.starts_with("https://ollama.com/"));
    assert_eq!(request.method, "GET");
    assert_eq!(
        request.headers.get("Authorization").map(String::as_str),
        Some("Bearer ollama-key")
    );

    let kept = fetch(
        provider.as_ref(),
        &account,
        &vault,
        &http,
        &mut state,
        now() + chrono::Duration::minutes(5),
        false,
    );
    assert_eq!(
        usage_queries(&http).len(),
        1,
        "no history read within the hour"
    );
    assert!(kept.lines.iter().any(|line| line.label() == "Last 30 Days"));

    fetch(
        provider.as_ref(),
        &account,
        &vault,
        &http,
        &mut state,
        now() + chrono::Duration::minutes(61),
        false,
    );
    fetch(
        provider.as_ref(),
        &account,
        &vault,
        &http,
        &mut state,
        now() + chrono::Duration::minutes(62),
        true,
    );
    assert_eq!(
        usage_queries(&http),
        vec![
            "range=30d&group_by=model".to_string(),
            "range=7d&group_by=model".to_string(),
            "range=7d&group_by=model".to_string()
        ]
    );
}

#[test]
fn a_legacy_plan_shows_only_requests_and_a_failed_read_hides_rows_for_an_hour() {
    let account = Account::new(ProviderKind::Ollama, "Ollama", None);
    let vault = MemoryVault::with(account.id, Credential::ApiKey("ollama-key".into()));
    let http = Api {
        seen: Mutex::new(vec![]),
        usage_status: Mutex::new(200),
        legacy: true,
        account: r#"{"Plan":"free"}"#.into(),
        account_status: 200,
    };
    let mut state = ProviderState::default();
    let usage = fetch(
        provider().as_ref(),
        &account,
        &vault,
        &http,
        &mut state,
        now(),
        true,
    );
    assert!(usage.lines.iter().all(|line| line.label() != "Today"));
    assert_eq!(
        values(
            usage
                .lines
                .iter()
                .find(|line| line.label() == "Requests")
                .unwrap()
        ),
        vec![(120.0, MetricKind::Count, Some("in 30 days".into()))]
    );

    let http = Api {
        seen: Mutex::new(vec![]),
        usage_status: Mutex::new(500),
        legacy: false,
        account: "{}".into(),
        account_status: 200,
    };
    let mut state = ProviderState::default();
    let usage = fetch(
        provider().as_ref(),
        &account,
        &vault,
        &http,
        &mut state,
        now(),
        true,
    );
    assert!(usage.lines.iter().all(|line| !matches!(
        line.label(),
        "Today" | "Yesterday" | "Last 30 Days" | "Requests"
    )));
    assert!(usage.lines.iter().any(|line| line.label() == "Monthly"));
    *http.usage_status.lock().unwrap() = 200;
    fetch(
        provider().as_ref(),
        &account,
        &vault,
        &http,
        &mut state,
        now() + chrono::Duration::minutes(5),
        false,
    );
    assert_eq!(
        usage_queries(&http).len(),
        1,
        "a failed read waits an hour too"
    );
    let usage = fetch(
        provider().as_ref(),
        &account,
        &vault,
        &http,
        &mut state,
        now() + chrono::Duration::minutes(61),
        false,
    );
    assert_eq!(
        usage_queries(&http),
        vec![
            "range=30d&group_by=model".to_string(),
            "range=30d&group_by=model".to_string()
        ]
    );
    assert!(usage
        .lines
        .iter()
        .any(|line| line.label() == "Last 30 Days"));
}

fn fetch(
    provider: &dyn Provider,
    account: &Account,
    vault: &MemoryVault,
    http: &Api,
    state: &mut ProviderState,
    at: DateTime<Utc>,
    force: bool,
) -> ProviderUsage {
    let mut ctx = FetchContext {
        account,
        http,
        vault,
        state,
        now: at,
        force,
    };
    provider.fetch(&mut ctx).unwrap()
}

fn usage_queries(http: &Api) -> Vec<String> {
    http.seen
        .lock()
        .unwrap()
        .iter()
        .filter(|request| request.url.contains("/api/usage"))
        .map(|request| {
            url::Url::parse(&request.url)
                .unwrap()
                .query()
                .unwrap()
                .to_string()
        })
        .collect()
}

struct Lines {
    lines: Mutex<VecDeque<String>>,
    shown: Mutex<Vec<LoginPrompt>>,
}

impl LoginPrompter for Lines {
    fn show(&self, prompt: &LoginPrompt) {
        self.shown.lock().unwrap().push(prompt.clone());
    }
    fn read_line(&self, _secret: bool) -> Option<String> {
        self.lines.lock().unwrap().pop_front()
    }
    fn pasted_codes(&self) -> std::sync::mpsc::Receiver<String> {
        std::sync::mpsc::channel().1
    }
}

struct LoginHttp {
    account: String,
    account_status: u16,
    balance_status: u16,
    me: Arc<Mutex<u32>>,
}

impl HttpClient for LoginHttp {
    fn send(&self, request: HttpRequest) -> Result<HttpResponse, ProviderError> {
        let path = url::Url::parse(&request.url).unwrap().path().to_string();
        if path == "/api/me" {
            *self.me.lock().unwrap() += 1;
            Ok(HttpResponse {
                status: self.account_status,
                headers: Default::default(),
                body: self.account.as_bytes().to_vec(),
            })
        } else if path == "/api/balance" {
            Ok(HttpResponse {
                status: self.balance_status,
                headers: Default::default(),
                body: br#"{"included":{"balance_usd":72.5,"allowance_usd":100}}"#.to_vec(),
            })
        } else {
            Ok(HttpResponse {
                status: 200,
                headers: Default::default(),
                body: br#"{"included":{}}"#.to_vec(),
            })
        }
    }
}

fn sign_in(account: &str, status: u16) -> (String, u32) {
    let me = Arc::new(Mutex::new(0));
    let http = LoginHttp {
        account: account.into(),
        account_status: status,
        balance_status: 200,
        me: me.clone(),
    };
    let prompter = Lines {
        lines: Mutex::new(VecDeque::from(["ollama-key".into()])),
        shown: Mutex::new(vec![]),
    };
    let (label, credential) = login(&LoginMethod::Default)
        .login(&prompter, &http)
        .unwrap();
    assert_eq!(credential, Credential::ApiKey("ollama-key".into()));
    assert_eq!(*me.lock().unwrap(), 1);
    (label, 1)
}

#[test]
fn key_login_names_the_account_from_the_plan_read() {
    for account in [
        r#"{"Email":"dev@example.com","Name":"dev","Plan":"pro"}"#,
        r#"{"email":"dev@example.com","name":"dev","plan":"pro"}"#,
    ] {
        assert_eq!(sign_in(account, 200).0, "dev@example.com");
    }
    for (account, status) in [
        (r#"{"Name":"dev","Plan":"free"}"#, 200),
        (r#"{"Email":""}"#, 200),
        ("{}", 401),
    ] {
        assert_eq!(sign_in(account, status).0, "Ollama");
    }
}

#[test]
fn a_rejected_key_is_not_saved() {
    let http = LoginHttp {
        account: "{}".into(),
        account_status: 200,
        balance_status: 401,
        me: Arc::new(Mutex::new(0)),
    };
    let prompter = Lines {
        lines: Mutex::new(VecDeque::from(["bad".into()])),
        shown: Mutex::new(vec![]),
    };
    assert_eq!(
        login(&LoginMethod::Default).login(&prompter, &http),
        Err(LoginError::Failed("Ollama rejected the API key.".into()))
    );
    match &prompter.shown.lock().unwrap()[0] {
        LoginPrompt::Secret { label, key_page } => {
            assert_eq!(label, "Ollama API key");
            assert_eq!(
                key_page.as_deref(),
                Some("https://ollama.com/settings/keys")
            );
        }
        other => panic!("{other:?}"),
    }
    let empty = Lines {
        lines: Mutex::new(VecDeque::from(["  ".into()])),
        shown: Mutex::new(vec![]),
    };
    assert_eq!(
        login(&LoginMethod::Default).login(&empty, &http),
        Err(LoginError::InvalidCode)
    );
}
