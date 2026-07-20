//! Build a redacted usage receipt for a Codex root thread and its subagents.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::error::Error;
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};

use clap::Parser;
use serde::{Deserialize, Serialize};
use serde_json::Value;

const RECEIPT_SCHEMA_V1: u32 = 1;
const MAX_ROLLOUT_BYTES: u64 = 64 * 1024 * 1024;
const MAX_SESSION_RECORD_BYTES: u64 = 4 * 1024 * 1024;
const MAX_DISCOVERED_ROLLOUTS: usize = 100_000;

type DynError = Box<dyn Error>;

#[derive(Debug, Parser)]
#[command(about = "Create a redacted Codex multi-agent usage receipt")]
struct Args {
    /// Private root Codex rollout JSONL. Its content is never copied to the receipt.
    #[arg(long)]
    root_rollout: PathBuf,
    /// Directory containing the root and child Codex rollout JSONL files.
    #[arg(long)]
    sessions_root: PathBuf,
    /// Stable, non-sensitive experiment identifier.
    #[arg(long)]
    experiment_id: String,
    /// Stable experiment-arm name such as thin_native.
    #[arg(long)]
    arm: String,
    /// Require exactly this many discovered child threads.
    #[arg(long)]
    expected_children: Option<usize>,
    /// Optional public gold manifest used to validate the root's final JSON answer.
    #[arg(long)]
    gold_manifest: Option<PathBuf>,
    /// Publishable receipt path.
    #[arg(long)]
    output: PathBuf,
    /// Optional publishable SVG token chart.
    #[arg(long)]
    svg: Option<PathBuf>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
struct UsageReceipt {
    total_input_tokens: u64,
    uncached_input_tokens: u64,
    cache_read_input_tokens: u64,
    output_tokens: u64,
    reasoning_output_tokens: u64,
    non_reasoning_output_tokens: u64,
}

impl UsageReceipt {
    fn parse(value: &Value) -> Result<Self, DynError> {
        let input = required_u64(value, "/input_tokens", "input tokens")?;
        let cached = required_u64(value, "/cached_input_tokens", "cached input tokens")?;
        let output = required_u64(value, "/output_tokens", "output tokens")?;
        let reasoning = required_u64(value, "/reasoning_output_tokens", "reasoning output tokens")?;
        let total = required_u64(value, "/total_tokens", "total tokens")?;
        if cached > input || reasoning > output || input.checked_add(output) != Some(total) {
            return Err("Codex provider usage fields are internally inconsistent".into());
        }
        Ok(Self {
            total_input_tokens: input,
            uncached_input_tokens: input - cached,
            cache_read_input_tokens: cached,
            output_tokens: output,
            reasoning_output_tokens: reasoning,
            non_reasoning_output_tokens: output - reasoning,
        })
    }

    fn checked_add(self, other: Self) -> Result<Self, DynError> {
        Ok(Self {
            total_input_tokens: checked_add(self.total_input_tokens, other.total_input_tokens)?,
            uncached_input_tokens: checked_add(
                self.uncached_input_tokens,
                other.uncached_input_tokens,
            )?,
            cache_read_input_tokens: checked_add(
                self.cache_read_input_tokens,
                other.cache_read_input_tokens,
            )?,
            output_tokens: checked_add(self.output_tokens, other.output_tokens)?,
            reasoning_output_tokens: checked_add(
                self.reasoning_output_tokens,
                other.reasoning_output_tokens,
            )?,
            non_reasoning_output_tokens: checked_add(
                self.non_reasoning_output_tokens,
                other.non_reasoning_output_tokens,
            )?,
        })
    }

    fn is_monotonic_after(self, previous: Self) -> bool {
        self.total_input_tokens >= previous.total_input_tokens
            && self.uncached_input_tokens >= previous.uncached_input_tokens
            && self.cache_read_input_tokens >= previous.cache_read_input_tokens
            && self.output_tokens >= previous.output_tokens
            && self.reasoning_output_tokens >= previous.reasoning_output_tokens
            && self.non_reasoning_output_tokens >= previous.non_reasoning_output_tokens
    }
}

#[derive(Debug, Serialize)]
struct FamilyReceipt {
    schema_version: u32,
    experiment_id: String,
    arm: String,
    source_root_rollout_blake3: String,
    source_family_blake3: String,
    receipt_binary_blake3: String,
    host: &'static str,
    topology: TopologyReceipt,
    provider_usage: UsageReceipt,
    first_request_usage: UsageReceipt,
    #[serde(skip_serializing_if = "Option::is_none")]
    task_evaluation: Option<TaskEvaluation>,
    thread_receipts: Vec<ThreadReceipt>,
    privacy: PrivacyBoundary,
    limitations: Vec<&'static str>,
}

#[derive(Debug, Serialize)]
struct TopologyReceipt {
    thread_count: usize,
    child_thread_count: usize,
    maximum_depth: usize,
    spawn_calls: usize,
    matched_spawn_calls: usize,
    collaboration_calls: BTreeMap<String, usize>,
    fork_turns: BTreeMap<String, usize>,
}

#[derive(Debug, Serialize)]
struct ThreadReceipt {
    thread_index: usize,
    parent_thread_index: Option<usize>,
    depth: usize,
    role: &'static str,
    source_rollout_blake3: String,
    host_version: String,
    model: String,
    provider: String,
    reasoning_effort: Option<String>,
    multi_agent_version: Option<String>,
    rollout_record_count: usize,
    inherited_history_record_count: usize,
    inherited_history_bytes: u64,
    turn_count: usize,
    completed_turns: usize,
    aborted_turns: usize,
    provider_request_count: usize,
    compaction_count: usize,
    task_duration_ms: u64,
    first_request_usage: UsageReceipt,
    provider_usage: UsageReceipt,
    tool_calls: ToolCallCounts,
}

#[derive(Debug, Default, Serialize)]
struct ToolCallCounts {
    collaboration: usize,
    mcp: usize,
    failed_mcp: usize,
    mcp_result_json_bytes: u64,
    mcp_text_content_bytes: u64,
    mcp_structured_content_bytes: u64,
    mcp_emitted_source_tokens: u64,
    shell: usize,
    other: usize,
}

#[derive(Debug, Serialize)]
struct PrivacyBoundary {
    raw_rollouts_retained: bool,
    prompts_retained: bool,
    tool_arguments_retained: bool,
    tool_outputs_retained: bool,
    credentials_retained: bool,
    absolute_paths_retained: bool,
    session_call_and_agent_ids_hashed_or_omitted: bool,
}

#[derive(Debug, Deserialize)]
struct GoldManifest {
    schema_version: u32,
    experiment_id: String,
    task_id: String,
    repository_revision: String,
    #[serde(default)]
    evaluation_mode: EvaluationMode,
    expected_evidence: Vec<GoldEvidence>,
}

#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum EvaluationMode {
    #[default]
    Exact,
    Path,
}

