use std::{
    ffi::OsStr,
    path::{Path, PathBuf},
};

use crate::{Error, Result};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct McpLauncher {
    command: PathBuf,
    pub(super) args: Vec<String>,
    uses_npx: bool,
}

impl McpLauncher {
    pub(super) fn current() -> Result<Self> {
        if std::env::var_os("npm_lifecycle_event").as_deref() == Some(OsStr::new("npx")) {
            let node = std::env::var_os("npm_node_execpath").ok_or_else(|| {
                Error::InvalidRequest("npx did not report its Node executable path".into())
            })?;
            let npm = std::env::var_os("npm_execpath").ok_or_else(|| {
                Error::InvalidRequest("npx did not report its npm CLI path".into())
            })?;
            return Self::from_npx_paths(Path::new(&node), Path::new(&npm));
        }
        Ok(Self::from_executable(
            &std::env::current_exe()?.canonicalize()?,
        ))
    }

    pub(super) fn from_executable(executable: &Path) -> Self {
        Self {
            command: executable.into(),
            args: vec!["mcp".into()],
            uses_npx: false,
        }
    }

    pub(super) fn uses_npx(&self) -> bool {
        self.uses_npx
    }

    pub(super) fn command(&self) -> Result<&str> {
        self.command
            .to_str()
            .ok_or_else(|| Error::InvalidRequest("LeanToken executable path is not UTF-8".into()))
    }

    fn from_npx_paths(node: &Path, npm: &Path) -> Result<Self> {
        if !node.is_absolute() || !npm.is_absolute() {
            return Err(Error::InvalidRequest(
                "npx reported a relative Node or npm CLI path".into(),
            ));
        }
        let npm = npm
            .to_str()
            .ok_or_else(|| Error::InvalidRequest("npm CLI path is not UTF-8".into()))?;
        Ok(Self {
            command: node.into(),
            args: vec![
                npm.into(),
                "exec".into(),
                "--yes".into(),
                "--package=leantoken@latest".into(),
                "--".into(),
                "leantoken".into(),
                "mcp".into(),
            ],
            uses_npx: true,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn npx_launcher_requests_latest_release_without_using_cache_path() {
        let root = if cfg!(windows) { r"C:\npm" } else { "/npm" };
        assert_eq!(
            McpLauncher::from_npx_paths(
                &Path::new(root).join("node"),
                &Path::new(root).join("npm-cli.js"),
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
                    "--package=leantoken@latest".into(),
                    "--".into(),
                    "leantoken".into(),
                    "mcp".into(),
                ],
                uses_npx: true,
            }
        );
    }
}
