//! The dashboard screen: tabs, reorder, share, and the claim preview.

use std::rc::Rc;

use crate::model::Model;
use crate::state::{ClaimPhase, ClaimUi};
use crate::view::{self, Actions, Chrome};
use crate::window::{self, Ctx};

pub(crate) fn view(ctx: &Rc<Ctx>, model: &Model, notice: Option<String>) -> view::Built {
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
                crate::model::toggle_display_mode();
                window::rebuild(&ctx, false);
            })
        },
        on_toggle_reset: {
            let ctx = ctx.clone();
            Rc::new(move || {
                crate::model::toggle_resets();
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
        on_reorder: {
            let hooks = crate::edit::hooks(ctx);
            Rc::new(move |kind, scope, dragged, target| {
                crate::edit::dispatch(&hooks, kind, scope, dragged, target)
            })
        },
        on_share: crate::edit::hooks(ctx).share,
        on_copy_breakdown: crate::edit::hooks(ctx).copy_breakdown,
        on_claim: {
            let ctx = ctx.clone();
            Rc::new(move |id| {
                ctx.claim.borrow_mut().account = Some(id);
                ctx.claim.borrow_mut().phase = ClaimPhase::Confirm {
                    detail: None,
                    expiring: None,
                };
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
    let chrome = Chrome {
        preview: ctx.preview.get(),
        claim: claim_snapshot(ctx),
        banner: notice,
    };
    view::build(
        model,
        ctx.tab.get(),
        ctx.refreshing.get(),
        &actions,
        &chrome,
        ctx.max_height.get(),
    )
}

fn claim_snapshot(ctx: &Ctx) -> ClaimUi {
    let claim = ctx.claim.borrow();
    ClaimUi {
        account: claim.account,
        phase: claim.phase.clone(),
    }
}
