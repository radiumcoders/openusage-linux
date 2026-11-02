//! Branded PNG of a provider card or a usage breakdown. Padding, panel background, and the
//! OpenUsage mark, matching `ShareCardView` / `UsageBreakdownShareCard`.

use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use gtk4::gdk;
use gtk4::prelude::*;
use pango::EllipsizeMode;

use openusage_core::dashboard::DashboardAccount;
use openusage_core::kind::ProviderKind;

use crate::breakdown::{self, Pop};
use crate::edit;
use crate::icons;
use crate::model::{self, Model};
use crate::rows::{self, Extra, Toggles};
use crate::ui;
use crate::window::{self, Ctx};

thread_local! {
    static RENDERING: Cell<bool> = const { Cell::new(false) };
}

/// True while a share card is mapped for a snapshot. The panel must not treat that as a focus loss.
pub(crate) fn rendering() -> bool {
    RENDERING.with(Cell::get)
}

struct RenderingGuard;
impl Drop for RenderingGuard {
    fn drop(&mut self) {
        RENDERING.with(|flag| flag.set(false));
    }
}

pub fn confirmation(copied: bool, saved: Option<&Path>) -> String {
    if copied {
        "Copied".into()
    } else if let Some(path) = saved {
        path.display().to_string()
    } else {
        "Couldn't copy the screenshot".into()
    }
}

/// The account name on a shared card. An email is left out; several accounts still need a caption.
pub fn share_name(label: &str, provider: &str, siblings: usize) -> Option<String> {
    if label.contains('@') {
        return (siblings > 1).then(|| provider.to_string());
    }
    if siblings > 1 || (!label.is_empty() && label != provider) {
        let name = if label.is_empty() { provider } else { label };
        return Some(name.to_string());
    }
    None
}

pub fn file_stem(provider: &str, account: Option<&str>) -> String {
    match account.filter(|name| !name.is_empty() && !name.contains('@')) {
        Some(name) => format!("{provider}-{name}"),
        None => provider.to_string(),
    }
}

pub fn resolve_pictures(xdg: Option<&str>, user_dirs: &str, home: &Path) -> PathBuf {
    if let Some(dir) = xdg.map(str::trim).filter(|dir| !dir.is_empty()) {
        return PathBuf::from(expand_home(dir, home));
    }
    for line in user_dirs.lines() {
        let Some(value) = line.trim().strip_prefix("XDG_PICTURES_DIR=") else {
            continue;
        };
        let value = value.trim().trim_matches('"');
        if !value.is_empty() {
            return PathBuf::from(expand_home(value, home));
        }
    }
    home.join("Pictures")
}

fn expand_home(value: &str, home: &Path) -> String {
    let home = home.display().to_string();
    value.replace("${HOME}", &home).replace("$HOME", &home)
}

pub fn pictures_dir() -> PathBuf {
    let home = openusage_core::paths::home();
    let env = std::env::var("XDG_PICTURES_DIR").ok();
    let user_dirs =
        std::fs::read_to_string(home.join(".config/user-dirs.dirs")).unwrap_or_default();
    resolve_pictures(env.as_deref(), &user_dirs, &home)
}

pub fn copy_accounts(ctx: &Rc<Ctx>, ids: &[String]) {
    let model = model::load_with(&edit::ensure(ctx));
    let Some(sheet) = provider_sheet(&model, ids) else {
        openusage_core::log::warn("panel", "share: no account to copy");
        window::show_pill(ctx, confirmation(false, None));
        return;
    };
    publish(ctx, sheet.upcast_ref(), &stem_for(&model, ids));
}

pub fn copy_breakdown(ctx: &Rc<Ctx>, pop: &Pop) -> bool {
    if pop.breakdown.models.is_empty() {
        return false;
    }
    let sheet = breakdown_sheet(pop);
    publish(ctx, sheet.upcast_ref(), pop.provider.as_str())
}

