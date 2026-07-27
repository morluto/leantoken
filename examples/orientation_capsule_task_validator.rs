use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use clap::Parser;
use serde::Serialize;

const RECEIPT_FILE: &str = "validation-receipt.json";

#[derive(Debug, Parser)]
#[command(about = "Validate one local orientation-capsule ARB task")]
struct Args {
    #[arg(long)]
    task: String,
    #[arg(long)]
    cargo: PathBuf,
    #[arg(long)]
    cargo_blake3: String,
    #[arg(long)]
    python: PathBuf,
    #[arg(long)]
    python_blake3: String,
}

#[derive(Debug, Serialize)]
struct ValidationReceipt {
    schema_version: u32,
    experiment_id: String,
    manifest_blake3: String,
    task_id: String,
    repetition: usize,
    arm: String,
    task: String,
    cargo_blake3: String,
    python_blake3: String,
    repository_revision: String,
    patch_blake3: String,
    validation_exit_code: Option<i32>,
    resolved: bool,
}

fn main() -> Result<(), Box<dyn Error>> {
    let args = Args::parse();
    if !matches!(args.task.as_str(), "clap" | "click") {
        return Err("task must be clap or click".into());
    }
    validate_hash(&args.cargo_blake3)?;
    validate_hash(&args.python_blake3)?;
    verify_file_hash(&args.cargo, &args.cargo_blake3)?;
    verify_file_hash(&args.python, &args.python_blake3)?;

    let artifacts = required_env_path("LEANTOKEN_MODEL_AB_ARTIFACTS_DIRECTORY")?;
    let experiment_id = required_env("LEANTOKEN_MODEL_AB_EXPERIMENT_ID")?;
    let manifest_blake3 = required_env("LEANTOKEN_MODEL_AB_MANIFEST_BLAKE3")?;
    validate_hash(&manifest_blake3)?;
    let task_id = required_env("LEANTOKEN_MODEL_AB_TASK_ID")?;
    let repetition = required_env("LEANTOKEN_MODEL_AB_REPETITION")?.parse::<usize>()?;
    let arm = required_env("LEANTOKEN_MODEL_AB_ARM")?;
    let repository = std::env::current_dir()?.canonicalize()?;
    let repository_revision = git_stdout(&repository, &["rev-parse", "HEAD"])?;
    let patch = git_bytes(
        &repository,
        &["diff", "--binary", "--full-index", "HEAD", "--"],
    )?;
    let patch_blake3 = blake3::hash(&patch).to_hex().to_string();

    let output = match args.task.as_str() {
        "clap" => validate_clap(&repository, &args.cargo)?,
        "click" => validate_click(&repository, &args.python)?,
        _ => unreachable!(),
    };
    fs::write(artifacts.join("validation-stdout.log"), &output.stdout)?;
    fs::write(artifacts.join("validation-stderr.log"), &output.stderr)?;
    let resolved = output.status.success();
    let receipt = ValidationReceipt {
        schema_version: 1,
        experiment_id,
        manifest_blake3,
        task_id,
        repetition,
        arm,
        task: args.task,
        cargo_blake3: args.cargo_blake3,
        python_blake3: args.python_blake3,
        repository_revision: repository_revision.trim().to_owned(),
        patch_blake3,
        validation_exit_code: output.status.code(),
        resolved,
    };
    fs::write(
        artifacts.join(RECEIPT_FILE),
        serde_json::to_vec_pretty(&receipt)?,
    )?;
    if !resolved {
        return Err(format!("orientation task validator exited with {}", output.status).into());
    }
    Ok(())
}

