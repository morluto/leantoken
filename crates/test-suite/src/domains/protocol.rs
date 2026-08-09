use std::sync::Arc;

use leantoken::{Config, ContextRequest, mcp::LeanTokenMcp, services::Services};
use rmcp::{
    ClientHandler, RoleClient,
    model::{
        CacheScope, CallToolRequestParams, CallToolResult, ClientInfo, ClientRequest, ErrorCode,
        ProtocolVersion, ReadResourceRequestParams, Request, ResourceContents, ResultType,
    },
    serve_client, serve_server,
    service::{Peer, PeerRequestOptions, ServiceError},
};

#[derive(Debug, Clone)]
struct ModernProtocolClient;

impl ClientHandler for ModernProtocolClient {
    fn get_info(&self) -> ClientInfo {
        let mut info = ClientInfo::default();
        info.protocol_version = ProtocolVersion::V_2026_07_28;
        info
    }
}

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

fn expect_tool_error(
    result: Result<CallToolResult, ServiceError>,
    category: &str,
) -> CallToolResult {
    let response = result.expect("semantic failure should be model-visible");
    assert_eq!(response.is_error, Some(true));
    assert_eq!(
        response
            .structured_content
            .as_ref()
            .and_then(|value| value["category"].as_str()),
        Some(category)
    );
    response
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
        if let Some(operation) = arguments
            .get_mut("operation")
            .and_then(serde_json::Value::as_object_mut)
        {
            operation.insert(field.into(), serde_json::json!(requested));
        } else {
            arguments[field] = serde_json::json!(requested);
        }
        let result = call_tool(peer, tool, arguments).await;
        if requested == 0 && !zero_is_valid {
            let error = expect_tool_error(result, "invalid_input");
            assert_eq!(
                error.structured_content,
                Some(serde_json::json!({
                    "category": "invalid_input",
                    "field": field,
                    "message": format!("invalid {field}: must be greater than zero"),
                    "status": "error",
                }))
            );
        } else if requested > limit {
            let error = expect_tool_error(result, "request_limit_exceeded");
            assert_eq!(
                error.structured_content,
                Some(serde_json::json!({
                    "category": "request_limit_exceeded",
                    "field": field,
                    "requested": requested,
                    "limit": limit,
                    "message": format!("{field} exceeds its configured limit"),
                    "status": "error",
                }))
            );
        } else {
            let response = result.expect("in-range limit should succeed");
            assert_ne!(response.is_error, Some(true));
        }
    }
}

async fn assert_mcp_limit_exceeded(
    peer: &Peer<RoleClient>,
    tool: &'static str,
    mut arguments: serde_json::Value,
    field: &'static str,
    requested: usize,
    limit: usize,
) {
    if let Some(operation) = arguments
        .get_mut("operation")
        .and_then(serde_json::Value::as_object_mut)
    {
        operation.insert(field.into(), serde_json::json!(requested));
    } else {
        arguments[field] = serde_json::json!(requested);
    }
    let error = expect_tool_error(
        call_tool(peer, tool, arguments).await,
        "request_limit_exceeded",
    );
    assert_eq!(
        error.structured_content,
        Some(serde_json::json!({
            "category": "request_limit_exceeded",
            "field": field,
            "requested": requested,
            "limit": limit,
            "message": format!("{field} exceeds its configured limit"),
            "status": "error",
        }))
    );
}

