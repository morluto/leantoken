use super::*;

#[tokio::test]
async fn canonical_symbol_identity_round_trips_without_silent_ambiguity() {
    require_git();

    let root = tempfile::tempdir().expect("root");
    std::fs::write(
        root.path().join("service.rs"),
        "pub struct ServiceCallOptions;\n\
         \n\
         pub struct Services;\n\
         impl Services {\n\
             pub fn wait_for_initial_index_cancellable() -> &'static str { \"services\" }\n\
             pub fn go() -> &'static str { \"short leaf\" }\n\
         }\n\
         \n\
         pub struct Other;\n\
         impl Other {\n\
             pub fn wait_for_initial_index_cancellable() -> &'static str { \"other\" }\n\
         }\n",
    )
    .expect("service source");
    init_git_repo(root.path());
    let services = Services::open(
        Config::discover(root.path(), Some(root.path().join("index.sqlite"))).expect("config"),
    )
    .expect("services");
    services
        .refresh(leantoken::IndexingMode::Reconcile)
        .await
        .expect("index");

    let qualified = "Services.wait_for_initial_index_cancellable";
    let outline = services
        .outline(OutlineRequest {
            paths: vec!["service.rs".into()],
            symbol_name: Some(qualified.into()),
            symbol_kind: Some("method".into()),
            max_results: Some(10),
            max_tokens: Some(1_000),
            receipt_id: None,
            cursor: None,
        })
        .await
        .expect("qualified outline");
    assert_eq!(outline.total_symbols, 1);
    assert_eq!(outline.files[0].symbols.len(), 1);
    let outlined = &outline.files[0].symbols[0];
    assert_eq!(outlined.name, "wait_for_initial_index_cancellable");
    assert_eq!(outlined.parent.as_deref(), Some("Services"));

    let search_request = |query: &str| SearchRequest {
        query: query.into(),
        mode: SearchMode::Symbol,
        include_paths: vec!["service.rs".into()],
        exclude_paths: Vec::new(),
        focus_paths: Vec::new(),
        max_results: Some(10),
        max_tokens: Some(1_000),
        context_lines: Some(0),
        case_sensitive: true,
        all_occurrences: false,
        prefer_structural: false,
        receipt_id: None,
        query_receipt: None,
        cursor: None,
    };
    let searched = services
        .search_with_options(
            search_request(qualified),
            ServiceCallOptions::new().with_max_response_tokens(5_000),
        )
        .await
        .expect("qualified symbol search");
    assert_eq!(searched.coverage.definitions.total, 1);
    assert_eq!(searched.hits.len(), 1);
    assert_eq!(
        searched.hits[0].symbol.as_deref(),
        Some("wait_for_initial_index_cancellable")
    );
    assert_eq!(
        searched.hits[0].enclosing_symbol.as_deref(),
        Some("Services")
    );
    assert_eq!(searched.hits[0].start_line, outlined.start_line);

    let options = services
        .search(search_request("ServiceCallOptions"))
        .await
        .expect("fresh exact struct symbol search");
    assert_eq!(options.hits.len(), 1);
    assert_eq!(
        options.hits[0].symbol.as_deref(),
        Some("ServiceCallOptions")
    );

    let short_qualified = services
        .search(search_request("Services.go"))
        .await
        .expect("qualified symbol with a short leaf");
    assert_eq!(short_qualified.hits.len(), 1);
    assert_eq!(short_qualified.hits[0].symbol.as_deref(), Some("go"));
    assert_eq!(
        short_qualified.hits[0].enclosing_symbol.as_deref(),
        Some("Services")
    );

    let read = services
        .read(ReadRequest {
            path: "service.rs".into(),
            start_line: None,
            end_line: None,
            symbol: Some(format!("  {qualified} ")),
            heading: None,
            heading_occurrence: None,
            continuation_cursor: None,
            max_tokens: Some(1_000),
            expected_hash: None,
            delta: false,
            receipt_id: None,
            policy: leantoken::ReadPolicy::default(),
        })
        .await
        .expect("qualified live read");
    assert_eq!(read.target_start_line, outlined.start_line);
    assert!(
        read.content
            .as_deref()
            .is_some_and(|content| content.contains("\"services\""))
    );

    let historical = services
        .history(HistoryRequest {
            operation: HistoryOperation::ReadSymbol {
                path: "service.rs".into(),
                symbol: format!("  {qualified} "),
                revision: " HEAD ".into(),
            },
            max_results: None,
            max_tokens: Some(1_000),
        })
        .await
        .expect("qualified historical read");
    let historical = historical.symbol.expect("historical symbol");
    assert_eq!(historical.target_start_line, outlined.start_line);
    assert_eq!(historical.name, outlined.name);
    assert_eq!(historical.parent, outlined.parent);

    let unqualified = "wait_for_initial_index_cancellable";
    let candidates = services
        .search(search_request(unqualified))
        .await
        .expect("bounded ambiguous candidates");
    assert_eq!(candidates.coverage.definitions.total, 2);
    assert_eq!(candidates.hits.len(), 2);

    let live_ambiguity = services
        .read(ReadRequest {
            path: "service.rs".into(),
            start_line: None,
            end_line: None,
            symbol: Some(unqualified.into()),
            heading: None,
            heading_occurrence: None,
            continuation_cursor: None,
            max_tokens: Some(1_000),
            expected_hash: None,
            delta: false,
            receipt_id: None,
            policy: leantoken::ReadPolicy::default(),
        })
        .await
        .expect_err("unqualified live symbol must not select the first match");
    assert!(matches!(
        live_ambiguity,
        Error::AmbiguousSymbol { path, symbol }
            if path == "service.rs" && symbol == unqualified
    ));

    let historical_ambiguity = services
        .history(HistoryRequest {
            operation: HistoryOperation::ReadSymbol {
                path: "service.rs".into(),
                symbol: unqualified.into(),
                revision: "HEAD".into(),
            },
            max_results: None,
            max_tokens: Some(1_000),
        })
        .await
        .expect_err("unqualified historical symbol must not select the first match");
    assert!(matches!(
        historical_ambiguity,
        Error::AmbiguousSymbol { path, symbol }
            if path.starts_with("service.rs@") && symbol == unqualified
    ));

    let batch = services
        .history_diff_symbols(DiffSymbolsRequest {
            targets: vec![
                DiffSymbolsTarget {
                    path: "service.rs".into(),
                    symbol: unqualified.into(),
                    head_path: None,
                    head_symbol: None,
                },
                DiffSymbolsTarget {
                    path: "service.rs".into(),
                    symbol: qualified.into(),
                    head_path: None,
                    head_symbol: None,
                },
            ],
            base_revision: "HEAD".into(),
            head_revision: "HEAD".into(),
            max_results: Some(10),
            max_tokens: Some(1_000),
            cursor: None,
        })
        .await
        .expect("batch ambiguity remains a per-target outcome");
    assert_eq!(batch.results.len(), 2);
    assert_eq!(batch.results[0].status, DiffSymbolsStatus::Unavailable);
    assert_eq!(
        batch.results[0].reason.as_deref(),
        Some("ambiguous_base_symbol")
    );
    assert_eq!(batch.results[1].status, DiffSymbolsStatus::Unchanged);
    assert!(!batch.result_complete);
}

