#[path = "support/model_ab_artifacts.rs"]
mod model_ab_artifacts;

use std::collections::{BTreeMap, HashSet};
use std::error::Error;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use clap::Parser;
use model_ab_artifacts::{
    ARTIFACT_SCHEMA_V1, PROVIDER_USAGE_FILE, ProviderUsage, ProviderUsageReceipt, RunBinding,
    TOOL_TRACE_FILE, TRAJECTORY_FILE, ToolOutcome, ToolTrace, Trajectory,
};
use serde::{Deserialize, Serialize};
use statrs::statistics::Statistics;
use wait_timeout::ChildExt;

const PATCH_FILE: &str = "patch.diff";
const VALIDATION_RECEIPT_FILE: &str = "validation-receipt.json";

#[derive(Debug, Parser)]
#[command(about = "Run controlled model-in-the-loop retrieval A/B experiments")]
struct Args {
    /// Frozen experiment manifest.
    #[arg(long)]
    manifest: PathBuf,
    /// Adapter executable that accepts one JSON request on stdin and returns one JSON result.
    #[arg(long)]
    adapter: PathBuf,
    /// Argument passed to the adapter (repeatable).
    #[arg(long = "adapter-arg")]
    adapter_args: Vec<String>,
    /// Repetitions per task and arm.
    #[arg(long, default_value_t = 1)]
    repetitions: usize,
    /// JSON report path.
    #[arg(long, default_value = "target/model_ab_report.json")]
    output: PathBuf,
    /// Root directory for immutable per-run trace, trajectory, usage, and patch artifacts.
    #[arg(long, default_value = "target/model_ab_artifacts")]
    artifacts_dir: PathBuf,
}

