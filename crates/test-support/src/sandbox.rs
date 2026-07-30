use std::collections::BTreeMap;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug)]
pub struct Sandbox {
    id: String,
    root: PathBuf,
    repo: PathBuf,
    cache: PathBuf,
    home: PathBuf,
    config: PathBuf,
    logs: PathBuf,
    artifacts: PathBuf,
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

        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or_default();
        let preservation_id = format!("{id}-{nonce:x}");
        let root = parent.join(&preservation_id);
        fs::create_dir(&root).map_err(|error| {
            if error.kind() == std::io::ErrorKind::AlreadyExists {
                SandboxError::CreationFailed(root.clone())
            } else {
                SandboxError::Io(error)
            }
        })?;

        let sandbox = Self {
            id,
            repo: root.join("repo"),
            cache: root.join("cache"),
            home: root.join("home"),
            config: root.join("config"),
            logs: root.join("logs"),
            artifacts: root.join("artifacts"),
            root,
            rerun: rerun_command(module, callsite, std::thread::current().name()),
            preservation_id,
        };
        for path in [
            &sandbox.repo,
            &sandbox.cache,
            &sandbox.home,
            &sandbox.config,
            &sandbox.logs,
            &sandbox.artifacts,
        ] {
            fs::create_dir_all(path)?;
        }
        Ok(sandbox)
    }

    pub fn id(&self) -> &str {
        &self.id
    }
    pub fn root(&self) -> &Path {
        &self.root
    }
    pub fn repo(&self) -> &Path {
        &self.repo
    }
    pub fn cache(&self) -> &Path {
        &self.cache
    }
    pub fn home(&self) -> &Path {
        &self.home
    }
    pub fn config(&self) -> &Path {
        &self.config
    }
    pub fn logs(&self) -> &Path {
        &self.logs
    }
    pub fn artifacts(&self) -> &Path {
        &self.artifacts
    }

    pub fn set_rerun_command(&mut self, command: impl Into<String>) {
        self.rerun = command.into();
    }

    /// Environment for child processes. It intentionally starts empty and
    /// restores only the executable search path plus sandbox-owned locations.
    pub fn environment(&self) -> BTreeMap<String, String> {
        let mut environment = BTreeMap::new();
        if let Some(path) = std::env::var_os("PATH") {
            environment.insert("PATH".to_owned(), path.to_string_lossy().into_owned());
        }
        environment.insert("HOME".to_owned(), self.home.display().to_string());
        environment.insert("USERPROFILE".to_owned(), self.home.display().to_string());
        environment.insert(
            "LEANTOKEN_CACHE_DIR".to_owned(),
            self.cache.display().to_string(),
        );
        environment.insert(
            "LEANTOKEN_CONFIG_DIR".to_owned(),
            self.config.display().to_string(),
        );
        environment.insert("GIT_CONFIG_NOSYSTEM".to_owned(), "1".to_owned());
        environment.insert(
            "GIT_CONFIG_GLOBAL".to_owned(),
            self.config.join("global.gitconfig").display().to_string(),
        );
        environment.insert("GIT_TERMINAL_PROMPT".to_owned(), "0".to_owned());
        environment.insert("GIT_PAGER".to_owned(), "cat".to_owned());
        environment.insert("PAGER".to_owned(), "cat".to_owned());
        environment.insert("GIT_AUTHOR_NAME".to_owned(), "LeanToken Test".to_owned());
        environment.insert(
            "GIT_AUTHOR_EMAIL".to_owned(),
            "leantoken-test@example.invalid".to_owned(),
        );
        environment.insert("GIT_COMMITTER_NAME".to_owned(), "LeanToken Test".to_owned());
        environment.insert(
            "GIT_COMMITTER_EMAIL".to_owned(),
            "leantoken-test@example.invalid".to_owned(),
        );
        environment.insert("LC_ALL".to_owned(), "C".to_owned());
        environment.insert("LANG".to_owned(), "C".to_owned());
        environment
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
        .unwrap_or_else(std::env::temp_dir)
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
    use super::{Sandbox, rerun_command, stable_id};

    #[test]
    fn creates_isolated_capability_directories() {
        let sandbox =
            Sandbox::new(module_path!(), "creates_isolated_capability_directories").unwrap();
        assert!(sandbox.repo().is_dir());
        assert!(sandbox.cache().is_dir());
        assert_eq!(sandbox.environment().get("LC_ALL").unwrap(), "C");
        assert!(!sandbox.environment().contains_key("RUSTUP_TOOLCHAIN"));
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
        assert_eq!(first.id(), second.id());
        assert_ne!(first.preservation_id, second.preservation_id);
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
