use super::*;

pub(super) const DATABASE_NAME: &str = DEFAULT_INDEX_DATABASE_NAME;
pub(super) const WAL_NAME: &str = "index.sqlite-wal";
pub(super) const PRUNABLE_ARTIFACTS: &[&str] = &[
    DATABASE_NAME,
    WAL_NAME,
    "index.sqlite-shm",
    "index.sqlite-journal",
];
pub(super) const SECONDS_PER_DAY: u64 = 24 * 60 * 60;
pub(super) const CACHE_LIST_CURSOR_PREFIX: &str = "cl1";
pub(super) const CACHE_LIST_V2_CURSOR_PREFIX: &str = "cl2";
pub(super) const CACHE_LIST_CURSOR_HASH_CHARS: usize = 16;
pub(super) const MAX_CACHE_LIST_CURSOR_BYTES: usize = 128;
pub(super) const MAX_CACHE_COMPATIBILITY_FILTERS: usize = 5;
pub(super) const MAX_CACHE_CONTENT_VERSION_FILTERS: usize = 32;

/// Default number of cache entries returned by one list page.
pub const DEFAULT_CACHE_LIST_LIMIT: usize = 20;
/// Maximum number of cache entries returned by one list page.
pub const MAX_CACHE_LIST_LIMIT: usize = 100;

/// Filters and bounds for one managed-cache list operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CacheListRequest {
    /// Return aggregate diagnostics without per-cache entries.
    pub summary: bool,
    /// Keep entries in any of these states; an empty list keeps every state.
    pub states: Vec<CacheState>,
    /// Keep the exact recorded repository root, when present.
    pub repository_root: Option<PathBuf>,
    /// Maximum entries returned by one page.
    pub limit: usize,
    /// Opaque continuation cursor returned by the same filters.
    pub cursor: Option<String>,
}

impl Default for CacheListRequest {
    fn default() -> Self {
        Self {
            summary: false,
            states: Vec::new(),
            repository_root: None,
            limit: DEFAULT_CACHE_LIST_LIMIT,
            cursor: None,
        }
    }
}

/// Versioned compatibility filters layered over the stable cache-list request.
///
/// This separate options type preserves Rust struct-literal compatibility for
/// [`CacheListRequest`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CacheListV2Request {
    /// Existing metadata-state filters and response bounds.
    pub request: CacheListRequest,
    /// Keep entries in any of these content-compatibility classes.
    pub compatibilities: Vec<CacheCompatibility>,
    /// Keep entries with one of these exact versioned content identities.
    pub index_content_versions: Vec<u32>,
    /// Keep only safely classifiable older or legacy-unversioned content.
    pub incompatible_with_current: bool,
}

/// Criteria and consent for one managed-cache prune operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CachePruneRequest {
    /// Remove caches not accessed for at least this many days.
    pub older_than_days: Option<u64>,
    /// Remove least-recently-used caches until managed bytes are at most this value.
    pub max_total_bytes: Option<u64>,
    /// Explicitly remove caches whose recorded repository root is missing.
    pub remove_missing_roots: bool,
    /// Report the resolved deletion plan without changing files.
    pub dry_run: bool,
    /// Confirm a non-dry-run deletion plan.
    pub yes: bool,
}

/// Versioned cache-prune criteria that preserve [`CachePruneRequest`] literals.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CachePruneV2Request {
    /// Existing age, storage, missing-root, and consent criteria.
    pub request: CachePruneRequest,
    /// Select inactive, recognizable older or legacy-unversioned caches.
    pub incompatible_with_current: bool,
}

/// Metadata quality available for one cache directory.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CacheState {
    /// Current schema and access metadata were read successfully.
    Current,
    /// A readable older schema lacks current access metadata.
    Legacy,
    /// Known cache artifacts exist without a readable database.
    Incomplete,
    /// The SQLite database could not be inspected.
    Corrupt,
    /// A newer schema or mismatched identity is not safe for this binary to prune.
    Unsupported,
    /// Unexpected content makes automatic deletion unsafe.
    Unrecognized,
}

/// Compatibility of indexed content with the current LeanToken build.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CacheCompatibility {
    /// Content was produced for the current index-content version.
    CompatibleCurrent,
    /// Content was produced by a known older index-content version.
    ObsoleteOlder,
    /// The legacy cache identity did not record an index-content version.
    LegacyUnversioned,
    /// Content was produced by a newer version this build must preserve.
    NewerUnsupported,
    /// Corrupt or unexpected metadata prevents a trustworthy classification.
    Unknown,
}

/// Source used for the last-access value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AccessTimeSource {
    /// Schema-v5 metadata updated during repository binding.
    Database,
    /// Latest artifact modification time used for an older or incomplete cache.
    FileMtime,
}

