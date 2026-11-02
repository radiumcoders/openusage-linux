//! 0.7.x's layout under the `openusage.layout.v1` keys: a port of `Stores/LayoutV1Keys.swift` and
//! `LayoutStore.seed`. These keys are read and never written, so a downgrade finds its layout as it left
//! it. The only account of its provider takes that provider's 0.7.x hidden metrics, metric order, and
//! place, once (`seeded_provider_ids` records it).

use std::collections::{BTreeMap, BTreeSet};

use plist::Value;
use serde::de::DeserializeOwned;
use serde::Deserialize;

use crate::account::Account;
use crate::descriptor::{descriptors, Descriptor};
use crate::kind::ProviderKind;
use crate::layout::LayoutState;
use crate::log;
use crate::store::Settings;

const KEY: &str = "openusage.layout.v1";

#[derive(Debug, Default, Clone, PartialEq)]
pub struct LayoutV1Keys {
    placed: Option<Vec<String>>,
    provider_order: Option<Vec<String>>,
    metric_order_by_provider: Option<BTreeMap<String, Vec<String>>>,
    menu_bar_pins: Option<Vec<String>>,
    enabled_providers: Option<Vec<String>>,
}

/// A 0.7.x account's layout, mapped onto an account added today.
#[derive(Debug, Clone, PartialEq)]
pub struct CarriedLayout {
    pub hidden: Vec<String>,
    pub metric_order: Option<Vec<String>>,
    /// Where the account goes among the others, or `None` when 0.7.x saved no place for its provider.
    pub provider_index: Option<usize>,
}

#[derive(Deserialize)]
struct Placed {
    /// Swift's `descriptorID` (serde's camelCase would expect `descriptorId`).
    #[serde(rename = "descriptorID")]
    descriptor_id: String,
}

fn decode<T: DeserializeOwned>(values: &BTreeMap<String, Value>, key: &str) -> Option<T> {
    let data = values.get(key)?.as_data()?;
    match serde_json::from_slice(data) {
        Ok(value) => Some(value),
        Err(error) => {
            log::warn("config", &format!("layout '{key}' failed to decode; leaving it out: {error}"));
            None
        }
    }
}

fn strings(values: &BTreeMap<String, Value>, key: &str) -> Option<Vec<String>> {
    let array = values.get(key)?.as_array()?;
    Some(array.iter().filter_map(|value| value.as_string().map(str::to_string)).collect())
}

/// The metric keys 0.7.16 had for each provider. 0.7.x's layout speaks only for these: a metric added
/// since is new to the user and starts shown.
fn metric_keys(kind: ProviderKind) -> &'static [&'static str] {
    match kind {
        ProviderKind::Claude => &["session", "weekly", "fable", "sonnet", "extra", "rateLimitResets", "trend", "today", "yesterday", "last30"],
        ProviderKind::Codex => &["session", "weekly", "spark", "sparkWeekly", "credits", "rateLimitResets", "trend", "today", "yesterday", "last30"],
        ProviderKind::Cursor => &["usage", "auto", "api", "grokBot", "onDemand", "requests", "credits", "trend", "today", "yesterday", "last30"],
        ProviderKind::Antigravity => &["geminiPro", "geminiWeekly", "argonWeekly", "claude", "claudeWeekly", "trend", "today", "yesterday", "last30"],
        ProviderKind::Copilot => &["premium", "extra", "orgCredits", "orgSpend", "chat", "completions"],
        ProviderKind::Devin => &["daily", "weekly", "extra"],
        ProviderKind::Grok => &["weekly", "payAsYouGo", "trend", "today", "yesterday", "last30"],
        ProviderKind::Ollama => &["session", "weekly", "monthly", "purchasedCredits"],
        ProviderKind::OpenCode => &["session", "weekly", "monthly", "trend", "today", "yesterday", "last30"],
        ProviderKind::OpenRouter => &["credits", "balance", "today", "week", "month", "keyLimit"],
        ProviderKind::Zai => &["session", "weekly", "webSearches"],
    }
}

fn keys_of<'a>(account: &str, ids: &'a [String]) -> Vec<&'a str> {
    let prefix = format!("{account}.");
    ids.iter().filter_map(|id| id.strip_prefix(&prefix)).collect()
}

fn account_of(id: &str) -> &str {
    id.split('.').next().unwrap_or(id)
}

fn kind_of(account: &str) -> Option<ProviderKind> {
    ProviderKind::from_raw(account.split('@').next().unwrap_or(account))
}

