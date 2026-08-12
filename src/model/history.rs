use super::*;

/// Git-backed symbol history operation.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum HistoryOperation {
    /// Read one parsed symbol from a historical file blob.
    ReadSymbol {
        /// Repository-relative source path.
        path: String,
        /// Exact parsed symbol name, optionally qualified as `parent.name`.
        symbol: String,
        /// Git revision containing the source blob.
        revision: String,
    },
    /// Compare one parsed symbol between two revisions.
    DiffSymbol {
        /// Repository-relative source path at both revisions.
        path: String,
        /// Exact parsed symbol name, optionally qualified as `parent.name`.
        symbol: String,
        /// Older Git revision.
        base_revision: String,
        /// Newer Git revision.
        head_revision: String,
    },
    /// List recent commits that touched the symbol's tracked line history.
    SymbolLog {
        /// Repository-relative source path.
        path: String,
        /// Exact parsed symbol name at `revision`, optionally qualified as `parent.name`.
        symbol: String,
        /// Revision from which line history starts; defaults to `HEAD`.
        #[serde(default)]
        revision: Option<String>,
    },
}

/// Input for Git-backed symbol history retrieval.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct HistoryRequest {
    /// Historical operation and its exact target.
    pub operation: HistoryOperation,
    /// Maximum commits returned by `symbol_log`; defaults to 20.
    #[serde(default)]
    pub max_results: Option<usize>,
    /// Maximum source or diff tokens returned; defaults to 8000.
    #[serde(default)]
    pub max_tokens: Option<usize>,
}

/// One parsed symbol read from an immutable Git revision.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct HistoricalSymbol {
    /// Resolved 12-character revision.
    pub revision: String,
    pub path: String,
    pub name: String,
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    /// First line of the complete historical symbol.
    pub target_start_line: usize,
    /// Last line of the complete historical symbol.
    pub target_end_line: usize,
    /// Last line represented by `content`; omitted from serialized metadata when
    /// source is absent.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub returned_end_line: usize,
    /// Whether source remains after `content`.
    pub truncated: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    pub content_hash: String,
}

/// One commit returned by symbol line-history traversal.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct SymbolHistoryCommit {
    /// Full commit object ID.
    pub commit: String,
    /// Commit author date in strict ISO 8601 format.
    pub authored_at: String,
    /// Commit subject.
    pub subject: String,
}

/// Git-backed symbol history response.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct HistoryResponse {
    /// Resolved operation kind.
    pub kind: String,
    /// Historical symbol for `read_symbol`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub symbol: Option<HistoricalSymbol>,
    /// Base-side symbol for `diff_symbol`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before: Option<HistoricalSymbol>,
    /// Head-side symbol for `diff_symbol`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after: Option<HistoricalSymbol>,
    /// Unified symbol diff for `diff_symbol`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diff: Option<String>,
    /// Whether the unified diff was truncated by `max_tokens`.
    #[serde(default)]
    pub diff_truncated: bool,
    /// Deterministic classification for `diff_symbol`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub semantic_change: Option<DiffSymbolChange>,
    /// Recent commits for `symbol_log`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub commits: Vec<SymbolHistoryCommit>,
    /// Whether all matching commits fit `max_results`.
    #[serde(default)]
    pub result_complete: bool,
    pub meta: ResponseMeta,
}

/// One exact symbol pairing for a bounded multi-symbol revision diff.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct DiffSymbolsTarget {
    /// Repository-relative path at the base revision.
    pub path: String,
    /// Exact base-side symbol name, optionally qualified as `parent.name`.
    pub symbol: String,
    /// Different repository-relative head path for an explicit rename or move.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head_path: Option<String>,
    /// Different exact head-side symbol name for an explicit rename.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head_symbol: Option<String>,
}

/// Input for one bounded, batched multi-symbol revision diff.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct DiffSymbolsRequest {
    /// Ordered exact symbol pairings; the order is preserved across pages.
    pub targets: Vec<DiffSymbolsTarget>,
    /// Older Git revision.
    pub base_revision: String,
    /// Newer Git revision.
    pub head_revision: String,
    /// Maximum symbol results returned on one page; defaults to 20.
    #[serde(default)]
    pub max_results: Option<usize>,
    /// Maximum tokens shared by all unified diffs on one page; defaults to 8000.
    #[serde(default)]
    pub max_tokens: Option<usize>,
    /// Opaque cursor returned by an earlier page of this exact immutable request.
    #[serde(default)]
    pub cursor: Option<String>,
}

/// Shared immutable commit metadata for one multi-symbol diff endpoint.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct HistoryRevisionMetadata {
    /// Resolved 12-character commit identity.
    pub revision: String,
    /// Commit author date in strict ISO 8601 format.
    pub authored_at: String,
    /// Commit subject.
    pub subject: String,
}

/// Per-target result state for a multi-symbol revision diff.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DiffSymbolsStatus {
    Unchanged,
    Added,
    Removed,
    Renamed,
    Modified,
    NotFound,
    Unavailable,
}

/// Why one returned multi-symbol diff is incomplete.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DiffSymbolsIncompleteReason {
    MaxTokens,
    MaxDiffBytes,
    MaxResponseTokens,
}

/// One result from a bounded multi-symbol revision diff.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct DiffSymbolsResult {
    /// Zero-based position in the complete ordered request.
    pub request_index: usize,
    /// Normalized requested symbol pairing.
    pub target: DiffSymbolsTarget,
    pub status: DiffSymbolsStatus,
    /// Base-side parsed symbol when present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before: Option<HistoricalSymbol>,
    /// Head-side parsed symbol when present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after: Option<HistoricalSymbol>,
    /// Unified diff for changed, added, removed, or renamed symbols.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diff: Option<String>,
    /// Whether the unified diff was truncated by a declared request bound.
    #[serde(default)]
    pub diff_truncated: bool,
    /// Deterministic semantic classification when a change was resolved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub semantic_change: Option<DiffSymbolChange>,
    /// Stable reason for an unavailable target or truncated diff.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Typed truncation reason when the result remains otherwise usable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub incomplete_reason: Option<DiffSymbolsIncompleteReason>,
}

/// Directly observed work counters for one bounded multi-symbol diff page.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct DiffSymbolsDiagnostics {
    /// Git subprocesses executed for identity, commit metadata, and batched blobs.
    pub git_subprocesses: usize,
    pub base_paths_requested: usize,
    pub head_paths_requested: usize,
    pub base_blob_bytes: usize,
    pub head_blob_bytes: usize,
    /// Parsed definitions retained across both endpoints.
    pub parsed_symbols: usize,
    /// Unified diff bytes present in the response after all fitting.
    pub retained_diff_bytes: usize,
}

/// Bounded multi-symbol revision diff with shared endpoint metadata.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct DiffSymbolsResponse {
    pub kind: String,
    pub base: HistoryRevisionMetadata,
    pub head: HistoryRevisionMetadata,
    pub results: Vec<DiffSymbolsResult>,
    /// Whether all requested targets and their diff text were returned completely.
    pub result_complete: bool,
    pub diagnostics: DiffSymbolsDiagnostics,
    pub meta: ResponseMeta,
}
