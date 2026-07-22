use std::sync::Arc;

use leantoken::{
    Config, ContextRequest, mcp::LeanTokenMcp, services::Services,
};
use rmcp::{
    RoleClient,
    model::{CallToolRequestParams, CallToolResult, ClientRequest, ErrorCode, Request},
    serve_client, serve_server,
    service::{Peer, PeerRequestOptions, ServiceError},
};

async fn call_tool(
    peer: &Peer<RoleClient>,
    tool: &'static str,
    arguments: serde_json::Value,
) -> Result<CallToolResult, ServiceError> {
    let arguments = arguments
        .as_object()
        .expect("tool arguments object")
        .clone();
    peer.call_tool(CallToolRequestParams::new(tool).with_arguments(arguments))
        .await
}

async fn assert_mcp_limit_contract(
    peer: &Peer<RoleClient>,
    tool: &'static str,
    base_arguments: serde_json::Value,
    field: &'static str,
    limit: usize,
    zero_is_valid: bool,
) {
    let default = call_tool(peer, tool, base_arguments.clone())
        .await
        .expect("omitted limit should use its default");
    assert_ne!(default.is_error, Some(true));

    for requested in [0, 1, limit, limit + 1] {
        let mut arguments = base_arguments.clone();
        arguments[field] = serde_json::json!(requested);
        let result = call_tool(peer, tool, arguments).await;
        if requested == 0 && !zero_is_valid {
            let ServiceError::McpError(error) = result.expect_err("zero must be rejected") else {
                panic!("zero returned a non-MCP error");
            };
            assert_eq!(error.code, ErrorCode::INVALID_PARAMS);
            assert_eq!(
                error.data,
                Some(serde_json::json!({
                    "category": "invalid_input",
                    "field": field,
                }))
            );
        } else if requested > limit {
            let ServiceError::McpError(error) =
                result.expect_err("oversized limit must be rejected")
            else {
                panic!("oversized limit returned a non-MCP error");
            };
            assert_eq!(error.code, ErrorCode::INVALID_PARAMS);
            assert_eq!(
                error.data,
                Some(serde_json::json!({
                    "category": "request_limit_exceeded",
                    "field": field,
                    "requested": requested,
                    "limit": limit,
                }))
            );
        } else {
            let response = result.expect("in-range limit should succeed");
            assert_ne!(response.is_error, Some(true));
        }
    }
}

#[tokio::test]
async fn mcp_transport_enforces_request_limit_boundaries() {
    let root = tempfile::tempdir().expect("temporary repository");
    std::fs::write(
        root.path().join("lib.rs"),
        "pub fn answer() -> u8 { 42 }\npub fn caller() -> u8 { answer() }\n",
    )
    .expect("write fixture");
    let config =
        Config::discover(root.path(), Some(root.path().join("index.sqlite"))).expect("config");
    let services = Arc::new(Services::open(config).expect("services"));
    services.index(false).await.expect("index fixture");

    let (client_stream, server_stream) = tokio::io::duplex(64 * 1024);
    let server_start = tokio::spawn(async move {
        serve_server(LeanTokenMcp::new(services), server_stream)
            .await
            .expect("start MCP server")
    });
    let mut client = serve_client((), client_stream)
        .await
        .expect("initialize MCP client");
    let mut server = server_start.await.expect("join server startup");

    assert_mcp_limit_contract(
        client.peer(),
        "leantoken_files",
        serde_json::json!({"operation": {"kind": "tree", "depth": 0}}),
        "max_results",
        100,
        false,
    )
    .await;
    assert_mcp_limit_contract(
        client.peer(),
        "leantoken_search",
        serde_json::json!({"query": "answer", "mode": "text"}),
        "max_results",
        100,
        false,
    )
    .await;
    assert_mcp_limit_contract(
        client.peer(),
        "leantoken_search",
        serde_json::json!({"query": "answer", "mode": "text"}),
        "max_tokens",
        32_000,
        false,
    )
    .await;
    assert_mcp_limit_contract(
        client.peer(),
        "leantoken_search",
        serde_json::json!({"query": "answer", "mode": "text"}),
        "context_lines",
        20,
        true,
    )
    .await;
    assert_mcp_limit_contract(
        client.peer(),
        "leantoken_outline",
        serde_json::json!({"paths": ["lib.rs"]}),
        "max_results",
        100,
        false,
    )
    .await;
    assert_mcp_limit_contract(
        client.peer(),
        "leantoken_outline",
        serde_json::json!({"paths": ["lib.rs"]}),
        "max_tokens",
        32_000,
        false,
    )
    .await;
    assert_mcp_limit_contract(
        client.peer(),
        "leantoken_read",
        serde_json::json!({
            "path": "lib.rs",
            "target": {"kind": "lines", "start": 1, "end": 1}
        }),
        "max_tokens",
        32_000,
        false,
    )
    .await;
    assert_mcp_limit_contract(
        client.peer(),
        "leantoken_context",
        serde_json::json!({"task": "find the answer definition"}),
        "token_budget",
        32_000,
        false,
    )
    .await;

    client.close().await.expect("close client");
    server.close().await.expect("close server");
}

