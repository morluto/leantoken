use std::{
    collections::{BTreeMap, BTreeSet, HashSet},
    io::{BufRead, Write},
    path::{Component, Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, UNIX_EPOCH},
};

use command_group::CommandGroup;
use ignore::WalkBuilder;
use tokio_util::sync::CancellationToken;
use wait_timeout::ChildExt;

use crate::config::DiscoveryLimits;
use crate::error::IndexLimitKind;
use crate::{Error, Result};

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

pub fn resolve_existing(root: &Path, requested: &str) -> Result<PathBuf> {
    let relative = validate_relative(requested)?;
    let canonical = root.join(relative).canonicalize()?;
    if !canonical.starts_with(root) {
        return Err(Error::PathOutsideRoot(canonical));
    }
    Ok(canonical)
}

pub fn validate_relative(requested: &str) -> Result<PathBuf> {
    Ok(PathBuf::from(normalize_relative(requested)?))
}

/// Validate and normalize a repository-relative request path.
///
/// Repository keys always use forward slashes, independent of the host
/// platform. This helper therefore recognizes both separator styles before
/// applying the relative-path contract.
pub fn normalize_relative(requested: &str) -> Result<String> {
    if requested.is_empty() || requested.contains('\0') {
        return Err(Error::InvalidInput {
            field: "path",
            reason: "must be a non-empty relative path",
        });
    }
    // `Path` only recognizes prefixes for the host platform. Reject common
    // Windows absolute forms explicitly so a request has the same contract on
    // Linux, macOS, and Windows.
    let bytes = requested.as_bytes();
    let has_windows_drive = bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':';
    let has_windows_root = requested.starts_with('\\');
    if has_windows_drive || has_windows_root {
        return Err(Error::PathOutsideRoot(PathBuf::from(requested)));
    }
    let normalized = requested.replace('\\', "/");
    if normalized.starts_with('/') {
        return Err(Error::PathOutsideRoot(PathBuf::from(requested)));
    }
    let path = Path::new(&normalized);
    if path.is_absolute() {
        return Err(Error::PathOutsideRoot(path.to_path_buf()));
    }
    let mut components = Vec::new();
    for component in normalized.split('/') {
        match component {
            "" | "." => {}
            ".." => return Err(Error::PathOutsideRoot(path.to_path_buf())),
            component => components.push(component),
        }
    }
    if components.is_empty() {
        return Ok(".".to_owned());
    }
    Ok(components.join("/"))
}

/// Return paths reported by `git status` as working-tree changes.
///
/// The result is capped at `max` entries to keep the call bounded. If the
/// root is not a Git repository, `git` is unavailable, or `git status` fails,
/// an empty set is returned so callers can safely proceed without a diff
/// signal.
pub fn git_changed_paths(root: &Path, max: usize) -> Result<HashSet<String>> {
    git_changed_paths_with(root, max, Path::new("git"), Duration::from_millis(500))
}

/// Bounded working-tree paths plus whether `git status` completed successfully.
pub(crate) struct GitWorkingTreeStatus {
    pub(crate) changed_paths: HashSet<String>,
    pub(crate) available: bool,
}

/// Observe bounded working-tree state without changing the public empty-set fallback.
pub(crate) fn git_working_tree_status(root: &Path, max: usize) -> GitWorkingTreeStatus {
    git_working_tree_status_with(root, max, Path::new("git"), Duration::from_millis(500))
}

fn git_changed_paths_with(
    root: &Path,
    max: usize,
    program: &Path,
    timeout: Duration,
) -> Result<HashSet<String>> {
    Ok(git_working_tree_status_with(root, max, program, timeout).changed_paths)
}

fn git_working_tree_status_with(
    root: &Path,
    max: usize,
    program: &Path,
    timeout: Duration,
) -> GitWorkingTreeStatus {
    if max == 0 {
        return GitWorkingTreeStatus {
            changed_paths: HashSet::new(),
            available: true,
        };
    }
    let prefix = git_worktree_prefix(root);
    let args = [
        "-c",
        "core.fsmonitor=false",
        "status",
        "--porcelain=v1",
        "-z",
        "--untracked-files=all",
        "--no-renames",
        "--",
        ".",
    ]
    .map(str::to_owned);
    let Ok(output) = run_git_capture(
        root,
        program,
        &args,
        GitCaptureOptions {
            timeout,
            field: "git status",
            timeout_reason: "git status timed out",
            failure_reason: "git status failed",
            max_output_bytes: bounded_git_output(max, GIT_PATH_OUTPUT_BYTES_PER_RESULT),
        },
    ) else {
        return GitWorkingTreeStatus {
            changed_paths: HashSet::new(),
            available: false,
        };
    };
    let (changed_paths, available) = parse_git_status_observation(output.as_slice(), max, &prefix);
    GitWorkingTreeStatus {
        changed_paths,
        available,
    }
}

fn bounded_git_output(max_results: usize, bytes_per_result: usize) -> usize {
    max_results
        .saturating_mul(bytes_per_result)
        .max(bytes_per_result)
        .min(MAX_GIT_DISCOVERY_OUTPUT_BYTES)
}

fn git_worktree_prefix(root: &Path) -> String {
    root.ancestors()
        .find(|ancestor| ancestor.join(".git").exists())
        .and_then(|worktree| root.strip_prefix(worktree).ok())
        .map(slash_path)
        .filter(|prefix| !prefix.is_empty())
        .map(|prefix| format!("{prefix}/"))
        .unwrap_or_default()
}

#[cfg(test)]
fn parse_git_status<R: BufRead>(mut reader: R, max: usize, prefix: &str) -> HashSet<String> {
    parse_git_status_observation(&mut reader, max, prefix).0
}

