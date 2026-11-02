//! Port of `AntigravityUsageMapper.swift`.

use chrono::{DateTime, NaiveDate, Utc};
use serde::Deserialize;
use serde_json::Value;

use crate::log;
use crate::metric::MetricLine;

const SESSION_MS: i64 = 5 * 60 * 60 * 1000;
const WEEK_MS: i64 = 7 * 24 * 60 * 60 * 1000;
const ARGON: &str = "Gemini 4 Argon";

struct Spec {
    id: &'static str,
    label: &'static str,
    period_ms: i64,
}

const SUMMARY: [Spec; 4] = [
    Spec {
        id: "gemini-5h",
        label: "Session",
        period_ms: SESSION_MS,
    },
    Spec {
        id: "gemini-weekly",
        label: "Weekly",
        period_ms: WEEK_MS,
    },
    Spec {
        id: "3p-5h",
        label: "Claude",
        period_ms: SESSION_MS,
    },
    Spec {
        id: "3p-weekly",
        label: "Claude Weekly",
        period_ms: WEEK_MS,
    },
];

struct Group {
    display: Option<String>,
    buckets: Vec<Bucket>,
}

struct Bucket {
    id: Option<String>,
    fraction: Option<f64>,
    reset: Option<String>,
    window: Option<String>,
    disabled: Option<bool>,
    invalid_disabled: bool,
}

pub(crate) fn parse_quota_summaries(bodies: &[Vec<u8>]) -> Option<Vec<MetricLine>> {
    let mut group_lists = Vec::new();
    for body in bodies {
        match summary_groups(body) {
            Some(groups) => group_lists.push(groups),
            None => log::warn(
                "plugin:antigravity",
                "quota summary response has no decodable groups; treating as not-a-summary",
            ),
        }
    }
    if group_lists.is_empty() {
        return None;
    }
    let mut pooled: Vec<(String, f64, Option<DateTime<Utc>>)> = Vec::new();
    for bucket in group_lists
        .iter()
        .flatten()
        .filter(|group| group.display.as_deref() != Some(ARGON))
        .flat_map(|group| &group.buckets)
    {
        let Some(id) = bucket.id.as_deref() else {
            log::warn(
                "plugin:antigravity",
                "quota summary: skipping unrecognized bucket id '<absent>'",
            );
            continue;
        };
        if !SUMMARY.iter().any(|spec| spec.id == id) {
            log::warn(
                "plugin:antigravity",
                &format!("quota summary: skipping unrecognized bucket id '{id}'"),
            );
            continue;
        }
        if pooled.iter().any(|(known, _, _)| known == id) {
            continue;
        }
        let Some(fraction) = bucket.fraction.filter(|fraction| fraction.is_finite()) else {
            log::warn("plugin:antigravity", &format!("quota summary: bucket '{id}' has no usable remainingFraction; dropping its line"));
            continue;
        };
        pooled.push((
            id.to_string(),
            fraction,
            bucket.reset.as_deref().and_then(parse_time),
        ));
    }
    let mut lines = Vec::new();
    for spec in SUMMARY {
        if let Some((_, fraction, reset)) = pooled.iter().find(|(id, _, _)| id == spec.id) {
            lines.push(line(spec.label, *fraction, *reset, spec.period_ms));
        }
    }
    if let Some(argon) = group_lists
        .iter()
        .find_map(|groups| argon_weekly_line(groups))
    {
        let index = lines
            .iter()
            .position(|line| line.label() == "Claude" || line.label() == "Claude Weekly")
            .unwrap_or(lines.len());
        lines.insert(index, argon);
    }
    Some(lines)
}

