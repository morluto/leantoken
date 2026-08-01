use std::{
    io::{BufRead, BufReader, Read, Write},
    process::{Child, ChildStdin, Stdio},
    sync::mpsc,
    time::{Duration, Instant},
};

use assert_cmd::Command;
use clap::Parser;
use wait_timeout::ChildExt;

fn assert_runtime_version(value: &serde_json::Value) {
    let version = value.as_str().expect("runtime version string");
    let fingerprint = version
        .strip_prefix(concat!(env!("CARGO_PKG_VERSION"), "+contract."))
        .expect("runtime version carries the current package version and contract fingerprint");
    assert_eq!(fingerprint.len(), 32);
    assert!(fingerprint.bytes().all(|byte| byte.is_ascii_hexdigit()));
}

#[test]
fn cli_indexes_statuses_and_searches_as_json() {
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
    assert_eq!(
        delta["observations"]["request_classification"]["useful"],
        1
    );
}

#[test]
fn cli_scoped_index_omits_dependencies_and_discloses_the_boundary() {
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

#[test]
fn cli_retrieval_reconciles_live_changes_unless_snapshot_consistency_is_requested() {
    let root = tempfile::tempdir().expect("temporary repository");
    let source = root.path().join("lib.rs");
    std::fs::write(&source, "pub fn answer() -> u8 { 41 }\n").expect("write fixture");
    let database = root.path().join("index.sqlite");

    run(root.path(), &database, &["index"]);
    std::fs::write(&source, "pub fn answer() -> u8 { 43 }\n").expect("edit fixture");

    let reconciled = run(
        root.path(),
        &database,
        &["search", "43", "--mode", "text"],
    );
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

#[test]
fn cli_savings_renders_a_color_aware_human_table() {
    let root = tempfile::tempdir().expect("temporary repository");
    std::fs::write(
        root.path().join("lib.rs"),
        "pub fn answer() -> u8 { 42 }\n",
    )
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
    assert!(plain.starts_with(
        "LeanToken Observed Token Accounting\n===================================\n"
    ));
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
    assert!(String::from_utf8(colored.stdout)
        .expect("colored UTF-8")
        .contains("\x1b[1;36mLeanToken Observed Token Accounting\x1b[0m"));

    let no_color = command()
        .env("CLICOLOR_FORCE", "1")
        .env("NO_COLOR", "1")
        .output()
        .expect("NO_COLOR savings report");
    assert!(no_color.status.success());
    assert!(!String::from_utf8(no_color.stdout)
        .expect("NO_COLOR UTF-8")
        .contains("\x1b["));
}

#[test]
fn cli_index_explains_skipped_binary_files_without_returning_paths() {
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

#[test]
fn cli_files_tree_treats_dot_as_the_repository_root() {
    let root = tempfile::tempdir().expect("temporary repository");
    std::fs::create_dir(root.path().join("src")).expect("src directory");
    std::fs::write(root.path().join("README.md"), "fixture\n").expect("readme");
    std::fs::write(root.path().join("src/lib.rs"), "pub fn answer() -> u8 { 42 }\n")
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

#[test]
fn cold_cli_status_and_retrieval_explain_index_readiness() {
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
    let error: serde_json::Value =
        serde_json::from_slice(&json.stderr).expect("structured error");
    assert_eq!(
        error,
        serde_json::json!({
            "error": guidance,
            "category": "index_not_ready"
        })
    );
}

#[test]
fn cli_json_errors_expose_stable_safe_metadata() {
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

#[test]
fn cli_json_parse_errors_are_structured_without_changing_clap_help() {
    assert_cli_parse_error(&[
        "files",
        "tree",
        "--max-results",
        "nope",
        "--json",
    ]);
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

#[test]
fn cli_index_limit_error_is_structured_and_does_not_publish_partial_files() {
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

const EXPECTED_INDEX_CONTENT_VERSION: u64 = 13;

#[test]
fn doctor_verifies_identity_catalog_and_first_retrieval() {
    let root = tempfile::tempdir().expect("temporary repository");
    std::fs::write(
        root.path().join("lib.rs"),
        "pub fn context_distillery_ready() -> bool { true }\n",
    )
    .expect("write fixture");
    let database = root.path().join("index.sqlite");

    let report = run(root.path(), &database, &["doctor"]);
    assert_eq!(report["status"], "ready");
    assert_eq!(report["server_name"], "leantoken");
    assert_runtime_version(&report["server_version"]);
    assert_eq!(
        report["index_content_version"],
        EXPECTED_INDEX_CONTENT_VERSION
    );
    assert_eq!(report["instructions_loaded"], true);
    assert_eq!(report["tools"].as_array().map(Vec::len), Some(9));
    assert_eq!(report["result_mode"], "structured");
    assert!(
        matches!(
            report["integration"]["registration_status"].as_str(),
            Some("registered" | "not_registered" | "unknown")
        ),
        "structured registration status: {report}"
    );
    assert!(
        matches!(
            report["integration"]["discovery_status"].as_str(),
            Some("installed" | "partial" | "missing" | "unknown")
        ),
        "structured discovery status: {report}"
    );
    assert_eq!(report["integration"]["launcher_status"], "healthy");
    assert_eq!(report["integration"]["handshake_status"], "healthy");
    assert_eq!(report["integration"]["catalog_status"], "healthy");
    assert!(
        report["integration"]["repair_command"]
            .as_str()
            .is_some_and(|command| command.contains("leantoken"))
    );
    assert_eq!(report["first_call"]["status"], "ready");
    assert!(
        report["first_call"]["attempts"]
            .as_u64()
            .is_some_and(|attempts| attempts >= 1)
    );
}

#[test]
fn doctor_can_exercise_the_exact_codex_registration() {
    let home = tempfile::tempdir().expect("temporary home");
    let root = tempfile::tempdir().expect("temporary repository");
    std::fs::write(root.path().join("lib.rs"), "fn configured_doctor_ready() {}\n")
        .expect("write fixture");
    let database = root.path().join("index.sqlite");
    let setup = Command::cargo_bin("leantoken")
        .expect("binary")
        .env("HOME", home.path())
        .env("USERPROFILE", home.path())
        .env_remove("npm_lifecycle_event")
        .args(["--json", "setup", "--codex", "--yes"])
        .output()
        .expect("configure Codex");
    assert!(
        setup.status.success(),
        "setup stderr: {}",
        String::from_utf8_lossy(&setup.stderr)
    );

    let output = Command::cargo_bin("leantoken")
        .expect("binary")
        .env("HOME", home.path())
        .env("USERPROFILE", home.path())
        .env_remove("npm_lifecycle_event")
        .args([
            "--root",
            root.path().to_str().expect("root UTF-8"),
            "--database",
            database.to_str().expect("database UTF-8"),
            "--json",
            "doctor",
            "--client",
            "codex",
        ])
        .output()
        .expect("run configured-client doctor");
    assert!(
        output.status.success(),
        "doctor stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("doctor report");
    assert_eq!(report["integration"]["verified_client"], "codex");
    let codex = report["integration"]["registrations"]
        .as_array()
        .and_then(|registrations| {
            registrations
                .iter()
                .find(|registration| registration["client"] == "codex")
        })
        .expect("Codex registration");
    assert_eq!(codex["managed"], true);
    assert_eq!(report["first_call"]["status"], "ready");
}

#[test]
fn doctor_surfaces_bounded_redacted_child_diagnostics() {
    let root = tempfile::tempdir().expect("temporary repository");
    std::fs::write(root.path().join("lib.rs"), "fn ready() {}\n").expect("write fixture");
    let blocked_parent = root.path().join("blocked");
    std::fs::write(&blocked_parent, "not a directory").expect("blocked parent");
    let database = blocked_parent.join("index.sqlite");

    let output = Command::cargo_bin("leantoken")
        .expect("binary")
        .args([
            "--root",
            root.path().to_str().expect("root UTF-8"),
            "--database",
            database.to_str().expect("database UTF-8"),
            "--json",
            "doctor",
        ])
        .output()
        .expect("run doctor");

    assert!(!output.status.success());
    let error: serde_json::Value =
        serde_json::from_slice(&output.stderr).expect("structured error");
    let message = error["error"].as_str().expect("error message");
    assert_eq!(error["category"], "doctor_failure");
    assert!(message.contains("child diagnostics:"), "{message}");
    assert!(message.contains("MCP indexing runtime failed"), "{message}");
    assert!(!message.contains(database.to_str().unwrap()), "{message}");
    assert!(message.len() < 5_000, "diagnostic must remain bounded");
}

#[test]
fn doctor_human_output_uses_context_distillery_handoff() {
    let root = tempfile::tempdir().expect("temporary repository");
    std::fs::write(root.path().join("lib.rs"), "fn ready() {}\n").expect("write fixture");
    let database = root.path().join("index.sqlite");
    let output = Command::cargo_bin("leantoken")
        .expect("binary")
        .args([
            "--root",
            root.path().to_str().expect("root UTF-8"),
            "--database",
            database.to_str().expect("database UTF-8"),
            "doctor",
        ])
        .output()
        .expect("run doctor");
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("Context Distillery is checking"));
    assert!(stdout.contains("LeanToken // Context Distillery"));
    assert!(stdout.contains("MCP identity: leantoken"));
    assert!(stdout.contains(&format!(
        "Index compatibility: v{EXPECTED_INDEX_CONTENT_VERSION}"
    )));
    assert!(stdout.contains("Tool catalog: 9 MCP tools"));
    assert!(stdout.contains("leantoken.context first"));
}

#[test]
fn mcp_repeatedly_exits_cleanly_on_stdio_eof() {
    let root = tempfile::tempdir().expect("temporary repository");
    std::fs::write(root.path().join("lib.rs"), "pub fn answer() -> u8 { 42 }\n")
        .expect("write fixture");
    let database = root.path().join("index.sqlite");

    for _ in 0..3 {
        Command::cargo_bin("leantoken")
            .expect("binary")
            .args([
                "--root",
                root.path().to_str().expect("root UTF-8"),
                "--database",
                database.to_str().expect("database UTF-8"),
                "mcp",
            ])
            .write_stdin("")
            // The deadline covers cold indexing and watcher startup as well as
            // transport shutdown, which is materially slower on Windows runners.
            .timeout(std::time::Duration::from_secs(30))
            .assert()
            .success();
    }
}

#[test]
fn mcp_survives_malformed_and_invalid_messages() {
    let root = tempfile::tempdir().expect("temporary repository");
    std::fs::write(root.path().join("lib.rs"), "pub fn answer() -> u8 { 42 }\n")
        .expect("write fixture");
    let database = root.path().join("index.sqlite");
    let mut process = McpProcess::spawn(root.path(), &database);
    process.initialize();
    process.send_initialized();

    // Oversized terminated and initially unterminated frames are discarded
    // without closing the transport. rmcp intentionally ignores unparsable input, but a well-formed value
    // with the wrong JSON-RPC shape receives Invalid Request. Neither may
    // close the stdio transport or poison the next tool call.
    process.send_raw(&vec![b'x'; 4 * 1024 * 1024 + 1]);
    process.send_raw_line("");
    process.send_raw_line("{not json");
    process.send_raw_line(r#"{"foo":"bar"}"#);
    // Keep this independent from host load: the process may still be finishing
    // watcher/index work while rmcp drains the malformed input.
    let invalid = process.message(Duration::from_secs(10));
    assert_eq!(invalid["error"]["code"], -32600);

    process.send(serde_json::json!({
        "jsonrpc": "2.0",
        "id": 100,
        "method": "tools/call",
        "params": {
            "name": "files",
            "arguments": { "operation": {"kind": "tree"}, "max_results": 1 }
        }
    }));
    let response = process.response(Duration::from_secs(10));
    assert_eq!(response["id"], 100);
    assert!(response.get("result").is_some(), "{response}");
    assert!(process.child.try_wait().expect("poll process").is_none());
}

#[test]
fn mcp_result_modes_project_exact_wire_shapes() {
    let root = tempfile::tempdir().expect("temporary repository");
    std::fs::write(root.path().join("lib.rs"), "pub fn answer() -> u8 { 42 }\n")
        .expect("write fixture");
    let database = root.path().join("index.sqlite");

    for (requested, client_name, client_version, protocol, text, structured) in [
        ("dual", "leantoken-test", "1", "2025-11-25", true, true),
        ("text", "leantoken-test", "1", "2025-11-25", true, false),
        (
            "structured",
            "leantoken-test",
            "1",
            "2025-11-25",
            false,
            true,
        ),
    ] {
        let mut process = McpProcess::spawn_with_mcp_args(
            root.path(),
            &database,
            &["--result-mode", requested],
        );
        process.initialize_as(client_name, client_version, protocol);
        process.send_initialized();
        process.wait_until_ready(Duration::from_secs(30));
        process.send(serde_json::json!({
            "jsonrpc": "2.0",
            "id": 900,
            "method": "tools/call",
            "params": {
                "name": "files",
                "arguments": {
                    "operation": {"kind": "tree"},
                    "max_results": 1
                }
            }
        }));
        let response = process.response(Duration::from_secs(10));
        let result = &response["result"];
        assert_eq!(
            result["content"]
                .as_array()
                .is_some_and(|content| !content.is_empty()),
            text,
            "{requested} {client_name} {client_version}: {result}"
        );
        assert_eq!(
            result.get("structuredContent").is_some(),
            structured,
            "{requested} {client_name} {client_version}: {result}"
        );
        process.stop();
    }
}

#[test]
fn mcp_receipt_created_by_one_process_is_reused_by_another() {
    let root = tempfile::tempdir().expect("temporary repository");
    std::fs::write(
        root.path().join("lib.rs"),
        "pub fn persistent_receipt_answer() -> u8 { 42 }\n",
    )
    .expect("write fixture");
    let database = root.path().join("index.sqlite");

    let mut first = McpProcess::spawn(root.path(), &database);
    first.initialize();
    first.send_initialized();
    first.wait_until_ready(Duration::from_secs(30));
    first.send(serde_json::json!({
        "jsonrpc": "2.0",
        "id": 901,
        "method": "tools/call",
        "params": {
            "name": "search",
            "arguments": {
                "query": "persistent_receipt_answer",
                "mode": "identifier",
                "max_results": 5,
                "max_tokens": 1_000
            }
        }
    }));
    let first_response = first.response(Duration::from_secs(10));
    let first_result = &first_response["result"]["structuredContent"];
    assert!(
        first_result["hits"]
            .as_array()
            .is_some_and(|hits| !hits.is_empty()),
        "{first_response}"
    );
    let receipt_id = first_result["meta"]["receipt_id"]
        .as_str()
        .expect("receipt id")
        .to_owned();
    first.stop();

    let mut second = McpProcess::spawn(root.path(), &database);
    second.initialize();
    second.send_initialized();
    second.wait_until_ready(Duration::from_secs(30));
    second.send(serde_json::json!({
        "jsonrpc": "2.0",
        "id": 902,
        "method": "tools/call",
        "params": {
            "name": "search",
            "arguments": {
                "query": "persistent_receipt_answer",
                "mode": "identifier",
                "max_results": 5,
                "max_tokens": 1_000,
                "receipt_id": receipt_id
            }
        }
    }));
    let second_response = second.response(Duration::from_secs(10));
    let second_result = &second_response["result"]["structuredContent"];
    assert!(
        second_result["hits"]
            .as_array()
            .is_some_and(Vec::is_empty),
        "{second_response}"
    );
    assert!(
        second_result["meta"]["receipt_suppressed_exact"]
            .as_u64()
            .unwrap_or_default()
            + second_result["meta"]["receipt_suppressed_overlap"]
                .as_u64()
                .unwrap_or_default()
            > 0,
        "{second_response}"
    );
    assert_eq!(second_result["meta"]["receipt_id"], receipt_id);
}

#[test]
fn mcp_query_receipt_created_by_one_process_is_reused_by_another() {
    let root = tempfile::tempdir().expect("temporary repository");
    std::fs::write(
        root.path().join("lib.rs"),
        "pub fn persistent_query_receipt_answer() -> u8 { 42 }\n",
    )
    .expect("write fixture");
    let database = root.path().join("index.sqlite");

    let mut first = McpProcess::spawn(root.path(), &database);
    first.initialize();
    first.send_initialized();
    first.wait_until_ready(Duration::from_secs(30));
    first.send(serde_json::json!({
        "jsonrpc": "2.0",
        "id": 903,
        "method": "tools/call",
        "params": {
            "name": "search",
            "arguments": {
                "query": "persistent_query_receipt_answer",
                "mode": "text",
                "all_occurrences": true,
                "coordinates_only": true,
                "max_results": 100,
                "max_tokens": 10_000,
                "query_receipt": {"kind": "record"}
            }
        }
    }));
    let first_response = first.response(Duration::from_secs(10));
    let first_result = &first_response["result"]["structuredContent"];
    assert_eq!(
        first_result["query_receipt"]["status"], "recorded",
        "{first_response}"
    );
    let receipt_id = first_result["query_receipt"]["receipt_id"]
        .as_str()
        .expect("query receipt id")
        .to_owned();
    first.stop();

    let mut second = McpProcess::spawn(root.path(), &database);
    second.initialize();
    second.send_initialized();
    second.wait_until_ready(Duration::from_secs(30));
    second.send(serde_json::json!({
        "jsonrpc": "2.0",
        "id": 904,
        "method": "tools/call",
        "params": {
            "name": "search",
            "arguments": {
                "query": "persistent_query_receipt_answer",
                "mode": "text",
                "all_occurrences": true,
                "coordinates_only": true,
                "max_results": 100,
                "max_tokens": 10_000,
                "query_receipt": {
                    "kind": "reuse",
                    "receipt_id": receipt_id
                }
            }
        }
    }));
    let second_response = second.response(Duration::from_secs(10));
    let second_result = &second_response["result"]["structuredContent"];
    assert_eq!(
        second_result["query_receipt"]["status"], "already_covered",
        "{second_response}"
    );
    assert_eq!(second_result["groups"], serde_json::json!([]));
    assert_eq!(second_result["occurrences_returned"], 0);
    assert_eq!(second_result["occurrences_total"], 1);
}

#[test]
fn mcp_receipt_rebase_is_cross_process_and_exact_only() {
    let root = tempfile::tempdir().expect("temporary repository");
    std::fs::write(
        root.path().join("lib.rs"),
        "pub fn cross_process_rebase_answer() -> u8 { 42 }\n",
    )
    .expect("write fixture");
    let database = root.path().join("index.sqlite");

    let mut first = McpProcess::spawn(root.path(), &database);
    first.initialize();
    first.send_initialized();
    first.wait_until_ready(Duration::from_secs(30));
    first.send(serde_json::json!({
        "jsonrpc": "2.0",
        "id": 903,
        "method": "tools/call",
        "params": {
            "name": "search",
            "arguments": {
                "query": "cross_process_rebase_answer",
                "mode": "identifier",
                "max_results": 5,
                "max_tokens": 1_000
            }
        }
    }));
    let first_response = first.response(Duration::from_secs(10));
    let source_receipt = first_response["result"]["structuredContent"]["meta"]["receipt_id"]
        .as_str()
        .expect("source receipt")
        .to_owned();
    let source_generation =
        first_response["result"]["structuredContent"]["meta"]["repository_generation"]
            .as_u64()
            .expect("source generation");
    first.stop();

    std::fs::write(root.path().join("unrelated.rs"), "fn unrelated() {}\n")
        .expect("write unrelated source");
    let mut second = McpProcess::spawn(root.path(), &database);
    second.initialize();
    second.send_initialized();
    second.wait_until_ready(Duration::from_secs(30));
    second.send(serde_json::json!({
        "jsonrpc": "2.0",
        "id": 904,
        "method": "tools/call",
        "params": {
            "name": "receipt_rebase",
            "arguments": {
                "receipt_id": source_receipt,
                "consistency": "reconcile_working_tree",
                "max_samples_per_outcome": 4
            }
        }
    }));
    let second_response = second.response(Duration::from_secs(10));
    let rebased = &second_response["result"]["structuredContent"];
    assert_eq!(rebased["counts"]["carried"], 1, "{second_response}");
    assert_eq!(rebased["counts"]["changed"], 0, "{second_response}");
    assert!(
        rebased["meta"]["repository_generation"]
            .as_u64()
            .is_some_and(|generation| generation > source_generation),
        "{second_response}"
    );
    let rebased_receipt = rebased["meta"]["receipt_id"]
        .as_str()
        .expect("rebased receipt")
        .to_owned();
    second.stop();

    let mut third = McpProcess::spawn(root.path(), &database);
    third.initialize();
    third.send_initialized();
    third.wait_until_ready(Duration::from_secs(30));
    third.send(serde_json::json!({
        "jsonrpc": "2.0",
        "id": 905,
        "method": "tools/call",
        "params": {
            "name": "search",
            "arguments": {
                "query": "cross_process_rebase_answer",
                "mode": "identifier",
                "max_results": 5,
                "max_tokens": 1_000,
                "receipt_id": rebased_receipt
            }
        }
    }));
    let third_response = third.response(Duration::from_secs(10));
    let third_result = &third_response["result"]["structuredContent"];
    assert!(
        third_result["hits"]
            .as_array()
            .is_some_and(Vec::is_empty),
        "{third_response}"
    );
    assert!(
        third_result["meta"]["receipt_suppressed_exact"]
            .as_u64()
            .unwrap_or_default()
            + third_result["meta"]["receipt_suppressed_overlap"]
                .as_u64()
                .unwrap_or_default()
            > 0,
        "{third_response}"
    );
}

#[test]
fn mcp_initialize_precedes_storage_open() {
    let root = tempfile::tempdir().expect("temporary repository");
    std::fs::write(root.path().join("lib.rs"), "fn answer() {}\n").expect("write fixture");
    let database = root.path().join("index.sqlite");
    let blocker = rusqlite::Connection::open(&database).expect("open blocking connection");
    blocker
        .execute_batch(
            "CREATE TABLE startup_blocker(value INTEGER); \
             BEGIN IMMEDIATE; \
             INSERT INTO startup_blocker(value) VALUES (1);",
        )
        .expect("hold database write lock");

    let mut process = McpProcess::spawn(root.path(), &database);
    process.send(serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": "2025-11-25",
            "capabilities": {},
            "clientInfo": { "name": "leantoken-test", "version": "1" }
        }
    }));
    let response = process.response(Duration::from_secs(5));
    assert_eq!(response["id"], 1);
    assert!(response.get("result").is_some(), "{response}");

    blocker.execute_batch("ROLLBACK").expect("release database");
    process.send(serde_json::json!({
        "jsonrpc": "2.0",
        "method": "notifications/initialized"
    }));
    wait_until(Duration::from_secs(10), || {
        database_state(&database).is_some_and(|(generation, files, _)| {
            generation == 1 && files == 1
        })
    });
}

#[test]
fn mcp_cold_first_call_completes_the_public_acceptance_flow() {
    let root = tempfile::tempdir().expect("temporary repository");
    std::fs::write(
        root.path().join("lib.rs"),
        "pub fn context_distillery_ready() -> bool { true }\n",
    )
    .expect("write fixture");
    let database = root.path().join("index.sqlite");
    let mut process = McpProcess::spawn(root.path(), &database);

    let initialize = process.initialize();
    assert_eq!(initialize["result"]["serverInfo"]["name"], "leantoken");
    assert_runtime_version(&initialize["result"]["serverInfo"]["version"]);
    assert!(
        initialize["result"]["instructions"]
            .as_str()
            .is_some_and(|instructions| {
                instructions.contains("call leantoken.savings directly")
                    && instructions.contains("call leantoken.context once")
                    && instructions.contains("plan_only=false")
            })
    );
    process.send_initialized();

    process.send(serde_json::json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/list",
        "params": {}
    }));
    let tools = process.response(Duration::from_secs(5));
    let names = tools["result"]["tools"]
        .as_array()
        .expect("tool catalog")
        .iter()
        .filter_map(|tool| tool["name"].as_str())
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(
        names,
        [
            "context",
            "files",
            "history",
            "json",
            "outline",
            "read",
            "receipt_rebase",
            "savings",
            "search",
        ]
        .into_iter()
        .collect()
    );

    let deadline = Instant::now() + Duration::from_secs(15);
    let mut id = 3;
    let mut saw_retryable = false;
    loop {
        process.send(serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "tools/call",
            "params": {
                "name": "context",
                "arguments": {
                    "task": "find context_distillery_ready",
                    "token_budget": 200
                }
            }
        }));
        let response = process.response(deadline.saturating_duration_since(Instant::now()));
        assert_ne!(response["result"]["isError"], true, "{response}");
        if response["result"]["structuredContent"]["status"] == "retryable" {
            saw_retryable = true;
            id += 1;
            std::thread::sleep(Duration::from_millis(50));
            continue;
        }
        assert_eq!(
            response["result"]["structuredContent"]["fragments"][0]["path"],
            "lib.rs",
            "{response}"
        );
        assert!(
            !saw_retryable,
            "short cold index escaped the bounded server-side wait"
        );
        break;
    }
}

#[test]
fn mcp_recovers_when_startup_database_contention_clears() {
    let root = tempfile::tempdir().expect("temporary repository");
    std::fs::write(root.path().join("lib.rs"), "fn answer() {}\n").expect("write fixture");
    let database = root.path().join("index.sqlite");
    let blocker = rusqlite::Connection::open(&database).expect("open blocking connection");
    blocker
        .execute_batch(
            "CREATE TABLE startup_blocker(value INTEGER); \
             BEGIN EXCLUSIVE; \
             INSERT INTO startup_blocker(value) VALUES (1);",
        )
        .expect("hold database lock");

    let mut process = McpProcess::spawn(root.path(), &database);
    process.initialize();
    process.send_initialized();

    // Cross more than one startup busy-timeout and retry interval. A one-shot
    // startup would be permanently unavailable before the lock is released.
    std::thread::sleep(Duration::from_millis(750));
    blocker.execute_batch("ROLLBACK").expect("release database");
    process.wait_until_ready(Duration::from_secs(10));
}

#[test]
fn mcp_eof_cancels_contended_startup_promptly() {
    let root = tempfile::tempdir().expect("temporary repository");
    std::fs::write(root.path().join("lib.rs"), "fn answer() {}\n").expect("write fixture");
    let database = root.path().join("index.sqlite");
    let blocker = rusqlite::Connection::open(&database).expect("open blocking connection");
    blocker
        .execute_batch(
            "CREATE TABLE startup_blocker(value INTEGER); \
             BEGIN EXCLUSIVE; \
             INSERT INTO startup_blocker(value) VALUES (1);",
        )
        .expect("hold database lock");

    let mut process = McpProcess::spawn(root.path(), &database);
    process.initialize();
    process.send_initialized();
    process.stdin.take();

    let status = process
        .child
        .wait_timeout(Duration::from_secs(2))
        .expect("wait for MCP process")
        .expect("MCP process should honor startup cancellation");
    assert!(status.success(), "MCP process exited with {status}");
    blocker.execute_batch("ROLLBACK").expect("release database");
}

#[test]
fn mcp_runtime_failure_transitions_tools_out_of_starting_state() {
    let root = tempfile::tempdir().expect("temporary repository");
    std::fs::write(root.path().join("lib.rs"), "fn answer() {}\n").expect("write fixture");
    let database = root.path().join("index.sqlite");
    std::fs::create_dir(database.with_extension("sqlite.leader.lock"))
        .expect("invalid leadership artifact");

    let mut process = McpProcess::spawn(root.path(), &database);
    process.initialize();
    process.send_initialized();
    process.wait_until_unavailable(Duration::from_secs(5));

    // Cross the former runtime-first shutdown timeout. A failed repository
    // service remains an operational MCP connection until the client closes
    // the stdio transport.
    std::thread::sleep(Duration::from_secs(6));
    assert!(process.child.try_wait().expect("poll process").is_none());
    process.send(serde_json::json!({
        "jsonrpc": "2.0",
        "id": 50,
        "method": "tools/list",
        "params": {}
    }));
    let catalog = process.response(Duration::from_secs(2));
    assert_eq!(
        catalog["result"]["tools"].as_array().map(Vec::len),
        Some(9),
        "{catalog}"
    );
}

#[test]
fn cli_json_mcp_failure_is_one_document_after_a_logged_error() {
    let root = tempfile::tempdir().expect("temporary repository");
    std::fs::write(root.path().join("lib.rs"), "fn answer() {}\n").expect("write fixture");
    let database = root.path().join("index.sqlite");
    std::fs::create_dir(database.with_extension("sqlite.leader.lock"))
        .expect("invalid leadership artifact");

    let mut process =
        McpProcess::spawn_with_captured_stderr(root.path(), &database, &["--json"]);
    process.initialize();
    process.send_initialized();
    process.wait_until_unavailable(Duration::from_secs(5));
    process.stdin.take();

    let status = process
        .child
        .wait_timeout(Duration::from_secs(5))
        .expect("wait for JSON MCP failure")
        .expect("JSON MCP process should exit after EOF");
    assert!(!status.success());

    let stderr = process.take_stderr();
    let error: serde_json::Value =
        serde_json::from_slice(&stderr).expect("one structured error without tracing records");
    assert_eq!(error["category"], "internal_error");
    assert!(error["error"].is_string());
    assert_eq!(error.as_object().map(serde_json::Map::len), Some(2));
}

#[test]
fn mcp_rejects_home_root_after_initialize_without_opening_storage() {
    let home = directories::BaseDirs::new()
        .expect("home directories")
        .home_dir()
        .canonicalize()
        .expect("canonical home");
    let cache = tempfile::tempdir().expect("cache");
    let database = cache.path().join("index.sqlite");
    let mut process = McpProcess::spawn(&home, &database);

    process.initialize();
    assert!(
        !database.exists(),
        "repository configuration ran before MCP initialization"
    );
    process.send_initialized();

    let deadline = Instant::now() + Duration::from_secs(5);
    let mut id = 2;
    loop {
        process.send(serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "tools/call",
            "params": {
                "name": "files",
                "arguments": { "operation": {"kind": "tree"}, "max_results": 1 }
            }
        }));
        let response = process.response(deadline.saturating_duration_since(Instant::now()));
        let message = response["result"]["content"][0]["text"]
            .as_str()
            .unwrap_or_default();
        if message.contains("unavailable") {
            assert_eq!(response["result"]["isError"], true);
            assert_eq!(
                response["result"]["structuredContent"]["reason"],
                "unsafe_repository_root"
            );
            assert!(
                !response.to_string().contains(home.to_str().expect("UTF-8 home")),
                "unsafe path leaked in tool response: {response}"
            );
            assert!(!database.exists(), "unsafe root opened its SQLite cache");
            assert!(process.child.try_wait().expect("poll process").is_none());
            break;
        }
        assert!(
            Instant::now() < deadline,
            "unsafe root remained hidden behind startup state: {response}"
        );
        id += 1;
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn mcp_index_limit_failure_is_terminal_and_does_not_retry() {
    let root = tempfile::tempdir().expect("temporary repository");
    std::fs::write(root.path().join("a.rs"), "fn original() {}\n").expect("fixture");
    std::fs::write(root.path().join("b.rs"), "fn crosses_limit() {}\n").expect("second file");
    let database = root.path().join("index.sqlite");
    let mut process = McpProcess::spawn_with_args(
        root.path(),
        &database,
        &["--max-files", "1"],
    );
    process.initialize();
    process.send_initialized();

    let deadline = Instant::now() + Duration::from_secs(10);
    let mut id = 100;
    loop {
        process.send(serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "tools/call",
            "params": {
                "name": "files",
                "arguments": { "operation": {"kind": "tree"}, "max_results": 1 }
            }
        }));
        let response = process.response(deadline.saturating_duration_since(Instant::now()));
        let message = response["result"]["content"][0]["text"]
            .as_str()
            .unwrap_or_default();
        if message.contains("unavailable") {
            assert_eq!(response["result"]["isError"], true);
            break;
        }
        assert!(Instant::now() < deadline, "limit remained retryable: {response}");
        id += 1;
        std::thread::sleep(Duration::from_millis(50));
    }
    assert_eq!(database_state(&database).map(|state| state.0), Some(0));
    assert_eq!(database_state(&database).map(|state| state.1), Some(0));

    std::fs::remove_file(root.path().join("b.rs")).expect("shrink tree");
    std::thread::sleep(Duration::from_millis(1_250));
    process.send(serde_json::json!({
        "jsonrpc": "2.0",
        "id": id + 1,
        "method": "tools/call",
        "params": {
            "name": "files",
            "arguments": { "operation": {"kind": "tree"}, "max_results": 1 }
        }
    }));
    let response = process.response(Duration::from_secs(5));
    assert_eq!(response["result"]["isError"], true, "runtime retried: {response}");
    assert!(
        response["result"]["content"][0]["text"]
            .as_str()
            .is_some_and(|message| message.contains("unavailable"))
    );
    assert_eq!(database_state(&database).map(|state| state.0), Some(0));
    assert_eq!(database_state(&database).map(|state| state.1), Some(0));
    assert!(process.child.try_wait().expect("poll process").is_none());
}

