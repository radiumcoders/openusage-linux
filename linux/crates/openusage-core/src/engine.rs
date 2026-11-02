//! Per-account refresh. A port of `WidgetDataStore` refresh, `RefreshBackoff`, and
//! `ProviderRefreshDeadline`: accounts are read in parallel, each under
//! `refresh::PROVIDER_TIMEOUT_SECONDS`, and a snapshot is written as it lands.
//!
//! Failure backoff and the message the panel shows live in `$XDG_STATE_HOME/openusage/status.json`,
//! one object per account id:
//!
//! ```json
//! {
//!   "claude@8ad3b217": {
//!     "failures": 1,
//!     "message": "Claude sign-in expired. Sign in again with `openusage add claude` to keep reading usage.",
//!     "providerAsked": false,
//!     "retryAt": "2026-07-13T01:45:00.000Z"
//!   }
//! }
//! ```
//!
//! `providerAsked` is true when the provider returned a rate limit. A forced refresh still waits
//! that one out. Other failures back off from five minutes, doubling to an hour, and a forced
//! refresh skips that wait. A connection failure retries every interval and does not grow the wait.

use std::collections::{BTreeMap, HashMap};
use std::sync::{mpsc, Arc};
#[cfg(test)]
use std::sync::Mutex;
use std::thread;
use std::time::{Duration, Instant};

use chrono::{DateTime, Duration as ChronoDuration, Utc};
use serde::{Deserialize, Serialize};

mod persist;
use persist::StatusFile;

use crate::account::Account;
use crate::http::{HttpClient, UreqClient};
use crate::kind::ProviderKind;
use crate::log;
use crate::metric::{ProviderSnapshot, ProviderUsage};
use crate::provider::{FetchContext, ProviderState};
use crate::refresh::{
    FAILURE_BACKOFF_BASE_SECONDS, FAILURE_BACKOFF_CAP_SECONDS, INTERVAL_SECONDS, PROVIDER_TIMEOUT_SECONDS,
};
use crate::store::{Settings, SnapshotCache};
use crate::vault::{SecretToolVault, Vault};
use crate::widget::{compact_clock, compact_duration};
use crate::error::ProviderError;

/// How long a repeated failure waits. `failures` is the count after this failure (1 for the first).
pub fn delay_seconds(failures: i64) -> f64 {
    let doublings = (failures - 1).clamp(0, 16) as f64;
    ((FAILURE_BACKOFF_BASE_SECONDS as f64) * 2f64.powf(doublings)).min(FAILURE_BACKOFF_CAP_SECONDS as f64)
}

