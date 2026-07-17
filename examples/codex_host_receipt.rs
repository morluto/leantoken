//! Build a publishable host receipt from private Codex rollout and MCP traces.

#[path = "support/wire_trace.rs"]
mod wire_trace;

use std::collections::{BTreeMap, HashMap, HashSet};
use std::error::Error;
use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};

use clap::{Parser, ValueEnum};
use leantoken::tokens::Tokenizer;
use serde::Serialize;
use serde_json::{Map, Value};
use wire_trace::{Direction, ProviderUsage, Trace};

const RECEIPT_SCHEMA_V1: u32 = 1;

#[derive(Debug, Parser)]
#[command(about = "Create a redacted Codex host/MCP correlation receipt")]
struct Args {
    /// Private Codex rollout JSONL. Its content is never copied to the receipt.
    #[arg(long)]
    rollout: PathBuf,
    /// Private exact MCP trace captured during the same host session.
    #[arg(long)]
    mcp_trace: PathBuf,
    /// Codex MCP server name used for namespaced function calls.
    #[arg(long, default_value = "leantoken")]
    mcp_server: String,
    /// Tokenizer for redacted tool-output size accounting.
    #[arg(long, default_value = "cl100k_base")]
    tokenizer: String,
    /// Full Git revision of the receipt/capture harness source.
    #[arg(long)]
    harness_revision: String,
    /// Full Git revision of the frozen LeanToken runtime source.
    #[arg(long)]
    runtime_revision: String,
    /// Native Codex host binary used for the session.
    #[arg(long)]
    host_binary: PathBuf,
    /// Frozen LeanToken server binary launched behind the capture proxy.
    #[arg(long)]
    runtime_binary: PathBuf,
    /// Exact capture proxy binary used by Codex.
    #[arg(long)]
    capture_binary: PathBuf,
    /// Publishable receipt path.
    #[arg(long)]
    output: PathBuf,
}

#[derive(Debug, Serialize)]
struct HostReceipt {
    schema_version: u32,
    source_rollout_blake3: String,
    source_mcp_trace_blake3: String,
    source_mcp_content_blake3: String,
    harness_revision: String,
    runtime_revision: String,
    host_binary_blake3: String,
    runtime_binary_blake3: String,
    capture_binary_blake3: String,
    receipt_binary_blake3: String,
    host: &'static str,
    host_version: String,
    model: String,
    provider: String,
    tokenizer: String,
    token_count_exact: bool,
    host_os: &'static str,
    host_arch: &'static str,
    repository: Option<RepositoryReceipt>,
    rollout_record_count: usize,
    turn_count: usize,
    completed_turns: usize,
    aborted_turns: usize,
    compactions: Vec<CompactionReceipt>,
    distinct_provider_usage_snapshots: usize,
    total_input_tokens: u64,
    provider_usage: ProviderUsage,
    tool_calls: Vec<ToolCallReceipt>,
    mcp_correlation: McpCorrelation,
    privacy: PrivacyBoundary,
    limitations: Vec<&'static str>,
}

#[derive(Debug, Serialize)]
struct RepositoryReceipt {
    revision: String,
    dirty_fingerprint: String,
}

#[derive(Debug, Serialize)]
struct CompactionReceipt {
    sequence: usize,
    after_turn: Option<usize>,
    window_number: Option<u64>,
    replacement_history_entries: usize,
}

#[derive(Debug, Serialize)]
struct ToolCallReceipt {
    sequence: usize,
    turn: usize,
    tool_name: String,
    call_id_blake3: String,
    output_blake3: String,
    output_tokens: usize,
    expected_hash_supplied: bool,
    known_hash_followup: bool,
    result_status: Option<String>,
    result_is_error: bool,
    followed_by_model_response: bool,
    followed_by_provider_usage: bool,
}

#[derive(Debug, Serialize)]
struct McpCorrelation {
    rollout_tool_calls: usize,
    mcp_tool_calls: usize,
    tool_order_matches: bool,
    protocol_order_valid: bool,
    semantic_output_matches: usize,
    all_semantic_outputs_match: bool,
    known_hash_followups: usize,
    not_modified_results: usize,
}

#[derive(Debug, Serialize)]
struct PrivacyBoundary {
    raw_rollout_retained: bool,
    raw_mcp_messages_retained: bool,
    prompts_retained: bool,
    tool_arguments_retained: bool,
    tool_outputs_retained: bool,
    credentials_retained: bool,
    absolute_paths_retained: bool,
    session_and_call_ids_hashed_or_omitted: bool,
}

#[derive(Debug)]
struct RolloutCall {
    turn: usize,
    tool_name: String,
    call_id: String,
    call_record_index: usize,
    output_record_index: Option<usize>,
    output: Option<Value>,
    output_bytes: Option<Vec<u8>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct UsageNumbers {
    input_tokens: u64,
    cached_input_tokens: u64,
    output_tokens: u64,
    reasoning_output_tokens: u64,
    total_tokens: u64,
}

#[derive(Debug)]
struct RolloutSummary {
    record_count: usize,
    host_version: String,
    model: String,
    provider: String,
    turn_count: usize,
    completed_turns: usize,
    aborted_turns: usize,
    compactions: Vec<CompactionReceipt>,
    usage_snapshots: Vec<(usize, usize, UsageNumbers)>,
    model_response_indexes: Vec<(usize, usize)>,
    calls: Vec<RolloutCall>,
}

#[derive(Debug)]
struct McpCall {
    tool_name: String,
    result: Value,
    expected_hash: Option<String>,
    content_hash: Option<String>,
    status: Option<String>,
    is_error: bool,
}

#[derive(Debug)]
struct FrozenIdentities {
    harness_revision: String,
    runtime_revision: String,
    host_binary_blake3: String,
    runtime_binary_blake3: String,
    capture_binary_blake3: String,
    receipt_binary_blake3: String,
}

fn main() -> Result<(), Box<dyn Error>> {
    let args = Args::parse();
    validate_safe_name(&args.mcp_server, "MCP server")?;
    let tokenizer = Tokenizer::from_str(&args.tokenizer, false)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidInput, error))?;
    let rollout_bytes = fs::read(&args.rollout)?;
    let mcp_bytes = fs::read(&args.mcp_trace)?;
    validate_lower_hex(&args.harness_revision, 40, "harness revision")?;
    validate_lower_hex(&args.runtime_revision, 40, "runtime revision")?;
    let identities = FrozenIdentities {
        harness_revision: args.harness_revision,
        runtime_revision: args.runtime_revision,
        host_binary_blake3: hash_file(&args.host_binary)?,
        runtime_binary_blake3: hash_file(&args.runtime_binary)?,
        capture_binary_blake3: hash_file(&args.capture_binary)?,
        receipt_binary_blake3: hash_file(&std::env::current_exe()?)?,
    };
    let receipt = build_receipt(
        &rollout_bytes,
        &mcp_bytes,
        &args.mcp_server,
        &tokenizer,
        &identities,
    )?;
    let json = serde_json::to_string_pretty(&receipt)?;
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

