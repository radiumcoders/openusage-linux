//! Port of `AntigravityLogin` in `BrowserProviderLogins.swift`.

use chrono::{Duration, Utc};
use serde_json::Value;

use crate::account::{Credential, OAuthTokens};
use crate::error::LoginError;
use crate::http::{HttpClient, HttpRequest};
use crate::login::{authorize_in_browser, random_state, LoopbackServer, Pkce};
use crate::provider::Login;

use super::client;

const AUTHORIZE_URL: &str = "https://accounts.google.com/o/oauth2/v2/auth";
const USERINFO_URL: &str = "https://www.googleapis.com/oauth2/v1/userinfo?alt=json";
const SCOPES: &str =
    "https://www.googleapis.com/auth/cloud-platform https://www.googleapis.com/auth/userinfo.email";

pub(crate) struct AntigravityLogin;

impl Login for AntigravityLogin {
    fn login(
        &self,
        prompter: &dyn crate::login::LoginPrompter,
        http: &dyn HttpClient,
    ) -> Result<(String, Credential), LoginError> {
        let pkce = Pkce::generate();
        let state = random_state();
        let server = LoopbackServer::start(&state, Some(51121), "/oauth-callback")?;
        let redirect = server.redirect_uri.clone();
        let code = authorize_in_browser(
            AUTHORIZE_URL,
            client::CLIENT_ID,
            SCOPES,
            &state,
            &pkce,
            server,
            &[("access_type", "offline"), ("prompt", "consent")],
            prompter,
        )?;
        let response = http
            .send(
                HttpRequest::post(client::TOKEN_URL)
                    .header("Accept", "application/json")
                    .form_body(&[
                        ("client_id", client::CLIENT_ID),
                        ("client_secret", client::CLIENT_SECRET),
                        ("code", &code),
                        ("grant_type", "authorization_code"),
                        ("redirect_uri", &redirect),
                        ("code_verifier", &pkce.verifier),
                    ]),
            )
            .map_err(LoginError::from)?;
        if !response.is_success() {
            return Err(LoginError::RequestFailed(response.status));
        }
        let body = response.json().ok_or(LoginError::InvalidResponse)?;
        let access = body
            .get("access_token")
            .and_then(Value::as_str)
            .filter(|token| !token.is_empty())
            .ok_or(LoginError::InvalidResponse)?
            .to_string();
        let refresh = body
            .get("refresh_token")
            .and_then(Value::as_str)
            .filter(|token| !token.is_empty())
            .map(str::to_string);
        let expires_at = body
            .get("expires_in")
            .and_then(Value::as_f64)
            .filter(|seconds| seconds.is_finite())
            .and_then(|seconds| {
                Utc::now().checked_add_signed(Duration::milliseconds((seconds * 1000.0) as i64))
            });
        let user = http
            .send(HttpRequest::get(USERINFO_URL).bearer(&access))
            .ok()
            .filter(|response| response.is_success())
            .and_then(|response| response.json());
        let email = user
            .as_ref()
            .and_then(|user| user.get("email"))
            .and_then(Value::as_str)
            .filter(|email| !email.is_empty())
            .map(str::to_string);
        let id = user
            .as_ref()
            .and_then(|user| user.get("id"))
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
            .map(str::to_string);
        let mut extras = std::collections::BTreeMap::new();
        if let Some(identity) = id.or(email.clone()) {
            extras.insert(OAuthTokens::IDENTITY_KEY.to_string(), identity);
        }
        let label = email.unwrap_or_else(|| "Google Account".into());
        Ok((
            label,
            Credential::OAuth(OAuthTokens {
                access_token: access,
                refresh_token: refresh,
                expires_at,
                extras,
            }),
        ))
    }
}
