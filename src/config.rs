use std::{
    path::{Path, PathBuf},
    time::Duration,
};

use crate::repository::DiscoveryPolicy;
use crate::tokens::Tokenizer;
use crate::{Error, Result};

pub(crate) const DEFAULT_RESULTS: usize = 20;
pub(crate) const MAX_RESULTS: usize = 100;
pub(crate) const DEFAULT_READ_TOKENS: usize = 8_000;
pub(crate) const MAX_OUTPUT_TOKENS: usize = 32_000;
pub(crate) const DEFAULT_CONTEXT_LINES: usize = 2;

/// Hard repository discovery and preparation limits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiscoveryLimits {
    /// Maximum filesystem entries yielded by one repository walk.
    pub max_walk_entries: u64,
    /// Maximum files admitted to one repository index.
    pub max_files: u64,
    /// Maximum aggregate bytes of files admitted to one repository index.
    pub max_total_source_bytes: u64,
    /// Maximum repository-relative depth below the root.
    pub max_depth: usize,
    /// Maximum bytes admitted from one file.
    pub max_file_bytes: u64,
    /// Maximum files scheduled in one preparation batch.
    pub max_prepare_batch_files: usize,
    /// Maximum discovered source bytes scheduled in one preparation batch.
    pub max_prepare_batch_bytes: u64,
}

impl DiscoveryLimits {
    /// Default maximum filesystem entries yielded by one walk.
    pub const DEFAULT_MAX_WALK_ENTRIES: u64 = 500_000;
    /// Default maximum admitted source files.
    pub const DEFAULT_MAX_FILES: u64 = 150_000;
    /// Default maximum aggregate admitted source bytes.
    pub const DEFAULT_MAX_TOTAL_SOURCE_BYTES: u64 = 2 * 1024 * 1024 * 1024;
    /// Default maximum repository-relative depth.
    pub const DEFAULT_MAX_DEPTH: usize = 64;
    /// Default maximum bytes admitted from one file.
    pub const DEFAULT_MAX_FILE_BYTES: u64 = 2 * 1024 * 1024;
    /// Default maximum files scheduled in one preparation batch.
    pub const DEFAULT_MAX_PREPARE_BATCH_FILES: usize = 256;
    /// Default maximum source bytes scheduled in one preparation batch.
    pub const DEFAULT_MAX_PREPARE_BATCH_BYTES: u64 = 64 * 1024 * 1024;

    pub(crate) fn validate(self) -> Result<()> {
        if self.max_walk_entries == 0
            || self.max_files == 0
            || self.max_total_source_bytes == 0
            || self.max_depth == 0
            || self.max_file_bytes == 0
            || self.max_prepare_batch_files == 0
            || self.max_prepare_batch_bytes == 0
        {
            return Err(Error::InvalidConfiguration(
                "repository discovery limits must be positive".into(),
            ));
        }
        if self.max_prepare_batch_bytes < self.max_file_bytes {
            return Err(Error::InvalidConfiguration(
                "max_prepare_batch_bytes must be at least max_file_bytes".into(),
            ));
        }
        Ok(())
    }
}

impl Default for DiscoveryLimits {
    fn default() -> Self {
        Self {
            max_walk_entries: Self::DEFAULT_MAX_WALK_ENTRIES,
            max_files: Self::DEFAULT_MAX_FILES,
            max_total_source_bytes: Self::DEFAULT_MAX_TOTAL_SOURCE_BYTES,
            max_depth: Self::DEFAULT_MAX_DEPTH,
            max_file_bytes: Self::DEFAULT_MAX_FILE_BYTES,
            max_prepare_batch_files: Self::DEFAULT_MAX_PREPARE_BATCH_FILES,
            max_prepare_batch_bytes: Self::DEFAULT_MAX_PREPARE_BATCH_BYTES,
        }
    }
}

