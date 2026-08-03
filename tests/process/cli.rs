use clap::Parser;

use super::support::{
    Command, EXPECTED_INDEX_CONTENT_VERSION, assert_cli_parse_error, database_state,
    leantoken_program_name, run, run_error,
};

pub(super) fn cli_indexes_statuses_and_searches_as_json() {
    let root = tempfile::tempdir().expect("temporary repository");
    std::fs::write(root.path().join("lib.rs"), "pub fn answer() -> u8 { 42 }\n")
        .expect("write fixture");
    let database = root.path().join("index.sqlite");

    let index = run(root.path(), &database, &["index"]);
    assert!(
        index["files_indexed"]
            .as_u64()
            .is_some_and(|value| value >= 1)
    );

    let status = run(root.path(), &database, &["status"]);
    assert_eq!(status["file_count"], 1);
    assert_eq!(
        status["index_content_version"],
        EXPECTED_INDEX_CONTENT_VERSION
    );
    assert_eq!(
        status["indexed_source_bytes"],
        "pub fn answer() -> u8 { 42 }\n".len()
    );
    assert!(
        status["index_storage_bytes"]
            .as_u64()
            .is_some_and(|bytes| bytes > 0)
    );
    assert!(
        status["index_amplification_ratio"]
            .as_f64()
            .is_some_and(|ratio| ratio > 1.0)
    );

    let search = run(
        root.path(),
        &database,
        &[
            "search",
            "answer",
            "--mode",
            "identifier",
            "--max-tokens",
            "100",
        ],
    );
    assert_eq!(search["hits"][0]["path"], "lib.rs");
    assert!(
        search["meta"]["source_tokens"]
            .as_u64()
            .is_some_and(|value| value <= 100)
    );

    let savings = run(root.path(), &database, &["savings"]);
    assert_eq!(savings["response_accounting"]["tracked_requests"], 1);
    let search_accounting = savings["response_accounting"]["by_operation"]
        .as_array()
        .and_then(|operations| {
            operations
                .iter()
                .find(|operation| operation["operation"] == "search")
        })
        .expect("search accounting");
    assert_eq!(search_accounting["tracked_requests"], 1);
    assert!(
        savings["response_accounting"]["estimated_net_tokens_saved"]
            .as_i64()
            .is_some()
    );
    assert_eq!(savings["window"], "lifetime");
    let snapshot = savings["snapshot"]
        .as_str()
        .expect("opaque savings snapshot")
        .to_owned();
    run(
        root.path(),
        &database,
        &["search", "answer", "--mode", "identifier"],
    );
    let delta = run(
        root.path(),
        &database,
        &["savings", "--snapshot", &snapshot],
    );
    assert_eq!(delta["window"], "delta");
    assert_eq!(delta["response_accounting"]["tracked_requests"], 1);
    assert_eq!(delta["observations"]["request_classification"]["useful"], 1);
}

pub(super) fn cli_scoped_index_omits_dependencies_and_discloses_the_boundary() {
    let root = tempfile::tempdir().expect("temporary repository");
    std::fs::create_dir(root.path().join("src")).expect("source directory");
    std::fs::create_dir(root.path().join("third_party")).expect("dependency directory");
    std::fs::write(
        root.path().join("src/lib.rs"),
        "pub fn selected_scope_target() {}\n",
    )
    .expect("write selected fixture");
    std::fs::write(
        root.path().join("third_party/lib.rs"),
        "pub fn dependency_scope_target() {}\n",
    )
    .expect("write dependency fixture");
    let database = root.path().join("scoped.sqlite");
    let scope_args = [
        "--index-include",
        "src/**",
        "--index-exclude",
        "third_party/**",
    ];

    let index = run(
        root.path(),
        &database,
        &[
            scope_args[0],
            scope_args[1],
            scope_args[2],
            scope_args[3],
            "index",
        ],
    );
    assert_eq!(index["files_seen"], 1);
    assert_eq!(index["files_indexed"], 1);

    let status = run(
        root.path(),
        &database,
        &[
            scope_args[0],
            scope_args[1],
            scope_args[2],
            scope_args[3],
            "status",
        ],
    );
    assert_eq!(status["index_scope"], "scoped");
    assert_eq!(status["index_include_paths"], serde_json::json!(["src/**"]));
    assert_eq!(
        status["index_exclude_paths"],
        serde_json::json!(["third_party/**"])
    );
    assert_eq!(status["file_count"], 1);

    let absent = run(
        root.path(),
        &database,
        &[
            scope_args[0],
            scope_args[1],
            scope_args[2],
            scope_args[3],
            "search",
            "dependency_scope_target",
            "--mode",
            "identifier",
        ],
    );
    assert!(absent["hits"].as_array().is_some_and(Vec::is_empty));
    assert_eq!(absent["meta"]["index_scope"], "scoped");
    assert_eq!(
        absent["meta"]["index_scope_digest"],
        status["index_scope_digest"]
    );
}

