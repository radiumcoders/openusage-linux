//! The card's Usage view: one concentric ring per account, filled to its tightest limit the way its
//! bar is (used or left, per Settings), with the tightest of them in the middle and a legend beside.
//! It reads only what the dashboard already shows.

use std::f64::consts::{FRAC_PI_2, TAU};

use gtk4::prelude::*;

use openusage_core::kind::ProviderKind;
use openusage_core::widget::{DisplayMode, Severity};

use crate::format;
use crate::model::Model;
use crate::prefers_dark;
use crate::rows;
use crate::spend::{color, dot, RING};
use crate::ui;

/// More accounts than this still get a legend row; the rings would get too thin.
const MAX_RINGS: usize = 5;
const RING_GAP: f64 = 3.0;
/// The center keeps at least this radius for its figure.
const MIN_HOLE: f64 = 24.0;

pub struct UsageRing {
    pub kind: ProviderKind,
    pub name: String,
    pub caption: String,
    /// How full the ring is drawn: used, or left, as the bars are.
    pub fraction: f64,
    /// What is left, which decides the tightest account.
    pub remaining: f64,
    pub amount: String,
    pub severity: Severity,
    pub mode: DisplayMode,
}

/// Each visible account's tightest limit with a reading, in dashboard order.
pub fn rings(model: &Model) -> Vec<UsageRing> {
    let mut out = Vec::new();
    for family in &model.families {
        let visible: Vec<usize> = family
            .indexes
            .iter()
            .copied()
            .filter(|index| {
                !crate::model::fully_hidden(&model.accounts[*index].account, &model.layout)
            })
            .collect();
        for index in &visible {
            let account = &model.accounts[*index];
            let Some(line) = account
                .lines
                .iter()
                .filter(|line| line.data.has_data && line.data.is_bounded())
                .min_by(|a, b| {
                    a.data
                        .remaining_fraction()
                        .total_cmp(&b.data.remaining_fraction())
                })
            else {
                continue;
            };
            let caption = if visible.len() > 1 {
                format!(
                    "{} · {}",
                    format::account_name(&account.account),
                    line.data.title
                )
            } else {
                line.data.title.clone()
            };
            out.push(UsageRing {
                kind: account.account.kind,
                name: account.account.kind.display_name().to_string(),
                caption,
                fraction: line.data.fraction(),
                remaining: line.data.remaining_fraction(),
                amount: line.data.amount_text(),
                severity: line.data.severity(),
                mode: line.data.display_mode,
            });
        }
    }
    out
}

pub fn body(rings: &[UsageRing]) -> gtk4::Box {
    let body = gtk4::Box::new(gtk4::Orientation::Horizontal, 18);
    body.append(&ring(rings));
    body.append(&legend(rings));
    body
}

fn ring(rings: &[UsageRing]) -> gtk4::Overlay {
    let dark = prefers_dark();
    let arcs: Vec<((f64, f64, f64), f64)> = rings
        .iter()
        .take(MAX_RINGS)
        .map(|ring| (color(ring.kind, dark), ring.fraction))
        .collect();
    let area = gtk4::DrawingArea::new();
    area.set_content_width(RING);
    area.set_content_height(RING);
    area.set_valign(gtk4::Align::Center);
    area.set_draw_func(move |_area, context, width, height| {
        paint(context, width, height, &arcs);
    });
    let overlay = gtk4::Overlay::new();
    overlay.set_child(Some(&area));
    if let Some(tightest) = rings
        .iter()
        .min_by(|a, b| a.remaining.total_cmp(&b.remaining))
    {
        let center = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
        center.set_halign(gtk4::Align::Center);
        center.set_valign(gtk4::Align::Center);
        let figure = format!("{}%", (tightest.fraction * 100.0).round() as i64);
        center.append(&ui::text(
            &figure,
            &["spend-center", "nums", rows::severity_class(tightest.severity, false)],
        ));
        center.append(&ui::text(tightest.mode.word(), &["spend-unit", "tertiary"]));
        overlay.add_overlay(&center);
    }
    overlay
}

fn paint(context: &cairo::Context, width: i32, height: i32, arcs: &[((f64, f64, f64), f64)]) {
    let count = arcs.len().max(1) as f64;
    let outer = f64::from(width.min(height)) / 2.0;
    let thickness = ((outer - MIN_HOLE - RING_GAP * (count - 1.0)) / count).clamp(5.0, 13.0);
    let (cx, cy) = (f64::from(width) / 2.0, f64::from(height) / 2.0);
    context.set_line_width(thickness);
    context.set_line_cap(cairo::LineCap::Round);
    for (index, ((red, green, blue), fraction)) in arcs.iter().enumerate() {
        let radius = outer - thickness / 2.0 - index as f64 * (thickness + RING_GAP);
        context.set_source_rgba(*red, *green, *blue, 0.16);
        context.new_sub_path();
        context.arc(cx, cy, radius, 0.0, TAU);
        let _ = context.stroke();
        let fraction = fraction.clamp(0.0, 1.0);
        if fraction <= 0.0 {
            continue;
        }
        context.set_source_rgb(*red, *green, *blue);
        context.new_sub_path();
        if fraction >= 1.0 {
            context.arc(cx, cy, radius, 0.0, TAU);
        } else {
            context.arc(cx, cy, radius, -FRAC_PI_2, -FRAC_PI_2 + TAU * fraction);
        }
        let _ = context.stroke();
    }
}

fn legend(rings: &[UsageRing]) -> gtk4::Box {
    let column = gtk4::Box::new(gtk4::Orientation::Vertical, 8);
    column.set_hexpand(true);
    column.set_valign(gtk4::Align::Center);
    for ring in rings {
        let row = gtk4::Box::new(gtk4::Orientation::Horizontal, 7);
        let mark = dot(ring.kind);
        mark.set_valign(gtk4::Align::Start);
        mark.set_margin_top(5);
        row.append(&mark);
        let names = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
        names.set_hexpand(true);
        let name = ui::text(&ring.name, &["spend-name"]);
        name.set_halign(gtk4::Align::Start);
        name.set_xalign(0.0);
        name.set_ellipsize(pango::EllipsizeMode::End);
        names.append(&name);
        let caption = ui::text(&ring.caption, &["caption", "secondary"]);
        caption.set_halign(gtk4::Align::Start);
        caption.set_xalign(0.0);
        caption.set_ellipsize(pango::EllipsizeMode::End);
        names.append(&caption);
        row.append(&names);
        let amount = ui::text(
            &ring.amount,
            &["spend-amount", "nums", rows::severity_class(ring.severity, true)],
        );
        amount.set_valign(gtk4::Align::Start);
        row.append(&amount);
        column.append(&row);
    }
    column
}