fn parse_git_status_observation<R: BufRead>(
    mut reader: R,
    max: usize,
    prefix: &str,
) -> (HashSet<String>, bool) {
    if max == 0 {
        return (HashSet::new(), true);
    }
    let mut changed = HashSet::new();
    let mut record = Vec::new();

    loop {
        record.clear();
        match reader.read_until(0, &mut record) {
            Ok(0) => break,
            Ok(_) => {}
            Err(_) => return (changed, false),
        }

        if record.last() == Some(&0) {
            record.pop();
        }
        if record.len() < 4 || record.get(2) != Some(&b' ') {
            continue;
        }

        let status = &record[..2];
        let path = String::from_utf8_lossy(&record[3..]).into_owned();

        // Ignore ignored files; keep modified, added, deleted, and untracked.
        if status == b"!!" {
            continue;
        }

        let Some(path) = path.strip_prefix(prefix) else {
            continue;
        };
        changed.insert(slash_path(Path::new(path)));
        if changed.len() == max {
            break;
        }
    }
    (changed, true)
}

pub fn slash_path(path: &Path) -> String {
    path.components()
        .filter_map(|component| match component {
            Component::Normal(value) => Some(value.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("/")
}

pub(crate) fn checked_slash_path(path: &Path) -> Result<String> {
    path.components()
        .filter_map(|component| match component {
            Component::Normal(value) => Some(
                value
                    .to_str()
                    .map(str::to_owned)
                    .ok_or_else(|| Error::UnsupportedPathEncoding(path.to_path_buf())),
            ),
            _ => None,
        })
        .collect::<Result<Vec<_>>>()
        .map(|components| components.join("/"))
}

/// Resolved diff scope: base/head short SHAs and the changed paths between them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitDiffResult {
    /// Short (12-char) SHA of the resolved base revision.
    pub base_revision: String,
    /// Short (12-char) SHA of the resolved head revision.
    pub head_revision: String,
    /// Repository-relative changed paths in the resolved diff scope.
    pub changed_paths: Vec<String>,
}

/// One target-side line range parsed from a zero-context Git diff.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitHunkRange {
    /// Repository-relative target path.
    pub path: String,
    /// First target line touched by the hunk, or the line after an empty hunk boundary.
    pub start_line: usize,
    /// Last target line touched by the hunk, inclusive.
    ///
    /// An empty target-side hunk has `end_line < start_line`.
    pub end_line: usize,
}

/// One immutable UTF-8 file blob loaded from a resolved Git revision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GitBlob {
    /// Resolved 12-character revision.
    pub revision: String,
    /// File contents at the revision.
    pub content: String,
}

/// Bounded UTF-8 blobs loaded together from one immutable Git revision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GitBlobBatch {
    /// Resolved 12-character revision.
    pub revision: String,
    /// Repository-relative paths mapped to UTF-8 contents.
    pub blobs: BTreeMap<String, String>,
    /// Requested paths that do not exist at the revision.
    pub missing_paths: Vec<String>,
    /// Requested paths larger than the per-file byte limit.
    pub oversized_paths: Vec<String>,
    /// Requested paths omitted after the aggregate byte limit was reached.
    pub total_limit_paths: Vec<String>,
    /// Requested paths whose blobs are not valid UTF-8.
    pub invalid_utf8_paths: Vec<String>,
    /// Requested paths that resolve to non-blob Git entries.
    pub unsupported_paths: Vec<String>,
}

/// One commit from Git's tracked line history.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GitLineCommit {
    pub commit: String,
    pub authored_at: String,
    pub subject: String,
}

/// Shared metadata for one exact immutable commit endpoint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GitCommitMetadata {
    pub revision: String,
    pub authored_at: String,
    pub subject: String,
}

/// Load one bounded UTF-8 repository file from an immutable Git revision.
pub(crate) fn git_blob_at_revision(
    root: &Path,
    revision: &str,
    path: &str,
    max_bytes: usize,
) -> Result<GitBlob> {
    let timeout = Duration::from_millis(1_000);
    let program = Path::new("git");
    let revision = resolve_revision_sha_for_field(root, program, revision, timeout, "revision")?;
    let repository_path = format!("{}{path}", git_worktree_prefix(root));
    let object = format!("{revision}:{repository_path}");
    let size_output = run_git_capture(
        root,
        program,
        &["cat-file".into(), "-s".into(), object.clone()],
        GitCaptureOptions {
            timeout,
            field: "path",
            timeout_reason: "git cat-file timed out",
            failure_reason: "file does not exist at revision",
            max_output_bytes: 4 * 1024,
        },
    )?;
    let size = std::str::from_utf8(&size_output)
        .ok()
        .and_then(|value| value.trim().parse::<usize>().ok())
        .ok_or_else(|| Error::InternalFailure("invalid git blob size".into()))?;
    if size > max_bytes {
        return Err(Error::RequestLimitExceeded {
            field: "historical file bytes",
            requested: size,
            limit: max_bytes,
        });
    }
    let content = run_git_capture(
        root,
        program,
        &["cat-file".into(), "blob".into(), object],
        GitCaptureOptions {
            timeout,
            field: "path",
            timeout_reason: "git cat-file timed out",
            failure_reason: "file does not exist at revision",
            max_output_bytes: max_bytes,
        },
    )?;
    let content = String::from_utf8(content).map_err(|_| Error::InvalidInput {
        field: "path",
        reason: "historical file is not valid UTF-8",
    })?;
    Ok(GitBlob { revision, content })
}

/// Load a bounded set of UTF-8 repository files from one immutable revision.
///
/// A single tree query resolves path-to-object identities, followed by one
/// `cat-file --batch` call for the selected unique blobs.
pub(crate) fn git_blobs_at_revision(
    root: &Path,
    revision: &str,
    paths: &[String],
    max_file_bytes: usize,
    max_total_bytes: usize,
) -> Result<GitBlobBatch> {
    let timeout = Duration::from_millis(2_000);
    let program = Path::new("git");
    let revision = resolve_revision_sha_for_field(root, program, revision, timeout, "revision")?;
    git_blobs_at_resolved_revision(root, &revision, paths, max_file_bytes, max_total_bytes)
}

