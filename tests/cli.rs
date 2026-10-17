use clap::{CommandFactory, Parser, error::ErrorKind};
use leantoken::cli::{AppRequest, Cli};

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
fn cli_remove_help_snapshot() {
    insta::assert_snapshot!("remove_help", help(&["remove"]));
}

#[test]
fn cli_remove_rejects_setup_only_options() {
    for option in ["--refresh", "--private-runtime", "--allow-outdated"] {
        let error = Cli::try_parse_from(["leantoken", "remove", option])
            .expect_err("setup-only option must be rejected by remove");
        assert_eq!(error.kind(), ErrorKind::UnknownArgument, "{option}");
    }
}

#[test]
fn cli_cache_help_snapshot() {
    insta::assert_snapshot!("cache_help", help(&["cache"]));
}

#[test]
fn cli_cache_compact_is_explicit_and_previews_by_default() {
    assert!(Cli::try_parse_from(["leantoken", "cache", "compact"]).is_err());
    let AppRequest::CacheCompact(preview) =
        parse(&["cache", "compact", "--id", "v15-0000000000000001"]).app_request()
    else {
        panic!("expected compact request");
    };
    assert!(preview.dry_run);
    assert!(!preview.yes);
    assert_eq!(preview.min_reclaim_bytes, 64 * 1024 * 1024);
    assert_eq!(preview.min_reclaim_percent, 10);
    let AppRequest::CacheCompact(apply) = parse(&[
        "cache",
        "compact",
        "--id",
        "v15-0000000000000001",
        "--yes",
        "--max-seconds",
        "30",
    ])
    .app_request() else {
        panic!("expected compact request");
    };
    assert!(!apply.dry_run);
    assert!(apply.yes);
    assert_eq!(apply.max_seconds, 30);
    let AppRequest::CacheCompact(override_preview) = parse(&[
        "cache",
        "compact",
        "--id",
        "v15-0000000000000001",
        "--yes",
        "--dry-run",
    ])
    .app_request() else {
        panic!("expected compact request");
    };
    assert!(override_preview.dry_run);
}

#[test]
fn cli_cache_compact_help_snapshot() {
    insta::assert_snapshot!("cache_compact_help", help(&["cache", "compact"]));
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
        if argument.is_hide_set() || matches!(argument.get_id().as_str(), "help" | "version") {
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
        let AppRequest::History { request, .. } = parse(&args).app_request() else {
            panic!("expected history request");
        };
        assert_eq!(request.max_tokens, Some(500));
    }

    for args in [
        ["json", "--max-items", "50", "query", "report.json"],
        ["json", "query", "report.json", "--max-items", "50"],
    ] {
        let AppRequest::Json { request, .. } = parse(&args).app_request() else {
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
            vec!["leantoken", "outline", "src/lib.rs", "--max-results", value],
        ] {
            assert!(Cli::try_parse_from(args).is_ok(), "rejected {value}");
        }
    }

    for value in ["1", "32000", "32001"] {
        for args in [
            vec!["leantoken", "search", "x", "--max-tokens", value],
            vec!["leantoken", "outline", "src/lib.rs", "--max-tokens", value],
            vec!["leantoken", "read", "src/lib.rs", "--max-tokens", value],
            vec!["leantoken", "context", "--task", "x", "--budget", value],
        ] {
            assert!(Cli::try_parse_from(args).is_ok(), "rejected {value}");
        }
    }

    for value in ["0", "1", "20", "21"] {
        assert!(
            Cli::try_parse_from(["leantoken", "search", "x", "--context-lines", value,]).is_ok(),
            "CLI should defer context-lines={value} to Services"
        );
    }
    assert!(Cli::try_parse_from(["leantoken", "files", "tree", "--depth", "0"]).is_ok());
}

#[test]
fn cli_runtime_lifecycle_is_bounded_and_dry_run_by_default() {
    assert!(matches!(
        parse(&["runtime", "list"]).app_request(),
        AppRequest::RuntimeList
    ));

    let AppRequest::RuntimePrune(defaults) = parse(&["runtime", "prune"]).app_request() else {
        panic!("expected runtime prune request");
    };
    assert_eq!(defaults.keep_latest, 2);
    assert!(defaults.dry_run);
    assert!(!defaults.yes);

    let AppRequest::RuntimePrune(apply) =
        parse(&["runtime", "prune", "--keep-latest", "0", "--yes"]).app_request()
    else {
        panic!("expected runtime prune request");
    };
    assert_eq!(apply.keep_latest, 0);
    assert!(!apply.dry_run);
    assert!(apply.yes);
    assert!(Cli::try_parse_from(["leantoken", "runtime", "prune", "--keep-latest", "65"]).is_err());
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
