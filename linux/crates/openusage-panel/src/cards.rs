//! Provider cards on All Usage: menus, drag handles, and metric rows.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gtk4::prelude::*;

use openusage_core::claims;
use openusage_core::dashboard::DashboardAccount;
use openusage_core::metric::UsageBreakdown;

use crate::breakdown::Pop;
use crate::drag::{self, Kind, Target};
use crate::icons;
use crate::menu;
use crate::model::{self, Model};
use crate::resets::ClaimChrome;
use crate::rows::{self, Extra, Toggles};
use crate::state::ClaimPhase;
use crate::view::{Actions, Chrome};

pub fn all_usage(model: &Model, actions: &Actions, chrome: &Chrome) -> gtk4::Widget {
    let list = gtk4::Box::new(gtk4::Orientation::Vertical, 14);
    let menu_shown = Cell::new(false);
    let breakdown_shown = Cell::new(false);
    let mut targets = Vec::new();
    for family in &model.families {
        let indexes: Vec<usize> = family
            .indexes
            .iter()
            .copied()
            .filter(|index| !model::fully_hidden(&model.accounts[*index].account, &model.layout))
            .collect();
        if indexes.is_empty() {
            continue;
        }
        targets.extend(provider_card(
            model,
            &indexes,
            actions,
            chrome,
            &menu_shown,
            &breakdown_shown,
            &list,
        ));
    }
    let on_reorder = actions.on_reorder.clone();
    drag::bind_rows(
        &list,
        targets,
        Rc::new(move |kind, scope, dragged, target| on_reorder(kind, scope, dragged, target)),
    );
    list.upcast()
}

fn provider_card(
    model: &Model,
    indexes: &[usize],
    actions: &Actions,
    chrome: &Chrome,
    menu_shown: &Cell<bool>,
    breakdown_shown: &Cell<bool>,
    list: &gtk4::Box,
) -> Vec<Target> {
    let card = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
    card.add_css_class("card");
    let inner = gtk4::Box::new(gtk4::Orientation::Vertical, 8);
    inner.set_margin_start(12);
    inner.set_margin_end(12);
    inner.set_margin_top(10);
    inner.set_margin_bottom(12);

    let first = &model.accounts[indexes[0]];
    let siblings = indexes.len();
    let card_ids: Vec<String> = indexes
        .iter()
        .map(|index| model.accounts[*index].account.card_id())
        .collect();
    let provider_id = card_ids[0].clone();
    let plan_on_header = !model::shows_account_label(first, siblings);
    let header = header(first, plan_on_header, &card_ids, &actions.on_share);
    bind_provider_menu(&header, indexes[0], first, &card_ids, actions);
    inner.append(&header);

    let mut targets = vec![Target {
        kind: Kind::Provider,
        scope: String::new(),
        id: provider_id,
        widget: card.clone().upcast(),
    }];
    let scope = first.account.kind.raw().to_string();
    for (offset, index) in indexes.iter().enumerate() {
        if offset > 0 {
            let rule = gtk4::Separator::new(gtk4::Orientation::Horizontal);
            rule.add_css_class("hairline");
            rule.set_margin_top(2);
            inner.append(&rule);
        }
        let account = &model.accounts[*index];
        let (block, metric_targets) = account_block(
            model,
            *index,
            account,
            siblings,
            actions,
            chrome,
            menu_shown,
            breakdown_shown,
        );
        if siblings > 1 {
            targets.push(Target {
                kind: Kind::Account,
                scope: scope.clone(),
                id: account.account.card_id(),
                widget: block.clone().upcast(),
            });
        }
        targets.extend(metric_targets);
        inner.append(&block);
    }
    card.append(&inner);
    // The card is appended by the caller via the returned widget... we need to append here.
    // Returning targets whose widgets are inside `card`. The caller must append `card`.
    // Stash the card by appending to a passed list.
    list.append(&card);
    targets
}

fn bind_provider_menu(
    widget: &impl IsA<gtk4::Widget>,
    index: usize,
    account: &DashboardAccount,
    card_ids: &[String],
    actions: &Actions,
) {
    let links = links_of(account);
    let on_menu = actions.on_menu.clone();
    menu::bind_right_click(
        widget.upcast_ref(),
        menu::provider_menu(card_ids, &links),
        Rc::new(move |act| on_menu(index, act)),
    );
}

