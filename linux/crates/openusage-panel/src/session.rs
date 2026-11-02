//! Navigation between the dashboard, accounts, add-account, and settings.

use std::rc::Rc;
use std::sync::atomic::Ordering;

use uuid::Uuid;

use openusage_core::kind::ProviderKind;
use openusage_core::provider::LoginMethod;
use openusage_core::store::Settings;

use crate::add_page::{self, Hooks as AddHooks};
use crate::accounts_page::{self, DetailHooks, ListHooks};
use crate::model::{self, Model};
use crate::prompter::{self, Drive};
use crate::settings_page::{self, Hooks as SettingsHooks};
use crate::state::{AddFlow, AddStep, ClaimPhase, ClaimUi, Screen};
use crate::ui;
use crate::view::{self, Actions, Chrome, Tab};
use crate::window::{self, Ctx};

pub fn build(ctx: &Rc<Ctx>, model: &Model) -> view::Built {
    let notice = ctx.notice.borrow().clone();
    match *ctx.screen.borrow() {
        Screen::Dashboard => dashboard(ctx, model, notice),
        Screen::Accounts => accounts(ctx, model, notice.as_deref()),
        Screen::Detail(id) => detail(ctx, model, id, notice.as_deref()),
        Screen::Add => add(ctx, model),
        Screen::Settings => settings(ctx),
    }
}

pub fn poll(ctx: &Rc<Ctx>) -> bool {
    let drive = {
        let mut signin = ctx.signin.borrow_mut();
        let mut add = ctx.add.borrow_mut();
        prompter::drive(&mut add, &mut signin)
    };
    let mut changed = !matches!(drive, Drive::Idle);
    match drive {
        Drive::Idle => {}
        Drive::Prompt(prompt) => crate::nav::open_prompt(&prompt),
        Drive::Saved => {
            *ctx.add.borrow_mut() = AddFlow::fresh();
            *ctx.screen.borrow_mut() = Screen::Dashboard;
            ctx.tab.set(Tab::Priority);
            window::reload_accounts();
        }
        Drive::Cancelled => crate::nav::back_from_signin(ctx),
        Drive::Failed(message) => openusage_core::log::warn("panel", &format!("sign-in didn't finish: {message}")),
    }
    if let Some(job) = ctx.claim_job.borrow().as_ref() {
        if job.done.load(Ordering::SeqCst) {
            let message = job.message.lock().expect("claim").clone().unwrap_or_else(|| "Couldn't reset usage. Please try again.".into());
            ctx.claim.borrow_mut().phase = ClaimPhase::Done(message);
            *ctx.claim_job.borrow_mut() = None;
            model::signal_daemon();
            changed = true;
        }
    }
    changed
}

pub fn on_escape(ctx: &Rc<Ctx>) -> bool {
    match *ctx.screen.borrow() {
        Screen::Dashboard => false,
        Screen::Detail(_) => {
            show_accounts(ctx);
            true
        }
        Screen::Accounts | Screen::Settings => {
            show_dashboard(ctx);
            true
        }
        Screen::Add => {
            crate::nav::back_add(ctx);
            true
        }
    }
}

pub fn on_return(ctx: &Rc<Ctx>) {
    match *ctx.screen.borrow() {
        Screen::Dashboard => show_accounts(ctx),
        Screen::Detail(_) => show_accounts(ctx),
        Screen::Accounts | Screen::Settings => show_dashboard(ctx),
        Screen::Add => {}
    }
}

pub fn show_accounts(ctx: &Rc<Ctx>) {
    ctx.confirm_remove.set(false);
    *ctx.renaming.borrow_mut() = None;
    *ctx.screen.borrow_mut() = Screen::Accounts;
    window::rebuild(ctx, true);
}

pub fn show_settings(ctx: &Rc<Ctx>) {
    *ctx.screen.borrow_mut() = Screen::Settings;
    window::rebuild(ctx, true);
}

