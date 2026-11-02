//! Quota alerts. A port of `PaceNotificationLogic` and `QuotaNotificationEvaluator`: the same three
//! switches as the Mac app, off until turned on, each sent once per metric per reset window.

use std::collections::{HashMap, HashSet};
use std::process::{Command, Stdio};

use chrono::{DateTime, Utc};
use openusage_core::account::Account;
use openusage_core::layout::{self, LayoutState};
use openusage_core::log;
use openusage_core::metric::ProviderSnapshot;
use openusage_core::store::Settings;
use openusage_core::widget::{DisplayMode, MeterState, Severity, WidgetData};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Milestone {
    UnderTen,
    UnderThirty,
    RunningOut,
}

impl Milestone {
    pub const ALL: [Milestone; 3] = [Self::UnderTen, Self::UnderThirty, Self::RunningOut];

    pub fn name(self) -> &'static str {
        match self {
            Self::UnderTen => "under-10",
            Self::UnderThirty => "under-30",
            Self::RunningOut => "running-out",
        }
    }

    pub fn setting_key(self) -> &'static str {
        match self {
            Self::UnderTen => "openusage.notifications.underTenPercent",
            Self::UnderThirty => "openusage.notifications.healthyToClose",
            Self::RunningOut => "openusage.notifications.closeToRunningOut",
        }
    }

    fn raw(self) -> &'static str {
        match self {
            Self::UnderTen => "underTenPercent",
            Self::UnderThirty => "healthyToClose",
            Self::RunningOut => "closeToRunningOut",
        }
    }

    pub fn subtitle(self) -> &'static str {
        match self {
            Self::UnderTen => "A limit drops under 10%",
            Self::UnderThirty => "A limit drops under 30%",
            Self::RunningOut => "A limit may run out before its reset",
        }
    }

    fn title(self) -> &'static str {
        match self {
            Self::UnderTen => "Almost Out",
            Self::UnderThirty => "Cutting It Close",
            Self::RunningOut => "Will Run Out",
        }
    }

    fn body(self) -> &'static str {
        match self {
            Self::UnderTen => "Under 10% left for this window.",
            Self::UnderThirty => "Under 30% left for this window.",
            Self::RunningOut => "May run out before the limit resets.",
        }
    }

    fn is_pace(self) -> bool {
        matches!(self, Self::RunningOut)
    }
}

pub fn parse_names(names: &[String]) -> Result<Vec<Milestone>, String> {
    if names.is_empty() {
        return Ok(Milestone::ALL.to_vec());
    }
    names
        .iter()
        .map(|name| {
            Milestone::ALL
                .into_iter()
                .find(|milestone| milestone.name() == name.to_ascii_lowercase())
                .ok_or_else(|| {
                    format!("Unknown alert: {name}. Expected under-10, under-30, running-out.")
                })
        })
        .collect()
}

pub fn list(settings: &Settings) -> Vec<String> {
    Milestone::ALL
        .into_iter()
        .map(|milestone| {
            let state = if settings.bool(milestone.setting_key()) {
                "on"
            } else {
                "off"
            };
            format!("{}\t{state}\t{}", milestone.name(), milestone.subtitle())
        })
        .collect()
}

