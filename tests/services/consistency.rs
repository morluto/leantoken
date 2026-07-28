use super::*;

#[tokio::test]
async fn working_tree_diff_boosts_changed_files() {
    if !git_available() {
        return;
    }

    let root = tempfile::tempdir().expect("root");
    std::fs::create_dir(root.path().join("src")).unwrap();
    std::fs::write(root.path().join("src/a.rs"), "fn shared() {}\n").unwrap();
    std::fs::write(root.path().join("src/b.rs"), "fn shared() {}\n").unwrap();
    init_git_repo(root.path());

    let config = Config::discover(root.path(), Some(root.path().join("index.sqlite"))).unwrap();
    let services = Services::open(config).unwrap();
    services.index(false).await.unwrap();

    // Modify b.rs after indexing; do not reindex so the diff signal is tested.
    std::fs::write(root.path().join("src/b.rs"), "fn shared() { let x = 1; }\n").unwrap();

    let response = services
        .context(ContextRequest {
            task: "update shared implementation".into(),
            token_budget: 500,
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
        verbose_diagnostics: false,
        })
        .await
        .unwrap();

    assert!(!response.fragments.is_empty());
    assert_eq!(response.fragments[0].path, "src/b.rs");
    assert!(
        response
            .fragments
            .iter()
            .any(|fragment| fragment.path == "src/b.rs" && fragment.reason.contains("changed"))
    );
}

#[tokio::test]
async fn tokenizer_configuration_is_scoped_to_each_service() {
    let root = tempfile::tempdir().expect("root");
    std::fs::write(
        root.path().join("lib.rs"),
        "fn independent_token_budget() { println!(\"hello\"); }\n",
    )
    .expect("source");
    let mut exact_config =
        Config::discover(root.path(), Some(root.path().join("exact.sqlite"))).expect("config");
    exact_config.tokenizer = leantoken::tokens::Tokenizer::O200kBase;
    let mut estimate_config =
        Config::discover(root.path(), Some(root.path().join("estimate.sqlite"))).expect("config");
    estimate_config.tokenizer = leantoken::tokens::Tokenizer::Estimate;
    let exact = Services::open(exact_config).expect("exact services");
    let estimate = Services::open(estimate_config).expect("estimate services");
    exact.index(false).await.expect("exact index");
    estimate.index(false).await.expect("estimate index");
    let request = ContextRequest {
        task: "change independent_token_budget".into(),
        token_budget: 100,
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
    verbose_diagnostics: false,
    };

    let (exact_response, estimate_response) =
        tokio::join!(exact.context(request.clone()), estimate.context(request),);

    let exact_response = exact_response.expect("exact context");
    let estimate_response = estimate_response.expect("estimate context");
    assert_response_token_accounting!(exact_response, Tokenizer::O200kBase);
    assert_response_token_accounting!(estimate_response, Tokenizer::Estimate);
}

#[tokio::test]
async fn context_declaration_excerpt_retains_long_body_across_chunks() {
    let root = tempfile::tempdir().expect("root");
    let body = (1..=48)
        .map(|line| format!("    let value_{line} = {line};\n"))
        .collect::<String>();
    std::fs::write(
        root.path().join("lib.rs"),
        format!("fn target_symbol() {{\n{body}    consume(value_48);\n}}\n"),
    )
    .expect("source");
    let mut config =
        Config::discover(root.path(), Some(root.path().join("index.sqlite"))).expect("config");
    config.chunk_lines = 3;
    let services = Services::open(config).expect("services");
    services.index(false).await.expect("index");

    let response = services
        .context(ContextRequest {
            task: "fix target_symbol".into(),
            token_budget: 600,
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
        verbose_diagnostics: false,
        })
        .await
        .expect("context");
    let declaration = response
        .fragments
        .iter()
        .find(|fragment| fragment.path == "lib.rs" && fragment.start_line == 1)
        .expect("declaration fragment");

    assert_eq!(declaration.end_line, 51);
    assert!(declaration.content.contains("consume(value_48)"));
}

