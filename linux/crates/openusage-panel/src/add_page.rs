//! Add Account: provider list, Copilot's choice, a hub, and the sign-in prompt.

use std::rc::Rc;

use gtk4::prelude::*;

use openusage_core::kind::ProviderKind;
use openusage_core::login::LoginPrompt;

use crate::icons;
use crate::state::{AddFlow, AddStep};
use crate::ui;

pub struct Hooks {
    pub on_provider: Rc<dyn Fn(ProviderKind)>,
    pub on_personal: Rc<dyn Fn()>,
    pub on_organization: Rc<dyn Fn()>,
    pub on_org_continue: Rc<dyn Fn()>,
    pub on_hub: Rc<dyn Fn()>,
    pub on_hub_connect: Rc<dyn Fn()>,
    pub on_cancel: Rc<dyn Fn()>,
    pub on_try: Rc<dyn Fn()>,
    pub on_submit: Rc<dyn Fn()>,
    pub on_open: Rc<dyn Fn()>,
    pub on_copy: Rc<dyn Fn()>,
    pub on_draft: Rc<dyn Fn(String)>,
    pub on_org: Rc<dyn Fn(String)>,
    pub on_hub_url: Rc<dyn Fn(String)>,
    pub on_hub_key: Rc<dyn Fn(String)>,
}

pub fn title(flow: &AddFlow) -> String {
    if flow.replace.is_some() {
        return "Sign In Again".into();
    }
    match flow.step {
        AddStep::Providers => "Add Account".into(),
        AddStep::Copilot => "Add Copilot".into(),
        AddStep::Organization => "Organization Billing".into(),
        AddStep::Hub => "Connect a CLIProxyAPI Hub".into(),
        AddStep::SignIn => match flow.kind {
            Some(kind) => format!("Sign In to {}", kind.display_name()),
            None => "Sign In".into(),
        },
    }
}

pub fn body(flow: &AddFlow, hooks: &Hooks) -> gtk4::Widget {
    let mut page = gtk4::Box::new(gtk4::Orientation::Vertical, 12);
    match flow.step {
        AddStep::Providers => providers(hooks, &mut page),
        AddStep::Copilot => copilot(hooks, &mut page),
        AddStep::Organization => organization(flow, hooks, &mut page),
        AddStep::Hub => hub(flow, hooks, &mut page),
        AddStep::SignIn => sign_in(flow, hooks, &mut page),
    }
    page.append(&ui::button("Cancel", "bordered", hooks.on_cancel.clone()));
    page.upcast()
}

fn providers(hooks: &Hooks, page: &mut gtk4::Box) {
    page.append(&note("Choose a provider to sign in to."));
    let card = ui::card();
    for kind in ProviderKind::ALL {
        let on_provider = hooks.on_provider.clone();
        let button = gtk4::Button::new();
        button.add_css_class("list-row");
        let row = gtk4::Box::new(gtk4::Orientation::Horizontal, 8);
        row.append(&icons::image(kind, 20));
        let name = ui::text(kind.display_name(), &["title-quiet"]);
        name.set_hexpand(true);
        row.append(&name);
        row.append(&ui::text("›", &["tertiary"]));
        button.set_child(Some(&row));
        button.connect_clicked(move |_| on_provider(kind));
        card.append(&button);
    }
    let on_hub = hooks.on_hub.clone();
    card.append(&ui::row_button("Connect a CLIProxyAPI Hub", "row-btn", Rc::new(move || on_hub())));
    page.append(&card);
}

fn copilot(hooks: &Hooks, page: &mut gtk4::Box) {
    page.append(&note("Sign in to your own seat, or add your organization's billing with a token."));
    let card = ui::card();
    card.append(&ui::row_button("Sign In with GitHub", "row-btn", hooks.on_personal.clone()));
    card.append(&ui::row_button("Organization Billing", "row-btn", hooks.on_organization.clone()));
    page.append(&card);
}

fn organization(flow: &AddFlow, hooks: &Hooks, page: &mut gtk4::Box) {
    page.append(&note("Enter your organization and a token that can read its billing."));
    let org = ui::entry("Organization", false, &flow.org);
    let on_org = hooks.on_org.clone();
    org.connect_changed(move |entry| on_org(entry.text().to_string()));
    let on_continue = hooks.on_org_continue.clone();
    org.connect_activate(move |_| on_continue());
    page.append(&org);
    page.append(&link_button("Create a Token", "https://github.com/settings/personal-access-tokens/new"));
    let copy = ui::text(
        "Shows your organization's Copilot credits and spend. Use a token from an organization owner or billing manager.",
        &["caption", "secondary"],
    );
    copy.set_wrap(true);
    page.append(&copy);
    if let Some(error) = &flow.error {
        page.append(&error_label(error));
    }
    let continue_ = ui::button("Continue", "primary", hooks.on_org_continue.clone());
    continue_.set_sensitive(!flow.org.trim().is_empty() && !flow.working);
    page.append(&continue_);
}

