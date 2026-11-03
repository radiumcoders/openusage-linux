//! Shared controls: labels, the navigation bar, and opening or copying a link.

use std::rc::Rc;

use gtk4::prelude::*;
use gtk4::{gdk, gio};

use crate::view::Built;

pub fn text(value: &str, classes: &[&str]) -> gtk4::Label {
    let label = gtk4::Label::new(Some(value));
    label.set_xalign(0.0);
    for class in classes {
        label.add_css_class(class);
    }
    label
}

pub fn open_uri(uri: &str) {
    if let Err(error) = gio::AppInfo::launch_default_for_uri(uri, None::<&gio::AppLaunchContext>) {
        openusage_core::log::warn("panel", &format!("could not open a link: {error}"));
    }
}

pub fn copy_text(value: &str) {
    if let Some(display) = gdk::Display::default() {
        display.clipboard().set_text(value);
    }
}

pub fn button(label: &str, class: &str, action: Rc<dyn Fn()>) -> gtk4::Button {
    let button = gtk4::Button::with_label(label);
    button.add_css_class(class);
    button.connect_clicked(move |_| action());
    button
}

pub fn nav_bar(
    title: &gtk4::Widget,
    on_back: &Rc<dyn Fn()>,
    trailing: Option<&gtk4::Widget>,
) -> gtk4::Box {
    let bar = gtk4::Box::new(gtk4::Orientation::Horizontal, 8);
    bar.add_css_class("nav");
    bar.set_margin_start(8);
    bar.set_margin_end(8);
    bar.set_margin_top(10);
    bar.set_margin_bottom(6);
    let back = button("‹", "nav-back", on_back.clone());
    back.set_size_request(28, 28);
    bar.append(&back);
    title.set_hexpand(true);
    title.set_halign(gtk4::Align::Center);
    bar.append(title);
    if let Some(trailing) = trailing {
        bar.append(trailing);
    } else {
        let spacer = gtk4::Label::new(None);
        spacer.set_size_request(28, 28);
        bar.append(&spacer);
    }
    bar
}

pub fn nav_title(title: &str) -> gtk4::Widget {
    let label = text(title, &["nav-title"]);
    label.set_halign(gtk4::Align::Center);
    label.set_ellipsize(pango::EllipsizeMode::End);
    label.upcast()
}

/// A titled page with a scrolling body. `footer` sits under the scroll, the way Add Account does.
pub fn page(
    title: &gtk4::Widget,
    on_back: &Rc<dyn Fn()>,
    body: &impl IsA<gtk4::Widget>,
    footer: Option<&gtk4::Widget>,
    max_height: i32,
) -> Built {
    let root = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
    root.add_css_class("panel");
    root.set_width_request(340);
    root.set_overflow(gtk4::Overflow::Hidden);
    let bar = nav_bar(title, on_back, None);
    root.append(&bar);

    let scroll = gtk4::ScrolledWindow::new();
    crate::smooth::install(&scroll);
    scroll.set_policy(gtk4::PolicyType::Never, gtk4::PolicyType::Automatic);
    scroll.set_propagate_natural_height(true);
    scroll.add_css_class("body");
    let (_, bar_h, _, _) = bar.measure(gtk4::Orientation::Vertical, 340);
    let footer_h = if footer.is_some() { 56 } else { 12 };
    scroll.set_max_content_height((max_height - bar_h - footer_h).max(160));
    body.set_margin_start(12);
    body.set_margin_end(12);
    body.set_margin_top(2);
    body.set_margin_bottom(8);
    scroll.set_child(Some(body));
    root.append(&scroll);
    if let Some(footer) = footer {
        root.append(footer);
    }
    Built {
        root,
        scroll,
        updated: None,
        next: None,
    }
}

pub fn card() -> gtk4::Box {
    let card = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
    card.add_css_class("card");
    card
}

pub fn section(title: &str) -> gtk4::Label {
    let label = text(title, &["section", "secondary"]);
    label.set_margin_start(4);
    label.set_margin_bottom(4);
    label.set_margin_top(2);
    label
}

pub fn row_button(label: &str, class: &str, action: Rc<dyn Fn()>) -> gtk4::Button {
    let button = gtk4::Button::new();
    button.add_css_class(class);
    button.add_css_class("row-btn");
    button.set_hexpand(true);
    let label = text(label, &[]);
    label.set_xalign(0.0);
    label.set_hexpand(true);
    button.set_child(Some(&label));
    button.connect_clicked(move |_| action());
    button
}

pub fn segments(labels: &[&str], selected: usize, on_select: Rc<dyn Fn(usize)>) -> gtk4::Box {
    let wrap = gtk4::Box::new(gtk4::Orientation::Horizontal, 0);
    wrap.add_css_class("segments");
    wrap.set_homogeneous(true);
    let mut group: Option<gtk4::ToggleButton> = None;
    for (index, label) in labels.iter().enumerate() {
        let button = gtk4::ToggleButton::with_label(label);
        button.add_css_class("segment");
        button.set_active(index == selected);
        if let Some(group) = &group {
            button.set_group(Some(group));
        } else {
            group = Some(button.clone());
        }
        let on_select = on_select.clone();
        button.connect_toggled(move |button| {
            if button.is_active() {
                on_select(index);
            }
        });
        wrap.append(&button);
    }
    wrap
}

pub fn entry(placeholder: &str, secret: bool, text: &str) -> gtk4::Entry {
    let entry = gtk4::Entry::new();
    entry.set_placeholder_text(Some(placeholder));
    entry.set_visibility(!secret);
    entry.set_text(text);
    entry.set_hexpand(true);
    if secret {
        entry.set_input_purpose(gtk4::InputPurpose::Password);
    }
    entry
}
