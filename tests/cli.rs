use clap::Parser;
use leantoken::cli::{AppRequest, Cli};
use leantoken::model::{FileOperation, SearchMode};
use leantoken::tokens::Tokenizer;
use leantoken::setup::SetupClient;

fn parse(args: &[&str]) -> Cli {
    Cli::try_parse_from(std::iter::once("leantoken").chain(args.iter().copied())).unwrap()
}

#[test]
fn cli_files_tree_request() {
    let cli = parse(&["files", "tree", "--path", "src", "--depth", "2"]);
    let AppRequest::Files(request) = cli.app_request() else {
        panic!("expected files request");
    };
    assert_eq!(request.operation, FileOperation::Tree);
    assert_eq!(request.path, Some("src".into()));
    assert_eq!(request.depth, Some(2));
}

#[test]
fn cli_files_find_request() {
    let cli = parse(&["files", "find", "--query", "cli", "--max-results", "10"]);
    let AppRequest::Files(request) = cli.app_request() else {
        panic!("expected files request");
    };
    assert_eq!(request.operation, FileOperation::Find);
    assert_eq!(request.query, Some("cli".into()));
    assert_eq!(request.max_results, Some(10));
}

#[test]
fn cli_files_glob_request() {
    let cli = parse(&["files", "glob", "--pattern", "*.rs"]);
    let AppRequest::Files(request) = cli.app_request() else {
        panic!("expected files request");
    };
    assert_eq!(request.operation, FileOperation::Glob);
    assert_eq!(request.pattern, Some("*.rs".into()));
}

#[test]
fn cli_search_request() {
    let cli = parse(&[
        "search",
        "foo",
        "--mode",
        "regex",
        "--include",
        "src",
        "--exclude",
        "tests",
        "--max-results",
        "5",
        "--max-tokens",
        "1024",
        "--context-lines",
        "3",
        "--case-sensitive",
    ]);
    let AppRequest::Search(request) = cli.app_request() else {
        panic!("expected search request");
    };
    assert_eq!(request.query, "foo");
    assert_eq!(request.mode, SearchMode::Regex);
    assert_eq!(request.include_paths, vec!["src".to_string()]);
    assert_eq!(request.exclude_paths, vec!["tests".to_string()]);
    assert_eq!(request.max_results, Some(5));
    assert_eq!(request.max_tokens, Some(1024));
    assert_eq!(request.context_lines, Some(3));
    assert!(request.case_sensitive);
}

#[test]
fn cli_search_default_mode_is_auto() {
    let cli = parse(&["search", "bar"]);
    let AppRequest::Search(request) = cli.app_request() else {
        panic!("expected search request");
    };
    assert_eq!(request.mode, SearchMode::Auto);
}

#[test]
fn cli_outline_request() {
    let cli = parse(&[
        "outline",
        "src/lib.rs",
        "src/main.rs",
        "--symbol-name",
        "Cli",
        "--max-tokens",
        "500",
    ]);
    let AppRequest::Outline(request) = cli.app_request() else {
        panic!("expected outline request");
    };
    assert_eq!(
        request.paths,
        vec!["src/lib.rs".to_string(), "src/main.rs".to_string()]
    );
    assert_eq!(request.symbol_name, Some("Cli".into()));
    assert_eq!(request.max_tokens, Some(500));
}

#[test]
fn cli_read_request() {
    let cli = parse(&[
        "read",
        "src/lib.rs",
        "--lines",
        "10:20",
        "--max-tokens",
        "100",
        "--expected-hash",
        "abc123",
    ]);
    let AppRequest::Read(request) = cli.app_request() else {
        panic!("expected read request");
    };
    assert_eq!(request.path, "src/lib.rs");
    assert_eq!(request.start_line, Some(10));
    assert_eq!(request.end_line, Some(20));
    assert_eq!(request.symbol, None);
    assert_eq!(request.max_tokens, Some(100));
    assert_eq!(request.expected_hash, Some("abc123".into()));
}