#[derive(Debug, Clone)]
/// Resolved repository paths and bounded runtime defaults.
pub struct Config {
    /// Canonical repository root.
    pub root: PathBuf,
    /// SQLite index path.
    pub database_path: PathBuf,
    /// Whether LeanToken owns this cache file and may rebuild it after
    /// confirmed SQLite corruption.
    pub(crate) database_is_managed_cache: bool,
    /// Maximum filesystem entries yielded by one repository walk.
    pub max_walk_entries: u64,
    /// Maximum files admitted to one repository index.
    pub max_files: u64,
    /// Maximum aggregate bytes admitted to one repository index.
    pub max_total_source_bytes: u64,
    /// Maximum repository-relative depth below the root.
    pub max_depth: usize,
    /// Largest file admitted to the index.
    pub max_file_bytes: u64,
    /// Maximum files scheduled in one preparation batch.
    pub max_prepare_batch_files: usize,
    /// Maximum discovered source bytes scheduled in one preparation batch.
    pub max_prepare_batch_bytes: u64,
    /// Whether known generated and package-cache trees are indexed.
    pub include_generated: bool,
    /// Default number of returned results.
    pub default_results: usize,
    /// Maximum number of returned results.
    pub max_results: usize,
    /// Default source-token limit for reads and searches.
    pub default_read_tokens: usize,
    /// Hard source-token ceiling for any response.
    pub max_output_tokens: usize,
    /// Default lines included around a search match.
    pub context_lines: usize,
    /// Maximum lines per searchable chunk.
    pub chunk_lines: usize,
    /// Maximum bytes per searchable chunk.
    pub chunk_bytes: usize,
    /// Maximum parallel file-preparation workers.
    pub max_index_workers: usize,
    /// Filesystem-event debounce interval.
    pub watcher_debounce: Duration,
    /// Tokenizer used for all source and protocol token accounting.
    pub tokenizer: Tokenizer,
}

impl Config {
    /// Resolve a repository root and apply bounded defaults.
    ///
    /// When `database_path` is absent, LeanToken chooses a per-repository cache
    /// path outside the source tree when the platform provides one. An existing
    /// explicit database, or otherwise its existing parent, is canonicalized so
    /// coordination and repository discovery use one cache identity across path
    /// aliases. Filesystem roots, the current user's home directory, and parents
    /// of that home directory are rejected by default.
    pub fn discover(root: impl AsRef<Path>, database_path: Option<PathBuf>) -> Result<Self> {
        Self::discover_with_broad_root(root, database_path, false)
    }

    /// Resolve a repository root with an explicit broad-root safety override.
    ///
    /// Set `allow_broad_root` only when indexing a filesystem root, the current
    /// user's home directory, or one of its parents is deliberate.
    pub fn discover_with_broad_root(
        root: impl AsRef<Path>,
        database_path: Option<PathBuf>,
        allow_broad_root: bool,
    ) -> Result<Self> {
        let root = root.as_ref().canonicalize().map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                Error::RootNotFound(root.as_ref().to_path_buf())
            } else {
                Error::Io(error)
            }
        })?;
        if !root.is_dir() {
            return Err(Error::InvalidConfiguration(format!(
                "repository root is not a directory: {}",
                root.display()
            )));
        }
        if !allow_broad_root && is_unsafe_repository_root(&root, home_directory().as_deref()) {
            return Err(Error::UnsafeRepositoryRoot(root));
        }
        let database_is_managed_cache = database_path.is_none();
        let database_path = database_path.unwrap_or_else(|| default_database_path(&root));
        let database_path = canonicalize_database_path(database_path);
        Ok(Self {
            root,
            database_path,
            database_is_managed_cache,
            max_walk_entries: DiscoveryLimits::DEFAULT_MAX_WALK_ENTRIES,
            max_files: DiscoveryLimits::DEFAULT_MAX_FILES,
            max_total_source_bytes: DiscoveryLimits::DEFAULT_MAX_TOTAL_SOURCE_BYTES,
            max_depth: DiscoveryLimits::DEFAULT_MAX_DEPTH,
            max_file_bytes: DiscoveryLimits::DEFAULT_MAX_FILE_BYTES,
            max_prepare_batch_files: DiscoveryLimits::DEFAULT_MAX_PREPARE_BATCH_FILES,
            max_prepare_batch_bytes: DiscoveryLimits::DEFAULT_MAX_PREPARE_BATCH_BYTES,
            include_generated: false,
            default_results: DEFAULT_RESULTS,
            max_results: MAX_RESULTS,
            default_read_tokens: DEFAULT_READ_TOKENS,
            max_output_tokens: MAX_OUTPUT_TOKENS,
            context_lines: DEFAULT_CONTEXT_LINES,
            chunk_lines: 80,
            chunk_bytes: 32 * 1024,
            max_index_workers: std::thread::available_parallelism()
                .map_or(1, std::num::NonZero::get)
                .min(8),
            watcher_debounce: Duration::from_millis(500),
            tokenizer: Tokenizer::default(),
        })
    }

    /// Return whether a repository-relative path names the SQLite database,
    /// one of its sidecars, or a coordination lock.
    #[must_use]
    pub fn is_database_artifact(&self, relative_path: &str) -> bool {
        self.is_database_artifact_path(&self.root.join(relative_path))
    }

    /// Return one immutable snapshot of repository discovery limits.
    #[must_use]
    pub fn discovery_limits(&self) -> DiscoveryLimits {
        DiscoveryLimits {
            max_walk_entries: self.max_walk_entries,
            max_files: self.max_files,
            max_total_source_bytes: self.max_total_source_bytes,
            max_depth: self.max_depth,
            max_file_bytes: self.max_file_bytes,
            max_prepare_batch_files: self.max_prepare_batch_files,
            max_prepare_batch_bytes: self.max_prepare_batch_bytes,
        }
    }

    /// Return one immutable repository visibility policy.
    #[must_use]
    pub fn discovery_policy(&self) -> DiscoveryPolicy {
        DiscoveryPolicy::new(self.include_generated)
    }

    #[must_use]
    pub(crate) fn is_database_artifact_path(&self, candidate: &Path) -> bool {
        if candidate == self.database_path {
            return true;
        }
        [
            "-wal",
            "-shm",
            ".lease.lock",
            ".leader.lock",
            ".index.lock",
            ".init.lock",
        ]
        .into_iter()
        .any(|suffix| {
            let mut sidecar = self.database_path.as_os_str().to_os_string();
            sidecar.push(suffix);
            candidate.as_os_str() == sidecar
        })
    }
}

