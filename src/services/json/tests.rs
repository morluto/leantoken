//! Colocated JSON invariant tests, adapted to the decomposed submodules.

use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use super::collapsed::collapse_json;
use super::cursor::{decode_json_cursor, json_query_hash, make_json_cursor};
use super::execution::{JsonCursorVersion, JsonKeyOrder};
use super::keys::key_entries;
use super::projection::{ProjectionState, count_nodes, project_json};
use super::schema::{SchemaProjectionCounters, build_schema_breadth_first, project_schema_page};
use super::validation::validate_json_request;
use super::{JsonExecutionOptions, MAX_JSON_DEPTH};
use crate::Error;
use crate::model::{JsonOperation, JsonProjection, JsonRequest};
use crate::services::{ServiceCallOptions, Services};

#[test]
fn keys_projection_deduplicates_homogeneous_array_paths_before_item_caps() {
    let value = json!([
        {"score": 1, "name": "a"},
        {"score": 2, "name": "b"},
        {"score": 3, "name": "c"}
    ]);
    let mut state = ProjectionState::new(4, 3);
    let projected =
        project_json(&value, JsonProjection::Keys, &mut state).expect("keys projection");

    assert!(state.is_complete());
    assert_eq!(projected.as_array().map(Vec::len), Some(4));
}

#[test]
fn keys_projection_detects_late_heterogeneous_paths_after_the_item_cap() {
    let value = json!([{"first": 1}, {"second": 2}]);
    let mut state = ProjectionState::new(3, 3);
    let projected =
        project_json(&value, JsonProjection::Keys, &mut state).expect("keys projection");

    assert!(!state.is_complete());
    assert_eq!(projected.as_array().map(Vec::len), Some(3));
    assert_eq!(key_entries(&value, None, JsonKeyOrder::Pointer).len(), 4);
}

#[test]
fn shallow_keys_are_depth_ordered_and_preserve_pointer_escaping() {
    let value = json!({
        "a/deep": {"buried": {"value": 1}},
        "array": [{"left": 1}, {"right": 2}],
        "β~eta": {},
    });

    let shallow = key_entries(&value, Some(1), JsonKeyOrder::DepthThenPointer);
    let shallow_pointers = shallow
        .iter()
        .filter_map(|entry| entry["pointer"].as_str())
        .collect::<Vec<_>>();
    assert_eq!(shallow_pointers, ["", "/array", "/a~1deep", "/β~0eta"]);
    assert_eq!(
        key_entries(&value, Some(0), JsonKeyOrder::DepthThenPointer),
        vec![json!({"pointer": "", "type": "object"})]
    );

    let complete = key_entries(&value, None, JsonKeyOrder::DepthThenPointer);
    let complete_pointers = complete
        .iter()
        .filter_map(|entry| entry["pointer"].as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        &complete_pointers[..6],
        [
            "",
            "/array",
            "/a~1deep",
            "/β~0eta",
            "/array/*",
            "/a~1deep/buried",
        ]
    );
    assert!(complete_pointers.contains(&"/array/*/left"));
    assert!(complete_pointers.contains(&"/array/*/right"));
}

#[test]
fn v2_key_cursors_bind_depth_and_reject_legacy_ordering() {
    let operation = JsonOperation::Query {
        path: "report.json".into(),
        selector: None,
        projection: JsonProjection::Keys,
    };
    let shallow =
        json_query_hash(&operation, JsonExecutionOptions::mcp(Some(1))).expect("shallow hash");
    let deep = json_query_hash(&operation, JsonExecutionOptions::mcp(Some(2))).expect("deep hash");
    assert_ne!(shallow, deep);

    let source = crate::text::hash("source");
    let legacy = make_json_cursor(JsonCursorVersion::V1, &source, &shallow, 1);
    assert!(matches!(
        decode_json_cursor(&legacy, JsonCursorVersion::V2),
        Err(Error::StaleCursor)
    ));
    let current = make_json_cursor(JsonCursorVersion::V2, &source, &shallow, 1);
    assert!(decode_json_cursor(&current, JsonCursorVersion::V2).is_ok());
}

