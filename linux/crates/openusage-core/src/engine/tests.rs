use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use chrono::{DateTime, Duration as ChronoDuration, Utc};

use super::*;
use crate::kind::ProviderKind;

static ENGINE_TEST: Mutex<()> = Mutex::new(());

struct Temp(PathBuf);
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn temp() -> Temp {
    let dir = std::env::temp_dir().join(format!("openusage-engine-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&dir).unwrap();
    Temp(dir)
}

fn engine_at(dir: &Path) -> Engine {
    Engine {
        settings: Settings::at(dir.join("settings.plist")),
        cache: SnapshotCache::at(dir.join("snapshots.json")),
        status: StatusFile::at(dir.join("status.json")),
        http: Arc::new(RejectHttp),
        vault: Arc::new(crate::vault::MemoryVault::default()),
        accounts: Vec::new(),
        snapshots: BTreeMap::new(),
        statuses: BTreeMap::new(),
        states: HashMap::new(),
        timeout: Duration::from_secs(PROVIDER_TIMEOUT_SECONDS),
    }
}

struct RejectHttp;
impl HttpClient for RejectHttp {
    fn send(&self, _: crate::http::HttpRequest) -> Result<crate::http::HttpResponse, ProviderError> {
        Err(ProviderError::ConnectionFailed)
    }
}

fn at(text: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(text).unwrap().with_timezone(&Utc)
}

fn sample(refreshed: DateTime<Utc>, notice: Option<&str>) -> ProviderSnapshot {
    ProviderSnapshot {
        breakdowns: Default::default(),
        provider_id: "claude@1".into(),
        display_name: "Claude".into(),
        plan: None,
        lines: vec![],
        notice: notice.map(str::to_string),
        refreshed_at: refreshed,
    }
}

#[test]
fn backoff_doubles_from_five_minutes_to_an_hour_and_a_rate_limit_holds_a_forced_refresh() {
    assert_eq!(delay_seconds(1), 300.0);
    assert_eq!(delay_seconds(2), 600.0);
    assert_eq!(delay_seconds(5), 3600.0);
    assert_eq!(delay_seconds(8), 3600.0);
    let now = at("2026-07-13T01:40:00Z");
    let mut current = None;
    let mut cursor = now;
    for expected in [300, 600, 1200, 2400, 3600, 3600] {
        let next = AccountStatus::after(FailureKind::Refused, "no".into(), current.as_ref(), cursor);
        assert_eq!(next.retry_at, cursor + ChronoDuration::seconds(expected));
        assert!(next.holds(cursor, false));
        assert!(!next.holds(cursor, true));
        cursor = next.retry_at;
        current = Some(next);
    }
    let down = AccountStatus::after(FailureKind::Unreachable, "down".into(), current.as_ref(), now);
    assert_eq!(down.failures, current.unwrap().failures);
    assert_eq!(down.retry_at, now + ChronoDuration::seconds(300));
    let limited = AccountStatus::after(FailureKind::RateLimited(Some(17.0)), "wait".into(), None, now);
    assert!(limited.provider_asked);
    assert_eq!(limited.retry_at, add_seconds(now, 17.0));
    assert!(limited.holds(now, true));
    assert!(!limited.holds(limited.retry_at, true));
    let doubled = AccountStatus::after(FailureKind::RateLimited(None), "wait".into(), None, now);
    assert_eq!(doubled.retry_at, now + ChronoDuration::seconds(300));
}

#[test]
fn problem_text_matches_the_mac_wording() {
    let now = at("2026-07-13T01:40:00Z");
    let account = Account::new(ProviderKind::Claude, "Work", None);
    let retry = now + ChronoDuration::minutes(30);
    let waiting = AccountStatus { failures: 1, message: "hidden".into(), provider_asked: true, retry_at: retry };
    let waiting_text = format!("Claude asked OpenUsage to wait. Retrying at {}.", compact_clock(retry));
    assert_eq!(problem_text(&account, None, Some(&waiting), now).as_deref(), Some(waiting_text.as_str()));
    let stale = sample(now - ChronoDuration::hours(3), None);
    assert_eq!(problem_text(&account, Some(&stale), None, now).as_deref(), Some("Last updated 3h ago"));
    let refused = AccountStatus { failures: 1, message: "Refused".into(), provider_asked: false, retry_at: retry };
    assert_eq!(problem_text(&account, Some(&stale), Some(&refused), now).as_deref(), Some("Refused"));
    let noticed = sample(now - ChronoDuration::hours(3), Some("heads up"));
    assert_eq!(problem_text(&account, Some(&noticed), None, now).as_deref(), Some("heads up"));
    let fresh = sample(now, None);
    let expired = AccountStatus { failures: 1, message: "hidden".into(), provider_asked: true, retry_at: now };
    assert_eq!(
        problem_text(&account, Some(&fresh), Some(&expired), now).as_deref(),
        Some("Waiting to retry after a rate limit.")
    );
}

#[test]
fn a_failed_read_backs_off_until_forced_and_a_success_clears_it() {
    let _guard = ENGINE_TEST.lock().unwrap();
    *TEST_HOOK.lock().unwrap() = Some(Arc::new(|_account, _state| {
        Err(ProviderError::Unavailable("Claude isn't supported in this build yet.".into()))
    }));
    let dir = temp();
    let mut engine = engine_at(&dir.0);
    engine.settings.update_accounts(|accounts| accounts.push(Account::new(ProviderKind::Claude, "Work", None))).unwrap();
    let now = Utc::now();
    let failed = engine.refresh_at(true, now);
    assert_eq!(failed[0].outcome, RefreshOutcome::Failed);
    let message = engine.statuses().values().next().unwrap().message.clone();
    assert!(message.contains("isn't supported"), "{message}");
    assert!(dir.0.join("status.json").is_file());
    let loaded = engine.status.load();
    assert_eq!(loaded.len(), 1);
    assert_eq!(engine.refresh_at(false, now)[0].outcome, RefreshOutcome::BackedOff);
    let forced = engine.refresh_at(true, now);
    assert_eq!(forced[0].outcome, RefreshOutcome::Failed);
    assert_eq!(engine.statuses().values().next().unwrap().failures, 2);

    let saw = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&saw);
    *TEST_HOOK.lock().unwrap() = Some(Arc::new(move |_account, state| {
        if state.get::<bool>("seen") == Some(true) {
            flag.store(true, Ordering::SeqCst);
        }
        state.set("seen", &true);
        Ok(ProviderUsage { plan: Some("Pro".into()), lines: vec![], notice: None, breakdowns: Default::default() })
    }));
    let updated = engine.refresh_at(true, now);
    assert_eq!(updated[0].outcome, RefreshOutcome::Refreshed);
    assert!(engine.statuses().is_empty());
    assert_eq!(
        engine.snapshots().get(&updated[0].account.card_id()).and_then(|s| s.plan.as_deref()),
        Some("Pro")
    );
    assert_eq!(engine.refresh_at(false, now)[0].outcome, RefreshOutcome::CacheHit);
    engine.refresh_at(true, now);
    assert!(saw.load(Ordering::SeqCst));
    *TEST_HOOK.lock().unwrap() = None;
    assert!(engine.accounts_changed_on_disk() == false);
    engine.settings.update_accounts(|accounts| accounts[0].label = "Renamed".into()).unwrap();
    assert!(engine.accounts_changed_on_disk());
}

#[test]
fn a_read_that_misses_the_deadline_keeps_the_last_state() {
    let _guard = ENGINE_TEST.lock().unwrap();
    let dir = temp();
    let mut engine = engine_at(&dir.0);
    engine.timeout = Duration::from_millis(40);
    engine.settings.update_accounts(|accounts| accounts.push(Account::new(ProviderKind::Zai, "Z.ai", None))).unwrap();
    *TEST_HOOK.lock().unwrap() = Some(Arc::new(|_account, state| {
        state.set("late", &true);
        thread::sleep(Duration::from_millis(200));
        Ok(ProviderUsage::default())
    }));
    let now = Utc::now();
    let reports = engine.refresh_at(true, now);
    assert_eq!(reports[0].outcome, RefreshOutcome::Failed);
    assert!(engine.statuses().values().next().unwrap().message.contains("timed out"));
    assert!(engine.snapshots().is_empty());
    assert!(engine.states.get(&reports[0].account.card_id()).and_then(|s| s.get::<bool>("late")).is_none());
    *TEST_HOOK.lock().unwrap() = None;
    thread::sleep(Duration::from_millis(180));
}
