//! Port of `CursorUsageClient.swift`.

use serde_json::{Map, Value};

use crate::error::ProviderError;
use crate::http::{HttpClient, HttpRequest, HttpResponse};
use crate::log;

use super::parse::{jwt_subject, trimmed_nonempty};

pub(crate) const USAGE_URL: &str =
    "https://api2.cursor.sh/aiserver.v1.DashboardService/GetCurrentPeriodUsage";
pub(crate) const GROK_URL: &str =
    "https://api2.cursor.sh/aiserver.v1.DashboardService/GetSandUsageStatus";
pub(crate) const PLAN_URL: &str = "https://api2.cursor.sh/aiserver.v1.DashboardService/GetPlanInfo";
pub(crate) const REFRESH_URL: &str = "https://api2.cursor.sh/oauth/token";
pub(crate) const CREDITS_URL: &str =
    "https://api2.cursor.sh/aiserver.v1.DashboardService/GetCreditGrantsBalance";
pub(crate) const REST_USAGE_URL: &str = "https://cursor.com/api/usage";
pub(crate) const SUMMARY_URL: &str = "https://cursor.com/api/usage-summary";
pub(crate) const STRIPE_URL: &str = "https://cursor.com/api/auth/stripe";
pub(crate) const PROFILE_URL: &str = "https://cursor.com/api/auth/me";
pub(crate) const EVENTS_URL: &str = "https://cursor.com/api/dashboard/get-filtered-usage-events";
pub(crate) const CLIENT_ID: &str = "KbZUR41cY7W6zRSdpSUJ7I7mLYBKOCmB";
pub(crate) const POLL_URL: &str = "https://api2.cursor.sh/auth/poll";

pub(crate) struct Session {
    pub user_id: String,
    pub cookie: String,
}

pub(crate) fn session(access_token: &str) -> Option<Session> {
    let subject = jwt_subject(access_token)?;
    let trimmed = subject.trim();
    if trimmed.is_empty() {
        return None;
    }
    let parts: Vec<&str> = trimmed.split('|').collect();
    let user_id = if parts.len() > 1 { parts[1] } else { parts[0] };
    if user_id.is_empty() {
        return None;
    }
    Some(Session {
        cookie: format!("WorkosCursorSessionToken={user_id}%3A%3A{access_token}"),
        user_id: user_id.to_string(),
    })
}

pub(crate) fn refresh_token(
    http: &dyn HttpClient,
    refresh_token: &str,
) -> Result<HttpResponse, ProviderError> {
    let mut request = HttpRequest::post(REFRESH_URL).json_body(&serde_json::json!({
        "grant_type": "refresh_token",
        "client_id": CLIENT_ID,
        "refresh_token": refresh_token,
    }));
    request.timeout = std::time::Duration::from_secs(15);
    http.send(request)
}

pub(crate) fn fetch_usage(
    http: &dyn HttpClient,
    access_token: &str,
) -> Result<HttpResponse, ProviderError> {
    connect_post(http, USAGE_URL, access_token)
}

pub(crate) fn fetch_grok(
    http: &dyn HttpClient,
    access_token: &str,
) -> Result<HttpResponse, ProviderError> {
    connect_post(http, GROK_URL, access_token)
}

pub(crate) fn fetch_plan(
    http: &dyn HttpClient,
    access_token: &str,
) -> Result<HttpResponse, ProviderError> {
    connect_post(http, PLAN_URL, access_token)
}

pub(crate) fn fetch_credits(
    http: &dyn HttpClient,
    access_token: &str,
) -> Result<HttpResponse, ProviderError> {
    connect_post(http, CREDITS_URL, access_token)
}

pub(crate) fn fetch_requests(
    http: &dyn HttpClient,
    access_token: &str,
) -> Result<Option<HttpResponse>, ProviderError> {
    let Some(session) = session(access_token) else {
        return Ok(None);
    };
    let mut url = url::Url::parse(REST_USAGE_URL).map_err(|_| ProviderError::InvalidResponse)?;
    url.query_pairs_mut().append_pair("user", &session.user_id);
    session_get(http, url.as_str(), &session.cookie).map(Some)
}

