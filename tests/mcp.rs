use std::sync::Arc;

use leantoken::{
    Config, ContextRequest, FileOperation, FilesRequest, mcp::LeanTokenMcp, services::Services,
};
use rmcp::{
    model::{CallToolRequestParams, ClientRequest, ErrorCode, Request},
    serve_client, serve_server,
    service::{PeerRequestOptions, ServiceError},
};

#[tokio::test]
async fn sdk_transport_initializes_lists_calls_and_closes() {
    let root = tempfile::tempdir().expect("temporary repository");
    std::fs::write(root.path().join("lib.rs"), "pub fn answer() -> u8 { 42 }\n")
        .expect("write fixture");
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

    let request = FilesRequest {
        operation: FileOperation::Tree,
        path: None,
        query: None,
        pattern: None,
        max_results: Some(10),
        cursor: None,
        depth: Some(2),
    };
    let files_arguments = serde_json::to_value(request)
        .expect("serialize request")
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

    let invalid_arguments = serde_json::json!({ "path": "../secret" })
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
        ServiceError::McpError(data) if data.code == ErrorCode::INVALID_PARAMS
    ));

    let context = ContextRequest {
        task: "find answer and its caller".into(),
        token_budget: 100,
        focus_paths: Vec::new(),
        focus_symbols: Vec::new(),
        exclude_paths: Vec::new(),
        known_hashes: Vec::new(),
        prior_repository_generation: None,
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
