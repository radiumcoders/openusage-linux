//! Isolated dashboard data for screenshots. Never writes the user's real config.

use std::collections::BTreeMap;
use std::path::Path;

use chrono::{Duration, Utc};

use openusage_core::account::Account;
use openusage_core::kind::ProviderKind;
use openusage_core::metric::{
    BreakdownModel, MetricKind, MetricLine, MetricValue, ProviderSnapshot, UsageBreakdown,
};
use openusage_core::store::{Settings, SnapshotCache};

pub fn write(root: &Path) {
    let config = root.join("config/openusage");
    let state = root.join("state/openusage");
    let runtime = root.join("runtime");
    std::fs::create_dir_all(&config).expect("config dir");
    std::fs::create_dir_all(&state).expect("state dir");
    std::fs::create_dir_all(&runtime).expect("runtime dir");

    let now = Utc::now();
    let settings = Settings::at(config.join("settings.plist"));
    settings
        .update_accounts(|accounts| {
            accounts.clear();
            let mut claude = Account::new(ProviderKind::Claude, "Claude", None);
            claude.added_at = now - Duration::hours(4);
            let mut work = Account::new(ProviderKind::Cursor, "Work", None);
            work.added_at = now - Duration::hours(3);
            let mut home = Account::new(ProviderKind::Cursor, "Home", None);
            home.added_at = now - Duration::hours(2);
            let mut jay = Account::new(ProviderKind::Codex, "Jay", None);
            jay.added_at = now - Duration::hours(1);
            accounts.extend([claude, work, home, jay]);
        })
        .expect("settings");

    let week = 7 * 24 * 3600 * 1000;
    let cache = SnapshotCache::at(state.join("snapshots.json"));
    for account in settings.accounts() {
        let snapshot = match account.kind {
            ProviderKind::Claude => ProviderSnapshot {
                breakdowns: Default::default(),
                provider_id: account.card_id(),
                display_name: "Claude".into(),
                plan: Some("Team 5x".into()),
                lines: vec![
                    MetricLine::percent(
                        "Session",
                        0.0,
                        Some(now + Duration::hours(5)),
                        Some(5 * 3600 * 1000),
                    ),
                    MetricLine::percent(
                        "Weekly",
                        64.0,
                        Some(now + Duration::hours(30)),
                        Some(week),
                    ),
                    MetricLine::percent("Fable", 73.0, Some(now + Duration::hours(30)), Some(week)),
                    MetricLine::percent(
                        "Sonnet",
                        92.0,
                        Some(now + Duration::hours(30)),
                        Some(week),
                    ),
                    MetricLine::values(
                        "Rate Limit Resets",
                        vec![MetricValue::labeled(2.0, MetricKind::Count, "available")],
                    ),
                ],
                notice: None,
                refreshed_at: now,
            },
            ProviderKind::Cursor if account.label == "Work" => ProviderSnapshot {
                breakdowns: today_breakdown(),
                provider_id: account.card_id(),
                display_name: "Cursor".into(),
                plan: Some("Pro".into()),
                lines: vec![
                    MetricLine::percent(
                        "Total usage",
                        22.0,
                        Some(now + Duration::days(12)),
                        Some(30 * 24 * 3600 * 1000),
                    ),
                    MetricLine::values(
                        "Credits",
                        vec![MetricValue::new(12.0, MetricKind::Dollars)],
                    ),
                    MetricLine::values(
                        "Today",
                        vec![
                            MetricValue::new(49.85, MetricKind::Dollars),
                            MetricValue::labeled(35_800_000.0, MetricKind::Count, "tokens"),
                        ],
                    ),
                    MetricLine::values(
                        "Yesterday",
                        vec![
                            MetricValue::new(1.31, MetricKind::Dollars),
                            MetricValue::labeled(738_500.0, MetricKind::Count, "tokens"),
                        ],
                    ),
                ],
                notice: None,
                refreshed_at: now,
            },
            ProviderKind::Cursor => ProviderSnapshot {
                breakdowns: Default::default(),
                provider_id: account.card_id(),
                display_name: "Cursor".into(),
                plan: None,
                lines: vec![],
                notice: Some("Cursor couldn't be reached.".into()),
                refreshed_at: now,
            },
            ProviderKind::Codex => ProviderSnapshot {
                breakdowns: Default::default(),
                provider_id: account.card_id(),
                display_name: "ChatGPT".into(),
                plan: Some("Plus".into()),
                lines: vec![
                    MetricLine::percent(
                        "Session",
                        100.0,
                        Some(now + Duration::hours(3)),
                        Some(5 * 3600 * 1000),
                    ),
                    MetricLine::percent("Weekly", 80.0, Some(now + Duration::days(2)), Some(week)),
                    MetricLine::values(
                        "Today",
                        vec![
                            MetricValue::new(18.20, MetricKind::Dollars),
                            MetricValue::labeled(9_400_000.0, MetricKind::Count, "tokens"),
                        ],
                    ),
                ],
                notice: None,
                refreshed_at: now - Duration::minutes(20),
            },
            _ => continue,
        };
        cache.store(&account.card_id(), &snapshot);
    }
}

fn today_breakdown() -> BTreeMap<String, UsageBreakdown> {
    let models = [
        ("Claude Opus", 32.4, 21_000_000.0),
        ("GPT-5", 11.2, 8_400_000.0),
        ("Composer", 4.1, 3_200_000.0),
        ("Claude Sonnet", 1.5, 1_800_000.0),
        ("Gemini", 0.45, 900_000.0),
        ("Grok", 0.2, 500_000.0),
    ]
    .into_iter()
    .map(|(name, dollars, tokens)| BreakdownModel {
        name: name.into(),
        dollars: Some(dollars),
        tokens: Some(tokens),
        share: None,
    })
    .collect();
    let mut map = BTreeMap::new();
    map.insert("today".into(), UsageBreakdown { models });
    map
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seeds_an_isolated_dashboard() {
        let root = std::env::temp_dir().join("openusage-panel-fixture");
        let _ = std::fs::remove_dir_all(&root);
        write(&root);
        let settings = Settings::at(root.join("config/openusage/settings.plist"));
        let accounts = settings.accounts();
        assert_eq!(accounts.len(), 4);
        let snapshots = SnapshotCache::at(root.join("state/openusage/snapshots.json")).load();
        assert_eq!(snapshots.len(), 4);
        assert!(snapshots
            .values()
            .any(|snapshot| snapshot.plan.as_deref() == Some("Team 5x")));
    }
}
