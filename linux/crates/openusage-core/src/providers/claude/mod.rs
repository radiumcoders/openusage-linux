//! Claude provider. Port of `Sources/OpenUsage/Providers/Claude` and `ClaudeLogin`.
//!
//! Reset grants are read here; `claims` spends one. The 429 window lives in this account's `ProviderState`
//! for the process; the Mac app also shares one login's window across cards and relaunches.

#[path = "login.rs"]
mod auth;
pub(crate) mod client;
pub(crate) mod mapper;

#[cfg(test)]
mod tests;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::account::{Credential, OAuthTokens};
use crate::error::ProviderError;
use crate::http::HttpRequest;
use crate::kind::ProviderKind;
use crate::log;
use crate::metric::ProviderUsage;
use crate::provider::{FetchContext, Provider};
use crate::refresh;

pub use auth::login;

const PLAN_KEY: &str = "claude.plan";
const GRANTS_KEY: &str = "claude.grants";
const WINDOW_KEY: &str = "claude.rate_window";
const EARLY_KEY: &str = "claude.early_renewal_ms";
const FORCED_GRANTS_SECONDS: i64 = 15 * 60;

struct ClaudeProvider;

pub fn provider() -> Box<dyn Provider> {
    Box::new(ClaudeProvider)
}

impl Provider for ClaudeProvider {
    fn fetch(&self, ctx: &mut FetchContext) -> Result<ProviderUsage, ProviderError> {
        fetch(ctx)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RateWindow {
    until_ms: i64,
    strikes: i32,
}

fn fetch(ctx: &mut FetchContext) -> Result<ProviderUsage, ProviderError> {
    if let Some(wait) = rate_limit_wait(ctx) {
        return Err(ProviderError::RateLimited {
            retry_after_seconds: Some(wait),
        });
    }
    let tokens = ctx.oauth()?;
    let (tokens, response) = authorized_usage(ctx, tokens)?;
    let mut lines = mapper::map_usage(&response)?;
    // Anthropic 429s a grants read sent in the same breath as usage, so a pass still sends usage
    // first and only then the hourly plan and grants reads.
    if ctx.force {
        if grants_due(ctx) {
            read_grants(ctx, &tokens.access_token);
        }
        if profile_due(ctx) {
            read_profile(ctx, &tokens);
        }
    } else if profile_due(ctx) || grants_due(ctx) {
        if profile_due(ctx) {
            read_profile(ctx, &tokens);
        }
        if grants_due(ctx) {
            read_grants(ctx, &tokens.access_token);
        }
    }
    if let Some(status) = ctx
        .state
        .get::<Option<mapper::GrantStatus>>(GRANTS_KEY)
        .flatten()
    {
        lines.push(mapper::resets_line(&status, ctx.now));
    }
    Ok(ProviderUsage {
        breakdowns: Default::default(),
        plan: ctx.state.get(PLAN_KEY),
        lines,
        notice: None,
    })
}

fn profile_due(ctx: &FetchContext) -> bool {
    ctx.state.is_due(
        PLAN_KEY,
        refresh::SECONDARY_READ_INTERVAL_SECONDS,
        ctx.now,
        ctx.force,
    )
}

/// A forced refresh rereads grants only once the last check is fifteen minutes old.
fn grants_due(ctx: &FetchContext) -> bool {
    let interval = if ctx.force {
        FORCED_GRANTS_SECONDS
    } else {
        refresh::SECONDARY_READ_INTERVAL_SECONDS
    };
    ctx.state.is_due(GRANTS_KEY, interval, ctx.now, false)
}

fn authorized_usage(
    ctx: &mut FetchContext,
    mut tokens: OAuthTokens,
) -> Result<(OAuthTokens, crate::http::HttpResponse), ProviderError> {
    tokens = fresh_tokens(ctx, tokens)?;
    let mut response = guarded(ctx, client::usage_request(&tokens.access_token))?;
    if response.status == 401 && tokens.refresh_token.is_some() {
        log::debug(
            "auth",
            &format!(
                "{} usage read unauthorized; renewing the sign-in once",
                ctx.account.card_id()
            ),
        );
        tokens = renew(ctx, tokens)?;
        response = guarded(ctx, client::usage_request(&tokens.access_token))?;
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
        .ok_or(ProviderError::SignInExpired(ProviderKind::Claude))?;
    let grant = client::refresh(&refresh_token, ctx.http, ctx.now)?;
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

fn read_profile(ctx: &mut FetchContext, tokens: &OAuthTokens) {
    let outcome = match guarded(ctx, client::profile_request(&tokens.access_token)) {
        Ok(response) if ProviderError::is_auth_failure(&response) => {
            Err(ProviderError::SignInExpired(ProviderKind::Claude))
        }
        Ok(response) => decode_and_store_profile(ctx, tokens, &response),
        Err(error) => Err(error),
    };
    if let Err(error) = outcome {
        log::warn(
            "plugin:claude",
            &format!("plan lookup failed; retrying in an hour: {error}"),
        );
    }
    ctx.state.mark_read(PLAN_KEY, ctx.now);
}

fn decode_and_store_profile(
    ctx: &mut FetchContext,
    tokens: &OAuthTokens,
    response: &crate::http::HttpResponse,
) -> Result<(), ProviderError> {
    let profile = client::decode_profile(response)?;
    save_identity(ctx, tokens, &profile);
    if let Some(plan) = profile_plan(&profile) {
        ctx.state.set(PLAN_KEY, &plan);
    }
    Ok(())
}

fn profile_plan(profile: &client::Profile) -> Option<String> {
    if !profile.has_organization {
        return None;
    }
    mapper::format_live_plan(
        profile.organization_type.as_deref(),
        profile.rate_limit_tier.as_deref(),
    )
}

fn save_identity(ctx: &mut FetchContext, tokens: &OAuthTokens, profile: &client::Profile) {
    if tokens.extras.contains_key(OAuthTokens::IDENTITY_KEY) {
        return;
    }
    let mut updated = tokens.clone();
    updated
        .extras
        .insert(OAuthTokens::IDENTITY_KEY.to_string(), profile.identity());
    if let Err(error) = ctx.save_credential(&Credential::OAuth(updated)) {
        log::error(
            "auth",
            &format!(
                "{} couldn't save the account identity: {error}",
                ctx.account.card_id()
            ),
        );
    }
}

fn read_grants(ctx: &mut FetchContext, access_token: &str) {
    let card = ctx.account.card_id();
    match guarded(ctx, client::grants_request(access_token)) {
        Ok(response) => match mapper::map_reset_grants(&response) {
            Ok(status) => ctx.state.set(GRANTS_KEY, &status),
            Err(error) => log::warn(
                "plugin:claude",
                &format!("{card} reset grants lookup failed; retrying in an hour: {error}"),
            ),
        },
        Err(error) => log::warn(
            "plugin:claude",
            &format!("{card} reset grants lookup failed; retrying in an hour: {error}"),
        ),
    }
    ctx.state.mark_read(GRANTS_KEY, ctx.now);
}

fn guarded(
    ctx: &mut FetchContext,
    request: HttpRequest,
) -> Result<crate::http::HttpResponse, ProviderError> {
    if let Some(wait) = rate_limit_wait(ctx) {
        return Err(ProviderError::RateLimited {
            retry_after_seconds: Some(wait),
        });
    }
    let response = ctx.http.send(request)?;
    if response.status == 429 {
        let strikes = ctx
            .state
            .get::<RateWindow>(WINDOW_KEY)
            .map(|window| window.strikes)
            .unwrap_or(0)
            + 1;
        let wait = response
            .retry_after_seconds()
            .unwrap_or(0.0)
            .max(backoff_seconds(strikes));
        ctx.state.set(
            WINDOW_KEY,
            &RateWindow {
                until_ms: ctx.now.timestamp_millis() + (wait * 1000.0).round() as i64,
                strikes,
            },
        );
        return Err(ProviderError::RateLimited {
            retry_after_seconds: Some(wait),
        });
    }
    if response.is_success() {
        ctx.state.remove(WINDOW_KEY);
    }
    Ok(response)
}

fn rate_limit_wait(ctx: &FetchContext) -> Option<f64> {
    let window = ctx.state.get::<RateWindow>(WINDOW_KEY)?;
    let remaining = (window.until_ms - ctx.now.timestamp_millis()) as f64 / 1000.0;
    (remaining > 0.0).then_some(remaining)
}

fn backoff_seconds(failures: i32) -> f64 {
    let doublings = (failures - 1).clamp(0, 16);
    let scaled = (refresh::FAILURE_BACKOFF_BASE_SECONDS as f64) * 2_f64.powi(doublings);
    scaled.min(refresh::FAILURE_BACKOFF_CAP_SECONDS as f64)
}
