use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use clap::Parser;
use serde::Serialize;

const RECEIPT_FILE: &str = "validation-receipt.json";

#[derive(Debug, Parser)]
#[command(about = "Validate one frozen SWE-bench patch with the official Docker harness")]
struct Args {
    #[arg(long)]
    dataset: PathBuf,
    #[arg(long)]
    dataset_blake3: String,
    #[arg(long)]
    instance_id: String,
    #[arg(long)]
    harness_repository: PathBuf,
    #[arg(long)]
    harness_revision: String,
    #[arg(long)]
    python: PathBuf,
    #[arg(long)]
    python_blake3: String,
    #[arg(long)]
    uv: PathBuf,
    #[arg(long)]
    uv_blake3: String,
    #[arg(long)]
    environment_blake3: String,
    #[arg(long)]
    docker_image: String,
    #[arg(long)]
    docker_image_digest: String,
    #[arg(long, default_value = "swebench")]
    namespace: String,
    #[arg(long, default_value_t = 1800)]
    timeout_seconds: u64,
}

#[derive(Debug, Serialize)]
struct ValidationReceipt {
    schema_version: u32,
    experiment_id: String,
    manifest_blake3: String,
    task_id: String,
    repetition: usize,
    arm: String,
    instance_id: String,
    dataset_blake3: String,
    harness_revision: String,
    python_blake3: String,
    uv_blake3: String,
    environment_blake3: String,
    docker_image: String,
    docker_image_digest: String,
    prediction_blake3: String,
    official_report_blake3: Option<String>,
    official_exit_code: Option<i32>,
    completed: bool,
    resolved: bool,
}

fn main() -> Result<(), Box<dyn Error>> {
    let args = Args::parse();
    validate_hex(&args.dataset_blake3, 64, "dataset_blake3")?;
    validate_hex(&args.harness_revision, 40, "harness_revision")?;
    validate_hex(&args.python_blake3, 64, "python_blake3")?;
    validate_hex(&args.uv_blake3, 64, "uv_blake3")?;
    validate_hex(&args.environment_blake3, 64, "environment_blake3")?;
    validate_sha256(&args.docker_image_digest, "docker_image_digest")?;
    if args.docker_image.trim().is_empty()
        || args.namespace.trim().is_empty()
        || args.timeout_seconds <= 10
    {
        return Err("Docker image, namespace, and timeout must be usable".into());
    }

    let artifacts = required_env_path("LEANTOKEN_MODEL_AB_ARTIFACTS_DIRECTORY")?;
    let experiment_id = required_env("LEANTOKEN_MODEL_AB_EXPERIMENT_ID")?;
    let manifest_blake3 = required_env("LEANTOKEN_MODEL_AB_MANIFEST_BLAKE3")?;
    validate_hex(&manifest_blake3, 64, "manifest_blake3")?;
    let task_id = required_env("LEANTOKEN_MODEL_AB_TASK_ID")?;
    let repetition = required_env("LEANTOKEN_MODEL_AB_REPETITION")?.parse::<usize>()?;
    let arm = required_env("LEANTOKEN_MODEL_AB_ARM")?;
    if task_id != args.instance_id {
        return Err("model A/B task ID does not match the SWE-bench instance ID".into());
    }
    let repository = std::env::current_dir()?.canonicalize()?;
    let dataset = args.dataset.canonicalize()?;
    let harness_repository = args.harness_repository.canonicalize()?;
    if !args.python.is_absolute() {
        return Err("python path must be absolute".into());
    }
    let python = args.python.clone();
    let uv = args.uv.canonicalize()?;

    verify_file_hash(&dataset, &args.dataset_blake3, "dataset")?;
    verify_clean_revision(&harness_repository, &args.harness_revision)?;
    verify_file_hash(&python, &args.python_blake3, "python")?;
    verify_file_hash(&uv, &args.uv_blake3, "uv")?;
    verify_environment(&uv, &python, &args.environment_blake3)?;
    verify_docker_image(&args.docker_image, &args.docker_image_digest)?;

    let patch = capture_patch(&repository)?;
    let prediction_blake3 = blake3::hash(&patch).to_hex().to_string();
    let work = tempfile::tempdir()?;
    let predictions_path = work.path().join("predictions.jsonl");
    let prediction = serde_json::json!({
        "instance_id": &args.instance_id,
        "model_name_or_path": "leantoken-model-ab",
        "model_patch": String::from_utf8(patch)?,
    });
    fs::write(
        &predictions_path,
        format!("{}\n", serde_json::to_string(&prediction)?),
    )?;

    let run_id = format!("{experiment_id}-{task_id}-r{repetition}-{arm}");
    let output = Command::new(&python)
        .args(["-m", "swebench.harness.run_evaluation"])
        .arg("--dataset_name")
        .arg(&dataset)
        .args(["--split", "test", "--instance_ids"])
        .arg(&args.instance_id)
        .arg("--predictions_path")
        .arg(&predictions_path)
        .args(["--max_workers", "1", "--timeout"])
        .arg(args.timeout_seconds.to_string())
        .args(["--cache_level", "instance", "--clean", "false", "--run_id"])
        .arg(&run_id)
        .arg("--namespace")
        .arg(&args.namespace)
        .current_dir(work.path())
        .output()?;

    fs::write(artifacts.join("validation-stdout.log"), &output.stdout)?;
    fs::write(artifacts.join("validation-stderr.log"), &output.stderr)?;
    let report_path = work
        .path()
        .join(format!("leantoken-model-ab.{run_id}.json"));
    let (official_report_blake3, completed, resolved) = capture_official_reports(
        work.path(),
        &report_path,
        &artifacts,
        &run_id,
        &args.instance_id,
    )?;
    let receipt = ValidationReceipt {
        schema_version: 1,
        experiment_id,
        manifest_blake3,
        task_id,
        repetition,
        arm,
        instance_id: args.instance_id,
        dataset_blake3: args.dataset_blake3,
        harness_revision: args.harness_revision,
        python_blake3: args.python_blake3,
        uv_blake3: args.uv_blake3,
        environment_blake3: args.environment_blake3,
        docker_image: args.docker_image,
        docker_image_digest: args.docker_image_digest,
        prediction_blake3,
        official_report_blake3,
        official_exit_code: output.status.code(),
        completed,
        resolved,
    };
    fs::write(
        artifacts.join(RECEIPT_FILE),
        serde_json::to_vec_pretty(&receipt)?,
    )?;

    if !output.status.success() {
        return Err(format!("official SWE-bench harness exited with {}", output.status).into());
    }
    if !completed || !resolved {
        return Err("official SWE-bench harness did not resolve the instance".into());
    }
    Ok(())
}