fn account_block(
    model: &Model,
    index: usize,
    account: &DashboardAccount,
    siblings: usize,
    actions: &Actions,
    chrome: &Chrome,
    menu_shown: &Cell<bool>,
    breakdown_shown: &Cell<bool>,
) -> (gtk4::Box, Vec<Target>) {
    let block = gtk4::Box::new(gtk4::Orientation::Vertical, 6);
    if model::shows_account_label(account, siblings) {
        let caption = gtk4::Box::new(gtk4::Orientation::Horizontal, 6);
        let name = gtk4::Label::new(Some(&account.account.label));
        name.add_css_class("account");
        name.add_css_class("secondary");
        name.add_css_class("drag-handle");
        name.set_hexpand(true);
        name.set_halign(gtk4::Align::Start);
        name.set_ellipsize(if account.account.label.contains('@') {
            pango::EllipsizeMode::Middle
        } else {
            pango::EllipsizeMode::End
        });
        if siblings > 1 {
            drag::bind_source(
                &name,
                Kind::Account,
                account.account.kind.raw(),
                &account.account.card_id(),
            );
        }
        caption.append(&name);
        if let Some(plan) = account.plan.as_deref() {
            caption.append(&plan_badge(plan));
        }
        if let Some(mark) = status_mark(account) {
            caption.append(&mark);
        }
        block.append(&caption);
    }
    bind_account_menu(&block, index, account, actions);
    if siblings > 1 && preview_menu(chrome, account, menu_shown) {
        block.append(&menu_inline(index, account, actions));
    } else if plan_on_single(account, siblings) && preview_menu(chrome, account, menu_shown) {
        block.append(&menu_inline(index, account, actions));
    }
    let card_id = account.account.card_id();
    let targets = RefCell::new(Vec::new());
    let snapshot = model.snapshots.get(&card_id);
    let offer = claims::offer(&account.account, snapshot)
        .or_else(|| preview_offer(account, chrome.preview.claim));
    let toggles = toggles_for(account, actions);
    let body = rows::account_body(
        account,
        model.now,
        model.absolute_resets,
        &toggles,
        &|line| {
            let breakdown =
                snapshot.and_then(|snapshot| snapshot.breakdowns.get(line.descriptor.key).cloned());
            let inline_breakdown =
                breakdown.is_some() && chrome.preview.breakdown && !breakdown_shown.replace(true);
            let claim = claim_chrome(account, line, offer.as_ref(), chrome, actions);
            Extra {
                breakdown,
                inline_breakdown,
                claim,
            }
        },
        &|line, widget| {
            drag::bind_source(widget, Kind::Metric, &card_id, &line.descriptor.id);
            targets.borrow_mut().push(Target {
                kind: Kind::Metric,
                scope: card_id.clone(),
                id: line.descriptor.id.clone(),
                widget: widget.clone(),
            });
        },
    );
    block.append(&body);
    (block, targets.into_inner())
}

fn plan_on_single(account: &DashboardAccount, siblings: usize) -> bool {
    !model::shows_account_label(account, siblings)
}

fn toggles_for(account: &DashboardAccount, actions: &Actions) -> Toggles {
    let on_copy = actions.on_copy_breakdown.clone();
    let provider = account.account.kind.display_name().to_string();
    let kind = account.account.kind;
    Toggles {
        meter: actions.on_toggle_meter.clone(),
        reset: actions.on_toggle_reset.clone(),
        copied: Rc::new(move |title, breakdown| on_copy(pop(title, breakdown, &provider, kind))),
    }
}

fn pop(
    title: String,
    breakdown: UsageBreakdown,
    provider: &str,
    kind: openusage_core::kind::ProviderKind,
) -> Pop {
    Pop {
        title,
        breakdown,
        provider: provider.to_string(),
        kind,
    }
}

fn bind_account_menu(
    widget: &impl IsA<gtk4::Widget>,
    index: usize,
    account: &DashboardAccount,
    actions: &Actions,
) {
    let on_menu = actions.on_menu.clone();
    let rows = menu::account_menu(
        &account.account.card_id(),
        account.problem.as_deref(),
        menu::organization_billing(&account.account).as_deref(),
    );
    menu::bind_right_click(
        widget.upcast_ref(),
        rows,
        Rc::new(move |act| on_menu(index, act)),
    );
}

fn links_of(account: &DashboardAccount) -> Vec<(String, String)> {
    account
        .account
        .kind
        .links()
        .into_iter()
        .map(|link| (link.label.to_string(), link.url.to_string()))
        .collect()
}