#[derive(Debug, Deserialize)]
struct GoldEvidence {
    path: String,
    #[serde(default)]
    symbol: Option<String>,
}

#[derive(Debug, Deserialize)]
struct AgentAnswer {
    evidence: Vec<GoldEvidence>,
}

#[derive(Debug, Serialize)]
struct TaskEvaluation {
    task_id: String,
    repository_revision: String,
    gold_manifest_blake3: String,
    evaluation_mode: EvaluationMode,
    answer_json_valid: bool,
    expected_evidence_count: usize,
    reported_evidence_count: usize,
    reported_path_count: usize,
    matched_path_count: usize,
    matched_exact_evidence_count: usize,
    unexpected_evidence_count: usize,
    task_success: bool,
}

#[derive(Debug)]
struct SessionLocator {
    id: String,
    parent_id: Option<String>,
    is_subagent: bool,
    path: PathBuf,
}

#[derive(Debug)]
struct ParsedRollout {
    id: String,
    parent_id: Option<String>,
    host_version: String,
    provider: String,
    model: String,
    reasoning_effort: Option<String>,
    multi_agent_version: Option<String>,
    record_count: usize,
    inherited_history_record_count: usize,
    inherited_history_bytes: u64,
    turn_count: usize,
    completed_turns: usize,
    aborted_turns: usize,
    provider_request_count: usize,
    compaction_count: usize,
    task_duration_ms: u64,
    first_request_usage: UsageReceipt,
    provider_usage: UsageReceipt,
    source_hash: String,
    final_answer: Option<String>,
    tool_calls: ToolCallCounts,
    collaboration_calls: Vec<CollaborationCall>,
    started_children: HashMap<String, String>,
}

#[derive(Debug)]
struct CollaborationCall {
    call_id: String,
    tool: String,
    fork_turns: Option<String>,
}

fn main() -> Result<(), DynError> {
    let args = Args::parse();
    validate_label(&args.experiment_id, "experiment ID")?;
    validate_label(&args.arm, "experiment arm")?;
    let receipt = build_receipt(
        &args.root_rollout,
        &args.sessions_root,
        args.experiment_id,
        args.arm,
        args.expected_children,
        args.gold_manifest.as_deref(),
    )?;
    fs::write(&args.output, serde_json::to_vec_pretty(&receipt)?)?;
    if let Some(path) = args.svg {
        fs::write(path, render_svg(&receipt))?;
    }
    Ok(())
}

fn build_receipt(
    root_rollout: &Path,
    sessions_root: &Path,
    experiment_id: String,
    arm: String,
    expected_children: Option<usize>,
    gold_manifest: Option<&Path>,
) -> Result<FamilyReceipt, DynError> {
    let root_meta = read_session_locator(root_rollout)?
        .ok_or("root rollout does not start with Codex session_meta")?;
    if root_meta.parent_id.is_some() || root_meta.is_subagent {
        return Err("root rollout identifies itself as a subagent".into());
    }
    let locators = discover_session_locators(sessions_root)?;
    let family = resolve_family(&root_meta, locators)?;
    if let Some(expected) = expected_children
        && family.len().saturating_sub(1) != expected
    {
        return Err(format!(
            "expected {expected} child threads, discovered {}",
            family.len().saturating_sub(1)
        )
        .into());
    }

    let mut parsed = Vec::with_capacity(family.len());
    for locator in &family {
        parsed.push(parse_rollout(&locator.path)?);
    }
    let index_by_id: HashMap<&str, usize> = parsed
        .iter()
        .enumerate()
        .map(|(index, rollout)| (rollout.id.as_str(), index))
        .collect();
    let depths = compute_depths(&parsed, &index_by_id)?;

    let mut family_usage = UsageReceipt::default();
    let mut first_request_usage = UsageReceipt::default();
    let mut collaboration_calls = BTreeMap::<String, usize>::new();
    let mut fork_turns = BTreeMap::<String, usize>::new();
    let mut spawn_calls = 0usize;
    let mut matched_spawn_calls = 0usize;
    let mut thread_receipts = Vec::with_capacity(parsed.len());
    let mut family_hash_entries = Vec::with_capacity(parsed.len());

    for (index, rollout) in parsed.iter().enumerate() {
        family_usage = family_usage.checked_add(rollout.provider_usage)?;
        first_request_usage = first_request_usage.checked_add(rollout.first_request_usage)?;
        family_hash_entries.push(format!("{index}:{}", rollout.source_hash));
        for call in &rollout.collaboration_calls {
            *collaboration_calls.entry(call.tool.clone()).or_default() += 1;
            if call.tool == "spawn_agent" {
                spawn_calls += 1;
                let mode = call.fork_turns.as_deref().unwrap_or("unknown");
                *fork_turns.entry(mode.to_owned()).or_default() += 1;
                if rollout
                    .started_children
                    .get(&call.call_id)
                    .is_some_and(|child| index_by_id.contains_key(child.as_str()))
                {
                    matched_spawn_calls += 1;
                }
            }
        }
        let parent_thread_index = rollout
            .parent_id
            .as_deref()
            .map(|parent| {
                index_by_id
                    .get(parent)
                    .copied()
                    .ok_or("family thread references an undiscovered parent")
            })
            .transpose()?;
        thread_receipts.push(ThreadReceipt {
            thread_index: index,
            parent_thread_index,
            depth: depths[index],
            role: if index == 0 { "root" } else { "subagent" },
            source_rollout_blake3: rollout.source_hash.clone(),
            host_version: rollout.host_version.clone(),
            model: rollout.model.clone(),
            provider: rollout.provider.clone(),
            reasoning_effort: rollout.reasoning_effort.clone(),
            multi_agent_version: rollout.multi_agent_version.clone(),
            rollout_record_count: rollout.record_count,
            inherited_history_record_count: rollout.inherited_history_record_count,
            inherited_history_bytes: rollout.inherited_history_bytes,
            turn_count: rollout.turn_count,
            completed_turns: rollout.completed_turns,
            aborted_turns: rollout.aborted_turns,
            provider_request_count: rollout.provider_request_count,
            compaction_count: rollout.compaction_count,
            task_duration_ms: rollout.task_duration_ms,
            first_request_usage: rollout.first_request_usage,
            provider_usage: rollout.provider_usage,
            tool_calls: ToolCallCounts {
                collaboration: rollout.tool_calls.collaboration,
                mcp: rollout.tool_calls.mcp,
                failed_mcp: rollout.tool_calls.failed_mcp,
                mcp_result_json_bytes: rollout.tool_calls.mcp_result_json_bytes,
                mcp_text_content_bytes: rollout.tool_calls.mcp_text_content_bytes,
                mcp_structured_content_bytes: rollout.tool_calls.mcp_structured_content_bytes,
                mcp_emitted_source_tokens: rollout.tool_calls.mcp_emitted_source_tokens,
                shell: rollout.tool_calls.shell,
                other: rollout.tool_calls.other,
            },
        });
    }
    let source_family_blake3 = blake3::hash(family_hash_entries.join("\n").as_bytes())
        .to_hex()
        .to_string();
    let task_evaluation = gold_manifest
        .map(|path| evaluate_task(path, &experiment_id, parsed[0].final_answer.as_deref()))
        .transpose()?;
    let mut limitations = vec![
        "Provider usage is session-native accounting; provider request framing is unavailable.",
        "Cached input is reported, but cache-creation input is unavailable.",
        "Tool-call counts identify categories but do not retain arguments, outputs, or source evidence.",
    ];
    if task_evaluation.is_none() {
        limitations.push(
            "No gold manifest was supplied, so the receipt does not establish task correctness.",
        );
    }

    Ok(FamilyReceipt {
        schema_version: RECEIPT_SCHEMA_V1,
        experiment_id,
        arm,
        source_root_rollout_blake3: parsed[0].source_hash.clone(),
        source_family_blake3,
        receipt_binary_blake3: hash_file(&std::env::current_exe()?)?,
        host: "codex-cli",
        topology: TopologyReceipt {
            thread_count: parsed.len(),
            child_thread_count: parsed.len().saturating_sub(1),
            maximum_depth: depths.iter().copied().max().unwrap_or(0),
            spawn_calls,
            matched_spawn_calls,
            collaboration_calls,
            fork_turns,
        },
        provider_usage: family_usage,
        first_request_usage,
        task_evaluation,
        thread_receipts,
        privacy: PrivacyBoundary {
            raw_rollouts_retained: false,
            prompts_retained: false,
            tool_arguments_retained: false,
            tool_outputs_retained: false,
            credentials_retained: false,
            absolute_paths_retained: false,
            session_call_and_agent_ids_hashed_or_omitted: true,
        },
        limitations,
    })
}

