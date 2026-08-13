use super::*;

#[tokio::test]
async fn multi_path_outline_reports_each_path_without_aborting_indexed_results() {
    let root = tempfile::tempdir().expect("temporary repository");
    std::fs::create_dir_all(root.path().join("src")).expect("source directory");
    std::fs::create_dir(root.path().join(".git")).expect("git marker");
    std::fs::write(
        root.path().join("src/indexed.rs"),
        "fn first() {}\nfn second() {}\n",
    )
    .expect("indexed source");
    std::fs::write(root.path().join("ignored.rs"), "fn ignored() {}\n").expect("ignored source");
    std::fs::write(root.path().join(".gitignore"), "ignored.rs\n").expect("ignore rules");
    let services = Services::open(
        Config::discover(root.path(), Some(root.path().join("index.sqlite"))).expect("config"),
    )
    .expect("services");
    services
        .index(leantoken::IndexingMode::Reconcile)
        .await
        .expect("index");

    let request = OutlineRequest {
        paths: vec![
            "src/./indexed.rs".into(),
            "ignored.rs".into(),
            "missing.rs".into(),
        ],
        symbol_name: None,
        symbol_kind: None,
        max_results: Some(1),
        max_tokens: Some(32_000),
        receipt_id: None,
        cursor: None,
    };
    let first = services
        .outline(request.clone())
        .await
        .expect("partial outline page");
    assert_eq!(
        first.path_results,
        vec![
            OutlinePathResult {
                request_index: 0,
                path: "src/indexed.rs".into(),
                status: OutlinePathStatus::Indexed,
            },
            OutlinePathResult {
                request_index: 1,
                path: "ignored.rs".into(),
                status: OutlinePathStatus::NotIndexed,
            },
            OutlinePathResult {
                request_index: 2,
                path: "missing.rs".into(),
                status: OutlinePathStatus::NotIndexed,
            },
        ]
    );
    assert_eq!(first.files.len(), 1);
    assert_eq!(first.files[0].path, "src/indexed.rs");
    assert_eq!(first.total_symbols, 2);
    assert_eq!(first.returned_symbols, 1);
    assert!(!first.parse_complete);
    assert!(!first.result_complete);
    assert!(first.truncated_by_max_results);
    let wire = serde_json::to_value(&first).expect("serialize partial outline");
    assert_eq!(
        wire["path_results"],
        serde_json::json!([
            {
                "request_index": 0,
                "path": "src/indexed.rs",
                "status": "indexed"
            },
            {
                "request_index": 1,
                "path": "ignored.rs",
                "status": "not_indexed"
            },
            {
                "request_index": 2,
                "path": "missing.rs",
                "status": "not_indexed"
            }
        ])
    );
    let cursor = first.meta.next_cursor.clone().expect("continuation");

    let mut continued_request = request.clone();
    continued_request.cursor = Some(cursor.clone());
    let second = services
        .outline(continued_request.clone())
        .await
        .expect("continued partial outline");
    assert_eq!(second.path_results, first.path_results);
    assert_eq!(second.returned_symbols, 1);
    assert!(!second.truncated_by_max_results);
    assert!(second.meta.next_cursor.is_none());

    continued_request.paths.swap(1, 2);
    assert!(matches!(
        services.outline(continued_request).await,
        Err(Error::StaleCursor)
    ));

    let signatures = services
        .outline_signatures(request)
        .await
        .expect("signature partial outline");
    assert_eq!(signatures.path_results, first.path_results);
    assert_eq!(signatures.files.len(), 1);
    assert!(!signatures.parse_complete);
    assert!(!signatures.result_complete);

    let missing = services
        .outline(OutlineRequest {
            paths: vec!["missing.rs".into()],
            symbol_name: None,
            symbol_kind: None,
            max_results: Some(100),
            max_tokens: Some(32_000),
            receipt_id: None,
            cursor: None,
        })
        .await
        .expect("typed missing-path outcome");
    assert_eq!(
        missing.path_results,
        vec![OutlinePathResult {
            request_index: 0,
            path: "missing.rs".into(),
            status: OutlinePathStatus::NotIndexed,
        }]
    );
    assert!(missing.files.is_empty());
    assert!(!missing.parse_complete);
    assert!(!missing.result_complete);
    assert_eq!(missing.total_symbols, 0);
    assert_eq!(missing.total_imports, 0);
}

