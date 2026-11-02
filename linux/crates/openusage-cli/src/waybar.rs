//! Waybar JSON and the long-running producer. Text is the menu-bar strip; the tooltip is Pango for
//! every account. Launch refreshes without forcing, SIGUSR1 forces, SIGUSR2 reloads, and a change
//! to the account file does too. Lines are reprinted every 30 seconds so countdowns move.

use std::collections::BTreeMap;
use std::io::{self, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU8, Ordering};
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use openusage_core::account::Account;
use openusage_core::daemon::PidLock;
use openusage_core::dashboard::{self, DashboardAccount};
use openusage_core::engine::{self, AccountStatus, Engine};
use openusage_core::layout::{self, LayoutState};
use openusage_core::metric::ProviderSnapshot;
use openusage_core::refresh::INTERVAL_SECONDS;
use openusage_core::store::{Settings, SnapshotCache};
use openusage_core::widget::{DisplayMode, Severity, WidgetData, IMMINENT};
use serde_json::{Map, Value};

use crate::notify::Notifier;
use crate::text;

/// nf-md-speedometer. Waybar's usual Nerd Font has it; `--icon` replaces it.
pub const DEFAULT_ICON: &str = "\u{F04C5}";

static WAKE_FD: AtomicI32 = AtomicI32::new(-1);
static RELOAD: AtomicU8 = AtomicU8::new(0);
static STOP: AtomicBool = AtomicBool::new(false);

pub fn run(once: bool, icon: &str) -> i32 {
    if once {
        let settings = Settings::open();
        let accounts = settings.accounts();
        let snapshots = SnapshotCache::open().load();
        let statuses = engine::load_statuses();
        let _ = emit_line(&render(
            &accounts,
            &layout::load_for(&settings, &accounts),
            &snapshots,
            &statuses,
            icon,
            &bar_options(),
            Utc::now(),
        ));
        return 0;
    }
    let _lock = match PidLock::acquire() {
        Ok(lock) => lock,
        Err(message) => {
            text::error(&message);
            return 1;
        }
    };
    let wake = match Wake::install() {
        Ok(wake) => wake,
        Err(message) => {
            text::error(&message);
            return 1;
        }
    };
    let mut engine = Engine::open();
    let mut quotas = Notifier::default();
    if !cycle(&mut engine, &mut quotas, icon, false) {
        return 0;
    }
    let interval = Duration::from_secs(INTERVAL_SECONDS as u64);
    let mut next_refresh = Instant::now() + interval;
    let mut hourly_at = Instant::now() + interval / 2;
    let mut hourly_done = false;
    while !wake.stopped() {
        if let Some(force) = wake
            .take_reload()
            .or_else(|| engine.accounts_changed_on_disk().then_some(false))
        {
            if !cycle(&mut engine, &mut quotas, icon, force) {
                return 0;
            }
            let started = Instant::now();
            next_refresh = started + interval;
            hourly_at = started + interval / 2;
            hourly_done = false;
            continue;
        }
        let now = Instant::now();
        if now >= next_refresh {
            if engine.accounts_changed_on_disk() {
                continue;
            }
            if !cycle(&mut engine, &mut quotas, icon, false) {
                return 0;
            }
            let started = Instant::now();
            next_refresh = started + interval;
            hourly_at = started + interval / 2;
            hourly_done = false;
            continue;
        }
        if !hourly_done && now >= hourly_at {
            // Secondary reads run inside fetch when they are due. Reprint so the countdown moves.
            if !emit_view(&engine, icon) {
                return 0;
            }
            hourly_done = true;
            continue;
        }
        let tick = now + Duration::from_secs(30);
        let hourly_deadline = if hourly_done { next_refresh } else { hourly_at };
        wake.wait_until(tick.min(next_refresh).min(hourly_deadline));
        if wake.stopped() {
            break;
        }
        let woke = Instant::now();
        if wake.reload_pending() || engine.accounts_changed_on_disk() {
            continue;
        }
        if woke < next_refresh && (hourly_done || woke < hourly_at) && !emit_view(&engine, icon) {
            return 0;
        }
    }
    0
}

