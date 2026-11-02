//! ChatGPT provider. Port of `Sources/OpenUsage/Providers/Codex` and `CodexLogin`.
//!
//! Reset credits are read here; `claims` spends one. The hourly rest of history follows a user refresh
//! (`FetchContext::force`); the Mac app can also force a provider read without rereading history.

#[path = "login.rs"]
mod auth;
pub(crate) mod client;
mod history;
pub(crate) mod mapper;
mod reports;

#[cfg(test)]
mod tests;

use chrono::{DateTime, Utc};

use crate::account::{Credential, OAuthTokens};
use crate::error::ProviderError;
use crate::http::HttpResponse;
use crate::kind::ProviderKind;
use crate::log;
use crate::metric::ProviderUsage;
use crate::provider::{FetchContext, Provider};
use crate::refresh;

pub use auth::login;

const RESET_KEY: &str = "codex.reset_credits";
const EARLY_KEY: &str = "codex.early_renewal_ms";

struct CodexProvider;

pub fn provider() -> Box<dyn Provider> {
    Box::new(CodexProvider)
}

impl Provider for CodexProvider {
    fn fetch(&self, ctx: &mut FetchContext) -> Result<ProviderUsage, ProviderError> {
        fetch(ctx)
    }
}

fn fetch(ctx: &mut FetchContext) -> Result<ProviderUsage, ProviderError> {
    let tokens = ctx.oauth()?;
    let (tokens, response) = authorized(ctx, tokens)?;
    let account_id = tokens
        .extras
        .get("chatgpt_account_id")
        .map(String::as_str)
        .filter(|id| !id.is_empty());
    let reset_credits = reset_credits(ctx, &tokens.access_token, account_id, &response);
    let mut mapped = mapper::map_usage(&response, reset_credits.as_ref(), ctx.now)?;
    let mut breakdowns = Default::default();
    history::attach(
        ctx,
        &tokens.access_token,
        account_id,
        &response.body,
        &mut mapped.lines,
        &mut breakdowns,
    );
    Ok(ProviderUsage {
        breakdowns,
        plan: mapped.plan,
        lines: mapped.lines,
        notice: None,
    })
}

fn authorized(
    ctx: &mut FetchContext,
    mut tokens: OAuthTokens,
) -> Result<(OAuthTokens, HttpResponse), ProviderError> {
    tokens = fresh_tokens(ctx, tokens)?;
    let account_id = tokens.extras.get("chatgpt_account_id").cloned();
    let mut response = ctx.http.send(client::usage_request(
        &tokens.access_token,
        account_id.as_deref(),
    ))?;
    if response.status == 401
        && tokens
            .refresh_token
            .as_ref()
            .is_some_and(|token| !token.is_empty())
    {
        log::debug(
            "auth",
            &format!(
                "{} usage read unauthorized; renewing the sign-in once",
                ctx.account.card_id()
            ),
        );
        tokens = renew(ctx, tokens)?;
        let account_id = tokens.extras.get("chatgpt_account_id").cloned();
        response = ctx.http.send(client::usage_request(
            &tokens.access_token,
            account_id.as_deref(),
        ))?;
    }
    Ok((tokens, response))
}

fn fresh_tokens(ctx: &mut FetchContext, tokens: OAuthTokens) -> Result<OAuthTokens, ProviderError> {
    let Some(expires_at) = tokens.expires_at else {
        return Ok(tokens);
    };
    if !tokens.needs_refresh(ctx.now) || early_renewal_held(ctx, expires_at) {
        return Ok(tokens);
    }
    match renew(ctx, tokens.clone()) {
        Ok(updated) => Ok(updated),
        Err(error) if expires_at > ctx.now => {
            hold_early_renewal(ctx, &error);
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

fn renew(ctx: &mut FetchContext, tokens: OAuthTokens) -> Result<OAuthTokens, ProviderError> {
    let refresh_token = tokens
        .refresh_token
        .clone()
        .filter(|token| !token.is_empty())
        .ok_or(ProviderError::SignInExpired(ProviderKind::Codex))?;
    let grant = client::refresh(&refresh_token, ctx.http)?;
    let mut updated = tokens;
    updated.access_token = grant.access_token;
    if let Some(refresh_token) = grant.refresh_token {
        updated.refresh_token = Some(refresh_token);
    }
    updated.expires_at = grant.expires_at;
    if let Err(error) = ctx.save_credential(&Credential::OAuth(updated.clone())) {
        log::error(
            "auth",
            &format!(
                "{} couldn't save the renewed sign-in; keeping it and retrying: {error}",
                ctx.account.card_id()
            ),
        );
    }
    Ok(updated)
}

fn early_renewal_held(ctx: &FetchContext, expires_at: DateTime<Utc>) -> bool {
    expires_at > ctx.now
        && ctx
            .state
            .get::<i64>(EARLY_KEY)
            .is_some_and(|until| ctx.now.timestamp_millis() < until)
}

fn hold_early_renewal(ctx: &mut FetchContext, error: &ProviderError) {
    let ProviderError::RateLimited {
        retry_after_seconds: Some(wait),
    } = error
    else {
        return;
    };
    ctx.state.set(
        EARLY_KEY,
        &(ctx.now.timestamp_millis() + (wait * 1000.0).round() as i64),
    );
}

/// The dedicated list is hourly. A failure keeps the previous body and still counts as the hour's read.
fn reset_credits(
    ctx: &mut FetchContext,
    access_token: &str,
    account_id: Option<&str>,
    usage: &HttpResponse,
) -> Option<HttpResponse> {
    if !usage.is_success() {
        return None;
    }
    if !ctx.state.is_due(
        RESET_KEY,
        refresh::SECONDARY_READ_INTERVAL_SECONDS,
        ctx.now,
        ctx.force,
    ) {
        return cached_reset(ctx);
    }
    match ctx
        .http
        .send(client::reset_credits_request(access_token, account_id))
    {
        Ok(response) if response.is_success() => {
            if let Ok(body) = String::from_utf8(response.body.clone()) {
                ctx.state.set(RESET_KEY, &body);
            }
            ctx.state.mark_read(RESET_KEY, ctx.now);
            Some(response)
        }
        Ok(_) => {
            log::warn(
                "plugin:codex",
                &format!(
                    "{} reset-credit fetch failed; retrying in an hour",
                    ctx.account.card_id()
                ),
            );
            ctx.state.mark_read(RESET_KEY, ctx.now);
            cached_reset(ctx)
        }
        Err(error) => {
            log::warn(
                "plugin:codex",
                &format!(
                    "{} reset-credit fetch failed; retrying in an hour: {error}",
                    ctx.account.card_id()
                ),
            );
            ctx.state.mark_read(RESET_KEY, ctx.now);
            cached_reset(ctx)
        }
    }
}

fn cached_reset(ctx: &FetchContext) -> Option<HttpResponse> {
    let body = ctx.state.get::<String>(RESET_KEY)?;
    Some(HttpResponse {
        status: 200,
        headers: Default::default(),
        body: body.into_bytes(),
    })
}
