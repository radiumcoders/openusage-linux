//! Metric rows. Bar rows follow `WidgetData::row`: title and pace, the bar, then the amount and the reset.

use std::rc::Rc;

use chrono::{DateTime, Utc};
use gtk4::prelude::*;

use openusage_core::dashboard::{DashboardAccount, DashboardLine};
use openusage_core::metric::UsageBreakdown;
use openusage_core::widget::{Mark, Severity};

use crate::format;
use crate::prefers_dark;
use crate::resets::ClaimChrome;
use crate::ui;

pub struct Extra {
    pub breakdown: Option<UsageBreakdown>,
    pub inline_breakdown: bool,
    pub claim: Option<ClaimChrome>,
}

#[derive(Clone)]
pub struct Toggles {
    pub meter: Rc<dyn Fn()>,
    pub reset: Rc<dyn Fn()>,
    /// Copies a PNG of this figure. The bool is whether the picture was copied or saved.
    pub copied: Rc<dyn Fn(String, UsageBreakdown) -> bool>,
}

pub fn account_body(
    account: &DashboardAccount,
    now: DateTime<Utc>,
    absolute: bool,
    toggles: &Toggles,
    extra: &dyn Fn(&DashboardLine) -> Extra,
    bind: &dyn Fn(&DashboardLine, &gtk4::Widget),
) -> gtk4::Box {
    let box_ = gtk4::Box::new(gtk4::Orientation::Vertical, 12);
    if account.waiting {
        box_.append(&ui::text("Waiting for usage", &["secondary"]));
        return box_;
    }
    if account.lines.is_empty() && account.problem.is_none() {
        box_.append(&ui::text("No usage reported.", &["secondary"]));
    }
    for line in &account.lines {
        let extra = extra(line);
        let widget = metric_row(line, now, absolute, toggles, &extra);
        bind(line, &widget);
        box_.append(&widget);
        if extra.inline_breakdown {
            if let Some(breakdown) = &extra.breakdown {
                let card = crate::breakdown::card(&line.data.title, breakdown);
                card.add_css_class("card");
                let copied = toggles.copied.clone();
                let title = line.data.title.clone();
                let shot = breakdown.clone();
                box_.append(&crate::breakdown::with_copy(
                    card,
                    breakdown,
                    Rc::new(move || copied(title.clone(), shot.clone())),
                ));
            }
        }
    }
    if let Some(problem) = &account.problem {
        let label = ui::text(problem, &["problem"]);
        label.set_wrap(true);
        label.set_xalign(0.0);
        label.set_max_width_chars(36);
        box_.append(&label);
    }
    box_
}

fn metric_row(
    line: &DashboardLine,
    now: DateTime<Utc>,
    absolute: bool,
    toggles: &Toggles,
    extra: &Extra,
) -> gtk4::Widget {
    let row = line.data.row(now);
    let widget = match &row.mark {
        Mark::Bar {
            fill,
            severity,
            value,
            used_up,
            marker,
            ..
        } => {
            let marker = row.pace.is_some().then_some(*marker).flatten();
            let reset = format::bar_reset(&line.data, now, absolute);
            bar_row(
                &row.title,
                row.pace.as_deref(),
                *fill,
                *severity,
                value,
                reset.as_deref(),
                *used_up,
                marker,
                line.data.has_meter_style_toggle(),
                toggles,
            )
        }
        Mark::Value {
            value,
            detail,
            severity,
        } => value_row(
            &row.title,
            value,
            detail.as_deref(),
            *severity,
            extra,
            toggles,
        )
        .upcast(),
    };
    widget
}

fn bar_row(
    title: &str,
    pace: Option<&str>,
    fill: f64,
    severity: Severity,
    value: &str,
    reset: Option<&str>,
    used_up: bool,
    marker: Option<f64>,
    toggle: bool,
    toggles: &Toggles,
) -> gtk4::Widget {
    let block = gtk4::Box::new(gtk4::Orientation::Vertical, 2);
    let top = gtk4::Box::new(gtk4::Orientation::Horizontal, 8);
    let title_label = ui::text(title, &["title"]);
    title_label.set_hexpand(true);
    title_label.set_halign(gtk4::Align::Start);
    title_label.set_ellipsize(pango::EllipsizeMode::End);
    top.append(&title_label);
    if let Some(pace) = pace {
        let pace_label = ui::text(pace, &["pace", "secondary", "nums"]);
        pace_label.set_halign(gtk4::Align::End);
        top.append(&pace_label);
    }
    block.append(&top);
    block.append(&level_bar(fill, severity, marker));

    let bottom = gtk4::Box::new(gtk4::Orientation::Horizontal, 8);
    bottom.append(&amount(value, severity, toggle, &toggles.meter));
    let spring = gtk4::Label::new(None);
    spring.set_hexpand(true);
    bottom.append(&spring);
    if let Some(reset) = reset {
        let classes: &[&str] = if used_up {
            &["reset", "crit", "nums"]
        } else {
            &["reset", "secondary", "nums"]
        };
        bottom.append(&reset_button(reset, classes, &toggles.reset));
    }
    block.append(&bottom);
    if used_up {
        block.set_opacity(0.5);
    }
    block.upcast()
}

