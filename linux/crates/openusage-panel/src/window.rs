//! Layer-shell window: one process, toggled open and closed, anchored to the bar's corner.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use gtk4::prelude::*;
use gtk4::{gdk, gio, glib};
use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};

use openusage_core::widget::DisplayMode;

use crate::model;
use crate::position::{self, Corner};
use crate::view::{self, Actions, Tab};
use crate::{forced_scheme, prefers_dark};

struct Ctx {
    window: gtk4::ApplicationWindow,
    mode: Cell<DisplayMode>,
    tab: Cell<Tab>,
    refreshed_at: Cell<Option<DateTime<Utc>>>,
    refreshing: Cell<bool>,
    rebuilding: Cell<bool>,
    dismiss_armed: Cell<bool>,
    dirty: Cell<bool>,
    shown: Cell<bool>,
    updated: RefCell<Option<gtk4::Label>>,
    next: RefCell<Option<gtk4::Label>>,
    scroll: RefCell<Option<gtk4::ScrolledWindow>>,
    monitors: RefCell<Vec<gio::FileMonitor>>,
    corner: Corner,
    max_height: Cell<i32>,
}

pub fn handle(app: &gtk4::Application, toggle: bool) {
    if let Some(window) = app.active_window().or_else(|| app.windows().into_iter().next()) {
        if toggle && window.is_visible() {
            window.set_visible(false);
            return;
        }
        window.set_visible(true);
        window.present();
        if let Some(ctx) = ctx_from(app) {
            show(&ctx);
        }
        return;
    }
    let window = gtk4::ApplicationWindow::builder().application(app).title("OpenUsage").build();
    window.add_css_class("openusage");
    match forced_scheme() {
        Some(true) => window.add_css_class("force-dark"),
        Some(false) => window.add_css_class("force-light"),
        None if prefers_dark() => window.add_css_class("force-dark"),
        None => {}
    }
    window.set_decorated(false);
    window.set_resizable(false);
    window.set_can_focus(true);

    let placement = position::placement();
    let corner = placement.corner;
    if gtk4_layer_shell::is_supported() {
        window.init_layer_shell();
        window.set_layer(Layer::Overlay);
        window.set_namespace(Some("openusage-panel"));
        window.set_keyboard_mode(KeyboardMode::OnDemand);
        window.set_exclusive_zone(0);
        window.set_anchor(Edge::Top, corner.top);
        window.set_anchor(Edge::Bottom, !corner.top);
        window.set_anchor(Edge::Left, corner.left);
        window.set_anchor(Edge::Right, !corner.left);
        window.set_margin(if corner.top { Edge::Top } else { Edge::Bottom }, corner.vertical_margin);
        window.set_margin(if corner.left { Edge::Left } else { Edge::Right }, corner.horizontal_margin);
        if let Some(monitor) = placement.output.as_deref().and_then(monitor_named) {
            window.set_monitor(Some(&monitor));
        }
    }
    window.set_overflow(gtk4::Overflow::Hidden);

    let ctx = Rc::new(Ctx {
        window: window.clone(),
        mode: Cell::new(DisplayMode::Remaining),
        tab: Cell::new(Tab::initial()),
        refreshed_at: Cell::new(None),
        refreshing: Cell::new(false),
        rebuilding: Cell::new(false),
        dismiss_armed: Cell::new(false),
        dirty: Cell::new(false),
        shown: Cell::new(false),
        updated: RefCell::new(None),
        next: RefCell::new(None),
        scroll: RefCell::new(None),
        monitors: RefCell::new(Vec::new()),
        corner,
        max_height: Cell::new(available_height(corner)),
    });
    install_dismiss(&ctx);
    watch_files(&ctx);
    glib::timeout_add_local(Duration::from_secs(1), {
        let ctx = ctx.clone();
        move || {
            tick_footer(&ctx);
            glib::ControlFlow::Continue
        }
    });
    glib::timeout_add_local(Duration::from_secs(30), {
        let ctx = ctx.clone();
        move || {
            rebuild(&ctx, false);
            glib::ControlFlow::Continue
        }
    });
    remember(app, &ctx);
    rebuild(&ctx, true);
    window.present();
    show(&ctx);
}

fn show(ctx: &Rc<Ctx>) {
    ctx.shown.set(true);
    ctx.max_height.set(available_height(ctx.corner));
    if ctx.dirty.get() {
        ctx.dirty.set(false);
        rebuild(ctx, false);
    }
    ctx.window.grab_focus();
    arm(ctx);
    schedule_dump(ctx);
}

