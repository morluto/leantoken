use super::*;

/// Bounded aggregate counts for files skipped during index preparation.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct IndexSkipReasonCounts {
    /// Files detected as binary during preparation.
    pub binary: usize,
    /// Files admitted by discovery that exceeded the byte limit before reading completed.
    pub oversized_during_read: usize,
    /// Files whose preparation failed before searchable content could be produced.
    pub failed: usize,
}

impl IndexSkipReasonCounts {
    /// Return the total number of preparation skips across every public reason.
    #[must_use]
    pub fn total(&self) -> usize {
        self.binary
            .saturating_add(self.oversized_during_read)
            .saturating_add(self.failed)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct IndexResponse {
    pub repository_generation: u64,
    pub files_seen: usize,
    pub files_indexed: usize,
    pub files_unchanged: usize,
    pub files_removed: usize,
    pub files_skipped: usize,
    pub warnings: Vec<String>,
}

/// Additive index details serialized beside the compatible response fields.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct IndexReport {
    /// Source-compatible index response retained for existing Rust consumers.
    #[serde(flatten)]
    pub response: IndexResponse,
    /// Known aggregate preparation skip counts whose sum equals `files_skipped`.
    ///
    /// Legacy deserialized responses omit this field because their reason
    /// breakdown is unknown. Responses produced by this version always include
    /// the fixed-shape object.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skip_reasons: Option<IndexSkipReasonCounts>,
}

impl IndexReport {
    /// Attach a known preparation breakdown to a compatible index response.
    #[must_use]
    pub fn with_skip_reasons(response: IndexResponse, skip_reasons: IndexSkipReasonCounts) -> Self {
        Self {
            response,
            skip_reasons: Some(skip_reasons),
        }
    }

    /// Discard additive details and return the compatible index response.
    #[must_use]
    pub fn into_response(self) -> IndexResponse {
        self.response
    }
}

impl std::ops::Deref for IndexReport {
    type Target = IndexResponse;

    fn deref(&self) -> &Self::Target {
        &self.response
    }
}

/// Bounded phases exposed while the first repository generation is built.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum IndexProgressPhase {
    /// Ignore-aware bounded repository traversal.
    Discovery,
    /// Existing-state load, hashing, and reconciliation planning.
    HashAndPlan,
    /// Parallel bounded-batch source preparation.
    Preparation,
    /// Import resolution and relational transaction staging.
    RelationalWrite,
    /// Word-tokenized chunk FTS rebuild.
    ChunkWordFts,
    /// Trigram-tokenized chunk FTS rebuild.
    ChunkTrigramFts,
    /// Symbol trigram FTS rebuild.
    SymbolFts,
    /// Symbol-reference trigram FTS rebuild.
    ReferenceFts,
    /// Atomic commit, including any SQLite auto-checkpoint work.
    CommitAndCheckpoint,
    /// The complete generation committed successfully.
    Completed,
    /// The attempt ended with a non-cancellation error.
    Failed,
    /// Cooperative cancellation stopped the attempt.
    Cancelled,
}

/// Bounded, read-only progress for an initial repository reconciliation.
///
/// Detailed fields are absent when this process is only following an index
/// leader and cannot observe its in-memory counters safely.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct IndexProgressSnapshot {
    /// Opaque identity for the cache this attempt belongs to.
    pub cache_namespace: String,
    /// Whether process-local attempt details are available.
    pub detail_available: bool,
    /// Whether a reconciliation is currently active.
    pub active: bool,
    /// Last committed repository generation observed by this snapshot.
    pub current_generation: u64,
    /// Opaque identity that changes for every local reconciliation attempt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attempt_id: Option<String>,
    /// Current or terminal phase for the local attempt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase: Option<IndexProgressPhase>,
    /// Unix timestamp when the attempt began.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_unix_ms: Option<u64>,
    /// Monotonic elapsed duration sampled while assembling this response.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub elapsed_ms: Option<u64>,
    /// Unix timestamp of the most recent phase or bounded-counter update.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_progress_unix_ms: Option<u64>,
    /// Monotonic sequence within this attempt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub update_sequence: Option<u64>,
    /// Filesystem entries yielded during completed discovery.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub walk_entries: Option<u64>,
    /// Files admitted by completed discovery.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub files_discovered: Option<u64>,
    /// Aggregate metadata bytes admitted by completed discovery.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub discovered_source_bytes: Option<u64>,
    /// Files consumed by completed preparation batches.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub files_prepared: Option<u64>,
    /// Searchable files staged in the unpublished transaction.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub files_staged: Option<u64>,
    /// Completed bounded preparation batches.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preparation_batches: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct StatusResponse {
    pub repository_root: String,
    pub database_path: String,
    /// Index-content compatibility version used by this binary.
    #[serde(default)]
    pub index_content_version: u32,
    /// Whether the cache covers the full ignore-visible repository.
    #[serde(default)]
    pub index_scope: IndexScopeMode,
    /// Compact opaque identity for a scoped cache.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub index_scope_digest: Option<String>,
    /// Canonical bounded include patterns that define indexed membership.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub index_include_paths: Vec<String>,
    /// Canonical bounded exclude patterns that define indexed membership.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub index_exclude_paths: Vec<String>,
    pub repository_generation: u64,
    /// Whether a committed generation is available for retrieval.
    pub index_state: IndexState,
    /// Whether this read-only status request checked the live working tree.
    #[serde(default)]
    pub working_tree_checked: bool,
    pub freshness: Freshness,
    pub file_count: usize,
    pub chunk_count: usize,
    pub symbol_count: usize,
    /// Bytes occupied by the SQLite main, WAL, and shared-memory files.
    pub index_storage_bytes: u64,
    /// Sum of complete source bytes represented by indexed files.
    pub indexed_source_bytes: u64,
    /// Index storage divided by indexed source bytes, when source is non-empty.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub index_amplification_ratio: Option<f64>,
    /// Resident memory for the current LeanToken process when the platform exposes it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub process_rss_bytes: Option<u64>,
    /// Initial-index progress, omitted after a committed generation is available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub index_progress: Option<IndexProgressSnapshot>,
    pub languages: Vec<LanguageCount>,
    pub warnings: Vec<String>,
}
