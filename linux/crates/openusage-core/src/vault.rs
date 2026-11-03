//! Sign-ins live in the desktop keyring (the freedesktop Secret Service: GNOME Keyring, KWallet, …),
//! through libsecret's `secret-tool`, under the service the Mac app uses for its Keychain items. Secrets
//! travel over stdin and stdout only, never as arguments.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::os::fd::AsRawFd;
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::Duration;

use uuid::Uuid;

use crate::account::{Credential, OAuthTokens};
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
    /// Waits before asking again when a lookup finds nothing. Right after login the keyring daemon
    /// answers before it has loaded its collection, and a lookup then misses an item that is there;
    /// reading that as a lost sign-in asked the user to sign in again after every reboot.
    pub miss_retries: Vec<Duration>,
}

impl Default for SecretToolVault {
    fn default() -> Self {
        Self {
            executable: "secret-tool".into(),
            service: SERVICE.into(),
            miss_retries: [1, 2, 4].map(Duration::from_secs).to_vec(),
        }
    }
}

enum Lookup {
    Found(Vec<u8>),
    Missing,
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
                stdin
                    .write_all(input)
                    .map_err(|error| ProviderError::Vault(error.to_string()))?;
            }
        }
        let output = child
            .wait_with_output()
            .map_err(|error| ProviderError::Vault(error.to_string()))?;
        Ok(Output {
            status: output.status.code().unwrap_or(-1),
            stdout: output.stdout,
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
        })
    }

    fn lookup(&self, account: &str) -> Result<Lookup, ProviderError> {
        let output = self.run(
            &["lookup", "service", &self.service, "account", account],
            None,
        )?;
        match output.status {
            0 => Ok(Lookup::Found(output.stdout)),
            // `lookup` exits 1 without a message when nothing matches, and also while the keyring
            // is still loading or stays locked.
            1 if output.stderr.is_empty() => Ok(Lookup::Missing),
            _ => Err(Self::failure(&output)),
        }
    }

    fn failure(output: &Output) -> ProviderError {
        let line = output.stderr.lines().next().unwrap_or("").to_string();
        ProviderError::Vault(if line.is_empty() {
            format!("secret-tool exited {}", output.status)
        } else {
            line
        })
    }
}

impl Vault for SecretToolVault {
    fn load(&self, id: Uuid) -> Result<Option<Credential>, ProviderError> {
        let account = id.to_string().to_uppercase();
        let mut waits = self.miss_retries.iter();
        let data = loop {
            match self.lookup(&account)? {
                Lookup::Found(data) => break data,
                Lookup::Missing => match waits.next() {
                    Some(wait) => std::thread::sleep(*wait),
                    None => return Ok(None),
                },
            }
        };
        // An item with an empty secret holds no sign-in: report it missing, so the account asks to
        // sign in again instead of showing a keyring error.
        if data.iter().all(u8::is_ascii_whitespace) {
            crate::log::warn(
                "auth:accounts",
                &format!("keyring item for {account} is empty; treating the sign-in as missing"),
            );
            return Ok(None);
        }
        serde_json::from_slice(&data)
            .map(Some)
            .map_err(|_| ProviderError::Vault("the stored sign-in is unreadable".into()))
    }

    fn save(&self, id: Uuid, credential: &Credential) -> Result<(), ProviderError> {
        let account = id.to_string().to_uppercase();
        let data = serde_json::to_vec(credential)
            .map_err(|error| ProviderError::Vault(error.to_string()))?;
        let label = format!("--label=OpenUsage account {account}");
        let output = self.run(
            &[
                "store",
                &label,
                "service",
                &self.service,
                "account",
                &account,
            ],
            Some(&data),
        )?;
        if output.status == 0 {
            Ok(())
        } else {
            Err(Self::failure(&output))
        }
    }

    fn delete(&self, id: Uuid) -> Result<(), ProviderError> {
        let account = id.to_string().to_uppercase();
        let output = self.run(
            &["clear", "service", &self.service, "account", &account],
            None,
        )?;
        // `clear` exits 1 when nothing matched, which leaves the account as deleted as asked.
        if output.status == 0 || (output.status == 1 && output.stderr.is_empty()) {
            Ok(())
        } else {
            Err(Self::failure(&output))
        }
    }
}

/// Held while one process renews an account's OAuth sign-in. The Waybar module, `openusage refresh`,
/// and the panel's claims all renew, and a provider that rotates refresh tokens refuses the old one
/// once another process has spent it, which read as an expired sign-in.
pub struct RenewalLock(Option<File>);