#[test]
fn concurrent_mcp_startup_initializes_once_and_followers_read() {
    let root = tempfile::tempdir().expect("temporary repository");
    write_rust_fixture_set(root.path(), "file", 20, 100);
    let database = root.path().join("index.sqlite");
    let mut processes = (0..3)
        .map(|_| McpProcess::spawn(root.path(), &database))
        .collect::<Vec<_>>();

    for process in &mut processes {
        process.send(serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-11-25",
                "capabilities": {},
                "clientInfo": { "name": "leantoken-test", "version": "1" }
            }
        }));
    }
    let initialize_deadline = Instant::now() + Duration::from_secs(5);
    for process in &processes {
        let response = process.response(initialize_deadline.saturating_duration_since(Instant::now()));
        assert_eq!(response["id"], 1);
        assert!(response.get("result").is_some(), "{response}");
    }
    for process in &mut processes {
        process.send(serde_json::json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized"
        }));
    }

    wait_until(Duration::from_secs(15), || {
        database_state(&database).is_some_and(|(generation, files, _)| {
            generation == 1 && files == 20
        })
    });
    for process in &mut processes {
        process.wait_until_ready(Duration::from_secs(5));
    }
    assert_eq!(
        database_state(&database).map(|state| state.0),
        Some(1),
        "concurrent MCP followers must not publish duplicate generations"
    );
}

