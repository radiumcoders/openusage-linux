//! Port of `CursorUsageMapper.swift`.

use chrono::{DateTime, Utc};
use serde_json::{Map, Value};

use crate::error::ProviderError;
use crate::metric::{MetricKind, MetricLine, MetricValue, ProgressFormat, ProviderUsage};

use super::parse::{
    as_int, cents_to_dollars, clamp_percent, millis_date, number_at, parse_iso, strict_bool,
    title_case, trimmed_nonempty, MONTH_MS, WEEK_MS,
};

pub(crate) const ORG_MEMBERSHIPS: [&str; 2] = ["enterprise", "team"];

#[derive(Debug, Clone)]
pub(crate) struct PlanFacts {
    pub is_enabled: bool,
    pub has_plan_usage: bool,
    pub limit: Option<f64>,
    pub total_percent_used: Option<f64>,
    pub has_model_pools: bool,
    pub is_team_by_shape: bool,
}

impl PlanFacts {
    pub(crate) fn from_usage(usage: &Map<String, Value>) -> Self {
        let is_enabled = usage.get("enabled").and_then(strict_bool) != Some(false);
        let plan = usage.get("planUsage").and_then(Value::as_object);
        let limit = plan.and_then(|plan| number_at(plan, "limit"));
        let total_percent_used = plan.and_then(|plan| number_at(plan, "totalPercentUsed"));
        let auto = plan.and_then(|plan| number_at(plan, "autoPercentUsed"));
        let api = plan.and_then(|plan| number_at(plan, "apiPercentUsed"));
        let spent = plan
            .and_then(|plan| number_at(plan, "totalSpend"))
            .unwrap_or_else(|| {
                let limit = limit.unwrap_or(0.0);
                let remaining = plan
                    .and_then(|plan| number_at(plan, "remaining"))
                    .unwrap_or(limit);
                limit - remaining
            });
        let has_model_pools = match (auto, api) {
            (Some(auto), Some(api))
                if auto.is_finite() && api.is_finite() && auto >= 0.0 && api >= 0.0 =>
            {
                auto > 0.0 || api > 0.0 || spent == 0.0
            }
            _ => false,
        };
        let spend = usage.get("spendLimitUsage").and_then(Value::as_object);
        let spend_type = spend
            .and_then(|spend| spend.get("limitType"))
            .and_then(Value::as_str)
            .map(|value| value.to_ascii_lowercase());
        let pooled_limit = spend
            .and_then(|spend| number_at(spend, "pooledLimit"))
            .unwrap_or(0.0);
        Self {
            is_enabled,
            has_plan_usage: plan.is_some(),
            limit,
            total_percent_used,
            has_model_pools,
            is_team_by_shape: spend_type.as_deref() == Some("team") || pooled_limit > 0.0,
        }
    }

    pub(crate) fn plan_usage_unusable(&self) -> bool {
        !self.has_plan_usage
            || (self.has_plan_usage && self.limit.is_none() && !self.has_model_pools)
    }

    pub(crate) fn should_try_generic_request_fallback(&self) -> bool {
        self.is_enabled
            && self.has_plan_usage
            && self.limit.is_none()
            && self.total_percent_used.is_none()
            && !self.has_model_pools
    }
}

pub(crate) struct Fallback {
    pub should: bool,
    pub message: String,
}

pub(crate) fn should_use_request_fallback(facts: &PlanFacts, plan_name: Option<&str>) -> Fallback {
    let no = Fallback {
        should: false,
        message: String::new(),
    };
    if !facts.is_enabled {
        return no;
    }
    let normalized = plan_name
        .map(|name| name.trim().to_ascii_lowercase())
        .unwrap_or_default();
    let unusable = facts.plan_usage_unusable() && facts.total_percent_used.is_none();
    if unusable && normalized == "enterprise" {
        return Fallback {
            should: true,
            message: "Enterprise usage data unavailable. Try again later.".into(),
        };
    }
    if unusable && normalized == "team" {
        return Fallback {
            should: true,
            message: "Team request-based usage data unavailable. Try again later.".into(),
        };
    }
    if unusable && normalized.is_empty() && plan_name.is_none() {
        return Fallback {
            should: true,
            message: "Cursor request-based usage data unavailable. Try again later.".into(),
        };
    }
    if facts.is_team_by_shape
        && facts.has_plan_usage
        && facts.limit.is_none()
        && !facts.has_model_pools
    {
        return Fallback {
            should: true,
            message: "Cursor request-based usage data unavailable. Try again later.".into(),
        };
    }
    no
}

pub(crate) fn normalized_membership(value: Option<&str>) -> Option<String> {
    value
        .and_then(trimmed_nonempty)
        .map(|value| value.to_ascii_lowercase())
}

