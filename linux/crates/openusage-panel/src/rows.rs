//! Metric rows and the Priority tab. Bar rows follow `WidgetData::row`: title and pace, the bar, then
//! the amount and the reset.

use std::rc::Rc;

use chrono::{DateTime, Utc};
use gtk4::prelude::*;

use openusage_core::dashboard::{DashboardAccount, PriorityBoard, PriorityItem, Reason};
use openusage_core::widget::{Mark, Severity};

use crate::icons;
use crate::model::Model;
use crate::prefers_dark;

pub fn account_body(account: &DashboardAccount, now: DateTime<Utc>, on_toggle: &Rc<dyn Fn()>) -> gtk4::Box {
    let box_ = gtk4::Box::new(gtk4::Orientation::Vertical, 12);
    if account.waiting {
        box_.append(&note("Waiting for usage", "secondary"));
        return box_;
    }
    if account.lines.is_empty() && account.problem.is_none() {
        box_.append(&note("No usage reported.", "secondary"));
    }
    for line in &account.lines {
        box_.append(&metric_row(&line.data.row(now), line.data.has_meter_style_toggle(), on_toggle));
    }
    if let Some(problem) = &account.problem {
        let label = note(problem, "problem");
        label.set_wrap(true);
        label.set_xalign(0.0);
        label.set_max_width_chars(36);
        box_.append(&label);
    }
    box_
}

fn metric_row(row: &openusage_core::widget::Row, toggle: bool, on_toggle: &Rc<dyn Fn()>) -> gtk4::Widget {
    match &row.mark {
        Mark::Bar { fill, severity, value, reset, used_up, marker } => {
            let marker = row.pace.is_some().then_some(*marker).flatten();
            bar_row(&row.title, row.pace.as_deref(), *fill, *severity, value, reset.as_deref(), *used_up, marker, toggle, on_toggle)
        }
        Mark::Value { value, detail, severity } => value_row(&row.title, value, detail.as_deref(), *severity).upcast(),
    }
}

fn bar_row(
    title: &str,
    pace: Option<&str>,
    fill: f64,
    severity: Severity,
    value: &str,
    reset: Option<&str>,
    used_up: bool,
    marker: Option<f64>,
    toggle: bool,
    on_toggle: &Rc<dyn Fn()>,
) -> gtk4::Widget {
    let block = gtk4::Box::new(gtk4::Orientation::Vertical, 2);
    let top = gtk4::Box::new(gtk4::Orientation::Horizontal, 8);
    let title_label = text(title, &["title"]);
    title_label.set_hexpand(true);
    title_label.set_halign(gtk4::Align::Start);
    title_label.set_ellipsize(pango::EllipsizeMode::End);
    top.append(&title_label);
    if let Some(pace) = pace {
        let pace_label = text(pace, &["pace", "secondary", "nums"]);
        pace_label.set_halign(gtk4::Align::End);
        top.append(&pace_label);
    }
    block.append(&top);
    block.append(&level_bar(fill, severity, marker));

    let bottom = gtk4::Box::new(gtk4::Orientation::Horizontal, 8);
    bottom.append(&amount(value, severity, toggle, on_toggle));
    let spring = gtk4::Label::new(None);
    spring.set_hexpand(true);
    bottom.append(&spring);
    if let Some(reset) = reset {
        let classes: &[&str] = if used_up { &["reset", "crit", "nums"] } else { &["reset", "secondary", "nums"] };
        bottom.append(&text(reset, classes));
    }
    block.append(&bottom);
    if used_up {
        block.set_opacity(0.5);
    }
    block.upcast()
}

fn value_row(title: &str, value: &str, detail: Option<&str>, severity: Severity) -> gtk4::Box {
    let line = gtk4::Box::new(gtk4::Orientation::Horizontal, 8);
    line.set_margin_top(1);
    line.set_margin_bottom(1);
    let title_label = text(title, &["title-quiet"]);
    title_label.set_hexpand(true);
    title_label.set_halign(gtk4::Align::Start);
    title_label.set_ellipsize(pango::EllipsizeMode::End);
    line.append(&title_label);

    let figures = gtk4::Box::new(gtk4::Orientation::Horizontal, 0);
    figures.set_halign(gtk4::Align::End);
    let mut value_classes = vec!["figure", "nums"];
    value_classes.push(severity_class(severity, false));
    figures.append(&text(value, &value_classes));
    if let Some(detail) = detail {
        figures.append(&text(&format!(" · {detail}"), &["figure", "secondary", "nums"]));
    }
    line.append(&figures);
    line
}

