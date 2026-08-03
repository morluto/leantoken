use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
    time::Duration,
};

use toml_edit::DocumentMut;

use crate::coordination::{
    is_coordination_sidecar_for_database, is_recognized_stale_coordination_sidecar,
};
use crate::repository::{DiscoveryPolicy, IndexScope};
use crate::tokens::Tokenizer;
use crate::{Error, Result};

pub(crate) const DEFAULT_RESULTS: usize = 20;
pub(crate) const MAX_RESULTS: usize = 100;
pub(crate) const DEFAULT_READ_TOKENS: usize = 8_000;
pub(crate) const DEFAULT_CONTEXT_TOKENS: usize = 3_000;
pub(crate) const DEFAULT_CONTEXT_FRAGMENTS: usize = 8;
pub(crate) const MAX_OUTPUT_TOKENS: usize = 32_000;
pub(crate) const DEFAULT_CONTEXT_LINES: usize = 2;
pub(crate) const MAX_CONTEXT_LINES: usize = 20;
pub(crate) const INDEX_CONTENT_VERSION: u32 = 13;
const REPOSITORY_CONFIG_FILE: &str = ".leantoken.toml";
const MAX_REPOSITORY_CONFIG_BYTES: u64 = 64 * 1024;
const MAX_CONTEXT_EXCLUDE_PATHS: usize = 256;
const MAX_CONTEXT_PATH_PATTERN_BYTES: usize = 4 * 1024;
const MANAGED_CACHE_HASH_BYTES: usize = 16;
const FALLBACK_CACHE_DIRECTORY: &str = ".leantoken";
pub(crate) const DEFAULT_CONTEXT_EXCLUDE_PATHS: &[&str] = &[
    "artifacts/runtime_reports/**",
    "artifacts/viability_audit/**",
    "artifacts/replay_reports/**",
    "notes/runs/**",
    "node_modules/**",
];

