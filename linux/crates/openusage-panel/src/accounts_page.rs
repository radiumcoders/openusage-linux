//! Accounts list and one account's page.

use std::rc::Rc;
use std::time::Duration;

use gtk4::glib;
use gtk4::prelude::*;
use uuid::Uuid;

use openusage_core::dashboard::DashboardAccount;
use openusage_core::descriptor::Descriptor;
use openusage_core::hub::Hub;
use openusage_core::layout::LayoutState;

use crate::drag::{self, Kind, Target};
use crate::icons;
use crate::ui;
use crate::view::Built;

pub struct ListHooks {
    pub on_back: Rc<dyn Fn()>,
    pub on_open: Rc<dyn Fn(Uuid)>,
    pub on_add: Rc<dyn Fn()>,
    pub on_disconnect: Rc<dyn Fn(Uuid)>,
    pub on_confirm_hub: Rc<dyn Fn()>,
    pub on_cancel_hub: Rc<dyn Fn()>,
    pub on_move: Rc<dyn Fn(String, String)>,
}

pub fn list(
    accounts: &[DashboardAccount],
    hubs: &[Hub],
    confirm_hub: Option<Uuid>,
    notice: Option<&str>,
    hooks: &ListHooks,
    max_height: i32,
) -> Built {
    let body = gtk4::Box::new(gtk4::Orientation::Vertical, 12);
    if let Some(notice) = notice {
        let label = ui::text(notice, &["problem"]);
        label.set_wrap(true);
        body.append(&label);
    }
    if accounts.is_empty() && hubs.is_empty() {
        let empty = ui::text("No Accounts", &["empty-title", "secondary"]);
        empty.set_halign(gtk4::Align::Center);
        empty.set_margin_top(28);
        empty.set_margin_bottom(12);
        body.append(&empty);
    } else if !accounts.is_empty() {
        let card = ui::card();
        let mut targets = Vec::new();
        for account in accounts {
            let row = account_row(account, &hooks.on_open);
            targets.push(Target {
                kind: Kind::Account,
                scope: "accounts".into(),
                id: account.account.card_id(),
                widget: row.clone().upcast(),
            });
            card.append(&row);
        }
        let on_move = hooks.on_move.clone();
        drag::bind_rows(
            &card,
            targets,
            Rc::new(move |_, _, dragged, target| on_move(dragged, target)),
        );
        body.append(&card);
    }
    for hub in hubs {
        body.append(&hub_row(hub, confirm_hub == Some(hub.id), hooks));
    }
    let footer = gtk4::Box::new(gtk4::Orientation::Horizontal, 0);
    footer.add_css_class("footer");
    footer.set_margin_start(12);
    footer.set_margin_end(12);
    footer.set_margin_top(8);
    footer.set_margin_bottom(12);
    let add = ui::button("Add New Account", "primary", hooks.on_add.clone());
    add.set_hexpand(true);
    footer.append(&add);
    ui::page(
        &ui::nav_title("Accounts"),
        &hooks.on_back,
        &body,
        Some(footer.upcast_ref()),
        max_height,
    )
}

fn account_row(account: &DashboardAccount, on_open: &Rc<dyn Fn(Uuid)>) -> gtk4::Box {
    let line = gtk4::Box::new(gtk4::Orientation::Horizontal, 0);
    let grip = gtk4::Label::new(Some("⠿"));
    grip.add_css_class("grip");
    grip.add_css_class("tertiary");
    drag::bind_source(&grip, Kind::Account, "accounts", &account.account.card_id());
    line.append(&grip);
    line.append(&account_button(account, on_open));
    line
}

