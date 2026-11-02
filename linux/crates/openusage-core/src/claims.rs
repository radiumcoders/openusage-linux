//! Claiming a rate-limit reset: a port of `Services/ResetClaimService.swift` and the claim halves of
//! `AccountProviderRuntime+ClaudeResets.swift` / `+CodexResets.swift`. The panel offers a Claim button on
//! the Limit Resets row when `offer` returns one; `claim` runs the claim (blocking) and the caller then
//! asks the daemon to refresh (`daemon::signal_running(SIGUSR1)`).
//!
//! The panel process does not share the daemon's provider state, so a claim reads grant or credit status
//! again instead of trusting a cached check. An answer that never arrived is retried with the same
//! request id for that account, in this process.

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};

use chrono::{DateTime, Utc};
use uuid::Uuid;

use crate::account::{Account, Credential, OAuthTokens};
use crate::error::ProviderError;
use crate::http::{HttpClient, HttpRequest, HttpResponse};
use crate::kind::ProviderKind;
use crate::log;
use crate::metric::{MetricLine, ProviderSnapshot};
use crate::providers::{claude, codex};
use crate::vault::Vault;

#[derive(Debug, Clone, PartialEq)]
pub enum ClaimFailure {
    SignInExpired,
    NotPermitted,
    Connection,
    Refused,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ClaimOutcome {
    Success,
    NothingToReset,
    NoCredit,
    Unconfirmed,
    UnavailableNow { until: Option<DateTime<Utc>> },
    Failed(ClaimFailure),
}

impl ClaimOutcome {
    /// The Mac app's wording, with the Linux command where it points at its window.
    pub fn message(&self, now: DateTime<Utc>) -> String {
        match self {
            Self::Success => "Reset claimed. Enjoy!".into(),
            Self::NothingToReset => "Your usage doesn't need a reset yet".into(),
            Self::NoCredit => "That reset is no longer available".into(),
            Self::Unconfirmed => "Your reset may still be going through. Check again in a minute.".into(),
            Self::UnavailableNow { until } => {
                let wait = until.and_then(|until| crate::widget::compact_duration((until - now).num_seconds() as f64));
                match wait {
                    Some(wait) if (*until).is_some_and(|until| (until - now).num_seconds() > 300) => {
                        format!("Resets can't be used right now. Try again in {wait}.")
                    }
                    Some(_) => "Resets can't be used right now. Try again soon.".into(),
                    None => "Resets can't be used right now.".into(),
                }
            }
            Self::Failed(ClaimFailure::SignInExpired) => {
                "Your sign-in expired. Sign in again with `openusage add`.".into()
            }
            Self::Failed(ClaimFailure::NotPermitted) => {
                "This sign-in can't use resets. Use the reset in Claude Code instead.".into()
            }
            Self::Failed(ClaimFailure::Connection) => {
                "Couldn't connect. Check your internet connection and try again.".into()
            }
            Self::Failed(ClaimFailure::Refused) => "Couldn't reset usage. Please try again.".into(),
        }
    }
}

/// A reset the account can claim now.
#[derive(Debug, Clone, PartialEq)]
pub struct ClaimOffer {
    /// Extra text for the confirmation ("Resets your weekly limit"), when the provider gives one.
    pub confirm_detail: Option<String>,
    /// When each available reset expires, soonest first.
    pub expiries: Vec<DateTime<Utc>>,
}

/// Whether `account` has a reset to claim, judged from its last snapshot. No network.
pub fn offer(account: &Account, snapshot: Option<&ProviderSnapshot>) -> Option<ClaimOffer> {
    if !matches!(account.kind, ProviderKind::Claude | ProviderKind::Codex) {
        return None;
    }
    let MetricLine::Values { values, expiries_at, detail, .. } = snapshot?.line("Rate Limit Resets")? else {
        return None;
    };
    let available = values.iter().find(|value| value.label.as_deref() == Some("available"))?;
    if available.number < 1.0 {
        return None;
    }
    if account.kind == ProviderKind::Claude && detail.is_none() {
        return None;
    }
    let mut expiries = expiries_at.clone();
    expiries.sort();
    Some(ClaimOffer {
        confirm_detail: detail.clone().filter(|text| !text.is_empty()),
        expiries,
    })
}

/// Claims one reset (the one expiring at `expiring_at`, for ChatGPT). Blocking.
pub fn claim(
    account: &Account,
    vault: &dyn Vault,
    http: &dyn HttpClient,
    expiring_at: Option<DateTime<Utc>>,
) -> ClaimOutcome {
    match account.kind {
        ProviderKind::Claude => claim_claude(account, vault, http),
        ProviderKind::Codex => claim_codex(account, vault, http, expiring_at),
        _ => ClaimOutcome::Failed(ClaimFailure::Refused),
    }
}

struct ClaudePending {
    grant_id: String,
    request_id: String,
}

struct CodexPending {
    expiry: DateTime<Utc>,
    request_id: String,
    credit_id: String,
}

static CLAUDE_PENDING: LazyLock<Mutex<HashMap<Uuid, ClaudePending>>> = LazyLock::new(|| Mutex::new(HashMap::new()));
static CODEX_PENDING: LazyLock<Mutex<HashMap<Uuid, CodexPending>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

fn claim_claude(account: &Account, vault: &dyn Vault, http: &dyn HttpClient) -> ClaimOutcome {
    let tag = "plugin:claude";
    let mut tokens = match load_oauth(vault, account) {
        Ok(tokens) => tokens,
        Err(outcome) => return outcome,
    };
    tokens = match prepare(account, vault, tokens, |tokens| renew_claude(http, tokens)) {
        Ok(tokens) => tokens,
        Err(outcome) => {
            log::error(tag, &format!("{} reset claim: couldn't prepare the claim", account.card_id()));
            return outcome;
        }
    };
    let (mut tokens, response) = match authorize(account, vault, http, tokens, |tokens| {
        claude::client::grants_request(&tokens.access_token)
    }, renew_claude)
    {
        Ok(pair) => pair,
        Err(outcome) => return outcome,
    };
    let status = match claude::mapper::map_reset_grants(&response) {
        Ok(status) => status,
        Err(error) => return as_outcome(error),
    };
    let Some(grant_id) = status.as_ref().and_then(claude::mapper::claimable_id).map(str::to_string) else {
        claude_lock().remove(&account.id);
        log::warn(tag, &format!("{} reset claim: no claimable grant in the fresh reset-grants check", account.card_id()));
        return ClaimOutcome::NoCredit;
    };
    if !grant_id_ok(&grant_id) {
        log::error(tag, &format!("{} reset claim: refusing a malformed grant id", account.card_id()));
        return ClaimOutcome::Failed(ClaimFailure::Refused);
    }
    let request_id = {
        let pending = claude_lock();
        match pending.get(&account.id) {
            Some(pending) if pending.grant_id == grant_id && request_id_ok(&pending.request_id) => pending.request_id.clone(),
            _ => Uuid::new_v4().to_string(),
        }
    };
    if !request_id_ok(&request_id) {
        log::error(tag, &format!("{} reset claim: refusing a malformed request id", account.card_id()));
        return ClaimOutcome::Failed(ClaimFailure::Refused);
    }
    let organization = match organization_uuid(account, vault, http, &mut tokens) {
        Ok(uuid) => uuid,
        Err(outcome) => {
            log::error(tag, &format!("{} reset claim: couldn't prepare the claim", account.card_id()));
            return outcome;
        }
    };
    claude_lock().insert(account.id, ClaudePending { grant_id: grant_id.clone(), request_id: request_id.clone() });
    let response = match authorize(account, vault, http, tokens, |tokens| {
        claude::client::claim_request(&tokens.access_token, &organization, &grant_id, &request_id)
    }, renew_claude)
    {
        Ok((_, response)) => response,
        Err(outcome) => return outcome,
    };
    settle_claude(account.id, &response);
    let outcome = claude::mapper::reset_claim_outcome(&response);
    log_outcome(tag, &account.card_id(), &outcome, response.status);
    outcome
}

fn claim_codex(
    account: &Account,
    vault: &dyn Vault,
    http: &dyn HttpClient,
    expiring_at: Option<DateTime<Utc>>,
) -> ClaimOutcome {
    let tag = "plugin:codex";
    let Some(expiring_at) = expiring_at else {
        return ClaimOutcome::NoCredit;
    };
    let mut tokens = match load_oauth(vault, account) {
        Ok(tokens) => tokens,
        Err(outcome) => return outcome,
    };
    tokens = match prepare(account, vault, tokens, |tokens| renew_codex(http, tokens)) {
        Ok(tokens) => tokens,
        Err(outcome) => {
            log::error(tag, &format!("{} reset claim: couldn't prepare the claim", account.card_id()));
            return outcome;
        }
    };
    let replay = codex_lock().get(&account.id).and_then(|pending| {
        close_enough(pending.expiry, expiring_at).then(|| (pending.request_id.clone(), pending.credit_id.clone()))
    });
    let (request_id, credit_id) = if let Some(replay) = replay {
        replay
    } else {
        let account_id = chatgpt_account_id(&tokens);
        let (fresh, response) = match authorize(account, vault, http, tokens, |tokens| {
            codex::client::reset_credits_request(&tokens.access_token, account_id.as_deref())
        }, renew_codex)
        {
            Ok(pair) => pair,
            Err(outcome) => {
                log::error(tag, &format!("{} reset claim: credit list fetch failed", account.card_id()));
                return outcome;
            }
        };
        tokens = fresh;
        if ProviderError::is_auth_failure(&response) {
            log::error(tag, &format!("{} reset claim: credit list fetch rejected (HTTP {})", account.card_id(), response.status));
            return ClaimOutcome::Failed(ClaimFailure::SignInExpired);
        }
        if !response.is_success() || response.json().filter(|value| value.is_object()).is_none() {
            log::error(tag, &format!("{} reset claim: credit list fetch failed (HTTP {})", account.card_id(), response.status));
            return ClaimOutcome::Failed(ClaimFailure::Refused);
        }
        let Some(credit_id) = codex::mapper::reset_credit_id(&response.body, expiring_at) else {
            codex_lock().remove(&account.id);
            log::warn(tag, &format!("{} reset claim: no available credit matches the picked expiry", account.card_id()));
            return ClaimOutcome::NoCredit;
        };
        (Uuid::new_v4().to_string(), credit_id)
    };
    let account_id = chatgpt_account_id(&tokens);
    codex_lock().insert(
        account.id,
        CodexPending { expiry: expiring_at, request_id: request_id.clone(), credit_id: credit_id.clone() },
    );
    let response = match authorize(account, vault, http, tokens, |tokens| {
        codex::client::consume_reset_credit(&tokens.access_token, account_id.as_deref(), &credit_id, &request_id)
    }, renew_codex)
    {
        Ok((_, response)) => response,
        Err(outcome) => {
            log::error(tag, &format!("{} reset claim: consume request failed", account.card_id()));
            return outcome;
        }
    };
    if response.is_success() && response.json().is_some_and(|body| body.get("code").and_then(serde_json::Value::as_str).is_some()) {
        codex_lock().remove(&account.id);
    }
    let outcome = codex::mapper::reset_claim_outcome(&response);
    log_outcome(tag, &account.card_id(), &outcome, response.status);
    outcome
}

fn organization_uuid(
    account: &Account,
    vault: &dyn Vault,
    http: &dyn HttpClient,
    tokens: &mut OAuthTokens,
) -> Result<String, ClaimOutcome> {
    if let Some(uuid) = org_from(tokens) {
        return Ok(uuid);
    }
    let (fresh, response) = authorize(account, vault, http, tokens.clone(), |tokens| {
        claude::client::profile_request(&tokens.access_token)
    }, renew_claude)?;
    *tokens = fresh;
    if ProviderError::is_auth_failure(&response) {
        return Err(ClaimOutcome::Failed(ClaimFailure::SignInExpired));
    }
    let profile = claude::client::decode_profile(&response).map_err(as_outcome)?;
    let Some(uuid) = profile.organization_uuid.clone().filter(|uuid| !uuid.is_empty()) else {
        return Err(ClaimOutcome::Failed(ClaimFailure::Refused));
    };
    if org_from(tokens).is_none() {
        tokens.extras.insert(OAuthTokens::IDENTITY_KEY.to_string(), profile.identity());
        save(account, vault, tokens);
    }
    Ok(uuid)
}

fn settle_claude(id: Uuid, response: &HttpResponse) {
    let succeeded = response.is_success();
    let has_result = succeeded && response.json().is_some_and(|body| body.get("result").is_some());
    if has_result || !(succeeded || response.status >= 500) {
        claude_lock().remove(&id);
    }
}

fn org_from(tokens: &OAuthTokens) -> Option<String> {
    let (_, organization) = tokens.extras.get(OAuthTokens::IDENTITY_KEY)?.split_once('/')?;
    let organization = organization.trim();
    (!organization.is_empty()).then(|| organization.to_string())
}

fn chatgpt_account_id(tokens: &OAuthTokens) -> Option<String> {
    tokens.extras.get("chatgpt_account_id").cloned().filter(|id| !id.is_empty())
}

fn prepare(
    account: &Account,
    vault: &dyn Vault,
    tokens: OAuthTokens,
    mut renew: impl FnMut(&OAuthTokens) -> Result<OAuthTokens, ProviderError>,
) -> Result<OAuthTokens, ClaimOutcome> {
    let now = Utc::now();
    if !tokens.needs_refresh(now) {
        return Ok(tokens);
    }
    match renew(&tokens) {
        Ok(updated) => {
            save(account, vault, &updated);
            Ok(updated)
        }
        Err(error) if tokens.expires_at.is_some_and(|expires| expires > now) => {
            log::warn(
                "auth",
                &format!("{} early token renewal failed; keeping the current token: {error}", account.card_id()),
            );
            Ok(tokens)
        }
        Err(error) => Err(as_outcome(error)),
    }
}

fn authorize(
    account: &Account,
    vault: &dyn Vault,
    http: &dyn HttpClient,
    mut tokens: OAuthTokens,
    mut request: impl FnMut(&OAuthTokens) -> HttpRequest,
    mut renew: impl FnMut(&dyn HttpClient, &OAuthTokens) -> Result<OAuthTokens, ProviderError>,
) -> Result<(OAuthTokens, HttpResponse), ClaimOutcome> {
    let response = http.send(request(&tokens)).map_err(as_outcome)?;
    if response.status != 401 || tokens.refresh_token.as_ref().is_none_or(|token| token.is_empty()) {
        return Ok((tokens, response));
    }
    log::debug(
        "auth",
        &format!("{} usage read unauthorized; renewing the sign-in once", account.card_id()),
    );
    tokens = renew(http, &tokens).map_err(as_outcome)?;
    save(account, vault, &tokens);
    let response = http.send(request(&tokens)).map_err(as_outcome)?;
    Ok((tokens, response))
}

fn renew_claude(http: &dyn HttpClient, tokens: &OAuthTokens) -> Result<OAuthTokens, ProviderError> {
    let Some(refresh_token) = tokens.refresh_token.clone().filter(|token| !token.is_empty()) else {
        return Ok(tokens.clone());
    };
    let grant = claude::client::refresh(&refresh_token, http, Utc::now())?;
    let mut updated = tokens.clone();
    updated.access_token = grant.access_token;
    if let Some(refresh_token) = grant.refresh_token {
        updated.refresh_token = Some(refresh_token);
    }
    updated.expires_at = grant.expires_at;
    Ok(updated)
}

fn renew_codex(http: &dyn HttpClient, tokens: &OAuthTokens) -> Result<OAuthTokens, ProviderError> {
    let Some(refresh_token) = tokens.refresh_token.clone().filter(|token| !token.is_empty()) else {
        return Ok(tokens.clone());
    };
    let grant = codex::client::refresh(&refresh_token, http)?;
    let mut updated = tokens.clone();
    updated.access_token = grant.access_token;
    if let Some(refresh_token) = grant.refresh_token {
        updated.refresh_token = Some(refresh_token);
    }
    updated.expires_at = grant.expires_at;
    Ok(updated)
}

fn load_oauth(vault: &dyn Vault, account: &Account) -> Result<OAuthTokens, ClaimOutcome> {
    match vault.load(account.id) {
        Ok(Some(Credential::OAuth(tokens))) => Ok(tokens),
        Ok(Some(_)) => Err(ClaimOutcome::Failed(ClaimFailure::Refused)),
        Ok(None) | Err(_) => Err(ClaimOutcome::Failed(ClaimFailure::SignInExpired)),
    }
}

fn save(account: &Account, vault: &dyn Vault, tokens: &OAuthTokens) {
    if let Err(error) = vault.save(account.id, &Credential::OAuth(tokens.clone())) {
        log::error(
            "auth",
            &format!("{} couldn't save the renewed sign-in; keeping it and retrying: {error}", account.card_id()),
        );
    }
}

fn as_outcome(error: ProviderError) -> ClaimOutcome {
    ClaimOutcome::Failed(match error {
        ProviderError::SignInExpired(_) | ProviderError::MissingCredential => ClaimFailure::SignInExpired,
        ProviderError::ConnectionFailed => ClaimFailure::Connection,
        _ => ClaimFailure::Refused,
    })
}

fn log_outcome(tag: &str, card: &str, outcome: &ClaimOutcome, status: u16) {
    if matches!(outcome, ClaimOutcome::Success) {
        log::info(tag, &format!("{card} reset claim: claimed (HTTP {status})"));
    } else {
        log::warn(tag, &format!("{card} reset claim: {outcome:?} (HTTP {status})"));
    }
}

fn grant_id_ok(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 40
        && id.bytes().all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_' || byte == b'-')
}

fn request_id_ok(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
}

fn close_enough(left: DateTime<Utc>, right: DateTime<Utc>) -> bool {
    (left - right).num_milliseconds().unsigned_abs() < 1000
}

fn claude_lock() -> std::sync::MutexGuard<'static, HashMap<Uuid, ClaudePending>> {
    CLAUDE_PENDING.lock().unwrap_or_else(|poison| poison.into_inner())
}

fn codex_lock() -> std::sync::MutexGuard<'static, HashMap<Uuid, CodexPending>> {
    CODEX_PENDING.lock().unwrap_or_else(|poison| poison.into_inner())
}

#[cfg(test)]
#[path = "claims_tests.rs"]
mod tests;
