use rmcp::{serve_client, serve_server};

use super::*;

fn test_server() -> (tempfile::TempDir, LeanTokenMcp) {
    let root = tempfile::tempdir().expect("repository root");
    std::fs::write(root.path().join("lib.rs"), "pub fn indexed() {}\n").expect("source");
    let config = Config::discover(root.path(), Some(root.path().join("index.sqlite")))
        .expect("configuration");
    let services = Arc::new(Services::open(config).expect("services"));
    (root, LeanTokenMcp::new(services))
}

#[test]
fn tool_catalog_is_the_complete_retrieval_kernel() {
    let catalog = LeanTokenMcp::tool_router().list_all();
    let names = catalog
        .iter()
        .map(|tool| tool.name.as_ref())
        .collect::<Vec<_>>();

    assert_eq!(names, ["context", "outline", "read", "refresh", "search"]);
    assert_eq!(mcp_schema_fingerprint().len(), 32);
}

#[test]
fn request_admission_has_an_exact_shared_boundary() {
    let (_root, server) = test_server();
    let clone = server.clone();
    let permits = (0..DEFAULT_ACTIVE_TOOL_CALL_CAPACITY)
        .map(|_| clone.request_admission.try_admit().expect("admitted call"))
        .collect::<Vec<_>>();

    assert!(matches!(
        server.request_admission.try_admit(),
        Err(crate::Error::RetrievalOverloaded)
    ));
    drop(permits);
    assert_eq!(
        server.request_admission.available_permits(),
        DEFAULT_ACTIVE_TOOL_CALL_CAPACITY
    );
}

#[test]
fn result_modes_emit_only_the_selected_representation() {
    for (mode, text, structured) in [
        (McpResultMode::Text, true, false),
        (McpResultMode::Structured, false, true),
        (McpResultMode::Dual, true, true),
    ] {
        let result = tool_result(serde_json::json!({"answer": 42}), mode).expect("tool result");
        assert_eq!(!result.content.is_empty(), text);
        assert_eq!(result.structured_content.is_some(), structured);
        assert_eq!(result.is_error, Some(false));
    }
}

#[test]
fn unknown_request_fields_are_rejected() {
    assert!(
        serde_json::from_value::<RefreshMcpRequest>(serde_json::json!({
            "repository_context": "another-repository"
        }))
        .is_err()
    );
    assert!(
        serde_json::from_value::<ReadMcpRequest>(serde_json::json!({
            "path": "lib.rs",
            "delta": true
        }))
        .is_err()
    );
}

#[tokio::test]
async fn rmcp_owns_initialization_and_tool_listing() {
    let (_root, server) = test_server();
    let (client_stream, server_stream) = tokio::io::duplex(64 * 1024);
    let server_start = tokio::spawn(async move {
        serve_server(server, server_stream)
            .await
            .expect("start server")
    });
    let mut client = serve_client((), client_stream)
        .await
        .expect("initialize client");
    let mut server = server_start.await.expect("join server startup");

    let peer = client.peer().peer_info().expect("initialize response");
    assert_eq!(
        peer.server_info.as_ref().expect("server identity").name,
        "leantoken"
    );
    assert_eq!(
        client
            .peer()
            .list_all_tools()
            .await
            .expect("list tools")
            .len(),
        5
    );

    client.close().await.expect("close client");
    server.close().await.expect("close server");
}