fn account_button(account: &DashboardAccount, on_open: &Rc<dyn Fn(Uuid)>) -> gtk4::Button {
    let id = account.account.id;
    let on_open = on_open.clone();
    let button = gtk4::Button::new();
    button.add_css_class("list-row");
    button.set_hexpand(true);
    let row = gtk4::Box::new(gtk4::Orientation::Horizontal, 8);
    row.append(&icons::image(account.account.kind, 18));
    let name = ui::text(account.account.kind.display_name(), &["title-quiet"]);
    row.append(&name);
    if account.account.label != account.account.kind.display_name()
        && !account.account.label.is_empty()
    {
        let label = ui::text(&account.account.label, &["secondary"]);
        label.set_ellipsize(pango::EllipsizeMode::Middle);
        label.set_hexpand(true);
        row.append(&label);
    } else {
        let spring = gtk4::Label::new(None);
        spring.set_hexpand(true);
        row.append(&spring);
    }
    row.append(&ui::text("›", &["tertiary"]));
    button.set_child(Some(&row));
    button.connect_clicked(move |_| on_open(id));
    button
}

fn hub_row(hub: &Hub, confirming: bool, hooks: &ListHooks) -> gtk4::Box {
    let card = ui::card();
    let row = gtk4::Box::new(gtk4::Orientation::Vertical, 4);
    row.set_margin_start(12);
    row.set_margin_end(12);
    row.set_margin_top(10);
    row.set_margin_bottom(10);
    let title = gtk4::Box::new(gtk4::Orientation::Horizontal, 8);
    title.append(&ui::text("CLIProxyAPI", &["title-quiet"]));
    let host = ui::text(&hub.host, &["secondary"]);
    host.set_hexpand(true);
    host.set_ellipsize(pango::EllipsizeMode::Middle);
    title.append(&host);
    row.append(&title);
    row.append(&ui::text("Connected", &["caption", "secondary"]));
    card.append(&row);
    if confirming {
        card.append(&confirm_hub(hub, hooks));
    } else {
        let id = hub.id;
        let on_disconnect = hooks.on_disconnect.clone();
        let button = ui::row_button(
            "Disconnect",
            "destructive",
            Rc::new(move || on_disconnect(id)),
        );
        card.append(&button);
    }
    let wrap = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
    wrap.append(&card);
    wrap
}

fn confirm_hub(hub: &Hub, hooks: &ListHooks) -> gtk4::Box {
    let box_ = gtk4::Box::new(gtk4::Orientation::Vertical, 6);
    box_.add_css_class("confirm");
    box_.set_margin_start(12);
    box_.set_margin_end(12);
    box_.set_margin_bottom(12);
    let title = ui::text(
        &format!("Disconnect the hub at {}?", hub.host),
        &["title-quiet"],
    );
    title.set_wrap(true);
    box_.append(&title);
    let copy = ui::text(
        "Its accounts are removed from OpenUsage. Nothing changes on the hub.",
        &["caption", "secondary"],
    );
    copy.set_wrap(true);
    box_.append(&copy);
    let actions = gtk4::Box::new(gtk4::Orientation::Horizontal, 8);
    actions.set_homogeneous(true);
    actions.append(&ui::button(
        "Disconnect",
        "destructive-fill",
        hooks.on_confirm_hub.clone(),
    ));
    actions.append(&ui::button(
        "Cancel",
        "bordered",
        hooks.on_cancel_hub.clone(),
    ));
    box_.append(&actions);
    box_
}

pub struct DetailHooks {
    pub on_back: Rc<dyn Fn()>,
    pub on_start_rename: Rc<dyn Fn()>,
    pub on_commit_rename: Rc<dyn Fn(String)>,
    pub on_cancel_rename: Rc<dyn Fn()>,
    pub on_remove: Rc<dyn Fn()>,
    pub on_confirm_remove: Rc<dyn Fn()>,
    pub on_cancel_remove: Rc<dyn Fn()>,
    pub on_sign_in: Rc<dyn Fn()>,
    pub on_metric_shown: Rc<dyn Fn(String, bool)>,
    pub on_metric_move: Rc<dyn Fn(String, String)>,
}

