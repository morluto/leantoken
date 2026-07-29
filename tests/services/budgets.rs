use super::*;


#[tokio::test]
async fn retrieval_call_options_enforce_final_service_response_bounds() {
    let (root, services) = fixture().await;
    std::fs::write(
        root.path().join("data.json"),
        r#"{"alpha":{"escaped":"line\nvalue"},"βeta":[1,2,3]}"#,
    )
    .expect("write JSON fixture");
    let options = ServiceCallOptions::new().with_max_response_tokens(32_000);

    let files = services
        .files_with_options(
            FilesRequest {
                operation: FileOperation::Tree,
                path: None,
                query: None,
                pattern: None,
                max_results: Some(10),
                cursor: None,
                depth: Some(2),
            },
            options,
        )
        .await
        .expect("bounded files");
    let search = services
        .search_with_options(
            SearchRequest {
                query: "greet".into(),
                mode: SearchMode::Auto,
                include_paths: Vec::new(),
                exclude_paths: Vec::new(),
                focus_paths: Vec::new(),
                max_results: Some(10),
                max_tokens: None,
                context_lines: None,
                case_sensitive: false,
                all_occurrences: false,
                prefer_structural: false,
                receipt_id: None,
                query_receipt: None,
                cursor: None,
            },
            options,
        )
        .await
        .expect("bounded search");
    let outline = services
        .outline_with_options(
            OutlineRequest {
                paths: vec!["src/lib.rs".into()],
                symbol_name: None,
                symbol_kind: None,
                max_results: Some(20),
                max_tokens: None,
                receipt_id: None,
                cursor: None,
            },
            options,
        )
        .await
        .expect("bounded outline");
    let read = services
        .read_with_options(
            ReadRequest {
                path: "src/lib.rs".into(),
                start_line: Some(1),
                end_line: Some(6),
                symbol: None,
                heading: None,
                heading_occurrence: None,
                continuation_cursor: None,
                max_tokens: None,
                expected_hash: None,
                delta: false,
                receipt_id: None,
            },
            options,
        )
        .await
        .expect("bounded read");
    let json = services
        .json_with_options(
            JsonRequest {
                operation: JsonOperation::Query {
                    path: "data.json".into(),
                    selector: None,
                    projection: JsonProjection::Keys,
                },
                max_tokens: Some(1_000),
                max_items: Some(100),
                array_sample_size: None,
                cursor: None,
            },
            options,
        )
        .await
        .expect("bounded JSON");

    for total in [
        files.meta.total_response_tokens,
        search.meta.total_response_tokens,
        outline.meta.total_response_tokens,
        read.meta.total_response_tokens,
        json.meta.total_response_tokens,
    ] {
        assert!(total <= 32_000);
    }
    for payload in [
        serde_json::to_string(&files).expect("serialize files"),
        serde_json::to_string(&search).expect("serialize search"),
        serde_json::to_string(&outline).expect("serialize outline"),
        serde_json::to_string(&read).expect("serialize read"),
        serde_json::to_string(&json).expect("serialize JSON"),
    ] {
        assert!(Tokenizer::default().count(&payload) <= 32_000);
    }

    let validation_request = FilesRequest {
        operation: FileOperation::Tree,
        path: None,
        query: None,
        pattern: None,
        max_results: Some(1),
        cursor: None,
        depth: None,
    };
    let invalid = services
        .files_with_options(
            validation_request.clone(),
            ServiceCallOptions::new().with_max_response_tokens(0),
        )
        .await
        .expect_err("zero response limit must fail before retrieval");
    assert!(matches!(
        invalid,
        Error::InvalidInput {
            field: "max_response_tokens",
            ..
        }
    ));
    let oversized = services
        .files_with_options(
            validation_request,
            ServiceCallOptions::new().with_max_response_tokens(32_001),
        )
        .await
        .expect_err("server maximum must apply to service callers");
    assert!(matches!(
        oversized,
        Error::RequestLimitExceeded {
            field: "max_response_tokens",
            requested: 32_001,
            limit: 32_000
        }
    ));
    let too_small = services
        .files_with_options(
            FilesRequest {
                operation: FileOperation::Tree,
                path: None,
                query: None,
                pattern: None,
                max_results: Some(1),
                cursor: None,
                depth: None,
            },
            ServiceCallOptions::new().with_max_response_tokens(1),
        )
        .await
        .expect_err("mandatory files skeleton must fail loudly");
    let _ = assert_response_budget_error(too_small, 1);
}