#[test]
fn mcp_depth_is_bounded_and_keys_only() {
    let keys = JsonRequest {
        operation: JsonOperation::Query {
            path: "report.json".into(),
            selector: None,
            projection: JsonProjection::Keys,
        },
        max_tokens: None,
        max_items: None,
        array_sample_size: None,
        cursor: None,
    };
    assert!(matches!(
        validate_json_request(&keys, JsonExecutionOptions::mcp(Some(MAX_JSON_DEPTH + 1))),
        Err(Error::RequestLimitExceeded { field: "depth", .. })
    ));

    let value = JsonRequest {
        operation: JsonOperation::Query {
            path: "report.json".into(),
            selector: None,
            projection: JsonProjection::Value,
        },
        ..keys
    };
    assert!(matches!(
        validate_json_request(&value, JsonExecutionOptions::mcp(Some(1))),
        Err(Error::InvalidInput { field: "depth", .. })
    ));
}

#[tokio::test]
async fn mcp_key_pages_preserve_shallow_parity_and_stale_cursor_boundaries() {
    let root = tempfile::tempdir().expect("root");
    std::fs::write(
        root.path().join("report.json"),
        serde_json::to_vec(&json!({
            "alpha": {"deep": 1},
            "array": [{"nested": 2}],
            "empty": {},
            "βeta": true,
        }))
        .expect("serialize fixture"),
    )
    .expect("write fixture");
    let config = crate::Config::discover(root.path(), Some(root.path().join("index.sqlite")))
        .expect("config");
    let services = Services::open(config).expect("services");
    let operation = JsonOperation::Query {
        path: "report.json".into(),
        selector: None,
        projection: JsonProjection::Keys,
    };
    let mut request = JsonRequest {
        operation: operation.clone(),
        max_tokens: Some(1_000),
        max_items: Some(2),
        array_sample_size: None,
        cursor: None,
    };
    let execution = JsonExecutionOptions::mcp(Some(1));
    let mut pointers = Vec::new();
    let first_cursor = loop {
        let response = services
            .json_cancellable_with_execution_options(
                request.clone(),
                ServiceCallOptions::new(),
                execution,
                CancellationToken::new(),
            )
            .await
            .expect("shallow keys page");
        pointers.extend(
            response
                .value
                .as_ref()
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|entry| entry["pointer"].as_str().map(str::to_owned)),
        );
        let next = response.meta.next_cursor;
        if let Some(cursor) = next {
            let first = request.cursor.get_or_insert_with(|| cursor.clone()).clone();
            request.cursor = Some(cursor);
            if pointers.len() == 2 {
                assert!(first.starts_with("j2:"));
            }
        } else {
            break request.cursor.expect("at least one cursor");
        }
    };
    assert_eq!(pointers, ["", "/alpha", "/array", "/empty", "/βeta"]);

    let stale_depth = services
        .json_cancellable_with_execution_options(
            JsonRequest {
                operation: operation.clone(),
                max_tokens: Some(1_000),
                max_items: Some(2),
                array_sample_size: None,
                cursor: Some(first_cursor),
            },
            ServiceCallOptions::new(),
            JsonExecutionOptions::mcp(Some(2)),
            CancellationToken::new(),
        )
        .await
        .expect_err("depth-bound cursor");
    assert!(matches!(stale_depth, Error::StaleCursor));

    let legacy = services
        .json(JsonRequest {
            operation,
            max_tokens: Some(1_000),
            max_items: Some(2),
            array_sample_size: None,
            cursor: None,
        })
        .await
        .expect("legacy first page")
        .meta
        .next_cursor
        .expect("legacy cursor");
    let stale_legacy = services
        .json_cancellable_with_execution_options(
            JsonRequest {
                operation: JsonOperation::Query {
                    path: "report.json".into(),
                    selector: None,
                    projection: JsonProjection::Keys,
                },
                max_tokens: Some(1_000),
                max_items: Some(2),
                array_sample_size: None,
                cursor: Some(legacy),
            },
            ServiceCallOptions::new(),
            execution,
            CancellationToken::new(),
        )
        .await
        .expect_err("legacy cursor under depth ordering");
    assert!(matches!(stale_legacy, Error::StaleCursor));
}

