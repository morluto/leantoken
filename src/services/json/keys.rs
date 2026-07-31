//! Keys projection: pointer enumeration, depth-then-pointer ordering, and
//! token-bounded pagination.

use std::collections::BTreeMap;

use serde_json::{Value, json};

use super::cursor::{JsonCursor, make_json_cursor};
use super::execution::{JsonExecutionOptions, JsonKeyOrder};
use super::projection::{ProjectionState, escape_pointer, json_type, take_item};
use super::source::json_tokens;
use crate::services::Services;
use crate::{Error, Result};

pub(super) struct KeyProjectionPage {
    value: Value,
    total_items: usize,
    returned_items: usize,
    remaining_items: usize,
    incomplete_reason: Option<crate::model::JsonIncompleteReason>,
    next_cursor: Option<String>,
    projected_tokens: usize,
}

pub(super) struct KeyProjectionContext<'a> {
    cursor: Option<&'a JsonCursor>,
    source_hash: &'a str,
    query_hash: &'a str,
    execution: JsonExecutionOptions,
}

impl<'a> KeyProjectionContext<'a> {
    pub(super) fn new(
        cursor: Option<&'a JsonCursor>,
        source_hash: &'a str,
        query_hash: &'a str,
        execution: JsonExecutionOptions,
    ) -> Self {
        Self {
            cursor,
            source_hash,
            query_hash,
            execution,
        }
    }
}

impl KeyProjectionPage {
    pub(super) fn into_parts(
        self,
    ) -> (
        Value,
        usize,
        usize,
        usize,
        Option<crate::model::JsonIncompleteReason>,
        Option<String>,
        usize,
    ) {
        (
            self.value,
            self.total_items,
            self.returned_items,
            self.remaining_items,
            self.incomplete_reason,
            self.next_cursor,
            self.projected_tokens,
        )
    }
}

fn collect_all_keys(
    value: &Value,
    pointer: &str,
    depth: usize,
    max_depth: Option<usize>,
    keys: &mut BTreeMap<String, (usize, &'static str)>,
) {
    if !keys.contains_key(pointer) {
        keys.insert(pointer.to_owned(), (depth, json_type(value)));
    }
    if max_depth.is_some_and(|maximum| depth >= maximum) {
        return;
    }
    match value {
        Value::Object(values) => {
            for (key, value) in values {
                let pointer = format!("{pointer}/{}", escape_pointer(key));
                collect_all_keys(value, &pointer, depth.saturating_add(1), max_depth, keys);
            }
        }
        Value::Array(values) => {
            let pointer = format!("{pointer}/*");
            for value in values {
                collect_all_keys(value, &pointer, depth.saturating_add(1), max_depth, keys);
            }
        }
        _ => {}
    }
}

pub(super) fn key_entries(
    value: &Value,
    max_depth: Option<usize>,
    order: JsonKeyOrder,
) -> Vec<Value> {
    let mut keys = BTreeMap::new();
    collect_all_keys(value, "", 0, max_depth, &mut keys);
    let mut entries = keys.into_iter().collect::<Vec<_>>();
    if order == JsonKeyOrder::DepthThenPointer {
        entries.sort_by(
            |(left_pointer, (left_depth, _)), (right_pointer, (right_depth, _))| {
                left_depth
                    .cmp(right_depth)
                    .then_with(|| left_pointer.cmp(right_pointer))
            },
        );
    }
    entries
        .into_iter()
        .map(|(pointer, (_, value_type))| json!({"pointer": pointer, "type": value_type}))
        .collect()
}

fn key_prefix_tokens(services: &Services, entries: &[Value], length: usize) -> Result<usize> {
    json_tokens(services, &Value::Array(entries[..length].to_vec()))
}

fn largest_key_prefix_within_tokens(
    services: &Services,
    entries: &[Value],
    max_tokens: usize,
) -> Result<(usize, usize)> {
    if entries.is_empty() {
        return Ok((0, json_tokens(services, &Value::Array(Vec::new()))?));
    }
    let full_tokens = key_prefix_tokens(services, entries, entries.len())?;
    if full_tokens <= max_tokens {
        return Ok((entries.len(), full_tokens));
    }

    let mut lower = 0usize;
    let mut upper = entries.len();
    while lower < upper {
        let middle = lower.saturating_add(upper).saturating_add(1) / 2;
        if key_prefix_tokens(services, entries, middle)? <= max_tokens {
            lower = middle;
        } else {
            upper = middle.saturating_sub(1);
        }
    }
    let tokens = key_prefix_tokens(services, entries, lower)?;
    Ok((lower, tokens))
}

pub(super) fn project_key_page(
    services: &Services,
    value: &Value,
    max_items: usize,
    max_tokens: usize,
    context: KeyProjectionContext<'_>,
) -> Result<KeyProjectionPage> {
    let entries = key_entries(
        value,
        context.execution.depth(),
        context.execution.key_order(),
    );
    let total_items = entries.len();
    let offset = match context.cursor {
        Some(cursor) if cursor.matches(context.source_hash, context.query_hash) => cursor.offset(),
        Some(_) => return Err(Error::StaleCursor),
        None => 0,
    };
    if offset > total_items || (offset == total_items && offset != 0) {
        return Err(Error::StaleCursor);
    }

    let page_end = offset.saturating_add(max_items).min(total_items);
    let candidates = &entries[offset..page_end];
    let (returned_items, projected_tokens) =
        largest_key_prefix_within_tokens(services, candidates, max_tokens)?;
    if returned_items == 0 && !candidates.is_empty() {
        return Err(Error::RequestLimitExceeded {
            field: "one projected JSON item tokens",
            requested: key_prefix_tokens(services, candidates, 1)?,
            limit: max_tokens,
        });
    }

    let consumed = offset.saturating_add(returned_items);
    let remaining_items = total_items.saturating_sub(consumed);
    let incomplete_reason = (remaining_items > 0).then_some(if returned_items < candidates.len() {
        crate::model::JsonIncompleteReason::MaxTokens
    } else {
        crate::model::JsonIncompleteReason::MaxItems
    });
    let next_cursor = (remaining_items > 0).then(|| {
        make_json_cursor(
            context.execution.cursor_version(),
            context.source_hash,
            context.query_hash,
            consumed,
        )
    });
    Ok(KeyProjectionPage {
        value: Value::Array(candidates[..returned_items].to_vec()),
        total_items,
        returned_items,
        remaining_items,
        incomplete_reason,
        next_cursor,
        projected_tokens,
    })
}

/// Streaming keys projection used by the standard `project_json` path. Honors
/// the shared `ProjectionState` item budget and deduplicates pointers.
pub(super) fn collect_keys(
    value: &Value,
    pointer: &str,
    keys: &mut BTreeMap<String, &'static str>,
    state: &mut ProjectionState,
) {
    if !keys.contains_key(pointer) {
        if !take_item(state) {
            return;
        }
        keys.insert(pointer.to_owned(), json_type(value));
    }
    match value {
        Value::Object(values) => {
            for (key, value) in values {
                let pointer = format!("{pointer}/{}", escape_pointer(key));
                collect_keys(value, &pointer, keys, state);
            }
        }
        Value::Array(values) => {
            let pointer = format!("{pointer}/*");
            for value in values {
                collect_keys(value, &pointer, keys, state);
            }
        }
        _ => {}
    }
}