#[tokio::test]
async fn modern_rmcp_contract_uses_native_result_and_cache_fields() {
    let root = tempfile::tempdir().expect("temporary repository");
    std::fs::write(root.path().join("lib.rs"), "pub fn answer() -> u8 { 42 }\n")
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
    let mut client = serve_client(ModernProtocolClient, client_stream)
        .await
        .expect("start modern MCP client");
    let mut server = server_start.await.expect("join server startup");

    let tools = client
        .peer()
        .list_tools(None)
        .await
        .expect("list modern tools");
    assert_eq!(tools.ttl_ms, Some(0));
    assert_eq!(tools.cache_scope, Some(CacheScope::Public));
    let resources = client
        .peer()
        .list_resources(None)
        .await
        .expect("list modern resources");
    assert_eq!(resources.ttl_ms, Some(0));
    assert_eq!(resources.cache_scope, Some(CacheScope::Private));

    let response = call_tool(
        client.peer(),
        "files",
        serde_json::json!({"operation": {"kind": "tree", "max_results": 1}}),
    )
    .await
    .expect("call modern tool");
    assert_eq!(response.result_type, Some(ResultType::COMPLETE));
    assert!(response.content.is_empty());
    assert!(response.structured_content.is_some());

    client.close().await.expect("close client");
    server.close().await.expect("close server");
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
        "files",
        serde_json::json!({"operation": {"kind": "tree", "depth": 0}}),
        "max_results",
        100,
        false,
    )
    .await;
    assert_mcp_limit_contract(
        client.peer(),
        "search",
        serde_json::json!({"operation": {"kind": "text", "query": "answer"}}),
        "max_results",
        100,
        false,
    )
    .await;
    assert_mcp_limit_contract(
        client.peer(),
        "search",
        serde_json::json!({"operation": {"kind": "text", "query": "answer"}}),
        "max_tokens",
        32_000,
        false,
    )
    .await;
    assert_mcp_limit_contract(
        client.peer(),
        "search",
        serde_json::json!({"operation": {"kind": "text", "query": "answer"}}),
        "context_lines",
        20,
        true,
    )
    .await;
    assert_mcp_limit_contract(
        client.peer(),
        "outline",
        serde_json::json!({"paths": ["lib.rs"]}),
        "max_results",
        100,
        false,
    )
    .await;
    assert_mcp_limit_contract(
        client.peer(),
        "outline",
        serde_json::json!({"paths": ["lib.rs"]}),
        "max_tokens",
        32_000,
        false,
    )
    .await;
    assert_mcp_limit_contract(
        client.peer(),
        "read",
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
        "context",
        serde_json::json!({"task": "find the answer definition"}),
        "token_budget",
        32_000,
        false,
    )
    .await;
    assert_mcp_limit_contract(
        client.peer(),
        "context",
        serde_json::json!({"task": "find the answer definition"}),
        "max_fragments",
        100,
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
    config.default_context_tokens = 40;
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
        "files",
        serde_json::json!({"operation": {"kind": "tree"}}),
    )
    .await
    .expect("files with configured default");
    assert_eq!(
        files
            .structured_content
            .as_ref()
            .and_then(|value| value["entries"].as_array())
            .map(Vec::len),
        Some(1)
    );
    let repository_id = files
        .structured_content
        .as_ref()
        .and_then(|value| value.pointer("/meta/repository_id"))
        .and_then(serde_json::Value::as_str)
        .expect("repository identity")
        .to_owned();

    let search = call_tool(
        client.peer(),
        "search",
        serde_json::json!({
            "operation": {"kind": "text", "query": "answer"},
            "expected_repository_id": repository_id,
        }),
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

    expect_tool_error(
        call_tool(
            client.peer(),
            "search",
            serde_json::json!({
                "operation": {"kind": "text", "query": "answer"},
                "expected_repository_id": "different-repository",
            }),
        )
        .await,
        "repository_identity_mismatch",
    );

    for (tool, arguments) in [
        ("outline", serde_json::json!({"paths": ["lib.rs"]})),
        (
            "read",
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

    let context = call_tool(
        client.peer(),
        "context",
        serde_json::json!({
            "task": "find the answer definition",
            "workflow": "investigation",
        }),
    )
    .await
    .expect("context with configured token default");
    assert_ne!(context.is_error, Some(true));
    assert_eq!(
        context
            .structured_content
            .as_ref()
            .and_then(|value| value["workflow"].as_str()),
        Some("investigation")
    );
    assert!(
        context
            .structured_content
            .as_ref()
            .and_then(|value| value.pointer("/meta/source_tokens"))
            .and_then(serde_json::Value::as_u64)
            .is_some_and(|tokens| tokens <= 40)
    );

    client.close().await.expect("close client");
    server.close().await.expect("close server");
}

#[tokio::test(start_paused = true)]
async fn customized_mcp_limits_apply_while_starting_and_after_failure() {
    let root = tempfile::tempdir().expect("temporary repository");
    let mut config =
        Config::discover(root.path(), Some(root.path().join("index.sqlite"))).expect("config");
    config.default_results = 1;
    config.max_results = 1;
    config.default_read_tokens = 50;
    config.default_context_tokens = 40;
    config.max_output_tokens = 50;

    let (server, state) = LeanTokenMcp::pending();
    state.configure_limits(&config).expect("configured limits");
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

    let cases = [
        (
            "files",
            serde_json::json!({"operation": {"kind": "tree"}}),
            "max_results",
            2,
            1,
        ),
        (
            "search",
            serde_json::json!({"operation": {"kind": "text", "query": "answer"}}),
            "max_results",
            2,
            1,
        ),
        (
            "search",
            serde_json::json!({"operation": {"kind": "text", "query": "answer"}}),
            "max_tokens",
            51,
            50,
        ),
        (
            "outline",
            serde_json::json!({"paths": ["lib.rs"]}),
            "max_results",
            2,
            1,
        ),
        (
            "outline",
            serde_json::json!({"paths": ["lib.rs"]}),
            "max_tokens",
            51,
            50,
        ),
        (
            "read",
            serde_json::json!({
                "path": "lib.rs",
                "target": {"kind": "lines", "start": 1, "end": 1}
            }),
            "max_tokens",
            51,
            50,
        ),
        (
            "context",
            serde_json::json!({"task": "find answer"}),
            "token_budget",
            51,
            50,
        ),
    ];

    for (tool, arguments, field, requested, limit) in &cases {
        assert_mcp_limit_exceeded(
            client.peer(),
            tool,
            arguments.clone(),
            field,
            *requested,
            *limit,
        )
        .await;
    }

    let starting = call_tool(
        client.peer(),
        "files",
        serde_json::json!({"operation": {"kind": "tree", "max_results": 1}}),
    )
    .await
    .expect("valid starting request");
    assert_eq!(
        starting
            .structured_content
            .as_ref()
            .and_then(|value| value["reason"].as_str()),
        Some("index_starting")
    );

    state.set_failed(&leantoken::Error::McpRuntimeStopped);
    for (tool, arguments, field, requested, limit) in cases {
        assert_mcp_limit_exceeded(client.peer(), tool, arguments, field, requested, limit).await;
    }

    let failed = call_tool(
        client.peer(),
        "files",
        serde_json::json!({"operation": {"kind": "tree", "max_results": 1}}),
    )
    .await
    .expect("valid failed-state request");
    assert_eq!(failed.is_error, Some(true));

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
    assert_eq!(
        server_info
            .server_info
            .as_ref()
            .expect("server identity")
            .name,
        "leantoken"
    );
    assert!(server_info.capabilities.resources.is_some());
    let (runtime_version, contract_fingerprint) = server_info
        .server_info
        .as_ref()
        .expect("server identity")
        .version
        .split_once("+contract.")
        .expect("runtime version carries the MCP contract fingerprint");
    assert!(!runtime_version.is_empty());
    assert_eq!(contract_fingerprint.len(), 32);
    assert!(
        contract_fingerprint
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
    );
    let instructions = server_info
        .instructions
        .clone()
        .expect("server instructions");
    assert!(instructions.contains("preferred repository discovery"));
    assert!(instructions.contains("call leantoken.context once"));
    assert!(instructions.contains("plan_only=false"));
    assert!(instructions.contains("For a known scope"));
    assert!(instructions.contains("Use native tools for edits, builds, tests"));
    assert!(instructions.contains("consistency=reconcile_working_tree"));
    assert!(instructions.contains("status=retryable"));
    assert!(instructions.contains("configured repository_context names"));
    assert!(instructions.contains("Use savings for token statistics"));

    let tool_page = client
        .peer()
        .list_tools(None)
        .await
        .expect("list tools page");
    if server_info.protocol_version >= ProtocolVersion::V_2026_07_28 {
        assert_eq!(tool_page.ttl_ms, Some(0));
        assert_eq!(tool_page.cache_scope, Some(CacheScope::Public));
    } else {
        assert_eq!(tool_page.ttl_ms, None);
        assert_eq!(tool_page.cache_scope, None);
    }
    let tools = client.peer().list_all_tools().await.expect("list tools");
    let names = tools
        .iter()
        .map(|tool| tool.name.as_ref())
        .collect::<std::collections::BTreeSet<_>>();
    let expected_names = [
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
    .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(names, expected_names);
    for tool in &tools {
        let description = tool
            .description
            .as_deref()
            .unwrap_or_else(|| panic!("{} description missing", tool.name));
        assert!(
            (40..=1_024).contains(&description.len()),
            "{} description has {} bytes",
            tool.name,
            description.len()
        );
        assert_eq!(
            tool.input_schema.get("type"),
            Some(&serde_json::json!("object"))
        );
    }
    assert!(
        client
            .peer()
            .list_all_resources()
            .await
            .expect("list resources")
            .is_empty()
    );
    let templates = client
        .peer()
        .list_all_resource_templates()
        .await
        .expect("list resource templates");
    assert_eq!(templates.len(), 1);
    assert_eq!(
        templates[0].uri_template,
        "leantoken://receipt/v1/{receipt_id}"
    );
    assert_eq!(
        templates[0].mime_type.as_deref(),
        Some("application/vnd.leantoken.retrieval-receipt+json;version=1")
    );

    let files_arguments = serde_json::json!({
        "operation": {"kind": "tree", "depth": 2, "max_results": 10}
    })
    .as_object()
    .expect("request object")
    .clone();
    let response = client
        .peer()
        .call_tool(CallToolRequestParams::new("files").with_arguments(files_arguments.clone()))
        .await
        .expect("call files tool");
    assert_ne!(response.is_error, Some(true));
    let structured = response.structured_content.expect("structured response");
    assert_eq!(structured["entries"][0]["path"], "lib.rs");

    let response = call_tool(
        client.peer(),
        "search",
        serde_json::json!({
            "operation": {
                "kind": "text",
                "query": "answer",
                "all_occurrences": true,
                "include_paths": ["lib.rs"],
                "context_lines": 0,
                "max_results": 10
            }
        }),
    )
    .await
    .expect("call exhaustive search");
    assert_ne!(response.is_error, Some(true));
    let structured = response
        .structured_content
        .as_ref()
        .expect("occurrence response");
    assert!(structured.get("hits").is_none());
    assert_eq!(structured["groups_returned"], 1);
    assert_eq!(structured["occurrences_returned"], 1);
    assert_eq!(structured["occurrences_total"], 1);
    assert_eq!(structured["groups"][0]["occurrences"][0]["line"], 1);
    assert_eq!(structured["groups"][0]["occurrences"][0]["start_column"], 7);
    let receipt_uri = structured["receipt_resource"]["uri"]
        .as_str()
        .expect("receipt resource URI")
        .to_owned();
    assert_eq!(
        structured["receipt_resource"]["id"],
        structured["meta"]["receipt_id"]
    );
    assert!(response.content.is_empty());
    let receipt = client
        .peer()
        .read_resource(ReadResourceRequestParams::new(receipt_uri.clone()))
        .await
        .expect("read receipt resource");
    assert_eq!(receipt.contents.len(), 1);
    if server_info.protocol_version >= ProtocolVersion::V_2026_07_28 {
        assert_eq!(receipt.cache_scope, Some(CacheScope::Private));
        assert_eq!(receipt.ttl_ms, Some(0));
    } else {
        assert_eq!(receipt.cache_scope, None);
        assert_eq!(receipt.ttl_ms, None);
    }
    let ResourceContents::TextResourceContents {
        uri,
        mime_type,
        text,
        ..
    } = &receipt.contents[0]
    else {
        panic!("receipt resource must be JSON text");
    };
    assert_eq!(uri, &receipt_uri);
    assert_eq!(
        mime_type.as_deref(),
        Some("application/vnd.leantoken.retrieval-receipt+json;version=1")
    );
    let receipt_json: serde_json::Value =
        serde_json::from_str(text).expect("receipt resource JSON");
    assert_eq!(receipt_json["uri"], receipt_uri);
    assert_eq!(receipt_json["complete"], true);
    assert_eq!(receipt_json["source_free"], true);
    assert_eq!(
        receipt_json["evidence_count"].as_u64(),
        receipt_json["evidence"]
            .as_array()
            .map(|items| items.len() as u64)
    );
    let ServiceError::McpError(not_found) = client
        .peer()
        .read_resource(ReadResourceRequestParams::new(
            receipt_uri.to_ascii_uppercase(),
        ))
        .await
        .expect_err("altered receipt URI must not resolve")
    else {
        panic!("altered receipt URI returned a non-MCP error");
    };
    assert_eq!(
        not_found.code,
        if server_info.protocol_version >= ProtocolVersion::V_2026_07_28 {
            ErrorCode::INVALID_PARAMS
        } else {
            ErrorCode::RESOURCE_NOT_FOUND
        }
    );

    let response = call_tool(
        client.peer(),
        "search",
        serde_json::json!({
            "operation": {
                "kind": "text",
                "query": "answer",
                "all_occurrences": true,
                "coordinates_only": true,
                "include_paths": ["lib.rs"],
                "context_lines": 0,
                "max_results": 10,
                "query_receipt": {"kind": "record"}
            }
        }),
    )
    .await
    .expect("record exhaustive query coverage");
    assert_ne!(response.is_error, Some(true));
    let structured = response
        .structured_content
        .expect("recorded occurrence response");
    assert_eq!(structured["query_receipt"]["status"], "recorded");
    assert_eq!(structured["query_receipt"]["complete"], true);
    let query_receipt_id = structured["query_receipt"]["receipt_id"]
        .as_str()
        .expect("query receipt id")
        .to_owned();

    let response = call_tool(
        client.peer(),
        "search",
        serde_json::json!({
            "operation": {
                "kind": "text",
                "query": "answer",
                "all_occurrences": true,
                "coordinates_only": true,
                "include_paths": ["lib.rs"],
                "context_lines": 0,
                "max_results": 10,
                "query_receipt": {
                    "kind": "reuse",
                    "receipt_id": query_receipt_id
                }
            }
        }),
    )
    .await
    .expect("reuse exhaustive query coverage");
    assert_ne!(response.is_error, Some(true));
    let structured = response
        .structured_content
        .expect("reused occurrence response");
    assert_eq!(structured["query_receipt"]["status"], "already_covered");
    assert_eq!(structured["groups"], serde_json::json!([]));
    assert_eq!(structured["occurrences_returned"], 0);
    assert_eq!(structured["occurrences_total"], 1);

    let response = call_tool(
        client.peer(),
        "context",
        serde_json::json!({
            "task": "find the answer definition",
            "plan_only": true,
            "max_fragments": 2
        }),
    )
    .await
    .expect("call context plan");
    assert_ne!(response.is_error, Some(true));
    let structured = response.structured_content.expect("structured response");
    assert_eq!(structured["fragments"], serde_json::json!([]));
    assert_eq!(structured["meta"]["source_tokens"], 0);
    assert!(
        structured["plan"]["candidates"]
            .as_array()
            .is_some_and(|candidates| !candidates.is_empty())
    );
    assert!(
        structured["plan"]["candidates"]
            .as_array()
            .expect("plan candidates")
            .iter()
            .all(|candidate| candidate.get("content").is_none())
    );

    for (arguments, expected_path) in [
        (
            serde_json::json!({"operation": {"kind": "find", "query": "many"}}),
            "many.rs",
        ),
        (
            serde_json::json!({"operation": {"kind": "glob", "pattern": "lib.rs"}}),
            "lib.rs",
        ),
    ] {
        let response = call_tool(client.peer(), "files", arguments)
            .await
            .expect("call documented files operation");
        assert_ne!(response.is_error, Some(true));
        let entries = response
            .structured_content
            .and_then(|value| value["entries"].as_array().cloned())
            .expect("files entries");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0]["path"], expected_path);
    }

    let nested_files_arguments =
        serde_json::json!({"operation": {"kind": "find", "query": "many"}})
            .as_object()
            .expect("legacy files arguments")
            .clone();
    let legacy_result = client
        .peer()
        .call_tool(CallToolRequestParams::new("files").with_arguments(nested_files_arguments))
        .await
        .expect("nested arguments receive an MCP tool result");
    assert_ne!(legacy_result.is_error, Some(true));
    let entries = legacy_result
        .structured_content
        .and_then(|value| value["entries"].as_array().cloned())
        .expect("legacy files entries");
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0]["path"], "many.rs");

    std::fs::write(
        root.path().join("new_package.rs"),
        "pub fn newly_committed_package() {}\n",
    )
    .expect("write source after initial index");
    let reconcile_working_tree_arguments = serde_json::json!({
        "operation": {
            "kind": "identifier",
            "query": "newly_committed_package",
            "max_results": 5,
            "max_tokens": 100,
            "consistency": "reconcile_working_tree"
        }
    })
    .as_object()
    .expect("working-tree search arguments")
    .clone();
    let response = client
        .peer()
        .call_tool(
            CallToolRequestParams::new("search").with_arguments(reconcile_working_tree_arguments),
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
    let response = client
        .peer()
        .call_tool(CallToolRequestParams::new("read").with_arguments(invalid_arguments))
        .await
        .expect("invalid path should return a tool error result");
    assert_eq!(response.is_error, Some(true));
    assert!(
        !serde_json::to_string(&response)
            .expect("serialize bounded tool error")
            .contains("../secret")
    );

    let unknown_field = call_tool(
        client.peer(),
        "files",
        serde_json::json!({
            "operation": {"kind": "tree"},
            "bogus": true
        }),
    )
    .await
    .expect("unknown fields should return a tool error result");
    assert_eq!(unknown_field.is_error, Some(true));
    assert!(unknown_field.content.iter().any(|content| {
        content
            .as_text()
            .is_some_and(|text| text.text.contains("unknown field") && text.text.contains("bogus"))
    }));

    expect_tool_error(
        call_tool(
            client.peer(),
            "json",
            serde_json::json!({
                "operation": {
                    "kind": "query",
                    "path": "missing.json"
                }
            }),
        )
        .await,
        "not_found",
    );

    let response_budget = expect_tool_error(
        call_tool(
            client.peer(),
            "files",
            serde_json::json!({
                "operation": {
                    "kind": "tree",
                    "max_results": 1,
                    "max_response_tokens": 1
                }
            }),
        )
        .await,
        "request_limit_exceeded",
    );
    assert_eq!(
        response_budget
            .structured_content
            .as_ref()
            .and_then(|value| value["field"].as_str()),
        Some("max_response_tokens")
    );
    assert!(
        response_budget
            .structured_content
            .as_ref()
            .is_some_and(|value| {
                value["minimum_required_response_tokens"]
                    .as_u64()
                    .is_some_and(|minimum| minimum > 1)
            })
    );

    let oversized_arguments = serde_json::json!({
        "operation": {
            "kind": "text",
            "query": "x".repeat(65 * 1024),
            "max_results": 1,
            "max_tokens": 10
        }
    })
    .as_object()
    .expect("oversized search arguments")
    .clone();
    expect_tool_error(
        client
            .peer()
            .call_tool(CallToolRequestParams::new("search").with_arguments(oversized_arguments))
            .await,
        "input_too_long",
    );

    let boundary_id = "x".repeat(128);
    expect_tool_error(
        call_tool(
            client.peer(),
            "files",
            serde_json::json!({
                "operation": {"kind": "tree"},
                "expected_repository_id": boundary_id
            }),
        )
        .await,
        "repository_identity_mismatch",
    );

    let oversized_id = "x".repeat(129);
    let oversized_error = expect_tool_error(
        call_tool(
            client.peer(),
            "files",
            serde_json::json!({
                "operation": {"kind": "tree"},
                "expected_repository_id": oversized_id
            }),
        )
        .await,
        "input_too_long",
    );
    assert!(
        !serde_json::to_string(&oversized_error)
            .expect("serialize bounded tool error")
            .contains(&oversized_id)
    );

    expect_tool_error(
        call_tool(
            client.peer(),
            "files",
            serde_json::json!({
                "operation": {"kind": "tree"},
                "expected_repository_id": "é".repeat(64)
            }),
        )
        .await,
        "repository_identity_mismatch",
    );
    expect_tool_error(
        call_tool(
            client.peer(),
            "files",
            serde_json::json!({
                "operation": {"kind": "tree"},
                "expected_repository_id": "é".repeat(65)
            }),
        )
        .await,
        "input_too_long",
    );

    let bounded_arguments = serde_json::json!({
        "operation": {
            "kind": "text",
            "query": "answer",
            "max_results": 100,
            "max_tokens": 50
        }
    })
    .as_object()
    .expect("bounded search arguments")
    .clone();
    let bounded = client
        .peer()
        .call_tool(CallToolRequestParams::new("search").with_arguments(bounded_arguments))
        .await
        .expect("large bounded search");
    assert!(
        bounded
            .structured_content
            .as_ref()
            .and_then(|value| value.pointer("/meta/source_tokens"))
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
        .call_tool(CallToolRequestParams::new("context").with_arguments(default_context_arguments))
        .await
        .expect("context with default token budget");
    assert_ne!(default_context.is_error, Some(true));
    assert!(
        default_context
            .structured_content
            .as_ref()
            .and_then(|value| value.pointer("/meta/source_tokens"))
            .and_then(serde_json::Value::as_u64)
            .is_some_and(|tokens| tokens <= 3_000)
    );

    let handoff_context = call_tool(
        client.peer(),
        "context",
        serde_json::json!({
            "task": "find the answer definition",
            "handoff": {
                "summary": "continue the answer change",
                "validations": [{
                    "command": "cargo test answer",
                    "status": "passed"
                }],
                "avoid_rules": ["do not copy source bodies"]
            }
        }),
    )
    .await
    .expect("context with handoff manifest");
    let handoff_manifest = handoff_context
        .structured_content
        .as_ref()
        .and_then(|value| value.pointer("/handoff_manifest"))
        .expect("structured handoff manifest");
    assert!(
        handoff_manifest["evidence"]
            .as_array()
            .is_some_and(|evidence| !evidence.is_empty())
    );
    assert_eq!(
        handoff_manifest["validations"][0]["command"],
        "cargo test answer"
    );
    let handoff_json = serde_json::to_string(handoff_manifest).expect("serialize handoff");
    assert!(!handoff_json.contains("\"content\""));
    assert!(!handoff_json.contains("pub fn answer"));

    let savings = client
        .peer()
        .call_tool(CallToolRequestParams::new("savings").with_arguments(Default::default()))
        .await
        .expect("call savings tool");
    assert_ne!(savings.is_error, Some(true));
    let savings_structured = savings.structured_content.expect("structured savings");
    assert!(
        savings_structured["response_accounting"]["tracked_requests"]
            .as_u64()
            .is_some_and(|requests| requests >= 1)
    );
    assert!(
        savings_structured["response_accounting"]["estimated_net_tokens_saved"]
            .as_u64()
            .is_some()
    );

    let repeated_savings = client
        .peer()
        .call_tool(CallToolRequestParams::new("savings").with_arguments(Default::default()))
        .await
        .expect("repeat savings tool");
    assert_eq!(
        repeated_savings.structured_content,
        Some(savings_structured),
        "observing savings must not update the tracker"
    );

    let context = ContextRequest {
        task: "find answer and its caller".into(),
        token_budget: 100,
        include_paths: Vec::new(),
        must_include_paths: Vec::new(),
        must_include_symbols: Vec::new(),
        required_evidence: Vec::new(),
        max_fragments: None,
        plan_only: false,
        focus_paths: Vec::new(),
        strict_focus_paths: false,
        minimum_fragments_per_focus_path: None,
        focus_symbols: Vec::new(),
        exclude_paths: Vec::new(),
        known_hashes: Vec::new(),
        receipt_id: None,
        prior_repository_generation: None,
        base_revision: None,
        changed_paths: Vec::new(),
        strict_changed_paths: false,
        explain_diagnostics: false,
    };
    let context_arguments = serde_json::to_value(context)
        .expect("serialize context request")
        .as_object()
        .expect("context request object")
        .clone();
    let request = ClientRequest::CallToolRequest(Request::new(
        CallToolRequestParams::new("context").with_arguments(context_arguments),
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
        .call_tool(CallToolRequestParams::new("files").with_arguments(files_arguments))
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
    let external_path = outside.path().join("sensitive-marker-target.rs");
    std::fs::write(&indexed_path, "fn indexed_before_escape() {}\n").expect("indexed fixture");
    std::fs::write(&external_path, "fn external_marker() {}\n").expect("external fixture");
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
        "/home/example/sensitive-marker.rs",
        r"C:\Users\example\sensitive-marker.rs",
    ] {
        let arguments = serde_json::json!({
            "path": requested,
            "target": {"kind": "lines", "start": 1, "end": 1}
        })
        .as_object()
        .expect("read arguments")
        .clone();
        // RMCP 3.1 reports parameter-deserialization failures as tool-result
        // errors, while service-level path checks remain MCP errors. Both
        // paths must keep caller-controlled and canonical paths out of wire
        // diagnostics.
        let response = client
            .peer()
            .call_tool(CallToolRequestParams::new("read").with_arguments(arguments))
            .await;
        let wire = match response {
            Ok(response) => {
                assert_eq!(response.is_error, Some(true));
                serde_json::to_string(&response).expect("serialize tool error")
            }
            Err(ServiceError::McpError(data)) => {
                assert_eq!(
                    data.data
                        .as_ref()
                        .and_then(|value| value["category"].as_str()),
                    Some("path_outside_root")
                );
                serde_json::to_string(&data).expect("serialize MCP error")
            }
            Err(other) => panic!("unexpected service error: {other}"),
        };
        for marker in [
            requested,
            external_path.to_str().expect("external UTF-8"),
            "sensitive-marker",
            "/home/example",
            r"C:\Users\example",
        ] {
            assert!(!wire.contains(marker), "MCP error leaked {marker}: {wire}");
        }
    }

    client.close().await.expect("close client");
    server.close().await.expect("close server");
}

#[tokio::test(start_paused = true)]
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
            "files",
            serde_json::json!({"operation": {"kind": "tree", "depth": 0}}),
            "max_results",
            100,
            false,
        ),
        (
            "search",
            serde_json::json!({"operation": {"kind": "text", "query": "answer"}}),
            "max_results",
            100,
            false,
        ),
        (
            "search",
            serde_json::json!({"operation": {"kind": "text", "query": "answer"}}),
            "max_tokens",
            32_000,
            false,
        ),
        (
            "search",
            serde_json::json!({"operation": {"kind": "text", "query": "answer"}}),
            "context_lines",
            20,
            true,
        ),
        (
            "outline",
            serde_json::json!({"paths": ["lib.rs"]}),
            "max_results",
            100,
            false,
        ),
        (
            "outline",
            serde_json::json!({"paths": ["lib.rs"]}),
            "max_tokens",
            32_000,
            false,
        ),
        (
            "read",
            serde_json::json!({
                "path": "lib.rs",
                "target": {"kind": "lines", "start": 1, "end": 1}
            }),
            "max_tokens",
            32_000,
            false,
        ),
        (
            "context",
            serde_json::json!({"task": "find the answer definition"}),
            "token_budget",
            32_000,
            false,
        ),
    ] {
        assert_mcp_limit_contract(client.peer(), tool, arguments, field, limit, zero_is_valid)
            .await;
    }

    let request = || {
        let arguments = serde_json::json!({ "operation": {"kind": "tree"} })
            .as_object()
            .expect("arguments")
            .clone();
        CallToolRequestParams::new("files").with_arguments(arguments)
    };

    let starting = client
        .peer()
        .call_tool(request())
        .await
        .expect("starting result");
    assert_eq!(starting.is_error, Some(false));
    assert_eq!(
        starting
            .structured_content
            .as_ref()
            .and_then(|value| value["reason"].as_str()),
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
        building
            .structured_content
            .as_ref()
            .and_then(|value| value["reason"].as_str()),
        Some("index_building")
    );
    let progress = &building
        .structured_content
        .as_ref()
        .expect("structured building result")["index_progress"];
    assert_eq!(progress["detail_available"], false);
    assert_eq!(progress["active"], false);
    assert_eq!(
        progress["cache_namespace"]
            .as_str()
            .expect("opaque cache namespace")
            .len(),
        32
    );
    assert!(
        progress.get("files_discovered").is_none(),
        "unavailable follower detail must not invent zero counters"
    );
    assert!(
        !progress
            .to_string()
            .contains(root.path().to_string_lossy().as_ref()),
        "progress must not expose the repository path"
    );

    let peer = client.peer().clone();
    let waiting = tokio::spawn(async move {
        let arguments = serde_json::json!({ "operation": {"kind": "tree"} })
            .as_object()
            .expect("arguments")
            .clone();
        peer.call_tool(CallToolRequestParams::new("files").with_arguments(arguments))
            .await
    });
    tokio::task::yield_now().await;
    assert!(!waiting.is_finished());
    services.index(false).await.expect("index");
    let ready = waiting
        .await
        .expect("join waiting request")
        .expect("ready result");
    assert_ne!(ready.is_error, Some(true));

    state.set_failed(&leantoken::Error::McpRuntimeStopped);
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
