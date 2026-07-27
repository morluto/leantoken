//! Explicit inspection and pruning of centrally managed repository caches.

use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::OsStr,
    fs,
    io::Write,
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use rusqlite::{Connection, OpenFlags};
use serde::Serialize;

use crate::config::{
    INDEX_CONTENT_VERSION, ManagedCacheIdentity, managed_cache_id_matches_root, managed_cache_root,
    parse_managed_cache_id,
};
use crate::coordination::{
    COORDINATION_LOCK_SUFFIXES, DEFAULT_INDEX_DATABASE_NAME, IndexCoordination, LEASE_LOCK_SUFFIX,
    coordination_sidecar_path, is_coordination_sidecar_for_database,
};
use crate::storage::{CURRENT_MIGRATION_VERSION, CURRENT_SCHEMA_VERSION};
use crate::{Error, Result};

const DATABASE_NAME: &str = DEFAULT_INDEX_DATABASE_NAME;
const WAL_NAME: &str = "index.sqlite-wal";
const PRUNABLE_ARTIFACTS: &[&str] = &[
    DATABASE_NAME,
    WAL_NAME,
    "index.sqlite-shm",
    "index.sqlite-journal",
];
const SECONDS_PER_DAY: u64 = 24 * 60 * 60;
const CACHE_LIST_CURSOR_PREFIX: &str = "cl1";
const CACHE_LIST_V2_CURSOR_PREFIX: &str = "cl2";
const CACHE_LIST_CURSOR_HASH_CHARS: usize = 16;
const MAX_CACHE_LIST_CURSOR_BYTES: usize = 128;
const MAX_CACHE_COMPATIBILITY_FILTERS: usize = 5;
const MAX_CACHE_CONTENT_VERSION_FILTERS: usize = 32;

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
    /// Stable directory identifier derived from the content version and repository root.
    pub id: String,
    /// Managed cache directory.
    pub path: PathBuf,
    /// Index-content compatibility version encoded by this cache identity.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub index_content_version: Option<u32>,
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
    const ALL: [Self; 6] = [
        Self::Current,
        Self::Legacy,
        Self::Incomplete,
        Self::Corrupt,
        Self::Unsupported,
        Self::Unrecognized,
    ];

    fn label(self) -> &'static str {
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
    const ALL: [Self; 5] = [
        Self::CompatibleCurrent,
        Self::ObsoleteOlder,
        Self::LegacyUnversioned,
        Self::NewerUnsupported,
        Self::Unknown,
    ];

    fn classify(entry: &CacheEntry) -> Self {
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

    fn label(self) -> &'static str {
        match self {
            Self::CompatibleCurrent => "compatible_current",
            Self::ObsoleteOlder => "obsolete_older",
            Self::LegacyUnversioned => "legacy_unversioned",
            Self::NewerUnsupported => "newer_unsupported",
            Self::Unknown => "unknown",
        }
    }

    fn safely_incompatible(self) -> bool {
        matches!(self, Self::ObsoleteOlder | Self::LegacyUnversioned)
    }
}

impl CachePruneAction {
    fn label(self) -> &'static str {
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
struct CacheManager {
    root: PathBuf,
    now: u64,
}

#[derive(Debug)]
struct InspectedCache {
    entry: CacheEntry,
    compatibility: CacheCompatibility,
    safe_to_prune: bool,
}

#[derive(Debug)]
struct ArtifactScan {
    size_bytes: u64,
    latest_access_mtime: Option<u64>,
    has_artifacts: bool,
    unexpected: bool,
}

/// List the first bounded page of centrally managed caches for the current user.
pub fn list() -> Result<CacheListReport> {
    list_with(&CacheListRequest::default())
}

/// List centrally managed caches using explicit filters and response bounds.
pub fn list_with(request: &CacheListRequest) -> Result<CacheListReport> {
    CacheManager::for_current_user()?.list_with(request)
}

/// List managed caches with explicit content-compatibility diagnostics.
pub fn list_v2_with(request: &CacheListV2Request) -> Result<CacheListV2Report> {
    CacheManager::for_current_user()?.list_v2_with(request)
}

/// Prune centrally managed repository caches using explicit criteria.
pub fn prune(request: &CachePruneRequest) -> Result<CachePruneReport> {
    CacheManager::for_current_user()?.prune(request)
}

/// Prune caches with versioned compatibility criteria.
pub fn prune_v2(request: &CachePruneV2Request) -> Result<CachePruneReport> {
    CacheManager::for_current_user()?.prune_v2(request)
}

impl CacheManager {
    fn for_current_user() -> Result<Self> {
        let root = managed_cache_root().ok_or_else(|| {
            Error::InvalidConfiguration(
                "this platform does not provide a central managed cache directory".into(),
            )
        })?;
        Ok(Self::new(root, unix_seconds(SystemTime::now())))
    }

    fn new(root: PathBuf, now: u64) -> Self {
        Self { root, now }
    }

    #[cfg(test)]
    fn list(&self) -> Result<CacheListReport> {
        self.list_with(&CacheListRequest::default())
    }

    fn list_with(&self, request: &CacheListRequest) -> Result<CacheListReport> {
        validate_list_request(request)?;
        let repository_root = request
            .repository_root
            .as_deref()
            .map(normalize_repository_root_filter);
        let filter_hash = cache_list_filter_hash(request, repository_root.as_deref());
        let after_id = request
            .cursor
            .as_deref()
            .map(|cursor| decode_cache_list_cursor(cursor, &filter_hash))
            .transpose()?;

        let (entries, ignored_entries) = self.inspect_all()?;
        let total_bytes = entries.iter().fold(0u64, |total, cache| {
            total.saturating_add(cache.entry.size_bytes)
        });
        let matching = entries
            .iter()
            .filter(|cache| {
                (request.states.is_empty() || request.states.contains(&cache.entry.state))
                    && repository_root
                        .as_ref()
                        .is_none_or(|root| cache.entry.repository_root.as_ref() == Some(root))
            })
            .collect::<Vec<_>>();
        let matched_bytes = matching.iter().fold(0u64, |total, cache| {
            total.saturating_add(cache.entry.size_bytes)
        });
        let active_entries = matching.iter().filter(|cache| cache.entry.active).count();
        let missing_root_entries = matching
            .iter()
            .filter(|cache| cache.entry.repository_available == Some(false))
            .count();
        let mut state_counts = CacheState::ALL
            .into_iter()
            .map(|state| (state.label().to_owned(), 0usize))
            .collect::<BTreeMap<_, _>>();
        for cache in &matching {
            *state_counts
                .get_mut(cache.entry.state.label())
                .expect("every cache state has a summary bucket") += 1;
        }

        let start = after_id.as_deref().map_or(0, |after_id| {
            matching.partition_point(|cache| cache.entry.id.as_str() <= after_id)
        });
        let end = if request.summary {
            start
        } else {
            start.saturating_add(request.limit).min(matching.len())
        };
        let page = matching[start..end]
            .iter()
            .map(|cache| cache.entry.clone())
            .collect::<Vec<_>>();
        let next_cursor = if !request.summary && end < matching.len() {
            page.last()
                .map(|entry| encode_cache_list_cursor(&filter_hash, &entry.id))
        } else {
            None
        };
        Ok(CacheListReport {
            cache_root: self.root.clone(),
            total_entries: entries.len(),
            matched_entries: matching.len(),
            returned_entries: page.len(),
            total_bytes,
            matched_bytes,
            active_entries,
            missing_root_entries,
            state_counts,
            ignored_entries,
            summary_only: request.summary,
            next_cursor,
            entries: page,
        })
    }

