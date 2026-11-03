//! The panel chrome: tabs, the dashboard body, and the footer. Cards live in `cards`.

use std::rc::Rc;

use gtk4::gio;
use gtk4::prelude::*;
use uuid::Uuid;

use crate::icons;
use crate::menu::Act;
use crate::model::{self, Model};
use crate::priority;
use crate::state::{ClaimUi, Preview};
use crate::ui;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Priority,
    All,
}

impl Tab {
    pub fn initial() -> Self {
        match std::env::var("OPENUSAGE_TAB").as_deref() {
            Ok("all") | Ok("usage") => Self::All,
            _ => Self::Priority,
        }
    }
}

pub struct Actions {
    pub on_tab: Rc<dyn Fn(Tab)>,
    pub on_toggle_meter: Rc<dyn Fn()>,
    pub on_toggle_reset: Rc<dyn Fn()>,
    pub on_refresh: Rc<dyn Fn()>,
    pub on_fix: Rc<dyn Fn(Uuid)>,
    pub on_add: Rc<dyn Fn()>,
    pub on_menu: Rc<dyn Fn(usize, Act)>,
    pub on_reorder: Rc<dyn Fn(crate::drag::Kind, String, String, String)>,
    pub on_share: Rc<dyn Fn(Vec<String>)>,
    pub on_copy_breakdown: Rc<dyn Fn(crate::breakdown::Pop) -> bool>,
    pub on_claim: Rc<dyn Fn(Uuid)>,
    pub on_claim_reset: Rc<dyn Fn()>,
    pub on_claim_cancel: Rc<dyn Fn()>,
    pub on_spend_period: Rc<dyn Fn(openusage_core::spend::SpendPeriod)>,
    pub on_spend_metric: Rc<dyn Fn(crate::model::CardMetric)>,
}

pub struct Chrome {
    pub preview: Preview,
    pub claim: ClaimUi,
    pub banner: Option<String>,
}

pub struct Built {
    pub root: gtk4::Box,
    pub scroll: gtk4::ScrolledWindow,
    pub updated: Option<gtk4::Label>,
    pub next: Option<gtk4::Label>,
}

pub fn build(
    model: &Model,
    tab: Tab,
    refreshing: bool,
    actions: &Actions,
    chrome: &Chrome,
    max_height: i32,
) -> Built {
    let root = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
    root.add_css_class("panel");
    root.set_width_request(340);
    root.set_overflow(gtk4::Overflow::Hidden);

    let top = tabs(tab, &actions.on_tab);
    root.append(&top);

    let content = if model.all_metrics_hidden {
        hidden_metrics()
    } else if model.accounts.is_empty() {
        empty_state(&actions.on_add)
    } else {
        match tab {
            Tab::Priority => priority::view(
                model,
                &actions.on_toggle_meter,
                &actions.on_toggle_reset,
                &{
                    let on_tab = actions.on_tab.clone();
                    Rc::new(move || on_tab(Tab::All)) as Rc<dyn Fn()>
                },
                &actions.on_fix,
            ),
            Tab::All => crate::cards::all_usage(model, actions, chrome),
        }
    };
    let body = gtk4::Box::new(gtk4::Orientation::Vertical, 10);
    if let Some(banner) = &chrome.banner {
        let label = ui::text(banner, &["caption"]);
        label.set_wrap(true);
        label.add_css_class("banner");
        body.append(&label);
    }
    body.append(&content);
    body.set_margin_start(12);
    body.set_margin_end(12);
    body.set_margin_top(2);
    body.set_margin_bottom(8);

    let scroll = gtk4::ScrolledWindow::new();
    scroll.set_policy(gtk4::PolicyType::Never, gtk4::PolicyType::Automatic);
    scroll.set_propagate_natural_height(true);
    scroll.add_css_class("body");
    let (_, top_h, _, _) = top.measure(gtk4::Orientation::Vertical, 340);
    let footer_h = 52;
    scroll.set_max_content_height((max_height - top_h - footer_h).max(160));
    scroll.set_child(Some(&body));
    root.append(&scroll);

    let (footer, updated, next) = if model.accounts.is_empty() {
        (menu_footer(), None, None)
    } else {
        let (footer, updated, next) = footer(model, refreshing, &actions.on_refresh);
        (footer, Some(updated), Some(next))
    };
    // As on the Mac, no rule above the footer: scrolled content fades out before it (`undershoot`).
    root.append(&footer);

    Built {
        root,
        scroll,
        updated,
        next,
    }
}