pub fn request_refresh() -> i32 {
    if openusage_core::daemon::signal_running(libc::SIGUSR1) {
        0
    } else {
        text::error("The Waybar module is not running.");
        1
    }
}

fn cycle(engine: &mut Engine, quotas: &mut Notifier, icon: &str, force: bool) -> bool {
    engine.refresh(force);
    let now = Utc::now();
    quotas.evaluate(engine.accounts(), engine.snapshots(), now);
    emit_view(engine, icon)
}

fn emit_view(engine: &Engine, icon: &str) -> bool {
    emit_line(&render(
        engine.accounts(),
        &layout::load_for(&Settings::open(), engine.accounts()),
        engine.snapshots(),
        engine.statuses(),
        icon,
        &bar_options(),
        Utc::now(),
    ))
}

struct BarOptions {
    mode: DisplayMode,
    absolute_resets: bool,
    limit: usize,
}

fn bar_options() -> BarOptions {
    let settings = Settings::open();
    BarOptions {
        mode: openusage_core::settings::display_mode(&settings),
        absolute_resets: openusage_core::settings::absolute_resets(&settings),
        limit: openusage_core::settings::menu_bar_limit(&settings),
    }
}

fn emit_line(line: &str) -> bool {
    let mut out = io::stdout().lock();
    writeln!(out, "{line}").is_ok() && out.flush().is_ok()
}

fn render(
    accounts: &[Account],
    layout: &LayoutState,
    snapshots: &BTreeMap<String, ProviderSnapshot>,
    statuses: &BTreeMap<String, AccountStatus>,
    icon: &str,
    options: &BarOptions,
    now: DateTime<Utc>,
) -> String {
    let mode = options.mode;
    let absolute = options.absolute_resets;
    let limit = options.limit;
    let problems = engine::problem_map(accounts, snapshots, statuses, now);
    let view = dashboard::accounts(accounts, layout, snapshots, &problems, mode, now);
    let board = dashboard::PriorityBoard::make(&view, now);
    let badges = dashboard::badges(accounts);
    let (items, overflow) = dashboard::strip(&board, limit, &badges);
    let failed = accounts.iter().any(|account| {
        statuses
            .get(&account.card_id())
            .is_some_and(|status| !status.provider_asked)
    });
    let stale = accounts.iter().any(|account| {
        snapshots
            .get(&account.card_id())
            .is_some_and(|snapshot| snapshot.is_stale(now))
    });
    let (percentage, severity) = module_meter(&view);
    let classes = classes(accounts.is_empty(), severity, failed, stale);
    let tooltip = if accounts.is_empty() {
        "<b>OpenUsage</b>\nNo accounts yet. Add one in a terminal:\nopenusage add &lt;provider&gt;"
            .to_string()
    } else {
        view.iter()
            .map(|account| account_markup(account, absolute, now))
            .collect::<Vec<_>>()
            .join("\n\n")
    };
    let mut payload = Map::new();
    payload.insert("alt".into(), Value::String(classes.join(" ")));
    payload.insert(
        "class".into(),
        Value::Array(
            classes
                .iter()
                .map(|name| Value::String((*name).to_string()))
                .collect(),
        ),
    );
    payload.insert("percentage".into(), Value::Number(percentage.into()));
    payload.insert(
        "text".into(),
        Value::String(strip_text(&items, overflow, icon)),
    );
    payload.insert("tooltip".into(), Value::String(tooltip));
    serde_json::to_string(&Value::Object(payload)).unwrap_or_else(|_| {
        r#"{"alt":"error","class":["error"],"percentage":0,"text":"?","tooltip":""}"#.into()
    })
}

