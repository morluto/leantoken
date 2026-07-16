use std::{
    path::{Path, PathBuf},
    time::Duration,
};

use crate::tokens::Tokenizer;
use crate::{Error, Result};

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
    /// Largest file admitted to the index.
    pub max_file_bytes: u64,
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
    /// aliases.
    pub fn discover(root: impl AsRef<Path>, database_path: Option<PathBuf>) -> Result<Self> {
        let root = root.as_ref().canonicalize().map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                Error::RootNotFound(root.as_ref().to_path_buf())
            } else {
                Error::Io(error)
            }
        })?;
        if !root.is_dir() {
            return Err(Error::InvalidRequest(format!(
                "repository root is not a directory: {}",
                root.display()
            )));
        }
        let database_is_managed_cache = database_path.is_none();
        let database_path = database_path.unwrap_or_else(|| default_database_path(&root));
        let database_path = canonicalize_database_path(database_path);
        Ok(Self {
            root,
            database_path,
            database_is_managed_cache,
            max_file_bytes: 2 * 1024 * 1024,
            default_results: 20,
            max_results: 100,
            default_read_tokens: 8_000,
            max_output_tokens: 32_000,
            context_lines: 2,
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

    #[must_use]
    pub(crate) fn is_database_artifact_path(&self, candidate: &Path) -> bool {
        if candidate == self.database_path {
            return true;
        }
        ["-wal", "-shm", ".leader.lock", ".index.lock", ".init.lock"]
            .into_iter()
            .any(|suffix| {
                let mut sidecar = self.database_path.as_os_str().to_os_string();
                sidecar.push(suffix);
                candidate.as_os_str() == sidecar
            })
    }
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

fn default_database_path(root: &Path) -> PathBuf {
    let root_hash = blake3::hash(root.to_string_lossy().as_bytes()).to_hex();
    if let Some(project_dirs) = directories::ProjectDirs::from("dev", "LeanToken", "leantoken") {
        return project_dirs
            .cache_dir()
            .join(&root_hash.as_str()[..16])
            .join("index.sqlite");
    }
    root.join(".leantoken").join("index.sqlite")
}
