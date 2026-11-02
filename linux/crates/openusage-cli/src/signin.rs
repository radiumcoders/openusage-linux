//! Interactive `openusage add`. The provider drives the prompts; this is the terminal.

use std::io::{self, Write};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};

use openusage_core::account::{Account, Credential, OAuthTokens};
use openusage_core::daemon;
use openusage_core::error::LoginError;
use openusage_core::http::UreqClient;
use openusage_core::kind::{AuthMethod, ProviderKind};
use openusage_core::log;
use openusage_core::login::{LoginPrompt, LoginPrompter};
use openusage_core::provider::LoginMethod;
use openusage_core::providers;
use openusage_core::store::Settings;
use openusage_core::vault::{SecretToolVault, Vault};
use sha2::{Digest, Sha256};

use crate::text;

struct Echo {
    saved: std::cell::UnsafeCell<libc::termios>,
    hidden: AtomicBool,
}

unsafe impl Sync for Echo {}

static ECHO: Echo = Echo {
    saved: std::cell::UnsafeCell::new(unsafe { std::mem::zeroed() }),
    hidden: AtomicBool::new(false),
};

fn suppress_echo() {
    if unsafe { libc::isatty(libc::STDIN_FILENO) } == 0 {
        return;
    }
    let mut original = unsafe { std::mem::zeroed() };
    if unsafe { libc::tcgetattr(libc::STDIN_FILENO, &mut original) } != 0 {
        return;
    }
    unsafe { *ECHO.saved.get() = original };
    ECHO.hidden.store(true, Ordering::SeqCst);
    let mut hidden = original;
    hidden.c_lflag &= !libc::ECHO;
    unsafe { libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &hidden) };
}

fn restore_echo() {
    if ECHO.hidden.swap(false, Ordering::SeqCst) {
        unsafe { libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &*ECHO.saved.get()) };
    }
}

extern "C" fn on_sigint(_: i32) {
    restore_echo();
    let message = b"openusage: Sign-in cancelled.\n";
    unsafe { libc::write(libc::STDERR_FILENO, message.as_ptr() as *const libc::c_void, message.len()) };
    unsafe { libc::_exit(130) };
}

struct InterruptGuard;

impl InterruptGuard {
    fn install() -> Self {
        unsafe { libc::signal(libc::SIGINT, on_sigint as *const () as libc::sighandler_t) };
        Self
    }
}

impl Drop for InterruptGuard {
    fn drop(&mut self) {
        unsafe { libc::signal(libc::SIGINT, libc::SIG_DFL) };
        restore_echo();
    }
}

struct TerminalPrompter {
    stop: Arc<AtomicBool>,
    paste: Mutex<Option<JoinHandle<()>>>,
}

impl TerminalPrompter {
    fn new() -> Self {
        Self { stop: Arc::new(AtomicBool::new(false)), paste: Mutex::new(None) }
    }
}

impl Drop for TerminalPrompter {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(handle) = self.paste.lock().unwrap().take() {
            let _ = handle.join();
        }
    }
}

impl LoginPrompter for TerminalPrompter {
    fn show(&self, prompt: &LoginPrompt) {
        match prompt {
            LoginPrompt::Browser { url, accepts_pasted_code } => {
                text::line(url);
                open_browser(url);
                if *accepts_pasted_code {
                    text::note("Paste the authorization code and press Enter, or finish in the browser.");
                } else {
                    text::note("Finish sign-in in the browser.");
                }
            }
            LoginPrompt::BrowserToken { url } => {
                text::line(url);
                open_browser(url);
                if is_tty() {
                    text::prompt("Token: ");
                }
            }
            LoginPrompt::DeviceCode { user_code, verification_url } => {
                text::line(&format!("Code: {user_code}"));
                text::line(verification_url);
                open_browser(verification_url);
                text::note("Confirm the code in the browser.");
            }
            LoginPrompt::Secret { label, key_page } => {
                if let Some(page) = key_page {
                    text::line(page);
                    open_browser(page);
                }
                if is_tty() {
                    text::prompt(&secret_prompt(label));
                }
            }
        }
    }

