use super::*;

#[tokio::test]
async fn json_structural_queries_summarize_ignored_artifacts_and_diff_fields() {
    let root = tempfile::tempdir().expect("root");
    std::fs::create_dir(root.path().join("artifacts")).expect("artifact directory");
    std::fs::write(root.path().join(".gitignore"), "artifacts/\n").expect("ignore file");
    std::fs::write(
        root.path().join("artifacts/before.json"),
        r#"{"graph_index":{"corpora":[{"cold_index_ms":1},{"cold_index_ms":2},{"cold_index_ms":3},{"cold_index_ms":100}]},"runs":[{"name":"a"},{"name":"b"},{"name":"c"},{"name":"d"}],"version":1}"#,
    )
    .expect("base JSON");
    std::fs::write(
        root.path().join("artifacts/after.json"),
        r#"{"graph_index":{"corpora":[{"cold_index_ms":2},{"cold_index_ms":4}]},"runs":[{"name":"a"},{"name":"b"}],"status":"done"}"#,
    )
    .expect("head JSON");
    let config =
        Config::discover(root.path(), Some(root.path().join("index.sqlite"))).expect("config");
    let services = Services::open(config).expect("services");

    let query = services
        .json(JsonRequest {
            operation: JsonOperation::Query {
                path: "artifacts/before.json".into(),
                selector: Some(JsonSelector::Pointer {
                    pointer: "/version".into(),
                }),
                projection: JsonProjection::Value,
            },
            max_tokens: Some(100),
            max_items: None,
            array_sample_size: None,
            cursor: None,
        })
        .await
        .expect("pointer query");
    assert_eq!(query.value, Some(serde_json::json!(1)));
    assert_eq!(query.sources[0].path, "artifacts/before.json");
    assert_response_token_accounting!(query, Tokenizer::default());

    let collapsed = services
        .json(JsonRequest {
            operation: JsonOperation::Query {
                path: "artifacts/before.json".into(),
                selector: Some(JsonSelector::Jmespath {
                    expression: "runs".into(),
                }),
                projection: JsonProjection::Collapsed,
            },
            max_tokens: Some(500),
            max_items: Some(100),
            array_sample_size: Some(1),
            cursor: None,
        })
        .await
        .expect("collapsed JMESPath query");
    assert_eq!(collapsed.value.as_ref().expect("value")["$array"]["count"], 4);
    assert_eq!(
        collapsed.value.as_ref().expect("value")["$array"]["sample"]
            .as_array()
            .expect("sample")
            .len(),
        1
    );

    for projection in [JsonProjection::Keys, JsonProjection::Schema] {
        let projected = services
            .json(JsonRequest {
                operation: JsonOperation::Query {
                    path: "artifacts/before.json".into(),
                    selector: None,
                    projection,
                },
                max_tokens: Some(1_000),
                max_items: Some(100),
                array_sample_size: None,
                cursor: None,
            })
            .await
            .expect("structural projection");
        assert!(projected.value.is_some());
        assert!(projected.result_complete);
    }

    let summary = services
        .json(JsonRequest {
            operation: JsonOperation::NumericSummary {
                path: "artifacts/before.json".into(),
                selector: Some(JsonSelector::Jmespath {
                    expression: "graph_index.corpora[].cold_index_ms".into(),
                }),
            },
            max_tokens: None,
            max_items: None,
            array_sample_size: None,
            cursor: None,
        })
        .await
        .expect("numeric summary");
    let statistics = summary.numeric_summary.expect("statistics");
    assert_eq!(statistics.count, 4);
    assert_eq!(statistics.min, Some(1.0));
    assert_eq!(statistics.median, Some(2.5));
    assert_eq!(statistics.p95, Some(100.0));
    assert_eq!(statistics.max, Some(100.0));

    let diff = services
        .json(JsonRequest {
            operation: JsonOperation::DiffFields {
                base_path: "artifacts/before.json".into(),
                head_path: "artifacts/after.json".into(),
                selectors: vec![
                    JsonSelector::Pointer {
                        pointer: "/version".into(),
                    },
                    JsonSelector::Pointer {
                        pointer: "/status".into(),
                    },
                    JsonSelector::Jmespath {
                        expression: "graph_index.corpora[].cold_index_ms".into(),
                    },
                ],
                projection: JsonProjection::Collapsed,
            },
            max_tokens: Some(1_000),
            max_items: Some(100),
            array_sample_size: Some(2),
            cursor: None,
        })
        .await
        .expect("selected-field diff");
    assert_eq!(diff.differences.len(), 3);
    assert!(diff.differences.iter().all(|field| field.changed));
    assert!(diff.differences[0].before_present);
    assert!(!diff.differences[0].after_present);
    assert!(!diff.differences[1].before_present);
    assert!(diff.differences[1].after_present);
    assert_response_token_accounting!(diff, Tokenizer::default());
    let report = services
        .token_savings_report()
        .await
        .expect("JSON response accounting");
    let json = report
        .response_accounting
        .by_operation
        .iter()
        .find(|row| row.operation == TokenAccountingOperation::Json)
        .expect("JSON accounting row");
    assert_eq!(json.tracked_requests, 6);
    assert_eq!(json.baseline_requests, 6);
    assert!(json.baseline_source_tokens > json.response_source_tokens);
    assert!(json.total_response_tokens >= json.response_source_tokens);
    assert_eq!(
        json.estimated_net_tokens_saved,
        i64::try_from(json.baseline_source_tokens).expect("small JSON baseline")
            - i64::try_from(json.total_response_tokens).expect("small JSON responses")
    );
}

