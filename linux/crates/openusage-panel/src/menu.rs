//! The account context menu: the error, Refresh, Copy Usage, provider links, Sign In Again, Remove.

use std::rc::Rc;

use gtk4::prelude::*;
use gtk4::gdk;

use crate::ui;

#[derive(Clone)]
pub enum Act {
    Info,
    Refresh,
    Copy,
    Open(String),
    SignIn,
    Remove,
}

#[derive(Clone)]
pub struct Item {
    pub title: String,
    pub act: Act,
    pub enabled: bool,
}

pub fn items(problem: Option<&str>, links: &[(String, String)], managed: bool) -> Vec<Item> {
    let mut rows = Vec::new();
    if let Some(problem) = problem.filter(|text| !text.is_empty()) {
        rows.push(Item { title: problem.to_string(), act: Act::Info, enabled: false });
    }
    rows.push(Item { title: "Refresh".into(), act: Act::Refresh, enabled: true });
    rows.push(Item { title: "Copy Usage".into(), act: Act::Copy, enabled: true });
    for (label, url) in links {
        rows.push(Item { title: label.clone(), act: Act::Open(url.clone()), enabled: true });
    }
    if !managed {
        rows.push(Item { title: "Sign In Again".into(), act: Act::SignIn, enabled: true });
        rows.push(Item { title: "Remove…".into(), act: Act::Remove, enabled: true });
    }
    rows
}

pub fn popup(anchor: &gtk4::Widget, x: f64, y: f64, rows: Vec<Item>, act: Rc<dyn Fn(Act)>) {
    let popover = gtk4::Popover::new();
    popover.add_css_class("menu-pop");
    popover.set_child(Some(&column(&rows, &act)));
    popover.set_parent(anchor);
    popover.set_has_arrow(false);
    popover.set_pointing_to(Some(&gdk::Rectangle::new(x as i32, y as i32, 1, 1)));
    popover.set_autohide(true);
    popover.popup();
    popover.connect_closed(|popover| {
        popover.unparent();
    });
}

pub fn inline(rows: &[Item], act: Rc<dyn Fn(Act)>) -> gtk4::Box {
    let box_ = column(rows, &act);
    box_.add_css_class("card");
    box_.set_margin_top(8);
    box_
}

fn column(rows: &[Item], act: &Rc<dyn Fn(Act)>) -> gtk4::Box {
    let box_ = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
    box_.add_css_class("menu-list");
    let mut saw_info = false;
    for (index, item) in rows.iter().enumerate() {
        let split = matches!(item.act, Act::Open(_)) && index > 0 && !matches!(rows[index - 1].act, Act::Open(_))
            || matches!(item.act, Act::SignIn);
        if split || (saw_info && index == 1) {
            let rule = gtk4::Separator::new(gtk4::Orientation::Horizontal);
            rule.add_css_class("hairline");
            box_.append(&rule);
        }
        if !item.enabled {
            saw_info = true;
            let label = ui::text(&item.title, &["menu-info", "problem"]);
            label.set_wrap(true);
            label.set_max_width_chars(32);
            label.set_margin_start(12);
            label.set_margin_end(12);
            label.set_margin_top(8);
            label.set_margin_bottom(8);
            box_.append(&label);
            continue;
        }
        let action = act.clone();
        let kind = item.act.clone();
        let button = gtk4::Button::new();
        button.add_css_class("menu-item");
        let label = ui::text(&item.title, &[]);
        label.set_xalign(0.0);
        label.set_hexpand(true);
        button.set_child(Some(&label));
        button.connect_clicked(move |_| action(kind.clone()));
        if matches!(item.act, Act::Remove) {
            button.add_css_class("destructive");
        }
        box_.append(&button);
    }
    box_
}

pub fn bind_right_click(widget: &gtk4::Widget, rows: Vec<Item>, act: Rc<dyn Fn(Act)>) {
    let gesture = gtk4::GestureClick::new();
    gesture.set_button(gdk::BUTTON_SECONDARY);
    gesture.connect_pressed(move |gesture, _, x, y| {
        let Some(widget) = gesture.widget() else { return };
        popup(&widget, x, y, rows.clone(), act.clone());
    });
    widget.add_controller(gesture);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_problem_leads_the_menu_and_a_hub_account_cannot_be_removed_here() {
        let links = vec![("Status Page".into(), "https://example.test".into())];
        let rows = items(Some("Claude sign-in expired."), &links, false);
        assert_eq!(rows[0].title, "Claude sign-in expired.");
        assert!(!rows[0].enabled);
        assert_eq!(rows[1].title, "Refresh");
        assert_eq!(rows[2].title, "Copy Usage");
        assert_eq!(rows[3].title, "Status Page");
        assert_eq!(rows[4].title, "Sign In Again");
        assert_eq!(rows[5].title, "Remove…");
        let managed = items(None, &links, true);
        assert!(managed.iter().all(|item| !matches!(item.act, Act::Remove | Act::SignIn)));
    }
}