#[test]
fn cli_read_rejects_conflicting_or_invalid_ranges() {
    assert!(
        Cli::try_parse_from([
            "leantoken",
            "read",
            "src/lib.rs",
            "--lines",
            "10:20",
            "--symbol",
            "foo",
        ])
        .is_err()
    );
    assert!(Cli::try_parse_from(["leantoken", "read", "x", "--lines", ":"]).is_err());
}

#[test]
fn cli_global_json_works_before_or_after_subcommand() {
    assert!(parse(&["--json", "status"]).json);
    assert!(parse(&["status", "--json"]).json);
}

#[test]
fn cli_tokenizer_global_option() {
    let cli = parse(&["--tokenizer", "o200k_base", "status"]);
    assert_eq!(cli.tokenizer, Tokenizer::O200kBase);
    assert_eq!(parse(&["status"]).tokenizer, Tokenizer::default());
}

#[test]
fn cli_read_line_range_allows_open_ends() {
    let cli = parse(&["read", "src/lib.rs", "--lines", "10:"]);
    let AppRequest::Read(request) = cli.app_request() else {
        panic!("expected read request");
    };
    assert_eq!(request.start_line, Some(10));
    assert_eq!(request.end_line, None);

    let cli = parse(&["read", "src/lib.rs", "--lines", ":20"]);
    let AppRequest::Read(request) = cli.app_request() else {
        panic!("expected read request");
    };
    assert_eq!(request.start_line, None);
    assert_eq!(request.end_line, Some(20));
}

#[test]
fn cli_context_request() {
    let cli = parse(&[
        "context",
        "--task",
        "fix the bug",
        "--budget",
        "1024",
        "--focus",
        "src",
        "--focus-symbol",
        "sym",
        "--exclude",
        "tests",
        "--known-hash",
        "abc",
        "--prior-generation",
        "7",
    ]);
    let AppRequest::Context(request) = cli.app_request() else {
        panic!("expected context request");
    };
    assert_eq!(request.task, "fix the bug");
    assert_eq!(request.token_budget, 1024);
    assert_eq!(request.focus_paths, vec!["src".to_string()]);
    assert_eq!(request.focus_symbols, vec!["sym".to_string()]);
    assert_eq!(request.exclude_paths, vec!["tests".to_string()]);
    assert_eq!(request.known_hashes, vec!["abc".to_string()]);
    assert_eq!(request.prior_repository_generation, Some(7));
}

#[test]
fn cli_context_requires_task_and_budget() {
    let no_task = Cli::try_parse_from(["leantoken", "context", "--budget", "100"]);
    assert!(no_task.is_err());

    let no_budget = Cli::try_parse_from(["leantoken", "context", "--task", "x"]);
    assert!(no_budget.is_err());
}

#[test]
fn cli_request_limit_boundaries_reject_only_meaningless_zero_values() {
    for args in [
        &["leantoken", "files", "tree", "--max-results", "0"][..],
        &["leantoken", "search", "x", "--max-results", "0"],
        &["leantoken", "search", "x", "--max-tokens", "0"],
        &["leantoken", "outline", "src/lib.rs", "--max-results", "0"],
        &["leantoken", "outline", "src/lib.rs", "--max-tokens", "0"],
        &["leantoken", "read", "src/lib.rs", "--max-tokens", "0"],
        &["leantoken", "context", "--task", "x", "--budget", "0"],
    ] {
        assert!(Cli::try_parse_from(args).is_err(), "accepted {args:?}");
    }

    for value in ["1", "100", "101"] {
        for args in [
            vec!["leantoken", "files", "tree", "--max-results", value],
            vec!["leantoken", "search", "x", "--max-results", value],
            vec![
                "leantoken",
                "outline",
                "src/lib.rs",
                "--max-results",
                value,
            ],
        ] {
            assert!(Cli::try_parse_from(args).is_ok(), "rejected {value}");
        }
    }

    for value in ["1", "32000", "32001"] {
        for args in [
            vec!["leantoken", "search", "x", "--max-tokens", value],
            vec![
                "leantoken",
                "outline",
                "src/lib.rs",
                "--max-tokens",
                value,
            ],
            vec![
                "leantoken",
                "read",
                "src/lib.rs",
                "--max-tokens",
                value,
            ],
            vec!["leantoken", "context", "--task", "x", "--budget", value],
        ] {
            assert!(Cli::try_parse_from(args).is_ok(), "rejected {value}");
        }
    }

    for value in ["0", "1", "20", "21"] {
        assert!(
            Cli::try_parse_from([
                "leantoken",
                "search",
                "x",
                "--context-lines",
                value,
            ])
            .is_ok(),
            "CLI should defer context-lines={value} to Services"
        );
    }
    assert!(Cli::try_parse_from(["leantoken", "files", "tree", "--depth", "0"]).is_ok());
}