#[test]
fn breadth_first_schema_preserves_complete_shape_and_shallow_siblings() {
    let value = json!({
        "a": {"deep": {"value": 1}},
        "b": true,
        "c": [],
    });
    let total = count_nodes(&value);
    let complete = build_schema_breadth_first(&value, total);
    assert_eq!(
        complete["properties"]["a"]["properties"]["deep"]["type"],
        "object"
    );

    let shallow = build_schema_breadth_first(&value, 4);
    let properties = shallow["properties"]
        .as_object()
        .expect("shallow properties");
    assert_eq!(properties.len(), 3);
    assert_eq!(properties["a"]["properties"], json!({}));
    assert_eq!(
        shallow["x-leantoken-incomplete"]["omitted_subtree_count"],
        1
    );
    assert_eq!(
        shallow["x-leantoken-incomplete"]["omitted_subtree_pointers"],
        json!(["/a"])
    );
}

#[test]
fn collapsed_projection_reports_actual_bounded_sample() {
    let value = json!([1, 2, 3, 4]);
    let mut state = ProjectionState::new(2, 3);
    let projected = collapse_json(&value, &mut state);

    assert!(!state.is_complete());
    assert_eq!(
        projected["$array"]["sample"].as_array().map(Vec::len),
        Some(1)
    );
    assert_eq!(projected["$array"]["omitted"], 3);
}

#[test]
fn schema_projection_plan_preserves_byte_for_byte_output_across_token_budgets() {
    let root = tempfile::tempdir().expect("root");
    let mut deep = json!(true);
    for index in (0..40).rev() {
        deep = json!({format!("level_{index:02}"): deep});
    }
    let mut fixture = serde_json::Map::new();
    fixture.insert("deep".into(), deep);
    fixture.insert("empty_array".into(), json!([]));
    fixture.insert("gate".into(), json!({"enabled": true, "mode": "strict"}));
    for index in 0..10 {
        fixture.insert(format!("top_{index:02}"), json!(index));
    }
    let fixture_value = Value::Object(fixture);
    std::fs::write(
        root.path().join("wide.json"),
        serde_json::to_vec_pretty(&fixture_value).expect("serialize fixture"),
    )
    .expect("write fixture");
    let config = crate::Config::discover(root.path(), Some(root.path().join("index.sqlite")))
        .expect("config");
    let services = Services::open(config).expect("services");

    // Full-budget projection: should be complete.
    let full = project_schema_page(&services, &fixture_value, 10_000, 32_000).expect("full schema");
    let (full_value, full_total, full_returned, full_remaining, full_reason, _full_tokens, _) =
        full.into_parts();
    assert_eq!(full_returned, full_total);
    assert_eq!(full_remaining, 0);
    assert!(full_reason.is_none());
    let full_serialized = serde_json::to_string(&full_value).expect("serialize full");

    // Token-limited projection: should be incomplete with fewer items.
    // Use a budget large enough to include some top-level keys but not all nodes.
    let partial =
        project_schema_page(&services, &fixture_value, 10_000, 200).expect("partial schema");
    let (
        partial_value,
        partial_total,
        partial_returned,
        partial_remaining,
        partial_reason,
        partial_tokens,
        _,
    ) = partial.into_parts();
    assert_eq!(partial_total, full_total);
    assert!(partial_returned < partial_total);
    assert!(partial_remaining > 0);
    assert_eq!(
        partial_reason,
        Some(crate::model::JsonIncompleteReason::MaxTokens)
    );
    assert!(partial_tokens <= 200);
    let partial_serialized = serde_json::to_string(&partial_value).expect("serialize partial");

    // The partial schema must carry omission markers.
    assert!(
        partial_value["x-leantoken-incomplete"]["omitted_subtree_count"]
            .as_u64()
            .is_some_and(|c| c > 0)
    );

    // Re-running with the same budget must produce identical bytes and tokens.
    let repeat =
        project_schema_page(&services, &fixture_value, 10_000, 200).expect("repeat partial");
    let (repeat_value, _, _, _, _, repeat_tokens, _) = repeat.into_parts();
    let repeat_serialized = serde_json::to_string(&repeat_value).expect("serialize repeat");
    assert_eq!(partial_serialized, repeat_serialized);
    assert_eq!(partial_tokens, repeat_tokens);

    // The full and partial serializations must differ (different item counts).
    assert_ne!(full_serialized, partial_serialized);
}