#[tokio::test]
async fn outline_distinguishes_parse_completeness_from_result_completeness() {
    let root = tempfile::tempdir().expect("temporary repository");
    let constants = (0..120)
        .map(|index| format!("const VALUE_{index:03}: usize = {index};\n"))
        .collect::<String>();
    let functions = (0..20)
        .map(|index| format!("fn operation_{index:03}() {{}}\n"))
        .collect::<String>();
    std::fs::write(
        root.path().join("many.rs"),
        format!("use std::fmt; use std::io;\n{constants}{functions}"),
    )
    .expect("many symbols");
    std::fs::write(root.path().join("broken.rs"), "fn broken( {\n").expect("malformed source");
    let services = Services::open(
        Config::discover(root.path(), Some(root.path().join("index.sqlite"))).expect("config"),
    )
    .expect("services");
    services
        .index(leantoken::IndexingMode::Reconcile)
        .await
        .expect("index");

    let first = services
        .outline(OutlineRequest {
            paths: vec!["many.rs".into()],
            symbol_name: None,
            symbol_kind: None,
            max_results: Some(100),
            max_tokens: Some(32_000),
            receipt_id: None,
            cursor: None,
        })
        .await
        .expect("first outline page");
    assert!(first.parse_complete);
    assert!(first.files[0].parse_complete);
    assert!(first.files[0].parse_complete);
    assert!(!first.result_complete);
    assert_eq!(first.total_symbols, 140);
    assert_eq!(first.returned_symbols, 100);
    assert_eq!(first.total_imports, 2);
    assert_eq!(first.returned_imports, 0);
    assert!(first.truncated_by_max_results);
    assert!(!first.truncated_by_max_tokens);
    assert_eq!(first.symbol_counts_by_kind.get("constant"), Some(&120));
    assert_eq!(first.symbol_counts_by_kind.get("function"), Some(&20));
    let cursor = first.meta.next_cursor.clone().expect("continuation cursor");

    let changed_query = services
        .outline(OutlineRequest {
            paths: vec!["many.rs".into()],
            symbol_name: None,
            symbol_kind: Some("function".into()),
            max_results: Some(100),
            max_tokens: Some(32_000),
            receipt_id: None,
            cursor: Some(cursor.clone()),
        })
        .await
        .expect_err("cursor must remain bound to the original filters");
    assert!(matches!(changed_query, Error::StaleCursor));

    let second = services
        .outline(OutlineRequest {
            paths: vec!["many.rs".into()],
            symbol_name: None,
            symbol_kind: None,
            max_results: Some(41),
            max_tokens: Some(32_000),
            receipt_id: None,
            cursor: Some(cursor),
        })
        .await
        .expect("second outline page");
    assert!(second.parse_complete);
    assert!(!second.result_complete);
    assert_eq!(second.total_symbols, 140);
    assert_eq!(second.returned_symbols, 40);
    assert_eq!(second.returned_imports, 1);
    assert!(second.truncated_by_max_results);
    assert!(!second.truncated_by_max_tokens);
    let final_cursor = second.meta.next_cursor.clone().expect("final cursor");

    let third = services
        .outline(OutlineRequest {
            paths: vec!["many.rs".into()],
            symbol_name: None,
            symbol_kind: None,
            max_results: Some(100),
            max_tokens: Some(32_000),
            receipt_id: None,
            cursor: Some(final_cursor),
        })
        .await
        .expect("third outline page");
    assert_eq!(third.returned_symbols, 0);
    assert_eq!(third.returned_imports, 1);
    assert!(!third.truncated_by_max_results);
    assert!(third.meta.next_cursor.is_none());
    let names = first.files[0]
        .symbols
        .iter()
        .chain(&second.files[0].symbols)
        .map(|symbol| symbol.name.as_str())
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(names.len(), 140);
    assert!(names.contains("VALUE_000"));
    assert!(names.contains("operation_019"));
    let imports = second.files[0]
        .imports
        .iter()
        .chain(&third.files[0].imports)
        .map(|import| import.raw_target.as_str())
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(imports, ["std::fmt", "std::io"].into_iter().collect());

    let token_limited = services
        .outline(OutlineRequest {
            paths: vec!["many.rs".into()],
            symbol_name: None,
            symbol_kind: None,
            max_results: Some(100),
            max_tokens: Some(1),
            receipt_id: None,
            cursor: None,
        })
        .await
        .expect("token-limited outline");
    assert!(token_limited.parse_complete);
    assert!(!token_limited.result_complete);
    assert!(token_limited.returned_symbols < token_limited.total_symbols);
    assert!(!token_limited.truncated_by_max_results);
    assert!(token_limited.truncated_by_max_tokens);
    assert!(token_limited.meta.next_cursor.is_none());

    let malformed = services
        .outline(OutlineRequest {
            paths: vec!["broken.rs".into()],
            symbol_name: None,
            symbol_kind: None,
            max_results: Some(100),
            max_tokens: Some(1_000),
            receipt_id: None,
            cursor: None,
        })
        .await
        .expect("malformed outline");
    assert!(!malformed.parse_complete);
    assert!(!malformed.files[0].parse_complete);
    assert!(!malformed.files[0].parse_complete);
    assert!(malformed.result_complete);
}

