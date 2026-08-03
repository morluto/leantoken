use std::path::{Path, PathBuf};

use crate::invocation::{InvocationIdentity, InvocationMetadata, PackageManager};
use crate::{Error, Result};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct McpLauncher {
    command: PathBuf,
    pub(super) args: Vec<String>,
    version: String,
    package_manager: Option<PackageManager>,
    package: Option<String>,
}

impl McpLauncher {
    pub(super) fn current() -> Result<Self> {
        let executable = std::env::current_exe()?.canonicalize()?;
        let node = std::env::var_os("npm_node_execpath").map(PathBuf::from);
        let npm = std::env::var_os("npm_execpath").map(PathBuf::from);
        let npm_command = std::env::var("npm_command").ok();
        let lifecycle = std::env::var("npm_lifecycle_event").ok();
        let yarn_version = std::env::var("YARN_VERSION").ok();
        let yarn_package_json = std::env::var_os("npm_package_json").map(PathBuf::from);
        let pnpm_script_src_dir = std::env::var_os("PNPM_SCRIPT_SRC_DIR")
            .or_else(|| std::env::var_os("PNPM_SCRIPT_SRC"))
            .map(PathBuf::from);
        let identity = InvocationIdentity::detect(
            &executable,
            &std::env::args().collect::<Vec<_>>(),
            InvocationMetadata {
                npm_command: npm_command.as_deref(),
                npm_lifecycle_event: lifecycle.as_deref(),
                npm_execpath: npm.as_deref(),
                npm_node_execpath: node.as_deref(),
                yarn_version: yarn_version.as_deref(),
                yarn_package_json: yarn_package_json.as_deref(),
                pnpm_script_src_dir: pnpm_script_src_dir.as_deref(),
            },
        );
        if let Some(package_manager) = identity.package_manager {
            return match package_manager {
                PackageManager::Npx => Self::from_npx_paths(
                    node.as_deref().ok_or_else(|| {
                        Error::SetupFailure("npx did not report its Node executable path".into())
                    })?,
                    npm.as_deref().ok_or_else(|| {
                        Error::SetupFailure("npx did not report its CLI path".into())
                    })?,
                ),
                PackageManager::Npm => Self::from_npm_paths(
                    node.as_deref().ok_or_else(|| {
                        Error::SetupFailure("npm did not report its Node executable path".into())
                    })?,
                    npm.as_deref().ok_or_else(|| {
                        Error::SetupFailure("npm did not report its CLI path".into())
                    })?,
                ),
                PackageManager::Pnpm | PackageManager::Yarn => {
                    Self::from_package_manager_with_version_resolved(
                        package_manager,
                        env!("CARGO_PKG_VERSION"),
                    )
                }
            };
        }
        Ok(Self::from_executable(&executable))
    }

    pub(super) fn from_executable(executable: &Path) -> Self {
        Self::from_executable_with_version(executable, env!("CARGO_PKG_VERSION"))
    }

    pub(super) fn from_executable_with_version(executable: &Path, version: &str) -> Self {
        Self {
            command: executable.into(),
            args: vec!["--managed-by-setup".into(), "mcp".into()],
            version: version.into(),
            package_manager: None,
            package: None,
        }
    }

    pub(super) fn uses_npx(&self) -> bool {
        matches!(
            self.package_manager,
            Some(PackageManager::Npx | PackageManager::Npm)
        )
    }

    pub(super) fn is_ephemeral(&self) -> bool {
        self.package_manager.is_some()
    }

    pub(super) fn version(&self) -> &str {
        &self.version
    }

    pub(super) fn npm_package(&self) -> Option<&str> {
        self.package.as_deref()
    }

    pub(super) fn doctor_command(&self) -> String {
        let Some(package_manager) = self.package_manager else {
            return "leantoken doctor --json".into();
        };
        let package = self.package.as_deref().unwrap_or("leantoken");
        match package_manager {
            PackageManager::Npx => format!("npx --yes {package} doctor --json"),
            PackageManager::Npm => {
                format!("npm exec --yes --package={package} -- leantoken doctor --json")
            }
            PackageManager::Pnpm => format!("pnpm dlx {package} doctor --json"),
            PackageManager::Yarn => format!("yarn dlx {package} doctor --json"),
        }
    }