fn build_receipt(
    rollout_bytes: &[u8],
    mcp_bytes: &[u8],
    mcp_server: &str,
    tokenizer: &Tokenizer,
    identities: &FrozenIdentities,
) -> Result<HostReceipt, Box<dyn Error>> {
    let rollout = parse_rollout(rollout_bytes, mcp_server)?;
    let mcp_trace: Trace = serde_json::from_slice(mcp_bytes)?;
    mcp_trace
        .validate_version()
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidInput, error))?;
    if mcp_trace.host != "codex-cli" {
        return Err("MCP trace host must be codex-cli".into());
    }
    if mcp_trace.host_version != rollout.host_version {
        return Err("Codex rollout and MCP trace host versions differ".into());
    }
    if mcp_trace.model.as_deref() != Some(rollout.model.as_str())
        || mcp_trace.provider.as_deref() != Some(rollout.provider.as_str())
    {
        return Err("Codex rollout and MCP trace model/provider identities differ".into());
    }
    validate_mcp_host_order(&mcp_trace)?;
    let mcp_calls = extract_mcp_calls(&mcp_trace)?;
    let correlation = correlate_calls(&rollout.calls, &mcp_calls)?;
    let final_usage = rollout
        .usage_snapshots
        .last()
        .map(|(_, _, usage)| *usage)
        .ok_or("Codex rollout has no provider usage snapshot")?;
    let distinct_provider_usage_snapshots = rollout
        .usage_snapshots
        .iter()
        .map(|(_, _, usage)| *usage)
        .collect::<HashSet<_>>()
        .len();
    let provider_usage = usage_receipt(final_usage)?;
    let usage_indexes = rollout
        .usage_snapshots
        .iter()
        .map(|(index, turn, _)| (*index, *turn))
        .collect::<Vec<_>>();
    let mut seen_content_hashes = HashSet::new();
    let mut tool_calls = Vec::with_capacity(rollout.calls.len());
    for (sequence, (call, mcp_call)) in rollout.calls.iter().zip(&mcp_calls).enumerate() {
        let output_bytes = call
            .output_bytes
            .as_deref()
            .ok_or("Codex MCP call has no function_call_output")?;
        let output_record_index = call
            .output_record_index
            .ok_or("Codex MCP call has no output record index")?;
        let known_hash_followup = mcp_call
            .expected_hash
            .as_ref()
            .is_some_and(|hash| seen_content_hashes.contains(hash));
        if let Some(hash) = &mcp_call.content_hash {
            seen_content_hashes.insert(hash.clone());
        }
        tool_calls.push(ToolCallReceipt {
            sequence,
            turn: call.turn,
            tool_name: call.tool_name.clone(),
            call_id_blake3: blake3::hash(call.call_id.as_bytes()).to_hex().to_string(),
            output_blake3: blake3::hash(output_bytes).to_hex().to_string(),
            output_tokens: tokenizer.count(&String::from_utf8_lossy(output_bytes)),
            expected_hash_supplied: mcp_call.expected_hash.is_some(),
            known_hash_followup,
            result_status: mcp_call.status.clone(),
            result_is_error: mcp_call.is_error,
            followed_by_model_response: rollout
                .model_response_indexes
                .iter()
                .any(|(index, turn)| *turn == call.turn && *index > output_record_index),
            followed_by_provider_usage: usage_indexes
                .iter()
                .any(|(index, turn)| *turn == call.turn && *index > output_record_index),
        });
    }
    if tool_calls
        .iter()
        .any(|call| !call.followed_by_model_response || !call.followed_by_provider_usage)
    {
        return Err(
            "a Codex MCP tool output was not followed by a model response and provider usage"
                .into(),
        );
    }
    let repository = mcp_trace
        .repository
        .as_ref()
        .map(|identity| {
            validate_lower_hex(&identity.revision, 40, "repository revision")?;
            validate_lower_hex(
                &identity.dirty_fingerprint,
                64,
                "repository dirty fingerprint",
            )?;
            Ok::<_, Box<dyn Error>>(RepositoryReceipt {
                revision: identity.revision.clone(),
                dirty_fingerprint: identity.dirty_fingerprint.clone(),
            })
        })
        .transpose()?;
    Ok(HostReceipt {
        schema_version: RECEIPT_SCHEMA_V1,
        source_rollout_blake3: blake3::hash(rollout_bytes).to_hex().to_string(),
        source_mcp_trace_blake3: blake3::hash(mcp_bytes).to_hex().to_string(),
        source_mcp_content_blake3: mcp_trace.content_blake3()?,
        harness_revision: identities.harness_revision.clone(),
        runtime_revision: identities.runtime_revision.clone(),
        host_binary_blake3: identities.host_binary_blake3.clone(),
        runtime_binary_blake3: identities.runtime_binary_blake3.clone(),
        capture_binary_blake3: identities.capture_binary_blake3.clone(),
        receipt_binary_blake3: identities.receipt_binary_blake3.clone(),
        host: "codex-cli",
        host_version: rollout.host_version,
        model: rollout.model,
        provider: rollout.provider,
        tokenizer: tokenizer.name().to_owned(),
        token_count_exact: tokenizer.is_exact(),
        host_os: std::env::consts::OS,
        host_arch: std::env::consts::ARCH,
        repository,
        rollout_record_count: rollout.record_count,
        turn_count: rollout.turn_count,
        completed_turns: rollout.completed_turns,
        aborted_turns: rollout.aborted_turns,
        compactions: rollout.compactions,
        distinct_provider_usage_snapshots,
        total_input_tokens: final_usage.input_tokens,
        provider_usage,
        tool_calls,
        mcp_correlation: correlation,
        privacy: PrivacyBoundary {
            raw_rollout_retained: false,
            raw_mcp_messages_retained: false,
            prompts_retained: false,
            tool_arguments_retained: false,
            tool_outputs_retained: false,
            credentials_retained: false,
            absolute_paths_retained: false,
            session_and_call_ids_hashed_or_omitted: true,
        },
        limitations: vec![
            "Codex rollout usage is provider-native session accounting, but the receipt does not contain provider request framing.",
            "Codex exposes cached input but not cache-creation input, so cache_creation_input_tokens remains null.",
            "Tool-output token counts use the selected local tokenizer and are not provider billing counts.",
            "A later provider-usage event proves another model request occurred after each tool output; it does not expose the provider's serialized request body.",
            "Private rollout and MCP payloads are represented only by BLAKE3 identities and must remain outside version control.",
        ],
    })
}