    fn list_v2_with(&self, request: &CacheListV2Request) -> Result<CacheListV2Report> {
        validate_list_request(&request.request)?;
        if request.compatibilities.len() > MAX_CACHE_COMPATIBILITY_FILTERS {
            return Err(Error::RequestLimitExceeded {
                field: "cache compatibility filters",
                requested: request.compatibilities.len(),
                limit: MAX_CACHE_COMPATIBILITY_FILTERS,
            });
        }
        if request.index_content_versions.len() > MAX_CACHE_CONTENT_VERSION_FILTERS {
            return Err(Error::RequestLimitExceeded {
                field: "cache content-version filters",
                requested: request.index_content_versions.len(),
                limit: MAX_CACHE_CONTENT_VERSION_FILTERS,
            });
        }
        if request.index_content_versions.contains(&0) {
            return Err(Error::InvalidInput {
                field: "cache content-version filter",
                reason: "must be positive",
            });
        }
        let repository_root = request
            .request
            .repository_root
            .as_deref()
            .map(normalize_repository_root_filter);
        let filter_hash = cache_list_v2_filter_hash(request, repository_root.as_deref());
        let after_id = request
            .request
            .cursor
            .as_deref()
            .map(|cursor| {
                decode_cache_list_cursor_with_prefix(
                    cursor,
                    CACHE_LIST_V2_CURSOR_PREFIX,
                    &filter_hash,
                )
            })
            .transpose()?;

        let (entries, ignored_entries) = self.inspect_all()?;
        let total_bytes = entries.iter().fold(0u64, |total, cache| {
            total.saturating_add(cache.entry.size_bytes)
        });
        let matching = entries
            .iter()
            .filter(|cache| {
                (request.request.states.is_empty()
                    || request.request.states.contains(&cache.entry.state))
                    && repository_root
                        .as_ref()
                        .is_none_or(|root| cache.entry.repository_root.as_ref() == Some(root))
                    && (request.compatibilities.is_empty()
                        || request.compatibilities.contains(&cache.compatibility))
                    && (request.index_content_versions.is_empty()
                        || cache.entry.index_content_version.is_some_and(|version| {
                            request.index_content_versions.contains(&version)
                        }))
                    && (!request.incompatible_with_current
                        || cache.compatibility.safely_incompatible())
            })
            .collect::<Vec<_>>();
        let matched_bytes = matching.iter().fold(0u64, |total, cache| {
            total.saturating_add(cache.entry.size_bytes)
        });
        let active_entries = matching.iter().filter(|cache| cache.entry.active).count();
        let missing_root_entries = matching
            .iter()
            .filter(|cache| cache.entry.repository_available == Some(false))
            .count();
        let mut state_counts = CacheState::ALL
            .into_iter()
            .map(|state| (state.label().to_owned(), 0usize))
            .collect::<BTreeMap<_, _>>();
        let mut compatibility_counts = CacheCompatibility::ALL
            .into_iter()
            .map(|compatibility| {
                (
                    compatibility.label().to_owned(),
                    CacheCompatibilitySummary::default(),
                )
            })
            .collect::<BTreeMap<_, _>>();
        let mut safely_reclaimable_incompatible_entries = 0usize;
        let mut safely_reclaimable_incompatible_bytes = 0u64;
        for cache in &matching {
            *state_counts
                .get_mut(cache.entry.state.label())
                .expect("every cache state has a summary bucket") += 1;
            let summary = compatibility_counts
                .get_mut(cache.compatibility.label())
                .expect("every compatibility has a summary bucket");
            summary.entries = summary.entries.saturating_add(1);
            summary.bytes = summary.bytes.saturating_add(cache.entry.size_bytes);
            if cache.compatibility.safely_incompatible()
                && cache.safe_to_prune
                && !cache.entry.active
            {
                safely_reclaimable_incompatible_entries =
                    safely_reclaimable_incompatible_entries.saturating_add(1);
                safely_reclaimable_incompatible_bytes =
                    safely_reclaimable_incompatible_bytes.saturating_add(cache.entry.size_bytes);
            }
        }

        let start = after_id.as_deref().map_or(0, |after_id| {
            matching.partition_point(|cache| cache.entry.id.as_str() <= after_id)
        });
        let end = if request.request.summary {
            start
        } else {
            start
                .saturating_add(request.request.limit)
                .min(matching.len())
        };
        let page = matching[start..end]
            .iter()
            .map(|cache| CacheEntryV2 {
                entry: cache.entry.clone(),
                compatibility: cache.compatibility,
            })
            .collect::<Vec<_>>();
        let next_cursor = if !request.request.summary && end < matching.len() {
            page.last().map(|entry| {
                encode_cache_list_cursor_with_prefix(
                    CACHE_LIST_V2_CURSOR_PREFIX,
                    &filter_hash,
                    &entry.entry.id,
                )
            })
        } else {
            None
        };
        Ok(CacheListV2Report {
            report_version: 2,
            cache_root: self.root.clone(),
            total_entries: entries.len(),
            matched_entries: matching.len(),
            returned_entries: page.len(),
            total_bytes,
            matched_bytes,
            active_entries,
            missing_root_entries,
            state_counts,
            compatibility_counts,
            safely_reclaimable_incompatible_entries,
            safely_reclaimable_incompatible_bytes,
            ignored_entries,
            summary_only: request.request.summary,
            next_cursor,
            entries: page,
        })
    }

    fn prune(&self, request: &CachePruneRequest) -> Result<CachePruneReport> {
        self.prune_with_compatibility(request, false)
    }

    fn prune_v2(&self, request: &CachePruneV2Request) -> Result<CachePruneReport> {
        self.prune_with_compatibility(&request.request, request.incompatible_with_current)
    }

    fn prune_with_compatibility(
        &self,
        request: &CachePruneRequest,
        incompatible_with_current: bool,
    ) -> Result<CachePruneReport> {
        validate_prune_request(request, incompatible_with_current)?;
        let (entries, _) = self.inspect_all()?;
        let total_bytes_before = entries.iter().fold(0u64, |total, cache| {
            total.saturating_add(cache.entry.size_bytes)
        });
        let selected = select_prune_candidates(
            &entries,
            request,
            total_bytes_before,
            incompatible_with_current,
        );
        let mut reclaimed_bytes = 0u64;
        let mut results = Vec::with_capacity(entries.len());

        for cache in entries {
            let Some(mut reasons) = selected.get(&cache.entry.id).cloned() else {
                results.push(prune_result(
                    &cache,
                    CachePruneAction::Kept,
                    Vec::new(),
                    None,
                ));
                continue;
            };
            if cache.entry.active {
                results.push(prune_result(
                    &cache,
                    CachePruneAction::SkippedActive,
                    reasons,
                    None,
                ));
                continue;
            }
            if !cache.safe_to_prune {
                results.push(prune_result(
                    &cache,
                    CachePruneAction::SkippedUnsafe,
                    reasons,
                    None,
                ));
                continue;
            }
            if request.dry_run {
                reclaimed_bytes = reclaimed_bytes.saturating_add(cache.entry.size_bytes);
                results.push(prune_result(
                    &cache,
                    CachePruneAction::WouldDelete,
                    reasons,
                    None,
                ));
                continue;
            }

            let database = cache.entry.path.join(DATABASE_NAME);
            let coordination = IndexCoordination::for_database(&database);
            let _lease = match coordination.try_acquire_prune_lease() {
                Ok(Some(lease)) => lease,
                Ok(None) => {
                    reasons.push("prune_lease_unavailable".into());
                    results.push(prune_result(
                        &cache,
                        CachePruneAction::SkippedActive,
                        reasons,
                        None,
                    ));
                    continue;
                }
                Err(error) => {
                    results.push(prune_result(
                        &cache,
                        CachePruneAction::Failed,
                        reasons,
                        Some(error.to_string()),
                    ));
                    continue;
                }
            };
            let current = match self.inspect_cache(&cache.entry.id, false) {
                Ok(current) => current,
                Err(error) => {
                    results.push(prune_result(
                        &cache,
                        CachePruneAction::Failed,
                        reasons,
                        Some(error.to_string()),
                    ));
                    continue;
                }
            };
            let selected_for_compatibility = reasons
                .iter()
                .any(|reason| reason.starts_with("incompatible_with_current:"));
            if selected_for_compatibility && !current.compatibility.safely_incompatible() {
                reasons.retain(|reason| !reason.starts_with("incompatible_with_current:"));
                if reasons.is_empty() {
                    reasons.push(format!(
                        "incompatible_with_current_revalidated:{}",
                        current.compatibility.label()
                    ));
                    results.push(prune_result(
                        &current,
                        CachePruneAction::Kept,
                        reasons,
                        None,
                    ));
                    continue;
                }
            }
            if !current.safe_to_prune {
                results.push(prune_result(
                    &current,
                    CachePruneAction::SkippedUnsafe,
                    reasons,
                    None,
                ));
                continue;
            }
            if reasons.len() == 1
                && reasons[0] == "missing_repository"
                && current.entry.repository_available != Some(false)
            {
                results.push(prune_result(
                    &current,
                    CachePruneAction::Kept,
                    reasons,
                    None,
                ));
                continue;
            }

            let removal = remove_managed_artifacts(&current.entry.path);
            reclaimed_bytes = reclaimed_bytes.saturating_add(removal.reclaimed_bytes);
            match removal.error {
                None => {
                    results.push(prune_result(
                        &current,
                        CachePruneAction::Deleted,
                        reasons,
                        None,
                    ));
                }
                Some(error) => results.push(prune_result(
                    &current,
                    CachePruneAction::Failed,
                    reasons,
                    Some(error),
                )),
            }
        }

        results.sort_by(|left, right| left.id.cmp(&right.id));
        Ok(CachePruneReport {
            cache_root: self.root.clone(),
            dry_run: request.dry_run,
            total_bytes_before,
            total_bytes_after: total_bytes_before.saturating_sub(reclaimed_bytes),
            reclaimed_bytes,
            results,
        })
    }