#[tokio::test]
async fn context_text_hits_use_bounded_declaration_excerpts() {
    let root = tempfile::tempdir().expect("root");
    let body = (1..=160)
        .map(|line| format!("    let filler_{line} = {line};\n"))
        .collect::<String>();
    std::fs::write(
        root.path().join("lib.rs"),
        format!(
            "fn very_large_handler() {{\n{body}    let rare_runtime_marker = filler_160;\n    consume(rare_runtime_marker);\n}}\n"
        ),
    )
    .expect("source");
    let config =
        Config::discover(root.path(), Some(root.path().join("index.sqlite"))).expect("config");
    let services = Services::open(config).expect("services");
    services.index(false).await.expect("index");

    let response = services
        .context(ContextRequest {
            task: "fix rare_runtime_marker behavior".into(),
            token_budget: 1200,
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
        verbose_diagnostics: false,
        })
        .await
        .expect("context");
    let text_fragment = response
        .fragments
        .iter()
        .find(|fragment| {
            fragment.path == "lib.rs" && fragment.reason.contains("text")
        })
        .expect("text fragment");

    assert!(
        text_fragment.token_count <= 320,
        "oversized text fragment: {text_fragment:?}"
    );
    assert!(text_fragment.content.contains("rare_runtime_marker"));
}

#[tokio::test]
async fn regex_search_respects_absolute_candidate_cap() {
    let root = tempfile::tempdir().expect("root");
    // Many matching files so limit*20 alone would exceed MAX_REGEX_CANDIDATES if
    // uncapped; the hard cap must still bound results.
    for index in 0..80 {
        std::fs::write(
            root.path().join(format!("f{index}.rs")),
            "fn needle() { let needle = 1; }\n".repeat(40),
        )
        .expect("write");
    }
    let config =
        Config::discover(root.path(), Some(root.path().join("index.sqlite"))).expect("config");
    let services = Services::open(config).expect("services");
    services.index(false).await.expect("index");

    let response = services
        .search(SearchRequest {
            query: "needle".into(),
            mode: SearchMode::Regex,
            include_paths: Vec::new(),
            exclude_paths: Vec::new(),
            focus_paths: Vec::new(),
            max_results: Some(100),
            max_tokens: Some(32_000),
            context_lines: Some(0),
            case_sensitive: false,
            all_occurrences: false,
            prefer_structural: false,
            receipt_id: None,
            cursor: None,
        })
        .await
        .expect("regex search");
    assert!(!response.hits.is_empty());
    // max_results bounds the returned page, but the path must complete without
    // scanning unbounded; generation must be a committed snapshot.
    assert!(response.meta.repository_generation >= 1);
    assert!(response.hits.len() <= 100);
}

#[tokio::test]
async fn reconcile_working_tree_search_reconciles_file_created_after_index() {
    let root = tempfile::tempdir().expect("root");
    std::fs::write(root.path().join("lib.rs"), "fn existing() {}\n").expect("initial source");
    let config =
        Config::discover(root.path(), Some(root.path().join("index.sqlite"))).expect("config");
    let services = Services::open(config).expect("services");
    let initial = services.index(false).await.expect("initial index");

    std::fs::write(
        root.path().join("new_package.rs"),
        "fn newly_committed_package() {}\n",
    )
    .expect("new source");

    let response = services
        .search_with_consistency_cancellable(
            SearchRequest {
                query: "newly_committed_package".into(),
                mode: SearchMode::Identifier,
                include_paths: Vec::new(),
                exclude_paths: Vec::new(),
                focus_paths: Vec::new(),
                max_results: Some(10),
                max_tokens: Some(100),
                context_lines: Some(0),
                case_sensitive: false,
                all_occurrences: false,
                prefer_structural: false,
                receipt_id: None,
                cursor: None,
            },
            IndexConsistency::ReconcileWorkingTree,
            CancellationToken::new(),
        )
        .await
        .expect("working-tree search");

    assert_eq!(response.hits.len(), 1);
    assert_eq!(response.hits[0].path, "new_package.rs");
    assert!(response.meta.repository_generation > initial.repository_generation);
}

#[tokio::test]
async fn indexed_generation_search_does_not_reconcile_file_created_after_index() {
    let root = tempfile::tempdir().expect("root");
    std::fs::write(root.path().join("lib.rs"), "fn existing() {}\n").expect("initial source");
    let config =
        Config::discover(root.path(), Some(root.path().join("index.sqlite"))).expect("config");
    let services = Services::open(config).expect("services");
    let initial = services.index(false).await.expect("initial index");

    std::fs::write(
        root.path().join("new_package.rs"),
        "fn newly_committed_package() {}\n",
    )
    .expect("new source");

    let response = services
        .search_with_consistency_cancellable(
            SearchRequest {
                query: "newly_committed_package".into(),
                mode: SearchMode::Identifier,
                include_paths: Vec::new(),
                exclude_paths: Vec::new(),
                focus_paths: Vec::new(),
                max_results: Some(10),
                max_tokens: Some(100),
                context_lines: Some(0),
                case_sensitive: false,
                all_occurrences: false,
                prefer_structural: false,
                receipt_id: None,
                cursor: None,
            },
            IndexConsistency::IndexedGeneration,
            CancellationToken::new(),
        )
        .await
        .expect("committed search");

    assert!(response.hits.is_empty());
    assert_eq!(
        response.meta.repository_generation,
        initial.repository_generation
    );
}

