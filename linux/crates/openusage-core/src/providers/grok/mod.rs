//! Port of `GrokUsageClient`, `GrokLogin`, and Grok's token renewal in `AccountProviderRuntime`.

mod mapper;

use base64::Engine;
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::Value;

use crate::account::{Credential, OAuthTokens};
use crate::error::{LoginError, ProviderError};
use crate::http::{HttpClient, HttpRequest, HttpResponse};
use crate::kind::ProviderKind;
use crate::log;
use crate::login::{poll_device_code, LoginPrompt, LoginPrompter, Poll};
use crate::metric::ProviderUsage;
use crate::provider::{FetchContext, Login, LoginMethod, Provider};
use crate::refresh::SECONDARY_READ_INTERVAL_SECONDS;

const CLIENT_ID: &str = "b1a00492-073a-47ea-816f-4c329264a828";
const SCOPES: &str = "openid email offline_access grok-cli:access";
const TOKEN_URL: &str = "https://auth.x.ai/oauth2/token";
const DEVICE_URL: &str = "https://auth.x.ai/oauth2/device/code";
const CREDITS_URL: &str = "https://cli-chat-proxy.grok.com/v1/billing?format=credits";
const SETTINGS_URL: &str = "https://cli-chat-proxy.grok.com/v1/settings";
const TOKEN_AUTH: &str = "xai-grok-cli";
const PLAN: &str = "plan";
const UNSAVED: &str = "unsaved_renewal";
const EARLY_HOLD: &str = "early_renewal_not_before";

const REFUSALS: &[&str] = &[
    "invalid_grant",
    "refresh_token_expired",
    "refresh_token_reused",
    "refresh_token_invalidated",
];

pub fn provider() -> Box<dyn Provider> {
    Box::new(GrokProvider)
}

pub fn login(_method: &LoginMethod) -> Box<dyn Login> {
    Box::new(GrokLogin)
}

struct GrokProvider;

impl Provider for GrokProvider {
    fn fetch(&self, ctx: &mut FetchContext) -> Result<ProviderUsage, ProviderError> {
        let mut tokens = load_tokens(ctx)?;
        tokens = fresh_tokens(ctx, tokens)?;
        let mut response = credits(ctx.http, &tokens.access_token)?;
        if response.status == 401 && tokens.refresh_token.is_some() {
            log::debug(
                "auth",
                &format!(
                    "{} usage read unauthorized; renewing the sign-in once",
                    ctx.account.card_id()
                ),
            );
            tokens = renew(ctx, tokens)?;
            response = credits(ctx.http, &tokens.access_token)?;
        }
        let mapped = mapper::map_credits(&response)?;
        Ok(ProviderUsage {
            breakdowns: Default::default(),
            plan: read_plan(ctx, &tokens.access_token),
            lines: mapped.lines,
            notice: mapped.notice,
        })
    }
}

struct GrokLogin;

impl Login for GrokLogin {
    fn login(
        &self,
        prompter: &dyn LoginPrompter,
        http: &dyn HttpClient,
    ) -> Result<(String, Credential), LoginError> {
        let start = http
            .send(form_request(
                DEVICE_URL,
                &[("client_id", CLIENT_ID), ("scope", SCOPES)],
                true,
                true,
            ))
            .map_err(LoginError::from)?;
        if !(200..300).contains(&start.status) {
            return Err(LoginError::RequestFailed(start.status));
        }
        let device: DeviceCode =
            serde_json::from_slice(&start.body).map_err(|_| LoginError::InvalidResponse)?;
        let verification =
            url::Url::parse(&device.verification_uri).map_err(|_| LoginError::InvalidResponse)?;
        if verification.scheme() != "https" {
            return Err(LoginError::InvalidResponse);
        }
        prompter.show(&LoginPrompt::DeviceCode {
            user_code: device.user_code.clone(),
            verification_url: device.verification_uri.clone(),
        });
        let interval = device.interval.unwrap_or(5.0).max(1.0) as u64;
        let device_code = device.device_code.clone();
        let token = poll_device_code(interval, device.expires_in, || {
            poll_token(http, &device_code)
        })?;
        let claims = jwt_object(&token.access_token);
        let email = claim(claims.as_ref(), "email");
        let sub = claim(claims.as_ref(), "sub");
        let label = email.clone().unwrap_or_else(|| "Grok Account".into());
        let mut tokens = OAuthTokens::new(token.access_token);
        tokens.refresh_token = token.refresh_token;
        tokens.expires_at = token
            .expires_in
            .map(|seconds| add_seconds(Utc::now(), seconds));
        if let Some(id) = sub.or(email) {
            tokens
                .extras
                .insert(OAuthTokens::IDENTITY_KEY.to_string(), id);
        }
        Ok((label, Credential::OAuth(tokens)))
    }
}