fn amount(value: &str, severity: Severity, toggle: bool, on_toggle: &Rc<dyn Fn()>) -> gtk4::Widget {
    let classes = ["figure", "nums", severity_class(severity, true)];
    if !toggle {
        return text(value, &classes).upcast();
    }
    let label = text(value, &classes);
    let button = gtk4::Button::new();
    button.add_css_class("plain");
    button.set_child(Some(&label));
    let on_toggle = on_toggle.clone();
    button.connect_clicked(move |_| on_toggle());
    button.upcast()
}

fn severity_class(severity: Severity, bar_amount: bool) -> &'static str {
    match severity {
        Severity::Warning => "warn",
        Severity::Critical => "crit",
        Severity::Normal if bar_amount => "secondary",
        Severity::Normal => "figure-strong",
    }
}

pub fn level_bar(fill: f64, severity: Severity, marker: Option<f64>) -> gtk4::DrawingArea {
    let area = gtk4::DrawingArea::new();
    area.set_content_height(9);
    area.set_hexpand(true);
    area.set_draw_func(move |_area, context, width, height| {
        paint_bar(context, width, height, fill, severity, marker);
    });
    area
}

fn paint_bar(context: &cairo::Context, width: i32, height: i32, fill: f64, severity: Severity, marker: Option<f64>) {
    let dark = prefers_dark();
    let bar_h = 5.0;
    let y = (height as f64 - bar_h) / 2.0;
    let radius = bar_h / 2.0;
    let track = if dark { (1.0, 1.0, 1.0, 0.12) } else { (0.0, 0.0, 0.0, 0.09) };
    rounded(context, 0.0, y, width as f64, bar_h, radius);
    context.set_source_rgba(track.0, track.1, track.2, track.3);
    let _ = context.fill();

    let clamped = fill.clamp(0.0, 1.0);
    if clamped > 0.0 {
        let filled = ((width as f64) * clamped).max(4.0).min(width as f64);
        context.save().ok();
        rounded(context, 0.0, y, width as f64, bar_h, radius);
        let _ = context.clip();
        context.rectangle(0.0, y, filled, bar_h);
        let (red, green, blue) = severity_rgb(severity, dark);
        context.set_source_rgb(red, green, blue);
        let _ = context.fill();
        let _ = context.restore();
    }

    if let Some(marker) = marker {
        let x = ((width as f64) * marker.clamp(0.0, 1.0)).clamp(1.0, width as f64 - 1.0);
        let tick_h = bar_h + 4.0;
        let tick_y = y - 2.0;
        rounded(context, x - 1.0, tick_y, 2.0, tick_h, 1.0);
        if dark {
            context.set_source_rgba(0.96, 0.96, 0.97, 0.92);
        } else {
            context.set_source_rgba(0.11, 0.11, 0.12, 0.78);
        }
        let _ = context.fill();
    }
}

fn severity_rgb(severity: Severity, dark: bool) -> (f64, f64, f64) {
    let hex = match (severity, dark) {
        (Severity::Warning, _) => 0xff9f0a,
        (Severity::Critical, true) => 0xff453a,
        (Severity::Critical, false) => 0xff3b30,
        (Severity::Normal, true) => 0x0a84ff,
        (Severity::Normal, false) => 0x007aff,
    };
    (((hex >> 16) & 0xff) as f64 / 255.0, ((hex >> 8) & 0xff) as f64 / 255.0, (hex & 0xff) as f64 / 255.0)
}