    fn inspect_all(&self) -> Result<(Vec<InspectedCache>, usize)> {
        let read_dir = match fs::read_dir(&self.root) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok((Vec::new(), 0));
            }
            Err(error) => return Err(error.into()),
        };
        let mut entries = Vec::new();
        let mut ignored = 0usize;
        for entry in read_dir {
            let entry = entry?;
            let file_type = entry.file_type()?;
            let Some(id) = entry.file_name().to_str().map(str::to_owned) else {
                ignored += 1;
                continue;
            };
            if !file_type.is_dir() || !is_cache_id(&id) {
                ignored += 1;
                continue;
            }
            let cache = self.inspect_cache(&id, true)?;
            if cache.entry.size_bytes == 0 && cache.entry.state == CacheState::Incomplete {
                continue;
            }
            entries.push(cache);
        }
        entries.sort_by(|left, right| left.entry.id.cmp(&right.entry.id));
        Ok((entries, ignored))
    }

    fn inspect_cache(&self, id: &str, probe_active: bool) -> Result<InspectedCache> {
        let path = self.root.join(id);
        let database = path.join(DATABASE_NAME);
        let identity = parse_managed_cache_id(id).expect("validated managed cache identity");
        let index_content_version = match identity {
            ManagedCacheIdentity::Legacy => None,
            ManagedCacheIdentity::Versioned(version) => Some(version),
        };
        let initial_scan = scan_artifacts(&path)?;
        let latest_access_mtime = initial_scan.latest_access_mtime;
        let mut unexpected = initial_scan.unexpected;
        let mut metadata_safe = true;

        let lease_path = coordination_sidecar_path(&database, LEASE_LOCK_SUFFIX);
        let active = if probe_active && lease_path.exists() {
            IndexCoordination::for_database(&database)
                .try_acquire_prune_lease()?
                .is_none()
        } else {
            false
        };
        let mut entry = CacheEntry {
            id: id.into(),
            path,
            index_content_version,
            repository_root: None,
            repository_available: None,
            last_access_unix_seconds: latest_access_mtime,
            access_time_source: latest_access_mtime.map(|_| AccessTimeSource::FileMtime),
            age_seconds: latest_access_mtime.map(|accessed| self.now.saturating_sub(accessed)),
            schema_version: None,
            size_bytes: initial_scan.size_bytes,
            active,
            state: CacheState::Incomplete,
            detail: None,
        };

        let database_is_regular =
            fs::symlink_metadata(&database).is_ok_and(|metadata| metadata.file_type().is_file());
        if initial_scan.has_artifacts && database_is_regular {
            match inspect_database(&database) {
                Ok(metadata) => {
                    entry.schema_version = metadata.schema_version;
                    entry.repository_root = metadata.repository_root;
                    entry.repository_available =
                        entry.repository_root.as_deref().and_then(root_available);
                    if let Some(accessed) = metadata.last_access_unix_seconds {
                        entry.last_access_unix_seconds = Some(accessed);
                        entry.access_time_source = Some(AccessTimeSource::Database);
                        entry.age_seconds = Some(self.now.saturating_sub(accessed));
                    }
                    entry.state = if metadata.future_schema {
                        metadata_safe = false;
                        entry.detail = Some("cache uses a newer unsupported schema".into());
                        CacheState::Unsupported
                    } else if metadata.current {
                        CacheState::Current
                    } else {
                        CacheState::Legacy
                    };
                    if let Some(repository_root) = &entry.repository_root
                        && !managed_cache_id_matches_root(id, repository_root)
                    {
                        metadata_safe = false;
                        entry.state = CacheState::Unsupported;
                        entry.detail =
                            Some("cache identity does not match its recorded root".into());
                    }
                }
                Err(error) => {
                    metadata_safe = false;
                    entry.state = CacheState::Corrupt;
                    entry.detail = Some(error.to_string());
                }
            }
        }
        if index_content_version.is_some_and(|version| version > INDEX_CONTENT_VERSION) {
            metadata_safe = false;
            entry.state = CacheState::Unsupported;
            entry.detail = Some("cache uses a newer index-content version".into());
        }
        let final_scan = scan_artifacts(&entry.path)?;
        entry.size_bytes = final_scan.size_bytes;
        unexpected |= final_scan.unexpected;
        if unexpected {
            entry.state = CacheState::Unrecognized;
            entry.detail = Some("cache directory contains unexpected entries".into());
        }
        let compatibility = CacheCompatibility::classify(&entry);

        Ok(InspectedCache {
            safe_to_prune: final_scan.has_artifacts && !unexpected && metadata_safe,
            entry,
            compatibility,
        })
    }
}

fn scan_artifacts(path: &Path) -> Result<ArtifactScan> {
    let mut scan = ArtifactScan {
        size_bytes: 0,
        latest_access_mtime: None,
        has_artifacts: false,
        unexpected: false,
    };
    let database = path.join(DATABASE_NAME);
    let lease_path = coordination_sidecar_path(&database, LEASE_LOCK_SUFFIX);
    for child in fs::read_dir(path)? {
        let child = child?;
        let metadata = fs::symlink_metadata(child.path())?;
        let child_path = child.path();
        let known = child
            .file_name()
            .to_str()
            .is_some_and(|name| PRUNABLE_ARTIFACTS.contains(&name))
            || is_coordination_sidecar_for_database(&child_path, &database);
        if !known || !metadata.file_type().is_file() {
            scan.unexpected = true;
            continue;
        }
        if child_path == lease_path {
            continue;
        }
        scan.has_artifacts = true;
        scan.size_bytes = scan.size_bytes.saturating_add(metadata.len());
        let name = child.file_name();
        // Read-only WAL inspection can refresh SHM and lock-file mtimes.
        if (name == OsStr::new(DATABASE_NAME) || name == OsStr::new(WAL_NAME))
            && let Ok(modified) = metadata.modified()
        {
            let modified = unix_seconds(modified);
            scan.latest_access_mtime = Some(
                scan.latest_access_mtime
                    .map_or(modified, |current| current.max(modified)),
            );
        }
    }
    Ok(scan)
}

#[derive(Debug)]
struct DatabaseMetadata {
    schema_version: Option<i64>,
    repository_root: Option<PathBuf>,
    last_access_unix_seconds: Option<u64>,
    current: bool,
    future_schema: bool,
}

fn inspect_database(path: &Path) -> Result<DatabaseMetadata> {
    let connection = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    connection.busy_timeout(Duration::from_millis(100))?;
    let migration_version =
        connection.pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))?;
    if migration_version > CURRENT_MIGRATION_VERSION {
        return Ok(DatabaseMetadata {
            schema_version: None,
            repository_root: None,
            last_access_unix_seconds: None,
            current: false,
            future_schema: true,
        });
    }
    let mut statement = connection.prepare("PRAGMA table_info(meta)")?;
    let columns = statement
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<std::result::Result<BTreeSet<_>, _>>()?;
    if !columns.contains("schema_version") {
        return Err(Error::InvalidConfiguration(
            "cache metadata table has no schema version".into(),
        ));
    }
    let schema_version =
        connection.query_row("SELECT schema_version FROM meta WHERE id = 1", [], |row| {
            row.get::<_, i64>(0)
        })?;
    if schema_version > CURRENT_SCHEMA_VERSION {
        return Ok(DatabaseMetadata {
            schema_version: Some(schema_version),
            repository_root: None,
            last_access_unix_seconds: None,
            current: false,
            future_schema: true,
        });
    }
    let repository_root = if columns.contains("repository_root") {
        let root =
            connection.query_row("SELECT repository_root FROM meta WHERE id = 1", [], |row| {
                row.get::<_, String>(0)
            })?;
        (!root.is_empty()).then(|| PathBuf::from(root))
    } else {
        None
    };
    let last_access_unix_seconds = if columns.contains("last_access_unix_seconds") {
        let accessed = connection.query_row(
            "SELECT last_access_unix_seconds FROM meta WHERE id = 1",
            [],
            |row| row.get::<_, i64>(0),
        )?;
        u64::try_from(accessed).ok().filter(|value| *value > 0)
    } else {
        None
    };
    Ok(DatabaseMetadata {
        schema_version: Some(schema_version),
        repository_root,
        last_access_unix_seconds,
        current: schema_version == CURRENT_SCHEMA_VERSION
            && columns.contains("last_access_unix_seconds"),
        future_schema: false,
    })
}

fn validate_list_request(request: &CacheListRequest) -> Result<()> {
    if request.limit == 0 {
        return Err(Error::InvalidInput {
            field: "cache list limit",
            reason: "must be greater than zero",
        });
    }
    if request.limit > MAX_CACHE_LIST_LIMIT {
        return Err(Error::RequestLimitExceeded {
            field: "cache list limit",
            requested: request.limit,
            limit: MAX_CACHE_LIST_LIMIT,
        });
    }
    if request.summary && request.cursor.is_some() {
        return Err(Error::InvalidInput {
            field: "cache list cursor",
            reason: "cannot be combined with summary mode",
        });
    }
    Ok(())
}

fn normalize_repository_root_filter(path: &Path) -> PathBuf {
    let absolute = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    absolute.canonicalize().unwrap_or(absolute)
}

fn cache_list_filter_hash(request: &CacheListRequest, repository_root: Option<&Path>) -> String {
    let mut hasher = blake3::Hasher::new();
    if request.states.is_empty() {
        hasher.update(b"all-states");
    } else {
        for state in CacheState::ALL {
            if request.states.contains(&state) {
                hasher.update(state.label().as_bytes());
                hasher.update(b"\0");
            }
        }
    }
    hasher.update(b"\xff");
    if let Some(root) = repository_root {
        hasher.update(root.as_os_str().as_encoded_bytes());
    }
    hasher.finalize().to_hex()[..CACHE_LIST_CURSOR_HASH_CHARS].to_owned()
}

fn cache_list_v2_filter_hash(
    request: &CacheListV2Request,
    repository_root: Option<&Path>,
) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(cache_list_filter_hash(&request.request, repository_root).as_bytes());
    hasher.update(b"\xffcompatibility\0");
    if request.compatibilities.is_empty() {
        hasher.update(b"all");
    } else {
        for compatibility in CacheCompatibility::ALL {
            if request.compatibilities.contains(&compatibility) {
                hasher.update(compatibility.label().as_bytes());
                hasher.update(b"\0");
            }
        }
    }
    hasher.update(b"\xffcontent-versions\0");
    let mut versions = request.index_content_versions.clone();
    versions.sort_unstable();
    versions.dedup();
    if versions.is_empty() {
        hasher.update(b"all");
    } else {
        for version in versions {
            hasher.update(&version.to_le_bytes());
        }
    }
    hasher.update(b"\xffincompatible\0");
    hasher.update(&[u8::from(request.incompatible_with_current)]);
    hasher.finalize().to_hex()[..CACHE_LIST_CURSOR_HASH_CHARS].to_owned()
}

fn encode_cache_list_cursor(filter_hash: &str, after_id: &str) -> String {
    encode_cache_list_cursor_with_prefix(CACHE_LIST_CURSOR_PREFIX, filter_hash, after_id)
}

fn encode_cache_list_cursor_with_prefix(prefix: &str, filter_hash: &str, after_id: &str) -> String {
    format!("{prefix}:{filter_hash}:{after_id}")
}

fn decode_cache_list_cursor(cursor: &str, expected_filter_hash: &str) -> Result<String> {
    decode_cache_list_cursor_with_prefix(cursor, CACHE_LIST_CURSOR_PREFIX, expected_filter_hash)
}

