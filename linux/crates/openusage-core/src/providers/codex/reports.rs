//! ChatGPT's daily reports. A report with any malformed day is rejected whole.
//! Port of `CodexUsageReports`. Token rows use the day totals; model turns and credits feed the
//! history hover.

use std::collections::BTreeMap;

use serde_json::Value;

#[derive(Clone, Debug, Default)]
pub(super) struct CountModel {
    pub tokens: Option<f64>,
    pub turns: f64,
}

#[derive(Clone, Debug)]
pub(super) struct CountedDay {
    pub tokens: Option<f64>,
    pub turns: f64,
    pub models: BTreeMap<String, CountModel>,
}

#[derive(Clone, Debug, Default)]
pub(super) struct CreditDay {
    pub total: f64,
    pub models: BTreeMap<String, f64>,
}

/// Days keyed by `yyyy-MM-dd`. `Some` is `text_total_tokens` (including zero); `None` is a day
/// the report named without a token total.
#[cfg(test)]
pub(super) fn counts(body: &[u8]) -> Option<BTreeMap<String, Option<f64>>> {
    Some(counted(body)?.into_iter().map(|(date, day)| (date, day.tokens)).collect())
}

pub(super) fn counted(body: &[u8]) -> Option<BTreeMap<String, CountedDay>> {
    let records = records(body)?;
    let mut days = BTreeMap::new();
    for record in records {
        let date = date(&record)?;
        let totals = record.get("totals")?.as_object()?;
        let turns = count(totals.get("turns"))?;
        let tokens = match totals.get("text_total_tokens") {
            None | Some(Value::Null) => None,
            Some(value) => Some(count(Some(value))?),
        };
        for key in [
            "uncached_text_input_tokens",
            "cached_text_input_tokens",
            "text_output_tokens",
        ] {
            if let Some(value) = totals.get(key).filter(|value| !value.is_null()) {
                count(Some(value))?;
            }
        }
        add_counted(&mut days, date, CountedDay { tokens, turns, models: count_models(&record) });
    }
    Some(days)
}

/// `Some` when the workspace credits report is usable, including one that charges nothing.
/// `None` rejects the whole report. A report that charges nothing has no days.
#[cfg(test)]
pub(super) fn workspace_credits(body: &[u8]) -> Option<()> {
    workspace_models(body).map(|_| ())
}

pub(super) fn workspace_models(body: &[u8]) -> Option<BTreeMap<String, CreditDay>> {
    let records = records(body)?;
    let mut days = BTreeMap::new();
    for record in records {
        let date = date(&record)?;
        let Some(premium) = record
            .pointer("/premium_usage_values/credit_usage_credits")
            .filter(|value| value.is_object())
        else {
            return Some(BTreeMap::new());
        };
        let charged = number_map(premium)?;
        let mut models = BTreeMap::new();
        if let Some(entries) = record.get("models").and_then(Value::as_array) {
            for model in entries.iter().filter_map(Value::as_object) {
                let Some(credits) = num(model.get("on_demand_credits")) else { continue };
                let name = model.get("model").and_then(Value::as_str).unwrap_or("Unknown");
                *models.entry(name.to_string()).or_insert(0.0) += credits;
            }
        }
        add_credits(
            &mut days,
            date,
            CreditDay { total: charged.values().sum(), models },
        );
    }
    Some(days)
}

#[cfg(test)]
pub(super) fn enterprise_credits(body: &[u8]) -> Option<()> {
    enterprise_models(body).map(|_| ())
}

pub(super) fn enterprise_models(body: &[u8]) -> Option<BTreeMap<String, CreditDay>> {
    let records = records(body)?;
    let mut days = BTreeMap::new();
    for record in records {
        let date = date(&record)?;
        let Some(values) = record.get("values").filter(|value| value.is_object()) else {
            return Some(BTreeMap::new());
        };
        let models = number_map(values)?;
        add_credits(
            &mut days,
            date,
            CreditDay { total: models.values().sum(), models },
        );
    }
    Some(days)
}

fn records(body: &[u8]) -> Option<Vec<Value>> {
    let value: Value = serde_json::from_slice(body).ok()?;
    let data = value.get("data")?.as_array()?;
    Some(data.clone())
}

fn date(record: &Value) -> Option<String> {
    let date = record.get("date")?.as_str()?;
    (date.len() >= 10).then(|| date[..10].to_string())
}