fn rounded(context: &cairo::Context, x: f64, y: f64, width: f64, height: f64, radius: f64) {
    let radius = radius.min(width / 2.0).min(height / 2.0).max(0.0);
    context.new_sub_path();
    context.arc(x + width - radius, y + radius, radius, -std::f64::consts::FRAC_PI_2, 0.0);
    context.arc(x + width - radius, y + height - radius, radius, 0.0, std::f64::consts::FRAC_PI_2);
    context.arc(x + radius, y + height - radius, radius, std::f64::consts::FRAC_PI_2, std::f64::consts::PI);
    context.arc(x + radius, y + radius, radius, std::f64::consts::PI, std::f64::consts::PI + std::f64::consts::FRAC_PI_2);
    context.close_path();
}

pub fn priority_view(model: &Model, on_toggle: &Rc<dyn Fn()>, on_show_all: &Rc<dyn Fn()>) -> gtk4::Widget {
    let board = PriorityBoard::make(&model.accounts, model.now);
    let notices: Vec<&DashboardAccount> = model
        .accounts
        .iter()
        .filter(|account| account.problem.as_deref().is_some_and(|text| !text.starts_with("Last updated")))
        .collect();
    let all_waiting = !model.accounts.is_empty() && model.accounts.iter().all(|account| account.waiting);
    let any_limit = model.accounts.iter().any(|account| account.lines.iter().any(|line| line.data.is_bounded()));

    if all_waiting {
        return centered_message("Waiting for Usage", None);
    }
    if board.attention.is_empty() && board.used_up.is_empty() && notices.is_empty() {
        if !any_limit {
            let box_ = gtk4::Box::new(gtk4::Orientation::Vertical, 4);
            box_.set_halign(gtk4::Align::Center);
            box_.set_margin_top(18);
            box_.set_margin_bottom(18);
            box_.append(&text("No Reported Limits", &["empty-title"]));
            box_.append(&link_button("View All Usage", on_show_all));
            return box_.upcast();
        }
        return all_clear();
    }

    let page = gtk4::Box::new(gtk4::Orientation::Vertical, 14);
    if !board.attention.is_empty() {
        page.append(&section("Running Out", attention_rows(&board, model.now, on_toggle), false));
    } else if any_limit && notices.is_empty() {
        page.append(&clear_row());
    }
    if !board.used_up.is_empty() {
        page.append(&section("Used Up", used_up_rows(&board, model.now), true));
    }
    if !notices.is_empty() {
        page.append(&section("Needs Attention", notice_rows(&notices), false));
    }
    page.upcast()
}

fn attention_rows(board: &PriorityBoard, now: DateTime<Utc>, on_toggle: &Rc<dyn Fn()>) -> gtk4::Box {
    let box_ = gtk4::Box::new(gtk4::Orientation::Vertical, 2);
    for item in &board.attention {
        box_.append(&attention_row(item, now, on_toggle));
    }
    box_
}

fn attention_row(item: &PriorityItem, now: DateTime<Utc>, on_toggle: &Rc<dyn Fn()>) -> gtk4::Box {
    let row = gtk4::Box::new(gtk4::Orientation::Horizontal, 10);
    row.set_size_request(-1, 46);
    row.append(&gauge(item.line.data.fraction(), item.reason.severity(), item.account.account.kind));

    let names = gtk4::Box::new(gtk4::Orientation::Vertical, 1);
    names.set_hexpand(true);
    names.set_valign(gtk4::Align::Center);
    let title = text(&item.line.data.title, &["prio-title"]);
    title.set_halign(gtk4::Align::Start);
    title.set_ellipsize(pango::EllipsizeMode::End);
    names.append(&title);
    let caption = text(account_name(&item.account.account), &["caption", "secondary"]);
    caption.set_halign(gtk4::Align::Start);
    caption.set_ellipsize(pango::EllipsizeMode::End);
    names.append(&caption);
    row.append(&names);

    let figures = gtk4::Box::new(gtk4::Orientation::Vertical, 1);
    figures.set_valign(gtk4::Align::Center);
    figures.set_halign(gtk4::Align::End);
    let toggle = item.line.data.has_meter_style_toggle();
    let value = amount(&item.line.data.amount_text(), item.reason.severity(), toggle, on_toggle);
    value.set_halign(gtk4::Align::End);
    figures.append(&value);
    if let Some(reset) = item.line.data.reset_text(now) {
        let reset = text(&reset, &["caption", "secondary", "nums"]);
        reset.set_halign(gtk4::Align::End);
        figures.append(&reset);
    }
    row.append(&figures);
    row
}

