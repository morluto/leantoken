use std::collections::BTreeMap;
use std::error::Error;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use clap::Parser;
use serde::{Deserialize, Serialize};
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
    provider_usage: serde_json::Value,
    #[serde(default)]
    evidence_receipt: Option<serde_json::Value>,
}

#[derive(Debug, Serialize)]
struct RunReport {
    task_id: String,
    repetition: usize,
    arm: Arm,
    primary_model: String,
    executor_model: Option<String>,
    duration_ms: u128,
    validation_duration_ms: u128,
    validation_exit_code: Option<i32>,
    agent_reported_success: bool,
    result: AdapterResult,
}

#[derive(Debug, Default, Serialize)]
struct ArmAggregate {
    runs: usize,
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
                let mut result = invoke_adapter(
                    &args.adapter,
                    &args.adapter_args,
                    &request,
                    Duration::from_secs(manifest.timeout_seconds),
                )?;
                if result.tool_calls > manifest.tool_call_limit {
                    return Err(
                        format!("{} {:?} exceeded the tool-call limit", task.id, arm).into(),
                    );
                }
                let agent_reported_success = result.task_success;
                let validation_started = Instant::now();
                let validation_exit_code = run_validation(
                    workspace.path(),
                    &task.success_command,
                    Duration::from_secs(manifest.timeout_seconds),
                )?;
                result.task_success = validation_exit_code == Some(0);
                runs.push(RunReport {
                    task_id: task.id.clone(),
                    repetition,
                    arm,
                    primary_model: manifest.primary_model.clone(),
                    executor_model: (arm == Arm::Prewalk).then(|| manifest.executor_model.clone()),
                    duration_ms: started.elapsed().as_millis(),
                    validation_duration_ms: validation_started.elapsed().as_millis(),
                    validation_exit_code,
                    agent_reported_success,
                    result,
                });
            }
        }
    }

    let report = Report {
        schema_version: manifest.schema_version,
        experiment_id: manifest.experiment_id,
        manifest_blake3,
        generated_at_unix_seconds: SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs(),
        primary_model: manifest.primary_model,
        executor_model: manifest.executor_model,
        tool_call_limit: manifest.tool_call_limit,
        repetitions: args.repetitions,
        arms: aggregate(&runs),
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
) -> Result<AdapterResult, Box<dyn Error>> {
    let mut child = Command::new(adapter)
        .args(adapter_args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()?;
    let input = serde_json::to_vec(request)?;
    child
        .stdin
        .take()
        .ok_or("adapter stdin unavailable")?
        .write_all(&input)?;
    let mut stdout = child.stdout.take().ok_or("adapter stdout unavailable")?;
    let output_reader = std::thread::spawn(move || {
        let mut output = Vec::new();
        stdout.read_to_end(&mut output)?;
        Ok::<_, std::io::Error>(output)
    });
    let status = child.wait_timeout(timeout)?;
    let Some(status) = status else {
        child.kill()?;
        let _ = child.wait();
        let _ = output_reader.join();
        return Err("model adapter timed out".into());
    };
    let output = output_reader
        .join()
        .map_err(|_| "adapter output reader panicked")??;
    if !status.success() {
        return Err(format!("model adapter exited with {status}").into());
    }
    Ok(serde_json::from_slice(&output)?)
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

fn aggregate(runs: &[RunReport]) -> BTreeMap<Arm, ArmAggregate> {
    let mut aggregates = BTreeMap::<Arm, ArmAggregate>::new();
    for run in runs {
        let item = aggregates.entry(run.arm).or_default();
        item.runs += 1;
        item.successes += usize::from(run.result.task_success);
        item.total_input_tokens += run.result.total_input_tokens;
        item.total_output_tokens += run.result.total_output_tokens;
        item.total_duration_ms += run.duration_ms;
        item.tool_calls += run.result.tool_calls;
        item.rereads += run.result.rereads;
        item.reread_tokens += run.result.reread_tokens;
        item.failed_searches += run.result.failed_searches;
        item.provider_reported_cost_usd = match (
            item.provider_reported_cost_usd,
            run.result.provider_reported_cost_usd,
        ) {
            (Some(total), Some(cost)) => Some(total + cost),
            (None, Some(cost)) if item.runs == 1 => Some(cost),
            _ => None,
        };
    }
    for item in aggregates.values_mut() {
        item.success_rate = if item.runs == 0 {
            0.0
        } else {
            item.successes as f64 / item.runs as f64
        };
    }
    aggregates
}

#[cfg(test)]
mod tests {
    use super::*;

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
