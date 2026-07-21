use std::{
    collections::HashSet,
    fs::File,
    io::{BufRead, BufReader, Seek},
    path::{Component, Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, UNIX_EPOCH},
};

use ignore::WalkBuilder;
use tokio_util::sync::CancellationToken;
use wait_timeout::ChildExt;

use crate::config::DiscoveryLimits;
use crate::error::IndexLimitKind;
use crate::{Error, Result};

const LEANTOKEN_IGNORE_FILE: &str = ".leantokenignore";
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
            let relative_path = slash_path(relative);
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
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                tracing::warn!(%error, "repository walk entry skipped");
                continue;
            }
        };
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
        let metadata = match entry.metadata() {
            Ok(metadata) => metadata,
            Err(error) => {
                tracing::warn!(path = %entry.path().display(), %error, "file metadata skipped");
                continue;
            }
        };
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
        let relative_path = slash_path(relative);
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
    path == ".git" || path.starts_with(".git/")
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
    let has_windows_drive = bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && matches!(bytes[2], b'/' | b'\\');
    let has_windows_root = requested.starts_with('\\');
    if has_windows_drive || has_windows_root {
        return Err(Error::PathOutsideRoot(PathBuf::from(requested)));
    }
    let path = Path::new(requested);
    if path.is_absolute() {
        return Err(Error::PathOutsideRoot(path.to_path_buf()));
    }
    for component in path.components() {
        if matches!(
            component,
            Component::ParentDir | Component::RootDir | Component::Prefix(_)
        ) {
            return Err(Error::PathOutsideRoot(path.to_path_buf()));
        }
    }
    Ok(path.to_path_buf())
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

fn git_changed_paths_with(
    root: &Path,
    max: usize,
    program: &Path,
    timeout: Duration,
) -> Result<HashSet<String>> {
    if max == 0 {
        return Ok(HashSet::new());
    }
    let prefix = git_worktree_prefix(root);
    let mut output = match tempfile::tempfile() {
        Ok(output) => output,
        Err(_) => return Ok(HashSet::new()),
    };
    let child_output = match output.try_clone() {
        Ok(output) => output,
        Err(_) => return Ok(HashSet::new()),
    };

    let mut child = match Command::new(program)
        .args([
            "-c",
            "core.fsmonitor=false",
            "status",
            "--porcelain=v1",
            "-z",
            "--untracked-files=all",
            "--no-renames",
            "--",
            ".",
        ])
        .current_dir(root)
        .stdout(Stdio::from(child_output))
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(child) => child,
        Err(_) => return Ok(HashSet::new()),
    };

    let status = match child.wait_timeout(timeout) {
        Ok(Some(status)) => status,
        Ok(None) | Err(_) => {
            let _ = child.kill();
            let _ = child.wait();
            return Ok(HashSet::new());
        }
    };
    if !status.success() || output.rewind().is_err() {
        return Ok(HashSet::new());
    }
    Ok(parse_git_status(output, max, &prefix))
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

fn parse_git_status(output: File, max: usize, prefix: &str) -> HashSet<String> {
    let mut reader = BufReader::new(output);
    let mut changed = HashSet::new();
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
        if record.len() < 4 || record.get(2) != Some(&b' ') {
            continue;
        }

        let status = &record[..2];
        let path = String::from_utf8_lossy(&record[3..]);

        // Ignore ignored files; keep modified, added, deleted, and untracked.
        if status == b"!!" {
            continue;
        }

        let Some(path) = path.strip_prefix(prefix) else {
            continue;
        };
        if changed.len() < max {
            changed.insert(slash_path(Path::new(path)));
        }
    }
    changed
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

/// Resolved diff scope: base/head short SHAs and the changed paths between them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitDiffResult {
    /// Short (12-char) SHA of the resolved base revision.
    pub base_revision: String,
    /// Short (12-char) SHA of the resolved head revision (HEAD).
    pub head_revision: String,
    /// Repository-relative changed paths between base and the working tree.
    pub changed_paths: Vec<String>,
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
    let changed = diff_name_only(root, program, &base_sha, max, timeout, &prefix)?;
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
    let mut child = match Command::new(program)
        .args(["rev-parse", "--verify", "--short=12", revision])
        .current_dir(root)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(child) => child,
        Err(_) => {
            return Err(Error::InvalidInput {
                field: "base revision",
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
                field: "base revision",
                reason: "git rev-parse timed out",
            });
        }
    };
    if !status.success() {
        return Err(Error::InvalidInput {
            field: "base revision",
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
    max: usize,
    timeout: Duration,
    prefix: &str,
) -> Result<Vec<String>> {
    let mut output = match tempfile::tempfile() {
        Ok(output) => output,
        Err(_) => return Ok(Vec::new()),
    };
    let child_output = match output.try_clone() {
        Ok(output) => output,
        Err(_) => return Ok(Vec::new()),
    };
    let mut child = match Command::new(program)
        .args([
            "-c",
            "core.fsmonitor=false",
            "diff",
            "--name-only",
            "-z",
            "--no-renames",
            base_sha,
            "--",
            ".",
        ])
        .current_dir(root)
        .stdout(Stdio::from(child_output))
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(child) => child,
        Err(_) => return Ok(Vec::new()),
    };
    let status = match child.wait_timeout(timeout) {
        Ok(Some(status)) => status,
        Ok(None) | Err(_) => {
            let _ = child.kill();
            let _ = child.wait();
            return Ok(Vec::new());
        }
    };
    if !status.success() || output.rewind().is_err() {
        return Ok(Vec::new());
    }
    Ok(parse_diff_names(output, max, prefix))
}

fn parse_diff_names(output: File, max: usize, prefix: &str) -> Vec<String> {
    let mut reader = BufReader::new(output);
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
        if changed.len() < max {
            changed.push(slash_path(Path::new(path)));
        }
    }
    changed
}

#[cfg(all(test, unix))]
mod tests {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::time::Instant;

    use super::*;

    #[test]
    fn git_changed_paths_kills_a_timed_out_process() {
        let root = tempfile::tempdir().expect("root");
        let program = root.path().join("slow-git");
        fs::write(&program, "#!/bin/sh\nexec sleep 5\n").expect("script");
        let mut permissions = fs::metadata(&program).expect("metadata").permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&program, permissions).expect("executable");
        let started = Instant::now();

        let changed = git_changed_paths_with(root.path(), 64, &program, Duration::from_millis(50))
            .expect("changed paths");

        assert!(changed.is_empty());
        assert!(started.elapsed() < Duration::from_secs(1));
    }
}