pub fn write_provider(
    window: &impl IsA<gtk4::Window>,
    model: &Model,
    ids: &[String],
    path: &Path,
) -> bool {
    let Some(sheet) = provider_sheet(model, ids) else {
        return false;
    };
    write_widget(window, sheet.upcast_ref(), path)
}

pub fn write_breakdown(window: &impl IsA<gtk4::Window>, pop: &Pop, path: &Path) -> bool {
    let sheet = breakdown_sheet(pop);
    write_widget(window, sheet.upcast_ref(), path)
}

fn stem_for(model: &Model, ids: &[String]) -> String {
    let accounts: Vec<&DashboardAccount> = picked(model, ids);
    let provider = accounts
        .first()
        .map(|account| account.account.kind.display_name())
        .unwrap_or("OpenUsage");
    let account = (accounts.len() == 1).then(|| accounts[0].account.label.as_str());
    file_stem(provider, account)
}

fn publish(ctx: &Rc<Ctx>, widget: &gtk4::Widget, stem: &str) -> bool {
    let Some(texture) = render(&ctx.window, widget) else {
        openusage_core::log::error("panel", "share: could not render the card");
        window::show_pill(ctx, confirmation(false, None));
        return false;
    };
    let saved = save_copy(&texture, stem);
    let copied = copy_texture(&texture);
    if saved.is_none() {
        openusage_core::log::warn("panel", "share: could not save a copy of the screenshot");
    }
    window::show_pill(ctx, confirmation(copied, saved.as_deref()));
    copied || saved.is_some()
}

fn write_widget(window: &impl IsA<gtk4::Window>, widget: &gtk4::Widget, path: &Path) -> bool {
    let Some(texture) = render(window, widget) else {
        eprintln!("openusage-panel: share render failed");
        return false;
    };
    match texture.save_to_png(path) {
        Ok(()) => {
            eprintln!("openusage-panel: wrote {}", path.display());
            true
        }
        Err(error) => {
            eprintln!("openusage-panel: share png failed: {error}");
            false
        }
    }
}

fn render(host: &impl IsA<gtk4::Window>, widget: &gtk4::Widget) -> Option<gdk::Texture> {
    let host = host.upcast_ref::<gtk4::Window>();
    let width = 360;
    widget.set_halign(gtk4::Align::Fill);
    let (min_height, natural_height, _, _) = widget.measure(gtk4::Orientation::Vertical, width);
    let height = natural_height.max(min_height).max(1);
    widget.set_size_request(width, height);
    let stage = gtk4::Window::new();
    stage.add_css_class("openusage");
    if host.has_css_class("force-dark") {
        stage.add_css_class("force-dark");
    }
    if host.has_css_class("force-light") {
        stage.add_css_class("force-light");
    }
    stage.set_decorated(false);
    stage.set_resizable(false);
    stage.set_can_focus(false);
    stage.set_focus_on_click(false);
    stage.set_default_size(width, height);
    stage.set_child(Some(widget));
    // The widget draws once it is mapped. Keep the panel from treating that as a click outside.
    RENDERING.with(|flag| flag.set(true));
    let _guard = RenderingGuard;
    stage.present();
    let start = std::time::Instant::now();
    while start.elapsed() < std::time::Duration::from_millis(700) {
        let _ = gtk4::glib::MainContext::default().iteration(true);
        if widget.is_mapped() && widget.width() > 200 && widget.height() > 40 {
            break;
        }
    }
    if let Some(surface) = gtk4::prelude::NativeExt::surface(&stage) {
        surface.hide();
    }
    let texture = snapshot_widget(
        widget,
        widget.width().max(1),
        widget.height().max(1),
        stage.renderer().or_else(|| host.renderer()),
    );
    stage.close();
    texture
}

