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

use crate::invocation::{InvocationIdentity, InvocationMetadata, PackageManager};
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

#[derive(Debug, Clone, Copy)]
enum UpgradeExecution {
    ReportOnly,
    Install(UpgradeConfirmation),
}

#[derive(Debug, Clone, Copy)]
enum UpgradeConfirmation {
    Prompt,
    Confirmed,
}

impl UpgradeOptions {
    fn execution(self) -> UpgradeExecution {
        if self.check || (!self.yes && (!std::io::stdin().is_terminal() || self.json)) {
            UpgradeExecution::ReportOnly
        } else if self.yes {
            UpgradeExecution::Install(UpgradeConfirmation::Confirmed)
        } else {
            UpgradeExecution::Install(UpgradeConfirmation::Prompt)
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum InstallContext {
    Npx,
    Npm,
    Pnpm,
    Yarn,
    GlobalNpm,
    Cargo,
    Unknown,
}

impl InstallContext {
    fn package_manager(self) -> Option<PackageManager> {
        match self {
            Self::Npx => Some(PackageManager::Npx),
            Self::Npm => Some(PackageManager::Npm),
            Self::Pnpm => Some(PackageManager::Pnpm),
            Self::Yarn => Some(PackageManager::Yarn),
            Self::GlobalNpm | Self::Cargo | Self::Unknown => None,
        }
    }
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
    #[serde(skip_serializing_if = "Option::is_none")]
    mcp_refresh_command: Option<String>,
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
    let execution = options.execution();
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
                mcp_refresh_command: None,
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
                mcp_refresh_command: None,
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
                mcp_refresh_command: None,
            },
            options.json,
        );
    }

    let command = upgrade_command(context, Some(&latest));

    if let Some(refresh_command) = ephemeral_refresh_command(context, &latest) {
        return print_report(
            UpgradeReport {
                status: UpgradeStatus::Ephemeral,
                context,
                current_version: env!("CARGO_PKG_VERSION"),
                latest_version: Some(latest),
                command: Some(refresh_command),
                mcp_refresh_command: None,
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
                mcp_refresh_command: None,
            },
            options.json,
        );
    };

    match execution {
        UpgradeExecution::ReportOnly => {
            return print_report(
                UpgradeReport {
                    status: UpgradeStatus::UpdateAvailable,
                    context,
                    current_version: env!("CARGO_PKG_VERSION"),
                    latest_version: Some(latest),
                    command: Some(command.display()),
                    mcp_refresh_command: None,
                },
                options.json,
            );
        }
        UpgradeExecution::Install(UpgradeConfirmation::Prompt)
            if !Confirm::new()
                .with_prompt(format!("Run `{}` now?", command.display()))
                .default(true)
                .interact()
                .map_err(|error| {
                    Error::SetupFailure(format!("update confirmation failed: {error}"))
                })? =>
        {
            return print_report(
                UpgradeReport {
                    status: UpgradeStatus::Skipped,
                    context,
                    current_version: env!("CARGO_PKG_VERSION"),
                    latest_version: Some(latest),
                    command: Some(command.display()),
                    mcp_refresh_command: None,
                },
                options.json,
            );
        }
        UpgradeExecution::Install(UpgradeConfirmation::Prompt | UpgradeConfirmation::Confirmed) => {
        }
    }
    run_command(&command, options.json)?;
    print_report(updated_report(context, latest, &command), options.json)
}

pub(crate) fn version_update_available(current: &str, latest: &str) -> Option<bool> {
    let current = Version::parse(current).ok()?;
    let latest = Version::parse(latest).ok()?;
    Some(current.cmp_precedence(&latest).is_lt())
}

pub(crate) fn latest_npm_version() -> Option<String> {
    command_stdout("npm", &["view", PACKAGE_NAME, "version", "--json"])
        .and_then(|value| serde_json::from_str::<String>(&value).ok())
}