fn tabs(selected: Tab, on_tab: &Rc<dyn Fn(Tab)>) -> gtk4::Box {
    let wrap = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
    wrap.set_margin_start(12);
    wrap.set_margin_end(12);
    wrap.set_margin_top(12);
    wrap.set_margin_bottom(8);

    let segments = gtk4::Box::new(gtk4::Orientation::Horizontal, 0);
    segments.add_css_class("segments");
    segments.set_homogeneous(true);
    let priority = segment("Priority", selected == Tab::Priority);
    let all = segment("All Usage", selected == Tab::All);
    all.set_group(Some(&priority));
    let on_priority = on_tab.clone();
    priority.connect_toggled(move |button| {
        if button.is_active() {
            on_priority(Tab::Priority);
        }
    });
    let on_all = on_tab.clone();
    all.connect_toggled(move |button| {
        if button.is_active() {
            on_all(Tab::All);
        }
    });
    segments.append(&priority);
    segments.append(&all);
    wrap.append(&segments);
    wrap
}

fn segment(label: &str, active: bool) -> gtk4::ToggleButton {
    let button = gtk4::ToggleButton::with_label(label);
    button.add_css_class("segment");
    button.set_active(active);
    button
}

fn hidden_metrics() -> gtk4::Widget {
    let label = ui::text(
        "Every metric is hidden. Turn them back on in Accounts.",
        &["secondary", "empty-copy"],
    );
    label.set_wrap(true);
    label.set_justify(gtk4::Justification::Center);
    label.set_halign(gtk4::Align::Center);
    label.set_max_width_chars(32);
    label.set_margin_top(22);
    label.set_margin_bottom(22);
    label.upcast()
}

fn footer(
    model: &Model,
    refreshing: bool,
    on_refresh: &Rc<dyn Fn()>,
) -> (gtk4::Box, gtk4::Label, gtk4::Label) {
    let footer = gtk4::Box::new(gtk4::Orientation::Horizontal, 8);
    footer.add_css_class("footer");
    footer.set_margin_start(14);
    footer.set_margin_end(12);
    footer.set_margin_top(8);
    footer.set_margin_bottom(12);

    let updated = gtk4::Label::new(Some(&model::updated_text(model.refreshed_at, model.now)));
    updated.add_css_class("secondary");
    updated.add_css_class("footer-text");
    updated.set_hexpand(true);
    updated.set_halign(gtk4::Align::Start);
    updated.set_xalign(0.0);
    footer.append(&updated);

    let next = gtk4::Label::new(Some(&model::next_text(
        model.refreshed_at,
        model.now,
        refreshing,
    )));
    next.add_css_class("footer-text");
    next.add_css_class("nums");
    let icon = gtk4::Label::new(Some("↻"));
    icon.add_css_class("refresh-icon");
    let child = gtk4::Box::new(gtk4::Orientation::Horizontal, 6);
    child.append(&next);
    child.append(&icon);

    let button = gtk4::Button::new();
    button.add_css_class("refresh");
    button.set_child(Some(&child));
    let on_refresh = on_refresh.clone();
    button.connect_clicked(move |_| on_refresh());
    footer.append(&button);
    footer.append(&options_button());
    (footer, updated, next)
}

fn menu_footer() -> gtk4::Box {
    let footer = gtk4::Box::new(gtk4::Orientation::Horizontal, 8);
    footer.add_css_class("footer");
    footer.set_margin_start(14);
    footer.set_margin_end(12);
    footer.set_margin_top(8);
    footer.set_margin_bottom(12);
    let spring = gtk4::Label::new(None);
    spring.set_hexpand(true);
    footer.append(&spring);
    footer.append(&options_button());
    footer
}

pub fn options_button() -> gtk4::MenuButton {
    let menu = gio::Menu::new();
    menu.append(Some("Accounts"), Some("win.accounts"));
    menu.append(Some("Settings"), Some("win.settings"));
    let button = gtk4::MenuButton::new();
    button.set_label("⋯");
    button.set_menu_model(Some(&menu));
    button.add_css_class("options");
    button.set_always_show_arrow(false);
    button
}

fn empty_state(on_add: &Rc<dyn Fn()>) -> gtk4::Widget {
    let box_ = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
    box_.set_halign(gtk4::Align::Center);
    box_.set_margin_top(22);
    box_.set_margin_bottom(26);
    box_.set_margin_start(16);
    box_.set_margin_end(16);

    let icon = icons::app_icon(56);
    icon.set_halign(gtk4::Align::Center);
    icon.set_margin_bottom(14);
    box_.append(&icon);

    let title = gtk4::Label::new(Some("Add an Account"));
    title.add_css_class("empty-headline");
    title.set_halign(gtk4::Align::Center);
    title.set_margin_bottom(7);
    box_.append(&title);

    let message = gtk4::Label::new(Some(
        "OpenUsage tracks however many accounts you'd like to monitor, even several of the same provider.",
    ));
    message.add_css_class("secondary");
    message.add_css_class("empty-copy");
    message.set_wrap(true);
    message.set_justify(gtk4::Justification::Center);
    message.set_max_width_chars(32);
    message.set_halign(gtk4::Align::Center);
    message.set_margin_bottom(16);
    box_.append(&message);

    let add = ui::button("Add Account", "primary", on_add.clone());
    add.set_halign(gtk4::Align::Center);
    box_.append(&add);
    box_.upcast()
}
