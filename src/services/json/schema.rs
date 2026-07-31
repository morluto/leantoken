//! Schema projection: breadth-first schema inference with bounded node counts
//! and omission metadata, plus token-bounded pagination.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use serde_json::{Map, Value, json};

use super::MAX_SCHEMA_OMITTED_POINTERS;
use super::projection::{ProjectionState, count_nodes, escape_pointer, json_type, take_item};
use super::source::json_tokens;
use crate::Result;
use crate::services::Services;

pub(super) struct SchemaProjection {
    value: Value,
    total_items: usize,
    returned_items: usize,
    remaining_items: usize,
    incomplete_reason: Option<crate::model::JsonIncompleteReason>,
    projected_tokens: usize,
}

impl SchemaProjection {
    pub(super) fn into_parts(
        self,
    ) -> (
        Value,
        usize,
        usize,
        usize,
        Option<crate::model::JsonIncompleteReason>,
        usize,
    ) {
        (
            self.value,
            self.total_items,
            self.returned_items,
            self.remaining_items,
            self.incomplete_reason,
            self.projected_tokens,
        )
    }
}

enum SchemaNodeKind {
    Scalar(&'static str),
    Object(BTreeMap<String, usize>),
    Array { count: usize, variants: Vec<usize> },
}

struct SchemaNode {
    kind: SchemaNodeKind,
    pointer: String,
}

fn schema_node(value: &Value, pointer: String) -> SchemaNode {
    let kind = match value {
        Value::Object(_) => SchemaNodeKind::Object(BTreeMap::new()),
        Value::Array(values) => SchemaNodeKind::Array {
            count: values.len(),
            variants: Vec::new(),
        },
        _ => SchemaNodeKind::Scalar(json_type(value)),
    };
    SchemaNode { kind, pointer }
}

fn schema_node_has_children(value: &Value) -> bool {
    match value {
        Value::Object(values) => !values.is_empty(),
        Value::Array(values) => !values.is_empty(),
        _ => false,
    }
}

fn schema_frontier(queued: &VecDeque<(&Value, usize, String)>, omitted: &mut BTreeSet<String>) {
    for (value, _, omission_root) in queued {
        if schema_node_has_children(value) {
            omitted.insert(omission_root.clone());
        }
    }
}

fn render_schema_node(nodes: &[SchemaNode], node_id: usize) -> Value {
    match &nodes[node_id].kind {
        SchemaNodeKind::Scalar(value_type) => json!({"type": value_type}),
        SchemaNodeKind::Object(children) => {
            let properties = children
                .iter()
                .map(|(key, child)| (key.clone(), render_schema_node(nodes, *child)))
                .collect::<Map<_, _>>();
            json!({"type": "object", "properties": properties})
        }
        SchemaNodeKind::Array { count, variants } => {
            let mut unique = BTreeMap::new();
            for variant in variants {
                let schema = render_schema_node(nodes, *variant);
                let key = serde_json::to_string(&schema).unwrap_or_default();
                unique.entry(key).or_insert(schema);
            }
            let items = match unique.len() {
                0 => json!({}),
                1 => unique.into_values().next().unwrap_or_else(|| json!({})),
                _ => json!({"anyOf": unique.into_values().collect::<Vec<_>>()}),
            };
            json!({"type": "array", "count": count, "items": items})
        }
    }
}

pub(super) fn build_schema_breadth_first(value: &Value, max_items: usize) -> Value {
    debug_assert!(max_items > 0);
    let mut nodes = vec![schema_node(value, String::new())];
    let mut queue = VecDeque::from([(value, 0usize, String::new())]);
    let mut omitted = BTreeSet::new();

    'build: while let Some((current, node_id, omission_root)) = queue.pop_front() {
        match current {
            Value::Object(values) => {
                for (key, child) in values {
                    let pointer = format!("{}/{}", nodes[node_id].pointer, escape_pointer(key));
                    let child_omission_root = if node_id == 0 {
                        pointer.clone()
                    } else {
                        omission_root.clone()
                    };
                    if nodes.len() >= max_items {
                        omitted.insert(child_omission_root);
                        for remaining_key in values.keys().skip_while(|value| *value != key).skip(1)
                        {
                            omitted.insert(if node_id == 0 {
                                format!(
                                    "{}/{}",
                                    nodes[node_id].pointer,
                                    escape_pointer(remaining_key)
                                )
                            } else {
                                omission_root.clone()
                            });
                        }
                        schema_frontier(&queue, &mut omitted);
                        break 'build;
                    }
                    let child_id = nodes.len();
                    nodes.push(schema_node(child, pointer));
                    let SchemaNodeKind::Object(children) = &mut nodes[node_id].kind else {
                        unreachable!("object source has object schema node");
                    };
                    children.insert(key.clone(), child_id);
                    if schema_node_has_children(child) {
                        queue.push_back((child, child_id, child_omission_root));
                    }
                }
            }
            Value::Array(values) => {
                let pointer = format!("{}/*", nodes[node_id].pointer);
                let child_omission_root = if node_id == 0 {
                    pointer.clone()
                } else {
                    omission_root.clone()
                };
                for child in values {
                    if nodes.len() >= max_items {
                        omitted.insert(child_omission_root.clone());
                        schema_frontier(&queue, &mut omitted);
                        break 'build;
                    }
                    let child_id = nodes.len();
                    nodes.push(schema_node(child, pointer.clone()));
                    let SchemaNodeKind::Array { variants, .. } = &mut nodes[node_id].kind else {
                        unreachable!("array source has array schema node");
                    };
                    variants.push(child_id);
                    if schema_node_has_children(child) {
                        queue.push_back((child, child_id, child_omission_root.clone()));
                    }
                }
            }
            _ => {}
        }
    }