fn evaluate_task(
    manifest_path: &Path,
    expected_experiment_id: &str,
    final_answer: Option<&str>,
) -> Result<TaskEvaluation, DynError> {
    let bytes = read_bounded(manifest_path)?;
    let manifest: GoldManifest = serde_json::from_slice(&bytes)?;
    if manifest.schema_version != 1 {
        return Err("unsupported multi-agent gold manifest schema".into());
    }
    if manifest.experiment_id != expected_experiment_id {
        return Err("gold manifest experiment ID does not match the receipt".into());
    }
    validate_label(&manifest.task_id, "task ID")?;
    validate_lower_hex(&manifest.repository_revision, 40, "repository revision")?;
    if manifest.expected_evidence.is_empty() || manifest.expected_evidence.len() > 256 {
        return Err("gold manifest must contain 1-256 evidence entries".into());
    }
    let expected_paths = manifest
        .expected_evidence
        .iter()
        .map(|evidence| {
            if evidence.path.trim().is_empty() {
                return Err("gold evidence path must be non-empty".into());
            }
            Ok(evidence.path.clone())
        })
        .collect::<Result<HashSet<_>, DynError>>()?;
    if matches!(manifest.evaluation_mode, EvaluationMode::Path)
        && expected_paths.len() != manifest.expected_evidence.len()
    {
        return Err("gold manifest contains duplicate evidence paths".into());
    }
    let expected_exact = manifest
        .expected_evidence
        .iter()
        .map(|evidence| {
            let symbol = evidence.symbol.as_deref().unwrap_or_default();
            if matches!(manifest.evaluation_mode, EvaluationMode::Exact) && symbol.trim().is_empty()
            {
                return Err("exact gold evidence symbols must be non-empty".into());
            }
            Ok((evidence.path.clone(), symbol.to_owned()))
        })
        .collect::<Result<HashSet<_>, DynError>>()?;
    if expected_exact.len() != manifest.expected_evidence.len() {
        return Err("gold manifest contains duplicate evidence".into());
    }
    let answer = final_answer.and_then(|answer| serde_json::from_str::<AgentAnswer>(answer).ok());
    let answer_json_valid = answer.is_some();
    let reported_evidence_count = answer.as_ref().map_or(0, |answer| answer.evidence.len());
    let reported = answer
        .as_ref()
        .map(|answer| {
            answer
                .evidence
                .iter()
                .map(|evidence| {
                    (
                        evidence.path.clone(),
                        evidence.symbol.clone().unwrap_or_default(),
                    )
                })
                .collect::<HashSet<_>>()
        })
        .unwrap_or_default();
    let reported_paths = reported
        .iter()
        .map(|(path, _)| path.clone())
        .collect::<HashSet<_>>();
    let matched_path_count = expected_paths.intersection(&reported_paths).count();
    let matched_exact_evidence_count = expected_exact.intersection(&reported).count();
    let unexpected_evidence_count = match manifest.evaluation_mode {
        EvaluationMode::Exact => {
            reported_evidence_count.saturating_sub(matched_exact_evidence_count)
        }
        EvaluationMode::Path => reported_paths.difference(&expected_paths).count(),
    };
    let task_success = answer_json_valid
        && match manifest.evaluation_mode {
            EvaluationMode::Exact => {
                matched_exact_evidence_count == manifest.expected_evidence.len()
                    && reported_evidence_count == manifest.expected_evidence.len()
            }
            EvaluationMode::Path => reported_paths == expected_paths,
        };
    Ok(TaskEvaluation {
        task_id: manifest.task_id,
        repository_revision: manifest.repository_revision,
        gold_manifest_blake3: blake3::hash(&bytes).to_hex().to_string(),
        evaluation_mode: manifest.evaluation_mode,
        answer_json_valid,
        expected_evidence_count: manifest.expected_evidence.len(),
        reported_evidence_count,
        reported_path_count: reported_paths.len(),
        matched_path_count,
        matched_exact_evidence_count,
        unexpected_evidence_count,
        task_success,
    })
}

