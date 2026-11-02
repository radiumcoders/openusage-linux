//! `OPENUSAGE_SCREEN` picks a page for a render dump. It never starts a sign-in.

use std::rc::Rc;

use openusage_core::kind::ProviderKind;
use openusage_core::login::LoginPrompt;

use crate::model::Model;
use crate::state::{AddStep, ClaimPhase, ClaimUi, Screen};
use crate::view::Tab;
use crate::window::Ctx;

pub fn apply(ctx: &Rc<Ctx>, model: &Model) {
    if ctx.launch_applied.replace(true) {
        return;
    }
    let Ok(screen) = std::env::var("OPENUSAGE_SCREEN") else { return };
    match screen.as_str() {
        "accounts" => *ctx.screen.borrow_mut() = Screen::Accounts,
        "detail" => {
            let account = model
                .accounts
                .iter()
                .find(|account| account.problem.as_deref().is_some_and(|text| !text.starts_with("Last updated")))
                .or(model.accounts.first());
            if let Some(account) = account {
                *ctx.screen.borrow_mut() = Screen::Detail(account.account.id);
            }
        }
        "add" => *ctx.screen.borrow_mut() = Screen::Add,
        "settings" => *ctx.screen.borrow_mut() = Screen::Settings,
        "copilot" => {
            let mut add = ctx.add.borrow_mut();
            add.step = AddStep::Copilot;
            add.kind = Some(ProviderKind::Copilot);
            *ctx.screen.borrow_mut() = Screen::Add;
        }
        "browser" => preview_prompt(
            ctx,
            ProviderKind::Claude,
            LoginPrompt::Browser { url: "https://claude.ai/oauth/authorize".into(), accepts_pasted_code: true },
        ),
        "device" => preview_prompt(
            ctx,
            ProviderKind::Copilot,
            LoginPrompt::DeviceCode { user_code: "ABCD-EFGH".into(), verification_url: "https://github.com/login/device".into() },
        ),
        "secret" => preview_prompt(
            ctx,
            ProviderKind::OpenRouter,
            LoginPrompt::Secret {
                label: "OpenRouter API key".into(),
                key_page: Some("https://openrouter.ai/settings/management-keys".into()),
            },
        ),
        "hub" => {
            ctx.add.borrow_mut().step = AddStep::Hub;
            *ctx.screen.borrow_mut() = Screen::Add;
        }
        "hubs" => {
            let mut preview = ctx.preview.get();
            preview.hub = true;
            ctx.preview.set(preview);
            *ctx.screen.borrow_mut() = Screen::Accounts;
        }
        "resets" => {
            ctx.tab.set(Tab::All);
            let mut preview = ctx.preview.get();
            preview.claim = true;
            ctx.preview.set(preview);
            if let Some(account) = model.accounts.iter().find(|account| {
                account.lines.iter().any(|line| line.descriptor.shows_reset_expiries() && line.data.has_data)
            }) {
                *ctx.claim.borrow_mut() = ClaimUi {
                    account: Some(account.account.id),
                    phase: ClaimPhase::Confirm { detail: Some("Resets your weekly limit".into()), expiring: None },
                };
            }
        }
        "breakdown" => {
            ctx.tab.set(Tab::All);
            let mut preview = ctx.preview.get();
            preview.breakdown = true;
            ctx.preview.set(preview);
        }
        "menu" => {
            ctx.tab.set(Tab::All);
            let mut preview = ctx.preview.get();
            preview.menu = true;
            ctx.preview.set(preview);
        }
        "all" => ctx.tab.set(Tab::All),
        "priority" => ctx.tab.set(Tab::Priority),
        _ => {}
    }
}

fn preview_prompt(ctx: &Rc<Ctx>, kind: ProviderKind, prompt: LoginPrompt) {
    let mut add = ctx.add.borrow_mut();
    add.step = AddStep::SignIn;
    add.kind = Some(kind);
    add.prompt = Some(prompt);
    add.working = false;
    *ctx.screen.borrow_mut() = Screen::Add;
}
