//! Shared projection state, small value helpers, and the streaming projection
//! dispatch used by the value/collapsed/keys/schema projections.

use std::collections::BTreeMap;

use serde_json::{Value, json};

use super::collapsed::collapse_json;
use super::keys::collect_keys;
use crate::model::JsonProjection;
use crate::{Error, Result};

pub(super) struct ProjectionState {
    remaining: usize,
    array_sample_size: usize,
    complete: bool,
}

impl ProjectionState {
    pub(super) fn new(max_items: usize, array_sample_size: usize) -> Self {
        Self {
            remaining: max_items,
            array_sample_size,
            complete: true,
        }
    }

    pub(super) fn remaining(&self) -> usize {
        self.remaining
    }

    pub(super) fn array_sample_size(&self) -> usize {
        self.array_sample_size
    }

    pub(super) fn is_complete(&self) -> bool {
        self.complete
    }

    pub(super) fn mark_incomplete(&mut self) {
        self.complete = false;
    }
}

pub(super) fn json_type(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

pub(super) fn count_nodes(value: &Value) -> usize {
    match value {
        Value::Array(values) => 1usize.saturating_add(
            values
                .iter()
                .map(count_nodes)
                .fold(0usize, usize::saturating_add),
        ),
        Value::Object(values) => 1usize.saturating_add(
            values
                .values()
                .map(count_nodes)
                .fold(0usize, usize::saturating_add),
        ),
        _ => 1,
    }
}

pub(super) fn take_item(state: &mut ProjectionState) -> bool {
    if state.remaining == 0 {
        state.complete = false;
        return false;
    }
    state.remaining -= 1;
    true
}

pub(super) fn escape_pointer(value: &str) -> String {
    value.replace('~', "~0").replace('/', "~1")
}

pub(super) fn projection_item_count(
    value: &Value,
    projection: JsonProjection,
    array_sample_size: usize,
) -> usize {
    match projection {
        JsonProjection::Value | JsonProjection::Schema => count_nodes(value),
        JsonProjection::Collapsed => {
            super::collapsed::collapsed_item_count(value, array_sample_size)
        }
        JsonProjection::Keys => {
            super::keys::key_entries(value, None, super::execution::JsonKeyOrder::Pointer).len()
        }
    }
}

pub(super) fn project_json(
    value: &Value,
    projection: JsonProjection,
    state: &mut ProjectionState,
) -> Result<Value> {
    match projection {
        JsonProjection::Value => {
            let items = count_nodes(value);
            if items > state.remaining {
                return Err(Error::RequestLimitExceeded {
                    field: "selected JSON items",
                    requested: items,
                    limit: state.remaining,
                });
            }
            state.remaining -= items;
            Ok(value.clone())
        }
        JsonProjection::Collapsed => Ok(collapse_json(value, state)),
        JsonProjection::Keys => {
            let mut keys = BTreeMap::new();
            collect_keys(value, "", &mut keys, state);
            Ok(Value::Array(
                keys.into_iter()
                    .map(|(pointer, value_type)| json!({"pointer": pointer, "type": value_type}))
                    .collect(),
            ))
        }
        JsonProjection::Schema => Err(Error::InvalidInput {
            field: "projection",
            reason: "schema projection requires bounded schema execution",
        }),
    }
}
