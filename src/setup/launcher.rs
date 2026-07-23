use std::{
    ffi::OsStr,
    path::{Path, PathBuf},
};

use crate::{Error, Result};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct McpLauncher {
    command: PathBuf,
    pub(super) args: Vec<String>,
    version: String,
    npm_package: Option<String>,
}

impl McpLauncher {
    pub(super) fn current() -> Result<Self> {
        if std::env::var_os("npm_lifecycle_event").as_deref() == Some(OsStr::new("npx")) {
            let node = std::env::var_os("npm_node_execpath").ok_or_else(|| {
                Error::InternalFailure("npx did not report its Node executable path".into())
            })?;
            let npm = std::env::var_os("npm_execpath").ok_or_else(|| {
                Error::InternalFailure("npx did not report its npm CLI path".into())
            })?;
            return Self::from_npx_paths(Path::new(&node), Path::new(&npm));
        }
        Ok(Self::from_executable(
            &std::env::current_exe()?.canonicalize()?,
        ))
    }

    pub(super) fn from_executable(executable: &Path) -> Self {
        Self::from_executable_with_version(executable, env!("CARGO_PKG_VERSION"))
    }

    pub(super) fn from_executable_with_version(executable: &Path, version: &str) -> Self {
        Self {
            command: executable.into(),
            args: vec!["mcp".into()],
            version: version.into(),
            npm_package: None,
        }
    }

    pub(super) fn uses_npx(&self) -> bool {
        self.npm_package.is_some()
    }

    pub(super) fn version(&self) -> &str {
        &self.version
    }

    pub(super) fn npm_package(&self) -> Option<&str> {
        self.npm_package.as_deref()
    }

    pub(super) fn command(&self) -> Result<&str> {
        self.command
            .to_str()
            .ok_or_else(|| Error::InternalFailure("LeanToken executable path is not UTF-8".into()))
    }

    fn from_npx_paths(node: &Path, npm: &Path) -> Result<Self> {
        Self::from_npx_paths_with_version(node, npm, env!("CARGO_PKG_VERSION"))
    }

    pub(super) fn from_npx_paths_with_version(
        node: &Path,
        npm: &Path,
        version: &str,
    ) -> Result<Self> {
        if !node.is_absolute() || !npm.is_absolute() {
            return Err(Error::InternalFailure(
                "npx reported a relative Node or npm CLI path".into(),
            ));
        }
        let npm = npm
            .to_str()
            .ok_or_else(|| Error::InternalFailure("npm CLI path is not UTF-8".into()))?;
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
                "mcp".into(),
            ],
            version: version.into(),
            npm_package: Some(package),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn npx_launcher_pins_exact_release_without_using_cache_path() {
        let root = if cfg!(windows) { r"C:\npm" } else { "/npm" };
        let version = "1.2.3";
        assert_eq!(
            McpLauncher::from_npx_paths_with_version(
                &Path::new(root).join("node"),
                &Path::new(root).join("npm-cli.js"),
                version,
            )
            .unwrap(),
            McpLauncher {
                command: Path::new(root).join("node"),
                args: vec![
                    Path::new(root)
                        .join("npm-cli.js")
                        .to_string_lossy()
                        .into_owned(),
                    "exec".into(),
                    "--yes".into(),
                    "--package=leantoken@1.2.3".into(),
                    "--".into(),
                    "leantoken".into(),
                    "mcp".into(),
                ],
                version: version.into(),
                npm_package: Some("leantoken@1.2.3".into()),
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
            &root.join("npm cli.js"),
            "1.2.3",
        )
        .unwrap();

        assert_eq!(launcher.command, root.join("node"));
        assert_eq!(launcher.args[0], root.join("npm cli.js").to_string_lossy());
        assert_eq!(launcher.args[3], "--package=leantoken@1.2.3");
    }
}
