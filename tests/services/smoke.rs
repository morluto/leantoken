use super::*;

#[tokio::test]
async fn five_services_return_bounded_grounded_responses() {
    let services = immutable_indexed_fixture().await.services.clone();

    let files = services
        .files(FilesRequest {
            operation: FileOperation::Tree,
            path: None,
            query: None,
            pattern: None,
            max_results: Some(10),
            cursor: None,
            depth: Some(3),
        })
        .await
        .expect("files");
    assert!(files.entries.iter().any(|entry| entry.path == "src/lib.rs"));
    assert_eq!(files.meta.source_tokens, 0);
    assert_response_token_accounting!(files, Tokenizer::Cl100kBase);
    assert!(files.meta.path_and_metadata_tokens > 0);

    let search = services
        .search(SearchRequest {
            query: "greet".into(),
            mode: SearchMode::Auto,
            include_paths: Vec::new(),
            exclude_paths: Vec::new(),
            focus_paths: Vec::new(),
            max_results: Some(5),
            max_tokens: Some(200),
            context_lines: Some(1),
            case_sensitive: false,
            all_occurrences: false,
            prefer_structural: false,
            receipt_id: None,
            query_receipt: None,
            cursor: None,
        })
        .await
        .expect("search");
    assert!(!search.hits.is_empty());
    assert!(search.meta.source_tokens <= 200);
    assert!(search.hits.iter().all(|hit| hit.start_line <= hit.end_line));
    assert_response_token_accounting!(search, Tokenizer::Cl100kBase);

    let outline = services
        .outline(OutlineRequest {
            paths: vec!["src/lib.rs".into()],
            symbol_name: None,
            symbol_kind: None,
            max_results: Some(10),
            max_tokens: Some(100),
            receipt_id: None,
            cursor: None,
        })
        .await
        .expect("outline");
    assert!(
        outline.files[0]
            .symbols
            .iter()
            .any(|symbol| symbol.name == "greet")
    );
    assert!(outline.meta.source_tokens <= 100);
    assert_response_token_accounting!(outline, Tokenizer::Cl100kBase);

    let first = services
        .read(ReadRequest {
            path: "src/lib.rs".into(),
            start_line: Some(1),
            end_line: Some(3),
            symbol: None,
            heading: None,
            heading_occurrence: None,
            continuation_cursor: None,
            max_tokens: Some(100),
            expected_hash: None,
            delta: false,
            receipt_id: None,
            policy: leantoken::ReadPolicy::default(),
        })
        .await
        .expect("first read");
    assert_response_token_accounting!(first, Tokenizer::Cl100kBase);
    let second = services
        .read(ReadRequest {
            path: "src/lib.rs".into(),
            start_line: Some(1),
            end_line: Some(3),
            symbol: None,
            heading: None,
            heading_occurrence: None,
            continuation_cursor: None,
            max_tokens: Some(100),
            expected_hash: Some(first.content_hash.clone()),
            delta: false,
            receipt_id: None,
            policy: leantoken::ReadPolicy::default(),
        })
        .await
        .expect("conditional read");
    assert_eq!(second.status, ReadStatus::NotModified);
    assert!(second.content.is_none());
    assert_eq!(second.meta.source_tokens, 0);
    assert_response_token_accounting!(second, Tokenizer::Cl100kBase);

    let context = services
        .context(ContextRequest {
            task: "change greet caller".into(),
            token_budget: 200,
            include_paths: Vec::new(),
            must_include_paths: Vec::new(),
            must_include_symbols: Vec::new(),
            required_evidence: Vec::new(),
            max_fragments: None,
            plan_only: false,
            focus_paths: Vec::new(),
            strict_focus_paths: false,
            minimum_fragments_per_focus_path: None,
            focus_symbols: Vec::new(),
            exclude_paths: Vec::new(),
            known_hashes: Vec::new(),
            receipt_id: None,
            prior_repository_generation: None,
        base_revision: None,
        changed_paths: Vec::new(),
        strict_changed_paths: false,
        explain_diagnostics: false,
        })
        .await
        .expect("context");
    assert!(!context.fragments.is_empty());
    assert!(context.meta.source_tokens <= 200);
    assert_response_token_accounting!(context, Tokenizer::Cl100kBase);
    assert_eq!(
        context.receipt.fragment_hashes.len(),
        context.fragments.len()
    );
    let repeated_context = services
        .context(ContextRequest {
            task: "change greet caller".into(),
            token_budget: 200,
            include_paths: Vec::new(),
            must_include_paths: Vec::new(),
            must_include_symbols: Vec::new(),
            required_evidence: Vec::new(),
            max_fragments: None,
            plan_only: false,
            focus_paths: Vec::new(),
            strict_focus_paths: false,
            minimum_fragments_per_focus_path: None,
            focus_symbols: Vec::new(),
            exclude_paths: Vec::new(),
            known_hashes: Vec::new(),
            receipt_id: None,
            prior_repository_generation: None,
        base_revision: None,
        changed_paths: Vec::new(),
        strict_changed_paths: false,
        explain_diagnostics: false,
        })
        .await
        .expect("repeated context");
    // Opaque IDs intentionally affect serialized accounting; compare the
    // deterministic retrieval payload separately.
    let mut deterministic_context = context.clone();
    deterministic_context.meta.receipt_id = None;
    deterministic_context.meta.path_and_metadata_tokens = 0;
    deterministic_context.meta.total_response_tokens = 0;
    deterministic_context.meta.total_response_tokens = 0;
    let mut deterministic_repeat = repeated_context.clone();
    deterministic_repeat.meta.receipt_id = None;
    deterministic_repeat.meta.path_and_metadata_tokens = 0;
    deterministic_repeat.meta.total_response_tokens = 0;
    deterministic_repeat.meta.total_response_tokens = 0;
    assert_eq!(
        serde_json::to_string(&deterministic_repeat).expect("serialize repeated context"),
        serde_json::to_string(&deterministic_context).expect("serialize context"),
        "the same repository generation and request must be deterministic"
    );

    let known = context.fragments[0].content_hash.clone();
    let delta = services
        .context(ContextRequest {
            task: "change greet caller".into(),
            token_budget: 200,
            include_paths: Vec::new(),
            must_include_paths: Vec::new(),
            must_include_symbols: Vec::new(),
            required_evidence: Vec::new(),
            max_fragments: None,
            plan_only: false,
            focus_paths: Vec::new(),
            strict_focus_paths: false,
            minimum_fragments_per_focus_path: None,
            focus_symbols: Vec::new(),
            exclude_paths: Vec::new(),
            known_hashes: vec![known.clone()],
            receipt_id: None,
            prior_repository_generation: Some(context.meta.repository_generation),
        base_revision: None,
        changed_paths: Vec::new(),
        strict_changed_paths: false,
        explain_diagnostics: false,
        })
        .await
        .expect("context delta");
    assert!(
        delta
            .fragments
            .iter()
            .all(|fragment| fragment.content_hash != known)
    );
    let report = services
        .token_savings_report()
        .await
        .expect("full response accounting");
    let files_accounting = report
        .response_accounting
        .by_operation
        .iter()
        .find(|row| row.operation == TokenAccountingOperation::Files)
        .expect("files accounting");
    assert_eq!(files_accounting.tracked_requests, 1);
    assert_eq!(files_accounting.baseline_requests, 0);
    assert_eq!(
        files_accounting.total_response_tokens,
        files.meta.total_response_tokens as u64
    );
    assert_eq!(
        files_accounting.estimated_net_tokens_saved,
        -(files.meta.total_response_tokens as i64)
    );
}

