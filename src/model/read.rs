use super::*;

/// Typed read target selected by the caller. The flat `ReadRequest` option
/// fields remain the wire-compatible input; this enum provides a typed
/// projection for programmatic callers and internal resolution.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ReadTarget {
    /// Read one indexed symbol definition.
    Symbol { identity: SymbolIdentity },
    /// Read one indexed Markdown or LaTeX section by exact title or outline signature.
    Heading {
        name: String,
        #[serde(default = "default_heading_occurrence")]
        occurrence: usize,
    },
    /// Read one inclusive one-based line range.
    Lines { start: usize, end: usize },
    /// Continue a truncated read without losing a partial final line.
    Continuation { cursor: String },
}

fn default_heading_occurrence() -> usize {
    1
}

/// I/O and verification policy for a live read.
///
/// `Bounded` (default) stops reading after the requested page is satisfied,
/// reports `index_state: unknown`, and emits a metadata-bound continuation
/// cursor. `Full` hashes the complete live file, reports current/stale with
/// live and indexed hashes, and is required for delta requests.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ReadPolicy {
    /// Stop after the requested page; no full-file hash or index staleness.
    #[default]
    Bounded,
    /// Hash the complete live file and report index verification metadata.
    Full,
}

impl ReadPolicy {
    pub(crate) const fn is_full(self) -> bool {
        matches!(self, Self::Full)
    }
}

impl std::fmt::Display for ReadPolicy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Bounded => write!(f, "bounded"),
            Self::Full => write!(f, "full"),
        }
    }
}

/// Index verification state reported by a read response.
///
/// `Unknown` is reported by bounded reads that stop before EOF. `Current` and
/// `Stale` are reported by full reads that hash the complete live file and
/// compare it to the indexed snapshot.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ReadIndexState {
    /// The live file hash matches the indexed snapshot.
    Current,
    /// The live file hash differs from the indexed snapshot.
    Stale,
    /// The read stopped before EOF and could not verify index freshness.
    #[default]
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
/// Input for `leantoken.read`.
pub struct ReadRequest {
    /// Repository-relative file path.
    pub path: String,
    /// First one-based line; defaults to the start of the file.
    #[serde(default)]
    pub start_line: Option<usize>,
    /// Last one-based line; defaults to the end of the file.
    #[serde(default)]
    pub end_line: Option<usize>,
    /// Indexed symbol to read; cannot be combined with line fields.
    #[serde(default)]
    pub symbol: Option<String>,
    /// Indexed Markdown or LaTeX section title or outline signature to read.
    #[serde(default)]
    pub heading: Option<String>,
    /// One-based occurrence of a duplicate document heading; defaults to 1.
    #[serde(default)]
    pub heading_occurrence: Option<usize>,
    /// Opaque cursor returned by a truncated read; cannot be combined with a new target.
    #[serde(default)]
    pub continuation_cursor: Option<String>,
    /// Maximum source tokens to return.
    #[serde(default)]
    pub max_tokens: Option<usize>,
    /// Hash from the same prior range; matching content returns `not_modified`.
    #[serde(default)]
    pub expected_hash: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
/// Input for an explicitly live worktree read.
pub struct WorktreeReadRequest {
    pub path: String,
    #[serde(default)]
    pub start_line: Option<usize>,
    #[serde(default)]
    pub end_line: Option<usize>,
    #[serde(default)]
    pub symbol: Option<String>,
    #[serde(default)]
    pub heading: Option<String>,
    #[serde(default)]
    pub heading_occurrence: Option<usize>,
    #[serde(default)]
    pub continuation_cursor: Option<String>,
    #[serde(default)]
    pub max_tokens: Option<usize>,
    #[serde(default)]
    pub expected_hash: Option<String>,
    /// Record a bounded base and prefer a cheaper changed follow-up. Without
    /// `expected_hash`, select the latest compatible base for this exact target.
    /// Requires `policy: full`.
    #[serde(default)]
    pub delta: bool,
    /// Server-managed receipt whose previously returned evidence should be suppressed.
    #[serde(default)]
    pub receipt_id: Option<String>,
    /// Live-file I/O and verification policy.
    #[serde(default)]
    pub policy: ReadPolicy,
}

impl WorktreeReadRequest {
    pub(crate) fn into_read_request(self) -> (ReadRequest, bool, Option<String>, ReadPolicy) {
        (
            ReadRequest {
                path: self.path,
                start_line: self.start_line,
                end_line: self.end_line,
                symbol: self.symbol,
                heading: self.heading,
                heading_occurrence: self.heading_occurrence,
                continuation_cursor: self.continuation_cursor,
                max_tokens: self.max_tokens,
                expected_hash: self.expected_hash,
            },
            self.delta,
            self.receipt_id,
            self.policy,
        )
    }
}

impl From<ReadRequest> for WorktreeReadRequest {
    fn from(read: ReadRequest) -> Self {
        Self {
            path: read.path,
            start_line: read.start_line,
            end_line: read.end_line,
            symbol: read.symbol,
            heading: read.heading,
            heading_occurrence: read.heading_occurrence,
            continuation_cursor: read.continuation_cursor,
            max_tokens: read.max_tokens,
            expected_hash: read.expected_hash,
            delta: false,
            receipt_id: None,
            policy: ReadPolicy::Bounded,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ReadResponse {
    pub path: String,
    pub status: ReadStatus,
    /// First line in the complete resolved target.
    #[serde(default)]
    pub target_start_line: usize,
    /// Last line in the complete resolved target.
    #[serde(default)]
    pub target_end_line: usize,
    /// First line represented by this response page.
    #[serde(default)]
    pub returned_start_line: usize,
    /// Last line represented by this response page.
    #[serde(default)]
    pub returned_end_line: usize,
    /// Whether source remains after this response page.
    #[serde(default)]
    pub truncated: bool,
    /// First line represented by the next response page.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_start_line: Option<usize>,
    /// Opaque continuation bound to this repository generation and live file content.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub continuation_cursor: Option<String>,
    /// Source-budget guidance for completing a truncated target.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub truncation_guidance: Option<ReadTruncationGuidance>,
    /// Whether an explicit or automatically selected base matched this response page.
    #[serde(default)]
    pub not_modified: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    /// Unified diff from the requested base hash to `content_hash`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delta: Option<String>,
    /// Bounded delta decision and accounting metadata.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delta_receipt: Option<ReadDeltaReceipt>,
    pub content_hash: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub indexed_hash: Option<String>,
    pub index_stale: bool,
    /// Index verification state. `unknown` for bounded reads; `current` or
    /// `stale` for full reads. Retained alongside `indexed_hash`/`index_stale`
    /// for backward-compatible clients.
    #[serde(default)]
    pub index_state: ReadIndexState,
    /// Number of live file bytes read to produce this response. Bounded reads
    /// stop after the requested page; full reads scan the complete file.
    #[serde(default)]
    pub live_bytes_read: usize,
    pub meta: ResponseMeta,
}

/// Bounded guidance for avoiding repeated undersized continuation reads.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct ReadTruncationGuidance {
    /// Evidence used to size the complete target and remaining suffix.
    pub basis: ReadTruncationGuidanceBasis,
    /// Source tokens in the complete resolved target.
    pub target_source_tokens: usize,
    /// Source tokens after the byte-exact progress represented by this page.
    pub remaining_source_tokens: usize,
    /// Additional calls estimated when the caller keeps this page's source budget.
    pub remaining_pages_at_current_budget: usize,
    /// Source budget recommended for the next continuation call.
    pub recommended_next_max_tokens: usize,
    /// Fewest additional calls possible under the configured source-token ceiling.
    pub minimum_remaining_pages: usize,
}

/// Confidence boundary for [`ReadTruncationGuidance`] token counts.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ReadTruncationGuidanceBasis {
    /// Counts come from the same immutable generation as the returned source.
    PublishedGeneration,
    /// Full live-file verification proved the indexed target is current.
    VerifiedLive,
    /// Counts come from the pinned indexed generation; the bounded live page may be newer.
    IndexedGenerationEstimate,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ReadStatus {
    Content,
    /// The response contains only part of the resolved target.
    Truncated,
    NotModified,
    /// A complete unified diff is returned instead of full current content.
    Delta,
    /// A server-managed evidence receipt already contained the exact current content.
    ReceiptSuppressed,
}

/// Result selected by an opt-in read delta request.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ReadDeltaOutcome {
    /// Full current content was returned.
    Full,
    /// A complete unified diff was returned.
    Delta,
    /// The requested or automatically selected base already identifies current content.
    NotModified,
    /// A general evidence receipt already contained the exact current content.
    ReceiptSuppressed,
}

/// Why an opt-in read delta attempt returned full content.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ReadDeltaFallback {
    /// No bounded base matched the exact target and optional requested hash.
    BaseUnavailable,
    /// The resolved target or returned coordinates changed.
    TargetChanged,
    /// The current response is truncated and cannot be a complete delta target.
    CurrentTruncated,
    /// The current page exceeds the per-entry delta-state bound.
    ContentTooLarge,
    /// The complete delta response was not smaller than a full-content response.
    DeltaNotSmaller,
}

/// Where the selected delta base was recovered.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ReadDeltaBaseSource {
    /// The base existed only in the current service process.
    ProcessLocal,
    /// The base was recovered from the bounded repository cache.
    Persistent,
}

