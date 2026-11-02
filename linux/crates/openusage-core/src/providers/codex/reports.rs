//! ChatGPT's daily reports. A report with any malformed day is rejected whole.
//! Port of `CodexUsageReports`. Credit amounts are validated so a bad report is a failed read;
//! the history rows show token counts only.

use std::collections::BTreeMap;

use serde_json::Value;

/// Days keyed by `yyyy-MM-dd`. `Some` is `text_total_tokens` (including zero); `None` is a day
/// the report named without a token total.
pub(super) fn counts(body: &[u8]) -> Option<BTreeMap<String, Option<f64>>> {
    let records = records(body)?;
    let mut days = BTreeMap::new();
    for record in records {
        let date = date(&record)?;
        let totals = record.get("totals")?.as_object()?;
        count(totals.get("turns"))?;
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
        add(&mut days, date, tokens);
    }
    Some(days)
}

/// `Some` when the workspace credits report is usable, including one that charges nothing.
/// `None` rejects the whole report.
pub(super) fn workspace_credits(body: &[u8]) -> Option<()> {
    let records = records(body)?;
    for record in records {
        date(&record)?;
        let Some(premium) = record
            .pointer("/premium_usage_values/credit_usage_credits")
            .filter(|value| value.is_object())
        else {
            return Some(());
        };
        numbers(premium)?;
    }
    Some(())
}

pub(super) fn enterprise_credits(body: &[u8]) -> Option<()> {
    let records = records(body)?;
    for record in records {
        date(&record)?;
        let Some(values) = record.get("values").filter(|value| value.is_object()) else {
            return Some(());
        };
        numbers(values)?;
    }
    Some(())
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

fn add(days: &mut BTreeMap<String, Option<f64>>, date: String, tokens: Option<f64>) {
    days.entry(date)
        .and_modify(|existing| {
            *existing = match (*existing, tokens) {
                (Some(left), Some(right)) => Some(left + right),
                (Some(left), None) => Some(left),
                (None, Some(right)) => Some(right),
                (None, None) => None,
            };
        })
        .or_insert(tokens);
}

fn numbers(amounts: &Value) -> Option<()> {
    let amounts = amounts.as_object()?;
    amounts
        .values()
        .all(|value| num(Some(value)).is_some())
        .then_some(())
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