pub(super) fn cli_retrieval_reconciles_live_changes_unless_snapshot_consistency_is_requested() {
    let root = tempfile::tempdir().expect("temporary repository");
    let source = root.path().join("lib.rs");
    std::fs::write(&source, "pub fn answer() -> u8 { 41 }\n").expect("write fixture");
    let database = root.path().join("index.sqlite");

    run(root.path(), &database, &["index"]);
    std::fs::write(&source, "pub fn answer() -> u8 { 43 }\n").expect("edit fixture");

    let reconciled = run(root.path(), &database, &["search", "43", "--mode", "text"]);
    assert_eq!(reconciled["hits"][0]["path"], "lib.rs");
    assert_eq!(reconciled["meta"]["repository_generation"], 2);

    std::fs::write(&source, "pub fn answer() -> u8 { 47 }\n").expect("edit fixture again");
    let snapshot = run(
        root.path(),
        &database,
        &[
            "search",
            "43",
            "--mode",
            "text",
            "--consistency",
            "indexed_generation",
        ],
    );
    assert_eq!(snapshot["hits"][0]["path"], "lib.rs");
    assert_eq!(snapshot["meta"]["repository_generation"], 2);

    let status = run(root.path(), &database, &["status"]);
    assert_eq!(status["working_tree_checked"], false);
}

pub(super) fn cli_savings_renders_a_color_aware_human_table() {
    let root = tempfile::tempdir().expect("temporary repository");
    std::fs::write(root.path().join("lib.rs"), "pub fn answer() -> u8 { 42 }\n")
        .expect("write fixture");
    let database = root.path().join("index.sqlite");
    run(root.path(), &database, &["index"]);
    run(
        root.path(),
        &database,
        &[
            "search",
            "answer",
            "--mode",
            "identifier",
            "--max-tokens",
            "100",
        ],
    );

    let command = || {
        let mut command = Command::cargo_bin("leantoken").expect("binary");
        command.args([
            "--root",
            root.path().to_str().expect("root UTF-8"),
            "--database",
            database.to_str().expect("database UTF-8"),
            "savings",
        ]);
        command
    };

    let plain = command()
        .env("NO_COLOR", "1")
        .output()
        .expect("plain savings report");
    assert!(plain.status.success());
    let plain = String::from_utf8(plain.stdout).expect("plain UTF-8");
    assert!(
        plain.starts_with(
            "LeanToken Observed Token Accounting\n===================================\n"
        )
    );
    assert!(plain.contains("response tokens"));
    assert!(plain.contains("Persisted observations"));
    assert!(plain.contains("Request classes:"));
    assert!(plain.contains("Unobserved task outcomes"));
    assert!(plain.contains("Operation"));
    assert!(plain.contains("Search"));
    assert!(plain.contains("response delta"));
    assert!(plain.contains("Window: lifetime"));
    assert!(plain.contains("Snapshot: lts1."));
    assert!(!plain.contains("\x1b["));

    let colored = command()
        .env_remove("NO_COLOR")
        .env("CLICOLOR_FORCE", "1")
        .output()
        .expect("colored savings report");
    assert!(colored.status.success());
    assert!(
        String::from_utf8(colored.stdout)
            .expect("colored UTF-8")
            .contains("\x1b[1;36mLeanToken Observed Token Accounting\x1b[0m")
    );

    let no_color = command()
        .env("CLICOLOR_FORCE", "1")
        .env("NO_COLOR", "1")
        .output()
        .expect("NO_COLOR savings report");
    assert!(no_color.status.success());
    assert!(
        !String::from_utf8(no_color.stdout)
            .expect("NO_COLOR UTF-8")
            .contains("\x1b[")
    );
}

