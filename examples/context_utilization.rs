//! Classify observable downstream signals for context evidence in one trajectory.
//!
//! This is an offline diagnostic. It deliberately reports separate signals
//! instead of collapsing relevance, explicit reuse, rereads, and task outcome
//! into a guessed utilization score.

#[path = "support/model_ab_artifacts.rs"]
#[allow(dead_code)]
mod model_ab_artifacts;

use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::fs::{self, File};
use std::io::Read;
use std::path::{Component, Path, PathBuf};

use clap::{Parser, ValueEnum};
use model_ab_artifacts::{
    ARTIFACT_SCHEMA_V1, RangeIdentity, RunBinding, ToolOutcome, ToolTrace, Trajectory,
};
use serde::Serialize;
use serde_json::Value;

const REPORT_SCHEMA_V1: u32 = 1;
const MAX_ARTIFACT_BYTES: u64 = 64 * 1024 * 1024;
const MAX_TRACE_CALLS: usize = 100_000;
const MAX_TRAJECTORY_EVENTS: usize = 100_000;
const MAX_TRACE_RANGES: usize = 100_000;
const MAX_RANGES_PER_PATH: usize = 10_000;
const MAX_CONTEXT_CALLS: usize = 1_000;
const MAX_CONTEXT_RANGES: usize = 1_000;
const MAX_RELEVANT_PATHS: usize = 256;
const MAX_HASH_INPUTS: usize = 100_000;
const MAX_PATH_BYTES: usize = 4_096;

type DynError = Box<dyn Error>;

#[derive(Debug, Parser)]
struct Args {
    /// Existing model A/B tool-trace artifact.
    #[arg(long)]
    tool_trace: PathBuf,
    /// Existing model A/B exact trajectory artifact.
    #[arg(long)]
    trajectory: PathBuf,
    /// Gold or final-patch path used only as an offline relevance proxy.
    #[arg(long = "relevant-path")]
    relevant_paths: Vec<String>,
    /// Observed task outcome; association is reported without claiming causality.
    #[arg(long, value_enum, default_value_t)]
    outcome: TaskOutcome,
    /// Write the versioned JSON report here.
    #[arg(long)]
    output: PathBuf,
}

#[derive(Debug, Default, Clone, Copy, ValueEnum, Serialize)]
#[serde(rename_all = "snake_case")]
enum TaskOutcome {
    Success,
    Failure,
    #[default]
    Unknown,
}

#[derive(Debug, Clone)]
struct TrajectoryCall<'a> {
    sequence: usize,
    tool: &'a str,
    arguments: &'a Value,
    structured_result: Option<&'a Value>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
struct RangeUtilization {
    origin_sequence: usize,
    repository_generation: u64,
    path: String,
    start_line: usize,
    end_line: usize,
    content_hash: String,
    source_tokens: Option<usize>,
    relevant_path_proxy: bool,
    explicit_hash_input_later: bool,
    exact_reread_later: bool,
    overlap_reread_later: bool,
    no_observed_downstream_signal: bool,
    first_follow_up_sequence: Option<usize>,
}