pub fn format_timestamp(date: DateTime<Utc>) -> String {
    format!("{}.{:03}Z", date.format("%Y-%m-%dT%H:%M:%S"), date.timestamp_subsec_millis())
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountStatus {
    pub failures: i64,
    pub message: String,
    pub provider_asked: bool,
    #[serde(with = "status_time")]
    pub retry_at: DateTime<Utc>,
}

mod status_time {
    use super::*;
    use serde::{Deserializer, Serializer};

    pub fn serialize<S: Serializer>(date: &DateTime<Utc>, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&format_timestamp(*date))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<DateTime<Utc>, D::Error> {
        let text = String::deserialize(deserializer)?;
        DateTime::parse_from_rfc3339(&text)
            .map(|date| date.with_timezone(&Utc))
            .map_err(serde::de::Error::custom)
    }
}

enum FailureKind {
    Unreachable,
    Refused,
    RateLimited(Option<f64>),
}

impl AccountStatus {
    fn after(kind: FailureKind, message: String, previous: Option<&AccountStatus>, now: DateTime<Utc>) -> Self {
        let previous_failures = previous.map(|status| status.failures).unwrap_or(0);
        match kind {
            FailureKind::Unreachable => Self {
                failures: previous_failures,
                message,
                provider_asked: false,
                retry_at: now + ChronoDuration::seconds(FAILURE_BACKOFF_BASE_SECONDS),
            },
            FailureKind::Refused => {
                let failures = previous_failures + 1;
                Self { failures, message, provider_asked: false, retry_at: add_seconds(now, delay_seconds(failures)) }
            }
            FailureKind::RateLimited(retry_after) => {
                let failures = previous_failures + 1;
                let wait = retry_after.unwrap_or_else(|| delay_seconds(failures));
                Self { failures, message, provider_asked: true, retry_at: add_seconds(now, wait) }
            }
        }
    }

    fn holds(&self, now: DateTime<Utc>, forced: bool) -> bool {
        now < self.retry_at && (self.provider_asked || !forced)
    }
}

fn add_seconds(now: DateTime<Utc>, seconds: f64) -> DateTime<Utc> {
    now + ChronoDuration::milliseconds((seconds * 1000.0).round() as i64)
}

fn is_fresh(snapshot: &ProviderSnapshot, now: DateTime<Utc>) -> bool {
    let age = (now - snapshot.refreshed_at).num_milliseconds() as f64 / 1000.0;
    (0.0..INTERVAL_SECONDS as f64).contains(&age)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefreshOutcome {
    Refreshed,
    Failed,
    CacheHit,
    Skipped,
    BackedOff,
}

pub struct RefreshReport {
    pub account: Account,
    pub outcome: RefreshOutcome,
}

/// The refresh error, rate-limit wait, or staleness hint for each account. This is the map
/// `dashboard::accounts` takes. Accounts with nothing to say are absent.
pub fn load_problems(now: DateTime<Utc>) -> BTreeMap<String, String> {
    problem_map(&Settings::open().accounts(), &SnapshotCache::open().load(), &StatusFile::open().load(), now)
}

pub fn load_statuses() -> BTreeMap<String, AccountStatus> {
    StatusFile::open().load()
}

pub fn forget_status(card_id: &str) {
    StatusFile::open().remove(card_id);
}

pub fn problem_map(
    accounts: &[Account],
    snapshots: &BTreeMap<String, ProviderSnapshot>,
    statuses: &BTreeMap<String, AccountStatus>,
    now: DateTime<Utc>,
) -> BTreeMap<String, String> {
    accounts
        .iter()
        .filter_map(|account| {
            let id = account.card_id();
            problem_text(account, snapshots.get(&id), statuses.get(&id), now).map(|text| (id, text))
        })
        .collect()
}

fn problem_text(
    account: &Account,
    snapshot: Option<&ProviderSnapshot>,
    status: Option<&AccountStatus>,
    now: DateTime<Utc>,
) -> Option<String> {
    if let Some(text) = notice(snapshot, status) {
        return Some(text);
    }
    staleness_hint(account, snapshot, status, now)
}

fn notice(snapshot: Option<&ProviderSnapshot>, status: Option<&AccountStatus>) -> Option<String> {
    match status {
        Some(status) if !status.provider_asked => Some(status.message.clone()),
        Some(_) => None,
        None => snapshot.and_then(|snapshot| snapshot.notice.clone()),
    }
}

fn staleness_hint(
    account: &Account,
    snapshot: Option<&ProviderSnapshot>,
    status: Option<&AccountStatus>,
    now: DateTime<Utc>,
) -> Option<String> {
    if let Some(status) = status {
        if status.provider_asked && now < status.retry_at {
            return Some(format!(
                "{} asked OpenUsage to wait. Retrying at {}.",
                account.kind.display_name(),
                compact_clock(status.retry_at)
            ));
        }
    }
    if let Some(snapshot) = snapshot {
        if snapshot.is_stale(now) {
            let age = (now - snapshot.refreshed_at).num_milliseconds() as f64 / 1000.0;
            if let Some(duration) = compact_duration(age) {
                return Some(format!("Last updated {duration} ago"));
            }
        }
    }
    status.filter(|status| status.provider_asked).map(|_| "Waiting to retry after a rate limit.".to_string())
}

pub struct Engine {
    settings: Settings,
    cache: SnapshotCache,
    status: StatusFile,
    http: Arc<dyn HttpClient>,
    vault: Arc<dyn Vault>,
    accounts: Vec<Account>,
    snapshots: BTreeMap<String, ProviderSnapshot>,
    statuses: BTreeMap<String, AccountStatus>,
    states: HashMap<String, ProviderState>,
    timeout: Duration,
}

impl Engine {
    pub fn open() -> Self {
        let mut engine = Self {
            settings: Settings::open(),
            cache: SnapshotCache::open(),
            status: StatusFile::open(),
            http: Arc::new(UreqClient::new()),
            vault: Arc::new(SecretToolVault::default()),
            accounts: Vec::new(),
            snapshots: BTreeMap::new(),
            statuses: BTreeMap::new(),
            states: HashMap::new(),
            timeout: Duration::from_secs(PROVIDER_TIMEOUT_SECONDS),
        };
        engine.reload();
        engine
    }

    pub fn accounts(&self) -> &[Account] {
        &self.accounts
    }

    pub fn snapshots(&self) -> &BTreeMap<String, ProviderSnapshot> {
        &self.snapshots
    }

    pub fn statuses(&self) -> &BTreeMap<String, AccountStatus> {
        &self.statuses
    }

    pub fn problems(&self, now: DateTime<Utc>) -> BTreeMap<String, String> {
        problem_map(&self.accounts, &self.snapshots, &self.statuses, now)
    }

    /// Re-reads accounts, snapshots, and status written by another process. Provider state for an
    /// account that is still configured is kept.
    pub fn reload(&mut self) {
        self.accounts = self.settings.accounts();
        self.snapshots = self.cache.load();
        let ids: Vec<String> = self.accounts.iter().map(|account| account.card_id()).collect();
        self.states.retain(|id, _| ids.iter().any(|keep| keep == id));
        self.status.retain(&ids);
        self.statuses = self.status.load();
    }

    pub fn accounts_changed_on_disk(&self) -> bool {
        fingerprint(&self.settings.accounts()) != fingerprint(&self.accounts)
    }

    pub fn refresh(&mut self, force: bool) -> Vec<RefreshReport> {
        self.refresh_at(force, Utc::now())
    }

    pub fn refresh_at(&mut self, force: bool, now: DateTime<Utc>) -> Vec<RefreshReport> {
        self.reload();
        let accounts = self.accounts.clone();
        let started = Instant::now();
        log::notice("refresh", &format!("batch start ({} providers, force={force})", accounts.len()));
        let mut outcomes = vec![RefreshOutcome::Skipped; accounts.len()];
        let mut jobs = Vec::new();
        for (index, account) in accounts.iter().enumerate() {
            let id = account.card_id();
            if !force {
                if self.snapshots.get(&id).is_some_and(|snapshot| is_fresh(snapshot, now)) {
                    log::debug("refresh", &format!("cache hit {id}"));
                    outcomes[index] = RefreshOutcome::CacheHit;
                    continue;
                }
                log::debug("refresh", &format!("cache miss {id}"));
            }
            if let Some(status) = self.statuses.get(&id) {
                if status.holds(now, force) {
                    let left = (status.retry_at - now).num_seconds();
                    log::debug("refresh", &format!("backoff skip {id} ({left}s left)"));
                    outcomes[index] = RefreshOutcome::BackedOff;
                    continue;
                }
            }
            jobs.push(Job { index, account: account.clone(), state: self.states.get(&id).cloned().unwrap_or_default() });
        }
        if !jobs.is_empty() {
            self.fetch_jobs(jobs, force, now, &mut outcomes);
        }
        let count = |outcome| outcomes.iter().filter(|item| **item == outcome).count();
        log::notice(
            "refresh",
            &format!(
                "batch end ({}ms, {} ok / {} failed / {} cached / {} backed off)",
                started.elapsed().as_millis(),
                count(RefreshOutcome::Refreshed),
                count(RefreshOutcome::Failed),
                count(RefreshOutcome::CacheHit),
                count(RefreshOutcome::BackedOff)
            ),
        );
        accounts.into_iter().zip(outcomes).map(|(account, outcome)| RefreshReport { account, outcome }).collect()
    }

    fn fetch_jobs(&mut self, jobs: Vec<Job>, force: bool, now: DateTime<Utc>, outcomes: &mut [RefreshOutcome]) {
        let (tx, rx) = mpsc::channel();
        let mut pending = HashMap::new();
        for job in jobs {
            pending.insert(job.account.card_id(), job.index);
            let tx = tx.clone();
            let http = Arc::clone(&self.http);
            let vault = Arc::clone(&self.vault);
            thread::spawn(move || {
                let started = Instant::now();
                let mut state = job.state;
                let result = provider_fetch(&job.account, &mut state, http.as_ref(), vault.as_ref(), now, force);
                let _ = tx.send(FetchDone {
                    index: job.index,
                    id: job.account.card_id(),
                    kind: job.account.kind,
                    state,
                    result,
                    elapsed: started.elapsed(),
                });
            });
        }
        drop(tx);
        let deadline = Instant::now() + self.timeout;
        let mut disconnected = false;
        while !pending.is_empty() {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            match rx.recv_timeout(left) {
                Ok(done) => {
                    pending.remove(&done.id);
                    self.apply_done(done, now, outcomes);
                }
                Err(mpsc::RecvTimeoutError::Timeout) => break,
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    disconnected = true;
                    break;
                }
            }
        }
        let early = disconnected && Instant::now() < deadline;
        let message = if early {
            "The refresh stopped before it finished.".to_string()
        } else {
            format!("Refresh timed out after {}s", self.timeout.as_secs())
        };
        for (id, index) in pending {
            self.fail(&id, FailureKind::Refused, message.clone(), now, outcomes, index);
        }
    }

    fn apply_done(&mut self, done: FetchDone, now: DateTime<Utc>, outcomes: &mut [RefreshOutcome]) {
        if done.elapsed >= Duration::from_secs(10) {
            log::warn(
                "refresh",
                &format!("{} slow refresh ({}ms, threshold=10000ms)", done.id, done.elapsed.as_millis()),
            );
        }
        match done.result {
            Ok(usage) => {
                let snapshot = ProviderSnapshot {
                    provider_id: done.id.clone(),
                    display_name: done.kind.display_name().to_string(),
                    plan: usage.plan,
                    lines: usage.lines,
                    notice: usage.notice,
                    refreshed_at: now,
                };
                self.cache.store(&done.id, &snapshot);
                self.snapshots.insert(done.id.clone(), snapshot);
                self.status.remove(&done.id);
                self.statuses.remove(&done.id);
                self.states.insert(done.id.clone(), done.state);
                log::info("refresh", &format!("{} ok ({}ms)", done.id, done.elapsed.as_millis()));
                outcomes[done.index] = RefreshOutcome::Refreshed;
            }
            Err(error) => {
                let (kind, message) = classify(&error);
                self.states.insert(done.id.clone(), done.state);
                self.fail(&done.id, kind, message, now, outcomes, done.index);
            }
        }
    }

    fn fail(
        &mut self,
        id: &str,
        kind: FailureKind,
        message: String,
        now: DateTime<Utc>,
        outcomes: &mut [RefreshOutcome],
        index: usize,
    ) {
        let previous = self.statuses.get(id).cloned();
        let status = AccountStatus::after(kind, message, previous.as_ref(), now);
        let wait = (status.retry_at - now).num_seconds().max(0);
        log::warn("refresh", &format!("{id} failed: {} Next try in {wait}s.", status.message));
        self.status.upsert(id, &status);
        self.statuses.insert(id.to_string(), status);
        outcomes[index] = RefreshOutcome::Failed;
    }
}

struct Job {
    index: usize,
    account: Account,
    state: ProviderState,
}

struct FetchDone {
    index: usize,
    id: String,
    kind: ProviderKind,
    state: ProviderState,
    result: Result<ProviderUsage, ProviderError>,
    elapsed: Duration,
}

fn classify(error: &ProviderError) -> (FailureKind, String) {
    let kind = match error {
        ProviderError::RateLimited { retry_after_seconds } => FailureKind::RateLimited(*retry_after_seconds),
        ProviderError::ConnectionFailed => FailureKind::Unreachable,
        _ => FailureKind::Refused,
    };
    (kind, error.to_string())
}

fn provider_fetch(
    account: &Account,
    state: &mut ProviderState,
    http: &dyn HttpClient,
    vault: &dyn Vault,
    now: DateTime<Utc>,
    force: bool,
) -> Result<ProviderUsage, ProviderError> {
    #[cfg(test)]
    if let Some(hook) = TEST_HOOK.lock().unwrap().clone() {
        return hook(account, state);
    }
    let provider = crate::providers::provider(account.kind);
    let mut ctx = FetchContext { account, http, vault, state, now, force };
    provider.fetch(&mut ctx)
}

fn fingerprint(accounts: &[Account]) -> String {
    accounts
        .iter()
        .map(|account| {
            format!(
                "{}|{}|{}|{}",
                account.id,
                account.label,
                account.organization.as_deref().unwrap_or(""),
                account.hub.as_ref().map(ToString::to_string).unwrap_or_default()
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
static TEST_HOOK: Mutex<Option<Arc<dyn Fn(&Account, &mut ProviderState) -> Result<ProviderUsage, ProviderError> + Send + Sync>>> =
    Mutex::new(None);

#[cfg(test)]
mod tests;