pub(super) fn cli_index_explains_skipped_binary_files_without_returning_paths() {
    let root = tempfile::tempdir().expect("temporary repository");
    std::fs::write(root.path().join("lib.rs"), "pub fn answer() -> u8 { 42 }\n")
        .expect("write text fixture");
    let binary_path = root.path().join("secret-binary.rs");
    std::fs::write(&binary_path, b"\0binary").expect("write binary fixture");
    let database = root.path().join("index.sqlite");

    let response = run(root.path(), &database, &["index"]);

    assert_eq!(response["files_seen"], 2);
    assert_eq!(response["files_indexed"], 1);
    assert_eq!(response["files_skipped"], 1);
    assert_eq!(
        response["skip_reasons"],
        serde_json::json!({
            "binary": 1,
            "oversized_during_read": 0,
            "failed": 0
        })
    );
    assert_eq!(response["warnings"], serde_json::json!([]));
    assert!(!response.to_string().contains("secret-binary.rs"));
}

pub(super) fn cli_files_tree_treats_dot_as_the_repository_root() {
    let root = tempfile::tempdir().expect("temporary repository");
    std::fs::create_dir(root.path().join("src")).expect("src directory");
    std::fs::write(root.path().join("README.md"), "fixture\n").expect("readme");
    std::fs::write(
        root.path().join("src/lib.rs"),
        "pub fn answer() -> u8 { 42 }\n",
    )
    .expect("source");
    let database = root.path().join("index.sqlite");
    run(root.path(), &database, &["index"]);

    let omitted = run(
        root.path(),
        &database,
        &["files", "tree", "--depth", "2", "--max-results", "2"],
    );
    let dotted = run(
        root.path(),
        &database,
        &[
            "files",
            "tree",
            "--path",
            ".",
            "--depth",
            "2",
            "--max-results",
            "2",
        ],
    );

    assert_eq!(dotted, omitted);
}

pub(super) fn cold_cli_status_and_retrieval_explain_index_readiness() {
    let root = tempfile::tempdir().expect("temporary repository");
    std::fs::write(root.path().join("lib.rs"), "fn pending() {}\n").expect("source");
    let database = root.path().join("index.sqlite");

    let status = run(root.path(), &database, &["status"]);
    assert_eq!(status["repository_generation"], 0);
    assert_eq!(status["index_state"], "uninitialized");
    assert_eq!(status["freshness"], "current");

    let guidance = "repository index is not ready; run `leantoken index` for direct CLI use \
        or `leantoken doctor` to verify MCP readiness";
    let human = Command::cargo_bin("leantoken")
        .expect("binary")
        .args([
            "--root",
            root.path().to_str().expect("root UTF-8"),
            "--database",
            database.to_str().expect("database UTF-8"),
            "files",
            "tree",
        ])
        .output()
        .expect("run human retrieval");
    assert!(!human.status.success());
    assert_eq!(
        String::from_utf8(human.stderr)
            .expect("UTF-8 stderr")
            .trim(),
        format!("Error: {guidance}")
    );

    let json = Command::cargo_bin("leantoken")
        .expect("binary")
        .args([
            "--root",
            root.path().to_str().expect("root UTF-8"),
            "--database",
            database.to_str().expect("database UTF-8"),
            "--json",
            "files",
            "tree",
        ])
        .output()
        .expect("run JSON retrieval");
    assert!(!json.status.success());
    let error: serde_json::Value = serde_json::from_slice(&json.stderr).expect("structured error");
    assert_eq!(
        error,
        serde_json::json!({
            "error": guidance,
            "category": "index_not_ready"
        })
    );
}