#[tokio::test]
async fn json_keys_paginate_by_item_and_token_limits_with_exact_diagnostics() {
    let root = tempfile::tempdir().expect("root");
    let path = root.path().join("report.json");
    std::fs::write(
        &path,
        r#"{"alpha":1,"beta":2,"nested":{"first":3,"second":4},"rows":[{"left":5},{"right":6}]}"#,
    )
    .expect("JSON fixture");
    let config =
        Config::discover(root.path(), Some(root.path().join("index.sqlite"))).expect("config");
    let services = Services::open(config).expect("services");
    let operation = JsonOperation::Query {
        path: "report.json".into(),
        selector: None,
        projection: JsonProjection::Keys,
    };

    let complete = services
        .json(JsonRequest {
            operation: operation.clone(),
            max_tokens: Some(1_000),
            max_items: Some(100),
            array_sample_size: None,
            cursor: None,
        })
        .await
        .expect("complete keys");
    let expected = complete
        .value
        .as_ref()
        .and_then(serde_json::Value::as_array)
        .expect("key array")
        .clone();
    assert!(complete.result_complete);
    assert_eq!(complete.total_items, Some(expected.len()));
    assert_eq!(complete.returned_items, Some(expected.len()));
    assert_eq!(complete.remaining_items, Some(0));
    assert_eq!(complete.incomplete_reason, None);
    assert!(complete.meta.next_cursor.is_none());

    let mut cursor = None;
    let mut observed = Vec::new();
    let mut previous_remaining = expected.len();
    loop {
        let page = services
            .json(JsonRequest {
                operation: operation.clone(),
                max_tokens: Some(1_000),
                max_items: Some(2),
                array_sample_size: None,
                cursor,
            })
            .await
            .expect("keys page");
        let page_values = page
            .value
            .as_ref()
            .and_then(serde_json::Value::as_array)
            .expect("page values");
        assert_eq!(page.total_items, Some(expected.len()));
        assert_eq!(page.returned_items, Some(page_values.len()));
        assert!(page_values.len() <= 2);
        observed.extend(page_values.iter().cloned());
        let remaining = page.remaining_items.expect("remaining count");
        assert_eq!(remaining, expected.len().saturating_sub(observed.len()));
        assert!(remaining <= previous_remaining);
        previous_remaining = remaining;
        if page.result_complete {
            assert_eq!(page.incomplete_reason, None);
            assert!(page.meta.next_cursor.is_none());
            break;
        }
        assert_eq!(
            page.incomplete_reason,
            Some(JsonIncompleteReason::MaxItems)
        );
        cursor = page.meta.next_cursor;
        assert!(cursor.is_some());
    }
    assert_eq!(observed, expected);

    let one_item = services
        .json(JsonRequest {
            operation: operation.clone(),
            max_tokens: Some(1_000),
            max_items: Some(1),
            array_sample_size: None,
            cursor: None,
        })
        .await
        .expect("one key");
    let token_limited = services
        .json(JsonRequest {
            operation,
            max_tokens: Some(one_item.meta.source_tokens),
            max_items: Some(100),
            array_sample_size: None,
            cursor: None,
        })
        .await
        .expect("token-limited key page");
    assert_eq!(token_limited.returned_items, Some(1));
    assert_eq!(
        token_limited.incomplete_reason,
        Some(JsonIncompleteReason::MaxTokens)
    );
    assert!(token_limited.meta.source_tokens <= one_item.meta.source_tokens);
    assert!(token_limited.meta.next_cursor.is_some());
    assert_response_token_accounting!(token_limited, Tokenizer::default());
}

