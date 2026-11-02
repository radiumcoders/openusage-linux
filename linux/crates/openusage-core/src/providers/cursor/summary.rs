//! Port of `CursorUsageSummaryMapper.swift`.

use chrono::{DateTime, Duration, Utc};
use serde_json::{Map, Value};

use crate::error::ProviderError;
use crate::metric::{MetricKind, MetricLine, MetricValue, ProgressFormat, ProviderUsage};

use super::mapper::resolved_plan_label;
use super::parse::{cents_to_dollars, number_at, parse_iso, strict_bool, MONTH_MS};

struct Cycle {
    resets_at: Option<DateTime<Utc>>,
    period_ms: i64,
}

pub(crate) fn has_usable_summary(summary: &Map<String, Value>) -> bool {
    let start = text(summary, "billingCycleStart").and_then(parse_iso);
    let end = text(summary, "billingCycleEnd").and_then(parse_iso);
    if matches!((start, end), (Some(start), Some(end)) if end > start) {
        return true;
    }
    let plan = individual(summary)
        .and_then(|usage| usage.get("plan"))
        .and_then(Value::as_object);
    if ["totalPercentUsed", "autoPercentUsed", "apiPercentUsed"]
        .iter()
        .any(|key| plan.and_then(|plan| number_at(plan, key)).is_some())
    {
        return true;
    }
    let team = team(summary);
    [
        individual(summary).and_then(|usage| usage.get("onDemand")),
        individual(summary).and_then(|usage| usage.get("overall")),
        team.and_then(|usage| usage.get("onDemand")),
        team.and_then(|usage| usage.get("pooled")),
    ]
    .into_iter()
    .any(|bucket| {
        bucket
            .and_then(Value::as_object)
            .is_some_and(usable_dollar_bucket)
    })
}

pub(crate) fn map(
    summary: Option<&Map<String, Value>>,
    request_usage: Option<&Map<String, Value>>,
    plan_name: Option<&str>,
    unavailable_message: &str,
) -> Result<ProviderUsage, ProviderError> {
    let cycle = billing_cycle(summary, request_usage);
    let mut lines = Vec::new();
    if !append_requests(request_usage, &cycle, &mut lines) {
        append_summary_total(summary, &cycle, &mut lines);
    }
    append_structured_percentages(summary, &cycle, &mut lines);
    append_on_demand(summary, &cycle, &mut lines);
    if lines.is_empty() {
        return Err(ProviderError::Unavailable(unavailable_message.to_string()));
    }
    let membership = summary
        .and_then(|summary| summary.get("membershipType"))
        .and_then(Value::as_str);
    Ok(ProviderUsage {
        plan: resolved_plan_label(plan_name, membership),
        lines,
        notice: None,
    })
}

pub(crate) fn append_missing_on_demand(summary: &Map<String, Value>, lines: &mut Vec<MetricLine>) {
    if lines.iter().any(|line| line.label() == "On-demand") {
        return;
    }
    append_on_demand(Some(summary), &billing_cycle(Some(summary), None), lines);
}

fn append_requests(
    usage: Option<&Map<String, Value>>,
    cycle: &Cycle,
    lines: &mut Vec<MetricLine>,
) -> bool {
    let Some(requests) = usage
        .and_then(|usage| usage.get("gpt-4"))
        .and_then(Value::as_object)
    else {
        return false;
    };
    let Some(limit) = number_at(requests, "maxRequestUsage").filter(|limit| *limit > 0.0) else {
        return false;
    };
    let used = number_at(requests, "numRequests")
        .or_else(|| number_at(requests, "numRequestsTotal"))
        .unwrap_or(0.0)
        .max(0.0);
    for label in ["Total usage", "Requests"] {
        lines.push(MetricLine::Progress {
            label: label.into(),
            used,
            limit,
            format: ProgressFormat::Count {
                suffix: "requests".into(),
            },
            resets_at: cycle.resets_at,
            period_duration_ms: Some(cycle.period_ms),
        });
    }
    true
}

fn append_summary_total(
    summary: Option<&Map<String, Value>>,
    cycle: &Cycle,
    lines: &mut Vec<MetricLine>,
) {
    let Some(summary) = summary else { return };
    let limit_type = summary
        .get("limitType")
        .and_then(Value::as_str)
        .map(|value| value.to_ascii_lowercase());
    if limit_type.as_deref() == Some("team") {
        if let Some(meter) = team(summary)
            .and_then(|usage| usage.get("pooled"))
            .and_then(|value| dollar_meter(value))
        {
            append_dollars(meter, "Total usage", cycle, lines);
            return;
        }
    }
    if let Some(percent) = individual(summary)
        .and_then(|usage| usage.get("plan"))
        .and_then(Value::as_object)
        .and_then(|plan| number_at(plan, "totalPercentUsed"))
    {
        lines.push(MetricLine::percent(
            "Total usage",
            percent,
            cycle.resets_at,
            Some(cycle.period_ms),
        ));
        return;
    }
    if let Some(meter) = individual(summary)
        .and_then(|usage| usage.get("overall"))
        .and_then(|value| dollar_meter(value))
    {
        append_dollars(meter, "Total usage", cycle, lines);
        return;
    }
    if let Some(meter) = team(summary)
        .and_then(|usage| usage.get("pooled"))
        .and_then(|value| dollar_meter(value))
    {
        append_dollars(meter, "Total usage", cycle, lines);
    }
}