fn decode_cache_list_cursor_with_prefix(
    cursor: &str,
    expected_prefix: &str,
    expected_filter_hash: &str,
) -> Result<String> {
    if cursor.len() > MAX_CACHE_LIST_CURSOR_BYTES {
        return Err(Error::InputTooLong {
            field: "cache list cursor",
            max_bytes: MAX_CACHE_LIST_CURSOR_BYTES,
        });
    }
    let mut parts = cursor.splitn(3, ':');
    let prefix = parts.next();
    let filter_hash = parts.next();
    let after_id = parts.next();
    if prefix != Some(expected_prefix)
        || filter_hash.is_none_or(|hash| {
            hash.len() != CACHE_LIST_CURSOR_HASH_CHARS
                || !hash.bytes().all(|byte| byte.is_ascii_hexdigit())
        })
        || after_id.is_none_or(|id| !is_cache_id(id))
    {
        return Err(Error::InvalidInput {
            field: "cache list cursor",
            reason: "must be an opaque cursor returned by cache list",
        });
    }
    if filter_hash != Some(expected_filter_hash) {
        return Err(Error::InvalidInput {
            field: "cache list cursor",
            reason: "does not match the active cache filters",
        });
    }
    Ok(after_id.expect("validated cache cursor id").to_owned())
}

fn validate_prune_request(
    request: &CachePruneRequest,
    incompatible_with_current: bool,
) -> Result<()> {
    if request.older_than_days.is_none()
        && request.max_total_bytes.is_none()
        && !request.remove_missing_roots
        && !incompatible_with_current
    {
        return Err(Error::InvalidRequest(
            "cache prune requires --older-than, --max-total-bytes, \
             --remove-missing-roots, or --incompatible-with-current"
                .into(),
        ));
    }
    if request.older_than_days == Some(0) {
        return Err(Error::InvalidRequest(
            "--older-than must be at least one day".into(),
        ));
    }
    if !request.dry_run && !request.yes {
        return Err(Error::InvalidRequest(
            "cache prune requires --yes unless --dry-run is used".into(),
        ));
    }
    Ok(())
}

fn select_prune_candidates(
    entries: &[InspectedCache],
    request: &CachePruneRequest,
    total_bytes: u64,
    incompatible_with_current: bool,
) -> BTreeMap<String, Vec<String>> {
    let mut selected = BTreeMap::<String, Vec<String>>::new();
    let minimum_age = request
        .older_than_days
        .map(|days| days.saturating_mul(SECONDS_PER_DAY));
    for cache in entries {
        if incompatible_with_current && cache.compatibility.safely_incompatible() {
            selected
                .entry(cache.entry.id.clone())
                .or_default()
                .push(format!(
                    "incompatible_with_current:{}",
                    cache.compatibility.label()
                ));
        }
        if minimum_age.is_some_and(|age| cache.entry.age_seconds.is_some_and(|value| value >= age))
        {
            selected
                .entry(cache.entry.id.clone())
                .or_default()
                .push("older_than".into());
        }
        if request.remove_missing_roots && cache.entry.repository_available == Some(false) {
            selected
                .entry(cache.entry.id.clone())
                .or_default()
                .push("missing_repository".into());
        }
    }

    let Some(max_total_bytes) = request.max_total_bytes else {
        return selected;
    };
    let mut projected = total_bytes;
    for cache in entries {
        if selected.contains_key(&cache.entry.id) && cache.safe_to_prune && !cache.entry.active {
            projected = projected.saturating_sub(cache.entry.size_bytes);
        }
    }
    let mut lru = entries
        .iter()
        .filter(|cache| {
            !selected.contains_key(&cache.entry.id) && cache.safe_to_prune && !cache.entry.active
        })
        .collect::<Vec<_>>();
    lru.sort_by(|left, right| {
        left.entry
            .last_access_unix_seconds
            .unwrap_or(0)
            .cmp(&right.entry.last_access_unix_seconds.unwrap_or(0))
            .then_with(|| left.entry.id.cmp(&right.entry.id))
    });
    for cache in lru {
        if projected <= max_total_bytes {
            break;
        }
        selected
            .entry(cache.entry.id.clone())
            .or_default()
            .push("max_total_bytes".into());
        projected = projected.saturating_sub(cache.entry.size_bytes);
    }
    selected
}

fn prune_result(
    cache: &InspectedCache,
    action: CachePruneAction,
    reasons: Vec<String>,
    error: Option<String>,
) -> CachePruneResult {
    let detail = match action {
        CachePruneAction::SkippedActive => Some("cache lease is held by a running process".into()),
        CachePruneAction::SkippedUnsafe => cache
            .entry
            .detail
            .clone()
            .or_else(|| Some("cache metadata is not safe to prune".into())),
        _ => None,
    };
    CachePruneResult {
        id: cache.entry.id.clone(),
        path: cache.entry.path.clone(),
        action,
        reasons,
        size_bytes: cache.entry.size_bytes,
        detail,
        error,
    }
}

struct RemovalOutcome {
    reclaimed_bytes: u64,
    error: Option<String>,
}

fn remove_managed_artifacts(directory: &Path) -> RemovalOutcome {
    let mut reclaimed_bytes = 0u64;
    let database = directory.join(DATABASE_NAME);
    let paths = PRUNABLE_ARTIFACTS
        .iter()
        .map(|artifact| directory.join(artifact))
        .chain(
            COORDINATION_LOCK_SUFFIXES
                .into_iter()
                .filter(|suffix| *suffix != LEASE_LOCK_SUFFIX)
                .map(|suffix| coordination_sidecar_path(&database, suffix)),
        );
    for path in paths {
        let size = fs::symlink_metadata(&path)
            .map(|metadata| metadata.len())
            .unwrap_or(0);
        match fs::remove_file(path) {
            Ok(()) => reclaimed_bytes = reclaimed_bytes.saturating_add(size),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return RemovalOutcome {
                    reclaimed_bytes,
                    error: Some(error.to_string()),
                };
            }
        }
    }
    RemovalOutcome {
        reclaimed_bytes,
        error: None,
    }
}

fn is_cache_id(value: &str) -> bool {
    parse_managed_cache_id(value).is_some()
}

fn root_available(path: &Path) -> Option<bool> {
    match fs::metadata(path) {
        Ok(_) => Some(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Some(false),
        Err(_) => None,
    }
}

fn unix_seconds(time: SystemTime) -> u64 {
    time.duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs())
}

/// Print a cache-list report as JSON or concise human-readable output.
pub fn print_list(report: &CacheListReport, json_output: bool) -> Result<()> {
    let stdout = std::io::stdout();
    let mut output = stdout.lock();
    if json_output {
        serde_json::to_writer(&mut output, report)?;
        output.write_all(b"\n")?;
        return Ok(());
    }
    writeln!(
        output,
        "Managed cache root: {}",
        report.cache_root.display()
    )?;
    writeln!(
        output,
        "{} total cache(s), {} bytes; {} matched, {} bytes; {} returned",
        report.total_entries,
        report.total_bytes,
        report.matched_entries,
        report.matched_bytes,
        report.returned_entries
    )?;
    let state_counts = report
        .state_counts
        .iter()
        .map(|(state, count)| format!("{state}={count}"))
        .collect::<Vec<_>>()
        .join(" ");
    writeln!(
        output,
        "states: {state_counts}; active={}; missing_roots={}; ignored={}",
        report.active_entries, report.missing_root_entries, report.ignored_entries
    )?;
    for entry in &report.entries {
        writeln!(
            output,
            "{}  {} bytes  {}  {}  last_access={}  root_available={}  {}",
            entry.id,
            entry.size_bytes,
            if entry.active { "active" } else { "inactive" },
            entry.state.label(),
            entry
                .last_access_unix_seconds
                .map_or_else(|| "unknown".into(), |timestamp| timestamp.to_string()),
            entry
                .repository_available
                .map_or("unknown", |available| if available { "yes" } else { "no" }),
            entry
                .repository_root
                .as_deref()
                .map_or_else(|| "unknown root".into(), |root| root.display().to_string())
        )?;
    }
    if let Some(cursor) = &report.next_cursor {
        writeln!(output, "next_cursor={cursor}")?;
    }
    Ok(())
}

/// Print a versioned cache-list report as JSON or concise human-readable output.
pub fn print_list_v2(report: &CacheListV2Report, json_output: bool) -> Result<()> {
    let stdout = std::io::stdout();
    let mut output = stdout.lock();
    if json_output {
        serde_json::to_writer(&mut output, report)?;
        output.write_all(b"\n")?;
        return Ok(());
    }
    writeln!(
        output,
        "Managed cache root: {}",
        report.cache_root.display()
    )?;
    writeln!(
        output,
        "{} total cache(s), {} bytes; {} matched, {} bytes; {} returned",
        report.total_entries,
        report.total_bytes,
        report.matched_entries,
        report.matched_bytes,
        report.returned_entries
    )?;
    let state_counts = report
        .state_counts
        .iter()
        .map(|(state, count)| format!("{state}={count}"))
        .collect::<Vec<_>>()
        .join(" ");
    let compatibility_counts = report
        .compatibility_counts
        .iter()
        .map(|(compatibility, summary)| {
            format!("{compatibility}={}/{}B", summary.entries, summary.bytes)
        })
        .collect::<Vec<_>>()
        .join(" ");
    writeln!(
        output,
        "states: {state_counts}; active={}; missing_roots={}; ignored={}",
        report.active_entries, report.missing_root_entries, report.ignored_entries
    )?;
    writeln!(
        output,
        "compatibility: {compatibility_counts}; safely_reclaimable={}/{}B",
        report.safely_reclaimable_incompatible_entries,
        report.safely_reclaimable_incompatible_bytes
    )?;
    for entry in &report.entries {
        writeln!(
            output,
            "{}  {} bytes  {}  {}  {}  last_access={}  root_available={}  {}",
            entry.entry.id,
            entry.entry.size_bytes,
            if entry.entry.active {
                "active"
            } else {
                "inactive"
            },
            entry.entry.state.label(),
            entry.compatibility.label(),
            entry
                .entry
                .last_access_unix_seconds
                .map_or_else(|| "unknown".into(), |timestamp| timestamp.to_string()),
            entry
                .entry
                .repository_available
                .map_or("unknown", |available| if available { "yes" } else { "no" }),
            entry
                .entry
                .repository_root
                .as_deref()
                .map_or_else(|| "unknown root".into(), |root| root.display().to_string())
        )?;
    }
    if let Some(cursor) = &report.next_cursor {
        writeln!(output, "next_cursor={cursor}")?;
    }
    Ok(())
}