pub(crate) fn fetch_summary(
    http: &dyn HttpClient,
    access_token: &str,
) -> Result<Option<HttpResponse>, ProviderError> {
    session_optional(http, SUMMARY_URL, access_token)
}

pub(crate) fn fetch_stripe(
    http: &dyn HttpClient,
    access_token: &str,
) -> Result<Option<HttpResponse>, ProviderError> {
    session_optional(http, STRIPE_URL, access_token)
}

pub(crate) fn fetch_profile(
    http: &dyn HttpClient,
    access_token: &str,
) -> Result<Option<HttpResponse>, ProviderError> {
    session_optional(http, PROFILE_URL, access_token)
}

pub(crate) fn fetch_events_page(
    http: &dyn HttpClient,
    access_token: &str,
    start_ms: i64,
    end_ms: i64,
    page: i64,
    page_size: i64,
) -> Result<Option<HttpResponse>, ProviderError> {
    let Some(session) = session(access_token) else {
        return Ok(None);
    };
    let mut request = HttpRequest::post(EVENTS_URL)
        .header("Cookie", session.cookie)
        .header("Origin", "https://cursor.com")
        .json_body(&serde_json::json!({
            "startDate": start_ms.to_string(),
            "endDate": end_ms.to_string(),
            "page": page,
            "pageSize": page_size,
        }));
    request.timeout = std::time::Duration::from_secs(15);
    http.send(request).map(Some)
}

fn session_optional(
    http: &dyn HttpClient,
    url: &str,
    access_token: &str,
) -> Result<Option<HttpResponse>, ProviderError> {
    let Some(session) = session(access_token) else {
        return Ok(None);
    };
    session_get(http, url, &session.cookie).map(Some)
}

fn session_get(
    http: &dyn HttpClient,
    url: &str,
    cookie: &str,
) -> Result<HttpResponse, ProviderError> {
    let mut request = HttpRequest::get(url).header("Cookie", cookie);
    request.timeout = std::time::Duration::from_secs(10);
    http.send(request)
}

fn connect_post(
    http: &dyn HttpClient,
    url: &str,
    access_token: &str,
) -> Result<HttpResponse, ProviderError> {
    let mut request = HttpRequest::post(url)
        .bearer(access_token)
        .header("Connect-Protocol-Version", "1")
        .json_body(&serde_json::json!({}));
    request.timeout = std::time::Duration::from_secs(10);
    http.send(request)
}

/// A secondary read that never fails the card: transport errors, non-2xx, and non-objects become `None`.
pub(crate) fn optional_json(
    account_id: &str,
    label: &str,
    request: Result<Option<HttpResponse>, ProviderError>,
) -> Option<Map<String, Value>> {
    let response = match request {
        Ok(Some(response)) => response,
        Ok(None) => {
            log::warn("plugin:cursor", &format!("{account_id} optional {label} request could not be prepared from the current session"));
            return None;
        }
        Err(error) => {
            log::warn(
                "plugin:cursor",
                &format!("{account_id} optional {label} request failed: {error}"),
            );
            return None;
        }
    };
    if !response.is_success() {
        log::warn(
            "plugin:cursor",
            &format!(
                "{account_id} optional {label} request returned HTTP {}",
                response.status
            ),
        );
        return None;
    }
    match response.json().and_then(|value| value.as_object().cloned()) {
        Some(object) => Some(object),
        None => {
            log::warn(
                "plugin:cursor",
                &format!("{account_id} optional {label} response was not a JSON object"),
            );
            None
        }
    }
}

pub(crate) fn plan_name(body: &Map<String, Value>) -> Option<String> {
    body.get("planInfo")
        .and_then(Value::as_object)
        .and_then(|info| info.get("planName"))
        .and_then(Value::as_str)
        .and_then(trimmed_nonempty)
        .map(str::to_string)
}
