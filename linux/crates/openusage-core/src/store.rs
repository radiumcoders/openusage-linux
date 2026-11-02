//! Process-safe storage. Accounts and settings live in `$XDG_CONFIG_HOME/openusage/settings.plist`, the
//! same property list (and the same keys) the Swift Linux edition wrote, so its accounts carry over. The
//! snapshot cache is JSON in `$XDG_STATE_HOME/openusage/snapshots.json`. Every write takes an exclusive
//! `flock` on a sidecar lock file, re-reads, edits, and renames a temporary file into place, so the
//! daemon and a CLI command never clobber each other's keys.

use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};

use plist::Value;

use crate::account::Account;
use crate::log;
use crate::metric::ProviderSnapshot;
use crate::paths;

pub const ACCOUNTS_KEY: &str = "accounts.v1";
pub const MENU_BAR_LIMIT_KEY: &str = "menuBarLimit";

struct Lock(File);

impl Lock {
    fn acquire(path: &Path, exclusive: bool) -> std::io::Result<Lock> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(path.with_extension("lock"))?;
        let operation = if exclusive {
            libc::LOCK_EX
        } else {
            libc::LOCK_SH
        };
        if unsafe { libc::flock(file.as_raw_fd(), operation) } != 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(Lock(file))
    }
}

impl Drop for Lock {
    fn drop(&mut self) {
        unsafe { libc::flock(self.0.as_raw_fd(), libc::LOCK_UN) };
    }
}

fn write_atomically(path: &Path, data: &[u8]) -> std::io::Result<()> {
    let temporary = path.with_extension(format!("tmp.{}", std::process::id()));
    {
        let mut file = File::create(&temporary)?;
        file.write_all(data)?;
        file.sync_all()?;
    }
    fs::rename(&temporary, path)
}

/// The settings property list: accounts, layout, and settings, as a dictionary of keys.
#[derive(Clone)]
pub struct Settings {
    path: PathBuf,
}

impl Settings {
    pub fn open() -> Self {
        Self {
            path: paths::settings_file(),
        }
    }

    pub fn at(path: PathBuf) -> Self {
        Self { path }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn read_unlocked(&self) -> BTreeMap<String, Value> {
        let Ok(data) = fs::read(&self.path) else {
            return BTreeMap::new();
        };
        match plist::from_bytes::<Value>(&data) {
            Ok(Value::Dictionary(dictionary)) => dictionary.into_iter().collect(),
            Ok(_) | Err(_) => {
                log::error(
                    "config",
                    &format!(
                        "{} is not a property list dictionary; ignoring it",
                        self.path.display()
                    ),
                );
                BTreeMap::new()
            }
        }
    }

    pub fn read_all(&self) -> BTreeMap<String, Value> {
        match Lock::acquire(&self.path, false) {
            Ok(_lock) => self.read_unlocked(),
            Err(error) => {
                log::error("config", &format!("cannot lock settings: {error}"));
                self.read_unlocked()
            }
        }
    }

    pub fn get(&self, key: &str) -> Option<Value> {
        self.read_all().remove(key)
    }

    /// Re-reads, applies `edit`, and writes back, all under the exclusive lock.
    pub fn update(&self, edit: impl FnOnce(&mut BTreeMap<String, Value>)) -> std::io::Result<()> {
        let _lock = Lock::acquire(&self.path, true)?;
        let mut values = self.read_unlocked();
        edit(&mut values);
        let dictionary: plist::Dictionary = values.into_iter().collect();
        let mut data = Vec::new();
        plist::to_writer_xml(&mut data, &Value::Dictionary(dictionary))
            .map_err(std::io::Error::other)?;
        write_atomically(&self.path, &data)
    }

    pub fn bool(&self, key: &str) -> bool {
        self.get(key)
            .and_then(|value| value.as_boolean())
            .unwrap_or(false)
    }

    pub fn set_bool(&self, key: &str, on: bool) -> std::io::Result<()> {
        self.update(|values| {
            values.insert(key.to_string(), Value::Boolean(on));
        })
    }

    pub fn string(&self, key: &str) -> Option<String> {
        self.get(key)
            .and_then(|value| value.as_string().map(str::to_string))
    }

    pub fn set_string(&self, key: &str, text: &str) -> std::io::Result<()> {
        self.update(|values| {
            values.insert(key.to_string(), Value::String(text.to_string()));
        })
    }

    /// The stored accounts in the order they were added (the user's order; a new account goes last).
    /// An entry this build cannot read is logged and kept as written.
    pub fn accounts(&self) -> Vec<Account> {
        let (accounts, _) = Self::decode_accounts(self.get(ACCOUNTS_KEY));
        accounts
    }

    fn decode_accounts(value: Option<Value>) -> (Vec<Account>, Vec<serde_json::Value>) {
        let Some(data) = value.and_then(|value| value.into_data()) else {
            return (vec![], vec![]);
        };
        let Ok(serde_json::Value::Array(entries)) =
            serde_json::from_slice::<serde_json::Value>(&data)
        else {
            log::error(
                "auth:accounts",
                "cannot read the stored account list, which is not a JSON array",
            );
            return (vec![], vec![]);
        };
        let mut accounts = Vec::new();
        let mut unreadable = Vec::new();
        for (index, entry) in entries.into_iter().enumerate() {
            match serde_json::from_value::<Account>(entry.clone()) {
                Ok(account) => accounts.push(account),
                Err(error) => {
                    log::error("auth:accounts", &format!("cannot read stored account entry {index}, keeping it unchanged: {error}"));
                    unreadable.push(entry);
                }
            }
        }
        accounts.sort_by_key(|account| account.added_at);
        (accounts, unreadable)
    }

    /// Edits the account list under the lock; unreadable entries are written back after the accounts.
    pub fn update_accounts(&self, edit: impl FnOnce(&mut Vec<Account>)) -> std::io::Result<()> {
        self.update(|values| {
            let (mut accounts, unreadable) = Self::decode_accounts(values.remove(ACCOUNTS_KEY));
            edit(&mut accounts);
            let mut entries: Vec<serde_json::Value> = accounts
                .iter()
                .filter_map(|account| serde_json::to_value(account).ok())
                .collect();
            entries.extend(unreadable);
            let data = serde_json::to_vec(&entries).unwrap_or_else(|_| b"[]".to_vec());
            values.insert(ACCOUNTS_KEY.to_string(), Value::Data(data));
        })
    }
}

/// The last successful read of each account, keyed by card id.
#[derive(Clone)]
pub struct SnapshotCache {
    path: PathBuf,
}

impl SnapshotCache {
    pub fn open() -> Self {
        Self {
            path: paths::snapshots_file(),
        }
    }

