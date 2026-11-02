//! Port of `OpenRouterUsageMapper`. Limits and budgets reset at midnight UTC; weeks start on Monday.

use std::collections::BTreeMap;

use chrono::{DateTime, Datelike, Duration, TimeZone, Utc};
use serde_json::{Map, Value};

use crate::error::ProviderError;
use crate::metric::{MetricKind, MetricLine, MetricValue, ProgressFormat};

#[cfg(test)]
pub const DAY_MS: i64 = 24 * 60 * 60 * 1000;

#[derive(Clone, Debug, PartialEq)]
pub struct WindowSpend {
    pub spend: f64,
    pub byok: Option<f64>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Spend {
    pub today: WindowSpend,
    pub week: WindowSpend,
    pub month: WindowSpend,
}

impl Spend {
    pub fn zero() -> Self {
        let window = WindowSpend {
            spend: 0.0,
            byok: Some(0.0),
        };
        Self {
            today: window.clone(),
            week: window.clone(),
            month: window,
        }
    }

    pub fn add(&mut self, other: &Spend) {
        self.today.add(&other.today);
        self.week.add(&other.week);
        self.month.add(&other.month);
    }
}

impl WindowSpend {
    fn add(&mut self, other: &WindowSpend) {
        self.spend += other.spend;
        if let Some(byok) = other.byok {
            *self.byok.get_or_insert(0.0) += byok;
        }
    }
}

pub struct KeyInfo {
    pub management: bool,
    pub plan: Option<String>,
    pub lines: Vec<MetricLine>,
    pub spend: Option<Spend>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ActivityDay {
    pub totals: BTreeMap<String, f64>,
    pub by_model: BTreeMap<String, BTreeMap<String, f64>>,
}

pub fn key(body: &[u8], now: DateTime<Utc>) -> Result<KeyInfo, ProviderError> {
    let data = data_object(body)?;
    let Some(management) = data.get("is_management_key").and_then(Value::as_bool) else {
        return Err(ProviderError::InvalidResponse);
    };
    let plan = data
        .get("is_free_tier")
        .and_then(Value::as_bool)
        .map(|free| if free { "Free" } else { "Pay-as-you-go" }.to_string());
    let lines = [
        key_limit(&data, now)?,
        budget_line(&data, now)?,
        free_requests(&data, now)?,
    ]
    .into_iter()
    .flatten()
    .collect();
    let spend = if management {
        None
    } else {
        Some(spend_of(&data)?)
    };
    Ok(KeyInfo {
        management,
        plan,
        lines,
        spend,
    })
}

pub fn credits_lines(body: &[u8]) -> Result<Vec<MetricLine>, ProviderError> {
    let data = data_object(body)?;
    let total_credits = number_at(&data, "total_credits").ok_or(ProviderError::InvalidResponse)?;
    let total_usage = number_at(&data, "total_usage").ok_or(ProviderError::InvalidResponse)?;
    let balance = (total_credits.max(0.0) - total_usage.max(0.0)).max(0.0);
    Ok(vec![MetricLine::values(
        "Balance",
        vec![MetricValue::new(balance, MetricKind::Dollars)],
    )])
}

pub fn keys_page(body: &[u8]) -> Result<Vec<Spend>, ProviderError> {
    let root: Value = serde_json::from_slice(body).map_err(|_| ProviderError::InvalidResponse)?;
    let Some(keys) = root.get("data").and_then(Value::as_array) else {
        return Err(ProviderError::InvalidResponse);
    };
    keys.iter()
        .map(|key| {
            key.as_object()
                .ok_or(ProviderError::InvalidResponse)
                .and_then(spend_of)
        })
        .collect()
}

/// Rows summed per UTC day. `None` when any row is malformed, so a read is never partly used.
pub fn activity_days(body: &[u8]) -> Option<BTreeMap<i64, ActivityDay>> {
    let root: Value = serde_json::from_slice(body).ok()?;
    let rows = root.get("data")?.as_array()?;
    let mut days = BTreeMap::new();
    for row in rows {
        let row = row.as_object()?;
        let date = row.get("date").and_then(Value::as_str)?;
        let instant = DateTime::parse_from_rfc3339(&format!("{date}T00:00:00Z"))
            .ok()?
            .with_timezone(&Utc);
        let model = row.get("model").and_then(Value::as_str)?;
        let totals = activity_totals(row)?;
        let day = days
            .entry(instant.timestamp())
            .or_insert_with(|| ActivityDay {
                totals: quiet(),
                by_model: BTreeMap::new(),
            });
        add(&mut day.totals, &totals);
        add(day.by_model.entry(model.to_string()).or_default(), &totals);
    }
    Some(days)
}

pub fn quiet() -> BTreeMap<String, f64> {
    [
        "spend",
        "byok",
        "tokens",
        "input",
        "cache",
        "output",
        "reasoning",
    ]
    .into_iter()
    .map(|key| (key.to_string(), 0.0))
    .collect()
}

fn spend_of(key: &Map<String, Value>) -> Result<Spend, ProviderError> {
    Ok(Spend {
        today: window_spend(key, "daily")?,
        week: window_spend(key, "weekly")?,
        month: window_spend(key, "monthly")?,
    })
}

fn window_spend(key: &Map<String, Value>, period: &str) -> Result<WindowSpend, ProviderError> {
    let spend = key
        .get(&format!("usage_{period}"))
        .and_then(number)
        .ok_or(ProviderError::InvalidResponse)?
        .max(0.0);
    let byok = key
        .get(&format!("byok_usage_{period}"))
        .and_then(number)
        .map(|value| value.max(0.0));
    Ok(WindowSpend { spend, byok })
}

fn key_limit(
    key: &Map<String, Value>,
    now: DateTime<Utc>,
) -> Result<Option<MetricLine>, ProviderError> {
    let Some(limit) = key
        .get("limit")
        .and_then(number)
        .filter(|limit| *limit > 0.0)
    else {
        return Ok(None);
    };
    let Some(remaining) = key.get("limit_remaining").and_then(number) else {
        return Err(ProviderError::InvalidResponse);
    };
    Ok(Some(paced(
        "Key Limit",
        (limit - remaining.max(0.0)).max(0.0),
        limit,
        ProgressFormat::Dollars,
        key.get("limit_reset").and_then(Value::as_str),
        now,
    )))
}

fn budget_line(
    key: &Map<String, Value>,
    now: DateTime<Utc>,
) -> Result<Option<MetricLine>, ProviderError> {
    let Some(budget) = key.get("effective_budget").and_then(Value::as_object) else {
        return Ok(None);
    };
    let Some(limit) = budget
        .get("limit_usd")
        .and_then(number)
        .filter(|limit| *limit > 0.0)
    else {
        return Ok(None);
    };
    let Some(spend) = budget.get("spend_usd").and_then(number) else {
        return Err(ProviderError::InvalidResponse);
    };
    Ok(Some(paced(
        "Budget",
        spend.max(0.0),
        limit,
        ProgressFormat::Dollars,
        budget.get("reset_interval").and_then(Value::as_str),
        now,
    )))
}

fn free_requests(
    key: &Map<String, Value>,
    now: DateTime<Utc>,
) -> Result<Option<MetricLine>, ProviderError> {
    let Some(free) = key
        .get("free_model_daily_requests")
        .and_then(Value::as_object)
    else {
        return Ok(None);
    };
    let Some(limit) = free
        .get("limit")
        .and_then(number)
        .filter(|limit| *limit > 0.0)
    else {
        return Ok(None);
    };
    let Some(used) = free.get("used").and_then(number) else {
        return Err(ProviderError::InvalidResponse);
    };
    Ok(Some(paced(
        "Free Requests",
        used.max(0.0),
        limit,
        ProgressFormat::Count {
            suffix: "requests".into(),
        },
        Some("daily"),
        now,
    )))
}

fn paced(
    label: &str,
    used: f64,
    limit: f64,
    format: ProgressFormat,
    period: Option<&str>,
    now: DateTime<Utc>,
) -> MetricLine {
    let window = period.and_then(|period| reset_window(period, now));
    MetricLine::Progress {
        label: label.into(),
        used,
        limit,
        format,
        resets_at: window.map(|(end, _)| end),
        period_duration_ms: window.map(|(_, ms)| ms),
    }
}

pub fn reset_window(period: &str, now: DateTime<Utc>) -> Option<(DateTime<Utc>, i64)> {
    let (start, end) = match period {
        "daily" => {
            let start = midnight(now);
            (start, start + Duration::days(1))
        }
        "weekly" => {
            let start = iso_week_start(now);
            (start, start + Duration::days(7))
        }
        "monthly" => (month_start(now), next_month(now)),
        _ => return None,
    };
    Some((end, (end - start).num_milliseconds()))
}

fn activity_totals(row: &Map<String, Value>) -> Option<BTreeMap<String, f64>> {
    let mut raw = BTreeMap::new();
    for (field, key) in [
        ("usage", "spend"),
        ("byok_usage_inference", "byok"),
        ("prompt_tokens", "prompt"),
        ("cached_tokens", "cache"),
        ("completion_tokens", "completion"),
        ("reasoning_tokens", "reasoning"),
    ] {
        match row.get(field) {
            None | Some(Value::Null) => {}
            Some(value) => {
                let number = number(value).filter(|number| *number >= 0.0)?;
                raw.insert(key, number);
            }
        }
    }
    let spend = *raw.get("spend")?;
    let prompt = *raw.get("prompt")?;
    let completion = *raw.get("completion")?;
    let mut totals = BTreeMap::new();
    totals.insert("spend".into(), spend);
    if let Some(byok) = raw.get("byok").copied() {
        totals.insert("byok".into(), byok);
    }
    totals.insert("tokens".into(), prompt + completion);
    totals.insert(
        "input".into(),
        prompt - raw.get("cache").copied().unwrap_or(0.0),
    );
    totals.insert(
        "output".into(),
        completion - raw.get("reasoning").copied().unwrap_or(0.0),
    );
    if let Some(cache) = raw.get("cache").copied() {
        totals.insert("cache".into(), cache);
    }
    if let Some(reasoning) = raw.get("reasoning").copied() {
        totals.insert("reasoning".into(), reasoning);
    }
    Some(totals)
}

fn add(into: &mut BTreeMap<String, f64>, row: &BTreeMap<String, f64>) {
    for (key, value) in row {
        *into.entry(key.clone()).or_insert(0.0) += *value;
    }
}

fn data_object(body: &[u8]) -> Result<Map<String, Value>, ProviderError> {
    let root: Value = serde_json::from_slice(body).map_err(|_| ProviderError::InvalidResponse)?;
    root.get("data")
        .and_then(Value::as_object)
        .cloned()
        .ok_or(ProviderError::InvalidResponse)
}

fn number(value: &Value) -> Option<f64> {
    match value {
        Value::Number(number) => number.as_f64().filter(|number| number.is_finite()),
        Value::String(text) => text
            .trim()
            .parse::<f64>()
            .ok()
            .filter(|number| number.is_finite()),
        _ => None,
    }
}

fn number_at(object: &Map<String, Value>, key: &str) -> Option<f64> {
    object.get(key).and_then(number)
}

#[cfg(test)]
pub(crate) fn keys_page_body(count: usize, daily: f64, weekly: f64, monthly: f64) -> String {
    let key = format!(
        r#"{{"hash":"f01d52606dc8f0a8303a7b5cc3fa07109c2e346cec7c0a16b40de462992ce943","name":"Agent","label":"sk-or-v1-0e6...1c96","disabled":false,"limit":null,"limit_remaining":null,"limit_reset":null,"include_byok_in_limit":false,"usage":{monthly},"usage_daily":{daily},"usage_weekly":{weekly},"usage_monthly":{monthly},"byok_usage":0,"byok_usage_daily":0,"byok_usage_weekly":0,"byok_usage_monthly":0,"created_at":"2025-08-24T10:30:00Z","updated_at":"2025-08-24T15:45:00Z","expires_at":null}}"#
    );
    format!(r#"{{"data":[{}]}}"#, vec![key; count].join(","))
}

pub fn midnight(now: DateTime<Utc>) -> DateTime<Utc> {
    Utc.from_utc_datetime(&now.date_naive().and_hms_opt(0, 0, 0).unwrap())
}

fn iso_week_start(now: DateTime<Utc>) -> DateTime<Utc> {
    let date = now.date_naive() - Duration::days(now.weekday().num_days_from_monday() as i64);
    Utc.from_utc_datetime(&date.and_hms_opt(0, 0, 0).unwrap())
}

fn month_start(now: DateTime<Utc>) -> DateTime<Utc> {
    let date = chrono::NaiveDate::from_ymd_opt(now.year(), now.month(), 1).unwrap();
    Utc.from_utc_datetime(&date.and_hms_opt(0, 0, 0).unwrap())
}

fn next_month(now: DateTime<Utc>) -> DateTime<Utc> {
    let (year, month) = if now.month() == 12 {
        (now.year() + 1, 1)
    } else {
        (now.year(), now.month() + 1)
    };
    let date = chrono::NaiveDate::from_ymd_opt(year, month, 1).unwrap();
    Utc.from_utc_datetime(&date.and_hms_opt(0, 0, 0).unwrap())
}

#[cfg(test)]
#[path = "mapper_tests.rs"]
mod tests;