fn capture_official_reports(
    work: &Path,
    report_path: &Path,
    artifacts: &Path,
    run_id: &str,
    instance_id: &str,
) -> Result<(Option<String>, bool, bool), Box<dyn Error>> {
    let Some(report) = read_optional(report_path)? else {
        return Ok((None, false, false));
    };
    fs::write(artifacts.join("validation-report.json"), &report)?;
    let value: serde_json::Value = serde_json::from_slice(&report)?;
    let completed = value["completed_ids"]
        .as_array()
        .is_some_and(|ids| ids.iter().any(|id| id.as_str() == Some(instance_id)));
    let resolved = value["resolved_ids"]
        .as_array()
        .is_some_and(|ids| ids.iter().any(|id| id.as_str() == Some(instance_id)));
    let instance_root = work
        .join("logs/run_evaluation")
        .join(run_id)
        .join("leantoken-model-ab")
        .join(instance_id);
    copy_if_present(
        &instance_root.join("report.json"),
        &artifacts.join("validation-instance-report.json"),
    )?;
    copy_if_present(
        &instance_root.join("test_output.txt"),
        &artifacts.join("validation-test-output.log"),
    )?;
    Ok((
        Some(blake3::hash(&report).to_hex().to_string()),
        completed,
        resolved,
    ))
}

fn capture_patch(repository: &Path) -> Result<Vec<u8>, Box<dyn Error>> {
    let temporary_index = tempfile::tempdir()?;
    let index = temporary_index.path().join("index");
    let read_tree = git_with_index(repository, &index, &["read-tree", "HEAD"])?;
    require_success(&read_tree, "git read-tree")?;
    let intent = git_with_index(repository, &index, &["add", "--intent-to-add", "--all"])?;
    require_success(&intent, "git add --intent-to-add")?;
    let diff = git_with_index(
        repository,
        &index,
        &["diff", "--binary", "--full-index", "HEAD", "--"],
    )?;
    require_success(&diff, "git diff")?;
    Ok(diff.stdout)
}

fn git_with_index(repository: &Path, index: &Path, args: &[&str]) -> std::io::Result<Output> {
    Command::new("git")
        .arg("-C")
        .arg(repository)
        .args(args)
        .env("GIT_INDEX_FILE", index)
        .output()
}

fn verify_clean_revision(repository: &Path, revision: &str) -> Result<(), Box<dyn Error>> {
    let head = command_stdout(
        Command::new("git")
            .arg("-C")
            .arg(repository)
            .args(["rev-parse", "HEAD"]),
    )?;
    if head.trim() != revision {
        return Err("official SWE-bench harness revision mismatch".into());
    }
    let status = command_stdout(Command::new("git").arg("-C").arg(repository).args([
        "status",
        "--porcelain=v1",
        "--untracked-files=all",
    ]))?;
    if !status.trim().is_empty() {
        return Err("official SWE-bench harness worktree is dirty".into());
    }
    Ok(())
}

fn verify_environment(uv: &Path, python: &Path, expected: &str) -> Result<(), Box<dyn Error>> {
    let output = Command::new(uv)
        .args(["pip", "freeze", "--python"])
        .arg(python)
        .output()?;
    require_success(&output, "uv pip freeze")?;
    if blake3::hash(&output.stdout).to_hex().as_str() != expected {
        return Err("SWE-bench Python environment hash mismatch".into());
    }
    Ok(())
}