fn append_structured_percentages(
    summary: Option<&Map<String, Value>>,
    cycle: &Cycle,
    lines: &mut Vec<MetricLine>,
) {
    let Some(plan) = summary
        .and_then(individual)
        .and_then(|usage| usage.get("plan"))
        .and_then(Value::as_object)
    else {
        return;
    };
    for (key, label) in [
        ("autoPercentUsed", "Cursor Models"),
        ("apiPercentUsed", "Other Models"),
    ] {
        if let Some(percent) = number_at(plan, key) {
            lines.push(MetricLine::percent(
                label,
                percent,
                cycle.resets_at,
                Some(cycle.period_ms),
            ));
        }
    }
}

fn append_on_demand(
    summary: Option<&Map<String, Value>>,
    cycle: &Cycle,
    lines: &mut Vec<MetricLine>,
) {
    let Some(summary) = summary else { return };
    if append_on_demand_bucket(
        individual(summary).and_then(|usage| usage.get("onDemand")),
        cycle,
        lines,
    ) {
        return;
    }
    append_on_demand_bucket(
        team(summary).and_then(|usage| usage.get("onDemand")),
        cycle,
        lines,
    );
}

/// An Enterprise response can include a disabled individual placeholder beside a valid team bucket.
fn append_on_demand_bucket(
    value: Option<&Value>,
    cycle: &Cycle,
    lines: &mut Vec<MetricLine>,
) -> bool {
    let Some(bucket) = value.and_then(Value::as_object) else {
        return false;
    };
    if bucket.get("enabled").and_then(strict_bool) == Some(false) {
        return false;
    }
    if let Some(meter) = value.and_then(dollar_meter) {
        append_dollars(meter, "On-demand", cycle, lines);
        return true;
    }
    if let Some(used) = number_at(bucket, "used").filter(|used| *used > 0.0) {
        lines.push(MetricLine::values(
            "On-demand",
            vec![MetricValue::new(
                cents_to_dollars(used),
                MetricKind::Dollars,
            )],
        ));
        return true;
    }
    false
}

fn dollar_meter(value: &Value) -> Option<(f64, f64)> {
    let bucket = value.as_object()?;
    if bucket.get("enabled").and_then(strict_bool) == Some(false) {
        return None;
    }
    let limit = number_at(bucket, "limit").filter(|limit| *limit > 0.0)?;
    let reported = number_at(bucket, "used");
    let inferred = (limit - number_at(bucket, "remaining").unwrap_or(limit)).max(0.0);
    let used = reported
        .filter(|used| *used > 0.0)
        .unwrap_or(inferred)
        .max(0.0);
    Some((used, limit))
}

fn usable_dollar_bucket(bucket: &Map<String, Value>) -> bool {
    if bucket.get("enabled").and_then(strict_bool) == Some(false) {
        return false;
    }
    dollar_meter(&Value::Object(bucket.clone())).is_some()
        || number_at(bucket, "used").unwrap_or(0.0) > 0.0
}

fn append_dollars(meter: (f64, f64), label: &str, cycle: &Cycle, lines: &mut Vec<MetricLine>) {
    lines.push(MetricLine::Progress {
        label: label.into(),
        used: cents_to_dollars(meter.0),
        limit: cents_to_dollars(meter.1),
        format: ProgressFormat::Dollars,
        resets_at: cycle.resets_at,
        period_duration_ms: Some(cycle.period_ms),
    });
}

fn billing_cycle(
    summary: Option<&Map<String, Value>>,
    request_usage: Option<&Map<String, Value>>,
) -> Cycle {
    let start = summary
        .and_then(|summary| text(summary, "billingCycleStart"))
        .and_then(parse_iso);
    let end = summary
        .and_then(|summary| text(summary, "billingCycleEnd"))
        .and_then(parse_iso);
    if let (Some(start), Some(end)) = (start, end) {
        if end > start {
            return Cycle {
                resets_at: Some(end),
                period_ms: end.timestamp_millis() - start.timestamp_millis(),
            };
        }
    }
    let request_start = request_usage
        .and_then(|usage| text(usage, "startOfMonth"))
        .and_then(parse_iso);
    Cycle {
        resets_at: request_start.map(|start| start + Duration::milliseconds(MONTH_MS)),
        period_ms: MONTH_MS,
    }
}

fn individual(summary: &Map<String, Value>) -> Option<&Map<String, Value>> {
    summary.get("individualUsage").and_then(Value::as_object)
}

fn team(summary: &Map<String, Value>) -> Option<&Map<String, Value>> {
    summary.get("teamUsage").and_then(Value::as_object)
}

fn text<'a>(object: &'a Map<String, Value>, key: &str) -> Option<&'a str> {
    object.get(key).and_then(Value::as_str)
}