#[test]
fn mcp_follower_takes_over_after_leader_exit() {
    let root = tempfile::tempdir().expect("temporary repository");
    std::fs::write(root.path().join("lib.rs"), "fn before_failover() {}\n")
        .expect("write fixture");
    let database = root.path().join("index.sqlite");
    let mut leader = McpProcess::spawn(root.path(), &database);
    leader.initialize();
    leader.send_initialized();
    wait_until(Duration::from_secs(10), || {
        database_state(&database).is_some_and(|(generation, files, _)| {
            generation == 1 && files == 1
        })
    });

    let mut follower = McpProcess::spawn(root.path(), &database);
    follower.initialize();
    follower.send_initialized();
    follower.wait_until_ready(Duration::from_secs(5));

    leader.stop();

    std::fs::write(
        root.path().join("lib.rs"),
        "fn changed_after_failover() {}\n",
    )
    .expect("modify repository after leader exit");
    wait_until(Duration::from_secs(15), || {
        database_state(&database).is_some_and(|(generation, files, changed)| {
            generation == 2 && files == 1 && changed
        })
    });
}

#[test]
fn mcp_follower_does_not_hide_terminal_generation_zero_failover() {
    let root = tempfile::tempdir().expect("temporary repository");
    std::fs::write(root.path().join("a.rs"), "fn first() {}\n").expect("first fixture");
    std::fs::write(root.path().join("b.rs"), "fn exceeds_limit() {}\n")
        .expect("second fixture");
    let database = root.path().join("index.sqlite");
    let coordination = leantoken::coordination::IndexCoordination::for_database(&database);
    let operation_blocker = coordination
        .acquire_operation(&tokio_util::sync::CancellationToken::new())
        .expect("block leader reconciliation");

    let mut leader =
        McpProcess::spawn_with_args(root.path(), &database, &["--max-files", "1"]);
    leader.initialize();
    leader.send_initialized();
    wait_until(Duration::from_secs(5), || {
        coordination
            .try_acquire_leadership()
            .expect("probe leadership")
            .is_none()
    });

    let mut follower =
        McpProcess::spawn_with_args(root.path(), &database, &["--max-files", "1"]);
    follower.initialize();
    follower.send_initialized();

    drop(operation_blocker);

    follower.send(serde_json::json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/call",
        "params": {
            "name": "files",
            "arguments": { "operation": {"kind": "tree"}, "max_results": 1 }
        }
    }));
    // Process scheduling can delay the follower's first response when unit
    // and integration tests share the host. This is a liveness observation,
    // not the one-second leadership-grace contract tested in Services.
    let first = follower.response(Duration::from_secs(30));
    if first["result"]["isError"] != true {
        assert_eq!(
            first["result"]["structuredContent"]["reason"],
            "index_building",
            "{first}"
        );
    }
    // Coverage instrumentation and concurrent process tests can delay terminal
    // propagation without changing the one-second leadership grace, which is
    // verified deterministically in the Services tests. Keep this process-level
    // liveness check bounded, but allow for full-suite instrumentation overhead.
    follower.wait_until_unavailable(Duration::from_secs(30));
    assert_eq!(database_state(&database).map(|state| state.0), Some(0));
}