/// Why a complete current base remained process-local.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ReadDeltaPersistenceFallback {
    /// A partial page cannot prove the complete target content.
    CurrentTruncated,
    /// The complete target exceeds the per-base persistence bound.
    ContentTooLarge,
    /// The live file hash differs from the indexed snapshot.
    LiveDiffersFromIndex,
    /// No same-generation indexed hash was available to prove eligibility.
    IndexedHashUnavailable,
    /// The bounded persistent cache could not retain another eligible base.
    StorageCapacity,
}

/// Provenance and token accounting for one opt-in read delta decision.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ReadDeltaReceipt {
    /// Stable hash of the repository and caller-selected target.
    pub target_key: String,
    /// Requested or automatically selected prior content hash.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_hash: Option<String>,
    /// Hash of the complete current response page.
    pub head_hash: String,
    /// Repository generation observed when the bounded base was captured.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_generation: Option<u64>,
    /// Storage tier that supplied the selected base.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_source: Option<ReadDeltaBaseSource>,
    /// Repository generation used to resolve the current target.
    pub head_generation: u64,
    /// Selected response form.
    pub outcome: ReadDeltaOutcome,
    /// Tokens required by full current content.
    pub full_tokens: usize,
    /// Tokens in the returned delta, or zero for `not_modified`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delta_tokens: Option<usize>,
    /// Full-content tokens avoided by the selected response.
    pub avoided_tokens: usize,
    /// Whether the complete current base was retained in the repository cache.
    #[serde(default)]
    pub head_persisted: bool,
    /// Why the current base was intentionally not persisted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub persistence_fallback_reason: Option<ReadDeltaPersistenceFallback>,
    /// Explicit reason full content was retained after a delta attempt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fallback_reason: Option<ReadDeltaFallback>,
}
