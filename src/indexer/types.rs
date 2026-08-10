use super::*;

#[cfg(test)]
pub(super) const PREVIOUS_INDEX_CONTENT_MARKER: &str = "leantoken-index-content-v10";

/// Owns discovery/parse publication for one repository cache.
///
/// The Rayon worker pool is built lazily on the first non-empty prepare and
/// then reused. Read-only follower processes therefore do not create indexing
/// threads merely by opening repository services.
#[derive(Clone)]
pub struct Indexer {
    pub(super) config: Arc<Config>,
    pub(super) storage: Storage,
    pub(super) pool: Arc<LazyWorkerPool>,
    pub(super) repository_root: Arc<Dir>,
    pub(super) progress: IndexProgressRegistry,
}

/// Phase and batch high-water diagnostics for one full reconciliation.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct IndexingDiagnostics {
    /// End-to-end reconciliation time, including storage commit.
    pub total_ms: f64,
    /// Ignore-aware repository discovery time.
    pub discovery_ms: f64,
    /// Existing-state load, hashing, and reconciliation planning time.
    pub hash_and_plan_ms: f64,
    /// Parallel file read, chunk, tokenize, and parse time.
    pub preparation_ms: f64,
    /// Summed worker time for profiled preparation subphases.
    ///
    /// These durations overlap across Rayon workers and therefore describe
    /// aggregate work, not additional wall time.
    pub preparation_detail: PreparationDiagnostics,
    /// Profiled worker time grouped by the prepared file's detected language.
    ///
    /// Only successfully prepared searchable files have a language owner.
    pub preparation_by_language: BTreeMap<String, PreparationDiagnostics>,
    /// Batch-consumption time after parallel preparation, including stage writes.
    pub insertion_ms: f64,
    /// Total lifetime of the generation publication transaction.
    pub publication_ms: f64,
    /// Storage-level phases and footprint captured only by profiled reconciliation.
    pub publication_detail: PublicationDiagnostics,
    /// Linux process write bytes across the complete reconciliation.
    #[serde(default)]
    pub process_write_bytes: Option<u64>,
    /// Sum of non-overlapping stage, relational, FTS, commit, and checkpoint writes.
    #[serde(default)]
    pub storage_phase_write_bytes: Option<u64>,
    /// Process writes not owned by one measured storage phase.
    #[serde(default)]
    pub unattributed_process_write_bytes: Option<u64>,
    /// Committed generation observed before publication verification.
    #[serde(default)]
    pub generation_before: u64,
    /// Committed generation returned after publication verification.
    #[serde(default)]
    pub generation_after: u64,
    /// Whether this reconciliation published a new generation.
    #[serde(default)]
    pub generation_published: bool,
    /// Number of bounded preparation batches consumed.
    pub preparation_batches: usize,
    /// Largest number of files held in one prepared batch.
    pub max_batch_files: usize,
    /// Largest aggregate discovered source bytes in one prepared batch.
    pub max_batch_source_bytes: u64,
    /// Filesystem entries yielded during discovery.
    pub walk_entries: u64,
    /// Files admitted by discovery.
    pub discovered_files: u64,
    /// Aggregate metadata bytes admitted by discovery.
    pub discovered_source_bytes: u64,
}

/// Diagnostic-only worker-time attribution for parallel file preparation.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct PreparationDiagnostics {
    /// Files for which subphase measurements were collected.
    pub files_profiled: usize,
    /// Summed end-to-end worker time across files.
    pub total_worker_ms: f64,
    /// Bounded source-file reads.
    pub read_ms: f64,
    /// UTF-8 validation, binary classification, and chunk boundary construction.
    pub text_prepare_ms: f64,
    /// Content hashing.
    pub hash_ms: f64,
    /// Tree-sitter language parsing and syntax extraction.
    pub parse_ms: f64,
    /// Whole-file source-token counting.
    pub source_token_count_ms: f64,
    /// Per-chunk token counting.
    pub chunk_token_count_ms: f64,
    /// Conversion from parser/text output into storage input records.
    pub projection_ms: f64,
}

/// Full reconciliation response paired with diagnostics excluded from MCP output.
#[derive(Debug, Clone)]
pub struct ProfiledIndexResponse {
    /// Ordinary index response returned by adapters and services.
    pub response: IndexResponse,
    /// Internal phase and batch measurements for profiling.
    pub diagnostics: IndexingDiagnostics,
}

/// Additive index report paired with full-reconciliation diagnostics.
#[derive(Debug, Clone)]
pub struct ProfiledIndexReport {
    /// Flattened wire response plus preparation skip reasons.
    pub report: IndexReport,
    /// Internal phase and batch measurements for profiling.
    pub diagnostics: IndexingDiagnostics,
}

