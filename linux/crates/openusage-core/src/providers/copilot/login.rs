//! Port of `CopilotLogin` and `CopilotOrganizationLogin`.

use std::thread;
use std::time::{Duration, Instant};

use base64::Engine;
use serde_json::Value;

use crate::account::{Credential, OAuthTokens};
use crate::error::{LoginError, ProviderError};
use crate::http::{HttpClient, HttpRequest, HttpResponse};
use crate::log;
use crate::login::{LoginPrompt, LoginPrompter};
use crate::provider::{Login, LoginMethod};

use super::client;

const DEVICE_URL: &str = "https://github.com/login/device/code";
const TOKEN_URL: &str = "https://github.com/login/oauth/access_token";
const PROFILE_URL: &str = "https://api.github.com/user";
const SCOPES: &str = "read:user";

pub(crate) struct CopilotLogin;

pub(crate) struct OrganizationLogin {
    pub organization: String,
}

impl Login for CopilotLogin {
    fn login(
        &self,
        prompter: &dyn LoginPrompter,
        http: &dyn HttpClient,
    ) -> Result<(String, Credential), LoginError> {
        let client_id = github_client_id();
        let start = post_form(
            http,
            DEVICE_URL,
            &[("client_id", client_id.as_str()), ("scope", SCOPES)],
        )?;
        let device = decode_device(&start)?;
        prompter.show(&LoginPrompt::DeviceCode {
            user_code: device.user_code.clone(),
            verification_url: device.verification_url.clone(),
        });
        let token = poll_for(device.interval, device.expires_in, || {
            poll_token(http, &client_id, &device.device_code)
        })?;
        let profile = get_profile(http, &token)?;
        let user = decode_user(&profile)?;
        let identity = user
            .id
            .map(|id| format!("github:{id}"))
            .unwrap_or_else(|| format!("github:{}", user.login));
        let mut extras = std::collections::BTreeMap::new();
        extras.insert(OAuthTokens::IDENTITY_KEY.to_string(), identity);
        Ok((
            user.login,
            Credential::OAuth(OAuthTokens {
                access_token: token,
                refresh_token: None,
                expires_at: None,
                extras,
            }),
        ))
    }
}

impl Login for OrganizationLogin {
    fn login(
        &self,
        prompter: &dyn LoginPrompter,
        http: &dyn HttpClient,
    ) -> Result<(String, Credential), LoginError> {
        prompter.show(&LoginPrompt::Secret {
            label: format!("{} organization token", self.organization),
            key_page: None,
        });
        let token = prompter.read_line(true).ok_or(LoginError::Cancelled)?;
        let token = token.trim().to_string();
        if token.is_empty() {
            return Err(LoginError::InvalidCode);
        }
        match client::organization_usage(http, &self.organization, &token) {
            Ok(_) => Ok((self.organization.clone(), Credential::ApiKey(token))),
            Err(ProviderError::KeyRejected(message)) => Err(LoginError::Failed(message)),
            Err(error) => Err(error.into()),
        }
    }
}

pub(crate) fn dispatch(method: &LoginMethod) -> Box<dyn Login> {
    match method {
        LoginMethod::Organization(name) => Box::new(OrganizationLogin {
            organization: name.clone(),
        }),
        LoginMethod::Default => Box::new(CopilotLogin),
    }
}

fn github_client_id() -> String {
    let bytes = base64::engine::general_purpose::STANDARD
        .decode("SXYxLmI1MDdhMDhjODdlY2ZlOTg=")
        .expect("copilot client id");
    String::from_utf8(bytes).expect("copilot client id")
}

struct Device {
    device_code: String,
    user_code: String,
    verification_url: String,
    expires_in: f64,
    interval: f64,
}

fn decode_device(response: &HttpResponse) -> Result<Device, LoginError> {
    if !response.is_success() {
        return Err(LoginError::RequestFailed(response.status));
    }
    let json = response.json().ok_or(LoginError::InvalidResponse)?;
    let device_code = json
        .get("device_code")
        .and_then(Value::as_str)
        .ok_or(LoginError::InvalidResponse)?
        .to_string();
    let user_code = json
        .get("user_code")
        .and_then(Value::as_str)
        .ok_or(LoginError::InvalidResponse)?
        .to_string();
    let verification = json
        .get("verification_uri")
        .and_then(Value::as_str)
        .ok_or(LoginError::InvalidResponse)?;
    let url = url::Url::parse(verification).map_err(|_| LoginError::InvalidResponse)?;
    if url.scheme() != "https" {
        return Err(LoginError::InvalidResponse);
    }
    let expires_in = json
        .get("expires_in")
        .and_then(Value::as_f64)
        .ok_or(LoginError::InvalidResponse)?;
    let interval = json.get("interval").and_then(Value::as_f64).unwrap_or(5.0);
    Ok(Device {
        device_code,
        user_code,
        verification_url: url.to_string(),
        expires_in,
        interval,
    })
}

