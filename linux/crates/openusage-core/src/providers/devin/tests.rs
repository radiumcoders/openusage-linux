use std::sync::Mutex;

use super::client;
use super::mapper;
use super::*;
use crate::account::{Account, Credential, OAuthTokens};
use crate::error::{LoginError, ProviderError};
use crate::http::{HttpClient, HttpRequest, HttpResponse};
use crate::kind::ProviderKind;
use crate::login::{LoginPrompt, LoginPrompter};
use crate::metric::{MetricKind, MetricLine, MetricValue};
use crate::provider::{FetchContext, LoginMethod, ProviderState};
use crate::vault::MemoryVault;

fn wrapped(plan: &str) -> Vec<u8> {
    format!(r#"{{"userStatus":{{"planStatus":{plan}}}}}"#).into_bytes()
}

fn percent(label: &str, used: f64, period: i64) -> MetricLine {
    MetricLine::percent(label, used, None, Some(period))
}

#[test]
fn maps_the_plan_both_quotas_as_used_and_the_balance() {
    let usage = mapper::map(&wrapped(
        r#"{"planInfo":{"planName":" Max "},"dailyQuotaRemainingPercent":100,"weeklyQuotaRemainingPercent":40,"overageBalanceMicros":"964220000","dailyQuotaResetAtUnix":"1774080000","weeklyQuotaResetAtUnix":"1774166400"}"#,
    ))
    .unwrap();
    assert_eq!(usage.plan.as_deref(), Some("Max"));
    assert_eq!(
        usage.lines[0],
        MetricLine::percent(
            "Daily quota",
            0.0,
            chrono::DateTime::from_timestamp(1_774_080_000, 0),
            Some(86_400_000)
        )
    );
    assert_eq!(
        usage.lines[1],
        MetricLine::percent(
            "Weekly quota",
            60.0,
            chrono::DateTime::from_timestamp(1_774_166_400, 0),
            Some(7 * 86_400_000)
        )
    );
    assert_eq!(
        usage.lines[2],
        MetricLine::values(
            "Extra usage balance",
            vec![MetricValue::new(964.22, MetricKind::Dollars)]
        )
    );
}

#[test]
fn a_zero_or_negative_balance_reads_zero_dollars() {
    for micros in ["0", "-44347"] {
        let usage = mapper::map(&wrapped(&format!(
            r#"{{"weeklyQuotaRemainingPercent":40,"overageBalanceMicros":"{micros}"}}"#
        )))
        .unwrap();
        assert_eq!(
            usage.lines.last(),
            Some(&MetricLine::values(
                "Extra usage balance",
                vec![MetricValue::new(0.0, MetricKind::Dollars)]
            ))
        );
    }
}

#[test]
fn a_hidden_daily_quota_fills_the_weekly_row() {
    let usage = mapper::map(&wrapped(r#"{"planInfo":{"planName":"Pro","hideDailyQuota":true},"dailyQuotaRemainingPercent":30,"dailyQuotaResetAtUnix":"1774080000"}"#)).unwrap();
    assert_eq!(
        usage.lines,
        vec![percent("Weekly quota", 70.0, 7 * 86_400_000)]
    );
}

#[test]
fn an_omitted_share_beside_a_reset_is_used_up() {
    for hide in [true, false] {
        let usage = mapper::map(&wrapped(&format!(
            r#"{{"planInfo":{{"hideDailyQuota":{hide}}},"dailyQuotaRemainingPercent":100,"dailyQuotaResetAtUnix":"1774080000","weeklyQuotaResetAtUnix":"1774166400"}}"#
        )))
        .unwrap();
        let weekly = usage
            .lines
            .iter()
            .find(|line| line.label() == "Weekly quota")
            .unwrap();
        assert_eq!(
            weekly,
            &MetricLine::percent(
                "Weekly quota",
                100.0,
                chrono::DateTime::from_timestamp(1_774_166_400, 0),
                Some(7 * 86_400_000)
            )
        );
    }
    let usage = mapper::map(&wrapped(
        r#"{"weeklyQuotaRemainingPercent":40,"dailyQuotaResetAtUnix":"1774080000"}"#,
    ))
    .unwrap();
    assert_eq!(
        usage.lines[0],
        MetricLine::percent(
            "Daily quota",
            100.0,
            chrono::DateTime::from_timestamp(1_774_080_000, 0),
            Some(86_400_000)
        )
    );
}

#[test]
fn an_unreadable_share_is_an_invalid_response() {
    for key in ["dailyQuotaRemainingPercent", "weeklyQuotaRemainingPercent"] {
        for malformed in [r#""abc""#, "true", "{}"] {
            let body = wrapped(&format!(
                r#"{{"{key}":{malformed},"dailyQuotaResetAtUnix":"1774080000","weeklyQuotaResetAtUnix":"1774166400"}}"#
            ));
            assert_eq!(
                mapper::map(&body).unwrap_err(),
                ProviderError::InvalidResponse,
                "{key} {malformed}"
            );
        }
    }
}

#[test]
fn an_account_with_no_quota_says_so_and_a_body_without_user_status_is_invalid() {
    assert_eq!(
        mapper::map(&wrapped(r#"{"planInfo":{"planName":"Max"}}"#))
            .unwrap_err()
            .to_string(),
        "Devin reports no quota for this account."
    );
    assert_eq!(
        mapper::map(br#"{"code":"internal"}"#).unwrap_err(),
        ProviderError::InvalidResponse
    );
}

const STATUS: &str = r#"{"userStatus":{"email":"rob@example.com","teamId":"devin-team$account-1","planStatus":{"planInfo":{"planName":"Max"},"dailyQuotaRemainingPercent":80,"weeklyQuotaRemainingPercent":25}}}"#;

struct Script {
    requests: Mutex<Vec<HttpRequest>>,
    register: u16,
    server: Option<&'static str>,
    status: u16,
    status_body: &'static str,
}

impl HttpClient for Script {
    fn send(&self, request: HttpRequest) -> Result<HttpResponse, ProviderError> {
        self.requests.lock().unwrap().push(request.clone());
        if request.url.contains("register_user") {
            let body = if self.register == 200 {
                format!(
                    r#"{{"api_key":"devin-session-token$abc","api_server_url":"{}"}}"#,
                    self.server
                        .unwrap_or("https://server.self-serve.windsurf.com")
                )
            } else {
                r#"{"code":"unauthenticated"}"#.into()
            };
            return Ok(HttpResponse {
                status: self.register,
                headers: Default::default(),
                body: body.into_bytes(),
            });
        }
        Ok(HttpResponse {
            status: self.status,
            headers: Default::default(),
            body: self.status_body.as_bytes().to_vec(),
        })
    }
}

struct Prompt {
    line: Mutex<Vec<Option<String>>>,
    shown: Mutex<Vec<LoginPrompt>>,
}

impl LoginPrompter for Prompt {
    fn show(&self, prompt: &LoginPrompt) {
        self.shown.lock().unwrap().push(prompt.clone());
    }
    fn read_line(&self, secret: bool) -> Option<String> {
        assert!(secret);
        self.line.lock().unwrap().pop().flatten()
    }
    fn pasted_codes(&self) -> std::sync::mpsc::Receiver<String> {
        std::sync::mpsc::channel().1
    }
}

fn sign_in(pasted: Option<&str>, http: &Script) -> Result<(String, Credential), LoginError> {
    let prompt = Prompt {
        line: Mutex::new(vec![pasted.map(str::to_string)]),
        shown: Mutex::new(vec![]),
    };
    let result = login(&LoginMethod::Default).login(&prompt, http);
    if pasted.is_none() {
        assert!(http.requests.lock().unwrap().is_empty());
    }
    let LoginPrompt::BrowserToken { url } = &prompt.shown.lock().unwrap()[0] else {
        panic!("token page")
    };
    let url = url::Url::parse(url).unwrap();
    assert_eq!(
        url.as_str().split('?').next(),
        Some("https://windsurf.com/profile")
    );
    let query: std::collections::BTreeMap<_, _> = url.query_pairs().into_owned().collect();
    assert_eq!(
        query.get("response_type").map(String::as_str),
        Some("token")
    );
    assert_eq!(
        query.get("redirect_uri").map(String::as_str),
        Some("show-auth-token")
    );
    assert_eq!(
        query.get("scope").map(String::as_str),
        Some("openid profile email")
    );
    assert_eq!(
        query.get("redirect_parameters_type").map(String::as_str),
        Some("query")
    );
    assert!(query.get("state").is_some());
    result
}

#[test]
fn two_sign_ins_use_different_states_and_send_nothing_before_a_paste() {
    let http = Script {
        requests: Mutex::new(vec![]),
        register: 500,
        server: None,
        status: 500,
        status_body: STATUS,
    };
    let first = Prompt {
        line: Mutex::new(vec![None]),
        shown: Mutex::new(vec![]),
    };
    let _ = login(&LoginMethod::Default).login(&first, &http);
    let second = Prompt {
        line: Mutex::new(vec![None]),
        shown: Mutex::new(vec![]),
    };
    let _ = login(&LoginMethod::Default).login(&second, &http);
    let state = |prompt: &Prompt| {
        let LoginPrompt::BrowserToken { url } = &prompt.shown.lock().unwrap()[0] else {
            panic!("page")
        };
        url::Url::parse(url)
            .unwrap()
            .query_pairs()
            .find(|(key, _)| key == "state")
            .unwrap()
            .1
            .into_owned()
    };
    assert_ne!(state(&first), state(&second));
    assert!(http.requests.lock().unwrap().is_empty());
}

#[test]
fn a_pasted_token_is_traded_for_the_api_key_on_the_named_server() {
    let http = Script {
        requests: Mutex::new(vec![]),
        register: 200,
        server: Some("https://server.self-serve.windsurf.com"),
        status: 200,
        status_body: STATUS,
    };
    let (label, credential) = sign_in(Some(" one-time-token\n"), &http).unwrap();
    assert_eq!(label, "rob@example.com");
    let Credential::OAuth(tokens) = credential else {
        panic!("oauth")
    };
    assert_eq!(tokens.access_token, "devin-session-token$abc");
    assert_eq!(
        tokens
            .extras
            .get(OAuthTokens::IDENTITY_KEY)
            .map(String::as_str),
        Some("devin:rob@example.com/devin-team$account-1")
    );
    assert_eq!(
        tokens
            .extras
            .get(client::API_SERVER_KEY)
            .map(String::as_str),
        Some("https://server.self-serve.windsurf.com")
    );
    let urls: Vec<_> = http
        .requests
        .lock()
        .unwrap()
        .iter()
        .map(|request| request.url.clone())
        .collect();
    assert_eq!(
        urls,
        vec![
            "https://api.codeium.com/register_user/".to_string(),
            "https://server.self-serve.windsurf.com/exa.seat_management_pb.SeatManagementService/GetUserStatus".to_string()
        ]
    );
    let register: serde_json::Value =
        serde_json::from_slice(http.requests.lock().unwrap()[0].body.as_deref().unwrap()).unwrap();
    assert_eq!(register["firebase_id_token"], "one-time-token");
    let status: serde_json::Value =
        serde_json::from_slice(http.requests.lock().unwrap()[1].body.as_deref().unwrap()).unwrap();
    assert_eq!(status["metadata"]["apiKey"], "devin-session-token$abc");
}

#[test]
fn an_api_key_pasted_instead_of_a_token_signs_in_on_the_default_server() {
    let http = Script {
        requests: Mutex::new(vec![]),
        register: 401,
        server: None,
        status: 200,
        status_body: r#"{"userStatus":{"email":"rob@example.com"}}"#,
    };
    let (_, credential) = sign_in(Some("devin-session-token$pasted"), &http).unwrap();
    let Credential::OAuth(tokens) = credential else {
        panic!("oauth")
    };
    assert_eq!(tokens.access_token, "devin-session-token$pasted");
    assert_eq!(
        tokens
            .extras
            .get(OAuthTokens::IDENTITY_KEY)
            .map(String::as_str),
        Some("devin:rob@example.com")
    );
    assert!(http
        .requests
        .lock()
        .unwrap()
        .last()
        .unwrap()
        .url
        .starts_with("https://server.codeium.com/"));
}

#[test]
fn a_token_windsurf_refuses_asks_for_a_new_one() {
    let http = Script {
        requests: Mutex::new(vec![]),
        register: 401,
        server: None,
        status: 401,
        status_body: STATUS,
    };
    let error = sign_in(Some("expired-token"), &http).unwrap_err();
    assert_eq!(error.to_string(), "Windsurf rejected that token. A token works for 5 minutes, so click Try Again to get a new one.");
}

#[test]
fn an_api_server_outside_https_never_receives_the_key() {
    let http = Script {
        requests: Mutex::new(vec![]),
        register: 200,
        server: Some("http://server.example"),
        status: 200,
        status_body: STATUS,
    };
    let error = sign_in(Some("one-time-token"), &http).unwrap_err();
    assert_eq!(error, LoginError::InvalidResponse);
    assert_eq!(http.requests.lock().unwrap().len(), 1);
}

#[test]
fn a_refresh_reads_usage_from_the_accounts_own_server() {
    let account = Account::new(ProviderKind::Devin, "rob@example.com", None);
    let mut tokens = OAuthTokens::new("devin-session-token$abc");
    tokens.extras.insert(
        client::API_SERVER_KEY.into(),
        "https://server.self-serve.windsurf.com".into(),
    );
    let vault = MemoryVault::with(account.id, Credential::OAuth(tokens));
    let http = Script {
        requests: Mutex::new(vec![]),
        register: 500,
        server: None,
        status: 200,
        status_body: STATUS,
    };
    let mut state = ProviderState::default();
    let usage = {
        let mut ctx = FetchContext {
            account: &account,
            http: &http,
            vault: &vault,
            state: &mut state,
            now: chrono::Utc::now(),
            force: false,
        };
        provider().fetch(&mut ctx).unwrap()
    };
    assert_eq!(usage.plan.as_deref(), Some("Max"));
    assert_eq!(
        usage.lines,
        vec![
            percent("Daily quota", 20.0, 86_400_000),
            percent("Weekly quota", 75.0, 7 * 86_400_000)
        ]
    );
    assert_eq!(http.requests.lock().unwrap()[0].url, "https://server.self-serve.windsurf.com/exa.seat_management_pb.SeatManagementService/GetUserStatus");
}
