use clap::Parser;
use leantoken::cli::{AppRequest, Cli};
use leantoken::model::{FileOperation, SearchMode};

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
    assert!(matches!(cli.app_request(), AppRequest::Mcp));
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
    assert_eq!(config.database_path, db);
}
