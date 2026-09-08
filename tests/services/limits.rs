use super::*;

// Exhaustive integer boundaries live in services::request_limits unit tests.
// This indexed witness proves that all public adapters consume those limits.
#[tokio::test]
async fn configured_limits_reach_all_service_operations() {
    let root = tempfile::tempdir().expect("configured indexed repository");
    std::fs::create_dir(root.path().join("src")).unwrap();
    let source = (0..8)
        .map(|index| {
            format!(
                "pub fn greet_{index}() -> usize {{\n    let value = {index};\n    value + 1\n}}\n"
            )
        })
        .collect::<String>();
    for name in ["lib.rs", "second.rs", "third.rs"] {
        std::fs::write(root.path().join("src").join(name), &source).unwrap();
    }
    let mut config = Config::discover(root.path(), Some(root.path().join("index.sqlite"))).unwrap();
    config.default_results = 2;
    config.max_results = 2;
    config.default_read_tokens = 50;
    config.default_context_tokens = 40;
    config.max_output_tokens = 50;
    config.context_lines = 0;
    let services = Services::open(config).unwrap();
    services
        .index(leantoken::IndexingMode::Reconcile)
        .await
        .unwrap();
    let mut files_request = files_limit_request(None);
    files_request.path = Some("src".into());
    files_request.depth = None;
    let files = services.files(files_request).await.unwrap();
    assert_eq!(files.entries.len(), 2);
    let search = services
        .search(search_limit_request(None, None, None))
        .await
        .expect("default search limits");
    assert_eq!(search.hits.len(), 2);
    assert!(search.hits.iter().all(|hit| hit.start_line == hit.end_line));
    assert!(search.meta.source_tokens <= 50);
    let outline = services
        .outline(outline_limit_request(None, None))
        .await
        .expect("default outline limits");
    assert_eq!(outline.total_symbols, 8);
    assert_eq!(outline.returned_symbols, 2);
    assert!(outline.truncated_by_max_results);
    assert!(outline.meta.source_tokens <= 50);
    let mut read_request = read_limit_request(None);
    read_request.end_line = None;
    let read = services
        .read(read_request)
        .await
        .expect("default read limit");
    assert!(read.meta.source_tokens > 0 && read.meta.source_tokens <= 50);
    let context = services
        .context(context_limit_request(
            services.config().default_context_tokens,
        ))
        .await
        .expect("configured context budget");
    assert!(context.meta.source_tokens <= 40);
}

#[tokio::test]
async fn context_tiny_budget_does_not_claim_candidates_are_missing() {
    let (_root, services) = indexed_fixture().await;

    let response = services
        .context(context_limit_request(1))
        .await
        .expect("tiny valid token budget");

    assert!(response.fragments.is_empty());
    assert!(response.omission_summary.budget_or_result_limit > 0);
    assert!(
        !response
            .warnings
            .iter()
            .any(|warning| warning == "no relevant indexed evidence found")
    );
}

#[tokio::test]
async fn reconcile_working_tree_limit_errors_do_not_reconcile_the_index() {
    let (root, services) = indexed_fixture().await;
    let generation = services
        .status()
        .await
        .expect("initial status")
        .repository_generation;
    std::fs::write(
        root.path().join("src/unreconciled.rs"),
        "pub fn unreconciled() {}\n",
    )
    .expect("write unindexed source");

    let error = services
        .files_with_consistency_cancellable(
            files_limit_request(Some(0)),
            IndexConsistency::ReconcileWorkingTree,
            CancellationToken::new(),
        )
        .await
        .expect_err("invalid files limit");
    assert_zero_limit(error, "max_results");

    for (request, field) in [
        (
            search_limit_request(Some(0), Some(1), Some(0)),
            "max_results",
        ),
        (
            search_limit_request(Some(1), Some(0), Some(0)),
            "max_tokens",
        ),
    ] {
        let error = services
            .search_with_consistency_cancellable(
                request,
                IndexConsistency::ReconcileWorkingTree,
                CancellationToken::new(),
            )
            .await
            .expect_err("invalid search limit");
        assert_zero_limit(error, field);
    }
    let error = services
        .search_with_consistency_cancellable(
            search_limit_request(Some(1), Some(1), Some(21)),
            IndexConsistency::ReconcileWorkingTree,
            CancellationToken::new(),
        )
        .await
        .expect_err("invalid search context limit");
    assert_limit_exceeded(error, "context_lines", 21, 20);

    for (request, field) in [
        (outline_limit_request(Some(0), Some(1)), "max_results"),
        (outline_limit_request(Some(1), Some(0)), "max_tokens"),
    ] {
        let error = services
            .outline_with_consistency_cancellable(
                request,
                IndexConsistency::ReconcileWorkingTree,
                CancellationToken::new(),
            )
            .await
            .expect_err("invalid outline limit");
        assert_zero_limit(error, field);
    }

    let error = services
        .read_with_consistency_cancellable(
            read_limit_request(Some(0)),
            IndexConsistency::ReconcileWorkingTree,
            CancellationToken::new(),
        )
        .await
        .expect_err("invalid read limit");
    assert_zero_limit(error, "max_tokens");
    let error = services
        .context_with_consistency_cancellable(
            context_limit_request(0),
            IndexConsistency::ReconcileWorkingTree,
            CancellationToken::new(),
        )
        .await
        .expect_err("invalid context limit");
    assert_zero_limit(error, "token_budget");

    let after = services
        .status()
        .await
        .expect("status after invalid requests");
    assert_eq!(after.repository_generation, generation);
    let committed = services
        .files(FilesRequest {
            operation: FileOperation::Find,
            path: None,
            query: Some("unreconciled".into()),
            pattern: None,
            max_results: Some(1),
            cursor: None,
            depth: None,
        })
        .await
        .expect("committed lookup");
    assert!(committed.entries.is_empty());
}