/// Load bounded UTF-8 blobs after the caller has resolved the immutable revision.
///
/// This executes one `ls-tree` subprocess and at most one `cat-file --batch`
/// subprocess, independent of the number of requested paths.
pub(crate) fn git_blobs_at_resolved_revision(
    root: &Path,
    revision: &str,
    paths: &[String],
    max_file_bytes: usize,
    max_total_bytes: usize,
) -> Result<GitBlobBatch> {
    let timeout = Duration::from_millis(2_000);
    let program = Path::new("git");
    let revision = revision.to_owned();
    let prefix = git_worktree_prefix(root);
    let requested = paths.iter().cloned().collect::<BTreeSet<_>>();
    if requested.is_empty() {
        return Ok(GitBlobBatch {
            revision,
            blobs: BTreeMap::new(),
            missing_paths: Vec::new(),
            oversized_paths: Vec::new(),
            total_limit_paths: Vec::new(),
            invalid_utf8_paths: Vec::new(),
            unsupported_paths: Vec::new(),
        });
    }

    let mut args = vec![
        "ls-tree".into(),
        "-r".into(),
        "-z".into(),
        "-l".into(),
        "--full-tree".into(),
        revision.clone(),
        "--".into(),
    ];
    args.extend(requested.iter().map(|path| format!("{prefix}{path}")));
    let tree_output_limit = requested.iter().fold(1_024usize, |limit, path| {
        limit.saturating_add(path.len()).saturating_add(160)
    });
    let tree_output = run_git_capture(
        root,
        program,
        &args,
        GitCaptureOptions {
            timeout,
            field: "path",
            timeout_reason: "git ls-tree timed out",
            failure_reason: "failed to inspect files at revision",
            max_output_bytes: tree_output_limit,
        },
    )?;

    let mut objects = BTreeMap::<String, (String, usize)>::new();
    let mut present_paths = BTreeSet::new();
    let mut unsupported_paths = Vec::new();
    for record in tree_output
        .split(|byte| *byte == 0)
        .filter(|record| !record.is_empty())
    {
        let Some(tab) = record.iter().position(|byte| *byte == b'\t') else {
            return Err(Error::InternalFailure("invalid git ls-tree record".into()));
        };
        let metadata = std::str::from_utf8(&record[..tab])
            .map_err(|_| Error::InternalFailure("invalid git ls-tree metadata".into()))?;
        let mut fields = metadata.split_whitespace();
        let _mode = fields.next();
        let object_type = fields.next();
        let object_id = fields.next();
        let size = fields.next().and_then(|value| value.parse::<usize>().ok());
        let path = std::str::from_utf8(&record[tab + 1..])
            .map_err(|_| Error::InternalFailure("invalid git ls-tree path".into()))?;
        let Some(path) = path.strip_prefix(&prefix) else {
            continue;
        };
        if requested.contains(path) {
            present_paths.insert(path.to_owned());
        }
        if object_type == Some("blob")
            && let (Some(object_id), Some(size)) = (object_id, size)
            && requested.contains(path)
        {
            objects.insert(path.to_owned(), (object_id.to_owned(), size));
        } else if requested.contains(path) {
            unsupported_paths.push(path.to_owned());
        }
    }

    let mut missing_paths = Vec::new();
    let mut oversized_paths = Vec::new();
    let mut total_limit_paths = Vec::new();
    let mut selected = Vec::new();
    let mut total_bytes = 0usize;
    for path in &requested {
        let Some((object_id, size)) = objects.get(path) else {
            if !present_paths.contains(path) {
                missing_paths.push(path.clone());
            }
            continue;
        };
        if *size > max_file_bytes {
            oversized_paths.push(path.clone());
            continue;
        }
        if total_bytes.saturating_add(*size) > max_total_bytes {
            total_limit_paths.push(path.clone());
            continue;
        }
        total_bytes += *size;
        selected.push((path.clone(), object_id.clone(), *size));
    }

    let unique_objects = selected
        .iter()
        .map(|(_, object_id, size)| (object_id.clone(), *size))
        .collect::<BTreeMap<_, _>>();
    let mut input = Vec::new();
    for object_id in unique_objects.keys() {
        input.extend_from_slice(object_id.as_bytes());
        input.push(b'\n');
    }
    let batch_output_limit = total_bytes
        .saturating_add(unique_objects.len().saturating_mul(96))
        .saturating_add(1_024);
    let batch_output = if input.is_empty() {
        Vec::new()
    } else {
        run_git_capture_with_input(
            root,
            program,
            &["cat-file".into(), "--batch".into()],
            &input,
            GitCaptureOptions {
                timeout,
                field: "path",
                timeout_reason: "git cat-file batch timed out",
                failure_reason: "failed to load files at revision",
                max_output_bytes: batch_output_limit,
            },
        )?
    };
    let mut contents = BTreeMap::<String, Vec<u8>>::new();
    let mut cursor = 0usize;
    for (expected_object, expected_size) in &unique_objects {
        let header_end = batch_output[cursor..]
            .iter()
            .position(|byte| *byte == b'\n')
            .map(|offset| cursor + offset)
            .ok_or_else(|| Error::InternalFailure("invalid git cat-file header".into()))?;
        let header = std::str::from_utf8(&batch_output[cursor..header_end])
            .map_err(|_| Error::InternalFailure("invalid git cat-file header".into()))?;
        let mut fields = header.split_whitespace();
        let object_id = fields.next();
        let object_type = fields.next();
        let size = fields.next().and_then(|value| value.parse::<usize>().ok());
        if object_id != Some(expected_object.as_str())
            || object_type != Some("blob")
            || size != Some(*expected_size)
        {
            return Err(Error::InternalFailure(
                "unexpected git cat-file batch response".into(),
            ));
        }
        let content_start = header_end + 1;
        let content_end = content_start
            .checked_add(*expected_size)
            .ok_or_else(|| Error::InternalFailure("git blob size overflow".into()))?;
        if batch_output.get(content_end) != Some(&b'\n') {
            return Err(Error::InternalFailure(
                "truncated git cat-file batch response".into(),
            ));
        }
        contents.insert(
            expected_object.clone(),
            batch_output[content_start..content_end].to_vec(),
        );
        cursor = content_end + 1;
    }
    if cursor != batch_output.len() {
        return Err(Error::InternalFailure(
            "unexpected trailing git cat-file output".into(),
        ));
    }

    let mut blobs = BTreeMap::new();
    let mut invalid_utf8_paths = Vec::new();
    for (path, object_id, _) in selected {
        let content = contents
            .get(&object_id)
            .ok_or_else(|| Error::InternalFailure("missing batched git blob".into()))?
            .clone();
        match String::from_utf8(content) {
            Ok(content) => {
                blobs.insert(path, content);
            }
            Err(_) => invalid_utf8_paths.push(path),
        }
    }
    Ok(GitBlobBatch {
        revision,
        blobs,
        missing_paths,
        oversized_paths,
        total_limit_paths,
        invalid_utf8_paths,
        unsupported_paths,
    })
}

