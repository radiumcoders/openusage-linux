//! The user's dashboard layout: provider and account order, metric order per account, and hidden
//! metrics. A port of `LayoutStore`, `LayoutStore+Customization`, `LayoutStoreModels`, and
//! `LayoutPersistence`. The Mac app stores it as JSON in a property-list data blob under
//! `openusage.layout.v2`. A blob the Mac app wrote reads here.
//!
//! The app never sorts accounts or metrics. A new provider goes to the bottom, a new account goes
//! to the bottom of its provider's accounts, and a metric the user has not placed follows the
//! provider's declared order after the metrics they have placed. Mutations change this value only;
//! call [`save`] to write them. Undo is per session and is not written.
//!
//! 0.7.x seeding (`LayoutV1Keys`) is not applied. `seededProviderIDs` still round-trips, and every
//! `openusage.layout.v1` key is left as it was.

use std::collections::{BTreeMap, BTreeSet};

use plist::Value;
use serde::{Deserialize, Serialize};

use crate::account::Account;
use crate::descriptor::{descriptors, Descriptor};
use crate::log;
use crate::store::Settings;

pub const KEY: &str = "openusage.layout.v2";

/// Session undo remembers this many layout edits, matching `LayoutStore.maxUndoDepth`.
pub const MAX_UNDO_DEPTH: usize = 40;

/// The Mac `LayoutState`, field for field. `==` compares the saved fields and ignores undo.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LayoutState {
    /// Absent in a layout saved before hiding existed, which means nothing is hidden.
    /// The key is `hiddenMetricIDs`, matching Swift. `rename_all` would write `hiddenMetricIds`.
    #[serde(default, rename = "hiddenMetricIDs")]
    pub hidden_metric_ids: BTreeSet<String>,
    pub provider_order: Vec<String>,
    pub metric_order_by_provider: BTreeMap<String, Vec<String>>,
    #[serde(rename = "seededProviderIDs")]
    pub seeded_provider_ids: BTreeSet<String>,
    #[serde(skip)]
    undo: Vec<Checkpoint>,
}

#[derive(Clone, Debug)]
struct Checkpoint {
    hidden_metric_ids: BTreeSet<String>,
    provider_order: Vec<String>,
    metric_order_by_provider: BTreeMap<String, Vec<String>>,
    seeded_provider_ids: BTreeSet<String>,
}

impl PartialEq for LayoutState {
    fn eq(&self, other: &Self) -> bool {
        self.hidden_metric_ids == other.hidden_metric_ids
            && self.provider_order == other.provider_order
            && self.metric_order_by_provider == other.metric_order_by_provider
            && self.seeded_provider_ids == other.seeded_provider_ids
    }
}

impl Eq for LayoutState {}

pub fn load(settings: &Settings) -> LayoutState {
    let Some(value) = settings.get(KEY) else {
        return LayoutState::default();
    };
    let Some(data) = value.into_data() else {
        log::warn(
            "config",
            "saved layout failed to decode; starting a new one: openusage.layout.v2 is not data",
        );
        return LayoutState::default();
    };
    match serde_json::from_slice::<LayoutState>(&data) {
        Ok(state) => state,
        Err(error) => {
            log::warn(
                "config",
                &format!("saved layout failed to decode; starting a new one: {error}"),
            );
            LayoutState::default()
        }
    }
}

pub fn save(settings: &Settings, state: &LayoutState) -> std::io::Result<()> {
    let data = serde_json::to_vec(state).map_err(std::io::Error::other)?;
    settings.update(|values| {
        values.insert(KEY.to_string(), Value::Data(data));
    })
}

/// Loads the layout and, when an account has no saved place, writes that place. A new provider goes
/// to the bottom and a new account goes to the bottom of its provider. Unchanged layouts are not
/// rewritten. An account seen for the first time that is the only one of its provider takes that
/// provider's 0.7.x layout (`layout_v1`). This is the read the dashboard, the bar, and `openusage
/// accounts` use.
pub fn load_for(settings: &Settings, accounts: &[Account]) -> LayoutState {
    let mut state = load(settings);
    let seeded = crate::layout_v1::seed_all(&mut state, accounts, &crate::layout_v1::LayoutV1Keys::read(settings));
    if state.materialize(accounts) | seeded {
        state.prune(accounts);
        if let Err(error) = save(settings, &state) {
            log::warn("config", &format!("failed to persist layout: {error}"));
        }
    } else {
        state.prune(accounts);
    }
    state
}