pub(crate) fn default_context_exclude_paths() -> Vec<String> {
    DEFAULT_CONTEXT_EXCLUDE_PATHS
        .iter()
        .map(|pattern| (*pattern).to_owned())
        .collect()
}

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
    /// Whether a managed platform cache was replaced by the repository-local
    /// fallback because the preferred location was not writable.
    pub(crate) database_uses_repository_fallback: bool,
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
    /// Immutable, cache-identified repository indexing boundary.
    index_scope: IndexScope,
    /// Repository-relative patterns excluded from context unless explicitly included.
    pub context_exclude_paths: Vec<String>,
    /// Default number of returned results.
    pub default_results: usize,
    /// Maximum number of returned results, up to the public protocol ceiling.
    pub max_results: usize,
    /// Default source-token limit for reads and searches.
    pub default_read_tokens: usize,
    /// Default source-token budget for assembled task context.
    pub default_context_tokens: usize,
    /// Hard source-token ceiling for any response, up to the public protocol ceiling.
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
        Self::discover_scoped_with_broad_root(root, database_path, false, IndexScope::default())
    }

    /// Resolve a repository root with an explicit cache-identified index scope.
    pub fn discover_scoped(
        root: impl AsRef<Path>,
        database_path: Option<PathBuf>,
        index_scope: IndexScope,
    ) -> Result<Self> {
        Self::discover_scoped_with_broad_root(root, database_path, false, index_scope)
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
        Self::discover_scoped_with_broad_root(
            root,
            database_path,
            allow_broad_root,
            IndexScope::default(),
        )
    }

    /// Resolve a repository root, broad-root policy, and explicit index scope.
    pub fn discover_scoped_with_broad_root(
        root: impl AsRef<Path>,
        database_path: Option<PathBuf>,
        allow_broad_root: bool,
        index_scope: IndexScope,
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
        let database_path =
            database_path.unwrap_or_else(|| default_database_path_for_scope(&root, &index_scope));
        let database_path = if database_is_managed_cache {
            canonicalize_managed_database_path(database_path)
        } else {
            canonicalize_database_path(database_path)
        };
        let context_exclude_paths = load_context_exclude_paths(&root)?;
        Ok(Self {
            root,
            database_path,
            database_is_managed_cache,
            database_uses_repository_fallback: false,
            max_walk_entries: DiscoveryLimits::DEFAULT_MAX_WALK_ENTRIES,
            max_files: DiscoveryLimits::DEFAULT_MAX_FILES,
            max_total_source_bytes: DiscoveryLimits::DEFAULT_MAX_TOTAL_SOURCE_BYTES,
            max_depth: DiscoveryLimits::DEFAULT_MAX_DEPTH,
            max_file_bytes: DiscoveryLimits::DEFAULT_MAX_FILE_BYTES,
            max_prepare_batch_files: DiscoveryLimits::DEFAULT_MAX_PREPARE_BATCH_FILES,
            max_prepare_batch_bytes: DiscoveryLimits::DEFAULT_MAX_PREPARE_BATCH_BYTES,
            include_generated: false,
            index_scope,
            context_exclude_paths,
            default_results: DEFAULT_RESULTS,
            max_results: MAX_RESULTS,
            default_read_tokens: DEFAULT_READ_TOKENS,
            default_context_tokens: DEFAULT_CONTEXT_TOKENS,
            max_output_tokens: MAX_OUTPUT_TOKENS,
            context_lines: DEFAULT_CONTEXT_LINES,
            chunk_lines: 80,
            chunk_bytes: 32 * 1024,
            max_index_workers: std::thread::available_parallelism()
                .map_or(1, std::num::NonZero::get)
                .min(4),
            watcher_debounce: Duration::from_millis(500),
            tokenizer: Tokenizer::default(),
        })
    }

    pub(crate) fn validate(&self) -> Result<()> {
        self.discovery_limits().validate()?;
        if self.default_results == 0 || self.max_results == 0 {
            return Err(Error::InvalidConfiguration(
                "default_results and max_results must be positive".into(),
            ));
        }
        if self.default_results > self.max_results {
            return Err(Error::InvalidConfiguration(
                "default_results must not exceed max_results".into(),
            ));
        }
        if self.max_results > MAX_RESULTS {
            return Err(Error::InvalidConfiguration(format!(
                "max_results must not exceed {MAX_RESULTS}"
            )));
        }
        if self.default_read_tokens == 0
            || self.default_context_tokens == 0
            || self.max_output_tokens == 0
        {
            return Err(Error::InvalidConfiguration(
                "default_read_tokens, default_context_tokens, and max_output_tokens must be positive"
                    .into(),
            ));
        }
        if self.default_read_tokens > self.max_output_tokens {
            return Err(Error::InvalidConfiguration(
                "default_read_tokens must not exceed max_output_tokens".into(),
            ));
        }
        if self.default_context_tokens > self.max_output_tokens {
            return Err(Error::InvalidConfiguration(
                "default_context_tokens must not exceed max_output_tokens".into(),
            ));
        }
        if self.max_output_tokens > MAX_OUTPUT_TOKENS {
            return Err(Error::InvalidConfiguration(format!(
                "max_output_tokens must not exceed {MAX_OUTPUT_TOKENS}"
            )));
        }
        if self.context_lines > MAX_CONTEXT_LINES {
            return Err(Error::InvalidConfiguration(format!(
                "context_lines must not exceed {MAX_CONTEXT_LINES}"
            )));
        }
        if self.chunk_lines == 0 || self.chunk_bytes == 0 {
            return Err(Error::InvalidConfiguration(
                "chunk_lines and chunk_bytes must be positive".into(),
            ));
        }
        if self.max_index_workers == 0 {
            return Err(Error::InvalidConfiguration(
                "max_index_workers must be positive".into(),
            ));
        }
        validate_context_exclude_paths(&self.context_exclude_paths)?;
        Ok(())
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
        DiscoveryPolicy::new(self.include_generated).with_index_scope(self.index_scope.clone())
    }

    /// Return the immutable cache-identified indexing boundary.
    #[must_use]
    pub fn index_scope(&self) -> &IndexScope {
        &self.index_scope
    }

    #[must_use]
    pub(crate) fn is_database_artifact_path(&self, candidate: &Path) -> bool {
        let fallback_cache = self.root.join(FALLBACK_CACHE_DIRECTORY);
        if self.database_is_managed_cache
            && self.database_path.starts_with(&fallback_cache)
            && candidate.starts_with(&fallback_cache)
        {
            return true;
        }
        if candidate == self.database_path {
            return true;
        }
        ["-wal", "-shm"].into_iter().any(|suffix| {
            let mut sidecar = self.database_path.as_os_str().to_os_string();
            sidecar.push(suffix);
            candidate.as_os_str() == sidecar
        }) || is_coordination_sidecar_for_database(candidate, &self.database_path)
            || is_recognized_stale_coordination_sidecar(candidate)
    }

    pub(crate) fn repository_cache_fallback(&self) -> Option<Self> {
        if !self.database_is_managed_cache || self.database_uses_repository_fallback {
            return None;
        }
        let mut fallback = self.clone();
        fallback.database_path = repository_fallback_database_path(&self.root, &self.index_scope);
        fallback.database_uses_repository_fallback = true;
        Some(fallback)
    }

    /// Return whether the active managed index uses repository-local storage.
    #[must_use]
    pub const fn uses_repository_cache_fallback(&self) -> bool {
        self.database_uses_repository_fallback
    }
}