#[test]
fn mcp_follower_rebuilds_after_leader_is_killed_during_reconciliation() {
    let root = tempfile::tempdir().expect("temporary repository");
    std::fs::write(root.path().join("old.rs"), "fn committed_before_crash() {}\n")
        .expect("old fixture");
    let database = root.path().join("index.sqlite");
    let initial = run(root.path(), &database, &["index"]);
    assert_eq!(initial["repository_generation"], 1);
    assert_eq!(database_state(&database).map(|state| state.1), Some(1));

    // Keep reconciliation large enough to kill the leader mid-flight without
    // making every product-loop run parse thousands of unnecessary symbols.
    write_rust_fixture_set(root.path(), "new", 20, 150);

    let coordination =
        leantoken::coordination::IndexCoordination::for_database(&database);
    let operation_blocker = coordination
        .acquire_operation(&tokio_util::sync::CancellationToken::new())
        .expect("block reconciliation");

    let mut leader = McpProcess::spawn(root.path(), &database);
    leader.initialize();
    leader.send_initialized();
    wait_until(Duration::from_secs(5), || {
        coordination
            .try_acquire_leadership()
            .expect("probe leadership")
            .is_none()
    });

    let mut follower = McpProcess::spawn(root.path(), &database);
    follower.initialize();
    follower.send_initialized();
    follower.wait_until_ready(Duration::from_secs(5));

    drop(operation_blocker);
    wait_until(Duration::from_secs(5), || {
        coordination.is_reconciling().expect("probe reconciliation")
    });
    leader.kill_now();

    wait_until(Duration::from_secs(5), || {
        database_state(&database).is_some_and(|(generation, files, _)| {
            generation == 1 && files == 1
        })
    });
    wait_until(Duration::from_secs(20), || {
        database_state(&database).is_some_and(|(generation, files, _)| {
            generation == 2 && files == 21
        })
    });
    follower.wait_until_ready(Duration::from_secs(5));
}