impl LayoutState {
    /// Drops hidden metrics, metric orders, and seeded ids whose accounts or metrics are gone.
    /// `provider_order` keeps unknown ids, matching `LayoutStore.loadLayout`; [`Self::ordered_accounts`]
    /// ignores them.
    pub fn prune(&mut self, accounts: &[Account]) {
        let live_ids: BTreeSet<String> = accounts.iter().map(Account::card_id).collect();
        let live_metrics: BTreeSet<String> = accounts
            .iter()
            .flat_map(|account| {
                descriptors(account)
                    .into_iter()
                    .map(|descriptor| descriptor.id)
            })
            .collect();
        self.hidden_metric_ids
            .retain(|id| live_metrics.contains(id));
        self.metric_order_by_provider
            .retain(|id, _| live_ids.contains(id));
        self.seeded_provider_ids.retain(|id| live_ids.contains(id));
    }

    /// Re-reads the saved layout. Undo stays when `accounts` has the same ids as `previous`
    /// (`LayoutStore.apply` keeps it across a rename) and is dropped when a card was added or removed.
    pub fn reload(&mut self, settings: &Settings, previous: &[Account], accounts: &[Account]) {
        let previous_ids: BTreeSet<String> = previous.iter().map(Account::card_id).collect();
        let account_ids: BTreeSet<String> = accounts.iter().map(Account::card_id).collect();
        if previous_ids != account_ids {
            self.undo.clear();
        }
        let undo = std::mem::take(&mut self.undo);
        *self = load(settings);
        self.prune(accounts);
        self.undo = undo;
    }