fn parse_rollout(bytes: &[u8], mcp_server: &str) -> Result<RolloutSummary, Box<dyn Error>> {
    let text = std::str::from_utf8(bytes)?;
    let mut host_version = None;
    let mut provider = None;
    let mut models = HashSet::new();
    let mut active_turn = None;
    let mut turn_count = 0usize;
    let mut completed_turns = 0usize;
    let mut aborted_turns = 0usize;
    let mut calls = Vec::<RolloutCall>::new();
    let mut call_indexes = HashMap::<String, usize>::new();
    let mut compactions = Vec::new();
    let mut usage_snapshots = Vec::new();
    let mut model_response_indexes = Vec::new();
    let mut last_usage = None;
    let mut record_count = 0usize;

    for (record_index, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        record_count += 1;
        let record: Value = serde_json::from_str(line).map_err(|error| {
            format!("invalid Codex rollout JSONL record {record_index}: {error}")
        })?;
        let record_type = required_str(&record, "/type", "rollout record type")?;
        let payload = record.pointer("/payload").unwrap_or(&Value::Null);
        match record_type {
            "session_meta" => {
                if record_index != 0 || host_version.is_some() {
                    return Err("Codex rollout must start with exactly one session_meta".into());
                }
                let version = required_str(payload, "/cli_version", "Codex CLI version")?;
                let model_provider =
                    required_str(payload, "/model_provider", "Codex model provider")?;
                validate_safe_metadata(version, "Codex CLI version")?;
                validate_safe_metadata(model_provider, "Codex model provider")?;
                host_version = Some(version.to_owned());
                provider = Some(model_provider.to_owned());
            }
            "turn_context" => {
                let model = required_str(payload, "/model", "Codex turn model")?;
                validate_safe_metadata(model, "Codex model")?;
                models.insert(model.to_owned());
            }
            "event_msg" => match payload.pointer("/type").and_then(Value::as_str) {
                Some("task_started") => {
                    if active_turn.is_some() {
                        return Err(
                            "Codex rollout starts a turn before closing the previous turn".into(),
                        );
                    }
                    active_turn = Some(turn_count);
                    turn_count += 1;
                }
                Some("task_complete") => {
                    let turn =
                        active_turn.ok_or("Codex rollout completes a turn that was not started")?;
                    if calls
                        .iter()
                        .any(|call| call.turn == turn && call.output.is_none())
                    {
                        return Err(
                            "Codex rollout completes a turn with an unresolved MCP call".into()
                        );
                    }
                    if active_turn.take().is_none() {
                        return Err("Codex rollout completes a turn that was not started".into());
                    }
                    completed_turns += 1;
                }
                Some("turn_aborted") => {
                    if active_turn.take().is_none() {
                        return Err("Codex rollout aborts a turn that was not started".into());
                    }
                    aborted_turns += 1;
                }
                Some("token_count") => {
                    let turn =
                        active_turn.ok_or("Codex provider usage occurs outside an active turn")?;
                    if let Some(total) = payload.pointer("/info/total_token_usage") {
                        let usage = parse_usage(total)?;
                        if let Some(previous) = last_usage {
                            validate_monotonic_usage(previous, usage)?;
                        }
                        last_usage = Some(usage);
                        usage_snapshots.push((record_index, turn, usage));
                    }
                }
                Some("mcp_tool_call_end")
                    if payload
                        .pointer("/invocation/server")
                        .and_then(Value::as_str)
                        == Some(mcp_server) =>
                {
                    let turn =
                        active_turn.ok_or("Codex MCP result occurs outside an active turn")?;
                    let tool_name = required_str(payload, "/invocation/tool", "Codex MCP tool")?;
                    validate_safe_name(tool_name, "Codex MCP tool")?;
                    let call_id = required_str(payload, "/call_id", "Codex MCP event call ID")?;
                    if call_id.trim().is_empty() || call_indexes.contains_key(call_id) {
                        return Err(
                            "Codex rollout has an empty or duplicate MCP event call ID".into()
                        );
                    }
                    let result = payload
                        .pointer("/result/Ok")
                        .ok_or("Codex MCP tool event did not complete successfully")?;
                    call_indexes.insert(call_id.to_owned(), calls.len());
                    calls.push(RolloutCall {
                        turn,
                        tool_name: tool_name.to_owned(),
                        call_id: call_id.to_owned(),
                        call_record_index: record_index,
                        output_record_index: Some(record_index),
                        output: Some(result.clone()),
                        output_bytes: Some(serde_json::to_vec(result)?),
                    });
                }
                _ => {}
            },
            "response_item" => {
                let payload_type = payload.pointer("/type").and_then(Value::as_str);
                if matches!(
                    payload_type,
                    Some("reasoning" | "message" | "function_call" | "custom_tool_call")
                ) && let Some(turn) = active_turn
                {
                    model_response_indexes.push((record_index, turn));
                }
                if matches!(payload_type, Some("function_call"))
                    && let Some(tool_name) = codex_mcp_tool_name(payload, mcp_server)?
                {
                    let turn = active_turn.ok_or("Codex MCP call occurs outside an active turn")?;
                    let call_id = required_str(payload, "/call_id", "Codex function call ID")?;
                    if call_id.trim().is_empty() || call_indexes.contains_key(call_id) {
                        return Err(
                            "Codex rollout has an empty or duplicate function call ID".into()
                        );
                    }
                    call_indexes.insert(call_id.to_owned(), calls.len());
                    calls.push(RolloutCall {
                        turn,
                        tool_name,
                        call_id: call_id.to_owned(),
                        call_record_index: record_index,
                        output_record_index: None,
                        output: None,
                        output_bytes: None,
                    });
                } else if matches!(payload_type, Some("function_call_output")) {
                    let call_id = required_str(payload, "/call_id", "Codex function output ID")?;
                    if let Some(call_index) = call_indexes.get(call_id).copied() {
                        let call = &mut calls[call_index];
                        if active_turn != Some(call.turn)
                            || call.output.is_some()
                            || record_index <= call.call_record_index
                        {
                            return Err(
                                "Codex rollout has a duplicate or misordered tool output".into()
                            );
                        }
                        let output = payload
                            .pointer("/output")
                            .ok_or("Codex function output has no output value")?;
                        call.output = Some(parse_embedded_json(output));
                        call.output_bytes = Some(output_bytes(output)?);
                        call.output_record_index = Some(record_index);
                    }
                }
            }
            "compacted" => {
                let window_number = payload.pointer("/window_number").and_then(Value::as_u64);
                if let (Some(previous), Some(current)) = (
                    compactions
                        .last()
                        .and_then(|receipt: &CompactionReceipt| receipt.window_number),
                    window_number,
                ) && current <= previous
                {
                    return Err("Codex compaction window numbers are not increasing".into());
                }
                compactions.push(CompactionReceipt {
                    sequence: compactions.len(),
                    after_turn: active_turn.or_else(|| turn_count.checked_sub(1)),
                    window_number,
                    replacement_history_entries: payload
                        .pointer("/replacement_history")
                        .and_then(Value::as_array)
                        .map_or(0, Vec::len),
                });
            }
            _ => {}
        }
    }
    if host_version.is_none() {
        return Err("Codex rollout has no session_meta".into());
    }
    if active_turn.is_some() {
        return Err("Codex rollout ends with an open turn".into());
    }
    if turn_count == 0 || completed_turns + aborted_turns != turn_count {
        return Err("Codex rollout turn lifecycle is incomplete".into());
    }
    if models.len() != 1 {
        return Err("Codex rollout must use exactly one model".into());
    }
    if calls.is_empty() {
        return Err("Codex rollout has no calls for the selected MCP server".into());
    }
    if calls.iter().any(|call| call.output.is_none()) {
        return Err("Codex rollout has an MCP call without a result".into());
    }
    Ok(RolloutSummary {
        record_count,
        host_version: host_version.expect("validated host version"),
        model: models.into_iter().next().expect("validated model"),
        provider: provider.expect("validated provider"),
        turn_count,
        completed_turns,
        aborted_turns,
        compactions,
        usage_snapshots,
        model_response_indexes,
        calls,
    })
}