pub fn show_dashboard(ctx: &Rc<Ctx>) {
    *ctx.screen.borrow_mut() = Screen::Dashboard;
    window::rebuild(ctx, true);
}

fn dashboard(ctx: &Rc<Ctx>, model: &Model, notice: Option<String>) -> view::Built {
    let actions = Actions {
        on_tab: {
            let ctx = ctx.clone();
            Rc::new(move |tab| {
                if ctx.rebuilding.get() || ctx.tab.get() == tab {
                    return;
                }
                ctx.tab.set(tab);
                window::rebuild(&ctx, true);
            })
        },
        on_toggle_meter: {
            let ctx = ctx.clone();
            Rc::new(move || {
                model::toggle_display_mode();
                window::rebuild(&ctx, false);
            })
        },
        on_toggle_reset: {
            let ctx = ctx.clone();
            Rc::new(move || {
                model::toggle_resets();
                window::rebuild(&ctx, false);
            })
        },
        on_refresh: {
            let ctx = ctx.clone();
            Rc::new(move || window::refresh_now(&ctx))
        },
        on_fix: {
            let ctx = ctx.clone();
            Rc::new(move |id| crate::nav::open_detail(&ctx, id))
        },
        on_add: {
            let ctx = ctx.clone();
            Rc::new(move || crate::nav::start_add(&ctx))
        },
        on_menu: {
            let ctx = ctx.clone();
            Rc::new(move |index, act| crate::nav::menu_act(&ctx, index, act))
        },
        on_claim: {
            let ctx = ctx.clone();
            Rc::new(move |id| {
                ctx.claim.borrow_mut().account = Some(id);
                ctx.claim.borrow_mut().phase = ClaimPhase::Confirm { detail: None, expiring: None };
                window::rebuild(&ctx, false);
            })
        },
        on_claim_reset: {
            let ctx = ctx.clone();
            Rc::new(move || crate::nav::run_claim(&ctx))
        },
        on_claim_cancel: {
            let ctx = ctx.clone();
            Rc::new(move || {
                *ctx.claim.borrow_mut() = ClaimUi::default();
                window::rebuild(&ctx, false);
            })
        },
    };
    let chrome = Chrome { preview: ctx.preview.get(), claim: claim_snapshot(ctx), banner: notice };
    view::build(model, ctx.tab.get(), ctx.refreshing.get(), &actions, &chrome, ctx.max_height.get())
}

fn claim_snapshot(ctx: &Ctx) -> ClaimUi {
    let claim = ctx.claim.borrow();
    ClaimUi { account: claim.account, phase: claim.phase.clone() }
}

fn accounts(ctx: &Rc<Ctx>, model: &Model, notice: Option<&str>) -> view::Built {
    let mut hubs = openusage_core::hub::list(&Settings::open());
    if ctx.preview.get().hub {
        hubs.push(openusage_core::hub::Hub {
            id: Uuid::nil(),
            url: "http://127.0.0.1:8317".into(),
            host: "127.0.0.1:8317".into(),
        });
    }
    let hooks = ListHooks {
        on_back: {
            let ctx = ctx.clone();
            Rc::new(move || show_dashboard(&ctx))
        },
        on_open: {
            let ctx = ctx.clone();
            Rc::new(move |id| crate::nav::open_detail(&ctx, id))
        },
        on_add: {
            let ctx = ctx.clone();
            Rc::new(move || crate::nav::start_add(&ctx))
        },
        on_disconnect: {
            let ctx = ctx.clone();
            Rc::new(move |id| {
                *ctx.confirm_hub.borrow_mut() = Some(id);
                window::rebuild(&ctx, false);
            })
        },
        on_confirm_hub: {
            let ctx = ctx.clone();
            Rc::new(move || crate::nav::disconnect_hub(&ctx))
        },
        on_cancel_hub: {
            let ctx = ctx.clone();
            Rc::new(move || {
                *ctx.confirm_hub.borrow_mut() = None;
                window::rebuild(&ctx, false);
            })
        },
    };
    accounts_page::list(
        &model.accounts,
        &hubs,
        ctx.confirm_hub.borrow().as_ref().copied(),
        notice,
        &hooks,
        ctx.max_height.get(),
    )
}

