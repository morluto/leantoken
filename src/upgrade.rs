//! Package-manager-aware updates for persistent LeanToken installations.

use std::{
    env,
    io::{IsTerminal, Write},
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
};

use dialoguer::Confirm;
use semver::Version;
use serde::Serialize;

use crate::{Error, Result};

const PACKAGE_NAME: &str = "leantoken";
const NPM_PACKAGE: &str = "leantoken@latest";
const GIT_REPOSITORY: &str = "https://github.com/morluto/leantoken";

/// User-selected update behavior.
#[derive(Debug, Clone, Copy)]
pub struct UpgradeOptions {
    /// Only check for a newer release.
    pub check: bool,
    /// Skip confirmation for a persistent installation.
    pub yes: bool,
    /// Emit one JSON report.
    pub json: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum InstallContext {
    Npx,
    GlobalNpm,
    Cargo,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CommandSpec {
    program: &'static str,
    arguments: Vec<String>,
}

impl CommandSpec {
    fn new(program: &'static str, arguments: impl IntoIterator<Item = impl Into<String>>) -> Self {
        Self {
            program,
            arguments: arguments.into_iter().map(Into::into).collect(),
        }
    }

    fn display(&self) -> String {
        std::iter::once(self.program)
            .chain(self.arguments.iter().map(String::as_str))
            .collect::<Vec<_>>()
            .join(" ")
    }
}

#[derive(Debug, Serialize)]
struct UpgradeReport {
    status: UpgradeStatus,
    context: InstallContext,
    current_version: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    latest_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    command: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
enum UpgradeStatus {
    CheckFailed,
    UpToDate,
    Ephemeral,
    UpdateAvailable,
    Updated,
    Skipped,
    ManualUpdateRequired,
}

/// Check for and optionally install the latest LeanToken release.
///
/// # Errors
///
/// Returns an error when confirmation cannot be read or the selected package
/// manager fails to install the release.
pub fn run(options: UpgradeOptions) -> Result<()> {
    let executable = env::current_exe()?.canonicalize()?;
    let context = detect_current_context(&executable);
    let latest = latest_version(context);
    let Some(latest) = latest else {
        return print_report(
            UpgradeReport {
                status: UpgradeStatus::CheckFailed,
                context,
                current_version: env!("CARGO_PKG_VERSION"),
                latest_version: None,
                command: None,
            },
            options.json,
        );
    };

    let Some(update_available) = version_update_available(env!("CARGO_PKG_VERSION"), &latest)
    else {
        return print_report(
            UpgradeReport {
                status: UpgradeStatus::CheckFailed,
                context,
                current_version: env!("CARGO_PKG_VERSION"),
                latest_version: Some(latest),
                command: None,
            },
            options.json,
        );
    };

    if !update_available {
        return print_report(
            UpgradeReport {
                status: UpgradeStatus::UpToDate,
                context,
                current_version: env!("CARGO_PKG_VERSION"),
                latest_version: Some(latest),
                command: None,
            },
            options.json,
        );
    }

    let command = upgrade_command(context, Some(&latest));

    if context == InstallContext::Npx {
        let refresh_command = npx_refresh_command(&latest);
        return print_report(
            UpgradeReport {
                status: UpgradeStatus::Ephemeral,
                context,
                current_version: env!("CARGO_PKG_VERSION"),
                latest_version: Some(latest),
                command: Some(refresh_command),
            },
            options.json,
        );
    }

    let Some(command) = command else {
        return print_report(
            UpgradeReport {
                status: UpgradeStatus::ManualUpdateRequired,
                context,
                current_version: env!("CARGO_PKG_VERSION"),
                latest_version: Some(latest),
                command: None,
            },
            options.json,
        );
    };

    if options.check || (!options.yes && (!std::io::stdin().is_terminal() || options.json)) {
        return print_report(
            UpgradeReport {
                status: UpgradeStatus::UpdateAvailable,
                context,
                current_version: env!("CARGO_PKG_VERSION"),
                latest_version: Some(latest),
                command: Some(command.display()),
            },
            options.json,
        );
    }

    if !options.yes
        && !Confirm::new()
            .with_prompt(format!("Run `{}` now?", command.display()))
            .default(true)
            .interact()
            .map_err(|error| {
                Error::InternalFailure(format!("update confirmation failed: {error}"))
            })?
    {
        return print_report(
            UpgradeReport {
                status: UpgradeStatus::Skipped,
                context,
                current_version: env!("CARGO_PKG_VERSION"),
                latest_version: Some(latest),
                command: Some(command.display()),
            },
            options.json,
        );
    }

    run_command(&command, options.json)?;
    print_report(
        UpgradeReport {
            status: UpgradeStatus::Updated,
            context,
            current_version: env!("CARGO_PKG_VERSION"),
            latest_version: Some(latest),
            command: Some(command.display()),
        },
        options.json,
    )
}

fn version_update_available(current: &str, latest: &str) -> Option<bool> {
    let current = Version::parse(current).ok()?;
    let latest = Version::parse(latest).ok()?;
    Some(current.cmp_precedence(&latest).is_lt())
}

fn detect_current_context(executable: &Path) -> InstallContext {
    let npm_command = env::var("npm_command").unwrap_or_default();
    let lifecycle = env::var("npm_lifecycle_event").unwrap_or_default();
    if npm_command == "exec" || lifecycle == "npx" {
        return InstallContext::Npx;
    }

    if path_contains(executable, ".cargo") {
        return InstallContext::Cargo;
    }

    let npm_root = command_stdout("npm", &["root", "--global"]).map(PathBuf::from);
    detect_install_context(executable, false, npm_root.as_deref())
}

fn detect_install_context(
    executable: &Path,
    ephemeral_npx: bool,
    global_npm_root: Option<&Path>,
) -> InstallContext {
    if ephemeral_npx {
        return InstallContext::Npx;
    }
    if path_contains(executable, ".cargo") {
        return InstallContext::Cargo;
    }
    if global_npm_root.is_some_and(|root| executable.starts_with(root)) {
        return InstallContext::GlobalNpm;
    }
    InstallContext::Unknown
}

fn path_contains(path: &Path, component: &str) -> bool {
    path.components()
        .any(|part| part.as_os_str() == std::ffi::OsStr::new(component))
}

fn upgrade_command(context: InstallContext, latest_version: Option<&str>) -> Option<CommandSpec> {
    match context {
        InstallContext::GlobalNpm => Some(CommandSpec::new(
            "npm",
            ["install", "--global", NPM_PACKAGE],
        )),
        InstallContext::Cargo => {
            let mut arguments = vec!["install".into(), "--git".into(), GIT_REPOSITORY.into()];
            if let Some(version) = latest_version {
                arguments.extend(["--tag".into(), format!("v{version}")]);
            }
            arguments.push("--force".into());
            Some(CommandSpec::new("cargo", arguments))
        }
        InstallContext::Npx | InstallContext::Unknown => None,
    }
}

fn npx_refresh_command(version: &str) -> String {
    format!("npx --yes leantoken@{version} setup --refresh --yes")
}

fn latest_version(context: InstallContext) -> Option<String> {
    match context {
        InstallContext::Cargo => command_stdout(
            "git",
            &[
                "ls-remote",
                "--tags",
                "--refs",
                "--sort=-v:refname",
                GIT_REPOSITORY,
            ],
        )
        .and_then(|output| {
            output
                .lines()
                .next()?
                .split("refs/tags/v")
                .nth(1)
                .map(str::to_owned)
        }),
        InstallContext::Npx | InstallContext::GlobalNpm | InstallContext::Unknown => {
            command_stdout("npm", &["view", PACKAGE_NAME, "version", "--json"])
                .and_then(|value| serde_json::from_str::<String>(&value).ok())
        }
    }
}

fn command_stdout(program: &str, arguments: &[&str]) -> Option<String> {
    let output = Command::new(program).args(arguments).output().ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

fn run_command(command: &CommandSpec, capture_output: bool) -> Result<()> {
    let mut child = Command::new(command.program);
    child.args(&command.arguments);
    if capture_output {
        let output = child.output().map_err(|error| {
            Error::InternalFailure(format!("failed to run {}: {error}", command.program))
        })?;
        require_success(command, &output)
    } else {
        let status = child
            .stdin(Stdio::inherit())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .status()?;
        if status.success() {
            Ok(())
        } else {
            Err(Error::InternalFailure(format!(
                "update command failed: {}",
                command.display()
            )))
        }
    }
}

fn require_success(command: &CommandSpec, output: &Output) -> Result<()> {
    if output.status.success() {
        return Ok(());
    }
    let detail = String::from_utf8_lossy(&output.stderr);
    Err(Error::InternalFailure(format!(
        "update command failed: {}{}{}",
        command.display(),
        if detail.trim().is_empty() { "" } else { ": " },
        detail.trim()
    )))
}

fn print_report(report: UpgradeReport, json: bool) -> Result<()> {
    if json {
        println!("{}", serde_json::to_string(&report)?);
        return Ok(());
    }

    match report.status {
        UpgradeStatus::UpToDate => {
            println!("LeanToken is up to date (v{}).", report.current_version)
        }
        UpgradeStatus::Ephemeral => {
            println!(
                "Update available: v{} -> v{}",
                report.current_version,
                report.latest_version.as_deref().unwrap_or("unknown")
            );
            println!("You are running LeanToken through npx; nothing is installed globally.");
            if let Some(command) = report.command {
                println!("Refresh existing MCP entries with: {command}");
            }
            println!("Or install the shell command with: npm install --global leantoken@latest");
        }
        UpgradeStatus::UpdateAvailable => {
            println!(
                "Update available: v{} -> v{}",
                report.current_version,
                report.latest_version.as_deref().unwrap_or("unknown")
            );
            if let Some(command) = report.command {
                println!("Run: {command}");
            }
        }
        UpgradeStatus::Updated => println!(
            "LeanToken updated to v{}.",
            report.latest_version.as_deref().unwrap_or("latest")
        ),
        UpgradeStatus::Skipped => println!("Update skipped."),
        UpgradeStatus::ManualUpdateRequired => print_manual_commands(),
        UpgradeStatus::CheckFailed => {
            println!("Could not check for LeanToken updates right now.");
            print_manual_commands();
        }
    }
    std::io::stdout().flush()?;
    Ok(())
}

fn print_manual_commands() {
    println!("Update manually with one of:");
    println!("  npm install --global {NPM_PACKAGE}");
    println!("  cargo install --git {GIT_REPOSITORY} --force");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn distinguishes_ephemeral_global_npm_cargo_and_unknown() {
        assert_eq!(
            detect_install_context(Path::new("/tmp/leantoken"), true, None),
            InstallContext::Npx
        );
        assert_eq!(
            detect_install_context(
                Path::new("/usr/lib/node_modules/leantoken/bin/leantoken"),
                false,
                Some(Path::new("/usr/lib/node_modules"))
            ),
            InstallContext::GlobalNpm
        );
        assert_eq!(
            detect_install_context(Path::new("/home/me/.cargo/bin/leantoken"), false, None),
            InstallContext::Cargo
        );
        assert_eq!(
            detect_install_context(Path::new("/usr/local/bin/leantoken"), false, None),
            InstallContext::Unknown
        );
    }

    #[test]
    fn upgrade_commands_target_the_selected_release() {
        assert_eq!(upgrade_command(InstallContext::Npx, Some("1.2.3")), None);
        assert_eq!(
            upgrade_command(InstallContext::GlobalNpm, Some("1.2.3"))
                .unwrap()
                .display(),
            "npm install --global leantoken@latest"
        );
        assert_eq!(
            upgrade_command(InstallContext::Cargo, Some("1.2.3"))
                .unwrap()
                .display(),
            "cargo install --git https://github.com/morluto/leantoken --tag v1.2.3 --force"
        );
    }

    #[test]
    fn npx_refresh_command_uses_the_resolved_exact_version() {
        assert_eq!(
            npx_refresh_command("1.2.3"),
            "npx --yes leantoken@1.2.3 setup --refresh --yes"
        );
        assert!(!npx_refresh_command("1.2.3").contains("@latest"));
    }

    #[test]
    fn upgrade_requires_a_newer_semantic_version() {
        assert_eq!(version_update_available("0.1.12", "0.1.12"), Some(false));
        assert_eq!(version_update_available("0.1.12", "0.1.13"), Some(true));
        assert_eq!(
            version_update_available("0.2.0-beta.1", "0.1.12"),
            Some(false)
        );
        assert_eq!(
            version_update_available("0.2.0-beta.1", "0.2.0"),
            Some(true)
        );
        assert_eq!(
            version_update_available("0.1.12+local", "0.1.12+remote"),
            Some(false)
        );
        assert_eq!(version_update_available("0.1.12", "not-semver"), None);
    }
}