pub(crate) fn membership_type(summary: &Map<String, Value>) -> Option<String> {
    normalized_membership(summary.get("membershipType").and_then(Value::as_str))
}

/// Badge plan. Prefer an org billing membership over GetPlanInfo when that still says Pro.
pub(crate) fn resolved_plan_name(
    plan_info_name: Option<&str>,
    membership_type: Option<&str>,
) -> Option<String> {
    let membership = normalized_membership(membership_type);
    let plan_info = plan_info_name.and_then(trimmed_nonempty);
    if let Some(membership) = membership.as_deref() {
        let plan_lower = plan_info.unwrap_or("").to_ascii_lowercase();
        if ORG_MEMBERSHIPS.contains(&membership)
            && (plan_info.is_none() || plan_lower == "pro" || plan_lower == "pro plan")
        {
            return Some(membership.to_string());
        }
    }
    plan_info.map(str::to_string).or(membership)
}

pub(crate) fn resolved_plan_label(
    plan_info_name: Option<&str>,
    membership_type: Option<&str>,
) -> Option<String> {
    plan_label(resolved_plan_name(plan_info_name, membership_type).as_deref())
}

pub(crate) fn plan_label(value: Option<&str>) -> Option<String> {
    let trimmed = value.and_then(trimmed_nonempty)?;
    Some(title_case(trimmed, char::is_whitespace, false))
}

pub(crate) fn stripe_balance_cents(body: Option<&Map<String, Value>>) -> f64 {
    let Some(balance) = body.and_then(|body| number_at(body, "customerBalance")) else {
        return 0.0;
    };
    if balance < 0.0 {
        balance.abs()
    } else {
        0.0
    }
}

struct Cycle {
    resets_at: Option<DateTime<Utc>>,
    period_ms: i64,
}

fn billing_cycle(usage: &Map<String, Value>) -> Cycle {
    let start = number_at(usage, "billingCycleStart");
    let end = number_at(usage, "billingCycleEnd");
    if let (Some(start), Some(end)) = (start, end) {
        if end > start {
            if let Some(period) = as_int(end - start) {
                return Cycle {
                    resets_at: millis_date(end),
                    period_ms: period,
                };
            }
        }
    }
    Cycle {
        resets_at: end.and_then(millis_date),
        period_ms: MONTH_MS,
    }
}

fn percent(label: &str, used: f64, cycle: &Cycle) -> MetricLine {
    MetricLine::percent(label, used, cycle.resets_at, Some(cycle.period_ms))
}

fn on_demand_spend(spend: &Map<String, Value>, limit: f64, remaining: f64) -> f64 {
    let reported: Vec<f64> = ["individualUsed", "pooledUsed", "totalSpend"]
        .into_iter()
        .filter_map(|key| number_at(spend, key))
        .collect();
    if let Some(positive) = reported.iter().copied().find(|value| *value > 0.0) {
        return positive;
    }
    let inferred = (limit - remaining).max(0.0);
    if inferred > 0.0 {
        inferred
    } else {
        reported.first().copied().unwrap_or(0.0)
    }
}

fn append_credits(
    lines: &mut Vec<MetricLine>,
    grants: Option<&Map<String, Value>>,
    stripe_cents: f64,
) {
    let has_grants = grants
        .and_then(|grants| grants.get("hasCreditGrants"))
        .and_then(strict_bool)
        == Some(true);
    let total = if has_grants {
        grants
            .and_then(|grants| number_at(grants, "totalCents"))
            .unwrap_or(0.0)
    } else {
        0.0
    };
    let used = if has_grants {
        grants
            .and_then(|grants| number_at(grants, "usedCents"))
            .unwrap_or(0.0)
    } else {
        0.0
    };
    let valid = has_grants && total > 0.0;
    let combined = (if valid { total } else { 0.0 }) + stripe_cents;
    if combined <= 0.0 {
        return;
    }
    let remaining = (combined - if valid { used } else { 0.0 }).max(0.0);
    lines.push(MetricLine::values(
        "Credits",
        vec![MetricValue::new(
            cents_to_dollars(remaining),
            MetricKind::Dollars,
        )],
    ));
}

