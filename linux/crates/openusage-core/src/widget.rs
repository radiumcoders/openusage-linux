//! How a reading is shown: a port of `WidgetData.swift`, `Pace.swift`, `MetricFormatter.swift`,
//! `Formatters.swift`, and `DashboardRowModel.swift`. The panel, the bar module, and the tooltip all draw
//! from `WidgetData::row`, so they read exactly like the Mac dashboard ("36% left", "1d 6h").

use chrono::{DateTime, Duration, Local, Utc};

use crate::descriptor::{Descriptor, SessionStartSignal, Style};
use crate::metric::{MetricKind, MetricLine, MetricValue, ProgressFormat, ProviderSnapshot};

pub const NO_DATA_VALUE: &str = "—";
pub const NO_DATA_DETAIL: &str = "No data";
pub const IMMINENT: &str = "soon";

/// Used or left: the Mac's meter style. Left is its default.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DisplayMode {
    Used,
    #[default]
    Remaining,
}

impl DisplayMode {
    pub fn word(self) -> &'static str {
        match self {
            Self::Used => "used",
            Self::Remaining => "left",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    Normal,
    Warning,
    Critical,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MeterState {
    NoData,
    Spent,
    RunningOut,
    CloseToLimit,
    Healthy,
    Unpaced,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PaceStatus {
    Ahead,
    OnTrack,
    Behind,
}

#[derive(Debug, Clone, PartialEq)]
pub struct WidgetData {
    pub title: String,
    pub kind: MetricKind,
    pub used: f64,
    pub limit: Option<f64>,
    pub display_mode: DisplayMode,
    pub resets_at: Option<DateTime<Utc>>,
    pub expiries_at: Vec<DateTime<Utc>>,
    pub shows_reset_expiries: bool,
    pub period_duration_ms: Option<i64>,
    pub value_text_override: Option<String>,
    pub unbounded_value_word: Option<String>,
    pub has_data: bool,
    pub values: Vec<MetricValue>,
    pub session_start: Option<SessionStartSignal>,
}

pub fn clamp_percent(value: f64) -> f64 {
    if value.is_finite() { value.clamp(0.0, 100.0) } else { 0.0 }
}

impl WidgetData {
    /// The descriptor's empty reading.
    pub fn sample(descriptor: &Descriptor) -> Self {
        let mut data = WidgetData {
            title: descriptor.title.to_string(),
            kind: MetricKind::Dollars,
            used: 0.0,
            limit: None,
            display_mode: DisplayMode::Remaining,
            resets_at: None,
            expiries_at: vec![],
            shows_reset_expiries: false,
            period_duration_ms: None,
            value_text_override: None,
            unbounded_value_word: None,
            has_data: true,
            values: vec![],
            session_start: None,
        };
        match &descriptor.style {
            Style::Percent { session_start } => {
                data.kind = MetricKind::Percent;
                data.limit = Some(100.0);
                data.session_start = *session_start;
            }
            Style::BoundedDollars { limit, value_word } => {
                data.limit = Some(*limit);
                data.unbounded_value_word = value_word.map(str::to_string);
            }
            Style::BoundedCount { limit, period_ms } => {
                data.kind = MetricKind::Count;
                data.limit = Some(*limit);
                data.period_duration_ms = *period_ms;
            }
            Style::Values { value_word, shows_reset_expiries } => {
                data.unbounded_value_word = value_word.map(str::to_string);
                data.shows_reset_expiries = *shows_reset_expiries;
            }
            Style::DollarBalance { value_word } => data.unbounded_value_word = Some(value_word.to_string()),
            Style::Badge => data.kind = MetricKind::Count,
            Style::History => {}
        }
        data
    }

    /// The descriptor's reading in `snapshot`, or its sample marked as having no data.
    pub fn resolve(descriptor: &Descriptor, snapshot: Option<&ProviderSnapshot>, mode: DisplayMode) -> Self {
        let line = snapshot.and_then(|snapshot| snapshot.line(descriptor.metric_label));
        let mut data = match line {
            Some(MetricLine::Progress { used, limit, format, resets_at, period_duration_ms, .. }) => {
                let mut data = Self::sample(descriptor);
                data.kind = format.metric_kind();
                data.used = if *format == ProgressFormat::Percent { clamp_percent(*used) } else { *used };
                data.limit = Some(*limit);
                data.resets_at = *resets_at;
                data.period_duration_ms = *period_duration_ms;
                data.values.clear();
                data.unbounded_value_word = None;
                data
            }
            Some(MetricLine::Values { values, expiries_at, .. }) => {
                let mut data = Self::sample(descriptor);
                data.values = values.clone();
                data.limit = None;
                data.expiries_at = expiries_at.clone();
                data.has_data = !values.is_empty();
                data
            }
            Some(MetricLine::Badge { text, .. }) => {
                let mut data = Self::sample(descriptor);
                data.value_text_override = Some(text.clone());
                data
            }
            None => {
                let mut data = Self::sample(descriptor);
                data.has_data = false;
                data
            }
        };
        data.display_mode = mode;
        data
    }

    pub fn is_bounded(&self) -> bool {
        self.limit.is_some()
    }

    pub fn has_meter_style_toggle(&self) -> bool {
        self.has_data && self.is_bounded() && self.value_text_override.is_none()
    }

    pub fn displayed_value(&self) -> f64 {
        match (self.display_mode, self.limit) {
            (DisplayMode::Remaining, Some(limit)) => (limit - self.used).max(0.0),
            _ => self.used,
        }
    }

    fn rounded_at_display_precision(&self, value: f64) -> f64 {
        match self.kind {
            MetricKind::Percent => value.round(),
            MetricKind::Count => (value * 10.0).round() / 10.0,
            MetricKind::Dollars => (value * 100.0).round() / 100.0,
        }
    }

    /// How full the bar is drawn.
    pub fn fraction(&self) -> f64 {
        match self.limit {
            Some(limit) if limit > 0.0 => (self.rounded_at_display_precision(self.displayed_value()) / limit).clamp(0.0, 1.0),
            _ => 0.0,
        }
    }

    pub fn remaining_fraction(&self) -> f64 {
        match self.limit {
            Some(limit) if limit > 0.0 => ((limit - self.used) / limit).clamp(0.0, 1.0),
            _ => 0.0,
        }
    }

    /// The bar's color, and whether Priority lists it, from what's left alone.
    pub fn severity(&self) -> Severity {
        match self.limit {
            Some(limit) if self.has_data && limit > 0.0 => {
                let percent_left = (self.remaining_fraction() * 100.0).round() as i64;
                if percent_left <= 10 {
                    Severity::Critical
                } else if percent_left <= 30 {
                    Severity::Warning
                } else {
                    Severity::Normal
                }
            }
            _ => Severity::Normal,
        }
    }

    pub fn value_text(&self) -> String {
        if !self.has_data {
            return NO_DATA_VALUE.into();
        }
        if let Some(text) = &self.value_text_override {
            return text.clone();
        }
        if let Some(first) = self.values.first() {
            return format_number(first.number, first.kind, FormatStyle::Row);
        }
        format_number(self.displayed_value(), self.kind, FormatStyle::Row)
    }

    /// The reading with the word that explains it, like `49% left`.
    pub fn amount_text(&self) -> String {
        if self.has_meter_style_toggle() {
            format!("{} {}", self.value_text(), self.display_mode.word())
        } else {
            self.value_text()
        }
    }

    /// The compact figure the bar strip shows.
    pub fn menu_bar_value(&self) -> String {
        match self.limit {
            Some(limit) if self.kind == MetricKind::Percent && limit > 0.0 => {
                format_number(self.displayed_value() / limit * 100.0, MetricKind::Percent, FormatStyle::Tray)
            }
            _ => format_number(self.displayed_value(), self.kind, FormatStyle::Tray),
        }
    }

    pub fn unbounded_figures(&self) -> Vec<String> {
        if !self.has_data {
            return vec![NO_DATA_DETAIL.into()];
        }
        if let Some(text) = &self.value_text_override {
            return vec![text.clone()];
        }
        if self.values.len() == 1 && self.values[0].kind == MetricKind::Dollars {
            if let Some(word) = &self.unbounded_value_word {
                return vec![format!("{} {word}", format_number(self.values[0].number, MetricKind::Dollars, FormatStyle::Row))];
            }
        }
        if !self.values.is_empty() {
            let has_dollars = self.values.iter().any(|value| value.kind == MetricKind::Dollars);
            return self
                .values
                .iter()
                .filter(|value| !has_dollars || value.label.as_deref() != Some("credits"))
                .map(|value| format_value(value, FormatStyle::Row))
                .collect();
        }
        let word = self.unbounded_value_word.clone().unwrap_or_else(|| self.display_mode.word().to_string());
        vec![format!("{} {word}", self.value_text())]
    }

    pub fn expiry_severity(&self, now: DateTime<Utc>) -> Severity {
        let Some(soonest) = self.expiries_at.iter().min().filter(|_| self.has_data) else { return Severity::Normal };
        let seconds = (*soonest - now).num_seconds();
        if seconds <= 48 * 3600 {
            Severity::Critical
        } else if seconds <= 7 * 24 * 3600 {
            Severity::Warning
        } else {
            Severity::Normal
        }
    }

    fn pace_context(&self) -> Option<(f64, DateTime<Utc>, f64)> {
        let limit = self.limit.filter(|limit| self.has_data && *limit > 0.0)?;
        let period = self.period_duration_ms.filter(|period| *period > 0)?;
        Some((limit, self.resets_at?, period as f64 / 1000.0))
    }

    fn pace(&self, now: DateTime<Utc>) -> Option<(PaceStatus, f64)> {
        if self.is_fresh_session_window(now) {
            return None;
        }
        let (limit, resets_at, period) = self.pace_context()?;
        let (status, projected) = evaluate_pace(self.used, limit, resets_at, period, now)?;
        if status != PaceStatus::Ahead && self.used / limit < 0.05 {
            return None;
        }
        Some((status, projected / limit))
    }

    pub fn meter_state(&self, now: DateTime<Utc>) -> MeterState {
        let Some(limit) = self.limit.filter(|limit| self.has_data && *limit > 0.0) else {
            return if self.has_data { MeterState::Unpaced } else { MeterState::NoData };
        };
        if self.rounded_at_display_precision(limit - self.used) <= 0.0 {
            return MeterState::Spent;
        }
        match self.pace(now) {
            None => MeterState::Unpaced,
            Some((PaceStatus::Ahead, _)) => MeterState::Healthy,
            Some((PaceStatus::OnTrack, projected)) => {
                if ((1.0 - projected) * 100.0).round() >= 1.0 { MeterState::CloseToLimit } else { MeterState::RunningOut }
            }
            Some((PaceStatus::Behind, _)) => MeterState::RunningOut,
        }
    }

    /// "~22% left at reset": the pace line under a bar.
    pub fn pace_text(&self, now: DateTime<Utc>) -> Option<String> {
        if self.meter_state(now) == MeterState::Spent {
            return Some("Limit reached".into());
        }
        let (status, projected) = self.pace(now)?;
        let percent = |share: f64| (share * 100.0).round() as i64;
        Some(match status {
            PaceStatus::Ahead => format!("~{}% left at reset", percent(1.0 - projected)),
            PaceStatus::OnTrack => format!("~{}% used at reset", percent(projected)),
            PaceStatus::Behind => format!("~{}% over limit at reset", percent(projected - 1.0).max(1)),
        })
    }

    /// Where the pace tick sits on the bar: the share of the window that has elapsed.
    pub fn pace_marker(&self, now: DateTime<Utc>) -> Option<f64> {
        if self.is_fresh_session_window(now) {
            return None;
        }
        let (_, resets_at, period) = self.pace_context()?;
        let elapsed = (now - (resets_at - Duration::milliseconds((period * 1000.0) as i64))).num_milliseconds() as f64 / 1000.0;
        (elapsed > 0.0 && now < resets_at).then(|| {
            let share = (elapsed / period).clamp(0.0, 1.0);
            if self.display_mode == DisplayMode::Remaining { 1.0 - share } else { share }
        })
    }

    /// How long until the limit at the current pace.
    pub fn runway(&self, now: DateTime<Utc>) -> Option<f64> {
        let (_, projected) = self.pace(now)?;
        let (limit, _, period) = self.pace_context()?;
        Some((1.0 - self.used / limit).max(0.0) * period / projected)
    }

    pub fn is_fresh_session_window(&self, now: DateTime<Utc>) -> bool {
        let Some(signal) = self.session_start else { return false };
        if !self.has_data || self.limit.is_none() || self.used > 0.0 {
            return false;
        }
        match signal {
            SessionStartSignal::ZeroUsage => self.resets_at.is_some_and(|resets_at| now < resets_at),
            SessionStartSignal::MissingResetDate => self.resets_at.is_none(),
        }
    }

    /// "1d 6h" until the reset, "soon" within five minutes, nil for a fresh window.
    pub fn reset_text(&self, now: DateTime<Utc>) -> Option<String> {
        let resets_at = self.resets_at.filter(|_| self.has_data && !self.is_fresh_session_window(now))?;
        let seconds = (resets_at - now).num_milliseconds() as f64 / 1000.0;
        if seconds <= 0.0 {
            return Some(IMMINENT.into());
        }
        compact_duration(seconds)
    }

    /// The row the dashboard draws.
    pub fn row(&self, now: DateTime<Utc>) -> Row {
        if !self.has_data || !self.is_bounded() {
            let figures = self.unbounded_figures();
            let detail = (figures.len() > 1).then(|| figures[1..].join(" · "));
            return Row {
                title: self.title.clone(),
                mark: Mark::Value { value: figures[0].clone(), detail, severity: self.expiry_severity(now) },
                pace: None,
            };
        }
        Row {
            title: self.title.clone(),
            mark: Mark::Bar {
                fill: self.fraction(),
                severity: self.severity(),
                value: self.amount_text(),
                reset: self.reset_text(now).map(|text| if text == IMMINENT { "Resets soon".into() } else { format!("Resets in {text}") }),
                used_up: self.meter_state(now) == MeterState::Spent,
                marker: self.pace_marker(now),
            },
            pace: self.pace_text(now),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Mark {
    Bar { fill: f64, severity: Severity, value: String, reset: Option<String>, used_up: bool, marker: Option<f64> },
    Value { value: String, detail: Option<String>, severity: Severity },
}

/// One dashboard row: "Weekly  ~22% left at reset / [bar] / 36% left  Resets in 1d 6h".
#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    pub title: String,
    pub mark: Mark,
    pub pace: Option<String>,
}

fn evaluate_pace(used: f64, limit: f64, resets_at: DateTime<Utc>, period: f64, now: DateTime<Utc>) -> Option<(PaceStatus, f64)> {
    if limit <= 0.0 || period <= 0.0 || used <= 0.0 {
        return None;
    }
    let start = resets_at - Duration::milliseconds((period * 1000.0) as i64);
    let elapsed = (now - start).num_milliseconds() as f64 / 1000.0;
    if elapsed < (period * 0.01).max(60.0) || now >= resets_at {
        return None;
    }
    let projected = used / elapsed * period;
    if used >= limit {
        return Some((PaceStatus::Behind, projected));
    }
    let status = if projected <= limit * 0.9 {
        PaceStatus::Ahead
    } else if projected <= limit {
        PaceStatus::OnTrack
    } else {
        PaceStatus::Behind
    };
    Some((status, projected))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FormatStyle {
    Tray,
    Row,
}

fn group_thousands(whole: u64) -> String {
    let digits = whole.to_string();
    let mut out = String::new();
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index) % 3 == 0 {
            out.push(',');
        }
        out.push(digit);
    }
    out
}

/// `value` with `decimals` fraction digits, thousands grouped, trailing zeros kept.
fn fixed(value: f64, decimals: usize) -> String {
    let negative = value < 0.0;
    let text = format!("{:.*}", decimals, value.abs());
    let (whole, fraction) = text.split_once('.').map_or((text.as_str(), None), |(w, f)| (w, Some(f)));
    let grouped = group_thousands(whole.parse().unwrap_or(0));
    let sign = if negative && text.trim_matches(['0', '.']) != "" { "-" } else { "" };
    match fraction {
        Some(fraction) => format!("{sign}{grouped}.{fraction}"),
        None => format!("{sign}{grouped}"),
    }
}

/// Up to one fraction digit, dropped when zero ("1.5", "2").
fn up_to_one(value: f64) -> String {
    let rounded = (value * 10.0).round() / 10.0;
    if rounded.fract() == 0.0 { fixed(rounded, 0) } else { fixed(rounded, 1) }
}

/// "1.2K", "3.4M", "5B", "1.2T" with up to one fraction digit.
fn compact(value: f64) -> String {
    let magnitude = value.abs();
    let (scaled, suffix) = if magnitude >= 1e12 {
        (value / 1e12, "T")
    } else if magnitude >= 1e9 {
        (value / 1e9, "B")
    } else if magnitude >= 1e6 {
        (value / 1e6, "M")
    } else {
        (value / 1e3, "K")
    };
    format!("{}{suffix}", up_to_one(scaled))
}

pub fn format_number(value: f64, kind: MetricKind, style: FormatStyle) -> String {
    match kind {
        MetricKind::Percent => format!("{}%", clamp_percent(value).round() as i64),
        MetricKind::Dollars => {
            if value.abs() >= 1000.0 {
                return format!("${}", compact(value));
            }
            match style {
                FormatStyle::Tray => {
                    let digits = if value != 0.0 && value.abs() < 1.0 { 2 } else { 0 };
                    format!("${}", fixed(value, digits))
                }
                FormatStyle::Row => {
                    let whole = ((value * 100.0).round() % 100.0) == 0.0;
                    let text = fixed(value.abs(), if whole { 0 } else { 2 });
                    if value < 0.0 { format!("-${text}") } else { format!("${text}") }
                }
            }
        }
        MetricKind::Count => {
            if value.abs() >= 1000.0 { compact(value) } else { up_to_one(value) }
        }
    }
}

pub fn format_value(value: &MetricValue, style: FormatStyle) -> String {
    let text = format_number(value.number, value.kind, style);
    match value.label.as_deref().filter(|label| !label.is_empty()) {
        Some(label) => format!("{text} {label}"),
        None => text,
    }
}

/// "1d 6h", "3h 12m", "3h", "12m".
pub fn compact_duration(seconds: f64) -> Option<String> {
    if !seconds.is_finite() || seconds <= 0.0 {
        return None;
    }
    let total_minutes = ((seconds / 60.0).ceil() as i64).max(1);
    let days = total_minutes / (24 * 60);
    let hours = (total_minutes % (24 * 60)) / 60;
    let minutes = total_minutes % 60;
    Some(if days > 0 {
        format!("{days}d {hours}h")
    } else if hours > 0 {
        if minutes > 0 { format!("{hours}h {minutes}m") } else { format!("{hours}h") }
    } else {
        format!("{minutes}m")
    })
}

/// A local clock time for "Retrying at 14:05".
pub fn compact_clock(date: DateTime<Utc>) -> String {
    date.with_timezone(&Local).format("%H:%M").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::account::Account;
    use crate::descriptor::descriptors;
    use crate::kind::ProviderKind;

    fn snapshot(lines: Vec<MetricLine>, now: DateTime<Utc>) -> ProviderSnapshot {
        ProviderSnapshot {
            breakdowns: Default::default(),
            provider_id: "x".into(),
            display_name: "Claude".into(),
            plan: None,
            lines,
            notice: None,
            refreshed_at: now,
        }
    }

    #[test]
    fn a_weekly_bar_reads_like_the_mac_dashboard() {
        let now = Utc::now();
        let account = Account::new(ProviderKind::Claude, "Work", None);
        let weekly = descriptors(&account).into_iter().find(|d| d.key == "weekly").unwrap();
        let resets = now + Duration::hours(30);
        let line = MetricLine::percent("Weekly", 64.0, Some(resets), Some(7 * 24 * 3600 * 1000));
        let data = WidgetData::resolve(&weekly, Some(&snapshot(vec![line], now)), DisplayMode::Remaining);
        let row = data.row(now);
        let Mark::Bar { value, reset, severity, .. } = row.mark else { panic!("expected a bar") };
        assert_eq!(value, "36% left");
        assert_eq!(reset.as_deref(), Some("Resets in 1d 6h"));
        assert_eq!(severity, Severity::Normal);
        assert!(row.pace.unwrap().contains("at reset"));
    }

    #[test]
    fn formats_match_metric_formatter() {
        assert_eq!(format_number(7121.12, MetricKind::Dollars, FormatStyle::Row), "$7.1K");
        assert_eq!(format_number(49.85, MetricKind::Dollars, FormatStyle::Row), "$49.85");
        assert_eq!(format_number(12.0, MetricKind::Dollars, FormatStyle::Row), "$12");
        assert_eq!(format_number(0.5, MetricKind::Dollars, FormatStyle::Tray), "$0.50");
        assert_eq!(format_number(35_800_000.0, MetricKind::Count, FormatStyle::Row), "35.8M");
        assert_eq!(format_number(738_500.0, MetricKind::Count, FormatStyle::Row), "738.5K");
        assert_eq!(format_number(2.0, MetricKind::Count, FormatStyle::Row), "2");
        assert_eq!(compact_duration(3600.0 * 5.0), Some("5h".into()));
        assert_eq!(compact_duration(90.0), Some("2m".into()));
    }

    #[test]
    fn missing_reading_is_no_data_and_low_remaining_is_critical() {
        let now = Utc::now();
        let account = Account::new(ProviderKind::Zai, "Z.ai", None);
        let session = descriptors(&account).into_iter().next().unwrap();
        assert_eq!(WidgetData::resolve(&session, None, DisplayMode::Remaining).value_text(), NO_DATA_VALUE);
        let line = MetricLine::percent("Session", 95.0, None, None);
        let data = WidgetData::resolve(&session, Some(&snapshot(vec![line], now)), DisplayMode::Remaining);
        assert_eq!(data.severity(), Severity::Critical);
        assert_eq!(data.menu_bar_value(), "5%");
    }
}
