const LEANTOKEN_IGNORE_FILE: &str = ".leantokenignore";
const GIT_PATH_OUTPUT_BYTES_PER_RESULT: usize = 4_096;
const GIT_HUNK_OUTPUT_BYTES_PER_RESULT: usize = 64 * 1024;
const MAX_GIT_DISCOVERY_OUTPUT_BYTES: usize = 8 * 1024 * 1024;
const GENERATED_DIRECTORY_NAMES: &[&str] = &[
    ".cache",
    ".gradle",
    ".mypy_cache",
    ".npm",
    ".pnpm-store",
    ".pytest_cache",
    ".ruff_cache",
    ".rustup",
    ".tox",
    ".venv",
    "__pycache__",
    "node_modules",
    "target",
    "venv",
];
const GENERATED_DIRECTORY_PATHS: &[&[&str]] = &[
    &[".bun", "install", "cache"],
    &[".local", "share"],
    &[".yarn", "cache"],
];

/// Repository visibility policy shared by discovery, reconciliation, and watching.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DiscoveryPolicy {
    include_generated: bool,
}

impl DiscoveryPolicy {
    /// Build a policy, optionally admitting known generated and cache trees.
    #[must_use]
    pub const fn new(include_generated: bool) -> Self {
        Self { include_generated }
    }

    /// Return whether known generated and cache trees are admitted.
    #[must_use]
    pub const fn includes_generated(self) -> bool {
        self.include_generated
    }

    /// Return whether one normalized repository-relative path is visible.
    ///
    /// `path_is_directory` distinguishes a directory named `target` from an
    /// ordinary file with that name. Paths must use the slash-normalized form
    /// returned by [`slash_path`].
    #[must_use]
    pub fn includes_path(self, relative_path: &str, path_is_directory: bool) -> bool {
        self.include_generated || !is_generated_path(relative_path, path_is_directory)
    }

    pub(crate) fn is_ignore_control_path(self, relative_path: &str) -> bool {
        relative_path == ".gitignore"
            || relative_path == ".ignore"
            || relative_path == LEANTOKEN_IGNORE_FILE
            || relative_path.ends_with("/.gitignore")
            || relative_path.ends_with("/.ignore")
            || relative_path.ends_with("/.leantokenignore")
    }
}

#[derive(Debug, Clone)]
pub struct DiscoveredFile {
    pub absolute_path: PathBuf,
    pub relative_path: String,
    pub size_bytes: u64,
    pub modified_ns: Option<u128>,
}

/// Counters collected while walking one repository snapshot.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DiscoveryStats {
    /// Filesystem entries yielded by the ignore-aware walker, including the root.
    pub walk_entries: u64,
    /// Files admitted after ignore, metadata, size, and owner filters.
    pub files: u64,
    /// Aggregate metadata bytes of admitted files.
    pub total_source_bytes: u64,
    /// Deepest yielded entry relative to the repository root.
    pub max_depth: usize,
}

/// Complete bounded result of one repository discovery pass.
#[derive(Debug, Clone)]
pub struct DiscoveryResult {
    /// Admitted repository files sorted by relative path.
    pub files: Vec<DiscoveredFile>,
    /// Traversal and admission counters for the completed pass.
    pub stats: DiscoveryStats,
}

pub fn discover_files(root: &Path, max_file_bytes: u64) -> Result<Vec<DiscoveredFile>> {
    discover_files_cancellable(root, max_file_bytes, &CancellationToken::new())
}

/// Discover repository files while honoring caller-owned cancellation.
pub fn discover_files_cancellable(
    root: &Path,
    max_file_bytes: u64,
    cancellation: &CancellationToken,
) -> Result<Vec<DiscoveredFile>> {
    let limits = DiscoveryLimits {
        max_file_bytes,
        max_prepare_batch_bytes: DiscoveryLimits::DEFAULT_MAX_PREPARE_BATCH_BYTES
            .max(max_file_bytes),
        ..DiscoveryLimits::default()
    };
    Ok(discover_files_with_limits_cancellable(root, limits, cancellation)?.files)
}

/// Discover repository files under explicit hard resource limits.
///
/// # Errors
///
/// Returns a typed limit error at the first value outside an inclusive bound;
/// partial discovery results are never returned.
pub fn discover_files_with_limits(root: &Path, limits: DiscoveryLimits) -> Result<DiscoveryResult> {
    discover_files_with_limits_cancellable(root, limits, &CancellationToken::new())
}

/// Discover repository files under explicit limits and visibility policy.
///
/// # Errors
///
/// Returns a typed limit, traversal, or path error without returning a
/// truncated repository result.
pub fn discover_files_with_limits_and_policy(
    root: &Path,
    limits: DiscoveryLimits,
    policy: DiscoveryPolicy,
) -> Result<DiscoveryResult> {
    discover_files_with_limits_policy_and_filter(
        root,
        limits,
        policy,
        &CancellationToken::new(),
        |_| true,
    )
}

/// Discover repository files under explicit limits and caller-owned cancellation.
///
/// # Errors
///
/// Returns a typed limit error, cancellation, or path error without returning a
/// truncated repository result.
pub fn discover_files_with_limits_cancellable(
    root: &Path,
    limits: DiscoveryLimits,
    cancellation: &CancellationToken,
) -> Result<DiscoveryResult> {
    discover_files_with_limits_policy_and_filter(
        root,
        limits,
        DiscoveryPolicy::default(),
        cancellation,
        |_| true,
    )
}

