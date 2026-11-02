//! Port of `CursorUsageEvents.swift`.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde_json::{Map, Value};

use super::parse::number;

pub(crate) const CHARGEABLE_KIND: &str = "USAGE_EVENT_KIND_USAGE_BASED";

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Event {
    pub date: DateTime<Utc>,
    pub model: Option<String>,
    pub spend: f64,
    pub tokens: f64,
    pub input_tokens: f64,
    pub cache_read_tokens: f64,
    pub cache_write_tokens: f64,
    pub output_tokens: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Page {
    pub total: usize,
    pub events: Vec<Event>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct ModelTotals {
    pub spend: f64,
    pub tokens: f64,
    pub events: f64,
    pub input_tokens: f64,
    pub cache_read_tokens: f64,
    pub cache_write_tokens: f64,
    pub output_tokens: f64,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct Day {
    pub spend: f64,
    pub tokens: f64,
    pub events: f64,
    pub input_tokens: f64,
    pub cache_read_tokens: f64,
    pub cache_write_tokens: f64,
    pub output_tokens: f64,
    pub by_model: BTreeMap<String, ModelTotals>,
}

const TOKEN_FIELDS: [&str; 4] = [
    "inputTokens",
    "cacheReadTokens",
    "cacheWriteTokens",
    "outputTokens",
];

/// `None` when the page or any event is not what Cursor sends, so a day is never summed from part of its events.
pub(crate) fn parse_page(body: &[u8]) -> Option<Page> {
    if body.is_empty() {
        return None;
    }
    let value: Value = serde_json::from_slice(body).ok()?;
    let object = value.as_object()?;
    let total = number(object.get("totalUsageEventsCount")?)?;
    if !total.is_finite() || total < 0.0 || total.round() != total || total >= 1e15 {
        return None;
    }
    let raw_events = match object.get("usageEventsDisplay") {
        None => Vec::new(),
        Some(Value::Array(items)) => items.clone(),
        Some(_) => return None,
    };
    let mut events = Vec::with_capacity(raw_events.len());
    for raw in &raw_events {
        events.push(parse_event(raw)?);
    }
    Some(Page {
        total: total as usize,
        events,
    })
}

fn parse_event(raw: &Value) -> Option<Event> {
    let event = raw.as_object()?;
    let millis = number(event.get("timestamp")?)?;
    if millis < 0.0 {
        return None;
    }
    let date = DateTime::from_timestamp_millis(millis as i64)?;
    let mut spend = 0.0;
    let chargeable_kind = event.get("kind").and_then(Value::as_str) == Some(CHARGEABLE_KIND);
    let chargeable = event.get("isChargeable").and_then(Value::as_bool) != Some(false);
    if chargeable_kind && chargeable {
        if let Some(raw_cents) = event.get("chargedCents") {
            let cents = number(raw_cents)?;
            if cents < 0.0 {
                return None;
            }
            spend = cents / 100.0;
        }
    }
    let usage = match event.get("tokenUsage") {
        None => Map::new(),
        Some(Value::Object(map)) => map.clone(),
        Some(_) => return None,
    };
    let mut counts = [0.0; 4];
    let mut tokens = 0.0;
    for (index, field) in TOKEN_FIELDS.iter().enumerate() {
        let count = if let Some(raw_count) = usage.get(*field) {
            let parsed = number(raw_count)?;
            if parsed < 0.0 {
                return None;
            }
            parsed
        } else {
            0.0
        };
        counts[index] = count;
        tokens += count;
    }
    Some(Event {
        date,
        model: event
            .get("model")
            .and_then(Value::as_str)
            .map(str::to_string),
        spend,
        tokens,
        input_tokens: counts[0],
        cache_read_tokens: counts[1],
        cache_write_tokens: counts[2],
        output_tokens: counts[3],
    })
}

fn add(totals: &mut ModelTotals, event: &Event) {
    totals.spend += event.spend;
    totals.tokens += event.tokens;
    totals.events += 1.0;
    totals.input_tokens += event.input_tokens;
    totals.cache_read_tokens += event.cache_read_tokens;
    totals.cache_write_tokens += event.cache_write_tokens;
    totals.output_tokens += event.output_tokens;
}

/// One day's events summed. A day with no events reports zeros.
pub(crate) fn day(events: &[Event]) -> Day {
    let mut day = Day::default();
    for event in events {
        day.spend += event.spend;
        day.tokens += event.tokens;
        day.events += 1.0;
        day.input_tokens += event.input_tokens;
        day.cache_read_tokens += event.cache_read_tokens;
        day.cache_write_tokens += event.cache_write_tokens;
        day.output_tokens += event.output_tokens;
        if let Some(model) = &event.model {
            add(day.by_model.entry(model.clone()).or_default(), event);
        }
    }
    day
}
