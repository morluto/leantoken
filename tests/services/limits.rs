use super::*;

#[tokio::test]
async fn files_enforces_result_limit_contract() {
    let (_root, services) = fixture().await;
    let limit = services.config().max_results;

    services
        .files(files_limit_request(None))
        .await
        .expect("default result limit");
    for requested in [1, limit] {
        services
            .files(files_limit_request(Some(requested)))
            .await
            .expect("valid result limit");
    }
    let error = services
        .files(files_limit_request(Some(0)))
        .await
        .expect_err("zero result limit");
    assert_zero_limit(error, "max_results");
    let error = services
        .files(files_limit_request(Some(limit + 1)))
        .await
        .expect_err("oversized result limit");
    assert_limit_exceeded(error, "max_results", limit + 1, limit);
}

#[tokio::test]
async fn search_enforces_all_limit_contracts() {
    let (_root, services) = fixture().await;
    let result_limit = services.config().max_results;
    let token_limit = services.config().max_output_tokens;

    services
        .search(search_limit_request(None, None, None))
        .await
        .expect("default search limits");
    for requested in [1, result_limit] {
        services
            .search(search_limit_request(Some(requested), Some(1), Some(0)))
            .await
            .expect("valid result limit");
    }
    let error = services
        .search(search_limit_request(Some(0), Some(1), Some(0)))
        .await
        .expect_err("zero result limit");
    assert_zero_limit(error, "max_results");
    let error = services
        .search(search_limit_request(
            Some(result_limit + 1),
            Some(1),
            Some(0),
        ))
        .await
        .expect_err("oversized result limit");
    assert_limit_exceeded(error, "max_results", result_limit + 1, result_limit);

    for requested in [1, token_limit] {
        services
            .search(search_limit_request(Some(1), Some(requested), Some(0)))
            .await
            .expect("valid token limit");
    }
    let error = services
        .search(search_limit_request(Some(1), Some(0), Some(0)))
        .await
        .expect_err("zero token limit");
    assert_zero_limit(error, "max_tokens");
    let error = services
        .search(search_limit_request(
            Some(1),
            Some(token_limit + 1),
            Some(0),
        ))
        .await
        .expect_err("oversized token limit");
    assert_limit_exceeded(error, "max_tokens", token_limit + 1, token_limit);

    for requested in [0, 1, 20] {
        services
            .search(search_limit_request(Some(1), Some(1), Some(requested)))
            .await
            .expect("valid context-line limit");
    }
    let error = services
        .search(search_limit_request(Some(1), Some(1), Some(21)))
        .await
        .expect_err("oversized context-line limit");
    assert_limit_exceeded(error, "context_lines", 21, 20);
}

#[tokio::test]
async fn outline_enforces_result_and_token_limit_contracts() {
    let (_root, services) = fixture().await;
    let result_limit = services.config().max_results;
    let token_limit = services.config().max_output_tokens;

    services
        .outline(outline_limit_request(None, None))
        .await
        .expect("default outline limits");
    for requested in [1, result_limit] {
        services
            .outline(outline_limit_request(Some(requested), Some(1)))
            .await
            .expect("valid result limit");
    }
    let error = services
        .outline(outline_limit_request(Some(0), Some(1)))
        .await
        .expect_err("zero result limit");
    assert_zero_limit(error, "max_results");
    let error = services
        .outline(outline_limit_request(Some(result_limit + 1), Some(1)))
        .await
        .expect_err("oversized result limit");
    assert_limit_exceeded(error, "max_results", result_limit + 1, result_limit);

    for requested in [1, token_limit] {
        services
            .outline(outline_limit_request(Some(1), Some(requested)))
            .await
            .expect("valid token limit");
    }
    let error = services
        .outline(outline_limit_request(Some(1), Some(0)))
        .await
        .expect_err("zero token limit");
    assert_zero_limit(error, "max_tokens");
    let error = services
        .outline(outline_limit_request(Some(1), Some(token_limit + 1)))
        .await
        .expect_err("oversized token limit");
    assert_limit_exceeded(error, "max_tokens", token_limit + 1, token_limit);
}

#[tokio::test]
async fn read_enforces_token_limit_contract() {
    let (_root, services) = fixture().await;
    let limit = services.config().max_output_tokens;

    services
        .read(read_limit_request(None))
        .await
        .expect("default token limit");
    for requested in [1, limit] {
        services
            .read(read_limit_request(Some(requested)))
            .await
            .expect("valid token limit");
    }
    let error = services
        .read(read_limit_request(Some(0)))
        .await
        .expect_err("zero token limit");
    assert_zero_limit(error, "max_tokens");
    let error = services
        .read(read_limit_request(Some(limit + 1)))
        .await
        .expect_err("oversized token limit");
    assert_limit_exceeded(error, "max_tokens", limit + 1, limit);
}

