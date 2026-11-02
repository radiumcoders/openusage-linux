//! `openusage hub`: connect a CLIProxyAPI hub, list hubs, or remove one.

use openusage_core::daemon;
use openusage_core::engine;
use openusage_core::http::UreqClient;
use openusage_core::hub;
use openusage_core::store::{Settings, SnapshotCache};
use openusage_core::vault::SecretToolVault;
use uuid::Uuid;

use crate::signin;
use crate::text;

pub fn list() -> i32 {
    let settings = Settings::open();
    let hubs = hub::list(&settings);
    if hubs.is_empty() {
        text::line("No hubs.");
        return 0;
    }
    let accounts = settings.accounts();
    for item in hubs {
        let count = accounts
            .iter()
            .filter(|account| {
                account
                    .hub_link()
                    .is_some_and(|link| link.hub_id == item.id)
            })
            .count();
        let noun = if count == 1 { "account" } else { "accounts" };
        text::line(&format!(
            "{}\t{}\t{}\t{count} {noun}",
            item.id.to_string().to_ascii_uppercase(),
            item.host,
            item.url
        ));
    }
    0
}

pub fn add(url: &str) -> i32 {
    if hub::base_url(url).is_none() {
        text::error("Enter the hub\u{2019}s address, like http://192.168.1.20:8317.");
        return 1;
    }
    let Some(key) = signin::prompt_secret_line("Management key: ") else {
        text::error("No management key was entered.");
        return 1;
    };
    let settings = Settings::open();
    match hub::connect(
        url,
        &key,
        &UreqClient::new(),
        &SecretToolVault::default(),
        &settings,
    ) {
        Ok(item) => {
            let count = settings
                .accounts()
                .iter()
                .filter(|account| {
                    account
                        .hub_link()
                        .is_some_and(|link| link.hub_id == item.id)
                })
                .count();
            text::line(&format!(
                "Connected {} ({}). {count} accounts.",
                item.host,
                item.id.to_string().to_ascii_uppercase()
            ));
            daemon::notify_accounts_changed();
            0
        }
        Err(error) => {
            text::error(&error.to_string());
            1
        }
    }
}

pub fn remove(token: &str) -> i32 {
    let settings = Settings::open();
    let item = match find(&settings, token) {
        Ok(item) => item,
        Err(message) => {
            text::error(&message);
            return 2;
        }
    };
    let removed: Vec<_> = settings
        .accounts()
        .into_iter()
        .filter(|account| {
            account
                .hub_link()
                .is_some_and(|link| link.hub_id == item.id)
        })
        .collect();
    if let Err(error) = hub::disconnect(item.id, &SecretToolVault::default(), &settings) {
        text::error(&error.to_string());
        return 1;
    }
    let remaining: Vec<String> = settings
        .accounts()
        .iter()
        .map(|account| account.card_id())
        .collect();
    SnapshotCache::open().retain(&remaining);
    for account in &removed {
        engine::forget_status(&account.card_id());
    }
    daemon::notify_accounts_changed();
    let noun = if removed.len() == 1 {
        "account"
    } else {
        "accounts"
    };
    text::line(&format!(
        "Removed the hub at {} and {} {noun}.",
        item.host,
        removed.len()
    ));
    0
}

fn find(settings: &Settings, token: &str) -> Result<hub::Hub, String> {
    let hubs = hub::list(settings);
    let token = token.trim();
    if let Ok(id) = Uuid::parse_str(token) {
        return hubs
            .into_iter()
            .find(|item| item.id == id)
            .ok_or_else(|| format!("No hub {token}."));
    }
    let mut matches: Vec<_> = hubs
        .into_iter()
        .filter(|item| {
            item.host.eq_ignore_ascii_case(token) || item.url.eq_ignore_ascii_case(token)
        })
        .collect();
    match matches.len() {
        1 => Ok(matches.remove(0)),
        0 => Err(format!("No hub {token}.")),
        _ => Err(format!(
            "More than one hub matches {token}. Pass the hub id from `openusage hub list`."
        )),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use chrono::Utc;
    use openusage_core::account::Account;
    use openusage_core::kind::ProviderKind;
    use openusage_core::metric::ProviderSnapshot;
    use serde_json::json;

    use crate::limits::encode;

    #[test]
    fn a_hub_account_display_name_matches_the_mac_full_name() {
        let mut account = Account::new(ProviderKind::Claude, "alice@example.com", None);
        account.hub = Some(json!({
            "hubID": "8AD3B217-0000-4000-8000-0000000000AA",
            "hubURL": "http://mini.local:8317",
            "loginID": "claude-alice@example.com.json",
            "authIndex": "b799da203df01af5"
        }));
        assert_eq!(account.full_name(), "Claude · alice@example.com");
        assert_eq!(
            account.accounts_label(),
            "alice@example.com (mini.local:8317)"
        );
        let fetched = Utc::now();
        let snapshot = ProviderSnapshot {
            provider_id: account.card_id(),
            display_name: "Claude".into(),
            plan: None,
            lines: vec![],
            breakdowns: Default::default(),
            notice: None,
            refreshed_at: fetched,
        };
        let mut snapshots = BTreeMap::new();
        snapshots.insert(account.card_id(), snapshot);
        let encoded: serde_json::Value =
            serde_json::from_str(&encode(&[account.clone()], &snapshots, fetched)).unwrap();
        assert_eq!(
            encoded["providers"][&account.card_id()]["displayName"],
            "Claude · alice@example.com"
        );
    }
}
