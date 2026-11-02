//! The usage-breakdown card: the top models behind a history row, dollars, tokens, and share.

use std::rc::Rc;

use gtk4::prelude::*;

use openusage_core::kind::ProviderKind;
use openusage_core::metric::UsageBreakdown;

use crate::format::{self, ModelRow};
use crate::ui;

/// What the copy button puts on the clipboard: the provider and the figure that is showing.
#[derive(Clone)]
pub struct Pop {
    pub title: String,
    pub breakdown: UsageBreakdown,
    pub provider: String,
    pub kind: ProviderKind,
}

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
        let note = ui::text(
            "No model breakdown for this range.",
            &["caption", "secondary"],
        );
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

/// The card plus the corner copy button. A range with no models has nothing to copy.
pub fn with_copy(
    body: gtk4::Box,
    breakdown: &UsageBreakdown,
    on_copy: Rc<dyn Fn() -> bool>,
) -> gtk4::Widget {
    if breakdown.models.is_empty() {
        return body.upcast();
    }
    let overlay = gtk4::Overlay::new();
    overlay.set_child(Some(&body));
    overlay.add_overlay(&copy_button(on_copy));
    overlay.upcast()
}

/// Opens the breakdown. `keep` is the Mac's click: a popover that stays until a click outside or Escape,
/// with the Copy button. Without `keep` it is the hover preview: it takes no input grab and closes when
/// the pointer leaves the figure (a grabbing popup would swallow the click that follows the hover).
pub fn show_popover(
    anchor: &gtk4::Widget,
    title: &str,
    breakdown: &UsageBreakdown,
    on_copy: Rc<dyn Fn() -> bool>,
    keep: bool,
) -> gtk4::Popover {
    let popover = gtk4::Popover::new();
    popover.add_css_class("breakdown");
    let body = if keep { with_copy(card(title, breakdown), breakdown, on_copy) } else { card(title, breakdown).upcast() };
    popover.set_child(Some(&body));
    popover.set_parent(anchor);
    popover.set_autohide(keep);
    popover.set_can_target(keep);
    popover.popup();
    crate::menu::track(&popover);
    popover.connect_closed(|popover| {
        popover.unparent();
    });
    popover
}

fn copy_button(on_copy: Rc<dyn Fn() -> bool>) -> gtk4::Button {
    let button = gtk4::Button::with_label("Copy");
    button.add_css_class("copy-corner");
    button.set_halign(gtk4::Align::End);
    button.set_valign(gtk4::Align::Start);
    button.connect_clicked(move |button| {
        if on_copy() {
            button.set_label("Copied");
        }
    });
    button
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
