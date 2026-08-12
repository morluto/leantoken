//! Evidence-based classification of the executable invocation.
//!
//! Package-manager lifecycle variables are ambient process state. They are
//! useful only when they agree with executable/argument evidence; otherwise a
//! normal persistent invocation must continue to work.

use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum InvocationKind {
    Persistent,
    Ephemeral,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PackageManager {
    Npx,
    Npm,
    Pnpm,
    Yarn,
}

impl PackageManager {
    pub(crate) const fn command(self) -> &'static str {
        match self {
            Self::Npx => "npx",
            Self::Npm => "npm",
            Self::Pnpm => "pnpm",
            Self::Yarn => "yarn",
        }
    }

    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Npx => "npx",
            Self::Npm => "npm exec",
            Self::Pnpm => "pnpm",
            Self::Yarn => "Yarn",
        }
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct InvocationMetadata<'a> {
    pub(crate) npm_command: Option<&'a str>,
    pub(crate) npm_lifecycle_event: Option<&'a str>,
    pub(crate) npm_execpath: Option<&'a Path>,
    pub(crate) npm_node_execpath: Option<&'a Path>,
    pub(crate) yarn_version: Option<&'a str>,
    pub(crate) yarn_package_json: Option<&'a Path>,
    pub(crate) pnpm_script_src_dir: Option<&'a Path>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct InvocationIdentity {
    pub(crate) kind: InvocationKind,
    pub(crate) executable: PathBuf,
    pub(crate) package_manager: Option<PackageManager>,
}

impl InvocationIdentity {
    pub(crate) fn detect(
        executable: &Path,
        argv: &[String],
        metadata: InvocationMetadata<'_>,
    ) -> Self {
        let npm_metadata = metadata.npm_node_execpath.is_some_and(Path::is_absolute)
            && metadata
                .npm_execpath
                .is_some_and(|path| Path::is_absolute(path) && looks_like_npx_executable(path))
            && (metadata.npm_command == Some("exec")
                || metadata.npm_lifecycle_event == Some("npx"));
        let npm_exec_metadata = metadata.npm_node_execpath.is_some_and(Path::is_absolute)
            && metadata
                .npm_execpath
                .is_some_and(|path| Path::is_absolute(path) && looks_like_npm_executable(path))
            && metadata.npm_command == Some("exec");
        let package_layout = looks_like_package_layout(executable);
        // Package-manager launchers execute the package binary directly, so
        // the child argv normally contains only `leantoken ...`, not `npx`,
        // `pnpm`, or `yarn`. The package layout and manager-owned metadata are
        // the stable evidence; ambient lifecycle variables without that
        // executable evidence remain insufficient.
        let pnpm_metadata = metadata.pnpm_script_src_dir.is_some_and(Path::is_absolute)
            && (has_ephemeral_path_component(executable, "dlx")
                || argv.windows(2).any(|pair| {
                    pair[0].ends_with("pnpm") && (pair[1] == "dlx" || pair[1] == "exec")
                }));
        let yarn_metadata = metadata.yarn_version.is_some()
            && metadata
                .npm_execpath
                .is_some_and(|path| Path::is_absolute(path) && looks_like_yarn_executable(path))
            && (has_ephemeral_path_component(executable, "dlx")
                || metadata.yarn_package_json.is_some_and(|path| {
                    Path::is_absolute(path) && has_ephemeral_path_component(path, "dlx")
                }));

        let package_manager =
            if package_layout && npm_metadata && looks_like_npm_temp_layout(executable) {
                Some(PackageManager::Npx)
            } else if package_layout && npm_exec_metadata && looks_like_npm_temp_layout(executable)
            {
                Some(PackageManager::Npm)
            } else if package_layout && pnpm_metadata {
                Some(PackageManager::Pnpm)
            } else if package_layout && yarn_metadata {
                Some(PackageManager::Yarn)
            } else {
                None
            };

        Self {
            kind: if package_manager.is_some() {
                InvocationKind::Ephemeral
            } else {
                InvocationKind::Persistent
            },
            executable: executable.to_path_buf(),
            package_manager,
        }
    }
}

fn has_path_component(path: &Path, expected: &str) -> bool {
    path.components()
        .any(|component| component.as_os_str() == std::ffi::OsStr::new(expected))
}