#[tokio::test]
async fn immutable_indexed_fixture_is_shared_at_one_generation() {
    let first = immutable_indexed_fixture().await;
    let second = immutable_indexed_fixture().await;
    assert!(std::ptr::eq(first, second));
    assert_eq!(first.generation, 1);
    assert_eq!(second.generation, 1);
}

#[tokio::test]
async fn context_required_evidence_reports_bounded_path_inspection() {
    let root = tempfile::tempdir().expect("temporary repository");
    let docs = root.path().join("docs");
    std::fs::create_dir_all(&docs).expect("docs directory");
    for name in ["a.tex", "b.tex", "c.tex", "d.tex"] {
        std::fs::write(docs.join(name), "ordinary background\n").expect("background fixture");
    }
    std::fs::write(
        docs.join("e.tex"),
        "EVIDENCE_OUTSIDE_INSPECTION_BOUND\n",
    )
    .expect("bounded fixture");
    let config =
        Config::discover(root.path(), Some(root.path().join("index.sqlite"))).expect("config");
    let services = Services::open(config).expect("services");
    services.index(false).await.expect("index fixture");
    let mut request = context_limit_request(500);
    request.task = "Find EVIDENCE_OUTSIDE_INSPECTION_BOUND.".into();
    request.required_evidence = vec![ContextRequiredEvidence {
        path: "docs/*.tex".into(),
        queries: vec!["EVIDENCE_OUTSIDE_INSPECTION_BOUND".into()],
        minimum_query_matches: 1,
    }];

    let response = services
        .context(request)
        .await
        .expect("bounded evidence context");

    assert_eq!(response.coverage.evidence_scope_satisfied, Some(false));
    let coverage = &response.coverage.required_evidence[0];
    assert_eq!(coverage.indexed_paths, 5);
    assert_eq!(coverage.inspected_paths, 4);
    assert!(!coverage.satisfied);
    assert!(response.warnings.iter().any(|warning| {
        warning.contains("matched more indexed paths than the bounded local inspection covered")
    }));
}

