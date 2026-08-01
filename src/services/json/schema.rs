//! Schema projection: breadth-first schema inference with bounded node counts
//! and omission metadata, plus token-bounded pagination.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use serde_json::{Map, Value, json};

use super::MAX_SCHEMA_OMITTED_POINTERS;
use super::projection::{count_nodes, escape_pointer, json_type};
use crate::Result;
use crate::services::Services;

/// Work counters for one schema projection call, used by diagnostics and the
/// schema profile example to measure candidate reuse.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct SchemaProjectionCounters {
    /// Number of `build_schema_breadth_first` invocations.
    pub(super) schema_builds: usize,
    /// Number of `serde_json::to_string` calls on schema candidates.
    pub(super) schema_serializations: usize,
    /// Number of tokenizer count calls on serialized schema candidates.
    pub(super) schema_token_counts: usize,
    /// Number of times a cached `MeasuredProjection` was reused.
    pub(super) cache_hits: usize,
}

/// Cached projection of one schema candidate: the rendered JSON value, its
/// serialized string, and the exact tokenizer count over that string. The
/// serialized string is retained so the final selection does not need to be
/// re-serialized for token accounting.
pub(super) struct MeasuredProjection {
    value: Value,
    /// Cached serialized representation. Retained for the projection cache
    /// contract so callers that need the serialized form (e.g. response
    /// fitting diagnostics) can reuse it without re-serializing.
    #[allow(dead_code)]
    serialized: String,
    tokens: usize,
}

impl MeasuredProjection {
    fn measure(services: &Services, value: Value, counters: &mut SchemaProjectionCounters) -> Result<Self> {
        let serialized = serde_json::to_string(&value)
            .map_err(|error| crate::Error::SerializationFailure(error.to_string()))?;
        counters.schema_serializations = counters.schema_serializations.saturating_add(1);
        let tokens = services.config.tokenizer.count(&serialized);
        counters.schema_token_counts = counters.schema_token_counts.saturating_add(1);
        Ok(Self { value, serialized, tokens })
    }

    pub(super) fn tokens(&self) -> usize {
        self.tokens
    }

    pub(super) fn into_value(self) -> Value {
        self.value
    }
}

/// Request-local plan that memoizes `MeasuredProjection` results by `max_items`
/// across the binary fit loop so already-computed candidates are reused.
pub(super) struct SchemaProjectionPlan<'a> {
    services: &'a Services,
    cache: BTreeMap<usize, MeasuredProjection>,
    counters: SchemaProjectionCounters,
}

impl<'a> SchemaProjectionPlan<'a> {
    fn new(services: &'a Services) -> Self {
        Self {
            services,
            cache: BTreeMap::new(),
            counters: SchemaProjectionCounters::default(),
        }
    }

    /// Build and measure a breadth-first schema at `max_items`, or return a
    /// previously cached measurement. The schema value, serialized string, and
    /// exact token count are all reused on cache hits.
    fn candidate(&mut self, value: &Value, max_items: usize) -> Result<&MeasuredProjection> {
        if !self.cache.contains_key(&max_items) {
            let built = build_schema_breadth_first(value, max_items);
            self.counters.schema_builds = self.counters.schema_builds.saturating_add(1);
            let measured = MeasuredProjection::measure(self.services, built, &mut self.counters)?;
            self.cache.insert(max_items, measured);
        } else {
            self.counters.cache_hits = self.counters.cache_hits.saturating_add(1);
        }
        Ok(self.cache.get(&max_items).expect("entry was just inserted or cached"))
    }

    fn counters(&self) -> &SchemaProjectionCounters {
        &self.counters
    }
}

pub(super) struct SchemaProjection {
    value: Value,
    total_items: usize,
    returned_items: usize,
    remaining_items: usize,
    incomplete_reason: Option<crate::model::JsonIncompleteReason>,
    projected_tokens: usize,
    counters: SchemaProjectionCounters,
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
        SchemaProjectionCounters,
    ) {
        (
            self.value,
            self.total_items,
            self.returned_items,
            self.remaining_items,
            self.incomplete_reason,
            self.projected_tokens,
            self.counters,
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
    let mut plan = SchemaProjectionPlan::new(services);
    let item_limited = plan.candidate(value, item_limit)?;
    let item_limited_tokens = item_limited.tokens();
    let (returned_items, schema, projected_tokens) = if item_limited_tokens <= max_tokens {
        (
            item_limit,
            plan.cache.remove(&item_limit).expect("item_limit was just measured").into_value(),
            item_limited_tokens,
        )
    } else {
        let root = plan.candidate(value, 1)?;
        let root_tokens = root.tokens();
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
        let mut best_key = 1usize;
        let mut best_tokens = root_tokens;
        while lower <= upper {
            let middle = lower.saturating_add(upper.saturating_sub(lower) / 2);
            let candidate = plan.candidate(value, middle)?;
            let candidate_tokens = candidate.tokens();
            if candidate_tokens <= max_tokens {
                lower = middle.saturating_add(1);
                best_items = middle;
                best_key = middle;
                best_tokens = candidate_tokens;
            } else {
                upper = middle.saturating_sub(1);
            }
        }
        let schema = plan
            .cache
            .remove(&best_key)
            .expect("best key was cached during binary search")
            .into_value();
        (best_items, schema, best_tokens)
    };
    let counters = plan.counters().clone();
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
        counters,
    })
}