fn detect_current_context(executable: &Path) -> InstallContext {
    let npm_command = env::var("npm_command").ok();
    let lifecycle = env::var("npm_lifecycle_event").ok();
    let npm_execpath = env::var_os("npm_execpath").map(PathBuf::from);
    let npm_node_execpath = env::var_os("npm_node_execpath").map(PathBuf::from);
    let yarn_version = env::var("YARN_VERSION").ok();
    let yarn_package_json = env::var_os("npm_package_json").map(PathBuf::from);
    let pnpm_script_src_dir = env::var_os("PNPM_SCRIPT_SRC_DIR")
        .or_else(|| env::var_os("PNPM_SCRIPT_SRC"))
        .map(PathBuf::from);
    let identity = InvocationIdentity::detect(
        executable,
        &env::args().collect::<Vec<_>>(),
        InvocationMetadata {
            npm_command: npm_command.as_deref(),
            npm_lifecycle_event: lifecycle.as_deref(),
            npm_execpath: npm_execpath.as_deref(),
            npm_node_execpath: npm_node_execpath.as_deref(),
            yarn_version: yarn_version.as_deref(),
            yarn_package_json: yarn_package_json.as_deref(),
            pnpm_script_src_dir: pnpm_script_src_dir.as_deref(),
        },
    );
    if let Some(package_manager) = identity.package_manager {
        return match package_manager {
            PackageManager::Npx => InstallContext::Npx,
            PackageManager::Npm => InstallContext::Npm,
            PackageManager::Pnpm => InstallContext::Pnpm,
            PackageManager::Yarn => InstallContext::Yarn,
        };
    }

    if path_contains(executable, ".cargo") {
        return InstallContext::Cargo;
    }

    let npm_root = command_stdout("npm", &["root", "--global"]).map(PathBuf::from);
    detect_install_context(executable, None, npm_root.as_deref())
}

fn detect_install_context(
    executable: &Path,
    ephemeral_manager: Option<PackageManager>,
    global_npm_root: Option<&Path>,
) -> InstallContext {
    if let Some(package_manager) = ephemeral_manager {
        return match package_manager {
            PackageManager::Npx => InstallContext::Npx,
            PackageManager::Npm => InstallContext::Npm,
            PackageManager::Pnpm => InstallContext::Pnpm,
            PackageManager::Yarn => InstallContext::Yarn,
        };
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
        InstallContext::GlobalNpm => {
            let package = format!(
                "{}@{}",
                PACKAGE_NAME,
                latest_version.expect("npm context resolves the latest version")
            );
            Some(CommandSpec::new(
                "npm",
                vec!["install".into(), "--global".into(), package],
            ))
        }
        InstallContext::Cargo => {
            let mut arguments = vec!["install".into(), "--git".into(), GIT_REPOSITORY.into()];
            if let Some(version) = latest_version {
                arguments.extend(["--tag".into(), format!("v{version}")]);
            }
            arguments.push("--force".into());
            Some(CommandSpec::new("cargo", arguments))
        }
        InstallContext::Npx
        | InstallContext::Npm
        | InstallContext::Pnpm
        | InstallContext::Yarn
        | InstallContext::Unknown => None,
    }
}

fn npx_refresh_command(version: &str) -> String {
    ephemeral_refresh_command(InstallContext::Npx, version).expect("npx is ephemeral")
}

fn ephemeral_refresh_command(context: InstallContext, version: &str) -> Option<String> {
    let package_manager = context.package_manager()?;
    let package = format!("leantoken@{version}");
    Some(match package_manager {
        PackageManager::Npx => format!("npx --yes {package} setup --refresh --yes"),
        PackageManager::Npm => {
            format!("npm exec --yes --package={package} -- leantoken setup --refresh --yes")
        }
        PackageManager::Pnpm => format!("pnpm dlx {package} setup --refresh --yes"),
        PackageManager::Yarn => format!("yarn dlx {package} setup --refresh --yes"),
    })
}

fn persistent_upgrade_guidance(refresh_command: &str) -> String {
    format!(
        "Existing MCP entries were left unchanged. Refresh them explicitly with: {refresh_command}"
    )
}

fn updated_report(
    context: InstallContext,
    latest_version: String,
    command: &CommandSpec,
) -> UpgradeReport {
    let mcp_refresh_command = npx_refresh_command(&latest_version);
    UpgradeReport {
        status: UpgradeStatus::Updated,
        context,
        current_version: env!("CARGO_PKG_VERSION"),
        latest_version: Some(latest_version),
        command: Some(command.display()),
        mcp_refresh_command: Some(mcp_refresh_command),
    }
}

