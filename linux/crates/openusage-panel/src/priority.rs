//! The Priority tab. Running Out, Used Up, and Needs Attention, most urgent first.

use std::rc::Rc;

use gtk4::prelude::*;
use uuid::Uuid;

use openusage_core::dashboard::{DashboardAccount, PriorityBoard, PriorityItem, Reason};
use openusage_core::widget::Severity;

use crate::format;
use crate::icons;
use crate::model::Model;
use crate::prefers_dark;
use crate::rows;
use crate::ui;

pub fn view(
    model: &Model,
    on_meter: &Rc<dyn Fn()>,
    on_reset: &Rc<dyn Fn()>,
    on_show_all: &Rc<dyn Fn()>,
    on_fix: &Rc<dyn Fn(Uuid)>,
) -> gtk4::Widget {
    let board = PriorityBoard::make(&model.accounts, model.now);
    let notices: Vec<&DashboardAccount> = model
        .accounts
        .iter()
        .filter(|account| account.problem.as_deref().is_some_and(|text| !text.starts_with("Last updated")))
        .collect();
    let all_waiting = !model.accounts.is_empty() && model.accounts.iter().all(|account| account.waiting);
    let any_limit = model.accounts.iter().any(|account| account.lines.iter().any(|line| line.data.is_bounded()));

    if all_waiting {
        return centered_message("Waiting for Usage");
    }
    if board.attention.is_empty() && board.used_up.is_empty() && notices.is_empty() {
        if !any_limit {
            let box_ = gtk4::Box::new(gtk4::Orientation::Vertical, 4);
            box_.set_halign(gtk4::Align::Center);
            box_.set_margin_top(18);
            box_.set_margin_bottom(18);
            box_.append(&ui::text("No Reported Limits", &["empty-title"]));
            box_.append(&link_button("View All Usage", on_show_all));
            return box_.upcast();
        }
        return all_clear();
    }

    let page = gtk4::Box::new(gtk4::Orientation::Vertical, 14);
    if !board.attention.is_empty() {
        page.append(&section("Running Out", attention_rows(&board, model, on_meter, on_reset), false));
    } else if any_limit && notices.is_empty() {
        page.append(&clear_row());
    }
    if !board.used_up.is_empty() {
        page.append(&section("Used Up", used_up_rows(&board, model), true));
    }
    if !notices.is_empty() {
        page.append(&section("Needs Attention", notice_rows(&notices, on_fix), false));
    }
    page.upcast()
}

fn attention_rows(board: &PriorityBoard, model: &Model, on_meter: &Rc<dyn Fn()>, on_reset: &Rc<dyn Fn()>) -> gtk4::Box {
    let box_ = gtk4::Box::new(gtk4::Orientation::Vertical, 2);
    for item in &board.attention {
        box_.append(&attention_row(item, model, on_meter, on_reset));
    }
    box_
}

fn attention_row(item: &PriorityItem, model: &Model, on_meter: &Rc<dyn Fn()>, on_reset: &Rc<dyn Fn()>) -> gtk4::Box {
    let row = gtk4::Box::new(gtk4::Orientation::Horizontal, 10);
    row.set_size_request(-1, 46);
    row.append(&gauge(item.line.data.fraction(), item.reason.severity(), item.account.account.kind));

    let names = gtk4::Box::new(gtk4::Orientation::Vertical, 1);
    names.set_hexpand(true);
    names.set_valign(gtk4::Align::Center);
    let title = ui::text(&item.line.data.title, &["prio-title"]);
    title.set_halign(gtk4::Align::Start);
    title.set_ellipsize(pango::EllipsizeMode::End);
    names.append(&title);
    let caption = ui::text(format::account_name(&item.account.account), &["caption", "secondary"]);
    caption.set_halign(gtk4::Align::Start);
    caption.set_ellipsize(pango::EllipsizeMode::End);
    names.append(&caption);
    row.append(&names);

    let figures = gtk4::Box::new(gtk4::Orientation::Vertical, 1);
    figures.set_valign(gtk4::Align::Center);
    figures.set_halign(gtk4::Align::End);
    let toggle = item.line.data.has_meter_style_toggle();
    let value = rows::amount(&item.line.data.amount_text(), item.reason.severity(), toggle, on_meter);
    value.set_halign(gtk4::Align::End);
    figures.append(&value);
    if let Some(reset) = format::short_reset(&item.line.data, model.now, model.absolute_resets) {
        let classes: &[&str] = &["caption", "secondary", "nums"];
        let label = ui::text(&reset, classes);
        let button = gtk4::Button::new();
        button.add_css_class("plain");
        button.set_halign(gtk4::Align::End);
        button.set_child(Some(&label));
        let on_reset = on_reset.clone();
        button.connect_clicked(move |_| on_reset());
        figures.append(&button);
    }
    row.append(&figures);
    row
}