    /// Stores the order [`Self::ordered_accounts`] would show, when an account is missing from
    /// `provider_order`. Returns whether it changed. Ids of removed accounts stay until then.
    pub fn materialize(&mut self, accounts: &[Account]) -> bool {
        let computed: Vec<String> = self
            .ordered_accounts(accounts)
            .iter()
            .map(Account::card_id)
            .collect();
        let known: BTreeSet<String> = accounts.iter().map(Account::card_id).collect();
        let saved_known: Vec<String> = self
            .provider_order
            .iter()
            .filter(|id| known.contains(*id))
            .cloned()
            .collect();
        if saved_known == computed {
            return false;
        }
        self.provider_order = computed;
        true
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn undo(&mut self) -> bool {
        let Some(previous) = self.undo.pop() else {
            return false;
        };
        self.hidden_metric_ids = previous.hidden_metric_ids;
        self.provider_order = previous.provider_order;
        self.metric_order_by_provider = previous.metric_order_by_provider;
        self.seeded_provider_ids = previous.seeded_provider_ids;
        true
    }

    /// `accounts` in the user's order. Pass them in the order they were added. A saved id with no
    /// account is skipped; an account with no saved place goes after the last account of its kind,
    /// or to the bottom when it is the first of its kind.
    pub fn ordered_accounts(&self, accounts: &[Account]) -> Vec<Account> {
        let mut by_id = BTreeMap::new();
        for (index, account) in accounts.iter().enumerate() {
            by_id.entry(account.card_id()).or_insert(index);
        }
        let mut ordered: Vec<String> = self
            .provider_order
            .iter()
            .filter(|id| by_id.contains_key(id.as_str()))
            .cloned()
            .collect();
        // Computed once, as `WidgetRegistry.orderedProviderIDs` does, so a newly placed account is
        // not treated as already saved while the rest of the slice is placed.
        let saved: BTreeSet<String> = ordered.iter().cloned().collect();
        for account in accounts {
            let id = account.card_id();
            if saved.contains(&id) {
                continue;
            }
            let kind = account.kind;
            if let Some(last) = ordered.iter().rposition(|existing| {
                by_id
                    .get(existing)
                    .is_some_and(|index| accounts[*index].kind == kind)
            }) {
                ordered.insert(last + 1, id);
            } else {
                ordered.push(id);
            }
        }
        ordered
            .into_iter()
            .filter_map(|id| by_id.get(&id).map(|index| accounts[*index].clone()))
            .collect()
    }

    /// Every metric of `account`, in the user's order, hidden ones included (the account page's list).
    /// Saved ids come first. A metric that is not saved follows in the provider's declared order.
    pub fn ordered_descriptors(&self, account: &Account) -> Vec<Descriptor> {
        let valid = descriptors(account);
        let mut seen = BTreeSet::new();
        let mut ordered = Vec::new();
        if let Some(saved) = self.metric_order_by_provider.get(&account.card_id()) {
            for id in saved {
                if valid.iter().any(|descriptor| descriptor.id == *id) && seen.insert(id.clone()) {
                    ordered.push(id.clone());
                }
            }
        }
        for descriptor in &valid {
            if seen.insert(descriptor.id.clone()) {
                ordered.push(descriptor.id.clone());
            }
        }
        ordered
            .into_iter()
            .filter_map(|id| valid.iter().find(|descriptor| descriptor.id == id).cloned())
            .collect()
    }

    /// The metrics the dashboard shows, in order. Hiding removes a metric here; it stays in
    /// [`Self::ordered_descriptors`].
    pub fn visible_descriptors(&self, account: &Account) -> Vec<Descriptor> {
        self.ordered_descriptors(account)
            .into_iter()
            .filter(|descriptor| !self.is_hidden(&descriptor.id))
            .collect()
    }

    pub fn is_hidden(&self, descriptor_id: &str) -> bool {
        self.hidden_metric_ids.contains(descriptor_id)
    }

    /// Shows or hides a metric everywhere. Returns whether anything changed.
    pub fn set_hidden(&mut self, descriptor_id: &str, hidden: bool) -> bool {
        let before = self.checkpoint();
        let changed = if hidden {
            self.hidden_metric_ids.insert(descriptor_id.to_string())
        } else {
            self.hidden_metric_ids.remove(descriptor_id)
        };
        self.push_undo(before, changed)
    }

    /// Moves the provider (every account of `dragged`'s kind) to where `target`'s provider is.
    /// Card ids. The same kind is a no-op; move one account with [`Self::move_account`].
    pub fn move_provider(&mut self, accounts: &[Account], dragged: &str, target: &str) -> bool {
        let Some(from) = accounts.iter().find(|account| account.card_id() == dragged) else {
            return false;
        };
        let Some(to) = accounts.iter().find(|account| account.card_id() == target) else {
            return false;
        };
        let ordered = self.ordered_accounts(accounts);
        let mut families = Vec::new();
        let mut members: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for account in &ordered {
            let family = account.kind.raw().to_string();
            if !members.contains_key(&family) {
                families.push(family.clone());
            }
            members.entry(family).or_default().push(account.card_id());
        }
        let Some(next) = reordered(&families, from.kind.raw(), to.kind.raw()) else {
            return false;
        };
        let before = self.checkpoint();
        self.prune(accounts);
        self.provider_order = next
            .into_iter()
            .flat_map(|family| members.remove(&family).unwrap_or_default())
            .collect();
        self.push_undo(before, true)
    }

    /// Moves one account to `target`'s place. Within its provider this is free. Across providers it
    /// is allowed only when each of the two providers has one account. Card ids.
    pub fn move_account(&mut self, accounts: &[Account], dragged: &str, target: &str) -> bool {
        let Some(from) = accounts.iter().find(|account| account.card_id() == dragged) else {
            return false;
        };
        let Some(to) = accounts.iter().find(|account| account.card_id() == target) else {
            return false;
        };
        if from.kind != to.kind {
            let count = accounts
                .iter()
                .filter(|account| account.kind == from.kind || account.kind == to.kind)
                .count();
            if count != 2 {
                return false;
            }
        }
        let ids: Vec<String> = self
            .ordered_accounts(accounts)
            .iter()
            .map(Account::card_id)
            .collect();
        let Some(next) = reordered(&ids, dragged, target) else {
            return false;
        };
        let before = self.checkpoint();
        self.prune(accounts);
        self.provider_order = next;
        self.push_undo(before, true)
    }

    /// Moves a metric within its account. Descriptor ids.
    pub fn move_metric(&mut self, account: &Account, dragged: &str, target: &str) -> bool {
        let ids: Vec<String> = self
            .ordered_descriptors(account)
            .iter()
            .map(|descriptor| descriptor.id.clone())
            .collect();
        let Some(next) = reordered(&ids, dragged, target) else {
            return false;
        };
        let before = self.checkpoint();
        self.metric_order_by_provider
            .insert(account.card_id(), next);
        self.push_undo(before, true)
    }

    fn checkpoint(&self) -> Checkpoint {
        Checkpoint {
            hidden_metric_ids: self.hidden_metric_ids.clone(),
            provider_order: self.provider_order.clone(),
            metric_order_by_provider: self.metric_order_by_provider.clone(),
            seeded_provider_ids: self.seeded_provider_ids.clone(),
        }
    }

    fn push_undo(&mut self, before: Checkpoint, changed: bool) -> bool {
        if !changed {
            return false;
        }
        self.undo.push(before);
        let overflow = self.undo.len().saturating_sub(MAX_UNDO_DEPTH);
        if overflow > 0 {
            self.undo.drain(0..overflow);
        }
        true
    }
}

/// Remove `dragged` and reinsert it next to `target`: after it when moving down, before it when
/// moving up. `None` when either id is missing or they are the same.
fn reordered(ids: &[String], dragged: &str, target: &str) -> Option<Vec<String>> {
    if dragged == target {
        return None;
    }
    let from = ids.iter().position(|id| id == dragged)?;
    let to = ids.iter().position(|id| id == target)?;
    let mut next = ids.to_vec();
    next.remove(from);
    let adjusted = next.iter().position(|id| id == target)?;
    let insert = if from < to { adjusted + 1 } else { adjusted };
    next.insert(insert.min(next.len()), dragged.to_string());
    Some(next)
}

#[cfg(test)]
#[path = "layout_tests.rs"]
mod tests;