fn strip_text(items: &[dashboard::StripItem], overflow: usize, icon: &str) -> String {
    if items.is_empty() {
        return icon.to_string();
    }
    let mut parts = Vec::new();
    for item in items {
        let name = match &item.badge {
            Some(badge) => format!("{} {badge}", item.provider),
            None => item.provider.clone(),
        };
        parts.push(format!("{name} {} {}", item.label, item.value));
    }
    let mut text = parts.join(" · ");
    if overflow > 0 {
        text.push_str(&format!(" +{overflow}"));
    }
    text
}

fn module_meter(accounts: &[DashboardAccount]) -> (i64, Severity) {
    let mut percentage = 0;
    let mut worst = Severity::Normal;
    for account in accounts {
        for line in &account.lines {
            let data = &line.data;
            let Some(limit) = data.limit.filter(|limit| data.has_data && *limit > 0.0) else {
                continue;
            };
            percentage = percentage.max(used_percent(data.used, limit));
            if rank(data.severity()) > rank(worst) {
                worst = data.severity();
            }
        }
    }
    (percentage, worst)
}

fn classes(empty: bool, severity: Severity, failed: bool, stale: bool) -> Vec<&'static str> {
    if empty {
        return vec!["empty"];
    }
    let mut names = vec![match severity {
        Severity::Normal => "normal",
        Severity::Warning => "warning",
        Severity::Critical => "critical",
    }];
    if failed {
        names.push("error");
    }
    if stale {
        names.push("stale");
    }
    names
}

fn account_markup(account: &DashboardAccount, absolute: bool, now: DateTime<Utc>) -> String {
    let mut header = format!("<b>{}</b>", escape(&account.account.full_name()));
    if let Some(plan) = account.plan.as_ref().filter(|plan| !plan.is_empty()) {
        header.push_str(" · ");
        header.push_str(&escape(plan));
    }
    let mut lines = vec![header];
    for line in &account.lines {
        lines.push(metric_markup(&line.data, absolute, now));
    }
    if let Some(problem) = account
        .problem
        .as_ref()
        .filter(|problem| !problem.is_empty())
    {
        lines.push(escape(problem));
    }
    lines.join("\n")
}

/// One plain line per metric, in the Mac row's wording: "Session: 75% left · resets in 39m".
/// No drawn bars: block characters render unevenly in bar fonts.
fn metric_markup(data: &WidgetData, absolute: bool, now: DateTime<Utc>) -> String {
    let amount = if data.has_data && data.is_bounded() {
        data.amount_text()
    } else {
        data.unbounded_figures().join(" · ")
    };
    let mut line = format!("{}: {}", escape(&data.title), escape(&amount));
    if let Some(reset) = reset_label(data, absolute, now).filter(|reset| !reset.is_empty()) {
        let reset = if absolute || reset == IMMINENT { format!("resets {reset}") } else { format!("resets in {reset}") };
        line.push_str(&format!(" · {}", escape(&reset)));
    }
    line
}

fn reset_label(data: &WidgetData, absolute: bool, now: DateTime<Utc>) -> Option<String> {
    if !absolute {
        return data.reset_text(now);
    }
    let resets_at = data
        .resets_at
        .filter(|_| data.has_data && !data.is_fresh_session_window(now))?;
    if resets_at <= now {
        Some(IMMINENT.to_string())
    } else {
        Some(openusage_core::widget::compact_clock(resets_at))
    }
}

fn used_percent(used: f64, limit: f64) -> i64 {
    if limit <= 0.0 || !used.is_finite() || !limit.is_finite() {
        return 0;
    }
    ((used / limit).clamp(0.0, 1.0) * 100.0).round() as i64
}

