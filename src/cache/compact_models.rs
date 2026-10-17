use super::*;

/// Maximum explicit cache identities inspected by one compaction request.
pub const MAX_CACHE_COMPACT_IDS: usize = 8;
/// Default minimum reusable page space (64 MiB).
pub const DEFAULT_COMPACT_MIN_BYTES: u64 = 64 * 1024 * 1024;
/// Default minimum freelist fraction, in percent.
pub const DEFAULT_COMPACT_MIN_PERCENT: u8 = 10;
/// Default maximum logical database and sidecar footprint (1 GiB).
pub const DEFAULT_COMPACT_MAX_BYTES: u64 = 1024 * 1024 * 1024;
/// Default cooperative execution deadline per database, in seconds.
pub const DEFAULT_COMPACT_MAX_SECONDS: u64 = 120;

/// Explicit selection, benefit thresholds, and execution bounds for compaction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CacheCompactRequest {
    /// Exact managed identities; one to eight, with duplicates rejected.
    pub ids: Vec<String>,
    /// Minimum freelist bytes; both benefit thresholds must pass.
    pub min_reclaim_bytes: u64,
    /// Minimum freelist fraction (1–100 percent).
    pub min_reclaim_percent: u8,
    /// Maximum logical database or database-plus-sidecar bytes.
    pub max_database_bytes: u64,
    /// Cooperative deadline per database (1–3600 seconds).
    pub max_seconds: u64,
    /// Preview eligibility without checkpointing or vacuuming.
    pub dry_run: bool,
    /// Explicitly apply compaction.
    pub yes: bool,
}

impl Default for CacheCompactRequest {
    fn default() -> Self {
        Self {
            ids: Vec::new(),
            min_reclaim_bytes: DEFAULT_COMPACT_MIN_BYTES,
            min_reclaim_percent: DEFAULT_COMPACT_MIN_PERCENT,
            max_database_bytes: DEFAULT_COMPACT_MAX_BYTES,
            max_seconds: DEFAULT_COMPACT_MAX_SECONDS,
            dry_run: true,
            yes: false,
        }
    }
}

/// Compaction decision, binding diagnostics to skip or failure states.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum CacheCompactOutcome {
    /// An eligible inactive database would be compacted.
    WouldCompact,
    /// Compaction and the final checkpoint completed.
    Compacted,
    /// A live cache lease prevented maintenance.
    SkippedActive { detail: String },
    /// Metadata, filesystem identity, or contents were unsafe.
    SkippedUnsafe { detail: String },
    /// Reusable pages failed at least one benefit threshold.
    SkippedLowBenefit,
    /// The configured database work bound was exceeded.
    SkippedTooLarge,
    /// The database or SQLite temporary volume lacked the required headroom.
    SkippedInsufficientSpace { detail: String },
    /// SQLite or filesystem maintenance failed, possibly after VACUUM committed.
    Failed { error: String },
}

/// Auditable measurements and decision for one explicitly selected cache.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CacheCompactResult {
    /// Exact cache identity.
    pub id: String,
    /// Managed directory inspected.
    pub path: PathBuf,
    /// Decision and diagnostic payload.
    #[serde(flatten)]
    pub outcome: CacheCompactOutcome,
    /// Observed artifact bytes before the operation, excluding the stable lease file.
    pub size_bytes_before: Option<u64>,
    /// Artifact bytes after the operation, if observed.
    pub size_bytes_after: Option<u64>,
    /// Logical SQLite page bytes from a consistent read transaction.
    pub database_bytes: Option<u64>,
    /// Reusable freelist bytes; an estimate, not promised savings.
    pub reusable_bytes: Option<u64>,
    /// Measured artifact reduction; previews never report actual reclaimed bytes.
    pub reclaimed_bytes: u64,
    /// Whether VACUUM committed, including a later checkpoint failure.
    pub vacuum_committed: bool,
    /// Elapsed milliseconds for inspection and maintenance.
    pub elapsed_millis: u64,
}

/// Bounded compaction results in stable cache-identity order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CacheCompactReport {
    /// Platform-managed cache root.
    pub cache_root: PathBuf,
    /// Whether the request prohibited VACUUM and checkpoints.
    pub dry_run: bool,
    /// Actual artifact reduction, excluding estimated preview savings.
    pub reclaimed_bytes: u64,
    /// At most eight per-cache decisions.
    pub results: Vec<CacheCompactResult>,
}

impl CacheCompactReport {
    /// Whether any maintenance operation failed.
    #[must_use]
    pub fn has_failures(&self) -> bool {
        self.results
            .iter()
            .any(|row| matches!(row.outcome, CacheCompactOutcome::Failed { .. }))
    }
}

impl CacheCompactOutcome {
    pub(super) fn label(&self) -> &'static str {
        match self {
            Self::WouldCompact => "would_compact",
            Self::Compacted => "compacted",
            Self::SkippedActive { .. } => "skipped_active",
            Self::SkippedUnsafe { .. } => "skipped_unsafe",
            Self::SkippedLowBenefit => "skipped_low_benefit",
            Self::SkippedTooLarge => "skipped_too_large",
            Self::SkippedInsufficientSpace { .. } => "skipped_insufficient_space",
            Self::Failed { .. } => "failed",
        }
    }

    pub(super) fn diagnostic(&self) -> Option<&str> {
        match self {
            Self::SkippedActive { detail }
            | Self::SkippedUnsafe { detail }
            | Self::SkippedInsufficientSpace { detail } => Some(detail),
            Self::Failed { error } => Some(error),
            _ => None,
        }
    }
}
