//! Port of `AntigravityUsageClient.swift`. The Linux user agent says `os_type=linux`.

use std::time::Duration;

use chrono::{DateTime, Duration as ChronoDuration, Utc};
use serde_json::Value;

use crate::error::ProviderError;
use crate::http::{HttpClient, HttpRequest, HttpResponse};
use crate::kind::ProviderKind;
use crate::log;

use super::mapper;

pub(crate) const UNAVAILABLE: &str =
    "Antigravity usage is temporarily unavailable. Try again shortly.";
const BASES: [&str; 2] = [
    "https://cloudcode-pa.googleapis.com",
    "https://daily-cloudcode-pa.googleapis.com",
];
const LOAD_PATH: &str = "/v1internal:loadCodeAssist";
const QUOTA_PATH: &str = "/v1internal:retrieveUserQuotaSummary";
const LOAD_PAYLOAD: &[u8] = br#"{"metadata":{"ideType":"ANTIGRAVITY","platform":"PLATFORM_UNSPECIFIED","pluginType":"GEMINI"}}"#;
pub(crate) const TOKEN_URL: &str = "https://oauth2.googleapis.com/token";
pub(crate) const CLIENT_ID: &str =
    "1071006060591-tmhssin2h21lcre235vtolojh4g403ep.apps.googleusercontent.com";
pub(crate) const CLIENT_SECRET: &str = "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";

pub(crate) enum QuotaError {
    Refused,
    Failed(ProviderError),
}

pub(crate) struct Grant {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub expires_at: Option<DateTime<Utc>>,
}

pub(crate) fn user_agent() -> &'static str {
    if cfg!(target_arch = "aarch64") {
        "antigravity/2.24.0 (aidev_client; os_type=linux; arch=arm64)"
    } else {
        "antigravity/2.24.0 (aidev_client; os_type=linux; arch=amd64)"
    }
}

pub(crate) fn load_code_assist(
    http: &dyn HttpClient,
    token: &str,
) -> Result<Vec<u8>, ProviderError> {
    for base in BASES {
        let Ok(response) = http.send(cloud_request(
            &format!("{base}{LOAD_PATH}"),
            token,
            LOAD_PAYLOAD,
        )) else {
            continue;
        };
        if ProviderError::is_auth_failure(&response) {
            return Err(ProviderError::SignInExpired(ProviderKind::Antigravity));
        }
        if response.is_success() {
            return Ok(response.body);
        }
    }
    Err(ProviderError::Unavailable(UNAVAILABLE.into()))
}

pub(crate) fn quota_summary(
    http: &dyn HttpClient,
    token: &str,
    project: Option<&str>,
) -> Result<Vec<Vec<u8>>, QuotaError> {
    let payload = match project {
        Some(project) => serde_json::to_vec(&serde_json::json!({ "project": project }))
            .unwrap_or_else(|_| b"{}".to_vec()),
        None => b"{}".to_vec(),
    };
    let mut responses = Vec::new();
    let mut transport = None;
    for (index, base) in BASES.iter().enumerate() {
        let response = match http.send(cloud_request(
            &format!("{base}{QUOTA_PATH}"),
            token,
            &payload,
        )) {
            Ok(response) => response,
            Err(error) => {
                transport = Some(error);
                continue;
            }
        };
        let stop =
            index == 0 && response.is_success() && mapper::is_complete_summary(&response.body);
        responses.push(response);
        if stop {
            break;
        }
    }
    if responses.is_empty() {
        return Err(QuotaError::Failed(
            transport.unwrap_or(ProviderError::ConnectionFailed),
        ));
    }
    let bodies: Vec<Vec<u8>> = responses
        .iter()
        .filter(|response| response.is_success())
        .map(|response| response.body.clone())
        .collect();
    if !bodies.is_empty() {
        return Ok(bodies);
    }
    if responses.iter().any(|response| response.status == 401) {
        return Err(QuotaError::Refused);
    }
    if responses.iter().any(requires_subscription) {
        let message = if project.is_none() {
            "Antigravity couldn't find a project with quota access for this account. Try again shortly."
        } else {
            "Google reports no Antigravity quota subscription for this project."
        };
        return Err(QuotaError::Failed(ProviderError::Unavailable(
            message.into(),
        )));
    }
    if responses.iter().any(ProviderError::is_auth_failure) {
        return Err(QuotaError::Failed(ProviderError::SignInExpired(
            ProviderKind::Antigravity,
        )));
    }
    if let Some(limited) = responses.iter().find(|response| response.status == 429) {
        return Err(QuotaError::Failed(ProviderError::failed(limited)));
    }
    Err(QuotaError::Failed(ProviderError::Unavailable(
        UNAVAILABLE.into(),
    )))
}

