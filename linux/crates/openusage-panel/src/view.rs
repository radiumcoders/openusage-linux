//! The panel chrome: tabs, provider cards, and the footer. Row drawing lives in `rows`.

use std::rc::Rc;

use gtk4::prelude::*;

use openusage_core::dashboard::DashboardAccount;

use crate::icons;
use crate::model::{self, Model};
use crate::rows;

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
    pub on_toggle_mode: Rc<dyn Fn()>,
    pub on_refresh: Rc<dyn Fn()>,
}

pub struct Built {
    pub root: gtk4::Box,
    pub scroll: gtk4::ScrolledWindow,
    pub updated: Option<gtk4::Label>,
    pub next: Option<gtk4::Label>,
}

pub fn build(model: &Model, tab: Tab, refreshing: bool, actions: &Actions, max_height: i32) -> Built {
    let root = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
    root.add_css_class("panel");
    root.set_width_request(340);
    root.set_overflow(gtk4::Overflow::Hidden);

    let top = tabs(tab, &actions.on_tab);
    root.append(&top);

    let content = if model.accounts.is_empty() {
        empty_state()
    } else {
        match tab {
            Tab::Priority => rows::priority_view(model, &actions.on_toggle_mode, &{
                let on_tab = actions.on_tab.clone();
                Rc::new(move || on_tab(Tab::All)) as Rc<dyn Fn()>
            }),
            Tab::All => all_usage(model, &actions.on_toggle_mode),
        }
    };
    content.set_margin_start(12);
    content.set_margin_end(12);
    content.set_margin_top(2);
    content.set_margin_bottom(8);

    let scroll = gtk4::ScrolledWindow::new();
    scroll.set_policy(gtk4::PolicyType::Never, gtk4::PolicyType::Automatic);
    scroll.set_propagate_natural_height(true);
    scroll.add_css_class("body");
    let (_, top_h, _, _) = top.measure(gtk4::Orientation::Vertical, 340);
    let footer_h = if model.accounts.is_empty() { 0 } else { 52 };
    scroll.set_max_content_height((max_height - top_h - footer_h).max(160));
    scroll.set_child(Some(&content));
    root.append(&scroll);

    let (updated, next) = if model.accounts.is_empty() {
        (None, None)
    } else {
        let (footer, updated, next) = footer(model, refreshing, &actions.on_refresh);
        let rule = gtk4::Separator::new(gtk4::Orientation::Horizontal);
        rule.add_css_class("footer-line");
        root.append(&rule);
        root.append(&footer);
        (Some(updated), Some(next))
    };

    Built { root, scroll, updated, next }
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

fn all_usage(model: &Model, on_toggle: &Rc<dyn Fn()>) -> gtk4::Widget {
    let list = gtk4::Box::new(gtk4::Orientation::Vertical, 14);
    for family in &model.families {
        list.append(&provider_card(model, family.indexes.as_slice(), on_toggle));
    }
    list.upcast()
}

fn provider_card(model: &Model, indexes: &[usize], on_toggle: &Rc<dyn Fn()>) -> gtk4::Box {
    let card = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
    card.add_css_class("card");
    let inner = gtk4::Box::new(gtk4::Orientation::Vertical, 8);
    inner.set_margin_start(12);
    inner.set_margin_end(12);
    inner.set_margin_top(10);
    inner.set_margin_bottom(12);

    let first = &model.accounts[indexes[0]];
    let siblings = indexes.len();
    let plan_on_header = !model::shows_account_label(first, siblings);
    inner.append(&header(first, plan_on_header));

    for (offset, index) in indexes.iter().enumerate() {
        if offset > 0 {
            let rule = gtk4::Separator::new(gtk4::Orientation::Horizontal);
            rule.add_css_class("hairline");
            rule.set_margin_top(2);
            inner.append(&rule);
        }
        let account = &model.accounts[*index];
        inner.append(&account_block(account, siblings, on_toggle, model.now));
    }
    card.append(&inner);
    card
}

fn header(account: &DashboardAccount, plan_on_header: bool) -> gtk4::Box {
    let row = gtk4::Box::new(gtk4::Orientation::Horizontal, 8);
    row.append(&icons::image(account.account.kind, 16));
    let name = gtk4::Label::new(Some(account.account.kind.display_name()));
    name.add_css_class("provider");
    name.set_hexpand(true);
    name.set_halign(gtk4::Align::Start);
    name.set_xalign(0.0);
    name.set_ellipsize(pango::EllipsizeMode::End);
    row.append(&name);
    if plan_on_header {
        if let Some(plan) = account.plan.as_deref() {
            row.append(&plan_badge(plan));
        }
        if let Some(mark) = status_mark(account) {
            row.append(&mark);
        }
    }
    row
}

fn account_block(account: &DashboardAccount, siblings: usize, on_toggle: &Rc<dyn Fn()>, now: chrono::DateTime<chrono::Utc>) -> gtk4::Box {
    let block = gtk4::Box::new(gtk4::Orientation::Vertical, 6);
    if model::shows_account_label(account, siblings) {
        let caption = gtk4::Box::new(gtk4::Orientation::Horizontal, 6);
        let name = gtk4::Label::new(Some(&account.account.label));
        name.add_css_class("account");
        name.add_css_class("secondary");
        name.set_hexpand(true);
        name.set_halign(gtk4::Align::Start);
        name.set_ellipsize(if account.account.label.contains('@') {
            pango::EllipsizeMode::Middle
        } else {
            pango::EllipsizeMode::End
        });
        caption.append(&name);
        if let Some(plan) = account.plan.as_deref() {
            caption.append(&plan_badge(plan));
        }
        if let Some(mark) = status_mark(account) {
            caption.append(&mark);
        }
        block.append(&caption);
    }
    block.append(&rows::account_body(account, now, on_toggle));
    block
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

fn status_mark(account: &DashboardAccount) -> Option<gtk4::Label> {
    let label = if account.problem.as_deref().is_some_and(|text| !text.starts_with("Last updated")) {
        let label = gtk4::Label::new(Some("!"));
        label.add_css_class("status-warn");
        label
    } else if account.stale {
        let label = gtk4::Label::new(Some("◷"));
        label.add_css_class("tertiary");
        label
    } else {
        return None;
    };
    label.add_css_class("status");
    Some(label)
}

fn footer(model: &Model, refreshing: bool, on_refresh: &Rc<dyn Fn()>) -> (gtk4::Box, gtk4::Label, gtk4::Label) {
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

    let next = gtk4::Label::new(Some(&model::next_text(model.refreshed_at, model.now, refreshing)));
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
    (footer, updated, next)
}

fn empty_state() -> gtk4::Widget {
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
        "OpenUsage tracks however many accounts you'd like to monitor. Add them from the terminal and they show up here, even several of the same provider.",
    ));
    message.add_css_class("secondary");
    message.add_css_class("empty-copy");
    message.set_wrap(true);
    message.set_justify(gtk4::Justification::Center);
    message.set_max_width_chars(32);
    message.set_halign(gtk4::Align::Center);
    message.set_margin_bottom(16);
    box_.append(&message);

    let command = gtk4::Label::new(Some("openusage add <provider>"));
    command.add_css_class("command");
    command.set_selectable(true);
    command.set_halign(gtk4::Align::Center);
    let pill = gtk4::Box::new(gtk4::Orientation::Horizontal, 0);
    pill.add_css_class("command-pill");
    pill.set_halign(gtk4::Align::Center);
    pill.append(&command);
    box_.append(&pill);
    box_.upcast()
}