    fn read_line(&self, secret: bool) -> Option<String> {
        let hide = secret && is_tty();
        if hide {
            suppress_echo();
        }
        let line = read_one_line(&self.stop);
        if hide {
            restore_echo();
            let _ = writeln!(io::stderr());
        }
        line.map(|line| line.trim().to_string())
    }

    fn pasted_codes(&self) -> Receiver<String> {
        let (sender, receiver) = mpsc::channel();
        let stop = Arc::clone(&self.stop);
        let handle = thread::spawn(move || {
            while !stop.load(Ordering::SeqCst) {
                match read_one_line(&stop) {
                    Some(line) => {
                        let trimmed = line.trim().to_string();
                        if !trimmed.is_empty() && sender.send(trimmed).is_err() {
                            break;
                        }
                    }
                    None => break,
                }
            }
        });
        *self.paste.lock().unwrap() = Some(handle);
        receiver
    }
}

fn secret_prompt(label: &str) -> String {
    let lower = label.to_ascii_lowercase();
    if lower.contains("api key") {
        "API key: ".into()
    } else if lower.contains("organization") {
        "Organization token: ".into()
    } else if lower.contains("token") {
        "Token: ".into()
    } else {
        format!("{label}: ")
    }
}

fn is_tty() -> bool {
    unsafe { libc::isatty(libc::STDIN_FILENO) != 0 }
}

fn read_one_line(stop: &AtomicBool) -> Option<String> {
    let mut buffer = Vec::new();
    let mut byte = [0u8; 1];
    while !stop.load(Ordering::SeqCst) {
        let mut poll = libc::pollfd { fd: libc::STDIN_FILENO, events: libc::POLLIN, revents: 0 };
        let ready = unsafe { libc::poll(&mut poll, 1, 200) };
        if ready == 0 {
            continue;
        }
        if ready < 0 {
            if io::Error::last_os_error().raw_os_error() == Some(libc::EINTR) {
                continue;
            }
            return None;
        }
        let count = unsafe { libc::read(libc::STDIN_FILENO, byte.as_mut_ptr() as *mut libc::c_void, 1) };
        if count == 0 {
            return if buffer.is_empty() { None } else { Some(String::from_utf8_lossy(&buffer).into_owned()) };
        }
        if count < 0 {
            if io::Error::last_os_error().raw_os_error() == Some(libc::EINTR) {
                continue;
            }
            return None;
        }
        if byte[0] == b'\n' {
            return Some(String::from_utf8_lossy(&buffer).into_owned());
        }
        if byte[0] != b'\r' {
            buffer.push(byte[0]);
        }
    }
    None
}

fn open_browser(url: &str) {
    match Command::new("xdg-open").arg(url).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn() {
        Ok(mut child) => {
            thread::spawn(move || {
                let _ = child.wait();
            });
        }
        Err(error) => {
            log::warn("auth", &format!("could not open a browser: {error}"));
            text::note("Could not open a browser. Use the URL above.");
        }
    }
}

pub fn add(provider: &str, organization: Option<String>) -> i32 {
    let Some(kind) = ProviderKind::from_raw(provider) else {
        let names = ProviderKind::ALL.iter().map(|kind| kind.raw()).collect::<Vec<_>>().join(", ");
        text::error(&format!("Unknown provider: {provider}. Expected {names}."));
        return 2;
    };
    let organization = organization.map(|name| name.trim().to_string());
    if let Some(name) = &organization {
        if kind != ProviderKind::Copilot {
            text::error("--org is only for a Copilot organization account.");
            return 2;
        }
        if name.is_empty() {
            text::error("--org needs an organization name.");
            return 2;
        }
    }
    let method = match &organization {
        Some(name) => LoginMethod::Organization(name.clone()),
        None => LoginMethod::Default,
    };
    let _interrupt = InterruptGuard::install();
    let prompter = TerminalPrompter::new();
    let http = UreqClient::new();
    match providers::login(kind, &method).login(&prompter, &http) {
        Ok((label, credential)) => save(kind, organization, label, credential),
        Err(LoginError::Cancelled) => cancelled(kind, organization.is_some()),
        Err(error) => {
            text::error(&error.to_string());
            1
        }
    }
}