fn home_directory() -> Option<PathBuf> {
    directories::BaseDirs::new().map(|directories| {
        directories
            .home_dir()
            .canonicalize()
            .unwrap_or_else(|_| directories.home_dir().to_path_buf())
    })
}

fn is_unsafe_repository_root(root: &Path, home: Option<&Path>) -> bool {
    root.parent().is_none() || home.is_some_and(|home| home.starts_with(root))
}

fn canonicalize_database_path(path: PathBuf) -> PathBuf {
    let path = std::path::absolute(&path).unwrap_or(path);
    if let Ok(canonical) = path.canonicalize() {
        return canonical;
    }

    let mut ancestor = path.as_path();
    let mut missing = Vec::new();
    loop {
        if let Ok(canonical) = ancestor.canonicalize() {
            return missing
                .iter()
                .rev()
                .fold(canonical, |resolved, component| resolved.join(component));
        }
        let Some(component) = ancestor.file_name() else {
            return path;
        };
        missing.push(component.to_os_string());
        let Some(parent) = ancestor.parent() else {
            return path;
        };
        ancestor = parent;
    }
}

pub(crate) fn managed_cache_root() -> Option<PathBuf> {
    directories::ProjectDirs::from("dev", "LeanToken", "leantoken")
        .map(|project_dirs| project_dirs.cache_dir().to_path_buf())
}

pub(crate) fn managed_cache_id(root: &Path) -> String {
    blake3::hash(root.to_string_lossy().as_bytes()).to_hex()[..16].to_string()
}

fn default_database_path(root: &Path) -> PathBuf {
    if let Some(cache_root) = managed_cache_root() {
        return cache_root.join(managed_cache_id(root)).join("index.sqlite");
    }
    root.join(".leantoken").join("index.sqlite")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unsafe_root_policy_rejects_home_and_its_ancestors() {
        let directory = tempfile::tempdir().expect("directory");
        let home = directory.path().join("users/example");
        std::fs::create_dir_all(&home).expect("home");

        assert!(is_unsafe_repository_root(directory.path(), Some(&home)));
        assert!(is_unsafe_repository_root(&home, Some(&home)));
        assert!(!is_unsafe_repository_root(
            &directory.path().join("workspace"),
            Some(&home)
        ));
    }

    #[test]
    fn unsafe_root_policy_rejects_a_filesystem_root_without_home_context() {
        let root = std::env::current_dir()
            .expect("current directory")
            .ancestors()
            .last()
            .expect("filesystem root")
            .to_path_buf();

        assert!(is_unsafe_repository_root(&root, None));
    }
}