fn discover_session_locators(root: &Path) -> Result<Vec<SessionLocator>, DynError> {
    let mut paths = Vec::new();
    collect_jsonl_paths(root, &mut paths)?;
    if paths.len() > MAX_DISCOVERED_ROLLOUTS {
        return Err("sessions directory contains too many rollout files".into());
    }
    paths.sort();
    let mut locators = Vec::new();
    let mut ids = HashSet::new();
    for path in paths {
        let Some(locator) = read_session_locator(&path)? else {
            continue;
        };
        if !ids.insert(locator.id.clone()) {
            return Err("sessions directory contains a duplicate Codex session ID".into());
        }
        locators.push(locator);
    }
    Ok(locators)
}

fn collect_jsonl_paths(root: &Path, output: &mut Vec<PathBuf>) -> Result<(), DynError> {
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        let path = entry.path();
        if file_type.is_dir() {
            collect_jsonl_paths(&path, output)?;
        } else if file_type.is_file() && path.extension().is_some_and(|value| value == "jsonl") {
            output.push(path);
        }
    }
    Ok(())
}

fn read_session_locator(path: &Path) -> Result<Option<SessionLocator>, DynError> {
    let file = File::open(path)?;
    let mut line = String::new();
    BufReader::new(file)
        .take(MAX_SESSION_RECORD_BYTES)
        .read_line(&mut line)?;
    if line.trim().is_empty() {
        return Ok(None);
    }
    let record: Value = match serde_json::from_str(&line) {
        Ok(record) => record,
        Err(_) => return Ok(None),
    };
    if record.pointer("/type").and_then(Value::as_str) != Some("session_meta") {
        return Ok(None);
    }
    let payload = record
        .pointer("/payload")
        .ok_or("session_meta has no payload")?;
    let id = required_str(payload, "/id", "session ID")?.to_owned();
    let parent_id = payload
        .pointer("/parent_thread_id")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let is_subagent = payload.pointer("/thread_source").and_then(Value::as_str) == Some("subagent")
        || payload.pointer("/source/subagent").is_some();
    Ok(Some(SessionLocator {
        id,
        parent_id,
        is_subagent,
        path: path.to_owned(),
    }))
}

fn resolve_family(
    root: &SessionLocator,
    locators: Vec<SessionLocator>,
) -> Result<Vec<SessionLocator>, DynError> {
    let mut by_parent = HashMap::<String, Vec<SessionLocator>>::new();
    for locator in locators {
        if locator.id == root.id {
            continue;
        }
        if locator.is_subagent
            && let Some(parent) = locator.parent_id.clone()
        {
            by_parent.entry(parent).or_default().push(locator);
        }
    }
    for children in by_parent.values_mut() {
        children.sort_by(|left, right| left.path.cmp(&right.path));
    }
    let mut family = vec![SessionLocator {
        id: root.id.clone(),
        parent_id: None,
        is_subagent: false,
        path: root.path.clone(),
    }];
    let mut queue = VecDeque::from([root.id.clone()]);
    let mut seen = HashSet::from([root.id.clone()]);
    while let Some(parent) = queue.pop_front() {
        for child in by_parent.remove(&parent).unwrap_or_default() {
            if !seen.insert(child.id.clone()) {
                return Err("Codex session family contains a cycle or duplicate".into());
            }
            queue.push_back(child.id.clone());
            family.push(child);
        }
    }
    Ok(family)
}

fn parse_rollout(path: &Path) -> Result<ParsedRollout, DynError> {
    let bytes = read_bounded(path)?;
    let text = std::str::from_utf8(&bytes)?;
    let records = text
        .lines()
        .enumerate()
        .filter(|(_, line)| !line.trim().is_empty())
        .map(|(record_index, line)| {
            serde_json::from_str::<Value>(line)
                .map(|record| (record_index, line.len() + 1, record))
                .map_err(|error| {
                    format!("invalid Codex rollout JSONL record {record_index}: {error}")
                })
        })
        .collect::<Result<Vec<_>, _>>()?;
    let (_, _, first) = records.first().ok_or("Codex rollout is empty")?;
    if first.pointer("/type").and_then(Value::as_str) != Some("session_meta") {
        return Err("Codex rollout does not start with session_meta".into());
    }
    let session = first
        .pointer("/payload")
        .ok_or("session_meta has no payload")?;
    let id = required_str(session, "/id", "session ID")?.to_owned();
    let parent_id = session
        .pointer("/parent_thread_id")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let host_version = required_str(session, "/cli_version", "Codex CLI version")?.to_owned();
    let provider = required_str(session, "/model_provider", "Codex model provider")?.to_owned();
    let multi_agent_version = session
        .pointer("/multi_agent_version")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let live_start = live_record_start(&records, parent_id.is_some())?;
    let inherited_history_record_count = live_start.saturating_sub(1);
    let inherited_history_bytes = records[1..live_start]
        .iter()
        .try_fold(0u64, |total, (_, bytes, _)| {
            checked_add(total, u64::try_from(*bytes)?)
        })?;

    let mut model = None;
    let mut reasoning_effort = None;
    let mut active_turn = false;
    let mut turn_count = 0usize;
    let mut completed_turns = 0usize;
    let mut aborted_turns = 0usize;
    let mut provider_request_count = 0usize;
    let mut compaction_count = 0usize;
    let mut task_duration_ms = 0u64;
    let mut first_request_usage = None;
    let mut provider_usage = None;
    let mut final_answer = None;
    let mut tool_calls = ToolCallCounts::default();
    let mut collaboration_calls = Vec::new();
    let mut started_children = HashMap::new();
    for (_, _, record) in records.iter().skip(live_start) {
        let record_type = required_str(record, "/type", "rollout record type")?;
        let payload = record.pointer("/payload").unwrap_or(&Value::Null);
        match record_type {
            "session_meta" => {
                return Err("Codex live rollout contains an unexpected session_meta".into());
            }
            "turn_context" => {
                let current_model = required_str(payload, "/model", "Codex model")?;
                if model.as_deref().is_some_and(|value| value != current_model) {
                    return Err("Codex rollout changes model between turns".into());
                }
                model = Some(current_model.to_owned());
                let current_effort = payload
                    .pointer("/effort")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                if reasoning_effort.is_some() && reasoning_effort != current_effort {
                    return Err("Codex rollout changes reasoning effort between turns".into());
                }
                reasoning_effort = current_effort;
            }
            "event_msg" => ingest_event(
                payload,
                &mut active_turn,
                &mut turn_count,
                &mut completed_turns,
                &mut aborted_turns,
                &mut provider_request_count,
                &mut task_duration_ms,
                &mut first_request_usage,
                &mut provider_usage,
                &mut final_answer,
                &mut tool_calls,
                &mut started_children,
            )?,
            "response_item" => {
                ingest_response_item(payload, &mut tool_calls, &mut collaboration_calls)?
            }
            "compacted" => compaction_count += 1,
            _ => {}
        }
    }
    if active_turn || turn_count == 0 || completed_turns + aborted_turns != turn_count {
        return Err("Codex rollout turn lifecycle is incomplete".into());
    }
    let provider_usage = provider_usage.ok_or("Codex rollout has no provider usage snapshot")?;
    Ok(ParsedRollout {
        id,
        parent_id,
        host_version,
        provider,
        model: model.ok_or("Codex rollout has no turn context")?,
        reasoning_effort,
        multi_agent_version,
        record_count: records.len(),
        inherited_history_record_count,
        inherited_history_bytes,
        turn_count,
        completed_turns,
        aborted_turns,
        provider_request_count,
        compaction_count,
        task_duration_ms,
        first_request_usage: first_request_usage
            .ok_or("Codex rollout has no first-request usage")?,
        provider_usage,
        source_hash: blake3::hash(&bytes).to_hex().to_string(),
        final_answer,
        tool_calls,
        collaboration_calls,
        started_children,
    })
}