/// Print a cache-prune report as JSON or concise human-readable output.
pub fn print_prune(report: &CachePruneReport, json_output: bool) -> Result<()> {
    let stdout = std::io::stdout();
    let mut output = stdout.lock();
    if json_output {
        serde_json::to_writer(&mut output, report)?;
        output.write_all(b"\n")?;
        return Ok(());
    }
    writeln!(
        output,
        "Managed cache prune{}: {} -> {} bytes",
        if report.dry_run { " dry-run" } else { "" },
        report.total_bytes_before,
        report.total_bytes_after
    )?;
    for result in &report.results {
        let detail = result.error.as_deref().or(result.detail.as_deref());
        writeln!(
            output,
            "{}  {}  {} bytes{}{}",
            result.action.label(),
            result.id,
            result.size_bytes,
            if result.reasons.is_empty() {
                String::new()
            } else {
                format!("  {}", result.reasons.join(","))
            },
            detail.map_or_else(String::new, |detail| format!("  {detail}"))
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Config;
    use crate::config::managed_cache_id;
    use crate::services::Services;
    use crate::storage::Storage;

    const FIRST_ID: &str = "0000000000000001";
    const SECOND_ID: &str = "0000000000000002";

    fn request() -> CachePruneRequest {
        CachePruneRequest {
            older_than_days: None,
            max_total_bytes: None,
            remove_missing_roots: false,
            dry_run: true,
            yes: false,
        }
    }

    fn create_current_cache(
        manager: &CacheManager,
        repository: &Path,
        accessed_at: u64,
    ) -> (String, PathBuf) {
        let id = managed_cache_id(repository);
        let directory = manager.root.join(&id);
        fs::create_dir_all(&directory).expect("cache directory");
        let database = directory.join(DATABASE_NAME);
        drop(Storage::open_for_repository(&database, repository).expect("cache database"));
        Connection::open(&database)
            .expect("cache metadata")
            .execute(
                "UPDATE meta SET last_access_unix_seconds = ?1 WHERE id = 1",
                [i64::try_from(accessed_at).expect("test timestamp")],
            )
            .expect("access timestamp");
        (id, database)
    }

    fn create_cache_with_content_identity(
        manager: &CacheManager,
        repository: &Path,
        accessed_at: u64,
        version: Option<u32>,
    ) -> (String, PathBuf) {
        let (current_id, database) = create_current_cache(manager, repository, accessed_at);
        let root_hash = current_id
            .split_once('-')
            .expect("versioned cache identity")
            .1;
        let id = version.map_or_else(
            || root_hash.to_owned(),
            |version| format!("v{version}-{root_hash}"),
        );
        let directory = manager.root.join(&id);
        fs::rename(database.parent().expect("cache directory"), &directory)
            .expect("move cache identity");
        (id, directory.join(DATABASE_NAME))
    }

    fn create_legacy_wal_cache(manager: &CacheManager, id: &str, accessed_at: u64) {
        let directory = manager.root.join(id);
        fs::create_dir_all(&directory).expect("cache directory");
        let source_database = manager
            .root
            .parent()
            .expect("managed cache parent")
            .join("legacy-source.sqlite");
        let connection = Connection::open(&source_database).expect("legacy database");
        connection
            .execute_batch(
                "PRAGMA journal_mode = WAL;
                 PRAGMA wal_autocheckpoint = 0;
                 CREATE TABLE meta (
                     id INTEGER PRIMARY KEY,
                     schema_version INTEGER NOT NULL,
                     repository_root TEXT NOT NULL
                 );
                 INSERT INTO meta VALUES (1, 4, '');",
            )
            .expect("legacy WAL schema");

        for name in [DATABASE_NAME, WAL_NAME, "index.sqlite-shm"] {
            let source = if name == DATABASE_NAME {
                source_database.clone()
            } else {
                source_database.with_file_name(format!(
                    "{}{}",
                    source_database
                        .file_name()
                        .expect("source database name")
                        .to_string_lossy(),
                    &name[DATABASE_NAME.len()..]
                ))
            };
            fs::copy(source, directory.join(name)).expect("copy WAL artifact");
        }
        drop(connection);

        let modified = UNIX_EPOCH + Duration::from_secs(accessed_at);
        for name in [DATABASE_NAME, WAL_NAME, "index.sqlite-shm"] {
            let artifact = directory.join(name);
            assert!(
                artifact.exists(),
                "missing WAL artifact {}",
                artifact.display()
            );
            fs::File::options()
                .read(true)
                .write(true)
                .open(&artifact)
                .expect("open WAL artifact")
                .set_times(fs::FileTimes::new().set_modified(modified))
                .expect("set WAL artifact mtime");
        }
    }

    #[test]
    fn list_reports_current_metadata_and_ignores_non_cache_directories() {
        let temp = tempfile::tempdir().expect("temporary directory");
        let root = temp.path().join("managed");
        let repository = temp.path().join("repository");
        fs::create_dir(&repository).expect("repository");
        let manager = CacheManager::new(root.clone(), 10_000);
        create_current_cache(&manager, &repository, 9_000);
        fs::create_dir_all(root.join("not-managed")).expect("unmanaged directory");

        let report = manager.list().expect("cache list");

        assert_eq!(report.entries.len(), 1);
        assert_eq!(report.total_entries, 1);
        assert_eq!(report.matched_entries, 1);
        assert_eq!(report.returned_entries, 1);
        assert_eq!(report.ignored_entries, 1);
        assert_eq!(report.active_entries, 0);
        assert_eq!(report.missing_root_entries, 0);
        assert_eq!(report.state_counts["current"], 1);
        assert!(!report.summary_only);
        assert!(report.next_cursor.is_none());
        assert_eq!(report.entries[0].state, CacheState::Current);
        assert_eq!(
            report.entries[0].index_content_version,
            Some(INDEX_CONTENT_VERSION)
        );
        assert_eq!(
            report.entries[0].repository_root.as_deref(),
            Some(repository.as_path())
        );
        assert_eq!(report.entries[0].repository_available, Some(true));
        assert_eq!(report.entries[0].last_access_unix_seconds, Some(9_000));
        assert_eq!(report.entries[0].age_seconds, Some(1_000));
        assert_eq!(
            report.entries[0].access_time_source,
            Some(AccessTimeSource::Database)
        );
        assert!(report.total_bytes > 0);
        assert_eq!(report.matched_bytes, report.total_bytes);
    }

    #[test]
    fn list_v2_separates_metadata_state_from_content_compatibility() {
        let temp = tempfile::tempdir().expect("temporary directory");
        let manager = CacheManager::new(temp.path().join("managed"), 10_000);
        let repositories = (0..4)
            .map(|index| {
                let repository = temp.path().join(format!("repository-{index}"));
                fs::create_dir(&repository).expect("repository");
                repository
            })
            .collect::<Vec<_>>();
        let (current_id, _) = create_current_cache(&manager, &repositories[0], 9_000);
        let (older_id, _) = create_cache_with_content_identity(
            &manager,
            &repositories[1],
            8_000,
            Some(INDEX_CONTENT_VERSION - 1),
        );
        let (legacy_id, _) =
            create_cache_with_content_identity(&manager, &repositories[2], 7_000, None);
        let (future_id, _) = create_cache_with_content_identity(
            &manager,
            &repositories[3],
            6_000,
            Some(INDEX_CONTENT_VERSION + 1),
        );
        let corrupt_id = FIRST_ID;
        let corrupt = manager.root.join(corrupt_id);
        fs::create_dir_all(&corrupt).expect("corrupt cache directory");
        fs::write(corrupt.join(DATABASE_NAME), b"not sqlite").expect("corrupt database");

        let report = manager
            .list_v2_with(&CacheListV2Request::default())
            .expect("versioned cache list");

        assert_eq!(report.report_version, 2);
        assert_eq!(report.total_entries, 5);
        assert_eq!(report.state_counts["current"], 3);
        assert_eq!(report.state_counts["unsupported"], 1);
        assert_eq!(report.state_counts["corrupt"], 1);
        for compatibility in CacheCompatibility::ALL {
            assert_eq!(
                report.compatibility_counts[compatibility.label()].entries,
                1,
                "{compatibility:?}"
            );
        }
        assert_eq!(report.safely_reclaimable_incompatible_entries, 2);
        assert!(report.safely_reclaimable_incompatible_bytes > 0);
        let project = |id: &str| {
            report
                .entries
                .iter()
                .find(|entry| entry.entry.id == id)
                .map(|entry| (entry.entry.state, entry.compatibility))
                .expect("listed cache")
        };
        assert_eq!(
            project(&current_id),
            (CacheState::Current, CacheCompatibility::CompatibleCurrent)
        );
        assert_eq!(
            project(&older_id),
            (CacheState::Current, CacheCompatibility::ObsoleteOlder)
        );
        assert_eq!(
            project(&legacy_id),
            (CacheState::Current, CacheCompatibility::LegacyUnversioned)
        );
        assert_eq!(
            project(&future_id),
            (
                CacheState::Unsupported,
                CacheCompatibility::NewerUnsupported
            )
        );
        assert_eq!(
            project(corrupt_id),
            (CacheState::Corrupt, CacheCompatibility::Unknown)
        );
        let serialized = serde_json::to_value(&report).expect("serialize cache report");
        assert!(
            serialized["entries"]
                .as_array()
                .expect("entries")
                .iter()
                .any(|entry| {
                    entry["id"] == legacy_id
                        && entry["state"] == "current"
                        && entry["compatibility"] == "legacy_unversioned"
                })
        );
    }

    #[test]
    fn list_v2_filters_and_cursors_bind_every_compatibility_dimension() {
        let temp = tempfile::tempdir().expect("temporary directory");
        let manager = CacheManager::new(temp.path().join("managed"), 10_000);
        for (index, version) in [
            INDEX_CONTENT_VERSION - 1,
            INDEX_CONTENT_VERSION - 2,
            INDEX_CONTENT_VERSION,
        ]
        .into_iter()
        .enumerate()
        {
            let repository = temp.path().join(format!("repository-{index}"));
            fs::create_dir(&repository).expect("repository");
            if version == INDEX_CONTENT_VERSION {
                create_current_cache(&manager, &repository, 9_000);
            } else {
                create_cache_with_content_identity(&manager, &repository, 9_000, Some(version));
            }
        }

        let first_request = CacheListV2Request {
            request: CacheListRequest {
                limit: 1,
                ..CacheListRequest::default()
            },
            incompatible_with_current: true,
            ..CacheListV2Request::default()
        };
        let first = manager
            .list_v2_with(&first_request)
            .expect("first incompatible page");
        assert_eq!(first.matched_entries, 2);
        assert_eq!(first.returned_entries, 1);
        let cursor = first.next_cursor.expect("next cursor");
        let second = manager
            .list_v2_with(&CacheListV2Request {
                request: CacheListRequest {
                    limit: 1,
                    cursor: Some(cursor.clone()),
                    ..CacheListRequest::default()
                },
                incompatible_with_current: true,
                ..CacheListV2Request::default()
            })
            .expect("second incompatible page");
        assert_eq!(second.returned_entries, 1);
        assert!(second.next_cursor.is_none());

        for changed in [
            CacheListV2Request {
                request: CacheListRequest {
                    cursor: Some(cursor.clone()),
                    ..CacheListRequest::default()
                },
                compatibilities: vec![CacheCompatibility::ObsoleteOlder],
                incompatible_with_current: true,
                ..CacheListV2Request::default()
            },
            CacheListV2Request {
                request: CacheListRequest {
                    cursor: Some(cursor.clone()),
                    ..CacheListRequest::default()
                },
                index_content_versions: vec![INDEX_CONTENT_VERSION - 1],
                incompatible_with_current: true,
                ..CacheListV2Request::default()
            },
            CacheListV2Request {
                request: CacheListRequest {
                    cursor: Some(cursor.clone()),
                    ..CacheListRequest::default()
                },
                incompatible_with_current: false,
                ..CacheListV2Request::default()
            },
        ] {
            assert!(matches!(
                manager.list_v2_with(&changed),
                Err(Error::InvalidInput {
                    field: "cache list cursor",
                    reason: "does not match the active cache filters"
                })
            ));
        }

        let exact = manager
            .list_v2_with(&CacheListV2Request {
                index_content_versions: vec![INDEX_CONTENT_VERSION],
                ..CacheListV2Request::default()
            })
            .expect("exact content-version filter");
        assert_eq!(exact.matched_entries, 1);
        assert_eq!(
            exact.entries[0].compatibility,
            CacheCompatibility::CompatibleCurrent
        );
    }

    #[test]
    fn list_filters_summarizes_and_pages_with_filter_bound_cursors() {
        let temp = tempfile::tempdir().expect("temporary directory");
        let root = temp.path().join("managed");
        let repository = temp.path().join("repository");
        fs::create_dir(&repository).expect("repository");
        // Config canonicalizes repository roots before binding cache metadata;
        // preserve that contract on macOS, where /var is commonly a symlink.
        let repository = fs::canonicalize(repository).expect("canonical repository");
        let manager = CacheManager::new(root.clone(), 10_000);
        for id in [FIRST_ID, SECOND_ID] {
            let directory = root.join(id);
            fs::create_dir_all(&directory).expect("corrupt cache directory");
            fs::write(directory.join(DATABASE_NAME), id.as_bytes()).expect("corrupt cache");
        }
        create_current_cache(&manager, &repository, 9_000);

        let first = manager
            .list_with(&CacheListRequest {
                limit: 2,
                ..CacheListRequest::default()
            })
            .expect("first cache page");
        assert_eq!(first.total_entries, 3);
        assert_eq!(first.matched_entries, 3);
        assert_eq!(first.returned_entries, 2);
        assert_eq!(
            first
                .entries
                .iter()
                .map(|entry| entry.id.as_str())
                .collect::<Vec<_>>(),
            vec![FIRST_ID, SECOND_ID]
        );
        let cursor = first.next_cursor.clone().expect("next cache cursor");

        let second = manager
            .list_with(&CacheListRequest {
                limit: 2,
                cursor: Some(cursor.clone()),
                ..CacheListRequest::default()
            })
            .expect("second cache page");
        assert_eq!(second.returned_entries, 1);
        assert_eq!(second.entries[0].state, CacheState::Current);
        assert!(second.next_cursor.is_none());

        let summary = manager
            .list_with(&CacheListRequest {
                summary: true,
                states: vec![CacheState::Corrupt],
                ..CacheListRequest::default()
            })
            .expect("corrupt cache summary");
        assert!(summary.summary_only);
        assert_eq!(summary.total_entries, 3);
        assert_eq!(summary.matched_entries, 2);
        assert_eq!(summary.returned_entries, 0);
        assert_eq!(summary.state_counts["corrupt"], 2);
        assert_eq!(summary.state_counts["current"], 0);
        assert!(summary.matched_bytes > 0);
        assert!(summary.entries.is_empty());
        assert!(summary.next_cursor.is_none());

        let by_root = manager
            .list_with(&CacheListRequest {
                repository_root: Some(repository.clone()),
                ..CacheListRequest::default()
            })
            .expect("repository cache filter");
        assert_eq!(by_root.matched_entries, 1);
        assert_eq!(by_root.entries[0].state, CacheState::Current);
        assert_eq!(
            by_root.entries[0].repository_root.as_deref(),
            Some(repository.as_path())
        );

        let error = manager
            .list_with(&CacheListRequest {
                states: vec![CacheState::Current],
                cursor: Some(cursor),
                ..CacheListRequest::default()
            })
            .expect_err("cursor must be bound to filters");
        assert!(matches!(
            error,
            Error::InvalidInput {
                field: "cache list cursor",
                reason: "does not match the active cache filters"
            }
        ));
    }

    #[test]
    fn list_rejects_invalid_response_bounds_and_cursors() {
        let temp = tempfile::tempdir().expect("temporary directory");
        let invalid_root = temp.path().join("not-a-directory");
        fs::write(&invalid_root, b"must not be inspected").expect("invalid cache root fixture");
        let manager = CacheManager::new(invalid_root, 10_000);
        let zero = manager
            .list_with(&CacheListRequest {
                limit: 0,
                ..CacheListRequest::default()
            })
            .expect_err("zero cache list limit");
        assert!(matches!(
            zero,
            Error::InvalidInput {
                field: "cache list limit",
                ..
            }
        ));

        let excessive = manager
            .list_with(&CacheListRequest {
                limit: MAX_CACHE_LIST_LIMIT + 1,
                ..CacheListRequest::default()
            })
            .expect_err("excessive cache list limit");
        assert!(matches!(
            excessive,
            Error::RequestLimitExceeded {
                field: "cache list limit",
                requested,
                limit: MAX_CACHE_LIST_LIMIT,
            } if requested == MAX_CACHE_LIST_LIMIT + 1
        ));

        let malformed = manager
            .list_with(&CacheListRequest {
                cursor: Some("not-a-cache-cursor".into()),
                ..CacheListRequest::default()
            })
            .expect_err("malformed cache list cursor");
        assert!(matches!(
            malformed,
            Error::InvalidInput {
                field: "cache list cursor",
                ..
            }
        ));

        let summary_cursor = manager
            .list_with(&CacheListRequest {
                summary: true,
                cursor: Some("not-used".into()),
                ..CacheListRequest::default()
            })
            .expect_err("summary cursor conflict");
        assert!(matches!(
            summary_cursor,
            Error::InvalidInput {
                field: "cache list cursor",
                reason: "cannot be combined with summary mode"
            }
        ));

        let compatibility_limit = manager
            .list_v2_with(&CacheListV2Request {
                compatibilities: vec![
                    CacheCompatibility::CompatibleCurrent;
                    MAX_CACHE_COMPATIBILITY_FILTERS + 1
                ],
                ..CacheListV2Request::default()
            })
            .expect_err("compatibility filter fan-out");
        assert!(matches!(
            compatibility_limit,
            Error::RequestLimitExceeded {
                field: "cache compatibility filters",
                ..
            }
        ));
        let version_limit = manager
            .list_v2_with(&CacheListV2Request {
                index_content_versions: vec![
                    INDEX_CONTENT_VERSION;
                    MAX_CACHE_CONTENT_VERSION_FILTERS + 1
                ],
                ..CacheListV2Request::default()
            })
            .expect_err("content-version filter fan-out");
        assert!(matches!(
            version_limit,
            Error::RequestLimitExceeded {
                field: "cache content-version filters",
                ..
            }
        ));
        assert!(matches!(
            manager.list_v2_with(&CacheListV2Request {
                index_content_versions: vec![0],
                ..CacheListV2Request::default()
            }),
            Err(Error::InvalidInput {
                field: "cache content-version filter",
                reason: "must be positive"
            })
        ));
    }

    #[test]
    fn legacy_repository_only_identity_remains_visible_and_prunable() {
        let temp = tempfile::tempdir().expect("temporary directory");
        let root = temp.path().join("managed");
        let repository = temp.path().join("repository");
        fs::create_dir(&repository).expect("repository");
        let repository = fs::canonicalize(repository).expect("canonical repository");
        let current_id = managed_cache_id(&repository);
        let legacy_id = current_id.split_once('-').expect("versioned identity").1;
        let directory = root.join(legacy_id);
        fs::create_dir_all(&directory).expect("legacy cache directory");
        let database = directory.join(DATABASE_NAME);
        drop(Storage::open_for_repository(&database, &repository).expect("legacy cache database"));
        let manager = CacheManager::new(root, 10_000);

        let listed = manager.list().expect("cache list");

        assert_eq!(listed.entries.len(), 1);
        assert_eq!(listed.entries[0].index_content_version, None);
        assert_eq!(listed.entries[0].state, CacheState::Current);

        let mut request = request();
        request.max_total_bytes = Some(0);
        let pruned = manager.prune(&request).expect("legacy prune plan");
        assert_eq!(pruned.results[0].action, CachePruneAction::WouldDelete);
        assert!(database.exists());
    }

    #[test]
    fn active_service_clones_block_prune_until_every_lease_is_dropped() {
        let temp = tempfile::tempdir().expect("temporary directory");
        let root = temp.path().join("managed");
        let repository = temp.path().join("repository");
        fs::create_dir(&repository).expect("repository");
        let repository = fs::canonicalize(repository).expect("canonical repository");
        let directory = root.join(managed_cache_id(&repository));
        fs::create_dir_all(&directory).expect("cache directory");
        let database = directory.join(DATABASE_NAME);
        let config = Config::discover(&repository, Some(database.clone())).expect("config");
        let services = Services::open(config).expect("services");
        let follower = services.clone();
        let manager = CacheManager::new(root, unix_seconds(SystemTime::now()));
        let mut prune = request();
        prune.max_total_bytes = Some(1);
        prune.dry_run = false;
        prune.yes = true;

        let first = manager.prune(&prune).expect("active prune");
        assert_eq!(first.results[0].action, CachePruneAction::Kept);
        assert!(database.exists());
        drop(services);
        let second = manager.prune(&prune).expect("follower prune");
        assert_eq!(second.results[0].action, CachePruneAction::Kept);
        drop(follower);

        let deleted = manager.prune(&prune).expect("inactive prune");
        assert_eq!(
            deleted.results[0].action,
            CachePruneAction::Deleted,
            "unexpected prune report: {deleted:#?}"
        );
        assert!(!database.exists());
        assert!(coordination_sidecar_path(&database, LEASE_LOCK_SUFFIX).exists());
        assert!(manager.list().expect("empty list").entries.is_empty());
    }

    #[test]
    fn missing_repository_requires_age_or_explicit_override() {
        let temp = tempfile::tempdir().expect("temporary directory");
        let repository = temp.path().join("offline-repository");
        fs::create_dir(&repository).expect("repository");
        let manager = CacheManager::new(temp.path().join("managed"), 10 * SECONDS_PER_DAY);
        create_current_cache(&manager, &repository, 9 * SECONDS_PER_DAY);
        fs::remove_dir(&repository).expect("take repository offline");

        let mut age_only = request();
        age_only.older_than_days = Some(30);
        let kept = manager.prune(&age_only).expect("age plan");
        assert_eq!(kept.results[0].action, CachePruneAction::Kept);

        age_only.remove_missing_roots = true;
        let selected = manager.prune(&age_only).expect("missing-root plan");
        assert_eq!(selected.results[0].action, CachePruneAction::WouldDelete);
        assert_eq!(selected.results[0].reasons, ["missing_repository"]);
    }

    #[test]
    fn lru_budget_selects_oldest_cache_and_dry_run_preserves_files() {
        let temp = tempfile::tempdir().expect("temporary directory");
        let first_root = temp.path().join("first-repository");
        let second_root = temp.path().join("second-repository");
        fs::create_dir(&first_root).expect("first repository");
        fs::create_dir(&second_root).expect("second repository");
        let manager = CacheManager::new(temp.path().join("managed"), 1_000);
        let (first_id, first) = create_current_cache(&manager, &first_root, 100);
        let (second_id, second) = create_current_cache(&manager, &second_root, 900);
        let listed = manager.list().expect("cache list");
        let oldest_size = listed
            .entries
            .iter()
            .find(|entry| entry.id == first_id)
            .expect("oldest cache")
            .size_bytes;
        let mut prune = request();
        prune.max_total_bytes = Some(listed.total_bytes - oldest_size);

        let report = manager.prune(&prune).expect("LRU plan");

        assert_eq!(report.total_bytes_before, listed.total_bytes);
        let first_result = report
            .results
            .iter()
            .find(|result| result.id == first_id)
            .expect("oldest result");
        let second_result = report
            .results
            .iter()
            .find(|result| result.id == second_id)
            .expect("newest result");
        assert_eq!(first_result.action, CachePruneAction::WouldDelete);
        assert_eq!(first_result.reasons, ["max_total_bytes"]);
        assert_eq!(second_result.action, CachePruneAction::Kept);
        assert_eq!(report.reclaimed_bytes, oldest_size);
        assert!(first.exists());
        assert!(second.exists());
    }

    #[test]
    fn corrupt_and_legacy_caches_are_listed_without_mutation() {
        let temp = tempfile::tempdir().expect("temporary directory");
        let root = temp.path().join("managed");
        let corrupt = root.join(FIRST_ID);
        fs::create_dir_all(&corrupt).expect("corrupt directory");
        fs::write(corrupt.join(DATABASE_NAME), b"not sqlite").expect("corrupt database");
        let legacy = root.join(SECOND_ID);
        fs::create_dir_all(&legacy).expect("legacy directory");
        let connection = Connection::open(legacy.join(DATABASE_NAME)).expect("legacy database");
        connection
            .execute_batch(
                "CREATE TABLE meta (
                    id INTEGER PRIMARY KEY,
                    schema_version INTEGER NOT NULL,
                    repository_root TEXT NOT NULL
                );
                INSERT INTO meta VALUES (1, 4, '');",
            )
            .expect("legacy schema");
        drop(connection);
        let manager = CacheManager::new(root, 10_000);

        let report = manager.list().expect("cache list");

        assert_eq!(report.entries[0].state, CacheState::Corrupt);
        assert_eq!(report.entries[1].state, CacheState::Legacy);
        assert!(corrupt.join(DATABASE_NAME).exists());
        assert!(legacy.join(DATABASE_NAME).exists());

        let mut prune = request();
        prune.max_total_bytes = Some(0);
        let plan = manager.prune(&prune).expect("prune plan");
        assert_eq!(plan.results[0].action, CachePruneAction::Kept);
        assert_eq!(plan.results[1].action, CachePruneAction::WouldDelete);
        assert!(corrupt.join(DATABASE_NAME).exists());
        assert!(legacy.join(DATABASE_NAME).exists());
    }

    #[test]
    fn legacy_wal_list_keeps_file_mtime_access_age_stable() {
        let temp = tempfile::tempdir().expect("temporary directory");
        let manager = CacheManager::new(temp.path().join("managed"), 20 * SECONDS_PER_DAY);
        create_legacy_wal_cache(&manager, FIRST_ID, SECONDS_PER_DAY);

        let first = manager.list().expect("first cache list");
        let second = manager.list().expect("second cache list");

        assert_eq!(first.entries[0].state, CacheState::Legacy);
        assert_eq!(
            first.entries[0].access_time_source,
            Some(AccessTimeSource::FileMtime)
        );
        assert_eq!(
            first.entries[0].last_access_unix_seconds,
            Some(SECONDS_PER_DAY)
        );
        assert_eq!(
            second.entries[0].last_access_unix_seconds,
            first.entries[0].last_access_unix_seconds
        );
        assert_eq!(second.entries[0].age_seconds, first.entries[0].age_seconds);
    }

    #[test]
    fn legacy_wal_dry_run_keeps_age_selection_stable() {
        let temp = tempfile::tempdir().expect("temporary directory");
        let manager = CacheManager::new(temp.path().join("managed"), 20 * SECONDS_PER_DAY);
        create_legacy_wal_cache(&manager, FIRST_ID, SECONDS_PER_DAY);
        let mut request = request();
        request.older_than_days = Some(7);

        let first = manager.prune(&request).expect("first prune plan");
        let second = manager.prune(&request).expect("second prune plan");

        assert_eq!(first.results[0].action, CachePruneAction::WouldDelete);
        assert_eq!(first.results[0].reasons, ["older_than"]);
        assert_eq!(second.results[0], first.results[0]);
    }

    #[test]
    fn unexpected_content_is_never_removed_automatically() {
        let temp = tempfile::tempdir().expect("temporary directory");
        let directory = temp.path().join("managed").join(FIRST_ID);
        fs::create_dir_all(&directory).expect("cache directory");
        fs::write(directory.join(DATABASE_NAME), b"not sqlite").expect("database");
        fs::write(directory.join("keep.txt"), b"owner data").expect("unexpected file");
        let manager = CacheManager::new(temp.path().join("managed"), 10_000);
        let mut prune = request();
        prune.max_total_bytes = Some(1);
        prune.dry_run = false;
        prune.yes = true;

        let report = manager.prune(&prune).expect("prune");

        assert_eq!(report.results[0].action, CachePruneAction::Kept);
        assert!(report.results[0].reasons.is_empty());
        assert!(directory.join(DATABASE_NAME).exists());
        assert!(directory.join("keep.txt").exists());
    }

    #[test]
    fn future_schema_and_mismatched_identity_are_never_removed_automatically() {
        let temp = tempfile::tempdir().expect("temporary directory");
        let root = temp.path().join("managed");
        let future_root = temp.path().join("future-repository");
        let mismatch_root = temp.path().join("mismatch-repository");
        fs::create_dir(&future_root).expect("future repository");
        fs::create_dir(&mismatch_root).expect("mismatch repository");
        let manager = CacheManager::new(root, 10_000);
        let (future_id, future_database) = create_current_cache(&manager, &future_root, 100);
        Connection::open(&future_database)
            .expect("future database")
            .execute(
                "UPDATE meta SET schema_version = ?1, repository_root = x'80' WHERE id = 1",
                [CURRENT_SCHEMA_VERSION + 1],
            )
            .expect("future schema");
        let mismatch_id = FIRST_ID;
        assert_ne!(mismatch_id, managed_cache_id(&mismatch_root));
        let mismatch_directory = manager.root.join(mismatch_id);
        fs::create_dir_all(&mismatch_directory).expect("mismatch directory");
        let mismatch_database = mismatch_directory.join(DATABASE_NAME);
        drop(
            Storage::open_for_repository(&mismatch_database, &mismatch_root)
                .expect("mismatch database"),
        );
        let future_migration_id = SECOND_ID;
        let future_migration_directory = manager.root.join(future_migration_id);
        fs::create_dir_all(&future_migration_directory).expect("future migration directory");
        let future_migration_database = future_migration_directory.join(DATABASE_NAME);
        Connection::open(&future_migration_database)
            .expect("future migration database")
            .execute_batch(&format!(
                "PRAGMA user_version = {}; CREATE TABLE replacement(value INTEGER);",
                CURRENT_MIGRATION_VERSION + 1
            ))
            .expect("future migration");
        let mut prune = request();
        prune.max_total_bytes = Some(0);
        prune.dry_run = false;
        prune.yes = true;

        let report = manager.prune(&prune).expect("prune plan");

        let future = report
            .results
            .iter()
            .find(|result| result.id == future_id)
            .expect("future result");
        let mismatch = report
            .results
            .iter()
            .find(|result| result.id == mismatch_id)
            .expect("mismatch result");
        let future_migration = report
            .results
            .iter()
            .find(|result| result.id == future_migration_id)
            .expect("future migration result");
        assert_eq!(future.action, CachePruneAction::Kept);
        assert_eq!(mismatch.action, CachePruneAction::Kept);
        assert_eq!(future_migration.action, CachePruneAction::Kept);
        assert!(future_database.exists());
        assert!(mismatch_database.exists());
        assert!(future_migration_database.exists());
    }

    #[test]
    fn future_index_content_cache_is_visible_but_never_removed() {
        let temp = tempfile::tempdir().expect("temporary directory");
        let root = temp.path().join("managed");
        let repository = temp.path().join("repository");
        fs::create_dir(&repository).expect("repository");
        let current_id = managed_cache_id(&repository);
        let root_hash = current_id.split_once('-').expect("versioned identity").1;
        let future_id = format!("v{}-{root_hash}", INDEX_CONTENT_VERSION + 1);
        let directory = root.join(&future_id);
        fs::create_dir_all(&directory).expect("future cache directory");
        let database = directory.join(DATABASE_NAME);
        drop(
            Storage::open_for_repository(&database, &repository)
                .expect("future cache database fixture"),
        );
        let manager = CacheManager::new(root, 10_000);

        let listed = manager.list().expect("cache list");

        assert_eq!(listed.entries.len(), 1);
        assert_eq!(listed.entries[0].id, future_id);
        assert_eq!(
            listed.entries[0].index_content_version,
            Some(INDEX_CONTENT_VERSION + 1)
        );
        assert_eq!(listed.entries[0].state, CacheState::Unsupported);
        assert_eq!(
            listed.entries[0].detail.as_deref(),
            Some("cache uses a newer index-content version")
        );

        let mut request = request();
        request.max_total_bytes = Some(0);
        let pruned = manager.prune(&request).expect("future cache prune plan");
        assert_eq!(pruned.results[0].action, CachePruneAction::Kept);
        assert!(database.exists());
    }

    #[test]
    fn incompatible_prune_is_dry_run_first_and_fail_closed() {
        let temp = tempfile::tempdir().expect("temporary directory");
        let manager = CacheManager::new(temp.path().join("managed"), 10_000);
        let repositories = (0..4)
            .map(|index| {
                let repository = temp.path().join(format!("repository-{index}"));
                fs::create_dir(&repository).expect("repository");
                repository
            })
            .collect::<Vec<_>>();
        let (_, current_database) = create_current_cache(&manager, &repositories[0], 9_000);
        let (older_id, older_database) = create_cache_with_content_identity(
            &manager,
            &repositories[1],
            8_000,
            Some(INDEX_CONTENT_VERSION - 1),
        );
        let (legacy_id, legacy_database) =
            create_cache_with_content_identity(&manager, &repositories[2], 7_000, None);
        let (_, future_database) = create_cache_with_content_identity(
            &manager,
            &repositories[3],
            6_000,
            Some(INDEX_CONTENT_VERSION + 1),
        );
        let corrupt = manager.root.join(FIRST_ID).join(DATABASE_NAME);
        fs::create_dir_all(corrupt.parent().expect("corrupt directory"))
            .expect("corrupt cache directory");
        fs::write(&corrupt, b"not sqlite").expect("corrupt database");
        let dry_run = CachePruneV2Request {
            request: request(),
            incompatible_with_current: true,
        };

        let plan = manager.prune_v2(&dry_run).expect("incompatible dry run");

        for id in [&older_id, &legacy_id] {
            let result = plan
                .results
                .iter()
                .find(|result| &result.id == id)
                .expect("incompatible result");
            assert_eq!(result.action, CachePruneAction::WouldDelete);
            assert_eq!(result.reasons.len(), 1);
            assert!(result.reasons[0].starts_with("incompatible_with_current:"));
        }
        assert!(
            plan.results
                .iter()
                .filter(|result| result.id != older_id && result.id != legacy_id)
                .all(|result| result.action == CachePruneAction::Kept)
        );
        for database in [
            &current_database,
            &older_database,
            &legacy_database,
            &future_database,
            &corrupt,
        ] {
            assert!(database.exists(), "dry run removed {}", database.display());
        }

        let applied = manager
            .prune_v2(&CachePruneV2Request {
                request: CachePruneRequest {
                    dry_run: false,
                    yes: true,
                    ..request()
                },
                incompatible_with_current: true,
            })
            .expect("apply incompatible prune");
        for id in [&older_id, &legacy_id] {
            assert_eq!(
                applied
                    .results
                    .iter()
                    .find(|result| &result.id == id)
                    .expect("deleted incompatible result")
                    .action,
                CachePruneAction::Deleted
            );
        }
        assert!(!older_database.exists());
        assert!(!legacy_database.exists());
        assert!(current_database.exists());
        assert!(future_database.exists());
        assert!(corrupt.exists());
    }

    #[test]
    fn incompatible_prune_never_projects_an_active_cache_as_reclaimable() {
        let temp = tempfile::tempdir().expect("temporary directory");
        let repository = temp.path().join("repository");
        fs::create_dir(&repository).expect("repository");
        let repository = repository.canonicalize().expect("canonical repository");
        let manager = CacheManager::new(temp.path().join("managed"), 10_000);
        let (_, database) = create_cache_with_content_identity(
            &manager,
            &repository,
            9_000,
            Some(INDEX_CONTENT_VERSION - 1),
        );
        let config =
            Config::discover(&repository, Some(database.clone())).expect("active cache config");
        let services = Services::open(config).expect("active cache service");
        let request = CachePruneV2Request {
            request: request(),
            incompatible_with_current: true,
        };

        let listed = manager
            .list_v2_with(&CacheListV2Request::default())
            .expect("active compatibility summary");
        assert_eq!(listed.compatibility_counts["obsolete_older"].entries, 1);
        assert_eq!(listed.safely_reclaimable_incompatible_entries, 0);
        assert_eq!(listed.safely_reclaimable_incompatible_bytes, 0);

        let active = manager.prune_v2(&request).expect("active prune plan");
        assert_eq!(active.results[0].action, CachePruneAction::SkippedActive);
        assert!(database.exists());

        drop(services);
        let inactive = manager.prune_v2(&request).expect("inactive prune plan");
        assert_eq!(inactive.results[0].action, CachePruneAction::WouldDelete);
        assert!(database.exists());
    }

    #[test]
    fn stale_cache_is_deleted_after_age_and_confirmation() {
        let temp = tempfile::tempdir().expect("temporary directory");
        let repository = temp.path().join("repository");
        fs::create_dir(&repository).expect("repository");
        let manager = CacheManager::new(temp.path().join("managed"), 40 * SECONDS_PER_DAY);
        let (id, database) = create_current_cache(&manager, &repository, SECONDS_PER_DAY);
        let mut prune = request();
        prune.older_than_days = Some(30);
        prune.dry_run = false;
        prune.yes = true;

        let report = manager.prune(&prune).expect("prune stale cache");

        let result = report
            .results
            .iter()
            .find(|result| result.id == id)
            .expect("stale result");
        assert_eq!(result.action, CachePruneAction::Deleted);
        assert_eq!(result.reasons, ["older_than"]);
        assert!(!database.exists());
        assert!(database.with_extension("sqlite.lease.lock").exists());
    }

    #[test]
    fn explicit_database_outside_managed_root_is_never_considered() {
        let temp = tempfile::tempdir().expect("temporary directory");
        let repository = temp.path().join("repository");
        fs::create_dir(&repository).expect("repository");
        let explicit = temp.path().join("explicit.sqlite");
        let config = Config::discover(&repository, Some(explicit.clone())).expect("config");
        drop(Services::open(config).expect("services"));
        let manager = CacheManager::new(temp.path().join("managed"), 10_000);

        assert!(manager.list().expect("cache list").entries.is_empty());
        let mut prune = request();
        prune.max_total_bytes = Some(1);
        assert!(manager.prune(&prune).expect("prune").results.is_empty());
        assert!(explicit.exists());
    }

    #[test]
    fn prune_requires_an_explicit_policy_and_mutation_consent() {
        let temp = tempfile::tempdir().expect("temporary directory");
        let manager = CacheManager::new(temp.path().join("managed"), 10_000);
        let empty = request();
        assert!(
            manager
                .prune(&empty)
                .unwrap_err()
                .to_string()
                .contains("requires --older-than")
        );

        let mut mutation = request();
        mutation.max_total_bytes = Some(1);
        mutation.dry_run = false;
        assert!(
            manager
                .prune(&mutation)
                .unwrap_err()
                .to_string()
                .contains("requires --yes")
        );
    }
}