fn rank(severity: Severity) -> u8 {
    match severity {
        Severity::Normal => 0,
        Severity::Warning => 1,
        Severity::Critical => 2,
    }
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

struct Wake {
    read: OwnedFd,
    write: OwnedFd,
}

impl Wake {
    fn install() -> Result<Self, String> {
        let mut fds = [0; 2];
        if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC | libc::O_NONBLOCK) } != 0 {
            return Err(format!(
                "Could not watch for refresh signals: {}",
                std::io::Error::last_os_error()
            ));
        }
        let wake = Self {
            read: unsafe { OwnedFd::from_raw_fd(fds[0]) },
            write: unsafe { OwnedFd::from_raw_fd(fds[1]) },
        };
        WAKE_FD.store(wake.write.as_raw_fd(), Ordering::SeqCst);
        unsafe {
            libc::signal(libc::SIGPIPE, libc::SIG_IGN);
            libc::signal(libc::SIGUSR1, on_signal as *const () as libc::sighandler_t);
            libc::signal(libc::SIGUSR2, on_signal as *const () as libc::sighandler_t);
            libc::signal(libc::SIGINT, on_signal as *const () as libc::sighandler_t);
            libc::signal(libc::SIGTERM, on_signal as *const () as libc::sighandler_t);
        }
        Ok(wake)
    }

    fn stopped(&self) -> bool {
        STOP.load(Ordering::SeqCst)
    }

    fn reload_pending(&self) -> bool {
        RELOAD.load(Ordering::SeqCst) != 0
    }

    fn take_reload(&self) -> Option<bool> {
        match RELOAD.swap(0, Ordering::SeqCst) {
            0 => None,
            1 => Some(false),
            _ => Some(true),
        }
    }

    fn wait_until(&self, deadline: Instant) {
        loop {
            if Instant::now() >= deadline || self.stopped() || self.reload_pending() {
                self.drain();
                return;
            }
            let left = deadline.saturating_duration_since(Instant::now());
            let millis = left.as_millis().min(i32::MAX as u128) as i32;
            let mut poll = libc::pollfd {
                fd: self.read.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            let ready = unsafe { libc::poll(&mut poll, 1, millis) };
            if ready == 0 {
                continue;
            }
            if ready < 0 {
                continue;
            }
            self.drain();
            return;
        }
    }

    fn drain(&self) {
        let mut buffer = [0u8; 64];
        loop {
            let count = unsafe {
                libc::read(
                    self.read.as_raw_fd(),
                    buffer.as_mut_ptr() as *mut libc::c_void,
                    buffer.len(),
                )
            };
            if count <= 0 {
                break;
            }
        }
    }
}

impl Drop for Wake {
    fn drop(&mut self) {
        WAKE_FD.store(-1, Ordering::SeqCst);
    }
}