/// Auditable description of one managed cache.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CacheEntry {
    /// Stable directory identifier derived from content version, repository root, and scope.
    pub id: String,
    /// Managed cache directory.
    pub path: PathBuf,
    /// Index-content compatibility version encoded by this cache identity.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub index_content_version: Option<u32>,
    /// Whether the cache identity represents full or explicitly scoped membership.
    pub index_scope: IndexScopeMode,
    /// Compact opaque scope identity encoded by the managed-cache directory.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub index_scope_digest: Option<String>,
    /// Recorded canonical repository root, when readable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repository_root: Option<PathBuf>,
    /// Whether the recorded repository root is currently reachable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repository_available: Option<bool>,
    /// Most recent known access time as Unix seconds.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_access_unix_seconds: Option<u64>,
    /// Provenance for `last_access_unix_seconds`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub access_time_source: Option<AccessTimeSource>,
    /// Age at inspection time.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub age_seconds: Option<u64>,
    /// SQLite schema recorded by the cache, when readable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub schema_version: Option<i64>,
    /// Bytes in direct managed cache artifacts.
    pub size_bytes: u64,
    /// Whether a lease-aware LeanToken process currently uses this cache.
    pub active: bool,
    /// Metadata and directory safety classification.
    pub state: CacheState,
    /// Local diagnostic when metadata could not be read completely.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// Complete report for `cache list`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CacheListReport {
    /// Platform-managed cache root inspected by the command.
    pub cache_root: PathBuf,
    /// Number of recognized caches before filters.
    pub total_entries: usize,
    /// Number of recognized caches after filters.
    pub matched_entries: usize,
    /// Number of entries included in this response page.
    pub returned_entries: usize,
    /// Sum of managed artifact bytes before filters.
    pub total_bytes: u64,
    /// Sum of managed artifact bytes after filters.
    pub matched_bytes: u64,
    /// Active leases among caches after filters.
    pub active_entries: usize,
    /// Recorded missing repository roots among caches after filters.
    pub missing_root_entries: usize,
    /// Counts by metadata state after filters.
    pub state_counts: BTreeMap<String, usize>,
    /// Entries ignored because their names are not managed cache identities.
    pub ignored_entries: usize,
    /// Whether the request omitted per-cache entries.
    pub summary_only: bool,
    /// Cursor for the next stable identifier page.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
    /// Stable cache entries sorted by identifier and bounded to one page.
    pub entries: Vec<CacheEntry>,
}

/// Entry with explicit content compatibility for the versioned list report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CacheEntryV2 {
    /// Existing auditable metadata fields, including the legacy `state` field.
    #[serde(flatten)]
    pub entry: CacheEntry,
    /// Content compatibility independent from metadata/access state.
    pub compatibility: CacheCompatibility,
}

/// Aggregate entry and byte counts for one compatibility class.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct CacheCompatibilitySummary {
    /// Number of matched entries in this class.
    pub entries: usize,
    /// Managed artifact bytes in this class.
    pub bytes: u64,
}

/// Versioned `cache list` report with content-compatibility diagnostics.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CacheListV2Report {
    /// Report schema version.
    pub report_version: u32,
    /// Platform-managed cache root inspected by the command.
    pub cache_root: PathBuf,
    /// Number of recognized caches before filters.
    pub total_entries: usize,
    /// Number of recognized caches after filters.
    pub matched_entries: usize,
    /// Number of entries included in this response page.
    pub returned_entries: usize,
    /// Sum of managed artifact bytes before filters.
    pub total_bytes: u64,
    /// Sum of managed artifact bytes after filters.
    pub matched_bytes: u64,
    /// Active leases among caches after filters.
    pub active_entries: usize,
    /// Recorded missing repository roots among caches after filters.
    pub missing_root_entries: usize,
    /// Counts by the existing metadata/access state after filters.
    pub state_counts: BTreeMap<String, usize>,
    /// Entry and byte totals by content compatibility after filters.
    pub compatibility_counts: BTreeMap<String, CacheCompatibilitySummary>,
    /// Inactive older or legacy entries whose metadata is safe to prune.
    pub safely_reclaimable_incompatible_entries: usize,
    /// Bytes in safely reclaimable incompatible entries.
    pub safely_reclaimable_incompatible_bytes: u64,
    /// Entries ignored because their names are not managed cache identities.
    pub ignored_entries: usize,
    /// Whether the request omitted per-cache entries.
    pub summary_only: bool,
    /// Cursor for the next stable identifier page.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
    /// Stable cache entries sorted by identifier and bounded to one page.
    pub entries: Vec<CacheEntryV2>,
}