    let mut schema = render_schema_node(&nodes, 0);
    if !omitted.is_empty()
        && let Value::Object(root) = &mut schema
    {
        let omitted_subtree_count = omitted.len();
        let omitted_subtree_pointers = omitted
            .into_iter()
            .take(MAX_SCHEMA_OMITTED_POINTERS)
            .collect::<Vec<_>>();
        root.insert(
            "x-leantoken-incomplete".into(),
            json!({
                "omitted_subtree_count": omitted_subtree_count,
                "omitted_subtree_pointers": omitted_subtree_pointers,
            }),
        );
    }
    schema
}

pub(super) fn project_schema_page(
    services: &Services,
    value: &Value,
    max_items: usize,
    max_tokens: usize,
) -> Result<SchemaProjection> {
    let total_items = count_nodes(value);
    let item_limit = total_items.min(max_items).max(1);
    let item_limited = build_schema_breadth_first(value, item_limit);
    let item_limited_tokens = json_tokens(services, &item_limited)?;
    let (returned_items, schema, projected_tokens) = if item_limited_tokens <= max_tokens {
        (item_limit, item_limited, item_limited_tokens)
    } else {
        let root = build_schema_breadth_first(value, 1);
        let root_tokens = json_tokens(services, &root)?;
        if root_tokens > max_tokens {
            return Err(crate::Error::RequestLimitExceeded {
                field: "one projected JSON item tokens",
                requested: root_tokens,
                limit: max_tokens,
            });
        }
        let mut lower = 1usize;
        let mut upper = item_limit.saturating_sub(1);
        let mut best_items = 1usize;
        let mut best = root;
        let mut best_tokens = root_tokens;
        while lower <= upper {
            let middle = lower.saturating_add(upper.saturating_sub(lower) / 2);
            let candidate = build_schema_breadth_first(value, middle);
            let candidate_tokens = json_tokens(services, &candidate)?;
            if candidate_tokens <= max_tokens {
                lower = middle.saturating_add(1);
                best_items = middle;
                best = candidate;
                best_tokens = candidate_tokens;
            } else {
                upper = middle.saturating_sub(1);
            }
        }
        (best_items, best, best_tokens)
    };
    let remaining_items = total_items.saturating_sub(returned_items);
    let incomplete_reason = (remaining_items > 0).then_some(if returned_items < item_limit {
        crate::model::JsonIncompleteReason::MaxTokens
    } else {
        crate::model::JsonIncompleteReason::MaxItems
    });
    Ok(SchemaProjection {
        value: schema,
        total_items,
        returned_items,
        remaining_items,
        incomplete_reason,
        projected_tokens,
    })
}

/// Legacy depth-first schema inference used by the streaming `project_json`
/// path. Kept for behavioral parity with the breadth-first builder under full
/// item budgets.
pub(super) fn infer_schema(value: &Value, state: &mut ProjectionState) -> Value {
    if !take_item(state) {
        return json!({"type": "unknown"});
    }
    match value {
        Value::Object(values) => {
            let mut properties = Map::new();
            for (key, value) in values {
                if state.remaining() == 0 {
                    state.mark_incomplete();
                    break;
                }
                properties.insert(key.clone(), infer_schema(value, state));
            }
            json!({"type": "object", "properties": properties})
        }
        Value::Array(values) => {
            let mut variants = BTreeMap::new();
            for value in values {
                if state.remaining() == 0 {
                    state.mark_incomplete();
                    break;
                }
                let schema = infer_schema(value, state);
                let key = serde_json::to_string(&schema).unwrap_or_default();
                variants.entry(key).or_insert(schema);
            }
            let items = match variants.len() {
                0 => json!({}),
                1 => variants.into_values().next().unwrap_or_else(|| json!({})),
                _ => json!({"anyOf": variants.into_values().collect::<Vec<_>>() }),
            };
            json!({"type": "array", "count": values.len(), "items": items})
        }
        _ => json!({"type": json_type(value)}),
    }
}