fn validate_clap(repository: &Path, cargo: &Path) -> Result<Output, Box<dyn Error>> {
    let project = tempfile::tempdir()?;
    fs::create_dir(project.path().join("src"))?;
    let dependency_path = serde_json::to_string(repository.to_string_lossy().as_ref())?;
    fs::write(
        project.path().join("Cargo.toml"),
        format!(
            "[package]\nname = \"orientation-capsule-clap-validator\"\nversion = \"0.0.0\"\n\
             edition = \"2021\"\n\n[dependencies]\nclap = {{ path = {dependency_path} }}\n"
        ),
    )?;
    fs::write(
        project.path().join("src/lib.rs"),
        r#"#[cfg(test)]
mod tests {
    use clap::{Arg, ArgAction, Command};

    fn command() -> Command {
        Command::new("test")
            .arg(
                Arg::new("opt")
                    .long("opt")
                    .action(ArgAction::Set),
            )
            .arg(
                Arg::new("args")
                    .long("args")
                    .num_args(2)
                    .default_values_if("opt", "value", ["df1", "df2"]),
            )
    }

    #[test]
    fn conditional_multiple_defaults_are_injected_only_on_match() {
        let matches = command()
            .try_get_matches_from(["test", "--opt", "value"])
            .expect("matching command");
        let values = matches
            .get_many::<String>("args")
            .expect("conditional defaults")
            .map(String::as_str)
            .collect::<Vec<_>>();
        assert_eq!(values, ["df1", "df2"]);

        let matches = command()
            .try_get_matches_from(["test", "--opt", "other"])
            .expect("non-matching command");
        assert!(matches.get_many::<String>("args").is_none());
    }
}
"#,
    )?;
    Ok(Command::new(cargo)
        .args(["test", "--offline", "--quiet"])
        .current_dir(project.path())
        .env("CARGO_NET_OFFLINE", "true")
        .output()?)
}

fn validate_click(repository: &Path, python: &Path) -> Result<Output, Box<dyn Error>> {
    const SCRIPT: &str = r#"
import sys
from pathlib import Path

sys.path.insert(0, str(Path.cwd() / "src"))
import click
from click.testing import CliRunner

CASES = [
    ({"type": click.BOOL}, False, "False"),
    ({"type": click.BOOL}, True, "True"),
    ({"type": click.BOOL, "default": True}, False, "True"),
    ({"type": click.BOOL, "default": True}, True, "False"),
    ({"type": str}, False, ""),
    ({"type": str}, True, "True"),
]

runner = CliRunner()
for opts, pass_flag, expected in CASES:
    @click.command()
    @click.option("--foo", is_flag=True, **opts)
    def cmd(foo):
        click.echo(foo)

    result = runner.invoke(cmd, ["--foo"] if pass_flag else [])
    assert result.exception is None, result.output
    assert result.output == f"{expected}\n", (opts, pass_flag, result.output)
"#;
    Ok(Command::new(python)
        .args(["-c", SCRIPT])
        .current_dir(repository)
        .env("PYTHONDONTWRITEBYTECODE", "1")
        .output()?)
}

fn required_env(name: &str) -> Result<String, Box<dyn Error>> {
    let value = std::env::var(name)?;
    if value.trim().is_empty() {
        return Err(format!("{name} is empty").into());
    }
    Ok(value)
}

fn required_env_path(name: &str) -> Result<PathBuf, Box<dyn Error>> {
    Ok(PathBuf::from(required_env(name)?).canonicalize()?)
}

fn validate_hash(value: &str) -> Result<(), Box<dyn Error>> {
    if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("expected a 64-character hexadecimal hash".into());
    }
    Ok(())
}

fn verify_file_hash(path: &Path, expected: &str) -> Result<(), Box<dyn Error>> {
    let actual = blake3::hash(&fs::read(path)?).to_hex().to_string();
    if actual != expected {
        return Err(format!("executable hash mismatch: {}", path.display()).into());
    }
    Ok(())
}

fn git_stdout(repository: &Path, args: &[&str]) -> Result<String, Box<dyn Error>> {
    Ok(String::from_utf8(git_bytes(repository, args)?)?)
}

fn git_bytes(repository: &Path, args: &[&str]) -> Result<Vec<u8>, Box<dyn Error>> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repository)
        .args(args)
        .output()?;
    if !output.status.success() {
        return Err(format!("git command failed: {}", output.status).into());
    }
    Ok(output.stdout)
}
