//! One module per provider, each a port of `Sources/OpenUsage/Providers/<Name>/` plus its sign-in.
//! `provider(kind)` and `login(kind, method)` are the only entry points the app uses. Each module exposes
//! `pub fn provider() -> Box<dyn Provider>` and `pub fn login(method: &LoginMethod) -> Box<dyn Login>`.

use crate::account::{Account, Credential};
use crate::error::{LoginError, ProviderError};
use crate::http::HttpClient;
use crate::kind::{AuthMethod, ProviderKind};
use crate::login::{LoginPrompt, LoginPrompter};
use crate::metric::ProviderUsage;
use crate::provider::{FetchContext, Login, LoginMethod, Provider, ProviderState};
use crate::vault::MemoryVault;

pub mod antigravity;
pub mod claude;
pub mod codex;
pub mod copilot;
pub mod cursor;
pub mod devin;
pub mod grok;
pub mod ollama;
pub mod opencode;
pub mod openrouter;
pub mod zai;

pub fn provider(kind: ProviderKind) -> Box<dyn Provider> {
    match kind {
        ProviderKind::Claude => claude::provider(),
        ProviderKind::Codex => codex::provider(),
        ProviderKind::Cursor => cursor::provider(),
        ProviderKind::Antigravity => antigravity::provider(),
        ProviderKind::Copilot => copilot::provider(),
        ProviderKind::Devin => devin::provider(),
        ProviderKind::Grok => grok::provider(),
        ProviderKind::Ollama => ollama::provider(),
        ProviderKind::OpenCode => opencode::provider(),
        ProviderKind::OpenRouter => openrouter::provider(),
        ProviderKind::Zai => zai::provider(),
    }
}

pub fn login(kind: ProviderKind, method: &LoginMethod) -> Box<dyn Login> {
    match kind {
        ProviderKind::Claude => claude::login(method),
        ProviderKind::Codex => codex::login(method),
        ProviderKind::Cursor => cursor::login(method),
        ProviderKind::Antigravity => antigravity::login(method),
        ProviderKind::Copilot => copilot::login(method),
        ProviderKind::Devin => devin::login(method),
        ProviderKind::Grok => grok::login(method),
        ProviderKind::Ollama => ollama::login(method),
        ProviderKind::OpenCode => opencode::login(method),
        ProviderKind::OpenRouter => openrouter::login(method),
        ProviderKind::Zai => zai::login(method),
    }
}

/// Reads one account with `credential` held in memory, without touching the keyring: an API-key sign-in
/// proves its key this way before it is saved (the Swift `APIKeyLogin`).
pub(crate) fn check_credential(
    kind: ProviderKind,
    credential: Credential,
    http: &dyn HttpClient,
) -> Result<(ProviderUsage, ProviderState), ProviderError> {
    let account = Account::new(kind, kind.display_name(), None);
    let vault = MemoryVault::with(account.id, credential);
    let mut state = ProviderState::default();
    let mut ctx = FetchContext { account: &account, http, vault: &vault, state: &mut state, now: chrono::Utc::now(), force: true };
    let usage = provider(kind).fetch(&mut ctx)?;
    Ok((usage, state))
}

/// The sign-in of a provider read with an API key: asks for the key, proves it with one read, and names
/// the account after the provider. A rejected key is a sign-in error, not a saved account.
pub(crate) struct ApiKeyLogin(pub ProviderKind);

impl ApiKeyLogin {
    /// Asks for the key and proves it; returns the key, the read, and the read's state (for a port that
    /// names the account from it, like Ollama's email).
    pub(crate) fn ask_and_check(
        &self,
        prompter: &dyn LoginPrompter,
        http: &dyn HttpClient,
    ) -> Result<(String, ProviderUsage, ProviderState), LoginError> {
        let key_page = match self.0.auth_methods().first() {
            Some(AuthMethod::ApiKey { key_page }) => Some(key_page.to_string()),
            _ => None,
        };
        prompter.show(&LoginPrompt::Secret { label: format!("{} API key", self.0.display_name()), key_page });
        let key = prompter.read_line(true).ok_or(LoginError::Cancelled)?.trim().to_string();
        if key.is_empty() {
            return Err(LoginError::InvalidCode);
        }
        match check_credential(self.0, Credential::ApiKey(key.clone()), http) {
            Ok((usage, state)) => Ok((key, usage, state)),
            Err(ProviderError::KeyRejected(message)) => Err(LoginError::Failed(message)),
            Err(error) => Err(LoginError::Provider(error)),
        }
    }
}

impl Login for ApiKeyLogin {
    fn login(&self, prompter: &dyn LoginPrompter, http: &dyn HttpClient) -> Result<(String, Credential), LoginError> {
        let (key, _, _) = self.ask_and_check(prompter, http)?;
        Ok((self.0.display_name().to_string(), Credential::ApiKey(key)))
    }
}