fn codex_mcp_tool_name(payload: &Value, server: &str) -> Result<Option<String>, Box<dyn Error>> {
    let name = required_str(payload, "/name", "Codex function name")?;
    let tool_name = if payload.pointer("/namespace").and_then(Value::as_str) == Some(server) {
        Some(name)
    } else {
        name.strip_prefix(&format!("mcp__{server}__"))
    };
    tool_name
        .map(|name| {
            validate_safe_name(name, "Codex MCP tool")?;
            Ok(name.to_owned())
        })
        .transpose()
}

fn parse_usage(value: &Value) -> Result<UsageNumbers, Box<dyn Error>> {
    let usage = UsageNumbers {
        input_tokens: required_u64(value, "/input_tokens", "input_tokens")?,
        cached_input_tokens: required_u64(value, "/cached_input_tokens", "cached_input_tokens")?,
        output_tokens: required_u64(value, "/output_tokens", "output_tokens")?,
        reasoning_output_tokens: required_u64(
            value,
            "/reasoning_output_tokens",
            "reasoning_output_tokens",
        )?,
        total_tokens: required_u64(value, "/total_tokens", "total_tokens")?,
    };
    if usage.cached_input_tokens > usage.input_tokens
        || usage.reasoning_output_tokens > usage.output_tokens
        || usage.input_tokens.checked_add(usage.output_tokens) != Some(usage.total_tokens)
    {
        return Err("Codex provider usage fields are internally inconsistent".into());
    }
    Ok(usage)
}

fn validate_monotonic_usage(
    previous: UsageNumbers,
    current: UsageNumbers,
) -> Result<(), Box<dyn Error>> {
    if current.input_tokens < previous.input_tokens
        || current.cached_input_tokens < previous.cached_input_tokens
        || current.output_tokens < previous.output_tokens
        || current.reasoning_output_tokens < previous.reasoning_output_tokens
        || current.total_tokens < previous.total_tokens
    {
        return Err("Codex cumulative provider usage regressed".into());
    }
    Ok(())
}

fn usage_receipt(usage: UsageNumbers) -> Result<ProviderUsage, Box<dyn Error>> {
    Ok(ProviderUsage {
        uncached_input_tokens: Some(
            usage
                .input_tokens
                .checked_sub(usage.cached_input_tokens)
                .ok_or("cached input exceeds total input")?,
        ),
        cache_creation_input_tokens: None,
        cache_read_input_tokens: Some(usage.cached_input_tokens),
        output_tokens: Some(usage.output_tokens),
        reasoning_tokens: Some(usage.reasoning_output_tokens),
    })
}

fn validate_mcp_host_order(trace: &Trace) -> Result<(), Box<dyn Error>> {
    let mut initialize_request = None;
    let mut initialized_notification = None;
    let mut tools_list_request = None;
    let mut first_tool_call = None;
    let mut responses = HashMap::<String, usize>::new();
    for (index, event) in trace.events.iter().enumerate() {
        let Some(message) = event_message(event)? else {
            continue;
        };
        let method = message.pointer("/method").and_then(Value::as_str);
        if matches!(event.direction, Direction::ClientToServer) {
            match method {
                Some("initialize") => {
                    if initialize_request.is_some() {
                        return Err("MCP trace has duplicate initialize requests".into());
                    }
                    initialize_request = Some((
                        index,
                        json_rpc_id(&message).ok_or("MCP initialize request has no scalar ID")?,
                    ));
                }
                Some("notifications/initialized")
                    if initialized_notification.replace(index).is_some() =>
                {
                    return Err("MCP trace has duplicate initialized notifications".into());
                }
                Some("tools/list") => {
                    if tools_list_request.is_some() {
                        return Err("MCP trace has duplicate tools/list requests".into());
                    }
                    tools_list_request = Some((
                        index,
                        json_rpc_id(&message).ok_or("MCP tools/list request has no scalar ID")?,
                    ));
                }
                Some("tools/call") => {
                    first_tool_call.get_or_insert(index);
                }
                _ => {}
            }
        } else if matches!(event.direction, Direction::ServerToClient)
            && let Some(id) = json_rpc_id(&message)
            && responses.insert(id, index).is_some()
        {
            return Err("MCP trace has duplicate response IDs".into());
        }
    }
    let (initialize_request_index, initialize_id) =
        initialize_request.ok_or("MCP trace has no initialize request")?;
    let initialize_response_index = responses
        .get(&initialize_id)
        .copied()
        .ok_or("MCP trace has no initialize response")?;
    let initialized_notification_index =
        initialized_notification.ok_or("MCP trace has no initialized notification")?;
    let (tools_list_request_index, tools_list_id) =
        tools_list_request.ok_or("MCP trace has no tools/list request")?;
    let tools_list_response_index = responses
        .get(&tools_list_id)
        .copied()
        .ok_or("MCP trace has no tools/list response")?;
    let first_tool_call_index = first_tool_call.ok_or("MCP trace has no tools/call request")?;
    if !(initialize_request_index < initialize_response_index
        && initialize_response_index < initialized_notification_index
        && initialized_notification_index < tools_list_request_index
        && tools_list_request_index < tools_list_response_index
        && tools_list_response_index < first_tool_call_index)
    {
        return Err("MCP host events are out of protocol order".into());
    }
    Ok(())
}