fn cancelled(kind: ProviderKind, organization: bool) -> i32 {
    if organization {
        text::error("No organization token was entered.");
        return 1;
    }
    if matches!(kind.auth_methods().first(), Some(AuthMethod::ApiKey { .. })) {
        text::error("No API key was entered.");
        return 1;
    }
    if kind == ProviderKind::Devin {
        text::error("No token was entered.");
        return 1;
    }
    text::error("Sign-in was cancelled.");
    130
}

fn save(kind: ProviderKind, organization: Option<String>, label: String, credential: Credential) -> i32 {
    let settings = Settings::open();
    let vault = SecretToolVault::default();
    if let Err(message) = check_duplicate(&vault, &settings.accounts(), &credential, kind, organization.as_deref()) {
        text::error(&message);
        return 1;
    }
    let account = Account::new(kind, label, organization);
    if let Err(error) = vault.save(account.id, &credential) {
        text::error(&error.to_string());
        return 1;
    }
    let saved = account.clone();
    if let Err(error) = settings.update_accounts(|accounts| accounts.push(saved)) {
        let _ = vault.delete(account.id);
        text::error(&error.to_string());
        return 1;
    }
    text::line(&format!("Added {} ({}, {}).", account.card_id(), account.kind.display_name(), account.label));
    daemon::notify_accounts_changed();
    0
}

fn identity(credential: &Credential) -> Option<String> {
    match credential {
        Credential::OAuth(tokens) => tokens.extras.get(OAuthTokens::IDENTITY_KEY).cloned(),
        Credential::ApiKey(key) => {
            let digest = Sha256::digest(key.as_bytes());
            let hex: String = digest.iter().map(|byte| format!("{byte:02x}")).collect();
            Some(format!("key:{hex}"))
        }
    }
}

fn check_duplicate(
    vault: &dyn Vault,
    accounts: &[Account],
    credential: &Credential,
    kind: ProviderKind,
    organization: Option<&str>,
) -> Result<(), String> {
    let identity = identity(credential);
    for account in accounts {
        if account.kind != kind || account.hub.is_some() {
            continue;
        }
        if let Some(organization) = organization {
            if account.organization.as_deref().is_none_or(|name| !name.eq_ignore_ascii_case(organization)) {
                continue;
            }
        } else {
            if account.organization.is_some() {
                continue;
            }
            let Some(identity) = identity.as_deref() else { return Ok(()) };
            match vault.load(account.id) {
                Ok(Some(existing)) if self::identity(&existing).as_deref() == Some(identity) => {}
                Ok(_) => continue,
                Err(error) => {
                    log::error(
                        "auth",
                        &format!("{} could not check its keyring item for a duplicate sign-in: {error}", account.card_id()),
                    );
                    return Err(format!(
                        "OpenUsage couldn't check the saved sign-in for {}. Check Keychain access and try again.",
                        account.label
                    ));
                }
            }
        }
        return Err(format!("This account is already added as {}.", account.label));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use openusage_core::vault::MemoryVault;

    #[test]
    fn a_second_copy_of_an_api_key_is_the_same_account() {
        let existing = Account::new(ProviderKind::OpenRouter, "Personal", None);
        let key = "secret-key";
        let vault = MemoryVault::with(existing.id, Credential::ApiKey(key.into()));
        let again = Credential::ApiKey(key.into());
        let error = check_duplicate(&vault, &[existing.clone()], &again, ProviderKind::OpenRouter, None).unwrap_err();
        assert_eq!(error, "This account is already added as Personal.");
        let other = Credential::ApiKey("different".into());
        assert!(check_duplicate(&vault, &[existing], &other, ProviderKind::OpenRouter, None).is_ok());
    }

    #[test]
    fn an_oauth_sign_in_without_an_identity_is_not_checked() {
        let existing = Account::new(ProviderKind::Claude, "Work", None);
        let vault = MemoryVault::default();
        let fresh = Credential::OAuth(OAuthTokens::new("token"));
        assert!(check_duplicate(&vault, &[existing], &fresh, ProviderKind::Claude, None).is_ok());
    }

    #[test]
    fn prompt_words_follow_the_secret_label() {
        assert_eq!(secret_prompt("Claude API key"), "API key: ");
        assert_eq!(secret_prompt("Organization token"), "Organization token: ");
        assert_eq!(secret_prompt("Devin token"), "Token: ");
        assert_eq!(secret_prompt("Passphrase"), "Passphrase: ");
    }
}