    pub fn at(path: PathBuf) -> Self {
        Self { path }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn read_unlocked(&self) -> BTreeMap<String, ProviderSnapshot> {
        let Ok(data) = fs::read(&self.path) else {
            return BTreeMap::new();
        };
        serde_json::from_slice(&data).unwrap_or_else(|error| {
            log::warn(
                "cache",
                &format!("cache decode failed, dropping stored snapshots: {error}"),
            );
            BTreeMap::new()
        })
    }

    pub fn load(&self) -> BTreeMap<String, ProviderSnapshot> {
        match Lock::acquire(&self.path, false) {
            Ok(_lock) => self.read_unlocked(),
            Err(_) => self.read_unlocked(),
        }
    }

    /// Writes one account's snapshot, keeping the others another process may have written.
    pub fn store(&self, id: &str, snapshot: &ProviderSnapshot) {
        self.edit(|snapshots| {
            snapshots.insert(id.to_string(), snapshot.clone());
        });
    }

    /// Drops snapshots of accounts that no longer exist.
    pub fn retain(&self, ids: &[String]) {
        self.edit(|snapshots| snapshots.retain(|id, _| ids.contains(id)));
    }

    fn edit(&self, change: impl FnOnce(&mut BTreeMap<String, ProviderSnapshot>)) {
        let result = Lock::acquire(&self.path, true).and_then(|_lock| {
            let mut snapshots = self.read_unlocked();
            change(&mut snapshots);
            write_atomically(
                &self.path,
                &serde_json::to_vec(&snapshots).unwrap_or_default(),
            )
        });
        if let Err(error) = result {
            log::warn("cache", &format!("snapshot not persisted: {error}"));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kind::ProviderKind;

    fn temp_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("openusage-store-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn accounts_round_trip_and_keep_unreadable_entries() {
        let dir = temp_dir();
        let settings = Settings::at(dir.join("settings.plist"));
        settings
            .update(|values| {
                let entries = br#"[{"id":"F00DF00D-0000-4000-8000-000000000000","kind":"future","label":"X","addedAt":1}]"#;
                values.insert(ACCOUNTS_KEY.into(), Value::Data(entries.to_vec()));
            })
            .unwrap();
        settings
            .update_accounts(|accounts| {
                accounts.push(Account::new(ProviderKind::Zai, "Z.ai", None))
            })
            .unwrap();
        assert_eq!(settings.accounts().len(), 1);
        let raw = settings.get(ACCOUNTS_KEY).unwrap().into_data().unwrap();
        assert!(String::from_utf8(raw).unwrap().contains("future"));
        settings
            .set_bool("openusage.notifications.underTenPercent", true)
            .unwrap();
        assert!(Settings::at(dir.join("settings.plist"))
            .bool("openusage.notifications.underTenPercent"));
        fs::remove_dir_all(dir).unwrap();
    }
}
