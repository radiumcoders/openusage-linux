//! Port of `AccountProviderRuntime+CursorAuth.swift`.
//! Cursor's access token is a 60-day JWT that is also its refresh token. Refreshing a week early keeps
//! the account alive when the machine is off around expiry. Rotated tokens are saved before they are used.

use chrono::{DateTime, Duration, Utc};
use serde_json::Value;

use crate::account::{Credential, OAuthTokens};
use crate::error::ProviderError;
use crate::http::HttpResponse;
use crate::kind::ProviderKind;
use crate::log;
use crate::provider::FetchContext;

use super::client;
use super::parse::jwt_expiry;

const LEAD: Duration = Duration::seconds(7 * 86_400);
const EARLY_HOLD: &str = "cursor_early_renewal_not_before";

pub(crate) fn prepare(ctx: &mut FetchContext<'_>) -> Result<OAuthTokens, ProviderError> {
    let mut tokens = ctx.oauth()?;
    let had_expiry = tokens.expires_at.is_some();
    if tokens.expires_at.is_none() {
        tokens.expires_at = jwt_expiry(&tokens.access_token);
    }
    let Some(expires_at) = tokens.expires_at else {
        return Ok(tokens);
    };
    let refresh_at = expires_at.checked_sub_signed(LEAD).unwrap_or(expires_at);
    if refresh_at <= ctx.now && !early_held(ctx, expires_at) {
        match refresh(ctx, &tokens) {
            Ok(refreshed) => return Ok(refreshed),
            Err(error) if expires_at > ctx.now => {
                hold_early(ctx, &error);
                log::warn(
                    "plugin:cursor",
                    &format!(
                        "{} early token refresh failed; keeping the current token: {error}",
                        ctx.account.card_id()
                    ),
                );
            }
            Err(error) => return Err(error),
        }
    }
    if !had_expiry {
        ctx.save_credential(&Credential::OAuth(tokens.clone()))?;
    }
    Ok(tokens)
}

/// Trades the refresh token at `/oauth/token`. Cursor answers with `access_token` and no `refresh_token`;
/// the new JWT is the next refresh token. `shouldLogout` means Cursor ended the session.
pub(crate) fn refresh(
    ctx: &mut FetchContext<'_>,
    tokens: &OAuthTokens,
) -> Result<OAuthTokens, ProviderError> {
    let Some(refresh_token) = tokens.refresh_token.clone() else {
        return Err(ProviderError::SignInExpired(ProviderKind::Cursor));
    };
    let (_renewal, elsewhere) = ctx.begin_renewal(tokens);
    if let Some(stored) = elsewhere {
        return Ok(stored);
    }
    let response = client::refresh_token(ctx.http, &refresh_token)?;
    if response
        .json()
        .is_some_and(|body| body.get("shouldLogout").and_then(Value::as_bool) == Some(true))
    {
        return Err(ProviderError::SignInExpired(ProviderKind::Cursor));
    }
    let granted = renewal(&response, ProviderKind::Cursor)?;
    let mut refreshed = tokens.clone();
    refreshed.access_token = granted.access_token.clone();
    refreshed.refresh_token = Some(granted.refresh_token.unwrap_or(granted.access_token));
    refreshed.expires_at = jwt_expiry(&refreshed.access_token);
    save_renewal(ctx, &refreshed);
    Ok(refreshed)
}

fn save_renewal(ctx: &FetchContext<'_>, tokens: &OAuthTokens) {
    if let Err(error) = ctx.save_credential(&Credential::OAuth(tokens.clone())) {
        log::error(
            "plugin:cursor",
            &format!(
                "{} couldn't save the renewed sign-in; keeping it for this read: {error}",
                ctx.account.card_id()
            ),
        );
    }
}

fn early_held(ctx: &FetchContext<'_>, expires_at: DateTime<Utc>) -> bool {
    expires_at > ctx.now
        && ctx
            .state
            .get::<DateTime<Utc>>(EARLY_HOLD)
            .is_some_and(|until| ctx.now < until)
}

fn hold_early(ctx: &mut FetchContext<'_>, error: &ProviderError) {
    let ProviderError::RateLimited {
        retry_after_seconds: Some(seconds),
    } = error
    else {
        return;
    };
    let millis = (*seconds * 1000.0) as i64;
    if let Some(until) = ctx.now.checked_add_signed(Duration::milliseconds(millis)) {
        ctx.state.set(EARLY_HOLD, &until);
    }
}

struct Granted {
    access_token: String,
    refresh_token: Option<String>,
}

/// `OAuthTokenResponse(renewal:)`. Only a refusal of the refresh token itself is an expired sign-in.
fn renewal(response: &HttpResponse, kind: ProviderKind) -> Result<Granted, ProviderError> {
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
                    "plugin:cursor",
                    &format!("{} sign-in renewal refused: {code}", kind.raw()),
                );
                return Err(ProviderError::SignInExpired(kind));
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
        .ok_or(ProviderError::InvalidResponse)?;
    let refresh_token = body
        .get("refresh_token")
        .and_then(Value::as_str)
        .filter(|token| !token.is_empty())
        .map(str::to_string);
    Ok(Granted {
        access_token: access_token.to_string(),
        refresh_token,
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