#[derive(Debug, Default, Serialize, PartialEq, Eq)]
struct SignalCount {
    ranges: usize,
    source_tokens: u64,
    source_tokens_complete: bool,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
struct UtilizationSummary {
    context_calls: usize,
    successful_context_calls: usize,
    failed_context_calls: usize,
    context_ranges: SignalCount,
    relevant_path_proxy: SignalCount,
    explicit_hash_input_later: SignalCount,
    exact_reread_later: SignalCount,
    overlap_reread_later: SignalCount,
    no_observed_downstream_signal: SignalCount,
    receipt_follow_up_calls: usize,
    follow_up_retrieval_calls: usize,
}

#[derive(Debug, Serialize)]
struct SourceReceipt {
    classifier_source_blake3: String,
    tool_trace_blake3: String,
    trajectory_blake3: String,
    artifact_schema_version: u32,
    binding: RunBinding,
}

#[derive(Debug, Serialize)]
struct Report {
    schema_version: u32,
    report_kind: &'static str,
    diagnostic_only: bool,
    outcome: TaskOutcome,
    source: SourceReceipt,
    bounds: Bounds,
    summary: UtilizationSummary,
    ranges: Vec<RangeUtilization>,
    limitations: Vec<&'static str>,
}

#[derive(Debug, Serialize)]
struct Bounds {
    max_artifact_bytes: u64,
    max_trace_calls: usize,
    max_trajectory_events: usize,
    max_trace_ranges: usize,
    max_ranges_per_path: usize,
    max_context_calls: usize,
    max_context_ranges: usize,
    max_relevant_paths: usize,
    max_hash_inputs: usize,
    max_path_bytes: usize,
}

fn main() -> Result<(), DynError> {
    let args = Args::parse();
    if args.relevant_paths.len() > MAX_RELEVANT_PATHS {
        return Err(format!(
            "relevant paths exceed bound: {} > {MAX_RELEVANT_PATHS}",
            args.relevant_paths.len()
        )
        .into());
    }
    let relevant_paths = args
        .relevant_paths
        .iter()
        .map(|path| normalize_path(path))
        .collect::<Result<BTreeSet<_>, _>>()?;
    let trace_bytes = read_bounded(&args.tool_trace)?;
    let trajectory_bytes = read_bounded(&args.trajectory)?;
    let trace: ToolTrace = serde_json::from_slice(&trace_bytes)?;
    let trajectory: Trajectory = serde_json::from_slice(&trajectory_bytes)?;
    validate_artifacts(&trace, &trajectory)?;
    let (summary, ranges) = classify(&trace, &trajectory, &relevant_paths)?;
    let report = Report {
        schema_version: REPORT_SCHEMA_V1,
        report_kind: "context_utilization_trajectory",
        diagnostic_only: true,
        outcome: args.outcome,
        source: SourceReceipt {
            classifier_source_blake3: blake3::hash(include_bytes!("context_utilization.rs"))
                .to_hex()
                .to_string(),
            tool_trace_blake3: blake3::hash(&trace_bytes).to_hex().to_string(),
            trajectory_blake3: blake3::hash(&trajectory_bytes).to_hex().to_string(),
            artifact_schema_version: ARTIFACT_SCHEMA_V1,
            binding: trace.binding,
        },
        bounds: Bounds {
            max_artifact_bytes: MAX_ARTIFACT_BYTES,
            max_trace_calls: MAX_TRACE_CALLS,
            max_trajectory_events: MAX_TRAJECTORY_EVENTS,
            max_trace_ranges: MAX_TRACE_RANGES,
            max_ranges_per_path: MAX_RANGES_PER_PATH,
            max_context_calls: MAX_CONTEXT_CALLS,
            max_context_ranges: MAX_CONTEXT_RANGES,
            max_relevant_paths: MAX_RELEVANT_PATHS,
            max_hash_inputs: MAX_HASH_INPUTS,
            max_path_bytes: MAX_PATH_BYTES,
        },
        summary,
        ranges,
        limitations: vec![
            "relevant_path_proxy is label- or final-patch-based and does not prove that the model used the returned source",
            "explicit_hash_input_later proves that the caller retained an identity, not that the model reasoned from its content",
            "exact and overlap rereads measure downstream retrieval pressure, not useful evidence reuse",
            "no_observed_downstream_signal is not proof that evidence was unused",
            "task outcome is associated with the trajectory but this report does not claim causality",
        ],
    };
    if let Some(parent) = args.output.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&args.output, serde_json::to_vec_pretty(&report)?)?;
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}