#[tokio::test]
async fn csharp_qualified_symbols_support_historical_reads_and_diffs() {
    require_git();

    let root = tempfile::tempdir().expect("root");
    std::fs::write(
        root.path().join("Worker.cs"),
        "class Worker {\n    int Run() {\n        return 1;\n    }\n}\n",
    )
    .expect("base C# source");
    init_git_repo(root.path());
    let revision = |name: &str| {
        String::from_utf8(
            std::process::Command::new("git")
                .args(["rev-parse", name])
                .current_dir(root.path())
                .output()
                .expect("resolve revision")
                .stdout,
        )
        .expect("UTF-8 revision")
        .trim()
        .to_owned()
    };
    let base = revision("HEAD");

    std::fs::write(
        root.path().join("Worker.cs"),
        "class Worker {\n    int Run() {\n        return 2;\n    }\n}\n",
    )
    .expect("updated C# source");
    for args in [
        vec!["add", "Worker.cs"],
        vec!["commit", "-m", "update C# method"],
    ] {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(root.path())
            .output()
            .expect("git commit command");
        assert!(output.status.success());
    }
    let head = revision("HEAD");

    let config =
        Config::discover(root.path(), Some(root.path().join("index.sqlite"))).expect("config");
    let services = Services::open(config).expect("services");
    services
        .refresh(leantoken::IndexingMode::Reconcile)
        .await
        .expect("index fixture");

    let read = services
        .history(HistoryRequest {
            operation: HistoryOperation::ReadSymbol {
                path: "Worker.cs".into(),
                symbol: "Worker.Run".into(),
                revision: base.clone(),
            },
            max_results: None,
            max_tokens: Some(200),
        })
        .await
        .expect("historical C# read");
    assert!(
        read.symbol
            .as_ref()
            .and_then(|symbol| symbol.content.as_deref())
            .is_some_and(|content| content.contains("return 1"))
    );

    let diff = services
        .history(HistoryRequest {
            operation: HistoryOperation::DiffSymbol {
                path: "Worker.cs".into(),
                symbol: "Worker.Run".into(),
                base_revision: base,
                head_revision: head,
            },
            max_results: None,
            max_tokens: Some(200),
        })
        .await
        .expect("historical C# diff");
    let patch = diff.diff.as_deref().expect("C# method diff");
    assert!(patch.contains("-        return 1;"));
    assert!(patch.contains("+        return 2;"));
    let report = services
        .token_savings_report()
        .await
        .expect("history response accounting");
    let history = report
        .response_accounting
        .by_operation
        .iter()
        .find(|row| row.operation == TokenAccountingOperation::History)
        .expect("history accounting");
    assert_eq!(history.tracked_requests, 2);
    assert_eq!(history.baseline_requests, 0);
    assert_eq!(
        history.total_response_tokens,
        (read.meta.total_response_tokens + diff.meta.total_response_tokens) as u64
    );
}

