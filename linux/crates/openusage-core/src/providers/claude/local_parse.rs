//! Line parsing for `local`: what one Claude Code log line holds, under ccusage's validity rules.

use chrono::{DateTime, Utc};
use serde::de::IgnoredAny;
use serde::{Deserialize, Deserializer};

use super::Entry;
use crate::pricing::Tokens;

/// A field that can be absent, `null`, or a value: Claude never writes `null` into the fields read
/// here, so a line that does is a foreign shape and is skipped.
#[derive(Default)]
enum Slot<T> {
    #[default]
    Missing,
    Null,
    Present(T),
}

impl<'de, T: Deserialize<'de>> Deserialize<'de> for Slot<T> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Option::<T>::deserialize(deserializer).map(|value| value.map_or(Slot::Null, Slot::Present))
    }
}

impl<T> Slot<T> {
    fn is_null(&self) -> bool {
        matches!(self, Slot::Null)
    }

    fn value(self) -> Option<T> {
        match self {
            Slot::Present(value) => Some(value),
            _ => None,
        }
    }

    fn as_ref(&self) -> Option<&T> {
        match self {
            Slot::Present(value) => Some(value),
            _ => None,
        }
    }
}

#[derive(Deserialize)]
struct RawLine {
    timestamp: Option<String>,
    #[serde(default)]
    cwd: Slot<IgnoredAny>,
    #[serde(rename = "costUSD", default)]
    cost_usd: Slot<f64>,
    #[serde(default)]
    version: Slot<String>,
    #[serde(rename = "sessionId", default)]
    session_id: Slot<String>,
    #[serde(rename = "requestId", default)]
    request_id: Slot<String>,
    #[serde(rename = "isApiErrorMessage", default)]
    is_api_error: Slot<IgnoredAny>,
    #[serde(rename = "isSidechain", default)]
    is_sidechain: Option<bool>,
    message: Option<RawMessage>,
}

#[derive(Deserialize)]
struct RawMessage {
    #[serde(default)]
    id: Slot<String>,
    #[serde(default)]
    model: Slot<String>,
    usage: Option<RawUsage>,
}

#[derive(Deserialize)]
struct RawUsage {
    input_tokens: Option<f64>,
    output_tokens: Option<f64>,
    #[serde(default)]
    cache_creation_input_tokens: Slot<f64>,
    #[serde(default)]
    cache_read_input_tokens: Slot<f64>,
    #[serde(default)]
    speed: Slot<String>,
    cache_creation: Option<RawCacheCreation>,
    iterations: Option<Vec<RawIteration>>,
}

#[derive(Deserialize)]
struct RawCacheCreation {
    ephemeral_5m_input_tokens: Option<f64>,
    ephemeral_1h_input_tokens: Option<f64>,
}

#[derive(Deserialize)]
struct RawIteration {
    #[serde(rename = "type")]
    kind: Option<String>,
    model: Option<String>,
    input_tokens: Option<f64>,
    output_tokens: Option<f64>,
    cache_creation_input_tokens: Option<f64>,
    cache_read_input_tokens: Option<f64>,
    speed: Option<String>,
    cache_creation: Option<RawCacheCreation>,
}

fn count(value: Option<f64>) -> u64 {
    value.filter(|number| number.is_finite() && *number > 0.0).unwrap_or(0.0) as u64
}

/// The buckets and whether a `speed` was written; none for an unknown speed or missing counts.
fn breakdown(
    input: Option<f64>,
    output: Option<f64>,
    cache_creation: Option<&RawCacheCreation>,
    cache_creation_total: Option<f64>,
    cache_read: Option<f64>,
    speed: Option<&str>,
) -> Option<(Tokens, bool)> {
    let (input, output) = (input?, output?);
    if speed.is_some_and(|speed| speed != "fast" && speed != "standard") {
        return None;
    }
    let (write_5m, write_1h) = match cache_creation {
        Some(split) => (
            count(split.ephemeral_5m_input_tokens),
            count(split.ephemeral_1h_input_tokens),
        ),
        None => (count(cache_creation_total), 0),
    };
    Some((
        Tokens {
            input: count(Some(input)),
            cache_write_5m: write_5m,
            cache_write_1h: write_1h,
            cache_read: count(cache_read),
            output: count(Some(output)),
            fast: speed == Some("fast"),
        },
        speed.is_some(),
    ))
}

