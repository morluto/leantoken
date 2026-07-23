use std::{
    io::{BufRead, BufReader, Read, Write},
    process::{Child, ChildStdin, Stdio},
    sync::mpsc,
    time::{Duration, Instant},
};

use assert_cmd::Command;
use clap::Parser;
use wait_timeout::ChildExt;

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
        search["meta"]["emitted_tokens"]
            .as_u64()
            .is_some_and(|value| value <= 100)
    );

    let savings = run(root.path(), &database, &["savings"]);
    assert_eq!(savings["tracked_requests"], 1);
    assert_eq!(savings["by_operation"][0]["operation"], "search");
    assert_eq!(savings["by_operation"][0]["tracked_requests"], 1);
    assert!(
        savings["estimated_source_tokens_saved"]
            .as_u64()
            .is_some()
    );
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
    assert_eq!(report["server_version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(report["instructions_loaded"], true);
    assert_eq!(report["tools"].as_array().map(Vec::len), Some(5));
    assert_eq!(report["first_call"]["status"], "ready");
    assert!(
        report["first_call"]["attempts"]
            .as_u64()
            .is_some_and(|attempts| attempts >= 1)
    );
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
    assert!(stdout.contains("Tool catalog: 5 retrieval tools"));
    assert!(stdout.contains("leantoken_context first"));
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

    // rmcp intentionally ignores unparsable input, but a well-formed value
    // with the wrong JSON-RPC shape receives Invalid Request. Neither may
    // close the stdio transport or poison the next tool call.
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
            "name": "leantoken_files",
            "arguments": { "operation": {"kind": "tree"}, "max_results": 1 }
        }
    }));
    let response = process.response(Duration::from_secs(10));
    assert_eq!(response["id"], 100);
    assert!(response.get("result").is_some(), "{response}");
    assert!(process.child.try_wait().expect("poll process").is_none());
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
    assert_eq!(
        initialize["result"]["serverInfo"]["version"],
        env!("CARGO_PKG_VERSION")
    );
    assert!(
        initialize["result"]["instructions"]
            .as_str()
            .is_some_and(|instructions| instructions.contains("call leantoken_context first"))
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
            "leantoken_context",
            "leantoken_files",
            "leantoken_outline",
            "leantoken_read",
            "leantoken_search",
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
                "name": "leantoken_context",
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
        assert!(saw_retryable, "cold first call never exposed retry guidance");
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
                "name": "leantoken_files",
                "arguments": { "operation": {"kind": "tree"}, "max_results": 1 }
            }
        }));
        let response = process.response(deadline.saturating_duration_since(Instant::now()));
        let message = response["result"]["content"][0]["text"]
            .as_str()
            .unwrap_or_default();
        if message.contains("unavailable") {
            assert_eq!(response["result"]["isError"], true);
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
                "name": "leantoken_files",
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
            "name": "leantoken_files",
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
fn mcp_follower_rebuilds_after_leader_is_killed_during_reconciliation() {
    let root = tempfile::tempdir().expect("temporary repository");
    std::fs::write(root.path().join("old.rs"), "fn committed_before_crash() {}\n")
        .expect("old fixture");
    let database = root.path().join("index.sqlite");
    let initial = run(root.path(), &database, &["index"]);
    assert_eq!(initial["repository_generation"], 1);
    assert_eq!(database_state(&database).map(|state| state.1), Some(1));

    write_rust_fixture_set(root.path(), "new", 40, 150);

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
            generation == 2 && files == 41
        })
    });
    follower.wait_until_ready(Duration::from_secs(5));
}