#[tokio::test]
async fn reconcile_working_tree_consistency_applies_to_each_retrieval_service() {
    let root = tempfile::tempdir().expect("root");
    std::fs::write(root.path().join("lib.rs"), "fn existing() {}\n").expect("initial source");
    let config =
        Config::discover(root.path(), Some(root.path().join("index.sqlite"))).expect("config");
    let services = Services::open(config).expect("services");
    services.index(false).await.expect("initial index");

    std::fs::write(root.path().join("files_package.rs"), "fn files_package() {}\n")
        .expect("files source");
    let files = services
        .files_with_consistency_cancellable(
            FilesRequest {
                operation: FileOperation::Find,
                path: None,
                query: Some("files_package".into()),
                pattern: None,
                max_results: Some(10),
                cursor: None,
                depth: None,
            },
            IndexConsistency::ReconcileWorkingTree,
            CancellationToken::new(),
        )
        .await
        .expect("working-tree files");
    assert!(files.entries.iter().any(|entry| entry.path == "files_package.rs"));

    std::fs::write(
        root.path().join("outline_package.rs"),
        "fn outlined_package() {}\n",
    )
    .expect("outline source");
    let outline = services
        .outline_with_consistency_cancellable(
            OutlineRequest {
                paths: vec!["outline_package.rs".into()],
                symbol_name: Some("outlined_package".into()),
                symbol_kind: None,
                max_results: Some(10),
                max_tokens: Some(100),
                receipt_id: None,
                cursor: None,
            },
            IndexConsistency::ReconcileWorkingTree,
            CancellationToken::new(),
        )
        .await
        .expect("working-tree outline");
    assert_eq!(outline.files[0].symbols[0].name, "outlined_package");

    std::fs::write(
        root.path().join("read_package.rs"),
        "fn readable_package() {}\n",
    )
    .expect("read source");
    let read = services
        .read_with_consistency_cancellable(
            ReadRequest {
                path: "read_package.rs".into(),
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
            },
            IndexConsistency::ReconcileWorkingTree,
            CancellationToken::new(),
        )
        .await
        .expect("working-tree read");
    assert!(read.content.as_deref().is_some_and(|value| value.contains("readable_package")));
    assert!(!read.index_stale);

    std::fs::write(
        root.path().join("context_package.rs"),
        "fn contextual_package_marker() {}\n",
    )
    .expect("context source");
    let context = services
        .context_with_consistency_cancellable(
            ContextRequest {
                task: "change contextual_package_marker".into(),
                token_budget: 200,
                include_paths: Vec::new(),
                must_include_paths: Vec::new(),
                must_include_symbols: Vec::new(),
                required_evidence: Vec::new(),
                max_fragments: None,
                plan_only: false,
                focus_paths: vec!["context_package.rs".into()],
                strict_focus_paths: false,
                minimum_fragments_per_focus_path: None,
                focus_symbols: vec!["contextual_package_marker".into()],
                exclude_paths: Vec::new(),
                known_hashes: Vec::new(),
                receipt_id: None,
                prior_repository_generation: None,
            base_revision: None,
            changed_paths: Vec::new(),
            strict_changed_paths: false,
            verbose_diagnostics: false,
            },
            IndexConsistency::ReconcileWorkingTree,
            CancellationToken::new(),
        )
        .await
        .expect("working-tree context");
    assert!(
        context
            .fragments
            .iter()
            .any(|fragment| fragment.path == "context_package.rs")
    );
    let report = services
        .token_savings_report()
        .await
        .expect("consistent response accounting");
    let context_accounting = report
        .response_accounting
        .by_operation
        .iter()
        .find(|row| row.operation == TokenAccountingOperation::Context)
        .expect("context accounting");
    assert_eq!(context_accounting.tracked_requests, 1);
    assert_eq!(
        context_accounting.total_response_tokens,
        context.meta.total_response_tokens as u64
    );
}

