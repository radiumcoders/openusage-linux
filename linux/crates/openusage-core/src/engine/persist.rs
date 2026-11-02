//! `$XDG_STATE_HOME/openusage/status.json`, one refresh-status object per account id.

use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};

use super::AccountStatus;
use crate::log;
use crate::paths;

pub(super) struct StatusFile {
    path: PathBuf,
}

impl StatusFile {
    pub(super) fn open() -> Self {
        Self { path: paths::state_dir().join("status.json") }
    }

    pub(super) fn at(path: PathBuf) -> Self {
        Self { path }
    }

    pub(super) fn load(&self) -> BTreeMap<String, AccountStatus> {
        match Lock::acquire(&self.path, false) {
            Ok(_lock) => self.read_unlocked(),
            Err(_) => self.read_unlocked(),
        }
    }

    fn read_unlocked(&self) -> BTreeMap<String, AccountStatus> {
        let Ok(data) = fs::read(&self.path) else { return BTreeMap::new() };
        if data.is_empty() {
            return BTreeMap::new();
        }
        serde_json::from_slice(&data).unwrap_or_else(|error| {
            log::warn("refresh", &format!("status decode failed, ignoring stored refresh status: {error}"));
            BTreeMap::new()
        })
    }

    pub(super) fn upsert(&self, id: &str, status: &AccountStatus) {
        self.edit(|map| {
            map.insert(id.to_string(), status.clone());
            true
        });
    }

    pub(super) fn remove(&self, id: &str) {
        self.edit(|map| map.remove(id).is_some());
    }

    pub(super) fn retain(&self, ids: &[String]) {
        self.edit(|map| {
            let before = map.len();
            map.retain(|id, _| ids.iter().any(|keep| keep == id));
            map.len() != before
        });
    }

    fn edit(&self, change: impl FnOnce(&mut BTreeMap<String, AccountStatus>) -> bool) {
        let result = (|| -> std::io::Result<()> {
            let _lock = Lock::acquire(&self.path, true)?;
            let mut map = self.read_unlocked();
            if !change(&mut map) {
                return Ok(());
            }
            let data = serde_json::to_vec_pretty(&map).unwrap_or_else(|_| b"{}".to_vec());
            write_atomically(&self.path, &data)
        })();
        if let Err(error) = result {
            log::warn("refresh", &format!("status not persisted: {error}"));
        }
    }
}

struct Lock(File);

impl Lock {
    fn acquire(path: &Path, exclusive: bool) -> std::io::Result<Lock> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let file = OpenOptions::new().create(true).read(true).write(true).truncate(false).open(path.with_extension("lock"))?;
        let operation = if exclusive { libc::LOCK_EX } else { libc::LOCK_SH };
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
