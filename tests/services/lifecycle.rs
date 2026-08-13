use super::*;
use std::time::Instant;

#[tokio::test]
async fn oversized_query_is_rejected_without_stopping_services() {
    let (_root, services) = fixture().await;
    let oversized = "x".repeat(64 * 1024 + 1);
    let error = services
        .search(SearchRequest {
            query: oversized,
            mode: SearchMode::Text,
            include_paths: Vec::new(),
            exclude_paths: Vec::new(),
            focus_paths: Vec::new(),
            max_results: None,
            max_tokens: None,
            context_lines: None,
            case_sensitive: false,
            all_occurrences: false,
            prefer_structural: false,
            receipt_id: None,
            query_receipt: None,
            cursor: None,
        })
        .await
        .expect_err("oversized query must fail");
    assert!(error.to_string().contains("exceeds"));

    let status = services.status().await.expect("service remains live");
    assert_eq!(status.file_count, 1);
}

#[tokio::test]
async fn cancelled_blocking_queries_stop_cooperatively_without_poisoning_services() {
    let (_root, services) = fixture().await;
    let cancellation = CancellationToken::new();
    cancellation.cancel();

    let search = services
        .search_cancellable(
            SearchRequest {
                query: "greet".into(),
                mode: SearchMode::Regex,
                include_paths: Vec::new(),
                exclude_paths: Vec::new(),
                focus_paths: Vec::new(),
                max_results: Some(10),
                max_tokens: Some(100),
                context_lines: Some(2),
                case_sensitive: false,
                all_occurrences: false,
                prefer_structural: false,
                receipt_id: None,
                query_receipt: None,
                cursor: None,
            },
            cancellation.child_token(),
        )
        .await
        .expect_err("cancelled search");
    assert!(matches!(search, Error::Cancelled));

    let context = services
        .context_cancellable(
            ContextRequest {
                task: "change greet".into(),
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
                explain_diagnostics: false,
            },
            cancellation,
        )
        .await
        .expect_err("cancelled context");
    assert!(matches!(context, Error::Cancelled));
    assert_eq!(services.status().await.expect("status").file_count, 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_queries_observe_one_committed_generation_during_reconciliation() {
    let (root, services) = fixture().await;
    let services = std::sync::Arc::new(services);
    let before = services
        .status()
        .await
        .expect("before status")
        .repository_generation;
    std::fs::write(
        root.path().join("src/lib.rs"),
        "pub fn replacement() -> u8 { 42 }\n",
    )
    .expect("replace source");

    let indexing_services = std::sync::Arc::clone(&services);
    let indexing = tokio::spawn(async move {
        indexing_services
            .refresh(leantoken::IndexingMode::Reconcile)
            .await
            .expect("refresh")
    });
    let mut queries = tokio::task::JoinSet::new();
    // Stay within the documented portable minimum execution bound so a slow
    // four-core Windows runner cannot turn this snapshot-consistency test into
    // a queue-timeout test. Exact queue and overload behavior is covered
    // separately.
    for index in 0..4 {
        let services = std::sync::Arc::clone(&services);
        queries.spawn(async move {
            let query = if index % 2 == 0 {
                "greet"
            } else {
                "replacement"
            };
            let response = services
                .search(SearchRequest {
                    query: query.into(),
                    mode: SearchMode::Identifier,
                    include_paths: Vec::new(),
                    exclude_paths: Vec::new(),
                    focus_paths: Vec::new(),
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
                .expect("concurrent search");
            (query, response)
        });
    }

    let after = indexing.await.expect("index task").repository_generation;
    assert!(after > before);
    while let Some(result) = queries.join_next().await {
        let (query, response) = result.expect("query task");
        assert!(
            matches!(response.meta.repository_generation, value if value == before || value == after)
        );
        if response.meta.repository_generation == before {
            assert_eq!(response.hits.is_empty(), query == "replacement");
        } else {
            assert_eq!(response.hits.is_empty(), query == "greet");
        }
    }
}

#[tokio::test]
async fn managed_corrupt_index_is_deleted_and_rebuilt() {
    let root = tempfile::tempdir().expect("root");
    std::fs::write(root.path().join("lib.rs"), "fn recovered() {}\n").expect("source");
    let config = Config::discover(root.path(), None).expect("config");
    let database = config.database_path.clone();
    let database_parent = database.parent().expect("database parent").to_owned();
    std::fs::create_dir_all(&database_parent).expect("parent");
    std::fs::write(&database, b"not a sqlite database").expect("corrupt database");

    let services = Services::open(config).expect("recover managed cache");
    services
        .refresh(leantoken::IndexingMode::Reconcile)
        .await
        .expect("rebuild index");
    assert_eq!(services.status().await.expect("status").file_count, 1);
    assert!(
        std::fs::metadata(&database)
            .expect("rebuilt database")
            .len()
            > 32
    );
    drop(services);
    std::fs::remove_dir_all(database_parent).expect("remove managed cache fixture");
}

#[tokio::test]
async fn managed_invalid_projection_discards_the_whole_generation() {
    let root = tempfile::tempdir().expect("root");
    std::fs::write(root.path().join("lib.rs"), "fn recovered() {}\n").expect("source");
    let config = Config::discover(root.path(), None).expect("config");
    let database = config.database_path.clone();
    let database_parent = database.parent().expect("database parent").to_owned();

    let services = Services::open(config.clone()).expect("open managed cache");
    services
        .refresh(leantoken::IndexingMode::Reconcile)
        .await
        .expect("initial generation");
    drop(services);

    let connection = rusqlite::Connection::open(&database).expect("raw index");
    connection
        .execute(
            "DELETE FROM path_entries WHERE file_id = (SELECT id FROM files LIMIT 1)",
            [],
        )
        .expect("damage projection");
    drop(connection);

    let recovered = Services::open(config).expect("discard invalid managed generation");
    assert_eq!(
        recovered
            .status()
            .await
            .expect("empty replacement status")
            .repository_generation,
        0
    );
    recovered
        .refresh(leantoken::IndexingMode::Reconcile)
        .await
        .expect("publish replacement generation");
    assert_eq!(recovered.status().await.expect("status").file_count, 1);
    drop(recovered);
    std::fs::remove_dir_all(database_parent).expect("remove managed cache fixture");
}

#[test]
fn explicit_corrupt_database_is_not_deleted() {
    let root = tempfile::tempdir().expect("root");
    let database = root.path().join("explicit.sqlite");
    let original = b"caller-owned data";
    std::fs::write(&database, original).expect("database fixture");
    let config = Config::discover(root.path(), Some(database.clone())).expect("config");

    Services::open(config).expect_err("explicit corruption must be reported");
    assert_eq!(
        std::fs::read(database).expect("preserved database"),
        original
    );
}

#[tokio::test]
async fn empty_index_reports_status_but_retrieval_is_not_ready() {
    let root = tempfile::tempdir().expect("root");
    std::fs::write(root.path().join("lib.rs"), "fn pending() {}\n").expect("source");
    let config = Config::discover(root.path(), Some(root.path().join("index.sqlite"))).unwrap();
    let services = Services::open(config).unwrap();

    let status = services.status().await.expect("status");
    assert_eq!(status.repository_generation, 0);
    assert_eq!(status.index_state, IndexState::Uninitialized);
    assert_eq!(status.freshness, Freshness::Current);
    assert_eq!(status.file_count, 0);
    let progress = status.index_progress.expect("uninitialized progress");
    assert!(!progress.detail_available);
    assert!(!progress.active);
    assert_eq!(progress.current_generation, 0);
    assert_eq!(progress.phase, None);
    assert_eq!(progress.files_discovered, None);

    let error = services
        .files(FilesRequest {
            operation: FileOperation::Tree,
            path: None,
            query: None,
            pattern: None,
            max_results: Some(10),
            cursor: None,
            depth: Some(2),
        })
        .await
        .expect_err("retrieval must not report an empty success");
    assert!(matches!(error, leantoken::Error::IndexNotReady));
}

#[tokio::test]
async fn parser_coverage_reports_across_incremental_row_generations() {
    let root = tempfile::tempdir().expect("root");
    let rust_source = "pub fn ready() -> bool { true }\n";
    let incomplete_typescript = "const missing = ;\n";
    let unrecognized = "name: fixture\n";
    std::fs::write(root.path().join("lib.rs"), rust_source).expect("rust source");
    std::fs::write(root.path().join("broken.ts"), incomplete_typescript)
        .expect("TypeScript source");
    std::fs::write(root.path().join("settings.yaml"), unrecognized).expect("YAML source");
    let database = root.path().join("index.sqlite");
    let config = Config::discover(root.path(), Some(database.clone())).expect("config");
    let services = Services::open(config).expect("services");

    services
        .refresh(leantoken::IndexingMode::Reconcile)
        .await
        .expect("initial index");
    let initial_report = services.parser_coverage().await.expect("initial coverage");
    assert_eq!(initial_report.repository_generation, 1);
    let initial = initial_report.coverage;
    assert_eq!(
        initial.indexed,
        leantoken::ParserCoverageCount {
            files: 3,
            source_bytes: u64::try_from(
                rust_source.len() + incomplete_typescript.len() + unrecognized.len()
            )
            .expect("source bytes"),
        }
    );
    assert_eq!(initial.recognized.files, 2);
    assert_eq!(initial.complete.files, 1);
    assert_eq!(initial.incomplete.files, 1);
    assert_eq!(initial.unrecognized.files, 1);
    assert_eq!(
        initial
            .languages
            .iter()
            .map(|language| (
                language.language.as_str(),
                language.complete.files,
                language.incomplete.files,
            ))
            .collect::<Vec<_>>(),
        vec![("rust", 1, 0), ("typescript", 0, 1)]
    );
    assert_eq!(
        initial
            .unrecognized_extensions
            .iter()
            .map(|extension| (extension.extension.as_str(), extension.total.files))
            .collect::<Vec<_>>(),
        vec![(".yaml", 1)]
    );

    let updated_rust = "pub fn ready() -> bool { false }\n";
    std::fs::write(root.path().join("lib.rs"), updated_rust).expect("updated rust source");
    services
        .refresh(leantoken::IndexingMode::Reconcile)
        .await
        .expect("incremental index");
    let connection = rusqlite::Connection::open(database).expect("database");
    let distinct_generations: i64 = connection
        .query_row("SELECT count(DISTINCT generation) FROM files", [], |row| {
            row.get(0)
        })
        .expect("row generations");
    assert!(
        distinct_generations > 1,
        "fixture did not retain unchanged rows from the earlier generation"
    );

    let updated_report = services.parser_coverage().await.expect("updated coverage");
    assert_eq!(updated_report.repository_generation, 2);
    let updated = updated_report.coverage;
    assert_eq!(updated.indexed.files, 3);
    assert_eq!(updated.recognized.files, 2);
    assert_eq!(updated.complete.files, 1);
    assert_eq!(updated.incomplete.files, 1);
    assert_eq!(updated.unrecognized.files, 1);
    assert_eq!(
        updated.indexed.source_bytes,
        u64::try_from(updated_rust.len() + incomplete_typescript.len() + unrecognized.len())
            .expect("updated source bytes")
    );
}

#[tokio::test]
async fn parser_coverage_reports_an_empty_uninitialized_index() {
    let root = tempfile::tempdir().expect("root");
    let services = Services::open(
        Config::discover(root.path(), Some(root.path().join("index.sqlite"))).expect("config"),
    )
    .expect("services");

    let report = services.parser_coverage().await.expect("empty coverage");

    assert_eq!(report.repository_generation, 0);
    assert_eq!(report.coverage, leantoken::ParserCoverageSummary::default());
}

#[tokio::test]
async fn first_index_reports_uninitialized_while_reconciling() {
    let root = tempfile::tempdir().expect("root");
    std::fs::write(root.path().join("lib.rs"), "fn pending() {}\n").expect("source");
    let database = root.path().join("index.sqlite");
    let services =
        Services::open(Config::discover(root.path(), Some(database.clone())).expect("config"))
            .expect("services");
    let coordination = IndexCoordination::for_database(&database);
    let operation = coordination
        .acquire_operation(&CancellationToken::new())
        .expect("hold reconciliation lock");
    let indexing_services = services.clone();
    let indexing = tokio::spawn(async move {
        indexing_services
            .refresh(leantoken::IndexingMode::Reconcile)
            .await
    });
    tokio::task::yield_now().await;

    let during = services.status().await.expect("status during first index");
    assert_eq!(during.repository_generation, 0);
    assert_eq!(during.index_state, IndexState::Uninitialized);
    assert_eq!(during.freshness, Freshness::Reconciling);
    let progress = during.index_progress.expect("follower progress");
    assert!(!progress.detail_available);
    assert!(progress.active);
    assert_eq!(progress.files_discovered, None);

    drop(operation);
    indexing.await.expect("join index").expect("complete index");
    let after = services.status().await.expect("status after first index");
    assert!(after.repository_generation > 0);
    assert_eq!(after.index_state, IndexState::Ready);
    assert_eq!(after.freshness, Freshness::Current);
    assert_eq!(after.index_progress, None);
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
    exact
        .refresh(leantoken::IndexingMode::Reconcile)
        .await
        .expect("exact refresh");
    estimate
        .refresh(leantoken::IndexingMode::Reconcile)
        .await
        .expect("estimate refresh");
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
        explain_diagnostics: false,
    };

    let (exact_response, estimate_response) =
        tokio::join!(exact.context(request.clone()), estimate.context(request),);

    let exact_response = exact_response.expect("exact context");
    let estimate_response = estimate_response.expect("estimate context");
    assert_response_token_accounting!(exact_response, Tokenizer::O200kBase);
    assert_response_token_accounting!(estimate_response, Tokenizer::Estimate);
}

#[tokio::test]

async fn status_reports_reconciling_when_shared_operation_lock_is_held() {
    let root = tempfile::tempdir().expect("root");
    std::fs::write(root.path().join("lib.rs"), "fn ready() {}\n").expect("write");
    let database = root.path().join("index.sqlite");
    let config = Config::discover(root.path(), Some(database.clone())).expect("config");
    let services = Services::open(config).expect("services");
    services
        .refresh(leantoken::IndexingMode::Reconcile)
        .await
        .expect("refresh");

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