#[derive(Debug, Default)]
pub(super) struct PreparationMetrics {
    pub(super) preparation: Duration,
    pub(super) detail: FilePreparationDiagnostics,
    pub(super) detail_by_language: BTreeMap<String, FilePreparationDiagnostics>,
    pub(super) insertion: Duration,
    pub(super) insertion_write_bytes: Option<u64>,
    pub(super) batches: usize,
    pub(super) max_batch_files: usize,
    pub(super) max_batch_source_bytes: u64,
}

#[derive(Debug, Default)]
pub(super) struct FilePreparationDiagnostics {
    pub(super) files_profiled: usize,
    pub(super) total: Duration,
    pub(super) read: Duration,
    pub(super) text_prepare: Duration,
    pub(super) hash: Duration,
    pub(super) parse: Duration,
    pub(super) source_token_count: Duration,
    pub(super) chunk_token_count: Duration,
    pub(super) projection: Duration,
}

impl FilePreparationDiagnostics {
    pub(super) fn add(&mut self, other: &Self) {
        self.files_profiled = self.files_profiled.saturating_add(other.files_profiled);
        self.total += other.total;
        self.read += other.read;
        self.text_prepare += other.text_prepare;
        self.hash += other.hash;
        self.parse += other.parse;
        self.source_token_count += other.source_token_count;
        self.chunk_token_count += other.chunk_token_count;
        self.projection += other.projection;
    }

    pub(super) fn report(&self) -> PreparationDiagnostics {
        PreparationDiagnostics {
            files_profiled: self.files_profiled,
            total_worker_ms: duration_ms(self.total),
            read_ms: duration_ms(self.read),
            text_prepare_ms: duration_ms(self.text_prepare),
            hash_ms: duration_ms(self.hash),
            parse_ms: duration_ms(self.parse),
            source_token_count_ms: duration_ms(self.source_token_count),
            chunk_token_count_ms: duration_ms(self.chunk_token_count),
            projection_ms: duration_ms(self.projection),
        }
    }
}

pub(super) struct LazyWorkerPool {
    pub(super) pool: OnceLock<ThreadPool>,
    pub(super) init: Mutex<()>,
}

#[derive(Debug, Default)]
/// Explicit filesystem membership classification used to drive incremental work.
///
/// Only creations and deletions can change which bounded import candidate paths
/// resolve. Content-only modifications do not trigger reverse-import expansion.
pub(super) struct ChangeSet {
    pub(super) created: Vec<String>,
    pub(super) modified: Vec<String>,
    pub(super) deleted: Vec<String>,
}

#[derive(Debug)]
pub(super) struct RelocationPlan {
    pub(super) old_path: String,
    pub(super) new_file: DiscoveredFile,
    pub(super) expected_hash: String,
}

#[derive(Debug, Eq, Hash, PartialEq)]
pub(super) struct RelocationKey {
    pub(super) content_hash: String,
    pub(super) size_bytes: u64,
    pub(super) language: Option<String>,
}

impl ChangeSet {
    pub(super) fn classify(
        existing: &HashMap<String, crate::storage::FileRecord>,
        candidates: &HashMap<String, DiscoveredFile>,
        deletions: &HashSet<String>,
    ) -> Self {
        let mut created = Vec::new();
        let mut modified = Vec::new();
        for path in candidates.keys() {
            if existing.contains_key(path) {
                modified.push(path.clone());
            } else {
                created.push(path.clone());
            }
        }
        let mut deleted = deletions.iter().cloned().collect::<Vec<_>>();
        created.sort_unstable();
        modified.sort_unstable();
        deleted.sort_unstable();
        Self {
            created,
            modified,
            deleted,
        }
    }

    pub(super) fn membership_changes(&self) -> Vec<String> {
        let mut paths = Vec::with_capacity(self.created.len() + self.deleted.len());
        paths.extend(self.created.iter().cloned());
        paths.extend(self.deleted.iter().cloned());
        paths
    }
}

impl LazyWorkerPool {
    pub(super) fn new() -> Self {
        Self {
            pool: OnceLock::new(),
            init: Mutex::new(()),
        }
    }

    pub(super) fn get_or_build(&self, workers: usize) -> Result<&ThreadPool> {
        if let Some(pool) = self.pool.get() {
            return Ok(pool);
        }

        // Serialize fallible initialization without caching a failure. A later
        // reconciliation may retry after a transient thread-creation failure.
        let _guard = self
            .init
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(pool) = self.pool.get() {
            return Ok(pool);
        }

        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(workers.max(1))
            .thread_name(|index| format!("leantoken-index-{index}"))
            .build()?;
        let _ = self.pool.set(pool);
        Ok(self
            .pool
            .get()
            .expect("worker pool is initialized while holding its init lock"))
    }
}

impl fmt::Debug for Indexer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Indexer")
            .field("config", &self.config)
            .field("storage", &self.storage)
            .field(
                "pool_threads",
                &self.pool.pool.get().map(ThreadPool::current_num_threads),
            )
            .finish()
    }
}