#[test]
fn cli_index_and_status_and_mcp_commands() {
    let cli = parse(&["index"]);
    assert!(matches!(
        cli.app_request(),
        AppRequest::Index { rebuild: false }
    ));

    let cli = parse(&["index", "--rebuild"]);
    assert!(matches!(
        cli.app_request(),
        AppRequest::Index { rebuild: true }
    ));

    let cli = parse(&["status"]);
    assert!(matches!(cli.app_request(), AppRequest::Status));

    let cli = parse(&["mcp"]);
    assert!(matches!(
        cli.app_request(),
        AppRequest::Mcp {
            result_mode: leantoken::mcp::McpResultMode::Dual
        }
    ));
    let cli = parse(&["mcp", "--result-mode", "structured"]);
    assert!(matches!(
        cli.app_request(),
        AppRequest::Mcp {
            result_mode: leantoken::mcp::McpResultMode::Structured
        }
    ));
}

#[test]
fn cli_setup_and_remove_select_clients() {
    let cli = parse(&["setup", "--claude", "--codex", "--yes"]);
    let AppRequest::Setup(request) = cli.app_request() else {
        panic!("expected setup request");
    };
    assert_eq!(
        request.clients,
        vec![SetupClient::Claude, SetupClient::Codex]
    );
    assert!(!request.all);
    assert!(!request.refresh);
    assert!(request.yes);
    assert!(!request.dry_run);

    let cli = parse(&["remove", "--all", "-y"]);
    let AppRequest::Remove(request) = cli.app_request() else {
        panic!("expected remove request");
    };
    assert!(request.clients.is_empty());
    assert!(request.all);
    assert!(!request.refresh);
    assert!(request.yes);

    let cli = parse(&["setup", "--cursor", "--dry-run"]);
    let AppRequest::Setup(request) = cli.app_request() else {
        panic!("expected setup request");
    };
    assert_eq!(request.clients, vec![SetupClient::Cursor]);
    assert!(request.dry_run);

    let cli = parse(&["setup", "--refresh", "--yes"]);
    let AppRequest::Setup(request) = cli.app_request() else {
        panic!("expected setup request");
    };
    assert!(request.clients.is_empty());
    assert!(!request.all);
    assert!(request.refresh);
    assert!(request.yes);
}

#[test]
fn cli_doctor_selects_executable_readiness_diagnostic() {
    let cli = parse(&["doctor"]);
    assert!(matches!(cli.app_request(), AppRequest::Doctor));
}

#[test]
fn cli_update_and_upgrade_are_aliases() {
    assert!(matches!(
        parse(&["update", "--check"]).app_request(),
        AppRequest::Upgrade {
            check: true,
            yes: false
        }
    ));
    assert!(matches!(
        parse(&["upgrade", "--yes"]).app_request(),
        AppRequest::Upgrade {
            check: false,
            yes: true
        }
    ));
}