pub(super) fn parse_entries(text: &str) -> Vec<Entry> {
    let Ok(line) = serde_json::from_str::<RawLine>(text) else {
        return Vec::new();
    };
    let Some(timestamp) = line
        .timestamp
        .as_deref()
        .and_then(|raw| DateTime::parse_from_rfc3339(raw).ok())
        .map(|stamp| stamp.with_timezone(&Utc))
    else {
        return Vec::new();
    };
    let Some(message) = line.message else {
        return Vec::new();
    };
    let Some(usage) = message.usage else {
        return Vec::new();
    };
    let null_field = line.cwd.is_null()
        || line.cost_usd.is_null()
        || line.version.is_null()
        || line.session_id.is_null()
        || line.request_id.is_null()
        || line.is_api_error.is_null()
        || message.id.is_null()
        || message.model.is_null()
        || usage.speed.is_null()
        || usage.cache_read_input_tokens.is_null()
        || usage.cache_creation_input_tokens.is_null();
    if null_field {
        return Vec::new();
    }
    if line.version.as_ref().is_some_and(|version| !is_semver_prefix(version)) {
        return Vec::new();
    }
    let empty = |slot: &Slot<String>| slot.as_ref().is_some_and(String::is_empty);
    if empty(&line.session_id) || empty(&line.request_id) || empty(&message.id) || empty(&message.model)
    {
        return Vec::new();
    }
    let Some((tokens, has_speed)) = breakdown(
        usage.input_tokens,
        usage.output_tokens,
        usage.cache_creation.as_ref(),
        usage.cache_creation_input_tokens.as_ref().copied(),
        usage.cache_read_input_tokens.as_ref().copied(),
        usage.speed.as_ref().map(String::as_str),
    ) else {
        return Vec::new();
    };
    let message_id = message.id.value();
    let request_id = line.request_id.value();
    let sidechain = line.is_sidechain.unwrap_or(false);
    let parent = Entry {
        timestamp,
        tokens,
        message_id: message_id.clone(),
        request_id: request_id.clone(),
        sidechain,
        has_speed,
        cost_usd: line.cost_usd.value(),
        model: message.model.value().filter(|model| model != "<synthetic>"),
    };
    let mut entries = vec![parent];
    let mut advisor = 0;
    for iteration in usage.iterations.unwrap_or_default() {
        if iteration.kind.as_deref() != Some("advisor_message") {
            continue;
        }
        let Some(model) = iteration.model.filter(|model| !model.is_empty()) else {
            continue;
        };
        let Some((tokens, has_speed)) = breakdown(
            iteration.input_tokens,
            iteration.output_tokens,
            iteration.cache_creation.as_ref(),
            iteration.cache_creation_input_tokens,
            iteration.cache_read_input_tokens,
            iteration.speed.as_deref(),
        ) else {
            continue;
        };
        entries.push(Entry {
            timestamp,
            tokens,
            message_id: message_id.as_ref().map(|id| format!("{id}:advisor:{advisor}")),
            request_id: request_id.clone(),
            sidechain,
            has_speed,
            cost_usd: None,
            model: Some(model),
        });
        advisor += 1;
    }
    entries
}

/// `digits.digits.digit…`: accepts "1.0.24" and pre-release suffixes, rejects "unknown".
pub(super) fn is_semver_prefix(value: &str) -> bool {
    let bytes = value.as_bytes();
    let mut index = 0;
    let digits = |index: &mut usize| {
        let start = *index;
        while *index < bytes.len() && bytes[*index].is_ascii_digit() {
            *index += 1;
        }
        *index > start
    };
    for _ in 0..2 {
        if !digits(&mut index) || bytes.get(index) != Some(&b'.') {
            return false;
        }
        index += 1;
    }
    bytes.get(index).is_some_and(u8::is_ascii_digit)
}