#[tokio::test]
async fn read_reports_index_stale_when_live_file_diverges() {
    let root = tempfile::tempdir().expect("root");
    std::fs::write(root.path().join("lib.rs"), "fn first() { 1 }\n").expect("write");
    let config =
        Config::discover(root.path(), Some(root.path().join("index.sqlite"))).expect("config");
    let services = Services::open(config).expect("services");
    services.index(false).await.expect("index");

    std::fs::write(root.path().join("lib.rs"), "fn second() { 2 }\n").expect("edit live");
    let response = services
        .read(ReadRequest {
            path: "lib.rs".into(),
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
        })
        .await
        .expect("read");
    assert!(response.index_stale, "live rewrite without reindex must set index_stale");
    assert!(response.content.as_deref().is_some_and(|c| c.contains("second")));
    assert!(response.indexed_hash.is_some());
    assert_ne!(
        response.indexed_hash.as_deref(),
        Some(response.content_hash.as_str()),
        "range hash and whole-file indexed hash differ in meaning but live file is stale"
    );
    assert_eq!(response.meta.repository_generation, 1);
    assert_eq!(response.meta.freshness, Freshness::Current);
}

#[tokio::test]
async fn read_not_modified_still_reports_index_stale_against_live_file() {
    let root = tempfile::tempdir().expect("root");
    std::fs::write(root.path().join("lib.rs"), "fn first() { 1 }\n").expect("write");
    let config =
        Config::discover(root.path(), Some(root.path().join("index.sqlite"))).expect("config");
    let services = Services::open(config).expect("services");
    services.index(false).await.expect("index");

    let first = services
        .read(ReadRequest {
            path: "lib.rs".into(),
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
        })
        .await
        .expect("first read");
    assert!(!first.index_stale);
    assert_eq!(first.status, ReadStatus::Content);

    // Live body changes but the caller still presents the old range hash.
    std::fs::write(root.path().join("lib.rs"), "fn other() { 9 }\n").expect("edit");
    let second = services
        .read(ReadRequest {
            path: "lib.rs".into(),
            start_line: Some(1),
            end_line: Some(1),
            symbol: None,
            heading: None,
            heading_occurrence: None,
            continuation_cursor: None,
            max_tokens: Some(100),
            expected_hash: Some(first.content_hash.clone()),
            delta: false,
            receipt_id: None,
        })
        .await
        .expect("second read");
    // expected_hash compares against the live range hash, so a changed file is
    // Content + index_stale rather than NotModified.
    assert_eq!(second.status, ReadStatus::Content);
    assert!(second.index_stale);
    assert!(second.content.as_deref().is_some_and(|c| c.contains("other")));
}

#[tokio::test]
async fn status_reports_reconciling_when_shared_operation_lock_is_held() {
    let root = tempfile::tempdir().expect("root");
    std::fs::write(root.path().join("lib.rs"), "fn ready() {}\n").expect("write");
    let database = root.path().join("index.sqlite");
    let config = Config::discover(root.path(), Some(database.clone())).expect("config");
    let services = Services::open(config).expect("services");
    services.index(false).await.expect("index");

    let before = services.status().await.expect("status before");
    assert_eq!(before.freshness, Freshness::Current);
    assert_eq!(before.index_state, IndexState::Ready);
    assert!(before.repository_generation >= 1);

    let coordination = IndexCoordination::for_database(&database);
    let _operation = coordination
        .acquire_operation(&CancellationToken::new())
        .expect("hold shared operation lock");

    let during = services.status().await.expect("status during lock");
    assert_eq!(
        during.freshness,
        Freshness::Reconciling,
        "followers must see reconciling via the shared operation lock"
    );
    assert_eq!(during.index_state, IndexState::Ready);
    assert_eq!(during.repository_generation, before.repository_generation);
}

#[test]
fn read_only_status_does_not_wait_for_an_active_writer() {
    let root = tempfile::tempdir().expect("root");
    std::fs::write(root.path().join("lib.rs"), "fn ready() {}\n").expect("write");
    let database = root.path().join("index.sqlite");
    let config = Config::discover(root.path(), Some(database.clone())).expect("config");
    let services = Services::open(config.clone()).expect("services");

    let connection = rusqlite::Connection::open(&database).expect("writer connection");
    connection
        .execute_batch("BEGIN IMMEDIATE")
        .expect("hold writer transaction");

    let started = Instant::now();
    let status = Services::status_without_initializing(config).expect("read-only status");
    assert!(
        started.elapsed().as_secs() < 1,
        "status waited on writer for {:?}",
        started.elapsed()
    );
    assert_eq!(status.repository_generation, 0);
    assert_eq!(status.index_state, IndexState::Uninitialized);

    drop(services);
    connection
        .execute_batch("ROLLBACK")
        .expect("release writer transaction");
}

