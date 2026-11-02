//! The management API and the per-account tunnel. A port of `CLIProxyHubClient`, `HubRefusal`,
//! `CLIProxyHubGate`, and `HubTunnel`. Provider modules keep calling `HttpClient`; a hub account's
//! client wraps each call in `POST /v0/management/api-call` and unwraps the upstream answer.
//! One hub is called one request at a time. A rejected key or an IP ban is not sent again.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use uuid::Uuid;

use super::{host_of, HubLink};
use crate::account::{Account, Credential};
use crate::error::ProviderError;
use crate::http::{HttpClient, HttpRequest, HttpResponse};
use crate::kind::ProviderKind;
use crate::log;
use crate::vault::Vault;

const MANAGEMENT_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Refusal {
    KeyRejected,
    Banned,
    RemoteManagementOff,
    ManagementOff,
    LoginMissing,
    NoToken,
    UpstreamUnreachable,
    Failed(u16),
}

impl Refusal {
    fn from_response(response: &HttpResponse) -> Option<Self> {
        if (200..300).contains(&response.status) {
            return None;
        }
        let message = error_text(&response.body);
        Some(match response.status {
            401 => Self::KeyRejected,
            403 if message.starts_with("ip banned") => Self::Banned,
            403 if message.contains("remote management disabled") => Self::RemoteManagementOff,
            404 => Self::ManagementOff,
            400 if message.contains("auth credential not found") => Self::LoginMissing,
            400 if message.contains("token") => Self::NoToken,
            502 => Self::UpstreamUnreachable,
            status => Self::Failed(status),
        })
    }

    /// A repeat would be another strike toward the hub's IP ban.
    fn closes_gate(self) -> bool {
        matches!(self, Self::KeyRejected | Self::Banned)
    }

    fn label(self) -> &'static str {
        match self {
            Self::KeyRejected => "key rejected",
            Self::Banned => "ip banned",
            Self::RemoteManagementOff => "remote management off",
            Self::ManagementOff => "management off",
            Self::LoginMissing => "login missing",
            Self::NoToken => "no token",
            Self::UpstreamUnreachable => "upstream unreachable",
            Self::Failed(_) => "failed",
        }
    }

    fn error(self, host: &str, upstream: Option<&str>) -> ProviderError {
        let message = match self {
            Self::KeyRejected => format!(
                "The CLIProxyAPI hub at {host} rejected the management key. Connect it again with the right key (`openusage hub remove`, then `openusage hub add`)."
            ),
            Self::Banned => format!(
                "The CLIProxyAPI hub at {host} blocked this computer after too many wrong keys. Wait 30 minutes, then connect it again with the right key."
            ),
            Self::RemoteManagementOff => format!(
                "The CLIProxyAPI hub at {host} only allows management from its own machine. Turn on allow-remote in its config."
            ),
            Self::ManagementOff => format!(
                "The CLIProxyAPI hub at {host} has its management API turned off. Set a management key in its config."
            ),
            Self::LoginMissing => format!("This login is no longer on the CLIProxyAPI hub at {host}."),
            Self::NoToken => no_token(host),
            Self::UpstreamUnreachable => format!(
                "The CLIProxyAPI hub at {host} couldn\u{2019}t reach {}.",
                upstream.unwrap_or("the provider")
            ),
            Self::Failed(status) => {
                format!("The CLIProxyAPI hub at {host} answered with an error (HTTP {status}).")
            }
        };
        match self {
            Self::UpstreamUnreachable => ProviderError::Unreachable(message),
            _ => ProviderError::Unavailable(message),
        }
    }
}

fn no_token(host: &str) -> String {
    format!(
        "The CLIProxyAPI hub at {host} has no working sign-in for this login. Sign it in again on the hub."
    )
}

fn missing_key(host: &str) -> String {
    format!(
        "The management key for the CLIProxyAPI hub at {host} is missing. Connect the hub again with `openusage hub add`."
    )
}

fn cant_reach(host: &str) -> String {
    format!("Can\u{2019}t reach the CLIProxyAPI hub at {host}.")
}

fn error_text(body: &[u8]) -> String {
    serde_json::from_slice::<serde_json::Value>(body)
        .ok()
        .and_then(|value| {
            value
                .get("error")?
                .as_str()
                .map(|text| text.to_ascii_lowercase())
        })
        .unwrap_or_default()
}

fn log_refusal(host: &str, path: &str, response: &HttpResponse, refusal: Refusal) {
    let version = response
        .header("x-cpa-version")
        .unwrap_or("version unknown");
    log::error(
        "auth:cliproxy",
        &format!(
            "{host} {path} -> HTTP {} ({}, hub {version})",
            response.status,
            refusal.label()
        ),
    );
}