#[tokio::test]
async fn context_enforces_token_budget_contract() {
    let (_root, services) = fixture().await;
    let limit = services.config().max_output_tokens;

    for requested in [1, limit] {
        services
            .context(context_limit_request(requested))
            .await
            .expect("valid token budget");
    }
    let error = services
        .context(context_limit_request(0))
        .await
        .expect_err("zero token budget");
    assert_zero_limit(error, "token_budget");
    let error = services
        .context(context_limit_request(limit + 1))
        .await
        .expect_err("oversized token budget");
    assert_limit_exceeded(error, "token_budget", limit + 1, limit);
}

#[tokio::test]
async fn context_tiny_budget_does_not_claim_candidates_are_missing() {
    let (_root, services) = fixture().await;

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
    let (root, services) = fixture().await;
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
    let (root, services) = fixture().await;
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

    assert_static_error!(
        services.files_with_consistency_cancellable(
            FilesRequest {
                operation: FileOperation::Find,
                path: None,
                query: None,
                pattern: None,
                max_results: Some(1),
                cursor: None,
                depth: None,
            },
            IndexConsistency::ReconcileWorkingTree,
            CancellationToken::new(),
        ),
        "missing find query"
    );
    assert_static_error!(
        services.files_with_consistency_cancellable(
            FilesRequest {
                operation: FileOperation::Tree,
                path: Some("../outside.rs".into()),
                query: None,
                pattern: None,
                max_results: Some(1),
                cursor: None,
                depth: None,
            },
            IndexConsistency::ReconcileWorkingTree,
            CancellationToken::new(),
        ),
        "unsafe tree root"
    );
    assert_static_error!(
        services.files_with_consistency_cancellable(
            FilesRequest {
                operation: FileOperation::Glob,
                path: None,
                query: None,
                pattern: Some("[".into()),
                max_results: Some(1),
                cursor: None,
                depth: None,
            },
            IndexConsistency::ReconcileWorkingTree,
            CancellationToken::new(),
        ),
        "invalid files glob"
    );
    let mut files = files_limit_request(Some(1));
    files.cursor = Some("invalid".into());
    assert_static_error!(
        services.files_with_consistency_cancellable(
            files,
            IndexConsistency::ReconcileWorkingTree,
            CancellationToken::new(),
        ),
        "malformed files cursor"
    );

    let mut search = search_limit_request(Some(1), Some(1), Some(0));
    search.query = " ".into();
    assert_static_error!(
        services.search_with_consistency_cancellable(
            search,
            IndexConsistency::ReconcileWorkingTree,
            CancellationToken::new(),
        ),
        "empty search query"
    );
    let mut search = search_limit_request(Some(1), Some(1), Some(0));
    search.query = "[".into();
    search.mode = SearchMode::Regex;
    assert_static_error!(
        services.search_with_consistency_cancellable(
            search,
            IndexConsistency::ReconcileWorkingTree,
            CancellationToken::new(),
        ),
        "invalid search regex"
    );
    let mut search = search_limit_request(Some(1), Some(1), Some(0));
    search.focus_paths = vec!["[".into()];
    assert_static_error!(
        services.search_with_consistency_cancellable(
            search,
            IndexConsistency::ReconcileWorkingTree,
            CancellationToken::new(),
        ),
        "invalid search path glob"
    );
    let mut search = search_limit_request(Some(1), Some(1), Some(0));
    search.query = "x".repeat(64 * 1024 + 1);
    assert_static_error!(
        services.search_with_consistency_cancellable(
            search,
            IndexConsistency::ReconcileWorkingTree,
            CancellationToken::new(),
        ),
        "oversized search query"
    );
    let mut search = search_limit_request(Some(1), Some(1), Some(0));
    search.cursor = Some("invalid".into());
    assert_static_error!(
        services.search_with_consistency_cancellable(
            search,
            IndexConsistency::ReconcileWorkingTree,
            CancellationToken::new(),
        ),
        "malformed search cursor"
    );

    let mut outline = outline_limit_request(Some(1), Some(1));
    outline.paths = Vec::new();
    assert_static_error!(
        services.outline_with_consistency_cancellable(
            outline,
            IndexConsistency::ReconcileWorkingTree,
            CancellationToken::new(),
        ),
        "empty outline paths"
    );
    let mut outline = outline_limit_request(Some(1), Some(1));
    outline.paths = (0..257).map(|index| format!("src/{index}.rs")).collect();
    assert_static_error!(
        services.outline_with_consistency_cancellable(
            outline,
            IndexConsistency::ReconcileWorkingTree,
            CancellationToken::new(),
        ),
        "excessive outline paths"
    );
    let mut outline = outline_limit_request(Some(1), Some(1));
    outline.paths = vec!["../outside.rs".into()];
    assert_static_error!(
        services.outline_with_consistency_cancellable(
            outline,
            IndexConsistency::ReconcileWorkingTree,
            CancellationToken::new(),
        ),
        "unsafe outline path"
    );

    let mut read = read_limit_request(Some(1));
    read.start_line = Some(0);
    assert_static_error!(
        services.read_with_consistency_cancellable(
            read,
            IndexConsistency::ReconcileWorkingTree,
            CancellationToken::new(),
        ),
        "invalid read range"
    );
    let mut read = read_limit_request(Some(1));
    read.symbol = Some("greet".into());
    assert_static_error!(
        services.read_with_consistency_cancellable(
            read,
            IndexConsistency::ReconcileWorkingTree,
            CancellationToken::new(),
        ),
        "conflicting read target"
    );
    let mut read = read_limit_request(Some(1));
    read.start_line = None;
    read.end_line = None;
    read.symbol = Some(String::new());
    let error = services
        .read_with_consistency_cancellable(
            read,
            IndexConsistency::ReconcileWorkingTree,
            CancellationToken::new(),
        )
        .await
        .expect_err("empty read symbol must fail");
    let current = services
        .status()
        .await
        .expect("status after empty read symbol");
    assert_eq!(
        current.repository_generation, generation,
        "empty read symbol must not reconcile"
    );
    assert!(
        matches!(
            error,
            Error::InvalidInput {
                field: "symbol",
                reason: "must not be empty"
            }
        ),
        "unexpected empty read symbol error: {error:?}"
    );
    expected_failures += 1;

    let mut context = context_limit_request(1);
    context.task = " ".into();
    assert_static_error!(
        services.context_with_consistency_cancellable(
            context,
            IndexConsistency::ReconcileWorkingTree,
            CancellationToken::new(),
        ),
        "empty context task"
    );
    let mut context = context_limit_request(1);
    context.focus_paths = vec!["[".into()];
    assert_static_error!(
        services.context_with_consistency_cancellable(
            context,
            IndexConsistency::ReconcileWorkingTree,
            CancellationToken::new(),
        ),
        "invalid context path glob"
    );
    let mut context = context_limit_request(1);
    context.focus_symbols = vec!["symbol".into(); 257];
    assert_static_error!(
        services.context_with_consistency_cancellable(
            context,
            IndexConsistency::ReconcileWorkingTree,
            CancellationToken::new(),
        ),
        "excessive context symbols"
    );
    let mut context = context_limit_request(1);
    context.changed_paths = vec!["../outside.rs".into()];
    assert_static_error!(
        services.context_with_consistency_cancellable(
            context,
            IndexConsistency::ReconcileWorkingTree,
            CancellationToken::new(),
        ),
        "unsafe context changed path"
    );
    let mut context = context_limit_request(1);
    context.base_revision = Some("r".repeat(257));
    assert_static_error!(
        services.context_with_consistency_cancellable(
            context,
            IndexConsistency::ReconcileWorkingTree,
            CancellationToken::new(),
        ),
        "oversized context base revision"
    );
    let mut context = context_limit_request(1);
    context.changed_paths = (0..513).map(|index| format!("src/{index}.rs")).collect();
    assert_static_error!(
        services.context_with_consistency_cancellable(
            context,
            IndexConsistency::ReconcileWorkingTree,
            CancellationToken::new(),
        ),
        "excessive context changed paths"
    );
    let mut context = context_limit_request(1);
    context.task = "a_".repeat(30_000);
    assert_static_error!(
        services.context_with_consistency_cancellable(
            context,
            IndexConsistency::ReconcileWorkingTree,
            CancellationToken::new(),
        ),
        "oversized derived context matcher"
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
    let (root, services) = fixture().await;
    let generation = services
        .status()
        .await
        .expect("initial status")
        .repository_generation;
    std::fs::write(
        root.path().join("src/reconciled.rs"),
        "pub fn reconciled() {}\n",
    )
    .expect("write unindexed source");

    let mut request = search_limit_request(Some(1), Some(1), Some(0));
    // Create a cursor for the current generation using the new authenticated format.
    // After reconciliation, the generation will change, making this cursor stale.
    let cursor_hash = leantoken::services::cursor::binding_hash(&["query", "text"]);
    request.cursor = Some(leantoken::services::cursor::encode_cursor(
        "search",
        generation,
        &cursor_hash,
        0,
    ));
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
