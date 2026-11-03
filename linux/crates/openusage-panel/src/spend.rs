//! The Total Spend card at the top of All Usage, a port of 0.7.x's `TotalSpendCard`: a Today /
//! Yesterday / 30 Days switch over a donut of each provider's share of the spend, the total in its
//! hole, and a legend beside it. The figures come from `openusage_core::spend`.

use std::f64::consts::{FRAC_PI_2, TAU};
use std::rc::Rc;

use gtk4::prelude::*;

use openusage_core::account::Account;
use openusage_core::kind::ProviderKind;
use openusage_core::spend::{self, Projection, SpendMetric, SpendPeriod};

use crate::model::Model;
use crate::prefers_dark;
use crate::ui;

const RING: i32 = 112;
/// The hole's share of the diameter, the golden-ratio donut of the Mac card.
const HOLE: f64 = 0.618;
/// Arc length, at the outer rim, between neighboring slices.
const GAP: f64 = 1.6;
/// A tiny provider next to a large one still shows a sliver. The legend keeps the true amounts.
const MIN_SHARE: f64 = 0.025;

pub struct Hooks {
    pub on_period: Rc<dyn Fn(SpendPeriod)>,
    pub on_share: Rc<dyn Fn()>,
}

/// The card, when Settings shows it and an account on the dashboard tracks spend.
pub fn card(model: &Model, hooks: &Hooks) -> Option<gtk4::Widget> {
    if !model.spend.shown {
        return None;
    }
    let accounts = spend_accounts(model);
    if accounts.is_empty() {
        return None;
    }
    let section = gtk4::Box::new(gtk4::Orientation::Vertical, 4);
    section.add_css_class("provider-section");
    section.append(&header(&accounts, Some(&hooks.on_share)));
    section.append(&body(model, &accounts, Some(&hooks.on_period)));
    Some(section.upcast())
}

/// The card as the share button copies it: no switch handlers, no share button.
pub fn sheet(model: &Model) -> Option<gtk4::Box> {
    let accounts = spend_accounts(model);
    if accounts.is_empty() {
        return None;
    }
    let section = gtk4::Box::new(gtk4::Orientation::Vertical, 4);
    section.append(&header(&accounts, None));
    section.append(&body(model, &accounts, None));
    Some(section)
}

/// Visible accounts whose provider reports spend, in dashboard order.
fn spend_accounts(model: &Model) -> Vec<Account> {
    model
        .accounts
        .iter()
        .map(|account| account.account.clone())
        .filter(|account| !crate::model::fully_hidden(account, &model.layout))
        .filter(spend::tracks_spend)
        .collect()
}

/// "Total Spend", the note on what it counts, and the share button where a provider shows its mark.
fn header(accounts: &[Account], on_share: Option<&Rc<dyn Fn()>>) -> gtk4::Box {
    let row = gtk4::Box::new(gtk4::Orientation::Horizontal, 5);
    row.add_css_class("section-header");
    row.set_margin_start(12);
    row.set_margin_end(12);
    row.append(&ui::text("Total Spend", &["provider"]));
    let info = info_mark();
    info.set_tooltip_text(Some(&included(accounts)));
    row.append(&info);
    let spring = gtk4::Label::new(None);
    spring.set_hexpand(true);
    row.append(&spring);
    if let Some(on_share) = on_share {
        let button = gtk4::Button::new();
        button.add_css_class("plain");
        button.add_css_class("logo-share");
        button.set_child(Some(&share_mark()));
        let on_share = on_share.clone();
        button.connect_clicked(move |_| on_share());
        row.append(&button);
    }
    row
}

/// "Only includes Claude and Cursor.", naming the providers that feed the ring.
fn included(accounts: &[Account]) -> String {
    let mut names: Vec<&str> = Vec::new();
    for account in accounts {
        let name = account.kind.display_name();
        if !names.contains(&name) {
            names.push(name);
        }
    }
    let list = match names.as_slice() {
        [] => String::new(),
        [one] => (*one).to_string(),
        [first, second] => format!("{first} and {second}"),
        [rest @ .., last] => format!("{}, and {last}", rest.join(", ")),
    };
    format!("Only includes {list}.")
}

