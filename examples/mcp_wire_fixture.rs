use std::{error::Error, fs, path::PathBuf};

use clap::Parser;
use leantoken::mcp::{McpResultMode, tool_catalog_json, tool_result};
use leantoken::model::{
    ContextFragment, ContextRequest, ContextResponse, EvidenceReceipt, Freshness, ResponseMeta,
};
use serde::Serialize;

#[derive(Debug, Parser)]
#[command(about = "Generate a deterministic synthetic MCP wire-cost fixture")]
struct Args {
    /// Synthetic trace destination.
    #[arg(long, default_value = "benchmarks/wire_trace.synthetic.json")]
    output: PathBuf,
}

#[derive(Debug, Serialize)]
struct Trace {
    schema_version: u32,
    host: &'static str,
    host_version: &'static str,
    tokenizer: &'static str,
    provider_total_input_tokens: Option<u64>,
    events: Vec<Event>,
}

#[derive(Debug, Serialize)]
struct Event {
    direction: &'static str,
    raw_json: String,
    provider_input_tokens: Option<u64>,
}

fn main() -> Result<(), Box<dyn Error>> {
    let args = Args::parse();
    let trace = synthetic_trace()?;
    let json = serde_json::to_string_pretty(&trace)?;
    if let Some(parent) = args
        .output
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
    {
        fs::create_dir_all(parent)?;
    }
    fs::write(&args.output, format!("{json}\n"))?;
    println!("{json}");
    Ok(())
}

fn synthetic_trace() -> Result<Trace, Box<dyn Error>> {
    let request = ContextRequest {
        task: "find the request-validation boundary".into(),
        token_budget: 400,
        focus_paths: vec!["src/mcp.rs".into()],
        focus_symbols: Vec::new(),
        exclude_paths: Vec::new(),
        known_hashes: Vec::new(),
        prior_repository_generation: None,
    };
    let response = ContextResponse {
        fragments: vec![ContextFragment {
            path: "src/mcp.rs".into(),
            start_line: 298,
            end_line: 322,
            representation: "source".into(),
            content:
                "fn into_mcp_error(error: crate::Error) -> ErrorData {\n    // fixture excerpt\n}\n"
                    .into(),
            content_hash: "fixture-fragment-hash".into(),
            score: 1.0,
            reason: "exact symbol and focus path".into(),
            token_count: 21,
        }],
        receipt: EvidenceReceipt {
            task_fingerprint: "fixture-task-fingerprint".into(),
            fragment_hashes: vec!["fixture-fragment-hash".into()],
        },
        omitted: Vec::new(),
        warnings: Vec::new(),
        meta: ResponseMeta {
            repository_generation: 7,
            freshness: Freshness::Current,
            emitted_tokens: 21,
            token_count_exact: true,
            next_cursor: None,
        },
    };
    let result = tool_result(response, McpResultMode::Dual)?;
    let tools: serde_json::Value = serde_json::from_str(&tool_catalog_json())?;
    let messages = [
        (
            "client_to_server",
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": 0,
                "method": "initialize",
                "params": {
                    "protocolVersion": "2025-11-25",
                    "capabilities": {},
                    "clientInfo": {"name": "synthetic-fixture", "version": "1"}
                }
            }),
        ),
        (
            "server_to_client",
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": 0,
                "result": {
                    "protocolVersion": "2025-11-25",
                    "capabilities": {"tools": {}},
                    "serverInfo": {"name": "leantoken", "version": env!("CARGO_PKG_VERSION")}
                }
            }),
        ),
        (
            "client_to_server",
            serde_json::json!({
                "jsonrpc": "2.0",
                "method": "notifications/initialized"
            }),
        ),
        (
            "client_to_server",
            serde_json::json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}),
        ),
        (
            "server_to_client",
            serde_json::json!({"jsonrpc": "2.0", "id": 1, "result": {"tools": tools}}),
        ),
        (
            "client_to_server",
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": 2,
                "method": "tools/call",
                "params": {"name": "leantoken_context", "arguments": request}
            }),
        ),
        (
            "server_to_client",
            serde_json::json!({"jsonrpc": "2.0", "id": 2, "result": result}),
        ),
    ];

    Ok(Trace {
        schema_version: 1,
        host: "synthetic-fixture",
        host_version: env!("CARGO_PKG_VERSION"),
        tokenizer: "cl100k_base",
        provider_total_input_tokens: None,
        events: messages
            .into_iter()
            .map(|(direction, message)| {
                Ok(Event {
                    direction,
                    raw_json: serde_json::to_string(&message)?,
                    provider_input_tokens: None,
                })
            })
            .collect::<Result<Vec<_>, serde_json::Error>>()?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixture_uses_the_current_catalog_and_dual_result() {
        let trace = synthetic_trace().expect("trace");
        assert_eq!(trace.events.len(), 7);
        let catalog: serde_json::Value =
            serde_json::from_str(&trace.events[4].raw_json).expect("catalog message");
        assert_eq!(
            catalog["result"]["tools"]
                .as_array()
                .expect("tools array")
                .len(),
            5
        );
        let result: serde_json::Value =
            serde_json::from_str(&trace.events[6].raw_json).expect("tool result");
        assert!(
            result["result"]["content"]
                .as_array()
                .is_some_and(|content| !content.is_empty())
        );
        assert!(result["result"].get("structuredContent").is_some());
    }
}