fn argon_weekly_line(groups: &[Group]) -> Option<MetricLine> {
    let argon: Vec<&Group> = groups
        .iter()
        .filter(|group| group.display.as_deref() == Some(ARGON))
        .collect();
    if argon.is_empty() {
        return None;
    }
    let buckets: Vec<&Bucket> = argon
        .iter()
        .flat_map(|group| group.buckets.iter())
        .collect();
    for bucket in &buckets {
        if bucket.window.as_deref() != Some("weekly") {
            log::warn(
                "plugin:antigravity",
                &format!(
                    "Argon quota has unsupported window '{}'; skipping its bucket",
                    bucket.window.as_deref().unwrap_or("<absent>")
                ),
            );
        }
    }
    let weekly: Vec<&Bucket> = buckets
        .iter()
        .copied()
        .filter(|bucket| bucket.window.as_deref() == Some("weekly"))
        .collect();
    let Some(bucket) = (weekly.len() == 1).then(|| weekly[0]) else {
        log::warn(
            "plugin:antigravity",
            "Argon quota group has no unambiguous separate weekly bucket; dropping its line",
        );
        return None;
    };
    let Some(id) = bucket.id.as_deref().filter(|id| !id.is_empty()) else {
        log::warn(
            "plugin:antigravity",
            "Argon quota group has no unambiguous separate weekly bucket; dropping its line",
        );
        return None;
    };
    if SUMMARY.iter().any(|spec| spec.id == id) {
        log::warn(
            "plugin:antigravity",
            "Argon quota group has no unambiguous separate weekly bucket; dropping its line",
        );
        return None;
    }
    if bucket.invalid_disabled {
        log::warn(
            "plugin:antigravity",
            "Argon weekly quota has an unusable disabled flag; dropping its line",
        );
        return None;
    }
    if bucket.disabled == Some(true) {
        return None;
    }
    let Some(fraction) = bucket.fraction.filter(|fraction| fraction.is_finite()) else {
        log::warn(
            "plugin:antigravity",
            "Argon weekly quota has no usable remainingFraction; dropping its line",
        );
        return None;
    };
    Some(line(
        "Argon Weekly",
        fraction,
        bucket.reset.as_deref().and_then(parse_time),
        WEEK_MS,
    ))
}

pub(crate) fn is_complete_summary(body: &[u8]) -> bool {
    let Some(groups) = summary_groups(body) else {
        return false;
    };
    let mut usable = Vec::new();
    for bucket in groups
        .iter()
        .filter(|group| group.display.as_deref() != Some(ARGON))
        .flat_map(|group| &group.buckets)
    {
        if bucket.fraction.is_some_and(|fraction| fraction.is_finite()) {
            if let Some(id) = &bucket.id {
                usable.push(id.clone());
            }
        }
    }
    SUMMARY
        .iter()
        .all(|spec| usable.iter().any(|id| id == spec.id))
}

fn summary_groups(body: &[u8]) -> Option<Vec<Group>> {
    let value: Value = serde_json::from_slice(body).ok()?;
    let object = value.as_object()?;
    if let Some(response) = object.get("response") {
        if !response.is_null() {
            let response = response.as_object()?;
            if let Some(groups) = response.get("groups").filter(|groups| !groups.is_null()) {
                return parse_groups(groups);
            }
        }
    }
    match object.get("groups").filter(|groups| !groups.is_null()) {
        Some(groups) => parse_groups(groups),
        None => None,
    }
}

fn parse_groups(value: &Value) -> Option<Vec<Group>> {
    value.as_array()?.iter().map(parse_group).collect()
}

fn parse_group(value: &Value) -> Option<Group> {
    let object = value.as_object()?;
    let display = match object.get("displayName") {
        Some(Value::String(text)) => Some(text.clone()),
        _ => None,
    };
    let buckets = match object.get("buckets") {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(items)) => items.iter().map(parse_bucket).collect(),
        Some(_) => return None,
    };
    Some(Group { display, buckets })
}

