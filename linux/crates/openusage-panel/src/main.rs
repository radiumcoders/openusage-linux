mod accounts_page;
mod add_page;
mod breakdown;
mod cards;
mod dash;
mod drag;
mod edit;
mod fixture;
mod format;
mod icons;
mod launch;
mod menu;
mod model;
mod nav;
mod position;
mod priority;
mod prompter;
mod reorder;
mod resets;
mod rows;
mod session;
mod settings_page;
mod share;
mod shot;
mod spend;
mod state;
mod ui;
mod view;
mod window;

use gtk4::prelude::*;
use gtk4::{gdk, gio};

fn main() {
    if let Some(root) = std::env::var_os("OPENUSAGE_FIXTURE") {
        fixture::write(std::path::Path::new(&root));
    }
    let app = gtk4::Application::builder()
        .application_id("dev.openusage.Panel")
        .flags(gio::ApplicationFlags::HANDLES_COMMAND_LINE)
        .build();

    app.connect_startup(|_| {
        install_css();
    });

    app.connect_command_line(|app, command_line| {
        // The guard releases on drop. The panel stays up after the window hides, so the hold lives for the process.
        std::mem::forget(app.hold());
        let toggle = command_line.arguments().iter().any(|arg| arg == "--toggle");
        window::handle(app, toggle);
        gtk4::glib::ExitCode::SUCCESS
    });

    let code = app.run();
    std::process::exit(code.into());
}

fn install_css() {
    let provider = gtk4::CssProvider::new();
    provider.load_from_string(include_str!("style.css"));
    if let Some(display) = gdk::Display::default() {
        gtk4::style_context_add_provider_for_display(
            &display,
            &provider,
            gtk4::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
    }
}

/// `OPENUSAGE_COLOR_SCHEME=light|dark` forces a scheme for screenshots. Unset, the CSS
/// `@media (prefers-color-scheme)` follows the session.
pub fn forced_scheme() -> Option<bool> {
    match std::env::var("OPENUSAGE_COLOR_SCHEME").as_deref() {
        Ok("dark") => Some(true),
        Ok("light") => Some(false),
        _ => None,
    }
}

pub fn prefers_dark() -> bool {
    forced_scheme().unwrap_or_else(system_prefers_dark)
}

fn system_prefers_dark() -> bool {
    use std::sync::OnceLock;
    static DARK: OnceLock<bool> = OnceLock::new();
    *DARK.get_or_init(|| {
        let Some(settings) = gtk4::Settings::default() else {
            return false;
        };
        match settings.gtk_interface_color_scheme() {
            gtk4::InterfaceColorScheme::Dark => return true,
            gtk4::InterfaceColorScheme::Light => return false,
            _ => {}
        }
        // Desktops that predate the color-scheme setting (GTK 4.20) still set the older prefer-dark flag.
        #[allow(deprecated)]
        if settings.is_gtk_application_prefer_dark_theme() {
            return true;
        }
        settings
            .gtk_theme_name()
            .is_some_and(|name| name.to_ascii_lowercase().contains("dark"))
    })
}
