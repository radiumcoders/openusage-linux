//! The refresh daemon's pid file. `openusage waybar` holds the lock; `openusage waybar --refresh`,
//! account changes, and the panel send a signal through it.

use std::fs::{self, File, OpenOptions};
use std::io::{Seek, Write};
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};

use crate::paths;

/// Holds the daemon pid file. Dropping it removes the file and releases the lock.
pub struct PidLock {
    file: Option<File>,
    path: PathBuf,
}

impl PidLock {
    pub fn acquire() -> Result<Self, String> {
        Self::acquire_at(&paths::daemon_pid_file())
    }

    pub fn acquire_at(path: &Path) -> Result<Self, String> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|error| format!("Could not create the Waybar pid file: {error}"))?;
        }
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)
            .map_err(|error| format!("Could not create the Waybar pid file: {error}"))?;
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            let blocked = matches!(
                std::io::Error::last_os_error().raw_os_error(),
                Some(error) if error == libc::EWOULDBLOCK || error == libc::EAGAIN
            );
            if blocked {
                let pid = fs::read_to_string(path).ok().and_then(|text| text.trim().parse::<i32>().ok());
                return Err(match pid {
                    Some(pid) if pid > 0 => format!("Waybar module is already running (pid {pid})."),
                    _ => "Waybar module is already running.".into(),
                });
            }
            return Err(format!("Could not lock the Waybar pid file: {}", std::io::Error::last_os_error()));
        }
        let text = format!("{}\n", std::process::id());
        file.set_len(0).ok();
        file.seek(std::io::SeekFrom::Start(0)).ok();
        file.write_all(text.as_bytes()).ok();
        Ok(Self { file: Some(file), path: path.to_path_buf() })
    }
}

impl Drop for PidLock {
    fn drop(&mut self) {
        let Some(file) = self.file.take() else { return };
        let _ = fs::remove_file(&self.path);
        unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_UN) };
    }
}

/// Sends `signal` to the daemon named in the pid file, when a live process holds that file's lock.
/// A pid file with nobody holding the lock is stale and is not signaled.
/// `SIGUSR1` refreshes every account now. `SIGUSR2` reloads accounts and refreshes what is due.
pub fn signal_running(signal: i32) -> bool {
    signal_running_at(&paths::daemon_pid_file(), signal)
}

pub fn signal_running_at(path: &Path, signal: i32) -> bool {
    let Ok(file) = OpenOptions::new().read(true).write(true).open(path) else { return false };
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
        unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_UN) };
        return false;
    }
    let blocked = matches!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(error) if error == libc::EWOULDBLOCK || error == libc::EAGAIN
    );
    if !blocked {
        return false;
    }
    let Ok(text) = fs::read_to_string(path) else { return false };
    let Ok(pid) = text.trim().parse::<i32>() else { return false };
    if pid <= 0 {
        return false;
    }
    unsafe { libc::kill(pid, signal) == 0 }
}

/// Tells a running daemon to reload its accounts (`SIGUSR2`).
pub fn notify_accounts_changed() -> bool {
    signal_running(libc::SIGUSR2)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_lock_names_this_process_and_a_second_lock_is_refused() {
        let dir = std::env::temp_dir().join(format!("openusage-pid-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("daemon.pid");
        let lock = PidLock::acquire_at(&path).unwrap();
        let recorded = fs::read_to_string(&path).unwrap();
        assert_eq!(recorded.trim(), std::process::id().to_string());
        match PidLock::acquire_at(&path) {
            Err(error) => assert!(error.contains("already running"), "{error}"),
            Ok(_) => panic!("a second lock should be refused"),
        }
        assert!(signal_running_at(&path, 0));
        drop(lock);
        assert!(!signal_running_at(&path, 0));
        fs::write(&path, "1\n").unwrap();
        assert!(!signal_running_at(&path, 0), "an unlocked pid file is stale and must not be signaled");
        let _ = fs::remove_dir_all(dir);
    }
}