#[test]
fn schema_projection_plan_reuses_root_candidate_on_cache_hit() {
    let root = tempfile::tempdir().expect("root");
    // A fixture with enough nodes that the full schema is clearly larger than
    // the 1-node root schema (even with omission metadata). 12 top-level keys
    // + root = 13 nodes. The binary search with item_limit=13 and a budget
    // that fits only the root will revisit max_items=1, hitting the cache.
    let mut fixture = serde_json::Map::new();
    for index in 0..12 {
        fixture.insert(format!("key_{index:02}"), json!(index));
    }
    let fixture_value = Value::Object(fixture);
    std::fs::write(
        root.path().join("medium.json"),
        serde_json::to_vec_pretty(&fixture_value).expect("serialize"),
    )
    .expect("write fixture");
    let config = crate::Config::discover(root.path(), Some(root.path().join("index.sqlite")))
        .expect("config");
    let services = Services::open(config).expect("services");

    // With a token budget that fits the full 13-node schema, no binary search.
    let full = project_schema_page(&services, &fixture_value, 10_000, 32_000).expect("full");
    let (_, _, _, _, _, _, full_counters) = full.into_parts();
    assert_eq!(full_counters.schema_builds, 1);
    assert_eq!(full_counters.schema_serializations, 1);
    assert_eq!(full_counters.schema_token_counts, 1);
    assert_eq!(full_counters.cache_hits, 0);

    // Compute the root (1-node) token count. The root schema includes omission
    // metadata, so it may be larger than expected. Use it as the budget to
    // force the binary search to find that only the root fits.
    let root_schema = build_schema_breadth_first(&fixture_value, 1);
    let root_str = serde_json::to_string(&root_schema).expect("serialize root");
    let root_tokens = services.config.tokenizer.count(&root_str);

    // Verify the full schema is larger than the root (otherwise no binary search).
    let full_schema = build_schema_breadth_first(&fixture_value, 13);
    let full_str = serde_json::to_string(&full_schema).expect("serialize full");
    let full_tokens = services.config.tokenizer.count(&full_str);
    assert!(
        full_tokens > root_tokens,
        "full schema ({full_tokens} tokens) must exceed root ({root_tokens} tokens) for binary search"
    );

    let partial =
        project_schema_page(&services, &fixture_value, 10_000, root_tokens).expect("partial");
    let (_, _, _returned, _, _, _, partial_counters) = partial.into_parts();
    // The item_limit (13) and root (1) are built before the loop. The binary
    // search starts with lower=1, upper=12. When middle=1, it's a cache hit.
    assert!(
        partial_counters.cache_hits >= 1,
        "expected at least 1 cache hit, got {}",
        partial_counters.cache_hits
    );
    assert!(
        partial_counters.schema_builds >= 2,
        "expected at least 2 builds, got {}",
        partial_counters.schema_builds
    );
}

#[test]
fn schema_projection_counters_are_zero_for_empty_cache() {
    let counters = SchemaProjectionCounters::default();
    assert_eq!(counters.schema_builds, 0);
    assert_eq!(counters.schema_serializations, 0);
    assert_eq!(counters.schema_token_counts, 0);
    assert_eq!(counters.cache_hits, 0);
}
