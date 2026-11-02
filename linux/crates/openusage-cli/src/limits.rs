use std::collections::BTreeMap;
use std::io::{self, Write};

use chrono::{DateTime, Utc};
use openusage_core::account::Account;
use openusage_core::descriptor::{descriptors, LimitKind, LimitSource};
use openusage_core::engine::format_timestamp;
use openusage_core::metric::{MetricLine, ProgressFormat, ProviderSnapshot};
use openusage_core::store::{Settings, SnapshotCache};
use serde_json::{Map, Value};

use crate::text;

pub fn print_limits(token: Option<&str>, force: bool) -> i32 {
    if force {
        let _ = writeln!(
            io::stderr(),
            "openusage: --force does nothing; the command prints what the app last fetched"
        );
    }
    let accounts = Settings::open().accounts();
    let selected = match matching(&accounts, token) {
        Ok(accounts) => accounts,
        Err(token) => {
            text::error(&format!("Unknown provider: {token}"));
            return 2;
        }
    };
    let snapshots = SnapshotCache::open().load();
    let has_snapshots = selected
        .iter()
        .any(|account| snapshots.contains_key(&account.card_id()));
    text::line(&encode(&selected, &snapshots, Utc::now()));
    if has_snapshots {
        0
    } else {
        text::error("nothing fetched yet; add an account with 'openusage add <provider>' or run 'openusage refresh'");
        4
    }
}

pub fn matching(accounts: &[Account], token: Option<&str>) -> Result<Vec<Account>, String> {
    let Some(token) = token else {
        return Ok(accounts.to_vec());
    };
    let token = token.to_lowercase();
    let selected: Vec<Account> = accounts
        .iter()
        .filter(|account| account.card_id() == token || account.kind.raw() == token)
        .cloned()
        .collect();
    if selected.is_empty() {
        Err(token)
    } else {
        Ok(selected)
    }
}

pub fn encode(
    accounts: &[Account],
    snapshots: &BTreeMap<String, ProviderSnapshot>,
    generated_at: DateTime<Utc>,
) -> String {
    let mut providers = Map::new();
    for account in accounts {
        let Some(snapshot) = snapshots.get(&account.card_id()) else {
            continue;
        };
        providers.insert(
            account.card_id(),
            Value::Object(provider_entry(account, snapshot, generated_at)),
        );
    }
    let mut root = Map::new();
    root.insert(
        "generatedAt".into(),
        Value::String(format_timestamp(generated_at)),
    );
    root.insert("providers".into(), Value::Object(providers));
    root.insert("schema".into(), Value::String("openusage.limits.v2".into()));
    serde_json::to_string(&Value::Object(root)).unwrap_or_else(|_| {
        r#"{"generatedAt":"","providers":{},"schema":"openusage.limits.v2"}"#.into()
    })
}

fn provider_entry(
    account: &Account,
    snapshot: &ProviderSnapshot,
    generated_at: DateTime<Utc>,
) -> Map<String, Value> {
    let mut resources = Map::new();
    for descriptor in descriptors(account) {
        let Some(export) = &descriptor.export else {
            continue;
        };
        let Some(line) = snapshot.line(descriptor.metric_label) else {
            continue;
        };
        if let Some(resource) = resource(export, line) {
            resources.insert(export.key.to_string(), Value::Object(resource));
        }
    }
    let mut entry = Map::new();
    entry.insert("displayName".into(), Value::String(account.full_name()));
    entry.insert(
        "expiresAt".into(),
        Value::String(format_timestamp(snapshot.stale_at())),
    );
    entry.insert(
        "fetchedAt".into(),
        Value::String(format_timestamp(snapshot.refreshed_at)),
    );
    if let Some(plan) = snapshot.plan.as_ref().filter(|plan| !plan.is_empty()) {
        entry.insert("plan".into(), Value::String(plan.clone()));
    }
    entry.insert("resources".into(), Value::Object(resources));
    entry.insert("stale".into(), Value::Bool(snapshot.is_stale(generated_at)));
    entry
}

