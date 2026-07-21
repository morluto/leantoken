use std::{fs, path::Path, path::PathBuf, sync::Arc, time::Instant};

use leantoken::{
    Config, ContextRequest,
    mcp::{LeanTokenMcp, McpResultMode, tool_catalog_json, tool_result},
    services::Services,
};
use rmcp::{ServerHandler, model::ProtocolVersion};
use serde::Serialize;

#[derive(Serialize)]
struct Report {
    fixture: String,
    tokenizer: &'static str,
    token_count_exact: bool,
    schema_tokens: usize,
    initialize_request_tokens: usize,
    initialize_response_tokens: usize,
    initialized_notification_tokens: usize,
    tools_list_request_tokens: usize,
    tools_list_response_tokens: usize,
    call_request_tokens: usize,
    response_json_tokens: usize,
    dual_result_tokens: usize,
    text_result_tokens: usize,
    structured_result_tokens: usize,
    handoff_tokens: usize,
    latency_ms: f64,
    limitations: Vec<&'static str>,
}

#[tokio::test]
async fn mcp_handoff_token_costs() {
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/sample_repo");
    let temp = tempfile::tempdir().expect("temporary repository");
    let root = temp.path().join("repo");
    copy_tree(&source, &root);
    let config = Config::discover(&root, Some(temp.path().join("index.sqlite"))).expect("config");
    let tokenizer = config.tokenizer;
    let services = Arc::new(Services::open(config).expect("services"));
    services.index(true).await.expect("cold index");

    let started = Instant::now();

    let schema_json = tool_catalog_json();
    let schema: serde_json::Value = serde_json::from_str(&schema_json).expect("tool catalog");
    let protocol_version = ProtocolVersion::LATEST.as_str();

    let initialize_request = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 0,
        "method": "initialize",
        "params": {
            "protocolVersion": protocol_version,
            "capabilities": {},
            "clientInfo": { "name": "benchmark-client", "version": "1" }
        }
    });
    let mut server_info = LeanTokenMcp::new(Arc::clone(&services)).get_info();
    server_info.protocol_version = ProtocolVersion::LATEST;
    let initialize_response = serde_json::json!({
        "jsonrpc": "2.0", "id": 0, "result": server_info
    });
    let initialized_notification = serde_json::json!({
        "jsonrpc": "2.0", "method": "notifications/initialized"
    });
    let tools_list_request = serde_json::json!({
        "jsonrpc": "2.0", "id": 1, "method": "tools/list", "params": {}
    });
    let tools_list_response = serde_json::json!({
        "jsonrpc": "2.0", "id": 1, "result": { "tools": schema }
    });

    let context_request = ContextRequest {
        task: "change the Rust Point distance calculation and its caller".into(),
        token_budget: 500,
        focus_paths: Vec::new(),
        focus_symbols: Vec::new(),
        exclude_paths: Vec::new(),
        known_hashes: Vec::new(),
        prior_repository_generation: None,
    };
    let context = services.context(context_request).await.expect("context");
    let context_value = serde_json::to_value(&context).expect("context JSON");
    let response_json_tokens = tokenizer.count(&context_value.to_string());

    let call_request = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/call",
        "params": {
            "name": "leantoken_context",
            "arguments": {
                "task": "change the Rust Point distance calculation and its caller",
                "token_budget": 500,
                "focus_paths": [],
                "focus_symbols": [],
                "exclude_paths": [],
                "known_hashes": [],
                "prior_repository_generation": null
            }
        }
    });
    let dual_result = tool_result(context_value.clone(), McpResultMode::Dual)
        .expect("dual result");
    let text_result = tool_result(context_value.clone(), McpResultMode::Text)
        .expect("text result");
    let structured_result = tool_result(context_value, McpResultMode::Structured)
        .expect("structured result");
    assert!(dual_result.structured_content.is_some());
    let call_result = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 2,
        "result": dual_result
    });
    let text_call_result = serde_json::json!({
        "jsonrpc": "2.0", "id": 2, "result": text_result
    });
    let structured_call_result = serde_json::json!({
        "jsonrpc": "2.0", "id": 2, "result": structured_result
    });

    let schema_tokens = tokenizer.count(&schema_json);
    let initialize_request_tokens = tokenizer.count(&initialize_request.to_string());
    let initialize_response_tokens = tokenizer.count(&initialize_response.to_string());
    let initialized_notification_tokens = tokenizer.count(&initialized_notification.to_string());
    let tools_list_request_tokens = tokenizer.count(&tools_list_request.to_string());
    let tools_list_response_tokens = tokenizer.count(&tools_list_response.to_string());
    let call_request_tokens = tokenizer.count(&call_request.to_string());
    let dual_result_tokens = tokenizer.count(&call_result.to_string());
    let text_result_tokens = tokenizer.count(&text_call_result.to_string());
    let structured_result_tokens = tokenizer.count(&structured_call_result.to_string());
    let handoff_tokens = initialize_request_tokens
        + initialize_response_tokens
        + initialized_notification_tokens
        + tools_list_request_tokens
        + tools_list_response_tokens
        + call_request_tokens
        + dual_result_tokens;

    let latency_ms = started.elapsed().as_secs_f64() * 1_000.0;

    let report = Report {
        fixture: source.display().to_string(),
        tokenizer: tokenizer.name(),
        token_count_exact: tokenizer.is_exact(),
        schema_tokens,
        initialize_request_tokens,
        initialize_response_tokens,
        initialized_notification_tokens,
        tools_list_request_tokens,
        tools_list_response_tokens,
        call_request_tokens,
        response_json_tokens,
        dual_result_tokens,
        text_result_tokens,
        structured_result_tokens,
        handoff_tokens,
        latency_ms,
        limitations: vec![
            "Dual mode is the compatibility default and serializes JSON in both text content and structuredContent; text-only and structured-only costs are reported separately.",
            "The modeled trace serializes initialize, notifications/initialized, tools/list, and tools/call messages but excludes transport framing outside JSON-RPC.",
            "No model consumes the handoff, so practical sufficiency is not measured here.",
        ],
    };

    let pretty = serde_json::to_string_pretty(&report).expect("serialize report");
    let report_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/mcp_token_costs.json");
    fs::write(&report_path, &pretty).expect("write report");
    println!("{pretty}");

    assert!(
        schema_tokens <= 2_250,
        "five-tool schema exceeds allowed budget: {}",
        schema_tokens
    );
    assert!(handoff_tokens > schema_tokens + dual_result_tokens);
    assert!(structured_result_tokens < dual_result_tokens);
    assert!(text_result_tokens < dual_result_tokens);
    assert!(
        response_json_tokens <= 531,
        "compact context response exceeds the frozen budget: {response_json_tokens}"
    );
    assert!(
        dual_result_tokens <= 1_123,
        "dual context result exceeds the frozen budget: {dual_result_tokens}"
    );
    assert!(
        handoff_tokens <= 3_785,
        "complete MCP handoff exceeds the frozen budget: {handoff_tokens}"
    );
    assert!(tokenizer.is_exact(), "default tokenizer should be exact");
}

fn copy_tree(source: &Path, destination: &Path) {
    fs::create_dir_all(destination).expect("create destination");
    for entry in fs::read_dir(source).expect("read fixture directory") {
        let entry = entry.expect("fixture entry");
        let target: PathBuf = destination.join(entry.file_name());
        if entry.file_type().expect("fixture type").is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), target).expect("copy fixture file");
        }
    }
}
