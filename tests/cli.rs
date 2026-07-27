use clap::{CommandFactory, Parser, error::ErrorKind};
use leantoken::cache::{CacheCompatibility, CacheState, DEFAULT_CACHE_LIST_LIMIT};
use leantoken::cli::{AppRequest, Cli};
use leantoken::model::{
    ContextWorkflow, FileOperation, HistoryOperation, IndexConsistency, JsonOperation,
    JsonProjection, JsonSelector, SearchMode,
};
use leantoken::tokens::Tokenizer;
use leantoken::setup::SetupClient;

fn parse(args: &[&str]) -> Cli {
    Cli::try_parse_from(std::iter::once("leantoken").chain(args.iter().copied())).unwrap()
}

fn help(args: &[&str]) -> String {
    let error = Cli::try_parse_from(
        std::iter::once("leantoken")
            .chain(args.iter().copied())
            .chain(std::iter::once("--help")),
    )
    .expect_err("help exits before producing a parsed CLI");
    assert_eq!(error.kind(), ErrorKind::DisplayHelp);
    error
        .to_string()
        .lines()
        .map(str::trim_end)
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn cli_root_help_snapshot() {
    insta::assert_snapshot!("root_help", help(&[]));
}

#[test]
fn cli_search_help_snapshot() {
    insta::assert_snapshot!("search_help", help(&["search"]));
}

#[test]
fn cli_setup_help_snapshot() {
    insta::assert_snapshot!("setup_help", help(&["setup"]));
}

#[test]
fn cli_cache_help_snapshot() {
    insta::assert_snapshot!("cache_help", help(&["cache"]));
}

#[test]
fn usage_guide_tracks_runtime_cli_surface() {
    let command = Cli::command();
    let runtime_commands = command
        .get_subcommands()
        .filter(|subcommand| subcommand.get_name() != "help")
        .map(|subcommand| subcommand.get_name().to_owned())
        .collect::<std::collections::BTreeSet<_>>();

    let usage = include_str!("../docs/usage.md");
    let command_section = usage
        .split_once("## CLI commands\n")
        .expect("CLI command section")
        .1
        .split_once("\n\nUse `leantoken <command> --help`")
        .expect("CLI command section end")
        .0;
    let documented_commands = command_section
        .lines()
        .filter_map(|line| line.strip_prefix("leantoken "))
        .map(|line| line.split_once(' ').map_or(line, |(name, _)| name))
        .map(str::to_owned)
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(documented_commands, runtime_commands);

    for argument in command.get_arguments() {
        if matches!(argument.get_id().as_str(), "help" | "version") {
            continue;
        }
        if let Some(long) = argument.get_long() {
            assert!(
                usage.contains(&format!("--{long}")),
                "usage guide is missing runtime option --{long}"
            );
        }
    }
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
fn cli_retrieval_response_budget_uses_nonbreaking_options_variants() {
    let cli = parse(&[
        "files",
        "tree",
        "--path",
        "src",
        "--max-response-tokens",
        "777",
    ]);
    let AppRequest::FilesWithOptions {
        request,
        max_response_tokens,
    } = cli.app_request()
    else {
        panic!("expected response-bounded files request");
    };
    assert_eq!(request.operation, FileOperation::Tree);
    assert_eq!(max_response_tokens, 777);

    let error = Cli::try_parse_from([
        "leantoken",
        "search",
        "foo",
        "--max-response-tokens",
        "0",
    ])
    .expect_err("zero response budget");
    assert_eq!(error.kind(), ErrorKind::ValueValidation);
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
        "--all-occurrences",
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
    assert!(request.all_occurrences);
}

#[test]
fn cli_identifier_search_prefers_structural_hits() {
    let cli = parse(&[
        "search",
        "target",
        "--mode",
        "identifier",
        "--prefer-structural",
    ]);
    let AppRequest::Search(request) = cli.app_request() else {
        panic!("expected search request");
    };

    assert_eq!(request.mode, SearchMode::Identifier);
    assert!(request.prefer_structural);
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
        "--cursor",
        "12:34",
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
    assert_eq!(request.cursor, Some("12:34".into()));
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
    assert_eq!(request.heading, None);
    assert_eq!(request.heading_occurrence, None);
    assert_eq!(request.continuation_cursor, None);
    assert_eq!(request.max_tokens, Some(100));
    assert_eq!(request.expected_hash, Some("abc123".into()));
    assert!(!request.delta);
}

#[test]
fn cli_read_does_not_expose_process_local_delta_state() {
    let error = Cli::try_parse_from(["leantoken", "read", "src/lib.rs", "--delta"])
        .expect_err("one-shot CLI must not advertise process-local delta state");

    assert_eq!(error.kind(), ErrorKind::UnknownArgument);
    assert!(!help(&["read"]).contains("--delta"));
}

#[test]
fn cli_history_request() {
    let cli = parse(&[
        "history",
        "diff-symbol",
        "src/lib.rs",
        "Services",
        "main~1",
        "main",
        "--max-tokens",
        "500",
    ]);
    let AppRequest::History(request) = cli.app_request() else {
        panic!("expected history request");
    };
    assert_eq!(request.max_tokens, Some(500));
    assert!(matches!(
        request.operation,
        HistoryOperation::DiffSymbol {
            path,
            symbol,
            base_revision,
            head_revision,
        } if path == "src/lib.rs"
            && symbol == "Services"
            && base_revision == "main~1"
            && head_revision == "main"
    ));
}

#[test]
fn cli_json_request() {
    let cli = parse(&[
        "json",
        "diff-fields",
        "before.json",
        "after.json",
        "--pointer",
        "/version",
        "--jmespath",
        "runs[].score",
        "--projection",
        "collapsed",
        "--max-items",
        "50",
        "--array-sample-size",
        "2",
    ]);
    let AppRequest::Json(request) = cli.app_request() else {
        panic!("expected JSON request");
    };
    assert_eq!(request.max_items, Some(50));
    assert_eq!(request.array_sample_size, Some(2));
    assert!(request.cursor.is_none());
    assert!(matches!(
        request.operation,
        JsonOperation::DiffFields {
            base_path,
            head_path,
            selectors,
            projection: JsonProjection::Collapsed,
        } if base_path == "before.json"
            && head_path == "after.json"
            && selectors == vec![
                JsonSelector::Pointer {
                    pointer: "/version".into()
                },
                JsonSelector::Jmespath {
                    expression: "runs[].score".into()
                }
            ]
    ));

    let cli = parse(&[
        "json",
        "--cursor",
        "j1:source:query:2",
        "query",
        "report.json",
        "--projection",
        "keys",
    ]);
    let AppRequest::Json(request) = cli.app_request() else {
        panic!("expected paged JSON request");
    };
    assert_eq!(request.cursor.as_deref(), Some("j1:source:query:2"));
    assert!(matches!(
        request.operation,
        JsonOperation::Query {
            path,
            selector: None,
            projection: JsonProjection::Keys,
        } if path == "report.json"
    ));
}

#[test]
fn cli_nested_leaf_help_describes_inherited_limits_and_positionals() {
    let history_help = help(&["history", "read-symbol"]);
    assert!(history_help.contains("<PATH>"));
    assert!(history_help.contains("Repository-relative source file path"));
    assert!(history_help.contains("<SYMBOL>"));
    assert!(history_help.contains("Exact parsed symbol name"));
    assert!(history_help.contains("<REVISION>"));
    assert!(history_help.contains("Immutable Git revision"));
    assert!(history_help.contains("--max-tokens <MAX_TOKENS>"));

    let json_help = help(&["json", "query"]);
    assert!(json_help.contains("<PATH>"));
    assert!(json_help.contains("Repository-relative JSON file path"));
    assert!(json_help.contains("--max-tokens <MAX_TOKENS>"));
    assert!(json_help.contains("--max-items <MAX_ITEMS>"));
    assert!(json_help.contains("--array-sample-size <ARRAY_SAMPLE_SIZE>"));
    assert!(json_help.contains("--cursor <CURSOR>"));

    let context_help = help(&["context"]);
    assert!(context_help.contains("Maximum source tokens across returned fragments"));
    assert!(!context_help.contains("Token budget for the response"));
}

#[test]
fn cli_nested_limits_accept_parent_and_leaf_placement() {
    for args in [
        [
            "history",
            "--max-tokens",
            "500",
            "symbol-log",
            "src/lib.rs",
            "Cli",
        ],
        [
            "history",
            "symbol-log",
            "src/lib.rs",
            "Cli",
            "--max-tokens",
            "500",
        ],
    ] {
        let AppRequest::History(request) = parse(&args).app_request() else {
            panic!("expected history request");
        };
        assert_eq!(request.max_tokens, Some(500));
    }

    for args in [
        ["json", "--max-items", "50", "query", "report.json"],
        ["json", "query", "report.json", "--max-items", "50"],
    ] {
        let AppRequest::Json(request) = parse(&args).app_request() else {
            panic!("expected JSON request");
        };
        assert_eq!(request.max_items, Some(50));
    }
}

#[test]
fn cli_advanced_repository_options_are_discoverable_once_and_remain_global() {
    let root_help = help(&[]);
    assert!(root_help.contains("Advanced repository options:"));
    assert!(root_help.contains("--max-files <COUNT>"));

    let leaf_help = help(&["history", "read-symbol"]);
    assert!(!leaf_help.contains("Advanced repository options:"));
    assert!(!leaf_help.contains("--max-files <COUNT>"));

    let cli = parse(&[
        "history",
        "read-symbol",
        "src/lib.rs",
        "Cli",
        "main",
        "--max-files",
        "17",
    ]);
    assert_eq!(cli.max_files.map(|value| value.get()), Some(17));
}

#[test]
fn cli_read_markdown_heading_occurrence() {
    let cli = parse(&[
        "read",
        "README.md",
        "--heading",
        "Installation",
        "--heading-occurrence",
        "2",
    ]);
    let AppRequest::Read(request) = cli.app_request() else {
        panic!("expected read request");
    };
    assert_eq!(request.heading.as_deref(), Some("Installation"));
    assert_eq!(request.heading_occurrence, Some(2));
    assert!(request.symbol.is_none());
    assert!(request.start_line.is_none());
    assert!(request.end_line.is_none());

    assert!(
        Cli::try_parse_from([
            "leantoken",
            "read",
            "README.md",
            "--heading-occurrence",
            "2",
        ])
        .is_err()
    );
    assert!(
        Cli::try_parse_from([
            "leantoken",
            "read",
            "README.md",
            "--heading",
            "Installation",
            "--symbol",
            "run",
        ])
        .is_err()
    );
}

#[test]
fn cli_read_continuation_request_conflicts_with_new_targets() {
    let cli = parse(&["read", "src/lib.rs", "--cursor", "opaque"]);
    let AppRequest::Read(request) = cli.app_request() else {
        panic!("expected read request");
    };
    assert_eq!(request.path, "src/lib.rs");
    assert_eq!(request.continuation_cursor, Some("opaque".into()));
    assert!(request.symbol.is_none());
    assert!(request.heading.is_none());
    assert!(request.heading_occurrence.is_none());
    assert!(request.start_line.is_none());
    assert!(request.end_line.is_none());

    assert!(
        Cli::try_parse_from([
            "leantoken",
            "read",
            "src/lib.rs",
            "--cursor",
            "opaque",
            "--symbol",
            "run",
        ])
        .is_err()
    );
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
fn cli_index_backed_retrievals_default_to_live_consistency_with_snapshot_opt_out() {
    for arguments in [
        &["files", "tree"][..],
        &["search", "needle"][..],
        &["outline", "src/lib.rs"][..],
        &["read", "src/lib.rs"][..],
        &["context", "--task", "find the owner"][..],
    ] {
        assert_eq!(
            parse(arguments).retrieval_consistency(),
            Some(IndexConsistency::ReconcileWorkingTree),
            "default consistency for {arguments:?}"
        );
    }

    for arguments in [
        &["files", "tree", "--consistency", "indexed_generation"][..],
        &[
            "search",
            "needle",
            "--consistency",
            "indexed_generation",
        ][..],
        &[
            "outline",
            "src/lib.rs",
            "--consistency",
            "indexed_generation",
        ][..],
        &[
            "read",
            "src/lib.rs",
            "--consistency",
            "indexed_generation",
        ][..],
        &[
            "context",
            "--task",
            "find the owner",
            "--consistency",
            "indexed_generation",
        ][..],
    ] {
        assert_eq!(
            parse(arguments).retrieval_consistency(),
            Some(IndexConsistency::IndexedGeneration),
            "snapshot consistency for {arguments:?}"
        );
    }

    assert_eq!(parse(&["status"]).retrieval_consistency(), None);
}

#[cfg(unix)]
#[test]
fn repository_scope_validation_detects_non_utf8_option_values() {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;

    let arguments = vec![
        OsString::from("leantoken"),
        OsString::from_vec(b"--root=\x80".to_vec()),
        OsString::from("setup"),
        OsString::from("--all"),
        OsString::from("--dry-run"),
    ];
    let cli = Cli::try_parse_from(arguments.clone()).expect("non-UTF-8 path argument");

    assert!(cli.validate_option_scope(&arguments).is_err());
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
        "--max-response-tokens",
        "2048",
        "--include",
        "src/**",
        "--must-include",
        "src/owner.rs",
        "--must-include-symbol",
        "owner_symbol",
        "--max-fragments",
        "12",
        "--plan-only",
        "--focus",
        "src",
        "--strict-focus-paths",
        "--minimum-fragments-per-focus-path",
        "2",
        "--focus-symbol",
        "sym",
        "--exclude",
        "tests",
        "--known-hash",
        "abc",
        "--prior-generation",
        "7",
        "--strict-changed-paths",
        "--verbose-diagnostics",
        "--workflow",
        "contribution",
    ]);
    let AppRequest::Context {
        request,
        workflow,
        handoff,
        max_response_tokens,
    } = cli.app_request()
    else {
        panic!("expected context request");
    };
    assert!(handoff.is_none());
    assert_eq!(workflow, ContextWorkflow::Contribution);
    assert_eq!(max_response_tokens, Some(2048));
    assert_eq!(request.task, "fix the bug");
    assert_eq!(request.token_budget, 1024);
    assert_eq!(request.include_paths, vec!["src/**".to_string()]);
    assert_eq!(
        request.must_include_paths,
        vec!["src/owner.rs".to_string()]
    );
    assert_eq!(
        request.must_include_symbols,
        vec!["owner_symbol".to_string()]
    );
    assert_eq!(request.max_fragments, Some(12));
    assert!(request.plan_only);
    assert_eq!(request.focus_paths, vec!["src".to_string()]);
    assert!(request.strict_focus_paths);
    assert_eq!(request.minimum_fragments_per_focus_path, Some(2));
    assert_eq!(request.focus_symbols, vec!["sym".to_string()]);
    assert_eq!(request.exclude_paths, vec!["tests".to_string()]);
    assert_eq!(request.known_hashes, vec!["abc".to_string()]);
    assert_eq!(request.prior_repository_generation, Some(7));
    assert!(request.strict_changed_paths);
    assert!(request.verbose_diagnostics);
}

#[test]
fn cli_context_requires_task_and_defaults_budget() {
    let no_task = Cli::try_parse_from(["leantoken", "context", "--budget", "100"]);
    assert!(no_task.is_err());

    let no_budget = Cli::try_parse_from(["leantoken", "context", "--task", "x"]);
    assert!(no_budget.is_ok());
    let AppRequest::Context { request, .. } = no_budget.expect("default budget").app_request()
    else {
        panic!("expected context request");
    };
    assert_eq!(request.token_budget, 3_000);
}

#[test]
fn cli_context_maps_opt_in_handoff_summary() {
    let cli = parse(&[
        "context",
        "--task",
        "continue the implementation",
        "--handoff",
        "--handoff-summary",
        "bounded executor state",
    ]);
    let AppRequest::Context { handoff, .. } = cli.app_request() else {
        panic!("expected context request");
    };
    assert_eq!(
        handoff.expect("handoff").summary.as_deref(),
        Some("bounded executor state")
    );

    assert!(
        Cli::try_parse_from([
            "leantoken",
            "context",
            "--task",
            "continue",
            "--handoff-summary",
            "missing opt in",
        ])
        .is_err()
    );
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
        &[
            "leantoken",
            "context",
            "--task",
            "x",
            "--minimum-fragments-per-focus-path",
            "0",
        ],
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

    let cli = parse(&["savings"]);
    assert!(matches!(cli.app_request(), AppRequest::Savings));

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
    assert!(!request.allow_outdated);

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

    let cli = parse(&["setup", "--codex", "--yes", "--allow-outdated"]);
    let AppRequest::Setup(request) = cli.app_request() else {
        panic!("expected setup request");
    };
    assert!(request.allow_outdated);
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
fn cli_cache_list_resolves_without_repository_configuration() {
    let AppRequest::CacheListV2(default_list) = parse(&["cache", "list"]).app_request() else {
        panic!("expected cache list request");
    };
    assert!(!default_list.request.summary);
    assert!(default_list.request.states.is_empty());
    assert!(default_list.request.repository_root.is_none());
    assert_eq!(default_list.request.limit, DEFAULT_CACHE_LIST_LIMIT);
    assert!(default_list.request.cursor.is_none());
    assert!(default_list.compatibilities.is_empty());
    assert!(default_list.index_content_versions.is_empty());
    assert!(!default_list.incompatible_with_current);

    let AppRequest::CacheListV2(filtered_list) = parse(&[
        "cache",
        "list",
        "--state",
        "corrupt",
        "--state",
        "legacy",
        "--compatibility",
        "obsolete-older",
        "--index-content-version",
        "11",
        "--incompatible-with-current",
        "--repository-root",
        "repository",
        "--limit",
        "7",
        "--cursor",
        "opaque",
    ])
    .app_request()
    else {
        panic!("expected filtered cache list request");
    };
    assert_eq!(
        filtered_list.request.states,
        vec![CacheState::Corrupt, CacheState::Legacy]
    );
    assert_eq!(
        filtered_list.compatibilities,
        vec![CacheCompatibility::ObsoleteOlder]
    );
    assert_eq!(filtered_list.index_content_versions, vec![11]);
    assert!(filtered_list.incompatible_with_current);
    assert_eq!(
        filtered_list.request.repository_root.as_deref(),
        Some(std::path::Path::new("repository"))
    );
    assert_eq!(filtered_list.request.limit, 7);
    assert_eq!(filtered_list.request.cursor.as_deref(), Some("opaque"));

    let AppRequest::CacheListV2(summary) =
        parse(&["cache", "list", "--summary", "--state", "current"]).app_request()
    else {
        panic!("expected summary cache list request");
    };
    assert!(summary.request.summary);
    assert_eq!(summary.request.states, vec![CacheState::Current]);
    assert!(
        Cli::try_parse_from([
            "leantoken",
            "cache",
            "list",
            "--summary",
            "--cursor",
            "opaque",
        ])
        .is_err()
    );
    assert!(
        Cli::try_parse_from(["leantoken", "cache", "list", "--limit", "101"]).is_err()
    );
}

#[test]
fn cli_cache_prune_resolves_without_repository_configuration() {
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
    let AppRequest::CachePruneV2(request) = request else {
        panic!("expected cache prune request");
    };
    assert_eq!(request.request.older_than_days, Some(30));
    assert_eq!(request.request.max_total_bytes, Some(1_048_576));
    assert!(request.request.remove_missing_roots);
    assert!(request.request.dry_run);
    assert!(!request.request.yes);
    assert!(!request.incompatible_with_current);

    let AppRequest::CachePruneV2(zero_budget) =
        parse(&["cache", "prune", "--max-total-bytes", "0", "--dry-run"])
            .app_request()
    else {
        panic!("expected zero-budget cache prune request");
    };
    assert_eq!(zero_budget.request.max_total_bytes, Some(0));

    let AppRequest::CachePruneV2(incompatible) =
        parse(&["cache", "prune", "--incompatible-with-current"]).app_request()
    else {
        panic!("expected incompatible cache prune request");
    };
    assert!(incompatible.incompatible_with_current);
    assert!(incompatible.request.dry_run);
    assert!(!incompatible.request.yes);
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

#[test]
fn cli_index_worker_limit_is_explicit_and_positive() {
    let root = tempfile::tempdir().expect("root");
    let cli = parse(&[
        "status",
        "--root",
        root.path().to_str().expect("root UTF-8"),
        "--max-index-workers",
        "2",
    ]);
    assert_eq!(cli.config().expect("config").max_index_workers, 2);
    assert!(Cli::try_parse_from([
        "leantoken",
        "status",
        "--max-index-workers",
        "0",
    ])
    .is_err());
}
