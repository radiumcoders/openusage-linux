//! A `LoginPrompter` the panel can drive. Prompts are posted for the main thread; secret lines and
//! pasted codes travel back on channels. Cancelling drops those senders, which is what the CLI does
//! when input ends, and a late result is ignored because the generation no longer matches.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use uuid::Uuid;

use openusage_core::accounts;
use openusage_core::error::LoginError;
use openusage_core::http::UreqClient;
use openusage_core::kind::ProviderKind;
use openusage_core::login::{LoginPrompt, LoginPrompter};
use openusage_core::provider::LoginMethod;
use openusage_core::providers;
use openusage_core::store::Settings;
use openusage_core::vault::SecretToolVault;

use crate::state::{AddFlow, Signin};

pub enum Outcome {
    Saved,
    Cancelled,
    Failed(String),
}

pub enum Drive {
    Idle,
    Prompt(LoginPrompt),
    Saved,
    Cancelled,
    Failed(String),
}

pub struct Prompter {
    cancel: AtomicBool,
    prompt: Mutex<Option<LoginPrompt>>,
    dirty: AtomicBool,
    line_tx: Mutex<Option<Sender<Option<String>>>>,
    line_rx: Mutex<Option<Receiver<Option<String>>>>,
    paste_tx: Mutex<Option<Sender<String>>>,
    pending_paste: Mutex<Option<String>>,
}

impl Prompter {
    pub fn new() -> Self {
        let (line_tx, line_rx) = mpsc::channel();
        Self {
            cancel: AtomicBool::new(false),
            prompt: Mutex::new(None),
            dirty: AtomicBool::new(false),
            line_tx: Mutex::new(Some(line_tx)),
            line_rx: Mutex::new(Some(line_rx)),
            paste_tx: Mutex::new(None),
            pending_paste: Mutex::new(None),
        }
    }

    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::SeqCst);
        self.line_tx.lock().expect("line").take();
        self.paste_tx.lock().expect("paste").take();
    }

    pub fn submit_secret(&self, text: String) {
        if let Some(sender) = self.line_tx.lock().expect("line").as_ref() {
            let _ = sender.send(Some(text));
        }
    }

    pub fn submit_paste(&self, text: String) {
        if let Some(sender) = self.paste_tx.lock().expect("paste").as_ref() {
            let _ = sender.send(text);
        } else {
            *self.pending_paste.lock().expect("pending") = Some(text);
        }
    }

    fn take_prompt(&self) -> Option<LoginPrompt> {
        if !self.dirty.swap(false, Ordering::SeqCst) {
            return None;
        }
        self.prompt.lock().expect("prompt").clone()
    }
}

impl LoginPrompter for Prompter {
    fn show(&self, prompt: &LoginPrompt) {
        *self.prompt.lock().expect("prompt") = Some(prompt.clone());
        self.dirty.store(true, Ordering::SeqCst);
    }

    fn read_line(&self, _secret: bool) -> Option<String> {
        let receiver = self.line_rx.lock().expect("line").take()?;
        loop {
            if self.cancel.load(Ordering::SeqCst) {
                return None;
            }
            match receiver.recv_timeout(Duration::from_millis(100)) {
                Ok(None) => return None,
                Ok(Some(line)) => return Some(line.trim().to_string()),
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => return None,
            }
        }
    }

    fn pasted_codes(&self) -> Receiver<String> {
        let (sender, receiver) = mpsc::channel();
        if let Some(pending) = self.pending_paste.lock().expect("pending").take() {
            let _ = sender.send(pending);
        }
        *self.paste_tx.lock().expect("paste") = Some(sender);
        receiver
    }
}

