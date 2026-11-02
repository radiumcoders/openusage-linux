//! XDG locations, the same ones the Swift Linux edition used.

use std::path::PathBuf;

pub fn home() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/"))
}

fn xdg(variable: &str, fallback: &str) -> PathBuf {
    std::env::var_os(variable)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| home().join(fallback))
}

pub fn config_dir() -> PathBuf {
    xdg("XDG_CONFIG_HOME", ".config").join("openusage")
}

pub fn state_dir() -> PathBuf {
    xdg("XDG_STATE_HOME", ".local/state").join("openusage")
}

pub fn runtime_dir() -> PathBuf {
    match std::env::var_os("XDG_RUNTIME_DIR").filter(|value| !value.is_empty()) {
        Some(dir) => PathBuf::from(dir).join("openusage"),
        None => std::env::temp_dir().join(format!("openusage-{}", unsafe { libc::getuid() })),
    }
}

pub fn settings_file() -> PathBuf {
    config_dir().join("settings.plist")
}

pub fn snapshots_file() -> PathBuf {
    state_dir().join("snapshots.json")
}

/// The refresh daemon's pid file; `openusage` commands signal it through this.
pub fn daemon_pid_file() -> PathBuf {
    runtime_dir().join("daemon.pid")
}