    pub(super) fn command(&self) -> Result<&str> {
        self.command
            .to_str()
            .ok_or_else(|| Error::SetupFailure("LeanToken executable path is not UTF-8".into()))
    }

    fn from_npx_paths(node: &Path, npx: &Path) -> Result<Self> {
        Self::from_npx_paths_with_version(node, npx, env!("CARGO_PKG_VERSION"))
    }

    fn from_npm_paths(node: &Path, npm: &Path) -> Result<Self> {
        Self::from_npm_paths_with_version(node, npm, env!("CARGO_PKG_VERSION"))
    }

    pub(super) fn from_npx_paths_with_version(
        node: &Path,
        npx: &Path,
        version: &str,
    ) -> Result<Self> {
        if !node.is_absolute() || !npx.is_absolute() {
            return Err(Error::SetupFailure(
                "npx reported a relative Node or CLI path".into(),
            ));
        }
        let npx = npx
            .to_str()
            .ok_or_else(|| Error::SetupFailure("npx CLI path is not UTF-8".into()))?;
        let package = format!("leantoken@{version}");
        Ok(Self {
            command: node.into(),
            // `npm_execpath` points to npx-cli.js when setup itself is run by
            // npx, so invoke the npx CLI directly instead of adding npm's
            // `exec` subcommand.
            args: vec![
                npx.into(),
                "--yes".into(),
                "--prefer-offline".into(),
                format!("--package={package}"),
                "--".into(),
                "leantoken".into(),
                "--managed-by-setup".into(),
                "mcp".into(),
            ],
            version: version.into(),
            package_manager: Some(PackageManager::Npx),
            package: Some(package),
        })
    }

    pub(super) fn from_npm_paths_with_version(
        node: &Path,
        npm: &Path,
        version: &str,
    ) -> Result<Self> {
        if !node.is_absolute() || !npm.is_absolute() {
            return Err(Error::SetupFailure(
                "npm reported a relative Node or CLI path".into(),
            ));
        }
        let npm = npm
            .to_str()
            .ok_or_else(|| Error::SetupFailure("npm CLI path is not UTF-8".into()))?;
        let package = format!("leantoken@{version}");
        Ok(Self {
            command: node.into(),
            args: vec![
                npm.into(),
                "exec".into(),
                "--yes".into(),
                format!("--package={package}"),
                "--".into(),
                "leantoken".into(),
                "--managed-by-setup".into(),
                "mcp".into(),
            ],
            version: version.into(),
            package_manager: Some(PackageManager::Npm),
            package: Some(package),
        })
    }

    pub(super) fn from_package_manager_with_version(
        package_manager: PackageManager,
        version: &str,
    ) -> Self {
        let package = format!("leantoken@{version}");
        let args = match package_manager {
            PackageManager::Npx => vec![
                "--yes".into(),
                package.clone(),
                "--managed-by-setup".into(),
                "mcp".into(),
            ],
            PackageManager::Npm => vec![
                "exec".into(),
                "--yes".into(),
                format!("--package={package}"),
                "--".into(),
                "leantoken".into(),
                "--managed-by-setup".into(),
                "mcp".into(),
            ],
            PackageManager::Pnpm | PackageManager::Yarn => {
                vec![
                    "dlx".into(),
                    package.clone(),
                    "--managed-by-setup".into(),
                    "mcp".into(),
                ]
            }
        };
        Self {
            command: package_manager.command().into(),
            args,
            version: version.into(),
            package_manager: Some(package_manager),
            package: Some(package),
        }
    }