pub fn spawn(
    kind: ProviderKind,
    method: LoginMethod,
    replace: Option<Uuid>,
    generation: u64,
) -> Signin {
    let prompter = Arc::new(Prompter::new());
    let done = Arc::new(AtomicBool::new(false));
    let outcome = Arc::new(Mutex::new(None));
    let signin = Signin {
        generation,
        prompter: Arc::clone(&prompter),
        done: Arc::clone(&done),
        outcome: Arc::clone(&outcome),
    };
    thread::spawn(move || {
        let http = UreqClient::new();
        let result = providers::login(kind, &method).login(prompter.as_ref(), &http);
        let produced = if prompter.cancel.load(Ordering::SeqCst) {
            Outcome::Cancelled
        } else {
            match result {
                Ok((label, credential)) if !prompter.cancel.load(Ordering::SeqCst) => {
                    save(kind, &method, replace, label, &credential)
                }
                Ok(_) | Err(LoginError::Cancelled) => Outcome::Cancelled,
                Err(error) => Outcome::Failed(error.to_string()),
            }
        };
        *outcome.lock().expect("outcome") = Some(produced);
        done.store(true, Ordering::SeqCst);
    });
    signin
}

pub fn spawn_hub(url: String, key: String, generation: u64) -> Signin {
    let prompter = Arc::new(Prompter::new());
    let done = Arc::new(AtomicBool::new(false));
    let outcome = Arc::new(Mutex::new(None));
    let signin = Signin {
        generation,
        prompter: Arc::clone(&prompter),
        done: Arc::clone(&done),
        outcome: Arc::clone(&outcome),
    };
    thread::spawn(move || {
        let http = UreqClient::new();
        let vault = SecretToolVault::default();
        let settings = Settings::open();
        let produced = if prompter.cancel.load(Ordering::SeqCst) {
            Outcome::Cancelled
        } else {
            match openusage_core::hub::connect(&url, &key, &http, &vault, &settings) {
                Ok(_) => Outcome::Saved,
                Err(error) => Outcome::Failed(error.to_string()),
            }
        };
        *outcome.lock().expect("outcome") = Some(produced);
        done.store(true, Ordering::SeqCst);
    });
    signin
}

fn save(
    kind: ProviderKind,
    method: &LoginMethod,
    replace: Option<Uuid>,
    label: String,
    credential: &openusage_core::account::Credential,
) -> Outcome {
    let settings = Settings::open();
    let vault = SecretToolVault::default();
    let result = if let Some(id) = replace {
        accounts::replace_sign_in(&settings, &vault, id, credential)
    } else {
        let organization = match method {
            LoginMethod::Organization(name) => Some(name.clone()),
            LoginMethod::Default => None,
        };
        accounts::save_new(&settings, &vault, kind, label, organization, credential).map(|_| ())
    };
    match result {
        Ok(()) => Outcome::Saved,
        Err(message) => Outcome::Failed(message),
    }
}

/// Applies a finished prompt or result onto the flow. The caller opens browser URLs and rebuilds.
pub fn drive(flow: &mut AddFlow, signin: &mut Option<Signin>) -> Drive {
    let Some(active) = signin.as_ref() else {
        return Drive::Idle;
    };
    if active.generation != flow.generation {
        *signin = None;
        return Drive::Idle;
    }
    if active.done.load(Ordering::SeqCst) {
        let outcome = active.outcome.lock().expect("outcome").take();
        *signin = None;
        flow.working = false;
        flow.submitted = false;
        return match outcome {
            Some(Outcome::Saved) => Drive::Saved,
            Some(Outcome::Cancelled) | None => Drive::Cancelled,
            Some(Outcome::Failed(message)) => {
                flow.error = Some(message.clone());
                flow.prompt = None;
                Drive::Failed(message)
            }
        };
    }
    match active.prompter.take_prompt() {
        Some(prompt) => {
            flow.prompt = Some(prompt.clone());
            flow.working = true;
            flow.error = None;
            Drive::Prompt(prompt)
        }
        None => Drive::Idle,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cancel_unblocks_a_secret_prompt() {
        let prompter = Arc::new(Prompter::new());
        let waiting = {
            let prompter = Arc::clone(&prompter);
            thread::spawn(move || LoginPrompter::read_line(prompter.as_ref(), true))
        };
        thread::sleep(Duration::from_millis(30));
        prompter.cancel();
        assert_eq!(waiting.join().unwrap(), None);
    }
}