#[derive(Debug, Deserialize)]
struct Manifest {
    schema_version: u32,
    experiment_id: String,
    random_seed: u64,
    primary_model: String,
    executor_model: String,
    timeout_seconds: u64,
    arms: BTreeMap<Arm, ArmDefinition>,
    tasks: Vec<Task>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Task {
    id: String,
    repository: PathBuf,
    revision: String,
    prompt: String,
    success_command: Vec<String>,
    #[serde(default)]
    success_command_executable_blake3: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
enum Arm {
    Filesystem,
    LeanTokenBaseline,
    LeanTokenAdaptive,
    LeanTokenAdaptiveRecovery,
    Prewalk,
}

impl Arm {
    const REQUIRED: [Self; 4] = [
        Self::Filesystem,
        Self::LeanTokenBaseline,
        Self::LeanTokenAdaptive,
        Self::LeanTokenAdaptiveRecovery,
    ];

    fn as_str(self) -> &'static str {
        match self {
            Self::Filesystem => "filesystem",
            Self::LeanTokenBaseline => "lean_token_baseline",
            Self::LeanTokenAdaptive => "lean_token_adaptive",
            Self::LeanTokenAdaptiveRecovery => "lean_token_adaptive_recovery",
            Self::Prewalk => "prewalk",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ArmDefinition {
    runtime_repository: PathBuf,
    runtime_revision: String,
    runtime_binary: PathBuf,
    runtime_binary_blake3: String,
    adapter_repository: PathBuf,
    adapter_revision: String,
    adapter_binary_blake3: String,
    configuration: serde_json::Value,
    tool_catalog: Vec<String>,
    budget: ArmBudget,
    retrieval_contract: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ArmBudget {
    tool_call_limit: usize,
    context_token_limit: usize,
}

#[derive(Debug, Serialize)]
struct AdapterRequest<'a> {
    schema_version: u32,
    experiment_id: &'a str,
    manifest_blake3: &'a str,
    random_seed: u64,
    repetition: usize,
    arm_order_index: usize,
    arm: Arm,
    primary_model: &'a str,
    executor_model: Option<&'a str>,
    arm_definition: &'a ArmDefinition,
    repository: &'a Path,
    revision: &'a str,
    task_id: &'a str,
    prompt: &'a str,
    success_command: &'a [String],
    artifacts_directory: &'a Path,
    timeout_seconds: u64,
}

#[derive(Debug, Deserialize, Serialize)]
struct AdapterResult {
    schema_version: u32,
    task_success: bool,
    total_input_tokens: Option<u64>,
    #[serde(default)]
    total_output_tokens: Option<u64>,
    #[serde(default)]
    provider_reported_cost_usd: Option<f64>,
    tool_calls: usize,
    rereads: usize,
    #[serde(default)]
    reread_tokens: u64,
    failed_tool_calls: usize,
    failed_searches: usize,
    #[serde(default)]
    dead_end_reads: usize,
    #[serde(default)]
    provider_usage: ProviderUsage,
    #[serde(default)]
    evidence_receipt: Option<serde_json::Value>,
    #[serde(default)]
    repository_generation: Option<u64>,
}

#[derive(Debug, Serialize)]
struct RunReport {
    task_id: String,
    repetition: usize,
    arm_order_index: usize,
    arm: Arm,
    primary_model: String,
    executor_model: Option<String>,
    duration_ms: u128,
    status: RunStatus,
    validation_duration_ms: Option<u128>,
    validation_exit_code: Option<i32>,
    agent_reported_success: Option<bool>,
    error: Option<String>,
    artifacts: RunArtifacts,
    result: Option<AdapterResult>,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
enum RunStatus {
    Completed,
    AdapterFailed,
    AdapterTimedOut,
    ValidationFailed,
    ValidationTimedOut,
}

#[derive(Debug, Default, Serialize)]
struct ArmAggregate {
    runs: usize,
    adapter_completed: usize,
    run_errors: usize,
    successes: usize,
    success_rate: f64,
    total_input_tokens: Option<u64>,
    total_output_tokens: Option<u64>,
    provider_reported_cost_usd: Option<f64>,
    input_tokens_per_success: Option<f64>,
    provider_reported_cost_per_success_usd: Option<f64>,
    total_duration_ms: u128,
    tool_calls: usize,
    rereads: usize,
    reread_tokens: u64,
    failed_tool_calls: usize,
    failed_searches: usize,
    dead_end_reads: usize,
    input_tokens: SampleStatistics,
    output_tokens: SampleStatistics,
    provider_cost_usd: SampleStatistics,
    duration_ms: SampleStatistics,
    successful_input_tokens: SampleStatistics,
    successful_duration_ms: SampleStatistics,
}

#[derive(Debug, Default, Serialize)]
struct SampleStatistics {
    samples: usize,
    minimum: Option<f64>,
    median: Option<f64>,
    mean: Option<f64>,
    maximum: Option<f64>,
    sample_variance: Option<f64>,
}

#[derive(Debug, Serialize)]
struct Report {
    schema_version: u32,
    experiment_id: String,
    manifest_blake3: String,
    random_seed: u64,
    generated_at_unix_seconds: u64,
    harness_revision: String,
    harness_worktree_dirty: bool,
    harness_binary_blake3: String,
    adapter_binary_blake3: String,
    primary_model: String,
    executor_model: String,
    repetitions: usize,
    arm_definitions: BTreeMap<Arm, ArmDefinition>,
    task_definitions: Vec<Task>,
    schedules: Vec<RunSchedule>,
    arms: BTreeMap<Arm, ArmAggregate>,
    task_arms: BTreeMap<String, BTreeMap<Arm, ArmAggregate>>,
    runs: Vec<RunReport>,
    limitations: Vec<&'static str>,
}

#[derive(Debug, Serialize)]
struct RunSchedule {
    task_id: String,
    repetition: usize,
    arms: Vec<Arm>,
}

#[derive(Debug, Serialize)]
struct RunArtifacts {
    directory: PathBuf,
    tool_trace: Option<ArtifactIdentity>,
    trajectory: Option<ArtifactIdentity>,
    provider_usage: Option<ArtifactIdentity>,
    validation_receipt: Option<ArtifactIdentity>,
    patch: ArtifactIdentity,
    patch_valid: bool,
    tool_call_records: Option<usize>,
    range_identities: Option<usize>,
    trajectory_events: Option<usize>,
}

#[derive(Debug, Serialize)]
struct ArtifactIdentity {
    path: PathBuf,
    bytes: u64,
    blake3: String,
}

struct SourceIdentity {
    revision: String,
    dirty: bool,
}

struct ValidationContext<'a> {
    artifacts_directory: &'a Path,
    experiment_id: &'a str,
    manifest_blake3: &'a str,
    task_id: &'a str,
    repetition: usize,
    arm: Arm,
}

#[derive(Debug, Deserialize)]
struct ValidationReceiptBinding {
    schema_version: u32,
    experiment_id: String,
    manifest_blake3: String,
    task_id: String,
    repetition: usize,
    arm: String,
}

fn main() -> Result<(), Box<dyn Error>> {
    let args = Args::parse();
    if args.repetitions == 0 {
        return Err("repetitions must be positive".into());
    }
    let manifest_json = fs::read_to_string(&args.manifest)?;
    let manifest_blake3 = blake3::hash(manifest_json.as_bytes()).to_hex().to_string();
    let manifest: Manifest = serde_json::from_str(&manifest_json)?;
    validate_manifest(&manifest)?;
    let harness_root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let harness_identity = source_identity(harness_root)?;
    if harness_identity.dirty {
        return Err("model A/B harness worktree must be clean".into());
    }
    let harness_binary_blake3 = hash_file(&std::env::current_exe()?)?;
    let adapter_binary_blake3 = hash_file(&args.adapter)?;
    let arm_definitions =
        prepare_arm_definitions(&manifest.arms, &args.adapter, &adapter_binary_blake3)?;
    fs::create_dir_all(&args.artifacts_dir)?;
    let artifacts_root = args.artifacts_dir.canonicalize()?;

    let mut runs = Vec::new();
    let mut schedules = Vec::new();
    for repetition in 1..=args.repetitions {
        for task in &manifest.tasks {
            verify_clean_revision(&task.repository, &task.revision)?;
            verify_success_command_identity(task, manifest.schema_version)?;
            let arm_order = seeded_arm_order(
                manifest.random_seed,
                &task.id,
                repetition,
                arm_definitions.keys().copied(),
            );
            schedules.push(RunSchedule {
                task_id: task.id.clone(),
                repetition,
                arms: arm_order.clone(),
            });
            for (arm_order_index, arm) in arm_order.into_iter().enumerate() {
                let arm_definition = arm_definitions.get(&arm).expect("scheduled arm definition");
                let workspace = IsolatedWorkspace::create(&task.repository, &task.revision)?;
                let binding = RunBinding {
                    experiment_id: manifest.experiment_id.clone(),
                    manifest_blake3: manifest_blake3.clone(),
                    task_id: task.id.clone(),
                    repetition,
                    arm: arm.as_str().to_owned(),
                };
                let artifacts_directory = create_run_artifact_directory(
                    &artifacts_root,
                    &manifest.experiment_id,
                    &task.id,
                    repetition,
                    arm,
                )?;
                let request = AdapterRequest {
                    schema_version: 3,
                    experiment_id: &manifest.experiment_id,
                    manifest_blake3: &manifest_blake3,
                    random_seed: manifest.random_seed,
                    repetition,
                    arm_order_index,
                    arm,
                    primary_model: &manifest.primary_model,
                    executor_model: (arm == Arm::Prewalk)
                        .then_some(manifest.executor_model.as_str()),
                    arm_definition,
                    repository: workspace.path(),
                    revision: &task.revision,
                    task_id: &task.id,
                    prompt: &task.prompt,
                    success_command: &task.success_command,
                    artifacts_directory: &artifacts_directory,
                    timeout_seconds: manifest.timeout_seconds,
                };
                let started = Instant::now();
                let invocation = invoke_adapter(
                    &args.adapter,
                    &args.adapter_args,
                    &request,
                    Duration::from_secs(manifest.timeout_seconds),
                );
                let mut artifacts =
                    capture_run_artifacts(workspace.path(), &task.revision, &artifacts_directory)?;
                let mut result = match invocation {
                    Ok(result) => result,
                    Err(failure) => {
                        runs.push(RunReport {
                            task_id: task.id.clone(),
                            repetition,
                            arm_order_index,
                            arm,
                            primary_model: manifest.primary_model.clone(),
                            executor_model: (arm == Arm::Prewalk)
                                .then(|| manifest.executor_model.clone()),
                            duration_ms: started.elapsed().as_millis(),
                            status: match failure.kind {
                                AdapterFailureKind::Failed => RunStatus::AdapterFailed,
                                AdapterFailureKind::TimedOut => RunStatus::AdapterTimedOut,
                            },
                            validation_duration_ms: None,
                            validation_exit_code: None,
                            agent_reported_success: None,
                            error: Some(failure.message),
                            artifacts,
                            result: None,
                        });
                        continue;
                    }
                };
                if let Err(error) = validate_run_artifacts(
                    &artifacts_directory,
                    &binding,
                    arm_definition,
                    &result,
                    &mut artifacts,
                ) {
                    runs.push(RunReport {
                        task_id: task.id.clone(),
                        repetition,
                        arm_order_index,
                        arm,
                        primary_model: manifest.primary_model.clone(),
                        executor_model: (arm == Arm::Prewalk)
                            .then(|| manifest.executor_model.clone()),
                        duration_ms: started.elapsed().as_millis(),
                        status: RunStatus::AdapterFailed,
                        validation_duration_ms: None,
                        validation_exit_code: None,
                        agent_reported_success: Some(result.task_success),
                        error: Some(format!("adapter artifact validation failed: {error}")),
                        artifacts,
                        result: Some(result),
                    });
                    continue;
                }
                if result.tool_calls > arm_definition.budget.tool_call_limit {
                    runs.push(RunReport {
                        task_id: task.id.clone(),
                        repetition,
                        arm_order_index,
                        arm,
                        primary_model: manifest.primary_model.clone(),
                        executor_model: (arm == Arm::Prewalk)
                            .then(|| manifest.executor_model.clone()),
                        duration_ms: started.elapsed().as_millis(),
                        status: RunStatus::AdapterFailed,
                        validation_duration_ms: None,
                        validation_exit_code: None,
                        agent_reported_success: Some(result.task_success),
                        error: Some(format!(
                            "adapter reported {} tool calls, exceeding the limit of {}",
                            result.tool_calls, arm_definition.budget.tool_call_limit
                        )),
                        artifacts,
                        result: Some(result),
                    });
                    continue;
                }
                let agent_reported_success = result.task_success;
                let validation_started = Instant::now();
                let validation_context = ValidationContext {
                    artifacts_directory: &artifacts_directory,
                    experiment_id: &manifest.experiment_id,
                    manifest_blake3: &manifest_blake3,
                    task_id: &task.id,
                    repetition,
                    arm,
                };
                verify_success_command_identity(task, manifest.schema_version)?;
                let validation = run_validation(
                    workspace.path(),
                    &task.success_command,
                    &validation_context,
                    Duration::from_secs(manifest.timeout_seconds),
                );
                let validation_duration_ms = validation_started.elapsed().as_millis();
                artifacts.validation_receipt =
                    artifact_identity_if_present(&artifacts_directory, VALIDATION_RECEIPT_FILE)?;
                let receipt_error = if manifest.schema_version >= 3 {
                    validate_validation_receipt(&artifacts_directory, &validation_context)
                        .err()
                        .map(|error| error.to_string())
                } else {
                    None
                };
                let (status, validation_exit_code, error) = match validation {
                    Ok(None) => (
                        RunStatus::ValidationTimedOut,
                        None,
                        Some("success command timed out".to_owned()),
                    ),
                    Err(error) => (RunStatus::ValidationFailed, None, Some(error.to_string())),
                    Ok(Some(_)) if receipt_error.is_some() => {
                        (RunStatus::ValidationFailed, None, receipt_error)
                    }
                    Ok(Some(exit_code)) => (RunStatus::Completed, Some(exit_code), None),
                };
                result.task_success =
                    matches!(status, RunStatus::Completed) && validation_exit_code == Some(0);
                runs.push(RunReport {
                    task_id: task.id.clone(),
                    repetition,
                    arm_order_index,
                    arm,
                    primary_model: manifest.primary_model.clone(),
                    executor_model: (arm == Arm::Prewalk).then(|| manifest.executor_model.clone()),
                    duration_ms: started.elapsed().as_millis(),
                    status,
                    validation_duration_ms: Some(validation_duration_ms),
                    validation_exit_code,
                    agent_reported_success: Some(agent_reported_success),
                    error,
                    artifacts,
                    result: Some(result),
                });
            }
        }
    }
    prepare_arm_definitions(&arm_definitions, &args.adapter, &adapter_binary_blake3)?;
    for task in &manifest.tasks {
        verify_success_command_identity(task, manifest.schema_version)?;
    }

    let report = Report {
        schema_version: 5,
        experiment_id: manifest.experiment_id,
        manifest_blake3,
        random_seed: manifest.random_seed,
        generated_at_unix_seconds: SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs(),
        harness_revision: harness_identity.revision,
        harness_worktree_dirty: harness_identity.dirty,
        harness_binary_blake3,
        adapter_binary_blake3,
        primary_model: manifest.primary_model,
        executor_model: manifest.executor_model,
        repetitions: args.repetitions,
        arm_definitions,
        task_definitions: manifest.tasks,
        schedules,
        arms: aggregate(&runs),
        task_arms: aggregate_by_task(&runs),
        runs,
        limitations: vec![
            "Each run receives a fresh detached Git worktree at the frozen revision. The harness runs the frozen success command after the adapter; agent-reported success is retained only as a diagnostic.",
            "The random seed, actual arm order, clean source revisions, and verified harness, adapter, and runtime binary hashes are recorded for reproducibility; they do not establish provider determinism.",
            "Provider-reported usage is retained verbatim. Local tokenizer estimates must not be substituted for provider billing counts without an explicit label.",
            "One run per arm is a smoke test, not evidence of a stable pass-rate difference; use repeated runs and report medians, ranges, and sample variance for claims.",
        ],
    };
    let json = serde_json::to_string_pretty(&report)?;
    if let Some(parent) = args
        .output
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
    {
        fs::create_dir_all(parent)?;
    }
    fs::write(&args.output, &json)?;
    println!("{json}");
    Ok(())
}

struct IsolatedWorkspace {
    source: PathBuf,
    path: PathBuf,
    _temporary_directory: tempfile::TempDir,
}

impl IsolatedWorkspace {
    fn create(source: &Path, revision: &str) -> Result<Self, Box<dyn Error>> {
        let temporary_directory = tempfile::tempdir()?;
        let path = temporary_directory.path().join("repository");
        let output = Command::new("git")
            .args(["-C", source.to_string_lossy().as_ref(), "worktree", "add"])
            .arg("--detach")
            .arg(&path)
            .arg(revision)
            .output()?;
        if !output.status.success() {
            return Err(format!(
                "could not create isolated worktree for {revision}: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            )
            .into());
        }
        let workspace = Self {
            source: source.to_owned(),
            path,
            _temporary_directory: temporary_directory,
        };
        verify_revision(workspace.path(), revision)?;
        Ok(workspace)
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for IsolatedWorkspace {
    fn drop(&mut self) {
        let status = Command::new("git")
            .args([
                "-C",
                self.source.to_string_lossy().as_ref(),
                "worktree",
                "remove",
                "--force",
            ])
            .arg(&self.path)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        match status {
            Ok(status) if !status.success() => {
                eprintln!("warning: git worktree removal exited with {status}");
            }
            Err(error) => eprintln!("warning: could not remove isolated worktree: {error}"),
            Ok(_) => {}
        }
    }
}

fn validate_manifest(manifest: &Manifest) -> Result<(), Box<dyn Error>> {
    if !matches!(manifest.schema_version, 2 | 3) {
        return Err("unsupported model A/B manifest schema".into());
    }
    if manifest.experiment_id.trim().is_empty()
        || manifest.primary_model.trim().is_empty()
        || manifest.tasks.is_empty()
        || manifest.timeout_seconds == 0
    {
        return Err("model A/B manifest has empty or zero required fields".into());
    }
    validate_artifact_path_segment(&manifest.experiment_id, "experiment_id")?;
    for arm in Arm::REQUIRED {
        if !manifest.arms.contains_key(&arm) {
            return Err(format!(
                "model A/B manifest is missing required arm {}",
                arm.as_str()
            )
            .into());
        }
    }
    if manifest.arms.contains_key(&Arm::Prewalk) && manifest.executor_model.trim().is_empty() {
        return Err("prewalk arm requires executor_model".into());
    }
    for (arm, definition) in &manifest.arms {
        validate_revision(&definition.runtime_revision, "runtime_revision")?;
        validate_revision(&definition.adapter_revision, "adapter_revision")?;
        validate_blake3(&definition.runtime_binary_blake3, "runtime_binary_blake3")?;
        validate_blake3(&definition.adapter_binary_blake3, "adapter_binary_blake3")?;
        if definition.runtime_repository.as_os_str().is_empty()
            || definition.runtime_binary.as_os_str().is_empty()
            || definition.adapter_repository.as_os_str().is_empty()
            || definition.retrieval_contract.trim().is_empty()
            || definition.tool_catalog.is_empty()
            || definition.budget.tool_call_limit == 0
            || definition.budget.context_token_limit == 0
        {
            return Err(format!("arm {} has an empty or zero required field", arm.as_str()).into());
        }
        let mut tools = HashSet::new();
        for tool in &definition.tool_catalog {
            if tool.trim().is_empty() || !tools.insert(tool) {
                return Err(format!("arm {} has an empty or duplicate tool", arm.as_str()).into());
            }
        }
    }
    let mut task_ids = HashSet::new();
    for task in &manifest.tasks {
        if task.id.trim().is_empty()
            || task.prompt.trim().is_empty()
            || task.repository.as_os_str().is_empty()
            || task.success_command.is_empty()
        {
            return Err(format!("task {} is incomplete", task.id).into());
        }
        validate_revision(&task.revision, "task revision")?;
        validate_artifact_path_segment(&task.id, "task id")?;
        if manifest.schema_version >= 3 {
            let digest = task
                .success_command_executable_blake3
                .as_deref()
                .ok_or("schema v3 task is missing success_command_executable_blake3")?;
            validate_blake3(digest, "success_command_executable_blake3")?;
            if !Path::new(&task.success_command[0]).is_absolute() {
                return Err("schema v3 success command executable must be absolute".into());
            }
        }
        if !task_ids.insert(task.id.as_str()) {
            return Err(format!("duplicate task id: {}", task.id).into());
        }
    }
    Ok(())
}

fn verify_success_command_identity(
    task: &Task,
    manifest_schema_version: u32,
) -> Result<(), Box<dyn Error>> {
    if manifest_schema_version < 3 {
        return Ok(());
    }
    let executable = Path::new(&task.success_command[0]).canonicalize()?;
    let expected = task
        .success_command_executable_blake3
        .as_deref()
        .ok_or("schema v3 task is missing success command identity")?;
    if hash_file(&executable)? != expected {
        return Err(format!("task {} success command hash mismatch", task.id).into());
    }
    Ok(())
}

fn validate_artifact_path_segment(value: &str, field: &str) -> Result<(), Box<dyn Error>> {
    if matches!(value, "." | "..")
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        return Err(format!(
            "{field} must contain only ASCII letters, digits, dots, underscores, or hyphens"
        )
        .into());
    }
    Ok(())
}

fn validate_revision(value: &str, field: &str) -> Result<(), Box<dyn Error>> {
    if value.len() != 40
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        return Err(format!("{field} must be a lowercase full 40-character Git revision").into());
    }
    Ok(())
}

fn validate_blake3(value: &str, field: &str) -> Result<(), Box<dyn Error>> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        return Err(format!("{field} must be a lowercase 64-character BLAKE3 digest").into());
    }
    Ok(())
}

fn seeded_arm_order(
    seed: u64,
    task_id: &str,
    repetition: usize,
    arms: impl IntoIterator<Item = Arm>,
) -> Vec<Arm> {
    let mut keyed = arms
        .into_iter()
        .map(|arm| {
            let mut hasher = blake3::Hasher::new();
            hasher.update(b"leantoken.model-ab.arm-order.v1\0");
            hasher.update(&seed.to_le_bytes());
            hasher.update(&(task_id.len() as u64).to_le_bytes());
            hasher.update(task_id.as_bytes());
            hasher.update(&(repetition as u64).to_le_bytes());
            hasher.update(arm.as_str().as_bytes());
            (hasher.finalize(), arm)
        })
        .collect::<Vec<_>>();
    keyed.sort_unstable_by_key(|(key, arm)| (*key.as_bytes(), *arm));
    keyed.into_iter().map(|(_, arm)| arm).collect()
}

fn prepare_arm_definitions(
    definitions: &BTreeMap<Arm, ArmDefinition>,
    adapter: &Path,
    adapter_binary_blake3: &str,
) -> Result<BTreeMap<Arm, ArmDefinition>, Box<dyn Error>> {
    let adapter = adapter.canonicalize()?;
    let actual_adapter_blake3 = hash_file(&adapter)?;
    if actual_adapter_blake3 != adapter_binary_blake3 {
        return Err("adapter binary changed during preflight".into());
    }
    let mut prepared = BTreeMap::new();
    for (arm, definition) in definitions {
        if definition.adapter_binary_blake3 != actual_adapter_blake3 {
            return Err(format!("arm {} adapter binary hash mismatch", arm.as_str()).into());
        }
        let mut definition = definition.clone();
        definition.runtime_repository = definition.runtime_repository.canonicalize()?;
        definition.adapter_repository = definition.adapter_repository.canonicalize()?;
        definition.runtime_binary = definition.runtime_binary.canonicalize()?;
        verify_clean_revision(&definition.runtime_repository, &definition.runtime_revision)?;
        verify_clean_revision(&definition.adapter_repository, &definition.adapter_revision)?;
        let actual_runtime_blake3 = hash_file(&definition.runtime_binary)?;
        if definition.runtime_binary_blake3 != actual_runtime_blake3 {
            return Err(format!("arm {} runtime binary hash mismatch", arm.as_str()).into());
        }
        prepared.insert(*arm, definition);
    }
    let baseline = &prepared[&Arm::LeanTokenBaseline].runtime_repository;
    let adaptive = &prepared[&Arm::LeanTokenAdaptive].runtime_repository;
    if baseline == adaptive {
        return Err(
            "baseline and adaptive runtime repositories must be independent worktrees".into(),
        );
    }
    Ok(prepared)
}

fn hash_file(path: &Path) -> Result<String, Box<dyn Error>> {
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

fn create_run_artifact_directory(
    root: &Path,
    experiment_id: &str,
    task_id: &str,
    repetition: usize,
    arm: Arm,
) -> Result<PathBuf, Box<dyn Error>> {
    let directory = root
        .join(experiment_id)
        .join(task_id)
        .join(format!("repetition-{repetition}"))
        .join(arm.as_str());
    let parent = directory.parent().expect("run artifact parent");
    fs::create_dir_all(parent)?;
    fs::create_dir(&directory).map_err(|error| {
        format!(
            "could not create immutable run artifact directory {}: {error}",
            directory.display()
        )
    })?;
    Ok(directory.canonicalize()?)
}

fn capture_run_artifacts(
    repository: &Path,
    revision: &str,
    directory: &Path,
) -> Result<RunArtifacts, Box<dyn Error>> {
    verify_revision(repository, revision)?;
    let temporary_index = tempfile::tempdir()?;
    let index_path = temporary_index.path().join("index");
    let read_tree = git_raw_output_with_index(repository, &["read-tree", "HEAD"], &index_path)?;
    if !read_tree.status.success() {
        return Err(format!(
            "could not initialize temporary patch index: {}",
            String::from_utf8_lossy(&read_tree.stderr).trim()
        )
        .into());
    }
    let intent = git_raw_output_with_index(
        repository,
        &["add", "--intent-to-add", "--all"],
        &index_path,
    )?;
    if !intent.status.success() {
        return Err(format!(
            "could not prepare untracked files for patch capture: {}",
            String::from_utf8_lossy(&intent.stderr).trim()
        )
        .into());
    }
    let diff = git_raw_output_with_index(
        repository,
        &["diff", "--binary", "--full-index", "HEAD", "--"],
        &index_path,
    )?;
    if !diff.status.success() {
        return Err(format!(
            "could not capture run patch: {}",
            String::from_utf8_lossy(&diff.stderr).trim()
        )
        .into());
    }
    let patch_path = directory.join(PATCH_FILE);
    fs::write(&patch_path, &diff.stdout)?;
    let diff_check =
        git_raw_output_with_index(repository, &["diff", "--check", "HEAD", "--"], &index_path)?;
    let reverse_apply_valid = if diff.stdout.is_empty() {
        true
    } else {
        git_raw_output(
            repository,
            &[
                "apply",
                "--check",
                "--reverse",
                patch_path.to_string_lossy().as_ref(),
            ],
        )?
        .status
        .success()
    };
    Ok(RunArtifacts {
        directory: directory.to_owned(),
        tool_trace: artifact_identity_if_present(directory, TOOL_TRACE_FILE)?,
        trajectory: artifact_identity_if_present(directory, TRAJECTORY_FILE)?,
        provider_usage: artifact_identity_if_present(directory, PROVIDER_USAGE_FILE)?,
        validation_receipt: None,
        patch: artifact_identity(&patch_path)?,
        patch_valid: diff_check.status.success() && reverse_apply_valid,
        tool_call_records: None,
        range_identities: None,
        trajectory_events: None,
    })
}

fn artifact_identity_if_present(
    directory: &Path,
    name: &str,
) -> Result<Option<ArtifactIdentity>, Box<dyn Error>> {
    let path = directory.join(name);
    match fs::symlink_metadata(&path) {
        Ok(metadata) => {
            if !metadata.file_type().is_file() {
                return Err(
                    format!("run artifact {} is not a regular file", path.display()).into(),
                );
            }
            Ok(Some(artifact_identity(&path)?))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn artifact_identity(path: &Path) -> Result<ArtifactIdentity, Box<dyn Error>> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.file_type().is_file() {
        return Err(format!("run artifact {} is not a regular file", path.display()).into());
    }
    Ok(ArtifactIdentity {
        path: path.canonicalize()?,
        bytes: metadata.len(),
        blake3: hash_file(path)?,
    })
}

fn validate_run_artifacts(
    directory: &Path,
    binding: &RunBinding,
    arm: &ArmDefinition,
    result: &AdapterResult,
    artifacts: &mut RunArtifacts,
) -> Result<(), Box<dyn Error>> {
    if directory.join(VALIDATION_RECEIPT_FILE).exists() {
        return Err(format!(
            "adapter wrote reserved artifact {VALIDATION_RECEIPT_FILE} before validation"
        )
        .into());
    }
    if result.schema_version != 3 {
        return Err("unsupported model A/B adapter result schema".into());
    }
    if !artifacts.patch_valid {
        return Err("captured patch failed Git validation".into());
    }
    for (name, artifact) in [
        (TOOL_TRACE_FILE, artifacts.tool_trace.as_ref()),
        (TRAJECTORY_FILE, artifacts.trajectory.as_ref()),
        (PROVIDER_USAGE_FILE, artifacts.provider_usage.as_ref()),
    ] {
        if artifact.is_none() {
            return Err(format!("adapter did not persist required artifact {name}").into());
        }
    }

    let trace: ToolTrace = read_artifact_json(directory, TOOL_TRACE_FILE)?;
    validate_artifact_header(
        trace.schema_version,
        &trace.binding,
        binding,
        TOOL_TRACE_FILE,
    )?;
    let mut call_ids = HashSet::new();
    let mut result_ids = HashSet::new();
    let mut rereads = 0usize;
    let mut reread_tokens = 0u64;
    let mut failed_tool_calls = 0usize;
    let mut failed_searches = 0usize;
    let mut dead_end_reads = 0usize;
    let mut range_identities = 0usize;
    for (expected_sequence, call) in trace.calls.iter().enumerate() {
        if call.sequence != expected_sequence {
            return Err(format!(
                "tool trace sequence {} is not contiguous at {expected_sequence}",
                call.sequence
            )
            .into());
        }
        if call.tool_name.trim().is_empty() || !arm.tool_catalog.contains(&call.tool_name) {
            return Err(format!("tool trace names unavailable tool {}", call.tool_name).into());
        }
        if call.call_id.trim().is_empty() || !call_ids.insert(call.call_id.as_str()) {
            return Err("tool trace has an empty or duplicate call_id".into());
        }
        if call.result_id.trim().is_empty() || !result_ids.insert(call.result_id.as_str()) {
            return Err("tool trace has an empty or duplicate result_id".into());
        }
        rereads += usize::from(call.reread);
        if call.reread {
            reread_tokens = reread_tokens
                .checked_add(call.result_source_tokens)
                .ok_or("reread source token total overflow")?;
        }
        failed_tool_calls += usize::from(matches!(
            call.outcome,
            ToolOutcome::FailedSearch | ToolOutcome::Error
        ));
        failed_searches += usize::from(call.outcome == ToolOutcome::FailedSearch);
        dead_end_reads += usize::from(call.outcome == ToolOutcome::DeadEndRead);
        for range in &call.ranges {
            validate_range_identity(range)?;
            range_identities += 1;
        }
    }
    if trace.calls.len() != result.tool_calls
        || rereads != result.rereads
        || reread_tokens != result.reread_tokens
        || failed_tool_calls != result.failed_tool_calls
        || failed_searches != result.failed_searches
        || dead_end_reads != result.dead_end_reads
    {
        return Err("adapter summary counters do not match the exact tool trace".into());
    }

    let trajectory: Trajectory = read_artifact_json(directory, TRAJECTORY_FILE)?;
    validate_artifact_header(
        trajectory.schema_version,
        &trajectory.binding,
        binding,
        TRAJECTORY_FILE,
    )?;
    let usage: ProviderUsageReceipt = read_artifact_json(directory, PROVIDER_USAGE_FILE)?;
    validate_artifact_header(
        usage.schema_version,
        &usage.binding,
        binding,
        PROVIDER_USAGE_FILE,
    )?;
    if usage.usage != result.provider_usage {
        return Err("adapter provider usage does not match its persisted raw receipt".into());
    }
    if usage.raw_receipt.is_null() {
        return Err("persisted provider usage must retain a non-null raw receipt".into());
    }
    validate_usage_totals(result)?;
    artifacts.tool_call_records = Some(trace.calls.len());
    artifacts.range_identities = Some(range_identities);
    artifacts.trajectory_events = Some(trajectory.events.len());
    Ok(())
}

fn validate_artifact_header(
    schema_version: u32,
    actual: &RunBinding,
    expected: &RunBinding,
    name: &str,
) -> Result<(), Box<dyn Error>> {
    if schema_version != ARTIFACT_SCHEMA_V1 {
        return Err(format!("unsupported schema version in {name}").into());
    }
    if actual != expected {
        return Err(format!("run binding mismatch in {name}").into());
    }
    Ok(())
}

fn validate_range_identity(
    range: &model_ab_artifacts::RangeIdentity,
) -> Result<(), Box<dyn Error>> {
    let path = Path::new(&range.path);
    if range.path.is_empty()
        || range.path.contains('\\')
        || path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, std::path::Component::Normal(_)))
        || range.start_line == 0
        || range.end_line < range.start_line
    {
        return Err(format!("invalid tool-result range {}", range.path).into());
    }
    validate_blake3(&range.content_hash, "range content_hash")
}

fn validate_usage_totals(result: &AdapterResult) -> Result<(), Box<dyn Error>> {
    let usage = &result.provider_usage;
    if let (Some(uncached), Some(created), Some(read), Some(total)) = (
        usage.uncached_input_tokens,
        usage.cache_creation_input_tokens,
        usage.cache_read_input_tokens,
        result.total_input_tokens,
    ) {
        let categorized = uncached
            .checked_add(created)
            .and_then(|value| value.checked_add(read))
            .ok_or("provider input token total overflow")?;
        if categorized != total {
            return Err("provider input categories do not match total_input_tokens".into());
        }
    }
    if let (Some(provider_output), Some(total_output)) =
        (usage.output_tokens, result.total_output_tokens)
        && provider_output != total_output
    {
        return Err("provider output usage does not match total_output_tokens".into());
    }
    if result
        .provider_reported_cost_usd
        .is_some_and(|cost| !cost.is_finite() || cost < 0.0)
    {
        return Err("provider-reported cost must be finite and non-negative".into());
    }
    Ok(())
}

fn read_artifact_json<T: serde::de::DeserializeOwned>(
    directory: &Path,
    name: &str,
) -> Result<T, Box<dyn Error>> {
    Ok(serde_json::from_slice(&fs::read(directory.join(name))?)?)
}

fn invoke_adapter(
    adapter: &Path,
    adapter_args: &[String],
    request: &AdapterRequest<'_>,
    timeout: Duration,
) -> Result<AdapterResult, AdapterFailure> {
    let mut child = Command::new(adapter)
        .args(adapter_args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .map_err(AdapterFailure::from_error)?;
    let input = serde_json::to_vec(request).map_err(AdapterFailure::from_error)?;
    child
        .stdin
        .take()
        .ok_or_else(|| AdapterFailure::new("adapter stdin unavailable"))?
        .write_all(&input)
        .map_err(AdapterFailure::from_error)?;
    let mut stdout = child
        .stdout
        .take()
        .ok_or_else(|| AdapterFailure::new("adapter stdout unavailable"))?;
    let output_reader = std::thread::spawn(move || {
        let mut output = Vec::new();
        stdout.read_to_end(&mut output)?;
        Ok::<_, std::io::Error>(output)
    });
    let status = child
        .wait_timeout(timeout)
        .map_err(AdapterFailure::from_error)?;
    let Some(status) = status else {
        child.kill().map_err(AdapterFailure::from_error)?;
        let _ = child.wait();
        let _ = output_reader.join();
        return Err(AdapterFailure::timeout());
    };
    let output = output_reader
        .join()
        .map_err(|_| AdapterFailure::new("adapter output reader panicked"))?
        .map_err(AdapterFailure::from_error)?;
    if !status.success() {
        return Err(AdapterFailure::new(format!(
            "model adapter exited with {status}"
        )));
    }
    serde_json::from_slice(&output).map_err(AdapterFailure::from_error)
}

struct AdapterFailure {
    kind: AdapterFailureKind,
    message: String,
}

enum AdapterFailureKind {
    Failed,
    TimedOut,
}

impl AdapterFailure {
    fn new(message: impl Into<String>) -> Self {
        Self {
            kind: AdapterFailureKind::Failed,
            message: message.into(),
        }
    }

    fn from_error(error: impl Error) -> Self {
        Self::new(error.to_string())
    }

    fn timeout() -> Self {
        Self {
            kind: AdapterFailureKind::TimedOut,
            message: "model adapter timed out".to_owned(),
        }
    }
}

fn run_validation(
    repository: &Path,
    command: &[String],
    context: &ValidationContext<'_>,
    timeout: Duration,
) -> Result<Option<i32>, Box<dyn Error>> {
    let (program, args) = command.split_first().ok_or("success command is empty")?;
    let mut child = Command::new(program)
        .args(args)
        .current_dir(repository)
        .env(
            "LEANTOKEN_MODEL_AB_ARTIFACTS_DIRECTORY",
            context.artifacts_directory,
        )
        .env("LEANTOKEN_MODEL_AB_EXPERIMENT_ID", context.experiment_id)
        .env(
            "LEANTOKEN_MODEL_AB_MANIFEST_BLAKE3",
            context.manifest_blake3,
        )
        .env("LEANTOKEN_MODEL_AB_TASK_ID", context.task_id)
        .env(
            "LEANTOKEN_MODEL_AB_REPETITION",
            context.repetition.to_string(),
        )
        .env("LEANTOKEN_MODEL_AB_ARM", context.arm.as_str())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    if let Some(status) = child.wait_timeout(timeout)? {
        return Ok(status.code());
    }
    child.kill()?;
    let _ = child.wait();
    Ok(None)
}

fn validate_validation_receipt(
    directory: &Path,
    context: &ValidationContext<'_>,
) -> Result<(), Box<dyn Error>> {
    let path = directory.join(VALIDATION_RECEIPT_FILE);
    let receipt: ValidationReceiptBinding =
        serde_json::from_slice(&fs::read(&path).map_err(|error| {
            format!("success command did not persist readable {VALIDATION_RECEIPT_FILE}: {error}")
        })?)?;
    if receipt.schema_version != 1
        || receipt.experiment_id != context.experiment_id
        || receipt.manifest_blake3 != context.manifest_blake3
        || receipt.task_id != context.task_id
        || receipt.repetition != context.repetition
        || receipt.arm != context.arm.as_str()
    {
        return Err("validation receipt run binding mismatch".into());
    }
    Ok(())
}

fn verify_revision(repository: &Path, revision: &str) -> Result<(), Box<dyn Error>> {
    if git_output(repository, &["rev-parse", "HEAD"])? != revision {
        return Err(format!("{} is not checked out at {revision}", repository.display()).into());
    }
    Ok(())
}

fn verify_clean_revision(repository: &Path, revision: &str) -> Result<(), Box<dyn Error>> {
    let repository = repository.canonicalize()?;
    let top_level = PathBuf::from(git_output(&repository, &["rev-parse", "--show-toplevel"])?);
    if top_level.canonicalize()? != repository {
        return Err(format!("{} is not a Git top-level worktree", repository.display()).into());
    }
    verify_revision(&repository, revision)?;
    let status = git_output(
        &repository,
        &["status", "--porcelain=v1", "--untracked-files=all"],
    )?;
    if !status.is_empty() {
        return Err(format!(
            "{} has uncommitted or untracked files",
            repository.display()
        )
        .into());
    }
    Ok(())
}

fn source_identity(repository: &Path) -> Result<SourceIdentity, Box<dyn Error>> {
    Ok(SourceIdentity {
        revision: git_output(repository, &["rev-parse", "HEAD"])?,
        dirty: !git_output(
            repository,
            &["status", "--porcelain=v1", "--untracked-files=all"],
        )?
        .is_empty(),
    })
}

fn git_output(repository: &Path, args: &[&str]) -> Result<String, Box<dyn Error>> {
    let output = git_raw_output(repository, args)?;
    if !output.status.success() {
        return Err(format!(
            "git command failed with {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        )
        .into());
    }
    Ok(String::from_utf8(output.stdout)?.trim().to_owned())
}

fn git_raw_output(
    repository: &Path,
    args: &[&str],
) -> Result<std::process::Output, Box<dyn Error>> {
    Ok(Command::new("git")
        .arg("-C")
        .arg(repository)
        .args(args)
        .stdin(Stdio::null())
        .output()?)
}

fn git_raw_output_with_index(
    repository: &Path,
    args: &[&str],
    index_path: &Path,
) -> Result<std::process::Output, Box<dyn Error>> {
    Ok(Command::new("git")
        .arg("-C")
        .arg(repository)
        .args(args)
        .env("GIT_INDEX_FILE", index_path)
        .stdin(Stdio::null())
        .output()?)
}

fn aggregate<'a>(runs: impl IntoIterator<Item = &'a RunReport>) -> BTreeMap<Arm, ArmAggregate> {
    let runs = runs.into_iter().collect::<Vec<_>>();
    let mut aggregates = BTreeMap::<Arm, ArmAggregate>::new();
    for run in &runs {
        let item = aggregates.entry(run.arm).or_default();
        item.runs += 1;
        item.total_duration_ms += run.duration_ms;
        if let Some(result) = &run.result {
            item.adapter_completed += 1;
            item.successes += usize::from(result.task_success);
            item.tool_calls += result.tool_calls;
            item.rereads += result.rereads;
            item.reread_tokens += result.reread_tokens;
            item.failed_tool_calls += result.failed_tool_calls;
            item.failed_searches += result.failed_searches;
            item.dead_end_reads += result.dead_end_reads;
        }
        item.run_errors += usize::from(!matches!(run.status, RunStatus::Completed));
    }
    for item in aggregates.values_mut() {
        item.success_rate = if item.runs == 0 {
            0.0
        } else {
            item.successes as f64 / item.runs as f64
        };
    }
    for (arm, item) in &mut aggregates {
        let arm_runs = runs
            .iter()
            .filter(|run| run.arm == *arm)
            .copied()
            .collect::<Vec<_>>();
        let results = runs
            .iter()
            .filter(|run| run.arm == *arm)
            .filter_map(|run| run.result.as_ref())
            .collect::<Vec<_>>();
        let all_runs_have_results = results.len() == item.runs;
        item.provider_reported_cost_usd = (all_runs_have_results && !results.is_empty())
            .then(|| {
                results
                    .iter()
                    .map(|result| result.provider_reported_cost_usd)
                    .collect::<Option<Vec<_>>>()
                    .map(|costs| costs.into_iter().sum())
            })
            .flatten();
        item.total_input_tokens = all_runs_have_results
            .then(|| complete_sum(results.iter().map(|result| result.total_input_tokens)))
            .flatten();
        item.total_output_tokens = all_runs_have_results
            .then(|| complete_sum(results.iter().map(|result| result.total_output_tokens)))
            .flatten();
        item.input_tokens_per_success = item
            .total_input_tokens
            .filter(|_| item.successes > 0)
            .map(|tokens| tokens as f64 / item.successes as f64);
        item.provider_reported_cost_per_success_usd = item
            .provider_reported_cost_usd
            .filter(|_| item.successes > 0)
            .map(|cost| cost / item.successes as f64);
        item.input_tokens = sample_statistics(
            results
                .iter()
                .filter_map(|result| result.total_input_tokens.map(|tokens| tokens as f64))
                .collect(),
        );
        item.output_tokens = sample_statistics(
            results
                .iter()
                .filter_map(|result| result.total_output_tokens.map(|tokens| tokens as f64))
                .collect(),
        );
        item.provider_cost_usd = sample_statistics(
            results
                .iter()
                .filter_map(|result| result.provider_reported_cost_usd)
                .collect(),
        );
        item.duration_ms =
            sample_statistics(arm_runs.iter().map(|run| run.duration_ms as f64).collect());
        item.successful_input_tokens = sample_statistics(
            arm_runs
                .iter()
                .filter_map(|run| {
                    run.result
                        .as_ref()
                        .filter(|result| result.task_success)
                        .and_then(|result| result.total_input_tokens)
                        .map(|tokens| tokens as f64)
                })
                .collect(),
        );
        item.successful_duration_ms = sample_statistics(
            arm_runs
                .iter()
                .filter(|run| {
                    run.result
                        .as_ref()
                        .is_some_and(|result| result.task_success)
                })
                .map(|run| run.duration_ms as f64)
                .collect(),
        );
    }
    aggregates
}

fn complete_sum(values: impl IntoIterator<Item = Option<u64>>) -> Option<u64> {
    let mut count = 0usize;
    let mut total = 0u64;
    for value in values {
        total = total.checked_add(value?)?;
        count += 1;
    }
    (count > 0).then_some(total)
}

fn sample_statistics(mut samples: Vec<f64>) -> SampleStatistics {
    samples.sort_by(f64::total_cmp);
    let median = match samples.len() {
        0 => None,
        length if length % 2 == 1 => Some(samples[length / 2]),
        length => Some((samples[length / 2 - 1] + samples[length / 2]) / 2.0),
    };
    SampleStatistics {
        samples: samples.len(),
        minimum: samples.first().copied(),
        median,
        mean: (!samples.is_empty()).then(|| samples.as_slice().mean()),
        maximum: samples.last().copied(),
        sample_variance: (samples.len() > 1).then(|| samples.as_slice().variance()),
    }
}

fn aggregate_by_task(runs: &[RunReport]) -> BTreeMap<String, BTreeMap<Arm, ArmAggregate>> {
    let mut task_arms = BTreeMap::new();
    for task_id in runs.iter().map(|run| &run.task_id) {
        task_arms
            .entry(task_id.clone())
            .or_insert_with(|| aggregate(runs.iter().filter(|run| run.task_id == *task_id)));
    }
    task_arms
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model_ab_artifacts::{RangeIdentity, ToolCall};

    #[test]
    fn sample_statistics_reports_distribution_and_sample_variance() {
        let statistics = sample_statistics(vec![40.0, 10.0, 30.0, 20.0]);

        assert_eq!(statistics.samples, 4);
        assert_eq!(statistics.minimum, Some(10.0));
        assert_eq!(statistics.median, Some(25.0));
        assert_eq!(statistics.mean, Some(25.0));
        assert_eq!(statistics.maximum, Some(40.0));
        assert_eq!(statistics.sample_variance, Some(500.0 / 3.0));
        assert_eq!(
            sample_statistics(vec![10.0]).sample_variance,
            None,
            "one run cannot establish sample variance"
        );
    }

    #[test]
    fn failed_attempts_remain_in_aggregates_without_inventing_usage() {
        let run = RunReport {
            task_id: "task".to_owned(),
            repetition: 1,
            arm_order_index: 0,
            arm: Arm::Filesystem,
            primary_model: "provider/model".to_owned(),
            executor_model: None,
            duration_ms: 50,
            status: RunStatus::AdapterFailed,
            validation_duration_ms: None,
            validation_exit_code: None,
            agent_reported_success: None,
            error: Some("adapter failed".to_owned()),
            artifacts: RunArtifacts {
                directory: PathBuf::from("artifacts"),
                tool_trace: None,
                trajectory: None,
                provider_usage: None,
                validation_receipt: None,
                patch: ArtifactIdentity {
                    path: PathBuf::from("patch.diff"),
                    bytes: 0,
                    blake3: "0".repeat(64),
                },
                patch_valid: true,
                tool_call_records: None,
                range_identities: None,
                trajectory_events: None,
            },
            result: None,
        };

        let aggregates = aggregate([&run]);
        let filesystem = aggregates.get(&Arm::Filesystem).expect("filesystem arm");
        assert_eq!(filesystem.runs, 1);
        assert_eq!(filesystem.adapter_completed, 0);
        assert_eq!(filesystem.run_errors, 1);
        assert_eq!(filesystem.success_rate, 0.0);
        assert_eq!(filesystem.provider_reported_cost_usd, None);
        assert_eq!(filesystem.input_tokens.mean, None);
        assert_eq!(filesystem.duration_ms.mean, Some(50.0));
    }

    #[test]
    fn seeded_arm_order_is_reproducible_and_seed_sensitive() {
        let arms = [
            Arm::Filesystem,
            Arm::LeanTokenBaseline,
            Arm::LeanTokenAdaptive,
            Arm::LeanTokenAdaptiveRecovery,
            Arm::Prewalk,
        ];
        let first = seeded_arm_order(7, "task-a", 2, arms);
        let repeated = seeded_arm_order(7, "task-a", 2, arms);
        let different_seed = seeded_arm_order(11, "task-a", 2, arms);

        assert_eq!(first, repeated);
        assert_ne!(first, different_seed);
        assert_eq!(
            first.into_iter().collect::<HashSet<_>>(),
            arms.into_iter().collect()
        );
    }

    #[test]
    fn manifest_requires_all_core_arms_and_frozen_identity_fields() {
        let mut manifest = valid_manifest();
        validate_manifest(&manifest).expect("valid manifest");

        manifest.arms.remove(&Arm::LeanTokenAdaptiveRecovery);
        assert!(
            validate_manifest(&manifest)
                .expect_err("missing core arm")
                .to_string()
                .contains("missing required arm lean_token_adaptive_recovery")
        );

        let mut manifest = valid_manifest();
        manifest
            .arms
            .get_mut(&Arm::Filesystem)
            .expect("filesystem definition")
            .runtime_binary_blake3 = "not-a-hash".to_owned();
        assert!(
            validate_manifest(&manifest)
                .expect_err("invalid binary identity")
                .to_string()
                .contains("runtime_binary_blake3")
        );

        let mut manifest = valid_manifest();
        manifest.schema_version = 3;
        assert!(
            validate_manifest(&manifest)
                .expect_err("schema v3 must bind the validator executable")
                .to_string()
                .contains("success_command_executable_blake3")
        );
    }

    #[test]
    fn schema_v3_success_command_identity_is_verified() {
        let directory = tempfile::tempdir().expect("validator directory");
        let validator = directory.path().join("validator");
        fs::write(&validator, b"frozen validator").expect("validator fixture");
        let digest = hash_file(&validator).expect("validator hash");
        let mut manifest = valid_manifest();
        manifest.schema_version = 3;
        manifest.tasks[0].success_command = vec![validator.to_string_lossy().into_owned()];
        manifest.tasks[0].success_command_executable_blake3 = Some(digest);

        validate_manifest(&manifest).expect("valid schema v3 manifest");
        verify_success_command_identity(&manifest.tasks[0], manifest.schema_version)
            .expect("matching validator identity");
        fs::write(&validator, b"changed validator").expect("mutate validator");
        assert!(
            verify_success_command_identity(&manifest.tasks[0], manifest.schema_version)
                .expect_err("changed validator")
                .to_string()
                .contains("hash mismatch")
        );
    }

    #[test]
    fn arm_preflight_verifies_revisions_hashes_and_independent_worktrees() {
        let (baseline_repository, baseline_revision) = initialized_repository();
        let (adaptive_repository, adaptive_revision) = initialized_repository();
        let artifacts = tempfile::tempdir().expect("artifact directory");
        let binary = artifacts.path().join("runtime-adapter");
        fs::write(&binary, b"frozen artifact").expect("artifact");
        let binary_blake3 = hash_file(&binary).expect("artifact hash");
        let mut definitions = BTreeMap::new();
        for arm in [Arm::Filesystem, Arm::LeanTokenBaseline] {
            definitions.insert(
                arm,
                arm_definition(
                    baseline_repository.path(),
                    &baseline_revision,
                    baseline_repository.path(),
                    &baseline_revision,
                    &binary,
                    &binary_blake3,
                ),
            );
        }
        for arm in [Arm::LeanTokenAdaptive, Arm::LeanTokenAdaptiveRecovery] {
            definitions.insert(
                arm,
                arm_definition(
                    adaptive_repository.path(),
                    &adaptive_revision,
                    baseline_repository.path(),
                    &baseline_revision,
                    &binary,
                    &binary_blake3,
                ),
            );
        }

        let prepared = prepare_arm_definitions(&definitions, &binary, &binary_blake3)
            .expect("valid arm definitions");
        assert_ne!(
            prepared[&Arm::LeanTokenBaseline].runtime_repository,
            prepared[&Arm::LeanTokenAdaptive].runtime_repository
        );

        fs::write(&binary, b"changed artifact").expect("mutate artifact");
        let changed_blake3 = hash_file(&binary).expect("changed hash");
        assert!(
            prepare_arm_definitions(&definitions, &binary, &changed_blake3)
                .expect_err("manifest hash must bind the adapter")
                .to_string()
                .contains("adapter binary hash mismatch")
        );
    }

    #[test]
    fn clean_revision_preflight_rejects_untracked_files() {
        let (repository, revision) = initialized_repository();
        verify_clean_revision(repository.path(), &revision).expect("clean repository");

        fs::write(repository.path().join("untracked.txt"), "dirty\n").expect("untracked file");
        assert!(
            verify_clean_revision(repository.path(), &revision)
                .expect_err("dirty repository")
                .to_string()
                .contains("uncommitted or untracked")
        );
    }

    #[test]
    fn isolated_workspace_starts_at_revision_without_mutating_source() {
        let (source, revision) = initialized_repository();

        {
            let workspace =
                IsolatedWorkspace::create(source.path(), &revision).expect("isolated worktree");
            fs::write(workspace.path().join("file.txt"), "changed\n").expect("workspace edit");
            assert_eq!(
                fs::read_to_string(source.path().join("file.txt")).expect("source read"),
                "base\n"
            );
        }

        assert_eq!(
            git_output(source.path(), &["worktree", "list", "--porcelain"])
                .matches("worktree ")
                .count(),
            1
        );
    }

    #[test]
    fn run_artifacts_validate_exact_trace_binding_ranges_and_usage() {
        let directory = tempfile::tempdir().expect("artifact directory");
        let binding = artifact_binding();
        persist_valid_adapter_artifacts(directory.path(), &binding);
        let mut artifacts = artifact_identities(directory.path());

        validate_run_artifacts(
            directory.path(),
            &binding,
            &artifact_arm_definition(),
            &valid_adapter_result(),
            &mut artifacts,
        )
        .expect("valid artifact chain");

        assert_eq!(artifacts.tool_call_records, Some(2));
        assert_eq!(artifacts.range_identities, Some(2));
        assert_eq!(artifacts.trajectory_events, Some(1));
    }

    #[test]
    fn run_artifacts_reject_binding_and_summary_mismatches() {
        let directory = tempfile::tempdir().expect("artifact directory");
        let binding = artifact_binding();
        let mut wrong_binding = binding.clone();
        wrong_binding.repetition += 1;
        persist_valid_adapter_artifacts(directory.path(), &wrong_binding);
        let mut artifacts = artifact_identities(directory.path());
        assert!(
            validate_run_artifacts(
                directory.path(),
                &binding,
                &artifact_arm_definition(),
                &valid_adapter_result(),
                &mut artifacts,
            )
            .expect_err("binding mismatch")
            .to_string()
            .contains("run binding mismatch")
        );

        persist_valid_adapter_artifacts(directory.path(), &binding);
        let mut result = valid_adapter_result();
        result.reread_tokens += 1;
        let mut artifacts = artifact_identities(directory.path());
        assert!(
            validate_run_artifacts(
                directory.path(),
                &binding,
                &artifact_arm_definition(),
                &result,
                &mut artifacts,
            )
            .expect_err("summary mismatch")
            .to_string()
            .contains("summary counters")
        );
    }

    #[test]
    fn run_artifacts_reject_missing_required_receipt() {
        let directory = tempfile::tempdir().expect("artifact directory");
        let binding = artifact_binding();
        persist_valid_adapter_artifacts(directory.path(), &binding);
        fs::remove_file(directory.path().join(PROVIDER_USAGE_FILE)).expect("remove receipt");
        let mut artifacts = artifact_identities(directory.path());

        assert!(
            validate_run_artifacts(
                directory.path(),
                &binding,
                &artifact_arm_definition(),
                &valid_adapter_result(),
                &mut artifacts,
            )
            .expect_err("missing receipt")
            .to_string()
            .contains(PROVIDER_USAGE_FILE)
        );
    }

    #[test]
    fn adapter_cannot_prewrite_validation_receipt() {
        let directory = tempfile::tempdir().expect("artifact directory");
        let binding = artifact_binding();
        persist_valid_adapter_artifacts(directory.path(), &binding);
        fs::write(directory.path().join(VALIDATION_RECEIPT_FILE), b"reserved")
            .expect("reserved receipt");
        let mut artifacts = artifact_identities(directory.path());

        assert!(
            validate_run_artifacts(
                directory.path(),
                &binding,
                &artifact_arm_definition(),
                &valid_adapter_result(),
                &mut artifacts,
            )
            .expect_err("adapter-written validation receipt")
            .to_string()
            .contains("reserved artifact")
        );
    }

    #[test]
    fn validation_receipt_must_match_run_binding() {
        let directory = tempfile::tempdir().expect("artifact directory");
        let manifest_blake3 = "a".repeat(64);
        let context = ValidationContext {
            artifacts_directory: directory.path(),
            experiment_id: "experiment",
            manifest_blake3: &manifest_blake3,
            task_id: "task",
            repetition: 2,
            arm: Arm::LeanTokenAdaptive,
        };
        let mut receipt = serde_json::json!({
            "schema_version": 1,
            "experiment_id": context.experiment_id,
            "manifest_blake3": context.manifest_blake3,
            "task_id": context.task_id,
            "repetition": context.repetition,
            "arm": context.arm.as_str()
        });
        fs::write(
            directory.path().join(VALIDATION_RECEIPT_FILE),
            serde_json::to_vec(&receipt).expect("receipt JSON"),
        )
        .expect("receipt fixture");
        validate_validation_receipt(directory.path(), &context).expect("matching receipt");

        receipt["arm"] = serde_json::json!(Arm::Filesystem.as_str());
        fs::write(
            directory.path().join(VALIDATION_RECEIPT_FILE),
            serde_json::to_vec(&receipt).expect("receipt JSON"),
        )
        .expect("mismatched receipt fixture");
        assert!(
            validate_validation_receipt(directory.path(), &context)
                .expect_err("mismatched receipt")
                .to_string()
                .contains("binding mismatch")
        );
    }

    #[test]
    fn patch_capture_includes_tracked_and_untracked_changes() {
        let (repository, revision) = initialized_repository();
        fs::write(repository.path().join("file.txt"), "changed\n").expect("tracked edit");
        fs::write(repository.path().join("new.txt"), "new\n").expect("untracked edit");
        let directory = tempfile::tempdir().expect("artifact directory");

        let artifacts = capture_run_artifacts(repository.path(), &revision, directory.path())
            .expect("capture patch");
        let patch = fs::read_to_string(directory.path().join(PATCH_FILE)).expect("read patch");

        assert!(artifacts.patch_valid);
        assert!(patch.contains("a/file.txt"));
        assert!(patch.contains("b/new.txt"));
        assert_eq!(
            artifacts.patch.blake3,
            hash_file(&artifacts.patch.path).unwrap()
        );
        assert!(
            git_raw_output(repository.path(), &["diff", "--cached", "--quiet"])
                .expect("inspect real index")
                .status
                .success(),
            "patch capture must not mutate the task worktree index"
        );
    }

    #[test]
    fn artifact_directories_are_immutable_per_run_identity() {
        let root = tempfile::tempdir().expect("artifact root");
        create_run_artifact_directory(root.path(), "experiment", "task", 1, Arm::Filesystem)
            .expect("first run directory");

        assert!(
            create_run_artifact_directory(root.path(), "experiment", "task", 1, Arm::Filesystem)
                .expect_err("duplicate run directory")
                .to_string()
                .contains("immutable run artifact directory")
        );
    }

    #[test]
    fn complete_sums_preserve_missing_usage() {
        assert_eq!(complete_sum([Some(2), Some(3)]), Some(5));
        assert_eq!(complete_sum([Some(2), None]), None);
        assert_eq!(complete_sum([]), None);
        assert_eq!(complete_sum([Some(u64::MAX), Some(1)]), None);
    }

    fn artifact_binding() -> RunBinding {
        RunBinding {
            experiment_id: "experiment".to_owned(),
            manifest_blake3: "a".repeat(64),
            task_id: "task".to_owned(),
            repetition: 1,
            arm: Arm::Filesystem.as_str().to_owned(),
        }
    }

    fn valid_adapter_result() -> AdapterResult {
        AdapterResult {
            schema_version: 3,
            task_success: false,
            total_input_tokens: Some(6),
            total_output_tokens: Some(4),
            provider_reported_cost_usd: Some(0.01),
            tool_calls: 2,
            rereads: 1,
            reread_tokens: 17,
            failed_tool_calls: 1,
            failed_searches: 1,
            dead_end_reads: 1,
            provider_usage: ProviderUsage {
                uncached_input_tokens: Some(1),
                cache_creation_input_tokens: Some(2),
                cache_read_input_tokens: Some(3),
                output_tokens: Some(4),
                reasoning_tokens: Some(1),
            },
            evidence_receipt: None,
            repository_generation: Some(7),
        }
    }

    fn persist_valid_adapter_artifacts(directory: &Path, binding: &RunBinding) {
        write_json_fixture(
            directory.join(TOOL_TRACE_FILE),
            &ToolTrace {
                schema_version: ARTIFACT_SCHEMA_V1,
                binding: binding.clone(),
                calls: vec![
                    ToolCall {
                        sequence: 0,
                        tool_name: "file_read".to_owned(),
                        call_id: "call-0".to_owned(),
                        result_id: "result-0".to_owned(),
                        outcome: ToolOutcome::FailedSearch,
                        result_source_tokens: 17,
                        reread: true,
                        ranges: vec![RangeIdentity {
                            repository_generation: 7,
                            path: "src/lib.rs".to_owned(),
                            start_line: 2,
                            end_line: 4,
                            content_hash: "b".repeat(64),
                            source_tokens: Some(17),
                        }],
                    },
                    ToolCall {
                        sequence: 1,
                        tool_name: "file_read".to_owned(),
                        call_id: "call-1".to_owned(),
                        result_id: "result-1".to_owned(),
                        outcome: ToolOutcome::DeadEndRead,
                        result_source_tokens: 9,
                        reread: false,
                        ranges: vec![RangeIdentity {
                            repository_generation: 7,
                            path: "tests/integration.rs".to_owned(),
                            start_line: 1,
                            end_line: 1,
                            content_hash: "c".repeat(64),
                            source_tokens: Some(9),
                        }],
                    },
                ],
            },
        );
        write_json_fixture(
            directory.join(TRAJECTORY_FILE),
            &Trajectory {
                schema_version: ARTIFACT_SCHEMA_V1,
                binding: binding.clone(),
                events: vec![serde_json::json!({"kind": "model_turn", "sequence": 0})],
            },
        );
        write_json_fixture(
            directory.join(PROVIDER_USAGE_FILE),
            &ProviderUsageReceipt {
                schema_version: ARTIFACT_SCHEMA_V1,
                binding: binding.clone(),
                usage: valid_adapter_result().provider_usage,
                raw_receipt: serde_json::json!({"provider": "fixture"}),
            },
        );
    }

    fn artifact_arm_definition() -> ArmDefinition {
        arm_definition(
            Path::new("runtime-repository"),
            &"0".repeat(40),
            Path::new("adapter-repository"),
            &"0".repeat(40),
            Path::new("runtime-binary"),
            &"0".repeat(64),
        )
    }

    fn artifact_identities(directory: &Path) -> RunArtifacts {
        fs::write(directory.join(PATCH_FILE), []).expect("empty patch");
        RunArtifacts {
            directory: directory.to_owned(),
            tool_trace: artifact_identity_if_present(directory, TOOL_TRACE_FILE).unwrap(),
            trajectory: artifact_identity_if_present(directory, TRAJECTORY_FILE).unwrap(),
            provider_usage: artifact_identity_if_present(directory, PROVIDER_USAGE_FILE).unwrap(),
            validation_receipt: None,
            patch: artifact_identity(&directory.join(PATCH_FILE)).unwrap(),
            patch_valid: true,
            tool_call_records: None,
            range_identities: None,
            trajectory_events: None,
        }
    }

    fn write_json_fixture(path: PathBuf, value: &impl Serialize) {
        fs::write(
            path,
            serde_json::to_vec_pretty(value).expect("serialize fixture"),
        )
        .expect("write fixture");
    }

    fn initialized_repository() -> (tempfile::TempDir, String) {
        let repository = tempfile::tempdir().expect("source repository");
        run_git(repository.path(), &["init", "--quiet"]);
        run_git(
            repository.path(),
            &["config", "user.name", "LeanToken Test"],
        );
        run_git(
            repository.path(),
            &["config", "user.email", "leantoken@example.invalid"],
        );
        fs::write(repository.path().join("file.txt"), "base\n").expect("fixture");
        run_git(repository.path(), &["add", "file.txt"]);
        run_git(repository.path(), &["commit", "--quiet", "-m", "fixture"]);
        let revision = git_output(repository.path(), &["rev-parse", "HEAD"]);
        (repository, revision)
    }

    fn arm_definition(
        runtime_repository: &Path,
        runtime_revision: &str,
        adapter_repository: &Path,
        adapter_revision: &str,
        binary: &Path,
        binary_blake3: &str,
    ) -> ArmDefinition {
        ArmDefinition {
            runtime_repository: runtime_repository.to_owned(),
            runtime_revision: runtime_revision.to_owned(),
            runtime_binary: binary.to_owned(),
            runtime_binary_blake3: binary_blake3.to_owned(),
            adapter_repository: adapter_repository.to_owned(),
            adapter_revision: adapter_revision.to_owned(),
            adapter_binary_blake3: binary_blake3.to_owned(),
            configuration: serde_json::json!({"mode": "dry_run"}),
            tool_catalog: vec!["file_read".to_owned()],
            budget: ArmBudget {
                tool_call_limit: 1,
                context_token_limit: 1,
            },
            retrieval_contract: "dry run".to_owned(),
        }
    }

    fn valid_manifest() -> Manifest {
        let revision = "0".repeat(40);
        let binary_blake3 = "0".repeat(64);
        let mut arms = BTreeMap::new();
        for arm in Arm::REQUIRED {
            arms.insert(
                arm,
                arm_definition(
                    Path::new("runtime-repository"),
                    &revision,
                    Path::new("adapter-repository"),
                    &revision,
                    Path::new("runtime-binary"),
                    &binary_blake3,
                ),
            );
        }
        Manifest {
            schema_version: 2,
            experiment_id: "experiment".to_owned(),
            random_seed: 7,
            primary_model: "provider/model".to_owned(),
            executor_model: String::new(),
            timeout_seconds: 30,
            arms,
            tasks: vec![Task {
                id: "task".to_owned(),
                repository: PathBuf::from("task-repository"),
                revision,
                prompt: "fix the task".to_owned(),
                success_command: vec!["test-command".to_owned()],
                success_command_executable_blake3: None,
            }],
        }
    }

    fn run_git(repository: &Path, args: &[&str]) {
        let status = Command::new("git")
            .arg("-C")
            .arg(repository)
            .args(args)
            .status()
            .expect("run git");
        assert!(status.success());
    }

    fn git_output(repository: &Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .arg("-C")
            .arg(repository)
            .args(args)
            .output()
            .expect("run git");
        assert!(output.status.success());
        String::from_utf8(output.stdout)
            .expect("git UTF-8")
            .trim()
            .to_owned()
    }
}
