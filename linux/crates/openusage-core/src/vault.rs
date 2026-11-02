//! Sign-ins live in the desktop keyring (the freedesktop Secret Service: GNOME Keyring, KWallet, …),
//! through libsecret's `secret-tool`, under the service the Mac app uses for its Keychain items. Secrets
//! travel over stdin and stdout only, never as arguments.

use std::io::Write;
use std::process::{Command, Stdio};
use std::sync::Mutex;

use uuid::Uuid;

use crate::account::Credential;
use crate::error::ProviderError;

pub const SERVICE: &str = "com.robinebers.openusage.accounts";

pub trait Vault: Send + Sync {
    fn load(&self, id: Uuid) -> Result<Option<Credential>, ProviderError>;
    fn save(&self, id: Uuid, credential: &Credential) -> Result<(), ProviderError>;
    fn delete(&self, id: Uuid) -> Result<(), ProviderError>;
}

pub struct SecretToolVault {
    pub executable: String,
    pub service: String,
}

impl Default for SecretToolVault {
    fn default() -> Self {
        Self { executable: "secret-tool".into(), service: SERVICE.into() }
    }
}

struct Output {
    status: i32,
    stdout: Vec<u8>,
    stderr: String,
}

impl SecretToolVault {
    fn run(&self, args: &[&str], input: Option<&[u8]>) -> Result<Output, ProviderError> {
        let mut child = Command::new(&self.executable)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|_| {
                ProviderError::Vault(
                    "OpenUsage needs `secret-tool` (libsecret) and a running keyring such as GNOME Keyring".into(),
                )
            })?;
        if let Some(mut stdin) = child.stdin.take() {
            if let Some(input) = input {
                stdin.write_all(input).map_err(|error| ProviderError::Vault(error.to_string()))?;
            }
        }
        let output = child.wait_with_output().map_err(|error| ProviderError::Vault(error.to_string()))?;
        Ok(Output {
            status: output.status.code().unwrap_or(-1),
            stdout: output.stdout,
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
        })
    }

    fn failure(output: &Output) -> ProviderError {
        let line = output.stderr.lines().next().unwrap_or("").to_string();
        ProviderError::Vault(if line.is_empty() { format!("secret-tool exited {}", output.status) } else { line })
    }
}

impl Vault for SecretToolVault {
    fn load(&self, id: Uuid) -> Result<Option<Credential>, ProviderError> {
        let account = id.to_string().to_uppercase();
        let output = self.run(&["lookup", "service", &self.service, "account", &account], None)?;
        match output.status {
            0 => serde_json::from_slice(&output.stdout)
                .map(Some)
                .map_err(|_| ProviderError::Vault("the stored sign-in is unreadable".into())),
            // `lookup` exits 1 without a message when nothing matches.
            1 if output.stderr.is_empty() => Ok(None),
            _ => Err(Self::failure(&output)),
        }
    }

    fn save(&self, id: Uuid, credential: &Credential) -> Result<(), ProviderError> {
        let account = id.to_string().to_uppercase();
        let data = serde_json::to_vec(credential).map_err(|error| ProviderError::Vault(error.to_string()))?;
        let label = format!("--label=OpenUsage account {account}");
        let output = self.run(&["store", &label, "service", &self.service, "account", &account], Some(&data))?;
        if output.status == 0 { Ok(()) } else { Err(Self::failure(&output)) }
    }

    fn delete(&self, id: Uuid) -> Result<(), ProviderError> {
        let account = id.to_string().to_uppercase();
        let output = self.run(&["clear", "service", &self.service, "account", &account], None)?;
        // `clear` exits 1 when nothing matched, which leaves the account as deleted as asked.
        if output.status == 0 || (output.status == 1 && output.stderr.is_empty()) { Ok(()) } else { Err(Self::failure(&output)) }
    }
}

/// Holds sign-ins in memory: tests, and a new sign-in proving itself before it is saved.
#[derive(Default)]
pub struct MemoryVault {
    items: Mutex<std::collections::HashMap<Uuid, Credential>>,
}

impl MemoryVault {
    pub fn with(id: Uuid, credential: Credential) -> Self {
        let vault = Self::default();
        vault.items.lock().unwrap().insert(id, credential);
        vault
    }
}

impl Vault for MemoryVault {
    fn load(&self, id: Uuid) -> Result<Option<Credential>, ProviderError> {
        Ok(self.items.lock().unwrap().get(&id).cloned())
    }

    fn save(&self, id: Uuid, credential: &Credential) -> Result<(), ProviderError> {
        self.items.lock().unwrap().insert(id, credential.clone());
        Ok(())
    }

    fn delete(&self, id: Uuid) -> Result<(), ProviderError> {
        self.items.lock().unwrap().remove(&id);
        Ok(())
    }
}