fn has_ephemeral_path_component(path: &Path, prefix: &str) -> bool {
    path.components().any(|component| {
        let value = component.as_os_str().to_string_lossy();
        value == prefix || value.starts_with(&format!("{prefix}-"))
    })
}

fn looks_like_package_layout(executable: &Path) -> bool {
    let components = executable.components().collect::<Vec<_>>();
    components
        .windows(2)
        .any(|pair| pair[0].as_os_str() == "node_modules" && pair[1].as_os_str() == "leantoken")
        || (has_path_component(executable, ".yarn")
            && components.iter().any(|component| {
                let value = component.as_os_str().to_string_lossy();
                value.starts_with("leantoken@") || value.starts_with("leantoken-")
            }))
}

fn looks_like_npx_executable(path: &Path) -> bool {
    path.file_name()
        .is_some_and(|name| name.to_string_lossy().to_ascii_lowercase().contains("npx"))
}

fn looks_like_npm_temp_layout(path: &Path) -> bool {
    has_path_component(path, "_npx") || has_ephemeral_path_component(path, "npx")
}

fn looks_like_npm_executable(path: &Path) -> bool {
    path.file_name()
        .is_some_and(|name| name.to_string_lossy().to_ascii_lowercase().contains("npm"))
}

fn looks_like_yarn_executable(path: &Path) -> bool {
    path.file_name()
        .is_some_and(|name| name.to_string_lossy().to_ascii_lowercase().contains("yarn"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn metadata<'a>(
        npm_command: Option<&'a str>,
        lifecycle: Option<&'a str>,
        execpath: Option<&'a Path>,
        node: Option<&'a Path>,
    ) -> InvocationMetadata<'a> {
        InvocationMetadata {
            npm_command,
            npm_lifecycle_event: lifecycle,
            npm_execpath: execpath,
            npm_node_execpath: node,
            ..InvocationMetadata::default()
        }
    }

    fn absolute_path(relative: &str) -> PathBuf {
        std::env::current_dir()
            .expect("current directory")
            .join(relative)
    }

    #[test]
    fn genuine_npx_requires_metadata_and_package_layout() {
        let executable = absolute_path("home/.npm/_npx/123/node_modules/leantoken/bin/leantoken");
        let execpath = absolute_path("usr/lib/npm/npx-cli.js");
        let node = absolute_path("usr/bin/node");
        let identity = InvocationIdentity::detect(
            &executable,
            &["npx".into(), "leantoken".into()],
            metadata(Some("exec"), Some("npx"), Some(&execpath), Some(&node)),
        );
        assert_eq!(identity.kind, InvocationKind::Ephemeral);
        assert_eq!(identity.package_manager, Some(PackageManager::Npx));
    }

    #[test]
    fn npm_exec_keeps_its_distinct_launcher_identity() {
        let executable = absolute_path("home/.npm/_npx/123/node_modules/leantoken/bin/leantoken");
        let execpath = absolute_path("usr/lib/node_modules/npm/bin/npm-cli.js");
        let node = absolute_path("usr/bin/node");
        let identity = InvocationIdentity::detect(
            &executable,
            &["leantoken".into(), "setup".into()],
            metadata(Some("exec"), None, Some(&execpath), Some(&node)),
        );
        assert_eq!(identity.package_manager, Some(PackageManager::Npm));
    }

    #[test]
    fn npm_metadata_does_not_reclassify_a_local_package_binary() {
        let executable = absolute_path("workspace/node_modules/leantoken/bin/leantoken");
        let execpath = absolute_path("usr/lib/node_modules/npm/bin/npm-cli.js");
        let node = absolute_path("usr/bin/node");
        let identity = InvocationIdentity::detect(
            &executable,
            &["leantoken".into(), "setup".into()],
            metadata(Some("exec"), None, Some(&execpath), Some(&node)),
        );
        assert_eq!(identity.kind, InvocationKind::Persistent);
        assert_eq!(identity.package_manager, None);
    }

    #[test]
    fn isolated_lifecycle_contamination_stays_persistent() {
        let identity = InvocationIdentity::detect(
            Path::new("/usr/local/bin/leantoken"),
            &["leantoken".into(), "setup".into()],
            metadata(None, Some("npx"), None, None),
        );
        assert_eq!(identity.kind, InvocationKind::Persistent);
    }

    #[test]
    fn pnpm_and_yarn_layouts_need_their_own_evidence() {
        let pnpm_executable = absolute_path(
            "workspace/node_modules/.pnpm/leantoken@1/node_modules/leantoken/bin/leantoken",
        );
        let pnpm_dlx = absolute_path("tmp/pnpm/dlx-123");
        let pnpm = InvocationIdentity::detect(
            &pnpm_executable,
            &["pnpm".into(), "dlx".into(), "leantoken".into()],
            InvocationMetadata {
                pnpm_script_src_dir: Some(&pnpm_dlx),
                ..InvocationMetadata::default()
            },
        );
        assert_eq!(pnpm.package_manager, Some(PackageManager::Pnpm));

        let local_pnpm_src = absolute_path("workspace/node_modules/.pnpm");
        let local_pnpm = InvocationIdentity::detect(
            &pnpm_executable,
            &["leantoken".into(), "setup".into()],
            InvocationMetadata {
                pnpm_script_src_dir: Some(&local_pnpm_src),
                ..InvocationMetadata::default()
            },
        );
        assert_eq!(local_pnpm.kind, InvocationKind::Persistent);
        assert_eq!(local_pnpm.package_manager, None);

        let pnpm_dlx_executable =
            absolute_path("tmp/pnpm/dlx-123/node_modules/leantoken/bin/leantoken");
        let pnpm_dlx_temp = InvocationIdentity::detect(
            &pnpm_dlx_executable,
            &["leantoken".into(), "setup".into()],
            InvocationMetadata {
                pnpm_script_src_dir: Some(&pnpm_dlx),
                ..InvocationMetadata::default()
            },
        );
        assert_eq!(pnpm_dlx_temp.package_manager, Some(PackageManager::Pnpm));

        let yarn_executable = absolute_path(
            "workspace/.yarn/dlx/leantoken-npm-1/node_modules/leantoken/bin/leantoken",
        );
        let yarn_execpath = absolute_path("usr/bin/yarn");
        let yarn_package_json = absolute_path("workspace/.yarn/dlx/leantoken-npm-1/package.json");
        let yarn = InvocationIdentity::detect(
            &yarn_executable,
            &["leantoken".into(), "setup".into()],
            InvocationMetadata {
                yarn_version: Some("4.0.0"),
                npm_execpath: Some(&yarn_execpath),
                yarn_package_json: Some(&yarn_package_json),
                ..InvocationMetadata::default()
            },
        );
        assert_eq!(yarn.package_manager, Some(PackageManager::Yarn));
    }

    #[test]
    fn genuine_npx_child_argv_does_not_need_to_repeat_the_wrapper() {
        let executable = absolute_path("home/.npm/_npx/123/node_modules/leantoken/bin/leantoken");
        let execpath = absolute_path("usr/lib/node_modules/npm/bin/npx-cli.js");
        let node = absolute_path("usr/bin/node");
        let identity = InvocationIdentity::detect(
            &executable,
            &["leantoken".into(), "setup".into()],
            metadata(Some("exec"), None, Some(&execpath), Some(&node)),
        );
        assert_eq!(identity.package_manager, Some(PackageManager::Npx));
    }

    #[test]
    fn ambient_npm_metadata_cannot_reclassify_a_persistent_project_binary() {
        let executable = absolute_path("work/leantoken-project/target/debug/leantoken");
        let execpath = absolute_path("usr/lib/npm/npx-cli.js");
        let node = absolute_path("usr/bin/node");
        let identity = InvocationIdentity::detect(
            &executable,
            &["leantoken".into(), "setup".into()],
            metadata(Some("exec"), Some("npx"), Some(&execpath), Some(&node)),
        );
        assert_eq!(identity.kind, InvocationKind::Persistent);
        assert_eq!(identity.package_manager, None);
    }

    #[test]
    fn local_yarn_pnp_dependency_stays_persistent() {
        let executable = absolute_path(
            "workspace/.yarn/unplugged/leantoken-npm-1/node_modules/leantoken/bin/leantoken",
        );
        let yarn_execpath = absolute_path("usr/bin/yarn");
        let identity = InvocationIdentity::detect(
            &executable,
            &["leantoken".into(), "setup".into()],
            InvocationMetadata {
                yarn_version: Some("4.0.0"),
                npm_execpath: Some(&yarn_execpath),
                ..InvocationMetadata::default()
            },
        );
        assert_eq!(identity.kind, InvocationKind::Persistent);
        assert_eq!(identity.package_manager, None);
    }
}
