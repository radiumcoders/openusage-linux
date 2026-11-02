//! Read the dashboard from the shared store. The panel never calls a provider.

use std::collections::BTreeMap;
use std::path::PathBuf;

use chrono::{DateTime, Utc};

use openusage_core::dashboard::{self, DashboardAccount};
use openusage_core::kind::ProviderKind;
use openusage_core::metric::ProviderSnapshot;
use openusage_core::paths;
use openusage_core::refresh::INTERVAL_SECONDS;
use openusage_core::store::{Settings, SnapshotCache};
use openusage_core::widget::{self, DisplayMode};

pub const METER_KEY: &str = "meterStyle";
pub const RESET_KEY: &str = "resetDisplayMode";

pub struct Model {
    pub accounts: Vec<DashboardAccount>,
    pub families: Vec<Family>,
    pub refreshed_at: Option<DateTime<Utc>>,
    pub now: DateTime<Utc>,
    pub absolute_resets: bool,
    pub snapshots: BTreeMap<String, ProviderSnapshot>,
}

pub struct Family {
    pub kind: ProviderKind,
    pub indexes: Vec<usize>,
}

pub fn load() -> Model {
    let settings = Settings::open();
    let mode = display_mode(&settings);
    let absolute_resets = settings.string(RESET_KEY).as_deref() == Some("absolute");
    let stored = settings.accounts();
    let snapshots = SnapshotCache::open().load();
    let now = Utc::now();
    let refreshed_at = snapshots.values().map(|snapshot| snapshot.refreshed_at).max();
    let accounts = dashboard::accounts(&stored, &snapshots, &account_problems(now), mode, now);
    let families = families_of(&accounts);
    Model { accounts, families, refreshed_at, now, absolute_resets, snapshots }
}

pub fn display_mode(settings: &Settings) -> DisplayMode {
    match settings.string(METER_KEY).as_deref() {
        Some("used") => DisplayMode::Used,
        _ => DisplayMode::Remaining,
    }
}

pub fn set_display_mode(mode: DisplayMode) {
    let raw = match mode {
        DisplayMode::Used => "used",
        DisplayMode::Remaining => "remaining",
    };
    if let Err(error) = Settings::open().set_string(METER_KEY, raw) {
        openusage_core::log::error("panel", &format!("could not save meter style: {error}"));
    }
}

pub fn toggle_display_mode() {
    let next = match display_mode(&Settings::open()) {
        DisplayMode::Used => DisplayMode::Remaining,
        DisplayMode::Remaining => DisplayMode::Used,
    };
    set_display_mode(next);
}

pub fn set_absolute_resets(absolute: bool) {
    let raw = if absolute { "absolute" } else { "relative" };
    if let Err(error) = Settings::open().set_string(RESET_KEY, raw) {
        openusage_core::log::error("panel", &format!("could not save reset times: {error}"));
    }
}

pub fn toggle_resets() {
    let absolute = Settings::open().string(RESET_KEY).as_deref() == Some("absolute");
    set_absolute_resets(!absolute);
}

fn families_of(accounts: &[DashboardAccount]) -> Vec<Family> {
    let mut families = Vec::new();
    for (index, account) in accounts.iter().enumerate() {
        let kind = account.account.kind;
        if let Some(family) = families.iter_mut().find(|family: &&mut Family| family.kind == kind) {
            family.indexes.push(index);
        } else {
            families.push(Family { kind, indexes: vec![index] });
        }
    }
    families
}

/// Refresh errors, rate-limit waits, and staleness hints from `status.json`.
pub fn account_problems(now: DateTime<Utc>) -> BTreeMap<String, String> {
    openusage_core::engine::load_problems(now)
}

pub fn watch_dirs() -> Vec<PathBuf> {
    vec![paths::config_dir(), paths::state_dir()]
}

pub fn is_watched_file(name: &str) -> bool {
    matches!(name, "settings.plist" | "snapshots.json" | "status.json")
}

pub fn updated_text(at: Option<DateTime<Utc>>, now: DateTime<Utc>) -> String {
    let Some(at) = at else { return "Not updated yet".into() };
    let seconds = (now - at).num_milliseconds() as f64 / 1000.0;
    if seconds < 45.0 {
        return "Updated just now".into();
    }
    match widget::compact_duration(seconds) {
        Some(text) => format!("Updated {text} ago"),
        None => "Updated just now".into(),
    }
}

pub fn next_text(at: Option<DateTime<Utc>>, now: DateTime<Utc>, refreshing: bool) -> String {
    if refreshing {
        return "Refreshing…".into();
    }
    let Some(at) = at else { return "Refresh".into() };
    let remaining = (at + chrono::Duration::seconds(INTERVAL_SECONDS) - now).num_seconds();
    if remaining <= 0 {
        return "Update due".into();
    }
    if remaining >= 60 {
        let minutes = ((remaining as f64) / 60.0).ceil() as i64;
        format!("Next in {minutes}m")
    } else {
        format!("Next in {remaining}s")
    }
}

/// Ask the `openusage waybar` daemon to refresh now.
pub fn signal_daemon() {
    if !openusage_core::daemon::signal_running(libc::SIGUSR1) {
        openusage_core::log::warn("panel", "no refresh daemon is running");
    }
}

pub fn shows_account_label(account: &DashboardAccount, siblings: usize) -> bool {
    if siblings > 1 {
        return true;
    }
    let provider = account.account.kind.display_name();
    !account.label_is_provider() && !account.account.label.is_empty() && account.account.label != provider
}

trait LabelMatch {
    fn label_is_provider(&self) -> bool;
}

impl LabelMatch for DashboardAccount {
    fn label_is_provider(&self) -> bool {
        self.account.label == self.account.kind.display_name()
    }
}