#[test]
fn setup_and_remove_do_not_require_a_repository() {
    let temp = tempfile::tempdir().expect("temporary home");

    let setup = Command::cargo_bin("leantoken")
        .expect("binary")
        .env("HOME", temp.path())
        .env("USERPROFILE", temp.path())
        .env_remove("npm_lifecycle_event")
        .current_dir(temp.path())
        .args([
            "--json",
            "setup",
            "--claude",
            "--yes",
        ])
        .output()
        .expect("run setup");
    assert!(
        setup.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&setup.stderr)
    );
    let report: serde_json::Value =
        serde_json::from_slice(&setup.stdout).expect("setup JSON output");
    assert_eq!(report["results"][0]["status"], "configured");
    let config = std::fs::read_to_string(temp.path().join(".claude.json"))
        .expect("Claude configuration");
    assert!(config.contains("\"leantoken\""));
    assert!(config.contains("\"mcp\""));

    let remove = Command::cargo_bin("leantoken")
        .expect("binary")
        .env("HOME", temp.path())
        .env("USERPROFILE", temp.path())
        .env_remove("npm_lifecycle_event")
        .current_dir(temp.path())
        .args(["--json", "remove", "--claude", "--yes"])
        .output()
        .expect("run remove");
    assert!(
        remove.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&remove.stderr)
    );
    let config = std::fs::read_to_string(temp.path().join(".claude.json"))
        .expect("Claude configuration after removal");
    assert!(!config.contains("\"leantoken\""));
}

#[test]
fn repository_options_are_rejected_by_repository_free_commands() {
    for arguments in [
        vec!["--json", "--root", ".", "setup", "--all", "--dry-run"],
        vec!["--json", "--root", ".", "remove", "--all", "--dry-run"],
        vec!["--json", "cache", "list", "--max-file-bytes", "1"],
        vec![
            "--json",
            "--root",
            ".",
            "episode",
            "audit",
            "--adapter",
            "mcp-wire-report-v2",
            "--input",
            "trace.json",
        ],
        vec!["--json", "upgrade", "--tokenizer", "cl100k_base", "--check"],
    ] {
        let output = Command::cargo_bin("leantoken")
            .expect("binary")
            .args(arguments)
            .output()
            .expect("run command");
        assert!(!output.status.success());
        let error: serde_json::Value =
            serde_json::from_slice(&output.stderr).expect("structured parse error");
        assert_eq!(error["category"], "invalid_input");
        assert!(
            error["error"]
                .as_str()
                .is_some_and(|message| message.contains("repository option")),
            "{error}"
        );
    }
}

#[test]
fn episode_audit_is_repo_free_deterministic_and_read_only() {
    let temp = tempfile::tempdir().expect("temporary working directory");
    let input = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("benchmarks/reports/multi-agent-context-suite-v1-codex-0.144.1.json");
    let before = std::fs::read(&input).expect("read input before audit");
    let command = || {
        let mut command = Command::cargo_bin("leantoken").expect("binary");
        command.current_dir(temp.path());
        command
    };
    let arguments = [
        "--json",
        "episode",
        "audit",
        "--adapter",
        "multi-agent-suite-v1",
        "--input",
        input.to_str().expect("input UTF-8"),
    ];
    let first = command().args(arguments).output().expect("first JSON audit");
    let second = command().args(arguments).output().expect("second JSON audit");
    assert!(
        first.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&first.stderr)
    );
    assert_eq!(first.stdout, second.stdout);
    let report: serde_json::Value =
        serde_json::from_slice(&first.stdout).expect("normalized report");
    assert_eq!(report["schema_version"], 1);
    assert_eq!(report["report_kind"], "episode_audit");
    assert_eq!(report["adapter"]["name"], "multi_agent_suite");
    assert_eq!(report["summary"]["episodes"], 60);
    assert!(
        report["findings"]
            .as_array()
            .is_some_and(|findings| findings.iter().any(|finding| {
                finding["code"] == "provider_input_regression"
                    && finding["value"]
                        .as_f64()
                        .is_some_and(|value| (value - 0.509_299_914_852_593_3).abs() < 1e-12)
            }))
    );
    assert!(!String::from_utf8_lossy(&first.stdout).contains("flask-ipv6"));
    assert_eq!(
        std::fs::read(&input).expect("read input after audit"),
        before
    );
    assert_eq!(
        std::fs::read_dir(temp.path())
            .expect("temporary directory")
            .count(),
        0
    );

    let human = command()
        .args([
            "episode",
            "audit",
            "--adapter",
            "multi-agent-suite-v1",
            "--input",
            input.to_str().expect("input UTF-8"),
        ])
        .output()
        .expect("Markdown audit");
    assert!(human.status.success());
    let markdown = String::from_utf8(human.stdout).expect("Markdown UTF-8");
    assert!(markdown.starts_with("# LeanToken episode audit\n"));
    assert!(markdown.contains("| `provider_input_regression` | 20 | 50.93% |"));
}

#[test]
fn setup_requires_yes_before_non_interactive_mutation() {
    let temp = tempfile::tempdir().expect("temporary home");
    let output = Command::cargo_bin("leantoken")
        .expect("binary")
        .env("HOME", temp.path())
        .env("USERPROFILE", temp.path())
        .env_remove("npm_lifecycle_event")
        .args(["--json", "setup", "--codex"])
        .output()
        .expect("run setup");
    assert!(!output.status.success());
    assert!(!temp.path().join(".codex/config.toml").exists());
    let error: serde_json::Value =
        serde_json::from_slice(&output.stderr).expect("structured setup error");
    assert!(
        error["error"]
            .as_str()
            .is_some_and(|message| message.contains("requires explicit client flags"))
    );
    assert_eq!(error["category"], "invalid_request");
}

// Windows ProjectDirs uses the Known Folder API and cannot be redirected to a
// disposable cache root through per-process environment variables. The cache
// module tests cover Windows lease and deletion semantics without user data.
#[cfg(not(windows))]
#[test]
fn cache_list_and_prune_do_not_require_a_repository() {
    let temp = tempfile::tempdir().expect("temporary home");
    let command = || {
        let mut command = Command::cargo_bin("leantoken").expect("binary");
        command
            .env("HOME", temp.path())
            .env("USERPROFILE", temp.path())
            .env_remove("npm_lifecycle_event")
            .env("XDG_CACHE_HOME", temp.path().join("xdg-cache"))
            .env("LOCALAPPDATA", temp.path().join("local-app-data"));
        command
    };
    let listed = command()
        .args(["--json", "cache", "list"])
        .output()
        .expect("list caches");
    assert!(
        listed.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&listed.stderr)
    );
    let list: serde_json::Value =
        serde_json::from_slice(&listed.stdout).expect("cache list JSON");
    let cache_root = std::path::PathBuf::from(list["cache_root"].as_str().expect("cache root"));
    let cache = cache_root.join("0000000000000001");
    std::fs::create_dir_all(&cache).expect("cache directory");
    let database = cache.join("index.sqlite");
    std::fs::write(&database, b"corrupt managed cache").expect("cache fixture");
    let legacy_cache = cache_root.join("0000000000000002");
    std::fs::create_dir_all(&legacy_cache).expect("legacy cache directory");
    let legacy_database = legacy_cache.join("index.sqlite");
    rusqlite::Connection::open(&legacy_database)
        .expect("legacy database")
        .execute_batch(
            "CREATE TABLE meta (
                id INTEGER PRIMARY KEY,
                schema_version INTEGER NOT NULL,
                repository_root TEXT NOT NULL
            );
            INSERT INTO meta VALUES (1, 4, '');",
        )
        .expect("legacy metadata");

    let human_list = command()
        .args(["cache", "list"])
        .output()
        .expect("human cache list");
    assert!(human_list.status.success());
    let human_list = String::from_utf8_lossy(&human_list.stdout);
    assert!(human_list.contains("corrupt"));
    assert!(human_list.contains("last_access="));
    assert!(human_list.contains("root_available="));
    assert!(human_list.contains("scope=full"));

    let summary = command()
        .args([
            "--json",
            "cache",
            "list",
            "--summary",
            "--state",
            "corrupt",
        ])
        .output()
        .expect("cache summary");
    assert!(summary.status.success());
    let summary: serde_json::Value =
        serde_json::from_slice(&summary.stdout).expect("cache summary JSON");
    assert_eq!(summary["total_entries"], 2);
    assert_eq!(summary["matched_entries"], 1);
    assert_eq!(summary["returned_entries"], 0);
    assert_eq!(summary["state_counts"]["corrupt"], 1);
    assert_eq!(summary["entries"].as_array().map(Vec::len), Some(0));

    let dry_run = command()
        .args([
            "--json",
            "cache",
            "prune",
            "--max-total-bytes",
            "1",
            "--dry-run",
        ])
        .output()
        .expect("dry-run prune");
    assert!(dry_run.status.success());
    let dry_run: serde_json::Value =
        serde_json::from_slice(&dry_run.stdout).expect("prune JSON");
    assert_eq!(dry_run["results"][0]["action"], "kept");
    assert_eq!(dry_run["results"][1]["action"], "would_delete");
    assert!(database.exists());
    assert!(legacy_database.exists());

    let human_prune = command()
        .args([
            "cache",
            "prune",
            "--max-total-bytes",
            "1",
            "--dry-run",
        ])
        .output()
        .expect("human prune plan");
    assert!(human_prune.status.success());
    assert!(String::from_utf8_lossy(&human_prune.stdout).contains("would_delete"));

    let prune = command()
        .args([
            "--json",
            "cache",
            "prune",
            "--max-total-bytes",
            "1",
            "--yes",
        ])
        .output()
        .expect("prune cache");
    assert!(
        prune.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&prune.stderr)
    );
    let prune: serde_json::Value =
        serde_json::from_slice(&prune.stdout).expect("prune JSON");
    assert_eq!(prune["results"][0]["action"], "kept");
    assert_eq!(prune["results"][1]["action"], "deleted");
    assert!(database.exists());
    assert!(!legacy_database.exists());
}

