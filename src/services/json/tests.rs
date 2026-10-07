//! Colocated JSON invariant tests, adapted to the decomposed submodules.

use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use super::execution::JsonKeyOrder;
use super::keys::key_entries;
use super::projection::{ProjectionState, project_json};
use super::validation::parse_json_request;
use super::{JsonExecutionOptions, MAX_JSON_DEPTH};
use crate::Error;
use crate::model::{JsonOperation, JsonProjection, JsonRequest};
use crate::services::{ServiceCallOptions, Services};

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

    let mut request = JsonRequest {
        operation,
        max_tokens: Some(1_000),
        max_items: Some(2),
        array_sample_size: None,
        cursor: None,
    };
    let pointer_page = services
        .json(request.clone())
        .await
        .expect("pointer ordered page");
    request.cursor = Some(pointer_page.meta.next_cursor.expect("continuation"));
    let changed_order = services
        .json_cancellable_with_execution_options(
            request,
            ServiceCallOptions::new(),
            JsonExecutionOptions::mcp(None),
            CancellationToken::new(),
        )
        .await
        .expect_err("cursor cannot switch ordering between adapters");
    assert!(matches!(changed_order, Error::StaleCursor));
}