/// Keeps a wrong management key to one strike per hub. Calls for one hub go one at a time; after a
/// rejected key or an IP ban, later calls with that same key fail without being sent. A refusal is
/// stored for the key it was sent with, so a late answer for a replaced key does not stop the new one.
#[derive(Default)]
pub struct HubGate {
    slots: Mutex<BTreeMap<Uuid, Arc<Mutex<()>>>>,
    refused: Mutex<BTreeMap<Uuid, (String, ProviderError)>>,
}

impl HubGate {
    pub(crate) fn reopen(&self, hub: Uuid) {
        lock(&self.refused).remove(&hub);
    }

    fn slot(&self, hub: Uuid) -> Arc<Mutex<()>> {
        lock(&self.slots)
            .entry(hub)
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
    }

    fn refusal(&self, hub: Uuid, key: &str) -> Option<ProviderError> {
        lock(&self.refused)
            .get(&hub)
            .filter(|(stored, _)| stored == key)
            .map(|(_, error)| error.clone())
    }

    fn refuse(&self, hub: Uuid, key: &str, error: ProviderError) {
        lock(&self.refused).insert(hub, (key.to_string(), error));
    }

    fn send(
        &self,
        hub: Uuid,
        host: &str,
        path: &str,
        key: &str,
        request: impl FnOnce() -> Result<HttpResponse, ProviderError>,
    ) -> Result<HttpResponse, ProviderError> {
        if let Some(error) = self.refusal(hub, key) {
            return Err(error);
        }
        let slot = self.slot(hub);
        let _busy = lock(&slot);
        if let Some(error) = self.refusal(hub, key) {
            return Err(error);
        }
        let response = request()?;
        if let Some(refusal) = Refusal::from_response(&response) {
            if refusal.closes_gate() {
                log_refusal(host, path, &response, refusal);
                let error = refusal.error(host, None);
                self.refuse(hub, key, error.clone());
                log::error(
                    "auth:cliproxy",
                    &format!(
                        "{host} {refusal}; not calling it again until its key is replaced",
                        refusal = refusal.label()
                    ),
                );
                return Err(error);
            }
        }
        Ok(response)
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

pub(crate) fn management(
    http: &dyn HttpClient,
    gate: &HubGate,
    method: &str,
    path: &str,
    hub_id: Uuid,
    hub_url: &str,
    key: &str,
    body: Option<Vec<u8>>,
    timeout: Duration,
    upstream: Option<&str>,
) -> Result<HttpResponse, ProviderError> {
    let host = host_of(hub_url);
    let url = format!("{}/v0/management/{path}", hub_url.trim_end_matches('/'));
    let mut request = HttpRequest::new(method, url)
        .header("Authorization", format!("Bearer {key}"))
        .header("Accept", "application/json");
    if body.is_some() {
        request = request.header("Content-Type", "application/json");
    }
    request.body = body;
    request.timeout = timeout;
    let response = gate.send(hub_id, &host, path, key, || match http.send(request) {
        Err(ProviderError::ConnectionFailed) => {
            log::warn("auth:cliproxy", &format!("{host} {path} unreachable"));
            Err(ProviderError::Unreachable(cant_reach(&host)))
        }
        other => other,
    })?;
    if let Some(refusal) = Refusal::from_response(&response) {
        log_refusal(&host, path, &response, refusal);
        return Err(refusal.error(&host, upstream));
    }
    Ok(response)
}

/// The upstream response inside an `api-call` envelope: `{status_code, header: {name: [value]}, body}`.
pub(crate) fn unwrap_api_call(body: &[u8]) -> Option<HttpResponse> {
    #[derive(serde::Deserialize)]
    struct Envelope {
        status_code: u16,
        #[serde(default)]
        header: BTreeMap<String, Vec<String>>,
        #[serde(default)]
        body: Option<String>,
    }
    let envelope: Envelope = serde_json::from_slice(body).ok()?;
    let mut headers = BTreeMap::new();
    for (name, values) in envelope.header {
        if values.is_empty() {
            continue;
        }
        headers.insert(name.to_ascii_lowercase(), values.join(", "));
    }
    Some(HttpResponse {
        status: envelope.status_code,
        headers,
        body: envelope.body.unwrap_or_default().into_bytes(),
    })
}

fn call(
    http: &dyn HttpClient,
    gate: &HubGate,
    request: &HttpRequest,
    link: &HubLink,
    key: &str,
    upstream: &str,
) -> Result<HttpResponse, ProviderError> {
    let mut payload = serde_json::Map::new();
    payload.insert(
        "auth_index".into(),
        serde_json::Value::String(link.auth_index.clone()),
    );
    payload.insert(
        "method".into(),
        serde_json::Value::String(request.method.clone()),
    );
    payload.insert("url".into(), serde_json::Value::String(request.url.clone()));
    payload.insert(
        "header".into(),
        serde_json::to_value(&request.headers).unwrap_or_else(|_| serde_json::json!({})),
    );
    if let Some(body) = &request.body {
        payload.insert(
            "data".into(),
            serde_json::Value::String(String::from_utf8_lossy(body).into_owned()),
        );
    }
    let bytes = serde_json::to_vec(&payload).map_err(|_| ProviderError::InvalidResponse)?;
    let response = management(
        http,
        gate,
        "POST",
        "api-call",
        link.hub_id,
        &link.hub_url,
        key,
        Some(bytes),
        request.timeout.saturating_add(Duration::from_secs(10)),
        Some(upstream),
    )?;
    match unwrap_api_call(&response.body) {
        Some(upstream_response) => Ok(upstream_response),
        None => {
            log::error(
                "auth:cliproxy",
                &format!(
                    "{} sent an api-call answer OpenUsage can't read",
                    link.host()
                ),
            );
            Err(ProviderError::InvalidResponse)
        }
    }
}

pub(crate) struct HubTunnel {
    link: HubLink,
    kind: ProviderKind,
    vault: Arc<dyn Vault>,
    http: Arc<dyn HttpClient>,
    gate: Arc<HubGate>,
}

impl HttpClient for HubTunnel {
    fn send(&self, request: HttpRequest) -> Result<HttpResponse, ProviderError> {
        let host = self.link.host();
        let key = match self.vault.load(self.link.hub_id) {
            Ok(Some(Credential::ApiKey(key))) if !key.is_empty() => key,
            Ok(_) => {
                log::error(
                    "auth:cliproxy",
                    &format!("{host} has no stored management key"),
                );
                return Err(ProviderError::Unavailable(missing_key(&host)));
            }
            Err(error) => return Err(error),
        };
        call(
            self.http.as_ref(),
            &self.gate,
            &request,
            &self.link,
            &key,
            self.kind.display_name(),
        )
    }
}

struct UnreadableHub;

impl HttpClient for UnreadableHub {
    fn send(&self, _request: HttpRequest) -> Result<HttpResponse, ProviderError> {
        Err(ProviderError::Unavailable(
            "This account's CLIProxyAPI hub link can't be read. Remove the account and connect the hub again.".into(),
        ))
    }
}

/// The account's HTTP client: direct, or the tunnel through its hub. The link must name a connected
/// hub, and requests always go to that hub's stored address, so an edited account entry can never send
/// a hub's management key anywhere else.
/// A link naming a hub that isn't connected: nothing is sent.
struct DisconnectedHub;

impl HttpClient for DisconnectedHub {
    fn send(&self, _request: HttpRequest) -> Result<HttpResponse, ProviderError> {
        Err(ProviderError::Unavailable(
            "This account's CLIProxyAPI hub isn't connected, so it has no management key. Connect the hub again with `openusage hub add`.".into(),
        ))
    }
}

pub(crate) fn http_for(
    account: &Account,
    hubs: &[super::Hub],
    http: Arc<dyn HttpClient>,
    vault: Arc<dyn Vault>,
    gate: Arc<HubGate>,
) -> Arc<dyn HttpClient> {
    match &account.hub {
        None => http,
        Some(value) => match HubLink::from_value(value).map(|mut link| {
            let hub = hubs.iter().find(|hub| hub.id == link.hub_id)?;
            link.hub_url = hub.url.clone();
            Some(link)
        }) {
            Some(None) => Arc::new(DisconnectedHub),
            Some(Some(link)) => Arc::new(HubTunnel {
                link,
                kind: account.kind,
                vault,
                http,
                gate,
            }),
            None => Arc::new(UnreadableHub),
        },
    }
}

/// A hub login has no refresh token. A refused sign-in is reported against the hub, the way
/// `AccountProviderRuntime` rewrites `signInExpired` for a mirrored account.
pub(crate) fn account_failure(account: &Account, error: ProviderError) -> ProviderError {
    let Some(host) = account.hub_host() else {
        return error;
    };
    match error {
        ProviderError::SignInExpired(_) | ProviderError::MissingCredential => {
            ProviderError::Unavailable(no_token(&host))
        }
        other => other,
    }
}

pub(crate) fn fetch_logins(
    http: &dyn HttpClient,
    gate: &HubGate,
    hub_id: Uuid,
    hub_url: &str,
    key: &str,
) -> Result<Vec<super::HubLogin>, ProviderError> {
    let response = management(
        http,
        gate,
        "GET",
        "auth-files",
        hub_id,
        hub_url,
        key,
        None,
        MANAGEMENT_TIMEOUT,
        None,
    )?;
    super::parse_logins(&response.body).map_err(|_| {
        let host = host_of(hub_url);
        log::error(
            "auth:cliproxy",
            &format!("{host} sent a login listing OpenUsage can't read"),
        );
        ProviderError::Unavailable(format!(
            "The CLIProxyAPI hub at {host} sent a login list OpenUsage can't read."
        ))
    })
}
