#[path = "support/wire_trace.rs"]
mod wire_trace;

use std::{error::Error, fs, path::PathBuf};

use clap::Parser;
use leantoken::mcp::{McpResultMode, tool_catalog_json, tool_result};
use leantoken::model::{
    ContextFragment, ContextRequest, ContextResponse, EvidenceReceipt, Freshness, ResponseMeta,
};
use wire_trace::{Direction, Event, RangeIdentity, RepositoryIdentity, TRACE_SCHEMA_V2, Trace};

#[derive(Debug, Parser)]
#[command(about = "Generate a deterministic synthetic MCP wire-cost fixture")]
struct Args {
    /// Synthetic trace destination.
    #[arg(long, default_value = "benchmarks/wire_trace.synthetic.json")]
    output: PathBuf,
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
        base_revision: None,
        changed_paths: Vec::new(),
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
        diff_scope: None,
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
            Direction::ClientToServer,
            0,
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
            Direction::ServerToClient,
            0,
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
            Direction::ClientToServer,
            0,
            serde_json::json!({
                "jsonrpc": "2.0",
                "method": "notifications/initialized"
            }),
        ),
        (
            Direction::ClientToServer,
            0,
            serde_json::json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}),
        ),
        (
            Direction::ServerToClient,
            0,
            serde_json::json!({"jsonrpc": "2.0", "id": 1, "result": {"tools": tools}}),
        ),
        (
            Direction::ClientToServer,
            1,
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": 2,
                "method": "tools/call",
                "params": {"name": "leantoken_context", "arguments": request}
            }),
        ),
        (
            Direction::ServerToClient,
            1,
            serde_json::json!({"jsonrpc": "2.0", "id": 2, "result": result}),
        ),
    ];

    let mut events = messages
        .into_iter()
        .enumerate()
        .map(|(sequence, (direction, turn, message))| Event {
            sequence: Some(sequence as u64),
            direction,
            turn: Some(turn),
            timestamp_unix_millis: Some(1_000 + sequence as u64),
            latency_ms: None,
            category: None,
            raw_json: Some(serde_json::to_string(&message).expect("serialize fixture message")),
            message: None,
            provider_visible_payload: None,
            tool_name: None,
            call_id: None,
            result_id: None,
            ranges: Vec::new(),
            visible_through_turn: None,
            stable_prefix: Some(turn == 0),
            cache_eligible: Some(true),
            compaction: None,
            provider_usage: None,
            provider_input_tokens: None,
        })
        .collect::<Vec<_>>();
    let result_event = events.last_mut().expect("result event");
    result_event.tool_name = Some("leantoken_context".into());
    result_event.call_id = Some("2".into());
    result_event.result_id = Some("context-result-1".into());
    result_event.visible_through_turn = Some(3);
    result_event.ranges.push(RangeIdentity {
        repository_generation: 7,
        path: "src/mcp.rs".into(),
        start_line: 298,
        end_line: 322,
        content_hash: "fixture-fragment-hash".into(),
        source_tokens: Some(21),
    });
    events.push(Event {
        sequence: Some(events.len() as u64),
        direction: Direction::Handoff,
        turn: Some(2),
        timestamp_unix_millis: Some(1_100),
        latency_ms: None,
        category: Some("handoff".into()),
        message: None,
        raw_json: None,
        provider_visible_payload: Some(
            "{\"role\":\"tool\",\"result_id\":\"context-result-1\"}".into(),
        ),
        tool_name: Some("leantoken_context".into()),
        call_id: Some("2".into()),
        result_id: Some("context-result-1".into()),
        ranges: Vec::new(),
        visible_through_turn: None,
        stable_prefix: Some(false),
        cache_eligible: Some(true),
        compaction: None,
        provider_usage: None,
        provider_input_tokens: None,
    });

    let mut trace = Trace {
        schema_version: TRACE_SCHEMA_V2,
        trace_id: Some("synthetic-mcp-wire-v2".into()),
        trace_content_blake3: None,
        host: "synthetic-fixture".into(),
        host_version: env!("CARGO_PKG_VERSION").into(),
        model: Some("synthetic-model".into()),
        provider: Some("synthetic-provider".into()),
        tokenizer: "cl100k_base".into(),
        token_count_exact: Some(true),
        generated_at_unix_seconds: Some(0),
        repository: Some(RepositoryIdentity {
            revision: "0000000000000000000000000000000000000000".into(),
            dirty_fingerprint: "clean".into(),
        }),
        final_turn: Some(3),
        provider_usage: None,
        provider_total_input_tokens: None,
        outcome: None,
        events,
    };
    trace.seal_content_hash()?;
    Ok(trace)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixture_uses_current_catalog_dual_result_and_range_identity() {
        let trace = synthetic_trace().expect("trace");
        assert_eq!(trace.schema_version, TRACE_SCHEMA_V2);
        trace.validate_version().expect("valid sealed trace");
        let content_hash = trace.content_blake3().expect("content hash");
        assert_eq!(
            trace.trace_content_blake3.as_deref(),
            Some(content_hash.as_str())
        );
        assert_eq!(trace.events.len(), 8);
        let catalog: serde_json::Value =
            serde_json::from_str(trace.events[4].raw_json.as_deref().expect("catalog raw"))
                .expect("catalog message");
        assert_eq!(
            catalog["result"]["tools"]
                .as_array()
                .expect("tools array")
                .len(),
            5
        );
        let result: serde_json::Value =
            serde_json::from_str(trace.events[6].raw_json.as_deref().expect("result raw"))
                .expect("tool result");
        assert!(
            result["result"]["content"]
                .as_array()
                .is_some_and(|content| !content.is_empty())
        );
        assert!(result["result"].get("structuredContent").is_some());
        assert_eq!(trace.events[6].ranges.len(), 1);
        assert_eq!(trace.events[6].visible_through_turn, Some(3));
        assert_eq!(trace.events[7].direction, Direction::Handoff);
    }
}
