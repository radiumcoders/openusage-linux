use std::sync::Mutex;

use chrono::{DateTime, Duration, Utc};

use super::*;
use crate::account::{Account, Credential};
use crate::login::{LoginPrompt, LoginPrompter};
use crate::metric::{MetricKind, MetricLine, MetricValue, ProgressFormat};
use crate::provider::ProviderState;
use crate::vault::MemoryVault;

const MANAGEMENT_KEY: &str = r#"{"data":{"label":"sk-or-v1-0e6...1c96","limit":null,"limit_remaining":null,"limit_reset":null,"include_byok_in_limit":false,"usage":0,"usage_daily":0,"usage_weekly":0,"usage_monthly":0,"byok_usage":0,"byok_usage_daily":0,"byok_usage_weekly":0,"byok_usage_monthly":0,"is_free_tier":false,"is_management_key":true,"is_provisioning_key":true}}"#;
const STANDARD_KEY: &str = r#"{"data":{"label":"sk-or-v1-au7...890","limit":100,"limit_remaining":74.5,"limit_reset":"monthly","include_byok_in_limit":false,"usage":25.5,"usage_daily":1.5,"usage_weekly":4,"usage_monthly":12,"byok_usage":17.38,"byok_usage_daily":17.38,"byok_usage_weekly":17.38,"byok_usage_monthly":17.38,"is_free_tier":false,"is_management_key":false,"is_provisioning_key":false,"effective_budget":{"scope":"member","guardrail_id":"0df9e665-d932-5740-b2c7-b52af166bc11","budget_id":null,"limit_usd":50,"spend_usd":32.5,"remaining_usd":17.5,"reset_interval":"weekly","resets_at":"2026-11-02T00:00:00.000Z","include_byok":false},"free_model_daily_requests":{"used":12,"limit":50,"remaining":38}}}"#;
const CREDITS: &str = r#"{"data":{"total_credits":100,"total_usage":22.25}}"#;
const QUIET_DAY: &str = "2026-10-20";

fn thursday() -> DateTime<Utc> {
    DateTime::parse_from_rfc3339("2026-10-29T11:00:00Z")
        .unwrap()
        .with_timezone(&Utc)
}

fn at(text: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(text)
        .unwrap()
        .with_timezone(&Utc)
}

struct Api {
    seen: Mutex<Vec<String>>,
    credits_status: Mutex<u16>,
    activity_status: Mutex<u16>,
    keys_status: Mutex<u16>,
    standard: bool,
}

impl HttpClient for Api {
    fn send(&self, request: HttpRequest) -> Result<HttpResponse, ProviderError> {
        assert!(request
            .headers
            .get("Authorization")
            .is_some_and(|value| value.starts_with("Bearer ") && value.len() > "Bearer ".len()));
        assert_eq!(
            request.headers.get("Accept").map(String::as_str),
            Some("application/json")
        );
        self.seen.lock().unwrap().push(request.url.clone());
        let url = url::Url::parse(&request.url).unwrap();
        let (status, body) = match url.path() {
            "/api/v1/key" => (
                200,
                if self.standard {
                    STANDARD_KEY
                } else {
                    MANAGEMENT_KEY
                }
                .to_string(),
            ),
            "/api/v1/credits" => (*self.credits_status.lock().unwrap(), CREDITS.to_string()),
            "/api/v1/keys" => {
                let status = *self.keys_status.lock().unwrap();
                let offset = url
                    .query_pairs()
                    .find(|(name, _)| name == "offset")
                    .map(|(_, value)| value.into_owned());
                let body = match offset.as_deref() {
                    Some("0") => mapper::keys_page_body(100, 0.25, 0.5, 1.0),
                    Some("100") => mapper::keys_page_body(1, 1.5, 4.0, 12.0),
                    _ => "{}".into(),
                };
                (status, body)
            }
            "/api/v1/activity" => {
                let status = *self.activity_status.lock().unwrap();
                let date = url
                    .query_pairs()
                    .find(|(name, _)| name == "date")
                    .map(|(_, value)| value.into_owned());
                (status, activity_body(date.as_deref()))
            }
            _ => (404, String::new()),
        };
        Ok(HttpResponse {
            status,
            headers: Default::default(),
            body: body.into_bytes(),
        })
    }
}