fn used_up_rows(board: &PriorityBoard, model: &Model) -> gtk4::Box {
    let box_ = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
    for item in &board.used_up {
        box_.append(&used_up_row(item, model));
    }
    box_
}

fn used_up_row(item: &PriorityItem, model: &Model) -> gtk4::Box {
    let row = gtk4::Box::new(gtk4::Orientation::Horizontal, 8);
    row.set_size_request(-1, 28);
    let icon = icons::image(item.account.account.kind, 15);
    icon.set_margin_end(2);
    row.append(&icon);
    let title = ui::text(&item.line.data.title, &["prio-quiet"]);
    row.append(&title);
    let caption = ui::text(format::account_name(&item.account.account), &["caption", "secondary"]);
    caption.set_hexpand(true);
    caption.set_halign(gtk4::Align::Start);
    caption.set_ellipsize(pango::EllipsizeMode::End);
    caption.set_margin_start(6);
    row.append(&caption);
    let note = match &item.reason {
        Reason::EmptyBalance => item.line.data.amount_text(),
        Reason::UsedUp { .. } => format::short_reset(&item.line.data, model.now, model.absolute_resets).unwrap_or_else(|| "Used up".into()),
        Reason::Low { .. } => String::new(),
    };
    if !note.is_empty() {
        row.append(&ui::text(&note, &["figure", "nums"]));
    }
    row
}

fn notice_rows(notices: &[&DashboardAccount], on_fix: &Rc<dyn Fn(Uuid)>) -> gtk4::Box {
    let box_ = gtk4::Box::new(gtk4::Orientation::Vertical, 6);
    for account in notices {
        let row = gtk4::Box::new(gtk4::Orientation::Horizontal, 8);
        row.append(&icons::image(account.account.kind, 16));
        let text_box = gtk4::Box::new(gtk4::Orientation::Vertical, 1);
        text_box.set_hexpand(true);
        let name = ui::text(format::account_name(&account.account), &["prio-quiet"]);
        name.set_halign(gtk4::Align::Start);
        text_box.append(&name);
        if let Some(problem) = &account.problem {
            let problem = ui::text(problem, &["problem"]);
            problem.set_wrap(true);
            problem.set_xalign(0.0);
            problem.set_halign(gtk4::Align::Start);
            text_box.append(&problem);
        }
        row.append(&text_box);
        let id = account.account.id;
        let on_fix = on_fix.clone();
        let fix = ui::button("Fix", "fix", Rc::new(move || on_fix(id)));
        fix.set_valign(gtk4::Align::Center);
        row.append(&fix);
        box_.append(&row);
    }
    box_
}

fn section(title: &str, rows: gtk4::Box, faded: bool) -> gtk4::Box {
    let box_ = gtk4::Box::new(gtk4::Orientation::Vertical, 2);
    let label = ui::text(title, &["section", "secondary"]);
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
        let (red, green, blue) = rows::severity_rgb(severity, dark);
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
    names.append(&ui::text("All Clear", &["prio-title"]));
    names.append(&ui::text("Nothing is running out.", &["caption", "secondary"]));
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
    let title = ui::text("All Clear", &["all-clear-title"]);
    title.set_margin_top(12);
    box_.append(&title);
    let subtitle = ui::text("Nothing needs your attention right now.", &["secondary"]);
    subtitle.set_margin_top(3);
    box_.append(&subtitle);
    box_.upcast()
}

fn centered_message(title: &str) -> gtk4::Widget {
    let box_ = gtk4::Box::new(gtk4::Orientation::Vertical, 4);
    box_.set_halign(gtk4::Align::Center);
    box_.set_margin_top(28);
    box_.set_margin_bottom(28);
    box_.append(&ui::text(title, &["empty-title", "secondary"]));
    box_.upcast()
}

fn link_button(label: &str, action: &Rc<dyn Fn()>) -> gtk4::Button {
    let button = gtk4::Button::with_label(label);
    button.add_css_class("link");
    let action = action.clone();
    button.connect_clicked(move |_| action());
    button
}