#[tokio::test]
async fn symbol_history_reads_diffs_and_traces_immutable_revisions() {
    require_git();

    let root = tempfile::tempdir().expect("root");
    std::fs::create_dir(root.path().join("src")).expect("source directory");
    std::fs::write(
        root.path().join("src/lib.rs"),
        "pub fn tracked() -> u32 { 1 }\n\npub fn unrelated() -> u32 { 10 }\n",
    )
    .expect("base source");
    init_git_repo(root.path());
    let revision = |name: &str| {
        String::from_utf8(
            std::process::Command::new("git")
                .args(["rev-parse", name])
                .current_dir(root.path())
                .output()
                .expect("resolve revision")
                .stdout,
        )
        .expect("UTF-8 revision")
        .trim()
        .to_owned()
    };
    let commit = |message: &str| {
        for args in [vec!["add", "-A"], vec!["commit", "-m", message]] {
            let output = std::process::Command::new("git")
                .args(args)
                .current_dir(root.path())
                .output()
                .expect("git commit command");
            assert!(output.status.success());
        }
    };
    let base = revision("HEAD");
    std::fs::write(
        root.path().join("src/lib.rs"),
        "pub fn tracked() -> u32 { 2 }\n\npub fn unrelated() -> u32 { 10 }\n",
    )
    .expect("updated symbol");
    commit("update tracked");
    let changed = revision("HEAD");
    std::fs::write(
        root.path().join("src/other.rs"),
        "pub fn later_change() -> u32 { 11 }\n",
    )
    .expect("unrelated update");
    commit("update unrelated");

    let config =
        Config::discover(root.path(), Some(root.path().join("index.sqlite"))).expect("config");
    let services = Services::open(config).expect("services");
    services
        .refresh(leantoken::IndexingMode::Reconcile)
        .await
        .expect("index fixture");

    let read = services
        .history(HistoryRequest {
            operation: HistoryOperation::ReadSymbol {
                path: "src/lib.rs".into(),
                symbol: "tracked".into(),
                revision: base.clone(),
            },
            max_results: None,
            max_tokens: Some(100),
        })
        .await
        .expect("historical read");
    let historical = read.symbol.as_ref().expect("historical symbol");
    assert_eq!(
        historical.content.as_deref(),
        Some("pub fn tracked() -> u32 { 1 }")
    );
    let historical_hash = historical.content_hash.clone();
    assert!(!historical.truncated);
    assert_response_token_accounting!(read, Tokenizer::default());
    let bounded_limit = read.meta.total_response_tokens.saturating_sub(10);
    let bounded_read = services
        .history_with_options(
            HistoryRequest {
                operation: HistoryOperation::ReadSymbol {
                    path: "src/lib.rs".into(),
                    symbol: "tracked".into(),
                    revision: base.clone(),
                },
                max_results: None,
                max_tokens: Some(100),
            },
            ServiceCallOptions::new().with_max_response_tokens(bounded_limit),
        )
        .await
        .expect("response-bounded historical read");
    assert!(bounded_read.meta.total_response_tokens <= bounded_limit);
    assert!(bounded_read.symbol.as_ref().expect("symbol").truncated);
    assert_response_token_accounting!(bounded_read, Tokenizer::default());

    let truncated = services
        .history(HistoryRequest {
            operation: HistoryOperation::ReadSymbol {
                path: "src/lib.rs".into(),
                symbol: "tracked".into(),
                revision: historical.revision.clone(),
            },
            max_results: None,
            max_tokens: Some(1),
        })
        .await
        .expect("truncated historical read");
    assert!(!truncated.result_complete);
    assert!(truncated.symbol.as_ref().expect("symbol").truncated);

    let diff = services
        .history(HistoryRequest {
            operation: HistoryOperation::DiffSymbol {
                path: "src/lib.rs".into(),
                symbol: "tracked".into(),
                base_revision: base.clone(),
                head_revision: changed.clone(),
            },
            max_results: None,
            max_tokens: Some(100),
        })
        .await
        .expect("historical diff");
    let patch = diff.diff.as_deref().expect("symbol diff");
    assert!(patch.contains("-pub fn tracked() -> u32 { 1 }"));
    assert!(patch.contains("+pub fn tracked() -> u32 { 2 }"));
    assert!(!patch.contains("\\ No newline at end of file"));
    let before = diff.before.as_ref().expect("base endpoint");
    assert_eq!(before.returned_end_line, 0);
    assert_eq!(before.content_hash, historical_hash);
    let serialized_diff = serde_json::to_value(&diff).expect("serialize symbol diff");
    assert!(
        serialized_diff
            .pointer("/before/returned_end_line")
            .is_none()
    );
    assert!(
        serialized_diff
            .pointer("/after/returned_end_line")
            .is_none()
    );
    assert!(diff.result_complete);
    assert_response_token_accounting!(diff, Tokenizer::default());

    let batch_request = DiffSymbolsRequest {
        targets: vec![
            DiffSymbolsTarget {
                path: "src/lib.rs".into(),
                symbol: "tracked".into(),
                head_path: None,
                head_symbol: None,
            },
            DiffSymbolsTarget {
                path: "src/lib.rs".into(),
                symbol: "unrelated".into(),
                head_path: None,
                head_symbol: None,
            },
        ],
        base_revision: base.clone(),
        head_revision: changed.clone(),
        max_results: Some(2),
        max_tokens: Some(1_000),
        cursor: None,
    };
    let batch = services
        .history_diff_symbols(batch_request.clone())
        .await
        .expect("batched historical diff");
    assert_eq!(batch.kind, "diff_symbols");
    assert_eq!(batch.base.revision, base[..12]);
    assert_eq!(batch.head.revision, changed[..12]);
    assert!(!batch.base.authored_at.is_empty());
    assert_eq!(batch.head.subject, "update tracked");
    assert_eq!(batch.results.len(), 2);
    assert_eq!(batch.results[0].status, DiffSymbolsStatus::Modified);
    assert_eq!(batch.results[0].before, diff.before);
    assert_eq!(batch.results[0].after, diff.after);
    assert_eq!(batch.results[0].diff, diff.diff);
    assert_eq!(batch.results[0].semantic_change, diff.semantic_change);
    assert_eq!(batch.results[1].status, DiffSymbolsStatus::Unchanged);
    assert!(batch.results[1].diff.is_none());
    assert!(batch.result_complete);
    assert_eq!(batch.diagnostics.git_subprocesses, 7);
    assert_eq!(batch.diagnostics.base_paths_requested, 1);
    assert_eq!(batch.diagnostics.head_paths_requested, 1);
    assert!(batch.diagnostics.parsed_symbols >= 4);
    assert_response_token_accounting!(batch, Tokenizer::default());

    let response_limit = batch.meta.total_response_tokens.saturating_sub(1);
    let response_limited = services
        .history_diff_symbols_with_options(
            batch_request.clone(),
            ServiceCallOptions::new().with_max_response_tokens(response_limit),
        )
        .await
        .expect("response-limited batched history");
    assert_eq!(
        response_limited.results.len(),
        2,
        "response fitting must preserve symbol status coverage before diff text"
    );
    assert_eq!(
        response_limited.results[0].incomplete_reason,
        Some(DiffSymbolsIncompleteReason::MaxResponseTokens)
    );
    assert!(response_limited.results[0].diff_truncated);
    assert_eq!(
        response_limited.results[1].status,
        DiffSymbolsStatus::Unchanged
    );
    assert_eq!(
        response_limited.diagnostics.retained_diff_bytes,
        response_limited.results[0]
            .diff
            .as_ref()
            .map_or(0, String::len)
    );
    assert!(response_limited.meta.total_response_tokens <= response_limit);
    assert_response_token_accounting!(response_limited, Tokenizer::default());

    let mut first_page_request = batch_request.clone();
    first_page_request.max_results = Some(1);
    let first_page = services
        .history_diff_symbols(first_page_request.clone())
        .await
        .expect("first batched history page");
    assert_eq!(first_page.results.len(), 1);
    assert!(!first_page.result_complete);

    let fitted_page_limit = first_page.meta.total_response_tokens;
    let fitted_page = services
        .history_diff_symbols_with_options(
            batch_request.clone(),
            ServiceCallOptions::new().with_max_response_tokens(fitted_page_limit),
        )
        .await
        .expect("response fitting should page complete status records");
    assert_eq!(fitted_page.results.len(), 1);
    assert_eq!(fitted_page.results[0].request_index, 0);
    assert!(fitted_page.meta.total_response_tokens <= fitted_page_limit);
    let mut fitted_continuation_request = batch_request.clone();
    fitted_continuation_request.cursor = fitted_page.meta.next_cursor.clone();
    let fitted_continuation = services
        .history_diff_symbols(fitted_continuation_request)
        .await
        .expect("continue response-fitted history page");
    assert_eq!(fitted_continuation.results[0].request_index, 1);
    assert!(fitted_continuation.meta.next_cursor.is_none());

    let cursor = first_page.meta.next_cursor.clone().expect("history cursor");
    first_page_request.cursor = Some(cursor);
    let second_page = services
        .history_diff_symbols(first_page_request.clone())
        .await
        .expect("second batched history page");
    assert_eq!(second_page.results.len(), 1);
    assert_eq!(second_page.results[0].request_index, 1);
    assert_eq!(second_page.results[0].status, DiffSymbolsStatus::Unchanged);
    assert!(second_page.meta.next_cursor.is_none());
    assert!(second_page.result_complete);
    first_page_request.targets.swap(0, 1);
    let stale = services
        .history_diff_symbols(first_page_request)
        .await
        .expect_err("cursor must bind ordered targets");
    assert!(matches!(stale, Error::StaleCursor));

    let mut token_limited_request = batch_request;
    token_limited_request.max_tokens = Some(1);
    let token_limited = services
        .history_diff_symbols(token_limited_request)
        .await
        .expect("token-limited batched history");
    assert!(!token_limited.result_complete);
    assert!(token_limited.results[0].diff_truncated);
    assert_eq!(
        token_limited.results[0].incomplete_reason,
        Some(DiffSymbolsIncompleteReason::MaxTokens)
    );
    assert_eq!(
        token_limited.diagnostics.retained_diff_bytes,
        token_limited
            .results
            .iter()
            .filter_map(|result| result.diff.as_ref())
            .map(String::len)
            .sum::<usize>()
    );
    assert_response_token_accounting!(token_limited, Tokenizer::default());

    let log = services
        .history(HistoryRequest {
            operation: HistoryOperation::SymbolLog {
                path: "src/lib.rs".into(),
                symbol: "tracked".into(),
                revision: None,
            },
            max_results: Some(1),
            max_tokens: None,
        })
        .await
        .expect("symbol history");
    assert!(!log.result_complete);
    assert_eq!(log.commits.len(), 1);
    assert_eq!(log.commits[0].subject, "update tracked");
    assert!(
        log.commits
            .iter()
            .all(|commit| commit.subject != "update unrelated")
    );
    assert_eq!(
        log.symbol
            .as_ref()
            .expect("symbol log endpoint")
            .returned_end_line,
        0
    );
    assert!(
        serde_json::to_value(&log)
            .expect("serialize symbol log")
            .pointer("/symbol/returned_end_line")
            .is_none()
    );
    assert_response_token_accounting!(log, Tokenizer::default());

    let context = services
        .context(ContextRequest {
            task: "review tracked".into(),
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
            focus_symbols: vec!["tracked".into()],
            exclude_paths: Vec::new(),
            known_hashes: Vec::new(),
            receipt_id: None,
            prior_repository_generation: None,
            base_revision: Some(format!("{base}..{changed}")),
            changed_paths: Vec::new(),
            strict_changed_paths: true,
            explain_diagnostics: false,
        })
        .await
        .expect("immutable range context");
    let scope = context.diff_scope.expect("immutable range scope");
    assert_eq!(scope.base_revision.as_deref(), Some(&base[..12]));
    assert_eq!(scope.head_revision.as_deref(), Some(&changed[..12]));
    assert_eq!(scope.changed_paths, vec!["src/lib.rs"]);
    assert!(
        context
            .fragments
            .iter()
            .all(|fragment| fragment.path == "src/lib.rs")
    );
    assert!(
        scope
            .evidence
            .expect("range evidence")
            .changed_hunks
            .iter()
            .all(|hunk| hunk.path == "src/lib.rs")
    );
}