fn body(
    model: &Model,
    accounts: &[Account],
    on_period: Option<&Rc<dyn Fn(SpendPeriod)>>,
) -> gtk4::Box {
    let card = ui::card();
    let inner = gtk4::Box::new(gtk4::Orientation::Vertical, 12);
    inner.set_margin_start(14);
    inner.set_margin_end(14);
    inner.set_margin_top(12);
    inner.set_margin_bottom(14);
    let period = model.spend.period;
    let selected = SpendPeriod::ALL
        .iter()
        .position(|candidate| *candidate == period)
        .unwrap_or(0);
    let labels = SpendPeriod::ALL.map(SpendPeriod::short_label);
    let on_period = on_period.cloned();
    inner.append(&ui::segments(
        &labels,
        selected,
        Rc::new(move |index| {
            if let (Some(on_period), Some(period)) = (&on_period, SpendPeriod::ALL.get(index)) {
                on_period(*period);
            }
        }),
    ));
    let slices = spend::total(period, accounts, &model.snapshots, &model.layout);
    let projection = spend::project(&slices, SpendMetric::Cost);
    if projection.slices.is_empty() {
        // As on the Mac: a quiet line, never a made-up zero ring.
        let empty = ui::text(SpendMetric::Cost.empty_message(), &["secondary", "figure"]);
        empty.set_margin_top(24);
        empty.set_margin_bottom(24);
        inner.append(&empty);
    } else {
        let content = gtk4::Box::new(gtk4::Orientation::Horizontal, 18);
        content.append(&ring(&projection));
        content.append(&legend(&projection));
        inner.append(&content);
    }
    card.append(&inner);
    card
}

/// The ⓘ beside the title.
fn info_mark() -> gtk4::DrawingArea {
    let area = gtk4::DrawingArea::new();
    area.set_content_width(13);
    area.set_content_height(13);
    area.set_valign(gtk4::Align::Center);
    area.set_margin_top(2);
    area.set_draw_func(|_area, context, width, height| {
        let (gray, alpha) = mark_ink();
        context.set_source_rgba(gray, gray, gray, alpha);
        let (cx, cy) = (f64::from(width) / 2.0, f64::from(height) / 2.0);
        let radius = f64::from(width.min(height)) / 2.0 - 0.75;
        context.set_line_width(1.2);
        context.arc(cx, cy, radius, 0.0, TAU);
        let _ = context.stroke();
        context.arc(cx, cy - radius * 0.45, 0.9, 0.0, TAU);
        let _ = context.fill();
        context.set_line_width(1.3);
        context.move_to(cx, cy - radius * 0.12);
        context.line_to(cx, cy + radius * 0.55);
        let _ = context.stroke();
    });
    area
}

/// The share glyph: a tray with an arrow out of it.
fn share_mark() -> gtk4::DrawingArea {
    let area = gtk4::DrawingArea::new();
    area.set_content_width(16);
    area.set_content_height(16);
    area.set_draw_func(|_area, context, width, height| {
        let ink = if prefers_dark() { 0.96 } else { 0.11 };
        context.set_source_rgb(ink, ink, ink);
        let (w, h) = (f64::from(width), f64::from(height));
        context.set_line_width(1.3);
        context.set_line_cap(cairo::LineCap::Round);
        context.set_line_join(cairo::LineJoin::Round);
        // Tray.
        context.move_to(w * 0.34, h * 0.42);
        context.line_to(w * 0.2, h * 0.42);
        context.line_to(w * 0.2, h * 0.9);
        context.line_to(w * 0.8, h * 0.9);
        context.line_to(w * 0.8, h * 0.42);
        context.line_to(w * 0.66, h * 0.42);
        let _ = context.stroke();
        // Arrow.
        context.move_to(w * 0.5, h * 0.62);
        context.line_to(w * 0.5, h * 0.08);
        let _ = context.stroke();
        context.move_to(w * 0.34, h * 0.24);
        context.line_to(w * 0.5, h * 0.08);
        context.line_to(w * 0.66, h * 0.24);
        let _ = context.stroke();
    });
    area
}

fn mark_ink() -> (f64, f64) {
    if prefers_dark() {
        (1.0, 0.55)
    } else {
        (0.0, 0.5)
    }
}

fn ring(projection: &Projection) -> gtk4::Overlay {
    let arcs = arcs(projection);
    let area = gtk4::DrawingArea::new();
    area.set_content_width(RING);
    area.set_content_height(RING);
    area.set_valign(gtk4::Align::Center);
    area.set_draw_func(move |_area, context, width, height| {
        paint_ring(context, width, height, &arcs);
    });
    let overlay = gtk4::Overlay::new();
    overlay.set_child(Some(&area));
    // One short figure in the hole, as in the 0.7.x card ("$13.4K").
    let (figure, _) = spend::ring_center(projection.center, projection.metric);
    let center = ui::text(&figure, &["spend-center", "nums"]);
    center.set_halign(gtk4::Align::Center);
    center.set_valign(gtk4::Align::Center);
    overlay.add_overlay(&center);
    overlay
}