pub fn detail(
    account: &DashboardAccount,
    layout: &LayoutState,
    renaming: bool,
    confirming: bool,
    error: Option<&str>,
    hooks: &DetailHooks,
    max_height: i32,
) -> Built {
    let title: gtk4::Widget = if renaming {
        rename_entry(
            &account.account.label,
            &hooks.on_commit_rename,
            &hooks.on_cancel_rename,
        )
        .upcast()
    } else {
        let name = account.account.label.clone();
        let on_start = hooks.on_start_rename.clone();
        let button = ui::button(&name, "nav-title", Rc::new(move || on_start()));
        button.upcast()
    };
    let body = gtk4::Box::new(gtk4::Orientation::Vertical, 12);
    if let Some(problem) = account.problem.as_deref() {
        let banner = ui::text(problem, &["problem"]);
        banner.set_wrap(true);
        banner.add_css_class("problem-banner");
        body.append(&banner);
    }
    if let Some(card) = metrics(account, layout, hooks) {
        body.append(&card);
    }
    body.append(&facts(account));
    let links = account.account.kind.links();
    if !links.is_empty() {
        let card = ui::card();
        for link in links {
            let url = link.url.to_string();
            card.append(&ui::row_button(
                link.label,
                "row-btn",
                Rc::new(move || ui::open_uri(&url)),
            ));
        }
        body.append(&card);
    }
    if account.account.hub.is_some() {
        body.append(&ui::text("Managed by CLIProxyAPI", &["secondary"]));
    } else {
        let needs_fix = account
            .problem
            .as_deref()
            .is_some_and(|text| !text.starts_with("Last updated"));
        if needs_fix {
            body.append(&ui::row_button(
                "Sign In Again",
                "link-row",
                hooks.on_sign_in.clone(),
            ));
        }
        if confirming {
            body.append(&confirm_remove(account, hooks));
        } else {
            body.append(&ui::row_button(
                "Remove Account…",
                "destructive",
                hooks.on_remove.clone(),
            ));
        }
        if let Some(error) = error {
            let label = ui::text(error, &["problem"]);
            label.set_wrap(true);
            body.append(&label);
        }
    }
    ui::page(&title, &hooks.on_back, &body, None, max_height)
}

fn metrics(
    account: &DashboardAccount,
    layout: &LayoutState,
    hooks: &DetailHooks,
) -> Option<gtk4::Box> {
    let descriptors = layout.ordered_descriptors(&account.account);
    if descriptors.is_empty() {
        return None;
    }
    let card = ui::card();
    let mut targets = Vec::new();
    for descriptor in &descriptors {
        let row = metric_row(
            descriptor,
            !layout.is_hidden(&descriptor.id),
            &hooks.on_metric_shown,
        );
        targets.push(Target {
            kind: Kind::Metric,
            scope: descriptor.provider_id.clone(),
            id: descriptor.id.clone(),
            widget: row.clone().upcast(),
        });
        card.append(&row);
    }
    let on_move = hooks.on_metric_move.clone();
    drag::bind_rows(
        &card,
        targets,
        Rc::new(move |_, _, dragged, target| on_move(dragged, target)),
    );
    Some(card)
}

fn metric_row(
    descriptor: &Descriptor,
    shown: bool,
    on_shown: &Rc<dyn Fn(String, bool)>,
) -> gtk4::Box {
    let row = gtk4::Box::new(gtk4::Orientation::Horizontal, 8);
    row.add_css_class("metric-row");
    let grip = gtk4::Label::new(Some("⠿"));
    grip.add_css_class("grip");
    grip.add_css_class("tertiary");
    drag::bind_source(&grip, Kind::Metric, &descriptor.provider_id, &descriptor.id);
    row.append(&grip);
    let title = ui::text(descriptor.title, &["title-quiet"]);
    title.set_hexpand(true);
    title.set_ellipsize(pango::EllipsizeMode::End);
    row.append(&title);
    let switch = gtk4::Switch::new();
    switch.add_css_class("metric");
    switch.set_active(shown);
    switch.set_valign(gtk4::Align::Center);
    let id = descriptor.id.clone();
    let on_shown = on_shown.clone();
    switch.connect_state_set(move |_, state| {
        let on_shown = on_shown.clone();
        let id = id.clone();
        glib::timeout_add_local(Duration::from_millis(1), move || {
            on_shown(id.clone(), state);
            glib::ControlFlow::Break
        });
        glib::Propagation::Proceed
    });
    row.append(&switch);
    row
}

