//! The contract every provider port implements. A port lives in `providers/<name>.rs` and mirrors the
//! Swift `<Name>UsageClient` (HTTP) and `<Name>UsageMapper` (response → `MetricLine`s), plus the account's
//! token renewal from `AccountProviderRuntime` and its extensions.

use std::collections::HashMap;

use chrono::{DateTime, Duration, Utc};
use serde::de::DeserializeOwned;
use serde::Serialize;

use crate::account::{Account, Credential, OAuthTokens};
use crate::error::{LoginError, ProviderError};
use crate::http::HttpClient;
use crate::login::LoginPrompter;
use crate::metric::ProviderUsage;
use crate::vault::Vault;

/// State one account's provider keeps between reads in the same process: secondary reads made hourly
/// (plans, reset grants), ledgers, a rate-limit window. JSON values keyed by name; a port picks its keys.
#[derive(Debug, Default, Clone)]
pub struct ProviderState {
    values: HashMap<String, serde_json::Value>,
    read_at: HashMap<String, DateTime<Utc>>,
}

impl ProviderState {
    pub fn get<T: DeserializeOwned>(&self, key: &str) -> Option<T> {
        self.values.get(key).and_then(|value| serde_json::from_value(value.clone()).ok())
    }

    pub fn set<T: Serialize>(&mut self, key: &str, value: &T) {
        if let Ok(value) = serde_json::to_value(value) {
            self.values.insert(key.to_string(), value);
        }
    }

    pub fn remove(&mut self, key: &str) {
        self.values.remove(key);
        self.read_at.remove(key);
    }

    /// Whether a secondary read named `key` is due: never read, forced, or older than `interval`.
    /// Mirrors the Swift `SecondaryRead`.
    pub fn is_due(&self, key: &str, interval_seconds: i64, now: DateTime<Utc>, force: bool) -> bool {
        force || self.read_at.get(key).is_none_or(|at| now - *at >= Duration::seconds(interval_seconds))
    }

    pub fn mark_read(&mut self, key: &str, now: DateTime<Utc>) {
        self.read_at.insert(key.to_string(), now);
    }
}

/// Everything one read of one account needs.
pub struct FetchContext<'a> {
    pub account: &'a Account,
    pub http: &'a dyn HttpClient,
    pub vault: &'a dyn Vault,
    pub state: &'a mut ProviderState,
    pub now: DateTime<Utc>,
    /// A refresh the user asked for (click, `openusage refresh`): secondary reads are due again.
    pub force: bool,
}

impl FetchContext<'_> {
    /// The stored sign-in. A missing one is reported the way the Mac app does: a refused sign-in or key.
    pub fn credential(&self) -> Result<Credential, ProviderError> {
        self.vault.load(self.account.id)?.ok_or_else(|| match self.account.auth_method() {
            crate::kind::AuthMethod::OAuth => ProviderError::SignInExpired(self.account.kind),
            crate::kind::AuthMethod::ApiKey { .. } => {
                ProviderError::KeyRejected(format!("{} rejected the API key.", self.account.kind.display_name()))
            }
            crate::kind::AuthMethod::OrganizationToken => ProviderError::KeyRejected("GitHub rejected the token.".into()),
        })
    }

    pub fn api_key(&self) -> Result<String, ProviderError> {
        match self.credential()? {
            Credential::ApiKey(key) => Ok(key),
            Credential::OAuth(tokens) => Ok(tokens.access_token),
        }
    }

    pub fn oauth(&self) -> Result<OAuthTokens, ProviderError> {
        match self.credential()? {
            Credential::OAuth(tokens) => Ok(tokens),
            Credential::ApiKey(_) => Err(ProviderError::SignInExpired(self.account.kind)),
        }
    }

    /// Saves renewed tokens. A provider that rotates refresh tokens must save before using the new ones.
    pub fn save_credential(&self, credential: &Credential) -> Result<(), ProviderError> {
        self.vault.save(self.account.id, credential)
    }
}

pub trait Provider: Send + Sync {
    /// One read of the account's usage. Renew OAuth tokens here when `needs_refresh` says so (and on a
    /// 401 once), saving them with `ctx.save_credential`.
    fn fetch(&self, ctx: &mut FetchContext) -> Result<ProviderUsage, ProviderError>;
}

/// Which sign-in to run for an account being added.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoginMethod {
    Default,
    /// Copilot organization billing, read with an organization token.
    Organization(String),
}

pub trait Login: Send + Sync {
    /// Runs the sign-in through `prompter` and returns the account's label and credential.
    /// API-key logins check the key with one read before returning it.
    fn login(&self, prompter: &dyn LoginPrompter, http: &dyn HttpClient) -> Result<(String, Credential), LoginError>;
}