impl LayoutV1Keys {
    pub fn read(settings: &Settings) -> Self {
        let values = settings.read_all();
        Self {
            placed: decode::<Vec<Placed>>(&values, KEY).map(|placed| placed.into_iter().map(|p| p.descriptor_id).collect()),
            provider_order: decode(&values, &format!("{KEY}.providerOrder")),
            metric_order_by_provider: decode(&values, &format!("{KEY}.metricOrderByProvider")),
            menu_bar_pins: strings(&values, &format!("{KEY}.menuBarPins")),
            enabled_providers: strings(&values, "openusage.enabledProviders.v1"),
        }
    }

    pub fn is_empty(&self) -> bool {
        self == &Self::default()
    }

    /// The layout of the account 0.7.x showed first for `account`'s kind, mapped onto `metrics` and
    /// placed among `others`, the accounts already shown, in order. `None` when 0.7.x had it off.
    pub fn carried_over(&self, account: &Account, metrics: &[Descriptor], others: &[Account]) -> Option<CarriedLayout> {
        let old = self.first_account(account.kind)?;
        let known = metric_keys(account.kind);
        // A metric 0.7.x showed only in the menu bar was still one the user chose to see.
        let starred: BTreeSet<&str> = keys_of(&old, self.menu_bar_pins.as_deref().unwrap_or_default()).into_iter().collect();
        let mut hidden: Vec<&Descriptor> = match &self.placed {
            Some(placed) => {
                let shown: BTreeSet<&str> = keys_of(&old, placed).into_iter().chain(starred.iter().copied()).collect();
                metrics.iter().filter(|m| known.contains(&m.key) && !shown.contains(m.key)).collect()
            }
            None => vec![],
        };
        if hidden.len() == metrics.len() {
            hidden.clear();
        }
        Some(CarriedLayout {
            hidden: hidden.into_iter().map(|m| m.id.clone()).collect(),
            metric_order: self.metric_order_by_provider.as_ref().and_then(|orders| orders.get(&old)).map(|ids| {
                keys_of(&old, ids)
                    .into_iter()
                    .filter_map(|key| metrics.iter().find(|m| m.key == key).map(|m| m.id.clone()))
                    .collect()
            }),
            provider_index: self.provider_index(account.kind, others),
        })
    }

    /// The enabled account 0.7.x showed first for `kind`: by its saved provider order, then its dashboard order.
    fn first_account(&self, kind: ProviderKind) -> Option<String> {
        let enabled: Vec<&String> =
            self.enabled_providers.iter().flatten().filter(|id| kind_of(id) == Some(kind)).collect();
        let mut metric_accounts: Vec<&String> = self.metric_order_by_provider.iter().flat_map(|m| m.keys()).collect();
        metric_accounts.sort();
        self.provider_order
            .iter()
            .flatten()
            .map(String::as_str)
            .chain(self.placed.iter().flatten().map(|id| account_of(id)))
            .chain(metric_accounts.into_iter().map(String::as_str))
            .find(|account| enabled.iter().any(|id| id.as_str() == *account))
            .map(str::to_string)
    }

    /// After the last provider 0.7.x ordered before `kind`, or first when none was.
    fn provider_index(&self, kind: ProviderKind, others: &[Account]) -> Option<usize> {
        let enabled: BTreeSet<&String> = self.enabled_providers.iter().flatten().collect();
        let order: Vec<ProviderKind> = self
            .provider_order
            .iter()
            .flatten()
            .filter(|id| enabled.contains(id))
            .filter_map(|id| kind_of(id))
            .collect();
        let rank = order.iter().position(|k| *k == kind)?;
        let before = others
            .iter()
            .rposition(|other| order.iter().position(|k| *k == other.kind).is_some_and(|other_rank| other_rank < rank));
        Some(before.map_or(0, |index| index + 1))
    }
}

/// Seeds every account not seeded yet; returns whether the layout changed.
pub(crate) fn seed_all(state: &mut LayoutState, accounts: &[Account], v1: &LayoutV1Keys) -> bool {
    let unseeded: Vec<&Account> =
        accounts.iter().filter(|account| !state.seeded_provider_ids.contains(&account.card_id())).collect();
    for account in &unseeded {
        seed(state, account, accounts, v1);
    }
    !unseeded.is_empty()
}

