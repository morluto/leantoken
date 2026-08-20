//! Numeric summary aggregation over selected JSON values.

use serde_json::Value;

use crate::model::JsonNumericSummary;

pub(super) fn numeric_summary(value: &Value) -> JsonNumericSummary {
    let mut values = Vec::new();
    let mut non_numeric_count = 0usize;
    collect_numbers(value, &mut values, &mut non_numeric_count);
    values.sort_by(f64::total_cmp);
    let count = values.len();
    let median = match count {
        0 => None,
        count if count % 2 == 1 => Some(values[count / 2]),
        count => Some(values[count / 2 - 1].midpoint(values[count / 2])),
    };
    let p95 = (count > 0).then(|| {
        let rank = (count.saturating_mul(95).saturating_add(99)) / 100;
        values[rank.saturating_sub(1).min(count - 1)]
    });
    JsonNumericSummary {
        count,
        non_numeric_count,
        min: values.first().copied(),
        median,
        p95,
        max: values.last().copied(),
    }
}

fn collect_numbers(value: &Value, values: &mut Vec<f64>, non_numeric_count: &mut usize) {
    match value {
        Value::Number(value) => {
            if let Some(value) = value.as_f64() {
                values.push(value);
            } else {
                *non_numeric_count = non_numeric_count.saturating_add(1);
            }
        }
        Value::Array(items) => {
            for item in items {
                collect_numbers(item, values, non_numeric_count);
            }
        }
        Value::Object(items) => {
            for item in items.values() {
                collect_numbers(item, values, non_numeric_count);
            }
        }
        _ => *non_numeric_count = non_numeric_count.saturating_add(1),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::numeric_summary;

    #[test]
    fn even_median_stays_finite_for_large_json_numbers() {
        for (values, expected) in [
            (json!([1e308, 1e308]), 1e308),
            (json!([-1e308, 1e308]), 0.0),
            (
                json!([f64::from_bits(1), f64::from_bits(1)]),
                f64::from_bits(1),
            ),
        ] {
            let summary = numeric_summary(&values);

            assert_eq!(summary.median, Some(expected));
            assert!(
                serde_json::to_value(summary).expect("serializable summary")["median"].is_number(),
                "a finite input median must not serialize as null"
            );
        }
    }
}
