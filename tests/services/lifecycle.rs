use super::*;

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
            verbose_diagnostics: false,
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
    let before = services.status().await.expect("before status").repository_generation;
    std::fs::write(
        root.path().join("src/lib.rs"),
        "pub fn replacement() -> u8 { 42 }\n",
    )
    .expect("replace source");

    let indexing_services = std::sync::Arc::clone(&services);
    let indexing = tokio::spawn(async move {
        indexing_services
            .index_paths(vec!["src/lib.rs".into()])
            .await
            .expect("reconcile")
    });
    let mut queries = tokio::task::JoinSet::new();
    // Stay within the documented process-local active retrieval bound. Exact
    // overload behavior is covered separately; this test isolates generation
    // consistency while reconciliation publishes a replacement snapshot.
    for index in 0..16 {
        let services = std::sync::Arc::clone(&services);
        queries.spawn(async move {
            let query = if index % 2 == 0 { "greet" } else { "replacement" };
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
        assert!(matches!(response.meta.repository_generation, value if value == before || value == after));
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
    services.index(false).await.expect("rebuild index");
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

#[test]
fn explicit_corrupt_database_is_not_deleted() {
    let root = tempfile::tempdir().expect("root");
    let database = root.path().join("explicit.sqlite");
    let original = b"caller-owned data";
    std::fs::write(&database, original).expect("database fixture");
    let config = Config::discover(root.path(), Some(database.clone())).expect("config");

    Services::open(config).expect_err("explicit corruption must be reported");
    assert_eq!(std::fs::read(database).expect("preserved database"), original);
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
async fn first_index_reports_uninitialized_while_reconciling() {
    let root = tempfile::tempdir().expect("root");
    std::fs::write(root.path().join("lib.rs"), "fn pending() {}\n").expect("source");
    let database = root.path().join("index.sqlite");
    let services = Services::open(
        Config::discover(root.path(), Some(database.clone())).expect("config"),
    )
    .expect("services");
    let coordination = IndexCoordination::for_database(&database);
    let operation = coordination
        .acquire_operation(&CancellationToken::new())
        .expect("hold reconciliation lock");
    let indexing_services = services.clone();
    let indexing = tokio::spawn(async move { indexing_services.index(false).await });
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