pub fn set(settings: &Settings, milestones: &[Milestone], on: bool) -> std::io::Result<()> {
    for milestone in milestones {
        settings.set_bool(milestone.setting_key(), on)?;
    }
    Ok(())
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum PaceBucket {
    Untracked,
    Healthy,
    Close,
    RunningOut,
}

fn rank(bucket: PaceBucket) -> i8 {
    match bucket {
        PaceBucket::Untracked => -1,
        PaceBucket::Healthy => 0,
        PaceBucket::Close => 1,
        PaceBucket::RunningOut => 2,
    }
}

fn bucket_for(state: MeterState) -> PaceBucket {
    match state {
        MeterState::NoData | MeterState::Unpaced => PaceBucket::Untracked,
        MeterState::Healthy => PaceBucket::Healthy,
        MeterState::CloseToLimit => PaceBucket::Close,
        MeterState::RunningOut | MeterState::Spent => PaceBucket::RunningOut,
    }
}

#[derive(Clone)]
struct NoteState {
    resets_at: Option<DateTime<Utc>>,
    fired: HashSet<Milestone>,
    previous_bucket: PaceBucket,
    previous_severity: Severity,
    primed: bool,
}

impl Default for NoteState {
    fn default() -> Self {
        Self {
            resets_at: None,
            fired: HashSet::new(),
            previous_bucket: PaceBucket::Untracked,
            previous_severity: Severity::Normal,
            primed: false,
        }
    }
}

struct Transition {
    fire: Vec<Milestone>,
    new_state: NoteState,
}

#[derive(Clone)]
struct Item {
    key: String,
    provider_id: String,
    provider_name: String,
    title: String,
    state: MeterState,
    severity: Severity,
    resets_at: Option<DateTime<Utc>>,
}

pub struct Notifier {
    states: HashMap<String, NoteState>,
}

impl Default for Notifier {
    fn default() -> Self {
        Self {
            states: HashMap::new(),
        }
    }
}

impl Notifier {
    pub fn evaluate(
        &mut self,
        accounts: &[Account],
        snapshots: &std::collections::BTreeMap<String, ProviderSnapshot>,
        now: DateTime<Utc>,
    ) {
        let settings = Settings::open();
        let layout = layout::load_for(&settings, accounts);
        let items = items_from(accounts, &layout, snapshots, now);
        evaluate_items(
            &mut self.states,
            &items,
            |milestone| settings.bool(milestone.setting_key()),
            |item, milestone| {
                send(
                    &item.provider_id,
                    milestone,
                    milestone.title(),
                    &format!("{} {}", item.provider_name, item.title),
                    milestone.body(),
                )
            },
        );
    }
}

fn items_from(
    accounts: &[Account],
    layout: &LayoutState,
    snapshots: &std::collections::BTreeMap<String, ProviderSnapshot>,
    now: DateTime<Utc>,
) -> Vec<Item> {
    let mut items = Vec::new();
    for account in layout.ordered_accounts(accounts) {
        let snapshot = snapshots.get(&account.card_id());
        for descriptor in layout.visible_descriptors(&account) {
            let data = WidgetData::resolve(&descriptor, snapshot, DisplayMode::Remaining);
            if !data.is_bounded() {
                continue;
            }
            let state = data.meter_state(now);
            let severity = data.severity();
            let resets_at = data.resets_at;
            items.push(Item {
                key: descriptor.id,
                provider_id: account.card_id(),
                provider_name: account.full_name(),
                title: data.title,
                state,
                severity,
                resets_at,
            });
        }
    }
    items
}

fn evaluate_items(
    states: &mut HashMap<String, NoteState>,
    items: &[Item],
    is_on: impl Fn(Milestone) -> bool,
    mut post: impl FnMut(&Item, Milestone) -> bool,
) {
    let mut next_states = HashMap::new();
    for item in items {
        let previous = states.get(&item.key).cloned().unwrap_or_default();
        let result = transitions(item.state, item.severity, item.resets_at, &previous);
        let held = carried_over(&previous, item.resets_at);
        let mut next = result.new_state;
        let mut pace_delivered = false;
        let mut level_delivered = false;
        for milestone in &result.fire {
            if level_delivered && !milestone.is_pace() {
                next.fired.insert(*milestone);
                continue;
            }
            let delivered = is_on(*milestone) && post(item, *milestone);
            if delivered {
                if milestone.is_pace() {
                    pace_delivered = true;
                } else {
                    level_delivered = true;
                }
                next.fired.insert(*milestone);
            }
        }
        if result.fire.iter().any(|milestone| milestone.is_pace()) && !pace_delivered {
            next.previous_bucket = held.previous_bucket;
        }
        if result.fire.iter().any(|milestone| !milestone.is_pace()) && !level_delivered {
            next.previous_severity = held.previous_severity;
        }
        next_states.insert(item.key.clone(), next);
    }
    *states = next_states;
}

fn transitions(
    state: MeterState,
    severity: Severity,
    resets_at: Option<DateTime<Utc>>,
    previous: &NoteState,
) -> Transition {
    let mut next = carried_over(previous, resets_at);
    let current = bucket_for(state);
    if state == MeterState::NoData {
        return Transition {
            fire: vec![],
            new_state: next,
        };
    }
    if !next.primed {
        next.primed = true;
        next.previous_bucket = current;
        next.previous_severity = severity;
        next.fired.clear();
        return Transition {
            fire: vec![],
            new_state: next,
        };
    }
    let mut fire = Vec::new();
    if current != PaceBucket::Untracked {
        let previous_bucket = next.previous_bucket;
        if rank(current) >= rank(PaceBucket::RunningOut)
            && rank(previous_bucket) < rank(PaceBucket::RunningOut)
        {
            queue(&next.fired, &mut fire, Milestone::RunningOut);
        }
        if rank(current) < rank(previous_bucket) && rank(current) <= rank(PaceBucket::Close) {
            next.fired.remove(&Milestone::RunningOut);
        }
        next.previous_bucket = current;
    }
    if severity == Severity::Critical && next.previous_severity != Severity::Critical {
        queue(&next.fired, &mut fire, Milestone::UnderTen);
    }
    if severity != Severity::Normal && next.previous_severity == Severity::Normal {
        queue(&next.fired, &mut fire, Milestone::UnderThirty);
    }
    if severity != Severity::Critical {
        next.fired.remove(&Milestone::UnderTen);
    }
    if severity == Severity::Normal {
        next.fired.remove(&Milestone::UnderThirty);
    }
    next.previous_severity = severity;
    Transition {
        fire,
        new_state: next,
    }
}

fn queue(fired: &HashSet<Milestone>, fire: &mut Vec<Milestone>, milestone: Milestone) {
    if !fired.contains(&milestone) {
        fire.push(milestone);
    }
}

fn carried_over(previous: &NoteState, resets_at: Option<DateTime<Utc>>) -> NoteState {
    let mut next = previous.clone();
    if reset_advanced(resets_at, previous.resets_at) {
        next.fired.clear();
        next.previous_severity = Severity::Normal;
        next.previous_bucket = PaceBucket::Untracked;
    }
    next.resets_at = resets_at.or(previous.resets_at);
    next
}

fn reset_advanced(resets_at: Option<DateTime<Utc>>, previous: Option<DateTime<Utc>>) -> bool {
    let Some(resets_at) = resets_at else {
        return false;
    };
    match previous {
        None => true,
        Some(previous) => (resets_at - previous).num_milliseconds() as f64 / 1000.0 > 1.0,
    }
}

fn send(provider_id: &str, milestone: Milestone, title: &str, subtitle: &str, body: &str) -> bool {
    let text = format!("{subtitle}\n{body}");
    let id = format!("{provider_id}.{}", milestone.raw());
    match Command::new("notify-send")
        .args([
            "--app-name=OpenUsage",
            "--icon=dialog-information",
            title,
            &text,
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
    {
        Ok(status) if status.success() => {
            log::debug("notifications", &format!("posted {id}"));
            true
        }
        Ok(status) => {
            log::warn(
                "notifications",
                &format!(
                    "skip {id}: notify-send exited {}",
                    status.code().unwrap_or(-1)
                ),
            );
            false
        }
        Err(error) => {
            log::warn(
                "notifications",
                &format!("skip {id}: notify-send could not run: {error}"),
            );
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn severity_for(fraction_left: f64) -> Severity {
        let percent_left = (fraction_left * 100.0).round() as i64;
        if percent_left <= 10 {
            Severity::Critical
        } else if percent_left <= 30 {
            Severity::Warning
        } else {
            Severity::Normal
        }
    }

    fn step(
        state: MeterState,
        fraction: f64,
        resets: DateTime<Utc>,
        previous: &NoteState,
    ) -> Transition {
        transitions(state, severity_for(fraction), Some(resets), previous)
    }

    #[test]
    fn levels_and_pace_fire_once_per_window_and_a_cold_start_is_quiet() {
        let reset = DateTime::parse_from_rfc3339("2026-07-13T06:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let first = step(MeterState::Healthy, 0.9, reset, &NoteState::default());
        assert!(first.fire.is_empty(), "cold start primes");
        let second = step(MeterState::Healthy, 0.25, reset, &first.new_state);
        assert_eq!(second.fire, vec![Milestone::UnderThirty]);
        assert!(step(MeterState::Healthy, 0.20, reset, &second.new_state)
            .fire
            .is_empty());

        let primed = step(MeterState::Healthy, 0.9, reset, &NoteState::default()).new_state;
        assert_eq!(
            step(MeterState::Healthy, 0.05, reset, &primed).fire,
            vec![Milestone::UnderTen, Milestone::UnderThirty]
        );
        let running = step(MeterState::RunningOut, 0.9, reset, &primed);
        assert_eq!(running.fire, vec![Milestone::RunningOut]);

        let mut states = HashMap::new();
        let item = Item {
            key: "a.weekly".into(),
            provider_id: "claude@1".into(),
            provider_name: "Claude".into(),
            title: "Weekly".into(),
            state: MeterState::Healthy,
            severity: Severity::Normal,
            resets_at: Some(reset),
        };
        evaluate_items(&mut states, &[item.clone()], |_| true, |_, _| true);
        let mut sent = Vec::new();
        let mut dropped = item;
        dropped.severity = Severity::Critical;
        evaluate_items(
            &mut states,
            std::slice::from_ref(&dropped),
            |_| true,
            |_, milestone| {
                sent.push(milestone);
                true
            },
        );
        assert_eq!(sent, vec![Milestone::UnderTen]);
    }

    #[test]
    fn alert_names_parse_and_round_trip_through_settings() {
        assert_eq!(parse_names(&[]).unwrap(), Milestone::ALL);
        assert!(parse_names(&["nope".into()]).is_err());
        let dir = std::env::temp_dir().join(format!(
            "openusage-alerts-{}-{}",
            std::process::id(),
            Utc::now().timestamp_micros()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let settings = Settings::at(dir.join("settings.plist"));
        set(&settings, &[Milestone::UnderTen], true).unwrap();
        let lines = list(&settings);
        assert_eq!(lines[0], "under-10\ton\tA limit drops under 10%");
        assert_eq!(lines[1], "under-30\toff\tA limit drops under 30%");
        assert_eq!(
            lines[2],
            "running-out\toff\tA limit may run out before its reset"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_hidden_metric_is_not_an_alert() {
        use openusage_core::kind::ProviderKind;
        use openusage_core::metric::{MetricLine, ProviderSnapshot};

        let account = Account::new(ProviderKind::Claude, "Work", None);
        let weekly = format!("{}.weekly", account.card_id());
        let mut layout = LayoutState::default();
        assert!(layout.set_hidden(&weekly, true));
        let now = Utc::now();
        let snapshot = ProviderSnapshot {
            breakdowns: Default::default(),
            provider_id: account.card_id(),
            display_name: "Claude".into(),
            plan: None,
            lines: vec![
                MetricLine::percent(
                    "Weekly",
                    95.0,
                    Some(now + chrono::Duration::hours(5)),
                    Some(3_600_000),
                ),
                MetricLine::percent(
                    "Session",
                    10.0,
                    Some(now + chrono::Duration::hours(1)),
                    Some(3_600_000),
                ),
            ],
            notice: None,
            refreshed_at: now,
        };
        let mut snapshots = std::collections::BTreeMap::new();
        snapshots.insert(account.card_id(), snapshot);
        let items = items_from(std::slice::from_ref(&account), &layout, &snapshots, now);
        assert!(items.iter().all(|item| item.key != weekly));
        assert!(items.iter().any(|item| item.key.ends_with(".session")));
    }
}
