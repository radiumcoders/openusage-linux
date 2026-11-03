//! The Total Spend card at the top of All Usage: a period switch over a donut of each provider's
//! share, the total in its hole, and a legend beside it. The title's menu picks Cost, Cost/MTok, or
//! Tokens. Port of 0.7.x's `TotalSpendCard`; the figures come from `openusage_core::spend`.

use std::f64::consts::{FRAC_PI_2, TAU};
use std::rc::Rc;

use gtk4::prelude::*;

use openusage_core::kind::ProviderKind;
use openusage_core::spend::{self, Projection, SpendMetric, SpendPeriod};

use crate::model::Model;
use crate::prefers_dark;
use crate::ui;

const RING: i32 = 104;
/// The hole's share of the diameter, the golden-ratio donut of the Mac card.
const HOLE: f64 = 0.618;
/// Arc length, at the outer rim, between neighboring slices.
const GAP: f64 = 1.6;
/// A tiny provider next to a large one still shows a sliver. The legend keeps the true amounts.
const MIN_SHARE: f64 = 0.025;

pub struct Hooks {
    pub on_period: Rc<dyn Fn(SpendPeriod)>,
    pub on_metric: Rc<dyn Fn(SpendMetric)>,
}

/// The card, when Settings shows it and an account on the dashboard reports history rows.
pub fn card(model: &Model, hooks: &Hooks) -> Option<gtk4::Widget> {
    if !model.spend.shown {
        return None;
    }
    let accounts: Vec<_> = model
        .accounts
        .iter()
        .map(|account| account.account.clone())
        .filter(|account| !crate::model::fully_hidden(account, &model.layout))
        .collect();
    if !accounts.iter().any(spend::tracks_spend) {
        return None;
    }
    let choice = model.spend;
    let slices = spend::total(choice.period, &accounts, &model.snapshots, &model.layout);
    let projection = spend::project(&slices, choice.metric);

    let section = gtk4::Box::new(gtk4::Orientation::Vertical, 4);
    section.add_css_class("provider-section");
    section.append(&header(choice.metric, &hooks.on_metric));

    let card = ui::card();
    let inner = gtk4::Box::new(gtk4::Orientation::Vertical, 12);
    inner.set_margin_start(14);
    inner.set_margin_end(14);
    inner.set_margin_top(12);
    inner.set_margin_bottom(12);
    let selected = SpendPeriod::ALL
        .iter()
        .position(|period| *period == choice.period)
        .unwrap_or(0);
    let labels = SpendPeriod::ALL.map(SpendPeriod::short_label);
    let on_period = hooks.on_period.clone();
    inner.append(&ui::segments(
        &labels,
        selected,
        Rc::new(move |index| {
            if let Some(period) = SpendPeriod::ALL.get(index) {
                on_period(*period);
            }
        }),
    ));
    if projection.slices.is_empty() {
        let empty = ui::text(choice.metric.empty_message(), &["secondary", "caption"]);
        empty.set_margin_top(18);
        empty.set_margin_bottom(18);
        inner.append(&empty);
    } else {
        let body = gtk4::Box::new(gtk4::Orientation::Horizontal, 18);
        body.append(&ring(&projection));
        body.append(&legend(&projection));
        inner.append(&body);
    }
    card.append(&inner);
    section.append(&card);
    Some(section.upcast())
}

/// "Total Spend" with the metric menu where a provider header shows its mark.
fn header(metric: SpendMetric, on_metric: &Rc<dyn Fn(SpendMetric)>) -> gtk4::Box {
    let row = gtk4::Box::new(gtk4::Orientation::Horizontal, 8);
    row.add_css_class("section-header");
    row.set_margin_start(12);
    row.set_margin_end(12);
    let title = ui::text("Total Spend", &["provider"]);
    title.set_hexpand(true);
    title.set_halign(gtk4::Align::Start);
    title.set_xalign(0.0);
    row.append(&title);

    let label = gtk4::Box::new(gtk4::Orientation::Horizontal, 4);
    label.append(&ui::text(metric.title(), &["caption", "secondary"]));
    label.append(&ui::text("▾", &["caption", "secondary"]));
    let button = gtk4::Button::new();
    button.add_css_class("plain");
    button.add_css_class("spend-metric");
    button.set_child(Some(&label));
    let on_metric = on_metric.clone();
    button.connect_clicked(move |button| metric_menu(button, metric, &on_metric));
    row.append(&button);
    row
}

fn metric_menu(anchor: &gtk4::Button, current: SpendMetric, on_metric: &Rc<dyn Fn(SpendMetric)>) {
    let popover = gtk4::Popover::new();
    popover.add_css_class("menu-pop");
    popover.set_has_arrow(false);
    let column = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
    column.add_css_class("menu-list");
    for metric in SpendMetric::ALL {
        let title = if metric == current {
            format!("✓ {}", metric.title())
        } else {
            metric.title().to_string()
        };
        let item = gtk4::Button::new();
        item.add_css_class("menu-item");
        let text = ui::text(&title, &[]);
        text.set_xalign(0.0);
        text.set_hexpand(true);
        item.set_child(Some(&text));
        let on_metric = on_metric.clone();
        item.connect_clicked(move |_| {
            crate::menu::close_open();
            let on_metric = on_metric.clone();
            gtk4::glib::idle_add_local_once(move || on_metric(metric));
        });
        column.append(&item);
    }
    popover.set_child(Some(&column));
    popover.set_parent(anchor);
    popover.set_autohide(true);
    popover.connect_closed(|popover| popover.unparent());
    popover.popup();
    crate::menu::track(&popover);
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
    let (figure, unit) = spend::ring_center(projection.center, projection.metric);
    let center = gtk4::Box::new(gtk4::Orientation::Vertical, 1);
    center.set_halign(gtk4::Align::Center);
    center.set_valign(gtk4::Align::Center);
    center.append(&ui::text(&figure, &["spend-center", "nums"]));
    center.append(&ui::text(unit, &["spend-unit", "tertiary"]));
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
    let column = gtk4::Box::new(gtk4::Orientation::Vertical, 7);
    column.set_hexpand(true);
    column.set_valign(gtk4::Align::Center);
    for (kind, amount) in &projection.slices {
        let row = gtk4::Box::new(gtk4::Orientation::Horizontal, 7);
        row.append(&dot(*kind));
        let name = ui::text(kind.display_name(), &["figure"]);
        name.set_hexpand(true);
        name.set_halign(gtk4::Align::Start);
        name.set_xalign(0.0);
        name.set_ellipsize(pango::EllipsizeMode::End);
        row.append(&name);
        row.append(&ui::text(
            &spend::legend_text(*amount, projection.metric),
            &["figure", "secondary", "nums"],
        ));
        column.append(&row);
    }
    column
}

fn dot(kind: ProviderKind) -> gtk4::DrawingArea {
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
fn color(kind: ProviderKind, dark: bool) -> (f64, f64, f64) {
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