fn facts(account: &DashboardAccount) -> gtk4::Box {
    let card = ui::card();
    card.append(&fact("Provider", account.account.kind.display_name()));
    card.append(&fact("Plan", account.plan.as_deref().unwrap_or("—")));
    let status = if account.waiting {
        "Waiting for Usage"
    } else if let Some(problem) = account.problem.as_deref() {
        problem
    } else {
        "Current"
    };
    card.append(&fact("Status", status));
    card
}

fn fact(name: &str, value: &str) -> gtk4::Box {
    let row = gtk4::Box::new(gtk4::Orientation::Horizontal, 8);
    row.add_css_class("fact");
    let name = ui::text(name, &["secondary"]);
    name.set_width_chars(10);
    row.append(&name);
    let value = ui::text(value, &["title-quiet"]);
    value.set_hexpand(true);
    value.set_wrap(true);
    value.set_ellipsize(pango::EllipsizeMode::End);
    row.append(&value);
    row
}

fn confirm_remove(account: &DashboardAccount, hooks: &DetailHooks) -> gtk4::Box {
    let box_ = gtk4::Box::new(gtk4::Orientation::Vertical, 6);
    box_.add_css_class("card");
    box_.add_css_class("confirm");
    let title = ui::text(
        &format!("Remove {}?", account.account.full_name()),
        &["title-quiet"],
    );
    title.set_wrap(true);
    box_.append(&title);
    let copy = ui::text(
        "Its sign-in is deleted from this computer. You can add the account again at any time.",
        &["caption", "secondary"],
    );
    copy.set_wrap(true);
    box_.append(&copy);
    let actions = gtk4::Box::new(gtk4::Orientation::Horizontal, 8);
    actions.set_homogeneous(true);
    actions.append(&ui::button(
        "Remove Account",
        "destructive-fill",
        hooks.on_confirm_remove.clone(),
    ));
    actions.append(&ui::button(
        "Cancel",
        "bordered",
        hooks.on_cancel_remove.clone(),
    ));
    box_.append(&actions);
    box_
}

fn rename_entry(
    name: &str,
    on_commit: &Rc<dyn Fn(String)>,
    on_cancel: &Rc<dyn Fn()>,
) -> gtk4::Entry {
    let entry = ui::entry("Name", false, name);
    entry.set_halign(gtk4::Align::Center);
    entry.set_width_chars(18);
    entry.add_css_class("rename");
    let commit = on_commit.clone();
    entry.connect_activate(move |entry| commit(entry.text().to_string()));
    // Clicking away keeps the name, as on the Mac. The entry's text child holds the focus, so this
    // watches focus leaving the entry, not the entry's own `has-focus`, which never turns on.
    let commit = on_commit.clone();
    let focus = gtk4::EventControllerFocus::new();
    focus.connect_leave(move |focus| {
        if let Some(entry) = focus.widget().and_downcast::<gtk4::Entry>() {
            commit(entry.text().to_string());
        }
    });
    entry.add_controller(focus);
    let cancel = on_cancel.clone();
    let keys = gtk4::EventControllerKey::new();
    keys.connect_key_pressed(move |_, key, _, _| {
        if key == gtk4::gdk::Key::Escape {
            cancel();
            gtk4::glib::Propagation::Stop
        } else {
            gtk4::glib::Propagation::Proceed
        }
    });
    entry.add_controller(keys);
    let entry_focus = entry.clone();
    entry.connect_realize(move |entry| {
        entry.select_region(0, -1);
        entry_focus.grab_focus();
    });
    entry
}
