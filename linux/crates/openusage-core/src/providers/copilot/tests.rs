use std::collections::BTreeMap;
use std::sync::Mutex;

use super::client;
use super::mapper;
use super::*;
use crate::account::{Account, Credential, OAuthTokens};
use crate::error::{LoginError, ProviderError};
use crate::http::{HttpClient, HttpRequest, HttpResponse};
use crate::kind::ProviderKind;
use crate::login::{LoginPrompt, LoginPrompter};
use crate::metric::{MetricKind, MetricLine, MetricValue, ProgressFormat};
use crate::provider::{FetchContext, LoginMethod, ProviderState};
use crate::vault::MemoryVault;

const ORG: &str = r#"{"usageItems":[{"product":"Copilot","grossQuantity":100,"netAmount":1.0},{"product":"Actions","grossQuantity":500,"netAmount":4.0}]}"#;

fn ok(body: &str) -> HttpResponse {
    HttpResponse {
        status: 200,
        headers: BTreeMap::new(),
        body: body.as_bytes().to_vec(),
    }
}

fn lines_of(body: &str) -> Option<Vec<MetricLine>> {
    mapper::usage_lines(body.as_bytes())
}

#[test]
fn org_usage_counts_copilot_credits_and_spend_only() {
    let credits = MetricLine::values(
        "Org Credits",
        vec![MetricValue::labeled(100.0, MetricKind::Count, "credits")],
    );
    let spend = MetricLine::values(
        "Org Spend",
        vec![MetricValue::new(1.0, MetricKind::Dollars)],
    );
    assert_eq!(lines_of(ORG).unwrap(), vec![credits, spend]);
    let zeros =
        lines_of(r#"{"usageItems":[{"product":"Actions","grossQuantity":500,"netAmount":4.0}]}"#)
            .unwrap();
    assert_eq!(
        zeros[0],
        MetricLine::values(
            "Org Credits",
            vec![MetricValue::labeled(0.0, MetricKind::Count, "credits")]
        )
    );
    assert_eq!(
        zeros[1],
        MetricLine::values(
            "Org Spend",
            vec![MetricValue::new(0.0, MetricKind::Dollars)]
        )
    );
}

#[test]
fn an_invalid_org_report_maps_to_nothing() {
    for body in [
        "not json",
        "{}",
        r#"{"usageItems":{"product":"Copilot"}}"#,
        r#"{"usageItems":[{"product":"Copilot","grossQuantity":100}]}"#,
    ] {
        assert!(lines_of(body).is_none(), "{body}");
    }
}

struct Script {
    requests: Mutex<Vec<HttpRequest>>,
}

impl HttpClient for Script {
    fn send(&self, request: HttpRequest) -> Result<HttpResponse, ProviderError> {
        self.requests.lock().unwrap().push(request.clone());
        let token = request
            .headers
            .get("Authorization")
            .map(String::as_str)
            .unwrap_or("");
        Ok(match token {
            "Bearer good" => ok(ORG),
            "Bearer garbage" => ok("<html>"),
            "Bearer elsewhere" => HttpResponse {
                status: 404,
                headers: BTreeMap::new(),
                body: b"{}".to_vec(),
            },
            "Bearer limited" => {
                let mut headers = BTreeMap::new();
                headers.insert("retry-after".into(), "60".into());
                HttpResponse {
                    status: 403,
                    headers,
                    body: b"{}".to_vec(),
                }
            }
            _ => HttpResponse {
                status: 403,
                headers: BTreeMap::new(),
                body: b"{}".to_vec(),
            },
        })
    }
}

struct Prompt {
    line: Mutex<Vec<String>>,
    shown: Mutex<Vec<LoginPrompt>>,
}

impl LoginPrompter for Prompt {
    fn show(&self, prompt: &LoginPrompt) {
        self.shown.lock().unwrap().push(prompt.clone());
    }
    fn read_line(&self, secret: bool) -> Option<String> {
        assert!(secret);
        self.line.lock().unwrap().pop()
    }
    fn pasted_codes(&self) -> std::sync::mpsc::Receiver<String> {
        std::sync::mpsc::channel().1
    }
}

#[test]
fn adding_an_organization_checks_the_token_with_one_read() {
    let http = Script {
        requests: Mutex::new(vec![]),
    };
    let prompt = Prompt {
        line: Mutex::new(vec!["good".into()]),
        shown: Mutex::new(vec![]),
    };
    let (label, credential) = login(&LoginMethod::Organization("acme".into()))
        .login(&prompt, &http)
        .unwrap();
    assert_eq!(label, "acme");
    assert_eq!(credential, Credential::ApiKey("good".into()));
    assert_eq!(http.requests.lock().unwrap().len(), 1);
    let request = http.requests.lock().unwrap()[0].clone();
    assert_eq!(
        request.url,
        "https://api.github.com/organizations/acme/settings/billing/ai_credit/usage"
    );
    assert_eq!(
        request
            .headers
            .get("X-GitHub-Api-Version")
            .map(String::as_str),
        Some("2022-11-28")
    );
    assert_eq!(
        request.headers.get("Accept").map(String::as_str),
        Some("application/vnd.github+json")
    );
    for (token, message) in [
        ("refused", "GitHub rejected the token."),
        (
            "elsewhere",
            "GitHub has no organization \u{201C}acme\u{201D} this token can read.",
        ),
        ("garbage", "The provider returned an invalid response."),
    ] {
        prompt.line.lock().unwrap().push(token.into());
        let error = login(&LoginMethod::Organization("acme".into()))
            .login(&prompt, &http)
            .unwrap_err();
        assert_eq!(error.to_string(), message, "{token}");
    }
}

#[test]
fn githubs_rate_limit_on_an_organization_read_is_a_rate_limit() {
    let http = Script {
        requests: Mutex::new(vec![]),
    };
    let error = client::organization_usage(&http, "acme", "limited").unwrap_err();
    assert_eq!(
        error,
        ProviderError::RateLimited {
            retry_after_seconds: Some(60.0)
        }
    );
}

#[test]
fn an_organization_account_shows_its_credits_and_spend() {
    let account = Account::new(ProviderKind::Copilot, "acme", Some("acme".into()));
    let vault = MemoryVault::with(account.id, Credential::ApiKey("good".into()));
    let http = Script {
        requests: Mutex::new(vec![]),
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
    assert!(usage.plan.is_none());
    assert_eq!(usage.lines, lines_of(ORG).unwrap());
}

#[test]
fn personal_usage_uses_the_reported_remaining_credits() {
    let body = br#"{"copilot_plan":"pro","quota_snapshots":{"premium_interactions":{"entitlement":100,"remaining":75}}}"#;
    let mapped = mapper::map_response(body).unwrap();
    assert_eq!(mapped.plan.as_deref(), Some("Pro"));
    match mapped.lines.iter().find(|line| line.label() == "Credits") {
        Some(MetricLine::Progress {
            used,
            limit,
            format,
            ..
        }) => {
            assert_eq!(*used, 25.0);
            assert_eq!(*limit, 100.0);
            assert_eq!(*format, ProgressFormat::Percent);
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn device_login_maps_the_github_profile() {
    let http = Script {
        requests: Mutex::new(vec![]),
    };
    // The org script keys off Authorization. Device login has no bearer on the first calls.
    // Use a dedicated client.
    let _ = http;
    struct Device;
    impl HttpClient for Device {
        fn send(&self, request: HttpRequest) -> Result<HttpResponse, ProviderError> {
            let path = url::Url::parse(&request.url).unwrap().path().to_string();
            let body = match path.as_str() {
                "/login/device/code" => {
                    let form: BTreeMap<_, _> =
                        url::form_urlencoded::parse(request.body.as_deref().unwrap())
                            .into_owned()
                            .collect();
                    assert_eq!(form.get("scope").map(String::as_str), Some("read:user"));
                    r#"{"device_code":"device","user_code":"ABCD-EFGH","verification_uri":"https://github.com/login/device","expires_in":600,"interval":1}"#
                }
                "/login/oauth/access_token" => {
                    let form: BTreeMap<_, _> =
                        url::form_urlencoded::parse(request.body.as_deref().unwrap())
                            .into_owned()
                            .collect();
                    assert_eq!(
                        form.get("grant_type").map(String::as_str),
                        Some("urn:ietf:params:oauth:grant-type:device_code")
                    );
                    assert_eq!(form.get("device_code").map(String::as_str), Some("device"));
                    r#"{"access_token":"copilot-access"}"#
                }
                "/user" => r#"{"login":"copilot-user"}"#,
                _ => {
                    return Ok(HttpResponse {
                        status: 404,
                        headers: BTreeMap::new(),
                        body: b"{}".to_vec(),
                    })
                }
            };
            Ok(ok(body))
        }
    }
    let prompt = Prompt {
        line: Mutex::new(vec![]),
        shown: Mutex::new(vec![]),
    };
    let (label, credential) = login(&LoginMethod::Default)
        .login(&prompt, &Device)
        .unwrap();
    assert_eq!(label, "copilot-user");
    let LoginPrompt::DeviceCode {
        user_code,
        verification_url,
    } = &prompt.shown.lock().unwrap()[0]
    else {
        panic!("device")
    };
    assert_eq!(user_code, "ABCD-EFGH");
    assert_eq!(verification_url, "https://github.com/login/device");
    let Credential::OAuth(tokens) = credential else {
        panic!("oauth")
    };
    assert_eq!(tokens.access_token, "copilot-access");
    assert_eq!(
        tokens
            .extras
            .get(OAuthTokens::IDENTITY_KEY)
            .map(String::as_str),
        Some("github:copilot-user")
    );
    let _ = LoginError::Cancelled;
}
