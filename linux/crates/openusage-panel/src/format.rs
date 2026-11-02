//! Reset clocks and breakdown figures. Absolute times follow the Mac wording
//! ("Wed 4 PM", "3:05 PM", "Nov 20").

use chrono::{DateTime, Datelike, Local, Timelike, Utc};

use openusage_core::account::Account;
use openusage_core::metric::{BreakdownModel, MetricKind, UsageBreakdown};
use openusage_core::widget::{self, FormatStyle, WidgetData, IMMINENT};

const WEEKDAYS: [&str; 7] = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];
const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

pub fn account_name(account: &Account) -> &str {
    if account.label.is_empty() {
        account.kind.display_name()
    } else {
        account.label.as_str()
    }
}

/// "3:05 PM" today, "Wed 4 PM" within a week, "Nov 20" after that.
pub fn exact_time(at: DateTime<Utc>, now: DateTime<Utc>) -> String {
    let local = at.with_timezone(&Local);
    let today = now.with_timezone(&Local).date_naive();
    let days = (local.date_naive() - today).num_days();
    let hour = match local.hour() % 12 {
        0 => 12,
        hour => hour,
    };
    let ampm = if local.hour() < 12 { "AM" } else { "PM" };
    if days <= 0 {
        format!("{hour}:{:02} {ampm}", local.minute())
    } else if days < 7 {
        let weekday = WEEKDAYS[local.weekday().num_days_from_monday() as usize];
        format!("{weekday} {hour} {ampm}")
    } else {
        format!("{} {}", MONTHS[local.month0() as usize], local.day())
    }
}

fn clock(data: &WidgetData, now: DateTime<Utc>) -> Option<String> {
    let at = data
        .resets_at
        .filter(|_| data.has_data && !data.is_fresh_session_window(now))?;
    let seconds = (at - now).num_milliseconds() as f64 / 1000.0;
    if seconds <= 0.0 {
        return Some(IMMINENT.into());
    }
    Some(exact_time(at, now))
}

/// The short reset Priority shows: "1d 6h" or "Wed 4 PM".
pub fn short_reset(data: &WidgetData, now: DateTime<Utc>, absolute: bool) -> Option<String> {
    if absolute {
        clock(data, now)
    } else {
        data.reset_text(now)
    }
}

/// The reset under a bar: "Resets in 1d 6h" or "Resets Wed 4 PM".
pub fn bar_reset(data: &WidgetData, now: DateTime<Utc>, absolute: bool) -> Option<String> {
    let text = short_reset(data, now, absolute)?;
    Some(if text == IMMINENT {
        "Resets soon".into()
    } else if absolute {
        format!("Resets {text}")
    } else {
        format!("Resets in {text}")
    })
}

pub struct ModelRow {
    pub name: String,
    pub figures: String,
}

/// The top models, with anything past the shown count summed as Other.
pub fn model_rows(breakdown: &UsageBreakdown) -> Vec<ModelRow> {
    let shown = UsageBreakdown::SHOWN_MODELS;
    let mut rows: Vec<ModelRow> = breakdown.models.iter().take(shown).map(model_row).collect();
    if breakdown.models.len() > shown {
        let mut other = BreakdownModel {
            name: "Other".into(),
            dollars: None,
            tokens: None,
            share: None,
        };
        for model in breakdown.models.iter().skip(shown) {
            add_opt(&mut other.dollars, model.dollars);
            add_opt(&mut other.tokens, model.tokens);
            add_opt(&mut other.share, model.share);
        }
        rows.push(model_row(&other));
    }
    rows
}

fn add_opt(slot: &mut Option<f64>, extra: Option<f64>) {
    if let Some(extra) = extra {
        *slot = Some(slot.unwrap_or(0.0) + extra);
    }
}

fn model_row(model: &BreakdownModel) -> ModelRow {
    let mut parts = Vec::new();
    if let Some(dollars) = model.dollars {
        parts.push(widget::format_number(
            dollars,
            MetricKind::Dollars,
            FormatStyle::Row,
        ));
    }
    if let Some(tokens) = model.tokens {
        parts.push(format!(
            "{} tokens",
            widget::format_number(tokens, MetricKind::Count, FormatStyle::Row)
        ));
    }
    if let Some(share) = model.share {
        parts.push(format!(
            "{}%",
            (share.clamp(0.0, 1.0) * 100.0).round() as i64
        ));
    }
    ModelRow {
        name: model.name.clone(),
        figures: parts.join(" · "),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn local(year: i32, month: u32, day: u32, hour: u32, minute: u32) -> DateTime<Utc> {
        Local
            .with_ymd_and_hms(year, month, day, hour, minute, 0)
            .single()
            .unwrap()
            .with_timezone(&Utc)
    }

    #[test]
    fn exact_times_read_like_the_mac_panel() {
        let now = local(2026, 11, 2, 12, 0);
        assert_eq!(exact_time(local(2026, 11, 2, 15, 5), now), "3:05 PM");
        assert_eq!(exact_time(local(2026, 11, 4, 16, 0), now), "Wed 4 PM");
        assert_eq!(exact_time(local(2026, 11, 20, 9, 0), now), "Nov 20");
    }

    #[test]
    fn breakdown_keeps_five_models_and_groups_the_rest() {
        let models = (0..7)
            .map(|index| BreakdownModel {
                name: format!("M{index}"),
                dollars: Some(index as f64),
                tokens: Some(1000.0 * index as f64),
                share: None,
            })
            .collect();
        let rows = model_rows(&UsageBreakdown { models });
        assert_eq!(rows.len(), 6);
        assert_eq!(rows[5].name, "Other");
        assert!(rows[5].figures.contains('$'));
    }
}
