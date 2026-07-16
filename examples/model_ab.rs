use std::collections::BTreeMap;
use std::error::Error;
use std::fs;
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
    primary_model: String,
    executor_model: String,
    tool_call_limit: usize,
    timeout_seconds: u64,
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

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
enum Arm {
    Filesystem,
    LeanTokenProgressive,
    LeanTokenContext,
    Prewalk,
}

impl Arm {
    const ALL: [Self; 4] = [
        Self::Filesystem,
        Self::LeanTokenProgressive,
        Self::LeanTokenContext,
        Self::Prewalk,
    ];

    fn retrieval_contract(self) -> &'static str {
        match self {
            Self::Filesystem => {
                "Use the host's ordinary path, search, and file-read tools. Do not use LeanToken."
            }
            Self::LeanTokenProgressive => {
                "Use LeanToken progressively: files, then outline/search, then exact read. Use context only when narrower retrieval remains uncertain."
            }
            Self::LeanTokenContext => {
                "Start with exactly one leantoken_context call. Use its evidence to complete the task without other LeanToken discovery calls."
            }
            Self::Prewalk => {
                "The primary model explores and creates a bounded todo list, then makes one valid edit. Transfer the complete trajectory and LeanToken evidence receipt to the executor model, which finishes and verifies the task."
            }
        }
    }
}

#[derive(Debug, Serialize)]
struct AdapterRequest<'a> {
    schema_version: u32,
    experiment_id: &'a str,
    repetition: usize,
    arm: Arm,
    primary_model: &'a str,
    executor_model: Option<&'a str>,
    tool_call_limit: usize,
    repository: &'a Path,
    revision: &'a str,
    task_id: &'a str,
    prompt: &'a str,
    success_command: &'a [String],
    retrieval_contract: &'static str,
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
    generated_at_unix_seconds: u64,
    primary_model: String,
    executor_model: String,
    tool_call_limit: usize,
    repetitions: usize,
    arms: BTreeMap<Arm, ArmAggregate>,
    task_arms: BTreeMap<String, BTreeMap<Arm, ArmAggregate>>,
    runs: Vec<RunReport>,
    limitations: Vec<&'static str>,
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

    let mut runs = Vec::new();
    for repetition in 1..=args.repetitions {
        for task in &manifest.tasks {
            verify_revision(&task.repository, &task.revision)?;
            for arm in Arm::ALL {
                let workspace = IsolatedWorkspace::create(&task.repository, &task.revision)?;
                let request = AdapterRequest {
                    schema_version: 1,
                    experiment_id: &manifest.experiment_id,
                    repetition,
                    arm,
                    primary_model: &manifest.primary_model,
                    executor_model: (arm == Arm::Prewalk)
                        .then_some(manifest.executor_model.as_str()),
                    tool_call_limit: manifest.tool_call_limit,
                    repository: workspace.path(),
                    revision: &task.revision,
                    task_id: &task.id,
                    prompt: &task.prompt,
                    success_command: &task.success_command,
                    retrieval_contract: arm.retrieval_contract(),
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
                            arm,
                            primary_model: manifest.primary_model.clone(),
                            executor_model: (arm == Arm::Prewalk)
                                .then(|| manifest.executor_model.clone()),
                            duration_ms: started.elapsed().as_millis(),
                            status: if failure.timed_out {
                                RunStatus::AdapterTimedOut
                            } else {
                                RunStatus::AdapterFailed
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
                if result.tool_calls > manifest.tool_call_limit {
                    runs.push(RunReport {
                        task_id: task.id.clone(),
                        repetition,
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
                            result.tool_calls, manifest.tool_call_limit
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

    let report = Report {
        schema_version: 2,
        experiment_id: manifest.experiment_id,
        manifest_blake3,
        generated_at_unix_seconds: SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs(),
        primary_model: manifest.primary_model,
        executor_model: manifest.executor_model,
        tool_call_limit: manifest.tool_call_limit,
        repetitions: args.repetitions,
        arms: aggregate(&runs),
        task_arms: aggregate_by_task(&runs),
        runs,
        limitations: vec![
            "Each run receives a fresh detached Git worktree at the frozen revision. The harness runs the frozen success command after the adapter; agent-reported success is retained only as a diagnostic.",
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
    if manifest.schema_version != 1 {
        return Err("unsupported model A/B manifest schema".into());
    }
    if manifest.experiment_id.trim().is_empty()
        || manifest.primary_model.trim().is_empty()
        || manifest.executor_model.trim().is_empty()
        || manifest.tasks.is_empty()
        || manifest.tool_call_limit == 0
        || manifest.timeout_seconds == 0
    {
        return Err("model A/B manifest has empty or zero required fields".into());
    }
    for task in &manifest.tasks {
        if task.id.trim().is_empty()
            || task.prompt.trim().is_empty()
            || task.revision.trim().is_empty()
            || task.success_command.is_empty()
        {
            return Err(format!("task {} is incomplete", task.id).into());
        }
    }
    Ok(())
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
    timed_out: bool,
    message: String,
}

impl AdapterFailure {
    fn new(message: impl Into<String>) -> Self {
        Self {
            timed_out: false,
            message: message.into(),
        }
    }

    fn from_error(error: impl Error) -> Self {
        Self::new(error.to_string())
    }

    fn timeout() -> Self {
        Self {
            timed_out: true,
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
    let output = Command::new("git")
        .args([
            "-C",
            repository.to_string_lossy().as_ref(),
            "rev-parse",
            "HEAD",
        ])
        .output()?;
    if !output.status.success() || String::from_utf8(output.stdout)?.trim() != revision {
        return Err(format!("{} is not checked out at {revision}", repository.display()).into());
    }
    Ok(())
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
    fn isolated_workspace_starts_at_revision_without_mutating_source() {
        let source = tempfile::tempdir().expect("source repository");
        run_git(source.path(), &["init"]);
        run_git(source.path(), &["config", "user.name", "LeanToken Test"]);
        run_git(
            source.path(),
            &["config", "user.email", "leantoken@example.invalid"],
        );
        fs::write(source.path().join("file.txt"), "base\n").expect("fixture");
        run_git(source.path(), &["add", "file.txt"]);
        run_git(source.path(), &["commit", "-m", "fixture"]);
        let revision = git_output(source.path(), &["rev-parse", "HEAD"]);

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
