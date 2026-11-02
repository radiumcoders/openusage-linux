//! ChatGPT HTTP: usage, reset credits, reports, and the refresh grant.
//! Port of `CodexUsageClient` and `CodexLogin.refresh`. Claiming a reset credit is out of scope.

use std::time::Duration;

use base64::Engine;
use chrono::{DateTime, TimeZone, Utc};
use serde::Deserialize;
use serde_json::Value;

use crate::error::ProviderError;
use crate::http::{HttpClient, HttpRequest, HttpResponse};
use crate::kind::ProviderKind;
use crate::log;

pub(super) const CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
pub(super) const AUTHORIZE_URL: &str = "https://auth.openai.com/oauth/authorize";
pub(super) const TOKEN_URL: &str = "https://auth.openai.com/oauth/token";
pub(super) const SCOPES: &str = "openid profile email offline_access";
pub(super) const USAGE_URL: &str = "https://chatgpt.com/backend-api/wham/usage";
pub(super) const RESET_CREDITS_URL: &str =
    "https://chatgpt.com/backend-api/wham/rate-limit-reset-credits";
pub(super) const REPORTS_URL: &str = "https://chatgpt.com/backend-api/wham/";
pub(super) const DEVICE_START_URL: &str =
    "https://auth.openai.com/api/accounts/deviceauth/usercode";
pub(super) const DEVICE_TOKEN_URL: &str = "https://auth.openai.com/api/accounts/deviceauth/token";
pub(super) const DEVICE_VERIFY_URL: &str = "https://auth.openai.com/codex/device";
pub(super) const DEVICE_REDIRECT: &str = "https://auth.openai.com/deviceauth/callback";
pub(super) const REDIRECT_PORT: u16 = 1455;
pub(super) const REDIRECT_PATH: &str = "/auth/callback";

#[derive(Debug)]
pub(super) struct Grant {
    pub access_token: String,
    pub refresh_token: Option<String>,
    /// The access token's `exp` claim. A token that isn't a JWT has no expiry, which replaces the old one.
    pub expires_at: Option<DateTime<Utc>>,
}

pub(super) fn usage_request(access_token: &str, account_id: Option<&str>) -> HttpRequest {
    timed(
        usage_headers(HttpRequest::get(USAGE_URL), access_token, account_id),
        10,
    )
}

pub(super) fn reset_credits_request(access_token: &str, account_id: Option<&str>) -> HttpRequest {
    timed(
        usage_headers(
            HttpRequest::get(RESET_CREDITS_URL),
            access_token,
            account_id,
        )
        .header("OpenAI-Beta", "codex-1")
        .header("originator", "Codex Desktop"),
        10,
    )
}

pub(super) fn report_request(
    url: &str,
    access_token: &str,
    account_id: Option<&str>,
) -> HttpRequest {
    timed(
        usage_headers(HttpRequest::get(url), access_token, account_id),
        15,
    )
}

fn usage_headers(
    request: HttpRequest,
    access_token: &str,
    account_id: Option<&str>,
) -> HttpRequest {
    let request = request
        .bearer(access_token)
        .header("Accept", "application/json")
        .header("User-Agent", "OpenUsage");
    match account_id {
        Some(account_id) if !account_id.is_empty() => {
            request.header("ChatGPT-Account-Id", account_id)
        }
        _ => request,
    }
}

fn timed(mut request: HttpRequest, seconds: u64) -> HttpRequest {
    request.timeout = Duration::from_secs(seconds);
    request
}

/// RFC 3986 percent-encoding: space is `%20`, hex is uppercase. `form_body` sorts and uses `+`.
pub(super) fn form_encode(value: &str) -> String {
    let mut encoded = String::new();
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                encoded.push(byte as char)
            }
            _ => encoded.push_str(&format!("%{byte:02X}")),
        }
    }
    encoded
}

pub(super) fn refresh(refresh_token: &str, http: &dyn HttpClient) -> Result<Grant, ProviderError> {
    let body = format!(
        "grant_type=refresh_token&client_id={}&refresh_token={}",
        form_encode(CLIENT_ID),
        form_encode(refresh_token)
    );
    let mut request =
        HttpRequest::post(TOKEN_URL).header("Content-Type", "application/x-www-form-urlencoded");
    request.body = Some(body.into_bytes());
    request.timeout = Duration::from_secs(15);
    parse_grant(&http.send(request)?)
}

pub(super) fn sorted_form(url: &str, fields: &[(&str, &str)]) -> HttpRequest {
    let mut pairs: Vec<String> = fields
        .iter()
        .map(|(key, value)| format!("{}={}", form_encode(key), form_encode(value)))
        .collect();
    pairs.sort();
    let mut request = HttpRequest::post(url)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .header("Accept", "application/json");
    request.body = Some(pairs.join("&").into_bytes());
    request
}

fn parse_grant(response: &HttpResponse) -> Result<Grant, ProviderError> {
    if matches!(response.status, 400 | 401 | 403) {
        if let Some(code) = error_code(&response.body) {
            if matches!(
                code.as_str(),
                "invalid_grant"
                    | "refresh_token_expired"
                    | "refresh_token_reused"
                    | "refresh_token_invalidated"
            ) {
                log::warn("auth", &format!("codex sign-in renewal refused: {code}"));
                return Err(ProviderError::SignInExpired(ProviderKind::Codex));
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
    Ok(Grant {
        expires_at: jwt_expiry(&token.access_token),
        refresh_token: token.refresh_token,
        access_token: token.access_token,
    })
}

#[derive(Deserialize)]
struct TokenBody {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
}

pub(super) fn jwt_payload(token: &str) -> Option<Value> {
    let payload = token.split('.').nth(1)?;
    let mut padded = payload.replace('-', "+").replace('_', "/");
    while padded.len() % 4 != 0 {
        padded.push('=');
    }
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(padded)
        .ok()?;
    serde_json::from_slice(&bytes).ok()
}

pub(super) fn jwt_expiry(token: &str) -> Option<DateTime<Utc>> {
    let exp = jwt_payload(token)?.get("exp")?.as_f64()?;
    exp.is_finite().then_some(exp).and_then(|seconds| {
        Utc.timestamp_millis_opt((seconds * 1000.0).round() as i64)
            .single()
    })
}

/// `https://api.openai.com/auth` → `chatgpt_account_id`. Slashes keep this off `Value::pointer`.
pub(super) fn chatgpt_account_id(payload: &Value) -> Option<String> {
    payload
        .get("https://api.openai.com/auth")
        .and_then(|auth| auth.get("chatgpt_account_id"))
        .and_then(Value::as_str)
        .map(str::to_string)
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