fn schedule_dump(ctx: &Rc<Ctx>) {
    let Some(path) = std::env::var_os("OPENUSAGE_DUMP") else { return };
    let ctx = ctx.clone();
    glib::timeout_add_local(Duration::from_millis(350), move || {
        dump_panel(&ctx, std::path::Path::new(&path));
        glib::ControlFlow::Break
    });
}

fn dump_panel(ctx: &Ctx, path: &std::path::Path) {
    let Some(child) = ctx.window.child() else { return };
    let width = child.width().max(1);
    let height = child.height().max(1);
    let paintable = gtk4::WidgetPaintable::new(Some(&child));
    let snapshot = gtk4::Snapshot::new();
    paintable.snapshot(&snapshot, width as f64, height as f64);
    let Some(node) = snapshot.to_node() else {
        eprintln!("openusage-panel: dump produced an empty frame");
        return;
    };
    let Some(renderer) = ctx.window.renderer() else {
        eprintln!("openusage-panel: window has no renderer yet");
        return;
    };
    match renderer.render_texture(&node, None).save_to_png(path) {
        Ok(()) => eprintln!("openusage-panel: wrote {}", path.display()),
        Err(error) => eprintln!("openusage-panel: dump failed: {error}"),
    }
}

fn arm(ctx: &Rc<Ctx>) {
    ctx.dismiss_armed.set(false);
    let ctx = ctx.clone();
    glib::timeout_add_local(Duration::from_millis(250), move || {
        if ctx.window.is_visible() {
            // Map is asynchronous; ask again once the layer surface exists so Escape reaches it.
            ctx.window.grab_focus();
            ctx.dismiss_armed.set(true);
        }
        glib::ControlFlow::Break
    });
}

fn hide(ctx: &Rc<Ctx>) {
    ctx.dismiss_armed.set(false);
    ctx.window.set_visible(false);
}

fn install_dismiss(ctx: &Rc<Ctx>) {
    let keys = gtk4::EventControllerKey::new();
    let ctx_keys = ctx.clone();
    keys.connect_key_pressed(move |_controller, key, _code, _state| {
        if key == gdk::Key::Escape {
            hide(&ctx_keys);
            glib::Propagation::Stop
        } else {
            glib::Propagation::Proceed
        }
    });
    ctx.window.add_controller(keys);

    let ctx_focus = ctx.clone();
    ctx.window.connect_notify_local(Some("is-active"), move |window, _| {
        if window.is_visible() && !window.is_active() && ctx_focus.dismiss_armed.get() && !ctx_focus.rebuilding.get() {
            hide(&ctx_focus);
        }
    });

    let ctx_close = ctx.clone();
    ctx.window.connect_close_request(move |_| {
        hide(&ctx_close);
        glib::Propagation::Stop
    });
}

fn rebuild(ctx: &Rc<Ctx>, reset_scroll: bool) {
    if ctx.rebuilding.get() {
        return;
    }
    if ctx.shown.get() && !ctx.window.is_visible() {
        ctx.dirty.set(true);
        return;
    }
    ctx.rebuilding.set(true);
    ctx.dismiss_armed.set(false);
    let preserved = if reset_scroll { 0.0 } else { scroll_value(ctx) };
    let model = model::load(ctx.mode.get());
    ctx.refreshed_at.set(model.refreshed_at);
    let built = view::build(&model, ctx.tab.get(), ctx.refreshing.get(), &actions(ctx), ctx.max_height.get());
    *ctx.updated.borrow_mut() = built.updated;
    *ctx.next.borrow_mut() = built.next;
    *ctx.scroll.borrow_mut() = Some(built.scroll.clone());
    ctx.window.set_child(Some(&built.root));
    fit(ctx, &built.root);
    if preserved > 0.0 {
        if let Some(scroll) = ctx.scroll.borrow().as_ref() {
            scroll.vadjustment().set_value(preserved);
        }
    }
    ctx.rebuilding.set(false);
    if ctx.window.is_visible() {
        arm(ctx);
    }
}

fn actions(ctx: &Rc<Ctx>) -> Actions {
    let tab_ctx = ctx.clone();
    let toggle_ctx = ctx.clone();
    let refresh_ctx = ctx.clone();
    Actions {
        on_tab: Rc::new(move |tab| {
            if tab_ctx.rebuilding.get() || tab_ctx.tab.get() == tab {
                return;
            }
            tab_ctx.tab.set(tab);
            rebuild(&tab_ctx, true);
        }),
        on_toggle_mode: Rc::new(move || {
            toggle_ctx.mode.set(match toggle_ctx.mode.get() {
                DisplayMode::Remaining => DisplayMode::Used,
                DisplayMode::Used => DisplayMode::Remaining,
            });
            rebuild(&toggle_ctx, false);
        }),
        on_refresh: Rc::new(move || refresh(&refresh_ctx)),
    }
}