    fn from_package_manager_with_version_resolved(
        package_manager: PackageManager,
        version: &str,
    ) -> Result<Self> {
        let command = resolve_path_command(package_manager.command())?;
        let mut launcher = Self::from_package_manager_with_version(package_manager, version);
        launcher.command = command;
        Ok(launcher)
    }
}

fn resolve_path_command(command: &str) -> Result<PathBuf> {
    let paths = std::env::var_os("PATH").ok_or_else(|| {
        Error::SetupFailure(format!("PATH is unavailable while resolving {command}"))
    })?;
    for directory in std::env::split_paths(&paths) {
        let candidate = directory.join(command);
        if candidate.is_file() {
            return candidate.canonicalize().map_err(Into::into);
        }
        #[cfg(windows)]
        for extension in [".cmd", ".exe", ".bat"] {
            let candidate = directory.join(format!("{command}{extension}"));
            if candidate.is_file() {
                return candidate.canonicalize().map_err(Into::into);
            }
        }
    }
    Err(Error::SetupFailure(format!(
        "could not resolve {command} to an executable path"
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn npx_launcher_invokes_npx_directly_and_pins_exact_release() {
        let root = if cfg!(windows) { r"C:\npm" } else { "/npm" };
        let version = "1.2.3";
        assert_eq!(
            McpLauncher::from_npx_paths_with_version(
                &Path::new(root).join("node"),
                &Path::new(root).join("npx-cli.js"),
                version,
            )
            .unwrap(),
            McpLauncher {
                command: Path::new(root).join("node"),
                args: vec![
                    Path::new(root)
                        .join("npx-cli.js")
                        .to_string_lossy()
                        .into_owned(),
                    "--yes".into(),
                    "--prefer-offline".into(),
                    "--package=leantoken@1.2.3".into(),
                    "--".into(),
                    "leantoken".into(),
                    "--managed-by-setup".into(),
                    "mcp".into(),
                ],
                version: version.into(),
                package_manager: Some(PackageManager::Npx),
                package: Some("leantoken@1.2.3".into()),
            }
        );
    }

    #[test]
    fn npx_launcher_preserves_paths_with_spaces_as_distinct_arguments() {
        let root = if cfg!(windows) {
            Path::new(r"C:\Program Files\nodejs")
        } else {
            Path::new("/opt/node runtime")
        };
        let launcher = McpLauncher::from_npx_paths_with_version(
            &root.join("node"),
            &root.join("npx cli.js"),
            "1.2.3",
        )
        .unwrap();

        assert_eq!(launcher.command, root.join("node"));
        assert_eq!(launcher.args[0], root.join("npx cli.js").to_string_lossy());
        assert_eq!(launcher.args[3], "--package=leantoken@1.2.3");
        assert_eq!(launcher.args[6], "--managed-by-setup");
    }

    #[test]
    fn package_manager_launchers_pin_the_exact_release() {
        let npm = McpLauncher::from_package_manager_with_version(PackageManager::Npm, "1.2.3");
        assert_eq!(
            npm.args,
            vec![
                "exec",
                "--yes",
                "--package=leantoken@1.2.3",
                "--",
                "leantoken",
                "--managed-by-setup",
                "mcp"
            ]
        );
        let pnpm = McpLauncher::from_package_manager_with_version(PackageManager::Pnpm, "1.2.3");
        assert_eq!(pnpm.command, Path::new("pnpm"));
        assert_eq!(
            pnpm.args,
            ["dlx", "leantoken@1.2.3", "--managed-by-setup", "mcp"]
        );
        assert_eq!(
            pnpm.doctor_command(),
            "pnpm dlx leantoken@1.2.3 doctor --json"
        );

        let yarn = McpLauncher::from_package_manager_with_version(PackageManager::Yarn, "1.2.3");
        assert_eq!(yarn.command, Path::new("yarn"));
        assert_eq!(
            yarn.args,
            ["dlx", "leantoken@1.2.3", "--managed-by-setup", "mcp"]
        );
        assert_eq!(
            yarn.doctor_command(),
            "yarn dlx leantoken@1.2.3 doctor --json"
        );
    }
}
