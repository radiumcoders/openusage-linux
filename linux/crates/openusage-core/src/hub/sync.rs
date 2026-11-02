//! Reconcile a hub's login list with the accounts mirrored from it.

use std::collections::{HashMap, HashSet};

use chrono::Utc;
use serde_json::Value as Json;
use uuid::Uuid;

use super::{load_stored, store_error, HubLink, HubLogin, StoredHub, TOKEN_PLACEHOLDER};
use crate::account::{Account, Credential, OAuthTokens};
use crate::error::ProviderError;
use crate::http::HttpClient;
use crate::kind::ProviderKind;
use crate::log;
use crate::store::Settings;
use crate::vault::Vault;

use super::tunnel::{self, HubGate};

/// Makes the hub's mirrored accounts match its login list. The same listing applied twice changes
/// nothing the second time. Returns whether any account was added, removed, or relinked.
pub(crate) fn mirror(
    settings: &Settings,
    vault: &dyn Vault,
    hub_id: Uuid,
    logins: &[HubLogin],
) -> Result<bool, ProviderError> {
    let Some(hub) = load_stored(settings)
        .into_iter()
        .find(|hub| hub.id == hub_id)
    else {
        return Ok(false);
    };
    if reconcile(&hub, logins, &settings.accounts()).is_empty() {
        return Ok(false);
    }
    let mut changed = false;
    settings
        .update_accounts(|accounts| {
            let plan = reconcile(&hub, logins, accounts);
            if plan.is_empty() {
                return;
            }
            changed = apply(&hub, vault, accounts, plan);
        })
        .map_err(store_error)?;
    Ok(changed)
}

/// Re-reads every hub not read within `max_age` and reconciles its accounts. A failed read still
/// counts, so a hub that is down is not asked again until the interval has passed.
pub(crate) fn sync_due(
    settings: &Settings,
    http: &dyn HttpClient,
    vault: &dyn Vault,
    gate: &HubGate,
    synced_at: &mut HashMap<Uuid, chrono::DateTime<Utc>>,
    now: chrono::DateTime<Utc>,
    max_age: chrono::Duration,
) {
    for hub in load_stored(settings) {
        let due = synced_at
            .get(&hub.id)
            .is_none_or(|at| now.signed_duration_since(*at) >= max_age);
        if !due {
            continue;
        }
        synced_at.insert(hub.id, now);
        let key = match vault.load(hub.id) {
            Ok(Some(Credential::ApiKey(key))) if !key.is_empty() => key,
            Ok(_) => {
                log::error(
                    "auth:cliproxy",
                    &format!("{}: the management key for this hub is missing", hub.host()),
                );
                continue;
            }
            Err(error) => {
                log::error("auth:cliproxy", &format!("{}: {error}", hub.host()));
                continue;
            }
        };
        match tunnel::fetch_logins(http, gate, hub.id, &hub.url, &key) {
            Ok(logins) => {
                if !load_stored(settings)
                    .iter()
                    .any(|stored| stored.id == hub.id)
                {
                    continue;
                }
                if let Err(error) = mirror(settings, vault, hub.id, &logins) {
                    log::error("auth:cliproxy", &format!("{}: {error}", hub.host()));
                } else {
                    synced_at.insert(hub.id, now);
                }
            }
            Err(error) => {
                if load_stored(settings)
                    .iter()
                    .any(|stored| stored.id == hub.id)
                {
                    log::error("auth:cliproxy", &format!("{}: {error}", hub.host()));
                }
            }
        }
    }
}

struct Plan {
    added: Vec<HubLogin>,
    removed: Vec<Uuid>,
    relinked: HashMap<Uuid, HubLink>,
}

impl Plan {
    fn is_empty(&self) -> bool {
        self.added.is_empty() && self.removed.is_empty() && self.relinked.is_empty()
    }
}

fn reconcile(hub: &StoredHub, logins: &[HubLogin], accounts: &[Account]) -> Plan {
    let supported: Vec<&HubLogin> = logins
        .iter()
        .filter(|login| !login.is_disabled && login.kind().is_some())
        .collect();
    let mut by_id: HashMap<&str, &HubLogin> = HashMap::new();
    for login in &supported {
        by_id.entry(login.id.as_str()).or_insert(*login);
    }
    let mut seen = HashSet::new();
    let mut removed = Vec::new();
    let mut relinked = HashMap::new();
    for account in accounts {
        let Some(link) = account.hub_link() else {
            continue;
        };
        if link.hub_id != hub.id {
            continue;
        }
        match by_id.get(link.login_id.as_str()) {
            Some(login)
                if login.kind() == Some(account.kind) && seen.insert(link.login_id.clone()) =>
            {
                let current = HubLink::from_login(hub, login);
                if current != link {
                    relinked.insert(account.id, current);
                }
            }
            _ => removed.push(account.id),
        }
    }
    let added = supported
        .into_iter()
        .filter(|login| seen.insert(login.id.clone()))
        .cloned()
        .collect();
    Plan {
        added,
        removed,
        relinked,
    }
}

fn apply(hub: &StoredHub, vault: &dyn Vault, accounts: &mut Vec<Account>, plan: Plan) -> bool {
    let mut removed = HashSet::new();
    for id in plan.removed {
        match vault.delete(id) {
            Ok(()) => {
                removed.insert(id);
            }
            Err(error) => {
                log::error(
                    "auth:cliproxy",
                    &format!("could not remove a login gone from {}: {error}", hub.host()),
                );
            }
        }
    }
    accounts.retain(|account| !removed.contains(&account.id));
    for account in accounts.iter_mut() {
        if let Some(link) = plan.relinked.get(&account.id) {
            account.hub = Some(link.to_value());
        }
    }
    let mut stamp = accounts
        .iter()
        .map(|account| account.added_at)
        .max()
        .unwrap_or_else(Utc::now);
    let now = Utc::now();
    if stamp < now {
        stamp = now;
    }
    let mut added = 0;
    for login in &plan.added {
        let Some(kind) = login.kind() else { continue };
        stamp += chrono::Duration::milliseconds(1);
        let link = HubLink::from_login(hub, login);
        let mut account = fresh_account(accounts, kind, login.label.clone(), link.to_value());
        account.added_at = stamp;
        let mut tokens = OAuthTokens::new(TOKEN_PLACEHOLDER);
        if let Some(chatgpt) = &login.chatgpt_account_id {
            tokens
                .extras
                .insert("chatgpt_account_id".into(), chatgpt.clone());
        }
        match vault.save(account.id, &Credential::OAuth(tokens)) {
            Ok(()) => {
                accounts.push(account);
                added += 1;
            }
            Err(error) => {
                log::error(
                    "auth:cliproxy",
                    &format!("could not save a login from {}: {error}", hub.host()),
                );
            }
        }
    }
    log::info(
        "auth:cliproxy",
        &format!(
            "{}: {added} added, {} removed, {} relinked",
            hub.host(),
            removed.len(),
            plan.relinked.len()
        ),
    );
    true
}

fn fresh_account(accounts: &[Account], kind: ProviderKind, label: String, hub: Json) -> Account {
    loop {
        let account = Account {
            id: Uuid::new_v4(),
            kind,
            label: label.clone(),
            organization: None,
            hub: Some(hub.clone()),
            added_at: Utc::now(),
        };
        if accounts
            .iter()
            .all(|existing| existing.card_id() != account.card_id())
        {
            return account;
        }
    }
}