/// Result action for one cache considered by prune.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CachePruneAction {
    /// No configured criterion selected this entry.
    Kept,
    /// Dry-run would delete this entry.
    WouldDelete,
    /// Managed artifacts were deleted.
    Deleted,
    /// A live process held the cache lease.
    SkippedActive,
    /// Unsupported metadata or unexpected content prevented automatic deletion.
    SkippedUnsafe,
    /// Filesystem deletion failed.
    Failed,
}

/// Auditable prune decision for one cache.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CachePruneResult {
    /// Managed cache identifier.
    pub id: String,
    /// Managed cache directory.
    pub path: PathBuf,
    /// Decision outcome.
    pub action: CachePruneAction,
    /// Selection reasons such as age, missing root, or total-byte budget.
    pub reasons: Vec<String>,
    /// Bytes associated with the entry at decision time.
    pub size_bytes: u64,
    /// Explanation for a skipped entry.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// Failure detail for a failed deletion.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Complete report for `cache prune`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CachePruneReport {
    /// Platform-managed cache root inspected by the command.
    pub cache_root: PathBuf,
    /// Whether no files were changed.
    pub dry_run: bool,
    /// Managed bytes before pruning.
    pub total_bytes_before: u64,
    /// Actual or projected managed bytes after pruning.
    pub total_bytes_after: u64,
    /// Actual or projected reclaimed bytes.
    pub reclaimed_bytes: u64,
    /// Stable per-entry decisions.
    pub results: Vec<CachePruneResult>,
}

impl CachePruneReport {
    /// Return true when one or more selected entries could not be deleted.
    #[must_use]
    pub fn has_failures(&self) -> bool {
        self.results
            .iter()
            .any(|result| result.action == CachePruneAction::Failed)
    }
}

impl CacheState {
    pub(super) const ALL: [Self; 6] = [
        Self::Current,
        Self::Legacy,
        Self::Incomplete,
        Self::Corrupt,
        Self::Unsupported,
        Self::Unrecognized,
    ];

    pub(super) fn label(self) -> &'static str {
        match self {
            Self::Current => "current",
            Self::Legacy => "legacy",
            Self::Incomplete => "incomplete",
            Self::Corrupt => "corrupt",
            Self::Unsupported => "unsupported",
            Self::Unrecognized => "unrecognized",
        }
    }
}

impl CacheCompatibility {
    pub(super) const ALL: [Self; 5] = [
        Self::CompatibleCurrent,
        Self::ObsoleteOlder,
        Self::LegacyUnversioned,
        Self::NewerUnsupported,
        Self::Unknown,
    ];

    pub(super) fn classify(entry: &CacheEntry) -> Self {
        if matches!(entry.state, CacheState::Corrupt | CacheState::Unrecognized) {
            return Self::Unknown;
        }
        match entry.index_content_version {
            Some(version) if version == INDEX_CONTENT_VERSION => Self::CompatibleCurrent,
            Some(version) if version < INDEX_CONTENT_VERSION => Self::ObsoleteOlder,
            Some(_) => Self::NewerUnsupported,
            None => Self::LegacyUnversioned,
        }
    }

    pub(super) fn label(self) -> &'static str {
        match self {
            Self::CompatibleCurrent => "compatible_current",
            Self::ObsoleteOlder => "obsolete_older",
            Self::LegacyUnversioned => "legacy_unversioned",
            Self::NewerUnsupported => "newer_unsupported",
            Self::Unknown => "unknown",
        }
    }

    pub(super) fn safely_incompatible(self) -> bool {
        matches!(self, Self::ObsoleteOlder | Self::LegacyUnversioned)
    }
}

impl CachePruneAction {
    pub(super) fn label(self) -> &'static str {
        match self {
            Self::Kept => "kept",
            Self::WouldDelete => "would_delete",
            Self::Deleted => "deleted",
            Self::SkippedActive => "skipped_active",
            Self::SkippedUnsafe => "skipped_unsafe",
            Self::Failed => "failed",
        }
    }
}

#[derive(Debug)]
pub(super) struct CacheManager {
    pub(super) root: PathBuf,
    pub(super) now: u64,
}

#[derive(Debug)]
pub(super) struct InspectedCache {
    pub(super) entry: CacheEntry,
    pub(super) compatibility: CacheCompatibility,
    pub(super) safe_to_prune: bool,
}

#[derive(Debug)]
pub(super) struct ArtifactScan {
    pub(super) size_bytes: u64,
    pub(super) latest_access_mtime: Option<u64>,
    pub(super) has_artifacts: bool,
    pub(super) unexpected: bool,
}