fn snapshot_widget(
    widget: &gtk4::Widget,
    width: i32,
    height: i32,
    renderer: Option<gtk4::gsk::Renderer>,
) -> Option<gdk::Texture> {
    let Some(renderer) = renderer else {
        eprintln!("openusage-panel: share has no renderer");
        return None;
    };
    let paintable = gtk4::WidgetPaintable::new(Some(widget));
    let snapshot = gtk4::Snapshot::new();
    // Match ShareCardRenderer's 4x image so the shared PNG stays sharp.
    snapshot.scale(4.0, 4.0);
    paintable.snapshot(&snapshot, width as f64, height as f64);
    let Some(node) = snapshot.to_node() else {
        eprintln!("openusage-panel: share snapshot was empty ({width}x{height})");
        return None;
    };
    Some(renderer.render_texture(&node, None))
}

fn copy_texture(texture: &gdk::Texture) -> bool {
    let Some(display) = gdk::Display::default() else {
        return false;
    };
    let provider = gdk::ContentProvider::for_value(&texture.to_value());
    display.clipboard().set_content(Some(&provider)).is_ok()
}

fn save_copy(texture: &gdk::Texture, stem: &str) -> Option<PathBuf> {
    let dir = pictures_dir().join("OpenUsage");
    std::fs::create_dir_all(&dir).ok()?;
    let safe: String = stem
        .chars()
        .map(|char| {
            if char.is_ascii_alphanumeric() || char == '-' {
                char
            } else {
                '-'
            }
        })
        .collect();
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0);
    let path = dir.join(format!("{safe}-{secs}.png"));
    texture.save_to_png(&path).ok()?;
    Some(path)
}

fn picked<'a>(model: &'a Model, ids: &[String]) -> Vec<&'a DashboardAccount> {
    model
        .accounts
        .iter()
        .filter(|account| ids.iter().any(|id| id == &account.account.card_id()))
        .collect()
}

fn provider_sheet(model: &Model, ids: &[String]) -> Option<gtk4::Box> {
    let accounts = picked(model, ids);
    let first = *accounts.first()?;
    let kind = first.account.kind;
    let siblings = accounts.len();
    let sheet = sheet();
    sheet.append(&provider_header(kind));
    let card = ui::card();
    let inner = gtk4::Box::new(gtk4::Orientation::Vertical, 8);
    inner.set_margin_start(12);
    inner.set_margin_end(12);
    inner.set_margin_top(10);
    inner.set_margin_bottom(12);
    let toggles = quiet_toggles();
    for (index, account) in accounts.iter().enumerate() {
        if index > 0 {
            let rule = gtk4::Separator::new(gtk4::Orientation::Horizontal);
            rule.add_css_class("hairline");
            inner.append(&rule);
        }
        if let Some(name) = share_name(&account.account.label, kind.display_name(), siblings) {
            inner.append(&caption(&name, account.plan.as_deref()));
        } else if let Some(plan) = &account.plan {
            let row = gtk4::Box::new(gtk4::Orientation::Horizontal, 0);
            let spring = gtk4::Label::new(None);
            spring.set_hexpand(true);
            row.append(&spring);
            row.append(&plan_badge(plan));
            inner.append(&row);
        }
        inner.append(&rows::account_body(
            account,
            model.now,
            model.absolute_resets,
            &toggles,
            &|_| Extra {
                breakdown: None,
                inline_breakdown: false,
                claim: None,
            },
            &|_, _| {},
        ));
    }
    card.append(&inner);
    sheet.append(&card);
    sheet.append(&watermark());
    Some(sheet)
}

fn breakdown_sheet(pop: &Pop) -> gtk4::Box {
    let sheet = sheet();
    sheet.append(&provider_header(pop.kind));
    let card = breakdown::card(&pop.title, &pop.breakdown);
    card.add_css_class("card");
    sheet.append(&card);
    sheet.append(&watermark());
    sheet
}

fn sheet() -> gtk4::Box {
    let sheet = gtk4::Box::new(gtk4::Orientation::Vertical, 12);
    sheet.add_css_class("share-sheet");
    sheet
}