fn activity_body(date: Option<&str>) -> String {
    let dates = match date {
        Some(date) => vec![date.to_string()],
        None => (1..=30)
            .map(|offset| {
                (thursday() - Duration::days(offset))
                    .format("%Y-%m-%d")
                    .to_string()
            })
            .collect(),
    };
    let rows = dates
        .into_iter()
        .flat_map(|date| activity_rows(&date))
        .collect::<Vec<_>>()
        .join(",");
    format!(r#"{{"data":[{rows}]}}"#)
}

fn activity_rows(date: &str) -> Vec<String> {
    if date == QUIET_DAY {
        return Vec::new();
    }
    vec![
        format!(
            r#"{{"date":"{date}","model":"anthropic/claude-sonnet-4.5","usage":1.25,"byok_usage_inference":0.5,"requests":10,"prompt_tokens":1000,"completion_tokens":200,"reasoning_tokens":50,"cached_tokens":600}}"#
        ),
        format!(
            r#"{{"date":"{date}","model":"openai/gpt-5","usage":0.25,"byok_usage_inference":0,"requests":2,"prompt_tokens":3000,"completion_tokens":100,"reasoning_tokens":0,"cached_tokens":0}}"#
        ),
    ]
}

fn api(standard: bool) -> Api {
    Api {
        seen: Mutex::new(vec![]),
        credits_status: Mutex::new(200),
        activity_status: Mutex::new(200),
        keys_status: Mutex::new(200),
        standard,
    }
}

fn fetch(
    http: &Api,
    state: &mut ProviderState,
    at: DateTime<Utc>,
    force: bool,
) -> Result<ProviderUsage, ProviderError> {
    let account = Account::new(ProviderKind::OpenRouter, "Work", None);
    let vault = MemoryVault::with(account.id, Credential::ApiKey("key".into()));
    let mut ctx = FetchContext {
        account: &account,
        http,
        vault: &vault,
        state,
        now: at,
        force,
    };
    provider().fetch(&mut ctx)
}

fn dollars(usage: &ProviderUsage, label: &str) -> Option<f64> {
    usage
        .lines
        .iter()
        .find(|line| line.label() == label)
        .and_then(|line| match line {
            MetricLine::Values { values, .. } => values.first().map(|value| value.number),
            _ => None,
        })
}

fn paths(http: &Api) -> Vec<String> {
    http.seen
        .lock()
        .unwrap()
        .iter()
        .map(|url| url::Url::parse(url).unwrap().path().to_string())
        .collect()
}

fn activity_dates(http: &Api) -> Vec<String> {
    http.seen
        .lock()
        .unwrap()
        .iter()
        .filter(|url| url.contains("/api/v1/activity"))
        .map(|url| {
            url::Url::parse(url)
                .unwrap()
                .query_pairs()
                .find(|(name, _)| name == "date")
                .map(|(_, value)| value.into_owned())
                .unwrap_or_else(|| "all".into())
        })
        .collect()
}

#[test]
fn a_management_key_shows_the_accounts_spend_summed_over_every_page_of_keys() {
    let http = api(false);
    let usage = fetch(&http, &mut ProviderState::default(), thursday(), false).unwrap();
    assert_eq!(
        usage
            .lines
            .iter()
            .map(|line| line.label())
            .collect::<Vec<_>>(),
        [
            "Balance",
            "Today",
            "Yesterday",
            "This Week",
            "This Month",
            "Prior 30 Days"
        ]
    );
    assert_eq!(dollars(&usage, "Today"), Some(26.5));
    assert_eq!(dollars(&usage, "This Week"), Some(54.0));
    assert_eq!(dollars(&usage, "This Month"), Some(112.0));
    assert_eq!(dollars(&usage, "Balance"), Some(77.75));
    assert_eq!(dollars(&usage, "Yesterday"), Some(1.5));
    assert_eq!(dollars(&usage, "Prior 30 Days"), Some(43.5));
    let prior = &usage.breakdowns["prior30"].models;
    assert_eq!(
        prior.iter().map(|model| model.name.as_str()).collect::<Vec<_>>(),
        ["anthropic/claude-sonnet-4.5", "openai/gpt-5"]
    );
    assert_eq!(prior[0].dollars, Some(36.25));
    assert_eq!(prior[0].tokens, Some(34_800.0));
    assert_eq!(prior[1].dollars, Some(7.25));
    assert_eq!(prior[1].tokens, Some(89_900.0));
    assert!(usage.breakdowns.get("today").is_none());
    let week = &usage.breakdowns["week"].models;
    assert_eq!(week[0].name, "anthropic/claude-sonnet-4.5");
    assert_eq!(week[0].tokens, Some(3_600.0));
    assert_eq!(week[1].tokens, Some(9_300.0));
    let yesterday = &usage.breakdowns["yesterday"].models;
    assert_eq!(yesterday[0].dollars, Some(1.25));
    assert_eq!(yesterday[0].tokens, Some(1_200.0));
    assert_eq!(yesterday[1].dollars, Some(0.25));
    assert_eq!(yesterday[1].tokens, Some(3_100.0));
    assert!(usage.lines.iter().all(|line| line.label() != "Credits"));
    assert_eq!(usage.plan.as_deref(), Some("Pay-as-you-go"));
    assert_eq!(
        paths(&http),
        [
            "/api/v1/key",
            "/api/v1/credits",
            "/api/v1/keys",
            "/api/v1/keys",
            "/api/v1/activity"
        ]
    );
    assert!(http
        .seen
        .lock()
        .unwrap()
        .iter()
        .all(|url| url.starts_with("https://openrouter.ai/")));
    let seen = http.seen.lock().unwrap();
    for (url, offset) in seen
        .iter()
        .filter(|url| url.contains("/api/v1/keys"))
        .zip(["0", "100"])
    {
        let query: Vec<(String, String)> = url::Url::parse(url)
            .unwrap()
            .query_pairs()
            .map(|(key, value)| (key.into_owned(), value.into_owned()))
            .collect();
        assert!(query.contains(&("include_disabled".into(), "true".into())));
        assert!(query.contains(&("include_expired".into(), "true".into())));
        assert!(query.contains(&("offset".into(), offset.into())));
    }
}

#[test]
fn a_workspace_scoped_management_key_shows_its_spend_without_credits() {
    let http = api(false);
    *http.credits_status.lock().unwrap() = 403;
    let usage = fetch(&http, &mut ProviderState::default(), thursday(), false).unwrap();
    assert!(usage.lines.iter().all(|line| line.label() != "Balance"));
    assert_eq!(dollars(&usage, "Today"), Some(26.5));
    assert_eq!(dollars(&usage, "This Week"), Some(54.0));
    assert_eq!(dollars(&usage, "This Month"), Some(112.0));
    assert_eq!(
        paths(&http),
        [
            "/api/v1/key",
            "/api/v1/credits",
            "/api/v1/keys",
            "/api/v1/keys",
            "/api/v1/activity"
        ]
    );
}

#[test]
fn a_standard_key_shows_its_own_spend_and_limit_without_asking_for_credits() {
    let http = api(true);
    let usage = fetch(&http, &mut ProviderState::default(), thursday(), false).unwrap();
    assert_eq!(
        usage.lines,
        vec![
            MetricLine::values("Today", vec![MetricValue::new(1.5, MetricKind::Dollars)]),
            MetricLine::values(
                "This Week",
                vec![MetricValue::new(4.0, MetricKind::Dollars)]
            ),
            MetricLine::values(
                "This Month",
                vec![MetricValue::new(12.0, MetricKind::Dollars)]
            ),
            MetricLine::Progress {
                label: "Key Limit".into(),
                used: 25.5,
                limit: 100.0,
                format: ProgressFormat::Dollars,
                resets_at: Some(at("2026-11-01T00:00:00Z")),
                period_duration_ms: Some(31 * mapper::DAY_MS),
            },
            MetricLine::Progress {
                label: "Budget".into(),
                used: 32.5,
                limit: 50.0,
                format: ProgressFormat::Dollars,
                resets_at: Some(at("2026-11-02T00:00:00Z")),
                period_duration_ms: Some(7 * mapper::DAY_MS),
            },
            MetricLine::Progress {
                label: "Free Requests".into(),
                used: 12.0,
                limit: 50.0,
                format: ProgressFormat::Count {
                    suffix: "requests".into()
                },
                resets_at: Some(at("2026-10-30T00:00:00Z")),
                period_duration_ms: Some(mapper::DAY_MS),
            },
        ]
    );
    assert_eq!(paths(&http), ["/api/v1/key"]);
}

#[test]
fn activity_is_read_again_hourly_and_a_new_utc_day_at_once() {
    let http = api(false);
    let mut state = ProviderState::default();
    let usage = fetch(&http, &mut state, thursday(), false).unwrap();
    assert_eq!(dollars(&usage, "Prior 30 Days"), Some(43.5));
    fetch(&http, &mut state, thursday() + Duration::minutes(5), false).unwrap();
    assert_eq!(
        activity_dates(&http),
        ["all"],
        "a pass within the hour reads no activity"
    );
    fetch(
        &http,
        &mut state,
        thursday() + Duration::seconds(3660),
        false,
    )
    .unwrap();
    assert_eq!(activity_dates(&http), ["all", "2026-10-28"]);
    fetch(
        &http,
        &mut state,
        thursday() + Duration::seconds(3660),
        true,
    )
    .unwrap();
    assert_eq!(activity_dates(&http), ["all", "2026-10-28", "2026-10-28"]);
    let usage = fetch(&http, &mut state, at("2026-10-30T00:05:00Z"), false).unwrap();
    assert_eq!(
        activity_dates(&http),
        ["all", "2026-10-28", "2026-10-28", "2026-10-29"]
    );
    assert_eq!(dollars(&usage, "Prior 30 Days"), Some(43.5));
}

#[test]
fn a_failed_activity_read_hides_its_rows_keeps_the_card_and_waits_an_hour() {
    let http = api(false);
    *http.activity_status.lock().unwrap() = 500;
    let mut state = ProviderState::default();
    let usage = fetch(&http, &mut state, thursday(), false).unwrap();
    assert!(usage
        .lines
        .iter()
        .all(|line| line.label() != "Yesterday" && line.label() != "Prior 30 Days"));
    assert_eq!(dollars(&usage, "Today"), Some(26.5));
    assert_eq!(dollars(&usage, "This Week"), Some(54.0));
    assert_eq!(dollars(&usage, "This Month"), Some(112.0));
    *http.activity_status.lock().unwrap() = 200;
    fetch(&http, &mut state, thursday() + Duration::minutes(5), false).unwrap();
    assert_eq!(activity_dates(&http), ["all"]);
    let usage = fetch(
        &http,
        &mut state,
        thursday() + Duration::seconds(3660),
        false,
    )
    .unwrap();
    assert_eq!(activity_dates(&http), ["all", "all"]);
    assert_eq!(dollars(&usage, "Prior 30 Days"), Some(43.5));
}

#[test]
fn a_failed_credits_or_keys_page_fails_the_card_and_a_refused_key_is_rejected() {
    let http = api(false);
    *http.credits_status.lock().unwrap() = 503;
    assert_eq!(
        fetch(&http, &mut ProviderState::default(), thursday(), false),
        Err(ProviderError::RequestFailed(503))
    );
    *http.credits_status.lock().unwrap() = 200;
    *http.keys_status.lock().unwrap() = 503;
    assert_eq!(
        fetch(&http, &mut ProviderState::default(), thursday(), false),
        Err(ProviderError::RequestFailed(503))
    );
    assert_eq!(
        fetch_refused(&Refused),
        Err(ProviderError::KeyRejected(
            "OpenRouter rejected the API key.".into()
        ))
    );
}

struct Refused;

impl HttpClient for Refused {
    fn send(&self, _request: HttpRequest) -> Result<HttpResponse, ProviderError> {
        Ok(HttpResponse {
            status: 401,
            headers: Default::default(),
            body: br#"{"error":{"message":"Unauthorized"}}"#.to_vec(),
        })
    }
}

fn fetch_refused(http: &Refused) -> Result<ProviderUsage, ProviderError> {
    let account = Account::new(ProviderKind::OpenRouter, "Work", None);
    let vault = MemoryVault::with(account.id, Credential::ApiKey("key".into()));
    let mut state = ProviderState::default();
    let mut ctx = FetchContext {
        account: &account,
        http,
        vault: &vault,
        state: &mut state,
        now: thursday(),
        force: true,
    };
    provider().fetch(&mut ctx)
}

#[test]
fn sign_in_checks_the_key_and_names_the_account_open_router() {
    let http = api(true);
    let prompter = Lines {
        line: Mutex::new(Some("sk-or-v1-test".into())),
        shown: Mutex::new(None),
    };
    let (label, credential) = login(&LoginMethod::Default)
        .login(&prompter, &http)
        .unwrap();
    assert_eq!(label, "OpenRouter");
    assert_eq!(credential, Credential::ApiKey("sk-or-v1-test".into()));
    assert_eq!(
        prompter.shown.lock().unwrap().clone(),
        Some(LoginPrompt::Secret {
            label: "OpenRouter API key".into(),
            key_page: Some("https://openrouter.ai/settings/management-keys".into()),
        })
    );
    let refused = Lines {
        line: Mutex::new(Some("bad".into())),
        shown: Mutex::new(None),
    };
    assert_eq!(
        login(&LoginMethod::Default)
            .login(&refused, &Refused)
            .unwrap_err(),
        crate::error::LoginError::Failed("OpenRouter rejected the API key.".into())
    );
}

struct Lines {
    line: Mutex<Option<String>>,
    shown: Mutex<Option<LoginPrompt>>,
}

impl LoginPrompter for Lines {
    fn show(&self, prompt: &LoginPrompt) {
        *self.shown.lock().unwrap() = Some(prompt.clone());
    }
    fn read_line(&self, _secret: bool) -> Option<String> {
        self.line.lock().unwrap().take()
    }
    fn pasted_codes(&self) -> std::sync::mpsc::Receiver<String> {
        std::sync::mpsc::channel().1
    }
}
