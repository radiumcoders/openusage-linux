//! Port of `CursorLogin` in `DeviceProviderLogins.swift`.
//! The browser opens Cursor's deep-control page; the sign-in finishes when the poll returns tokens.

use std::thread;
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::account::{Credential, OAuthTokens};
use crate::error::{LoginError, ProviderError};
use crate::http::{HttpClient, HttpRequest};
use crate::log;
use crate::login::{LoginPrompt, LoginPrompter, Pkce};
use crate::provider::Login;

use super::client::{self, session};
use super::parse::jwt_expiry;

const AUTHORIZE_URL: &str = "https://cursor.com/loginDeepControl";

pub(crate) struct CursorLogin;

impl Login for CursorLogin {
    fn login(
        &self,
        prompter: &dyn LoginPrompter,
        http: &dyn HttpClient,
    ) -> Result<(String, Credential), LoginError> {
        let pkce = Pkce::generate();
        let uuid = uuid::Uuid::new_v4().to_string().to_uppercase();
        let mut url = url::Url::parse(AUTHORIZE_URL).map_err(|_| LoginError::InvalidResponse)?;
        url.query_pairs_mut()
            .append_pair("challenge", &pkce.challenge)
            .append_pair("uuid", &uuid)
            .append_pair("mode", "login")
            .append_pair("redirectTarget", "cli");
        prompter.show(&LoginPrompt::Browser {
            url: url.to_string(),
            accepts_pasted_code: false,
        });
        let token = poll_for(2.0, 300.0, || poll_once(http, &uuid, &pkce.verifier))?;
        let user_id = session(&token.access_token).map(|session| session.user_id);
        let mut extras = std::collections::BTreeMap::new();
        if let Some(user_id) = &user_id {
            extras.insert(
                OAuthTokens::IDENTITY_KEY.to_string(),
                format!("cursor:{user_id}"),
            );
        }
        let expires_at = jwt_expiry(&token.access_token);
        let label =
            profile_name(http, &token.access_token).unwrap_or_else(|| "Cursor Account".into());
        Ok((
            label,
            Credential::OAuth(OAuthTokens {
                access_token: token.access_token,
                refresh_token: Some(token.refresh_token),
                expires_at,
                extras,
            }),
        ))
    }
}

struct PolledToken {
    access_token: String,
    refresh_token: String,
}

fn profile_name(http: &dyn HttpClient, access_token: &str) -> Option<String> {
    let response = match client::fetch_profile(http, access_token) {
        Ok(Some(response)) if response.is_success() => response,
        Ok(_) => {
            log::warn(
                "plugin:cursor",
                "profile read failed, naming the account Cursor Account",
            );
            return None;
        }
        Err(error) => {
            log::warn(
                "plugin:cursor",
                &format!("profile read failed, naming the account Cursor Account: {error}"),
            );
            return None;
        }
    };
    let body = response.json()?;
    let email = body
        .get("email")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty());
    let name = body
        .get("name")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty());
    email.or(name).map(str::to_string)
}

fn poll_once(
    http: &dyn HttpClient,
    uuid: &str,
    verifier: &str,
) -> Result<Poll<PolledToken>, LoginError> {
    let mut url = url::Url::parse(client::POLL_URL)
        .map_err(|_| LoginError::Failed("Cursor sign-in URL was invalid.".into()))?;
    url.query_pairs_mut()
        .append_pair("uuid", uuid)
        .append_pair("verifier", verifier);
    let response = match http.send(HttpRequest::get(url.as_str())) {
        Ok(response) => response,
        Err(ProviderError::ConnectionFailed) => {
            return Err(LoginError::Provider(ProviderError::ConnectionFailed))
        }
        Err(error) => return Err(LoginError::Provider(error)),
    };
    if response.is_success() {
        if let Some(token) = decode_token(&response.body) {
            return Ok(Poll::Done(token));
        }
    }
    if response.status == 404 || response.status == 202 {
        return Ok(Poll::Pending);
    }
    Ok(Poll::Failed)
}

fn decode_token(body: &[u8]) -> Option<PolledToken> {
    let json: Value = serde_json::from_slice(body).ok()?;
    let access_token = json.get("accessToken")?.as_str()?.to_string();
    let refresh_token = json.get("refreshToken")?.as_str()?.to_string();
    if access_token.is_empty() || refresh_token.is_empty() {
        return None;
    }
    Some(PolledToken {
        access_token,
        refresh_token,
    })
}

enum Poll<T> {
    Done(T),
    Pending,
    Failed,
}

/// `DeviceCodePoller`: the first request is immediate, then it waits `interval` seconds between tries.
fn poll_for<T>(
    interval: f64,
    expires_in: f64,
    mut poll: impl FnMut() -> Result<Poll<T>, LoginError>,
) -> Result<T, LoginError> {
    let deadline = Instant::now() + Duration::from_secs_f64(expires_in.max(0.0));
    let delay = interval.max(1.0);
    let mut saw_network = false;
    while Instant::now() < deadline {
        let result = match poll() {
            Ok(result) => {
                saw_network = false;
                result
            }
            Err(LoginError::Provider(ProviderError::ConnectionFailed)) => {
                log::warn("plugin:cursor", "device sign-in poll failed; polling again");
                saw_network = true;
                Poll::Pending
            }
            Err(error) => return Err(error),
        };
        match result {
            Poll::Done(value) => return Ok(value),
            Poll::Pending => {}
            Poll::Failed => return Err(LoginError::Failed("Cursor sign-in failed.".into())),
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