fn preview_menu(chrome: &Chrome, account: &DashboardAccount, shown: &Cell<bool>) -> bool {
    if !chrome.preview.menu || shown.get() {
        return false;
    }
    let problem = account
        .problem
        .as_deref()
        .is_some_and(|text| !text.starts_with("Last updated"));
    if !problem {
        return false;
    }
    shown.set(true);
    true
}

fn menu_inline(index: usize, account: &DashboardAccount, actions: &Actions) -> gtk4::Box {
    let on_menu = actions.on_menu.clone();
    let rows = menu::account_menu(
        &account.account.card_id(),
        account.problem.as_deref(),
        menu::organization_billing(&account.account).as_deref(),
    );
    menu::inline(&rows, Rc::new(move |act| on_menu(index, act)))
}

fn header(
    account: &DashboardAccount,
    plan_on_header: bool,
    card_ids: &[String],
    on_share: &Rc<dyn Fn(Vec<String>)>,
) -> gtk4::Box {
    let row = gtk4::Box::new(gtk4::Orientation::Horizontal, 8);
    let ids = card_ids.to_vec();
    let share = on_share.clone();
    let logo = gtk4::Button::new();
    logo.add_css_class("plain");
    logo.add_css_class("logo-share");
    logo.set_child(Some(&icons::image(account.account.kind, 16)));
    logo.connect_clicked(move |_| share(ids.clone()));
    row.append(&logo);
    let handle = gtk4::Box::new(gtk4::Orientation::Horizontal, 8);
    handle.set_hexpand(true);
    let name = gtk4::Label::new(Some(account.account.kind.display_name()));
    name.add_css_class("provider");
    name.set_hexpand(true);
    name.set_halign(gtk4::Align::Start);
    name.set_xalign(0.0);
    name.set_ellipsize(pango::EllipsizeMode::End);
    handle.append(&name);
    if plan_on_header {
        if let Some(plan) = account.plan.as_deref() {
            handle.append(&plan_badge(plan));
        }
        if let Some(mark) = status_mark(account) {
            handle.append(&mark);
        }
    }
    drag::bind_source(&handle, Kind::Provider, "", &card_ids[0]);
    row.append(&handle);
    row
}

fn preview_offer(account: &DashboardAccount, preview: bool) -> Option<claims::ClaimOffer> {
    if !preview {
        return None;
    }
    let has_resets = account
        .lines
        .iter()
        .any(|line| line.descriptor.shows_reset_expiries() && line.data.has_data);
    has_resets.then(|| claims::ClaimOffer {
        confirm_detail: Some("Resets your weekly limit".into()),
        expiries: vec![chrono::Utc::now() + chrono::Duration::days(3)],
    })
}

fn claim_chrome(
    account: &DashboardAccount,
    line: &openusage_core::dashboard::DashboardLine,
    offer: Option<&claims::ClaimOffer>,
    chrome: &Chrome,
    actions: &Actions,
) -> Option<ClaimChrome> {
    if !line.descriptor.shows_reset_expiries() || !line.data.has_data {
        return None;
    }
    let offer = offer?;
    let active = chrome.claim.account == Some(account.account.id);
    let phase = if active {
        chrome.claim.phase.clone()
    } else {
        ClaimPhase::Idle
    };
    let phase = match phase {
        ClaimPhase::Confirm { .. } => ClaimPhase::Confirm {
            detail: offer.confirm_detail.clone(),
            expiring: offer.expiries.first().copied(),
        },
        other => other,
    };
    let id = account.account.id;
    let on_claim = actions.on_claim.clone();
    Some(ClaimChrome {
        phase,
        on_use: Rc::new(move || on_claim(id)),
        on_reset: actions.on_claim_reset.clone(),
        on_cancel: actions.on_claim_cancel.clone(),
    })
}

fn plan_badge(plan: &str) -> gtk4::Box {
    let badge = gtk4::Box::new(gtk4::Orientation::Horizontal, 0);
    badge.add_css_class("plan-badge");
    let label = gtk4::Label::new(Some(plan));
    label.add_css_class("plan");
    label.add_css_class("secondary");
    badge.append(&label);
    badge
}

fn status_mark(account: &DashboardAccount) -> Option<gtk4::Label> {
    let label = if account
        .problem
        .as_deref()
        .is_some_and(|text| !text.starts_with("Last updated"))
    {
        let label = gtk4::Label::new(Some("!"));
        label.add_css_class("status-warn");
        label
    } else if account.stale {
        let label = gtk4::Label::new(Some("◷"));
        label.add_css_class("tertiary");
        label
    } else {
        return None;
    };
    label.add_css_class("status");
    Some(label)
}