fn detail(ctx: &Rc<Ctx>, model: &Model, id: Uuid, notice: Option<&str>) -> view::Built {
    let Some(account) = model.accounts.iter().find(|account| account.account.id == id) else {
        return accounts(ctx, model, notice);
    };
    let hooks = DetailHooks {
        on_back: {
            let ctx = ctx.clone();
            Rc::new(move || show_accounts(&ctx))
        },
        on_start_rename: {
            let ctx = ctx.clone();
            Rc::new(move || {
                *ctx.renaming.borrow_mut() = Some(id);
                window::rebuild(&ctx, false);
            })
        },
        on_commit_rename: {
            let ctx = ctx.clone();
            Rc::new(move |text| crate::nav::commit_rename(&ctx, id, text))
        },
        on_cancel_rename: {
            let ctx = ctx.clone();
            Rc::new(move || {
                *ctx.renaming.borrow_mut() = None;
                window::rebuild(&ctx, false);
            })
        },
        on_remove: {
            let ctx = ctx.clone();
            Rc::new(move || {
                ctx.confirm_remove.set(true);
                window::rebuild(&ctx, false);
            })
        },
        on_confirm_remove: {
            let ctx = ctx.clone();
            Rc::new(move || crate::nav::remove_account(&ctx, id))
        },
        on_cancel_remove: {
            let ctx = ctx.clone();
            Rc::new(move || {
                ctx.confirm_remove.set(false);
                window::rebuild(&ctx, false);
            })
        },
        on_sign_in: {
            let ctx = ctx.clone();
            Rc::new(move || crate::nav::start_replace(&ctx, id))
        },
    };
    let renaming = *ctx.renaming.borrow() == Some(id);
    accounts_page::detail(account, renaming, ctx.confirm_remove.get(), notice, &hooks, ctx.max_height.get())
}

fn add(ctx: &Rc<Ctx>, _model: &Model) -> view::Built {
    let flow = ctx.add.borrow().clone();
    let hooks = add_hooks(ctx);
    let body = add_page::body(&flow, &hooks);
    let title = add_page::title(&flow);
    ui::page(
        &ui::nav_title(&title),
        &{
            let ctx = ctx.clone();
            Rc::new(move || crate::nav::back_add(&ctx))
        },
        &body,
        None,
        ctx.max_height.get(),
    )
}