#[tokio::test]
async fn reconcile_working_tree_static_input_errors_do_not_reconcile_the_index() {
    let (root, services) = indexed_fixture().await;
    let generation = services
        .status()
        .await
        .expect("initial status")
        .repository_generation;
    std::fs::write(
        root.path().join("src/unreconciled.rs"),
        "pub fn unreconciled() {}\n",
    )
    .expect("write unindexed source");
    let mut expected_failures = 0u64;

    macro_rules! assert_static_error {
        ($future:expr, $case:literal) => {{
            assert!($future.await.is_err(), concat!($case, " must fail"));
            expected_failures += 1;
            let current = services.status().await.expect("status after static error");
            assert_eq!(
                current.repository_generation, generation,
                concat!($case, " must not reconcile")
            );
        }};
    }

    let mut request = files_limit_request(Some(1));
    request.operation = FileOperation::Find;
    assert_static_error!(
        services.files_with_consistency_cancellable(
            request,
            IndexConsistency::ReconcileWorkingTree,
            CancellationToken::new()
        ),
        "missing files query"
    );

    let mut request = search_limit_request(Some(1), Some(1), Some(0));
    request.query = " ".into();
    assert_static_error!(
        services.search_with_consistency_cancellable(
            request,
            IndexConsistency::ReconcileWorkingTree,
            CancellationToken::new()
        ),
        "empty search query"
    );

    let mut request = outline_limit_request(Some(1), Some(1));
    request.paths.clear();
    assert_static_error!(
        services.outline_with_consistency_cancellable(
            request,
            IndexConsistency::ReconcileWorkingTree,
            CancellationToken::new()
        ),
        "empty outline paths"
    );

    let mut request = read_limit_request(Some(1));
    request.symbol = Some("greet".into());
    assert_static_error!(
        services.read_with_consistency_cancellable(
            request,
            IndexConsistency::ReconcileWorkingTree,
            CancellationToken::new()
        ),
        "conflicting read target"
    );

    let mut request = context_limit_request(1);
    request.task = " ".into();
    assert_static_error!(
        services.context_with_consistency_cancellable(
            request,
            IndexConsistency::ReconcileWorkingTree,
            CancellationToken::new()
        ),
        "empty context task"
    );

    let committed = services
        .files(FilesRequest {
            operation: FileOperation::Find,
            path: None,
            query: Some("unreconciled".into()),
            pattern: None,
            max_results: Some(1),
            cursor: None,
            depth: None,
        })
        .await
        .expect("committed lookup");
    assert!(committed.entries.is_empty());
    let observed = services
        .observed_token_savings_report()
        .await
        .expect("observed static failures");
    assert_eq!(
        observed.observations.failed_service_requests, expected_failures,
        "each failed public service request must be observed exactly once"
    );
    assert_eq!(
        observed
            .observations
            .failed_by_operation_and_category
            .iter()
            .map(|failure| failure.failed_requests)
            .sum::<u64>(),
        expected_failures
    );
}

#[tokio::test]
async fn reconcile_working_tree_generation_checks_run_after_reconciliation() {
    let (root, services) = indexed_fixture().await;
    std::fs::write(
        root.path().join("src/second.rs"),
        "pub fn greet_again() { let _ = \"greet\"; }\n",
    )
    .expect("write second indexed source");
    services
        .index(leantoken::IndexingMode::Reconcile)
        .await
        .expect("index second source");
    let generation = services
        .status()
        .await
        .expect("initial status")
        .repository_generation;
    let request = search_limit_request(Some(1), Some(1_000), Some(0));
    let cursor = services
        .search(request.clone())
        .await
        .expect("pre-reconciliation search")
        .meta
        .next_cursor
        .expect("pre-reconciliation cursor");
    std::fs::write(
        root.path().join("src/reconciled.rs"),
        "pub fn reconciled() {}\n",
    )
    .expect("write unindexed source");

    let mut request = request;
    request.cursor = Some(cursor);
    let error = services
        .search_with_consistency_cancellable(
            request,
            IndexConsistency::ReconcileWorkingTree,
            CancellationToken::new(),
        )
        .await
        .expect_err("cursor from the pre-reconciliation generation must be stale");
    assert!(matches!(error, Error::StaleCursor));

    let after = services
        .status()
        .await
        .expect("status after reconciliation");
    assert!(after.repository_generation > generation);
    let committed = services
        .files(FilesRequest {
            operation: FileOperation::Find,
            path: None,
            query: Some("reconciled".into()),
            pattern: None,
            max_results: Some(1),
            cursor: None,
            depth: None,
        })
        .await
        .expect("committed lookup");
    assert_eq!(committed.entries.len(), 1);
}
