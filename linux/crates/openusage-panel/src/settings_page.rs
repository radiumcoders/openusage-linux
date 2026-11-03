//! Settings the panel shares with the Mac app: alerts, the menu bar, used or left, and reset times.

use std::rc::Rc;

use gtk4::prelude::*;

use openusage_core::store::{Settings, MENU_BAR_LIMIT_KEY};

use crate::ui;
use crate::view::Built;

const ALERTS: [(&str, &str, &str); 3] = [
    (
        "Almost Out",
        "A limit drops under 10%",
        "openusage.notifications.underTenPercent",
    ),
    (
        "Cutting It Close",
        "A limit drops under 30%",
        "openusage.notifications.healthyToClose",
    ),
    (
        "Will Run Out",
        "A limit may run out before its reset",
        "openusage.notifications.closeToRunningOut",
    ),
];

const MENU_LABELS: [&str; 6] = [
    "App Icon Only",
    "Up to 1",
    "Up to 2",
    "Up to 3",
    "Up to 4",
    "Up to 5",
];
const MENU_RAWS: [&str; 6] = [
    "iconOnly",
    "upToOne",
    "upToTwo",
    "upToThree",
    "upToFour",
    "upToFive",
];

pub struct Hooks {
    pub on_back: Rc<dyn Fn()>,
    pub on_meter: Rc<dyn Fn(bool)>,
    pub on_reset: Rc<dyn Fn(bool)>,
    pub on_menu: Rc<dyn Fn(usize)>,
    pub on_alert: Rc<dyn Fn(usize, bool)>,
    pub on_total_spend: Rc<dyn Fn(bool)>,
}

pub struct Snapshot {
    pub used: bool,
    pub absolute: bool,
    pub menu: usize,
    pub alerts: [bool; 3],
    pub total_spend: bool,
}

impl Snapshot {
    pub fn read() -> Self {
        let settings = Settings::open();
        let menu = settings
            .string(MENU_BAR_LIMIT_KEY)
            .as_deref()
            .and_then(|raw| MENU_RAWS.iter().position(|item| *item == raw))
            .unwrap_or(3);
        Self {
            used: settings.string(crate::model::METER_KEY).as_deref() == Some("used"),
            absolute: settings.string(crate::model::RESET_KEY).as_deref() == Some("absolute"),
            menu,
            alerts: ALERTS.map(|(_, _, key)| settings.bool(key)),
            total_spend: crate::model::SpendChoice::read(&settings).shown,
        }
    }
}

pub fn raw_menu(index: usize) -> &'static str {
    MENU_RAWS.get(index).copied().unwrap_or("upToThree")
}

pub fn alert_key(index: usize) -> &'static str {
    ALERTS
        .get(index)
        .map(|(_, _, key)| *key)
        .unwrap_or(ALERTS[0].2)
}

pub fn build(snapshot: &Snapshot, hooks: &Hooks, max_height: i32) -> Built {
    let body = gtk4::Box::new(gtk4::Orientation::Vertical, 14);
    body.append(&ui::section("Quota Alerts"));
    let alerts = ui::card();
    for (index, (title, subtitle, _)) in ALERTS.iter().enumerate() {
        alerts.append(&alert_row(
            title,
            subtitle,
            snapshot.alerts[index],
            index,
            &hooks.on_alert,
        ));
    }
    body.append(&alerts);

    body.append(&ui::section("Usage"));
    let usage = ui::card();
    usage.append(&choice_row(
        "Show Usage As",
        &["Used", "Left"],
        if snapshot.used { 0 } else { 1 },
        {
            let on_meter = hooks.on_meter.clone();
            Rc::new(move |index| on_meter(index == 0))
        },
    ));
    usage.append(&choice_row(
        "Reset Times",
        &["Countdown", "Exact Time"],
        if snapshot.absolute { 1 } else { 0 },
        {
            let on_reset = hooks.on_reset.clone();
            Rc::new(move |index| on_reset(index == 1))
        },
    ));
    usage.append(&switch_row(
        "Show Total Spend",
        "The spend donut at the top of All Usage",
        snapshot.total_spend,
        hooks.on_total_spend.clone(),
    ));
    body.append(&usage);

    body.append(&ui::section("Menu Bar Shows"));
    let menu_card = ui::card();
    menu_card.append(&menu_row(snapshot.menu, &hooks.on_menu));
    body.append(&menu_card);
    let note = ui::text(
        "The bar shows the limits closest to running out. Any others show as a count.",
        &["caption", "secondary"],
    );
    note.set_wrap(true);
    note.set_margin_start(4);
    body.append(&note);

    ui::page(
        &ui::nav_title("Settings"),
        &hooks.on_back,
        &body,
        None,
        max_height,
    )
}

fn alert_row(
    title: &str,
    subtitle: &str,
    on: bool,
    index: usize,
    on_alert: &Rc<dyn Fn(usize, bool)>,
) -> gtk4::Box {
    let on_alert = on_alert.clone();
    switch_row(title, subtitle, on, Rc::new(move |on| on_alert(index, on)))
}

fn switch_row(title: &str, subtitle: &str, on: bool, on_toggle: Rc<dyn Fn(bool)>) -> gtk4::Box {
    let row = gtk4::Box::new(gtk4::Orientation::Horizontal, 8);
    row.add_css_class("fact");
    let text = gtk4::Box::new(gtk4::Orientation::Vertical, 1);
    text.set_hexpand(true);
    text.append(&ui::text(title, &["title-quiet"]));
    text.append(&ui::text(subtitle, &["caption", "secondary"]));
    row.append(&text);
    let switch = gtk4::Switch::new();
    switch.set_active(on);
    switch.set_valign(gtk4::Align::Center);
    switch.connect_active_notify(move |switch| on_toggle(switch.is_active()));
    row.append(&switch);
    row
}

fn choice_row(
    title: &str,
    labels: &[&str],
    selected: usize,
    on_select: Rc<dyn Fn(usize)>,
) -> gtk4::Box {
    let row = gtk4::Box::new(gtk4::Orientation::Vertical, 6);
    row.add_css_class("fact");
    row.append(&ui::text(title, &["title-quiet"]));
    row.append(&ui::segments(labels, selected, on_select));
    row
}

fn menu_row(selected: usize, on_menu: &Rc<dyn Fn(usize)>) -> gtk4::Box {
    let row = gtk4::Box::new(gtk4::Orientation::Horizontal, 8);
    row.add_css_class("fact");
    let label = ui::text("Show In Menu Bar", &["title-quiet"]);
    label.set_hexpand(true);
    row.append(&label);
    let dropdown = gtk4::DropDown::from_strings(&MENU_LABELS);
    dropdown.set_selected(selected as u32);
    dropdown.add_css_class("menu-limit");
    let on_menu = on_menu.clone();
    dropdown.connect_selected_notify(move |dropdown| {
        let selected = dropdown.selected();
        if selected != u32::MAX {
            on_menu(selected as usize);
        }
    });
    row.append(&dropdown);
    row
}
