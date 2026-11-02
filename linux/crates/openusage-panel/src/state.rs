//! Which screen the panel is on, and the sign-in or claim it is in the middle of.

use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

use chrono::{DateTime, Utc};
use uuid::Uuid;

use openusage_core::kind::ProviderKind;
use openusage_core::login::LoginPrompt;
use openusage_core::provider::LoginMethod;

use crate::prompter::{Outcome, Prompter};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Screen {
    Dashboard,
    Accounts,
    Detail(Uuid),
    Add,
    Settings,
}

#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub struct Preview {
    pub claim: bool,
    pub breakdown: bool,
    pub menu: bool,
    pub hub: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum AddStep {
    Providers,
    Copilot,
    Organization,
    Hub,
    SignIn,
}

#[derive(Clone)]
pub struct AddFlow {
    pub step: AddStep,
    pub kind: Option<ProviderKind>,
    pub method: LoginMethod,
    pub replace: Option<Uuid>,
    pub org: String,
    pub draft: String,
    pub hub_url: String,
    pub hub_key: String,
    pub prompt: Option<LoginPrompt>,
    pub error: Option<String>,
    pub working: bool,
    pub submitted: bool,
    pub generation: u64,
}

impl AddFlow {
    pub fn fresh() -> Self {
        Self {
            step: AddStep::Providers,
            kind: None,
            method: LoginMethod::Default,
            replace: None,
            org: String::new(),
            draft: String::new(),
            hub_url: String::new(),
            hub_key: String::new(),
            prompt: None,
            error: None,
            working: false,
            submitted: false,
            generation: 0,
        }
    }
}

pub struct Signin {
    pub generation: u64,
    pub prompter: Arc<Prompter>,
    pub done: Arc<AtomicBool>,
    pub outcome: Arc<Mutex<Option<Outcome>>>,
}

#[derive(Clone)]
pub enum ClaimPhase {
    Idle,
    Confirm { detail: Option<String>, expiring: Option<DateTime<Utc>> },
    Working,
    Done(String),
}

impl Default for ClaimPhase {
    fn default() -> Self {
        Self::Idle
    }
}

pub struct ClaimUi {
    pub account: Option<Uuid>,
    pub phase: ClaimPhase,
}

impl Default for ClaimUi {
    fn default() -> Self {
        Self { account: None, phase: ClaimPhase::Idle }
    }
}

pub struct ClaimJob {
    pub done: Arc<AtomicBool>,
    pub message: Arc<Mutex<Option<String>>>,
}