#[derive(Deserialize)]
struct DeviceCode {
    device_code: String,
    user_code: String,
    verification_uri: String,
    expires_in: u64,
    #[serde(default)]
    interval: Option<f64>,
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    expires_in: Option<f64>,
}

fn poll_token(http: &dyn HttpClient, device_code: &str) -> Result<Poll<TokenResponse>, LoginError> {
    let request = form_request(
        TOKEN_URL,
        &[
            ("client_id", CLIENT_ID),
            ("device_code", device_code),
            ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
        ],
        true,
        true,
    );
    let response = match http.send(request) {
        Ok(response) => response,
        Err(ProviderError::ConnectionFailed) => {
            log::warn(
                "auth",
                "device sign-in poll failed; polling again: connection failed",
            );
            return Ok(Poll::Pending);
        }
        Err(error) => return Err(error.into()),
    };
    if let Ok(token) = serde_json::from_slice::<TokenResponse>(&response.body) {
        if !token.access_token.is_empty() {
            return Ok(Poll::Done(token));
        }
    }
    match response
        .json()
        .and_then(|body| {
            body.get("error")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .as_deref()
    {
        Some("authorization_pending") => Ok(Poll::Pending),
        Some("slow_down") => Ok(Poll::SlowDown),
        Some("expired_token") => Err(LoginError::Failed("xAI device sign-in expired.".into())),
        Some("access_denied") => Err(LoginError::Failed("xAI device sign-in failed.".into())),
        _ => Err(LoginError::Failed("xAI device sign-in failed.".into())),
    }
}

fn load_tokens(ctx: &mut FetchContext) -> Result<OAuthTokens, ProviderError> {
    if let Some(unsaved) = ctx.state.get::<OAuthTokens>(UNSAVED) {
        match ctx.save_credential(&Credential::OAuth(unsaved.clone())) {
            Ok(()) => ctx.state.remove(UNSAVED),
            Err(error) => {
                log::error(
                    "auth",
                    &format!(
                        "{} couldn't save the renewed sign-in; keeping it and retrying: {error}",
                        ctx.account.card_id()
                    ),
                );
                return Ok(unsaved);
            }
        }
    }
    ctx.oauth()
}

fn fresh_tokens(ctx: &mut FetchContext, tokens: OAuthTokens) -> Result<OAuthTokens, ProviderError> {
    let Some(expires_at) = tokens.expires_at else {
        return Ok(tokens);
    };
    if !tokens.needs_refresh(ctx.now) || early_held(ctx, expires_at) {
        return Ok(tokens);
    }
    match renew(ctx, tokens.clone()) {
        Ok(updated) => Ok(updated),
        Err(error) if expires_at > ctx.now => {
            hold_early(ctx, &error);
            log::warn(
                "auth",
                &format!(
                    "{} early token renewal failed; keeping the current token: {error}",
                    ctx.account.card_id()
                ),
            );
            Ok(tokens)
        }
        Err(error) => Err(error),
    }
}

/// Saves the rotated tokens before they are used. A provider that rotates refresh tokens refuses the old one.
fn renew(ctx: &mut FetchContext, tokens: OAuthTokens) -> Result<OAuthTokens, ProviderError> {
    let Some(refresh) = tokens.refresh_token.clone() else {
        return Ok(tokens);
    };
    // Tokens this process couldn't save are newer than the keyring's copy, which is not checked then.
    let (_renewal, elsewhere) = ctx.begin_renewal(&tokens);
    if let Some(stored) = elsewhere.filter(|_| ctx.state.get::<OAuthTokens>(UNSAVED).is_none()) {
        return Ok(stored);
    }
    let granted = refresh_grant(ctx.http, &refresh, ctx.now)?;
    let mut updated = tokens;
    updated.access_token = granted.access_token;
    if let Some(refresh) = granted.refresh_token {
        updated.refresh_token = Some(refresh);
    }
    updated.expires_at = granted.expires_at;
    match ctx.save_credential(&Credential::OAuth(updated.clone())) {
        Ok(()) => ctx.state.remove(UNSAVED),
        Err(error) => {
            log::error(
                "auth",
                &format!(
                    "{} couldn't save the renewed sign-in; keeping it and retrying: {error}",
                    ctx.account.card_id()
                ),
            );
            ctx.state.set(UNSAVED, &updated);
        }
    }
    Ok(updated)
}

struct Grant {
    access_token: String,
    refresh_token: Option<String>,
    expires_at: Option<DateTime<Utc>>,
}

fn refresh_grant(
    http: &dyn HttpClient,
    refresh_token: &str,
    now: DateTime<Utc>,
) -> Result<Grant, ProviderError> {
    let response = http.send(form_request(
        TOKEN_URL,
        &[
            ("grant_type", "refresh_token"),
            ("client_id", CLIENT_ID),
            ("refresh_token", refresh_token),
        ],
        false,
        false,
    ))?;
    if matches!(response.status, 400 | 401 | 403) {
        if let Some(code) = oauth_error_code(&response.body) {
            if REFUSALS.contains(&code.as_str()) {
                log::warn("auth", &format!("grok sign-in renewal refused: {code}"));
                return Err(ProviderError::SignInExpired(ProviderKind::Grok));
            }
        }
    }
    if !(200..300).contains(&response.status) {
        return Err(ProviderError::failed(&response));
    }
    let token: TokenResponse =
        serde_json::from_slice(&response.body).map_err(|_| ProviderError::InvalidResponse)?;
    if token.access_token.is_empty() {
        return Err(ProviderError::InvalidResponse);
    }
    Ok(Grant {
        access_token: token.access_token,
        refresh_token: token.refresh_token,
        expires_at: token.expires_in.map(|seconds| add_seconds(now, seconds)),
    })
}

fn oauth_error_code(body: &[u8]) -> Option<String> {
    let value: Value = serde_json::from_slice(body).ok()?;
    let error = value.get("error");
    error
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| {
            error
                .and_then(|error| error.get("code"))
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .or_else(|| {
            value
                .get("code")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
}

fn read_plan(ctx: &mut FetchContext, access_token: &str) -> Option<String> {
    if ctx
        .state
        .is_due(PLAN, SECONDARY_READ_INTERVAL_SECONDS, ctx.now, ctx.force)
    {
        if let Ok(response) = settings(ctx.http, access_token) {
            if let Some(plan) = mapper::plan_name(&response) {
                ctx.state.set(PLAN, &plan);
            }
        }
        ctx.state.mark_read(PLAN, ctx.now);
    }
    ctx.state.get(PLAN)
}

fn early_held(ctx: &FetchContext, expires_at: DateTime<Utc>) -> bool {
    if expires_at <= ctx.now {
        return false;
    }
    ctx.state
        .get::<i64>(EARLY_HOLD)
        .and_then(DateTime::from_timestamp_millis)
        .is_some_and(|until| ctx.now < until)
}

fn hold_early(ctx: &mut FetchContext, error: &ProviderError) {
    if let ProviderError::RateLimited {
        retry_after_seconds: Some(seconds),
    } = error
    {
        let until = ctx.now + chrono::Duration::milliseconds((seconds * 1000.0).round() as i64);
        ctx.state.set(EARLY_HOLD, &until.timestamp_millis());
    }
}

fn credits(http: &dyn HttpClient, access_token: &str) -> Result<HttpResponse, ProviderError> {
    authed(http, CREDITS_URL, access_token)
}

fn settings(http: &dyn HttpClient, access_token: &str) -> Result<HttpResponse, ProviderError> {
    authed(http, SETTINGS_URL, access_token)
}

fn authed(
    http: &dyn HttpClient,
    url: &str,
    access_token: &str,
) -> Result<HttpResponse, ProviderError> {
    let mut request = HttpRequest::get(url)
        .bearer(access_token.trim())
        .header("X-XAI-Token-Auth", TOKEN_AUTH)
        .header("Accept", "application/json")
        .header("User-Agent", "OpenUsage");
    request.timeout = Duration::from_secs(10);
    http.send(request)
}

fn form_request(url: &str, fields: &[(&str, &str)], sort: bool, accept_json: bool) -> HttpRequest {
    let mut pairs: Vec<String> = fields
        .iter()
        .map(|(key, value)| format!("{key}={}", percent_encode(value)))
        .collect();
    if sort {
        pairs.sort();
    }
    let mut request =
        HttpRequest::post(url).header("Content-Type", "application/x-www-form-urlencoded");
    if accept_json {
        request = request.header("Accept", "application/json");
    }
    request.body = Some(pairs.join("&").into_bytes());
    request
}

fn percent_encode(value: &str) -> String {
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

fn add_seconds(now: DateTime<Utc>, seconds: f64) -> DateTime<Utc> {
    now + chrono::Duration::milliseconds((seconds * 1000.0).round() as i64)
}

fn jwt_object(token: &str) -> Option<serde_json::Map<String, Value>> {
    let payload = token.split('.').nth(1)?.trim_end_matches('=');
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload)
        .ok()?;
    serde_json::from_slice(&bytes).ok()
}

fn claim(claims: Option<&serde_json::Map<String, Value>>, name: &str) -> Option<String> {
    claims?
        .get(name)
        .and_then(Value::as_str)
        .map(str::to_string)
}

#[cfg(test)]
mod tests;