#[tokio::test]
async fn omitted_mcp_limits_use_customized_service_defaults() {
    let root = tempfile::tempdir().expect("temporary repository");
    std::fs::write(
        root.path().join("lib.rs"),
        "fn before() {}\npub fn answer() -> u8 { 42 }\nfn after() {}\n",
    )
    .expect("write fixture");
    let mut config =
        Config::discover(root.path(), Some(root.path().join("index.sqlite"))).expect("config");
    config.default_results = 1;
    config.max_results = 1;
    config.default_read_tokens = 50;
    config.max_output_tokens = 50;
    config.context_lines = 0;
    let services = Arc::new(Services::open(config).expect("services"));
    services.index(false).await.expect("index fixture");

    let (client_stream, server_stream) = tokio::io::duplex(64 * 1024);
    let server_start = tokio::spawn(async move {
        serve_server(LeanTokenMcp::new(services), server_stream)
            .await
            .expect("start MCP server")
    });
    let mut client = serve_client((), client_stream)
        .await
        .expect("initialize MCP client");
    let mut server = server_start.await.expect("join server startup");

    let files = call_tool(
        client.peer(),
        "leantoken_files",
        serde_json::json!({"operation": {"kind": "tree"}}),
    )
    .await
    .expect("files with configured default");
    assert_eq!(
        files.structured_content.as_ref().and_then(|value| value["entries"].as_array()).map(Vec::len),
        Some(1)
    );

    let search = call_tool(
        client.peer(),
        "leantoken_search",
        serde_json::json!({"query": "answer", "mode": "text"}),
    )
    .await
    .expect("search with configured defaults");
    let hits = search
        .structured_content
        .as_ref()
        .and_then(|value| value["hits"].as_array())
        .expect("search hits");
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0]["start_line"], hits[0]["end_line"]);

    for (tool, arguments) in [
        ("leantoken_outline", serde_json::json!({"paths": ["lib.rs"]})),
        (
            "leantoken_read",
            serde_json::json!({
                "path": "lib.rs",
                "target": {"kind": "lines", "start": 2, "end": 2}
            }),
        ),
    ] {
        let response = call_tool(client.peer(), tool, arguments)
            .await
            .expect("tool with configured token default");
        assert_ne!(response.is_error, Some(true));
    }

    client.close().await.expect("close client");
    server.close().await.expect("close server");
}