fn seed(state: &mut LayoutState, account: &Account, accounts: &[Account], v1: &LayoutV1Keys) {
    let id = account.card_id();
    state.seeded_provider_ids.insert(id.clone());
    let others: Vec<Account> =
        state.ordered_accounts(accounts).into_iter().filter(|other| other.card_id() != id).collect();
    if others.iter().any(|other| other.kind == account.kind) {
        return;
    }
    let metrics = descriptors(account);
    let Some(carried) = v1.carried_over(account, &metrics, &others) else { return };
    state.hidden_metric_ids.extend(carried.hidden);
    if let Some(order) = carried.metric_order {
        state.metric_order_by_provider.insert(id.clone(), order);
    }
    if let Some(index) = carried.provider_index {
        let mut order: Vec<String> = others.iter().map(Account::card_id).collect();
        order.insert(index.min(order.len()), id.clone());
        state.provider_order = order;
    }
    log::notice("config", &format!("layout: {id} takes its provider's 0.7.x layout"));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v1() -> LayoutV1Keys {
        LayoutV1Keys {
            placed: Some(vec!["codex.session".into(), "codex.weekly".into(), "claude.session".into()]),
            provider_order: Some(vec!["codex".into(), "claude".into()]),
            metric_order_by_provider: Some(BTreeMap::from([(
                "codex".to_string(),
                vec!["codex.weekly".to_string(), "codex.session".to_string()],
            )])),
            menu_bar_pins: Some(vec!["codex.credits".into()]),
            enabled_providers: Some(vec!["codex".into(), "claude".into()]),
        }
    }

    #[test]
    fn a_new_account_takes_its_providers_0_7_layout() {
        let codex = Account::new(ProviderKind::Codex, "Work", None);
        let carried = v1().carried_over(&codex, &descriptors(&codex), &[]).unwrap();
        let id = |key: &str| format!("{}.{key}", codex.card_id());
        // Placed: session and weekly; pinned: credits. Spark, Spark Weekly, Limit Resets, and the history
        // rows 0.7.16 knew are hidden; Credit Limit is new since 0.7.16 and stays shown.
        assert!(carried.hidden.contains(&id("spark")));
        assert!(carried.hidden.contains(&id("today")));
        assert!(!carried.hidden.contains(&id("session")));
        assert!(!carried.hidden.contains(&id("credits")));
        assert!(!carried.hidden.contains(&id("creditLimit")));
        assert_eq!(carried.metric_order, Some(vec![id("weekly"), id("session")]));
        assert_eq!(carried.provider_index, Some(0));
    }

    #[test]
    fn placement_follows_the_0_7_provider_order_and_disabled_providers_carry_nothing() {
        let claude = Account::new(ProviderKind::Claude, "Claude", None);
        let codex = Account::new(ProviderKind::Codex, "Work", None);
        assert_eq!(v1().carried_over(&claude, &descriptors(&claude), &[codex]).unwrap().provider_index, Some(1));
        let cursor = Account::new(ProviderKind::Cursor, "Cursor", None);
        assert_eq!(v1().carried_over(&cursor, &descriptors(&cursor), &[]), None);
    }

    #[test]
    fn seeding_runs_once_per_account_and_skips_a_second_account_of_a_kind() {
        let codex = Account::new(ProviderKind::Codex, "Work", None);
        let mut state = LayoutState::default();
        assert!(seed_all(&mut state, std::slice::from_ref(&codex), &v1()));
        assert!(state.hidden_metric_ids.contains(&format!("{}.spark", codex.card_id())));
        assert!(!seed_all(&mut state, std::slice::from_ref(&codex), &v1()));

        let mut later = codex.clone();
        later.id = uuid::Uuid::new_v4();
        later.added_at = codex.added_at + chrono::Duration::seconds(1);
        let mut fresh = LayoutState::default();
        fresh.seeded_provider_ids.insert(codex.card_id());
        seed_all(&mut fresh, &[codex, later.clone()], &v1());
        assert!(!fresh.hidden_metric_ids.iter().any(|id| id.starts_with(&later.card_id())));
    }

    #[test]
    fn load_for_reads_mac_written_0_7_keys_from_settings() {
        let dir = std::env::temp_dir().join(format!("openusage-v1-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let settings = Settings::at(dir.join("settings.plist"));
        settings
            .update(|values| {
                values.insert(KEY.into(), Value::Data(br#"[{"descriptorID":"zai.session"}]"#.to_vec()));
                values.insert(format!("{KEY}.providerOrder"), Value::Data(br#"["zai"]"#.to_vec()));
                values.insert(
                    "openusage.enabledProviders.v1".into(),
                    Value::Array(vec![Value::String("zai".into())]),
                );
            })
            .unwrap();
        let zai = Account::new(ProviderKind::Zai, "Z.ai", None);
        let state = crate::layout::load_for(&settings, std::slice::from_ref(&zai));
        assert!(state.hidden_metric_ids.contains(&format!("{}.weekly", zai.card_id())));
        assert!(!state.hidden_metric_ids.contains(&format!("{}.session", zai.card_id())));
        // Seeded once, and saved, so a later read keeps the user's own changes.
        assert!(crate::layout::load(&settings).seeded_provider_ids.contains(&zai.card_id()));
        std::fs::remove_dir_all(dir).unwrap();
    }
}