fn read_bounded(path: &Path) -> Result<Vec<u8>, DynError> {
    let mut bytes = Vec::new();
    File::open(path)?
        .take(MAX_ARTIFACT_BYTES.saturating_add(1))
        .read_to_end(&mut bytes)?;
    if u64::try_from(bytes.len())? > MAX_ARTIFACT_BYTES {
        return Err(format!("artifact exceeds byte bound: > {MAX_ARTIFACT_BYTES} bytes").into());
    }
    Ok(bytes)
}

fn validate_artifacts(trace: &ToolTrace, trajectory: &Trajectory) -> Result<(), DynError> {
    if trace.schema_version != ARTIFACT_SCHEMA_V1 || trajectory.schema_version != ARTIFACT_SCHEMA_V1
    {
        return Err("unsupported trajectory artifact schema".into());
    }
    if trace.binding != trajectory.binding {
        return Err("tool trace and trajectory bindings differ".into());
    }
    if trace.calls.len() > MAX_TRACE_CALLS {
        return Err("tool trace exceeds call bound".into());
    }
    if trajectory.events.len() > MAX_TRAJECTORY_EVENTS {
        return Err("trajectory exceeds event bound".into());
    }
    let mut prior_sequence = None;
    let mut call_ids = BTreeSet::new();
    let mut range_count = 0usize;
    let mut ranges_by_path = BTreeMap::new();
    for call in &trace.calls {
        if prior_sequence.is_some_and(|prior| call.sequence <= prior) {
            return Err("tool trace sequences are not strictly increasing".into());
        }
        prior_sequence = Some(call.sequence);
        if !call_ids.insert(call.call_id.as_str()) {
            return Err("tool trace contains a duplicate call ID".into());
        }
        for range in &call.ranges {
            validate_range(range)?;
            range_count = range_count.saturating_add(1);
            if range_count > MAX_TRACE_RANGES {
                return Err("tool trace exceeds the total range bound".into());
            }
            let path_count = ranges_by_path
                .entry((range.repository_generation, normalize_path(&range.path)?))
                .or_insert(0usize);
            *path_count = path_count.saturating_add(1);
            if *path_count > MAX_RANGES_PER_PATH {
                return Err("tool trace exceeds the per-path range bound".into());
            }
        }
    }
    Ok(())
}

fn validate_range(range: &RangeIdentity) -> Result<(), DynError> {
    if range.start_line == 0 || range.end_line < range.start_line {
        return Err("tool trace contains an invalid range".into());
    }
    normalize_path(&range.path)?;
    if range.content_hash.len() != 32
        || !range
            .content_hash
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
    {
        return Err("tool trace contains an invalid content hash".into());
    }
    Ok(())
}

