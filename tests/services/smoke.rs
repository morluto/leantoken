use super::*;

#[tokio::test]
async fn context_required_evidence_reports_bounded_path_inspection() {
    let root = tempfile::tempdir().expect("temporary repository");
    let docs = root.path().join("docs");
    std::fs::create_dir_all(&docs).expect("docs directory");
    for name in ["a.tex", "b.tex", "c.tex", "d.tex"] {
        std::fs::write(docs.join(name), "ordinary background\n").expect("background fixture");
    }
    std::fs::write(docs.join("e.tex"), "EVIDENCE_OUTSIDE_INSPECTION_BOUND\n")
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