pub(crate) fn discover_files_with_limits_policy_and_filter(
    root: &Path,
    limits: DiscoveryLimits,
    policy: DiscoveryPolicy,
    cancellation: &CancellationToken,
    include: impl Fn(&Path) -> bool,
) -> Result<DiscoveryResult> {
    discover_files_with_limits_policy_filter_and_progress(
        root,
        limits,
        policy,
        cancellation,
        include,
        |_| {},
    )
}

const DISCOVERY_PROGRESS_INTERVAL_ENTRIES: u64 = 256;

pub(crate) fn discover_files_with_limits_policy_filter_and_progress(
    root: &Path,
    limits: DiscoveryLimits,
    policy: DiscoveryPolicy,
    cancellation: &CancellationToken,
    include: impl Fn(&Path) -> bool,
    mut observe: impl FnMut(DiscoveryStats),
) -> Result<DiscoveryResult> {
    limits.validate()?;
    let mut files = Vec::new();
    let mut stats = DiscoveryStats::default();
    let mut builder = WalkBuilder::new(root);
    builder
        .hidden(false)
        .follow_links(false)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .parents(true)
        .add_custom_ignore_filename(LEANTOKEN_IGNORE_FILE);
    if !policy.includes_generated() {
        let filter_root = root.to_path_buf();
        builder.filter_entry(move |entry| {
            let Ok(relative) = entry.path().strip_prefix(&filter_root) else {
                return false;
            };
            let Ok(relative_path) = checked_slash_path(relative) else {
                return true;
            };
            let is_directory = entry.file_type().is_some_and(|kind| kind.is_dir());
            policy.includes_path(&relative_path, is_directory)
        });
    }
    let walker = builder.build();

    for entry in walker {
        if cancellation.is_cancelled() {
            return Err(Error::Cancelled);
        }
        increment_limit(
            &mut stats.walk_entries,
            limits.max_walk_entries,
            IndexLimitKind::WalkEntries,
        )?;
        if stats.walk_entries % DISCOVERY_PROGRESS_INTERVAL_ENTRIES == 0 {
            observe(stats);
        }
        let entry = entry.map_err(Error::RepositoryTraversal)?;
        stats.max_depth = stats.max_depth.max(entry.depth());
        enforce_limit(
            IndexLimitKind::Depth,
            u64::try_from(entry.depth()).unwrap_or(u64::MAX),
            u64::try_from(limits.max_depth).unwrap_or(u64::MAX),
        )?;
        let Some(file_type) = entry.file_type() else {
            continue;
        };
        if !file_type.is_file() {
            continue;
        }
        let metadata = entry_metadata(&entry)?;
        if metadata.len() > limits.max_file_bytes {
            continue;
        }
        if !include(entry.path()) {
            continue;
        }
        let relative = entry
            .path()
            .strip_prefix(root)
            .map_err(|_| Error::PathOutsideRoot(entry.path().to_path_buf()))?;
        let relative_path = checked_slash_path(relative)?;
        if relative_path.is_empty() || is_git_metadata_path(&relative_path) {
            continue;
        }
        increment_limit(&mut stats.files, limits.max_files, IndexLimitKind::Files)?;
        stats.total_source_bytes = stats.total_source_bytes.saturating_add(metadata.len());
        enforce_limit(
            IndexLimitKind::TotalSourceBytes,
            stats.total_source_bytes,
            limits.max_total_source_bytes,
        )?;
        let modified_ns = metadata
            .modified()
            .ok()
            .and_then(|value| value.duration_since(UNIX_EPOCH).ok())
            .map(|duration| duration.as_nanos());
        files.push(DiscoveredFile {
            absolute_path: entry.into_path(),
            relative_path,
            size_bytes: metadata.len(),
            modified_ns,
        });
    }
    observe(stats);
    files.sort_unstable_by(|left, right| left.relative_path.cmp(&right.relative_path));
    Ok(DiscoveryResult { files, stats })
}

fn entry_metadata(entry: &ignore::DirEntry) -> Result<std::fs::Metadata> {
    entry.metadata().map_err(Error::RepositoryTraversal)
}

fn is_generated_path(relative_path: &str, path_is_directory: bool) -> bool {
    let components = relative_path
        .split('/')
        .filter(|component| !component.is_empty())
        .collect::<Vec<_>>();
    for (index, component) in components.iter().enumerate() {
        let matched = GENERATED_DIRECTORY_NAMES
            .iter()
            .any(|candidate| component_eq(component, candidate));
        if matched && (index + 1 < components.len() || path_is_directory) {
            return true;
        }
        for generated_path in GENERATED_DIRECTORY_PATHS {
            let end = index.saturating_add(generated_path.len());
            if end <= components.len()
                && components[index..end]
                    .iter()
                    .zip(*generated_path)
                    .all(|(actual, expected)| component_eq(actual, expected))
                && (end < components.len() || path_is_directory)
            {
                return true;
            }
        }
    }
    false
}

fn component_eq(actual: &str, expected: &str) -> bool {
    if cfg!(windows) {
        actual.eq_ignore_ascii_case(expected)
    } else {
        actual == expected
    }
}

fn increment_limit(current: &mut u64, limit: u64, kind: IndexLimitKind) -> Result<()> {
    *current = current.saturating_add(1);
    enforce_limit(kind, *current, limit)
}

pub(crate) fn enforce_limit(kind: IndexLimitKind, observed: u64, limit: u64) -> Result<()> {
    if observed > limit {
        Err(Error::IndexLimitExceeded {
            kind,
            observed,
            limit,
        })
    } else {
        Ok(())
    }
}

fn is_git_metadata_path(path: &str) -> bool {
    path.split('/').any(|component| component == ".git")
}
