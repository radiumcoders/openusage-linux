//! Claude browser sign-in. Port of `ClaudeLogin`.

use chrono::{DateTime, Utc};
use serde_json::{json, Value};

use crate::account::{Credential, OAuthTokens};
use crate::error::LoginError;
use crate::http::{HttpClient, HttpRequest};
use crate::log;
use crate::login::{authorize_in_browser, random_state, LoginPrompter, LoopbackServer, Pkce};
use crate::provider::{Login, LoginMethod};

use super::client::{self, CLIENT_ID, SCOPES, TOKEN_URL};

pub struct ClaudeLogin;

pub fn login(_method: &LoginMethod) -> Box<dyn Login> {
    Box::new(ClaudeLogin)
}

impl Login for ClaudeLogin {
    fn login(
        &self,
        prompter: &dyn LoginPrompter,
        http: &dyn HttpClient,
    ) -> Result<(String, Credential), LoginError> {
        let pkce = Pkce::generate();
        let state = random_state();
        let server = LoopbackServer::start(&state, None, "/callback")?;
        let redirect_uri = server.redirect_uri.clone();
        let code = authorize_in_browser(
            client::AUTHORIZE_URL,
            CLIENT_ID,
            SCOPES,
            &state,
            &pkce,
            server,
            &[("code", "true")],
            prompter,
        )?;
        let token = exchange(http, &code, &state, &redirect_uri, &pkce.verifier)?;
        let profile =
            match http.send(HttpRequest::get(client::PROFILE_URL).bearer(&token.access_token)) {
                Ok(response) if response.is_success() => response.json().and_then(login_profile),
                _ => None,
            };
        let Some(profile) = profile else {
            log::error(
                "auth:claude",
                "profile read failed after the token exchange",
            );
            return Err(LoginError::Failed(
                "Claude didn\u{2019}t return your profile. Click Try Again to sign in again."
                    .into(),
            ));
        };
        let mut tokens = OAuthTokens::new(token.access_token);
        tokens.refresh_token = token.refresh_token;
        tokens.expires_at = token.expires_at;
        tokens
            .extras
            .insert(OAuthTokens::IDENTITY_KEY.to_string(), profile.identity);
        Ok((profile.label, Credential::OAuth(tokens)))
    }
}

struct Exchanged {
    access_token: String,
    refresh_token: Option<String>,
    expires_at: Option<DateTime<Utc>>,
}

struct LoginProfile {
    label: String,
    identity: String,
}

fn exchange(
    http: &dyn HttpClient,
    code: &str,
    state: &str,
    redirect_uri: &str,
    verifier: &str,
) -> Result<Exchanged, LoginError> {
    let response = http
        .send(HttpRequest::post(TOKEN_URL).json_body(&json!({
            "grant_type": "authorization_code",
            "client_id": CLIENT_ID,
            "code": code,
            "state": state,
            "redirect_uri": redirect_uri,
            "code_verifier": verifier,
        })))
        .map_err(LoginError::from)?;
    if !response.is_success() {
        return Err(LoginError::RequestFailed(response.status));
    }
    let body = response.json().ok_or(LoginError::InvalidResponse)?;
    let access_token = body
        .get("access_token")
        .and_then(Value::as_str)
        .filter(|token| !token.is_empty())
        .ok_or(LoginError::InvalidResponse)?
        .to_string();
    let expires_at = body
        .get("expires_in")
        .and_then(Value::as_f64)
        .filter(|seconds| seconds.is_finite())
        .and_then(|seconds| {
            Utc::now().checked_add_signed(chrono::Duration::milliseconds(
                (seconds * 1000.0).round() as i64,
            ))
        });
    Ok(Exchanged {
        access_token,
        refresh_token: body
            .get("refresh_token")
            .and_then(Value::as_str)
            .map(str::to_string),
        expires_at,
    })
}

fn login_profile(body: Value) -> Option<LoginProfile> {
    let account = body.get("account")?;
    let uuid = account
        .get("uuid")
        .and_then(Value::as_str)
        .filter(|uuid| !uuid.is_empty())?;
    let organization = body
        .get("organization")
        .and_then(|org| org.get("uuid"))
        .and_then(Value::as_str);
    let identity = match organization {
        Some(organization) => format!("{uuid}/{organization}"),
        None => uuid.to_string(),
    };
    let label = account
        .get("email")
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| {
            body.get("email")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .unwrap_or_else(|| "Claude Account".into());
    Some(LoginProfile { label, identity })
}