#[cfg(not(windows))]
#[test]
fn runtime_list_and_prune_are_bounded_reference_safe_and_dry_run_by_default() {
    let temp = tempfile::tempdir().expect("temporary home");
    let command = || {
        let mut command = Command::cargo_bin("leantoken").expect("binary");
        command
            .env("HOME", temp.path())
            .env("USERPROFILE", temp.path())
            .env("XDG_DATA_HOME", temp.path().join("xdg-data"))
            .env_remove("npm_lifecycle_event");
        command
    };
    let initial = command()
        .args(["--json", "runtime", "list"])
        .output()
        .expect("initial runtime list");
    assert!(initial.status.success());
    let initial: serde_json::Value =
        serde_json::from_slice(&initial.stdout).expect("initial runtime JSON");
    let runtime_root =
        std::path::PathBuf::from(initial["runtime_root"].as_str().expect("runtime root"));
    let executable_name = if cfg!(windows) {
        "leantoken.exe"
    } else {
        "leantoken"
    };
    let runtime = |version: &str, bytes: &[u8]| {
        let directory = runtime_root.join(version);
        std::fs::create_dir_all(&directory).expect("runtime directory");
        let executable = directory.join(executable_name);
        std::fs::write(&executable, bytes).expect("runtime executable");
        executable
    };
    let oldest = runtime("1.0.0", b"old");
    let referenced = runtime("1.1.0", b"referenced");
    let newest = runtime("1.2.0", b"newest");
    let unsafe_runtime = runtime("0.9.0", b"unsafe");
    std::fs::write(unsafe_runtime.parent().unwrap().join("notes.txt"), b"keep")
        .expect("unrecognized sibling");
    let referenced_alias = referenced
        .parent()
        .expect("referenced runtime directory")
        .join("..")
        .join("1.1.0")
        .join(executable_name);
    assert_eq!(
        referenced_alias.canonicalize().unwrap(),
        referenced.canonicalize().unwrap()
    );
    let claude = serde_json::json!({
        "mcpServers": {
            "leantoken": {
                "command": referenced_alias,
                "args": ["--managed-by-setup", "mcp"]
            }
        }
    });
    std::fs::write(
        temp.path().join(".claude.json"),
        serde_json::to_vec(&claude).unwrap(),
    )
    .expect("Claude config");

    let listed = command()
        .args(["--json", "runtime", "list"])
        .output()
        .expect("runtime list");
    assert!(listed.status.success());
    let listed: serde_json::Value =
        serde_json::from_slice(&listed.stdout).expect("runtime list JSON");
    assert_eq!(listed["total_entries"], 4);
    let entries = listed["entries"].as_array().expect("runtime entries");
    let referenced_entry = entries
        .iter()
        .find(|entry| entry["version"] == "1.1.0")
        .expect("referenced runtime");
    assert_eq!(referenced_entry["referenced_by"], serde_json::json!(["claude"]));
    let unsafe_entry = entries
        .iter()
        .find(|entry| entry["version"] == "0.9.0")
        .expect("unsafe runtime");
    assert_eq!(unsafe_entry["safely_prunable"], false);

    let human_list = command()
        .args(["runtime", "list"])
        .output()
        .expect("human runtime list");
    assert!(human_list.status.success());
    let human_list = String::from_utf8_lossy(&human_list.stdout);
    assert!(human_list.contains("Private runtime root:"));
    assert!(human_list.contains("4 runtime(s)"));
    assert!(human_list.contains("referenced_by=Claude Code"));
    assert!(human_list.contains("inactive,unrecognized"));

    let planned = command()
        .args([
            "--json",
            "runtime",
            "prune",
            "--keep-latest",
            "0",
        ])
        .output()
        .expect("runtime prune plan");
    assert!(planned.status.success());
    let planned: serde_json::Value =
        serde_json::from_slice(&planned.stdout).expect("runtime prune JSON");
    assert_eq!(planned["dry_run"], true);
    assert!(oldest.exists());
    assert!(newest.exists());
    assert!(referenced.exists());
    assert!(unsafe_runtime.exists());
    let results = planned["results"].as_array().expect("prune decisions");
    assert!(results.iter().any(|result| {
        result["version"] == "1.1.0"
            && result["action"] == "retained"
            && result["reason"] == "referenced_by_client"
    }));
    assert!(results.iter().any(|result| {
        result["version"] == "0.9.0"
            && result["action"] == "retained"
            && result["reason"] == "unrecognized_directory_contents"
    }));

    let human_plan = command()
        .args(["runtime", "prune", "--keep-latest", "0"])
        .output()
        .expect("human runtime prune plan");
    assert!(human_plan.status.success());
    let human_plan = String::from_utf8_lossy(&human_plan.stdout);
    assert!(human_plan.contains("Private runtime prune dry-run:"));
    assert!(human_plan.contains("would_remove  1.0.0  3 bytes  outside_retention"));
    assert!(human_plan.contains("retained  0.9.0  6 bytes  unrecognized_directory_contents"));

    let applied = command()
        .args([
            "--json",
            "runtime",
            "prune",
            "--keep-latest",
            "0",
            "--yes",
        ])
        .output()
        .expect("apply runtime prune");
    assert!(
        applied.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&applied.stderr)
    );
    assert!(!oldest.exists());
    assert!(!newest.exists());
    assert!(referenced.exists());
    assert!(unsafe_runtime.exists());
    let final_list = command()
        .args(["--json", "runtime", "list"])
        .output()
        .expect("final runtime list");
    assert!(final_list.status.success());
    let final_list: serde_json::Value =
        serde_json::from_slice(&final_list.stdout).expect("final runtime JSON");
    assert_eq!(final_list["ignored_entries"], 0);

    std::fs::create_dir_all(temp.path().join(".cursor")).expect("Cursor config directory");
    let oversized = std::fs::File::create(temp.path().join(".cursor/mcp.json"))
        .expect("oversized Cursor config");
    oversized
        .set_len(8 * 1024 * 1024 + 1)
        .expect("extend Cursor config");
    let bounded = command()
        .args(["runtime", "list"])
        .output()
        .expect("bounded runtime list");
    assert!(!bounded.status.success());
    assert!(String::from_utf8_lossy(&bounded.stderr).contains("byte limit"));
}