fn hub(flow: &AddFlow, hooks: &Hooks, page: &mut gtk4::Box) {
    page.append(&note(
        "Enter your CLIProxyAPI server's address and management key. Its logins appear as accounts.",
    ));
    let url = ui::entry("http://192.168.1.20:8317", false, &flow.hub_url);
    let on_url = hooks.on_hub_url.clone();
    url.connect_changed(move |entry| on_url(entry.text().to_string()));
    page.append(&url);
    let key = ui::entry("Management Key", true, &flow.hub_key);
    let on_key = hooks.on_hub_key.clone();
    key.connect_changed(move |entry| on_key(entry.text().to_string()));
    let on_connect = hooks.on_hub_connect.clone();
    key.connect_activate(move |_| on_connect());
    page.append(&key);
    let copy = ui::text(
        "The hub needs remote management turned on. The key is stored in your keyring.",
        &["caption", "secondary"],
    );
    copy.set_wrap(true);
    page.append(&copy);
    if let Some(error) = &flow.error {
        page.append(&error_label(error));
    }
    let label = if flow.working { "Connecting…" } else { "Connect" };
    let connect = ui::button(label, "primary", hooks.on_hub_connect.clone());
    connect.set_sensitive(!flow.hub_url.trim().is_empty() && !flow.hub_key.trim().is_empty() && !flow.working);
    page.append(&connect);
}

fn sign_in(flow: &AddFlow, hooks: &Hooks, page: &mut gtk4::Box) {
    if let Some(error) = &flow.error {
        page.append(&error_label(error));
        page.append(&ui::button("Try Again", "primary", hooks.on_try.clone()));
        return;
    }
    let Some(prompt) = &flow.prompt else {
        page.append(&note("Preparing sign-in…"));
        return;
    };
    page.append(&note(&prompt_message(prompt)));
    match prompt {
        LoginPrompt::DeviceCode { user_code, .. } => {
            let code = ui::text(user_code, &["code"]);
            code.set_halign(gtk4::Align::Center);
            code.set_selectable(true);
            let well = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
            well.add_css_class("code-well");
            well.append(&code);
            page.append(&well);
            let row = gtk4::Box::new(gtk4::Orientation::Horizontal, 8);
            row.set_homogeneous(true);
            row.append(&ui::button("Copy", "bordered", hooks.on_copy.clone()));
            row.append(&ui::button("Open", "primary", hooks.on_open.clone()));
            page.append(&row);
            page.append(&note("Waiting for the provider to confirm…"));
        }
        LoginPrompt::Browser { accepts_pasted_code, .. } => {
            if *accepts_pasted_code {
                page.append(&paste_entry("Paste code", false, flow, hooks));
                page.append(&note("Paste the code here if your browser shows one."));
            }
            browser_buttons(flow, hooks, page);
        }
        LoginPrompt::BrowserToken { .. } => {
            page.append(&paste_entry("Paste token", true, flow, hooks));
            browser_buttons(flow, hooks, page);
        }
        LoginPrompt::Secret { label, key_page } => {
            page.append(&paste_entry(label, true, flow, hooks));
            if let Some(page_url) = key_page {
                page.append(&link_button("Get Your API Key", page_url));
            } else if label.to_ascii_lowercase().contains("organization") {
                page.append(&link_button("Create a Token", "https://github.com/settings/personal-access-tokens/new"));
            }
            let caption = if flow.submitted { "Signing in…" } else { "Add Account" };
            let add = ui::button(caption, "primary", hooks.on_submit.clone());
            add.set_sensitive(!flow.draft.trim().is_empty() && !flow.submitted);
            page.append(&add);
        }
    }
}

fn browser_buttons(flow: &AddFlow, hooks: &Hooks, page: &mut gtk4::Box) {
    let row = gtk4::Box::new(gtk4::Orientation::Horizontal, 8);
    row.set_homogeneous(true);
    row.append(&ui::button("Copy Link", "bordered", hooks.on_copy.clone()));
    let primary = if flow.submitted {
        "Signing in…"
    } else if flow.draft.trim().is_empty() {
        "Open Browser"
    } else {
        "Continue"
    };
    let action = if flow.draft.trim().is_empty() { hooks.on_open.clone() } else { hooks.on_submit.clone() };
    let button = ui::button(primary, "primary", action);
    button.set_sensitive(!flow.submitted);
    row.append(&button);
    page.append(&row);
    if !flow.submitted {
        page.append(&note("Waiting for your browser…"));
    }
}

fn paste_entry(placeholder: &str, secret: bool, flow: &AddFlow, hooks: &Hooks) -> gtk4::Entry {
    let entry = ui::entry(placeholder, secret, &flow.draft);
    entry.set_sensitive(!flow.submitted);
    let on_draft = hooks.on_draft.clone();
    entry.connect_changed(move |entry| on_draft(entry.text().to_string()));
    let on_submit = hooks.on_submit.clone();
    entry.connect_activate(move |_| on_submit());
    entry
}

fn prompt_message(prompt: &LoginPrompt) -> String {
    match prompt {
        LoginPrompt::DeviceCode { .. } => "Open the verification page in your browser and enter this code.".into(),
        LoginPrompt::BrowserToken { .. } => "Continue in your browser, then paste the token it shows here.".into(),
        LoginPrompt::Browser { .. } => "Continue in your browser. This sheet closes when you're signed in.".into(),
        LoginPrompt::Secret { .. } => "Enter the key. It's stored in your keyring.".into(),
    }
}

fn note(text: &str) -> gtk4::Label {
    let label = ui::text(text, &["secondary"]);
    label.set_wrap(true);
    label.set_max_width_chars(36);
    label
}

fn error_label(text: &str) -> gtk4::Label {
    let label = ui::text(text, &["problem"]);
    label.set_wrap(true);
    label.set_max_width_chars(36);
    label
}

fn link_button(label: &str, url: &str) -> gtk4::Button {
    let url = url.to_string();
    ui::button(label, "link", Rc::new(move || ui::open_uri(&url)))
}