fn refresh(ctx: &Rc<Ctx>) {
    model::signal_daemon();
    ctx.refreshing.set(true);
    tick_footer(ctx);
    let ctx = ctx.clone();
    glib::timeout_add_local(Duration::from_millis(1200), move || {
        ctx.refreshing.set(false);
        tick_footer(&ctx);
        glib::ControlFlow::Break
    });
}

fn tick_footer(ctx: &Ctx) {
    let now = Utc::now();
    let at = ctx.refreshed_at.get();
    if let Some(label) = ctx.updated.borrow().as_ref() {
        label.set_label(&model::updated_text(at, now));
    }
    if let Some(label) = ctx.next.borrow().as_ref() {
        label.set_label(&model::next_text(at, now, ctx.refreshing.get()));
    }
}

fn scroll_value(ctx: &Ctx) -> f64 {
    ctx.scroll.borrow().as_ref().map(|scroll| scroll.vadjustment().value()).unwrap_or(0.0)
}

fn fit(ctx: &Ctx, root: &impl gtk4::prelude::IsA<gtk4::Widget>) {
    let (_, natural, _, _) = root.measure(gtk4::Orientation::Vertical, 340);
    let height = natural.clamp(160, ctx.max_height.get().max(160));
    ctx.window.set_default_size(340, height);
    ctx.window.set_width_request(340);
    ctx.window.set_height_request(height);
}

fn monitor_named(name: &str) -> Option<gdk::Monitor> {
    let display = gdk::Display::default()?;
    let monitors = display.monitors();
    for index in 0..monitors.n_items() {
        let Some(monitor) = monitors.item(index).and_downcast::<gdk::Monitor>() else { continue };
        if monitor.connector().as_deref() == Some(name) {
            return Some(monitor);
        }
    }
    None
}

fn available_height(corner: Corner) -> i32 {
    let chrome = corner.vertical_margin + 12;
    let mut height = 720;
    if let Some(display) = gdk::Display::default() {
        let monitors = display.monitors();
        for index in 0..monitors.n_items() {
            if let Some(monitor) = monitors.item(index).and_downcast::<gdk::Monitor>() {
                height = height.max(monitor.geometry().height());
            }
        }
    }
    (height - chrome).clamp(320, 960)
}

fn watch_files(ctx: &Rc<Ctx>) {
    for directory in model::watch_dirs() {
        watch_dir(ctx, directory);
    }
}

fn watch_dir(ctx: &Rc<Ctx>, directory: std::path::PathBuf) {
    if !directory.is_dir() {
        if let Some(parent) = directory.parent() {
            let parent = parent.to_path_buf();
            if parent.is_dir() {
                let ctx = ctx.clone();
                let directory = directory.clone();
                if let Some(monitor) = monitor_directory(&parent, &ctx, move |file| file == directory) {
                    ctx.monitors.borrow_mut().push(monitor);
                }
            }
        }
        return;
    }
    let ctx_for_store = ctx.clone();
    if let Some(monitor) = monitor_directory(&directory, ctx, |_| true) {
        ctx_for_store.monitors.borrow_mut().push(monitor);
    }
}

fn monitor_directory(
    directory: &std::path::Path,
    ctx: &Rc<Ctx>,
    accept: impl Fn(&std::path::Path) -> bool + 'static,
) -> Option<gio::FileMonitor> {
    let file = gio::File::for_path(directory);
    let monitor = file.monitor_directory(gio::FileMonitorFlags::NONE, None::<&gio::Cancellable>).ok()?;
    let ctx = ctx.clone();
    let pending = Rc::new(Cell::new(false));
    monitor.connect_changed(move |_monitor, file, _other, _event| {
        let Some(path) = file.path() else { return };
        let watched = path.file_name().and_then(|name| name.to_str()).is_some_and(model::is_watched_file) || accept(&path);
        if !watched {
            return;
        }
        if pending.get() {
            return;
        }
        pending.set(true);
        let ctx = ctx.clone();
        let pending = pending.clone();
        glib::timeout_add_local(Duration::from_millis(80), move || {
            pending.set(false);
            rebuild(&ctx, false);
            glib::ControlFlow::Break
        });
    });
    Some(monitor)
}

thread_local! {
    static PANEL: RefCell<Option<Rc<Ctx>>> = const { RefCell::new(None) };
}

fn remember(_app: &gtk4::Application, ctx: &Rc<Ctx>) {
    PANEL.with(|slot| *slot.borrow_mut() = Some(ctx.clone()));
}

fn ctx_from(_app: &gtk4::Application) -> Option<Rc<Ctx>> {
    PANEL.with(|slot| slot.borrow().clone())
}