fn live_record_start(
    records: &[(usize, usize, Value)],
    is_subagent: bool,
) -> Result<usize, DynError> {
    if !is_subagent {
        return Ok(1);
    }
    let trigger_index = records
        .iter()
        .position(|(_, _, record)| {
            record.pointer("/type").and_then(Value::as_str)
                == Some("inter_agent_communication_metadata")
                && record
                    .pointer("/payload/trigger_turn")
                    .and_then(Value::as_bool)
                    == Some(true)
        })
        .ok_or("subagent rollout has no trigger-turn marker")?;
    (1..=trigger_index)
        .rev()
        .find(|index| {
            records[*index].2.pointer("/type").and_then(Value::as_str) == Some("event_msg")
                && records[*index]
                    .2
                    .pointer("/payload/type")
                    .and_then(Value::as_str)
                    == Some("task_started")
        })
        .ok_or_else(|| "subagent rollout has no live task start".into())
}

#[allow(clippy::too_many_arguments)]
fn ingest_event(
    payload: &Value,
    active_turn: &mut bool,
    turn_count: &mut usize,
    completed_turns: &mut usize,
    aborted_turns: &mut usize,
    provider_request_count: &mut usize,
    task_duration_ms: &mut u64,
    first_request_usage: &mut Option<UsageReceipt>,
    provider_usage: &mut Option<UsageReceipt>,
    final_answer: &mut Option<String>,
    tool_calls: &mut ToolCallCounts,
    started_children: &mut HashMap<String, String>,
) -> Result<(), DynError> {
    match payload.pointer("/type").and_then(Value::as_str) {
        Some("task_started") => {
            if *active_turn {
                return Err("Codex rollout starts a turn before closing the previous turn".into());
            }
            *active_turn = true;
            *turn_count += 1;
        }
        Some("task_complete") => {
            if !*active_turn {
                return Err("Codex rollout completes a turn that was not started".into());
            }
            *active_turn = false;
            *completed_turns += 1;
            if let Some(answer) = payload
                .pointer("/last_agent_message")
                .and_then(Value::as_str)
            {
                *final_answer = Some(answer.to_owned());
            }
            *task_duration_ms = checked_add(
                *task_duration_ms,
                payload
                    .pointer("/duration_ms")
                    .and_then(Value::as_u64)
                    .unwrap_or(0),
            )?;
        }
        Some("turn_aborted") => {
            if !*active_turn {
                return Err("Codex rollout aborts a turn that was not started".into());
            }
            *active_turn = false;
            *aborted_turns += 1;
        }
        Some("token_count") => {
            if !*active_turn {
                return Err("Codex provider usage occurs outside an active turn".into());
            }
            if let Some(total) = payload.pointer("/info/total_token_usage") {
                let current = UsageReceipt::parse(total)?;
                if provider_usage.is_some_and(|previous| !current.is_monotonic_after(previous)) {
                    return Err("Codex cumulative provider usage regressed".into());
                }
                let last = payload
                    .pointer("/info/last_token_usage")
                    .map(UsageReceipt::parse)
                    .transpose()?
                    .unwrap_or(current);
                if first_request_usage.is_none() {
                    *first_request_usage = Some(last);
                }
                *provider_usage = Some(current);
                *provider_request_count += 1;
            }
        }
        Some("sub_agent_activity")
            if payload.pointer("/kind").and_then(Value::as_str) == Some("started") =>
        {
            let call_id = required_str(payload, "/event_id", "subagent spawn event ID")?;
            let child = required_str(payload, "/agent_thread_id", "subagent thread ID")?;
            if started_children
                .insert(call_id.to_owned(), child.to_owned())
                .is_some()
            {
                return Err("Codex rollout repeats a subagent spawn event ID".into());
            }
        }
        Some("mcp_tool_call_end") => ingest_mcp_result(payload, tool_calls)?,
        _ => {}
    }
    Ok(())
}