fn parse_bucket(value: &Value) -> Bucket {
    let Some(object) = value.as_object() else {
        return Bucket {
            id: None,
            fraction: None,
            reset: None,
            window: None,
            disabled: None,
            invalid_disabled: false,
        };
    };
    let (disabled, invalid_disabled) = match object.get("disabled") {
        None | Some(Value::Null) => (None, false),
        Some(Value::Bool(flag)) => (Some(*flag), false),
        Some(_) => (None, true),
    };
    Bucket {
        id: object
            .get("bucketId")
            .and_then(Value::as_str)
            .map(str::to_string),
        fraction: object
            .get("remainingFraction")
            .and_then(Value::as_f64)
            .filter(|fraction| fraction.is_finite()),
        reset: object
            .get("resetTime")
            .and_then(Value::as_str)
            .map(str::to_string),
        window: object
            .get("window")
            .and_then(Value::as_str)
            .map(str::to_string),
        disabled,
        invalid_disabled,
    }
}

pub(crate) fn line(
    label: &str,
    fraction: f64,
    reset: Option<DateTime<Utc>>,
    period_ms: i64,
) -> MetricLine {
    let clamped = fraction.clamp(0.0, 1.0);
    let used = ((1.0 - clamped) * 100.0).round();
    MetricLine::percent(label, used, reset, Some(period_ms))
}

pub(crate) fn parse_plan(body: &[u8]) -> Option<String> {
    let envelope: LoadEnvelope = serde_json::from_slice(body).ok()?;
    if let Some(id) = envelope
        .paid_tier
        .as_ref()
        .and_then(|tier| tier.id.as_deref())
    {
        if let Some(plan) = paid_plan(id) {
            return Some(plan.to_string());
        }
    }
    format_plan(
        envelope
            .paid_tier
            .as_ref()
            .and_then(|tier| tier.name.as_deref())
            .or(envelope
                .current_tier
                .as_ref()
                .and_then(|tier| tier.name.as_deref())),
    )
}

pub(crate) fn parse_project(body: &[u8]) -> Option<String> {
    let envelope: LoadEnvelope = serde_json::from_slice(body).ok()?;
    envelope.project.filter(|project| !project.is_empty())
}

fn paid_plan(id: &str) -> Option<&'static str> {
    match id {
        "g1-pro-tier" => Some("Google AI Pro"),
        "g1-ultra-tier" => Some("Google AI Ultra"),
        "g1-ultra-lite-tier" => Some("Google AI Ultra Lite"),
        _ => None,
    }
}

fn format_plan(raw: Option<&str>) -> Option<String> {
    let trimmed = raw.map(str::trim).filter(|value| !value.is_empty())?;
    let lower = trimmed.to_lowercase();
    for tier in ["Ultra Lite", "Ultra", "Pro"] {
        if lower.contains(&tier.to_lowercase()) {
            return Some(format!("Google AI {tier}"));
        }
    }
    Some(
        trimmed
            .split_whitespace()
            .map(|word| {
                let mut chars = word.chars();
                let head: String = chars
                    .next()
                    .map(|character| character.to_uppercase().collect())
                    .unwrap_or_default();
                head + &chars.as_str().to_lowercase()
            })
            .collect::<Vec<_>>()
            .join(" "),
    )
}

fn parse_time(value: &str) -> Option<DateTime<Utc>> {
    let mut text = value.trim().to_string();
    if let Some(stripped) = text.strip_suffix(" UTC") {
        text = format!("{stripped}Z");
    }
    if text.contains(' ') && !text.contains('T') {
        text = text.replacen(' ', "T", 1);
    }
    if let Ok(date) = DateTime::parse_from_rfc3339(&text) {
        return Some(date.with_timezone(&Utc));
    }
    NaiveDate::parse_from_str(value.trim(), "%Y-%m-%d")
        .ok()
        .and_then(|date| date.and_hms_opt(0, 0, 0))
        .map(|date| date.and_utc())
}

#[derive(Deserialize)]
struct LoadEnvelope {
    #[serde(default, rename = "cloudaicompanionProject")]
    project: Option<String>,
    #[serde(default, rename = "currentTier")]
    current_tier: Option<Tier>,
    #[serde(default, rename = "paidTier")]
    paid_tier: Option<Tier>,
}

#[derive(Deserialize)]
struct Tier {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    name: Option<String>,
}
