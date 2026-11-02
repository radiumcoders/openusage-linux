//! CLIProxyAPI hubs: a port of `Models/CLIProxyHub.swift`, `Services/CLIProxyHubClient.swift`, and
//! `Stores/CLIProxyHubSync.swift`. A hub is not a provider: each supported login on it becomes an ordinary
//! account whose `hub` link names the hub, read through the hub's management API. Hubs are stored under
//! the Mac key `cliproxyHubs.v1`; a hub's management key lives in the keyring under the hub's id.

mod sync;
mod tunnel;

pub(crate) use sync::{mirror, sync_due};

use chrono::{DateTime, Utc};
use plist::Value;
use serde::{Deserialize, Serialize};
use serde_json::Value as Json;
use uuid::Uuid;

use crate::account::{swift_date, Credential};
use crate::error::ProviderError;
use crate::http::HttpClient;
use crate::kind::ProviderKind;
use crate::log;
use crate::store::Settings;
use crate::vault::Vault;

pub use tunnel::HubGate;
pub(crate) use tunnel::{account_failure, http_for};

/// What a mirrored account stores in place of a provider token. The hub substitutes its own.
pub const TOKEN_PLACEHOLDER: &str = "$TOKEN$";
pub const HUBS_KEY: &str = "cliproxyHubs.v1";

#[derive(Debug, Clone, PartialEq)]
pub struct Hub {
    pub id: Uuid,
    /// The hub's base URL, with the panel path and query removed.
    pub url: String,
    /// Host shown next to its mirrored accounts.
    pub host: String,
}

/// Where a mirrored account's login lives. `login_id` is the hub's file name; `auth_index` is what
/// `api-call` takes, and it changes when the hub renames the file. JSON keys match Swift's `HubLink`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HubLink {
    pub hub_id: Uuid,
    pub hub_url: String,
    pub login_id: String,
    pub auth_index: String,
}

impl HubLink {
    pub fn from_value(value: &Json) -> Option<Self> {
        let hub_id = Uuid::parse_str(value.get("hubID")?.as_str()?).ok()?;
        let hub_url = value.get("hubURL")?.as_str()?.to_string();
        let login_id = value.get("loginID")?.as_str()?.to_string();
        let auth_index = value.get("authIndex")?.as_str()?.to_string();
        if hub_url.is_empty() || login_id.is_empty() || auth_index.is_empty() {
            return None;
        }
        Some(Self {
            hub_id,
            hub_url,
            login_id,
            auth_index,
        })
    }

    pub fn to_value(&self) -> Json {
        serde_json::json!({
            "hubID": self.hub_id.to_string().to_ascii_uppercase(),
            "hubURL": self.hub_url,
            "loginID": self.login_id,
            "authIndex": self.auth_index,
        })
    }

    pub fn host(&self) -> String {
        host_of(&self.hub_url)
    }

    fn from_login(hub: &StoredHub, login: &HubLogin) -> Self {
        Self {
            hub_id: hub.id,
            hub_url: hub.url.clone(),
            login_id: login.id.clone(),
            auth_index: login.auth_index.clone(),
        }
    }
}

/// One entry of `GET /v0/management/auth-files`, reduced to what OpenUsage reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HubLogin {
    pub id: String,
    pub auth_index: String,
    pub provider: String,
    pub label: String,
    pub is_oauth: bool,
    pub is_disabled: bool,
    pub chatgpt_account_id: Option<String>,
}

impl HubLogin {
    /// The OpenUsage provider a login is mirrored as. An API key, or a provider without a session
    /// login here, is not mirrored.
    pub(crate) fn kind(&self) -> Option<ProviderKind> {
        if !self.is_oauth {
            return None;
        }
        Some(match self.provider.as_str() {
            "claude" => ProviderKind::Claude,
            "codex" => ProviderKind::Codex,
            "antigravity" => ProviderKind::Antigravity,
            "devin" => ProviderKind::Devin,
            "xai" => ProviderKind::Grok,
            _ => return None,
        })
    }
}

#[derive(Debug, Deserialize)]
struct Listing {
    files: Vec<Entry>,
}

#[derive(Debug, Deserialize)]
struct Entry {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    auth_index: Option<String>,
    #[serde(default)]
    provider: Option<String>,
    #[serde(default, rename = "type")]
    entry_type: Option<String>,
    #[serde(default)]
    email: Option<String>,
    #[serde(default)]
    account: Option<String>,
    #[serde(default)]
    label: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    account_type: Option<String>,
    #[serde(default)]
    disabled: Option<bool>,
    #[serde(default)]
    id_token: Option<IdToken>,
}

#[derive(Debug, Deserialize)]
struct IdToken {
    #[serde(default)]
    chatgpt_account_id: Option<String>,
}