/// Each slice's start and end as ring fractions, clockwise from noon, floored at `MIN_SHARE` and
/// renormalized so the ring closes.
fn arcs(projection: &Projection) -> Vec<(ProviderKind, f64, f64)> {
    let total: f64 = projection.slices.iter().map(|(_, amount)| amount).sum();
    if total <= 0.0 {
        return Vec::new();
    }
    let shares: Vec<f64> = projection
        .slices
        .iter()
        .map(|(_, amount)| (amount / total).max(MIN_SHARE))
        .collect();
    let sum: f64 = shares.iter().sum();
    let mut cursor = 0.0;
    projection
        .slices
        .iter()
        .zip(shares)
        .map(|((kind, _), share)| {
            let start = cursor;
            cursor += share / sum;
            (*kind, start, cursor)
        })
        .collect()
}

fn paint_ring(
    context: &cairo::Context,
    width: i32,
    height: i32,
    arcs: &[(ProviderKind, f64, f64)],
) {
    let dark = prefers_dark();
    let outer = f64::from(width.min(height)) / 2.0;
    let inner = outer * HOLE;
    let (cx, cy) = (f64::from(width) / 2.0, f64::from(height) / 2.0);
    if arcs.is_empty() {
        let shade = if dark { 1.0 } else { 0.0 };
        context.set_source_rgba(shade, shade, shade, if dark { 0.12 } else { 0.08 });
        context.arc(cx, cy, outer, 0.0, TAU);
        context.arc_negative(cx, cy, inner, TAU, 0.0);
        let _ = context.fill();
        return;
    }
    let half_gap = if arcs.len() > 1 { GAP / outer / 2.0 } else { 0.0 };
    for (kind, start, end) in arcs {
        let a0 = -FRAC_PI_2 + start * TAU + half_gap;
        let a1 = -FRAC_PI_2 + end * TAU - half_gap;
        if a1 - a0 <= 0.001 {
            continue;
        }
        let (red, green, blue) = color(*kind, dark);
        context.set_source_rgb(red, green, blue);
        context.new_sub_path();
        context.arc(cx, cy, outer, a0, a1);
        context.arc_negative(cx, cy, inner, a1, a0);
        context.close_path();
        let _ = context.fill();
    }
}

fn legend(projection: &Projection) -> gtk4::Box {
    let column = gtk4::Box::new(gtk4::Orientation::Vertical, 10);
    column.set_hexpand(true);
    column.set_valign(gtk4::Align::Center);
    for (kind, amount) in &projection.slices {
        let row = gtk4::Box::new(gtk4::Orientation::Horizontal, 7);
        row.append(&dot(*kind));
        let name = ui::text(kind.display_name(), &["spend-name"]);
        name.set_hexpand(true);
        name.set_halign(gtk4::Align::Start);
        name.set_xalign(0.0);
        name.set_ellipsize(pango::EllipsizeMode::End);
        row.append(&name);
        row.append(&ui::text(
            &spend::legend_text(*amount, projection.metric),
            &["spend-amount", "secondary", "nums"],
        ));
        column.append(&row);
    }
    column
}

pub(crate) fn dot(kind: ProviderKind) -> gtk4::DrawingArea {
    let area = gtk4::DrawingArea::new();
    area.set_content_width(8);
    area.set_content_height(8);
    area.set_valign(gtk4::Align::Center);
    area.set_draw_func(move |_area, context, width, height| {
        let (red, green, blue) = color(kind, prefers_dark());
        context.set_source_rgb(red, green, blue);
        let radius = f64::from(width.min(height)) / 2.0;
        context.arc(f64::from(width) / 2.0, f64::from(height) / 2.0, radius, 0.0, TAU);
        let _ = context.fill();
    });
    area
}

/// One color per provider, the same in the ring and the legend. Brands that are plain black flip
/// to near-white in dark mode. From the 0.7.x palette.
pub(crate) fn color(kind: ProviderKind, dark: bool) -> (f64, f64, f64) {
    let hex = match kind {
        ProviderKind::Claude => 0xDE7356,
        ProviderKind::Codex => 0x10A37F,
        ProviderKind::Cursor if dark => 0xF5F5F7,
        ProviderKind::Cursor => 0x13120A,
        ProviderKind::Grok if dark => 0x98989D,
        ProviderKind::Grok => 0x8E8E93,
        ProviderKind::OpenCode if dark => 0xAEAEB2,
        ProviderKind::OpenCode => 0x6E6E73,
        ProviderKind::OpenRouter => 0x6467F2,
        ProviderKind::Antigravity => 0x4285F4,
        ProviderKind::Copilot => 0xA855F7,
        ProviderKind::Zai if dark => 0xD1D1D6,
        ProviderKind::Zai => 0x2D2D2D,
        ProviderKind::Ollama if dark => 0xC7C7CC,
        ProviderKind::Ollama => 0x48484A,
        ProviderKind::Devin => 0x34C759,
    };
    let channel = |shift: u32| f64::from((hex >> shift) & 0xFF) / 255.0;
    (channel(16), channel(8), channel(0))
}