fn resource(
    export: &openusage_core::descriptor::LimitExport,
    line: &MetricLine,
) -> Option<Map<String, Value>> {
    let mut fields = Map::new();
    fields.insert("kind".into(), Value::String(export.kind.raw().to_string()));
    fields.insert(
        "unit".into(),
        Value::String(progress_unit(line).unwrap_or_else(|| export.unit.to_string())),
    );
    match (&export.source, line) {
        (
            LimitSource::Progress | LimitSource::ProgressOrValue { .. },
            MetricLine::Progress {
                used,
                limit,
                resets_at,
                period_duration_ms,
                ..
            },
        ) => {
            let bounded_limit = limit.max(0.0);
            let bounded_used = used.max(0.0);
            let figure = if export.kind == LimitKind::Balance {
                "available"
            } else {
                "used"
            };
            fields.insert(figure.into(), json_number(bounded_used));
            fields.insert("limit".into(), json_number(bounded_limit));
            fields.insert(
                "remaining".into(),
                json_number((bounded_limit - bounded_used).max(0.0)),
            );
            if bounded_limit > 0.0 {
                fields.insert(
                    "utilization".into(),
                    json_number(bounded_used / bounded_limit),
                );
            }
            if let Some(resets_at) = resets_at {
                fields.insert(
                    "resetsAt".into(),
                    Value::String(format_timestamp(*resets_at)),
                );
            }
            if let Some(period) = period_duration_ms {
                fields.insert("windowSeconds".into(), json_number(*period as f64 / 1000.0));
            }
        }
        (
            LimitSource::Value { .. } | LimitSource::ProgressOrValue { .. },
            MetricLine::Values {
                values,
                expiries_at,
                ..
            },
        ) => {
            let metric = export.value(values)?;
            let figure = if export.kind == LimitKind::Balance {
                "available"
            } else {
                "used"
            };
            fields.insert(figure.into(), json_number(metric.number));
            if !expiries_at.is_empty() {
                let mut times = expiries_at.clone();
                times.sort();
                fields.insert(
                    "expiresAt".into(),
                    Value::Array(
                        times
                            .into_iter()
                            .map(|time| Value::String(format_timestamp(time)))
                            .collect(),
                    ),
                );
            }
        }
        _ => return None,
    }
    Some(fields)
}

fn progress_unit(line: &MetricLine) -> Option<String> {
    let MetricLine::Progress { format, .. } = line else {
        return None;
    };
    match format {
        ProgressFormat::Percent => Some("percent".into()),
        ProgressFormat::Dollars => Some("usd".into()),
        ProgressFormat::Count { suffix } => {
            let unit = suffix.trim();
            if unit.is_empty() {
                None
            } else {
                Some(unit.to_string())
            }
        }
    }
}

fn json_number(value: f64) -> Value {
    if value.is_finite() {
        let rounded = value.round();
        if (value - rounded).abs() < 1e-9 && rounded.abs() <= i64::MAX as f64 {
            return Value::Number((rounded as i64).into());
        }
    }
    serde_json::Number::from_f64(value)
        .map(Value::Number)
        .unwrap_or(Value::Null)
}

#[cfg(test)]
mod tests {
    use super::*;
    use openusage_core::kind::ProviderKind;
    use openusage_core::metric::{MetricKind, MetricLine, MetricValue};
    fn stamp(text: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(text)
            .unwrap()
            .with_timezone(&Utc)
    }