fn classify(
    trace: &ToolTrace,
    trajectory: &Trajectory,
    relevant_paths: &BTreeSet<String>,
) -> Result<(UtilizationSummary, Vec<RangeUtilization>), DynError> {
    let trajectory_calls = trajectory_calls(trajectory, trace)?;
    let trace_by_id = trace
        .calls
        .iter()
        .map(|call| (call.call_id.as_str(), call))
        .collect::<BTreeMap<_, _>>();
    let mut context_calls = Vec::new();
    for (trace_id, call) in &trajectory_calls {
        if call.tool == "leantoken_context" {
            let trace_call = trace_by_id
                .get(trace_id.as_str())
                .ok_or("context trajectory call is missing from tool trace")?;
            context_calls.push((call, *trace_call));
        }
    }
    if context_calls.len() > MAX_CONTEXT_CALLS {
        return Err("trajectory exceeds the context-call bound".into());
    }

    let context_range_count = context_calls
        .iter()
        .map(|(_, call)| call.ranges.len())
        .sum::<usize>();
    if context_range_count > MAX_CONTEXT_RANGES {
        return Err("context evidence exceeds range bound".into());
    }

    let retrieval_calls = trajectory_calls
        .values()
        .filter(|call| is_retrieval_tool(call.tool))
        .collect::<Vec<_>>();
    let mut retrieval_ranges_by_path = BTreeMap::new();
    for call in &trace.calls {
        if !trajectory_call_for_trace_id(&trajectory_calls, &call.call_id)
            .is_some_and(|call| is_retrieval_tool(call.tool))
        {
            continue;
        }
        for range in &call.ranges {
            retrieval_ranges_by_path
                .entry((range.repository_generation, normalize_path(&range.path)?))
                .or_insert_with(Vec::new)
                .push((call.sequence, range.start_line, range.end_line));
        }
    }
    let mut hash_input_sequences = BTreeMap::<&str, Vec<usize>>::new();
    let mut hash_input_count = 0usize;
    for call in &retrieval_calls {
        for hash in argument_hashes(call.arguments) {
            hash_input_count = hash_input_count.saturating_add(1);
            if hash_input_count > MAX_HASH_INPUTS {
                return Err("trajectory exceeds the hash-input bound".into());
            }
            hash_input_sequences
                .entry(hash)
                .or_default()
                .push(call.sequence);
        }
    }
    let mut receipt_input_sequences = BTreeMap::<&str, Vec<usize>>::new();
    for call in &retrieval_calls {
        if let Some(receipt_id) = call.arguments["receipt_id"].as_str() {
            receipt_input_sequences
                .entry(receipt_id)
                .or_default()
                .push(call.sequence);
        }
    }
    let mut receipt_follow_up_ids = BTreeSet::new();
    let first_context_sequence = context_calls.iter().map(|(_, call)| call.sequence).min();
    let follow_up_retrieval_ids = retrieval_calls
        .iter()
        .filter(|call| first_context_sequence.is_some_and(|sequence| call.sequence > sequence))
        .map(|call| call.sequence)
        .collect::<BTreeSet<_>>();
    let mut ranges = Vec::with_capacity(context_range_count);
    for (trajectory_call, trace_call) in &context_calls {
        let receipt_id = trajectory_call
            .structured_result
            .and_then(|value| value.pointer("/meta/receipt_id"))
            .and_then(Value::as_str);
        if let Some(sequences) = receipt_id.and_then(|id| receipt_input_sequences.get(id)) {
            for sequence in sequences {
                if *sequence > trace_call.sequence {
                    receipt_follow_up_ids.insert(*sequence);
                }
            }
        }
        for range in &trace_call.ranges {
            let normalized_path = normalize_path(&range.path)?;
            let later_ranges = retrieval_ranges_by_path
                .get(&(range.repository_generation, normalized_path.clone()))
                .into_iter()
                .flatten()
                .filter(|(sequence, _, _)| *sequence > trace_call.sequence)
                .collect::<Vec<_>>();
            let explicit_hash_input_later = hash_input_sequences
                .get(range.content_hash.as_str())
                .is_some_and(|sequences| {
                    sequences
                        .iter()
                        .any(|sequence| *sequence > trace_call.sequence)
                });
            let exact_reread_later = later_ranges.iter().any(|(_, start_line, end_line)| {
                *start_line == range.start_line && *end_line == range.end_line
            });
            let overlap_reread_later = later_ranges.iter().any(|(_, start_line, end_line)| {
                *start_line <= range.end_line
                    && range.start_line <= *end_line
                    && (*start_line != range.start_line || *end_line != range.end_line)
            });
            let first_follow_up_sequence = later_ranges
                .iter()
                .filter(|(_, start_line, end_line)| {
                    *start_line <= range.end_line && range.start_line <= *end_line
                })
                .map(|(sequence, _, _)| *sequence)
                .min();
            let relevant_path_proxy = relevant_paths.contains(&normalized_path);
            let no_observed_downstream_signal = !relevant_path_proxy
                && !explicit_hash_input_later
                && !exact_reread_later
                && !overlap_reread_later;
            ranges.push(RangeUtilization {
                origin_sequence: trace_call.sequence,
                repository_generation: range.repository_generation,
                path: normalized_path,
                start_line: range.start_line,
                end_line: range.end_line,
                content_hash: range.content_hash.clone(),
                source_tokens: range.source_tokens,
                relevant_path_proxy,
                explicit_hash_input_later,
                exact_reread_later,
                overlap_reread_later,
                no_observed_downstream_signal,
                first_follow_up_sequence,
            });
        }
    }
    ranges.sort_by(|left, right| {
        (
            left.origin_sequence,
            &left.path,
            left.start_line,
            left.end_line,
            &left.content_hash,
        )
            .cmp(&(
                right.origin_sequence,
                &right.path,
                right.start_line,
                right.end_line,
                &right.content_hash,
            ))
    });
    let summary = UtilizationSummary {
        context_calls: context_calls.len(),
        successful_context_calls: context_calls
            .iter()
            .filter(|(_, call)| call.outcome == ToolOutcome::Success)
            .count(),
        failed_context_calls: context_calls
            .iter()
            .filter(|(_, call)| call.outcome != ToolOutcome::Success)
            .count(),
        context_ranges: signal_count(&ranges, |_| true)?,
        relevant_path_proxy: signal_count(&ranges, |range| range.relevant_path_proxy)?,
        explicit_hash_input_later: signal_count(&ranges, |range| range.explicit_hash_input_later)?,
        exact_reread_later: signal_count(&ranges, |range| range.exact_reread_later)?,
        overlap_reread_later: signal_count(&ranges, |range| range.overlap_reread_later)?,
        no_observed_downstream_signal: signal_count(&ranges, |range| {
            range.no_observed_downstream_signal
        })?,
        receipt_follow_up_calls: receipt_follow_up_ids.len(),
        follow_up_retrieval_calls: follow_up_retrieval_ids.len(),
    };
    Ok((summary, ranges))
}

