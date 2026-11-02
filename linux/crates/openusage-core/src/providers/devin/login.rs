//! Port of `DevinLogin.swift`. The page shows a token; the user pastes it.

use serde_json::Value;

use crate::account::{Credential, OAuthTokens};
use crate::error::{LoginError, ProviderError};
use crate::http::{HttpClient, HttpRequest};
use crate::login::{random_state, LoginPrompt, LoginPrompter};
use crate::provider::Login;

use super::client;

const REGISTER_URL: &str = "https://api.codeium.com/register_user/";

pub(crate) struct DevinLogin;

impl Login for DevinLogin {
    fn login(
        &self,
        prompter: &dyn LoginPrompter,
        http: &dyn HttpClient,
    ) -> Result<(String, Credential), LoginError> {
        let url = token_page(&random_state());
        prompter.show(&LoginPrompt::BrowserToken { url });
        let pasted = prompter.read_line(true).ok_or(LoginError::Cancelled)?;
        let pasted = pasted.trim();
        if pasted.is_empty() {
            return Err(LoginError::InvalidCode);
        }
        let mut tokens = registered_key(http, pasted)?.unwrap_or_else(|| OAuthTokens::new(pasted));
        let status = client::fetch_user_status(http, &tokens)?;
        if ProviderError::is_auth_failure(&status) {
            return Err(LoginError::Failed(
                "Windsurf rejected that token. A token works for 5 minutes, so click Try Again to get a new one.".into(),
            ));
        }
        if !status.is_success() {
            return Err(LoginError::RequestFailed(status.status));
        }
        let body = status.json().ok_or(LoginError::InvalidResponse)?;
        let user = body
            .get("userStatus")
            .and_then(Value::as_object)
            .ok_or(LoginError::InvalidResponse)?;
        let email = user
            .get("email")
            .and_then(Value::as_str)
            .filter(|email| !email.is_empty())
            .map(str::to_string);
        if let Some(email) = email.clone() {
            let team = user
                .get("teamId")
                .and_then(Value::as_str)
                .filter(|team| !team.is_empty());
            let identity = match team {
                Some(team) => format!("devin:{email}/{team}"),
                None => format!("devin:{email}"),
            };
            tokens
                .extras
                .insert(OAuthTokens::IDENTITY_KEY.to_string(), identity);
        }
        Ok((
            email.unwrap_or_else(|| "Devin Account".into()),
            Credential::OAuth(tokens),
        ))
    }
}

fn token_page(state: &str) -> String {
    let mut url = url::Url::parse("https://windsurf.com/profile").expect("devin token page");
    url.query_pairs_mut()
        .append_pair("response_type", "token")
        .append_pair("redirect_uri", "show-auth-token")
        .append_pair("state", state)
        .append_pair("scope", "openid profile email")
        .append_pair("redirect_parameters_type", "query");
    url.to_string()
}

/// RegisterUser refuses anything but a page token. After a refusal the pasted value is the API key.
fn registered_key(http: &dyn HttpClient, token: &str) -> Result<Option<OAuthTokens>, LoginError> {
    let response = http
        .send(
            HttpRequest::post(REGISTER_URL)
                .json_body(&serde_json::json!({ "firebase_id_token": token })),
        )
        .map_err(LoginError::from)?;
    if ProviderError::is_auth_failure(&response) {
        return Ok(None);
    }
    if !response.is_success() {
        return Err(LoginError::RequestFailed(response.status));
    }
    let body = response.json().ok_or(LoginError::InvalidResponse)?;
    let api_key = body
        .get("api_key")
        .and_then(Value::as_str)
        .filter(|key| !key.is_empty())
        .ok_or(LoginError::InvalidResponse)?
        .to_string();
    let mut tokens = OAuthTokens::new(api_key);
    if let Some(server) = body
        .get("api_server_url")
        .and_then(Value::as_str)
        .filter(|server| !server.is_empty())
    {
        let url = url::Url::parse(server).map_err(|_| LoginError::InvalidResponse)?;
        if url.scheme() != "https" || url.host().is_none() {
            return Err(LoginError::InvalidResponse);
        }
        tokens
            .extras
            .insert(client::API_SERVER_KEY.to_string(), server.to_string());
    }
    Ok(Some(tokens))
}
