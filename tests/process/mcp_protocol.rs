use super::support::{Command, Duration, McpProcess};

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
fn mcp_approved_repository_contexts_are_isolated() {
    let primary_root = tempfile::tempdir().expect("primary repository");
    let docs_root = tempfile::tempdir().expect("approved repository");
    std::fs::write(
        primary_root.path().join("primary.rs"),
        "pub fn primary_marker() -> u8 { 1 }\n",
    )
    .expect("primary source");
    std::fs::write(
        docs_root.path().join("docs.rs"),
        "pub fn docs_marker() -> u8 { 2 }\n",
    )
    .expect("approved source");
    std::fs::write(
        primary_root.path().join(".leantoken.toml"),
        format!(
            "[repository_contexts.docs]\nroot = {:?}\nallow_external = true\n",
            docs_root.path().to_string_lossy()
        ),
    )
    .expect("approved context config");
    let database = primary_root.path().join("index.sqlite");
    let mut process = McpProcess::spawn(primary_root.path(), &database);
    process.initialize();
    process.send_initialized();
    process.wait_until_ready(Duration::from_secs(30));

    process.send(serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1201,
        "method": "tools/call",
        "params": {
            "name": "files",
            "arguments": {
                "repository_context": "docs",
                "operation": {"kind": "tree", "path": ".", "max_results": 10}
            }
        }
    }));
    let docs_response = process.response(Duration::from_secs(30));
    assert_eq!(docs_response["id"], 1201);
    assert!(
        docs_response.to_string().contains("docs.rs"),
        "{docs_response}"
    );

    process.send(serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1202,
        "method": "tools/call",
        "params": {
            "name": "files",
            "arguments": {"operation": {"kind": "tree", "path": ".", "max_results": 10}}
        }
    }));
    let primary_response = process.response(Duration::from_secs(30));
    assert_eq!(primary_response["id"], 1202);
    assert!(
        primary_response.to_string().contains("primary.rs"),
        "{primary_response}"
    );
    assert_ne!(
        docs_response["result"]["structuredContent"]["meta"]["repository_id"],
        primary_response["result"]["structuredContent"]["meta"]["repository_id"]
    );

    process.send(serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1203,
        "method": "tools/call",
        "params": {
            "name": "files",
            "arguments": {
                "repository_context": "unapproved",
                "operation": {"kind": "find", "query": "docs_marker"}
            }
        }
    }));
    let rejected = process.response(Duration::from_secs(10));
    assert_eq!(rejected["id"], 1203);
    assert_eq!(rejected["result"]["isError"], true);
    assert_eq!(
        rejected["result"]["structuredContent"]["category"],
        "invalid_input"
    );
    process.stop();
}

