//! CLIProxyAPI hubs: a port of `Models/CLIProxyHub.swift`, `Services/CLIProxyHubClient.swift`, and
//! `Stores/CLIProxyHubSync.swift`. A hub is not a provider: each supported login on it becomes an ordinary
//! account whose `hub` link names the hub, read through the hub's management API. Hubs are stored under
//! the Mac key `cliproxyHubs.v1`; a hub's management key lives in the keyring under the hub's id.

use uuid::Uuid;

use crate::error::ProviderError;
use crate::http::HttpClient;
use crate::store::Settings;
use crate::vault::Vault;

#[derive(Debug, Clone, PartialEq)]
pub struct Hub {
    pub id: Uuid,
    /// The hub's base URL as the user entered it.
    pub url: String,
    /// Host shown next to its mirrored accounts.
    pub host: String,
}

/// The connected hubs. Not implemented yet: none.
pub fn list(_settings: &Settings) -> Vec<Hub> {
    vec![]
}

/// Connects a hub with its management key, saves it, and mirrors its logins as accounts. Not implemented yet.
pub fn connect(
    _url: &str,
    _management_key: &str,
    _http: &dyn HttpClient,
    _vault: &dyn Vault,
    _settings: &Settings,
) -> Result<Hub, ProviderError> {
    Err(ProviderError::Unavailable("CLIProxyAPI hubs aren't supported in this build yet.".into()))
}

/// Disconnects a hub, removing its mirrored accounts and its key. Not implemented yet.
pub fn disconnect(_id: Uuid, _vault: &dyn Vault, _settings: &Settings) -> Result<(), ProviderError> {
    Err(ProviderError::Unavailable("CLIProxyAPI hubs aren't supported in this build yet.".into()))
}