#[tokio::test]
async fn json_schema_degrades_breadth_first_under_token_limits() {
    let root = tempfile::tempdir().expect("root");
    let mut deep = serde_json::json!(true);
    for index in (0..80).rev() {
        deep = serde_json::json!({format!("level_{index:02}"): deep});
    }
    let mut fixture = serde_json::Map::new();
    fixture.insert("deep".into(), deep);
    fixture.insert("empty_array".into(), serde_json::json!([]));
    fixture.insert("empty_object".into(), serde_json::json!({}));
    fixture.insert(
        "gate".into(),
        serde_json::json!({"enabled": true, "mode": "strict"}),
    );
    for index in 0..16 {
        fixture.insert(format!("top_{index:02}"), serde_json::json!(index));
    }
    std::fs::write(
        root.path().join("wide.json"),
        serde_json::to_vec(&serde_json::Value::Object(fixture)).expect("serialize fixture"),
    )
    .expect("write fixture");
    let config =
        Config::discover(root.path(), Some(root.path().join("index.sqlite"))).expect("config");
    let services = Services::open(config).expect("services");
    let operation = JsonOperation::Query {
        path: "wide.json".into(),
        selector: None,
        projection: JsonProjection::Schema,
    };

    let full = services
        .json(JsonRequest {
            operation: operation.clone(),
            max_tokens: Some(32_000),
            max_items: Some(10_000),
            array_sample_size: None,
            cursor: None,
        })
        .await
        .expect("complete schema");
    assert!(full.result_complete);
    assert!(
        full.value
            .as_ref()
            .expect("complete schema value")
            .get("x-leantoken-incomplete")
            .is_none()
    );
    let partial_limit = full.meta.source_tokens.saturating_sub(1).max(1);
    let partial = services
        .json(JsonRequest {
            operation,
            max_tokens: Some(partial_limit),
            max_items: Some(10_000),
            array_sample_size: None,
            cursor: None,
        })
        .await
        .expect("token-bounded schema");

    assert!(!partial.result_complete);
    assert_eq!(
        partial.incomplete_reason,
        Some(JsonIncompleteReason::MaxTokens)
    );
    assert!(partial.meta.source_tokens <= partial_limit);
    assert!(partial.meta.next_cursor.is_none());
    assert!(
        partial
            .remaining_items
            .is_some_and(|remaining| remaining > 0)
    );
    let partial_value = partial.value.as_ref().expect("partial schema value");
    let properties = partial_value["properties"]
        .as_object()
        .expect("partial top-level properties");
    for key in [
        "deep",
        "empty_array",
        "empty_object",
        "gate",
        "top_00",
        "top_15",
    ] {
        assert!(properties.contains_key(key), "missing shallow key {key}");
    }
    assert!(
        partial_value["x-leantoken-incomplete"]["omitted_subtree_count"]
            .as_u64()
            .is_some_and(|count| count > 0)
    );
    assert!(
        partial_value["x-leantoken-incomplete"]["omitted_subtree_pointers"]
            .as_array()
            .is_some_and(|pointers| !pointers.is_empty())
    );
    assert_response_token_accounting!(partial, Tokenizer::default());

    let exact = services
        .json(JsonRequest {
            operation: JsonOperation::Query {
                path: "wide.json".into(),
                selector: Some(JsonSelector::Pointer {
                    pointer: "/gate".into(),
                }),
                projection: JsonProjection::Schema,
            },
            max_tokens: Some(100),
            max_items: Some(100),
            array_sample_size: None,
            cursor: None,
        })
        .await
        .expect("exact selector schema");
    assert!(exact.result_complete);
    assert_eq!(
        exact.value.as_ref().expect("exact schema")["properties"]["enabled"]["type"],
        "boolean"
    );
}