fn trajectory_calls<'a>(
    trajectory: &'a Trajectory,
    trace: &ToolTrace,
) -> Result<BTreeMap<String, TrajectoryCall<'a>>, DynError> {
    let sequences = trace
        .calls
        .iter()
        .map(|call| (call.call_id.as_str(), call.sequence))
        .collect::<BTreeMap<_, _>>();
    let mut calls = BTreeMap::new();
    for event in &trajectory.events {
        if event["type"].as_str() != Some("item.completed")
            || event.pointer("/item/type").and_then(Value::as_str) != Some("mcp_tool_call")
        {
            continue;
        }
        let item = &event["item"];
        let id = item["id"]
            .as_str()
            .ok_or("completed MCP trajectory item has no ID")?;
        let trace_id = resolve_trace_id(&sequences, id)
            .ok_or("completed MCP trajectory item is missing from tool trace")?;
        let sequence = sequences[trace_id];
        let tool = item["tool"]
            .as_str()
            .ok_or("completed MCP trajectory item has no tool")?;
        let structured_result = item
            .pointer("/result/structured_content")
            .or_else(|| item.pointer("/result/structuredContent"));
        if calls
            .insert(
                trace_id.to_owned(),
                TrajectoryCall {
                    sequence,
                    tool,
                    arguments: &item["arguments"],
                    structured_result,
                },
            )
            .is_some()
        {
            return Err("trajectory contains a duplicate MCP call ID".into());
        }
    }
    Ok(calls)
}

fn resolve_trace_id<'a>(
    sequences: &BTreeMap<&'a str, usize>,
    trajectory_id: &str,
) -> Option<&'a str> {
    if let Some((trace_id, _)) = sequences.get_key_value(trajectory_id) {
        return Some(*trace_id);
    }
    if let Some(id) = trajectory_id.strip_prefix("prewalk:")
        && let Some((trace_id, _)) = sequences.get_key_value(id)
    {
        return Some(*trace_id);
    }
    let prefixed = format!("prewalk:{trajectory_id}");
    sequences
        .get_key_value(prefixed.as_str())
        .map(|(trace_id, _)| *trace_id)
}