struct User {
    id: Option<i64>,
    login: String,
}

fn decode_user(response: &HttpResponse) -> Result<User, LoginError> {
    if !response.is_success() {
        return Err(LoginError::RequestFailed(response.status));
    }
    let json = response.json().ok_or(LoginError::InvalidResponse)?;
    let login = json
        .get("login")
        .and_then(Value::as_str)
        .filter(|login| !login.is_empty())
        .ok_or(LoginError::InvalidResponse)?
        .to_string();
    let id = json.get("id").and_then(Value::as_i64);
    Ok(User { id, login })
}

fn poll_token(
    http: &dyn HttpClient,
    client_id: &str,
    device_code: &str,
) -> Result<Poll, LoginError> {
    let response = match post_form(
        http,
        TOKEN_URL,
        &[
            ("client_id", client_id),
            ("device_code", device_code),
            ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
        ],
    ) {
        Ok(response) => response,
        Err(LoginError::Provider(ProviderError::ConnectionFailed)) => {
            return Err(LoginError::Provider(ProviderError::ConnectionFailed))
        }
        Err(error) => return Err(error),
    };
    if let Some(token) = response
        .json()
        .and_then(|json| {
            json.get("access_token")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .filter(|token| !token.is_empty())
    {
        return Ok(Poll::Done(token));
    }
    let error = response.json().and_then(|json| {
        json.get("error")
            .and_then(Value::as_str)
            .map(str::to_string)
    });
    Ok(match error.as_deref() {
        Some("authorization_pending") => Poll::Pending,
        Some("slow_down") => Poll::SlowDown,
        Some("expired_token") => Poll::Failed("GitHub device sign-in expired.".into()),
        Some("access_denied") => Poll::Failed("GitHub device sign-in was denied.".into()),
        _ => Poll::Failed("GitHub device sign-in failed.".into()),
    })
}

fn post_form(
    http: &dyn HttpClient,
    url: &str,
    fields: &[(&str, &str)],
) -> Result<HttpResponse, LoginError> {
    let request = HttpRequest::post(url)
        .header("Accept", "application/json")
        .header("User-Agent", "OpenUsage")
        .form_body(fields);
    match http.send(request) {
        Ok(response) => Ok(response),
        Err(ProviderError::ConnectionFailed) => {
            Err(LoginError::Provider(ProviderError::ConnectionFailed))
        }
        Err(error) => Err(LoginError::Provider(error)),
    }
}

fn get_profile(http: &dyn HttpClient, token: &str) -> Result<HttpResponse, LoginError> {
    let request = HttpRequest::get(PROFILE_URL)
        .bearer(token)
        .header("Accept", "application/vnd.github+json")
        .header("User-Agent", "OpenUsage")
        .header("X-GitHub-Api-Version", "2022-11-28");
    http.send(request).map_err(LoginError::from)
}

enum Poll {
    Done(String),
    Pending,
    SlowDown,
    Failed(String),
}

fn poll_for(
    interval: f64,
    expires_in: f64,
    mut poll: impl FnMut() -> Result<Poll, LoginError>,
) -> Result<String, LoginError> {
    let deadline = Instant::now() + Duration::from_secs_f64(expires_in.max(0.0));
    let mut delay = interval.max(1.0);
    let mut saw_network = false;
    while Instant::now() < deadline {
        let result = match poll() {
            Ok(result) => {
                saw_network = false;
                result
            }
            Err(LoginError::Provider(ProviderError::ConnectionFailed)) => {
                log::warn(
                    "plugin:copilot",
                    "device sign-in poll failed; polling again",
                );
                saw_network = true;
                Poll::Pending
            }
            Err(error) => return Err(error),
        };
        match result {
            Poll::Done(value) => return Ok(value),
            Poll::Pending => {}
            Poll::SlowDown => delay += 5.0,
            Poll::Failed(message) => return Err(LoginError::Failed(message)),
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }
        thread::sleep(Duration::from_secs_f64(delay).min(remaining));
    }
    if saw_network {
        Err(LoginError::Provider(ProviderError::ConnectionFailed))
    } else {
        Err(LoginError::Failed("Device sign-in expired.".into()))
    }
}
