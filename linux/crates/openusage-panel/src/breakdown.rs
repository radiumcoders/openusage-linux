//! The usage-breakdown card: the top models behind a history row, dollars, tokens, and share.

use gtk4::prelude::*;

use openusage_core::metric::UsageBreakdown;

use crate::format::{self, ModelRow};
use crate::ui;

pub fn card(title: &str, breakdown: &UsageBreakdown) -> gtk4::Box {
    let box_ = gtk4::Box::new(gtk4::Orientation::Vertical, 6);
    box_.add_css_class("breakdown-card");
    box_.set_margin_start(10);
    box_.set_margin_end(10);
    box_.set_margin_top(8);
    box_.set_margin_bottom(10);
    let heading = ui::text("Top Models", &["section", "secondary"]);
    box_.append(&heading);
    let rows = format::model_rows(breakdown);
    if rows.is_empty() {
        let empty = ui::text(title, &["secondary"]);
        empty.set_wrap(true);
        box_.append(&empty);
        let note = ui::text("No model breakdown for this range.", &["caption", "secondary"]);
        box_.append(&note);
        return box_;
    }
    let range = ui::text(title, &["caption", "secondary"]);
    box_.append(&range);
    for row in rows {
        box_.append(&model_line(&row));
    }
    box_
}

fn model_line(row: &ModelRow) -> gtk4::Box {
    let line = gtk4::Box::new(gtk4::Orientation::Horizontal, 8);
    let name = ui::text(&row.name, &["title-quiet"]);
    name.set_hexpand(true);
    name.set_ellipsize(pango::EllipsizeMode::End);
    line.append(&name);
    if !row.figures.is_empty() {
        let figures = ui::text(&row.figures, &["figure", "nums", "secondary"]);
        figures.set_halign(gtk4::Align::End);
        line.append(&figures);
    }
    line
}

pub fn show_popover(anchor: &gtk4::Widget, title: &str, breakdown: &UsageBreakdown) {
    let popover = gtk4::Popover::new();
    popover.add_css_class("breakdown");
    popover.set_child(Some(&card(title, breakdown)));
    popover.set_parent(anchor);
    popover.set_autohide(true);
    popover.popup();
    popover.connect_closed(|popover| {
        popover.unparent();
    });
}