fn extract_mcp_calls(trace: &Trace) -> Result<Vec<McpCall>, Box<dyn Error>> {
    let mut requests = Vec::<(String, String, Option<String>)>::new();
    let mut results = HashMap::<String, Value>::new();
    for event in &trace.events {
        let Some(message) = event_message(event)? else {
            continue;
        };
        if matches!(event.direction, Direction::ClientToServer)
            && message.pointer("/method").and_then(Value::as_str) == Some("tools/call")
        {
            let id = json_rpc_id(&message).ok_or("MCP tools/call request has no scalar ID")?;
            let name = required_str(&message, "/params/name", "MCP tool name")?;
            validate_safe_name(name, "MCP tool")?;
            let expected_hash = message
                .pointer("/params/arguments/expected_hash")
                .and_then(Value::as_str)
                .map(str::to_owned);
            if let Some(hash) = &expected_hash {
                validate_lower_hex(hash, 32, "MCP expected_hash")?;
            }
            requests.push((id, name.to_owned(), expected_hash));
        } else if matches!(event.direction, Direction::ServerToClient)
            && let Some(id) = json_rpc_id(&message)
            && let Some(result) = message.pointer("/result")
            && results.insert(id, result.clone()).is_some()
        {
            return Err("MCP trace has duplicate response IDs".into());
        }
    }
    requests
        .into_iter()
        .map(|(id, tool_name, expected_hash)| {
            let result = results
                .remove(&id)
                .ok_or("MCP tools/call has no matching result")?;
            let content_hash = result
                .pointer("/structuredContent/content_hash")
                .and_then(Value::as_str)
                .map(str::to_owned);
            if let Some(hash) = &content_hash {
                validate_lower_hex(hash, 32, "MCP result content_hash")?;
            }
            let status = result
                .pointer("/structuredContent/status")
                .and_then(Value::as_str)
                .map(str::to_owned);
            if let Some(status) = &status {
                validate_safe_name(status, "MCP result status")?;
            }
            let is_error = result
                .pointer("/isError")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            Ok(McpCall {
                tool_name,
                result,
                expected_hash,
                content_hash,
                status,
                is_error,
            })
        })
        .collect()
}

fn correlate_calls(
    rollout: &[RolloutCall],
    mcp: &[McpCall],
) -> Result<McpCorrelation, Box<dyn Error>> {
    if rollout.len() != mcp.len() {
        return Err(format!(
            "Codex rollout has {} selected MCP calls but the MCP trace has {}",
            rollout.len(),
            mcp.len()
        )
        .into());
    }
    let tool_order_matches = rollout
        .iter()
        .zip(mcp)
        .all(|(rollout, mcp)| rollout.tool_name == mcp.tool_name);
    if !tool_order_matches {
        return Err("Codex rollout and MCP trace tool order differs".into());
    }
    let mut semantic_output_matches = 0usize;
    let mut seen_content_hashes = HashSet::new();
    let mut known_hash_followups = 0usize;
    let mut not_modified_results = 0usize;
    for (rollout, mcp) in rollout.iter().zip(mcp) {
        let rollout_output = rollout.output.as_ref().expect("validated rollout output");
        if canonical_json_hash(rollout_output) != canonical_json_hash(&mcp.result) {
            return Err(format!(
                "Codex rollout and MCP result differ for tool {}",
                rollout.tool_name
            )
            .into());
        }
        semantic_output_matches += 1;
        known_hash_followups += usize::from(
            mcp.expected_hash
                .as_ref()
                .is_some_and(|hash| seen_content_hashes.contains(hash)),
        );
        if let Some(hash) = &mcp.content_hash {
            seen_content_hashes.insert(hash.clone());
        }
        not_modified_results += usize::from(mcp.status.as_deref() == Some("not_modified"));
    }
    Ok(McpCorrelation {
        rollout_tool_calls: rollout.len(),
        mcp_tool_calls: mcp.len(),
        tool_order_matches,
        protocol_order_valid: true,
        semantic_output_matches,
        all_semantic_outputs_match: semantic_output_matches == rollout.len(),
        known_hash_followups,
        not_modified_results,
    })
}

fn event_message(event: &wire_trace::Event) -> Result<Option<Value>, Box<dyn Error>> {
    match (&event.raw_json, &event.message) {
        (Some(raw), _) => Ok(Some(serde_json::from_str(raw)?)),
        (None, Some(message)) => Ok(Some(message.clone())),
        (None, None) if matches!(event.direction, Direction::Handoff) => Ok(None),
        (None, None) => Err("MCP trace event has neither raw_json nor message".into()),
    }
}

fn json_rpc_id(message: &Value) -> Option<String> {
    match message.pointer("/id")? {
        Value::String(value) => Some(format!("s:{value}")),
        Value::Number(value) => Some(format!("n:{value}")),
        _ => None,
    }
}

fn parse_embedded_json(value: &Value) -> Value {
    value
        .as_str()
        .and_then(|text| serde_json::from_str(text).ok())
        .unwrap_or_else(|| value.clone())
}

fn output_bytes(value: &Value) -> Result<Vec<u8>, Box<dyn Error>> {
    Ok(match value {
        Value::String(text) => text.as_bytes().to_vec(),
        _ => serde_json::to_vec(value)?,
    })
}

fn canonical_json_hash(value: &Value) -> blake3::Hash {
    let canonical = canonicalize_json(value);
    blake3::hash(&serde_json::to_vec(&canonical).expect("JSON value serialization"))
}