pub(crate) fn map_usage(
    usage: &Map<String, Value>,
    plan_name: Option<&str>,
    credit_grants: Option<&Map<String, Value>>,
    stripe_balance_cents: f64,
) -> Result<ProviderUsage, ProviderError> {
    let facts = PlanFacts::from_usage(usage);
    let Some(plan_usage) = usage
        .get("planUsage")
        .and_then(Value::as_object)
        .filter(|_| facts.is_enabled)
    else {
        return Err(ProviderError::Unavailable(
            "No active Cursor subscription.".into(),
        ));
    };
    if facts.limit.is_none() && facts.total_percent_used.is_none() && !facts.has_model_pools {
        return Err(ProviderError::Unavailable(
            "Total usage limit missing from API response.".into(),
        ));
    }

    let mut lines = Vec::new();
    append_credits(&mut lines, credit_grants, stripe_balance_cents);

    let plan_used_cents = number_at(plan_usage, "totalSpend").unwrap_or_else(|| {
        (facts.limit.unwrap_or(0.0)) - number_at(plan_usage, "remaining").unwrap_or(0.0)
    });
    let computed = facts
        .limit
        .and_then(|limit| (limit > 0.0).then_some(plan_used_cents / limit * 100.0))
        .unwrap_or(0.0);
    let total_percent = facts.total_percent_used.unwrap_or(computed);
    let cycle = billing_cycle(usage);
    let normalized = plan_name
        .map(|name| name.trim().to_ascii_lowercase())
        .unwrap_or_default();
    let is_team = normalized == "team" || facts.is_team_by_shape;

    if is_team && facts.has_model_pools {
        // Structured pool data wins; never synthesize a total from stale dollars when totalPercentUsed is absent.
        if let Some(percent_used) = facts.total_percent_used {
            lines.push(percent("Total usage", percent_used, &cycle));
        }
    } else if is_team {
        if let Some(limit_cents) = facts.limit {
            lines.push(MetricLine::Progress {
                label: "Total usage".into(),
                used: cents_to_dollars(plan_used_cents),
                limit: cents_to_dollars(limit_cents),
                format: ProgressFormat::Dollars,
                resets_at: cycle.resets_at,
                period_duration_ms: Some(cycle.period_ms),
            });
        } else if let Some(percent_used) = facts.total_percent_used {
            lines.push(percent("Total usage", percent_used, &cycle));
        } else {
            return Err(ProviderError::Unavailable(
                "Cursor request-based usage data unavailable. Try again later.".into(),
            ));
        }
    } else {
        lines.push(percent("Total usage", total_percent, &cycle));
    }

    if let Some(auto) = number_at(plan_usage, "autoPercentUsed") {
        lines.push(percent("Cursor Models", auto, &cycle));
    }
    if let Some(api) = number_at(plan_usage, "apiPercentUsed") {
        lines.push(percent("Other Models", api, &cycle));
    }

    if let Some(spend) = usage.get("spendLimitUsage").and_then(Value::as_object) {
        let limit = number_at(spend, "individualLimit")
            .or_else(|| number_at(spend, "pooledLimit"))
            .unwrap_or(0.0);
        let remaining = number_at(spend, "individualRemaining")
            .or_else(|| number_at(spend, "pooledRemaining"))
            .unwrap_or(0.0);
        let spent = on_demand_spend(spend, limit, remaining);
        if limit > 0.0 {
            lines.push(MetricLine::Progress {
                label: "On-demand".into(),
                used: cents_to_dollars(spent),
                limit: cents_to_dollars(limit),
                format: ProgressFormat::Dollars,
                resets_at: cycle.resets_at,
                period_duration_ms: Some(cycle.period_ms),
            });
        } else if spent > 0.0 {
            lines.push(MetricLine::values(
                "On-demand",
                vec![MetricValue::new(
                    cents_to_dollars(spent),
                    MetricKind::Dollars,
                )],
            ));
        }
    }

    Ok(ProviderUsage {
        plan: plan_label(plan_name),
        lines,
        notice: None,
    })
}

/// Grok Bot is Cursor's "Sand" product. Pooled enterprise accounts and accounts without an allowance have no meter.
pub(crate) fn map_grok(usage: &Map<String, Value>) -> Option<MetricLine> {
    if usage
        .get("usesPooledEnterpriseAllowance")
        .and_then(strict_bool)
        == Some(true)
    {
        return None;
    }
    if usage.get("hasNonZeroIncludedLimit").and_then(strict_bool) == Some(false) {
        return None;
    }
    if usage.get("includedLimitZero").and_then(strict_bool) == Some(true) {
        return None;
    }
    let percent_used = number_at(usage, "usagePercent")?;
    if percent_used < 0.0 {
        return None;
    }
    let reset = usage
        .get("nextResetTimestampUtc")
        .and_then(Value::as_str)
        .and_then(parse_iso);
    let start = usage
        .get("currentPeriodStart")
        .and_then(Value::as_str)
        .and_then(parse_iso);
    let period_ms = match (start, reset) {
        (Some(start), Some(reset)) if reset > start => {
            reset.timestamp_millis() - start.timestamp_millis()
        }
        _ => WEEK_MS,
    };
    Some(MetricLine::percent(
        "Grok Bot usage",
        clamp_percent(percent_used),
        reset,
        Some(period_ms),
    ))
}