#[tokio::test]
async fn sdk_transport_initializes_lists_calls_and_closes() {
    let root = tempfile::tempdir().expect("temporary repository");
    std::fs::write(root.path().join("lib.rs"), "pub fn answer() -> u8 { 42 }\n")
        .expect("write fixture");
    std::fs::write(
        root.path().join("many.rs"),
        (0..2_000)
            .map(|index| format!("fn answer_{index}() {{ answer(); }}\n"))
            .collect::<String>(),
    )
    .expect("write large fixture");
    let config =
        Config::discover(root.path(), Some(root.path().join("index.sqlite"))).expect("config");
    let services = Arc::new(Services::open(config).expect("services"));
    services.index(false).await.expect("index fixture");

    let (client_stream, server_stream) = tokio::io::duplex(64 * 1024);
    let server = LeanTokenMcp::new(services);
    let server_start = tokio::spawn(async move {
        serve_server(server, server_stream)
            .await
            .expect("start MCP server")
    });
    let mut client = serve_client((), client_stream)
        .await
        .expect("initialize MCP client");
    let mut server = server_start.await.expect("join server startup");

    let server_info = client.peer().peer_info().expect("server initialize result");
    assert_eq!(server_info.server_info.name, "leantoken");
    assert_eq!(server_info.server_info.version, env!("CARGO_PKG_VERSION"));
    let instructions = server_info
        .instructions
        .clone()
        .expect("server instructions");
    assert!(instructions.contains("preferred repository discovery"));
    assert!(instructions.contains("call leantoken_context first"));
    assert!(instructions.contains("leantoken_search over grep or rg"));
    assert!(instructions.contains("consistency=working_tree"));
    assert!(instructions.contains("native tools for edits, builds, tests"));

    let tools = client.peer().list_all_tools().await.expect("list tools");
    let names = tools
        .iter()
        .map(|tool| tool.name.as_ref())
        .collect::<std::collections::HashSet<_>>();
    assert_eq!(tools.len(), 5);
    for name in [
        "leantoken_files",
        "leantoken_search",
        "leantoken_outline",
        "leantoken_read",
        "leantoken_context",
    ] {
        assert!(names.contains(name));
    }

    let files_arguments = serde_json::json!({
        "operation": {"kind": "tree", "depth": 2},
        "max_results": 10
    })
        .as_object()
        .expect("request object")
        .clone();
    let response = client
        .peer()
        .call_tool(
            CallToolRequestParams::new("leantoken_files").with_arguments(files_arguments.clone()),
        )
        .await
        .expect("call files tool");
    assert_ne!(response.is_error, Some(true));
    let structured = response.structured_content.expect("structured response");
    assert_eq!(structured["entries"][0]["path"], "lib.rs");

    let legacy_files_arguments = serde_json::json!({"operation": "tree"})
        .as_object()
        .expect("legacy files arguments")
        .clone();
    let legacy_result = client
        .peer()
        .call_tool(
            CallToolRequestParams::new("leantoken_files")
                .with_arguments(legacy_files_arguments),
        )
        .await
        .expect("legacy arguments receive an MCP tool result");
    assert_eq!(legacy_result.is_error, Some(true));
    assert!(legacy_result.content[0]
        .as_text()
        .is_some_and(|text| text.text.contains("failed to deserialize parameters")));

    std::fs::write(
        root.path().join("new_package.rs"),
        "pub fn newly_committed_package() {}\n",
    )
    .expect("write source after initial index");
    let working_tree_arguments = serde_json::json!({
        "query": "newly_committed_package",
        "mode": "identifier",
        "max_results": 5,
        "max_tokens": 100,
        "consistency": "working_tree"
    })
    .as_object()
    .expect("working-tree search arguments")
    .clone();
    let response = client
        .peer()
        .call_tool(
            CallToolRequestParams::new("leantoken_search")
                .with_arguments(working_tree_arguments),
        )
        .await
        .expect("working-tree search");
    assert_ne!(response.is_error, Some(true));
    let structured = response.structured_content.expect("structured response");
    assert_eq!(structured["hits"][0]["path"], "new_package.rs");

    let invalid_arguments = serde_json::json!({
        "path": "../secret",
        "target": {"kind": "lines", "start": 1, "end": 1}
    })
        .as_object()
        .expect("invalid read arguments")
        .clone();
    let error = client
        .peer()
        .call_tool(CallToolRequestParams::new("leantoken_read").with_arguments(invalid_arguments))
        .await
        .expect_err("invalid path should be a protocol error");
    assert!(matches!(
        error,
        ServiceError::McpError(data)
            if data.code == ErrorCode::INVALID_PARAMS
                && data.data.as_ref().and_then(|value| value["category"].as_str())
                    == Some("path_outside_root")
    ));

    let oversized_arguments = serde_json::json!({
        "query": "x".repeat(65 * 1024),
        "mode": "text",
        "max_results": 1,
        "max_tokens": 10
    })
    .as_object()
    .expect("oversized search arguments")
    .clone();
    let error = client
        .peer()
        .call_tool(
            CallToolRequestParams::new("leantoken_search").with_arguments(oversized_arguments),
        )
        .await
        .expect_err("oversized request should be rejected");
    assert!(matches!(
        error,
        ServiceError::McpError(data)
            if data.code == ErrorCode::INVALID_PARAMS
                && data.data.as_ref().and_then(|value| value["category"].as_str())
                    == Some("input_too_long")
    ));

    let bounded_arguments = serde_json::json!({
        "query": "answer",
        "mode": "text",
        "max_results": 100,
        "max_tokens": 50
    })
    .as_object()
    .expect("bounded search arguments")
    .clone();
    let bounded = client
        .peer()
        .call_tool(
            CallToolRequestParams::new("leantoken_search").with_arguments(bounded_arguments),
        )
        .await
        .expect("large bounded search");
    assert!(
        bounded
            .structured_content
            .as_ref()
            .and_then(|value| value.pointer("/meta/emitted_tokens"))
            .and_then(serde_json::Value::as_u64)
            .is_some_and(|tokens| tokens <= 50)
    );

    let default_context_arguments = serde_json::json!({
        "task": "find the answer definition"
    })
    .as_object()
    .expect("default context arguments")
    .clone();
    let default_context = client
        .peer()
        .call_tool(
            CallToolRequestParams::new("leantoken_context")
                .with_arguments(default_context_arguments),
        )
        .await
        .expect("context with default token budget");
    assert_ne!(default_context.is_error, Some(true));
    assert!(
        default_context
            .structured_content
            .as_ref()
            .and_then(|value| value.pointer("/meta/emitted_tokens"))
            .and_then(serde_json::Value::as_u64)
            .is_some_and(|tokens| tokens <= 3_000)
    );

    let context = ContextRequest {
        task: "find answer and its caller".into(),
        token_budget: 100,
        focus_paths: Vec::new(),
        focus_symbols: Vec::new(),
        exclude_paths: Vec::new(),
        known_hashes: Vec::new(),
        prior_repository_generation: None,
    base_revision: None,
    changed_paths: Vec::new(),
    };
    let context_arguments = serde_json::to_value(context)
        .expect("serialize context request")
        .as_object()
        .expect("context request object")
        .clone();
    let request = ClientRequest::CallToolRequest(Request::new(
        CallToolRequestParams::new("leantoken_context").with_arguments(context_arguments),
    ));
    let handle = client
        .peer()
        .send_cancellable_request(request, PeerRequestOptions::no_options())
        .await
        .expect("send cancellable context request");
    handle
        .cancel(Some("integration test cancellation".into()))
        .await
        .expect("cancel context request");

    // A cancelled request must not poison the stdio transport or server.
    let response = client
        .peer()
        .call_tool(CallToolRequestParams::new("leantoken_files").with_arguments(files_arguments))
        .await
        .expect("call after cancellation");
    assert_ne!(response.is_error, Some(true));

    client.close().await.expect("close client");
    server.close().await.expect("close server");
}

