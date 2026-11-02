//! Sign-in, rename, remove, claim, and the account menu. The screen builders live in `session`.

use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

use chrono::{DateTime, Utc};
use uuid::Uuid;

use openusage_core::account::Account;
use openusage_core::accounts;
use openusage_core::claims;
use openusage_core::http::UreqClient;
use openusage_core::kind::ProviderKind;
use openusage_core::login::LoginPrompt;
use openusage_core::provider::LoginMethod;
use openusage_core::store::Settings;
use openusage_core::vault::SecretToolVault;

use crate::menu::Act;
use crate::model;
use crate::prompter;
use crate::session;
use crate::state::{AddFlow, AddStep, ClaimJob, ClaimPhase, Screen};
use crate::ui;
use crate::window::{self, Ctx};

pub(crate) fn start_add(ctx: &Rc<Ctx>) {
    *ctx.add.borrow_mut() = AddFlow::fresh();
    *ctx.screen.borrow_mut() = Screen::Add;
    window::rebuild(ctx, true);
}

pub(crate) fn open_detail(ctx: &Rc<Ctx>, id: Uuid) {
    ctx.confirm_remove.set(false);
    *ctx.renaming.borrow_mut() = None;
    *ctx.notice.borrow_mut() = None;
    *ctx.screen.borrow_mut() = Screen::Detail(id);
    window::rebuild(ctx, true);
}

pub(crate) fn choose_provider(ctx: &Rc<Ctx>, kind: ProviderKind) {
    if kind == ProviderKind::Copilot {
        ctx.add.borrow_mut().step = AddStep::Copilot;
        ctx.add.borrow_mut().kind = Some(kind);
        *ctx.screen.borrow_mut() = Screen::Add;
        window::rebuild(ctx, false);
    } else {
        begin_signin(ctx, kind, LoginMethod::Default, None);
    }
}

pub(crate) fn begin_signin(
    ctx: &Rc<Ctx>,
    kind: ProviderKind,
    method: LoginMethod,
    replace: Option<Uuid>,
) {
    if let Some(active) = ctx.signin.borrow().as_ref() {
        active.prompter.cancel();
    }
    let generation = {
        let mut flow = ctx.add.borrow_mut();
        flow.generation += 1;
        flow.step = AddStep::SignIn;
        flow.kind = Some(kind);
        flow.method = method.clone();
        flow.replace = replace;
        flow.prompt = None;
        flow.error = None;
        flow.working = true;
        flow.submitted = false;
        flow.draft.clear();
        flow.generation
    };
    *ctx.signin.borrow_mut() = Some(prompter::spawn(kind, method, replace, generation));
    *ctx.screen.borrow_mut() = Screen::Add;
    window::rebuild(ctx, false);
}

pub(crate) fn start_replace(ctx: &Rc<Ctx>, id: Uuid) {
    let Some(account) = Settings::open()
        .accounts()
        .into_iter()
        .find(|account| account.id == id)
    else {
        return;
    };
    let method = match account.organization.clone() {
        Some(name) => LoginMethod::Organization(name),
        None => LoginMethod::Default,
    };
    begin_signin(ctx, account.kind, method, Some(id));
}

pub(crate) fn retry(ctx: &Rc<Ctx>) {
    let (kind, method, replace) = {
        let flow = ctx.add.borrow();
        (flow.kind, flow.method.clone(), flow.replace)
    };
    if let Some(kind) = kind {
        begin_signin(ctx, kind, method, replace);
    }
}

pub(crate) fn submit_prompt(ctx: &Rc<Ctx>) {
    let prompt = ctx.add.borrow().prompt.clone();
    let draft = ctx.add.borrow().draft.clone();
    {
        let signin = ctx.signin.borrow();
        let Some(signin) = signin.as_ref() else {
            return;
        };
        match prompt {
            Some(LoginPrompt::Browser { .. }) => signin.prompter.submit_paste(draft),
            Some(LoginPrompt::BrowserToken { .. } | LoginPrompt::Secret { .. }) => {
                signin.prompter.submit_secret(draft)
            }
            _ => return,
        }
    }
    ctx.add.borrow_mut().submitted = true;
    window::rebuild(ctx, false);
}

pub(crate) fn connect_hub(ctx: &Rc<Ctx>) {
    let (url, key, generation) = {
        let mut flow = ctx.add.borrow_mut();
        let url = flow.hub_url.trim().to_string();
        let key = flow.hub_key.trim().to_string();
        if url.is_empty() || key.is_empty() {
            return;
        }
        flow.generation += 1;
        flow.working = true;
        flow.error = None;
        (url, key, flow.generation)
    };
    *ctx.signin.borrow_mut() = Some(prompter::spawn_hub(url, key, generation));
    window::rebuild(ctx, false);
}

pub(crate) fn back_add(ctx: &Rc<Ctx>) {
    if let Some(active) = ctx.signin.borrow().as_ref() {
        active.prompter.cancel();
    }
    *ctx.signin.borrow_mut() = None;
    let replace = ctx.add.borrow().replace;
    let step = ctx.add.borrow().step;
    ctx.add.borrow_mut().generation += 1;
    match (step, replace) {
        (AddStep::SignIn, Some(id)) => open_detail(ctx, id),
        (AddStep::Organization, _) => {
            ctx.add.borrow_mut().step = AddStep::Copilot;
            ctx.add.borrow_mut().error = None;
            ctx.add.borrow_mut().working = false;
            window::rebuild(ctx, false);
        }
        (AddStep::Providers, _) => session::show_accounts(ctx),
        _ => {
            ctx.add.borrow_mut().step = AddStep::Providers;
            ctx.add.borrow_mut().prompt = None;
            ctx.add.borrow_mut().error = None;
            ctx.add.borrow_mut().working = false;
            ctx.add.borrow_mut().replace = None;
            window::rebuild(ctx, false);
        }
    }
}