fn value_row(
    title: &str,
    value: &str,
    detail: Option<&str>,
    severity: Severity,
    extra: &Extra,
    toggles: &Toggles,
) -> gtk4::Box {
    let wrap = gtk4::Box::new(gtk4::Orientation::Vertical, 2);
    let line = gtk4::Box::new(gtk4::Orientation::Horizontal, 8);
    line.set_margin_top(1);
    line.set_margin_bottom(1);
    let title_label = ui::text(title, &["title-quiet"]);
    title_label.set_hexpand(true);
    title_label.set_halign(gtk4::Align::Start);
    title_label.set_ellipsize(pango::EllipsizeMode::End);
    line.append(&title_label);

    let figures = gtk4::Box::new(gtk4::Orientation::Horizontal, 0);
    figures.set_halign(gtk4::Align::End);
    let mut value_classes = vec!["figure", "nums"];
    value_classes.push(severity_class(severity, false));
    figures.append(&ui::text(value, &value_classes));
    if let Some(detail) = detail {
        figures.append(&ui::text(
            &format!(" · {detail}"),
            &["figure", "secondary", "nums"],
        ));
    }
    if let Some(breakdown) = extra.breakdown.clone() {
        let button = gtk4::Button::new();
        button.add_css_class("plain");
        button.set_child(Some(&figures));
        let copied = toggles.copied.clone();
        let title_owned = title.to_string();
        let preview: Rc<std::cell::RefCell<Option<gtk4::Popover>>> = Rc::default();
        let open: Rc<dyn Fn(&gtk4::Widget, UsageBreakdown, bool) -> gtk4::Popover> =
            Rc::new(move |anchor, breakdown, keep| {
                let copied = copied.clone();
                let title_for_button = title_owned.clone();
                let shot = breakdown.clone();
                crate::breakdown::show_popover(
                    anchor,
                    &title_owned,
                    &breakdown,
                    Rc::new(move || copied(title_for_button.clone(), shot.clone())),
                    keep,
                )
            });
        // Click keeps the details open (with Copy); hover previews them until the pointer leaves.
        let click_open = open.clone();
        let click_breakdown = breakdown.clone();
        let click_preview = preview.clone();
        button.connect_clicked(move |button| {
            if let Some(shown) = click_preview.borrow_mut().take() {
                shown.popdown();
            }
            click_open(button.upcast_ref(), click_breakdown.clone(), true);
        });
        let motion = gtk4::EventControllerMotion::new();
        let enter_preview = preview.clone();
        motion.connect_enter(move |motion, _, _| {
            if let Some(widget) = motion.widget() {
                if let Some(old) = enter_preview.replace(Some(open(&widget, breakdown.clone(), false))) {
                    old.popdown();
                }
            }
        });
        motion.connect_leave(move |_| {
            if let Some(shown) = preview.borrow_mut().take() {
                shown.popdown();
            }
        });
        button.add_controller(motion);
        line.append(&button);
    } else {
        line.append(&figures);
    }
    wrap.append(&line);
    if let Some(claim) = &extra.claim {
        wrap.append(&crate::resets::view(claim));
    }
    wrap
}

pub fn amount(
    value: &str,
    severity: Severity,
    toggle: bool,
    on_toggle: &Rc<dyn Fn()>,
) -> gtk4::Widget {
    let classes = ["figure", "nums", severity_class(severity, true)];
    if !toggle {
        return ui::text(value, &classes).upcast();
    }
    let label = ui::text(value, &classes);
    let button = gtk4::Button::new();
    button.add_css_class("plain");
    button.set_child(Some(&label));
    let on_toggle = on_toggle.clone();
    button.connect_clicked(move |_| on_toggle());
    button.upcast()
}