fn verify_docker_image(image: &str, digest: &str) -> Result<(), Box<dyn Error>> {
    let output = Command::new("docker")
        .args([
            "image",
            "inspect",
            image,
            "--format",
            "{{json .RepoDigests}}",
        ])
        .output()?;
    require_success(&output, "docker image inspect")?;
    let digests: Vec<String> = serde_json::from_slice(&output.stdout)?;
    if !digests.iter().any(|value| value.ends_with(digest)) {
        return Err(format!("Docker image {image} does not match {digest}").into());
    }
    Ok(())
}

fn verify_file_hash(path: &Path, expected: &str, label: &str) -> Result<(), Box<dyn Error>> {
    if hash_file(path)? != expected {
        return Err(format!("{label} hash mismatch").into());
    }
    Ok(())
}

fn hash_file(path: &Path) -> Result<String, Box<dyn Error>> {
    Ok(blake3::hash(&fs::read(path)?).to_hex().to_string())
}

fn command_stdout(command: &mut Command) -> Result<String, Box<dyn Error>> {
    let output = command.output()?;
    require_success(&output, "command")?;
    Ok(String::from_utf8(output.stdout)?)
}

fn require_success(output: &Output, label: &str) -> Result<(), Box<dyn Error>> {
    if !output.status.success() {
        return Err(format!(
            "{label} failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )
        .into());
    }
    Ok(())
}

fn required_env(name: &str) -> Result<String, Box<dyn Error>> {
    std::env::var(name).map_err(|_| format!("missing required environment variable {name}").into())
}

fn required_env_path(name: &str) -> Result<PathBuf, Box<dyn Error>> {
    Ok(PathBuf::from(required_env(name)?).canonicalize()?)
}

fn validate_hex(value: &str, length: usize, name: &str) -> Result<(), Box<dyn Error>> {
    if value.len() != length
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        return Err(format!("invalid {name}").into());
    }
    Ok(())
}

fn validate_sha256(value: &str, name: &str) -> Result<(), Box<dyn Error>> {
    let Some(digest) = value.strip_prefix("sha256:") else {
        return Err(format!("invalid {name}").into());
    };
    validate_hex(digest, 64, name)
}

fn read_optional(path: &Path) -> Result<Option<Vec<u8>>, Box<dyn Error>> {
    match fs::read(path) {
        Ok(value) => Ok(Some(value)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn copy_if_present(source: &Path, destination: &Path) -> Result<(), Box<dyn Error>> {
    if let Some(value) = read_optional(source)? {
        fs::write(destination, value)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validation_receipt_uses_stable_file_name() {
        assert_eq!(RECEIPT_FILE, "validation-receipt.json");
    }

    #[test]
    fn hex_validation_rejects_short_or_uppercase_values() {
        assert!(validate_hex(&"a".repeat(64), 64, "digest").is_ok());
        assert!(validate_hex(&"a".repeat(63), 64, "digest").is_err());
        assert!(validate_hex(&"A".repeat(64), 64, "digest").is_err());
        assert!(validate_sha256(&format!("sha256:{}", "a".repeat(64)), "digest").is_ok());
        assert!(validate_sha256(&format!("sha256:{}", "a".repeat(63)), "digest").is_err());
    }

    #[test]
    fn patch_capture_includes_tracked_and_untracked_files() {
        let repository = tempfile::tempdir().expect("repository");
        run_git(repository.path(), &["init", "--quiet"]);
        run_git(
            repository.path(),
            &["config", "user.name", "LeanToken Test"],
        );
        run_git(
            repository.path(),
            &["config", "user.email", "leantoken@example.invalid"],
        );
        fs::write(repository.path().join("tracked.txt"), "before\n").expect("tracked fixture");
        run_git(repository.path(), &["add", "tracked.txt"]);
        run_git(repository.path(), &["commit", "--quiet", "-m", "fixture"]);
        fs::write(repository.path().join("tracked.txt"), "after\n").expect("tracked edit");
        fs::write(repository.path().join("new.txt"), "new\n").expect("untracked edit");

        let patch = String::from_utf8(capture_patch(repository.path()).expect("capture patch"))
            .expect("patch UTF-8");
        assert!(patch.contains("a/tracked.txt"));
        assert!(patch.contains("b/new.txt"));
        assert!(
            Command::new("git")
                .arg("-C")
                .arg(repository.path())
                .args(["diff", "--cached", "--quiet"])
                .status()
                .expect("inspect real index")
                .success(),
            "validator patch capture must not mutate the task index"
        );
    }

    fn run_git(repository: &Path, args: &[&str]) {
        assert!(
            Command::new("git")
                .arg("-C")
                .arg(repository)
                .args(args)
                .status()
                .expect("run git")
                .success()
        );
    }
}