#[tokio::test]
async fn compact_response_projections_preserve_verifiable_coverage_and_reduce_tokens() {
    let root = tempfile::tempdir().expect("root");
    std::fs::create_dir(root.path().join("src")).expect("create src");
    let callers = (0..24)
        .map(|index| {
            format!(
                "pub fn caller_{index:02}() -> usize {{\n    target()\n}}\n\n"
            )
        })
        .collect::<String>();
    std::fs::write(
        root.path().join("src/lib.rs"),
        format!(
            "pub fn target() -> usize {{\n    42\n}}\n\n{callers}"
        ),
    )
    .expect("write primary source");
    std::fs::write(
        root.path().join("src/other.rs"),
        "use crate::target;\n\npub fn indirect() -> usize {\n    target()\n}\n",
    )
    .expect("write secondary source");
    let config =
        Config::discover(root.path(), Some(root.path().join("index.sqlite"))).expect("config");
    let services = Services::open(config).expect("services");
    services.index(false).await.expect("index fixture");

    let files_request = FilesRequest {
        operation: FileOperation::Find,
        path: None,
        query: Some("src".into()),
        pattern: None,
        max_results: Some(100),
        cursor: None,
        depth: None,
    };
    let full_files = services
        .files(files_request.clone())
        .await
        .expect("full files");
    let compact_files = services
        .files_paths(files_request)
        .await
        .expect("path-only files");
    assert_eq!(
        compact_files.paths,
        full_files
            .entries
            .iter()
            .map(|entry| entry.path.clone())
            .collect::<Vec<_>>()
    );
    assert!(
        compact_files.meta.total_response_tokens < full_files.meta.total_response_tokens,
        "path-only projection must reduce the complete serialized response"
    );
    assert_response_token_accounting!(compact_files, Tokenizer::default());

    let outline_request = OutlineRequest {
        paths: vec!["src/lib.rs".into(), "src/other.rs".into()],
        symbol_name: None,
        symbol_kind: None,
        max_results: Some(100),
        max_tokens: Some(32_000),
        receipt_id: None,
        cursor: None,
    };
    let full_outline = services
        .outline(outline_request.clone())
        .await
        .expect("full outline");
    let compact_outline = services
        .outline_signatures(outline_request)
        .await
        .expect("signature-only outline");
    let full_symbols = full_outline
        .files
        .iter()
        .flat_map(|file| {
            file.symbols.iter().map(|symbol| {
                (
                    file.path.clone(),
                    symbol.name.clone(),
                    symbol.kind.clone(),
                    symbol.parent.clone(),
                    symbol.signature.clone(),
                    symbol.start_line,
                    symbol.end_line,
                )
            })
        })
        .collect::<Vec<_>>();
    let compact_symbols = compact_outline
        .files
        .iter()
        .flat_map(|file| {
            assert_eq!(
                file.content_hash,
                leantoken::text::hash(
                    &serde_json::to_string(&file.signatures)
                        .expect("serialize compact signatures")
                )
            );
            file.signatures.iter().map(|symbol| {
                (
                    file.path.clone(),
                    symbol.name.clone(),
                    symbol.kind.clone(),
                    symbol.parent.clone(),
                    symbol.signature.clone(),
                    symbol.start_line,
                    symbol.end_line,
                )
            })
        })
        .collect::<Vec<_>>();
    assert_eq!(compact_symbols, full_symbols);
    assert_eq!(compact_outline.total_symbols, full_outline.total_symbols);
    assert_eq!(
        compact_outline.returned_symbols,
        full_outline.returned_symbols
    );
    assert_eq!(compact_outline.parse_complete, full_outline.parse_complete);
    assert!(
        compact_outline.meta.total_response_tokens < full_outline.meta.total_response_tokens,
        "signature projection must reduce the complete serialized response"
    );
    let compact_outline_json =
        serde_json::to_string(&compact_outline).expect("serialize compact outline");
    assert!(!compact_outline_json.contains("start_byte"));
    assert!(!compact_outline_json.contains("\"imports\""));
    assert_response_token_accounting!(compact_outline, Tokenizer::default());

    let search_request = SearchRequest {
        query: "target".into(),
        mode: SearchMode::Auto,
        include_paths: Vec::new(),
        exclude_paths: Vec::new(),
        focus_paths: Vec::new(),
        max_results: Some(100),
        max_tokens: Some(32_000),
        context_lines: Some(0),
        case_sensitive: false,
        all_occurrences: false,
        prefer_structural: true,
        receipt_id: None,
        query_receipt: None,
        cursor: None,
    };
    let full_search = services
        .search(search_request.clone())
        .await
        .expect("full search");
    let compact_search = services
        .search_grouped(search_request)
        .await
        .expect("grouped search");
    assert_eq!(
        compact_search
            .groups
            .iter()
            .map(|group| group.total_hits)
            .sum::<usize>(),
        full_search.hits.len()
    );
    assert!(
        compact_search
            .groups
            .iter()
            .any(|group| group.definition.is_some()),
        "grouped search must retain the exact definition"
    );
    let expected_references = full_search
        .hits
        .iter()
        .filter(|hit| {
            hit.role == Some(leantoken::ReferenceRole::Reference)
                || hit.match_kinds.iter().any(|kind| kind == "reference")
        })
        .count();
    assert_eq!(
        compact_search
            .groups
            .iter()
            .flat_map(|group| &group.references)
            .map(|references| references.count)
            .sum::<usize>(),
        expected_references
    );
    assert_eq!(compact_search.coverage, full_search.coverage);
    assert!(
        compact_search.meta.total_response_tokens < full_search.meta.total_response_tokens,
        "grouped search must reduce the complete serialized response"
    );
    let compact_search_json =
        serde_json::to_string(&compact_search).expect("serialize grouped search");
    assert!(!compact_search_json.contains("\"score\""));
    assert!(!compact_search_json.contains("score_reasons"));
    assert_response_token_accounting!(compact_search, Tokenizer::default());

    let mut files_page_request = FilesRequest {
        operation: FileOperation::Find,
        path: None,
        query: Some("src".into()),
        pattern: None,
        max_results: Some(1),
        cursor: None,
        depth: None,
    };
    let mut paged_paths = Vec::new();
    loop {
        let page = services
            .files_paths(files_page_request.clone())
            .await
            .expect("path-only page");
        paged_paths.extend(page.paths);
        let Some(cursor) = page.meta.next_cursor else {
            break;
        };
        files_page_request.cursor = Some(cursor);
    }
    assert_eq!(paged_paths, compact_files.paths);

    let outline_page_request = OutlineRequest {
        paths: vec!["src/lib.rs".into(), "src/other.rs".into()],
        symbol_name: None,
        symbol_kind: None,
        max_results: Some(5),
        max_tokens: Some(32_000),
        receipt_id: None,
        cursor: None,
    };
    let full_outline_cursor = services
        .outline(outline_page_request.clone())
        .await
        .expect("full outline page")
        .meta
        .next_cursor
        .expect("full outline continuation");
    let stale_projection = services
        .outline_signatures(OutlineRequest {
            cursor: Some(full_outline_cursor),
            ..outline_page_request.clone()
        })
        .await
        .expect_err("projection-bound outline cursor");
    assert!(matches!(stale_projection, Error::StaleCursor));

    let mut outline_page_request = outline_page_request;
    let mut paged_signatures = Vec::new();
    loop {
        let page = services
            .outline_signatures(outline_page_request.clone())
            .await
            .expect("signature outline page");
        paged_signatures.extend(page.files.iter().flat_map(|file| {
            file.signatures.iter().map(|symbol| {
                (
                    file.path.clone(),
                    symbol.name.clone(),
                    symbol.kind.clone(),
                    symbol.parent.clone(),
                    symbol.signature.clone(),
                    symbol.start_line,
                    symbol.end_line,
                )
            })
        }));
        let Some(cursor) = page.meta.next_cursor else {
            break;
        };
        outline_page_request.cursor = Some(cursor);
    }
    assert_eq!(paged_signatures, compact_symbols);

    let paged_search_request = SearchRequest {
        query: "target".into(),
        mode: SearchMode::Auto,
        include_paths: Vec::new(),
        exclude_paths: Vec::new(),
        focus_paths: Vec::new(),
        max_results: Some(4),
        max_tokens: Some(32_000),
        context_lines: Some(0),
        case_sensitive: false,
        all_occurrences: false,
        prefer_structural: true,
        receipt_id: None,
        query_receipt: None,
        cursor: None,
    };
    let mut full_page_request = paged_search_request.clone();
    let mut full_paged_hits = 0usize;
    loop {
        let page = services
            .search(full_page_request.clone())
            .await
            .expect("full search page");
        full_paged_hits = full_paged_hits.saturating_add(page.hits.len());
        let Some(cursor) = page.meta.next_cursor else {
            break;
        };
        full_page_request.cursor = Some(cursor);
    }
    let mut grouped_page_request = paged_search_request;
    let mut grouped_paged_hits = 0usize;
    loop {
        let page = services
            .search_grouped(grouped_page_request.clone())
            .await
            .expect("grouped search page");
        grouped_paged_hits = grouped_paged_hits.saturating_add(
            page.groups
                .iter()
                .map(|group| group.total_hits)
                .sum::<usize>(),
        );
        let Some(cursor) = page.meta.next_cursor else {
            break;
        };
        grouped_page_request.cursor = Some(cursor);
    }
    assert_eq!(grouped_paged_hits, full_paged_hits);

    let bounded_files = services
        .files_paths_with_options(
            FilesRequest {
                operation: FileOperation::Find,
                path: None,
                query: Some("src".into()),
                pattern: None,
                max_results: Some(100),
                cursor: None,
                depth: None,
            },
            ServiceCallOptions::new()
                .with_max_response_tokens(compact_files.meta.total_response_tokens),
        )
        .await
        .expect("exact path-only response bound");
    assert!(
        bounded_files.meta.total_response_tokens
            <= compact_files.meta.total_response_tokens
    );
    let bounded_outline = services
        .outline_signatures_with_options(
            OutlineRequest {
                paths: vec!["src/lib.rs".into(), "src/other.rs".into()],
                symbol_name: None,
                symbol_kind: None,
                max_results: Some(100),
                max_tokens: Some(32_000),
                receipt_id: None,
                cursor: None,
            },
            ServiceCallOptions::new()
                .with_max_response_tokens(compact_outline.meta.total_response_tokens),
        )
        .await
        .expect("exact signature response bound");
    assert!(
        bounded_outline.meta.total_response_tokens
            <= compact_outline.meta.total_response_tokens
    );
    let bounded_search = services
        .search_grouped_with_options(
            SearchRequest {
                query: "target".into(),
                mode: SearchMode::Auto,
                include_paths: Vec::new(),
                exclude_paths: Vec::new(),
                focus_paths: Vec::new(),
                max_results: Some(100),
                max_tokens: Some(32_000),
                context_lines: Some(0),
                case_sensitive: false,
                all_occurrences: false,
                prefer_structural: true,
                receipt_id: None,
                query_receipt: None,
                cursor: None,
            },
            ServiceCallOptions::new()
                .with_max_response_tokens(compact_search.meta.total_response_tokens),
        )
        .await
        .expect("exact grouped response bound");
    assert!(
        bounded_search.meta.total_response_tokens
            <= compact_search.meta.total_response_tokens
    );
}