#[test]
fn cli_cache_commands_resolve_without_repository_configuration() {
    assert!(matches!(
        parse(&["cache", "list"]).app_request(),
        AppRequest::CacheList
    ));

    let request = parse(&[
        "cache",
        "prune",
        "--older-than",
        "30",
        "--max-total-bytes",
        "1048576",
        "--remove-missing-roots",
        "--dry-run",
    ])
    .app_request();
    let AppRequest::CachePrune(request) = request else {
        panic!("expected cache prune request");
    };
    assert_eq!(request.older_than_days, Some(30));
    assert_eq!(request.max_total_bytes, Some(1_048_576));
    assert!(request.remove_missing_roots);
    assert!(request.dry_run);
    assert!(!request.yes);

    let AppRequest::CachePrune(zero_budget) =
        parse(&["cache", "prune", "--max-total-bytes", "0", "--dry-run"])
            .app_request()
    else {
        panic!("expected zero-budget cache prune request");
    };
    assert_eq!(zero_budget.max_total_bytes, Some(0));
}

#[test]
fn cli_global_root_and_database_options() {
    let root = tempfile::tempdir().unwrap();
    let db = root.path().join("custom.sqlite");
    let cli = parse(&[
        "--root",
        root.path().to_str().unwrap(),
        "--database",
        db.to_str().unwrap(),
        "status",
    ]);
    let config = cli.config().unwrap();
    assert_eq!(config.root, root.path().canonicalize().unwrap());
    assert_eq!(
        config.database_path,
        root.path().canonicalize().unwrap().join("custom.sqlite")
    );
    assert!(!cli.allow_broad_root);
    assert!(!cli.include_generated);
}

#[test]
fn cli_generated_tree_override_is_explicit_and_global() {
    let root = tempfile::tempdir().expect("root");
    let cli = parse(&[
        "status",
        "--root",
        root.path().to_str().expect("root UTF-8"),
        "--include-generated",
    ]);

    assert!(cli.include_generated);
    assert!(cli.config().expect("config").include_generated);
}

#[test]
fn cli_broad_root_override_is_explicit_and_global() {
    let home = directories::BaseDirs::new()
        .expect("home directories")
        .home_dir()
        .canonicalize()
        .expect("canonical home");
    let cli = parse(&[
        "status",
        "--root",
        home.to_str().expect("home UTF-8"),
        "--allow-broad-root",
    ]);

    assert!(cli.allow_broad_root);
    assert_eq!(cli.config().expect("explicit override").root, home);
}

#[test]
fn cli_discovery_limits_are_explicit_positive_global_options() {
    let root = tempfile::tempdir().expect("root");
    let cli = parse(&[
        "status",
        "--root",
        root.path().to_str().expect("root UTF-8"),
        "--max-walk-entries",
        "101",
        "--max-files",
        "102",
        "--max-total-source-bytes",
        "103",
        "--max-depth",
        "4",
        "--max-file-bytes",
        "5",
        "--max-prepare-batch-files",
        "6",
        "--max-prepare-batch-bytes",
        "7",
    ]);

    let limits = cli.config().expect("configured limits").discovery_limits();
    assert_eq!(limits.max_walk_entries, 101);
    assert_eq!(limits.max_files, 102);
    assert_eq!(limits.max_total_source_bytes, 103);
    assert_eq!(limits.max_depth, 4);
    assert_eq!(limits.max_file_bytes, 5);
    assert_eq!(limits.max_prepare_batch_files, 6);
    assert_eq!(limits.max_prepare_batch_bytes, 7);
}

#[test]
fn cli_discovery_limits_reject_zero_and_inconsistent_batches() {
    for flag in [
        "--max-walk-entries",
        "--max-files",
        "--max-total-source-bytes",
        "--max-depth",
        "--max-file-bytes",
        "--max-prepare-batch-files",
        "--max-prepare-batch-bytes",
    ] {
        assert!(
            Cli::try_parse_from(["leantoken", "status", flag, "0"]).is_err(),
            "{flag} accepted zero"
        );
    }

    let cli = parse(&[
        "status",
        "--max-file-bytes",
        "8",
        "--max-prepare-batch-bytes",
        "7",
    ]);
    assert!(cli.config().is_err());
}
