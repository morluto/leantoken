use std::{collections::BTreeMap, error::Error, fs, path::PathBuf};

use clap::{Parser, ValueEnum};
use leantoken::tokens::Tokenizer;
use serde::{Deserialize, Serialize};

#[derive(Debug, Parser)]
#[command(about = "Analyze a captured MCP JSON-RPC exchange")]
struct Args {
    /// Host trace containing exact JSON-RPC messages in observed order.
    #[arg(long)]
    trace: PathBuf,
    /// Optional report path.
    #[arg(long)]
    output: Option<PathBuf>,
}

#[derive(Debug, Deserialize)]
struct Trace {
    schema_version: u32,
    host: String,
    host_version: String,
    tokenizer: String,
    #[serde(default)]
    provider_total_input_tokens: Option<u64>,
    events: Vec<Event>,
}

#[derive(Debug, Deserialize)]
struct Event {
    direction: Direction,
    #[serde(default)]
    message: Option<serde_json::Value>,
    #[serde(default)]
    raw_json: Option<String>,
    #[serde(default)]
    provider_input_tokens: Option<u64>,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum Direction {
    ClientToServer,
    ServerToClient,
    Handoff,
}

#[derive(Debug, Default, Serialize)]
struct CategoryCost {
    events: usize,
    local_tokens: usize,
    provider_input_tokens: Option<u64>,
}

#[derive(Debug, Serialize)]
struct Report {
    schema_version: u32,
    trace_blake3: String,
    host: String,
    host_version: String,
    tokenizer: &'static str,
    token_count_exact: bool,
    event_count: usize,
    total_local_tokens: usize,
    total_provider_input_tokens: Option<u64>,
    categories: BTreeMap<String, CategoryCost>,
    tool_result_modes: BTreeMap<String, usize>,
    required_exchange_parts: BTreeMap<&'static str, bool>,
    limitations: Vec<&'static str>,
}

fn main() -> Result<(), Box<dyn Error>> {
    let args = Args::parse();
    let trace_json = fs::read_to_string(&args.trace)?;
    let trace_blake3 = blake3::hash(trace_json.as_bytes()).to_hex().to_string();
    let trace: Trace = serde_json::from_str(&trace_json)?;
    if trace.schema_version != 1 || trace.events.is_empty() {
        return Err("unsupported or empty wire trace".into());
    }
    let tokenizer = Tokenizer::from_str(&trace.tokenizer, false)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidInput, error))?;
    let mut categories = BTreeMap::<String, CategoryCost>::new();
    let mut result_modes = BTreeMap::<String, usize>::new();
    let mut required = BTreeMap::from([
        ("initialize_request", false),
        ("initialize_response", false),
        ("initialized_notification", false),
        ("tools_list", false),
        ("tools_call", false),
        ("tool_result", false),
    ]);
    let mut total_local_tokens = 0usize;
    let mut event_provider_tokens = Some(0u64);

    for event in &trace.events {
        let message = match (&event.raw_json, &event.message) {
            (Some(raw), _) => serde_json::from_str(raw)?,
            (None, Some(message)) => message.clone(),
            (None, None) => return Err("wire event has neither raw_json nor message".into()),
        };
        let serialized = event
            .raw_json
            .clone()
            .unwrap_or(serde_json::to_string(&message)?);
        let local_tokens = tokenizer.count(&serialized);
        total_local_tokens += local_tokens;
        event_provider_tokens = match (event_provider_tokens, event.provider_input_tokens) {
            (Some(total), Some(tokens)) => Some(total + tokens),
            _ => None,
        };
        let category = category(event.direction, &message, &mut required);
        let cost = categories.entry(category).or_default();
        cost.events += 1;
        cost.local_tokens += local_tokens;
        cost.provider_input_tokens = match (cost.provider_input_tokens, event.provider_input_tokens)
        {
            (Some(total), Some(tokens)) => Some(total + tokens),
            (None, Some(tokens)) if cost.events == 1 => Some(tokens),
            _ => None,
        };

        if matches!(event.direction, Direction::ServerToClient)
            && (message.pointer("/result/content").is_some()
                || message.pointer("/result/structuredContent").is_some())
        {
            let has_text = message
                .pointer("/result/content")
                .and_then(serde_json::Value::as_array)
                .is_some_and(|content| !content.is_empty());
            let has_structured = message.pointer("/result/structuredContent").is_some();
            let mode = match (has_text, has_structured) {
                (true, true) => "dual",
                (true, false) => "text",
                (false, true) => "structured",
                (false, false) => "empty",
            };
            *result_modes.entry(mode.to_owned()).or_default() += 1;
        }
    }

    let report = Report {
        schema_version: trace.schema_version,
        trace_blake3,
        host: trace.host,
        host_version: trace.host_version,
        tokenizer: tokenizer.name(),
        token_count_exact: tokenizer.is_exact(),
        event_count: trace.events.len(),
        total_local_tokens,
        total_provider_input_tokens: trace.provider_total_input_tokens.or(event_provider_tokens),
        categories,
        tool_result_modes: result_modes,
        required_exchange_parts: required,
        limitations: vec![
            "Local counts cover exact serialized JSON messages but not provider-specific conversation framing unless the host exports it as a handoff event.",
            "Provider input totals remain null unless the trace supplies one authoritative provider total or every event supplies a provider-native delta; partial event totals are not reported.",
            "Transport bytes outside JSON-RPC, such as process startup and stdio buffering, are outside token cost.",
        ],
    };
    let json = serde_json::to_string_pretty(&report)?;
    if let Some(output) = args.output {
        if let Some(parent) = output.parent().filter(|path| !path.as_os_str().is_empty()) {
            fs::create_dir_all(parent)?;
        }
        fs::write(output, &json)?;
    }
    println!("{json}");
    Ok(())
}

