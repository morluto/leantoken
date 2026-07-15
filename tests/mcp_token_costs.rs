use std::{fs, path::Path, path::PathBuf, time::Instant};

use leantoken::{
    Config, ContextRequest,
    mcp::{handoff_cost, tool_catalog_json},
    services::Services,
    tokens,
};
use serde::Serialize;

#[derive(Serialize)]
struct Report {
    fixture: String,
    tokenizer: &'static str,
    token_count_exact: bool,
    schema_tokens: usize,
    envelope_tokens: usize,
    result_tokens: usize,
    protocol_overhead_tokens: usize,
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
    let services = Services::open(config).expect("services");
    services.index(true).await.expect("cold index");

    let started = Instant::now();

    let schema_json = tool_catalog_json();

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
    let context_json = serde_json::to_string(&context).expect("context JSON");

    let call_request = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
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
    let call_result = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "result": {
            "content": [
                { "type": "text", "text": context_json }
            ],
            "isError": false
        }
    });

    let cost = handoff_cost(
        &schema_json,
        &call_request.to_string(),
        &call_result.to_string(),
    );

    let latency_ms = started.elapsed().as_secs_f64() * 1_000.0;

    let report = Report {
        fixture: source.display().to_string(),
        tokenizer: tokens::current().name(),
        token_count_exact: cost.exact,
        schema_tokens: cost.schema_tokens,
        envelope_tokens: cost.envelope_tokens,
        result_tokens: cost.result_tokens,
        protocol_overhead_tokens: cost.protocol_overhead_tokens,
        handoff_tokens: cost.handoff_tokens,
        latency_ms,
        limitations: vec![
            "The result payload wraps the real ContextResponse JSON as a text content item, matching the MCP wire format.",
            "The protocol overhead is a fixed model; a real transport includes the full tools/list request and initialize handshake.",
            "No model consumes the handoff, so practical sufficiency is not measured here.",
        ],
    };

    let pretty = serde_json::to_string_pretty(&report).expect("serialize report");
    let report_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/mcp_token_costs.json");
    fs::write(&report_path, &pretty).expect("write report");
    println!("{pretty}");

    assert!(
        cost.schema_tokens <= 6_000,
        "five-tool schema exceeds allowed budget: {}",
        cost.schema_tokens
    );
    assert!(
        cost.handoff_tokens > cost.schema_tokens + cost.envelope_tokens + cost.result_tokens,
        "handoff total should include the modeled overhead"
    );
    assert!(cost.exact, "default tokenizer should be exact");
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