/// Read metadata for resolved immutable endpoints in one bounded Git subprocess.
pub(crate) fn git_commit_metadata(
    root: &Path,
    revisions: &[String],
) -> Result<BTreeMap<String, GitCommitMetadata>> {
    let requested = revisions.iter().cloned().collect::<BTreeSet<_>>();
    if requested.is_empty() {
        return Ok(BTreeMap::new());
    }
    let mut args = vec![
        "show".into(),
        "-s".into(),
        "--no-walk=unsorted".into(),
        "--format=%H%x1f%aI%x1f%s%x00".into(),
        "--end-of-options".into(),
    ];
    args.extend(requested.iter().cloned());
    let output = run_git_capture(
        root,
        Path::new("git"),
        &args,
        GitCaptureOptions {
            timeout: Duration::from_millis(1_000),
            field: "revision",
            timeout_reason: "git commit metadata timed out",
            failure_reason: "could not read commit metadata",
            max_output_bytes: requested.len().saturating_mul(1_024).max(2_048),
        },
    )?;
    let mut metadata = BTreeMap::new();
    for record in output.split(|byte| *byte == 0) {
        let record = record.strip_prefix(b"\n").unwrap_or(record);
        let record = record.strip_suffix(b"\n").unwrap_or(record);
        if record.is_empty() {
            continue;
        }
        let mut fields = record.splitn(3, |byte| *byte == 0x1f);
        let revision = fields.next();
        let authored_at = fields.next();
        let subject = fields.next();
        let (Some(revision), Some(authored_at), Some(subject)) = (revision, authored_at, subject)
        else {
            return Err(Error::InternalFailure(
                "invalid git commit metadata record".into(),
            ));
        };
        let revision = std::str::from_utf8(revision)
            .map_err(|_| Error::InternalFailure("invalid git commit identity".into()))?;
        if revision.len() < 12 || !revision.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(Error::InternalFailure("invalid git commit identity".into()));
        }
        let short_revision = revision[..12].to_ascii_lowercase();
        metadata.insert(
            short_revision.clone(),
            GitCommitMetadata {
                revision: short_revision,
                authored_at: String::from_utf8_lossy(authored_at).into_owned(),
                subject: String::from_utf8_lossy(subject).into_owned(),
            },
        );
    }
    for revision in requested {
        if !metadata.contains_key(&revision) {
            return Err(Error::InternalFailure(format!(
                "missing commit metadata for resolved revision {revision}"
            )));
        }
    }
    Ok(metadata)
}

/// Return bounded commit metadata for one tracked historical line range.
pub(crate) fn git_line_history(
    root: &Path,
    revision: &str,
    path: &str,
    start_line: usize,
    end_line: usize,
    max: usize,
) -> Result<Vec<GitLineCommit>> {
    let timeout = Duration::from_millis(2_000);
    let program = Path::new("git");
    let revision = resolve_revision_sha_for_field(root, program, revision, timeout, "revision")?;
    let repository_path = format!("{}{path}", git_worktree_prefix(root));
    let line_range = format!("-L{start_line},{end_line}:{repository_path}");
    let output = run_git_capture(
        root,
        program,
        &[
            "log".into(),
            "--no-patch".into(),
            "--format=%H%x1f%aI%x1f%s%x00".into(),
            format!("--max-count={max}"),
            line_range,
            revision,
        ],
        GitCaptureOptions {
            timeout,
            field: "symbol",
            timeout_reason: "git line history timed out",
            failure_reason: "could not trace symbol line history",
            max_output_bytes: max.saturating_mul(1024).max(4 * 1024),
        },
    )?;
    let mut commits = Vec::new();
    for record in output.split(|byte| *byte == 0) {
        let record = record.strip_prefix(b"\n").unwrap_or(record);
        let record = record.strip_suffix(b"\n").unwrap_or(record);
        if record.is_empty() {
            continue;
        }
        let mut fields = record.splitn(3, |byte| *byte == 0x1f);
        let commit = fields.next();
        let authored_at = fields.next();
        let subject = fields.next();
        let (Some(commit), Some(authored_at), Some(subject)) = (commit, authored_at, subject)
        else {
            return Err(Error::InternalFailure(
                "invalid git line history record".into(),
            ));
        };
        commits.push(GitLineCommit {
            commit: String::from_utf8_lossy(commit).into_owned(),
            authored_at: String::from_utf8_lossy(authored_at).into_owned(),
            subject: String::from_utf8_lossy(subject).into_owned(),
        });
    }
    Ok(commits)
}

struct GitCaptureOptions {
    timeout: Duration,
    field: &'static str,
    timeout_reason: &'static str,
    failure_reason: &'static str,
    max_output_bytes: usize,
}

fn run_git_capture(
    root: &Path,
    program: &Path,
    args: &[String],
    options: GitCaptureOptions,
) -> Result<Vec<u8>> {
    run_git_capture_bounded(root, program, args, None, options)
}

fn run_git_capture_with_input(
    root: &Path,
    program: &Path,
    args: &[String],
    input: &[u8],
    options: GitCaptureOptions,
) -> Result<Vec<u8>> {
    run_git_capture_bounded(root, program, args, Some(input), options)
}