#[cfg(unix)]
#[tokio::test]
async fn mcp_path_errors_redact_external_and_absolute_paths() {
    use std::os::unix::fs::symlink;

    let root = tempfile::tempdir().expect("temporary repository");
    let outside = tempfile::tempdir().expect("external directory");
    let indexed_path = root.path().join("escape.rs");
    let external_path = outside.path().join("private-secret-target.rs");
    std::fs::write(&indexed_path, "fn indexed_before_escape() {}\n").expect("indexed fixture");
    std::fs::write(&external_path, "fn private_secret() {}\n").expect("external fixture");
    let config =
        Config::discover(root.path(), Some(root.path().join("index.sqlite"))).expect("config");
    let services = Arc::new(Services::open(config).expect("services"));
    services.index(false).await.expect("index fixture");
    std::fs::remove_file(&indexed_path).expect("remove indexed fixture");
    symlink(&external_path, &indexed_path).expect("external symlink");

    let (client_stream, server_stream) = tokio::io::duplex(64 * 1024);
    let server = LeanTokenMcp::new(services);
    let server_start = tokio::spawn(async move {
        serve_server(server, server_stream)
            .await
            .expect("start MCP server")
    });
    let mut client = serve_client((), client_stream)
        .await
        .expect("initialize MCP client");
    let mut server = server_start.await.expect("join server startup");

    for requested in [
        "escape.rs",
        "/home/alice/private-secret.rs",
        r"C:\Users\alice\private-secret.rs",
    ] {
        let arguments = serde_json::json!({
            "path": requested,
            "target": {"kind": "lines", "start": 1, "end": 1}
        })
        .as_object()
        .expect("read arguments")
        .clone();
        let error = client
            .peer()
            .call_tool(CallToolRequestParams::new("leantoken_read").with_arguments(arguments))
            .await
            .expect_err("path must be rejected");
        let ServiceError::McpError(data) = error else {
            panic!("unexpected service error: {error}");
        };
        assert_eq!(data.code, ErrorCode::INVALID_PARAMS);
        assert_eq!(
            data.data
                .as_ref()
                .and_then(|value| value["category"].as_str()),
            Some("path_outside_root")
        );
        let wire = serde_json::to_string(&data).expect("serialize error");
        for secret in [
            requested,
            external_path.to_str().expect("external UTF-8"),
            "private-secret",
            "/home/alice",
            r"C:\Users\alice",
        ] {
            assert!(!wire.contains(secret), "MCP error leaked {secret}: {wire}");
        }
    }

    client.close().await.expect("close client");
    server.close().await.expect("close server");
}

