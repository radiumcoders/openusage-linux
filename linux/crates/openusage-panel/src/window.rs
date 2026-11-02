//! Layer-shell window: one process, toggled open and closed, anchored to the bar's corner.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use gtk4::prelude::*;
use gtk4::{gdk, gio, glib};
use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};

use uuid::Uuid;

use crate::model;
use crate::position::{self, Corner};
use crate::state::{AddFlow, ClaimJob, ClaimUi, Preview, Screen, Signin};
use crate::view::Tab;
use crate::{forced_scheme, prefers_dark};

pub(crate) struct Ctx {
    pub(crate) window: gtk4::ApplicationWindow,
    pub(crate) screen: RefCell<Screen>,
    pub(crate) tab: Cell<Tab>,
    pub(crate) refreshed_at: Cell<Option<DateTime<Utc>>>,
    pub(crate) refreshing: Cell<bool>,
    pub(crate) rebuilding: Cell<bool>,
    /// A transparent full-screen layer under the panel: a click on it closes the panel, the way a click
    /// outside closes the Mac popover. (Focus loss can't be used: Hyprland's focus-follows-mouse takes
    /// focus from the panel without any click.)
    pub(crate) backdrop: Option<gtk4::Window>,
    pub(crate) dirty: Cell<bool>,
    pub(crate) shown: Cell<bool>,
    pub(crate) updated: RefCell<Option<gtk4::Label>>,
    pub(crate) next: RefCell<Option<gtk4::Label>>,
    pub(crate) scroll: RefCell<Option<gtk4::ScrolledWindow>>,
    pub(crate) monitors: RefCell<Vec<gio::FileMonitor>>,
    pub(crate) corner: Corner,
    pub(crate) max_height: Cell<i32>,
    pub(crate) renaming: RefCell<Option<Uuid>>,
    pub(crate) confirm_remove: Cell<bool>,
    pub(crate) confirm_hub: RefCell<Option<Uuid>>,
    pub(crate) notice: RefCell<Option<String>>,
    pub(crate) add: RefCell<AddFlow>,
    pub(crate) signin: RefCell<Option<Signin>>,
    pub(crate) claim: RefCell<ClaimUi>,
    pub(crate) claim_job: RefCell<Option<ClaimJob>>,
    pub(crate) launch_applied: Cell<bool>,
    pub(crate) preview: Cell<Preview>,
    pub(crate) layout: RefCell<Option<openusage_core::layout::LayoutState>>,
    pub(crate) undo: RefCell<crate::reorder::UndoStack>,
    pub(crate) pill: RefCell<Option<String>>,
    pub(crate) pill_gen: Cell<u64>,
}

