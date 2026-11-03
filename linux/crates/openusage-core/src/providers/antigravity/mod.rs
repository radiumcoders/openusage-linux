//! Antigravity quota. Ports `Providers/Antigravity/` and `AntigravityLogin`.

mod client;
mod login;
mod mapper;

use chrono::{DateTime, Duration, Utc};

use crate::account::{Credential, OAuthTokens};
use crate::error::ProviderError;
use crate::kind::ProviderKind;
use crate::log;
use crate::metric::ProviderUsage;
use crate::provider::{FetchContext, Login, LoginMethod, Provider};
use crate::refresh::SECONDARY_READ_INTERVAL_SECONDS;

use client::QuotaError;

const CODE_ASSIST: &str = "antigravity_code_assist";
const EARLY_HOLD: &str = "antigravity_early_renewal_not_before";

pub fn provider() -> Box<dyn Provider> {
    Box::new(AntigravityProvider)
}

pub fn login(_method: &LoginMethod) -> Box<dyn Login> {
    Box::new(login::AntigravityLogin)
}

struct AntigravityProvider;

impl Provider for AntigravityProvider {
    fn fetch(&self, ctx: &mut FetchContext) -> Result<ProviderUsage, ProviderError> {
        let tokens = fresh_tokens(ctx)?;
        match usage(ctx, &tokens.access_token) {
            Ok(usage) => Ok(usage),
            Err(QuotaError::Refused) => {
                log::debug(
                    "plugin:antigravity",
                    &format!(
                        "{} usage read unauthorized; renewing the sign-in once",
                        ctx.account.card_id()
                    ),
                );
                let renewed = renew(ctx, &tokens)?;
                match usage(ctx, &renewed.access_token) {
                    Ok(usage) => Ok(usage),
                    Err(QuotaError::Refused) => {
                        Err(ProviderError::SignInExpired(ProviderKind::Antigravity))
                    }
                    Err(QuotaError::Failed(error)) => Err(error),
                }
            }
            Err(QuotaError::Failed(error)) => Err(error),
        }
    }
}

fn usage(ctx: &mut FetchContext<'_>, token: &str) -> Result<ProviderUsage, QuotaError> {
    if ctx.state.is_due(
        CODE_ASSIST,
        SECONDARY_READ_INTERVAL_SECONDS,
        ctx.now,
        ctx.force,
    ) {
        if let Ok(body) = client::load_code_assist(ctx.http, token) {
            if let Ok(text) = String::from_utf8(body) {
                ctx.state.set(CODE_ASSIST, &text);
                ctx.state.mark_read(CODE_ASSIST, ctx.now);
            }
        }
    }
    let cached = ctx.state.get::<String>(CODE_ASSIST);
    let project = cached
        .as_deref()
        .and_then(|body| mapper::parse_project(body.as_bytes()));
    let bodies = client::quota_summary(ctx.http, token, project.as_deref())?;
    let lines = mapper::parse_quota_summaries(&bodies)
        .ok_or(QuotaError::Failed(ProviderError::InvalidResponse))?;
    let plan = cached
        .as_deref()
        .and_then(|body| mapper::parse_plan(body.as_bytes()));
    Ok(ProviderUsage {
        breakdowns: Default::default(),
        plan,
        lines,
        notice: None,
    })
}

fn fresh_tokens(ctx: &mut FetchContext<'_>) -> Result<OAuthTokens, ProviderError> {
    let Credential::OAuth(tokens) = ctx.credential()? else {
        return Err(ProviderError::InvalidResponse);
    };
    let Some(expires_at) = tokens.expires_at else {
        return Ok(tokens);
    };
    if !tokens.needs_refresh(ctx.now) || early_held(ctx, expires_at) {
        return Ok(tokens);
    }
    match renew(ctx, &tokens) {
        Ok(tokens) => Ok(tokens),
        Err(error) if expires_at > ctx.now => {
            hold_early(ctx, &error);
            log::warn(
                "plugin:antigravity",
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

fn renew(ctx: &mut FetchContext<'_>, tokens: &OAuthTokens) -> Result<OAuthTokens, ProviderError> {
    let (_renewal, elsewhere) = ctx.begin_renewal(tokens);
    if let Some(stored) = elsewhere {
        return Ok(stored);
    }
    let Some(refresh_token) = tokens.refresh_token.clone() else {
        return Ok(tokens.clone());
    };
    let granted = client::refresh(ctx.http, &refresh_token, ctx.now)?;
    let mut updated = tokens.clone();
    updated.access_token = granted.access_token;
    if let Some(refresh_token) = granted.refresh_token {
        updated.refresh_token = Some(refresh_token);
    }
    updated.expires_at = granted.expires_at;
    if let Err(error) = ctx.save_credential(&Credential::OAuth(updated.clone())) {
        log::error(
            "plugin:antigravity",
            &format!(
                "{} couldn't save the renewed sign-in; keeping it for this read: {error}",
                ctx.account.card_id()
            ),
        );
    }
    Ok(updated)
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
    if let Some(until) = ctx
        .now
        .checked_add_signed(Duration::milliseconds((*seconds * 1000.0) as i64))
    {
        ctx.state.set(EARLY_HOLD, &until);
    }
}

#[cfg(test)]
mod tests;