fn run_git_capture_bounded(
    root: &Path,
    program: &Path,
    args: &[String],
    input: Option<&[u8]>,
    options: GitCaptureOptions,
) -> Result<Vec<u8>> {
    use std::io::Read;
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };
    use std::thread;
    use std::time::Instant;

    let mut command = Command::new(program);
    command
        .args(args)
        .current_dir(root)
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut child = command.group_spawn().map_err(|_| Error::InvalidInput {
        field: options.field,
        reason: "git is unavailable",
    })?;

    let output_limit_exceeded = Arc::new(AtomicBool::new(false));
    let reader_exceeded = Arc::clone(&output_limit_exceeded);
    let (release_reader, reader_release) = std::sync::mpsc::channel();
    let max_output_bytes = options.max_output_bytes;
    let mut stdout = child
        .inner()
        .stdout
        .take()
        .ok_or_else(|| Error::InternalFailure("git stdout unavailable".into()))?;
    let reader = thread::spawn(move || -> std::io::Result<Vec<u8>> {
        let mut output = Vec::with_capacity(max_output_bytes.min(64 * 1024));
        let mut chunk = [0u8; 8 * 1024];
        loop {
            let read = stdout.read(&mut chunk)?;
            if read == 0 {
                return Ok(output);
            }
            if output.len().saturating_add(read) > max_output_bytes {
                reader_exceeded.store(true, Ordering::Release);
                // Keep the pipe open without draining it until the parent has
                // killed the producer. Otherwise a fast producer can observe
                // SIGPIPE, let a shell wrapper continue to its next command,
                // and perform work after the limit was crossed.
                let _ = reader_release.recv();
                return Ok(output);
            }
            output.extend_from_slice(&chunk[..read]);
        }
    });
    let writer = input.map(|input| {
        let input = input.to_vec();
        let mut stdin = child.inner().stdin.take();
        thread::spawn(move || -> std::io::Result<()> {
            stdin
                .as_mut()
                .ok_or_else(|| std::io::Error::other("git stdin unavailable"))?
                .write_all(&input)
        })
    });

    enum ChildOutcome {
        Exited(std::process::ExitStatus),
        OutputLimit,
        Timeout,
        WaitError(std::io::Error),
    }

    let deadline = Instant::now() + options.timeout;
    let outcome = loop {
        if output_limit_exceeded.load(Ordering::Acquire) {
            break ChildOutcome::OutputLimit;
        }
        match child.try_wait() {
            Ok(Some(status)) => break ChildOutcome::Exited(status),
            Ok(None) => {}
            Err(error) => break ChildOutcome::WaitError(error),
        }
        if Instant::now() >= deadline {
            break ChildOutcome::Timeout;
        }
        thread::sleep(Duration::from_millis(5));
    };

    // Terminate the whole process group even after the direct child exits:
    // external helpers can otherwise retain stdout and block the reader join.
    let _ = child.kill();
    let _ = child.wait();
    // If the reader crossed the bound just as the child exited, it may be
    // waiting for this signal even though the polling loop observed normal
    // exit first.
    let _ = release_reader.send(());

    let status = match outcome {
        ChildOutcome::Exited(status) => status,
        ChildOutcome::OutputLimit => {
            let _ = reader.join();
            if let Some(writer) = writer {
                let _ = writer.join();
            }
            return Err(Error::RequestLimitExceeded {
                field: "git output bytes",
                requested: options.max_output_bytes.saturating_add(1),
                limit: options.max_output_bytes,
            });
        }
        ChildOutcome::Timeout => {
            let _ = reader.join();
            if let Some(writer) = writer {
                let _ = writer.join();
            }
            return Err(Error::InvalidInput {
                field: options.field,
                reason: options.timeout_reason,
            });
        }
        ChildOutcome::WaitError(error) => {
            let _ = reader.join();
            if let Some(writer) = writer {
                let _ = writer.join();
            }
            return Err(error.into());
        }
    };
    if let Some(writer) = writer {
        writer
            .join()
            .map_err(|_| Error::InternalFailure("git stdin task panicked".into()))??;
    }
    let output = reader
        .join()
        .map_err(|_| Error::InternalFailure("git stdout task panicked".into()))??;
    if output_limit_exceeded.load(Ordering::Acquire) {
        return Err(Error::RequestLimitExceeded {
            field: "git output bytes",
            requested: options.max_output_bytes.saturating_add(1),
            limit: options.max_output_bytes,
        });
    }
    if !status.success() {
        return Err(Error::InvalidInput {
            field: options.field,
            reason: options.failure_reason,
        });
    }
    Ok(output)
}

/// Resolve changed paths between a base revision and the working tree.
///
/// Runs `git diff --name-only -z --no-renames <base> -- .` to capture both
/// committed and uncommitted changes relative to the base. The call is
/// bounded by `max` paths and a timeout. If git is unavailable or the
/// revision cannot be resolved, an error is returned so the caller can
/// surface an actionable message.
pub fn git_diff_paths(root: &Path, base_revision: &str, max: usize) -> Result<GitDiffResult> {
    git_diff_paths_with(
        root,
        base_revision,
        max,
        Path::new("git"),
        Duration::from_millis(1_000),
    )
}

/// Resolve changed paths between two immutable Git revisions.
pub fn git_diff_paths_between(
    root: &Path,
    base_revision: &str,
    head_revision: &str,
    max: usize,
) -> Result<GitDiffResult> {
    let timeout = Duration::from_millis(1_000);
    let program = Path::new("git");
    let base_sha =
        resolve_revision_sha_for_field(root, program, base_revision, timeout, "base revision")?;
    let head_sha =
        resolve_revision_sha_for_field(root, program, head_revision, timeout, "head revision")?;
    let changed_paths = diff_name_only(
        root,
        program,
        &base_sha,
        Some(&head_sha),
        max,
        timeout,
        &git_worktree_prefix(root),
    )?;
    Ok(GitDiffResult {
        base_revision: base_sha,
        head_revision: head_sha,
        changed_paths,
    })
}

