use std::time::Instant;

use leantoken::{
    Config, ContextRequest, ContextSignalPolicy, Error, FileOperation, FilesRequest, Freshness,
    IndexConsistency, IndexState, OutlineRequest, ReadRequest, ReadStatus, SearchMode, SearchRequest,
    TokenSavingsOperation, coordination::IndexCoordination, services::Services,
    tokens::Tokenizer,
};
use tokio_util::sync::CancellationToken;

async fn fixture() -> (tempfile::TempDir, Services) {
    let root = tempfile::tempdir().expect("temporary repository");
    std::fs::create_dir(root.path().join("src")).expect("create src");
    std::fs::write(
        root.path().join("src/lib.rs"),
        "pub fn greet(name: &str) -> String {\n    format!(\"hello {name}\")\n}\n\npub fn caller() {\n    let _ = greet(\"agent\");\n}\n",
    )
    .expect("write fixture");
    let config =
        Config::discover(root.path(), Some(root.path().join("index.sqlite"))).expect("config");
    let services = Services::open(config).expect("services");
    services.index(false).await.expect("index fixture");
    (root, services)
}

fn assert_zero_limit(error: Error, expected_field: &'static str) {
    assert!(
        matches!(
            error,
            Error::InvalidInput {
                field,
                reason: "must be greater than zero"
            } if field == expected_field
        ),
        "unexpected zero-limit error: {error:?}"
    );
}

fn assert_limit_exceeded(
    error: Error,
    expected_field: &'static str,
    expected_requested: usize,
    expected_limit: usize,
) {
    assert!(
        matches!(
            error,
            Error::RequestLimitExceeded {
                field,
                requested,
                limit,
            } if field == expected_field
                && requested == expected_requested
                && limit == expected_limit
        ),
        "unexpected request-limit error: {error:?}"
    );
}

fn files_limit_request(max_results: Option<usize>) -> FilesRequest {
    FilesRequest {
        operation: FileOperation::Tree,
        path: None,
        query: None,
        pattern: None,
        max_results,
        cursor: None,
        depth: Some(0),
    }
}

fn search_limit_request(
    max_results: Option<usize>,
    max_tokens: Option<usize>,
    context_lines: Option<usize>,
) -> SearchRequest {
    SearchRequest {
        query: "greet".into(),
        mode: SearchMode::Text,
        include_paths: Vec::new(),
        exclude_paths: Vec::new(),
        focus_paths: Vec::new(),
        max_results,
        max_tokens,
        context_lines,
        case_sensitive: false,
        cursor: None,
    }
}

fn outline_limit_request(
    max_results: Option<usize>,
    max_tokens: Option<usize>,
) -> OutlineRequest {
    OutlineRequest {
        paths: vec!["src/lib.rs".into()],
        symbol_name: None,
        symbol_kind: None,
        max_results,
        max_tokens,
    }
}

fn read_limit_request(max_tokens: Option<usize>) -> ReadRequest {
    ReadRequest {
        path: "src/lib.rs".into(),
        start_line: Some(1),
        end_line: Some(1),
        symbol: None,
        max_tokens,
        expected_hash: None,
    }
}