fn hash_file(path: &Path) -> Result<String, Box<dyn Error>> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.file_type().is_file() {
        return Err(format!("frozen artifact {} is not a regular file", path.display()).into());
    }
    let mut file = File::open(path)?;
    let mut hasher = blake3::Hasher::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hasher.finalize().to_hex().to_string())
}

fn canonicalize_json(value: &Value) -> Value {
    match value {
        Value::Array(values) => Value::Array(values.iter().map(canonicalize_json).collect()),
        Value::Object(values) => {
            let sorted = values
                .iter()
                .map(|(key, value)| (key.clone(), canonicalize_json(value)))
                .collect::<BTreeMap<_, _>>();
            Value::Object(sorted.into_iter().collect::<Map<_, _>>())
        }
        _ => value.clone(),
    }
}

fn required_str<'a>(
    value: &'a Value,
    pointer: &str,
    field: &str,
) -> Result<&'a str, Box<dyn Error>> {
    value
        .pointer(pointer)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| format!("{field} is missing or empty").into())
}

fn required_u64(value: &Value, pointer: &str, field: &str) -> Result<u64, Box<dyn Error>> {
    value
        .pointer(pointer)
        .and_then(Value::as_u64)
        .ok_or_else(|| format!("{field} is missing or not an unsigned integer").into())
}

fn validate_safe_metadata(value: &str, field: &str) -> Result<(), Box<dyn Error>> {
    if value.starts_with('/')
        || value.contains('\\')
        || !value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b'+' | b'/' | b':')
        })
    {
        return Err(format!("{field} is not safe publishable metadata").into());
    }
    Ok(())
}

fn validate_safe_name(value: &str, field: &str) -> Result<(), Box<dyn Error>> {
    if value.is_empty()
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
    {
        return Err(format!("{field} is not a safe identifier").into());
    }
    Ok(())
}