fn provider_header(kind: ProviderKind) -> gtk4::Box {
    let row = gtk4::Box::new(gtk4::Orientation::Horizontal, 8);
    row.append(&icons::image(kind, 16));
    let name = ui::text(kind.display_name(), &["provider"]);
    name.set_hexpand(true);
    name.set_halign(gtk4::Align::Start);
    row.append(&name);
    row
}

fn caption(name: &str, plan: Option<&str>) -> gtk4::Box {
    let row = gtk4::Box::new(gtk4::Orientation::Horizontal, 6);
    let label = ui::text(name, &["account", "secondary"]);
    label.set_hexpand(true);
    label.set_ellipsize(pango::EllipsizeMode::End);
    row.append(&label);
    if let Some(plan) = plan {
        row.append(&plan_badge(plan));
    }
    row
}

fn plan_badge(plan: &str) -> gtk4::Box {
    let badge = gtk4::Box::new(gtk4::Orientation::Horizontal, 0);
    badge.add_css_class("plan-badge");
    let label = gtk4::Label::new(Some(plan));
    label.add_css_class("plan");
    label.add_css_class("secondary");
    badge.append(&label);
    badge
}

fn watermark() -> gtk4::Box {
    let row = gtk4::Box::new(gtk4::Orientation::Horizontal, 6);
    row.set_halign(gtk4::Align::Center);
    row.set_hexpand(true);
    row.append(&icons::app_icon(14));
    let label = ui::text(
        "Monitor Your AI Subscriptions with OpenUsage",
        &["caption", "secondary"],
    );
    label.set_ellipsize(EllipsizeMode::End);
    label.set_max_width_chars(36);
    label.set_hexpand(false);
    row.append(&label);
    row
}

fn quiet_toggles() -> Toggles {
    Toggles {
        meter: Rc::new(|| {}),
        reset: Rc::new(|| {}),
        copied: Rc::new(|_: String, _: openusage_core::metric::UsageBreakdown| false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_email_is_left_out_of_the_shared_name() {
        assert_eq!(share_name("ada@example.com", "Claude", 1), None);
        assert_eq!(
            share_name("ada@example.com", "Claude", 2).as_deref(),
            Some("Claude")
        );
        assert_eq!(share_name("Work", "Cursor", 2).as_deref(), Some("Work"));
        assert_eq!(share_name("Claude", "Claude", 1), None);
        assert_eq!(share_name("Jay", "ChatGPT", 1).as_deref(), Some("Jay"));
    }

    #[test]
    fn the_file_name_uses_the_account_when_it_is_not_an_email() {
        assert_eq!(file_stem("Cursor", Some("Work")), "Cursor-Work");
        assert_eq!(file_stem("Claude", Some("ada@example.com")), "Claude");
        assert_eq!(file_stem("Claude", None), "Claude");
    }

    #[test]
    fn pictures_dir_prefers_the_env_then_user_dirs_then_home() {
        let home = Path::new("/home/ada");
        assert_eq!(
            resolve_pictures(Some("/shots"), "", home),
            PathBuf::from("/shots")
        );
        let dirs = "XDG_PICTURES_DIR=\"$HOME/Pictures\"\n";
        assert_eq!(
            resolve_pictures(None, dirs, home),
            PathBuf::from("/home/ada/Pictures")
        );
        assert_eq!(
            resolve_pictures(Some("  "), dirs, home),
            PathBuf::from("/home/ada/Pictures")
        );
        assert_eq!(resolve_pictures(None, "", home), home.join("Pictures"));
    }

    #[test]
    fn the_pill_names_the_file_only_when_the_clipboard_is_missing() {
        assert_eq!(
            confirmation(true, Some(Path::new("/tmp/Claude.png"))),
            "Copied"
        );
        assert_eq!(
            confirmation(false, Some(Path::new("/tmp/Claude.png"))),
            "/tmp/Claude.png"
        );
        assert_eq!(confirmation(false, None), "Couldn't copy the screenshot");
    }
}
