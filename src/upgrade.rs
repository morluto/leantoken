//! Package-manager-aware updates for persistent LeanToken installations.

use std::{
    env,
    path::Path,
    process::{Command, Output},
};

use serde::Serialize;

use crate::{Error, Result};

const NPM_PACKAGE: &str = "leantoken@latest";
const GIT_REPOSITORY: &str = "https://github.com/morluto/leantoken";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
enum InstallMethod {
    Npm,
    Cargo,
}

#[derive(Debug, Serialize)]
struct UpgradeReport {
    status: &'static str,
    method: InstallMethod,
}

/// Update LeanToken through the package manager that owns the executable.
///
/// # Errors
///
/// Returns an error when the installation method cannot be identified or the
/// package manager cannot install the latest release.
pub fn run(json: bool) -> Result<()> {
    let executable = env::current_exe()?.canonicalize()?;
    let method = detect_install_method(&executable, env::var_os("npm_execpath").is_some())
        .ok_or_else(unknown_install_error)?;

    let (program, arguments): (&str, &[&str]) = match method {
        InstallMethod::Npm => ("npm", &["install", "--global", NPM_PACKAGE]),
        InstallMethod::Cargo => ("cargo", &["install", "--git", GIT_REPOSITORY, "--force"]),
    };

    let output = Command::new(program)
        .args(arguments)
        .output()
        .map_err(|error| Error::InvalidRequest(format!("failed to run {program}: {error}")))?;
    require_success(program, arguments, &output)?;

    let report = UpgradeReport {
        status: "updated",
        method,
    };
    if json {
        println!("{}", serde_json::to_string(&report)?);
    } else {
        println!("LeanToken update completed through {program}.");
    }
    Ok(())
}

fn detect_install_method(executable: &Path, npm_environment: bool) -> Option<InstallMethod> {
    if npm_environment || path_contains(executable, "node_modules") {
        return Some(InstallMethod::Npm);
    }
    if path_contains(executable, ".cargo") {
        return Some(InstallMethod::Cargo);
    }
    None
}

fn path_contains(path: &Path, component: &str) -> bool {
    path.components()
        .any(|part| part.as_os_str() == std::ffi::OsStr::new(component))
}

fn unknown_install_error() -> Error {
    Error::InvalidRequest(format!(
        "could not identify the LeanToken installation method; update manually with \
         `npm install --global {NPM_PACKAGE}` or \
         `cargo install --git {GIT_REPOSITORY} --force`"
    ))
}

fn require_success(program: &str, arguments: &[&str], output: &Output) -> Result<()> {
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    let detail = stderr.trim();
    let command = std::iter::once(program)
        .chain(arguments.iter().copied())
        .collect::<Vec<_>>()
        .join(" ");
    Err(Error::InvalidRequest(if detail.is_empty() {
        format!("update command failed: {command}")
    } else {
        format!("update command failed: {command}: {detail}")
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_npm_from_environment_or_package_path() {
        assert_eq!(
            detect_install_method(Path::new("/tmp/leantoken"), true),
            Some(InstallMethod::Npm)
        );
        assert_eq!(
            detect_install_method(
                Path::new("/usr/lib/node_modules/leantoken/bin/leantoken"),
                false
            ),
            Some(InstallMethod::Npm)
        );
    }

    #[test]
    fn detects_cargo_and_rejects_unknown_paths() {
        assert_eq!(
            detect_install_method(Path::new("/home/me/.cargo/bin/leantoken"), false),
            Some(InstallMethod::Cargo)
        );
        assert_eq!(
            detect_install_method(Path::new("/usr/local/bin/leantoken"), false),
            None
        );
    }

    #[cfg(unix)]
    #[test]
    fn failed_commands_include_stderr() {
        use std::os::unix::process::ExitStatusExt;
        use std::process::ExitStatus;

        let output = Output {
            status: ExitStatus::from_raw(1 << 8),
            stdout: Vec::new(),
            stderr: b"permission denied\n".to_vec(),
        };
        let error = require_success("npm", &["install"], &output).unwrap_err();
        assert!(error.to_string().contains("permission denied"));
    }
}