impl RenewalLock {
    /// Waits for any other process renewing this account. Without a lock file the renewal goes ahead
    /// unguarded, as before.
    pub fn acquire(id: Uuid) -> Self {
        let directory = crate::paths::runtime_dir();
        let path = directory.join(format!("renew-{}.lock", id.to_string().to_lowercase()));
        let opened = std::fs::create_dir_all(&directory).and_then(|()| {
            OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .open(&path)
        });
        match opened {
            Ok(file) if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) } == 0 => {
                Self(Some(file))
            }
            Ok(_) | Err(_) => {
                crate::log::warn(
                    "auth",
                    &format!(
                        "couldn't lock {}; renewing without it: {}",
                        path.display(),
                        std::io::Error::last_os_error()
                    ),
                );
                Self(None)
            }
        }
    }
}

impl Drop for RenewalLock {
    fn drop(&mut self) {
        if let Some(file) = self.0.take() {
            unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_UN) };
        }
    }
}

/// The stored tokens, when another process renewed them after `stale` was read. Call it while
/// holding the account's `RenewalLock`, and use what it returns instead of renewing again.
pub fn renewed_elsewhere(vault: &dyn Vault, id: Uuid, stale: &OAuthTokens) -> Option<OAuthTokens> {
    match vault.load(id) {
        Ok(Some(Credential::OAuth(stored)))
            if stored.access_token != stale.access_token
                || stored.refresh_token != stale.refresh_token =>
        {
            Some(stored)
        }
        _ => None,
    }
}

/// Holds sign-ins in memory: tests, and a new sign-in proving itself before it is saved.
#[cfg(test)]
mod tests {
    use super::*;
    use crate::account::OAuthTokens;

    /// Runs against the real Secret Service under a throwaway service name; skipped without a keyring.
    #[test]
    fn round_trips_a_large_oauth_credential_through_secret_tool() {
        let vault = SecretToolVault {
            service: format!("com.robinebers.openusage.test.{}", Uuid::new_v4()),
            ..Default::default()
        };
        let id = Uuid::new_v4();
        let mut tokens = OAuthTokens::new("a".repeat(1800));
        tokens.refresh_token = Some("r".repeat(400));
        tokens
            .extras
            .insert(OAuthTokens::IDENTITY_KEY.into(), "acct:org".into());
        let credential = Credential::OAuth(tokens);
        if vault.save(id, &credential).is_err() {
            eprintln!("no usable keyring here; skipping");
            return;
        }
        assert_eq!(vault.load(id).unwrap(), Some(credential));
        vault.delete(id).unwrap();
        assert_eq!(vault.load(id).unwrap(), None);
    }

    /// A stand-in `secret-tool` whose lookups miss `misses` times, then find the item.
    fn flaky_secret_tool(misses: u32, item: &str) -> (std::path::PathBuf, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("openusage-vault-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let count = dir.join("count");
        let item_file = dir.join("item");
        std::fs::write(&item_file, item).unwrap();
        let script = dir.join("secret-tool");
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\nn=$(cat '{count}' 2>/dev/null || echo 0)\necho $((n + 1)) > '{count}'\n\
                 [ \"$n\" -lt {misses} ] && exit 1\ncat '{item}'\n",
                count = count.display(),
                item = item_file.display(),
            ),
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        (script, count)
    }

    #[test]
    fn a_lookup_that_misses_while_the_keyring_loads_asks_again() {
        let credential = Credential::ApiKey("sk-test".into());
        let json = serde_json::to_string(&credential).unwrap();
        let (script, count) = flaky_secret_tool(2, &json);
        let vault = SecretToolVault {
            executable: script.display().to_string(),
            service: "test".into(),
            miss_retries: vec![Duration::ZERO; 3],
        };
        assert_eq!(vault.load(Uuid::new_v4()).unwrap(), Some(credential));
        assert_eq!(std::fs::read_to_string(count).unwrap().trim(), "3");
    }

    #[test]
    fn a_lookup_that_keeps_missing_is_a_missing_sign_in() {
        let (script, count) = flaky_secret_tool(u32::MAX, "");
        let vault = SecretToolVault {
            executable: script.display().to_string(),
            service: "test".into(),
            miss_retries: vec![Duration::ZERO; 2],
        };
        assert_eq!(vault.load(Uuid::new_v4()).unwrap(), None);
        assert_eq!(std::fs::read_to_string(count).unwrap().trim(), "3");
    }

    #[test]
    fn renewed_elsewhere_returns_tokens_another_process_saved() {
        let id = Uuid::new_v4();
        let mut stale = OAuthTokens::new("old-access");
        stale.refresh_token = Some("old-refresh".into());
        let vault = MemoryVault::with(id, Credential::OAuth(stale.clone()));
        assert_eq!(renewed_elsewhere(&vault, id, &stale), None);
        let mut fresh = OAuthTokens::new("new-access");
        fresh.refresh_token = Some("new-refresh".into());
        vault.save(id, &Credential::OAuth(fresh.clone())).unwrap();
        let _lock = RenewalLock::acquire(id);
        assert_eq!(renewed_elsewhere(&vault, id, &stale), Some(fresh));
    }
}

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
