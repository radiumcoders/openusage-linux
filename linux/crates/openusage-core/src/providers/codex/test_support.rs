// Mapping, history cadence, renewal, and sign-in.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use chrono::{TimeZone, Utc};

use crate::account::{Account, Credential, OAuthTokens};
use crate::error::{LoginError, ProviderError};
use crate::http::{HttpClient, HttpRequest, HttpResponse};
use crate::kind::ProviderKind;
use crate::login::{LoginPrompt, LoginPrompter};
use crate::metric::{MetricKind, MetricLine, MetricValue, ProgressFormat};
use crate::provider::{FetchContext, LoginMethod, ProviderState};
use crate::vault::Vault;

use super::client::{self, CLIENT_ID};
use super::mapper;
use super::provider;

fn at(seconds: i64) -> chrono::DateTime<Utc> {
    Utc.timestamp_opt(seconds, 0).single().unwrap()
}

fn response(status: u16, body: &str) -> HttpResponse {
    HttpResponse {
        status,
        headers: BTreeMap::new(),
        body: body.as_bytes().to_vec(),
    }
}

fn with_header(mut response: HttpResponse, name: &str, value: &str) -> HttpResponse {
    response.headers.insert(name.into(), value.into());
    response
}

fn map(body: &str) -> mapper::Mapped {
    mapper::map_usage(&response(200, body), None, at(1_800_000_000)).unwrap()
}

fn meters(lines: &[MetricLine]) -> Vec<(String, f64, Option<i64>)> {
    lines
        .iter()
        .filter_map(|line| match line {
            MetricLine::Progress {
                label,
                used,
                limit,
                format: ProgressFormat::Percent,
                period_duration_ms,
                ..
            } if *limit == 100.0 => Some((label.clone(), *used, *period_duration_ms)),
            _ => None,
        })
        .collect()
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

fn jwt(payload: &str) -> String {
    use base64::Engine;
    let encode = |bytes: &[u8]| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes);
    format!(
        "{}.{}.sig",
        encode(br#"{"alg":"none"}"#),
        encode(payload.as_bytes())
    )
}

fn account_tokens() -> OAuthTokens {
    let mut tokens = OAuthTokens::new("access");
    tokens.refresh_token = Some("refresh".into());
    tokens.expires_at = Some(at(2_000_000_000));
    tokens
        .extras
        .insert("chatgpt_account_id".into(), "acct-fake".into());
    tokens
}

const USAGE: &str = r#"{"plan_type":"pro","rate_limit":{"primary_window":{"used_percent":29,"limit_window_seconds":604800,"reset_at":1793915535}},"credits":{"balance":"0","has_credits":false},"rate_limit_reset_credits":{"available_count":0}}"#;

const COUNTS: &str = r#"{"data":[
 {"date":"2026-10-31","totals":{"text_total_tokens":0,"turns":0}},
 {"date":"2026-10-02","totals":{"cached_text_input_tokens":470000000,"text_output_tokens":363029,"text_total_tokens":495363029,"turns":392,"uncached_text_input_tokens":25000000}},
 {"date":"2026-10-29","totals":{"cached_text_input_tokens":1475954688,"text_output_tokens":2180573,"text_total_tokens":1511437552,"turns":86,"uncached_text_input_tokens":33302291}},
 {"date":"2026-10-30","totals":{"cached_text_input_tokens":7183232,"text_output_tokens":27164,"text_total_tokens":8139475,"turns":0,"uncached_text_input_tokens":929079}}
]}"#;

fn path_of(url: &str) -> String {
    url::Url::parse(url)
        .map(|url| url.path().to_string())
        .unwrap_or_default()
}

fn query_of(url: &str) -> Vec<String> {
    url::Url::parse(url)
        .map(|url| {
            url.query_pairs()
                .map(|(key, value)| format!("{key}={value}"))
                .collect()
        })
        .unwrap_or_default()
}

struct Script {
    plan: String,
    counts_status: u16,
    credit_status: u16,
    counts_body: String,
    credit_body: String,
    paths: Mutex<Vec<String>>,
}

impl Script {
    fn personal() -> Arc<Self> {
        Arc::new(Self {
            plan: "pro".into(),
            counts_status: 200,
            credit_status: 200,
            counts_body: COUNTS.into(),
            credit_body: "{}".into(),
            paths: Mutex::new(Vec::new()),
        })
    }
}

impl HttpClient for Script {
    fn send(&self, request: HttpRequest) -> Result<HttpResponse, ProviderError> {
        let path = path_of(&request.url);
        self.paths.lock().unwrap().push(path.clone());
        let body = if path == "/backend-api/wham/usage" {
            USAGE.replace("\"pro\"", &format!("\"{}\"", self.plan))
        } else if path.ends_with("daily-workspace-usage-counts") {
            assert_eq!(
                request
                    .headers
                    .get("ChatGPT-Account-Id")
                    .map(String::as_str),
                Some("acct-fake")
            );
            assert_eq!(
                request.headers.get("Authorization").map(String::as_str),
                Some("Bearer access")
            );
            assert_eq!(request.timeout, Duration::from_secs(15));
            return Ok(response(self.counts_status, &self.counts_body));
        } else if path.contains("/usage/") {
            assert_eq!(request.headers.get("OpenAI-Beta"), None);
            return Ok(with_header(
                response(self.credit_status, &self.credit_body),
                "retry-after",
                "3600",
            ));
        } else if path.ends_with("rate-limit-reset-credits") {
            assert_eq!(
                request.headers.get("OpenAI-Beta").map(String::as_str),
                Some("codex-1")
            );
            assert_eq!(
                request.headers.get("originator").map(String::as_str),
                Some("Codex Desktop")
            );
            "{}".into()
        } else {
            return Ok(response(404, ""));
        };
        Ok(response(200, &body))
    }
}

fn fetch(
    script: &Script,
    now: chrono::DateTime<Utc>,
    force: bool,
    state: &mut ProviderState,
) -> crate::metric::ProviderUsage {
    let account = Account::new(ProviderKind::Codex, "ChatGPT", None);
    let vault = SharedVault::new(Credential::OAuth(account_tokens()));
    let mut ctx = FetchContext {
        account: &account,
        http: script,
        vault: &vault,
        state,
        now,
        force,
    };
    provider().fetch(&mut ctx).unwrap()
}

fn token_rows(usage: &crate::metric::ProviderUsage) -> Vec<(String, f64)> {
    usage
        .lines
        .iter()
        .filter_map(|line| match line {
            MetricLine::Values { label, values, .. }
                if values
                    .iter()
                    .any(|value| value.label.as_deref() == Some("tokens")) =>
            {
                Some((label.clone(), values[0].number))
            }
            _ => None,
        })
        .collect()
}

fn history_paths(script: &Script) -> Vec<String> {
    script
        .paths
        .lock()
        .unwrap()
        .iter()
        .filter(|path| path.contains("analytics/") || path.contains("/usage/"))
        .cloned()
        .collect()
}

const NOW: i64 = 1_793_433_600 + 8 * 3600;