#[tokio::test]
async fn repository_path_inputs_normalize_before_index_lookup_and_matching() {
    let (_root, services) = fixture().await;

    let read = services
        .read(ReadRequest {
            path: r".\src\lib.rs".into(),
            start_line: Some(1),
            end_line: Some(1),
            symbol: None,
            heading: None,
            heading_occurrence: None,
            continuation_cursor: None,
            max_tokens: Some(100),
            expected_hash: None,
            delta: false,
            receipt_id: None,
            policy: leantoken::ReadPolicy::default(),
        })
        .await
        .expect("normalized read");
    assert_eq!(read.path, "src/lib.rs");

    let outline = services
        .outline(OutlineRequest {
            paths: vec!["./src/lib.rs".into()],
            symbol_name: None,
            symbol_kind: None,
            max_results: Some(10),
            max_tokens: Some(100),
            receipt_id: None,
            cursor: None,
        })
        .await
        .expect("normalized outline");
    assert_eq!(outline.files[0].path, "src/lib.rs");

    let files = services
        .files(FilesRequest {
            operation: FileOperation::Glob,
            path: None,
            query: None,
            pattern: Some(r"src\*.rs".into()),
            max_results: Some(10),
            cursor: None,
            depth: None,
        })
        .await
        .expect("normalized files glob");
    assert_eq!(files.entries[0].path, "src/lib.rs");

    let search = services
        .search(SearchRequest {
            query: "greet".into(),
            mode: SearchMode::Auto,
            include_paths: vec![r"src\*.rs".into()],
            exclude_paths: Vec::new(),
            focus_paths: vec![r"src\lib.rs".into()],
            max_results: Some(10),
            max_tokens: Some(100),
            context_lines: Some(1),
            case_sensitive: false,
            all_occurrences: false,
            prefer_structural: false,
            receipt_id: None,
            query_receipt: None,
            cursor: None,
        })
        .await
        .expect("normalized search paths");
    assert!(search.hits.iter().any(|hit| hit.path == "src/lib.rs"));
    assert!(
        search
            .hits
            .iter()
            .any(|hit| hit.score_reasons.contains(&"focus path".to_owned()))
    );

    let context = services
        .context(ContextRequest {
            task: "find greet".into(),
            token_budget: 200,
            include_paths: Vec::new(),
            must_include_paths: Vec::new(),
            must_include_symbols: Vec::new(),
            required_evidence: Vec::new(),
            max_fragments: None,
            plan_only: false,
            focus_paths: Vec::new(),
            strict_focus_paths: false,
            minimum_fragments_per_focus_path: None,
            focus_symbols: Vec::new(),
            exclude_paths: Vec::new(),
            known_hashes: Vec::new(),
            receipt_id: None,
            prior_repository_generation: None,
            base_revision: None,
            changed_paths: vec![r".\src\lib.rs".into()],
            strict_changed_paths: false,
            explain_diagnostics: false,
        })
        .await
        .expect("normalized context path");
    let scope = context.diff_scope.expect("explicit diff scope");
    assert_eq!(scope.changed_paths, vec!["src/lib.rs"]);
    assert_eq!(scope.indexed_changed_paths, 1);
}