fn validate_lower_hex(value: &str, length: usize, field: &str) -> Result<(), Box<dyn Error>> {
    if value.len() != length
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        return Err(format!("{field} must be {length} lowercase hexadecimal characters").into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use wire_trace::{Event, RepositoryIdentity, TRACE_SCHEMA_V2};

    #[test]
    fn receipt_correlates_usage_compaction_and_tool_output() {
        let (rollout, mcp) = valid_inputs("private source /home/alice/project sk-secret");
        let tokenizer = Tokenizer::from_str("cl100k_base", false).expect("tokenizer");

        let receipt = build_receipt(&rollout, &mcp, "leantoken", &tokenizer, &identities())
            .expect("valid host receipt");

        assert_eq!(receipt.schema_version, RECEIPT_SCHEMA_V1);
        assert_eq!(receipt.turn_count, 1);
        assert_eq!(receipt.completed_turns, 1);
        assert_eq!(receipt.compactions.len(), 1);
        assert_eq!(receipt.compactions[0].replacement_history_entries, 1);
        assert_eq!(receipt.total_input_tokens, 100);
        assert_eq!(receipt.provider_usage.uncached_input_tokens, Some(60));
        assert_eq!(receipt.provider_usage.cache_read_input_tokens, Some(40));
        assert_eq!(receipt.provider_usage.cache_creation_input_tokens, None);
        assert_eq!(receipt.tool_calls.len(), 1);
        assert!(receipt.tool_calls[0].followed_by_model_response);
        assert!(receipt.tool_calls[0].followed_by_provider_usage);
        assert!(receipt.mcp_correlation.tool_order_matches);
        assert!(receipt.mcp_correlation.all_semantic_outputs_match);
    }

    #[test]
    fn receipt_serialization_does_not_retain_private_fields_or_absolute_paths() {
        let secret = "private source /home/alice/project sk-secret";
        let (rollout, mcp) = valid_inputs(secret);
        let tokenizer = Tokenizer::from_str("cl100k_base", false).expect("tokenizer");
        let receipt = build_receipt(&rollout, &mcp, "leantoken", &tokenizer, &identities())
            .expect("valid host receipt");
        let json = serde_json::to_string_pretty(&receipt).expect("serialize receipt");

        assert!(!json.contains(secret));
        assert!(!json.contains("/home/alice"));
        assert!(!json.contains("sk-secret"));
        assert!(!json.contains("private prompt"));
        assert!(!json.contains("private argument"));
        assert!(!json.contains("private response"));
        assert!(!json.contains("call-private-id"));
        assert!(json.contains("call_id_blake3"));
        assert!(json.contains("output_blake3"));
    }

    #[test]
    fn identical_private_inputs_produce_identical_receipts() {
        let (rollout, mcp) = valid_inputs("result");
        let tokenizer = Tokenizer::from_str("cl100k_base", false).expect("tokenizer");
        let first = serde_json::to_vec(
            &build_receipt(&rollout, &mcp, "leantoken", &tokenizer, &identities())
                .expect("first receipt"),
        )
        .expect("serialize first");
        let second = serde_json::to_vec(
            &build_receipt(&rollout, &mcp, "leantoken", &tokenizer, &identities())
                .expect("second receipt"),
        )
        .expect("serialize second");

        assert_eq!(first, second);
    }

    #[test]
    fn receipt_accepts_codex_mcp_tool_end_events_without_retaining_invocation() {
        let (rollout, mcp) = valid_inputs("event result /home/alice sk-secret");
        let trace: Trace = serde_json::from_slice(&mcp).expect("trace");
        let result = trace.events[6]
            .message
            .as_ref()
            .and_then(|message| message.pointer("/result"))
            .expect("MCP result")
            .clone();
        let mut lines = rollout
            .split(|byte| *byte == b'\n')
            .filter(|line| !line.is_empty())
            .map(|line| serde_json::from_slice::<Value>(line).expect("rollout line"))
            .filter(|line| {
                !matches!(
                    line.pointer("/payload/type").and_then(Value::as_str),
                    Some("function_call" | "function_call_output")
                )
            })
            .collect::<Vec<_>>();
        let insert_at = lines
            .iter()
            .position(|line| {
                line.pointer("/payload/type").and_then(Value::as_str) == Some("task_started")
            })
            .expect("task start")
            + 1;
        lines.insert(
            insert_at,
            serde_json::json!({
                "type": "event_msg",
                "payload": {
                    "type": "mcp_tool_call_end",
                    "call_id": "event-private-id",
                    "invocation": {
                        "server": "leantoken",
                        "tool": "leantoken_files",
                        "arguments": {"secret": "/home/alice"}
                    },
                    "result": {"Ok": result}
                }
            }),
        );
        let tokenizer = Tokenizer::from_str("cl100k_base", false).expect("tokenizer");

        let receipt = build_receipt(&jsonl(&lines), &mcp, "leantoken", &tokenizer, &identities())
            .expect("event-style receipt");
        let json = serde_json::to_string(&receipt).expect("receipt JSON");

        assert_eq!(receipt.tool_calls.len(), 1);
        assert!(!json.contains("event-private-id"));
        assert!(!json.contains("/home/alice"));
        assert!(!json.contains("event result"));
    }

    #[test]
    fn receipt_rejects_mcp_tool_order_or_output_mismatch() {
        let (rollout, mcp) = valid_inputs("result");
        let tokenizer = Tokenizer::from_str("cl100k_base", false).expect("tokenizer");
        let mut trace: Trace = serde_json::from_slice(&mcp).expect("trace");
        let call = trace.events[5].message.as_mut().expect("call message");
        call["params"]["name"] = Value::String("leantoken_read".into());
        trace.seal_content_hash().expect("reseal");
        let mismatched = serde_json::to_vec(&trace).expect("trace JSON");

        assert!(
            build_receipt(
                &rollout,
                &mismatched,
                "leantoken",
                &tokenizer,
                &identities(),
            )
            .expect_err("tool mismatch")
            .to_string()
            .contains("tool order")
        );

        let (_, mcp) = valid_inputs("different result");
        assert!(
            build_receipt(&rollout, &mcp, "leantoken", &tokenizer, &identities())
                .expect_err("output mismatch")
                .to_string()
                .contains("MCP result differ")
        );
    }

    #[test]
    fn receipt_rejects_out_of_order_mcp_host_lifecycle() {
        let (rollout, mcp) = valid_inputs("result");
        let tokenizer = Tokenizer::from_str("cl100k_base", false).expect("tokenizer");
        let mut trace: Trace = serde_json::from_slice(&mcp).expect("trace");
        let initialized = trace.events[2].message.clone();
        trace.events[2].message = trace.events[3].message.clone();
        trace.events[3].message = initialized;
        trace.seal_content_hash().expect("reseal");

        assert!(
            build_receipt(
                &rollout,
                &serde_json::to_vec(&trace).expect("trace JSON"),
                "leantoken",
                &tokenizer,
                &identities(),
            )
            .expect_err("out-of-order lifecycle")
            .to_string()
            .contains("protocol order")
        );
    }

    #[test]
    fn receipt_rejects_regressing_provider_usage_and_open_turns() {
        let (rollout, mcp) = valid_inputs("result");
        let tokenizer = Tokenizer::from_str("cl100k_base", false).expect("tokenizer");
        let mut lines = rollout
            .split(|byte| *byte == b'\n')
            .filter(|line| !line.is_empty())
            .map(|line| serde_json::from_slice::<Value>(line).expect("rollout line"))
            .collect::<Vec<_>>();
        let regressing = token_count(90, 30, 10, 2);
        lines.insert(lines.len() - 1, regressing);
        let regressing = jsonl(&lines);
        assert!(
            build_receipt(&regressing, &mcp, "leantoken", &tokenizer, &identities(),)
                .expect_err("usage regression")
                .to_string()
                .contains("usage regressed")
        );

        let mut lines = lines;
        lines.retain(|line| {
            line.pointer("/payload/type").and_then(Value::as_str) != Some("task_complete")
        });
        let open_turn = jsonl(&lines[..lines.len() - 1]);
        assert!(
            build_receipt(&open_turn, &mcp, "leantoken", &tokenizer, &identities(),)
                .expect_err("open turn")
                .to_string()
                .contains("open turn")
        );
    }

    #[test]
    fn receipt_does_not_use_a_later_turn_as_tool_followup_evidence() {
        let (rollout, mcp) = valid_inputs("result");
        let tokenizer = Tokenizer::from_str("cl100k_base", false).expect("tokenizer");
        let mut lines = rollout
            .split(|byte| *byte == b'\n')
            .filter(|line| !line.is_empty())
            .map(|line| serde_json::from_slice::<Value>(line).expect("rollout line"))
            .filter(|line| {
                line.pointer("/payload/type").and_then(Value::as_str) != Some("token_count")
                    && !(line.pointer("/type").and_then(Value::as_str) == Some("response_item")
                        && line.pointer("/payload/type").and_then(Value::as_str) == Some("message"))
            })
            .collect::<Vec<_>>();
        lines.extend([
            serde_json::json!({"type": "event_msg", "payload": {"type": "task_started"}}),
            serde_json::json!({
                "type": "response_item",
                "payload": {"type": "message", "role": "assistant", "content": []}
            }),
            token_count(100, 40, 20, 5),
            serde_json::json!({"type": "event_msg", "payload": {"type": "task_complete"}}),
        ]);

        assert!(
            build_receipt(&jsonl(&lines), &mcp, "leantoken", &tokenizer, &identities(),)
                .expect_err("cross-turn followup evidence")
                .to_string()
                .contains("not followed")
        );
    }

    #[test]
    fn receipt_rejects_non_hash_repository_identity() {
        let (rollout, mcp) = valid_inputs("result");
        let tokenizer = Tokenizer::from_str("cl100k_base", false).expect("tokenizer");
        let mut trace: Trace = serde_json::from_slice(&mcp).expect("trace");
        trace
            .repository
            .as_mut()
            .expect("repository")
            .dirty_fingerprint = "/home/alice/private".into();
        trace.seal_content_hash().expect("reseal");

        assert!(
            build_receipt(
                &rollout,
                &serde_json::to_vec(&trace).expect("trace JSON"),
                "leantoken",
                &tokenizer,
                &identities(),
            )
            .expect_err("unsafe repository identity")
            .to_string()
            .contains("dirty fingerprint")
        );
    }

    #[test]
    fn mcp_calls_track_known_hash_not_modified_followups() {
        let (_, mcp) = valid_inputs("result");
        let mut trace: Trace = serde_json::from_slice(&mcp).expect("trace");
        let content_hash = "c".repeat(32);
        let first_result = trace.events[6].message.as_mut().expect("first result");
        first_result["result"]["structuredContent"]["content_hash"] =
            Value::String(content_hash.clone());
        first_result["result"]["structuredContent"]["status"] = Value::String("content".into());
        trace.events.push(trace_event(
            7,
            Direction::ClientToServer,
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": 8,
                "method": "tools/call",
                "params": {
                    "name": "leantoken_files",
                    "arguments": {"expected_hash": content_hash}
                }
            }),
        ));
        trace.events.push(trace_event(
            8,
            Direction::ServerToClient,
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": 8,
                "result": {
                    "content": [],
                    "structuredContent": {
                        "content_hash": "c".repeat(32),
                        "status": "not_modified"
                    }
                }
            }),
        ));
        trace.seal_content_hash().expect("reseal");

        let calls = extract_mcp_calls(&trace).expect("extract calls");

        assert_eq!(calls.len(), 2);
        assert_eq!(
            calls[0].content_hash.as_deref(),
            Some("cccccccccccccccccccccccccccccccc")
        );
        assert_eq!(calls[1].expected_hash, calls[0].content_hash);
        assert_eq!(calls[1].status.as_deref(), Some("not_modified"));
    }

    fn valid_inputs(output_text: &str) -> (Vec<u8>, Vec<u8>) {
        let result = serde_json::json!({
            "content": [{"type": "text", "text": output_text}],
            "structuredContent": {"ok": true}
        });
        let rollout_output = serde_json::to_string(&result).expect("embedded result");
        let rollout = jsonl(&[
            serde_json::json!({
                "type": "session_meta",
                "payload": {
                    "cli_version": "0.144.1",
                    "model_provider": "openai",
                    "cwd": "/home/alice/project",
                    "base_instructions": "private prompt"
                }
            }),
            serde_json::json!({
                "type": "turn_context",
                "payload": {"model": "gpt-5.3-codex", "cwd": "/home/alice/project"}
            }),
            serde_json::json!({"type": "event_msg", "payload": {"type": "task_started"}}),
            serde_json::json!({
                "type": "response_item",
                "payload": {
                    "type": "function_call",
                    "namespace": "leantoken",
                    "name": "leantoken_files",
                    "call_id": "call-private-id",
                    "arguments": "private argument"
                }
            }),
            serde_json::json!({
                "type": "response_item",
                "payload": {
                    "type": "function_call_output",
                    "call_id": "call-private-id",
                    "output": rollout_output
                }
            }),
            serde_json::json!({
                "type": "response_item",
                "payload": {
                    "type": "message",
                    "role": "assistant",
                    "content": [{"type": "output_text", "text": "private response"}]
                }
            }),
            token_count(100, 40, 20, 5),
            serde_json::json!({
                "type": "compacted",
                "payload": {
                    "window_number": 1,
                    "replacement_history": [{"content": "private summary /home/alice"}]
                }
            }),
            serde_json::json!({"type": "event_msg", "payload": {"type": "task_complete"}}),
        ]);
        let mut trace = Trace {
            schema_version: TRACE_SCHEMA_V2,
            trace_id: Some("synthetic-codex-mcp".into()),
            trace_content_blake3: None,
            host: "codex-cli".into(),
            host_version: "0.144.1".into(),
            model: Some("gpt-5.3-codex".into()),
            provider: Some("openai".into()),
            tokenizer: "cl100k_base".into(),
            token_count_exact: Some(true),
            generated_at_unix_seconds: Some(1),
            repository: Some(RepositoryIdentity {
                revision: "a".repeat(40),
                dirty_fingerprint: "b".repeat(64),
            }),
            final_turn: None,
            provider_usage: None,
            provider_total_input_tokens: None,
            outcome: None,
            events: vec![
                trace_event(
                    0,
                    Direction::ClientToServer,
                    serde_json::json!({
                        "jsonrpc": "2.0",
                        "id": 0,
                        "method": "initialize",
                        "params": {}
                    }),
                ),
                trace_event(
                    1,
                    Direction::ServerToClient,
                    serde_json::json!({
                        "jsonrpc": "2.0",
                        "id": 0,
                        "result": {"protocolVersion": "2025-06-18"}
                    }),
                ),
                trace_event(
                    2,
                    Direction::ClientToServer,
                    serde_json::json!({
                        "jsonrpc": "2.0",
                        "method": "notifications/initialized"
                    }),
                ),
                trace_event(
                    3,
                    Direction::ClientToServer,
                    serde_json::json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}),
                ),
                trace_event(
                    4,
                    Direction::ServerToClient,
                    serde_json::json!({"jsonrpc": "2.0", "id": 1, "result": {"tools": []}}),
                ),
                trace_event(
                    5,
                    Direction::ClientToServer,
                    serde_json::json!({
                        "jsonrpc": "2.0",
                        "id": 7,
                        "method": "tools/call",
                        "params": {"name": "leantoken_files", "arguments": {}}
                    }),
                ),
                trace_event(
                    6,
                    Direction::ServerToClient,
                    serde_json::json!({"jsonrpc": "2.0", "id": 7, "result": result}),
                ),
            ],
        };
        trace.seal_content_hash().expect("seal trace");
        (rollout, serde_json::to_vec(&trace).expect("trace JSON"))
    }

    fn token_count(input: u64, cached: u64, output: u64, reasoning: u64) -> Value {
        serde_json::json!({
            "type": "event_msg",
            "payload": {
                "type": "token_count",
                "info": {
                    "total_token_usage": {
                        "input_tokens": input,
                        "cached_input_tokens": cached,
                        "output_tokens": output,
                        "reasoning_output_tokens": reasoning,
                        "total_tokens": input + output
                    }
                }
            }
        })
    }

    fn trace_event(sequence: u64, direction: Direction, message: Value) -> Event {
        Event {
            sequence: Some(sequence),
            direction,
            turn: None,
            timestamp_unix_millis: Some(sequence),
            latency_ms: None,
            category: None,
            message: Some(message),
            raw_json: None,
            provider_visible_payload: None,
            tool_name: None,
            call_id: None,
            result_id: None,
            ranges: Vec::new(),
            visible_through_turn: None,
            stable_prefix: None,
            cache_eligible: None,
            compaction: None,
            provider_usage: None,
            provider_input_tokens: None,
        }
    }

    fn jsonl(values: &[Value]) -> Vec<u8> {
        let mut output = values
            .iter()
            .map(|value| serde_json::to_string(value).expect("JSONL record"))
            .collect::<Vec<_>>()
            .join("\n")
            .into_bytes();
        output.push(b'\n');
        output
    }

    fn identities() -> FrozenIdentities {
        FrozenIdentities {
            harness_revision: "d".repeat(40),
            runtime_revision: "a".repeat(40),
            host_binary_blake3: "1".repeat(64),
            runtime_binary_blake3: "2".repeat(64),
            capture_binary_blake3: "3".repeat(64),
            receipt_binary_blake3: "4".repeat(64),
        }
    }
}