extern "C" fn on_signal(signal: i32) {
    if signal == libc::SIGUSR1 {
        RELOAD.store(2, Ordering::SeqCst);
    } else if signal == libc::SIGUSR2 {
        let _ = RELOAD.compare_exchange(0, 1, Ordering::SeqCst, Ordering::SeqCst);
    } else {
        STOP.store(true, Ordering::SeqCst);
    }
    let fd = WAKE_FD.load(Ordering::SeqCst);
    if fd >= 0 {
        let byte = [1u8];
        unsafe { libc::write(fd, byte.as_ptr() as *const libc::c_void, 1) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use openusage_core::kind::ProviderKind;
    use openusage_core::metric::{MetricLine, ProviderSnapshot};

    fn options() -> BarOptions {
        BarOptions {
            mode: DisplayMode::Remaining,
            absolute_resets: false,
            limit: 3,
        }
    }

    fn account(label: &str) -> Account {
        Account::new(ProviderKind::Claude, label, None)
    }

    fn weekly(
        account: &Account,
        used: f64,
        refreshed: DateTime<Utc>,
        notice: Option<&str>,
    ) -> ProviderSnapshot {
        ProviderSnapshot {
            breakdowns: Default::default(),
            provider_id: account.card_id(),
            display_name: "Claude".into(),
            plan: Some("Pro".into()),
            lines: vec![MetricLine::percent(
                "Weekly",
                used,
                Some(refreshed + chrono::Duration::hours(30)),
                Some(7 * 24 * 3600 * 1000),
            )],
            notice: notice.map(str::to_string),
            refreshed_at: refreshed,
        }
    }

    #[test]
    fn waybar_json_matches_the_strip_classes_and_escapes_pango() {
        let now = Utc::now();
        let options = options();
        let layout = LayoutState::default();
        let empty: Value = serde_json::from_str(&render(
            &[],
            &layout,
            &BTreeMap::new(),
            &BTreeMap::new(),
            DEFAULT_ICON,
            &options,
            now,
        ))
        .unwrap();
        assert_eq!(empty["text"], DEFAULT_ICON);
        assert_eq!(empty["class"], serde_json::json!(["empty"]));
        assert!(empty["tooltip"]
            .as_str()
            .unwrap()
            .contains("openusage add &lt;provider&gt;"));

        let calm = account("Work");
        let mut snapshots = BTreeMap::new();
        snapshots.insert(calm.card_id(), weekly(&calm, 10.0, now, None));
        let quiet: Value = serde_json::from_str(&render(
            &[calm],
            &layout,
            &snapshots,
            &BTreeMap::new(),
            "ICON",
            &options,
            now,
        ))
        .unwrap();
        assert_eq!(quiet["text"], "ICON");
        assert_eq!(quiet["class"], serde_json::json!(["normal"]));
        assert_eq!(quiet["percentage"], 10);
        assert!(!quiet["tooltip"].as_str().unwrap().contains('█'), "no drawn bars");
        assert!(
            quiet["tooltip"].as_str().unwrap().contains("90% left"),
            "reads like the Mac row: {}",
            quiet["tooltip"]
        );

        let hot = account("A&B <c>");
        let old = now - chrono::Duration::minutes(11);
        snapshots.clear();
        snapshots.insert(hot.card_id(), weekly(&hot, 95.0, old, Some("a <b> & c")));
        let mut statuses = BTreeMap::new();
        statuses.insert(
            hot.card_id(),
            AccountStatus {
                failures: 1,
                message: "a <b> & c".into(),
                provider_asked: false,
                retry_at: now + chrono::Duration::minutes(5),
            },
        );
        let value: Value = serde_json::from_str(&render(
            std::slice::from_ref(&hot),
            &layout,
            &snapshots,
            &statuses,
            "ICON",
            &options,
            now,
        ))
        .unwrap();
        assert_eq!(
            value["class"],
            serde_json::json!(["critical", "error", "stale"])
        );
        assert_eq!(value["alt"], "critical error stale");
        assert_eq!(value["percentage"], 95);
        assert!(value["text"].as_str().unwrap().contains("Claude"));
        assert!(value["text"].as_str().unwrap().contains("5%"));
        let tooltip = value["tooltip"].as_str().unwrap();
        assert!(tooltip.contains("Claude · A&amp;B &lt;c&gt;"), "{tooltip}");
        assert!(tooltip.contains("a &lt;b&gt; &amp; c"), "{tooltip}");
        assert!(tooltip.contains("Weekly: 5% left"), "{tooltip}");
    }

    #[test]
    fn a_hidden_metric_leaves_the_strip_and_the_tooltip() {
        let now = Utc::now();
        let hot = account("Work");
        let mut snapshots = BTreeMap::new();
        snapshots.insert(hot.card_id(), weekly(&hot, 95.0, now, None));
        let mut layout = LayoutState::default();
        assert!(layout.set_hidden(&format!("{}.weekly", hot.card_id()), true));
        let value: Value = serde_json::from_str(&render(
            std::slice::from_ref(&hot),
            &layout,
            &snapshots,
            &BTreeMap::new(),
            "ICON",
            &options(),
            now,
        ))
        .unwrap();
        assert_eq!(value["text"], "ICON");
        assert_eq!(value["percentage"], 0);
        let tooltip = value["tooltip"].as_str().unwrap();
        assert!(tooltip.contains("Claude"));
        assert!(!tooltip.contains("Weekly"), "{tooltip}");
    }
}
