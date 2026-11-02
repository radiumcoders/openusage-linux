//! Claim a limit reset: the button, the confirm step, and the outcome line.

use std::rc::Rc;

use gtk4::prelude::*;

use crate::state::ClaimPhase;
use crate::ui;

pub struct ClaimChrome {
    pub phase: ClaimPhase,
    pub on_use: Rc<dyn Fn()>,
    pub on_reset: Rc<dyn Fn()>,
    pub on_cancel: Rc<dyn Fn()>,
}

pub fn view(chrome: &ClaimChrome) -> gtk4::Widget {
    match &chrome.phase {
        ClaimPhase::Idle => {
            let button = ui::button("Claim", "claim", chrome.on_use.clone());
            button.set_halign(gtk4::Align::End);
            button.upcast()
        }
        ClaimPhase::Confirm { detail, .. } => confirm(detail.as_deref(), &chrome.on_reset, &chrome.on_cancel).upcast(),
        ClaimPhase::Working => {
            let label = ui::text("Resetting your usage…", &["caption", "secondary"]);
            label.set_margin_top(4);
            label.upcast()
        }
        ClaimPhase::Done(message) => {
            let label = ui::text(message, &["caption"]);
            label.set_wrap(true);
            label.set_margin_top(4);
            label.upcast()
        }
    }
}

fn confirm(detail: Option<&str>, on_reset: &Rc<dyn Fn()>, on_cancel: &Rc<dyn Fn()>) -> gtk4::Box {
    let box_ = gtk4::Box::new(gtk4::Orientation::Vertical, 6);
    box_.add_css_class("confirm");
    box_.set_margin_top(6);
    let title = ui::text("Use this reset?", &["title-quiet"]);
    box_.append(&title);
    let body = detail.unwrap_or("Immediately reset your usage limits. This can't be undone.");
    let copy = ui::text(body, &["caption", "secondary"]);
    copy.set_wrap(true);
    copy.set_max_width_chars(36);
    box_.append(&copy);
    let actions = gtk4::Box::new(gtk4::Orientation::Horizontal, 8);
    actions.set_homogeneous(true);
    actions.append(&ui::button("Reset", "primary", on_reset.clone()));
    actions.append(&ui::button("Cancel", "bordered", on_cancel.clone()));
    box_.append(&actions);
    box_
}
