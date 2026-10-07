use std::fs;
use std::path::{Path, PathBuf};
use tempfile::Builder;

#[derive(Debug)]
pub struct Sandbox {
    root: PathBuf,
    repo: PathBuf,
    rerun: String,
}

impl Sandbox {
    /// Create an isolated test tree. `module` and `callsite` form the stable
    /// diagnostic identity; the directory itself still receives a unique
    /// suffix so concurrent tests cannot collide.
    pub fn new(module: &str, callsite: &str) -> std::io::Result<Self> {
        let id = stable_id(module, callsite);
        let workspace_root = workspace_root();
        let parent = workspace_root.join("target").join("test-sandboxes");
        fs::create_dir_all(&parent)?;

        let root = Builder::new()
            .prefix(&format!("{id}-"))
            .tempdir_in(&parent)?
            .keep();
        let sandbox = Self {
            repo: root.join("repo"),
            root,
            rerun: rerun_command(module, callsite, std::thread::current().name()),
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
        if let Some(name) = self.root.file_name() {
            let destination = failure_root.join(name);
            let _ = fs::rename(&self.root, &destination);
            eprintln!(
                "LeanToken test sandbox preserved: {}",
                destination.display()
            );
            eprintln!("LeanToken focused rerun: {}", self.rerun);
        }
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