fn category(
    direction: Direction,
    message: &serde_json::Value,
    required: &mut BTreeMap<&'static str, bool>,
) -> String {
    if matches!(direction, Direction::Handoff) {
        return "handoff".to_owned();
    }
    let method = message.get("method").and_then(serde_json::Value::as_str);
    match (direction, method) {
        (Direction::ClientToServer, Some("initialize")) => {
            required.insert("initialize_request", true);
            "initialize_request"
        }
        (Direction::ClientToServer, Some("notifications/initialized")) => {
            required.insert("initialized_notification", true);
            "initialized_notification"
        }
        (Direction::ClientToServer, Some("tools/list")) => {
            required.insert("tools_list", true);
            "tools_list_request"
        }
        (Direction::ClientToServer, Some("tools/call")) => {
            required.insert("tools_call", true);
            "tool_call_request"
        }
        (Direction::ServerToClient, _) if message.pointer("/result/tools").is_some() => {
            required.insert("tools_list", true);
            "tools_list_response"
        }
        (Direction::ServerToClient, _)
            if message.pointer("/result/content").is_some()
                || message.pointer("/result/structuredContent").is_some() =>
        {
            required.insert("tool_result", true);
            "tool_call_response"
        }
        (Direction::ServerToClient, _) if message.pointer("/result/protocolVersion").is_some() => {
            required.insert("initialize_response", true);
            "initialize_response"
        }
        _ => "other",
    }
    .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn structured_only_result_is_classified_as_a_tool_result() {
        let mut required = BTreeMap::from([
            ("initialize_request", false),
            ("initialize_response", false),
            ("initialized_notification", false),
            ("tools_list", false),
            ("tools_call", false),
            ("tool_result", false),
        ]);
        let message = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 2,
            "result": {"structuredContent": {"answer": 42}}
        });

        assert_eq!(
            category(Direction::ServerToClient, &message, &mut required),
            "tool_call_response"
        );
        assert!(required["tool_result"]);
    }

    #[test]
    fn unrelated_success_response_is_not_classified_as_initialize() {
        let mut required = BTreeMap::from([
            ("initialize_request", false),
            ("initialize_response", false),
            ("initialized_notification", false),
            ("tools_list", false),
            ("tools_call", false),
            ("tool_result", false),
        ]);
        let message = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 3,
            "result": {}
        });

        assert_eq!(
            category(Direction::ServerToClient, &message, &mut required),
            "other"
        );
        assert!(!required["initialize_response"]);
    }
}
