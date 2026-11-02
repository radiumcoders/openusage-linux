//! Frame dumps for `OPENUSAGE_DUMP` and the transient "Copied" pill.

use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

use gtk4::glib;
use gtk4::prelude::*;

use crate::model;
use crate::window::{self, Ctx};

pub(crate) fn schedule_dump(ctx: &Rc<Ctx>) {
    let Some(path) = std::env::var_os("OPENUSAGE_DUMP") else {
        return;
    };
    let ctx = ctx.clone();
    let last_height = Rc::new(Cell::new(0));
    let tries = Rc::new(Cell::new(0u8));
    glib::timeout_add_local(Duration::from_millis(150), move || {
        tries.set(tries.get().saturating_add(1));
        let (width, height) = ctx
            .window
            .child()
            .map(|child| (child.width(), child.height()))
            .unwrap_or((0, 0));
        let settled = width >= 300 && height >= 160 && height == last_height.get();
        last_height.set(height);
        if settled || tries.get() >= 20 {
            dump_panel(&ctx, std::path::Path::new(&path));
            glib::ControlFlow::Break
        } else {
            glib::ControlFlow::Continue
        }
    });
}

fn dump_panel(ctx: &Ctx, path: &std::path::Path) {
    if let Some(child) = ctx.window.child() {
        let width = child.width().max(1);
        let height = child.height().max(1);
        let paintable = gtk4::WidgetPaintable::new(Some(&child));
        let snapshot = gtk4::Snapshot::new();
        paintable.snapshot(&snapshot, width as f64, height as f64);
        if let Some(node) = snapshot.to_node() {
            if let Some(renderer) = ctx.window.renderer() {
                match renderer.render_texture(&node, None).save_to_png(path) {
                    Ok(()) => eprintln!("openusage-panel: wrote {}", path.display()),
                    Err(error) => eprintln!("openusage-panel: dump failed: {error}"),
                }
            } else {
                eprintln!("openusage-panel: window has no renderer yet");
            }
        } else {
            eprintln!("openusage-panel: dump produced an empty frame");
        }
    }
    write_share_dumps(ctx);
    if let Some(app) = ctx.window.application() {
        app.quit();
    }
}

fn write_share_dumps(ctx: &Ctx) {
    let model = model::load_with(&crate::edit::ensure(ctx));
    if let Some(path) = std::env::var_os("OPENUSAGE_SHARE") {
        let ids = share_ids(&model);
        if !ids.is_empty() {
            crate::share::write_provider(&ctx.window, &model, &ids, std::path::Path::new(&path));
        }
    }
    if let Some(path) = std::env::var_os("OPENUSAGE_SHARE_BREAKDOWN") {
        if let Some(pop) = first_breakdown(&model) {
            crate::share::write_breakdown(&ctx.window, &pop, std::path::Path::new(&path));
        }
    }
}

fn share_ids(model: &model::Model) -> Vec<String> {
    let mut best = Vec::new();
    for family in &model.families {
        let ids: Vec<String> = family
            .indexes
            .iter()
            .map(|index| model.accounts[*index].account.card_id())
            .collect();
        if ids.len() > best.len() {
            best = ids;
        }
    }
    if best.is_empty() {
        model
            .accounts
            .iter()
            .take(1)
            .map(|account| account.account.card_id())
            .collect()
    } else {
        best
    }
}

fn first_breakdown(model: &model::Model) -> Option<crate::breakdown::Pop> {
    for account in &model.accounts {
        let Some(snapshot) = model.snapshots.get(&account.account.card_id()) else {
            continue;
        };
        let Some((key, breakdown)) = snapshot.breakdowns.iter().next() else {
            continue;
        };
        let title = account
            .lines
            .iter()
            .find(|line| line.descriptor.key == key.as_str())
            .map(|line| line.data.title.clone())
            .unwrap_or_else(|| key.clone());
        return Some(crate::breakdown::Pop {
            title,
            breakdown: breakdown.clone(),
            provider: account.account.kind.display_name().to_string(),
            kind: account.account.kind,
        });
    }
    None
}

pub(crate) fn show_pill(ctx: &Rc<Ctx>, text: String) {
    let gen = ctx.pill_gen.get().wrapping_add(1);
    ctx.pill_gen.set(gen);
    *ctx.pill.borrow_mut() = Some(text);
    window::rebuild(ctx, false);
    let ctx = ctx.clone();
    glib::timeout_add_local(Duration::from_millis(2500), move || {
        if ctx.pill_gen.get() == gen {
            *ctx.pill.borrow_mut() = None;
            window::rebuild(&ctx, false);
        }
        glib::ControlFlow::Break
    });
}

pub(crate) fn pill_overlay(root: &gtk4::Box, text: Option<&str>) -> gtk4::Widget {
    let Some(text) = text else {
        return root.clone().upcast();
    };
    let overlay = gtk4::Overlay::new();
    overlay.set_child(Some(root));
    let label = gtk4::Label::new(Some(text));
    label.add_css_class("copied-pill");
    label.set_wrap(true);
    label.set_max_width_chars(28);
    label.set_justify(gtk4::Justification::Center);
    label.set_halign(gtk4::Align::Center);
    label.set_valign(gtk4::Align::End);
    label.set_margin_bottom(64);
    label.set_can_target(false);
    overlay.add_overlay(&label);
    overlay.upcast()
}