fn reset_button(value: &str, classes: &[&str], on_toggle: &Rc<dyn Fn()>) -> gtk4::Widget {
    let label = ui::text(value, classes);
    let button = gtk4::Button::new();
    button.add_css_class("plain");
    button.set_child(Some(&label));
    let on_toggle = on_toggle.clone();
    button.connect_clicked(move |_| on_toggle());
    button.upcast()
}

pub fn severity_class(severity: Severity, bar_amount: bool) -> &'static str {
    match severity {
        Severity::Warning => "warn",
        Severity::Critical => "crit",
        Severity::Normal if bar_amount => "secondary",
        Severity::Normal => "figure-strong",
    }
}

pub fn level_bar(fill: f64, severity: Severity, marker: Option<f64>) -> gtk4::DrawingArea {
    let area = gtk4::DrawingArea::new();
    area.set_content_height(9);
    area.set_hexpand(true);
    area.set_draw_func(move |_area, context, width, height| {
        paint_bar(context, width, height, fill, severity, marker);
    });
    area
}

fn paint_bar(
    context: &cairo::Context,
    width: i32,
    height: i32,
    fill: f64,
    severity: Severity,
    marker: Option<f64>,
) {
    let dark = prefers_dark();
    let bar_h = 5.0;
    let y = (height as f64 - bar_h) / 2.0;
    let radius = bar_h / 2.0;
    let track = if dark {
        (1.0, 1.0, 1.0, 0.12)
    } else {
        (0.0, 0.0, 0.0, 0.09)
    };
    rounded(context, 0.0, y, width as f64, bar_h, radius);
    context.set_source_rgba(track.0, track.1, track.2, track.3);
    let _ = context.fill();

    let clamped = fill.clamp(0.0, 1.0);
    if clamped > 0.0 {
        let filled = ((width as f64) * clamped).max(4.0).min(width as f64);
        context.save().ok();
        rounded(context, 0.0, y, width as f64, bar_h, radius);
        let _ = context.clip();
        context.rectangle(0.0, y, filled, bar_h);
        let (red, green, blue) = severity_rgb(severity, dark);
        context.set_source_rgb(red, green, blue);
        let _ = context.fill();
        let _ = context.restore();
    }

    if let Some(marker) = marker {
        let x = ((width as f64) * marker.clamp(0.0, 1.0)).clamp(1.0, width as f64 - 1.0);
        let tick_h = bar_h + 4.0;
        let tick_y = y - 2.0;
        rounded(context, x - 1.0, tick_y, 2.0, tick_h, 1.0);
        if dark {
            context.set_source_rgba(0.96, 0.96, 0.97, 0.92);
        } else {
            context.set_source_rgba(0.11, 0.11, 0.12, 0.78);
        }
        let _ = context.fill();
    }
}

pub fn severity_rgb(severity: Severity, dark: bool) -> (f64, f64, f64) {
    let hex = match (severity, dark) {
        (Severity::Warning, _) => 0xff9f0a,
        (Severity::Critical, true) => 0xff453a,
        (Severity::Critical, false) => 0xff3b30,
        (Severity::Normal, true) => 0x0a84ff,
        (Severity::Normal, false) => 0x007aff,
    };
    (
        ((hex >> 16) & 0xff) as f64 / 255.0,
        ((hex >> 8) & 0xff) as f64 / 255.0,
        (hex & 0xff) as f64 / 255.0,
    )
}

fn rounded(context: &cairo::Context, x: f64, y: f64, width: f64, height: f64, radius: f64) {
    let radius = radius.min(width / 2.0).min(height / 2.0).max(0.0);
    context.new_sub_path();
    context.arc(
        x + width - radius,
        y + radius,
        radius,
        -std::f64::consts::FRAC_PI_2,
        0.0,
    );
    context.arc(
        x + width - radius,
        y + height - radius,
        radius,
        0.0,
        std::f64::consts::FRAC_PI_2,
    );
    context.arc(
        x + radius,
        y + height - radius,
        radius,
        std::f64::consts::FRAC_PI_2,
        std::f64::consts::PI,
    );
    context.arc(
        x + radius,
        y + radius,
        radius,
        std::f64::consts::PI,
        std::f64::consts::PI + std::f64::consts::FRAC_PI_2,
    );
    context.close_path();
}
