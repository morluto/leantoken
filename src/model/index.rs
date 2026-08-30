use super::*;

/// Caller-selected strategy for reconciling repository files into the index.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum IndexingMode {
    /// Reconcile changed repository state, rebuilding only when required for correctness.
    #[default]
    Reconcile,
    /// Replace the complete committed index even when incremental reconciliation is possible.
    Rebuild,
}

impl IndexingMode {
    pub(crate) const fn from_rebuild_flag(rebuild: bool) -> Self {
        if rebuild {
            Self::Rebuild
        } else {
            Self::Reconcile
        }
    }

    pub(crate) const fn is_rebuild(self) -> bool {
        matches!(self, Self::Rebuild)
    }
}

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
    /// Whether this index covers the full repository or an explicit scope.
    #[serde(default)]
    pub index_scope: IndexScopeMode,
    /// Compact identity for a scoped index, omitted for full indexes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub index_scope_digest: Option<String>,
    /// Canonical include patterns used to define a scoped index.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub index_include_paths: Vec<String>,
    /// Canonical exclude patterns used to define a scoped index.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub index_exclude_paths: Vec<String>,
    pub files_seen: usize,
    pub files_indexed: usize,
    pub files_unchanged: usize,
    pub files_removed: usize,
    pub files_skipped: usize,
    pub warnings: Vec<String>,
}

/// Additive index details serialized alongside the stable response fields.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct IndexReport {
    /// Stable index response fields shared by all report representations.
    #[serde(flatten)]
    pub response: IndexResponse,
    /// Known aggregate preparation skip counts whose sum equals `files_skipped`.
    ///
    /// Older deserialized responses omit this field because their reason
    /// breakdown is unknown. Responses produced by this version always include
    /// the fixed-shape object.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skip_reasons: Option<IndexSkipReasonCounts>,
}

impl IndexReport {
    /// Attach a known preparation breakdown to an index response.
    #[must_use]
    pub fn with_skip_reasons(response: IndexResponse, skip_reasons: IndexSkipReasonCounts) -> Self {
        Self {
            response,
            skip_reasons: Some(skip_reasons),
        }
    }

    /// Discard additive details and return the stable index response.
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

/// Exact file and source-byte totals for one parser-coverage category.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ParserCoverageCount {
    /// Indexed files in this category.
    pub files: usize,
    /// Complete source bytes represented by those files.
    pub source_bytes: u64,
}

/// Bounded coverage details for one recognized structural language.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ParserLanguageCoverage {
    /// Parser-owned stable language label.
    pub language: String,
    /// All indexed files recognized as this language.
    pub total: ParserCoverageCount,
    /// Recognized files whose syntax tree contained no recovery nodes.
    pub complete: ParserCoverageCount,
    /// Recognized files whose syntax tree required recovery.
    pub incomplete: ParserCoverageCount,
}

/// Bounded aggregate for one safe unrecognized extension family.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ParserExtensionCoverage {
    /// Lower-cased extension label or a fixed non-sensitive category.
    pub extension: String,
    /// Indexed files and bytes in this extension family.
    pub total: ParserCoverageCount,
}

/// Generation-scoped structural parser coverage for indexed source.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ParserCoverageSummary {
    /// Every indexed source file in the pinned snapshot.
    pub indexed: ParserCoverageCount,
    /// Files recognized by a configured structural parser.
    pub recognized: ParserCoverageCount,
    /// Recognized files whose syntax tree contained no recovery nodes.
    pub complete: ParserCoverageCount,
    /// Recognized files whose syntax tree required recovery.
    pub incomplete: ParserCoverageCount,
    /// Indexed files without a configured structural parser.
    pub unrecognized: ParserCoverageCount,
    /// Highest-count recognized languages in deterministic order.
    pub languages: Vec<ParserLanguageCoverage>,
    /// Exact remainder after the bounded language list.
    pub other_languages: ParserCoverageCount,
    /// Highest-count safe unrecognized extension families.
    pub unrecognized_extensions: Vec<ParserExtensionCoverage>,
    /// Exact remainder after the bounded extension list.
    pub other_unrecognized_extensions: ParserCoverageCount,
}

/// Explicit generation-scoped structural parser coverage report.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ParserCoverageReport {
    /// Repository generation pinned while all coverage metadata was read.
    pub repository_generation: u64,
    /// Exact totals and bounded deterministic group breakdowns.
    pub coverage: ParserCoverageSummary,
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

impl IndexProgressPhase {
    pub(crate) const fn is_active(self) -> bool {
        !matches!(self, Self::Completed | Self::Failed | Self::Cancelled)
    }
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
    /// Whether the managed platform cache was unavailable and the active index
    /// was placed in the repository-local fallback.
    #[serde(default)]
    pub repository_cache_fallback: bool,
    /// Index-content compatibility version used by this binary.
    #[serde(default)]
    pub index_content_version: u32,
    /// Exact runtime identity of code and dependencies that derive persisted rows.
    #[serde(default)]
    pub index_derivation_fingerprint: String,
    /// Identity recorded with the currently committed rows, when initialized.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub persisted_index_derivation_fingerprint: Option<String>,
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
