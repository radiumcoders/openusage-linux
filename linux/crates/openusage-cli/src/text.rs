use std::io::{self, Write};

pub fn error(message: &str) {
    let mut err = io::stderr().lock();
    let _ = writeln!(err, "openusage: {message}");
    let _ = err.flush();
}

pub fn note(message: &str) {
    let mut err = io::stderr().lock();
    let _ = writeln!(err, "{message}");
    let _ = err.flush();
}

pub fn prompt(message: &str) {
    let mut err = io::stderr().lock();
    let _ = write!(err, "{message}");
    let _ = err.flush();
}

pub fn line(message: &str) {
    let mut out = io::stdout().lock();
    let _ = writeln!(out, "{message}");
    let _ = out.flush();
}