#[tokio::test]
async fn batched_symbol_history_classifies_endpoints_renames_and_request_bounds() {
    require_git();

    let root = tempfile::tempdir().expect("root");
    std::fs::create_dir(root.path().join("src")).expect("source directory");
    std::fs::write(
        root.path().join("src/lib.rs"),
        "pub fn modified() -> u32 { 1 }\n\npub fn removed() -> u32 { 2 }\n\npub fn old_name() -> u32 { 3 }\n\npub fn stable() -> u32 { 4 }\n\npub struct Worker;\n\nimpl Worker {\n    pub fn same() -> u32 { 10 }\n}\n\npub struct Other;\n\nimpl Other {\n    pub fn same() -> u32 { 20 }\n}\n",
    )
    .expect("base source");
    init_git_repo(root.path());
    let revision = |name: &str| {
        String::from_utf8(
            std::process::Command::new("git")
                .args(["rev-parse", name])
                .current_dir(root.path())
                .output()
                .expect("resolve revision")
                .stdout,
        )
        .expect("UTF-8 revision")
        .trim()
        .to_owned()
    };
    let base = revision("HEAD");
    std::fs::write(
        root.path().join("src/lib.rs"),
        "pub fn modified() -> u32 { 10 }\n\npub fn added() -> u32 { 5 }\n\npub fn stable() -> u32 { 4 }\n\npub struct Worker;\n\nimpl Worker {\n    pub fn same() -> u32 { 11 }\n}\n\npub struct Other;\n\nimpl Other {\n    pub fn same() -> u32 { 20 }\n}\n",
    )
    .expect("head source");
    std::fs::write(
        root.path().join("src/moved.rs"),
        "pub fn new_name() -> u32 { 30 }\n",
    )
    .expect("renamed source");
    for args in [
        &["add", "-A"][..],
        &["commit", "-m", "change batched symbols"][..],
    ] {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(root.path())
            .output()
            .expect("git commit command");
        assert!(output.status.success());
    }
    let head = revision("HEAD");

    let config =
        Config::discover(root.path(), Some(root.path().join("index.sqlite"))).expect("config");
    let services = Services::open(config).expect("services");
    services
        .refresh(leantoken::IndexingMode::Reconcile)
        .await
        .expect("index fixture");
    let ordinary_target = |symbol: &str| DiffSymbolsTarget {
        path: "src/lib.rs".into(),
        symbol: symbol.into(),
        head_path: None,
        head_symbol: None,
    };
    let request = DiffSymbolsRequest {
        targets: vec![
            ordinary_target("modified"),
            ordinary_target("removed"),
            ordinary_target("added"),
            DiffSymbolsTarget {
                path: "src/lib.rs".into(),
                symbol: "old_name".into(),
                head_path: Some("src/moved.rs".into()),
                head_symbol: Some("new_name".into()),
            },
            ordinary_target("stable"),
            ordinary_target("never_existed"),
            ordinary_target("Worker.same"),
            ordinary_target("Other.same"),
        ],
        base_revision: base.clone(),
        head_revision: head.clone(),
        max_results: Some(8),
        max_tokens: Some(4_000),
        cursor: None,
    };
    let response = services
        .history_diff_symbols(request.clone())
        .await
        .expect("classify batched symbols");
    assert_eq!(
        response
            .results
            .iter()
            .map(|result| result.status)
            .collect::<Vec<_>>(),
        vec![
            DiffSymbolsStatus::Modified,
            DiffSymbolsStatus::Removed,
            DiffSymbolsStatus::Added,
            DiffSymbolsStatus::Renamed,
            DiffSymbolsStatus::Unchanged,
            DiffSymbolsStatus::NotFound,
            DiffSymbolsStatus::Modified,
            DiffSymbolsStatus::Unchanged,
        ]
    );
    assert_eq!(
        response.results[6]
            .before
            .as_ref()
            .and_then(|symbol| symbol.parent.as_deref()),
        Some("Worker")
    );
    assert_eq!(
        response.results[7]
            .before
            .as_ref()
            .and_then(|symbol| symbol.parent.as_deref()),
        Some("Other")
    );
    let renamed = &response.results[3];
    let semantic_rename = renamed.semantic_change.as_ref().expect("semantic rename");
    assert_eq!(semantic_rename.kind, DiffSymbolChangeKind::Renamed);
    assert_eq!(
        semantic_rename
            .before
            .as_ref()
            .map(|symbol| (symbol.path.as_str(), symbol.name.as_str())),
        Some(("src/lib.rs", "old_name"))
    );
    assert_eq!(
        semantic_rename
            .after
            .as_ref()
            .map(|symbol| (symbol.path.as_str(), symbol.name.as_str())),
        Some(("src/moved.rs", "new_name"))
    );
    assert_eq!(response.base.revision, base[..12]);
    assert_eq!(response.head.revision, head[..12]);
    assert_eq!(response.head.subject, "change batched symbols");
    assert_eq!(response.diagnostics.git_subprocesses, 7);
    assert_eq!(response.diagnostics.base_paths_requested, 1);
    assert_eq!(response.diagnostics.head_paths_requested, 2);
    assert!(response.diagnostics.retained_diff_bytes <= 1024 * 1024);
    assert!(response.result_complete);
    assert_response_token_accounting!(response, Tokenizer::default());

    let too_many_targets = DiffSymbolsRequest {
        targets: (0..65)
            .map(|index| ordinary_target(&format!("symbol_{index}")))
            .collect(),
        ..request.clone()
    };
    assert!(matches!(
        services
            .history_diff_symbols(too_many_targets)
            .await
            .expect_err("target bound"),
        Error::RequestLimitExceeded {
            field: "targets",
            requested: 65,
            limit: 64
        }
    ));

    let too_many_paths = DiffSymbolsRequest {
        targets: (0..33)
            .map(|index| DiffSymbolsTarget {
                path: format!("src/file_{index}.rs"),
                symbol: "item".into(),
                head_path: None,
                head_symbol: None,
            })
            .collect(),
        ..request.clone()
    };
    assert!(matches!(
        services
            .history_diff_symbols(too_many_paths)
            .await
            .expect_err("endpoint path bound"),
        Error::RequestLimitExceeded {
            field: "base paths",
            requested: 33,
            limit: 32
        }
    ));

    let duplicate = DiffSymbolsRequest {
        targets: vec![ordinary_target("stable"), ordinary_target("stable")],
        ..request.clone()
    };
    assert!(matches!(
        services
            .history_diff_symbols(duplicate)
            .await
            .expect_err("duplicate pairing"),
        Error::InvalidInput {
            field: "targets",
            reason: "must not contain duplicate symbol pairings"
        }
    ));

    let incomplete_pair = DiffSymbolsRequest {
        targets: vec![DiffSymbolsTarget {
            path: "src/lib.rs".into(),
            symbol: "stable".into(),
            head_path: Some("src/moved.rs".into()),
            head_symbol: None,
        }],
        ..request
    };
    assert!(matches!(
        services
            .history_diff_symbols(incomplete_pair)
            .await
            .expect_err("incomplete endpoint pairing"),
        Error::InvalidInput {
            field: "targets",
            reason: "head_path and head_symbol must be supplied together"
        }
    ));
}