/// Resolve diff endpoint identities without enumerating changed paths.
pub(crate) fn git_diff_identity(
    root: &Path,
    base_revision: &str,
    head_revision: Option<&str>,
) -> Result<GitDiffResult> {
    let timeout = Duration::from_millis(1_000);
    let program = Path::new("git");
    let base_sha =
        resolve_revision_sha_for_field(root, program, base_revision, timeout, "base revision")?;
    let head_sha = resolve_revision_sha_for_field(
        root,
        program,
        head_revision.unwrap_or("HEAD"),
        timeout,
        "head revision",
    )?;
    Ok(GitDiffResult {
        base_revision: base_sha,
        head_revision: head_sha,
        changed_paths: Vec::new(),
    })
}

/// Resolve the current Git `HEAD` to the same bounded short SHA used by diff receipts.
pub(crate) fn git_head_revision(root: &Path) -> Result<String> {
    resolve_revision_sha_for_field(
        root,
        Path::new("git"),
        "HEAD",
        Duration::from_millis(1_000),
        "head revision",
    )
}

/// Parse bounded target-side hunk ranges between a base revision and the working tree.
pub fn git_diff_hunks(root: &Path, base_revision: &str, max: usize) -> Result<Vec<GitHunkRange>> {
    git_diff_hunks_with_head(root, base_revision, None, &[], max)
}

/// Parse bounded target-side hunk ranges between two immutable Git revisions.
pub fn git_diff_hunks_between(
    root: &Path,
    base_revision: &str,
    head_revision: &str,
    max: usize,
) -> Result<Vec<GitHunkRange>> {
    git_diff_hunks_with_head(root, base_revision, Some(head_revision), &[], max)
}

/// Parse bounded target-side hunk ranges only for explicit repository paths.
pub(crate) fn git_diff_hunks_scoped(
    root: &Path,
    base_revision: &str,
    head_revision: Option<&str>,
    paths: &[String],
    max: usize,
) -> Result<Vec<GitHunkRange>> {
    git_diff_hunks_with_head(root, base_revision, head_revision, paths, max)
}

fn git_diff_hunks_with_head(
    root: &Path,
    base_revision: &str,
    head_revision: Option<&str>,
    paths: &[String],
    max: usize,
) -> Result<Vec<GitHunkRange>> {
    if max == 0 {
        return Ok(Vec::new());
    }
    let timeout = Duration::from_millis(1_000);
    let program = Path::new("git");
    let base_sha =
        resolve_revision_sha_for_field(root, program, base_revision, timeout, "base revision")?;
    let head_sha = head_revision
        .map(|revision| {
            resolve_revision_sha_for_field(root, program, revision, timeout, "head revision")
        })
        .transpose()?;
    let prefix = git_worktree_prefix(root);
    let mut args = vec![
        "-c".to_owned(),
        "core.fsmonitor=false".to_owned(),
        "diff".to_owned(),
        "--unified=0".to_owned(),
        "--no-renames".to_owned(),
        base_sha,
    ];
    args.extend(head_sha);
    args.push("--".to_owned());
    if paths.is_empty() {
        args.push(".".to_owned());
    } else {
        args.extend(paths.iter().cloned());
    }
    let output = run_git_capture(
        root,
        program,
        &args,
        GitCaptureOptions {
            timeout,
            field: "base revision",
            timeout_reason: "git diff timed out",
            failure_reason: "could not diff revision",
            max_output_bytes: bounded_git_output(max, GIT_HUNK_OUTPUT_BYTES_PER_RESULT),
        },
    )?;
    parse_git_diff_hunks(output.as_slice(), max, &prefix)
}

fn parse_git_diff_hunks<R: BufRead>(
    mut reader: R,
    max: usize,
    prefix: &str,
) -> Result<Vec<GitHunkRange>> {
    let mut ranges = Vec::new();
    let mut target_path = None;
    let mut line = String::new();
    while ranges.len() < max {
        line.clear();
        if reader.read_line(&mut line)? == 0 {
            break;
        }
        if let Some(path) = line.strip_prefix("+++ ") {
            target_path = path
                .strip_prefix("b/")
                .map(|path| path.trim_end_matches(['\r', '\n']))
                .and_then(|path| path.strip_prefix(prefix))
                .map(|path| slash_path(Path::new(path)));
            continue;
        }
        let Some(path) = target_path.as_ref() else {
            continue;
        };
        let Some(header) = line.strip_prefix("@@ ") else {
            continue;
        };
        let Some(target) = header.split_whitespace().find(|part| part.starts_with('+')) else {
            continue;
        };
        let target = &target[1..];
        let (start, length) = target
            .split_once(',')
            .map_or((target, "1"), |(start, length)| (start, length));
        let raw_start = start
            .parse::<usize>()
            .map_err(|error| Error::InternalFailure(error.to_string()))?;
        let length = length
            .parse::<usize>()
            .map_err(|error| Error::InternalFailure(error.to_string()))?;
        let (start_line, end_line) = if length == 0 {
            (
                raw_start
                    .checked_add(1)
                    .ok_or_else(|| Error::InternalFailure("git diff hunk range overflow".into()))?,
                raw_start,
            )
        } else {
            let start_line = raw_start.max(1);
            let end_line = start_line
                .checked_add(length - 1)
                .ok_or_else(|| Error::InternalFailure("git diff hunk range overflow".into()))?;
            (start_line, end_line)
        };
        ranges.push(GitHunkRange {
            path: path.clone(),
            start_line,
            end_line,
        });
    }
    Ok(ranges)
}

fn git_diff_paths_with(
    root: &Path,
    base_revision: &str,
    max: usize,
    program: &Path,
    timeout: Duration,
) -> Result<GitDiffResult> {
    if base_revision.trim().is_empty() {
        return Err(Error::InvalidInput {
            field: "base revision",
            reason: "must not be empty",
        });
    }
    if max == 0 {
        return Ok(GitDiffResult {
            base_revision: String::new(),
            head_revision: String::new(),
            changed_paths: Vec::new(),
        });
    }
    let prefix = git_worktree_prefix(root);
    let base_sha = resolve_revision_sha(root, program, base_revision, timeout)?;
    let head_sha = resolve_revision_sha(root, program, "HEAD", timeout)?;
    let changed = diff_name_only(root, program, &base_sha, None, max, timeout, &prefix)?;
    Ok(GitDiffResult {
        base_revision: base_sha,
        head_revision: head_sha,
        changed_paths: changed,
    })
}