    #[test]
    fn the_envelope_matches_the_documented_example() {
        let account = Account::new(ProviderKind::Codex, "Work", None);
        let fetched = stamp("2026-07-13T01:39:30.000Z");
        let generated = stamp("2026-07-13T01:40:00.000Z");
        let snapshot = ProviderSnapshot {
            breakdowns: Default::default(),
            provider_id: account.card_id(),
            display_name: "ChatGPT".into(),
            plan: Some("Pro 200".into()),
            lines: vec![
                MetricLine::percent(
                    "Session",
                    42.0,
                    Some(stamp("2026-07-13T06:00:00.000Z")),
                    Some(18_000_000),
                ),
                MetricLine::values(
                    "Credits",
                    vec![MetricValue::labeled(821.0, MetricKind::Count, "credits")],
                ),
                MetricLine::Badge {
                    label: "Pay as you go".into(),
                    text: "2500 cap".into(),
                },
            ],
            notice: None,
            refreshed_at: fetched,
        };
        let mut snapshots = BTreeMap::new();
        snapshots.insert(account.card_id(), snapshot);
        let actual: Value =
            serde_json::from_str(&encode(&[account.clone()], &snapshots, generated)).unwrap();
        let expected: Value = serde_json::from_str(
            &documented().replace("\"codex\":", &format!("\"{}\":", account.card_id())),
        )
        .unwrap();
        assert_eq!(actual, expected);

        let pending = Account::new(ProviderKind::Claude, "Other", None);
        let encoded: Value =
            serde_json::from_str(&encode(&[pending], &snapshots, generated)).unwrap();
        assert!(encoded["providers"].as_object().unwrap().is_empty());
        assert_eq!(
            matching(&[account.clone()], Some("CODEX")).unwrap().len(),
            1
        );
        assert_eq!(
            matching(&[account.clone()], Some(&account.card_id()))
                .unwrap()
                .len(),
            1
        );
        assert_eq!(matching(&[account], Some("nope")).unwrap_err(), "nope");
    }

    fn documented() -> String {
        r#"{
          "schema": "openusage.limits.v2",
          "generatedAt": "2026-07-13T01:40:00.000Z",
          "providers": {
            "codex": {
              "displayName": "ChatGPT · Work",
              "plan": "Pro 200",
              "fetchedAt": "2026-07-13T01:39:30.000Z",
              "expiresAt": "2026-07-13T01:49:30.000Z",
              "stale": false,
              "resources": {
                "session": {
                  "kind": "consumption",
                  "unit": "percent",
                  "used": 42,
                  "limit": 100,
                  "remaining": 58,
                  "utilization": 0.42,
                  "resetsAt": "2026-07-13T06:00:00.000Z",
                  "windowSeconds": 18000
                },
                "credits": {
                  "kind": "balance",
                  "unit": "credits",
                  "available": 821
                }
              }
            }
          }
        }"#
        .to_string()
    }

    #[test]
    fn a_count_progress_keeps_its_suffix_as_the_unit() {
        let account = Account::new(ProviderKind::Cursor, "Cursor", None);
        let snapshot = ProviderSnapshot {
            breakdowns: Default::default(),
            provider_id: account.card_id(),
            display_name: "Cursor".into(),
            plan: None,
            lines: vec![MetricLine::Progress {
                label: "Total usage".into(),
                used: 37.0,
                limit: 750.0,
                format: ProgressFormat::Count {
                    suffix: "requests".into(),
                },
                resets_at: None,
                period_duration_ms: None,
            }],
            notice: None,
            refreshed_at: stamp("2026-07-13T01:39:30.000Z"),
        };
        let mut snapshots = BTreeMap::new();
        snapshots.insert(account.card_id(), snapshot);
        let value: Value = serde_json::from_str(&encode(
            &[account],
            &snapshots,
            stamp("2026-07-13T01:40:00.000Z"),
        ))
        .unwrap();
        let total = &value["providers"]
            .as_object()
            .unwrap()
            .values()
            .next()
            .unwrap()["resources"]["totalUsage"];
        assert_eq!(total["unit"], "requests");
        assert_eq!(total["used"], 37);
        assert_eq!(total["limit"], 750);
        assert!(total.get("resetsAt").is_none());
    }
}
