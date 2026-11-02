//! Apply a layout change, remember it for Ctrl+Z, and tell the bar to follow.

use std::rc::Rc;

use openusage_core::layout::{self, LayoutState};
use openusage_core::store::Settings;

use crate::breakdown;
use crate::drag::Kind;
use crate::share;
use crate::window::{self, Ctx};

#[derive(Clone)]
pub struct Hooks {
    pub move_provider: Rc<dyn Fn(String, String)>,
    pub move_account: Rc<dyn Fn(String, String)>,
    pub move_metric: Rc<dyn Fn(String, String, String)>,
    pub set_shown: Rc<dyn Fn(String, bool)>,
    pub share: Rc<dyn Fn(Vec<String>)>,
    pub copy_breakdown: Rc<dyn Fn(breakdown::Pop) -> bool>,
}

pub fn hooks(ctx: &Rc<Ctx>) -> Hooks {
    let move_provider = {
        let ctx = ctx.clone();
        Rc::new(move |dragged: String, target: String| {
            change(&ctx, move |layout| {
                let accounts = Settings::open().accounts();
                layout.move_provider(&accounts, &dragged, &target)
            });
        }) as Rc<dyn Fn(String, String)>
    };
    let move_account = {
        let ctx = ctx.clone();
        Rc::new(move |dragged: String, target: String| {
            change(&ctx, move |layout| {
                let accounts = Settings::open().accounts();
                layout.move_account(&accounts, &dragged, &target)
            });
        }) as Rc<dyn Fn(String, String)>
    };
    let move_metric = {
        let ctx = ctx.clone();
        Rc::new(move |scope: String, dragged: String, target: String| {
            change(&ctx, move |layout| {
                let accounts = Settings::open().accounts();
                let Some(account) = accounts.iter().find(|account| account.card_id() == scope)
                else {
                    return false;
                };
                layout.move_metric(account, &dragged, &target)
            });
        }) as Rc<dyn Fn(String, String, String)>
    };
    let set_shown = {
        let ctx = ctx.clone();
        Rc::new(move |id: String, shown: bool| {
            change(&ctx, move |layout| layout.set_hidden(&id, !shown));
        }) as Rc<dyn Fn(String, bool)>
    };
    let share_accounts = {
        let ctx = ctx.clone();
        Rc::new(move |ids: Vec<String>| share::copy_accounts(&ctx, &ids)) as Rc<dyn Fn(Vec<String>)>
    };
    let copy_breakdown = {
        let ctx = ctx.clone();
        Rc::new(move |pop| share::copy_breakdown(&ctx, &pop)) as Rc<dyn Fn(breakdown::Pop) -> bool>
    };
    Hooks {
        move_provider,
        move_account,
        move_metric,
        set_shown,
        share: share_accounts,
        copy_breakdown,
    }
}

pub fn ensure(ctx: &Ctx) -> LayoutState {
    if let Some(layout) = ctx.layout.borrow().clone() {
        return layout;
    }
    let settings = Settings::open();
    let loaded = layout::load_for(&settings, &settings.accounts());
    *ctx.layout.borrow_mut() = Some(loaded.clone());
    loaded
}

pub fn undo(ctx: &Rc<Ctx>) {
    let Some(previous) = ctx.undo.borrow_mut().undo() else {
        return;
    };
    store(ctx, previous);
}

pub fn dispatch(hooks: &Hooks, kind: Kind, scope: String, dragged: String, target: String) {
    match kind {
        Kind::Provider => (hooks.move_provider)(dragged, target),
        Kind::Account => (hooks.move_account)(dragged, target),
        Kind::Metric => (hooks.move_metric)(scope, dragged, target),
    }
}

fn change(ctx: &Rc<Ctx>, body: impl FnOnce(&mut LayoutState) -> bool) {
    let mut layout = ensure(ctx);
    let before = layout.clone();
    if !body(&mut layout) || layout == before {
        return;
    }
    ctx.undo.borrow_mut().push(before);
    store(ctx, layout);
}

fn store(ctx: &Rc<Ctx>, layout: LayoutState) {
    if let Err(error) = layout::save(&Settings::open(), &layout) {
        openusage_core::log::error("panel", &format!("could not save the layout: {error}"));
    }
    *ctx.layout.borrow_mut() = Some(layout);
    window::reload_accounts();
    window::rebuild(ctx, false);
}