fn resolve_revision_sha(
    root: &Path,
    program: &Path,
    revision: &str,
    timeout: Duration,
) -> Result<String> {
    resolve_revision_sha_for_field(root, program, revision, timeout, "base revision")
}

fn resolve_revision_sha_for_field(
    root: &Path,
    program: &Path,
    revision: &str,
    timeout: Duration,
    field: &'static str,
) -> Result<String> {
    if revision.trim().is_empty() {
        return Err(Error::InvalidInput {
            field,
            reason: "must not be empty",
        });
    }
    let commit_revision = format!("{revision}^{{commit}}");
    let mut child = match Command::new(program)
        .args([
            "rev-parse",
            "--verify",
            "--short=12",
            "--end-of-options",
            &commit_revision,
        ])
        .current_dir(root)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(child) => child,
        Err(_) => {
            return Err(Error::InvalidInput {
                field,
                reason: "git is unavailable",
            });
        }
    };
    let status = match child.wait_timeout(timeout) {
        Ok(Some(status)) => status,
        Ok(None) | Err(_) => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(Error::InvalidInput {
                field,
                reason: "git rev-parse timed out",
            });
        }
    };
    if !status.success() {
        return Err(Error::InvalidInput {
            field,
            reason: "could not resolve revision",
        });
    }
    let output = child.stdout.take().map_or(Vec::new(), |mut s| {
        use std::io::Read;
        let mut buf = Vec::new();
        let _ = s.read_to_end(&mut buf);
        buf
    });
    let sha = String::from_utf8_lossy(&output).trim().to_owned();
    if sha.is_empty() {
        return Err(Error::InvalidInput {
            field: "base revision",
            reason: "resolved to an empty SHA",
        });
    }
    Ok(sha)
}

fn diff_name_only(
    root: &Path,
    program: &Path,
    base_sha: &str,
    head_sha: Option<&str>,
    max: usize,
    timeout: Duration,
    prefix: &str,
) -> Result<Vec<String>> {
    let mut args = vec![
        "-c".to_owned(),
        "core.fsmonitor=false".to_owned(),
        "diff".to_owned(),
        "--name-only".to_owned(),
        "-z".to_owned(),
        "--no-renames".to_owned(),
        base_sha.to_owned(),
    ];
    args.extend(head_sha.map(str::to_owned));
    args.extend(["--".to_owned(), ".".to_owned()]);
    let Ok(output) = run_git_capture(
        root,
        program,
        &args,
        GitCaptureOptions {
            timeout,
            field: "base revision",
            timeout_reason: "git diff timed out",
            failure_reason: "could not diff revision",
            max_output_bytes: bounded_git_output(max, GIT_PATH_OUTPUT_BYTES_PER_RESULT),
        },
    ) else {
        return Ok(Vec::new());
    };
    Ok(parse_diff_names(output.as_slice(), max, prefix))
}

fn parse_diff_names<R: BufRead>(mut reader: R, max: usize, prefix: &str) -> Vec<String> {
    if max == 0 {
        return Vec::new();
    }
    let mut changed = Vec::new();
    let mut record = Vec::new();
    loop {
        record.clear();
        match reader.read_until(0, &mut record) {
            Ok(0) => break,
            Ok(_) => {}
            Err(_) => break,
        }
        if record.last() == Some(&0) {
            record.pop();
        }
        if record.is_empty() {
            continue;
        }
        let path = String::from_utf8_lossy(&record);
        let Some(path) = path.strip_prefix(prefix) else {
            continue;
        };
        changed.push(slash_path(Path::new(path)));
        if changed.len() == max {
            break;
        }
    }
    changed
}

#[cfg(all(test, unix))]
mod tests {
    use std::fs;
    use std::io::Cursor;
    use std::os::unix::fs::PermissionsExt;
    use std::time::Instant;

    use super::*;

    #[test]
    fn discovery_reports_walker_errors_instead_of_returning_partial_results() {
        let directory = tempfile::tempdir().expect("directory");
        let missing = directory.path().join("missing");

        let error = discover_files(&missing, 1024).expect_err("missing root must fail");

        assert!(matches!(error, Error::RepositoryTraversal(_)));
    }

    #[test]
    fn discovery_reports_metadata_errors_instead_of_skipping_entries() {
        let directory = tempfile::tempdir().expect("directory");
        let path = directory.path().join("vanishing.rs");
        fs::write(&path, "fn vanishing() {}").expect("fixture");
        let entry = WalkBuilder::new(directory.path())
            .build()
            .filter_map(std::result::Result::ok)
            .find(|entry| entry.path() == path)
            .expect("file entry");
        fs::remove_file(&path).expect("remove fixture");

        let error = entry_metadata(&entry).expect_err("missing metadata must fail");

        assert!(matches!(error, Error::RepositoryTraversal(_)));
    }

    #[cfg(unix)]
    #[test]
    fn checked_slash_path_rejects_non_utf8_paths_without_lossy_aliases() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;

