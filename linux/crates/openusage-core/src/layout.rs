//! The user's dashboard layout: provider (account) order, metric order per account, and hidden metrics.
//! A port of `Stores/LayoutStore.swift`, `LayoutStore+Customization.swift`, `LayoutStoreModels.swift`, and
//! `LayoutPersistence.swift`, saved as JSON data under the Mac key `openusage.layout.v2` in settings.plist.
//! The app never sorts accounts or metrics itself: a new account goes to the bottom of its provider's
//! accounts (a new provider to the bottom), and a new metric shows in the provider's declared order.
//! STUB: the API is fixed; the implementation is being ported (order = stored account order, nothing hidden).

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::account::Account;
use crate::descriptor::{descriptors, Descriptor};
use crate::store::Settings;

pub const KEY: &str = "openusage.layout.v2";

/// The Mac `LayoutState`, field for field.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LayoutState {
    #[serde(default)]
    pub hidden_metric_ids: BTreeSet<String>,
    #[serde(default)]
    pub provider_order: Vec<String>,
    #[serde(default)]
    pub metric_order_by_provider: BTreeMap<String, Vec<String>>,
    #[serde(default)]
    pub seeded_provider_ids: BTreeSet<String>,
}

pub fn load(_settings: &Settings) -> LayoutState {
    LayoutState::default()
}

pub fn save(_settings: &Settings, _state: &LayoutState) -> std::io::Result<()> {
    Ok(())
}

impl LayoutState {
    /// `accounts` in the user's order (the dashboard, the Accounts list, the bar, notifications).
    pub fn ordered_accounts(&self, accounts: &[Account]) -> Vec<Account> {
        accounts.to_vec()
    }

    /// Every metric of `account`, in the user's order, hidden ones included (the account page's list).
    pub fn ordered_descriptors(&self, account: &Account) -> Vec<Descriptor> {
        descriptors(account)
    }

    /// The metrics the dashboard shows, in order.
    pub fn visible_descriptors(&self, account: &Account) -> Vec<Descriptor> {
        self.ordered_descriptors(account).into_iter().filter(|d| !self.is_hidden(&d.id)).collect()
    }

    pub fn is_hidden(&self, descriptor_id: &str) -> bool {
        self.hidden_metric_ids.contains(descriptor_id)
    }

    /// Shows or hides a metric everywhere. Returns whether anything changed.
    pub fn set_hidden(&mut self, _descriptor_id: &str, _hidden: bool) -> bool {
        false
    }

    /// Moves the provider (all accounts of `dragged`'s kind) to where `target`'s provider is. Card ids.
    pub fn move_provider(&mut self, _accounts: &[Account], _dragged: &str, _target: &str) -> bool {
        false
    }

    /// Moves one account to `target`'s place, following the Mac rules (within its provider; across
    /// providers only when both have one account). Card ids.
    pub fn move_account(&mut self, _accounts: &[Account], _dragged: &str, _target: &str) -> bool {
        false
    }

    /// Moves a metric within its account. Descriptor ids.
    pub fn move_metric(&mut self, _account: &Account, _dragged: &str, _target: &str) -> bool {
        false
    }
}