fn context_limit_request(token_budget: usize) -> ContextRequest {
    ContextRequest {
        task: "find greet".into(),
        token_budget,
        focus_paths: Vec::new(),
        focus_symbols: Vec::new(),
        exclude_paths: Vec::new(),
        known_hashes: Vec::new(),
        prior_repository_generation: None,
        base_revision: None,
        changed_paths: Vec::new(),
    }
}

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
async fn working_tree_limit_errors_do_not_reconcile_the_index() {
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
            IndexConsistency::WorkingTree,
            CancellationToken::new(),
        )
        .await
        .expect_err("invalid files limit");
    assert_zero_limit(error, "max_results");

    for (request, field) in [
        (search_limit_request(Some(0), Some(1), Some(0)), "max_results"),
        (search_limit_request(Some(1), Some(0), Some(0)), "max_tokens"),
    ] {
        let error = services
            .search_with_consistency_cancellable(
                request,
                IndexConsistency::WorkingTree,
                CancellationToken::new(),
            )
            .await
            .expect_err("invalid search limit");
        assert_zero_limit(error, field);
    }
    let error = services
        .search_with_consistency_cancellable(
            search_limit_request(Some(1), Some(1), Some(21)),
            IndexConsistency::WorkingTree,
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
                IndexConsistency::WorkingTree,
                CancellationToken::new(),
            )
            .await
            .expect_err("invalid outline limit");
        assert_zero_limit(error, field);
    }

    let error = services
        .read_with_consistency_cancellable(
            read_limit_request(Some(0)),
            IndexConsistency::WorkingTree,
            CancellationToken::new(),
        )
        .await
        .expect_err("invalid read limit");
    assert_zero_limit(error, "max_tokens");
    let error = services
        .context_with_consistency_cancellable(
            context_limit_request(0),
            IndexConsistency::WorkingTree,
            CancellationToken::new(),
        )
        .await
        .expect_err("invalid context limit");
    assert_zero_limit(error, "token_budget");

    let after = services.status().await.expect("status after invalid requests");
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
async fn working_tree_static_input_errors_do_not_reconcile_the_index() {
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

    macro_rules! assert_static_error {
        ($future:expr, $case:literal) => {{
            assert!($future.await.is_err(), concat!($case, " must fail"));
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
            IndexConsistency::WorkingTree,
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
            IndexConsistency::WorkingTree,
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
            IndexConsistency::WorkingTree,
            CancellationToken::new(),
        ),
        "invalid files glob"
    );
    let mut files = files_limit_request(Some(1));
    files.cursor = Some("invalid".into());
    assert_static_error!(
        services.files_with_consistency_cancellable(
            files,
            IndexConsistency::WorkingTree,
            CancellationToken::new(),
        ),
        "malformed files cursor"
    );

    let mut search = search_limit_request(Some(1), Some(1), Some(0));
    search.query = " ".into();
    assert_static_error!(
        services.search_with_consistency_cancellable(
            search,
            IndexConsistency::WorkingTree,
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
            IndexConsistency::WorkingTree,
            CancellationToken::new(),
        ),
        "invalid search regex"
    );
    let mut search = search_limit_request(Some(1), Some(1), Some(0));
    search.focus_paths = vec!["[".into()];
    assert_static_error!(
        services.search_with_consistency_cancellable(
            search,
            IndexConsistency::WorkingTree,
            CancellationToken::new(),
        ),
        "invalid search path glob"
    );
    let mut search = search_limit_request(Some(1), Some(1), Some(0));
    search.query = "x".repeat(64 * 1024 + 1);
    assert_static_error!(
        services.search_with_consistency_cancellable(
            search,
            IndexConsistency::WorkingTree,
            CancellationToken::new(),
        ),
        "oversized search query"
    );
    let mut search = search_limit_request(Some(1), Some(1), Some(0));
    search.cursor = Some("invalid".into());
    assert_static_error!(
        services.search_with_consistency_cancellable(
            search,
            IndexConsistency::WorkingTree,
            CancellationToken::new(),
        ),
        "malformed search cursor"
    );

    let mut outline = outline_limit_request(Some(1), Some(1));
    outline.paths = Vec::new();
    assert_static_error!(
        services.outline_with_consistency_cancellable(
            outline,
            IndexConsistency::WorkingTree,
            CancellationToken::new(),
        ),
        "empty outline paths"
    );
    let mut outline = outline_limit_request(Some(1), Some(1));
    outline.paths = (0..257).map(|index| format!("src/{index}.rs")).collect();
    assert_static_error!(
        services.outline_with_consistency_cancellable(
            outline,
            IndexConsistency::WorkingTree,
            CancellationToken::new(),
        ),
        "excessive outline paths"
    );
    let mut outline = outline_limit_request(Some(1), Some(1));
    outline.paths = vec!["../outside.rs".into()];
    assert_static_error!(
        services.outline_with_consistency_cancellable(
            outline,
            IndexConsistency::WorkingTree,
            CancellationToken::new(),
        ),
        "unsafe outline path"
    );

    let mut read = read_limit_request(Some(1));
    read.start_line = Some(0);
    assert_static_error!(
        services.read_with_consistency_cancellable(
            read,
            IndexConsistency::WorkingTree,
            CancellationToken::new(),
        ),
        "invalid read range"
    );
    let mut read = read_limit_request(Some(1));
    read.symbol = Some("greet".into());
    assert_static_error!(
        services.read_with_consistency_cancellable(
            read,
            IndexConsistency::WorkingTree,
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
            IndexConsistency::WorkingTree,
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

    let mut context = context_limit_request(1);
    context.task = " ".into();
    assert_static_error!(
        services.context_with_consistency_cancellable(
            context,
            IndexConsistency::WorkingTree,
            CancellationToken::new(),
        ),
        "empty context task"
    );
    let mut context = context_limit_request(1);
    context.focus_paths = vec!["[".into()];
    assert_static_error!(
        services.context_with_consistency_cancellable(
            context,
            IndexConsistency::WorkingTree,
            CancellationToken::new(),
        ),
        "invalid context path glob"
    );
    let mut context = context_limit_request(1);
    context.focus_symbols = vec!["symbol".into(); 257];
    assert_static_error!(
        services.context_with_consistency_cancellable(
            context,
            IndexConsistency::WorkingTree,
            CancellationToken::new(),
        ),
        "excessive context symbols"
    );
    let mut context = context_limit_request(1);
    context.changed_paths = vec!["../outside.rs".into()];
    assert_static_error!(
        services.context_with_consistency_cancellable(
            context,
            IndexConsistency::WorkingTree,
            CancellationToken::new(),
        ),
        "unsafe context changed path"
    );
    let mut context = context_limit_request(1);
    context.base_revision = Some("r".repeat(257));
    assert_static_error!(
        services.context_with_consistency_cancellable(
            context,
            IndexConsistency::WorkingTree,
            CancellationToken::new(),
        ),
        "oversized context base revision"
    );
    let mut context = context_limit_request(1);
    context.changed_paths = (0..513).map(|index| format!("src/{index}.rs")).collect();
    assert_static_error!(
        services.context_with_consistency_cancellable(
            context,
            IndexConsistency::WorkingTree,
            CancellationToken::new(),
        ),
        "excessive context changed paths"
    );
    let mut context = context_limit_request(1);
    context.task = "a_".repeat(30_000);
    assert_static_error!(
        services.context_with_consistency_cancellable(
            context,
            IndexConsistency::WorkingTree,
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
}

#[tokio::test]
async fn working_tree_generation_checks_run_after_reconciliation() {
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
    request.cursor = Some(format!("{generation}:0"));
    let error = services
        .search_with_consistency_cancellable(
            request,
            IndexConsistency::WorkingTree,
            CancellationToken::new(),
        )
        .await
        .expect_err("cursor from the pre-reconciliation generation must be stale");
    assert!(matches!(error, Error::StaleCursor));

    let after = services.status().await.expect("status after reconciliation");
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

async fn tree_pages(
    services: &Services,
    path: Option<&str>,
) -> Vec<(serde_json::Value, Option<String>)> {
    let mut cursor = None;
    let mut pages = Vec::new();
    loop {
        let response = services
            .files(FilesRequest {
                operation: FileOperation::Tree,
                path: path.map(str::to_owned),
                query: None,
                pattern: None,
                max_results: Some(2),
                cursor,
                depth: Some(2),
            })
            .await
            .expect("tree page");
        let next = response.meta.next_cursor;
        pages.push((
            serde_json::to_value(response.entries).expect("serialize tree entries"),
            next.clone(),
        ));
        let Some(next) = next else {
            break;
        };
        cursor = Some(next);
    }
    pages
}

async fn indexed_source(path: &str, content: &[u8]) -> (tempfile::TempDir, Services) {
    let root = tempfile::tempdir().expect("temporary repository");
    let source_path = root.path().join(path);
    if let Some(parent) = source_path.parent() {
        std::fs::create_dir_all(parent).expect("create source parent");
    }
    std::fs::write(source_path, content).expect("write source");
    let config =
        Config::discover(root.path(), Some(root.path().join("index.sqlite"))).expect("config");
    let services = Services::open(config).expect("services");
    services.index(false).await.expect("index source");
    (root, services)
}

#[test]
fn services_reject_database_owned_by_another_repository() {
    let first_root = tempfile::tempdir().expect("first root");
    let second_root = tempfile::tempdir().expect("second root");
    let cache = tempfile::tempdir().expect("cache");
    let database = cache.path().join("shared.sqlite");

    let first_config =
        Config::discover(first_root.path(), Some(database.clone())).expect("first config");
    let first = Services::open(first_config).expect("claim database");
    let second_config =
        Config::discover(second_root.path(), Some(database.clone())).expect("second config");
    let error = Services::open(second_config).expect_err("different root must be rejected");

    assert!(matches!(error, Error::RepositoryMismatch { .. }));
    drop(first);
    Services::open(
        Config::discover(first_root.path(), Some(database)).expect("same-root config"),
    )
    .expect("same root may share database");
}

#[tokio::test]
async fn same_repository_services_share_committed_generations() {
    let root = tempfile::tempdir().expect("root");
    std::fs::write(root.path().join("lib.rs"), "fn shared() {}\n").expect("source");
    let database = root.path().join("index.sqlite");
    let first = Services::open(
        Config::discover(root.path(), Some(database.clone())).expect("first config"),
    )
    .expect("first services");
    let second = Services::open(
        Config::discover(root.path(), Some(database)).expect("second config"),
    )
    .expect("second services");

    let indexed = first.index(false).await.expect("index");
    let observed = second.status().await.expect("follower status");

    assert_eq!(observed.repository_generation, indexed.repository_generation);
}

#[tokio::test]
async fn independent_repositories_index_concurrently_without_result_leakage() {
    let first_root = tempfile::tempdir().expect("first root");
    let second_root = tempfile::tempdir().expect("second root");
    let cache = tempfile::tempdir().expect("cache");
    std::fs::write(first_root.path().join("first.rs"), "fn alpha_only() {}\n")
        .expect("first source");
    std::fs::write(second_root.path().join("second.rs"), "fn beta_only() {}\n")
        .expect("second source");
    let first = Services::open(
        Config::discover(first_root.path(), Some(cache.path().join("first.sqlite")))
            .expect("first config"),
    )
    .expect("first services");
    let second = Services::open(
        Config::discover(second_root.path(), Some(cache.path().join("second.sqlite")))
            .expect("second config"),
    )
    .expect("second services");

    let (first_index, second_index) = tokio::join!(first.index(false), second.index(false));
    first_index.expect("first index");
    second_index.expect("second index");
    let first_status = first.status().await.expect("first status");
    let second_status = second.status().await.expect("second status");

    assert_eq!(first_status.file_count, 1);
    assert_eq!(second_status.file_count, 1);
    assert_ne!(first.config().database_path, second.config().database_path);
}

#[cfg(unix)]
#[tokio::test]
async fn index_excludes_database_below_missing_symlinked_parent() {
    let root = tempfile::tempdir().expect("root");
    let aliases = tempfile::tempdir().expect("aliases");
    let alias = aliases.path().join("repository");
    std::os::unix::fs::symlink(root.path(), &alias).expect("symlink root");
    std::fs::write(root.path().join("lib.rs"), "fn source() {}\n").expect("source");

    let config = Config::discover(
        root.path(),
        Some(alias.join("missing/cache/index.sqlite")),
    )
    .expect("config");
    let services = Services::open(config).expect("services");
    services.index(false).await.expect("index");

    let files = services
        .files(FilesRequest {
            operation: FileOperation::Tree,
            path: None,
            query: None,
            pattern: None,
            max_results: Some(100),
            cursor: None,
            depth: Some(8),
        })
        .await
        .expect("files");
    assert!(files.entries.iter().any(|entry| entry.path == "lib.rs"));
    assert!(
        files
            .entries
            .iter()
            .all(|entry| !entry.path.starts_with("missing/cache/index.sqlite")),
        "database artifacts leaked into the index: {:?}",
        files.entries
    );
}

#[tokio::test]
async fn database_artifact_notifications_do_not_publish_a_generation() {
    let (_root, services) = fixture().await;
    let before = services
        .status()
        .await
        .expect("status before artifacts")
        .repository_generation;

    let response = services
        .index_paths_report(vec![
            "index.sqlite".into(),
            "index.sqlite-wal".into(),
            "index.sqlite-shm".into(),
        ])
        .await
        .expect("ignore database artifacts");

    assert_eq!(response.repository_generation, before);
    assert_eq!(response.files_indexed, 0);
    assert_eq!(response.files_removed, 0);
    assert_eq!(response.files_unchanged, 0);
    assert_eq!(response.files_skipped, 0);
    assert_eq!(
        response
            .skip_reasons
            .as_ref()
            .expect("current skip reasons")
            .total(),
        0
    );
    assert!(response.warnings.is_empty());
    assert_eq!(
        services
            .status()
            .await
            .expect("status after artifacts")
            .repository_generation,
        before
    );
}

#[tokio::test]
async fn five_services_return_bounded_grounded_responses() {
    let (_root, services) = fixture().await;

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
            cursor: None,
        })
        .await
        .expect("search");
    assert!(!search.hits.is_empty());
    assert!(search.meta.emitted_tokens <= 200);
    assert!(search.hits.iter().all(|hit| hit.start_line <= hit.end_line));

    let outline = services
        .outline(OutlineRequest {
            paths: vec!["src/lib.rs".into()],
            symbol_name: None,
            symbol_kind: None,
            max_results: Some(10),
            max_tokens: Some(100),
        })
        .await
        .expect("outline");
    assert!(
        outline.files[0]
            .symbols
            .iter()
            .any(|symbol| symbol.name == "greet")
    );
    assert!(outline.meta.emitted_tokens <= 100);

    let first = services
        .read(ReadRequest {
            path: "src/lib.rs".into(),
            start_line: Some(1),
            end_line: Some(3),
            symbol: None,
            max_tokens: Some(100),
            expected_hash: None,
        })
        .await
        .expect("first read");
    let second = services
        .read(ReadRequest {
            path: "src/lib.rs".into(),
            start_line: Some(1),
            end_line: Some(3),
            symbol: None,
            max_tokens: Some(100),
            expected_hash: Some(first.content_hash.clone()),
        })
        .await
        .expect("conditional read");
    assert_eq!(second.status, ReadStatus::NotModified);
    assert!(second.content.is_none());
    assert_eq!(second.meta.emitted_tokens, 0);

    let context = services
        .context(ContextRequest {
            task: "change greet caller".into(),
            token_budget: 200,
            focus_paths: Vec::new(),
            focus_symbols: Vec::new(),
            exclude_paths: Vec::new(),
            known_hashes: Vec::new(),
            prior_repository_generation: None,
        base_revision: None,
        changed_paths: Vec::new(),
        })
        .await
        .expect("context");
    assert!(!context.fragments.is_empty());
    assert!(context.meta.emitted_tokens <= 200);
    assert_eq!(
        context.receipt.fragment_hashes.len(),
        context.fragments.len()
    );
    let repeated_context = services
        .context(ContextRequest {
            task: "change greet caller".into(),
            token_budget: 200,
            focus_paths: Vec::new(),
            focus_symbols: Vec::new(),
            exclude_paths: Vec::new(),
            known_hashes: Vec::new(),
            prior_repository_generation: None,
        base_revision: None,
        changed_paths: Vec::new(),
        })
        .await
        .expect("repeated context");
    assert_eq!(
        serde_json::to_string(&repeated_context).expect("serialize repeated context"),
        serde_json::to_string(&context).expect("serialize context"),
        "the same repository generation and request must be deterministic"
    );

    let known = context.fragments[0].content_hash.clone();
    let delta = services
        .context(ContextRequest {
            task: "change greet caller".into(),
            token_budget: 200,
            focus_paths: Vec::new(),
            focus_symbols: Vec::new(),
            exclude_paths: Vec::new(),
            known_hashes: vec![known.clone()],
            prior_repository_generation: Some(context.meta.repository_generation),
        base_revision: None,
        changed_paths: Vec::new(),
        })
        .await
        .expect("context delta");
    assert!(
        delta
            .fragments
            .iter()
            .all(|fragment| fragment.content_hash != known)
    );
}

#[tokio::test]
async fn token_savings_tracks_successful_source_retrievals_by_operation() {
    let (root, services) = fixture().await;
    let initial = services.token_savings().await.expect("initial savings");
    assert_eq!(initial.tracked_requests, 0);
    assert_eq!(initial.estimated_source_tokens_saved, 0);
    assert_eq!(initial.by_operation.len(), 4);

    let search = services
        .search(search_limit_request(Some(5), Some(100), Some(1)))
        .await
        .expect("search");
    let outline = services
        .outline(outline_limit_request(Some(10), Some(100)))
        .await
        .expect("outline");
    let first_read = services
        .read(ReadRequest {
            path: "src/lib.rs".into(),
            start_line: Some(1),
            end_line: Some(3),
            symbol: None,
            max_tokens: Some(100),
            expected_hash: None,
        })
        .await
        .expect("first read");
    let repeated_read = services
        .read(ReadRequest {
            path: "src/lib.rs".into(),
            start_line: Some(1),
            end_line: Some(3),
            symbol: None,
            max_tokens: Some(100),
            expected_hash: Some(first_read.content_hash),
        })
        .await
        .expect("conditional read");
    let context = services
        .context(context_limit_request(200))
        .await
        .expect("context");

    assert_eq!(repeated_read.status, ReadStatus::NotModified);
    let report = services.token_savings().await.expect("tracked savings");
    assert_eq!(report.tokenizer, services.config().tokenizer.name());
    assert_eq!(report.tracked_requests, 5);
    assert_eq!(report.by_operation.len(), 4);
    assert_eq!(
        report
            .by_operation
            .iter()
            .map(|row| (row.operation, row.tracked_requests))
            .collect::<Vec<_>>(),
        vec![
            (TokenSavingsOperation::Search, 1),
            (TokenSavingsOperation::Outline, 1),
            (TokenSavingsOperation::Read, 2),
            (TokenSavingsOperation::Context, 1),
        ]
    );
    assert_eq!(
        report.emitted_source_tokens,
        search.meta.emitted_tokens as u64
            + outline.meta.emitted_tokens as u64
            + first_read.meta.emitted_tokens as u64
            + context.meta.emitted_tokens as u64
    );
    assert!(report.baseline_source_tokens >= report.emitted_source_tokens);
    assert!(report.estimated_source_tokens_saved > 0);

    let config = Config::discover(root.path(), Some(root.path().join("index.sqlite")))
        .expect("reopen config");
    let reopened = Services::open(config).expect("reopen services");
    assert_eq!(
        reopened.token_savings().await.expect("persisted savings"),
        report
    );

    let mut alternate_config =
        Config::discover(root.path(), Some(root.path().join("index.sqlite")))
            .expect("alternate tokenizer config");
    alternate_config.tokenizer = Tokenizer::O200kBase;
    let alternate = Services::open(alternate_config).expect("alternate tokenizer services");
    alternate
        .outline(outline_limit_request(Some(10), Some(100)))
        .await
        .expect("outline against stale tokenizer index");
    assert_eq!(
        alternate
            .token_savings()
            .await
            .expect("alternate tokenizer savings")
            .tracked_requests,
        0
    );
}

#[tokio::test]
async fn multilingual_structural_indexing_returns_new_language_symbol_bodies() {
    let root = tempfile::tempdir().expect("root");
    for (path, source) in [
        (
            "target.c",
            "int c_target(int value) {\n    return value + 11;\n}\n",
        ),
        (
            "target.cpp",
            "class CppTarget {\npublic:\n    int cpp_target() { return 22; }\n};\n",
        ),
        (
            "JavaTarget.java",
            "class JavaTarget {\n    int javaTarget() {\n        return 33;\n    }\n}\n",
        ),
        (
            "target.php",
            "<?php\nfunction phpTarget() {\n    return 44;\n}\n",
        ),
        (
            "target.rb",
            "def ruby_target\n  55\nend\n",
        ),
    ] {
        std::fs::write(root.path().join(path), source).expect("source");
    }
    let config =
        Config::discover(root.path(), Some(root.path().join("index.sqlite"))).expect("config");
    let services = Services::open(config).expect("services");
    services.index(false).await.expect("index");

    for (path, symbol, marker) in [
        ("target.c", "c_target", "return value + 11"),
        ("target.cpp", "cpp_target", "return 22"),
        ("JavaTarget.java", "javaTarget", "return 33"),
        ("target.php", "phpTarget", "return 44"),
        ("target.rb", "ruby_target", "55"),
    ] {
        let outline = services
            .outline(OutlineRequest {
                paths: vec![path.into()],
                symbol_name: Some(symbol.into()),
                symbol_kind: None,
                max_results: Some(10),
                max_tokens: Some(200),
            })
            .await
            .expect("outline");
        assert!(
            outline.files[0]
                .symbols
                .iter()
                .any(|item| item.name == symbol && item.end_line >= item.start_line),
            "missing {symbol} in {path}: {:?}",
            outline.files[0].symbols
        );

        let context = services
            .context(ContextRequest {
                task: format!("Fix {symbol}"),
                token_budget: 300,
                focus_paths: Vec::new(),
                focus_symbols: Vec::new(),
                exclude_paths: Vec::new(),
                known_hashes: Vec::new(),
                prior_repository_generation: None,
            base_revision: None,
            changed_paths: Vec::new(),
            })
            .await
            .expect("context");
        assert!(
            context
                .fragments
                .iter()
                .any(|fragment| fragment.path == path && fragment.content.contains(marker)),
            "missing body for {symbol}: {:?}",
            context.fragments
        );
    }
}

#[tokio::test]
async fn import_expansion_is_exact_safe_and_requires_corroborated_symbols() {
    let root = tempfile::tempdir().expect("root");
    std::fs::create_dir(root.path().join("src")).expect("src");
    std::fs::write(
        root.path().join("src/seed.js"),
        "import { OwnerAlpha } from './target.js';\nexport function useOwner() { return new OwnerAlpha(); }\n",
    )
    .expect("seed");
    std::fs::write(
        root.path().join("src/target.js"),
        format!(
            "export class OwnerAlpha {{\n  run(input) {{\n    let total = input;\n{}    return total;\n  }}\n}}\n",
            (1..=44)
                .map(|index| format!("    total += input + {index};\n"))
                .collect::<String>()
        ),
    )
    .expect("target");
    let config =
        Config::discover(root.path(), Some(root.path().join("index.sqlite"))).expect("config");
    let services = Services::open(config).expect("services");
    services.index(false).await.expect("index");

    let exact = services
        .context_evaluation(ContextRequest {
            task: "Fix OwnerAlpha".into(),
            token_budget: 400,
            focus_paths: Vec::new(),
            focus_symbols: Vec::new(),
            exclude_paths: Vec::new(),
            known_hashes: Vec::new(),
            prior_repository_generation: None,
        base_revision: None,
        changed_paths: Vec::new(),
        })
        .await
        .expect("exact evaluation");
    assert!(
        exact
            .generated_candidates
            .iter()
            .all(|candidate| candidate.representation != "import_symbol")
    );

    let multi = services
        .context_evaluation(ContextRequest {
            task: "Fix OwnerAlpha and OtherSignal".into(),
            token_budget: 400,
            focus_paths: Vec::new(),
            focus_symbols: Vec::new(),
            exclude_paths: Vec::new(),
            known_hashes: Vec::new(),
            prior_repository_generation: None,
        base_revision: None,
        changed_paths: Vec::new(),
        })
        .await
        .expect("multi-concept evaluation");
    assert!(
        multi.generated_candidates.iter().any(|candidate| {
            candidate.path == "src/target.js" && candidate.representation == "import_symbol"
        }),
        "candidates: {:?}",
        multi.generated_candidates
    );
    let import_symbol = multi
        .generated_candidates
        .iter()
        .find(|candidate| {
            candidate.path == "src/target.js" && candidate.representation == "import_symbol"
        })
        .expect("import symbol candidate");
    assert_eq!(import_symbol.end_line, 50);
    assert!(
        import_symbol.token_count > 256,
        "import symbol fixture must cover the old cap: {import_symbol:?}"
    );
    assert!(
        multi
            .generated_candidates
            .iter()
            .all(|candidate| candidate.representation != "import_neighbor")
    );
}

#[tokio::test]
async fn context_signal_evaluation_keeps_graph_arms_additive_and_isolated() {
    let root = tempfile::tempdir().expect("root");
    std::fs::create_dir(root.path().join("src")).expect("src");
    std::fs::write(
        root.path().join("src/seed.js"),
        "import { OwnerAlpha } from './target.js';\nexport function useOwner() { return new OwnerAlpha(); }\n",
    )
    .expect("seed");
    std::fs::write(
        root.path().join("src/target.js"),
        "export class OwnerAlpha { run(input) { return input + OtherSignal; } }\n",
    )
    .expect("target");
    let config =
        Config::discover(root.path(), Some(root.path().join("index.sqlite"))).expect("config");
    let services = Services::open(config).expect("services");
    services.index(false).await.expect("index");
    let request = ContextRequest {
        task: "Fix OwnerAlpha and OtherSignal".into(),
        token_budget: 400,
        focus_paths: Vec::new(),
        focus_symbols: Vec::new(),
        exclude_paths: Vec::new(),
        known_hashes: Vec::new(),
        prior_repository_generation: None,
    base_revision: None,
    changed_paths: Vec::new(),
    };

    let baseline = services
        .context_signal_evaluation(request.clone(), ContextSignalPolicy::LexicalSyntax)
        .await
        .expect("baseline");
    let imports = services
        .context_signal_evaluation(request.clone(), ContextSignalPolicy::ImportNeighbor)
        .await
        .expect("imports");
    let reverse = services
        .context_signal_evaluation(request.clone(), ContextSignalPolicy::ReverseDependency)
        .await
        .expect("reverse dependency");
    let callers = services
        .context_signal_evaluation(request, ContextSignalPolicy::HighConfidenceCaller)
        .await
        .expect("callers");

    let candidate_keys = |evaluation: &leantoken::ContextEvaluation| {
        evaluation
            .generated_candidates
            .iter()
            .map(|candidate| {
                (
                    candidate.path.clone(),
                    candidate.start_line,
                    candidate.end_line,
                    candidate.representation.clone(),
                )
            })
            .collect::<std::collections::BTreeSet<_>>()
    };
    let baseline_keys = candidate_keys(&baseline);
    for evaluation in [&imports, &reverse, &callers] {
        assert!(baseline_keys.is_subset(&candidate_keys(evaluation)));
    }
    assert!(baseline.generated_candidates.iter().all(|candidate| {
        candidate.representation != "import_symbol"
            && !candidate.match_kinds.iter().any(|kind| kind == "reference")
            && !candidate
                .match_kinds
                .iter()
                .any(|kind| kind == "reverse-import")
    }));
    assert!(imports.generated_candidates.iter().any(|candidate| {
        candidate.representation == "import_symbol"
            && candidate.match_kinds.iter().any(|kind| kind == "import")
    }));
    assert!(callers
        .generated_candidates
        .iter()
        .any(|candidate| candidate.match_kinds.iter().any(|kind| kind == "reference")));
    assert!(reverse.generated_candidates.iter().any(|candidate| {
        candidate.path == "src/seed.js"
            && candidate
                .match_kinds
                .iter()
                .any(|kind| kind == "reverse-import")
    }));
    assert!(imports
        .generated_candidates
        .iter()
        .all(|candidate| !candidate.match_kinds.iter().any(|kind| kind == "reference")));
    assert!(callers.generated_candidates.iter().all(|candidate| {
        candidate.representation != "import_symbol"
            && !candidate
                .match_kinds
                .iter()
                .any(|kind| kind == "reverse-import")
    }));
}

#[tokio::test]
async fn file_operations_page_without_duplicates() {
    let root = tempfile::tempdir().expect("root");
    for name in ["alpha.rs", "bravo.rs", "charlie.rs", "delta.rs", "echo.rs"] {
        std::fs::write(root.path().join(name), format!("fn {}() {{}}\n", &name[..name.len() - 3]))
            .expect("source");
    }
    let config =
        Config::discover(root.path(), Some(root.path().join("index.sqlite"))).expect("config");
    let services = Services::open(config).expect("services");
    services.index(false).await.expect("index");

    for operation in [
        FileOperation::Tree,
        FileOperation::Glob,
        FileOperation::Find,
    ] {
        let mut cursor = None;
        let mut paths = Vec::new();
        loop {
            let response = services
                .files(FilesRequest {
                    operation: operation.clone(),
                    path: None,
                    query: matches!(operation, FileOperation::Find).then(|| "rs".into()),
                    pattern: matches!(operation, FileOperation::Glob).then(|| "*.rs".into()),
                    max_results: Some(2),
                    cursor,
                    depth: Some(1),
                })
                .await
                .expect("file page");
            paths.extend(response.entries.into_iter().map(|entry| entry.path));
            cursor = response.meta.next_cursor;
            if cursor.is_none() {
                break;
            }
        }
        let unique = paths.iter().collect::<std::collections::HashSet<_>>();
        assert_eq!(paths.len(), 5, "{operation:?}");
        assert_eq!(unique.len(), paths.len(), "{operation:?}");
    }

    let tree = services
        .files(FilesRequest {
            operation: FileOperation::Tree,
            path: None,
            query: None,
            pattern: None,
            max_results: Some(2),
            cursor: None,
            depth: Some(1),
        })
        .await
        .expect("tree page");
    let error = services
        .files(FilesRequest {
            operation: FileOperation::Glob,
            path: None,
            query: None,
            pattern: Some("*.rs".into()),
            max_results: Some(2),
            cursor: tree.meta.next_cursor,
            depth: None,
        })
        .await
        .expect_err("cursor from another operation");
    assert!(matches!(error, Error::StaleCursor));
}

#[tokio::test]
async fn file_tree_projection_respects_root_depth_and_removes_empty_directories() {
    let root = tempfile::tempdir().expect("root");
    std::fs::create_dir_all(root.path().join("src/deep")).expect("directories");
    std::fs::write(root.path().join("src/top.rs"), "fn top() {}\n").expect("top source");
    std::fs::write(root.path().join("src/deep/lib.rs"), "fn deep() {}\n")
        .expect("deep source");
    let services = Services::open(
        Config::discover(root.path(), Some(root.path().join("index.sqlite"))).expect("config"),
    )
    .expect("services");
    services.index(false).await.expect("index");

    let tree = services
        .files(FilesRequest {
            operation: FileOperation::Tree,
            path: Some("src".into()),
            query: None,
            pattern: None,
            max_results: Some(20),
            cursor: None,
            depth: Some(1),
        })
        .await
        .expect("tree");
    assert_eq!(
        tree.entries
            .iter()
            .map(|entry| entry.path.as_str())
            .collect::<Vec<_>>(),
        vec!["src", "src/deep", "src/top.rs"]
    );

    std::fs::remove_file(root.path().join("src/deep/lib.rs")).expect("delete deep source");
    services
        .index_paths(vec!["src/deep/lib.rs".into()])
        .await
        .expect("reconcile deletion");
    let after = services
        .files(FilesRequest {
            operation: FileOperation::Tree,
            path: Some("src".into()),
            query: None,
            pattern: None,
            max_results: Some(20),
            cursor: None,
            depth: Some(2),
        })
        .await
        .expect("tree after deletion");
    assert!(after.entries.iter().all(|entry| entry.path != "src/deep"));
}

#[tokio::test]
async fn file_tree_normalizes_equivalent_roots_before_query_and_pagination() {
    let root = tempfile::tempdir().expect("root");
    std::fs::create_dir_all(root.path().join("src/rust")).expect("directories");
    std::fs::write(root.path().join("README.md"), "fixture\n").expect("readme");
    std::fs::write(root.path().join("src/lib.rs"), "fn lib() {}\n").expect("lib source");
    std::fs::write(root.path().join("src/rust/a.rs"), "fn a() {}\n").expect("a source");
    std::fs::write(root.path().join("src/rust/b.rs"), "fn b() {}\n").expect("b source");
    let services = Services::open(
        Config::discover(root.path(), Some(root.path().join("index.sqlite"))).expect("config"),
    )
    .expect("services");
    services.index(false).await.expect("index");

    for aliases in [
        vec![None, Some(""), Some("."), Some("./")],
        vec![Some("src"), Some("./src"), Some("src/")],
        vec![
            Some("src/rust"),
            Some("./src//rust"),
            Some("src/rust/"),
        ],
    ] {
        let expected = tree_pages(&services, aliases[0]).await;
        assert!(expected.len() > 1, "fixture must exercise pagination");
        for alias in aliases.into_iter().skip(1) {
            assert_eq!(tree_pages(&services, alias).await, expected, "alias {alias:?}");
        }
    }
}

#[tokio::test]
async fn invalid_focus_glob_is_a_typed_error() {
    let (_root, services) = fixture().await;
    let error = services
        .search(SearchRequest {
            query: "greet".into(),
            mode: SearchMode::Auto,
            include_paths: Vec::new(),
            exclude_paths: Vec::new(),
            focus_paths: vec!["[".into()],
            max_results: None,
            max_tokens: None,
            context_lines: None,
            case_sensitive: false,
            cursor: None,
        })
        .await
        .expect_err("invalid glob must fail");
    assert!(error.to_string().contains("glob"));
}

#[tokio::test]
async fn file_tree_rejects_unsafe_roots() {
    let (_root, services) = fixture().await;
    for path in ["/src", "../src", "src/../rust", "src\0rust"] {
        services
            .files(FilesRequest {
                operation: FileOperation::Tree,
                path: Some(path.into()),
                query: None,
                pattern: None,
                max_results: None,
                cursor: None,
                depth: None,
            })
            .await
            .expect_err("unsafe tree root must fail");
    }
}

#[tokio::test]
async fn search_range_covers_the_returned_context_lines() {
    let (_root, services) = fixture().await;
    let response = services
        .search(SearchRequest {
            query: "agent".into(),
            mode: SearchMode::Text,
            include_paths: Vec::new(),
            exclude_paths: Vec::new(),
            focus_paths: Vec::new(),
            max_results: Some(1),
            max_tokens: Some(100),
            context_lines: Some(1),
            case_sensitive: false,
            cursor: None,
        })
        .await
        .expect("search");

    let hit = response.hits.first().expect("text hit");
    assert_eq!((hit.start_line, hit.end_line), (5, 7));
    assert_eq!(hit.excerpt.lines().count(), 3);
    assert_eq!(hit.enclosing_symbol.as_deref(), Some("caller"));
}

#[tokio::test]
async fn text_search_windows_keep_case_insensitive_matches_across_a_chunk() {
    let mut lines = (1..=60)
        .map(|line| format!("ordinary line {line}"))
        .collect::<Vec<_>>();
    let cases = [
        (30usize, "MiddleNeedle"),
        (59usize, "LateNeedle"),
        (2usize, "EarlyNeedle"),
    ];
    for (line, needle) in cases {
        lines[line - 1] = format!("{needle} is anchored here");
    }
    let source = format!("{}\n", lines.join("\n"));
    let (_root, services) = indexed_source("positions.txt", source.as_bytes()).await;

    for (match_line, needle) in cases {
        let response = services
            .search(SearchRequest {
                query: needle.to_ascii_lowercase(),
                mode: SearchMode::Text,
                include_paths: vec!["positions.txt".into()],
                exclude_paths: Vec::new(),
                focus_paths: Vec::new(),
                max_results: Some(1),
                max_tokens: Some(1_000),
                context_lines: Some(20),
                case_sensitive: false,
                cursor: None,
            })
            .await
            .expect("case-insensitive text search");

        let hit = response.hits.first().expect("text hit");
        assert!(
            hit.excerpt.contains(needle),
            "excerpt for line {match_line} omitted {needle}: {:?}",
            hit.excerpt
        );
        assert_eq!(hit.match_kind, "text");
        assert!(hit.start_line <= match_line && hit.end_line >= match_line);
        assert_eq!(
            hit.end_line - hit.start_line + 1,
            hit.excerpt.lines().count()
        );
        assert_eq!(hit.excerpt.lines().count(), 20);
    }
}

#[tokio::test]
async fn maximum_text_context_keeps_the_original_read_bounded_range_match() {
    let mut lines = (1..=50)
        .map(|line| format!("// legacy source line {line}"))
        .collect::<Vec<_>>();
    lines[29] = "fn read_bounded_range() {}".into();
    let source = format!("{}\n", lines.join("\n"));
    let (_root, services) = indexed_source("legacy.rs", source.as_bytes()).await;

    let response = services
        .search(SearchRequest {
            query: "read_bounded_range".into(),
            mode: SearchMode::Text,
            include_paths: vec!["legacy.rs".into()],
            exclude_paths: Vec::new(),
            focus_paths: Vec::new(),
            max_results: Some(1),
            max_tokens: Some(1_000),
            context_lines: Some(20),
            case_sensitive: true,
            cursor: None,
        })
        .await
        .expect("legacy reproduction search");

    let hit = response.hits.first().expect("legacy text hit");
    assert!(hit.excerpt.contains("read_bounded_range"));
    assert!(hit.start_line <= 30 && hit.end_line >= 30);
}

#[tokio::test]
async fn regex_search_keeps_a_multiline_match_that_exceeds_the_line_cap() {
    let mut lines = (1..=5)
        .map(|line| format!("prefix {line}"))
        .collect::<Vec<_>>();
    lines.push("MATCH_BEGIN".into());
    lines.extend((1..=24).map(|line| format!("matched body {line}")));
    lines.push("MATCH_END".into());
    lines.extend((1..=5).map(|line| format!("suffix {line}")));
    let source = format!("{}\n", lines.join("\n"));
    let (_root, services) = indexed_source("multiline.txt", source.as_bytes()).await;

    let response = services
        .search(SearchRequest {
            query: "(?s)MATCH_BEGIN.*?MATCH_END".into(),
            mode: SearchMode::Regex,
            include_paths: vec!["multiline.txt".into()],
            exclude_paths: Vec::new(),
            focus_paths: Vec::new(),
            max_results: Some(1),
            max_tokens: Some(5_000),
            context_lines: Some(20),
            case_sensitive: true,
            cursor: None,
        })
        .await
        .expect("multiline regex search");

    let hit = response.hits.first().expect("regex hit");
    assert!(hit.excerpt.contains("MATCH_BEGIN"));
    assert!(hit.excerpt.contains("MATCH_END"));
    assert_eq!((hit.start_line, hit.end_line), (6, 31));
    assert_eq!(
        hit.end_line - hit.start_line + 1,
        hit.excerpt.lines().count()
    );
    assert_eq!(hit.excerpt.lines().count(), 26);
}

#[tokio::test]
async fn symbol_search_caps_a_long_definition_without_losing_its_declaration() {
    let mut lines = (1..=20)
        .map(|line| format!("const PREFIX_{line}: usize = {line};"))
        .collect::<Vec<_>>();
    let declaration_line = lines.len() + 1;
    lines.push("fn long_target() -> usize {".into());
    lines.extend((1..=40).map(|line| format!("    let value_{line} = {line};")));
    lines.push("    40".into());
    lines.push("}".into());
    let source = format!("{}\n", lines.join("\n"));
    let (_root, services) = indexed_source("long_symbol.rs", source.as_bytes()).await;

    let response = services
        .search(SearchRequest {
            query: "long_target".into(),
            mode: SearchMode::Symbol,
            include_paths: vec!["long_symbol.rs".into()],
            exclude_paths: Vec::new(),
            focus_paths: Vec::new(),
            max_results: Some(1),
            max_tokens: Some(2_000),
            context_lines: Some(20),
            case_sensitive: true,
            cursor: None,
        })
        .await
        .expect("long symbol search");

    let hit = response.hits.first().expect("symbol hit");
    assert!(hit.excerpt.contains("fn long_target()"));
    assert!(hit.start_line <= declaration_line && hit.end_line >= declaration_line);
    assert_eq!(hit.excerpt.lines().count(), 30);
    assert_eq!(hit.end_line - hit.start_line + 1, 30);
}

#[tokio::test]
async fn reference_search_window_keeps_the_required_reference_span() {
    let mut lines = vec!["fn target() {}".to_string(), String::new(), "fn caller() {".into()];
    lines.extend((1..=25).map(|line| format!("    let value_{line} = {line};")));
    let reference_line = lines.len() + 1;
    lines.push("    target();".into());
    lines.push("}".into());
    let source = format!("{}\n", lines.join("\n"));
    let (_root, services) = indexed_source("reference.rs", source.as_bytes()).await;

    let response = services
        .search(SearchRequest {
            query: "target".into(),
            mode: SearchMode::Reference,
            include_paths: vec!["reference.rs".into()],
            exclude_paths: Vec::new(),
            focus_paths: Vec::new(),
            max_results: Some(1),
            max_tokens: Some(1_000),
            context_lines: Some(20),
            case_sensitive: true,
            cursor: None,
        })
        .await
        .expect("reference search");

    let hit = response.hits.first().expect("reference hit");
    assert!(hit.excerpt.contains("target();"));
    assert!(hit.start_line <= reference_line && hit.end_line >= reference_line);
    assert_eq!(
        hit.end_line - hit.start_line + 1,
        hit.excerpt.lines().count()
    );
    assert_eq!(hit.excerpt.lines().count(), 12);
}

#[tokio::test]
async fn text_search_reports_enclosing_symbols_across_languages() {
    let root = tempfile::tempdir().expect("temporary repository");
    std::fs::write(
        root.path().join("owner.rs"),
        "fn rust_owner() {\n    let known_hashes: Vec<String> = Vec::new();\n}\n",
    )
    .expect("Rust source");
    std::fs::write(
        root.path().join("owner.py"),
        "def python_owner():\n    known_hashes = []\n    return known_hashes\n",
    )
    .expect("Python source");
    std::fs::write(
        root.path().join("owner.js"),
        "function javascriptOwner() {\n  const known_hashes = [];\n  return known_hashes;\n}\n",
    )
    .expect("JavaScript source");
    let services = Services::open(
        Config::discover(root.path(), Some(root.path().join("index.sqlite"))).expect("config"),
    )
    .expect("services");
    services.index(false).await.expect("index");

    let response = services
        .search(SearchRequest {
            query: "known_hashes".into(),
            mode: SearchMode::Text,
            include_paths: Vec::new(),
            exclude_paths: Vec::new(),
            focus_paths: Vec::new(),
            max_results: Some(10),
            max_tokens: Some(1_000),
            context_lines: Some(1),
            case_sensitive: true,
            cursor: None,
        })
        .await
        .expect("search");
    let owners = response
        .hits
        .into_iter()
        .map(|hit| (hit.path, hit.enclosing_symbol))
        .collect::<std::collections::HashMap<_, _>>();

    assert_eq!(
        owners.get("owner.rs").and_then(Option::as_deref),
        Some("rust_owner")
    );
    assert_eq!(
        owners.get("owner.py").and_then(Option::as_deref),
        Some("python_owner")
    );
    assert_eq!(
        owners.get("owner.js").and_then(Option::as_deref),
        Some("javascriptOwner")
    );
}

#[tokio::test]
async fn text_search_preserves_multiline_matches_without_a_single_matching_line() {
    let root = tempfile::tempdir().expect("temporary repository");
    std::fs::write(
        root.path().join("owner.rs"),
        "fn multiline_owner() {\n    first_line();\n    second_line();\n}\n",
    )
    .expect("Rust source");
    let services = Services::open(
        Config::discover(root.path(), Some(root.path().join("index.sqlite"))).expect("config"),
    )
    .expect("services");
    services.index(false).await.expect("index");

    let response = services
        .search(SearchRequest {
            query: "first_line();\n    second_line();".into(),
            mode: SearchMode::Text,
            include_paths: Vec::new(),
            exclude_paths: Vec::new(),
            focus_paths: Vec::new(),
            max_results: Some(10),
            max_tokens: Some(1_000),
            context_lines: Some(1),
            case_sensitive: true,
            cursor: None,
        })
        .await
        .expect("search");

    let hit = response.hits.first().expect("multiline text hit");
    assert_eq!(hit.path, "owner.rs");
    assert!(hit.excerpt.contains("first_line();\n    second_line();"));
    assert_eq!(hit.enclosing_symbol.as_deref(), Some("multiline_owner"));
}

#[tokio::test]
async fn read_reports_live_content_that_differs_from_the_index() {
    let (root, services) = fixture().await;
    let first = services
        .read(ReadRequest {
            path: "src/lib.rs".into(),
            start_line: Some(1),
            end_line: Some(1),
            symbol: None,
            max_tokens: Some(100),
            expected_hash: None,
        })
        .await
        .expect("indexed read");

    std::fs::write(
        root.path().join("src/lib.rs"),
        "pub fn changed() -> bool { true }\n",
    )
    .expect("change live file");

    let changed = services
        .read(ReadRequest {
            path: "src/lib.rs".into(),
            start_line: Some(1),
            end_line: Some(1),
            symbol: None,
            max_tokens: Some(100),
            expected_hash: Some(first.content_hash.clone()),
        })
        .await
        .expect("live read");

    assert_eq!(changed.status, ReadStatus::Content);
    assert!(changed.index_stale);
    assert_ne!(changed.content_hash, first.content_hash);
    assert_eq!(
        changed.content.as_deref(),
        Some("pub fn changed() -> bool { true }\n")
    );
}

#[tokio::test]
async fn exact_and_open_reads_preserve_coordinates_hashes_and_live_content() {
    let source = b"one\ntwo\nthree\nfour\nfive\n";
    let (root, services) = indexed_source("lines.txt", source).await;

    let exact = services
        .read(ReadRequest {
            path: "lines.txt".into(),
            start_line: Some(2),
            end_line: Some(3),
            symbol: None,
            max_tokens: Some(100),
            expected_hash: None,
        })
        .await
        .expect("exact range");
    assert_eq!((exact.start_line, exact.end_line), (2, 3));
    assert_eq!(exact.content.as_deref(), Some("two\nthree\n"));

    let unchanged = services
        .read(ReadRequest {
            path: "lines.txt".into(),
            start_line: Some(2),
            end_line: Some(3),
            symbol: None,
            max_tokens: Some(100),
            expected_hash: Some(exact.content_hash.clone()),
        })
        .await
        .expect("conditional exact range");
    assert_eq!(unchanged.status, ReadStatus::NotModified);
    assert!(unchanged.content.is_none());
    assert_eq!(unchanged.meta.emitted_tokens, 0);

    let from_second = services
        .read(ReadRequest {
            path: "lines.txt".into(),
            start_line: Some(2),
            end_line: None,
            symbol: None,
            max_tokens: Some(100),
            expected_hash: None,
        })
        .await
        .expect("open-ended range");
    assert_eq!((from_second.start_line, from_second.end_line), (2, 5));
    assert_eq!(from_second.content.as_deref(), Some("two\nthree\nfour\nfive\n"));

    let through_third = services
        .read(ReadRequest {
            path: "lines.txt".into(),
            start_line: None,
            end_line: Some(3),
            symbol: None,
            max_tokens: Some(100),
            expected_hash: None,
        })
        .await
        .expect("open-start range");
    assert_eq!((through_third.start_line, through_third.end_line), (1, 3));
    assert_eq!(through_third.content.as_deref(), Some("one\ntwo\nthree\n"));

    let whole = services
        .read(ReadRequest {
            path: "lines.txt".into(),
            start_line: None,
            end_line: None,
            symbol: None,
            max_tokens: Some(100),
            expected_hash: None,
        })
        .await
        .expect("whole file");
    let exact_whole = services
        .read(ReadRequest {
            path: "lines.txt".into(),
            start_line: Some(1),
            end_line: Some(5),
            symbol: None,
            max_tokens: Some(100),
            expected_hash: None,
        })
        .await
        .expect("exact whole file");
    assert_eq!(whole.content.as_deref(), Some("one\ntwo\nthree\nfour\nfive\n"));
    assert_eq!(exact_whole.content, whole.content);
    assert_eq!(exact_whole.content_hash, whole.content_hash);

    let through_eof = services
        .read(ReadRequest {
            path: "lines.txt".into(),
            start_line: Some(4),
            end_line: Some(99),
            symbol: None,
            max_tokens: Some(100),
            expected_hash: None,
        })
        .await
        .expect("range through EOF");
    assert_eq!((through_eof.start_line, through_eof.end_line), (4, 5));
    assert_eq!(through_eof.content.as_deref(), Some("four\nfive\n"));

    std::fs::write(
        root.path().join("lines.txt"),
        b"one\nchanged\nthree\nfour\nfive\n",
    )
    .expect("edit source");
    let changed = services
        .read(ReadRequest {
            path: "lines.txt".into(),
            start_line: Some(2),
            end_line: Some(3),
            symbol: None,
            max_tokens: Some(100),
            expected_hash: Some(exact.content_hash.clone()),
        })
        .await
        .expect("changed exact range");
    assert_eq!(changed.status, ReadStatus::Content);
    assert!(changed.index_stale);
    assert_ne!(changed.content_hash, exact.content_hash);
    assert_eq!(changed.content.as_deref(), Some("changed\nthree\n"));
}

#[tokio::test]
async fn symbol_read_after_first_line_returns_the_complete_definition() {
    let source = b"const PREFIX: usize = 1;\n\nfn target() -> usize {\n    let value = PREFIX + 1;\n    value\n}\n\nfn after() {}\n";
    let (_root, services) = indexed_source("symbol.rs", source).await;

    let response = services
        .read(ReadRequest {
            path: "symbol.rs".into(),
            start_line: None,
            end_line: None,
            symbol: Some("target".into()),
            max_tokens: Some(100),
            expected_hash: None,
        })
        .await
        .expect("symbol range");

    assert_eq!((response.start_line, response.end_line), (3, 6));
    assert_eq!(
        response.content.as_deref(),
        Some("fn target() -> usize {\n    let value = PREFIX + 1;\n    value\n}\n")
    );
}

#[tokio::test]
async fn open_ended_read_bounds_live_suffix_before_returning_content() {
    let source = (0..50_000)
        .map(|line| format!("fn generated_{line}() {{}}\n"))
        .collect::<String>();
    let (_root, services) = indexed_source("large.rs", source.as_bytes()).await;

    let response = services
        .read(ReadRequest {
            path: "large.rs".into(),
            start_line: Some(25_000),
            end_line: None,
            symbol: None,
            max_tokens: Some(12),
            expected_hash: None,
        })
        .await
        .expect("bounded open-ended read");

    let content = response.content.as_deref().expect("content");
    assert!(content.len() <= 12 * 32);
    assert!(content.contains("generated_25000"));
    assert!(response.start_line >= 25_000);
    assert!(response.meta.emitted_tokens <= 12);
}

#[tokio::test]
async fn live_read_rejects_malformed_utf8_at_eof() {
    let (root, services) = indexed_source("malformed.rs", b"fn valid() {}\n").await;
    std::fs::write(root.path().join("malformed.rs"), b"a\xC3").expect("malformed edit");

    let error = services
        .read(ReadRequest {
            path: "malformed.rs".into(),
            start_line: Some(1),
            end_line: None,
            symbol: None,
            max_tokens: Some(100),
            expected_hash: None,
        })
        .await
        .expect_err("malformed UTF-8 must fail");
    assert!(matches!(
        error,
        Error::InvalidInput {
            field: "path",
            reason: "must identify UTF-8 text"
        }
    ));
}

#[tokio::test]
async fn live_read_rejects_line_after_terminal_newline() {
    let (root, services) = indexed_source("short.rs", b"a\n").await;
    std::fs::write(root.path().join("short.rs"), b"a\n").expect("short edit");

    let error = services
        .read(ReadRequest {
            path: "short.rs".into(),
            start_line: Some(2),
            end_line: None,
            symbol: None,
            max_tokens: Some(100),
            expected_hash: None,
        })
        .await
        .expect_err("line after terminal newline must fail");
    assert!(matches!(
        error,
        Error::InvalidInput {
            field: "line range",
            reason: "must be ordered and within the requested file"
        }
    ));
}

#[tokio::test]
async fn bounded_reads_preserve_crlf_and_missing_final_newline() {
    let source = b"alpha\r\nbeta\r\ngamma";
    let (_root, services) = indexed_source("endings.txt", source).await;

    let exact = services
        .read(ReadRequest {
            path: "endings.txt".into(),
            start_line: Some(2),
            end_line: Some(3),
            symbol: None,
            max_tokens: Some(100),
            expected_hash: None,
        })
        .await
        .expect("exact CRLF range");
    let open = services
        .read(ReadRequest {
            path: "endings.txt".into(),
            start_line: Some(2),
            end_line: None,
            symbol: None,
            max_tokens: Some(100),
            expected_hash: None,
        })
        .await
        .expect("open CRLF range");

    assert_eq!((exact.start_line, exact.end_line), (2, 3));
    assert_eq!(exact.content.as_deref(), Some("beta\r\ngamma"));
    assert_eq!(exact.content, open.content);
    assert_eq!(exact.content_hash, open.content_hash);

    let final_line = services
        .read(ReadRequest {
            path: "endings.txt".into(),
            start_line: Some(3),
            end_line: Some(3),
            symbol: None,
            max_tokens: Some(100),
            expected_hash: None,
        })
        .await
        .expect("final line");
    assert_eq!(final_line.content.as_deref(), Some("gamma"));
}

#[tokio::test]
async fn read_validates_ranges_and_preserves_empty_file_metadata() {
    let (_root, services) = indexed_source("empty.txt", b"").await;

    let empty = services
        .read(ReadRequest {
            path: "empty.txt".into(),
            start_line: None,
            end_line: None,
            symbol: None,
            max_tokens: Some(100),
            expected_hash: None,
        })
        .await
        .expect("empty file");
    assert_eq!((empty.start_line, empty.end_line), (1, 1));
    assert_eq!(empty.content.as_deref(), Some(""));

    for (start_line, end_line) in [(Some(0), Some(1)), (Some(3), Some(2)), (Some(2), Some(2))] {
        let error = services
            .read(ReadRequest {
                path: "empty.txt".into(),
                start_line,
                end_line,
                symbol: None,
                max_tokens: Some(100),
                expected_hash: None,
            })
            .await
            .expect_err("invalid range");
        assert!(matches!(error, Error::InvalidInput { field: "line range", .. }));
    }
}

#[tokio::test]
async fn token_truncated_read_reports_the_returned_line_range() {
    let source = b"header\nalpha beta gamma delta\nsecond retained line\nthird retained line\n";
    let (_root, services) = indexed_source("tokens.txt", source).await;

    let response = services
        .read(ReadRequest {
            path: "tokens.txt".into(),
            start_line: Some(2),
            end_line: Some(4),
            symbol: None,
            max_tokens: Some(3),
            expected_hash: None,
        })
        .await
        .expect("token-truncated range");
    let content = response.content.as_deref().expect("content");
    let returned_lines = content.lines().count().max(usize::from(!content.is_empty()));

    assert!(!content.is_empty());
    assert_eq!(response.start_line, 2);
    assert_eq!(response.end_line, response.start_line + returned_lines - 1);
    assert!(response.end_line <= 4);
    assert!(response.meta.emitted_tokens <= 3);
}

#[tokio::test]
async fn read_rejects_ignored_files() {
    let root = tempfile::tempdir().expect("temporary repository");
    std::fs::create_dir(root.path().join(".git")).expect("git marker");
    std::fs::write(root.path().join(".gitignore"), ".env\n").expect("ignore file");
    std::fs::write(root.path().join(".env"), "SECRET=do-not-return\n").expect("ignored file");
    std::fs::write(root.path().join("lib.rs"), "fn visible() {}\n").expect("indexed file");
    let services = Services::open(
        Config::discover(root.path(), Some(root.path().join("index.sqlite"))).expect("config"),
    )
    .expect("services");
    services.index(false).await.expect("index");

    let error = services
        .read(ReadRequest {
            path: ".env".into(),
            start_line: None,
            end_line: None,
            symbol: None,
            max_tokens: Some(100),
            expected_hash: None,
        })
        .await
        .expect_err("ignored file must not be readable");

    assert!(matches!(error, Error::NotIndexed(path) if path == ".env"));
}

#[tokio::test]
async fn symbol_reads_and_outline_filters_search_beyond_result_caps() {
    let root = tempfile::tempdir().expect("temporary repository");
    let source = (0..130)
        .map(|index| format!("fn symbol_{index:03}() {{}}\n"))
        .collect::<String>();
    std::fs::write(root.path().join("many.rs"), source).expect("source");
    let services = Services::open(
        Config::discover(root.path(), Some(root.path().join("index.sqlite"))).expect("config"),
    )
    .expect("services");
    services.index(false).await.expect("index");

    let read = services
        .read(ReadRequest {
            path: "many.rs".into(),
            start_line: None,
            end_line: None,
            symbol: Some("symbol_129".into()),
            max_tokens: Some(100),
            expected_hash: None,
        })
        .await
        .expect("late symbol read");
    assert_eq!(read.start_line, 130);
    assert!(
        read.content
            .as_deref()
            .is_some_and(|text| text.contains("symbol_129"))
    );

    let outline = services
        .outline(OutlineRequest {
            paths: vec!["many.rs".into()],
            symbol_name: Some("symbol_129".into()),
            symbol_kind: Some("function".into()),
            max_results: Some(1),
            max_tokens: Some(100),
        })
        .await
        .expect("filtered outline");
    assert_eq!(outline.files[0].symbols.len(), 1);
    assert_eq!(outline.files[0].symbols[0].name, "symbol_129");
}

#[tokio::test]
async fn fixture_outlines_deduplicate_methods_and_report_receiver_owners() {
    let root = tempfile::tempdir().expect("temporary repository");
    for (path, source) in [
        (
            "src/rust/math.rs",
            include_str!("../fixtures/sample_repo/src/rust/math.rs"),
        ),
        (
            "src/go/point.go",
            include_str!("../fixtures/sample_repo/src/go/point.go"),
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
    services.index(false).await.expect("index fixtures");

    let outline = services
        .outline(OutlineRequest {
            paths: vec!["src/rust/math.rs".into(), "src/go/point.go".into()],
            symbol_name: None,
            symbol_kind: None,
            max_results: Some(100),
            max_tokens: Some(2_000),
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
                focus_paths: Vec::new(),
                focus_symbols: Vec::new(),
                exclude_paths: Vec::new(),
                known_hashes: Vec::new(),
                prior_repository_generation: None,
            base_revision: None,
            changed_paths: Vec::new(),
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
    for index in 0..24 {
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

    drop(operation);
    indexing.await.expect("join index").expect("complete index");
    let after = services.status().await.expect("status after first index");
    assert!(after.repository_generation > 0);
    assert_eq!(after.index_state, IndexState::Ready);
    assert_eq!(after.freshness, Freshness::Current);
}

fn git_available() -> bool {
    std::process::Command::new("git")
        .arg("--version")
        .output()
        .is_ok()
}

fn init_git_repo(root: &std::path::Path) {
    let run = |args: &[&str]| {
        std::process::Command::new("git")
            .args(args)
            .current_dir(root)
            .output()
            .expect("git command");
    };
    run(&["init"]);
    run(&["config", "user.email", "test@example.com"]);
    run(&["config", "user.name", "Test"]);
    run(&["add", "-A"]);
    run(&["commit", "-m", "init"]);
}

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
            focus_paths: Vec::new(),
            focus_symbols: Vec::new(),
            exclude_paths: Vec::new(),
            known_hashes: Vec::new(),
            prior_repository_generation: None,
        base_revision: None,
        changed_paths: Vec::new(),
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
        focus_paths: Vec::new(),
        focus_symbols: Vec::new(),
        exclude_paths: Vec::new(),
        known_hashes: Vec::new(),
        prior_repository_generation: None,
    base_revision: None,
    changed_paths: Vec::new(),
    };

    let (exact_response, estimate_response) =
        tokio::join!(exact.context(request.clone()), estimate.context(request),);

    assert!(
        exact_response
            .expect("exact context")
            .meta
            .token_count_exact
    );
    assert!(
        !estimate_response
            .expect("estimate context")
            .meta
            .token_count_exact
    );
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
            focus_paths: Vec::new(),
            focus_symbols: Vec::new(),
            exclude_paths: Vec::new(),
            known_hashes: Vec::new(),
            prior_repository_generation: None,
        base_revision: None,
        changed_paths: Vec::new(),
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
            focus_paths: Vec::new(),
            focus_symbols: Vec::new(),
            exclude_paths: Vec::new(),
            known_hashes: Vec::new(),
            prior_repository_generation: None,
        base_revision: None,
        changed_paths: Vec::new(),
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
async fn working_tree_search_reconciles_file_created_after_index() {
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
                cursor: None,
            },
            IndexConsistency::WorkingTree,
            CancellationToken::new(),
        )
        .await
        .expect("working-tree search");

    assert_eq!(response.hits.len(), 1);
    assert_eq!(response.hits[0].path, "new_package.rs");
    assert!(response.meta.repository_generation > initial.repository_generation);
}

#[tokio::test]
async fn committed_search_does_not_reconcile_file_created_after_index() {
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
                cursor: None,
            },
            IndexConsistency::Committed,
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
async fn working_tree_consistency_applies_to_each_retrieval_service() {
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
            IndexConsistency::WorkingTree,
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
            },
            IndexConsistency::WorkingTree,
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
                max_tokens: Some(100),
                expected_hash: None,
            },
            IndexConsistency::WorkingTree,
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
                focus_paths: vec!["context_package.rs".into()],
                focus_symbols: vec!["contextual_package_marker".into()],
                exclude_paths: Vec::new(),
                known_hashes: Vec::new(),
                prior_repository_generation: None,
            base_revision: None,
            changed_paths: Vec::new(),
            },
            IndexConsistency::WorkingTree,
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
            max_tokens: Some(100),
            expected_hash: None,
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
            max_tokens: Some(100),
            expected_hash: None,
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
            max_tokens: Some(100),
            expected_hash: Some(first.content_hash.clone()),
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

#[tokio::test]
async fn diff_scoped_context_with_explicit_changed_paths_reports_receipt() {
    let (_root, services) = fixture().await;

    let response = services
        .context(ContextRequest {
            task: "change greet caller".into(),
            token_budget: 200,
            focus_paths: Vec::new(),
            focus_symbols: Vec::new(),
            exclude_paths: Vec::new(),
            known_hashes: Vec::new(),
            prior_repository_generation: None,
            base_revision: None,
            changed_paths: vec!["src/lib.rs".into()],
        })
        .await
        .expect("diff-scoped context");

    let scope = response
        .diff_scope
        .as_ref()
        .expect("diff scope receipt present");
    assert_eq!(scope.changed_paths, vec!["src/lib.rs".to_owned()]);
    assert!(scope.base_revision.is_none());
    assert!(scope.head_revision.is_none());
    assert_eq!(scope.indexed_changed_paths, 1);
}

#[tokio::test]
async fn diff_scoped_context_preserves_task_only_behavior_without_scope() {
    let (_root, services) = fixture().await;

    let response = services
        .context(ContextRequest {
            task: "change greet caller".into(),
            token_budget: 200,
            focus_paths: Vec::new(),
            focus_symbols: Vec::new(),
            exclude_paths: Vec::new(),
            known_hashes: Vec::new(),
            prior_repository_generation: None,
            base_revision: None,
            changed_paths: Vec::new(),
        })
        .await
        .expect("task-only context");

    assert!(
        response.diff_scope.is_none(),
        "no diff scope must not produce a receipt"
    );
    assert!(!response.fragments.is_empty());
}

#[tokio::test]
async fn diff_scoped_context_rejects_path_outside_repository() {
    let (_root, services) = fixture().await;

    let error = services
        .context(ContextRequest {
            task: "change greet caller".into(),
            token_budget: 200,
            focus_paths: Vec::new(),
            focus_symbols: Vec::new(),
            exclude_paths: Vec::new(),
            known_hashes: Vec::new(),
            prior_repository_generation: None,
            base_revision: None,
            changed_paths: vec!["../escape.rs".into()],
        })
        .await
        .expect_err("path traversal rejected");

    assert!(
        matches!(error, Error::PathOutsideRoot { .. }),
        "got {error:?}"
    );
}

#[tokio::test]
async fn diff_scoped_context_rejects_excessive_changed_path_count() {
    let (_root, services) = fixture().await;

    let too_many = (0..600).map(|i| format!("src/file{i}.rs")).collect::<Vec<_>>();
    let error = services
        .context(ContextRequest {
            task: "change greet caller".into(),
            token_budget: 200,
            focus_paths: Vec::new(),
            focus_symbols: Vec::new(),
            exclude_paths: Vec::new(),
            known_hashes: Vec::new(),
            prior_repository_generation: None,
            base_revision: None,
            changed_paths: too_many,
        })
        .await
        .expect_err("too many changed paths rejected");

    assert!(matches!(error, Error::LimitExceeded), "got {error:?}");
}

#[tokio::test]
async fn diff_scoped_context_counts_zero_for_nonexistent_changed_path() {
    let (_root, services) = fixture().await;

    let response = services
        .context(ContextRequest {
            task: "change greet caller".into(),
            token_budget: 200,
            focus_paths: Vec::new(),
            focus_symbols: Vec::new(),
            exclude_paths: Vec::new(),
            known_hashes: Vec::new(),
            prior_repository_generation: None,
            base_revision: None,
            changed_paths: vec!["src/nonexistent.rs".into()],
        })
        .await
        .expect("context with unindexed changed path");

    let scope = response
        .diff_scope
        .as_ref()
        .expect("diff scope receipt present");
    assert_eq!(scope.indexed_changed_paths, 0);
}