#[tokio::test]
async fn receipt_reserved_response_minimum_is_an_exact_retry_hint() {
    let (_root, services) = fixture().await;
    let search_request = SearchRequest {
        query: "greet".into(),
        mode: SearchMode::Identifier,
        include_paths: Vec::new(),
        exclude_paths: Vec::new(),
        focus_paths: Vec::new(),
        max_results: Some(5),
        max_tokens: Some(8_000),
        context_lines: Some(2),
        case_sensitive: false,
        all_occurrences: false,
        prefer_structural: false,
        receipt_id: None,
        query_receipt: None,
        cursor: None,
    };
    let (search_minimum, search_breakdown) = assert_response_budget_error(
        services
        .search_with_options(
            search_request.clone(),
            ServiceCallOptions::new().with_max_response_tokens(1),
        )
        .await
        .expect_err("one token cannot fit a search response"),
        1,
    );
    assert!(search_breakdown.receipt_reserve_tokens > 0);
    let retried_search = services
        .search_with_options(
            search_request.clone(),
            ServiceCallOptions::new().with_max_response_tokens(search_minimum),
        )
        .await
        .expect("reported search minimum must be directly retryable");
    assert!(retried_search.meta.total_response_tokens <= search_minimum);
    let (repeated_search_minimum, _) = assert_response_budget_error(
        services.search_with_options(
            search_request,
            ServiceCallOptions::new().with_max_response_tokens(search_minimum - 1),
        )
        .await
        .expect_err("one token below the search minimum must fail"),
        search_minimum - 1,
    );
    assert_eq!(repeated_search_minimum, search_minimum);

    let outline_request = OutlineRequest {
        paths: vec!["src/lib.rs".into()],
        symbol_name: None,
        symbol_kind: None,
        max_results: Some(20),
        max_tokens: Some(8_000),
        receipt_id: None,
        cursor: None,
    };
    let (outline_minimum, outline_breakdown) = assert_response_budget_error(
        services
        .outline_with_options(
            outline_request.clone(),
            ServiceCallOptions::new().with_max_response_tokens(1),
        )
        .await
        .expect_err("one token cannot fit an outline response"),
        1,
    );
    assert!(outline_breakdown.receipt_reserve_tokens > 0);
    let retried_outline = services
        .outline_with_options(
            outline_request.clone(),
            ServiceCallOptions::new().with_max_response_tokens(outline_minimum),
        )
        .await
        .expect("reported outline minimum must be directly retryable");
    assert!(retried_outline.meta.total_response_tokens <= outline_minimum);
    let (repeated_outline_minimum, _) = assert_response_budget_error(
        services.outline_with_options(
            outline_request,
            ServiceCallOptions::new().with_max_response_tokens(outline_minimum - 1),
        )
        .await
        .expect_err("one token below the outline minimum must fail"),
        outline_minimum - 1,
    );
    assert_eq!(repeated_outline_minimum, outline_minimum);
}

#[tokio::test]
async fn files_response_budget_uses_a_resumable_deterministic_prefix() {
    let (root, services) = fixture().await;
    for index in 0..24 {
        std::fs::write(
            root.path()
                .join("src")
                .join(format!("長い名前_{index:02}_escaped_quote.rs")),
            format!("pub const VALUE_{index}: usize = {index};\n"),
        )
        .expect("write path fixture");
    }
    services.index(false).await.expect("index added paths");
    let one_entry_request = FilesRequest {
        operation: FileOperation::Tree,
        path: Some("src".into()),
        query: None,
        pattern: None,
        max_results: Some(1),
        cursor: None,
        depth: Some(1),
    };
    let minimum = services
        .files(one_entry_request.clone())
        .await
        .expect("one-entry files page");
    let exact_limit = minimum.meta.total_response_tokens;
    let exact = services
        .files_with_options(
            one_entry_request.clone(),
            ServiceCallOptions::new().with_max_response_tokens(exact_limit),
        )
        .await
        .expect("exact one-entry response limit");
    assert_eq!(exact.entries.len(), 1);
    assert_eq!(exact.meta.total_response_tokens, exact_limit);
    let below_minimum = services
        .files_with_options(
            one_entry_request,
            ServiceCallOptions::new().with_max_response_tokens(exact_limit - 1),
        )
        .await
        .expect_err("one token below the resumable skeleton");
    let (reported_minimum, breakdown) =
        assert_response_budget_error(below_minimum, exact_limit - 1);
    assert_eq!(reported_minimum, exact_limit);
    assert_eq!(breakdown.receipt_reserve_tokens, 0);

    let request = FilesRequest {
        operation: FileOperation::Tree,
        path: Some("src".into()),
        query: None,
        pattern: None,
        max_results: Some(100),
        cursor: None,
        depth: Some(1),
    };
    let full = services.files(request.clone()).await.expect("full files page");
    assert!(full.entries.len() > 10);
    let limit = full.meta.total_response_tokens.saturating_sub(600);
    let bounded = services
        .files_with_options(
            request.clone(),
            ServiceCallOptions::new().with_max_response_tokens(limit),
        )
        .await
        .expect("fit files prefix");
    assert!(bounded.meta.total_response_tokens <= limit);
    assert!(bounded.entries.len() < full.entries.len());
    assert!(bounded.meta.next_cursor.is_some());
    assert_eq!(
        bounded
            .entries
            .iter()
            .map(|entry| &entry.path)
            .collect::<Vec<_>>(),
        full.entries[..bounded.entries.len()]
            .iter()
            .map(|entry| &entry.path)
            .collect::<Vec<_>>(),
        "fitting must preserve the original deterministic prefix"
    );

    let continuation = services
        .files(FilesRequest {
            cursor: bounded.meta.next_cursor.clone(),
            ..request
        })
        .await
        .expect("continue fitted files page");
    assert_eq!(
        continuation.entries.first().map(|entry| &entry.path),
        full.entries
            .get(bounded.entries.len())
            .map(|entry| &entry.path)
    );
}

