use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use tempfile::Builder;

#[derive(Debug)]
pub struct Sandbox {
    root: PathBuf,
    repo: PathBuf,
    rerun: String,
    preservation_id: String,
}

#[derive(Debug)]
pub enum SandboxError {
    Io(std::io::Error),
    CreationFailed(PathBuf),
}

impl fmt::Display for SandboxError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(f, "sandbox I/O error: {error}"),
            Self::CreationFailed(path) => {
                write!(f, "could not create sandbox at {}", path.display())
            }
        }
    }
}

impl std::error::Error for SandboxError {}

impl From<std::io::Error> for SandboxError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

impl Sandbox {
    /// Create an isolated test tree. `module` and `callsite` form the stable
    /// diagnostic identity; the directory itself still receives a unique
    /// suffix so concurrent tests cannot collide.
    pub fn new(module: &str, callsite: &str) -> Result<Self, SandboxError> {
        let id = stable_id(module, callsite);
        let workspace_root = workspace_root();
        let parent = workspace_root.join("target").join("test-sandboxes");
        fs::create_dir_all(&parent)?;

        let root = Builder::new()
            .prefix(&format!("{id}-"))
            .tempdir_in(&parent)?
            .keep();
        let preservation_id = root
            .file_name()
            .and_then(|name| name.to_str())
            .map(str::to_owned)
            .ok_or_else(|| SandboxError::CreationFailed(root.clone()))?;

        let sandbox = Self {
            repo: root.join("repo"),
            root,
            rerun: rerun_command(module, callsite, std::thread::current().name()),
            preservation_id,
        };
        fs::create_dir_all(&sandbox.repo)?;
        Ok(sandbox)
    }

    pub fn root(&self) -> &Path {
        &self.root
    }
    pub fn repo(&self) -> &Path {
        &self.repo
    }

    fn preserve(&self) -> bool {
        std::env::var_os("LEANTOKEN_TEST_KEEP").is_some_and(|value| value == "1")
            || std::thread::panicking()
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        if !self.preserve() {
            let _ = fs::remove_dir_all(&self.root);
            return;
        }

        let workspace_root = workspace_root();
        let failure_root = workspace_root.join("target").join("test-failures");
        let _ = fs::create_dir_all(&failure_root);
        let destination = failure_root.join(&self.preservation_id);
        let _ = fs::rename(&self.root, &destination);
        eprintln!(
            "LeanToken test sandbox preserved: {}",
            destination.display()
        );
        eprintln!("LeanToken focused rerun: {}", self.rerun);
    }
}

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .expect("test-support manifest is below the workspace root")
}

fn stable_id(module: &str, callsite: &str) -> String {
    let mut hash = 0xcbf29ce484222325u64;
    for byte in format!("{module}::{callsite}").bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    let safe_module = module
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '-' | '_') {
                character
            } else {
                '_'
            }
        })
        .collect::<String>();
    format!("{safe_module}-{hash:016x}")
}

fn rerun_command(module: &str, callsite: &str, test_name: Option<&str>) -> String {
    let package = match module.split("::").next() {
        Some("leantoken_test_support") => "leantoken-test-support",
        Some("leantoken_test_suite") => "leantoken-test-suite",
        _ => "leantoken",
    };
    let selector = test_name
        .filter(|name| !name.is_empty())
        .unwrap_or(callsite);
    format!("cargo test --locked --package {package} --all-features --lib {selector}")
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::thread;

    use super::{Sandbox, rerun_command, stable_id};

    #[test]
    fn creates_isolated_repository_directories() {
        let sandbox =
            Sandbox::new(module_path!(), "creates_isolated_repository_directories").unwrap();
        assert!(sandbox.root().is_dir());
        assert!(sandbox.repo().is_dir());
    }

    #[test]
    fn stable_ids_are_valid_cross_platform_path_components() {
        let id = stable_id("leantoken_test_suite::domains::storage", "storage_case");
        assert!(
            id.chars().all(
                |character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_')
            )
        );
    }

    #[test]
    fn preserved_sandboxes_have_unique_destinations_for_one_stable_id() {
        let first = Sandbox::new(module_path!(), "storage_case").unwrap();
        let second = Sandbox::new(module_path!(), "storage_case").unwrap();
        assert_ne!(first.preservation_id, second.preservation_id);
    }

    #[test]
    fn concurrent_sandbox_creation_has_unique_roots() {
        let sandboxes = (0..64)
            .map(|_| thread::spawn(|| Sandbox::new(module_path!(), "concurrent_case").unwrap()))
            .map(|handle| handle.join().unwrap())
            .collect::<Vec<_>>();
        let roots = sandboxes
            .iter()
            .map(|sandbox| sandbox.root().to_owned())
            .collect::<HashSet<_>>();

        assert_eq!(roots.len(), sandboxes.len());
    }

    #[test]
    fn rerun_command_uses_the_libtest_name_and_owning_package() {
        let command = rerun_command(
            "leantoken_test_suite::domains::storage",
            "storage_case",
            Some("domains::storage::reopens_existing_index"),
        );
        assert_eq!(
            command,
            "cargo test --locked --package leantoken-test-suite --all-features --lib domains::storage::reopens_existing_index"
        );
    }
}