/// The listing's logins in hub order. An entry without an id or an auth index can't be called and is
/// skipped. An undecodable body is an error.
pub(crate) fn parse_logins(body: &[u8]) -> Result<Vec<HubLogin>, ()> {
    let listing: Listing = serde_json::from_slice(body).map_err(|_| ())?;
    Ok(listing
        .files
        .into_iter()
        .filter_map(HubLogin::from_entry)
        .collect())
}

impl HubLogin {
    fn from_entry(entry: Entry) -> Option<Self> {
        let id = entry.id.filter(|id| !id.is_empty())?;
        let auth_index = entry.auth_index.filter(|index| !index.is_empty())?;
        let provider = entry
            .provider
            .or(entry.entry_type)
            .unwrap_or_default()
            .to_ascii_lowercase();
        let label = [entry.email, entry.account, entry.label, entry.name]
            .into_iter()
            .flatten()
            .map(|text| text.trim().to_string())
            .find(|text| !text.is_empty())
            .unwrap_or_else(|| id.clone());
        let account_type = entry
            .account_type
            .unwrap_or_else(|| "oauth".into())
            .to_ascii_lowercase();
        Some(Self {
            id,
            auth_index,
            provider,
            label,
            is_oauth: account_type == "oauth",
            is_disabled: entry.disabled.unwrap_or(false),
            chatgpt_account_id: entry.id_token.and_then(|token| token.chatgpt_account_id),
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StoredHub {
    #[serde(with = "uppercase_uuid")]
    id: Uuid,
    url: String,
    #[serde(with = "swift_date")]
    added_at: DateTime<Utc>,
}

impl StoredHub {
    fn view(&self) -> Hub {
        Hub {
            id: self.id,
            url: self.url.clone(),
            host: self.host(),
        }
    }

    fn host(&self) -> String {
        host_of(&self.url)
    }
}

mod uppercase_uuid {
    use serde::{Deserialize, Deserializer, Serializer};
    use uuid::Uuid;

    pub fn serialize<S: Serializer>(id: &Uuid, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&id.to_string().to_ascii_uppercase())
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Uuid, D::Error> {
        let text = String::deserialize(deserializer)?;
        Uuid::parse_str(&text).map_err(serde::de::Error::custom)
    }
}

/// The hub's base URL from what the user typed. The scheme defaults to http. The query, the fragment,
/// a user name, and a pasted panel or API path (`/management.html`, `/v0/management`, `/v1`) are
/// dropped. A path the hub sits under behind a reverse proxy is kept.
pub fn base_url(input: &str) -> Option<String> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return None;
    }
    let text = if trimmed.contains("://") {
        trimmed.to_string()
    } else {
        format!("http://{trimmed}")
    };
    let url = url::Url::parse(&text).ok()?;
    let scheme = url.scheme().to_ascii_lowercase();
    if scheme != "http" && scheme != "https" {
        return None;
    }
    let host = url.host_str().filter(|host| !host.is_empty())?;
    let host = if host.contains(':') {
        format!("[{host}]")
    } else {
        host.to_string()
    };
    let port = url
        .port()
        .map(|port| format!(":{port}"))
        .unwrap_or_default();
    let path = strip_pasted_path(url.path());
    let path = if path.is_empty() || path == "/" {
        ""
    } else {
        path.as_str()
    };
    Some(format!("{scheme}://{host}{port}{path}"))
}

/// Swift strips each suffix to completion, then moves on. A suffix that appears only after a later
/// one was removed is left in place.
fn strip_pasted_path(path: &str) -> String {
    let mut path = path.to_string();
    for suffix in [
        "/",
        "/management.html",
        "/v0/management",
        "/v8/management",
        "/v1",
    ] {
        loop {
            if path.len() >= suffix.len() && path.to_ascii_lowercase().ends_with(suffix) {
                path.truncate(path.len() - suffix.len());
            } else {
                break;
            }
        }
    }
    path
}

pub fn host_of(url: &str) -> String {
    let Ok(parsed) = url::Url::parse(url) else {
        return url.to_string();
    };
    match (parsed.host_str(), parsed.port()) {
        (Some(host), Some(port)) if host.contains(':') => format!("[{host}]:{port}"),
        (Some(host), Some(port)) => format!("{host}:{port}"),
        (Some(host), None) => host.to_string(),
        _ => url.to_string(),
    }
}

fn same_place(stored: &str, normalized: &str) -> bool {
    stored == normalized || base_url(stored).as_deref() == Some(normalized)
}

/// The connected hubs, in the order they were added.
pub fn list(settings: &Settings) -> Vec<Hub> {
    load_stored(settings).iter().map(StoredHub::view).collect()
}

/// Connects a hub once its login list answers to the key, then mirrors those logins as accounts.
/// A wrong address or key saves nothing.
pub fn connect(
    url: &str,
    management_key: &str,
    http: &dyn HttpClient,
    vault: &dyn Vault,
    settings: &Settings,
) -> Result<Hub, ProviderError> {
    let Some(url) = base_url(url) else {
        return Err(ProviderError::Unavailable(
            "Enter the hub\u{2019}s address, like http://192.168.1.20:8317.".into(),
        ));
    };
    let key = management_key.trim();
    if key.is_empty() {
        return Err(ProviderError::Unavailable(
            "Enter the hub\u{2019}s management key.".into(),
        ));
    }
    if load_stored(settings)
        .iter()
        .any(|hub| same_place(&hub.url, &url))
    {
        return Err(ProviderError::Unavailable(format!(
            "The hub at {} is already connected.",
            host_of(&url)
        )));
    }
    let stored = StoredHub {
        id: Uuid::new_v4(),
        url: url.clone(),
        added_at: Utc::now(),
    };
    let logins = tunnel::fetch_logins(http, &HubGate::default(), stored.id, &stored.url, key)?;
    if let Some(existing) = load_stored(settings)
        .into_iter()
        .find(|hub| same_place(&hub.url, &url))
    {
        return Ok(existing.view());
    }
    vault.save(stored.id, &Credential::ApiKey(key.to_string()))?;
    let mut hubs = load_stored(settings);
    hubs.push(stored.clone());
    save_hubs(settings, &hubs)?;
    mirror(settings, vault, stored.id, &logins)?;
    Ok(stored.view())
}

/// Saves a new management key only once the hub accepts it, then lets that hub be called again.
/// The check uses a fresh gate id, so a key the hub already rejected does not block the probe.
pub fn replace_key(
    hub_id: Uuid,
    management_key: &str,
    http: &dyn HttpClient,
    vault: &dyn Vault,
    settings: &Settings,
    gate: &HubGate,
) -> Result<(), ProviderError> {
    let Some(hub) = load_stored(settings)
        .into_iter()
        .find(|hub| hub.id == hub_id)
    else {
        return Ok(());
    };
    let key = management_key.trim();
    if key.is_empty() {
        return Err(ProviderError::Unavailable(
            "Enter the hub\u{2019}s management key.".into(),
        ));
    }
    let logins = tunnel::fetch_logins(http, gate, Uuid::new_v4(), &hub.url, key)?;
    vault.save(hub_id, &Credential::ApiKey(key.to_string()))?;
    gate.reopen(hub_id);
    mirror(settings, vault, hub_id, &logins)?;
    Ok(())
}

/// Removes a hub, every account mirrored from it, and its management key.
pub fn disconnect(id: Uuid, vault: &dyn Vault, settings: &Settings) -> Result<(), ProviderError> {
    let mirrored: Vec<Uuid> = settings
        .accounts()
        .into_iter()
        .filter(|account| account.hub_link().is_some_and(|link| link.hub_id == id))
        .map(|account| account.id)
        .collect();
    for account_id in &mirrored {
        vault.delete(*account_id)?;
    }
    vault.delete(id)?;
    settings
        .update_accounts(|accounts| accounts.retain(|account| !mirrored.contains(&account.id)))
        .map_err(store_error)?;
    let hubs: Vec<StoredHub> = load_stored(settings)
        .into_iter()
        .filter(|hub| hub.id != id)
        .collect();
    save_hubs(settings, &hubs)?;
    Ok(())
}

fn load_stored(settings: &Settings) -> Vec<StoredHub> {
    let Some(value) = settings.get(HUBS_KEY) else {
        return Vec::new();
    };
    let Some(data) = value.into_data() else {
        log::error(
            "auth:cliproxy",
            "cannot read the stored hub list, which is not data",
        );
        return Vec::new();
    };
    match serde_json::from_slice(&data) {
        Ok(hubs) => hubs,
        Err(error) => {
            log::error(
                "auth:cliproxy",
                &format!("cannot read the stored hub list: {error}"),
            );
            Vec::new()
        }
    }
}

fn save_hubs(settings: &Settings, hubs: &[StoredHub]) -> Result<(), ProviderError> {
    let data =
        serde_json::to_vec(hubs).map_err(|error| ProviderError::Unavailable(error.to_string()))?;
    settings
        .update(|values| {
            values.insert(HUBS_KEY.to_string(), Value::Data(data));
        })
        .map_err(store_error)
}

fn store_error(error: std::io::Error) -> ProviderError {
    ProviderError::Unavailable(format!("OpenUsage couldn't save the hub list ({error})."))
}

#[cfg(test)]
mod tests;