#[test]
fn mcp_survives_malformed_and_invalid_messages() {
    let root = tempfile::tempdir().expect("temporary repository");
    std::fs::write(root.path().join("lib.rs"), "pub fn answer() -> u8 { 42 }\n")
        .expect("write fixture");
    let database = root.path().join("index.sqlite");
    let mut process = McpProcess::spawn_with_captured_stderr(root.path(), &database, &[]);
    process.initialize();
    process.send_initialized();

    // Oversized terminated and initially unterminated frames are discarded
    // without closing the transport. RMCP's codec ignores unparsable input;
    // structurally invalid JSON-RPC receives Invalid Request. None may close
    // the stdio transport or poison the next tool call.
    process.send_raw(&vec![b'x'; 4 * 1024 * 1024 + 1]);
    process.send_raw_line("");
    process.send_raw_line("{not json");
    process.send_raw_line(r#"{"foo":"bar"}"#);
    let invalid = process.message(Duration::from_secs(10));
    assert_eq!(invalid["error"]["code"], -32600);

    process.send(serde_json::json!({
        "jsonrpc": "2.0",
        "id": 100,
        "method": "tools/call",
        "params": {
            "name": "files",
            "arguments": { "operation": {"kind": "tree", "max_results": 1} }
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
        let mut process =
            McpProcess::spawn_with_mcp_args(root.path(), &database, &["--result-mode", requested]);
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
                    "operation": {"kind": "tree", "max_results": 1}
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

    let mut process = McpProcess::spawn(root.path(), &database);
    process.initialize();
    process.send_initialized();
    process.wait_until_ready(Duration::from_secs(30));

    process.send(serde_json::json!({
        "jsonrpc": "2.0",
        "id": 910,
        "method": "tools/call",
        "params": {
            "name": "files",
            "arguments": {
                "operation": {
                    "kind": "find",
                    "query": "lib",
                    "max_results": 1,
                    "projection": "paths"
                }
            }
        }
    }));
    let files_response = process.response(Duration::from_secs(10));
    let files_result = &files_response["result"];
    let files = &files_result["structuredContent"];
    assert_eq!(files_result["isError"], false, "{files_response}");
    assert_eq!(files["paths"][0], "lib.rs", "{files_response}");
    assert!(
        files["paths"]
            .as_array()
            .is_some_and(|paths| !paths.is_empty())
    );
    assert!(files.get("entries").is_none(), "{files_response}");

    process.send(serde_json::json!({
        "jsonrpc": "2.0",
        "id": 911,
        "method": "tools/call",
        "params": {
            "name": "search",
            "arguments": {
                "operation": {
                    "kind": "identifier",
                    "query": "answer",
                    "max_results": 5,
                    "max_tokens": 1_000,
                    "projection": "compact"
                }
            }
        }
    }));
    let compact_response = process.response(Duration::from_secs(10));
    let compact_result = &compact_response["result"];
    let compact = &compact_result["structuredContent"];
    assert_eq!(compact_result["isError"], false, "{compact_response}");
    assert!(
        compact["hits"]
            .as_array()
            .is_some_and(|hits| !hits.is_empty()),
        "{compact_response}"
    );
    assert_eq!(compact["meta"]["source_tokens"], 0, "{compact_response}");
    assert!(
        compact["hits"][0].get("excerpt").is_none(),
        "{compact_response}"
    );

    process.send(serde_json::json!({
        "jsonrpc": "2.0",
        "id": 912,
        "method": "tools/call",
        "params": {
            "name": "search",
            "arguments": {
                "operation": {
                    "kind": "text",
                    "query": "answer",
                    "max_results": 5,
                    "max_tokens": 1_000,
                    "projection": "grouped"
                }
            }
        }
    }));
    let grouped_response = process.response(Duration::from_secs(10));
    let grouped_result = &grouped_response["result"];
    let grouped = &grouped_result["structuredContent"];
    assert_eq!(grouped_result["isError"], false, "{grouped_response}");
    assert!(
        grouped["groups"]
            .as_array()
            .is_some_and(|groups| !groups.is_empty()),
        "{grouped_response}"
    );
    assert!(
        grouped["groups"][0]["total_hits"]
            .as_u64()
            .unwrap_or_default()
            > 0
    );
    assert!(grouped.get("hits").is_none(), "{grouped_response}");

    process.send(serde_json::json!({
        "jsonrpc": "2.0",
        "id": 913,
        "method": "tools/call",
        "params": {
            "name": "outline",
            "arguments": {
                "paths": ["lib.rs"],
                "projection": "signatures"
            }
        }
    }));
    let outline_response = process.response(Duration::from_secs(10));
    let outline_result = &outline_response["result"];
    let outline = &outline_result["structuredContent"];
    assert_eq!(outline_result["isError"], false, "{outline_response}");
    assert!(
        outline["files"]
            .as_array()
            .is_some_and(|files| !files.is_empty()),
        "{outline_response}"
    );
    assert!(
        outline["files"][0]["signatures"]
            .as_array()
            .is_some_and(|signatures| !signatures.is_empty()),
        "{outline_response}"
    );
    assert!(
        outline["files"][0].get("symbols").is_none(),
        "{outline_response}"
    );
    assert!(
        outline["files"][0].get("imports").is_none(),
        "{outline_response}"
    );

    process.stop();
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
                "operation": {
                    "kind": "identifier",
                    "query": "persistent_receipt_answer",
                    "max_results": 5,
                    "max_tokens": 1_000
                }
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
                "operation": {
                    "kind": "identifier",
                    "query": "persistent_receipt_answer",
                    "max_results": 5,
                    "max_tokens": 1_000,
                    "receipt_id": receipt_id
                }
            }
        }
    }));
    let second_response = second.response(Duration::from_secs(10));
    let second_result = &second_response["result"]["structuredContent"];
    assert!(
        second_result["hits"].as_array().is_some_and(Vec::is_empty),
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
                "operation": {
                    "kind": "text",
                    "query": "persistent_query_receipt_answer",
                    "all_occurrences": true,
                    "coordinates_only": true,
                    "max_results": 100,
                    "max_tokens": 10_000,
                    "query_receipt": {"kind": "record"}
                }
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
                "operation": {
                    "kind": "text",
                    "query": "persistent_query_receipt_answer",
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

    let mut first = McpProcess::spawn_with_captured_stderr(root.path(), &database, &[]);
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
                "operation": {
                    "kind": "identifier",
                    "query": "cross_process_rebase_answer",
                    "max_results": 5,
                    "max_tokens": 1_000
                }
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
    let mut second = McpProcess::spawn_with_captured_stderr(root.path(), &database, &[]);
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

    let mut third = McpProcess::spawn_with_captured_stderr(root.path(), &database, &[]);
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
                "operation": {
                    "kind": "identifier",
                    "query": "cross_process_rebase_answer",
                    "max_results": 5,
                    "max_tokens": 1_000,
                    "receipt_id": rebased_receipt
                }
            }
        }
    }));
    let third_response = third.response(Duration::from_secs(10));
    let third_result = &third_response["result"]["structuredContent"];
    assert!(
        third_result["hits"].as_array().is_some_and(Vec::is_empty),
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
