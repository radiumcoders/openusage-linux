//! ChatGPT browser sign-in on port 1455, with the device-code fallback when that port is taken.
//! Port of `CodexLogin`.

use chrono::{DateTime, Utc};
use serde_json::{json, Value};

use crate::account::{Credential, OAuthTokens};
use crate::error::{LoginError, ProviderError};
use crate::http::{HttpClient, HttpRequest};
use crate::login::{
    authorize_in_browser, poll_device_code, random_state, LoginPrompt, LoginPrompter,
    LoopbackServer, Pkce, Poll,
};
use crate::provider::{Login, LoginMethod};

use super::client::{self, chatgpt_account_id, jwt_payload, CLIENT_ID, SCOPES, TOKEN_URL};

pub struct CodexLogin;

pub fn login(_method: &LoginMethod) -> Box<dyn Login> {
    Box::new(CodexLogin)
}

impl Login for CodexLogin {
    fn login(
        &self,
        prompter: &dyn LoginPrompter,
        http: &dyn HttpClient,
    ) -> Result<(String, Credential), LoginError> {
        let pkce = Pkce::generate();
        let state = random_state();
        let Ok(server) =
            LoopbackServer::start(&state, Some(client::REDIRECT_PORT), client::REDIRECT_PATH)
        else {
            return device_login(http, prompter);
        };
        let redirect_uri = server.redirect_uri.clone();
        let code = authorize_in_browser(
            client::AUTHORIZE_URL,
            CLIENT_ID,
            SCOPES,
            &state,
            &pkce,
            server,
            &[
                ("id_token_add_organizations", "true"),
                ("codex_cli_simplified_flow", "true"),
                ("originator", "pi"),
            ],
            prompter,
        )?;
        exchange(http, &code, &pkce.verifier, &redirect_uri)
    }
}

fn device_login(
    http: &dyn HttpClient,
    prompter: &dyn LoginPrompter,
) -> Result<(String, Credential), LoginError> {
    let start = http
        .send(
            HttpRequest::post(client::DEVICE_START_URL)
                .json_body(&json!({ "client_id": CLIENT_ID })),
        )
        .map_err(LoginError::from)?;
    if !start.is_success() {
        return Err(LoginError::RequestFailed(start.status));
    }
    let body = start.json().ok_or(LoginError::InvalidResponse)?;
    let device_auth_id = text(&body, "device_auth_id").ok_or(LoginError::InvalidResponse)?;
    let user_code = text(&body, "user_code").ok_or(LoginError::InvalidResponse)?;
    let interval = body
        .get("interval")
        .and_then(|value| {
            value
                .as_f64()
                .or_else(|| value.as_str().and_then(|text| text.trim().parse().ok()))
        })
        .filter(|seconds| seconds.is_finite() && *seconds >= 0.0)
        .map(|seconds| seconds.round() as u64)
        .unwrap_or(5);
    prompter.show(&LoginPrompt::DeviceCode {
        user_code: user_code.clone(),
        verification_url: client::DEVICE_VERIFY_URL.into(),
    });
    let token = poll_device_code(interval, 900, || poll(http, &device_auth_id, &user_code))?;
    exchange(
        http,
        &token.authorization_code,
        &token.code_verifier,
        client::DEVICE_REDIRECT,
    )
}

struct DeviceToken {
    authorization_code: String,
    code_verifier: String,
}

fn poll(
    http: &dyn HttpClient,
    device_auth_id: &str,
    user_code: &str,
) -> Result<Poll<DeviceToken>, LoginError> {
    let response = match http.send(
        HttpRequest::post(client::DEVICE_TOKEN_URL).json_body(&json!({
            "device_auth_id": device_auth_id,
            "user_code": user_code,
        })),
    ) {
        Ok(response) => response,
        Err(ProviderError::ConnectionFailed) => return Ok(Poll::Pending),
        Err(error) => return Err(error.into()),
    };
    if response.is_success() {
        let body = response.json().ok_or(LoginError::InvalidResponse)?;
        let authorization_code =
            text(&body, "authorization_code").ok_or(LoginError::InvalidResponse)?;
        let code_verifier = text(&body, "code_verifier").ok_or(LoginError::InvalidResponse)?;
        return Ok(Poll::Done(DeviceToken {
            authorization_code,
            code_verifier,
        }));
    }
    let error = response
        .json()
        .and_then(|body| {
            body.get("error")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .unwrap_or_default();
    if response.status == 403
        || response.status == 404
        || error == "deviceauth_authorization_pending"
    {
        return Ok(Poll::Pending);
    }
    if error == "slow_down" {
        return Ok(Poll::SlowDown);
    }
    Err(LoginError::Failed(if error == "expired_token" {
        "Device sign-in expired.".into()
    } else {
        "Device sign-in failed.".into()
    }))
}

fn exchange(
    http: &dyn HttpClient,
    code: &str,
    verifier: &str,
    redirect_uri: &str,
) -> Result<(String, Credential), LoginError> {
    let response = http
        .send(client::sorted_form(
            TOKEN_URL,
            &[
                ("grant_type", "authorization_code"),
                ("client_id", CLIENT_ID),
                ("code", code),
                ("code_verifier", verifier),
                ("redirect_uri", redirect_uri),
            ],
        ))
        .map_err(LoginError::from)?;
    if !response.is_success() {
        return Err(LoginError::RequestFailed(response.status));
    }
    let body = response.json().ok_or(LoginError::InvalidResponse)?;
    let access_token = text(&body, "access_token")
        .filter(|token| !token.is_empty())
        .ok_or(LoginError::InvalidResponse)?;
    let mut tokens = OAuthTokens::new(access_token.clone());
    tokens.refresh_token = text(&body, "refresh_token");
    tokens.expires_at = expires_in(&body);
    let access = jwt_payload(&access_token);
    let account_id = access.as_ref().and_then(chatgpt_account_id);
    let email = text(&body, "id_token")
        .as_deref()
        .and_then(jwt_payload)
        .and_then(|claims| text(&claims, "email"));
    if let Some(account_id) = account_id.clone() {
        tokens
            .extras
            .insert("chatgpt_account_id".into(), account_id);
    }
    if let (Some(account_id), Some(email)) = (account_id.as_ref(), email.as_ref()) {
        tokens.extras.insert(
            OAuthTokens::IDENTITY_KEY.into(),
            format!("{account_id}/{email}"),
        );
    }
    Ok((
        email.unwrap_or_else(|| "ChatGPT Account".into()),
        Credential::OAuth(tokens),
    ))
}

fn expires_in(body: &Value) -> Option<DateTime<Utc>> {
    let seconds = body
        .get("expires_in")
        .and_then(Value::as_f64)
        .filter(|seconds| seconds.is_finite())?;
    Utc::now().checked_add_signed(chrono::Duration::milliseconds(
        (seconds * 1000.0).round() as i64
    ))
}

fn text(body: &Value, key: &str) -> Option<String> {
    body.get(key).and_then(Value::as_str).map(str::to_string)
}
