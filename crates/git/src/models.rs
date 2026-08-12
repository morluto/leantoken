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
pub struct GitBlob {
    /// Resolved 12-character revision.
    pub revision: String,
    /// File contents at the revision.
    pub content: String,
}

/// Bounded UTF-8 blobs loaded together from one immutable Git revision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitBlobBatch {
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
pub struct GitLineCommit {
    pub commit: String,
    pub authored_at: String,
    pub subject: String,
}

/// Shared metadata for one exact immutable commit endpoint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitCommitMetadata {
    pub revision: String,
    pub authored_at: String,
    pub subject: String,
}
use super::*;