fn ingest_mcp_result(payload: &Value, tool_calls: &mut ToolCallCounts) -> Result<(), DynError> {
    tool_calls.mcp += 1;
    let result = payload
        .pointer("/result")
        .ok_or("Codex MCP lifecycle event has no result")?;
    let Some(ok) = result.pointer("/Ok") else {
        tool_calls.failed_mcp += 1;
        return Ok(());
    };
    if ok.pointer("/isError").and_then(Value::as_bool) == Some(true) {
        tool_calls.failed_mcp += 1;
    }
    tool_calls.mcp_result_json_bytes = checked_add(
        tool_calls.mcp_result_json_bytes,
        u64::try_from(serde_json::to_vec(ok)?.len())?,
    )?;
    if let Some(content) = ok.pointer("/content").and_then(Value::as_array) {
        let bytes = content.iter().try_fold(0u64, |total, item| {
            let text_bytes = item
                .pointer("/text")
                .and_then(Value::as_str)
                .map_or(0, str::len);
            checked_add(total, u64::try_from(text_bytes)?)
        })?;
        tool_calls.mcp_text_content_bytes = checked_add(tool_calls.mcp_text_content_bytes, bytes)?;
    }
    if let Some(structured) = ok.pointer("/structuredContent") {
        tool_calls.mcp_structured_content_bytes = checked_add(
            tool_calls.mcp_structured_content_bytes,
            u64::try_from(serde_json::to_vec(structured)?.len())?,
        )?;
        let emitted = structured
            .pointer("/meta/emitted_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        tool_calls.mcp_emitted_source_tokens =
            checked_add(tool_calls.mcp_emitted_source_tokens, emitted)?;
    }
    Ok(())
}

fn ingest_response_item(
    payload: &Value,
    tool_calls: &mut ToolCallCounts,
    collaboration_calls: &mut Vec<CollaborationCall>,
) -> Result<(), DynError> {
    let payload_type = payload.pointer("/type").and_then(Value::as_str);
    if payload_type != Some("function_call") && payload_type != Some("custom_tool_call") {
        return Ok(());
    }
    let name = required_str(payload, "/name", "Codex tool name")?;
    if is_collaboration_tool(name) {
        tool_calls.collaboration += 1;
        let call_id = required_str(payload, "/call_id", "Codex collaboration call ID")?;
        let fork_turns = if name == "spawn_agent" {
            payload
                .pointer("/arguments")
                .and_then(Value::as_str)
                .map(serde_json::from_str::<Value>)
                .transpose()?
                .as_ref()
                .and_then(|arguments| arguments.pointer("/fork_turns"))
                .map(normalize_fork_turns)
        } else {
            None
        };
        collaboration_calls.push(CollaborationCall {
            call_id: call_id.to_owned(),
            tool: name.to_owned(),
            fork_turns,
        });
    } else if payload.pointer("/namespace").is_some() || name.starts_with("mcp__") {
        // The matching mcp_tool_call_end lifecycle event is authoritative and
        // carries result-size and outcome metadata.
    } else if name == "exec" {
        let input = payload
            .pointer("/input")
            .and_then(Value::as_str)
            .unwrap_or("");
        if input.contains("tools.exec_command") || input.contains("tools.write_stdin") {
            tool_calls.shell += 1;
        } else if !input.contains("tools.mcp__") {
            tool_calls.other += 1;
        }
    } else if matches!(name, "exec_command" | "write_stdin" | "functions.exec") {
        tool_calls.shell += 1;
    } else {
        tool_calls.other += 1;
    }
    Ok(())
}

fn normalize_fork_turns(value: &Value) -> String {
    match value {
        Value::String(mode) if mode == "none" || mode == "all" => mode.clone(),
        Value::String(turns) if turns.parse::<u64>().is_ok() => "bounded".to_owned(),
        Value::Number(_) => "bounded".to_owned(),
        _ => "invalid".to_owned(),
    }
}

fn is_collaboration_tool(name: &str) -> bool {
    matches!(
        name,
        "spawn_agent"
            | "wait_agent"
            | "send_message"
            | "followup_task"
            | "interrupt_agent"
            | "list_agents"
    )
}

fn compute_depths(
    parsed: &[ParsedRollout],
    index_by_id: &HashMap<&str, usize>,
) -> Result<Vec<usize>, DynError> {
    let mut depths = vec![0usize; parsed.len()];
    for index in 1..parsed.len() {
        let parent = parsed[index]
            .parent_id
            .as_deref()
            .ok_or("subagent rollout has no parent thread")?;
        let parent_index = index_by_id
            .get(parent)
            .copied()
            .ok_or("subagent parent is outside the discovered family")?;
        if parent_index >= index {
            return Err("Codex session family is not in parent-before-child order".into());
        }
        depths[index] = depths[parent_index]
            .checked_add(1)
            .ok_or("thread depth overflow")?;
    }
    Ok(depths)
}

fn render_svg(receipt: &FamilyReceipt) -> String {
    let width = 1000u64;
    let row_height = 54u64;
    let height = 150 + row_height * receipt.thread_receipts.len() as u64;
    let bar_x = 245f64;
    let bar_width = 570f64;
    let max_input = receipt
        .thread_receipts
        .iter()
        .map(|thread| thread.provider_usage.total_input_tokens)
        .max()
        .unwrap_or(1)
        .max(1) as f64;
    let mut svg = format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{width}\" height=\"{height}\" viewBox=\"0 0 {width} {height}\">\n<rect width=\"100%\" height=\"100%\" fill=\"#0f172a\"/>\n<style>text{{font-family:ui-monospace,SFMono-Regular,Menlo,monospace;fill:#e2e8f0}} .muted{{fill:#94a3b8}} .title{{font-size:20px;font-weight:700}} .label{{font-size:13px}}</style>\n<text x=\"32\" y=\"38\" class=\"title\">Codex multi-agent token receipt</text>\n<text x=\"32\" y=\"66\" class=\"muted label\">experiment: {} · arm: {} · family input: {} · output: {}</text>\n<rect x=\"32\" y=\"86\" width=\"14\" height=\"14\" fill=\"#38bdf8\"/><text x=\"54\" y=\"98\" class=\"label\">cache read</text>\n<rect x=\"170\" y=\"86\" width=\"14\" height=\"14\" fill=\"#fb923c\"/><text x=\"192\" y=\"98\" class=\"label\">uncached input</text>\n",
        xml_escape(&receipt.experiment_id),
        xml_escape(&receipt.arm),
        receipt.provider_usage.total_input_tokens,
        receipt.provider_usage.output_tokens,
    );
    for thread in &receipt.thread_receipts {
        let y = 128 + row_height * thread.thread_index as u64;
        let cached_width =
            bar_width * thread.provider_usage.cache_read_input_tokens as f64 / max_input;
        let uncached_width =
            bar_width * thread.provider_usage.uncached_input_tokens as f64 / max_input;
        svg.push_str(&format!(
            "<text x=\"32\" y=\"{}\" class=\"label\">thread-{:03} {} d{}</text>\n<rect x=\"{bar_x}\" y=\"{}\" width=\"{cached_width:.2}\" height=\"18\" rx=\"2\" fill=\"#38bdf8\"/>\n<rect x=\"{:.2}\" y=\"{}\" width=\"{uncached_width:.2}\" height=\"18\" rx=\"2\" fill=\"#fb923c\"/>\n<text x=\"835\" y=\"{}\" class=\"muted label\">in {} · out {}</text>\n",
            y + 14,
            thread.thread_index,
            thread.role,
            thread.depth,
            y,
            bar_x + cached_width,
            y,
            y + 14,
            thread.provider_usage.total_input_tokens,
            thread.provider_usage.output_tokens,
        ));
    }
    svg.push_str("</svg>\n");
    svg
}

fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn read_bounded(path: &Path) -> Result<Vec<u8>, DynError> {
    let mut bytes = Vec::new();
    File::open(path)?
        .take(MAX_ROLLOUT_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_ROLLOUT_BYTES {
        return Err("Codex rollout exceeds the 64 MiB safety bound".into());
    }
    Ok(bytes)
}

fn hash_file(path: &Path) -> Result<String, DynError> {
    Ok(blake3::hash(&fs::read(path)?).to_hex().to_string())
}

fn checked_add(left: u64, right: u64) -> Result<u64, DynError> {
    left.checked_add(right)
        .ok_or_else(|| "usage overflow".into())
}

fn required_str<'a>(value: &'a Value, pointer: &str, label: &str) -> Result<&'a str, DynError> {
    value
        .pointer(pointer)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("missing or invalid {label}").into())
}

fn required_u64(value: &Value, pointer: &str, label: &str) -> Result<u64, DynError> {
    value
        .pointer(pointer)
        .and_then(Value::as_u64)
        .ok_or_else(|| format!("missing or invalid {label}").into())
}

fn validate_label(value: &str, label: &str) -> Result<(), DynError> {
    if value.is_empty()
        || value.len() > 128
        || !value
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || "-_.".contains(character))
    {
        return Err(format!("{label} must be 1-128 safe ASCII characters").into());
    }
    Ok(())
}

