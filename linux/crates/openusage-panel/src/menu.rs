//! Right-click menus, matching the Mac popover.
//! A provider card: Refresh, Manage Accounts…, Share Screenshot, and the provider's links.
//! An account: the reason when there is one, then Refresh, Rename, Edit Account…, Share Screenshot,
//! and Organization Billing for a Copilot organization account.

use std::rc::Rc;

use gtk4::gdk;
use gtk4::prelude::*;

use openusage_core::account::Account;
use openusage_core::kind::ProviderKind;

use crate::ui;

#[derive(Clone, PartialEq, Eq)]
pub enum Act {
    Info,
    Refresh,
    ManageAccounts,
    Rename,
    EditAccount,
    Share(Vec<String>),
    Open(String),
}

#[derive(Clone)]
pub struct Item {
    pub title: String,
    pub act: Act,
    pub enabled: bool,
    pub split: bool,
}

pub fn provider_menu(card_ids: &[String], links: &[(String, String)]) -> Vec<Item> {
    let mut rows = vec![
        item("Refresh", Act::Refresh, true, false),
        item("Manage Accounts…", Act::ManageAccounts, true, true),
        item(
            "Share Screenshot",
            Act::Share(card_ids.to_vec()),
            true,
            true,
        ),
    ];
    for (index, (label, url)) in links.iter().enumerate() {
        rows.push(item(label, Act::Open(url.clone()), true, index == 0));
    }
    rows
}

pub fn account_menu(card_id: &str, problem: Option<&str>, billing: Option<&str>) -> Vec<Item> {
    let mut rows = Vec::new();
    let lead = problem.is_some_and(|text| !text.is_empty());
    if let Some(problem) = problem.filter(|text| !text.is_empty()) {
        rows.push(item(problem, Act::Info, false, false));
    }
    rows.push(item("Refresh", Act::Refresh, true, lead));
    rows.push(item("Rename", Act::Rename, true, false));
    rows.push(item("Edit Account…", Act::EditAccount, true, false));
    rows.push(item(
        "Share Screenshot",
        Act::Share(vec![card_id.to_string()]),
        true,
        true,
    ));
    if let Some(url) = billing {
        rows.push(item(
            "Organization Billing",
            Act::Open(url.to_string()),
            true,
            true,
        ));
    }
    rows
}

/// GitHub billing for a Copilot organization account. Other accounts have none.
pub fn organization_billing(account: &Account) -> Option<String> {
    let org = account
        .organization
        .as_deref()
        .filter(|org| !org.is_empty())?;
    if account.kind != ProviderKind::Copilot {
        return None;
    }
    Some(format!(
        "https://github.com/organizations/{org}/settings/billing"
    ))
}

fn item(title: &str, act: Act, enabled: bool, split: bool) -> Item {
    Item {
        title: title.to_string(),
        act,
        enabled,
        split,
    }
}

thread_local! {
    /// The menu or breakdown popover that is open, if any. On Hyprland a popover on a layer surface
    /// doesn't take keyboard focus, so the panel's Escape closes it first, as Escape does on the Mac.
    static OPEN: std::cell::RefCell<Option<gtk4::glib::WeakRef<gtk4::Popover>>> = const { std::cell::RefCell::new(None) };
}

/// Remembers `popover` as the open one until it closes.
pub fn track(popover: &gtk4::Popover) {
    OPEN.with(|open| *open.borrow_mut() = Some(popover.downgrade()));
}

/// Closes the open popover; returns whether there was one.
pub fn close_open() -> bool {
    let open = OPEN.with(|open| open.borrow_mut().take()).and_then(|weak| weak.upgrade());
    match open {
        Some(popover) if popover.is_visible() => {
            popover.popdown();
            true
        }
        _ => false,
    }
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
    track(&popover);
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
    for item in rows {
        if item.split {
            let rule = gtk4::Separator::new(gtk4::Orientation::Horizontal);
            rule.add_css_class("hairline");
            box_.append(&rule);
        }
        if !item.enabled {
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
        box_.append(&button);
    }
    box_
}

pub fn bind_right_click(widget: &gtk4::Widget, rows: Vec<Item>, act: Rc<dyn Fn(Act)>) {
    let gesture = gtk4::GestureClick::new();
    gesture.set_button(gdk::BUTTON_SECONDARY);
    gesture.connect_pressed(move |gesture, _, x, y| {
        let Some(widget) = gesture.widget() else {
            return;
        };
        popup(&widget, x, y, rows.clone(), act.clone());
    });
    widget.add_controller(gesture);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_provider_card_menu_is_refresh_accounts_share_then_links() {
        let links = vec![("Status Page".into(), "https://example.test".into())];
        let rows = provider_menu(&["claude@abcd1234".into()], &links);
        assert_eq!(
            rows.iter()
                .map(|row| row.title.as_str())
                .collect::<Vec<_>>(),
            [
                "Refresh",
                "Manage Accounts…",
                "Share Screenshot",
                "Status Page"
            ]
        );
        assert!(rows[2].split && rows[3].split);
        assert!(matches!(&rows[2].act, Act::Share(ids) if ids == &["claude@abcd1234".to_string()]));
    }

    #[test]
    fn an_account_menu_leads_with_the_reason_and_adds_organization_billing() {
        let rows = account_menu(
            "copilot@abcd1234",
            Some("Copilot sign-in expired."),
            Some("https://github.com/organizations/acme/settings/billing"),
        );
        assert_eq!(rows[0].title, "Copilot sign-in expired.");
        assert!(!rows[0].enabled);
        assert_eq!(rows[1].title, "Refresh");
        assert!(rows[1].split);
        assert_eq!(rows[2].title, "Rename");
        assert_eq!(rows[3].title, "Edit Account…");
        assert_eq!(rows[4].title, "Share Screenshot");
        assert_eq!(rows[5].title, "Organization Billing");
        let plain = account_menu("claude@abcd1234", None, None);
        assert_eq!(
            plain
                .iter()
                .map(|row| row.title.as_str())
                .collect::<Vec<_>>(),
            ["Refresh", "Rename", "Edit Account…", "Share Screenshot"]
        );
    }

    #[test]
    fn only_a_copilot_organization_account_has_a_billing_page() {
        let mut account = Account::new(ProviderKind::Copilot, "Acme", Some("acme".into()));
        assert_eq!(
            organization_billing(&account).as_deref(),
            Some("https://github.com/organizations/acme/settings/billing")
        );
        account.organization = None;
        assert_eq!(organization_billing(&account), None);
        let claude = Account::new(ProviderKind::Claude, "Ada", Some("acme".into()));
        assert_eq!(organization_billing(&claude), None);
    }
}