fn load_context_exclude_paths(root: &Path) -> Result<Vec<String>> {
    let path = root.join(REPOSITORY_CONFIG_FILE);
    let metadata = match fs::metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(default_context_exclude_paths());
        }
        Err(error) => return Err(error.into()),
    };
    if metadata.len() > MAX_REPOSITORY_CONFIG_BYTES {
        return Err(Error::InvalidConfiguration(format!(
            "{REPOSITORY_CONFIG_FILE} exceeds the {MAX_REPOSITORY_CONFIG_BYTES}-byte limit"
        )));
    }
    let source = fs::read_to_string(path)?;
    if u64::try_from(source.len()).unwrap_or(u64::MAX) > MAX_REPOSITORY_CONFIG_BYTES {
        return Err(Error::InvalidConfiguration(format!(
            "{REPOSITORY_CONFIG_FILE} exceeds the {MAX_REPOSITORY_CONFIG_BYTES}-byte limit"
        )));
    }
    let document = source.parse::<DocumentMut>().map_err(|error| {
        Error::InvalidConfiguration(format!("invalid {REPOSITORY_CONFIG_FILE}: {error}"))
    })?;
    let mut patterns = default_context_exclude_paths();
    let Some(context) = document.get("context") else {
        return Ok(patterns);
    };
    let context = context.as_table().ok_or_else(|| {
        Error::InvalidConfiguration(format!(
            "{REPOSITORY_CONFIG_FILE} field `context` must be a table"
        ))
    })?;
    let Some(exclude_paths) = context.get("exclude_paths") else {
        return Ok(patterns);
    };
    let exclude_paths = exclude_paths.as_array().ok_or_else(|| {
        Error::InvalidConfiguration(format!(
            "{REPOSITORY_CONFIG_FILE} field `context.exclude_paths` must be an array"
        ))
    })?;
    for value in exclude_paths {
        let pattern = value.as_str().ok_or_else(|| {
            Error::InvalidConfiguration(format!(
                "{REPOSITORY_CONFIG_FILE} field `context.exclude_paths` must contain only strings"
            ))
        })?;
        patterns.push(pattern.to_owned());
    }
    let mut seen = HashSet::new();
    patterns.retain(|pattern| seen.insert(pattern.clone()));
    validate_context_exclude_paths(&patterns)?;
    Ok(patterns)
}