#[test]
fn setup_and_remove_do_not_require_a_repository() {
    let temp = tempfile::tempdir().expect("temporary home");
    let missing_root = temp.path().join("not-a-repository");

    let setup = Command::cargo_bin("leantoken")
        .expect("binary")
        .env("HOME", temp.path())
        .env("USERPROFILE", temp.path())
        .args([
            "--root",
            missing_root.to_str().expect("root UTF-8"),
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
fn setup_requires_yes_before_non_interactive_mutation() {
    let temp = tempfile::tempdir().expect("temporary home");
    let output = Command::cargo_bin("leantoken")
        .expect("binary")
        .env("HOME", temp.path())
        .env("USERPROFILE", temp.path())
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

    let human_list = command()
        .args(["cache", "list"])
        .output()
        .expect("human cache list");
    assert!(human_list.status.success());
    let human_list = String::from_utf8_lossy(&human_list.stdout);
    assert!(human_list.contains("corrupt"));
    assert!(human_list.contains("last_access="));
    assert!(human_list.contains("root_available="));

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
    assert_eq!(dry_run["results"][0]["action"], "would_delete");
    assert!(database.exists());

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
    assert_eq!(prune["results"][0]["action"], "deleted");
    assert!(!database.exists());
}

#[test]
fn setup_dry_run_reports_exact_plan_without_mutation() {
    let temp = tempfile::tempdir().expect("temporary home");
    let output = Command::cargo_bin("leantoken")
        .expect("binary")
        .env("HOME", temp.path())
        .env("USERPROFILE", temp.path())
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
        .args(["--json", "setup", "--claude", "--cursor", "--yes"])
        .output()
        .expect("run setup");
    assert!(!output.status.success());
    assert!(!temp.path().join(".cursor/mcp.json").exists());
    let error: serde_json::Value =
        serde_json::from_slice(&output.stderr).expect("structured setup error");
    assert_eq!(error["category"], "internal_error");
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
    let npm = runtime.join("npm cli.js");
    let setup = Command::cargo_bin("leantoken")
        .expect("binary")
        .env("HOME", temp.path())
        .env("USERPROFILE", temp.path())
        .env("npm_lifecycle_event", "npx")
        .env("npm_node_execpath", &node)
        .env("npm_execpath", &npm)
        .args(["--json", "setup", "--claude", "--yes"])
        .output()
        .expect("run npx setup");
    assert!(
        setup.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&setup.stderr)
    );
    let report: serde_json::Value =
        serde_json::from_slice(&setup.stdout).expect("setup JSON output");
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
            npm.to_str().unwrap(),
            "exec",
            "--yes",
            format!("--package=leantoken@{}", env!("CARGO_PKG_VERSION")),
            "--",
            "leantoken",
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
    assert!(setup.status.success());
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
        refresh.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&refresh.stderr)
    );
    let report: serde_json::Value =
        serde_json::from_slice(&refresh.stdout).expect("refresh JSON output");
    assert_eq!(report["plan"].as_array().unwrap().len(), 1);
    assert_eq!(report["plan"][0]["client"], "claude");
    assert_eq!(report["plan"][0]["action"], "already_current");
    let cursor = std::fs::read_to_string(temp.path().join(".cursor/mcp.json"))
        .expect("Cursor config after refresh");
    assert!(!cursor.contains("\"leantoken\""));
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
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("LeanToken // Context Distillery"));
    assert!(stdout.contains("LeanToken is configured for 1 client."));
    assert!(stdout.contains("Restart or reload"));
    assert!(stdout.contains(&format!(
        "npx leantoken@{} doctor",
        env!("CARGO_PKG_VERSION")
    )));
    assert!(stdout.contains("no global `leantoken` command was installed"));
    assert!(stdout.contains("npx --yes leantoken@latest setup --refresh --yes"));
    assert!(stdout.contains(&format!(
        "pinned to LeanToken v{}",
        env!("CARGO_PKG_VERSION")
    )));
    assert!(stdout.contains("npm install --global leantoken@latest"));
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
        let mut command = std::process::Command::new(assert_cmd::cargo::cargo_bin!("leantoken"));
        command
            .args([
                "--root",
                root.to_str().expect("root UTF-8"),
                "--database",
                database.to_str().expect("database UTF-8"),
            ])
            .args(arguments)
            .arg("mcp");
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
        self.send(serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-11-25",
                "capabilities": {},
                "clientInfo": { "name": "leantoken-test", "version": "1" }
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
                    "name": "leantoken_files",
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
                    "name": "leantoken_files",
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