#[tokio::test]
async fn outline_cursor_rejects_a_budget_change_that_would_reclassify_omitted_entries() {
    let root = tempfile::tempdir().expect("temporary repository");
    let parameters = (0..40)
        .map(|index| format!("argument_{index}: usize"))
        .collect::<Vec<_>>()
        .join(", ");
    std::fs::write(
        root.path().join("budget.rs"),
        format!("fn expensive({parameters}) {{}}\nfn cheap() {{}}\nfn later() {{}}\n"),
    )
    .expect("outline fixture");
    let services = Services::open(
        Config::discover(root.path(), Some(root.path().join("index.sqlite"))).expect("config"),
    )
    .expect("services");
    services
        .index(leantoken::IndexingMode::Reconcile)
        .await
        .expect("index");

    let request = |max_results, max_tokens, cursor| OutlineRequest {
        paths: vec!["budget.rs".into()],
        symbol_name: None,
        symbol_kind: None,
        max_results: Some(max_results),
        max_tokens: Some(max_tokens),
        receipt_id: None,
        cursor,
    };
    let complete = services
        .outline(request(10, 32_000, None))
        .await
        .expect("complete outline");
    let symbols = &complete.files[0].symbols;
    assert_eq!(
        symbols
            .iter()
            .map(|symbol| symbol.name.as_str())
            .collect::<Vec<_>>(),
        ["expensive", "cheap", "later"]
    );
    let costs = symbols
        .iter()
        .map(|symbol| {
            symbol
                .signature
                .as_deref()
                .map_or(1, |signature| Tokenizer::default().count(signature))
        })
        .collect::<Vec<_>>();
    assert!(
        costs[0] > costs[1],
        "fixture needs an expensive first entry"
    );

    let low_budget = costs[1];
    let first = services
        .outline(request(1, low_budget, None))
        .await
        .expect("token-limited first page");
    assert_eq!(first.files[0].symbols[0].name, "cheap");
    assert!(first.truncated_by_max_tokens);
    assert!(first.truncated_by_max_results);
    let cursor = first.meta.next_cursor.expect("later entry remains");

    let stale = services
        .outline(request(10, costs[0], Some(cursor)))
        .await
        .expect_err("changing the token budget changes the outline stream");
    assert!(matches!(stale, Error::StaleCursor));

    let restarted = services
        .outline(request(1, costs[0], None))
        .await
        .expect("restart with larger budget");
    assert_eq!(restarted.files[0].symbols[0].name, "expensive");
}

#[tokio::test]
async fn fixture_outlines_deduplicate_methods_and_report_receiver_owners() {
    let root = tempfile::tempdir().expect("temporary repository");
    for (path, source) in [
        (
            "src/rust/math.rs",
            include_str!("../../fixtures/sample_repo/src/rust/math.rs"),
        ),
        (
            "src/go/point.go",
            include_str!("../../fixtures/sample_repo/src/go/point.go"),
        ),
    ] {
        let absolute = root.path().join(path);
        std::fs::create_dir_all(absolute.parent().expect("fixture parent"))
            .expect("create fixture parent");
        std::fs::write(absolute, source).expect("write fixture source");
    }
    let services = Services::open(
        Config::discover(root.path(), Some(root.path().join("index.sqlite"))).expect("config"),
    )
    .expect("services");
    services
        .index(leantoken::IndexingMode::Reconcile)
        .await
        .expect("index fixtures");

    let outline = services
        .outline(OutlineRequest {
            paths: vec!["src/rust/math.rs".into(), "src/go/point.go".into()],
            symbol_name: None,
            symbol_kind: None,
            max_results: Some(100),
            max_tokens: Some(2_000),
            receipt_id: None,
            cursor: None,
        })
        .await
        .expect("fixture outline");
    let symbols = outline
        .files
        .iter()
        .flat_map(|file| file.symbols.iter())
        .collect::<Vec<_>>();

    for (name, parent) in [("distance", "Point"), ("Distance", "Point")] {
        let matching = symbols
            .iter()
            .filter(|symbol| symbol.name == name)
            .collect::<Vec<_>>();
        assert_eq!(matching.len(), 1, "symbols for {name}: {matching:?}");
        assert_eq!(matching[0].kind, "method");
        assert_eq!(matching[0].parent.as_deref(), Some(parent));
    }

    let status = services.status().await.expect("status");
    assert_eq!(status.symbol_count, symbols.len());
    assert_eq!(status.symbol_count, 6);
}
