use std::collections::{BTreeMap, HashSet};
use std::error::Error;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use clap::Parser;
use serde::{Deserialize, Serialize};
use statrs::statistics::Statistics;
use wait_timeout::ChildExt;

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
}

#[derive(Debug, Deserialize, Serialize)]
struct AdapterResult {
    task_success: bool,
    total_input_tokens: u64,
    #[serde(default)]
    total_output_tokens: u64,
    #[serde(default)]
    provider_reported_cost_usd: Option<f64>,
    tool_calls: usize,
    rereads: usize,
    #[serde(default)]
    reread_tokens: u64,
    failed_searches: usize,
    #[serde(default)]
    dead_end_reads: usize,
    #[serde(default)]
    provider_usage: serde_json::Value,
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
    total_input_tokens: u64,
    total_output_tokens: u64,
    provider_reported_cost_usd: Option<f64>,
    total_duration_ms: u128,
    tool_calls: usize,
    rereads: usize,
    reread_tokens: u64,
    failed_searches: usize,
    dead_end_reads: usize,
    input_tokens: SampleStatistics,
    output_tokens: SampleStatistics,
    duration_ms: SampleStatistics,
}

#[derive(Debug, Default, Serialize)]
struct SampleStatistics {
    samples: usize,
    mean: Option<f64>,
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

struct SourceIdentity {
    revision: String,
    dirty: bool,
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

    let mut runs = Vec::new();
    let mut schedules = Vec::new();
    for repetition in 1..=args.repetitions {
        for task in &manifest.tasks {
            verify_clean_revision(&task.repository, &task.revision)?;
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
                let request = AdapterRequest {
                    schema_version: 2,
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
                };
                let started = Instant::now();
                let invocation = invoke_adapter(
                    &args.adapter,
                    &args.adapter_args,
                    &request,
                    Duration::from_secs(manifest.timeout_seconds),
                );
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
                            result: None,
                        });
                        continue;
                    }
                };
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
                        result: Some(result),
                    });
                    continue;
                }
                let agent_reported_success = result.task_success;
                let validation_started = Instant::now();
                let validation = run_validation(
                    workspace.path(),
                    &task.success_command,
                    Duration::from_secs(manifest.timeout_seconds),
                );
                let validation_duration_ms = validation_started.elapsed().as_millis();
                let (status, validation_exit_code, error) = match validation {
                    Ok(Some(exit_code)) => (RunStatus::Completed, Some(exit_code), None),
                    Ok(None) => (
                        RunStatus::ValidationTimedOut,
                        None,
                        Some("success command timed out".to_owned()),
                    ),
                    Err(error) => (RunStatus::ValidationFailed, None, Some(error.to_string())),
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
                    result: Some(result),
                });
            }
        }
    }
    prepare_arm_definitions(&arm_definitions, &args.adapter, &adapter_binary_blake3)?;

    let report = Report {
        schema_version: 3,
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
        schedules,
        arms: aggregate(&runs),
        task_arms: aggregate_by_task(&runs),
        runs,
        limitations: vec![
            "Each run receives a fresh detached Git worktree at the frozen revision. The harness runs the frozen success command after the adapter; agent-reported success is retained only as a diagnostic.",
            "The random seed, actual arm order, clean source revisions, and verified harness, adapter, and runtime binary hashes are recorded for reproducibility; they do not establish provider determinism.",
            "Provider-reported usage is retained verbatim. Local tokenizer estimates must not be substituted for provider billing counts without an explicit label.",
            "One run per arm is a smoke test, not evidence of a stable pass-rate difference; use repeated runs and report variance for claims.",
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
    if manifest.schema_version != 2 {
        return Err("unsupported model A/B manifest schema".into());
    }
    if manifest.experiment_id.trim().is_empty()
        || manifest.primary_model.trim().is_empty()
        || manifest.tasks.is_empty()
        || manifest.timeout_seconds == 0
    {
        return Err("model A/B manifest has empty or zero required fields".into());
    }
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
        if !task_ids.insert(task.id.as_str()) {
            return Err(format!("duplicate task id: {}", task.id).into());
        }
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
    timeout: Duration,
) -> Result<Option<i32>, Box<dyn Error>> {
    let (program, args) = command.split_first().ok_or("success command is empty")?;
    let mut child = Command::new(program)
        .args(args)
        .current_dir(repository)
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
    let output = Command::new("git")
        .arg("-C")
        .arg(repository)
        .args(args)
        .stdin(Stdio::null())
        .output()?;
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
            item.total_input_tokens += result.total_input_tokens;
            item.total_output_tokens += result.total_output_tokens;
            item.tool_calls += result.tool_calls;
            item.rereads += result.rereads;
            item.reread_tokens += result.reread_tokens;
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
        let results = runs
            .iter()
            .filter(|run| run.arm == *arm)
            .filter_map(|run| run.result.as_ref())
            .collect::<Vec<_>>();
        item.provider_reported_cost_usd = (!results.is_empty())
            .then(|| {
                results
                    .iter()
                    .map(|result| result.provider_reported_cost_usd)
                    .collect::<Option<Vec<_>>>()
                    .map(|costs| costs.into_iter().sum())
            })
            .flatten();
        item.input_tokens = sample_statistics(
            results
                .iter()
                .map(|result| result.total_input_tokens as f64)
                .collect(),
        );
        item.output_tokens = sample_statistics(
            results
                .iter()
                .map(|result| result.total_output_tokens as f64)
                .collect(),
        );
        item.duration_ms = sample_statistics(
            runs.iter()
                .filter(|run| run.arm == *arm)
                .map(|run| run.duration_ms as f64)
                .collect(),
        );
    }
    aggregates
}

fn sample_statistics(samples: Vec<f64>) -> SampleStatistics {
    SampleStatistics {
        samples: samples.len(),
        mean: (!samples.is_empty()).then(|| samples.as_slice().mean()),
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

    #[test]
    fn sample_statistics_reports_mean_and_sample_variance() {
        let statistics = sample_statistics(vec![10.0, 20.0, 30.0]);

        assert_eq!(statistics.samples, 3);
        assert_eq!(statistics.mean, Some(20.0));
        assert_eq!(statistics.sample_variance, Some(100.0));
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