#[test]
fn setup_dry_run_reports_exact_plan_without_mutation() {
    let temp = tempfile::tempdir().expect("temporary home");
    let output = Command::cargo_bin("leantoken")
        .expect("binary")
        .env("HOME", temp.path())
        .env("USERPROFILE", temp.path())
        .env_remove("npm_lifecycle_event")
        .args(["--json", "setup", "--codex", "--dry-run"])
        .output()
        .expect("run setup dry-run");
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("dry-run JSON output");
    assert_eq!(report["dry_run"], true);
    assert_eq!(report["plan"][0]["client"], "codex");
    assert_eq!(report["plan"][0]["action"], "create");
    assert_eq!(report["launcher"]["version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(report["launcher"]["package"], serde_json::Value::Null);
    assert_eq!(report["launcher"]["may_contact_network"], false);
    assert!(!temp.path().join(".codex/config.toml").exists());
}

#[test]
fn malformed_selected_config_blocks_all_setup_writes() {
    let temp = tempfile::tempdir().expect("temporary home");
    std::fs::write(temp.path().join(".claude.json"), "{ broken")
        .expect("write malformed config");
    let output = Command::cargo_bin("leantoken")
        .expect("binary")
        .env("HOME", temp.path())
        .env("USERPROFILE", temp.path())
        .env_remove("npm_lifecycle_event")
        .args(["--json", "setup", "--claude", "--cursor", "--yes"])
        .output()
        .expect("run setup");
    assert!(!output.status.success());
    assert!(!temp.path().join(".cursor/mcp.json").exists());
    let error: serde_json::Value =
        serde_json::from_slice(&output.stderr).expect("structured setup error");
    assert_eq!(error["category"], "setup_failure");
    assert!(
        error["error"]
            .as_str()
            .is_some_and(|message| message.contains("refusing to overwrite malformed config"))
    );
    assert_eq!(error.as_object().map(serde_json::Map::len), Some(2));
}

#[test]
fn npx_setup_registers_exact_release_instead_of_its_cache_path() {
    let temp = tempfile::tempdir().expect("temporary home");
    let runtime = temp.path().join("node runtime");
    let node = runtime.join(if cfg!(windows) { "node.exe" } else { "node" });
    let npx = runtime.join("npx cli.js");
    let setup = Command::cargo_bin("leantoken")
        .expect("binary")
        .env("HOME", temp.path())
        .env("USERPROFILE", temp.path())
        .env("npm_lifecycle_event", "npx")
        .env("npm_node_execpath", &node)
        .env("npm_execpath", &npx)
        .args(["--json", "setup", "--claude", "--yes"])
        .output()
        .expect("run npx setup");
    assert!(
        !setup.status.success(),
        "the nonexistent npx launcher must fail verification"
    );
    let report: serde_json::Value =
        serde_json::from_slice(&setup.stdout).expect("setup JSON output");
    assert_eq!(report["verification"]["status"], "failed");
    let package = format!("leantoken@{}", env!("CARGO_PKG_VERSION"));
    assert_eq!(report["launcher"]["version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(report["launcher"]["package"], package);
    assert_eq!(report["launcher"]["may_contact_network"], true);
    assert!(!report.to_string().contains("@latest"));

    let config: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(temp.path().join(".claude.json"))
            .expect("Claude configuration"),
    )
    .expect("Claude JSON");
    assert_eq!(config["mcpServers"]["leantoken"]["command"], node.to_str().unwrap());
    assert_eq!(
        config["mcpServers"]["leantoken"]["args"],
        serde_json::json!([
            npx.to_str().unwrap(),
            "--yes",
            "--prefer-offline",
            format!("--package=leantoken@{}", env!("CARGO_PKG_VERSION")),
            "--",
            "leantoken",
            "--managed-by-setup",
            "mcp"
        ])
    );
    assert!(!config.to_string().contains("@latest"));
}

#[test]
fn setup_refresh_targets_only_existing_mcp_entries() {
    let temp = tempfile::tempdir().expect("temporary home");
    let node = temp.path().join("node");
    let npm = temp.path().join("npm-cli.js");
    let command = || {
        let mut command = Command::cargo_bin("leantoken").expect("binary");
        command
            .env("HOME", temp.path())
            .env("USERPROFILE", temp.path())
            .env("npm_lifecycle_event", "npx")
            .env("npm_node_execpath", &node)
            .env("npm_execpath", &npm);
        command
    };
    let setup = command()
        .args(["--json", "setup", "--claude", "--yes"])
        .output()
        .expect("run initial setup");
    assert!(
        !setup.status.success(),
        "the nonexistent npx launcher must fail verification"
    );
    let setup_report: serde_json::Value =
        serde_json::from_slice(&setup.stdout).expect("setup JSON output");
    assert_eq!(setup_report["verification"]["status"], "failed");
    std::fs::create_dir_all(temp.path().join(".cursor")).expect("Cursor directory");
    std::fs::write(
        temp.path().join(".cursor/mcp.json"),
        "{\"mcpServers\":{\"other\":{\"command\":\"other\"}}}\n",
    )
    .expect("Cursor config");

    let refresh = command()
        .args(["--json", "setup", "--refresh", "--yes"])
        .output()
        .expect("run setup refresh");
    assert!(
        !refresh.status.success(),
        "the nonexistent npx launcher must fail verification"
    );
    let report: serde_json::Value =
        serde_json::from_slice(&refresh.stdout).expect("refresh JSON output");
    assert_eq!(report["verification"]["status"], "failed");
    assert_eq!(report["plan"].as_array().unwrap().len(), 1);
    assert_eq!(report["plan"][0]["client"], "claude");
    assert_eq!(report["plan"][0]["action"], "already_current");
    let cursor = std::fs::read_to_string(temp.path().join(".cursor/mcp.json"))
        .expect("Cursor config after refresh");
    assert!(!cursor.contains("\"leantoken\""));
}

#[test]
fn private_runtime_setup_installs_and_registers_the_verified_native_binary() {
    let temp = tempfile::tempdir().expect("temporary home");
    let data_home = temp.path().join("data");
    let runtime = temp.path().join("node runtime");
    let setup = Command::cargo_bin("leantoken")
        .expect("binary")
        .env("HOME", temp.path())
        .env("USERPROFILE", temp.path())
        .env("XDG_DATA_HOME", &data_home)
        .env("LOCALAPPDATA", &data_home)
        .env("npm_lifecycle_event", "npx")
        .env("npm_node_execpath", runtime.join("node"))
        .env("npm_execpath", runtime.join("npm-cli.js"))
        .args([
            "--json",
            "setup",
            "--codex",
            "--private-runtime",
            "--yes",
        ])
        .output()
        .expect("run private runtime setup");
    assert!(
        setup.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&setup.stderr)
    );
    let report: serde_json::Value =
        serde_json::from_slice(&setup.stdout).expect("setup JSON output");
    let runtime_path = std::path::PathBuf::from(
        report["launcher"]["runtime_path"]
            .as_str()
            .expect("runtime path"),
    );
    assert!(runtime_path.exists());
    assert_eq!(report["launcher"]["package"], serde_json::Value::Null);
    assert_eq!(report["launcher"]["may_contact_network"], false);
    assert_eq!(report["discovery_plan"].as_array().map(Vec::len), Some(1));

    let version = Command::new(&runtime_path)
        .arg("--version")
        .output()
        .expect("run installed native executable");
    assert!(version.status.success());
    assert!(
        String::from_utf8_lossy(&version.stdout).contains(env!("CARGO_PKG_VERSION"))
    );
    let repository = temp.path().join("workspace");
    std::fs::create_dir(&repository).expect("workspace");
    std::fs::write(
        repository.join("lib.rs"),
        "pub fn private_runtime_retrieval() -> bool { true }\n",
    )
    .expect("workspace source");
    let doctor = Command::new(&runtime_path)
        .env("HOME", temp.path())
        .env("USERPROFILE", temp.path())
        .env("XDG_DATA_HOME", &data_home)
        .env("LOCALAPPDATA", &data_home)
        .args([
            "--root",
            repository.to_str().expect("repository UTF-8"),
            "--database",
            temp.path()
                .join("private-runtime.sqlite")
                .to_str()
                .expect("database UTF-8"),
            "--json",
            "doctor",
        ])
        .output()
        .expect("run installed runtime doctor");
    assert!(
        doctor.status.success(),
        "private runtime doctor stderr: {}",
        String::from_utf8_lossy(&doctor.stderr)
    );
    let doctor_report: serde_json::Value =
        serde_json::from_slice(&doctor.stdout).expect("doctor report");
    assert_eq!(
        doctor_report["tools"],
        serde_json::json!([
            "context",
            "files",
            "history",
            "json",
            "outline",
            "read",
            "receipt_rebase",
            "savings",
            "search"
        ])
    );
    assert_eq!(doctor_report["first_call"]["status"], "ready");
    let codex = std::fs::read_to_string(temp.path().join(".codex/config.toml"))
        .expect("Codex configuration");
    assert!(codex.contains(runtime_path.to_str().expect("UTF-8 runtime path")));
    assert!(!codex.contains("npm-cli"));
}

#[test]
fn npx_setup_explains_that_it_does_not_install_a_global_cli() {
    let temp = tempfile::tempdir().expect("temporary home");
    let output = Command::cargo_bin("leantoken")
        .expect("binary")
        .env("HOME", temp.path())
        .env("USERPROFILE", temp.path())
        .env("npm_lifecycle_event", "npx")
        .env("npm_node_execpath", temp.path().join("node"))
        .env("npm_execpath", temp.path().join("npm-cli.js"))
        .args(["setup", "--codex", "--yes"])
        .output()
        .expect("run npx setup");

    assert!(
        !output.status.success(),
        "the nonexistent npx launcher must fail verification"
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("LeanToken // Context Distillery"));
    assert!(stdout.contains("LeanToken is configured for 1 client."));
    assert!(stdout.contains(&format!(
        "npx --yes leantoken@{} doctor --client codex",
        env!("CARGO_PKG_VERSION")
    )));
    assert!(stdout.contains("no global `leantoken` command was installed"));
    assert!(stdout.contains("npx --yes leantoken@latest setup --refresh --yes"));
    assert!(stdout.contains(&format!(
        "npx --yes leantoken@{} setup --refresh --private-runtime --yes",
        env!("CARGO_PKG_VERSION")
    )));
    assert!(stdout.contains(&format!(
        "pinned to LeanToken v{}",
        env!("CARGO_PKG_VERSION")
    )));
    assert!(stdout.contains("npm install --global leantoken@latest"));
    assert!(stdout.contains("Launcher verification failed"));
    assert!(stdout.contains(
        "Client configuration succeeded, but launcher verification failed. The configured entries remain in place for diagnosis."
    ));
    assert!(stdout.contains("In-agent smoke test:"));
    assert!(!stdout.contains("Some selected clients failed"));
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("MCP launcher verification failed")
    );
}

fn run(
    root: &std::path::Path,
    database: &std::path::Path,
    arguments: &[&str],
) -> serde_json::Value {
    let output = Command::cargo_bin("leantoken")
        .expect("binary")
        .args([
            "--root",
            root.to_str().expect("root UTF-8"),
            "--database",
            database.to_str().expect("database UTF-8"),
            "--json",
        ])
        .args(arguments)
        .output()
        .expect("run leantoken");
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("JSON output")
}

fn run_error(
    root: &std::path::Path,
    database: &std::path::Path,
    arguments: &[&str],
) -> serde_json::Value {
    let output = Command::cargo_bin("leantoken")
        .expect("binary")
        .args([
            "--root",
            root.to_str().expect("root UTF-8"),
            "--database",
            database.to_str().expect("database UTF-8"),
            "--json",
        ])
        .args(arguments)
        .output()
        .expect("run leantoken");
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    serde_json::from_slice(&output.stderr).expect("structured error")
}

fn assert_cli_parse_error(arguments: &[&str]) {
    let expected = leantoken::cli::Cli::try_parse_from(
        std::iter::once(leantoken_program_name())
            .chain(arguments.iter().map(std::ffi::OsString::from)),
    )
    .expect_err("invalid CLI arguments")
    .to_string();
    let output = Command::cargo_bin("leantoken")
        .expect("binary")
        .args(arguments)
        .output()
        .expect("run CLI parse failure");

    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&output.stderr)
            .expect("structured parse error"),
        serde_json::json!({
            "error": expected.trim_end(),
            "category": "invalid_input"
        })
    );
}