fn add_hooks(ctx: &Rc<Ctx>) -> AddHooks {
    AddHooks {
        on_provider: {
            let ctx = ctx.clone();
            Rc::new(move |kind| crate::nav::choose_provider(&ctx, kind))
        },
        on_personal: {
            let ctx = ctx.clone();
            Rc::new(move || crate::nav::begin_signin(&ctx, ProviderKind::Copilot, LoginMethod::Default, None))
        },
        on_organization: {
            let ctx = ctx.clone();
            Rc::new(move || {
                ctx.add.borrow_mut().step = AddStep::Organization;
                ctx.add.borrow_mut().kind = Some(ProviderKind::Copilot);
                window::rebuild(&ctx, false);
            })
        },
        on_org_continue: {
            let ctx = ctx.clone();
            Rc::new(move || {
                let name = ctx.add.borrow().org.trim().to_string();
                if name.is_empty() {
                    return;
                }
                crate::nav::begin_signin(&ctx, ProviderKind::Copilot, LoginMethod::Organization(name), None);
            })
        },
        on_hub: {
            let ctx = ctx.clone();
            Rc::new(move || {
                ctx.add.borrow_mut().step = AddStep::Hub;
                window::rebuild(&ctx, false);
            })
        },
        on_hub_connect: {
            let ctx = ctx.clone();
            Rc::new(move || crate::nav::connect_hub(&ctx))
        },
        on_cancel: {
            let ctx = ctx.clone();
            Rc::new(move || crate::nav::back_add(&ctx))
        },
        on_try: {
            let ctx = ctx.clone();
            Rc::new(move || crate::nav::retry(&ctx))
        },
        on_submit: {
            let ctx = ctx.clone();
            Rc::new(move || crate::nav::submit_prompt(&ctx))
        },
        on_open: {
            let ctx = ctx.clone();
            Rc::new(move || {
                let prompt = ctx.add.borrow().prompt.clone();
                if let Some(url) = prompt.as_ref().and_then(crate::nav::prompt_url) {
                    ui::open_uri(url);
                }
            })
        },
        on_copy: {
            let ctx = ctx.clone();
            Rc::new(move || crate::nav::copy_prompt(&ctx))
        },
        on_draft: {
            let ctx = ctx.clone();
            Rc::new(move |text| ctx.add.borrow_mut().draft = text)
        },
        on_org: {
            let ctx = ctx.clone();
            Rc::new(move |text| ctx.add.borrow_mut().org = text)
        },
        on_hub_url: {
            let ctx = ctx.clone();
            Rc::new(move |text| ctx.add.borrow_mut().hub_url = text)
        },
        on_hub_key: {
            let ctx = ctx.clone();
            Rc::new(move |text| ctx.add.borrow_mut().hub_key = text)
        },
    }
}

fn settings(ctx: &Rc<Ctx>) -> view::Built {
    let snapshot = settings_page::Snapshot::read();
    let hooks = SettingsHooks {
        on_back: {
            let ctx = ctx.clone();
            Rc::new(move || show_dashboard(&ctx))
        },
        on_meter: {
            let ctx = ctx.clone();
            Rc::new(move |used| {
                if ctx.rebuilding.get() {
                    return;
                }
                let mode = if used { openusage_core::widget::DisplayMode::Used } else { openusage_core::widget::DisplayMode::Remaining };
                if model::display_mode(&Settings::open()) == mode {
                    return;
                }
                model::set_display_mode(mode);
                window::rebuild(&ctx, false);
            })
        },
        on_reset: {
            let ctx = ctx.clone();
            Rc::new(move |absolute| {
                if ctx.rebuilding.get() {
                    return;
                }
                let current = Settings::open().string(model::RESET_KEY).as_deref() == Some("absolute");
                if current == absolute {
                    return;
                }
                model::set_absolute_resets(absolute);
                window::rebuild(&ctx, false);
            })
        },
        on_menu: {
            let ctx = ctx.clone();
            Rc::new(move |index| {
                if ctx.rebuilding.get() {
                    return;
                }
                let raw = settings_page::raw_menu(index);
                if Settings::open().string(openusage_core::store::MENU_BAR_LIMIT_KEY).as_deref() == Some(raw) {
                    return;
                }
                if let Err(error) = Settings::open().set_string(openusage_core::store::MENU_BAR_LIMIT_KEY, raw) {
                    openusage_core::log::error("panel", &format!("could not save the menu bar setting: {error}"));
                }
                window::rebuild(&ctx, false);
            })
        },
        on_alert: {
            let ctx = ctx.clone();
            Rc::new(move |index, on| {
                if ctx.rebuilding.get() {
                    return;
                }
                let key = settings_page::alert_key(index);
                if Settings::open().bool(key) == on {
                    return;
                }
                if let Err(error) = Settings::open().set_bool(key, on) {
                    openusage_core::log::error("panel", &format!("could not save an alert setting: {error}"));
                }
                window::rebuild(&ctx, false);
            })
        },
    };
    settings_page::build(&snapshot, &hooks, ctx.max_height.get())
}

