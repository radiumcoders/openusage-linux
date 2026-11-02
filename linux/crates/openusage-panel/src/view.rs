//! The panel chrome: tabs, provider cards, and the footer. Row drawing lives in `rows`.

use std::cell::Cell;
use std::rc::Rc;

use gtk4::prelude::*;
use gtk4::gio;
use uuid::Uuid;

use openusage_core::claims;
use openusage_core::dashboard::DashboardAccount;

use crate::icons;
use crate::menu::{self, Act};
use crate::model::{self, Model};
use crate::priority;
use crate::resets::ClaimChrome;
use crate::rows::{self, Extra, Toggles};
use crate::state::{ClaimPhase, ClaimUi, Preview};
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
    pub on_claim: Rc<dyn Fn(Uuid)>,
    pub on_claim_reset: Rc<dyn Fn()>,
    pub on_claim_cancel: Rc<dyn Fn()>,
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

pub fn build(model: &Model, tab: Tab, refreshing: bool, actions: &Actions, chrome: &Chrome, max_height: i32) -> Built {
    let root = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
    root.add_css_class("panel");
    root.set_width_request(340);
    root.set_overflow(gtk4::Overflow::Hidden);

    let top = tabs(tab, &actions.on_tab);
    root.append(&top);

    let content = if model.accounts.is_empty() {
        empty_state(&actions.on_add)
    } else {
        match tab {
            Tab::Priority => priority::view(model, &actions.on_toggle_meter, &actions.on_toggle_reset, &{
                let on_tab = actions.on_tab.clone();
                Rc::new(move || on_tab(Tab::All)) as Rc<dyn Fn()>
            }, &actions.on_fix),
            Tab::All => all_usage(model, actions, chrome),
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
    let rule = gtk4::Separator::new(gtk4::Orientation::Horizontal);
    rule.add_css_class("footer-line");
    root.append(&rule);
    root.append(&footer);

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

fn all_usage(model: &Model, actions: &Actions, chrome: &Chrome) -> gtk4::Widget {
    let list = gtk4::Box::new(gtk4::Orientation::Vertical, 14);
    let toggles = Toggles {
        meter: actions.on_toggle_meter.clone(),
        reset: actions.on_toggle_reset.clone(),
        breakdown: Rc::new(|anchor, title, breakdown| crate::breakdown::show_popover(anchor, &title, &breakdown)),
    };
    let menu_shown = Cell::new(false);
    let breakdown_shown = Cell::new(false);
    for family in &model.families {
        list.append(&provider_card(model, family.indexes.as_slice(), actions, chrome, &toggles, &menu_shown, &breakdown_shown));
    }
    list.upcast()
}

fn provider_card(
    model: &Model,
    indexes: &[usize],
    actions: &Actions,
    chrome: &Chrome,
    toggles: &Toggles,
    menu_shown: &Cell<bool>,
    breakdown_shown: &Cell<bool>,
) -> gtk4::Box {
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
    let header = header(first, plan_on_header);
    if plan_on_header {
        bind_menu(&header, indexes[0], first, actions);
        inner.append(&header);
        if preview_menu(chrome, first, menu_shown) {
            inner.append(&menu_inline(indexes[0], first, actions));
        }
    } else {
        inner.append(&header);
    }

    for (offset, index) in indexes.iter().enumerate() {
        if offset > 0 {
            let rule = gtk4::Separator::new(gtk4::Orientation::Horizontal);
            rule.add_css_class("hairline");
            rule.set_margin_top(2);
            inner.append(&rule);
        }
        let account = &model.accounts[*index];
        inner.append(&account_block(model, *index, account, siblings, actions, chrome, toggles, menu_shown, breakdown_shown));
    }
    card.append(&inner);
    card
}

fn bind_menu(widget: &impl IsA<gtk4::Widget>, index: usize, account: &DashboardAccount, actions: &Actions) {
    let on_menu = actions.on_menu.clone();
    menu::bind_right_click(widget.upcast_ref(), menu_items(account), Rc::new(move |act| on_menu(index, act)));
}

fn menu_items(account: &DashboardAccount) -> Vec<menu::Item> {
    let links = account
        .account
        .kind
        .links()
        .into_iter()
        .map(|link| (link.label.to_string(), link.url.to_string()))
        .collect::<Vec<_>>();
    menu::items(account.problem.as_deref(), &links, account.account.hub.is_some())
}

fn preview_menu(chrome: &Chrome, account: &DashboardAccount, shown: &Cell<bool>) -> bool {
    if !chrome.preview.menu || shown.get() {
        return false;
    }
    let problem = account.problem.as_deref().is_some_and(|text| !text.starts_with("Last updated"));
    if !problem {
        return false;
    }
    shown.set(true);
    true
}

fn menu_inline(index: usize, account: &DashboardAccount, actions: &Actions) -> gtk4::Box {
    let on_menu = actions.on_menu.clone();
    menu::inline(&menu_items(account), Rc::new(move |act| on_menu(index, act)))
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

fn account_block(
    model: &Model,
    index: usize,
    account: &DashboardAccount,
    siblings: usize,
    actions: &Actions,
    chrome: &Chrome,
    toggles: &Toggles,
    menu_shown: &Cell<bool>,
    breakdown_shown: &Cell<bool>,
) -> gtk4::Box {
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
    bind_menu(&block, index, account, actions);
    if siblings > 1 && preview_menu(chrome, account, menu_shown) {
        block.append(&menu_inline(index, account, actions));
    }
    let snapshot = model.snapshots.get(&account.account.card_id());
    let offer = claims::offer(&account.account, snapshot).or_else(|| preview_offer(account, chrome.preview.claim));
    block.append(&rows::account_body(account, model.now, model.absolute_resets, toggles, &|line| {
        let breakdown = snapshot.and_then(|snapshot| snapshot.breakdowns.get(line.descriptor.key).cloned());
        let inline_breakdown = breakdown.is_some() && chrome.preview.breakdown && !breakdown_shown.replace(true);
        let claim = claim_chrome(account, line, offer.as_ref(), chrome, actions);
        Extra { breakdown, inline_breakdown, claim }
    }));
    block
}

fn preview_offer(account: &DashboardAccount, preview: bool) -> Option<openusage_core::claims::ClaimOffer> {
    if !preview {
        return None;
    }
    let has_resets = account.lines.iter().any(|line| line.descriptor.shows_reset_expiries() && line.data.has_data);
    has_resets.then(|| openusage_core::claims::ClaimOffer {
        confirm_detail: Some("Resets your weekly limit".into()),
        expiries: vec![model_now_plus_days(3)],
    })
}

fn model_now_plus_days(days: i64) -> chrono::DateTime<chrono::Utc> {
    chrono::Utc::now() + chrono::Duration::days(days)
}

fn claim_chrome(
    account: &DashboardAccount,
    line: &openusage_core::dashboard::DashboardLine,
    offer: Option<&openusage_core::claims::ClaimOffer>,
    chrome: &Chrome,
    actions: &Actions,
) -> Option<ClaimChrome> {
    if !line.descriptor.shows_reset_expiries() || !line.data.has_data {
        return None;
    }
    let offer = offer?;
    let active = chrome.claim.account == Some(account.account.id);
    let phase = if active { chrome.claim.phase.clone() } else { ClaimPhase::Idle };
    let phase = match phase {
        ClaimPhase::Idle => ClaimPhase::Idle,
        ClaimPhase::Confirm { .. } => ClaimPhase::Confirm {
            detail: offer.confirm_detail.clone(),
            expiring: offer.expiries.first().copied(),
        },
        other => other,
    };
    let id = account.account.id;
    let on_claim = actions.on_claim.clone();
    Some(ClaimChrome {
        phase,
        on_use: Rc::new(move || on_claim(id)),
        on_reset: actions.on_claim_reset.clone(),
        on_cancel: actions.on_claim_cancel.clone(),
    })
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