        for name in [b"\x80.rs".to_vec(), b"\x81.rs".to_vec()] {
            let path = PathBuf::from(OsString::from_vec(name));
            let error = checked_slash_path(&path).expect_err("non-UTF-8 path must be rejected");

            match error {
                Error::UnsupportedPathEncoding(rejected) => assert_eq!(rejected, path),
                other => panic!("unexpected error: {other}"),
            }
        }
    }

    #[test]
    fn git_status_parser_stops_after_collecting_max_paths() {
        let first = b"M  first.rs\0";
        let mut input = Cursor::new([first.as_slice(), b"M  second.rs\0"].concat());

        let changed = parse_git_status(&mut input, 1, "");

        assert_eq!(changed, HashSet::from(["first.rs".to_string()]));
        assert_eq!(input.position(), first.len() as u64);
    }

    #[test]
    fn diff_name_parser_stops_after_collecting_max_paths() {
        let first = b"first.rs\0";
        let mut input = Cursor::new([first.as_slice(), b"second.rs\0"].concat());

        let changed = parse_diff_names(&mut input, 1, "");

        assert_eq!(changed, vec!["first.rs".to_string()]);
        assert_eq!(input.position(), first.len() as u64);
    }

    #[test]
    fn diff_hunk_parser_reads_complete_records_beyond_the_old_byte_cap() {
        let mut diff = String::from("+++ b/first.rs\n@@ -1 +1 @@\n");
        diff.push_str(&format!(" {}\n", "x".repeat(8 * 1024 * 1024)));
        diff.push_str("+++ b/second.rs\n@@ -9,2 +10,3 @@\n");

        let ranges = parse_git_diff_hunks(Cursor::new(diff), 10, "").expect("diff hunks");

        assert_eq!(
            ranges,
            vec![
                GitHunkRange {
                    path: "first.rs".into(),
                    start_line: 1,
                    end_line: 1,
                },
                GitHunkRange {
                    path: "second.rs".into(),
                    start_line: 10,
                    end_line: 12,
                },
            ]
        );
    }

    #[test]
    fn diff_hunk_parser_preserves_empty_target_boundaries() {
        let diff = "+++ b/first.rs\n@@ -1 +0,0 @@\n+++ b/later.rs\n@@ -4 +3,0 @@\n";

        let ranges = parse_git_diff_hunks(Cursor::new(diff), 10, "").expect("diff hunks");

        assert_eq!(
            ranges,
            vec![
                GitHunkRange {
                    path: "first.rs".into(),
                    start_line: 1,
                    end_line: 0,
                },
                GitHunkRange {
                    path: "later.rs".into(),
                    start_line: 4,
                    end_line: 3,
                },
            ]
        );
    }

    #[test]
    fn git_changed_paths_kills_a_timed_out_process() {
        let root = tempfile::tempdir().expect("root");
        let program = root.path().join("slow-git");
        fs::write(&program, "#!/bin/sh\nexec sleep 5\n").expect("script");
        let mut permissions = fs::metadata(&program).expect("metadata").permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&program, permissions).expect("executable");
        let started = Instant::now();

        let observation =
            git_working_tree_status_with(root.path(), 64, &program, Duration::from_millis(50));

        assert!(observation.changed_paths.is_empty());
        assert!(!observation.available);
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[cfg(unix)]
    #[test]
    fn git_capture_kills_the_producer_when_output_crosses_the_budget() {
        let root = tempfile::tempdir().expect("root");
        let program = root.path().join("large-git");
        let marker = root.path().join("producer-finished");
        fs::write(
            &program,
            format!(
                "#!/bin/sh\nhead -c 1048576 /dev/zero\ntouch '{}'\n",
                marker.display()
            ),
        )
        .expect("script");
        let mut permissions = fs::metadata(&program).expect("metadata").permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&program, permissions).expect("executable");

        let error = run_git_capture(
            root.path(),
            &program,
            &[],
            GitCaptureOptions {
                timeout: Duration::from_secs(2),
                field: "test",
                timeout_reason: "timed out",
                failure_reason: "failed",
                max_output_bytes: 1_024,
            },
        )
        .expect_err("capture must stop at its byte budget");

        assert!(
            matches!(
                &error,
                Error::RequestLimitExceeded {
                    field: "git output bytes",
                    requested: 1_025,
                    limit: 1_024,
                }
            ),
            "unexpected capture error: {error:?}"
        );
        std::thread::sleep(Duration::from_millis(50));
        assert!(
            !marker.exists(),
            "producer ran after its output was rejected"
        );
    }

    #[cfg(unix)]
    #[test]
    fn name_only_probe_preserves_best_effort_failure_semantics() {
        let root = tempfile::tempdir().expect("root");
        let program = root.path().join("large-git");
        fs::write(&program, "#!/bin/sh\nhead -c 1048576 /dev/zero\n").expect("script");
        let mut permissions = fs::metadata(&program).expect("metadata").permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&program, permissions).expect("executable");

        let changed = diff_name_only(
            root.path(),
            &program,
            "base",
            None,
            1,
            Duration::from_secs(2),
            "",
        )
        .expect("name-only probe remains best effort");

        assert!(changed.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn git_capture_releases_an_oversized_reader_after_the_child_exits() {
        let root = tempfile::tempdir().expect("root");
        let program = root.path().join("fast-git");
        fs::write(&program, "#!/bin/sh\nprintf xx\n").expect("script");
        let mut permissions = fs::metadata(&program).expect("metadata").permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&program, permissions).expect("executable");

        let (result_sender, result_receiver) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let result = run_git_capture(
                root.path(),
                &program,
                &[],
                GitCaptureOptions {
                    timeout: Duration::from_secs(2),
                    field: "test",
                    timeout_reason: "timed out",
                    failure_reason: "failed",
                    max_output_bytes: 1,
                },
            );
            let _ = result_sender.send(result);
        });

        let error = result_receiver
            .recv_timeout(Duration::from_secs(3))
            .expect("oversized reader must be released after child exit")
            .expect_err("capture must reject output above its byte budget");
        assert!(matches!(
            error,
            Error::RequestLimitExceeded {
                field: "git output bytes",
                requested: 2,
                limit: 1,
            }
        ));
    }

    #[cfg(unix)]
    #[test]
    fn git_capture_terminates_descendants_that_inherit_stdout() {
        let root = tempfile::tempdir().expect("root");
        let program = root.path().join("forking-git");
        fs::write(&program, "#!/bin/sh\nsleep 30 &\nexit 0\n").expect("script");
        let mut permissions = fs::metadata(&program).expect("metadata").permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&program, permissions).expect("executable");

        let started = Instant::now();
        let output = run_git_capture(
            root.path(),
            &program,
            &[],
            GitCaptureOptions {
                timeout: Duration::from_secs(2),
                field: "test",
                timeout_reason: "timed out",
                failure_reason: "failed",
                max_output_bytes: 1_024,
            },
        )
        .expect("capture");

        assert!(output.is_empty());
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "inherited stdout kept the capture alive"
        );
    }
}