fn requires_subscription(response: &HttpResponse) -> bool {
    if response.status != 403 {
        return false;
    }
    let Some(details) = response
        .json()
        .and_then(|value| value.get("error")?.get("details")?.as_array().cloned())
    else {
        return false;
    };
    details
        .iter()
        .any(|detail| detail.get("reason").and_then(Value::as_str) == Some("SUBSCRIPTION_REQUIRED"))
}

fn cloud_request(url: &str, token: &str, payload: &[u8]) -> HttpRequest {
    let mut request = HttpRequest::post(url)
        .bearer(token)
        .header("Accept", "application/json")
        .header("Content-Type", "application/json")
        .header("User-Agent", user_agent());
    request.body = Some(payload.to_vec());
    request.timeout = Duration::from_secs(15);
    request
}

/// `AntigravityLogin.refresh`. A missing `expires_in` lasts an hour. The refresh token stays the stored one.
pub(crate) fn refresh(
    http: &dyn HttpClient,
    refresh_token: &str,
    now: DateTime<Utc>,
) -> Result<Grant, ProviderError> {
    let request = HttpRequest::post(TOKEN_URL).form_body(&[
        ("client_id", CLIENT_ID),
        ("client_secret", CLIENT_SECRET),
        ("refresh_token", refresh_token),
        ("grant_type", "refresh_token"),
    ]);
    let response = http.send(request)?;
    let granted = renewal(&response)?;
    let seconds = granted.expires_in.unwrap_or(3600.0);
    let expires_at =
        now.checked_add_signed(ChronoDuration::milliseconds((seconds * 1000.0) as i64));
    Ok(Grant {
        access_token: granted.access_token,
        refresh_token: granted.refresh_token,
        expires_at,
    })
}

struct Granted {
    access_token: String,
    refresh_token: Option<String>,
    expires_in: Option<f64>,
}

fn renewal(response: &HttpResponse) -> Result<Granted, ProviderError> {
    if [400, 401, 403].contains(&response.status) {
        if let Some(code) = error_code(&response.body) {
            if [
                "invalid_grant",
                "refresh_token_expired",
                "refresh_token_reused",
                "refresh_token_invalidated",
            ]
            .contains(&code.as_str())
            {
                log::warn(
                    "plugin:antigravity",
                    &format!("antigravity sign-in renewal refused: {code}"),
                );
                return Err(ProviderError::SignInExpired(ProviderKind::Antigravity));
            }
        }
    }
    if !response.is_success() {
        return Err(ProviderError::failed(response));
    }
    let body = response.json().ok_or(ProviderError::InvalidResponse)?;
    let access_token = body
        .get("access_token")
        .and_then(Value::as_str)
        .filter(|token| !token.is_empty())
        .ok_or(ProviderError::InvalidResponse)?
        .to_string();
    let refresh_token = body
        .get("refresh_token")
        .and_then(Value::as_str)
        .filter(|token| !token.is_empty())
        .map(str::to_string);
    let expires_in = body
        .get("expires_in")
        .and_then(Value::as_f64)
        .filter(|seconds| seconds.is_finite());
    Ok(Granted {
        access_token,
        refresh_token,
        expires_in,
    })
}

fn error_code(body: &[u8]) -> Option<String> {
    let json: Value = serde_json::from_slice(body).ok()?;
    let error = json.get("error");
    error
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| {
            error
                .and_then(|error| error.get("code"))
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .or_else(|| json.get("code").and_then(Value::as_str).map(str::to_string))
}