fn used_up_rows(board: &PriorityBoard, now: DateTime<Utc>) -> gtk4::Box {
    let box_ = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
    for item in &board.used_up {
        box_.append(&used_up_row(item, now));
    }
    box_
}

fn used_up_row(item: &PriorityItem, now: DateTime<Utc>) -> gtk4::Box {
    let row = gtk4::Box::new(gtk4::Orientation::Horizontal, 8);
    row.set_size_request(-1, 28);
    let icon = icons::image(item.account.account.kind, 15);
    icon.set_margin_end(2);
    row.append(&icon);
    let title = text(&item.line.data.title, &["prio-quiet"]);
    row.append(&title);
    let caption = text(account_name(&item.account.account), &["caption", "secondary"]);
    caption.set_hexpand(true);
    caption.set_halign(gtk4::Align::Start);
    caption.set_ellipsize(pango::EllipsizeMode::End);
    caption.set_margin_start(6);
    row.append(&caption);
    let note = match &item.reason {
        Reason::EmptyBalance => item.line.data.amount_text(),
        Reason::UsedUp { .. } => item.line.data.reset_text(now).unwrap_or_else(|| "Used up".into()),
        Reason::Low { .. } => String::new(),
    };
    if !note.is_empty() {
        row.append(&text(&note, &["figure", "nums"]));
    }
    row
}

fn notice_rows(notices: &[&DashboardAccount]) -> gtk4::Box {
    let box_ = gtk4::Box::new(gtk4::Orientation::Vertical, 6);
    for account in notices {
        let row = gtk4::Box::new(gtk4::Orientation::Horizontal, 8);
        row.append(&icons::image(account.account.kind, 16));
        let text_box = gtk4::Box::new(gtk4::Orientation::Vertical, 1);
        text_box.set_hexpand(true);
        let name = text(account_name(&account.account), &["prio-quiet"]);
        name.set_halign(gtk4::Align::Start);
        text_box.append(&name);
        if let Some(problem) = &account.problem {
            let problem = note(problem, "problem");
            problem.set_wrap(true);
            problem.set_xalign(0.0);
            problem.set_halign(gtk4::Align::Start);
            text_box.append(&problem);
        }
        row.append(&text_box);
        box_.append(&row);
    }
    box_
}

fn section(title: &str, rows: gtk4::Box, faded: bool) -> gtk4::Box {
    let box_ = gtk4::Box::new(gtk4::Orientation::Vertical, 2);
    let label = text(title, &["section", "secondary"]);
    label.set_halign(gtk4::Align::Start);
    label.set_margin_start(4);
    label.set_margin_bottom(2);
    box_.append(&label);
    rows.set_margin_start(4);
    rows.set_margin_end(4);
    box_.append(&rows);
    if faded {
        box_.set_opacity(0.5);
    }
    box_
}

fn gauge(fraction: f64, severity: Severity, kind: openusage_core::kind::ProviderKind) -> gtk4::Overlay {
    let area = gtk4::DrawingArea::new();
    area.set_content_width(32);
    area.set_content_height(32);
    area.set_draw_func(move |_area, context, width, height| {
        paint_gauge(context, width, height, fraction, severity);
    });
    let overlay = gtk4::Overlay::new();
    overlay.set_child(Some(&area));
    let icon = icons::image(kind, 16);
    icon.set_halign(gtk4::Align::Center);
    icon.set_valign(gtk4::Align::Center);
    overlay.add_overlay(&icon);
    overlay
}

