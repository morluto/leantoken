#[path = "support/model_ab_artifacts.rs"]
mod model_ab_artifacts;

use std::collections::HashSet;
use std::error::Error;
use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::Duration;

use leantoken::tokens::Tokenizer;
use model_ab_artifacts::{
    ARTIFACT_SCHEMA_V1, PROVIDER_USAGE_FILE, ProviderUsage, ProviderUsageReceipt, RangeIdentity,
    RunBinding, TOOL_TRACE_FILE, TRAJECTORY_FILE, ToolCall, ToolOutcome, ToolTrace, Trajectory,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use wait_timeout::ChildExt;

const MAX_CODEX_STDOUT_BYTES: u64 = 64 * 1024 * 1024;

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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum RetrievalPolicy {
    NativeOnly,
    LeanTokenOnly,
    LeanTokenThenNativeRecovery,
}

#[derive(Debug, Serialize)]
struct RetrievalContractEvidence {
    policy: RetrievalPolicy,
    leantoken_calls: usize,
    successful_leantoken_calls: usize,
    leantoken_evidence_calls: usize,
    native_retrieval_calls: usize,
    pre_leantoken_substantive_calls: usize,
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
    if request.schema_version != 3 {
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

    let prompt = build_prompt(&request, retrieval_policy);
    let output = run_codex(&request, &configuration, &prompt)?;
    let mut analysis = analyze_events(&output, configuration.tokenizer)?;
    let binding = RunBinding {
        experiment_id: request.experiment_id.clone(),
        manifest_blake3: request.manifest_blake3.clone(),
        task_id: request.task_id.clone(),
        repetition: request.repetition,
        arm: request.arm.clone(),
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
        schema_version: 3,
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
        ("lean_token_baseline", "baseline") | ("lean_token_adaptive", "adaptive") => {
            RetrievalPolicy::LeanTokenOnly
        }
        ("lean_token_adaptive_recovery", "adaptive_discovery_native_recovery") => {
            RetrievalPolicy::LeanTokenThenNativeRecovery
        }
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
        RetrievalPolicy::LeanTokenOnly => {
            has("shell")
                && has("edit")
                && has("leantoken")
                && native_catalog.iter().all(|tool| !has(tool))
        }
        RetrievalPolicy::LeanTokenThenNativeRecovery => {
            has("shell")
                && has("edit")
                && has("leantoken")
                && native_catalog.iter().all(|tool| has(tool))
        }
    };
    if !catalog_matches {
        return Err("arm tool catalog does not match the Codex adapter".into());
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
        RetrievalPolicy::LeanTokenOnly => {
            "LeanToken is available as the `leantoken` MCP server and is the only permitted repository discovery and source-reading tool. Call LeanToken before any substantive shell command or edit. Native shell commands remain available only for Git preflight and post-retrieval build, test, lint, and patch verification."
        }
        RetrievalPolicy::LeanTokenThenNativeRecovery => {
            "LeanToken is available as the `leantoken` MCP server. Call LeanToken before any substantive shell command or edit. Native repository discovery and source reads are permitted only after that initial LeanToken call."
        }
    };
    format!(
        "Solve the repository task below by inspecting and editing the current worktree. Run relevant tests before finishing. Do not use web search or network access, do not inspect benchmark gold patches, and do not merely describe a solution.\n\nFrozen retrieval contract: {}\n{}\nPer-call retrieval source budget: {} tokens. Total tool-call limit: {}.\n\nTask:\n{}",
        request.arm_definition.retrieval_contract,
        tools,
        request.arm_definition.budget.context_token_limit,
        request.arm_definition.budget.tool_call_limit,
        request.prompt
    )
}

fn run_codex(
    request: &AdapterRequest,
    configuration: &CodexConfiguration,
    prompt: &str,
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
        .arg(&request.primary_model)
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
        ));
    if configuration.mcp_enabled {
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
    let reader = thread::spawn(move || -> io::Result<Vec<u8>> {
        let mut output = Vec::new();
        stdout
            .take(MAX_CODEX_STDOUT_BYTES + 1)
            .read_to_end(&mut output)?;
        Ok(output)
    });
    let timeout = Duration::from_secs(request.timeout_seconds.saturating_sub(5));
    let status = match child.wait_timeout(timeout)? {
        Some(status) => status,
        None => {
            child.kill()?;
            let _ = child.wait();
            return Err("Codex CLI timed out".into());
        }
    };
    let output = reader
        .join()
        .map_err(|_| "Codex stdout reader panicked")??;
    if output.len() as u64 > MAX_CODEX_STDOUT_BYTES {
        return Err("Codex JSONL output exceeded 64 MiB".into());
    }
    if !status.success() {
        return Err(format!("Codex CLI exited with {status}").into());
    }
    Ok(output)
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
                "model": request.primary_model,
                "turn_completed_event": analysis.usage_event,
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
    if policy == RetrievalPolicy::NativeOnly {
        if leantoken_calls != 0 {
            return Err("native-only arm called LeanToken".into());
        }
    } else {
        if leantoken_calls == 0 {
            return Err("LeanToken arm completed without a LeanToken call".into());
        }
        if let Some(sequence) = analysis.pre_leantoken_substantive_sequences.first() {
            return Err(format!(
                "LeanToken arm made substantive tool call {sequence} before its first LeanToken call"
            )
            .into());
        }
    }
    if policy == RetrievalPolicy::LeanTokenOnly
        && let Some(sequence) = analysis.native_retrieval_sequences.first()
    {
        return Err(format!(
            "LeanToken-only arm used native repository retrieval at tool call {sequence}"
        )
        .into());
    }
    if policy == RetrievalPolicy::LeanTokenOnly && leantoken_evidence_calls == 0 {
        return Err("LeanToken-only arm received no successful LeanToken evidence".into());
    }
    Ok(RetrievalContractEvidence {
        policy,
        leantoken_calls,
        successful_leantoken_calls,
        leantoken_evidence_calls,
        native_retrieval_calls: analysis.native_retrieval_sequences.len(),
        pre_leantoken_substantive_calls: analysis.pre_leantoken_substantive_sequences.len(),
    })
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

    fn leantoken_event(id: &str) -> Value {
        serde_json::json!({
            "type":"item.completed",
            "item": {
                "id":id,
                "type":"mcp_tool_call",
                "server":"leantoken",
                "tool":"leantoken_context",
                "status":"completed",
                "result":{}
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
    fn leantoken_only_contract_rejects_native_source_reads() {
        let analysis = analyze(&[
            command_event("preflight", "/bin/bash -lc 'git status --short'"),
            leantoken_event("context"),
            command_event("read", "/bin/bash -lc \"sed -n '1,80p' src/lib.rs\""),
        ]);

        let error = validate_retrieval_execution(RetrievalPolicy::LeanTokenOnly, &analysis)
            .unwrap_err()
            .to_string();
        assert!(error.contains("native repository retrieval"));
        assert_eq!(analysis.native_retrieval_sequences, vec![2]);
        assert!(analysis.pre_leantoken_substantive_sequences.is_empty());
    }

    #[test]
    fn recovery_contract_requires_leantoken_before_substantive_tools() {
        let invalid = analyze(&[
            command_event("search", "/bin/bash -lc 'rg --files'"),
            leantoken_event("context"),
        ]);
        let error =
            validate_retrieval_execution(RetrievalPolicy::LeanTokenThenNativeRecovery, &invalid)
                .unwrap_err()
                .to_string();
        assert!(error.contains("before its first LeanToken call"));

        let valid = analyze(&[
            leantoken_event("context"),
            command_event("search", "/bin/bash -lc 'rg --files'"),
            command_event("test", "/bin/bash -lc 'cargo test'"),
        ]);
        let evidence =
            validate_retrieval_execution(RetrievalPolicy::LeanTokenThenNativeRecovery, &valid)
                .unwrap();
        assert_eq!(evidence.leantoken_calls, 1);
        assert_eq!(evidence.native_retrieval_calls, 1);
    }

    #[test]
    fn leantoken_only_contract_requires_successful_evidence() {
        let analysis = analyze(&[leantoken_event("empty")]);
        let error = validate_retrieval_execution(RetrievalPolicy::LeanTokenOnly, &analysis)
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
        assert!(is_preflight_command("/bin/bash -lc 'git rev-parse HEAD'"));
        assert!(!is_preflight_command(
            "/bin/bash -lc 'git status --short && cargo test'"
        ));
    }
}
