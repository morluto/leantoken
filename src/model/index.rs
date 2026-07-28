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

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct StatusResponse {
    pub repository_root: String,
    pub database_path: String,
    /// Index-content compatibility version used by this binary.
    #[serde(default)]
    pub index_content_version: u32,
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
    pub languages: Vec<LanguageCount>,
    pub warnings: Vec<String>,
}