fn paint_gauge(context: &cairo::Context, width: i32, height: i32, fraction: f64, severity: Severity) {
    let dark = prefers_dark();
    let cx = width as f64 / 2.0;
    let cy = height as f64 / 2.0;
    let radius = width as f64 / 2.0 - 2.0;
    context.set_line_width(3.0);
    context.set_line_cap(cairo::LineCap::Round);
    let track = if dark { (1.0, 1.0, 1.0, 0.12) } else { (0.0, 0.0, 0.0, 0.09) };
    context.set_source_rgba(track.0, track.1, track.2, track.3);
    context.arc(cx, cy, radius, 0.0, std::f64::consts::TAU);
    let _ = context.stroke();
    let fraction = fraction.clamp(0.0, 1.0);
    if fraction > 0.0 {
        let (red, green, blue) = severity_rgb(severity, dark);
        context.set_source_rgb(red, green, blue);
        context.arc(cx, cy, radius, -std::f64::consts::FRAC_PI_2, -std::f64::consts::FRAC_PI_2 + std::f64::consts::TAU * fraction);
        let _ = context.stroke();
    }
}

fn clear_row() -> gtk4::Box {
    let row = gtk4::Box::new(gtk4::Orientation::Horizontal, 10);
    row.set_size_request(-1, 46);
    row.set_margin_start(4);
    let mark = gtk4::Label::new(Some("✓"));
    mark.add_css_class("check-small");
    mark.set_size_request(32, 32);
    row.append(&mark);
    let names = gtk4::Box::new(gtk4::Orientation::Vertical, 1);
    names.append(&text("All Clear", &["prio-title"]));
    names.append(&text("Nothing is running out.", &["caption", "secondary"]));
    row.append(&names);
    row
}

fn all_clear() -> gtk4::Widget {
    let box_ = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
    box_.set_halign(gtk4::Align::Center);
    box_.set_margin_top(18);
    box_.set_margin_bottom(22);
    let outer = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
    outer.add_css_class("ring-outer");
    outer.set_size_request(110, 110);
    outer.set_halign(gtk4::Align::Center);
    let mid = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
    mid.add_css_class("ring-mid");
    mid.set_size_request(86, 86);
    mid.set_halign(gtk4::Align::Center);
    mid.set_valign(gtk4::Align::Center);
    mid.set_vexpand(true);
    let circle = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
    circle.add_css_class("check");
    circle.set_size_request(60, 60);
    circle.set_halign(gtk4::Align::Center);
    circle.set_valign(gtk4::Align::Center);
    let mark = gtk4::Label::new(Some("✓"));
    mark.add_css_class("check-mark");
    mark.set_halign(gtk4::Align::Center);
    mark.set_valign(gtk4::Align::Center);
    mark.set_vexpand(true);
    circle.append(&mark);
    mid.append(&circle);
    outer.append(&mid);
    box_.append(&outer);
    let title = text("All Clear", &["all-clear-title"]);
    title.set_margin_top(12);
    box_.append(&title);
    let subtitle = text("Nothing needs your attention right now.", &["secondary"]);
    subtitle.set_margin_top(3);
    box_.append(&subtitle);
    box_.upcast()
}

fn centered_message(title: &str, detail: Option<&str>) -> gtk4::Widget {
    let box_ = gtk4::Box::new(gtk4::Orientation::Vertical, 4);
    box_.set_halign(gtk4::Align::Center);
    box_.set_margin_top(28);
    box_.set_margin_bottom(28);
    box_.append(&text(title, &["empty-title", "secondary"]));
    if let Some(detail) = detail {
        box_.append(&text(detail, &["secondary"]));
    }
    box_.upcast()
}

fn link_button(label: &str, action: &Rc<dyn Fn()>) -> gtk4::Button {
    let button = gtk4::Button::with_label(label);
    button.add_css_class("link");
    let action = action.clone();
    button.connect_clicked(move |_| action());
    button
}

fn account_name(account: &openusage_core::account::Account) -> &str {
    if account.label.is_empty() { account.kind.display_name() } else { account.label.as_str() }
}

fn note(text_value: &str, class: &str) -> gtk4::Label {
    text(text_value, &[class])
}

fn text(value: &str, classes: &[&str]) -> gtk4::Label {
    let label = gtk4::Label::new(Some(value));
    label.set_xalign(0.0);
    for class in classes {
        label.add_css_class(class);
    }
    label
}