fn validate_lower_hex(value: &str, length: usize, label: &str) -> Result<(), DynError> {
    if value.len() != length
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(format!("{label} must be {length} lowercase hexadecimal characters").into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const ROOT_ID: &str = "private-root-session-id";
    const CHILD_ID: &str = "private-child-session-id";

    #[test]
    fn test_receipt_discovers_child_sums_usage_and_redacts_ids() {
        let directory = tempfile::tempdir().expect("temporary sessions directory");
        let root_path = directory.path().join("root.jsonl");
        let child_path = directory.path().join("child.jsonl");
        fs::write(&root_path, root_rollout()).expect("root fixture");
        fs::write(child_path, child_rollout()).expect("child fixture");

        let receipt = build_receipt(
            &root_path,
            directory.path(),
            "fixture".to_owned(),
            "thin_native".to_owned(),
            Some(1),
            None,
        )
        .expect("receipt");

        assert_eq!(receipt.topology.thread_count, 2);
        assert_eq!(receipt.topology.child_thread_count, 1);
        assert_eq!(receipt.topology.maximum_depth, 1);
        assert_eq!(receipt.topology.spawn_calls, 1);
        assert_eq!(receipt.topology.matched_spawn_calls, 1);
        assert_eq!(receipt.topology.fork_turns.get("none"), Some(&1));
        assert_eq!(receipt.provider_usage.total_input_tokens, 300);
        assert_eq!(receipt.provider_usage.cache_read_input_tokens, 230);
        assert_eq!(receipt.provider_usage.uncached_input_tokens, 70);
        assert_eq!(receipt.provider_usage.output_tokens, 30);
        assert_eq!(receipt.first_request_usage.total_input_tokens, 280);
        assert_eq!(receipt.thread_receipts[1].parent_thread_index, Some(0));
        let serialized = serde_json::to_string(&receipt).expect("receipt JSON");
        assert!(!serialized.contains(ROOT_ID));
        assert!(!serialized.contains(CHILD_ID));
        assert!(!render_svg(&receipt).contains(ROOT_ID));
    }

    #[test]
    fn test_receipt_rejects_inconsistent_provider_usage() {
        let value = serde_json::json!({
            "input_tokens": 10,
            "cached_input_tokens": 11,
            "output_tokens": 2,
            "reasoning_output_tokens": 0,
            "total_tokens": 12
        });
        assert!(UsageReceipt::parse(&value).is_err());
    }

    #[test]
    fn test_mcp_lifecycle_records_size_tokens_and_failure_without_content() {
        let mut calls = ToolCallCounts::default();
        let payload = serde_json::json!({
            "type":"mcp_tool_call_end",
            "result": {
                "Ok": {
                    "content":[{"type":"text","text":"private source"}],
                    "structuredContent":{"meta":{"emitted_tokens":7},"private":"source"},
                    "isError":true
                }
            }
        });

        ingest_mcp_result(&payload, &mut calls).expect("MCP result");

        assert_eq!(calls.mcp, 1);
        assert_eq!(calls.failed_mcp, 1);
        assert_eq!(calls.mcp_text_content_bytes, 14);
        assert!(calls.mcp_structured_content_bytes > 0);
        assert!(calls.mcp_result_json_bytes > calls.mcp_structured_content_bytes);
        assert_eq!(calls.mcp_emitted_source_tokens, 7);
        let serialized = serde_json::to_string(&calls).expect("call counts");
        assert!(!serialized.contains("private source"));
    }

    #[test]
    fn test_gold_evaluation_requires_exact_paths_and_symbols_without_retaining_answer() {
        let directory = tempfile::tempdir().expect("temporary manifest directory");
        let path = directory.path().join("gold.json");
        fs::write(
            &path,
            serde_json::to_vec(&serde_json::json!({
                "schema_version":1,
                "experiment_id":"fixture",
                "task_id":"owner-trace",
                "repository_revision":"a".repeat(40),
                "expected_evidence":[
                    {"path":"src/owner.rs","symbol":"owner"},
                    {"path":"src/owner.rs","symbol":"helper"},
                    {"path":"tests/owner.rs","symbol":"test_owner"}
                ]
            }))
            .expect("manifest JSON"),
        )
        .expect("manifest fixture");
        let answer = serde_json::json!({
            "evidence":[
                {"path":"src/owner.rs","symbol":"owner"},
                {"path":"src/owner.rs","symbol":"helper"},
                {"path":"tests/owner.rs","symbol":"test_owner"}
            ]
        })
        .to_string();

        let success = evaluate_task(&path, "fixture", Some(&answer)).expect("evaluation");
        assert!(success.task_success);
        assert_eq!(success.matched_path_count, 2);
        assert_eq!(success.matched_exact_evidence_count, 3);
        assert!(
            !serde_json::to_string(&success)
                .expect("evaluation JSON")
                .contains("src/owner.rs")
        );

        let wrong = answer.replace("test_owner", "wrapper");
        let failure = evaluate_task(&path, "fixture", Some(&wrong)).expect("failed evaluation");
        assert!(!failure.task_success);
        assert_eq!(failure.matched_path_count, 2);
        assert_eq!(failure.matched_exact_evidence_count, 2);
        assert_eq!(failure.unexpected_evidence_count, 1);
    }

    #[test]
    fn test_gold_evaluation_path_mode_requires_complete_non_duplicate_file_set() {
        let directory = tempfile::tempdir().expect("temporary manifest directory");
        let path = directory.path().join("gold.json");
        fs::write(
            &path,
            serde_json::to_vec(&serde_json::json!({
                "schema_version":1,
                "experiment_id":"fixture",
                "task_id":"file-trace",
                "repository_revision":"b".repeat(40),
                "evaluation_mode":"path",
                "expected_evidence":[
                    {"path":"src/owner.py"},
                    {"path":"tests/test_owner.py"}
                ]
            }))
            .expect("manifest JSON"),
        )
        .expect("manifest fixture");
        let answer = serde_json::json!({
            "evidence":[
                {"path":"src/owner.py","symbol":"Owner.run"},
                {"path":"tests/test_owner.py","symbol":"test_owner"}
            ]
        })
        .to_string();

        let success = evaluate_task(&path, "fixture", Some(&answer)).expect("evaluation");
        assert!(success.task_success);
        assert_eq!(success.matched_path_count, 2);
        assert_eq!(success.matched_exact_evidence_count, 0);

        let duplicate = serde_json::json!({
            "evidence":[
                {"path":"src/owner.py","symbol":"Owner.run"},
                {"path":"src/owner.py","symbol":"run"}
            ]
        })
        .to_string();
        let failure = evaluate_task(&path, "fixture", Some(&duplicate)).expect("evaluation");
        assert!(!failure.task_success);
        assert_eq!(failure.reported_path_count, 1);
        assert_eq!(failure.matched_path_count, 1);
        assert_eq!(failure.unexpected_evidence_count, 0);
    }

    #[test]
    fn test_full_fork_history_is_measured_but_not_parsed_as_live_usage() {
        let directory = tempfile::tempdir().expect("temporary sessions directory");
        let child_path = directory.path().join("child.jsonl");
        fs::write(&child_path, child_rollout_with_inherited_history()).expect("child fixture");

        let rollout = parse_rollout(&child_path).expect("forked child rollout");

        assert_eq!(rollout.turn_count, 1);
        assert_eq!(rollout.provider_request_count, 1);
        assert_eq!(rollout.provider_usage.total_input_tokens, 100);
        assert_eq!(rollout.inherited_history_record_count, 3);
        assert!(rollout.inherited_history_bytes > 0);
    }

    fn root_rollout() -> String {
        jsonl(&[
            serde_json::json!({
                "type": "session_meta",
                "payload": {
                    "id": ROOT_ID,
                    "parent_thread_id": null,
                    "thread_source": "user",
                    "source": "exec",
                    "cli_version": "0.test",
                    "model_provider": "fixture"
                }
            }),
            serde_json::json!({"type":"event_msg","payload":{"type":"task_started"}}),
            serde_json::json!({
                "type":"turn_context",
                "payload":{"model":"fixture-model","effort":"low"}
            }),
            serde_json::json!({
                "type":"response_item",
                "payload": {
                    "type":"function_call",
                    "name":"spawn_agent",
                    "call_id":"private-call-id",
                    "arguments":"{\"fork_turns\":\"none\",\"message\":\"private\"}"
                }
            }),
            serde_json::json!({
                "type":"event_msg",
                "payload": {
                    "type":"sub_agent_activity",
                    "kind":"started",
                    "event_id":"private-call-id",
                    "agent_thread_id":CHILD_ID
                }
            }),
            token_count(200, 150, 20, 5),
            serde_json::json!({
                "type":"event_msg",
                "payload":{"type":"task_complete","duration_ms":100}
            }),
        ])
    }

    fn child_rollout() -> String {
        jsonl(&[
            serde_json::json!({
                "type": "session_meta",
                "payload": {
                    "id": CHILD_ID,
                    "parent_thread_id": ROOT_ID,
                    "thread_source": "subagent",
                    "source": {"subagent":{}},
                    "cli_version": "0.test",
                    "model_provider": "fixture",
                    "multi_agent_version":"v2"
                }
            }),
            serde_json::json!({"type":"event_msg","payload":{"type":"task_started"}}),
            serde_json::json!({
                "type":"turn_context",
                "payload":{"model":"fixture-model","effort":"low"}
            }),
            serde_json::json!({
                "type":"inter_agent_communication_metadata",
                "payload":{"trigger_turn":true}
            }),
            token_count(100, 80, 10, 2),
            serde_json::json!({
                "type":"event_msg",
                "payload":{"type":"task_complete","duration_ms":50}
            }),
        ])
    }

    fn child_rollout_with_inherited_history() -> String {
        jsonl(&[
            serde_json::json!({
                "type": "session_meta",
                "payload": {
                    "id": CHILD_ID,
                    "parent_thread_id": ROOT_ID,
                    "thread_source": "subagent",
                    "source": {"subagent":{}},
                    "cli_version": "0.test",
                    "model_provider": "fixture",
                    "multi_agent_version":"v2"
                }
            }),
            serde_json::json!({
                "type":"session_meta",
                "payload":{"id":ROOT_ID,"parent_thread_id":null}
            }),
            serde_json::json!({"type":"event_msg","payload":{"type":"task_started"}}),
            serde_json::json!({"type":"response_item","payload":{"type":"message"}}),
            serde_json::json!({"type":"event_msg","payload":{"type":"task_started"}}),
            serde_json::json!({
                "type":"turn_context",
                "payload":{"model":"fixture-model","effort":"low"}
            }),
            serde_json::json!({
                "type":"inter_agent_communication_metadata",
                "payload":{"trigger_turn":true}
            }),
            token_count(100, 80, 10, 2),
            serde_json::json!({
                "type":"event_msg",
                "payload":{"type":"task_complete","duration_ms":50}
            }),
        ])
    }

    fn token_count(input: u64, cached: u64, output: u64, reasoning: u64) -> Value {
        serde_json::json!({
            "type":"event_msg",
            "payload": {
                "type":"token_count",
                "info": {
                    "total_token_usage": {
                        "input_tokens":input,
                        "cached_input_tokens":cached,
                        "output_tokens":output,
                        "reasoning_output_tokens":reasoning,
                        "total_tokens":input + output
                    },
                    "last_token_usage": {
                        "input_tokens":input - 10,
                        "cached_input_tokens":cached.saturating_sub(10),
                        "output_tokens":output,
                        "reasoning_output_tokens":reasoning,
                        "total_tokens":input + output - 10
                    }
                }
            }
        })
    }

    fn jsonl(records: &[Value]) -> String {
        let mut output = records
            .iter()
            .map(|record| serde_json::to_string(record).expect("JSON record"))
            .collect::<Vec<_>>()
            .join("\n");
        output.push('\n');
        output
    }
}