pub fn handle(app: &gtk4::Application, toggle: bool) {
    if let Some(window) = app
        .active_window()
        .or_else(|| app.windows().into_iter().next())
    {
        if toggle && window.is_visible() {
            match ctx_from(app) {
                Some(ctx) => hide(&ctx),
                None => window.set_visible(false),
            }
            return;
        }
        if let Some(ctx) = ctx_from(app) {
            retarget(&ctx);
        }
        window.set_visible(true);
        window.present();
        if let Some(ctx) = ctx_from(app) {
            show(&ctx);
        }
        return;
    }
    let window = gtk4::ApplicationWindow::builder()
        .application(app)
        .title("OpenUsage")
        .build();
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
        window.set_margin(
            if corner.top { Edge::Top } else { Edge::Bottom },
            corner.vertical_margin,
        );
        window.set_margin(
            if corner.left { Edge::Left } else { Edge::Right },
            corner.horizontal_margin,
        );
        if let Some(monitor) = placement.output.as_deref().and_then(monitor_named) {
            window.set_monitor(Some(&monitor));
        }
    }
    window.set_overflow(gtk4::Overflow::Hidden);
    let backdrop = gtk4_layer_shell::is_supported().then(|| make_backdrop(placement.output.as_deref()));

    let ctx = Rc::new(Ctx {
        window: window.clone(),
        screen: RefCell::new(Screen::Dashboard),
        tab: Cell::new(Tab::initial()),
        refreshed_at: Cell::new(None),
        refreshing: Cell::new(false),
        rebuilding: Cell::new(false),
        backdrop: backdrop.clone(),
        dirty: Cell::new(false),
        shown: Cell::new(false),
        updated: RefCell::new(None),
        next: RefCell::new(None),
        scroll: RefCell::new(None),
        monitors: RefCell::new(Vec::new()),
        corner,
        max_height: Cell::new(dump_max_height().unwrap_or_else(|| available_height(corner))),
        renaming: RefCell::new(None),
        confirm_remove: Cell::new(false),
        confirm_hub: RefCell::new(None),
        notice: RefCell::new(None),
        add: RefCell::new(AddFlow::fresh()),
        signin: RefCell::new(None),
        claim: RefCell::new(ClaimUi::default()),
        claim_job: RefCell::new(None),
        launch_applied: Cell::new(false),
        preview: Cell::new(Preview::default()),
        layout: RefCell::new(None),
        undo: RefCell::new(crate::reorder::UndoStack::default()),
        pill: RefCell::new(None),
        pill_gen: Cell::new(0),
    });
    install_commands(&ctx);
    install_dismiss(&ctx);
    watch_files(&ctx);
    glib::timeout_add_local(Duration::from_secs(1), {
        let ctx = ctx.clone();
        move || {
            tick_footer(&ctx);
            glib::ControlFlow::Continue
        }
    });
    glib::timeout_add_local(Duration::from_millis(120), {
        let ctx = ctx.clone();
        move || {
            if crate::session::poll(&ctx) {
                rebuild(&ctx, false);
            }
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
    if dump_max_height().is_none() {
        ctx.max_height.set(available_height(ctx.corner));
    }
    if ctx.dirty.get() {
        ctx.dirty.set(false);
        rebuild(ctx, false);
    }
    ctx.window.grab_focus();
    arm(ctx);
    crate::shot::schedule_dump(ctx);
}

pub(crate) use crate::shot::show_pill;

fn arm(ctx: &Rc<Ctx>) {
    if let Some(backdrop) = &ctx.backdrop {
        backdrop.set_visible(true);
    }
    let ctx = ctx.clone();
    glib::timeout_add_local(Duration::from_millis(250), move || {
        if ctx.window.is_visible() {
            // Map is asynchronous; ask again once the layer surface exists so Escape reaches it.
            ctx.window.grab_focus();
        }
        glib::ControlFlow::Break
    });
}

/// The click-outside catcher: transparent, over every app and the bar, under the panel (Overlay).
fn make_backdrop(output: Option<&str>) -> gtk4::Window {
    let backdrop = gtk4::Window::new();
    backdrop.add_css_class("openusage-backdrop");
    backdrop.set_decorated(false);
    backdrop.init_layer_shell();
    backdrop.set_layer(Layer::Top);
    backdrop.set_namespace(Some("openusage-backdrop"));
    backdrop.set_keyboard_mode(KeyboardMode::None);
    backdrop.set_exclusive_zone(-1);
    for edge in [Edge::Top, Edge::Bottom, Edge::Left, Edge::Right] {
        backdrop.set_anchor(edge, true);
    }
    if let Some(monitor) = output.and_then(monitor_named) {
        backdrop.set_monitor(Some(&monitor));
    }
    // A window with no child has no input region, and Hyprland passes input through a fully transparent
    // surface; the filler plus a 1% fill (style.css) make the backdrop catch the click.
    let filler = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
    filler.set_hexpand(true);
    filler.set_vexpand(true);
    backdrop.set_child(Some(&filler));

    backdrop
}

fn hide(ctx: &Rc<Ctx>) {
    ctx.window.set_visible(false);
    if let Some(backdrop) = &ctx.backdrop {
        backdrop.set_visible(false);
    }
}

fn install_dismiss(ctx: &Rc<Ctx>) {
    let keys = gtk4::EventControllerKey::new();
    let ctx_keys = ctx.clone();
    keys.connect_key_pressed(move |_controller, key, _code, _state| {
        if typing(&ctx_keys.window) {
            return glib::Propagation::Proceed;
        }
        if key == gdk::Key::Escape {
            if crate::menu::close_open() {
                return glib::Propagation::Stop;
            }
            if !crate::session::on_escape(&ctx_keys) {
                hide(&ctx_keys);
            }
            glib::Propagation::Stop
        } else if key == gdk::Key::Return || key == gdk::Key::KP_Enter {
            crate::session::on_return(&ctx_keys);
            glib::Propagation::Stop
        } else if (key == gdk::Key::z || key == gdk::Key::Z)
            && _state.contains(gdk::ModifierType::CONTROL_MASK)
        {
            crate::edit::undo(&ctx_keys);
            glib::Propagation::Stop
        } else {
            glib::Propagation::Proceed
        }
    });
    ctx.window.add_controller(keys);

    if let Some(backdrop) = &ctx.backdrop {
        let click = gtk4::GestureClick::new();
        click.set_button(0);
        let ctx_outside = ctx.clone();
        click.connect_pressed(move |_, _, _, _| {
            if !crate::share::rendering() {
                hide(&ctx_outside);
            }
        });
        backdrop.add_controller(click);
    }

    let ctx_close = ctx.clone();
    ctx.window.connect_close_request(move |_| {
        hide(&ctx_close);
        glib::Propagation::Stop
    });
}

pub(crate) fn rebuild(ctx: &Rc<Ctx>, reset_scroll: bool) {
    if ctx.rebuilding.get() {
        return;
    }
    if ctx.shown.get() && !ctx.window.is_visible() {
        ctx.dirty.set(true);
        return;
    }
    ctx.rebuilding.set(true);
    let preserved = if reset_scroll { 0.0 } else { scroll_value(ctx) };
    let model = model::load_with(&crate::edit::ensure(ctx));
    crate::launch::apply(ctx, &model);
    ctx.refreshed_at.set(model.refreshed_at);
    let built = crate::session::build(ctx, &model);
    *ctx.updated.borrow_mut() = built.updated;
    *ctx.next.borrow_mut() = built.next;
    *ctx.scroll.borrow_mut() = Some(built.scroll.clone());
    ctx.window.set_child(Some(&crate::shot::pill_overlay(
        &built.root,
        ctx.pill.borrow().as_deref(),
    )));
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

fn install_commands(ctx: &Rc<Ctx>) {
    let accounts = gio::SimpleAction::new("accounts", None);
    let ctx_accounts = ctx.clone();
    accounts.connect_activate(move |_, _| crate::session::show_accounts(&ctx_accounts));
    ctx.window.add_action(&accounts);

    let settings = gio::SimpleAction::new("settings", None);
    let ctx_settings = ctx.clone();
    settings.connect_activate(move |_, _| crate::session::show_settings(&ctx_settings));
    ctx.window.add_action(&settings);
}

fn typing(window: &gtk4::ApplicationWindow) -> bool {
    gtk4::prelude::GtkWindowExt::focus(window).is_some_and(|widget| widget.is::<gtk4::Entry>())
}

pub(crate) fn refresh_now(ctx: &Rc<Ctx>) {
    refresh(ctx);
}

pub(crate) fn reload_accounts() {
    if !openusage_core::daemon::signal_running(libc::SIGUSR2) {
        openusage_core::log::warn("panel", "no refresh daemon is running");
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
    ctx.scroll
        .borrow()
        .as_ref()
        .map(|scroll| scroll.vadjustment().value())
        .unwrap_or(0.0)
}

fn fit(ctx: &Ctx, root: &impl gtk4::prelude::IsA<gtk4::Widget>) {
    let (_, natural, _, _) = root.measure(gtk4::Orientation::Vertical, 340);
    let height = natural.clamp(160, ctx.max_height.get().max(160));
    ctx.window.set_default_size(340, height);
    ctx.window.set_width_request(340);
    ctx.window.set_height_request(height);
}

/// Moves the panel and its backdrop to the monitor under the pointer before they show again.
fn retarget(ctx: &Rc<Ctx>) {
    if !gtk4_layer_shell::is_supported() {
        return;
    }
    let Some(monitor) = position::pointer_output().as_deref().and_then(monitor_named) else { return };
    ctx.window.set_monitor(Some(&monitor));
    if let Some(backdrop) = &ctx.backdrop {
        backdrop.set_monitor(Some(&monitor));
    }
}

fn monitor_named(name: &str) -> Option<gdk::Monitor> {
    let display = gdk::Display::default()?;
    let monitors = display.monitors();
    for index in 0..monitors.n_items() {
        let Some(monitor) = monitors.item(index).and_downcast::<gdk::Monitor>() else {
            continue;
        };
        if monitor.connector().as_deref() == Some(name) {
            return Some(monitor);
        }
    }
    None
}

/// Dumps grow to the page instead of the monitor, so a screenshot includes the whole scroll.
fn dump_max_height() -> Option<i32> {
    std::env::var_os("OPENUSAGE_DUMP").map(|_| 4000)
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
                if let Some(monitor) =
                    monitor_directory(&parent, &ctx, move |file| file == directory)
                {
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
    let monitor = file
        .monitor_directory(gio::FileMonitorFlags::NONE, None::<&gio::Cancellable>)
        .ok()?;
    let ctx = ctx.clone();
    let pending = Rc::new(Cell::new(false));
    monitor.connect_changed(move |_monitor, file, _other, _event| {
        let Some(path) = file.path() else { return };
        let watched = path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(model::is_watched_file)
            || accept(&path);
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