#[tokio::test]
async fn json_keys_response_budget_preserves_cursor_completeness() {
    let (root, services) = fixture().await;
    let object = (0..80)
        .map(|index| {
            (
                format!("escaped_長い_key_{index:03}"),
                serde_json::json!({"nested": index}),
            )
        })
        .collect::<serde_json::Map<_, _>>();
    std::fs::write(
        root.path().join("keys.json"),
        serde_json::to_vec(&serde_json::Value::Object(object)).expect("serialize fixture"),
    )
    .expect("write JSON fixture");
    let request = JsonRequest {
        operation: JsonOperation::Query {
            path: "keys.json".into(),
            selector: None,
            projection: JsonProjection::Keys,
        },
        max_tokens: Some(8_000),
        max_items: Some(1_000),
        array_sample_size: None,
        cursor: None,
    };
    let full = services.json(request.clone()).await.expect("full keys page");
    let full_items = full.returned_items.expect("keys item count");
    assert!(full_items > 50);
    let limit = full.meta.total_response_tokens.saturating_sub(600);
    let bounded = services
        .json_with_options(
            request.clone(),
            ServiceCallOptions::new().with_max_response_tokens(limit),
        )
        .await
        .expect("fit keys page");
    assert!(bounded.meta.total_response_tokens <= limit);
    assert!(bounded.returned_items.expect("bounded item count") < full_items);
    assert!(!bounded.result_complete);
    assert_eq!(
        bounded.incomplete_reason,
        Some(JsonIncompleteReason::MaxTokens)
    );
    let cursor = bounded.meta.next_cursor.clone().expect("continuation cursor");
    let continuation = services
        .json(JsonRequest {
            cursor: Some(cursor),
            ..request
        })
        .await
        .expect("continue keys page");
    assert_eq!(
        bounded
            .returned_items
            .expect("bounded item count")
            .saturating_add(continuation.remaining_items.unwrap_or_default())
            .saturating_add(continuation.returned_items.unwrap_or_default()),
        full.total_items.expect("total keys"),
    );
}

#[tokio::test]
async fn read_response_budget_reduces_source_without_skipping_continuation() {
    let (root, services) = fixture().await;
    let source = (1..=120)
        .map(|line| format!("pub const 長い名前_{line:03}: &str = \"escaped-{line}\";\n"))
        .collect::<String>();
    std::fs::write(root.path().join("src/big.rs"), source).expect("write read fixture");
    services.index(false).await.expect("index read fixture");
    let request = ReadRequest {
        path: "src/big.rs".into(),
        start_line: Some(1),
        end_line: Some(120),
        symbol: None,
        heading: None,
        heading_occurrence: None,
        continuation_cursor: None,
        max_tokens: Some(2_000),
        expected_hash: None,
        delta: false,
        receipt_id: None,
    };
    let full = services.read(request.clone()).await.expect("full read");
    let limit = full.meta.total_response_tokens.saturating_sub(500);
    let bounded = services
        .read_with_options(
            request,
            ServiceCallOptions::new().with_max_response_tokens(limit),
        )
        .await
        .expect("fit read response");
    assert!(bounded.meta.total_response_tokens <= limit);
    assert!(bounded.truncated);
    let next_start_line = bounded.next_start_line.expect("next line");
    let cursor = bounded
        .continuation_cursor
        .clone()
        .expect("continuation cursor");
    let continuation = services
        .read(ReadRequest {
            path: "src/big.rs".into(),
            start_line: None,
            end_line: None,
            symbol: None,
            heading: None,
            heading_occurrence: None,
            continuation_cursor: Some(cursor),
            max_tokens: Some(2_000),
            expected_hash: None,
            delta: false,
            receipt_id: None,
        })
        .await
        .expect("continue bounded read");
    assert_eq!(continuation.returned_start_line, next_start_line);
}