#[tokio::test]
async fn pending_and_empty_indexes_return_successful_retry_guidance() {
    let root = tempfile::tempdir().expect("temporary repository");
    std::fs::write(root.path().join("lib.rs"), "pub fn answer() -> u8 { 42 }\n")
        .expect("write fixture");
    let config =
        Config::discover(root.path(), Some(root.path().join("index.sqlite"))).expect("config");

    let (server, state) = LeanTokenMcp::pending();
    let (client_stream, server_stream) = tokio::io::duplex(64 * 1024);
    let server_start = tokio::spawn(async move {
        serve_server(server, server_stream)
            .await
            .expect("start MCP server")
    });
    let mut client = serve_client((), client_stream)
        .await
        .expect("initialize MCP client");
    let mut server = server_start.await.expect("join server startup");

    for (tool, arguments, field, limit, zero_is_valid) in [
        (
            "leantoken_files",
            serde_json::json!({"operation": {"kind": "tree", "depth": 0}}),
            "max_results",
            100,
            false,
        ),
        (
            "leantoken_search",
            serde_json::json!({"query": "answer", "mode": "text"}),
            "max_results",
            100,
            false,
        ),
        (
            "leantoken_search",
            serde_json::json!({"query": "answer", "mode": "text"}),
            "max_tokens",
            32_000,
            false,
        ),
        (
            "leantoken_search",
            serde_json::json!({"query": "answer", "mode": "text"}),
            "context_lines",
            20,
            true,
        ),
        (
            "leantoken_outline",
            serde_json::json!({"paths": ["lib.rs"]}),
            "max_results",
            100,
            false,
        ),
        (
            "leantoken_outline",
            serde_json::json!({"paths": ["lib.rs"]}),
            "max_tokens",
            32_000,
            false,
        ),
        (
            "leantoken_read",
            serde_json::json!({
                "path": "lib.rs",
                "target": {"kind": "lines", "start": 1, "end": 1}
            }),
            "max_tokens",
            32_000,
            false,
        ),
        (
            "leantoken_context",
            serde_json::json!({"task": "find the answer definition"}),
            "token_budget",
            32_000,
            false,
        ),
    ] {
        assert_mcp_limit_contract(
            client.peer(),
            tool,
            arguments,
            field,
            limit,
            zero_is_valid,
        )
        .await;
    }

    let request = || {
        let arguments = serde_json::json!({ "operation": {"kind": "tree"} })
            .as_object()
            .expect("arguments")
            .clone();
        CallToolRequestParams::new("leantoken_files").with_arguments(arguments)
    };

    let starting = client
        .peer()
        .call_tool(request())
        .await
        .expect("starting result");
    assert_eq!(starting.is_error, Some(false));
    assert_eq!(
        starting.structured_content.as_ref().and_then(|value| value["reason"].as_str()),
        Some("index_starting")
    );

    let services = Arc::new(Services::open(config).expect("services"));
    state.set_ready(Arc::clone(&services));
    let building = client
        .peer()
        .call_tool(request())
        .await
        .expect("building result");
    assert_eq!(building.is_error, Some(false));
    assert_eq!(
        building.structured_content.as_ref().and_then(|value| value["reason"].as_str()),
        Some("index_building")
    );

    services.index(false).await.expect("index");
    let ready = client
        .peer()
        .call_tool(request())
        .await
        .expect("ready result");
    assert_ne!(ready.is_error, Some(true));

    state.set_failed();
    let failed = client
        .peer()
        .call_tool(request())
        .await
        .expect("failed result");
    assert_eq!(failed.is_error, Some(true));
    assert!(
        failed.content[0]
            .as_text()
            .is_some_and(|text| text.text.contains("unavailable"))
    );

    client.close().await.expect("close client");
    server.close().await.expect("close server");
}
