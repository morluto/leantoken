//! Colocated JSON invariant tests, adapted to the decomposed submodules.

use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use super::collapsed::collapse_json;
use super::cursor::{decode_json_cursor, json_query_hash, make_json_cursor};
use super::execution::JsonKeyOrder;
use super::keys::key_entries;
use super::projection::{ProjectionState, count_nodes, project_json};
use super::schema::{build_schema_breadth_first, project_schema_page};
use super::source::{JsonMeasurementCache, JsonMeasurementKey};
use super::validation::parse_json_request;
use super::{JsonExecutionOptions, MAX_JSON_DEPTH};
use crate::Error;
use crate::model::{JsonOperation, JsonProjection, JsonRequest, JsonSelector};
use crate::services::cursor::{CursorKind, StreamIdentityBuilder};
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
fn key_cursors_bind_depth_and_reject_another_ordering() {
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
    let stream = |query_hash: &str| {
        let mut stream = StreamIdentityBuilder::new(CursorKind::JsonKeys);
        stream.field_str("query_hash", query_hash);
        stream.finish()
    };
    let shallow_stream = stream(&shallow);
    let cursor = make_json_cursor(shallow_stream, &source, 1).expect("cursor");
    let decoded = decode_json_cursor(&cursor).expect("decode cursor");
    assert_eq!(
        decoded
            .offset_for(&source, shallow_stream)
            .expect("matching stream"),
        1
    );
    assert!(matches!(
        decoded.offset_for(&source, stream(&deep)),
        Err(Error::StaleCursor)
    ));
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
        parse_json_request(
            keys.clone(),
            JsonExecutionOptions::mcp(Some(MAX_JSON_DEPTH + 1))
        ),
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
        parse_json_request(value, JsonExecutionOptions::mcp(Some(1))),
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
            request.cursor = Some(cursor);
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
    let (full_value, full_total, full_returned, full_remaining, full_reason, _full_tokens) =
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
    let (repeat_value, _, _, _, _, repeat_tokens) = repeat.into_parts();
    let repeat_serialized = serde_json::to_string(&repeat_value).expect("serialize repeat");
    assert_eq!(partial_serialized, repeat_serialized);
    assert_eq!(partial_tokens, repeat_tokens);

    // The full and partial serializations must differ (different item counts).
    assert_ne!(full_serialized, partial_serialized);
}

#[test]
fn json_measurement_cache_preserves_exact_tokens_for_distinct_values() {
    let root = tempfile::tempdir().expect("root");
    let config = crate::Config::discover(root.path(), Some(root.path().join("index.sqlite")))
        .expect("config");
    let services = Services::open(config).expect("services");
    let value = json!([{"pointer": "/src/lib.rs", "type": "file"}]);
    let mut cache = JsonMeasurementCache::default();
    let first = cache
        .measure(&services, JsonMeasurementKey::KeysPrefix(1), &value)
        .expect("first measurement");
    let second = cache
        .measure(&services, JsonMeasurementKey::KeysPrefix(1), &value)
        .expect("cached measurement");
    assert_eq!(first, second);
    assert_eq!(
        first,
        services
            .config
            .tokenizer
            .count(&serde_json::to_string(&value).expect("serialize"))
    );
    let different_value = json!([{
        "pointer": "/src/main.rs",
        "type": "file",
        "details": "bounded response accounting ".repeat(64),
    }]);
    let different_tokens = cache
        .measure(
            &services,
            JsonMeasurementKey::KeysPrefix(1),
            &different_value,
        )
        .expect("different measurement");
    assert_eq!(
        different_tokens,
        services
            .config
            .tokenizer
            .count(&serde_json::to_string(&different_value).expect("serialize different value"))
    );
    assert!(different_tokens > first);
}

#[tokio::test]
async fn schema_diff_does_not_bypass_max_items_when_budget_is_exhausted() {
    let root = tempfile::tempdir().expect("root");
    let base = json!({
        "alpha": {"deep": {"nested": {"value": 1}}},
        "beta": {"deep": {"nested": {"value": 2}}},
    });
    let head = json!({
        "alpha": {"deep": {"nested": {"value": 3}}},
        "beta": {"deep": {"nested": {"value": 4}}},
    });
    std::fs::write(
        root.path().join("base.json"),
        serde_json::to_vec(&base).expect("serialize base"),
    )
    .expect("write base fixture");
    std::fs::write(
        root.path().join("head.json"),
        serde_json::to_vec(&head).expect("serialize head"),
    )
    .expect("write head fixture");
    let config = crate::Config::discover(root.path(), Some(root.path().join("index.sqlite")))
        .expect("config");
    let services = Services::open(config).expect("services");
    let response = services
        .json(JsonRequest {
            operation: JsonOperation::DiffFields {
                base_path: "base.json".into(),
                head_path: "head.json".into(),
                selectors: vec![
                    JsonSelector::Pointer {
                        pointer: "/alpha".into(),
                    },
                    JsonSelector::Pointer {
                        pointer: "/beta".into(),
                    },
                ],
                projection: JsonProjection::Schema,
            },
            max_tokens: Some(10_000),
            max_items: Some(3),
            array_sample_size: None,
            cursor: None,
        })
        .await
        .expect("schema diff");
    assert!(response.differences.len() == 2);
    // The first selector exhausts the item budget; the second selector
    // must not get a 1-item schema projection from the exhausted budget.
    let second_before = response.differences[1].before.as_ref();
    let second_after = response.differences[1].after.as_ref();
    assert!(
        second_before.is_none() || second_after.is_none(),
        "second selector should not get schema projections when budget is exhausted"
    );
    assert!(!response.result_complete);
}
