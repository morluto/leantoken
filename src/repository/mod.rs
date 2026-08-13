use std::{
    collections::HashSet,
    path::{Component, Path, PathBuf},
    time::UNIX_EPOCH,
};

use ignore::WalkBuilder;
use tokio_util::sync::CancellationToken;

use crate::config::DiscoveryLimits;
use crate::error::IndexLimitKind;
use crate::{Error, Result};

#[path = "discovery.rs"]
mod discovery;
#[path = "path.rs"]
mod path;
#[path = "scope.rs"]
mod scope;

pub(crate) use discovery::*;
pub(crate) use leantoken_git::{
    GitBlob, GitBlobBatch, GitCommitMetadata, GitWorkingTreeStatus, git_blobs_at_resolved_revision,
    git_blobs_at_revision, git_branch_name, git_commit_metadata, git_diff_hunks_scoped,
    git_diff_identity, git_head_revision, git_line_history, git_working_tree_status,
};
pub(crate) use path::*;

pub use discovery::{
    DiscoveredFile, DiscoveryPolicy, DiscoveryResult, DiscoveryStats, discover_files,
    discover_files_cancellable, discover_files_with_limits, discover_files_with_limits_and_policy,
    discover_files_with_limits_cancellable,
};
pub use leantoken_git::{GitDiffResult, GitHunkRange};

/// Resolve paths changed between a Git revision and the working tree.
pub fn git_diff_paths(root: &Path, base_revision: &str, max: usize) -> Result<GitDiffResult> {
    Ok(leantoken_git::git_diff_paths(root, base_revision, max)?)
}

/// Resolve paths changed between two immutable Git revisions.
pub fn git_diff_paths_between(
    root: &Path,
    base_revision: &str,
    head_revision: &str,
    max: usize,
) -> Result<GitDiffResult> {
    Ok(leantoken_git::git_diff_paths_between(
        root,
        base_revision,
        head_revision,
        max,
    )?)
}

/// Resolve target-side hunk ranges from a revision to the working tree.
pub fn git_diff_hunks(root: &Path, base_revision: &str, max: usize) -> Result<Vec<GitHunkRange>> {
    Ok(leantoken_git::git_diff_hunks(root, base_revision, max)?)
}

/// Resolve target-side hunk ranges between two immutable revisions.
pub fn git_diff_hunks_between(
    root: &Path,
    base_revision: &str,
    head_revision: &str,
    max: usize,
) -> Result<Vec<GitHunkRange>> {
    Ok(leantoken_git::git_diff_hunks_between(
        root,
        base_revision,
        head_revision,
        max,
    )?)
}

/// Return bounded working-tree changes, or an empty set when Git is unavailable.
pub fn git_changed_paths(root: &Path, max: usize) -> Result<HashSet<String>> {
    Ok(leantoken_git::git_changed_paths(root, max)?)
}

pub(crate) fn git_blob_at_revision(
    root: &Path,
    revision: &str,
    path: &str,
    max_bytes: usize,
) -> Result<leantoken_git::GitBlob> {
    Ok(leantoken_git::git_blob_at_revision(
        root, revision, path, max_bytes,
    )?)
}
pub use path::{
    RepositoryPath, RepositoryPattern, RepositoryPatternSet, normalize_relative, resolve_existing,
    slash_path, validate_relative,
};
pub use scope::IndexScope;

#[cfg(all(test, unix))]
mod tests;
