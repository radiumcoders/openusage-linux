//! Log lines go to stderr, which Waybar, the Omarchy shell, and systemd hand to the journal. Errors,
//! warnings, and notices always print; info and debug only with `OPENUSAGE_VERBOSE=1`. Never pass a
//! credential or a response body here.

use std::io::Write;
use std::sync::OnceLock;

fn verbose() -> bool {
    static VERBOSE: OnceLock<bool> = OnceLock::new();
    *VERBOSE.get_or_init(|| std::env::var("OPENUSAGE_VERBOSE").as_deref() == Ok("1"))
}

fn emit(level: &str, tag: &str, message: &str) {
    let _ = writeln!(std::io::stderr().lock(), "OpenUsage {level}: [{tag}] {message}");
}

pub fn error(tag: &str, message: &str) {
    emit("error", tag, message);
}

pub fn warn(tag: &str, message: &str) {
    emit("warning", tag, message);
}

pub fn notice(tag: &str, message: &str) {
    emit("notice", tag, message);
}

pub fn info(tag: &str, message: &str) {
    if verbose() {
        emit("info", tag, message);
    }
}

pub fn debug(tag: &str, message: &str) {
    if verbose() {
        emit("debug", tag, message);
    }
}