#[tokio::test]
async fn json_cursors_and_incomplete_results_fail_loud_with_typed_diagnostics() {
    let root = tempfile::tempdir().expect("root");
    let path = root.path().join("report.json");
    std::fs::write(&path, r#"{"version":1,"nested":{"answer":42},"tail":true}"#)
        .expect("JSON fixture");
    let config =
        Config::discover(root.path(), Some(root.path().join("index.sqlite"))).expect("config");
    let services = Services::open(config).expect("services");
    let operation = JsonOperation::Query {
        path: "report.json".into(),
        selector: None,
        projection: JsonProjection::Keys,
    };
    let first = services
        .json(JsonRequest {
            operation: operation.clone(),
            max_tokens: Some(1_000),
            max_items: Some(1),
            array_sample_size: None,
            cursor: None,
        })
        .await
        .expect("first page");
    let cursor = first.meta.next_cursor.expect("continuation cursor");

    let mismatched_query = services
        .json(JsonRequest {
            operation: JsonOperation::Query {
                path: "report.json".into(),
                selector: Some(JsonSelector::Pointer {
                    pointer: "/nested".into(),
                }),
                projection: JsonProjection::Keys,
            },
            max_tokens: Some(1_000),
            max_items: Some(1),
            array_sample_size: None,
            cursor: Some(cursor.clone()),
        })
        .await
        .expect_err("cursor query binding");
    assert!(matches!(mismatched_query, Error::StaleCursor));

    let unsupported_projection = services
        .json(JsonRequest {
            operation: JsonOperation::Query {
                path: "report.json".into(),
                selector: None,
                projection: JsonProjection::Schema,
            },
            max_tokens: Some(1_000),
            max_items: Some(1),
            array_sample_size: None,
            cursor: Some(cursor.clone()),
        })
        .await
        .expect_err("cursor projection boundary");
    assert!(matches!(
        unsupported_projection,
        Error::InvalidInput {
            field: "cursor",
            ..
        }
    ));

    std::fs::write(
        &path,
        r#"{"version":2,"nested":{"answer":42},"tail":true}"#,
    )
    .expect("mutated JSON fixture");
    let stale_source = services
        .json(JsonRequest {
            operation: operation.clone(),
            max_tokens: Some(1_000),
            max_items: Some(1),
            array_sample_size: None,
            cursor: Some(cursor),
        })
        .await
        .expect_err("cursor source binding");
    assert!(matches!(stale_source, Error::StaleCursor));

    let incomplete_schema = services
        .json(JsonRequest {
            operation: JsonOperation::Query {
                path: "report.json".into(),
                selector: None,
                projection: JsonProjection::Schema,
            },
            max_tokens: Some(1_000),
            max_items: Some(2),
            array_sample_size: None,
            cursor: None,
        })
        .await
        .expect("bounded schema");
    assert!(!incomplete_schema.result_complete);
    assert_eq!(incomplete_schema.returned_items, Some(2));
    assert!(
        incomplete_schema.total_items.expect("total") > incomplete_schema.returned_items.unwrap()
    );
    assert_eq!(
        incomplete_schema.remaining_items,
        Some(
            incomplete_schema.total_items.unwrap()
                - incomplete_schema.returned_items.unwrap()
        )
    );
    assert_eq!(
        incomplete_schema.incomplete_reason,
        Some(JsonIncompleteReason::MaxItems)
    );
    assert!(incomplete_schema.meta.next_cursor.is_none());

    let typed_selector = services
        .json(JsonRequest {
            operation: JsonOperation::Query {
                path: "report.json".into(),
                selector: Some(JsonSelector::Jmespath {
                    expression: "length(version)".into(),
                }),
                projection: JsonProjection::Value,
            },
            max_tokens: Some(100),
            max_items: Some(100),
            array_sample_size: None,
            cursor: None,
        })
        .await
        .expect_err("typed JMESPath error");
    assert!(matches!(
        &typed_selector,
        Error::InvalidJsonSelector {
            stage: "evaluate",
            offset: 6,
            line: 1,
            column: 7,
            reason,
            ..
        } if reason.contains("expects type") && reason.contains("given number")
    ), "{typed_selector:?}");

    let invalid_expression = services
        .json(JsonRequest {
            operation: JsonOperation::Query {
                path: "report.json".into(),
                selector: Some(JsonSelector::Jmespath {
                    expression: "length(".into(),
                }),
                projection: JsonProjection::Value,
            },
            max_tokens: Some(100),
            max_items: Some(100),
            array_sample_size: None,
            cursor: None,
        })
        .await
        .expect_err("JMESPath compile error");
    assert!(matches!(
        invalid_expression,
        Error::InvalidJsonSelector {
            stage: "compile",
            line: 1,
            ..
        }
    ));

    std::fs::write(&path, r#"{"outer":[1,]}"#).expect("invalid JSON fixture");
    let syntax = services
        .json(JsonRequest {
            operation,
            max_tokens: Some(100),
            max_items: Some(100),
            array_sample_size: None,
            cursor: None,
        })
        .await
        .expect_err("JSON syntax error");
    assert!(matches!(
        syntax,
        Error::InvalidJson {
            syntax_category: "syntax",
            byte_offset: 12,
            line: 1,
            column: 13,
            ..
        }
    ));
}
