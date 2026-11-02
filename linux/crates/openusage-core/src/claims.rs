//! Claiming a rate-limit reset: a port of `Services/ResetClaimService.swift` and the claim halves of
//! `AccountProviderRuntime+ClaudeResets.swift` / `+CodexResets.swift`. The panel offers a Claim button on
//! the Limit Resets row when `offer` returns one; `claim` runs the claim (blocking) and the caller then
//! asks the daemon to refresh (`daemon::signal_running(SIGUSR1)`).

use chrono::{DateTime, Utc};

use crate::account::Account;
use crate::http::HttpClient;
use crate::metric::ProviderSnapshot;
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

/// Whether `account` has a reset to claim, judged from its last snapshot. Not implemented yet: never offers.
pub fn offer(_account: &Account, _snapshot: Option<&ProviderSnapshot>) -> Option<ClaimOffer> {
    None
}

/// Claims one reset (the one expiring soonest, or `expiring_at`). Blocking. Not implemented yet.
pub fn claim(
    _account: &Account,
    _vault: &dyn Vault,
    _http: &dyn HttpClient,
    _expiring_at: Option<DateTime<Utc>>,
) -> ClaimOutcome {
    ClaimOutcome::Failed(ClaimFailure::Refused)
}