fn leantoken_program_name() -> std::ffi::OsString {
    assert_cmd::cargo::cargo_bin!("leantoken")
        .file_name()
        .expect("binary file name")
        .to_os_string()
}

struct McpProcess {
    child: Child,
    stdin: Option<ChildStdin>,
    lines: mpsc::Receiver<String>,
    stderr_task: Option<std::thread::JoinHandle<Vec<u8>>>,
}

impl McpProcess {
    fn spawn(root: &std::path::Path, database: &std::path::Path) -> Self {
        Self::spawn_with_args(root, database, &[])
    }

    fn spawn_with_args(
        root: &std::path::Path,
        database: &std::path::Path,
        arguments: &[&str],
    ) -> Self {
        Self::spawn_with_options(root, database, arguments, false)
    }

    fn spawn_with_mcp_args(
        root: &std::path::Path,
        database: &std::path::Path,
        arguments: &[&str],
    ) -> Self {
        Self::spawn_with_command_args(root, database, &[], arguments, false)
    }

    fn spawn_with_captured_stderr(
        root: &std::path::Path,
        database: &std::path::Path,
        arguments: &[&str],
    ) -> Self {
        Self::spawn_with_options(root, database, arguments, true)
    }

    fn spawn_with_options(
        root: &std::path::Path,
        database: &std::path::Path,
        arguments: &[&str],
        capture_stderr: bool,
    ) -> Self {
        Self::spawn_with_command_args(root, database, arguments, &[], capture_stderr)
    }

    fn spawn_with_command_args(
        root: &std::path::Path,
        database: &std::path::Path,
        arguments: &[&str],
        mcp_arguments: &[&str],
        capture_stderr: bool,
    ) -> Self {
        let mut command = std::process::Command::new(assert_cmd::cargo::cargo_bin!("leantoken"));
        command
            .args([
                "--root",
                root.to_str().expect("root UTF-8"),
                "--database",
                database.to_str().expect("database UTF-8"),
            ])
            .args(arguments)
            .arg("mcp")
            .args(mcp_arguments);
        command.stderr(if capture_stderr {
            Stdio::piped()
        } else {
            Stdio::null()
        });
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .expect("spawn MCP process");
        let stdin = child.stdin.take().expect("MCP stdin");
        let stdout = child.stdout.take().expect("MCP stdout");
        let stderr_task = child.stderr.take().map(|mut stderr| {
            std::thread::spawn(move || {
                let mut output = Vec::new();
                stderr
                    .read_to_end(&mut output)
                    .expect("read MCP stderr");
                output
            })
        });
        let (tx, lines) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        Self {
            child,
            stdin: Some(stdin),
            lines,
            stderr_task,
        }
    }

    fn take_stderr(&mut self) -> Vec<u8> {
        self.stderr_task
            .take()
            .expect("captured MCP stderr")
            .join()
            .expect("join MCP stderr reader")
    }

    fn initialize(&mut self) -> serde_json::Value {
        self.initialize_as("leantoken-test", "1", "2025-11-25")
    }

    fn initialize_as(
        &mut self,
        client_name: &str,
        client_version: &str,
        protocol_version: &str,
    ) -> serde_json::Value {
        self.send(serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": protocol_version,
                "capabilities": {},
                "clientInfo": { "name": client_name, "version": client_version }
            }
        }));
        let response = self.response(Duration::from_secs(5));
        assert_eq!(response["id"], 1);
        assert!(response.get("result").is_some(), "{response}");
        response
    }

    fn send_initialized(&mut self) {
        self.send(serde_json::json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized"
        }));
    }

    fn wait_until_ready(&mut self, timeout: Duration) {
        let deadline = Instant::now() + timeout;
        let mut id = 2;
        while Instant::now() < deadline {
            self.send(serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "method": "tools/call",
                "params": {
                    "name": "files",
                    "arguments": { "operation": {"kind": "tree"}, "max_results": 1 }
                }
            }));
            let response = self.response(deadline.saturating_duration_since(Instant::now()));
            if response["result"]["isError"] != true
                && response["result"]["structuredContent"]["status"] != "retryable"
            {
                return;
            }
            id += 1;
            std::thread::sleep(Duration::from_millis(50));
        }
        panic!("MCP process did not become ready within {timeout:?}");
    }

    fn wait_until_unavailable(&mut self, timeout: Duration) {
        let deadline = Instant::now() + timeout;
        let mut id = 2;
        loop {
            self.send(serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "method": "tools/call",
                "params": {
                    "name": "files",
                    "arguments": { "operation": {"kind": "tree"}, "max_results": 1 }
                }
            }));
            let response = self.response(deadline.saturating_duration_since(Instant::now()));
            let message = response["result"]["content"][0]["text"]
                .as_str()
                .unwrap_or_default();
            if message.contains("unavailable") {
                assert_eq!(response["result"]["isError"], true);
                assert!(self.child.try_wait().expect("poll process").is_none());
                return;
            }
            assert!(
                Instant::now() < deadline,
                "runtime failure remained hidden behind startup state: {response}"
            );
            id += 1;
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    fn send(&mut self, message: serde_json::Value) {
        let stdin = self.stdin.as_mut().expect("live MCP stdin");
        serde_json::to_writer(&mut *stdin, &message).expect("write MCP message");
        stdin.write_all(b"\n").expect("terminate MCP message");
        stdin.flush().expect("flush MCP message");
    }

    fn send_raw_line(&mut self, line: &str) {
        let stdin = self.stdin.as_mut().expect("live MCP stdin");
        stdin.write_all(line.as_bytes()).expect("write raw MCP line");
        stdin.write_all(b"\n").expect("terminate raw MCP line");
        stdin.flush().expect("flush raw MCP line");
    }

    fn send_raw(&mut self, bytes: &[u8]) {
        let stdin = self.stdin.as_mut().expect("live MCP stdin");
        stdin.write_all(bytes).expect("write raw MCP bytes");
        stdin.flush().expect("flush raw MCP bytes");
    }

    fn message(&self, timeout: Duration) -> serde_json::Value {
        let line = self
            .lines
            .recv_timeout(timeout)
            .expect("MCP message before deadline");
        serde_json::from_str(&line).expect("MCP JSON message")
    }

    fn response(&self, timeout: Duration) -> serde_json::Value {
        let deadline = Instant::now() + timeout;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            let value = self.message(remaining);
            if value.get("id").is_some() {
                return value;
            }
        }
    }

    fn stop(&mut self) {
        self.stdin.take();
        if self.child.try_wait().expect("poll child").is_none() {
            self.child.kill().expect("kill MCP child");
        }
        self.child.wait().expect("join MCP child");
    }

    fn kill_now(&mut self) {
        self.child.kill().expect("kill MCP child");
        self.child.wait().expect("join killed MCP child");
        self.stdin.take();
    }
}

impl Drop for McpProcess {
    fn drop(&mut self) {
        self.stdin.take();
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
        }
        let _ = self.child.wait();
        if let Some(task) = self.stderr_task.take() {
            let _ = task.join();
        }
    }
}

fn wait_until(timeout: Duration, mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if condition() {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("condition not met within {timeout:?}");
}

fn write_rust_fixture_set(
    root: &std::path::Path,
    prefix: &str,
    file_count: usize,
    functions_per_file: usize,
) {
    for file in 0..file_count {
        let content = (0..functions_per_file)
            .map(|function| format!("fn item_{file}_{function}() -> usize {{ {function} }}\n"))
            .collect::<String>();
        std::fs::write(root.join(format!("{prefix}_{file}.rs")), content)
            .expect("write generated Rust fixture");
    }
}

fn database_state(database: &std::path::Path) -> Option<(u64, u64, bool)> {
    let connection = rusqlite::Connection::open_with_flags(
        database,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .ok()?;
    connection.busy_timeout(Duration::from_millis(50)).ok()?;
    let generation = connection
        .query_row(
            "SELECT repository_generation FROM meta WHERE id = 1",
            [],
            |row| row.get::<_, i64>(0),
        )
        .ok()
        .and_then(|value| u64::try_from(value).ok())?;
    let files = connection
        .query_row("SELECT count(*) FROM files", [], |row| row.get::<_, i64>(0))
        .ok()
        .and_then(|value| u64::try_from(value).ok())?;
    let changed = connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM chunks WHERE content LIKE '%changed_after_failover%')",
            [],
            |row| row.get::<_, bool>(0),
        )
        .ok()?;
    Some((generation, files, changed))
}
