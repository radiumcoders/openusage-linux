//! Account commands other than sign-in: list, remove, rename, refresh, alerts, and the panel.

use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::Command;

use chrono::Utc;
use openusage_core::account::Account;
use openusage_core::daemon;
use openusage_core::engine::{self, Engine, RefreshOutcome};
use openusage_core::store::{Settings, SnapshotCache};
use openusage_core::vault::{SecretToolVault, Vault};

use crate::args::AlertChange;
use crate::notify;
use crate::text;

pub fn accounts() -> i32 {
    let accounts = Settings::open().accounts();
    if accounts.is_empty() {
        text::line("No accounts.");
        return 0;
    }
    let snapshots = SnapshotCache::open().load();
    for account in &accounts {
        let label = if account.label.is_empty() { account.kind.display_name().to_string() } else { account.label.clone() };
        let mut fields = vec![account.card_id(), account.kind.display_name().to_string(), label];
        if let Some(plan) = snapshots.get(&account.card_id()).and_then(|snapshot| snapshot.plan.clone()).filter(|plan| !plan.is_empty()) {
            fields.push(plan);
        }
        text::line(&fields.join("\t"));
    }
    0
}

pub fn remove(token: &str) -> i32 {
    let settings = Settings::open();
    let Some(account) = find(&settings.accounts(), token).cloned() else {
        text::error(&format!("No account {token}."));
        return 2;
    };
    let vault = SecretToolVault::default();
    if let Err(error) = vault.delete(account.id) {
        text::error(&error.to_string());
        return 1;
    }
    if let Err(error) = settings.update_accounts(|accounts| accounts.retain(|item| item.id != account.id)) {
        text::error(&error.to_string());
        return 1;
    }
    if settings.accounts().iter().any(|item| item.id == account.id) {
        text::error("The account could not be removed.");
        return 1;
    }
    let remaining: Vec<String> = settings.accounts().iter().map(Account::card_id).collect();
    SnapshotCache::open().retain(&remaining);
    engine::forget_status(&account.card_id());
    daemon::notify_accounts_changed();
    text::line(&format!("Removed {}.", account.card_id()));
    0
}

pub fn rename(token: &str, name: &str) -> i32 {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        text::error("rename needs a name.");
        return 2;
    }
    let settings = Settings::open();
    let Some(account) = find(&settings.accounts(), token).cloned() else {
        text::error(&format!("No account {token}."));
        return 2;
    };
    let renamed = trimmed.to_string();
    if let Err(error) = settings.update_accounts(|accounts| {
        if let Some(account) = accounts.iter_mut().find(|item| item.id == account.id) {
            account.label = renamed.clone();
        }
    }) {
        text::error(&error.to_string());
        return 1;
    }
    daemon::notify_accounts_changed();
    text::line(&format!("Renamed {} to {trimmed}.", account.card_id()));
    0
}

pub fn refresh() -> i32 {
    let mut engine = Engine::open();
    if engine.accounts().is_empty() {
        text::line("No accounts.");
        return 0;
    }
    let now = Utc::now();
    let reports = engine.refresh_at(true, now);
    let problems = engine.problems(now);
    let mut failed = false;
    for report in &reports {
        if report.outcome == RefreshOutcome::Failed {
            failed = true;
        }
        let detail = match report.outcome {
            RefreshOutcome::Refreshed => "updated".to_string(),
            RefreshOutcome::CacheHit => "cached".to_string(),
            RefreshOutcome::Skipped => "skipped".to_string(),
            RefreshOutcome::Failed | RefreshOutcome::BackedOff => {
                problems.get(&report.account.card_id()).cloned().unwrap_or_else(|| "failed".to_string())
            }
        };
        text::line(&format!("{} ({}): {detail}", report.account.card_id(), report.account.full_name()));
    }
    if failed { 1 } else { 0 }
}

pub fn alerts(change: Option<AlertChange>) -> i32 {
    let settings = Settings::open();
    if let Some(change) = change {
        if let Err(error) = notify::set(&settings, &change.milestones, change.on) {
            text::error(&error.to_string());
            return 1;
        }
    }
    for line in notify::list(&settings) {
        text::line(&line);
    }
    0
}

pub fn panel() -> i32 {
    let path = panel_binary();
    let error = Command::new(&path).arg("--toggle").exec();
    text::error(&format!("could not run {}: {error}", path.display()));
    1
}

fn panel_binary() -> PathBuf {
    if let Ok(current) = std::env::current_exe() {
        if let Some(directory) = current.parent() {
            let sibling = directory.join("openusage-panel");
            if sibling.is_file() {
                return sibling;
            }
        }
    }
    PathBuf::from("openusage-panel")
}

fn find<'a>(accounts: &'a [Account], token: &str) -> Option<&'a Account> {
    let token = token.to_lowercase();
    accounts.iter().find(|account| account.card_id() == token)
}