fn latest_version(context: InstallContext) -> Option<String> {
    match context {
        InstallContext::Cargo => latest_cargo_version(),
        InstallContext::Npx
        | InstallContext::Npm
        | InstallContext::Pnpm
        | InstallContext::Yarn
        | InstallContext::GlobalNpm
        | InstallContext::Unknown => latest_npm_version(),
    }
}

fn latest_cargo_version() -> Option<String> {
    command_stdout("git", &["ls-remote", "--tags", "--refs", GIT_REPOSITORY])
        .and_then(select_latest_stable_tag)
}

fn select_latest_stable_tag(output: String) -> Option<String> {
    output
        .lines()
        .filter_map(|line| {
            let tag = line.rsplit("refs/tags/").next()?.trim();
            let version_str = tag.strip_prefix('v')?;
            let version = Version::parse(version_str).ok()?;

            if !version.pre.is_empty() {
                return None;
            }

            Some((version, version_str.to_owned()))
        })
        .max_by(|a, b| a.0.cmp(&b.0))
        .map(|(_, version_str)| version_str)
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
            Error::SetupFailure(format!("failed to run {}: {error}", command.program))
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
            Err(Error::SetupFailure(format!(
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
    Err(Error::SetupFailure(format!(
        "update command failed: {}{}{}",
        command.display(),
        if detail.trim().is_empty() { "" } else { ": " },
        detail.trim()
    )))
}

fn print_report(report: UpgradeReport, json: bool) -> Result<()> {
    let stdout = std::io::stdout();
    let mut output = stdout.lock();
    write_report(&mut output, report, json)?;
    output.flush()?;
    Ok(())
}

fn write_report(output: &mut impl Write, report: UpgradeReport, json: bool) -> Result<()> {
    if json {
        writeln!(output, "{}", serde_json::to_string(&report)?)?;
        return Ok(());
    }

    match report.status {
        UpgradeStatus::UpToDate => writeln!(
            output,
            "LeanToken is up to date (v{}).",
            report.current_version
        )?,
        UpgradeStatus::Ephemeral => {
            let package_manager = report
                .context
                .package_manager()
                .map(PackageManager::label)
                .unwrap_or("a package manager");
            writeln!(
                output,
                "Update available: v{} -> v{}",
                report.current_version,
                report.latest_version.as_deref().unwrap_or("unknown")
            )?;
            writeln!(
                output,
                "You are running LeanToken through {package_manager}; there is no persistent CLI to replace."
            )?;
            if let Some(command) = report.command {
                writeln!(
                    output,
                    "To upgrade existing MCP entries to v{}, run:",
                    report.latest_version.as_deref().unwrap_or("latest")
                )?;
                writeln!(output, "  {command}")?;
            }
        }
        UpgradeStatus::UpdateAvailable => {
            writeln!(
                output,
                "Update available: v{} -> v{}",
                report.current_version,
                report.latest_version.as_deref().unwrap_or("unknown")
            )?;
            if let Some(command) = report.command {
                writeln!(output, "Run: {command}")?;
            }
        }
        UpgradeStatus::Updated => {
            writeln!(
                output,
                "LeanToken updated to v{}.",
                report.latest_version.as_deref().unwrap_or("latest")
            )?;
            if let Some(command) = report.mcp_refresh_command {
                writeln!(output, "{}", persistent_upgrade_guidance(&command))?;
            }
        }
        UpgradeStatus::Skipped => writeln!(output, "Update skipped.")?,
        UpgradeStatus::ManualUpdateRequired => print_manual_commands(output)?,
        UpgradeStatus::CheckFailed => {
            writeln!(output, "Could not check for LeanToken updates right now.")?;
            print_manual_commands(output)?;
        }
    }
    Ok(())
}

fn print_manual_commands(output: &mut impl Write) -> Result<()> {
    writeln!(output, "Update manually with one of:")?;
    writeln!(output, "  npm install --global {NPM_PACKAGE}")?;
    writeln!(output, "  cargo install --git {GIT_REPOSITORY} --force")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn distinguishes_ephemeral_global_npm_cargo_and_unknown() {
        assert_eq!(
            detect_install_context(Path::new("/tmp/leantoken"), Some(PackageManager::Npx), None,),
            InstallContext::Npx
        );
        assert_eq!(
            detect_install_context(
                Path::new("/usr/lib/node_modules/leantoken/bin/leantoken"),
                None,
                Some(Path::new("/usr/lib/node_modules"))
            ),
            InstallContext::GlobalNpm
        );
        assert_eq!(
            detect_install_context(Path::new("/home/me/.cargo/bin/leantoken"), None, None),
            InstallContext::Cargo
        );
        assert_eq!(
            detect_install_context(Path::new("/usr/local/bin/leantoken"), None, None),
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
            "npm install --global leantoken@1.2.3"
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
    fn package_manager_refresh_commands_preserve_the_exact_version() {
        assert_eq!(
            ephemeral_refresh_command(InstallContext::Pnpm, "1.2.3"),
            Some("pnpm dlx leantoken@1.2.3 setup --refresh --yes".into())
        );
        assert_eq!(
            ephemeral_refresh_command(InstallContext::Yarn, "1.2.3"),
            Some("yarn dlx leantoken@1.2.3 setup --refresh --yes".into())
        );
    }

    #[test]
    fn npx_upgrade_reports_only_the_existing_installation_refresh() {
        let report = UpgradeReport {
            status: UpgradeStatus::Ephemeral,
            context: InstallContext::Npx,
            current_version: "1.2.2",
            latest_version: Some("1.2.3".into()),
            command: Some(npx_refresh_command("1.2.3")),
            mcp_refresh_command: None,
        };

        let mut text = Vec::new();
        write_report(&mut text, report, false).unwrap();
        let text = String::from_utf8(text).unwrap();
        assert!(text.contains("there is no persistent CLI to replace"));
        assert!(text.contains(
            "To upgrade existing MCP entries to v1.2.3, run:\n  \
             npx --yes leantoken@1.2.3 setup --refresh --yes"
        ));
        assert!(!text.contains("npm install --global"));
    }

    #[test]
    fn persistent_upgrade_guidance_preserves_pins_and_names_exact_refresh() {
        let report = updated_report(
            InstallContext::GlobalNpm,
            "1.2.3".into(),
            &CommandSpec::new("npm", ["install", "--global", "leantoken@latest"]),
        );

        let mut text = Vec::new();
        write_report(&mut text, report, false).unwrap();
        let text = String::from_utf8(text).unwrap();
        assert!(text.contains("LeanToken updated to v1.2.3."));
        assert!(text.contains(
            "Existing MCP entries were left unchanged. Refresh them explicitly with: \
             npx --yes leantoken@1.2.3 setup --refresh --yes"
        ));

        let report = updated_report(
            InstallContext::GlobalNpm,
            "1.2.3".into(),
            &CommandSpec::new("npm", ["install", "--global", "leantoken@latest"]),
        );
        let mut json = Vec::new();
        write_report(&mut json, report, true).unwrap();
        let json: serde_json::Value = serde_json::from_slice(&json).unwrap();
        assert_eq!(
            json["mcp_refresh_command"],
            "npx --yes leantoken@1.2.3 setup --refresh --yes"
        );
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

    #[test]
    fn cargo_version_selects_greatest_stable_ignoring_prereleases() {
        let output = "foo refs/tags/v1.0.0
bar refs/tags/v1.0.1-alpha.1
baz refs/tags/v1.0.1
";

        assert_eq!(
            select_latest_stable_tag(output.into()),
            Some("1.0.1".into())
        );
    }

    #[test]
    fn cargo_version_ignores_malformed_tags() {
        let output = "abc refs/tags/v9foo
def refs/tags/v1.0.0
";

        assert_eq!(
            select_latest_stable_tag(output.into()),
            Some("1.0.0".into())
        );
    }

    #[test]
    fn cargo_version_returns_none_when_no_valid_tags() {
        let output = "foo refs/tags/v9foo
bar refs/tags/latest
";

        assert_eq!(select_latest_stable_tag(output.into()), None);
    }

    #[test]
    fn cargo_version_prefers_stable_over_newer_prerelease() {
        let output = "abc refs/tags/v2.0.0-beta.1
def refs/tags/v1.5.0
";
        assert_eq!(
            select_latest_stable_tag(output.into()),
            Some("1.5.0".into())
        );
    }
}
