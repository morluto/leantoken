//! Collapsed projection: bounded array sampling and object shrinking.

use serde_json::{Map, Value, json};

use super::projection::ProjectionState;

pub(super) fn collapsed_item_count(value: &Value, array_sample_size: usize) -> usize {
    match value {
        Value::Array(values) => 1usize.saturating_add(
            values
                .iter()
                .take(array_sample_size)
                .map(|value| collapsed_item_count(value, array_sample_size))
                .fold(0usize, usize::saturating_add),
        ),
        Value::Object(values) => 1usize.saturating_add(
            values
                .values()
                .map(|value| collapsed_item_count(value, array_sample_size))
                .fold(0usize, usize::saturating_add),
        ),
        _ => 1,
    }
}

pub(super) fn collapse_json(value: &Value, state: &mut ProjectionState) -> Value {
    if !super::projection::take_item(state) {
        return Value::Null;
    }
    match value {
        Value::Array(values) => {
            let mut sample = Vec::new();
            for value in values.iter().take(state.array_sample_size()) {
                if state.remaining() == 0 {
                    state.mark_incomplete();
                    break;
                }
                sample.push(collapse_json(value, state));
            }
            let omitted = values.len().saturating_sub(sample.len());
            json!({
                "$array": {
                    "count": values.len(),
                    "sample": sample,
                    "omitted": omitted,
                }
            })
        }
        Value::Object(values) => {
            let mut projected = Map::new();
            for (key, value) in values {
                if state.remaining() == 0 {
                    state.mark_incomplete();
                    break;
                }
                projected.insert(key.clone(), collapse_json(value, state));
            }
            Value::Object(projected)
        }
        _ => value.clone(),
    }
}