pub(crate) fn back_from_signin(ctx: &Rc<Ctx>) {
    let replace = ctx.add.borrow().replace;
    *ctx.signin.borrow_mut() = None;
    if let Some(id) = replace {
        open_detail(ctx, id);
    } else {
        ctx.add.borrow_mut().step = AddStep::Providers;
        ctx.add.borrow_mut().prompt = None;
        ctx.add.borrow_mut().error = None;
        ctx.add.borrow_mut().working = false;
        *ctx.screen.borrow_mut() = Screen::Add;
    }
}

pub(crate) fn copy_prompt(ctx: &Rc<Ctx>) {
    let prompt = ctx.add.borrow().prompt.clone();
    let text = match &prompt {
        Some(LoginPrompt::DeviceCode { user_code, .. }) => Some(user_code.clone()),
        Some(other) => prompt_url(other).map(str::to_string),
        None => None,
    };
    if let Some(text) = text {
        ui::copy_text(&text);
    }
}

pub(crate) fn prompt_url(prompt: &LoginPrompt) -> Option<&str> {
    match prompt {
        LoginPrompt::Browser { url, .. } | LoginPrompt::BrowserToken { url } => Some(url.as_str()),
        LoginPrompt::DeviceCode {
            verification_url, ..
        } => Some(verification_url.as_str()),
        LoginPrompt::Secret { .. } => None,
    }
}

/// Browser sign-in opens itself. A device code waits for Open, and a key stays a link.
pub(crate) fn open_prompt(prompt: &LoginPrompt) {
    if let LoginPrompt::Browser { url, .. } | LoginPrompt::BrowserToken { url } = prompt {
        ui::open_uri(url);
    }
}

pub(crate) fn menu_act(ctx: &Rc<Ctx>, index: usize, act: Act) {
    let model = model::load_with(&crate::edit::ensure(ctx));
    let Some(account) = model.accounts.get(index) else {
        return;
    };
    match act {
        Act::Info => {}
        Act::Refresh => window::refresh_now(ctx),
        Act::ManageAccounts => session::show_accounts(ctx),
        Act::Rename => {
            let id = account.account.id;
            open_detail(ctx, id);
            *ctx.renaming.borrow_mut() = Some(id);
            window::rebuild(ctx, false);
        }
        Act::EditAccount => open_detail(ctx, account.account.id),
        Act::Share(ids) => crate::share::copy_accounts(ctx, &ids),
        Act::Open(url) => ui::open_uri(&url),
    }
}

pub(crate) fn commit_rename(ctx: &Rc<Ctx>, id: Uuid, text: String) {
    if ctx.renaming.borrow_mut().take() != Some(id) {
        return;
    }
    match accounts::rename_account(&Settings::open(), id, text.trim()) {
        Ok(()) => window::reload_accounts(),
        Err(message) => *ctx.notice.borrow_mut() = Some(message),
    }
    window::rebuild(ctx, false);
}

pub(crate) fn remove_account(ctx: &Rc<Ctx>, id: Uuid) {
    match accounts::remove_account(&Settings::open(), &SecretToolVault::default(), id) {
        Ok(()) => {
            window::reload_accounts();
            ctx.confirm_remove.set(false);
            *ctx.notice.borrow_mut() = None;
            *ctx.screen.borrow_mut() = Screen::Accounts;
        }
        Err(message) => {
            ctx.confirm_remove.set(false);
            *ctx.notice.borrow_mut() = Some(message);
        }
    }
    window::rebuild(ctx, false);
}

pub(crate) fn disconnect_hub(ctx: &Rc<Ctx>) {
    let Some(id) = ctx.confirm_hub.borrow_mut().take() else {
        return;
    };
    let vault = SecretToolVault::default();
    match openusage_core::hub::disconnect(id, &vault, &Settings::open()) {
        Ok(()) => {
            window::reload_accounts();
            *ctx.notice.borrow_mut() = None;
        }
        Err(error) => *ctx.notice.borrow_mut() = Some(error.to_string()),
    }
    window::rebuild(ctx, false);
}

pub(crate) fn run_claim(ctx: &Rc<Ctx>) {
    let Some(id) = ctx.claim.borrow().account else {
        return;
    };
    let model = model::load();
    let Some(account) = model
        .accounts
        .into_iter()
        .find(|account| account.account.id == id)
    else {
        return;
    };
    let card = account.account.card_id();
    let snapshot = model.snapshots.get(&card).cloned();
    let remembered = match &ctx.claim.borrow().phase {
        ClaimPhase::Confirm { expiring, .. } => *expiring,
        _ => None,
    };
    let expiring = claims::offer(&account.account, snapshot.as_ref())
        .and_then(|offer| offer.expiries.first().copied())
        .or(remembered);
    ctx.claim.borrow_mut().phase = ClaimPhase::Working;
    *ctx.claim_job.borrow_mut() = Some(spawn_claim(account.account, expiring));
    window::rebuild(ctx, false);
}

fn spawn_claim(account: Account, expiring: Option<DateTime<Utc>>) -> ClaimJob {
    let done = Arc::new(AtomicBool::new(false));
    let message = Arc::new(Mutex::new(None));
    let done_flag = Arc::clone(&done);
    let slot = Arc::clone(&message);
    thread::spawn(move || {
        let outcome = claims::claim(
            &account,
            &SecretToolVault::default(),
            &UreqClient::new(),
            expiring,
        );
        *slot.lock().expect("claim") = Some(outcome.message(Utc::now()));
        done_flag.store(true, Ordering::SeqCst);
    });
    ClaimJob { done, message }
}