fn add_counted(days: &mut BTreeMap<String, CountedDay>, date: String, day: CountedDay) {
    if let Some(existing) = days.get_mut(&date) {
        existing.tokens = match (existing.tokens, day.tokens) {
            (Some(left), Some(right)) => Some(left + right),
            (Some(left), None) => Some(left),
            (None, Some(right)) => Some(right),
            (None, None) => None,
        };
        existing.turns += day.turns;
        for (name, model) in day.models {
            let slot = existing.models.entry(name).or_default();
            slot.tokens = match (slot.tokens, model.tokens) {
                (Some(left), Some(right)) => Some(left + right),
                (Some(left), None) => Some(left),
                (None, Some(right)) => Some(right),
                (None, None) => None,
            };
            slot.turns += model.turns;
        }
    } else {
        days.insert(date, day);
    }
}

fn add_credits(days: &mut BTreeMap<String, CreditDay>, date: String, day: CreditDay) {
    if let Some(existing) = days.get_mut(&date) {
        existing.total += day.total;
        for (name, credits) in day.models {
            *existing.models.entry(name).or_insert(0.0) += credits;
        }
    } else {
        days.insert(date, day);
    }
}

fn count_models(record: &Value) -> BTreeMap<String, CountModel> {
    let Some(models) = record.get("models").and_then(Value::as_array) else {
        return BTreeMap::new();
    };
    if models.is_empty() || models.iter().any(|model| !model.is_object()) {
        return BTreeMap::new();
    }
    let mut totals = BTreeMap::new();
    for model in models {
        let object = model.as_object().unwrap();
        let name = object.get("model").and_then(Value::as_str).unwrap_or("Unknown");
        let slot: &mut CountModel = totals.entry(name.to_string()).or_default();
        if let Some(turns) = count(object.get("turns")) {
            slot.turns += turns;
        }
        if let Some(tokens) = count(object.get("text_total_tokens")) {
            slot.tokens = Some(slot.tokens.unwrap_or(0.0) + tokens);
        }
    }
    totals
}

fn number_map(amounts: &Value) -> Option<BTreeMap<String, f64>> {
    let amounts = amounts.as_object()?;
    let mut numbers = BTreeMap::new();
    for (key, value) in amounts {
        numbers.insert(key.clone(), num(Some(value))?);
    }
    Some(numbers)
}

fn count(value: Option<&Value>) -> Option<f64> {
    num(value).filter(|number| *number >= 0.0)
}

fn num(value: Option<&Value>) -> Option<f64> {
    let number = match value? {
        Value::Number(number) => number.as_f64(),
        Value::String(text) => text.trim().parse().ok(),
        _ => None,
    }?;
    number.is_finite().then_some(number)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_malformed_day_rejects_the_whole_report() {
        for body in [
            r#"{"data":{}}"#,
            r#"{"data":[{"date":"2026-10-30"}]}"#,
            r#"{"data":[{"date":"2026-10-30","totals":{"turns":-1}}]}"#,
            r#"{"data":[{"date":"2026-10-30","totals":{"turns":3,"text_total_tokens":"lots"}}]}"#,
            r#"{"data":[{"totals":{"turns":3}}]}"#,
        ] {
            assert!(counts(body.as_bytes()).is_none(), "{body}");
        }
        assert!(workspace_credits(br#"{"data":[{"date":"2026-10-30","premium_usage_values":{"credit_usage_credits":{"cli":"lots"}}}]}"#).is_none());
        assert!(
            enterprise_credits(br#"{"data":[{"date":"2026-10-30","values":{"a":"lots"}}]}"#)
                .is_none()
        );
    }

    #[test]
    fn records_for_one_date_add_up_and_a_null_token_total_is_not_zero() {
        let counts = counts(
            br#"{"data":[
                {"date":"2026-10-30","totals":{"turns":10,"text_total_tokens":100}},
                {"date":"2026-10-30","totals":{"turns":10,"text_total_tokens":100}},
                {"date":"2026-10-29","totals":{"turns":3,"text_total_tokens":null}}
            ]}"#,
        )
        .unwrap();
        assert_eq!(counts.get("2026-10-30").copied(), Some(Some(200.0)));
        assert_eq!(counts.get("2026-10-29").copied(), Some(None));
    }

    #[test]
    fn a_credits_report_without_charged_credits_is_usable() {
        assert!(workspace_credits(
            br#"{"data":[{"date":"2026-10-30","models":[{"credits":100.0}]}]}"#
        )
        .is_some());
        assert!(enterprise_credits(br#"{"data":[{"date":"2026-10-30"}]}"#).is_some());
        assert!(workspace_credits(br#"{}"#).is_none());
    }
}
