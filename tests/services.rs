use leantoken::{
    Config, ContextRequest, Error, FileOperation, FilesRequest, OutlineRequest, ReadRequest,
    ReadStatus, SearchMode, SearchRequest, services::Services,
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
async fn file_tree_rejects_absolute_roots() {
    let (_root, services) = fixture().await;
    let error = services
        .files(FilesRequest {
            operation: FileOperation::Tree,
            path: Some("/src".into()),
            query: None,
            pattern: None,
            max_results: None,
            cursor: None,
            depth: None,
        })
        .await
        .expect_err("absolute tree root must fail");
    assert!(error.to_string().contains("escapes repository root"));
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
