//! Claude HTTP: usage, profile, reset grants, and the refresh grant.
//! Port of `ClaudeUsageClient` and `ClaudeLogin.refresh`.

use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::error::ProviderError;
use crate::http::{HttpClient, HttpRequest, HttpResponse};
use crate::kind::ProviderKind;
use crate::log;

pub(super) const CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";
pub(super) const AUTHORIZE_URL: &str = "https://claude.ai/oauth/authorize";
pub(super) const TOKEN_URL: &str = "https://platform.claude.com/v1/oauth/token";
pub(super) const SCOPES: &str = "user:profile";
pub(super) const USAGE_URL: &str = "https://api.anthropic.com/api/oauth/usage";
pub(super) const PROFILE_URL: &str = "https://api.anthropic.com/api/oauth/profile";
pub(super) const RESET_GRANTS_URL: &str =
    "https://api.anthropic.com/api/oauth/usage?cedar_ember=1&skip_spend=1";
pub(super) const CLAUDE_CODE_VERSION: &str = "2.1.294";
const BETA: &str = "oauth-2025-04-20";

pub(crate) struct Grant {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub expires_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone)]
pub(crate) struct Profile {
    pub account_uuid: String,
    pub organization_uuid: Option<String>,
    pub has_organization: bool,
    pub organization_type: Option<String>,
    pub rate_limit_tier: Option<String>,
}

impl Profile {
    pub(crate) fn identity(&self) -> String {
        match &self.organization_uuid {
            Some(organization) => format!("{}/{}", self.account_uuid, organization),
            None => self.account_uuid.clone(),
        }
    }
}

pub(super) fn usage_request(access_token: &str) -> HttpRequest {
    timed(
        claude_code_headers(HttpRequest::get(USAGE_URL), access_token),
        10,
    )
}

pub(crate) fn grants_request(access_token: &str) -> HttpRequest {
    timed(
        claude_code_headers(HttpRequest::get(RESET_GRANTS_URL), access_token),
        10,
    )
}

/// `POST /api/organizations/{uuid}/reset_rate_limits`. Body keys sort, matching Claude Code.
pub(crate) fn claim_request(
    access_token: &str,
    organization: &str,
    grant_id: &str,
    request_id: &str,
) -> HttpRequest {
    let url =
        format!("https://api.anthropic.com/api/organizations/{organization}/reset_rate_limits");
    let body = json!({
        "grant_id": grant_id,
        "program": "cedar_ember",
        "request_id": request_id,
    });
    timed(
        claude_code_headers(HttpRequest::post(url).json_body(&body), access_token),
        25,
    )
}

/// The hourly plan read. Login uses a bearer-only request; this one matches `fetchProfile`.
pub(crate) fn profile_request(access_token: &str) -> HttpRequest {
    timed(
        HttpRequest::get(PROFILE_URL)
            .bearer(access_token.trim())
            .header("Accept", "application/json")
            .header("anthropic-beta", BETA),
        10,
    )
}

fn claude_code_headers(request: HttpRequest, access_token: &str) -> HttpRequest {
    request
        .bearer(access_token.trim())
        .header("Accept", "application/json")
        .header("Content-Type", "application/json")
        .header("anthropic-beta", BETA)
        .header(
            "User-Agent",
            format!("claude-cli/{CLAUDE_CODE_VERSION} (external, cli)"),
        )
}

fn timed(mut request: HttpRequest, seconds: u64) -> HttpRequest {
    request.timeout = Duration::from_secs(seconds);
    request
}

pub(crate) fn refresh(
    refresh_token: &str,
    http: &dyn HttpClient,
    now: DateTime<Utc>,
) -> Result<Grant, ProviderError> {
    let body = json!({
        "grant_type": "refresh_token",
        "refresh_token": refresh_token,
        "client_id": CLIENT_ID,
        "scope": SCOPES,
    });
    let response = http.send(timed(HttpRequest::post(TOKEN_URL).json_body(&body), 15))?;
    parse_grant(&response, now)
}

pub(crate) fn decode_profile(response: &HttpResponse) -> Result<Profile, ProviderError> {
    if !response.is_success() {
        return Err(ProviderError::failed(response));
    }
    let body = response
        .json()
        .filter(|value| value.is_object())
        .ok_or(ProviderError::InvalidResponse)?;
    let account = body
        .get("account")
        .filter(|value| value.is_object())
        .ok_or(ProviderError::InvalidResponse)?;
    let account_uuid = account
        .get("uuid")
        .and_then(Value::as_str)
        .filter(|uuid| !uuid.is_empty())
        .ok_or(ProviderError::InvalidResponse)?
        .to_string();
    let organization = body.get("organization").filter(|value| value.is_object());
    Ok(Profile {
        account_uuid,
        organization_uuid: organization
            .and_then(|org| org.get("uuid"))
            .and_then(Value::as_str)
            .map(str::to_string),
        has_organization: organization.is_some(),
        organization_type: organization
            .and_then(|org| org.get("organization_type"))
            .and_then(Value::as_str)
            .map(str::to_string),
        rate_limit_tier: organization
            .and_then(|org| org.get("rate_limit_tier"))
            .and_then(Value::as_str)
            .map(str::to_string),
    })
}

fn parse_grant(response: &HttpResponse, now: DateTime<Utc>) -> Result<Grant, ProviderError> {
    if matches!(response.status, 400 | 401 | 403) {
        if let Some(code) = error_code(&response.body) {
            if matches!(
                code.as_str(),
                "invalid_grant"
                    | "refresh_token_expired"
                    | "refresh_token_reused"
                    | "refresh_token_invalidated"
            ) {
                log::warn("auth", &format!("claude sign-in renewal refused: {code}"));
                return Err(ProviderError::SignInExpired(ProviderKind::Claude));
            }
        }
    }
    if !response.is_success() {
        return Err(ProviderError::failed(response));
    }
    let token: TokenBody = response.decode()?;
    if token.access_token.is_empty() {
        return Err(ProviderError::InvalidResponse);
    }
    let expires_at = token
        .expires_in
        .filter(|seconds| seconds.is_finite())
        .and_then(|seconds| {
            now.checked_add_signed(chrono::Duration::milliseconds(
                (seconds * 1000.0).round() as i64
            ))
        });
    Ok(Grant {
        access_token: token.access_token,
        refresh_token: token.refresh_token,
        expires_at,
    })
}

#[derive(Deserialize)]
struct TokenBody {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    expires_in: Option<f64>,
}

fn error_code(body: &[u8]) -> Option<String> {
    let value: Value = serde_json::from_slice(body).ok()?;
    match value.get("error") {
        Some(Value::String(code)) => return Some(code.clone()),
        Some(Value::Object(map)) => {
            if let Some(code) = map.get("code").and_then(Value::as_str) {
                return Some(code.to_string());
            }
        }
        _ => {}
    }
    value
        .get("code")
        .and_then(Value::as_str)
        .map(str::to_string)
}