pub(super) fn cli_json_errors_expose_stable_safe_metadata() {
    let root = tempfile::tempdir().expect("temporary repository");
    std::fs::write(root.path().join("lib.rs"), "fn indexed() {}\n").expect("source");
    let database = root.path().join("index.sqlite");
    run(root.path(), &database, &["index"]);

    assert_eq!(
        run_error(root.path(), &database, &["files", "find"]),
        serde_json::json!({
            "error": "invalid query: is required for find",
            "category": "invalid_input",
            "field": "query"
        })
    );
    assert_eq!(
        run_error(
            root.path(),
            &database,
            &["files", "tree", "--max-results", "101"],
        ),
        serde_json::json!({
            "error": "max_results exceeds its configured limit: requested 101, limit 100",
            "category": "request_limit_exceeded",
            "field": "max_results",
            "requested": 101,
            "limit": 100
        })
    );
    assert_eq!(
        run_error(root.path(), &database, &["read", "missing.rs"]),
        serde_json::json!({
            "error": "path is not indexed: missing.rs",
            "category": "not_indexed"
        })
    );
    assert_eq!(
        run_error(
            root.path(),
            &database,
            &["files", "tree", "--cursor", "malformed"],
        ),
        serde_json::json!({
            "error": "stale cursor",
            "category": "stale_cursor"
        })
    );

    let database_directory = root.path().join("database-directory");
    std::fs::create_dir(&database_directory).expect("database directory");
    let internal = run_error(root.path(), &database_directory, &["status"]);
    assert_eq!(internal["category"], "internal_error");
    assert!(
        internal["error"]
            .as_str()
            .is_some_and(|message| message.starts_with("SQLite error:"))
    );
    assert_eq!(internal.as_object().map(serde_json::Map::len), Some(2));
}

pub(super) fn cli_json_parse_errors_are_structured_without_changing_clap_help() {
    assert_cli_parse_error(&["files", "tree", "--max-results", "nope", "--json"]);
    assert_cli_parse_error(&["--json", "--unknown"]);

    let human_arguments = ["files", "tree", "--max-results", "nope"];
    let expected = leantoken::cli::Cli::try_parse_from(
        std::iter::once(leantoken_program_name())
            .chain(human_arguments.into_iter().map(std::ffi::OsString::from)),
    )
    .expect_err("invalid numeric argument")
    .to_string();
    let human = Command::cargo_bin("leantoken")
        .expect("binary")
        .args(human_arguments)
        .output()
        .expect("run human parse failure");
    assert_eq!(human.status.code(), Some(2));
    assert!(human.stdout.is_empty());
    assert_eq!(human.stderr, expected.as_bytes());

    let help = Command::cargo_bin("leantoken")
        .expect("binary")
        .args(["--json", "--help"])
        .output()
        .expect("run JSON help");
    assert!(help.status.success());
    assert!(help.stderr.is_empty());
    assert!(String::from_utf8_lossy(&help.stdout).contains("Usage: leantoken"));
}

pub(super) fn cli_index_limit_error_is_structured_and_does_not_publish_partial_files() {
    let root = tempfile::tempdir().expect("temporary repository");
    std::fs::write(root.path().join("a.rs"), "fn a() {}\n").expect("a");
    std::fs::write(root.path().join("b.rs"), "fn b() {}\n").expect("b");
    let database = root.path().join("index.sqlite");

    let output = Command::cargo_bin("leantoken")
        .expect("binary")
        .args([
            "--root",
            root.path().to_str().expect("root UTF-8"),
            "--database",
            database.to_str().expect("database UTF-8"),
            "--max-files",
            "1",
            "--json",
            "index",
        ])
        .output()
        .expect("run index");

    assert!(!output.status.success());
    let error: serde_json::Value =
        serde_json::from_slice(&output.stderr).expect("structured error");
    assert_eq!(
        error["error"],
        "index source files limit exceeded: observed 2, limit 1"
    );
    assert_eq!(error["category"], "repository_index_limit");
    assert_eq!(database_state(&database).map(|state| state.0), Some(0));
    assert_eq!(database_state(&database).map(|state| state.1), Some(0));
}