fn trajectory_call_for_trace_id<'a>(
    calls: &'a BTreeMap<String, TrajectoryCall<'a>>,
    trace_id: &str,
) -> Option<&'a TrajectoryCall<'a>> {
    calls.get(trace_id).or_else(|| {
        trace_id
            .strip_prefix("prewalk:")
            .and_then(|id| calls.get(id))
    })
}

fn argument_hashes(arguments: &Value) -> BTreeSet<&str> {
    let mut hashes = arguments["known_hashes"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect::<BTreeSet<_>>();
    if let Some(hash) = arguments["expected_hash"].as_str() {
        hashes.insert(hash);
    }
    hashes
}

fn is_retrieval_tool(tool: &str) -> bool {
    matches!(
        tool,
        "leantoken_context"
            | "leantoken_search"
            | "leantoken_outline"
            | "leantoken_read"
            | "leantoken_history"
            | "leantoken_json"
            | "leantoken_files"
    )
}

fn signal_count(
    ranges: &[RangeUtilization],
    include: impl Fn(&RangeUtilization) -> bool,
) -> Result<SignalCount, DynError> {
    let selected = ranges
        .iter()
        .filter(|range| include(range))
        .collect::<Vec<_>>();
    let source_tokens = selected
        .iter()
        .filter_map(|range| range.source_tokens)
        .try_fold(0u64, |total, tokens| -> Result<u64, DynError> {
            let tokens = u64::try_from(tokens)
                .map_err(|_| -> DynError { "range source tokens exceed u64".into() })?;
            total
                .checked_add(tokens)
                .ok_or_else(|| -> DynError { "context-utilization source-token overflow".into() })
        })?;
    Ok(SignalCount {
        ranges: selected.len(),
        source_tokens,
        source_tokens_complete: selected.iter().all(|range| range.source_tokens.is_some()),
    })
}

fn normalize_path(path: &str) -> Result<String, DynError> {
    if path.len() > MAX_PATH_BYTES {
        return Err("path exceeds byte bound".into());
    }
    let path = Path::new(path);
    if path.as_os_str().is_empty() || path.is_absolute() {
        return Err("path must be non-empty and repository-relative".into());
    }
    let mut parts = Vec::new();
    for component in path.components() {
        match component {
            Component::Normal(value) => parts.push(value.to_string_lossy().into_owned()),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err("path escapes the repository".into());
            }
        }
    }
    if parts.is_empty() {
        return Err("path must contain a repository-relative component".into());
    }
    Ok(parts.join("/"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use model_ab_artifacts::{RunBinding, ToolCall};
    use serde_json::json;

    fn binding() -> RunBinding {
        RunBinding {
            experiment_id: "experiment".into(),
            manifest_blake3: "a".repeat(64),
            task_id: "task".into(),
            repetition: 1,
            arm: "lean_token_progressive".into(),
        }
    }

    fn range(path: &str, start_line: usize, end_line: usize, hash: char) -> RangeIdentity {
        RangeIdentity {
            repository_generation: 7,
            path: path.into(),
            start_line,
            end_line,
            content_hash: hash.to_string().repeat(32),
            source_tokens: Some(20),
        }
    }

    fn event(id: &str, tool: &str, arguments: Value, structured_result: Value) -> Value {
        json!({
            "type": "item.completed",
            "item": {
                "type": "mcp_tool_call",
                "id": id,
                "tool": tool,
                "arguments": arguments,
                "result": {"structured_content": structured_result}
            }
        })
    }

    #[test]
    fn classifies_context_signals_without_collapsing_them_into_one_score() {
        let trace = ToolTrace {
            schema_version: ARTIFACT_SCHEMA_V1,
            binding: binding(),
            calls: vec![
                ToolCall {
                    sequence: 1,
                    tool_name: "leantoken".into(),
                    call_id: "prewalk:context-1".into(),
                    result_id: "result-1".into(),
                    outcome: ToolOutcome::Success,
                    result_source_tokens: 60,
                    reread: false,
                    ranges: vec![
                        range("src/lib.rs", 10, 20, 'a'),
                        range("src/dead.rs", 1, 5, 'b'),
                        range("src/held.rs", 2, 8, 'c'),
                    ],
                },
                ToolCall {
                    sequence: 2,
                    tool_name: "leantoken".into(),
                    call_id: "read-1".into(),
                    result_id: "result-2".into(),
                    outcome: ToolOutcome::Success,
                    result_source_tokens: 20,
                    reread: true,
                    ranges: vec![range("src/lib.rs", 8, 24, 'd')],
                },
            ],
        };
        let trajectory = Trajectory {
            schema_version: ARTIFACT_SCHEMA_V1,
            binding: binding(),
            events: vec![
                event(
                    "context-1",
                    "leantoken_context",
                    json!({}),
                    json!({"meta": {"receipt_id": "r1"}}),
                ),
                event(
                    "read-1",
                    "leantoken_read",
                    json!({"receipt_id": "r1", "known_hashes": ["cccccccccccccccccccccccccccccccc"]}),
                    json!({}),
                ),
            ],
        };
        validate_artifacts(&trace, &trajectory).expect("valid fixtures");
        let (summary, ranges) =
            classify(&trace, &trajectory, &BTreeSet::from(["src/lib.rs".into()]))
                .expect("classification");

        assert_eq!(summary.context_ranges.ranges, 3);
        assert_eq!(summary.relevant_path_proxy.ranges, 1);
        assert_eq!(summary.explicit_hash_input_later.ranges, 1);
        assert_eq!(summary.overlap_reread_later.ranges, 1);
        assert_eq!(summary.exact_reread_later.ranges, 0);
        assert_eq!(summary.no_observed_downstream_signal.ranges, 1);
        assert_eq!(summary.receipt_follow_up_calls, 1);
        assert_eq!(summary.follow_up_retrieval_calls, 1);
        let by_path = ranges
            .iter()
            .map(|range| (range.path.as_str(), range))
            .collect::<BTreeMap<_, _>>();
        assert!(by_path["src/lib.rs"].relevant_path_proxy);
        assert!(by_path["src/lib.rs"].overlap_reread_later);
        assert!(by_path["src/dead.rs"].no_observed_downstream_signal);
        assert!(by_path["src/held.rs"].explicit_hash_input_later);
    }

    #[test]
    fn rejects_mismatched_bindings_and_invalid_paths() {
        let trace = ToolTrace {
            schema_version: ARTIFACT_SCHEMA_V1,
            binding: binding(),
            calls: vec![],
        };
        let mut trajectory = Trajectory {
            schema_version: ARTIFACT_SCHEMA_V1,
            binding: binding(),
            events: vec![],
        };
        trajectory.binding.arm = "other".into();
        assert!(validate_artifacts(&trace, &trajectory).is_err());
        assert!(normalize_path("../outside.rs").is_err());
        assert!(normalize_path("/absolute.rs").is_err());
    }

    #[test]
    fn missing_source_tokens_remain_explicitly_incomplete() {
        let ranges = vec![RangeUtilization {
            origin_sequence: 1,
            repository_generation: 1,
            path: "src/lib.rs".into(),
            start_line: 1,
            end_line: 2,
            content_hash: "a".repeat(32),
            source_tokens: None,
            relevant_path_proxy: false,
            explicit_hash_input_later: false,
            exact_reread_later: false,
            overlap_reread_later: false,
            no_observed_downstream_signal: true,
            first_follow_up_sequence: None,
        }];

        let count = signal_count(&ranges, |_| true).expect("bounded count");
        assert_eq!(count.source_tokens, 0);
        assert!(!count.source_tokens_complete);
    }
}
