#[path = "support/model_ab_artifacts.rs"]
mod model_ab_artifacts;

use std::collections::HashSet;
use std::error::Error;
use std::fs;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use leantoken::tokens::Tokenizer;
use model_ab_artifacts::{
    ARTIFACT_SCHEMA_V1, PREWALK_HANDOFF_FILE, PROVIDER_USAGE_FILE, PrewalkHandoff, ProviderUsage,
    ProviderUsageReceipt, RangeIdentity, RunBinding, TOOL_TRACE_FILE, TRAJECTORY_FILE, ToolCall,
    ToolOutcome, ToolTrace, Trajectory, ValidatedEdit,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use wait_timeout::ChildExt;

const MAX_CODEX_STDOUT_BYTES: u64 = 64 * 1024 * 1024;
const CODEX_POLL_INTERVAL: Duration = Duration::from_millis(10);

#[derive(Debug, Deserialize)]
struct AdapterRequest {
    schema_version: u32,
    experiment_id: String,
    manifest_blake3: String,
    random_seed: u64,
    repetition: usize,
    arm_order_index: usize,
    arm: String,
    primary_model: String,
    executor_model: Option<String>,
    arm_definition: ArmDefinition,
    repository: PathBuf,
    revision: String,
    task_id: String,
    prompt: String,
    artifacts_directory: PathBuf,
    timeout_seconds: u64,
}

#[derive(Debug, Deserialize)]
struct ArmDefinition {
    runtime_binary: PathBuf,
    runtime_binary_blake3: String,
    configuration: Value,
    tool_catalog: Vec<String>,
    budget: ArmBudget,
    retrieval_contract: String,
}

#[derive(Debug, Deserialize)]
struct ArmBudget {
    tool_call_limit: usize,
    context_token_limit: usize,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CodexConfiguration {
    retrieval: String,
    codex_executable: PathBuf,
    codex_executable_blake3: String,
    codex_version: String,
    reasoning_effort: String,
    service_tier: String,
    tokenizer: Tokenizer,
    mcp_enabled: bool,
    mcp_result_mode: String,
    #[serde(default)]
    prewalk_tool_call_limit: Option<usize>,
    #[serde(default)]
    executor_tool_call_limit: Option<usize>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum RetrievalPolicy {
    NativeOnly,
    LeanTokenProgressive,
    LeanTokenOneShot,
    Prewalk,
}

#[derive(Debug, Serialize)]
struct RetrievalContractEvidence {
    policy: RetrievalPolicy,
    leantoken_calls: usize,
    successful_leantoken_calls: usize,
    leantoken_evidence_calls: usize,
    leantoken_tools: Vec<String>,
    phase_boundary: Option<usize>,
    native_retrieval_calls: usize,
    pre_leantoken_substantive_calls: usize,
    first_validated_edit: Option<ValidatedEdit>,
}

#[derive(Debug, Serialize)]
struct AdapterResult {
    schema_version: u32,
    task_success: bool,
    total_input_tokens: Option<u64>,
    total_output_tokens: Option<u64>,
    provider_reported_cost_usd: Option<f64>,
    tool_calls: usize,
    rereads: usize,
    reread_tokens: u64,
    failed_tool_calls: usize,
    failed_searches: usize,
    dead_end_reads: usize,
    provider_usage: ProviderUsage,
    evidence_receipt: Value,
    repository_generation: Option<u64>,
}

#[derive(Debug)]
struct Analysis {
    events: Vec<Value>,
    calls: Vec<ToolCall>,
    usage: Option<ProviderUsage>,
    usage_event: Option<Value>,
    total_input_tokens: Option<u64>,
    total_output_tokens: Option<u64>,
    repository_generation: Option<u64>,
    thread_id_hash: Option<String>,
    seen_ranges: HashSet<RangeKey>,
    native_retrieval_sequences: Vec<usize>,
    pre_leantoken_substantive_sequences: Vec<usize>,
    leantoken_tools: Vec<String>,
    verification_sequences: Vec<usize>,
    phase_boundary: Option<usize>,
    tokenizer: Tokenizer,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct RangeKey {
    repository_generation: u64,
    path: String,
    start_line: usize,
    end_line: usize,
    content_hash: String,
}

fn main() -> Result<(), Box<dyn Error>> {
    let request: AdapterRequest = serde_json::from_reader(io::stdin())?;
    if request.schema_version != 4 {
        return Err("unsupported model A/B request schema".into());
    }
    let configuration: CodexConfiguration =
        serde_json::from_value(request.arm_definition.configuration.clone())?;
    let retrieval_policy = validate_request(&request, &configuration)?;
    verify_executable(
        &configuration.codex_executable,
        &configuration.codex_executable_blake3,
    )?;
    verify_executable(
        &request.arm_definition.runtime_binary,
        &request.arm_definition.runtime_binary_blake3,
    )?;
    let version = command_stdout(Command::new(&configuration.codex_executable).arg("--version"))?;
    if version.trim() != configuration.codex_version {
        return Err("Codex CLI version mismatch".into());
    }

    let binding = RunBinding {
        experiment_id: request.experiment_id.clone(),
        manifest_blake3: request.manifest_blake3.clone(),
        task_id: request.task_id.clone(),
        repetition: request.repetition,
        arm: request.arm.clone(),
    };
    let mut analysis = if retrieval_policy == RetrievalPolicy::Prewalk {
        execute_prewalk(&request, &configuration, &binding)?
    } else {
        let prompt = build_prompt(&request, retrieval_policy);
        let output = run_codex(
            &request,
            &configuration,
            &prompt,
            &request.primary_model,
            configuration.mcp_enabled,
            request.arm_definition.budget.tool_call_limit,
            Duration::from_secs(request.timeout_seconds.saturating_sub(5)),
        )?;
        analyze_events(&output, configuration.tokenizer)?
    };
    let provider_usage = analysis
        .usage
        .clone()
        .ok_or("Codex JSONL stream had no completed provider usage")?;
    persist_artifacts(
        &request.artifacts_directory,
        &binding,
        &analysis,
        &provider_usage,
        &configuration,
        &request,
    )?;
    let retrieval_contract = validate_retrieval_execution(retrieval_policy, &analysis)?;

    if analysis.calls.len() > request.arm_definition.budget.tool_call_limit {
        return Err(format!(
            "Codex used {} tool calls, exceeding frozen limit {}",
            analysis.calls.len(),
            request.arm_definition.budget.tool_call_limit
        )
        .into());
    }
    let maximum_result_tokens = analysis
        .calls
        .iter()
        .filter(|call| call.tool_name == "leantoken")
        .map(|call| call.result_source_tokens)
        .max()
        .unwrap_or(0);
    if maximum_result_tokens > request.arm_definition.budget.context_token_limit as u64 {
        return Err(format!(
            "a retrieval result used {maximum_result_tokens} source tokens, exceeding frozen per-call limit {}",
            request.arm_definition.budget.context_token_limit
        )
        .into());
    }

    let rereads = analysis.calls.iter().filter(|call| call.reread).count();
    let reread_tokens = analysis
        .calls
        .iter()
        .filter(|call| call.reread)
        .try_fold(0_u64, |sum, call| {
            sum.checked_add(call.result_source_tokens)
        })
        .ok_or("reread token total overflow")?;
    let failed_searches = analysis
        .calls
        .iter()
        .filter(|call| call.outcome == ToolOutcome::FailedSearch)
        .count();
    let failed_tool_calls = analysis
        .calls
        .iter()
        .filter(|call| matches!(call.outcome, ToolOutcome::FailedSearch | ToolOutcome::Error))
        .count();
    let dead_end_reads = analysis
        .calls
        .iter()
        .filter(|call| call.outcome == ToolOutcome::DeadEndRead)
        .count();
    let successful_edits = analysis
        .calls
        .iter()
        .filter(|call| call.tool_name == "edit" && call.outcome == ToolOutcome::Success)
        .count();
    let result = AdapterResult {
        schema_version: 4,
        task_success: successful_edits > 0 && failed_tool_calls == 0,
        total_input_tokens: analysis.total_input_tokens,
        total_output_tokens: analysis.total_output_tokens,
        provider_reported_cost_usd: None,
        tool_calls: analysis.calls.len(),
        rereads,
        reread_tokens,
        failed_tool_calls,
        failed_searches,
        dead_end_reads,
        provider_usage,
        evidence_receipt: serde_json::json!({
            "host": "codex-cli",
            "host_version": configuration.codex_version,
            "thread_id_blake3": analysis.thread_id_hash.take(),
            "retrieval": configuration.retrieval,
            "retrieval_contract": retrieval_contract,
            "successful_edits": successful_edits,
            "random_seed": request.random_seed,
            "arm_order_index": request.arm_order_index,
        }),
        repository_generation: analysis.repository_generation,
    };
    serde_json::to_writer(io::stdout(), &result)?;
    Ok(())
}

fn validate_request(
    request: &AdapterRequest,
    configuration: &CodexConfiguration,
) -> Result<RetrievalPolicy, Box<dyn Error>> {
    if request.timeout_seconds <= 10
        || request.prompt.trim().is_empty()
        || request.primary_model.trim().is_empty()
        || request.arm_definition.tool_catalog.is_empty()
        || request.arm_definition.budget.tool_call_limit == 0
        || request.arm_definition.budget.context_token_limit == 0
        || configuration.reasoning_effort.trim().is_empty()
        || configuration.service_tier.trim().is_empty()
    {
        return Err("incomplete Codex adapter request".into());
    }
    let retrieval_policy = match (request.arm.as_str(), configuration.retrieval.as_str()) {
        ("filesystem", "native") => RetrievalPolicy::NativeOnly,
        ("lean_token_progressive", "progressive") => RetrievalPolicy::LeanTokenProgressive,
        ("lean_token_one_shot", "one_shot_context") => RetrievalPolicy::LeanTokenOneShot,
        ("prewalk", "frontier_prewalk_executor") => RetrievalPolicy::Prewalk,
        _ => return Err("arm and retrieval configuration do not match".into()),
    };
    if configuration.mcp_enabled != (retrieval_policy != RetrievalPolicy::NativeOnly) {
        return Err("mcp_enabled does not match the model A/B arm".into());
    }
    if configuration.mcp_result_mode != "dual" {
        return Err("real model A/B keeps the frozen compatible MCP result mode dual".into());
    }
    let catalog = &request.arm_definition.tool_catalog;
    let has = |tool: &str| catalog.iter().any(|entry| entry == tool);
    let native_catalog = ["path_list", "text_search", "file_read"];
    let catalog_matches = match retrieval_policy {
        RetrievalPolicy::NativeOnly => {
            has("shell")
                && has("edit")
                && !has("leantoken")
                && native_catalog.iter().all(|tool| has(tool))
        }
        RetrievalPolicy::LeanTokenProgressive
        | RetrievalPolicy::LeanTokenOneShot
        | RetrievalPolicy::Prewalk => {
            has("shell")
                && has("edit")
                && has("leantoken")
                && native_catalog.iter().all(|tool| !has(tool))
        }
    };
    if !catalog_matches {
        return Err("arm tool catalog does not match the Codex adapter".into());
    }
    if retrieval_policy == RetrievalPolicy::Prewalk {
        let prewalk_limit = configuration
            .prewalk_tool_call_limit
            .ok_or("prewalk arm is missing its phase tool limit")?;
        let executor_limit = configuration
            .executor_tool_call_limit
            .ok_or("prewalk arm is missing its executor tool limit")?;
        let executor_model = request
            .executor_model
            .as_deref()
            .filter(|model| !model.trim().is_empty())
            .ok_or("prewalk arm is missing executor_model")?;
        if prewalk_limit == 0
            || executor_limit == 0
            || prewalk_limit.saturating_add(executor_limit)
                > request.arm_definition.budget.tool_call_limit
            || executor_model == request.primary_model
        {
            return Err("prewalk phase limits or model separation are invalid".into());
        }
    } else if request.executor_model.is_some()
        || configuration.prewalk_tool_call_limit.is_some()
        || configuration.executor_tool_call_limit.is_some()
    {
        return Err("non-prewalk arm contains prewalk-only configuration".into());
    }
    let repository = request.repository.canonicalize()?;
    let head = command_stdout(
        Command::new("git")
            .arg("-C")
            .arg(&repository)
            .args(["rev-parse", "HEAD"]),
    )?;
    if head.trim() != request.revision {
        return Err("task repository revision mismatch".into());
    }
    Ok(retrieval_policy)
}

fn build_prompt(request: &AdapterRequest, retrieval_policy: RetrievalPolicy) -> String {
    let tools = match retrieval_policy {
        RetrievalPolicy::NativeOnly => {
            "LeanToken is unavailable in this arm. Use only native repository tools."
        }
        RetrievalPolicy::LeanTokenProgressive => {
            "LeanToken is the only permitted repository discovery and source-reading tool. Do not call leantoken_context. Progress from the narrow files, outline, search, and read tools as evidence warrants. Call LeanToken before any substantive shell command or edit. Native shell commands remain available only for Git preflight and post-retrieval build, test, lint, and patch verification."
        }
        RetrievalPolicy::LeanTokenOneShot => {
            "LeanToken is the only permitted repository discovery and source-reading tool. Make exactly one leantoken_context call before any substantive shell command or edit, then implement from that fixed bundle without further repository retrieval. Native shell commands remain available only for build, test, lint, and patch verification."
        }
        RetrievalPolicy::Prewalk => unreachable!("prewalk uses phase-specific prompts"),
    };
    format!(
        "Solve the repository task below by inspecting and editing the current worktree. Run relevant tests before finishing. Do not use web search or network access, do not inspect benchmark gold patches, do not issue tool calls in parallel, and do not merely describe a solution.\n\nFrozen retrieval contract: {}\n{}\nPer-call retrieval source budget: {} tokens. Total tool-call limit: {}.\n\nTask:\n{}",
        request.arm_definition.retrieval_contract,
        tools,
        request.arm_definition.budget.context_token_limit,
        request.arm_definition.budget.tool_call_limit,
        request.prompt
    )
}

fn execute_prewalk(
    request: &AdapterRequest,
    configuration: &CodexConfiguration,
    binding: &RunBinding,
) -> Result<Analysis, Box<dyn Error>> {
    let started = Instant::now();
    let total_timeout = Duration::from_secs(request.timeout_seconds.saturating_sub(5));
    let prewalk_timeout = total_timeout / 2;
    let prewalk_prompt = format!(
        "Explore and begin the repository task below as the frontier prewalk. Use LeanToken as the only repository discovery and source-reading mechanism; native shell commands are allowed only for Git preflight and validation. Do not issue tool calls in parallel. Maintain a bounded todo list, gather grounded path/range evidence, make the first evidence-grounded edit, run a focused validation for that edit, then stop. Return a compact handoff summary, but do not finish the entire task when a validated first edit is available.\n\nFrozen retrieval contract: {}\nPer-call retrieval source budget: {} tokens. Prewalk tool-call limit: {}.\n\nTask:\n{}",
        request.arm_definition.retrieval_contract,
        request.arm_definition.budget.context_token_limit,
        configuration
            .prewalk_tool_call_limit
            .expect("validated prewalk limit"),
        request.prompt
    );
    let prewalk_output = run_codex(
        request,
        configuration,
        &prewalk_prompt,
        &request.primary_model,
        true,
        configuration
            .prewalk_tool_call_limit
            .expect("validated prewalk limit"),
        prewalk_timeout,
    )?;
    let mut prewalk = analyze_events(&prewalk_output, configuration.tokenizer)?;
    if prewalk.calls.len()
        > configuration
            .prewalk_tool_call_limit
            .expect("validated prewalk limit")
    {
        return Err("frontier prewalk exceeded its frozen phase tool limit".into());
    }
    let first_validated_edit = first_validated_edit(&prewalk)
        .ok_or("frontier prewalk did not transfer a first validated edit")?;
    let todo_events = prewalk
        .events
        .iter()
        .filter(|event| event.pointer("/item/type").and_then(Value::as_str) == Some("todo_list"))
        .cloned()
        .collect::<Vec<_>>();
    let mut evidence_calls = prewalk
        .calls
        .iter()
        .filter(|call| {
            call.tool_name == "leantoken"
                && call.outcome == ToolOutcome::Success
                && (call.result_source_tokens > 0 || !call.ranges.is_empty())
        })
        .cloned()
        .collect::<Vec<_>>();
    for call in &mut evidence_calls {
        call.call_id = format!("prewalk:{}", call.call_id);
        call.result_id = format!("prewalk:{}", call.result_id);
    }
    let patch = git_stdout(
        &request.repository,
        &["diff", "--binary", "--full-index", "HEAD", "--"],
    )?;
    let executor_model = request
        .executor_model
        .clone()
        .expect("validated executor model");
    let handoff = PrewalkHandoff {
        schema_version: ARTIFACT_SCHEMA_V1,
        binding: binding.clone(),
        primary_model: request.primary_model.clone(),
        executor_model: executor_model.clone(),
        trajectory_events: prewalk.events.clone(),
        todo_events,
        evidence_calls,
        worktree_patch: patch,
        first_validated_edit,
    };
    write_json(
        request.artifacts_directory.join(PREWALK_HANDOFF_FILE),
        &handoff,
    )?;
    let serialized_handoff = serde_json::to_string(&handoff)?;
    let executor_prompt = format!(
        "Continue the same repository task as the cheaper executor. The frontier prewalk already changed this worktree. The complete machine-readable handoff below contains its raw trajectory events, bounded todo state, grounded evidence calls with range identities, exact worktree patch, and first validated edit. Use that state directly; do not repeat repository discovery or source reads, do not use web search, and do not issue tool calls in parallel. Finish the implementation, run relevant validation, and leave the worktree ready for the independent validator. Executor tool-call limit: {}.\n\nPREWALK_HANDOFF_JSON\n{}\nEND_PREWALK_HANDOFF_JSON\n\nTask:\n{}",
        configuration
            .executor_tool_call_limit
            .expect("validated executor limit"),
        serialized_handoff,
        request.prompt
    );
    let elapsed = started.elapsed();
    let remaining = total_timeout
        .checked_sub(elapsed)
        .filter(|duration| *duration > Duration::from_secs(10))
        .ok_or("frontier prewalk left no executor time budget")?;
    let executor_output = run_codex(
        request,
        configuration,
        &executor_prompt,
        &executor_model,
        false,
        configuration
            .executor_tool_call_limit
            .expect("validated executor limit"),
        remaining,
    )?;
    let executor = analyze_events(&executor_output, configuration.tokenizer)?;
    if executor.calls.len()
        > configuration
            .executor_tool_call_limit
            .expect("validated executor limit")
    {
        return Err("cheaper executor exceeded its frozen phase tool limit".into());
    }
    merge_phase_analyses(&mut prewalk, executor)?;
    Ok(prewalk)
}

fn first_validated_edit(analysis: &Analysis) -> Option<ValidatedEdit> {
    let edit_sequence = analysis
        .calls
        .iter()
        .find(|call| call.tool_name == "edit" && call.outcome == ToolOutcome::Success)?
        .sequence;
    let validation_sequence = analysis
        .verification_sequences
        .iter()
        .copied()
        .find(|sequence| *sequence > edit_sequence)?;
    Some(ValidatedEdit {
        edit_sequence,
        validation_sequence,
    })
}

fn merge_phase_analyses(
    prewalk: &mut Analysis,
    mut executor: Analysis,
) -> Result<(), Box<dyn Error>> {
    let boundary = prewalk.calls.len();
    for call in &mut prewalk.calls {
        call.call_id = format!("prewalk:{}", call.call_id);
        call.result_id = format!("prewalk:{}", call.result_id);
    }
    for call in &mut executor.calls {
        call.sequence += boundary;
        call.call_id = format!("executor:{}", call.call_id);
        call.result_id = format!("executor:{}", call.result_id);
    }
    prewalk.native_retrieval_sequences.extend(
        executor
            .native_retrieval_sequences
            .iter()
            .map(|value| value + boundary),
    );
    prewalk.pre_leantoken_substantive_sequences.extend(
        executor
            .pre_leantoken_substantive_sequences
            .iter()
            .map(|value| value + boundary),
    );
    prewalk.verification_sequences.extend(
        executor
            .verification_sequences
            .iter()
            .map(|value| value + boundary),
    );
    prewalk.leantoken_tools.extend(executor.leantoken_tools);
    prewalk.calls.extend(executor.calls);
    prewalk.events.push(serde_json::json!({
        "type": "leantoken.phase_boundary",
        "phase": "executor",
        "tool_sequence": boundary
    }));
    prewalk.events.extend(executor.events);
    prewalk.total_input_tokens = sum_optional_u64(
        prewalk.total_input_tokens,
        executor.total_input_tokens,
        "provider input token total",
    )?;
    prewalk.total_output_tokens = sum_optional_u64(
        prewalk.total_output_tokens,
        executor.total_output_tokens,
        "provider output token total",
    )?;
    prewalk.usage = match (prewalk.usage.take(), executor.usage) {
        (Some(left), Some(right)) => Some(sum_provider_usage(left, right)?),
        _ => None,
    };
    prewalk.usage_event = Some(serde_json::json!({
        "prewalk": prewalk.usage_event.take(),
        "executor": executor.usage_event
    }));
    prewalk.thread_id_hash = Some(
        blake3::hash(
            format!(
                "{}:{}",
                prewalk.thread_id_hash.as_deref().unwrap_or_default(),
                executor.thread_id_hash.as_deref().unwrap_or_default()
            )
            .as_bytes(),
        )
        .to_hex()
        .to_string(),
    );
    prewalk.phase_boundary = Some(boundary);
    Ok(())
}

fn sum_provider_usage(
    left: ProviderUsage,
    right: ProviderUsage,
) -> Result<ProviderUsage, Box<dyn Error>> {
    Ok(ProviderUsage {
        uncached_input_tokens: sum_optional_u64(
            left.uncached_input_tokens,
            right.uncached_input_tokens,
            "uncached input tokens",
        )?,
        cache_creation_input_tokens: sum_optional_u64(
            left.cache_creation_input_tokens,
            right.cache_creation_input_tokens,
            "cache creation input tokens",
        )?,
        cache_read_input_tokens: sum_optional_u64(
            left.cache_read_input_tokens,
            right.cache_read_input_tokens,
            "cache read input tokens",
        )?,
        output_tokens: sum_optional_u64(left.output_tokens, right.output_tokens, "output tokens")?,
        reasoning_tokens: sum_optional_u64(
            left.reasoning_tokens,
            right.reasoning_tokens,
            "reasoning tokens",
        )?,
    })
}

fn sum_optional_u64(
    left: Option<u64>,
    right: Option<u64>,
    label: &str,
) -> Result<Option<u64>, Box<dyn Error>> {
    match (left, right) {
        (Some(left), Some(right)) => left
            .checked_add(right)
            .map(Some)
            .ok_or_else(|| format!("{label} overflow").into()),
        _ => Ok(None),
    }
}

fn run_codex(
    request: &AdapterRequest,
    configuration: &CodexConfiguration,
    prompt: &str,
    model: &str,
    mcp_enabled: bool,
    tool_call_limit: usize,
    timeout: Duration,
) -> Result<Vec<u8>, Box<dyn Error>> {
    let repository = request.repository.canonicalize()?;
    let mut command = Command::new(&configuration.codex_executable);
    command
        .arg("exec")
        .args([
            "--json",
            "--ephemeral",
            "--ignore-user-config",
            "--ignore-rules",
            "--strict-config",
            "--color",
            "never",
            "--sandbox",
            "workspace-write",
            "--model",
        ])
        .arg(model)
        .arg("--cd")
        .arg(&repository)
        .args(["--config", "approval_policy=\"never\""])
        .args(["--config", "web_search=\"disabled\""])
        .args(["--config", "include_apps_instructions=false"])
        .arg("--config")
        .arg(format!(
            "model_reasoning_effort={}",
            toml_string(&configuration.reasoning_effort)
        ))
        .arg("--config")
        .arg(format!(
            "service_tier={}",
            toml_string(&configuration.service_tier)
        ))
        .args(["--config", "model_supports_parallel_tool_calls=false"]);
    if mcp_enabled {
        command
            .arg("--config")
            .arg(format!(
                "mcp_servers.leantoken.command={}",
                toml_string(
                    request
                        .arm_definition
                        .runtime_binary
                        .canonicalize()?
                        .to_string_lossy()
                        .as_ref()
                )
            ))
            .arg("--config")
            .arg(format!(
                "mcp_servers.leantoken.args={}",
                serde_json::to_string(&vec![
                    "mcp".to_owned(),
                    "--root".to_owned(),
                    repository.to_string_lossy().into_owned(),
                    "--tokenizer".to_owned(),
                    configuration.tokenizer.name().to_owned(),
                    "--result-mode".to_owned(),
                    configuration.mcp_result_mode.clone(),
                ])?
            ))
            .args(["--config", "mcp_servers.leantoken.required=true"])
            .args([
                "--config",
                "mcp_servers.leantoken.default_tools_approval_mode=\"approve\"",
            ])
            .args(["--config", "mcp_servers.leantoken.startup_timeout_sec=300"])
            .args(["--config", "mcp_servers.leantoken.tool_timeout_sec=120"]);
    }
    command
        .arg("-")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());
    let mut child = command.spawn()?;
    child
        .stdin
        .take()
        .ok_or("Codex stdin unavailable")?
        .write_all(prompt.as_bytes())?;
    let stdout = child.stdout.take().ok_or("Codex stdout unavailable")?;
    let tool_limit_exceeded = Arc::new(AtomicBool::new(false));
    let output_limit_exceeded = Arc::new(AtomicBool::new(false));
    let reader_tool_limit_exceeded = Arc::clone(&tool_limit_exceeded);
    let reader_output_limit_exceeded = Arc::clone(&output_limit_exceeded);
    let reader = thread::spawn(move || -> io::Result<Vec<u8>> {
        let mut stdout = BufReader::new(stdout.take(MAX_CODEX_STDOUT_BYTES + 1));
        let mut output = Vec::new();
        let mut line = Vec::new();
        let mut tool_starts = 0usize;
        loop {
            line.clear();
            if stdout.read_until(b'\n', &mut line)? == 0 {
                break;
            }
            output.extend_from_slice(&line);
            if output.len() as u64 > MAX_CODEX_STDOUT_BYTES {
                reader_output_limit_exceeded.store(true, Ordering::Release);
                break;
            }
            if is_tool_start_record(&line) {
                tool_starts = tool_starts.saturating_add(1);
                if tool_starts > tool_call_limit {
                    reader_tool_limit_exceeded.store(true, Ordering::Release);
                    break;
                }
            }
        }
        Ok(output)
    });
    let started = Instant::now();
    let status = loop {
        if tool_limit_exceeded.load(Ordering::Acquire) {
            let _ = child.kill();
            let _ = child.wait();
            let _ = reader.join();
            return Err(
                format!("Codex exceeded the live tool-call limit of {tool_call_limit}").into(),
            );
        }
        if output_limit_exceeded.load(Ordering::Acquire) {
            let _ = child.kill();
            let _ = child.wait();
            let _ = reader.join();
            return Err("Codex JSONL output exceeded 64 MiB".into());
        }
        let Some(remaining) = timeout.checked_sub(started.elapsed()) else {
            let _ = child.kill();
            let _ = child.wait();
            let _ = reader.join();
            return Err("Codex CLI timed out".into());
        };
        if let Some(status) = child.wait_timeout(remaining.min(CODEX_POLL_INTERVAL))? {
            break status;
        }
    };
    let output = reader
        .join()
        .map_err(|_| "Codex stdout reader panicked")??;
    if tool_limit_exceeded.load(Ordering::Acquire) {
        return Err(format!("Codex exceeded the live tool-call limit of {tool_call_limit}").into());
    }
    if output.len() as u64 > MAX_CODEX_STDOUT_BYTES {
        return Err("Codex JSONL output exceeded 64 MiB".into());
    }
    if !status.success() {
        return Err(format!("Codex CLI exited with {status}").into());
    }
    Ok(output)
}

fn is_tool_start_record(line: &[u8]) -> bool {
    let Ok(event) = serde_json::from_slice::<Value>(line) else {
        return false;
    };
    event["type"].as_str() == Some("item.started")
        && matches!(
            event.pointer("/item/type").and_then(Value::as_str),
            Some("command_execution" | "file_change" | "mcp_tool_call")
        )
}

fn analyze_events(output: &[u8], tokenizer: Tokenizer) -> Result<Analysis, Box<dyn Error>> {
    let text = std::str::from_utf8(output)?;
    let mut analysis = Analysis {
        events: Vec::new(),
        calls: Vec::new(),
        usage: None,
        usage_event: None,
        total_input_tokens: None,
        total_output_tokens: None,
        repository_generation: None,
        thread_id_hash: None,
        seen_ranges: HashSet::new(),
        native_retrieval_sequences: Vec::new(),
        pre_leantoken_substantive_sequences: Vec::new(),
        leantoken_tools: Vec::new(),
        verification_sequences: Vec::new(),
        phase_boundary: None,
        tokenizer,
    };
    for (index, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let event: Value = serde_json::from_str(line)
            .map_err(|error| format!("invalid Codex JSONL record {}: {error}", index + 1))?;
        analysis.ingest(&event)?;
        analysis.events.push(event);
    }
    Ok(analysis)
}

impl Analysis {
    fn ingest(&mut self, event: &Value) -> Result<(), Box<dyn Error>> {
        match event["type"].as_str() {
            Some("thread.started") => {
                let thread_id = required_str(event, "/thread_id")?;
                self.thread_id_hash = Some(blake3::hash(thread_id.as_bytes()).to_hex().to_string());
            }
            Some("turn.completed") => self.ingest_usage(event)?,
            Some("turn.failed") | Some("error") => {
                return Err("Codex JSONL stream reported a failed turn".into());
            }
            Some("item.completed") => self.ingest_item(event)?,
            Some("item.started") | Some("item.updated") | Some("turn.started") => {}
            Some(other) => return Err(format!("unsupported Codex JSONL event {other}").into()),
            None => return Err("Codex JSONL event has no type".into()),
        }
        Ok(())
    }

    fn ingest_usage(&mut self, event: &Value) -> Result<(), Box<dyn Error>> {
        if self.usage_event.is_some() {
            return Err("Codex JSONL stream had multiple completed turns".into());
        }
        let input = required_u64(event, "/usage/input_tokens")?;
        let cached = required_u64(event, "/usage/cached_input_tokens")?;
        let output = required_u64(event, "/usage/output_tokens")?;
        let reasoning = required_u64(event, "/usage/reasoning_output_tokens")?;
        if cached > input {
            return Err("Codex cached input exceeds total input".into());
        }
        self.total_input_tokens = Some(input);
        self.total_output_tokens = Some(output);
        self.usage_event = Some(event.clone());
        self.usage = Some(ProviderUsage {
            uncached_input_tokens: Some(input - cached),
            cache_creation_input_tokens: None,
            cache_read_input_tokens: Some(cached),
            output_tokens: Some(output),
            reasoning_tokens: Some(reasoning),
        });
        Ok(())
    }

    fn ingest_item(&mut self, event: &Value) -> Result<(), Box<dyn Error>> {
        let item = event.pointer("/item").ok_or("completed item is missing")?;
        let Some(item_type) = item["type"].as_str() else {
            return Err("completed item has no type".into());
        };
        match item_type {
            "command_execution" => self.record_command(item),
            "file_change" => self.record_file_change(item),
            "mcp_tool_call" => self.record_mcp(item),
            "agent_message" | "reasoning" | "todo_list" => Ok(()),
            "web_search" | "collab_tool_call" => {
                Err(format!("Codex used unavailable tool type {item_type}").into())
            }
            "error" => Err("Codex completed an error item".into()),
            other => Err(format!("unsupported completed Codex item {other}").into()),
        }
    }

    fn record_command(&mut self, item: &Value) -> Result<(), Box<dyn Error>> {
        let command = required_str(item, "/command")?;
        let sequence = self.calls.len();
        if is_native_retrieval_command(command) {
            self.native_retrieval_sequences.push(sequence);
        }
        if !self.has_leantoken_call() && !is_preflight_command(command) {
            self.pre_leantoken_substantive_sequences.push(sequence);
        }
        let output = item["aggregated_output"].as_str().unwrap_or_default();
        let status = required_str(item, "/status")?;
        let exit_code = item["exit_code"].as_i64();
        let outcome = if status == "completed" && exit_code == Some(0) {
            ToolOutcome::Success
        } else if exit_code == Some(1) && is_search_command(command) {
            ToolOutcome::FailedSearch
        } else {
            ToolOutcome::Error
        };
        if outcome == ToolOutcome::Success && is_validation_command(command) {
            self.verification_sequences.push(sequence);
        }
        self.push_call(
            item,
            "shell",
            outcome,
            self.tokenizer.count(output) as u64,
            Vec::new(),
        )
    }

    fn record_file_change(&mut self, item: &Value) -> Result<(), Box<dyn Error>> {
        if !self.has_leantoken_call() {
            self.pre_leantoken_substantive_sequences
                .push(self.calls.len());
        }
        let outcome = if required_str(item, "/status")? == "completed" {
            ToolOutcome::Success
        } else {
            ToolOutcome::Error
        };
        self.push_call(item, "edit", outcome, 0, Vec::new())
    }

    fn record_mcp(&mut self, item: &Value) -> Result<(), Box<dyn Error>> {
        if required_str(item, "/server")? != "leantoken" {
            return Err("Codex called an unexpected MCP server".into());
        }
        let tool = required_str(item, "/tool")?;
        self.leantoken_tools.push(tool.to_owned());
        let status = required_str(item, "/status")?;
        let outcome = if status == "completed" {
            ToolOutcome::Success
        } else if tool.contains("search") {
            ToolOutcome::FailedSearch
        } else {
            ToolOutcome::Error
        };
        let structured = item.pointer("/result/structured_content");
        let result_source_tokens = structured
            .and_then(|value| value.pointer("/meta/emitted_tokens"))
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let ranges = structured
            .map(|value| extract_ranges(value, self.tokenizer))
            .transpose()?
            .unwrap_or_default();
        if let Some(generation) = structured
            .and_then(|value| value.pointer("/meta/repository_generation"))
            .and_then(Value::as_u64)
        {
            self.repository_generation = Some(
                self.repository_generation
                    .map_or(generation, |current| current.max(generation)),
            );
        }
        self.push_call(item, "leantoken", outcome, result_source_tokens, ranges)
    }

    fn push_call(
        &mut self,
        item: &Value,
        tool_name: &str,
        outcome: ToolOutcome,
        result_source_tokens: u64,
        ranges: Vec<RangeIdentity>,
    ) -> Result<(), Box<dyn Error>> {
        let call_id = required_str(item, "/id")?.to_owned();
        let result_id = blake3::hash(&serde_json::to_vec(item)?)
            .to_hex()
            .to_string();
        let reread = ranges.iter().any(|range| {
            !self.seen_ranges.insert(RangeKey {
                repository_generation: range.repository_generation,
                path: range.path.clone(),
                start_line: range.start_line,
                end_line: range.end_line,
                content_hash: range.content_hash.clone(),
            })
        });
        self.calls.push(ToolCall {
            sequence: self.calls.len(),
            tool_name: tool_name.to_owned(),
            call_id,
            result_id,
            outcome,
            result_source_tokens,
            reread,
            ranges,
        });
        Ok(())
    }

    fn has_leantoken_call(&self) -> bool {
        self.calls.iter().any(|call| call.tool_name == "leantoken")
    }
}

fn extract_ranges(
    response: &Value,
    tokenizer: Tokenizer,
) -> Result<Vec<RangeIdentity>, Box<dyn Error>> {
    let generation = response
        .pointer("/meta/repository_generation")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let mut ranges = Vec::new();
    if let Some(fragments) = response["fragments"].as_array() {
        for fragment in fragments {
            ranges.push(range_from_content(
                fragment, generation, "content", tokenizer,
            )?);
        }
    } else if let Some(hits) = response["hits"].as_array() {
        for hit in hits {
            ranges.push(range_from_content(hit, generation, "excerpt", tokenizer)?);
        }
    } else if response["status"].as_str() == Some("content") && response["content"].is_string() {
        ranges.push(range_from_content(
            response, generation, "content", tokenizer,
        )?);
    }
    Ok(ranges)
}

fn range_from_content(
    value: &Value,
    repository_generation: u64,
    content_field: &str,
    tokenizer: Tokenizer,
) -> Result<RangeIdentity, Box<dyn Error>> {
    let content = value[content_field]
        .as_str()
        .ok_or("LeanToken range has no source content")?;
    Ok(RangeIdentity {
        repository_generation,
        path: required_str(value, "/path")?.to_owned(),
        start_line: required_usize(value, "/start_line")?,
        end_line: required_usize(value, "/end_line")?,
        content_hash: blake3::hash(content.as_bytes()).to_hex().to_string(),
        source_tokens: Some(tokenizer.count(content)),
    })
}

fn persist_artifacts(
    directory: &Path,
    binding: &RunBinding,
    analysis: &Analysis,
    usage: &ProviderUsage,
    configuration: &CodexConfiguration,
    request: &AdapterRequest,
) -> Result<(), Box<dyn Error>> {
    write_json(
        directory.join(TOOL_TRACE_FILE),
        &ToolTrace {
            schema_version: ARTIFACT_SCHEMA_V1,
            binding: binding.clone(),
            calls: analysis.calls.clone(),
        },
    )?;
    write_json(
        directory.join(TRAJECTORY_FILE),
        &Trajectory {
            schema_version: ARTIFACT_SCHEMA_V1,
            binding: binding.clone(),
            events: analysis.events.clone(),
        },
    )?;
    write_json(
        directory.join(PROVIDER_USAGE_FILE),
        &ProviderUsageReceipt {
            schema_version: ARTIFACT_SCHEMA_V1,
            binding: binding.clone(),
            usage: usage.clone(),
            raw_receipt: serde_json::json!({
                "host": "codex-cli",
                "host_version": configuration.codex_version,
                "primary_model": request.primary_model,
                "executor_model": request.executor_model,
                "turn_completed_event": analysis.usage_event,
                "phase_boundary": analysis.phase_boundary,
                "cache_creation_input_tokens_exposed": false,
            }),
        },
    )?;
    Ok(())
}

fn write_json(path: PathBuf, value: &impl Serialize) -> Result<(), Box<dyn Error>> {
    fs::write(path, serde_json::to_vec_pretty(value)?)?;
    Ok(())
}

fn verify_executable(path: &Path, expected: &str) -> Result<(), Box<dyn Error>> {
    let path = path.canonicalize()?;
    if blake3::hash(&fs::read(path)?).to_hex().as_str() != expected {
        return Err("frozen executable hash mismatch".into());
    }
    Ok(())
}

fn command_stdout(command: &mut Command) -> Result<String, Box<dyn Error>> {
    let output = command.output()?;
    if !output.status.success() {
        return Err(format!(
            "command failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )
        .into());
    }
    Ok(String::from_utf8(output.stdout)?)
}

fn git_stdout(repository: &Path, args: &[&str]) -> Result<String, Box<dyn Error>> {
    command_stdout(Command::new("git").arg("-C").arg(repository).args(args))
}

fn toml_string(value: &str) -> String {
    serde_json::to_string(value).expect("JSON string syntax is valid TOML string syntax")
}

fn required_str<'a>(value: &'a Value, pointer: &str) -> Result<&'a str, Box<dyn Error>> {
    value
        .pointer(pointer)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("missing string {pointer}").into())
}

fn required_u64(value: &Value, pointer: &str) -> Result<u64, Box<dyn Error>> {
    value
        .pointer(pointer)
        .and_then(Value::as_i64)
        .and_then(|number| u64::try_from(number).ok())
        .ok_or_else(|| format!("missing non-negative integer {pointer}").into())
}

fn required_usize(value: &Value, pointer: &str) -> Result<usize, Box<dyn Error>> {
    usize::try_from(required_u64(value, pointer)?).map_err(Into::into)
}

fn validate_retrieval_execution(
    policy: RetrievalPolicy,
    analysis: &Analysis,
) -> Result<RetrievalContractEvidence, Box<dyn Error>> {
    let leantoken_calls = analysis
        .calls
        .iter()
        .filter(|call| call.tool_name == "leantoken")
        .count();
    let successful_leantoken_calls = analysis
        .calls
        .iter()
        .filter(|call| call.tool_name == "leantoken" && call.outcome == ToolOutcome::Success)
        .count();
    let leantoken_evidence_calls = analysis
        .calls
        .iter()
        .filter(|call| {
            call.tool_name == "leantoken"
                && call.outcome == ToolOutcome::Success
                && (call.result_source_tokens > 0 || !call.ranges.is_empty())
        })
        .count();
    match policy {
        RetrievalPolicy::NativeOnly => {
            if leantoken_calls != 0 {
                return Err("native-only arm called LeanToken".into());
            }
        }
        RetrievalPolicy::LeanTokenProgressive => {
            validate_leantoken_first_and_no_native(analysis, None)?;
            if leantoken_evidence_calls == 0 {
                return Err("progressive arm received no successful LeanToken evidence".into());
            }
            if analysis
                .leantoken_tools
                .iter()
                .any(|tool| tool == "leantoken_context")
            {
                return Err("progressive arm called leantoken_context".into());
            }
        }
        RetrievalPolicy::LeanTokenOneShot => {
            validate_leantoken_first_and_no_native(analysis, None)?;
            if leantoken_calls != 1
                || leantoken_evidence_calls != 1
                || analysis.leantoken_tools.as_slice() != ["leantoken_context"]
            {
                return Err(
                    "one-shot arm must make one successful evidence-bearing context call".into(),
                );
            }
        }
        RetrievalPolicy::Prewalk => {
            let boundary = analysis
                .phase_boundary
                .ok_or("prewalk result has no executor phase boundary")?;
            validate_leantoken_first_and_no_native(analysis, Some(boundary))?;
            if boundary == 0 || boundary >= analysis.calls.len() || leantoken_evidence_calls == 0 {
                return Err("prewalk did not transfer evidence to an active executor phase".into());
            }
            if first_validated_edit(analysis).is_none() {
                return Err("prewalk result has no first validated edit".into());
            }
        }
    }
    Ok(RetrievalContractEvidence {
        policy,
        leantoken_calls,
        successful_leantoken_calls,
        leantoken_evidence_calls,
        leantoken_tools: analysis.leantoken_tools.clone(),
        phase_boundary: analysis.phase_boundary,
        native_retrieval_calls: analysis.native_retrieval_sequences.len(),
        pre_leantoken_substantive_calls: analysis.pre_leantoken_substantive_sequences.len(),
        first_validated_edit: first_validated_edit(analysis),
    })
}

fn validate_leantoken_first_and_no_native(
    analysis: &Analysis,
    prewalk_boundary: Option<usize>,
) -> Result<(), Box<dyn Error>> {
    if analysis.leantoken_tools.is_empty() {
        return Err("LeanToken arm completed without a LeanToken call".into());
    }
    if let Some(sequence) = analysis
        .pre_leantoken_substantive_sequences
        .iter()
        .copied()
        .find(|sequence| prewalk_boundary.is_none_or(|boundary| *sequence < boundary))
    {
        return Err(format!(
            "LeanToken arm made substantive tool call {sequence} before its first LeanToken call"
        )
        .into());
    }
    if let Some(sequence) = analysis.native_retrieval_sequences.first() {
        return Err(format!(
            "LeanToken arm used native repository retrieval at tool call {sequence}"
        )
        .into());
    }
    Ok(())
}

fn is_search_command(command: &str) -> bool {
    let words = command_words(command);
    command_executables(&words)
        .iter()
        .any(|word| matches!(word.as_str(), "rg" | "grep" | "find" | "fd" | "fdfind"))
        || has_word_pair(&words, "git", "grep")
}

fn is_native_retrieval_command(command: &str) -> bool {
    let words = command_words(command);
    command_executables(&words).iter().any(|word| {
        matches!(
            word.as_str(),
            "rg" | "grep"
                | "find"
                | "fd"
                | "fdfind"
                | "locate"
                | "ls"
                | "tree"
                | "eza"
                | "cat"
                | "bat"
                | "sed"
                | "head"
                | "tail"
                | "less"
                | "more"
                | "awk"
                | "nl"
                | "strings"
        )
    }) || ["show", "grep", "ls-files", "blame"]
        .iter()
        .any(|subcommand| has_word_pair(&words, "git", subcommand))
        || has_word_pair(&words, "git", "log") && words.iter().any(|word| word == "-p")
        || interpreter_eval(&words, "python", "-c")
        || interpreter_eval(&words, "python3", "-c")
        || interpreter_eval(&words, "node", "-e")
        || interpreter_eval(&words, "ruby", "-e")
        || interpreter_eval(&words, "php", "-r")
}

fn is_preflight_command(command: &str) -> bool {
    let words = command_words(command);
    match words.as_slice() {
        [command] if command == "pwd" => true,
        [git, subcommand, arguments @ ..] if git == "git" && subcommand == "status" => {
            arguments.iter().all(|argument| argument.starts_with('-'))
        }
        [git, subcommand, arguments @ ..] if git == "git" && subcommand == "rev-parse" => {
            !arguments.is_empty()
                && arguments
                    .iter()
                    .all(|argument| argument == "head" || argument.starts_with('-'))
        }
        _ => false,
    }
}

fn is_validation_command(command: &str) -> bool {
    let words = command_words(command);
    if has_word_pair(&words, "git", "diff") && words.iter().any(|word| word == "--check") {
        return true;
    }
    command_executables(&words).iter().any(|executable| {
        matches!(
            executable.as_str(),
            "cargo"
                | "go"
                | "make"
                | "cmake"
                | "ctest"
                | "npm"
                | "npx"
                | "yarn"
                | "pnpm"
                | "bun"
                | "pytest"
                | "tox"
                | "mvn"
                | "mvnw"
                | "gradle"
                | "gradlew"
                | "bundle"
                | "rspec"
        )
    })
}

fn command_words(command: &str) -> Vec<String> {
    let words = shell_words::split(command).unwrap_or_else(|_| {
        command
            .split_ascii_whitespace()
            .map(ToOwned::to_owned)
            .collect()
    });
    let words = shell_payload(&words).unwrap_or(words);
    words
        .into_iter()
        .map(|word| word.to_ascii_lowercase())
        .collect()
}

fn shell_payload(words: &[String]) -> Option<Vec<String>> {
    let shell = words
        .first()?
        .rsplit('/')
        .next()
        .unwrap_or(words.first()?.as_str());
    if !matches!(shell, "bash" | "sh" | "zsh" | "dash") {
        return None;
    }
    let command_index = words
        .iter()
        .position(|word| matches!(word.as_str(), "-c" | "-lc"))?;
    shell_words::split(words.get(command_index + 1)?).ok()
}

fn command_executables(words: &[String]) -> Vec<String> {
    let mut executables = Vec::new();
    let mut expect_command = true;
    for word in words {
        if matches!(word.as_str(), "|" | "||" | "&&" | ";") {
            expect_command = true;
            continue;
        }
        if !expect_command {
            continue;
        }
        if word.contains('=') && !word.starts_with('=') {
            continue;
        }
        let executable = word.rsplit('/').next().unwrap_or(word);
        if matches!(executable, "env" | "command" | "sudo") {
            continue;
        }
        executables.push(executable.to_owned());
        expect_command = false;
    }
    executables
}

fn has_word_pair(words: &[String], first: &str, second: &str) -> bool {
    words
        .windows(2)
        .any(|pair| pair[0] == first && pair[1] == second)
}

fn interpreter_eval(words: &[String], interpreter: &str, flag: &str) -> bool {
    words.iter().enumerate().any(|(index, word)| {
        word == interpreter
            && words
                .get(index + 1..)
                .is_some_and(|arguments| arguments.iter().any(|argument| argument == flag))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn command_event(id: &str, command: &str) -> Value {
        serde_json::json!({
            "type":"item.completed",
            "item": {
                "id":id,
                "type":"command_execution",
                "command":command,
                "aggregated_output":"",
                "exit_code":0,
                "status":"completed"
            }
        })
    }

    fn edit_event(id: &str) -> Value {
        serde_json::json!({
            "type":"item.completed",
            "item": {"id":id, "type":"file_change", "status":"completed"}
        })
    }

    fn usage_event(input: u64, cached: u64, output: u64) -> Value {
        serde_json::json!({
            "type":"turn.completed",
            "usage": {
                "input_tokens": input,
                "cached_input_tokens": cached,
                "output_tokens": output,
                "reasoning_output_tokens": 2
            }
        })
    }

    fn leantoken_event(id: &str, tool: &str, evidence: bool) -> Value {
        let result = if evidence {
            serde_json::json!({"structured_content": {
                "hits": [{
                    "path": "src/lib.rs",
                    "start_line": 1,
                    "end_line": 1,
                    "excerpt": "fn answer() {}"
                }],
                "meta": {"repository_generation": 1, "emitted_tokens": 4}
            }})
        } else {
            serde_json::json!({})
        };
        serde_json::json!({
            "type":"item.completed",
            "item": {
                "id":id,
                "type":"mcp_tool_call",
                "server":"leantoken",
                "tool":tool,
                "status":"completed",
                "result":result
            }
        })
    }

    fn analyze(values: &[Value]) -> Analysis {
        let jsonl = values
            .iter()
            .map(Value::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        analyze_events(jsonl.as_bytes(), Tokenizer::O200kBase).unwrap()
    }

    #[test]
    fn live_budget_counts_only_started_tool_items() {
        for item_type in ["command_execution", "file_change", "mcp_tool_call"] {
            let event = serde_json::json!({
                "type": "item.started",
                "item": {"id": "item-1", "type": item_type}
            });
            assert!(is_tool_start_record(event.to_string().as_bytes()));
        }
        for event in [
            serde_json::json!({
                "type": "item.completed",
                "item": {"id": "item-1", "type": "command_execution"}
            }),
            serde_json::json!({
                "type": "item.started",
                "item": {"id": "item-1", "type": "agent_message"}
            }),
        ] {
            assert!(!is_tool_start_record(event.to_string().as_bytes()));
        }
        assert!(!is_tool_start_record(b"not-json\n"));
    }

    #[test]
    fn codex_events_reconstruct_usage_ranges_and_rereads() {
        let content = "fn answer() {}\n";
        let context_item = |id: &str| {
            serde_json::json!({
                "type": "item.completed",
                "item": {
                    "id": id,
                    "type": "mcp_tool_call",
                    "server": "leantoken",
                    "tool": "leantoken_context",
                    "status": "completed",
                    "result": {"structured_content": {
                        "fragments": [{
                            "path": "src/lib.rs",
                            "start_line": 1,
                            "end_line": 1,
                            "content": content
                        }],
                        "meta": {
                            "repository_generation": 7,
                            "emitted_tokens": 5
                        }
                    }}
                }
            })
        };
        let events = vec![
            serde_json::json!({"type":"thread.started","thread_id":"secret-id"}),
            context_item("call-1"),
            context_item("call-2"),
            serde_json::json!({
                "type":"turn.completed",
                "usage": {
                    "input_tokens": 100,
                    "cached_input_tokens": 30,
                    "output_tokens": 20,
                    "reasoning_output_tokens": 4
                }
            }),
        ];
        let jsonl = events
            .iter()
            .map(Value::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        let analysis = analyze_events(jsonl.as_bytes(), Tokenizer::O200kBase).unwrap();

        assert_eq!(analysis.calls.len(), 2);
        assert!(!analysis.calls[0].reread);
        assert!(analysis.calls[1].reread);
        assert_eq!(analysis.calls[0].ranges[0].repository_generation, 7);
        assert_eq!(analysis.calls[0].ranges[0].content_hash.len(), 64);
        assert_eq!(analysis.total_input_tokens, Some(100));
        assert_eq!(analysis.usage.unwrap().uncached_input_tokens, Some(70));
    }

    #[test]
    fn nonzero_search_exit_is_a_failed_search() {
        let event = serde_json::json!({
            "type":"item.completed",
            "item": {
                "id":"command-1",
                "type":"command_execution",
                "command":"rg missing src",
                "aggregated_output":"",
                "exit_code":1,
                "status":"failed"
            }
        });
        let analysis = analyze_events(event.to_string().as_bytes(), Tokenizer::O200kBase).unwrap();
        assert_eq!(analysis.calls[0].outcome, ToolOutcome::FailedSearch);
    }

    #[test]
    fn progressive_contract_rejects_native_source_reads() {
        let analysis = analyze(&[
            command_event("preflight", "/bin/bash -lc 'git status --short'"),
            leantoken_event("search", "leantoken_search", true),
            command_event("read", "/bin/bash -lc \"sed -n '1,80p' src/lib.rs\""),
        ]);

        let error = validate_retrieval_execution(RetrievalPolicy::LeanTokenProgressive, &analysis)
            .unwrap_err()
            .to_string();
        assert!(error.contains("native repository retrieval"));
        assert_eq!(analysis.native_retrieval_sequences, vec![2]);
        assert!(analysis.pre_leantoken_substantive_sequences.is_empty());
    }

    #[test]
    fn one_shot_contract_requires_one_context_call_before_substantive_tools() {
        let invalid = analyze(&[
            command_event("search", "/bin/bash -lc 'rg --files'"),
            leantoken_event("context", "leantoken_context", true),
        ]);
        let error = validate_retrieval_execution(RetrievalPolicy::LeanTokenOneShot, &invalid)
            .unwrap_err()
            .to_string();
        assert!(error.contains("before its first LeanToken call"));

        let valid = analyze(&[
            leantoken_event("context", "leantoken_context", true),
            command_event("test", "/bin/bash -lc 'cargo test'"),
        ]);
        let evidence = validate_retrieval_execution(RetrievalPolicy::LeanTokenOneShot, &valid)
            .expect("valid one-shot trace");
        assert_eq!(evidence.leantoken_calls, 1);
        assert_eq!(evidence.native_retrieval_calls, 0);
    }

    #[test]
    fn progressive_contract_rejects_context_and_requires_successful_evidence() {
        let context = analyze(&[leantoken_event("context", "leantoken_context", true)]);
        let error = validate_retrieval_execution(RetrievalPolicy::LeanTokenProgressive, &context)
            .unwrap_err()
            .to_string();
        assert!(error.contains("leantoken_context"));

        let empty = analyze(&[leantoken_event("empty", "leantoken_search", false)]);
        let error = validate_retrieval_execution(RetrievalPolicy::LeanTokenProgressive, &empty)
            .unwrap_err()
            .to_string();
        assert!(error.contains("no successful LeanToken evidence"));
    }

    #[test]
    fn command_classifier_separates_retrieval_from_verification() {
        assert!(is_native_retrieval_command(
            "/bin/bash -lc 'cd crate && rg --files | head'"
        ));
        assert!(is_native_retrieval_command(
            "/bin/bash -lc \"python3 -c 'print(open(\\\"src/lib.rs\\\").read())'\""
        ));
        assert!(!is_native_retrieval_command(
            "/bin/bash -lc 'cargo test --test head'"
        ));
        assert!(is_validation_command(
            "/bin/bash -lc 'cargo test --test head'"
        ));
        assert!(is_preflight_command("/bin/bash -lc 'git rev-parse HEAD'"));
        assert!(!is_preflight_command(
            "/bin/bash -lc 'git status --short && cargo test'"
        ));
    }

    #[test]
    fn prewalk_merge_retains_phase_boundary_usage_and_validated_edit() {
        let mut prewalk = analyze(&[
            leantoken_event("search", "leantoken_search", true),
            serde_json::json!({
                "type":"item.completed",
                "item": {"id":"todo", "type":"todo_list", "items":[]}
            }),
            edit_event("first-edit"),
            command_event("focused-test", "/bin/bash -lc 'cargo test --test focused'"),
            usage_event(100, 30, 10),
        ]);
        let executor = analyze(&[
            edit_event("executor-edit"),
            command_event("full-test", "/bin/bash -lc 'cargo test'"),
            usage_event(120, 40, 12),
        ]);

        merge_phase_analyses(&mut prewalk, executor).expect("merge phases");
        let evidence = validate_retrieval_execution(RetrievalPolicy::Prewalk, &prewalk)
            .expect("valid prewalk trace");

        assert_eq!(evidence.phase_boundary, Some(3));
        assert_eq!(evidence.first_validated_edit.unwrap().edit_sequence, 1);
        assert_eq!(prewalk.total_input_tokens, Some(220));
        assert_eq!(prewalk.total_output_tokens, Some(22));
        assert_eq!(
            prewalk.usage.as_ref().unwrap().uncached_input_tokens,
            Some(150)
        );
        assert!(prewalk.calls[0].call_id.starts_with("prewalk:"));
        assert!(prewalk.calls[3].call_id.starts_with("executor:"));
    }
}