#[tokio::test]
async fn symbol_history_resolves_qualified_names_and_absent_diff_endpoints() {
    require_git();

    let root = tempfile::tempdir().expect("root");
    std::fs::create_dir(root.path().join("src")).expect("source directory");
    std::fs::write(
        root.path().join("src/service.rs"),
        "pub struct Services;\n\nimpl Services {\n    pub fn existing() -> bool { true }\n}\n",
    )
    .expect("base service");
    std::fs::write(
        root.path().join("src/deleted.rs"),
        "pub fn deleted_endpoint() -> bool { true }\n",
    )
    .expect("deleted source");
    init_git_repo(root.path());
    let revision = |name: &str| {
        String::from_utf8(
            std::process::Command::new("git")
                .args(["rev-parse", name])
                .current_dir(root.path())
                .output()
                .expect("resolve revision")
                .stdout,
        )
        .expect("UTF-8 revision")
        .trim()
        .to_owned()
    };
    let base = revision("HEAD");

    std::fs::write(
        root.path().join("src/service.rs"),
        "pub struct Services;\n\nimpl Services {\n    pub fn existing() -> bool { true }\n\n    pub fn evaluate_read_receipt() -> bool { true }\n}\n",
    )
    .expect("head service");
    std::fs::write(
        root.path().join("src/added.rs"),
        "pub fn introduced_endpoint() -> bool { true }",
    )
    .expect("added source");
    std::fs::remove_file(root.path().join("src/deleted.rs")).expect("delete source");
    for args in [
        &["add", "-A"][..],
        &["commit", "-m", "change symbol endpoints"][..],
    ] {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(root.path())
            .output()
            .expect("git commit command");
        assert!(output.status.success());
    }
    let head = revision("HEAD");

    let config =
        Config::discover(root.path(), Some(root.path().join("index.sqlite"))).expect("config");
    let services = Services::open(config).expect("services");
    services
        .refresh(leantoken::IndexingMode::Reconcile)
        .await
        .expect("index fixture");

    let qualified = services
        .history(HistoryRequest {
            operation: HistoryOperation::ReadSymbol {
                path: "src/service.rs".into(),
                symbol: "Services.evaluate_read_receipt".into(),
                revision: head.clone(),
            },
            max_results: None,
            max_tokens: Some(200),
        })
        .await
        .expect("qualified historical read");
    let qualified_symbol = qualified.symbol.expect("qualified symbol");
    assert_eq!(qualified_symbol.name, "evaluate_read_receipt");
    assert_eq!(qualified_symbol.parent.as_deref(), Some("Services"));

    let qualified_log = services
        .history(HistoryRequest {
            operation: HistoryOperation::SymbolLog {
                path: "src/service.rs".into(),
                symbol: "Services.evaluate_read_receipt".into(),
                revision: Some(head.clone()),
            },
            max_results: Some(10),
            max_tokens: None,
        })
        .await
        .expect("qualified symbol log");
    assert!(
        qualified_log
            .commits
            .iter()
            .any(|commit| commit.subject == "change symbol endpoints")
    );

    let added_read = services
        .history(HistoryRequest {
            operation: HistoryOperation::ReadSymbol {
                path: "src/added.rs".into(),
                symbol: "introduced_endpoint".into(),
                revision: head.clone(),
            },
            max_results: None,
            max_tokens: Some(500),
        })
        .await
        .expect("read symbol from file without final newline");
    let added_raw = added_read.symbol.as_ref().expect("added raw symbol");
    assert_eq!(
        added_raw.content.as_deref(),
        Some("pub fn introduced_endpoint() -> bool { true }")
    );
    let added_raw_hash = added_raw.content_hash.clone();
    assert!(
        serde_json::to_value(&added_read)
            .expect("serialize raw historical read")
            .pointer("/symbol/returned_end_line")
            .is_some()
    );

    let added_symbol = services
        .history(HistoryRequest {
            operation: HistoryOperation::DiffSymbol {
                path: "src/service.rs".into(),
                symbol: "Services.evaluate_read_receipt".into(),
                base_revision: base.clone(),
                head_revision: head.clone(),
            },
            max_results: None,
            max_tokens: Some(500),
        })
        .await
        .expect("added nested symbol diff");
    assert!(added_symbol.before.is_none());
    assert_eq!(
        added_symbol
            .after
            .as_ref()
            .and_then(|symbol| symbol.parent.as_deref()),
        Some("Services")
    );
    assert_eq!(
        added_symbol
            .semantic_change
            .as_ref()
            .map(|change| change.kind),
        Some(DiffSymbolChangeKind::Added)
    );
    assert!(
        added_symbol
            .semantic_change
            .as_ref()
            .expect("added semantic change")
            .public_contract_changed
    );
    assert!(
        added_symbol
            .diff
            .as_deref()
            .expect("added symbol patch")
            .contains("+pub fn evaluate_read_receipt() -> bool { true }")
    );
    assert!(
        !added_symbol
            .diff
            .as_deref()
            .expect("added symbol patch")
            .contains("\\ No newline at end of file")
    );
    assert!(
        serde_json::to_value(&added_symbol)
            .expect("serialize added nested symbol")
            .pointer("/after/returned_end_line")
            .is_none()
    );

    let added_file = services
        .history(HistoryRequest {
            operation: HistoryOperation::DiffSymbol {
                path: "src/added.rs".into(),
                symbol: "introduced_endpoint".into(),
                base_revision: base.clone(),
                head_revision: head.clone(),
            },
            max_results: None,
            max_tokens: Some(500),
        })
        .await
        .expect("added file symbol diff");
    assert!(added_file.before.is_none());
    assert_eq!(
        added_file
            .semantic_change
            .as_ref()
            .map(|change| change.kind),
        Some(DiffSymbolChangeKind::Added)
    );
    let added_endpoint = added_file.after.as_ref().expect("added endpoint");
    assert_eq!(added_endpoint.returned_end_line, 0);
    assert_eq!(added_endpoint.content_hash, added_raw_hash);
    assert!(
        !added_file
            .diff
            .as_deref()
            .expect("added file patch")
            .contains("\\ No newline at end of file")
    );
    assert!(
        serde_json::to_value(&added_file)
            .expect("serialize added file diff")
            .pointer("/after/returned_end_line")
            .is_none()
    );
    assert!(added_file.result_complete);

    let truncated_added_file = services
        .history(HistoryRequest {
            operation: HistoryOperation::DiffSymbol {
                path: "src/added.rs".into(),
                symbol: "introduced_endpoint".into(),
                base_revision: base.clone(),
                head_revision: head.clone(),
            },
            max_results: None,
            max_tokens: Some(1),
        })
        .await
        .expect("truncated added file symbol diff");
    assert!(truncated_added_file.diff_truncated);
    assert!(!truncated_added_file.result_complete);
    assert!(truncated_added_file.before.is_none());
    assert!(truncated_added_file.after.is_some());
    assert!(
        serde_json::to_value(&truncated_added_file)
            .expect("serialize truncated added file diff")
            .pointer("/after/returned_end_line")
            .is_none()
    );
    assert_eq!(
        truncated_added_file
            .semantic_change
            .as_ref()
            .map(|change| change.kind),
        Some(DiffSymbolChangeKind::Added)
    );

    let deleted_file = services
        .history(HistoryRequest {
            operation: HistoryOperation::DiffSymbol {
                path: "src/deleted.rs".into(),
                symbol: "deleted_endpoint".into(),
                base_revision: base.clone(),
                head_revision: head.clone(),
            },
            max_results: None,
            max_tokens: Some(500),
        })
        .await
        .expect("deleted file symbol diff");
    assert!(deleted_file.after.is_none());
    assert_eq!(
        deleted_file
            .semantic_change
            .as_ref()
            .map(|change| change.kind),
        Some(DiffSymbolChangeKind::Removed)
    );
    assert!(
        deleted_file
            .semantic_change
            .as_ref()
            .expect("removed semantic change")
            .public_contract_changed
    );
    assert!(
        deleted_file
            .diff
            .as_deref()
            .expect("deleted symbol patch")
            .contains("-pub fn deleted_endpoint() -> bool { true }")
    );
    assert!(
        !deleted_file
            .diff
            .as_deref()
            .expect("deleted symbol patch")
            .contains("\\ No newline at end of file")
    );
    assert!(
        serde_json::to_value(&deleted_file)
            .expect("serialize deleted file diff")
            .pointer("/before/returned_end_line")
            .is_none()
    );
    assert_response_token_accounting!(deleted_file, Tokenizer::default());

    let missing_file_read = services
        .history(HistoryRequest {
            operation: HistoryOperation::ReadSymbol {
                path: "src/added.rs".into(),
                symbol: "introduced_endpoint".into(),
                revision: base.clone(),
            },
            max_results: None,
            max_tokens: Some(500),
        })
        .await
        .expect_err("ordinary historical reads keep missing-file errors");
    assert!(matches!(
        missing_file_read,
        Error::InvalidInput {
            field: "path",
            reason: "file does not exist at revision"
        }
    ));

    let missing = services
        .history(HistoryRequest {
            operation: HistoryOperation::DiffSymbol {
                path: "src/service.rs".into(),
                symbol: "Services.never_existed".into(),
                base_revision: base,
                head_revision: head,
            },
            max_results: None,
            max_tokens: Some(500),
        })
        .await
        .expect_err("both absent symbol endpoints must fail");
    assert!(matches!(missing, Error::SymbolNotFound { .. }));
}