fn validate_context_exclude_paths(patterns: &[String]) -> Result<()> {
    if patterns.len() > MAX_CONTEXT_EXCLUDE_PATHS {
        return Err(Error::InvalidConfiguration(format!(
            "context_exclude_paths must not contain more than {MAX_CONTEXT_EXCLUDE_PATHS} patterns"
        )));
    }
    for pattern in patterns {
        if pattern.trim_matches(['/', '\\']).is_empty() {
            return Err(Error::InvalidConfiguration(
                "context_exclude_paths must not contain empty patterns".into(),
            ));
        }
        if pattern.len() > MAX_CONTEXT_PATH_PATTERN_BYTES {
            return Err(Error::InvalidConfiguration(format!(
                "context exclusion patterns must not exceed {MAX_CONTEXT_PATH_PATTERN_BYTES} bytes"
            )));
        }
        crate::repository::RepositoryPattern::parse(pattern).map_err(|error| {
            Error::InvalidConfiguration(format!(
                "invalid context exclusion pattern `{pattern}`: {error}"
            ))
        })?;
    }
    Ok(())
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

fn canonicalize_managed_database_path(path: PathBuf) -> PathBuf {
    let path = std::path::absolute(&path).unwrap_or(path);
    let Some(database_name) = path.file_name() else {
        return path;
    };
    let Some(parent) = path.parent() else {
        return path;
    };
    canonicalize_database_path(parent.to_path_buf()).join(database_name)
}

pub(crate) fn managed_cache_root() -> Option<PathBuf> {
    directories::ProjectDirs::from("dev", "LeanToken", "leantoken")
        .map(|project_dirs| project_dirs.cache_dir().to_path_buf())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ManagedCacheIdentity {
    Unversioned,
    Versioned {
        version: u32,
        root_hash: String,
        scope_digest: Option<String>,
    },
}

#[cfg(test)]
pub(crate) fn managed_cache_id(root: &Path) -> String {
    managed_cache_id_for_scope(root, &IndexScope::default())
}

pub(crate) fn managed_cache_id_for_scope(root: &Path, scope: &IndexScope) -> String {
    managed_cache_id_for_version(root, INDEX_CONTENT_VERSION, scope.digest())
}

pub(crate) fn parse_managed_cache_id(value: &str) -> Option<ManagedCacheIdentity> {
    if is_managed_cache_hash(value) {
        return Some(ManagedCacheIdentity::Unversioned);
    }
    let (version_text, remainder) = value.strip_prefix('v')?.split_once('-')?;
    let version = version_text
        .parse::<u32>()
        .ok()
        .filter(|version| *version > 0)?;
    let mut parts = remainder.split('-');
    let root_hash = parts.next()?;
    let scope_digest = match parts.next() {
        Some(scope) => Some(scope.strip_prefix('s')?.to_owned()),
        None => None,
    };
    if parts.next().is_some()
        || version.to_string() != version_text
        || !is_managed_cache_hash(root_hash)
        || scope_digest
            .as_deref()
            .is_some_and(|digest| !is_managed_cache_hash(digest))
    {
        return None;
    }
    Some(ManagedCacheIdentity::Versioned {
        version,
        root_hash: root_hash.to_owned(),
        scope_digest,
    })
}

pub(crate) fn managed_cache_id_matches_root(value: &str, root: &Path) -> bool {
    let hash = managed_cache_root_hash(root);
    match parse_managed_cache_id(value) {
        Some(ManagedCacheIdentity::Unversioned) => value == hash,
        Some(ManagedCacheIdentity::Versioned { root_hash, .. }) => root_hash == hash,
        None => false,
    }
}

fn managed_cache_id_for_version(root: &Path, version: u32, scope_digest: Option<&str>) -> String {
    let base = format!("v{version}-{}", managed_cache_root_hash(root));
    scope_digest.map_or(base.clone(), |scope| format!("{base}-s{scope}"))
}

fn managed_cache_root_hash(root: &Path) -> String {
    blake3::hash(root.as_os_str().as_encoded_bytes()).to_hex()[..MANAGED_CACHE_HASH_BYTES]
        .to_string()
}

fn is_managed_cache_hash(value: &str) -> bool {
    value.len() == MANAGED_CACHE_HASH_BYTES
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

#[cfg(test)]
fn default_database_path(root: &Path) -> PathBuf {
    default_database_path_for_scope(root, &IndexScope::default())
}

fn default_database_path_for_scope(root: &Path, scope: &IndexScope) -> PathBuf {
    if let Some(cache_root) = managed_cache_root() {
        return cache_root
            .join(managed_cache_id_for_scope(root, scope))
            .join("index.sqlite");
    }
    repository_fallback_database_path(root, scope)
}

fn repository_fallback_database_path(root: &Path, scope: &IndexScope) -> PathBuf {
    let cache_directory = scope.digest().map_or_else(
        || format!("v{INDEX_CONTENT_VERSION}"),
        |digest| format!("v{INDEX_CONTENT_VERSION}-s{digest}"),
    );
    root.join(FALLBACK_CACHE_DIRECTORY)
        .join(cache_directory)
        .join("index.sqlite")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn managed_cache_ids_distinguish_non_utf8_roots() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;

        let first = PathBuf::from(OsString::from_vec(b"/tmp/repository-\x80".to_vec()));
        let second = PathBuf::from(OsString::from_vec(b"/tmp/repository-\x81".to_vec()));

        assert_ne!(managed_cache_id(&first), managed_cache_id(&second));
    }

    #[test]
    fn managed_cache_identity_is_versioned_and_strictly_parsed() {
        let root = Path::new("/tmp/repository");
        let id = managed_cache_id(root);
        let unversioned_id = id
            .split_once('-')
            .expect("versioned managed cache identity")
            .1;

        assert!(id.starts_with(&format!("v{INDEX_CONTENT_VERSION}-")));
        assert_ne!(id, unversioned_id);
        assert!(matches!(
            parse_managed_cache_id(&id),
            Some(ManagedCacheIdentity::Versioned {
                version: INDEX_CONTENT_VERSION,
                scope_digest: None,
                ..
            })
        ));
        assert!(managed_cache_id_matches_root(&id, root));
        assert!(managed_cache_id_matches_root(unversioned_id, root));
        assert_ne!(
            managed_cache_id_for_version(root, INDEX_CONTENT_VERSION - 1, None),
            id
        );
        assert_eq!(
            parse_managed_cache_id("0000000000000001"),
            Some(ManagedCacheIdentity::Unversioned)
        );
        assert!(parse_managed_cache_id("v0-0000000000000001").is_none());
        assert!(parse_managed_cache_id("v01-0000000000000001").is_none());
        assert!(parse_managed_cache_id("v12-000000000000000g").is_none());
    }

    #[test]
    fn managed_cache_identity_distinguishes_normalized_index_scopes() {
        let root = Path::new("/tmp/repository");
        let first = IndexScope::new(vec!["src\\**".into()], vec!["src/generated/**".into()])
            .expect("first scope");
        let equivalent = IndexScope::new(
            vec!["./src/**".into(), "src/**".into()],
            vec!["src//generated/**".into()],
        )
        .expect("equivalent scope");
        let different =
            IndexScope::new(vec!["tests/**".into()], Vec::new()).expect("different scope");

        let first_id = managed_cache_id_for_scope(root, &first);
        assert_eq!(first_id, managed_cache_id_for_scope(root, &equivalent));
        assert_ne!(first_id, managed_cache_id(root));
        assert_ne!(first_id, managed_cache_id_for_scope(root, &different));
        assert!(managed_cache_id_matches_root(&first_id, root));
        assert!(matches!(
            parse_managed_cache_id(&first_id),
            Some(ManagedCacheIdentity::Versioned {
                version: INDEX_CONTENT_VERSION,
                scope_digest: Some(_),
                ..
            })
        ));
    }

    #[test]
    fn default_database_path_uses_the_index_content_identity() {
        let root = Path::new("/tmp/repository");
        let database = default_database_path(root);

        if managed_cache_root().is_some() {
            assert_eq!(
                database.parent().and_then(Path::file_name),
                Some(managed_cache_id(root).as_ref())
            );
        } else {
            assert_eq!(
                database,
                root.join(FALLBACK_CACHE_DIRECTORY)
                    .join(format!("v{INDEX_CONTENT_VERSION}"))
                    .join("index.sqlite")
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn managed_database_path_preserves_a_final_symlink() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().expect("repository");
        let target = root.path().join("target.sqlite");
        let link = root.path().join("index.sqlite");
        fs::write(&target, b"not sqlite").expect("target");
        symlink(&target, &link).expect("database symlink");

        let expected = fs::canonicalize(root.path())
            .expect("canonical repository path")
            .join("index.sqlite");
        assert_eq!(canonicalize_managed_database_path(link), expected);
    }

    #[test]
    fn managed_fallback_excludes_current_and_unversioned_cache_artifacts() {
        let root = tempfile::tempdir().expect("repository");
        let database = root
            .path()
            .join(FALLBACK_CACHE_DIRECTORY)
            .join(format!("v{INDEX_CONTENT_VERSION}"))
            .join("index.sqlite");
        let mut config = Config::discover(root.path(), Some(database)).expect("config");
        config.database_is_managed_cache = true;
        let current_database =
            format!("{FALLBACK_CACHE_DIRECTORY}/v{INDEX_CONTENT_VERSION}/index.sqlite");

        assert!(config.is_database_artifact(&current_database));
        assert!(config.is_database_artifact(".leantoken/index.sqlite"));
        assert!(config.is_database_artifact(".leantoken/index.sqlite-wal"));
        assert!(!config.is_database_artifact(".leantoken.toml"));
    }

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

    #[test]
    fn repository_config_extends_default_context_exclusions() {
        let root = tempfile::tempdir().expect("root");
        fs::write(
            root.path().join(REPOSITORY_CONFIG_FILE),
            "[context]\nexclude_paths = [\"generated/**\", \"notes/runs/**\"]\n",
        )
        .expect("repository config");

        let config = Config::discover(root.path(), Some(root.path().join("index.sqlite")))
            .expect("resolved config");

        assert_eq!(
            config.context_exclude_paths,
            [
                DEFAULT_CONTEXT_EXCLUDE_PATHS
                    .iter()
                    .map(|pattern| (*pattern).to_owned())
                    .collect::<Vec<_>>(),
                vec!["generated/**".into()],
            ]
            .concat()
        );
    }

    #[test]
    fn repository_config_rejects_invalid_context_exclusions() {
        let root = tempfile::tempdir().expect("root");
        fs::write(
            root.path().join(REPOSITORY_CONFIG_FILE),
            "[context]\nexclude_paths = \"generated/**\"\n",
        )
        .expect("repository config");

        let error = Config::discover(root.path(), Some(root.path().join("index.sqlite")))
            .expect_err("invalid repository config");

        assert!(
            error
                .to_string()
                .contains("context.exclude_paths` must be an array")
        );
    }

    #[test]
    fn usage_documents_default_context_exclusions() {
        let usage = include_str!("../docs/usage.md");
        for pattern in DEFAULT_CONTEXT_EXCLUDE_PATHS {
            assert!(
                usage.contains(pattern),
                "usage guide is missing default context exclusion {pattern}"
            );
        }
    }

    #[test]
    fn usage_documents_discovery_defaults() {
        let usage = include_str!("../docs/usage.md");
        let expected = [
            (
                "max-walk-entries",
                DiscoveryLimits::DEFAULT_MAX_WALK_ENTRIES,
            ),
            ("max-files", DiscoveryLimits::DEFAULT_MAX_FILES),
            (
                "max-total-source-bytes",
                DiscoveryLimits::DEFAULT_MAX_TOTAL_SOURCE_BYTES,
            ),
            ("max-depth", DiscoveryLimits::DEFAULT_MAX_DEPTH as u64),
            ("max-file-bytes", DiscoveryLimits::DEFAULT_MAX_FILE_BYTES),
            (
                "max-prepare-batch-files",
                DiscoveryLimits::DEFAULT_MAX_PREPARE_BATCH_FILES as u64,
            ),
            (
                "max-prepare-batch-bytes",
                DiscoveryLimits::DEFAULT_MAX_PREPARE_BATCH_BYTES,
            ),
        ];

        for (option, value) in expected {
            assert!(usage.contains(&format!("--{option}")));
            assert!(
                usage.contains(&format!("default: {value}")),
                "usage guide is missing {option}'s default {value}"
            );
        }
    }
}
