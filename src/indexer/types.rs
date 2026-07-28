#[cfg(test)]
const PREVIOUS_INDEX_CONTENT_MARKER: &str = "leantoken-index-content-v10";

/// Owns discovery/parse publication for one repository cache.
///
/// The Rayon worker pool is built lazily on the first non-empty prepare and
/// then reused. Read-only follower processes therefore do not create indexing
/// threads merely by opening repository services.
#[derive(Clone)]
pub struct Indexer {
    config: Arc<Config>,
    storage: Storage,
    pool: Arc<LazyWorkerPool>,
    repository_root: Arc<Dir>,
}

/// Phase and batch high-water diagnostics for one full reconciliation.
#[derive(Debug, Clone, serde::Serialize)]
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
    /// Import resolution and SQLite insertion time inside batch callbacks.
    pub insertion_ms: f64,
    /// Total lifetime of the generation publication transaction.
    pub publication_ms: f64,
    /// Storage-level phases and footprint captured only by profiled reconciliation.
    pub publication_detail: PublicationDiagnostics,
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
#[derive(Debug, Clone, Default, serde::Serialize)]
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
    /// Flattened-compatible response plus preparation skip reasons.
    pub report: IndexReport,
    /// Internal phase and batch measurements for profiling.
    pub diagnostics: IndexingDiagnostics,
}

#[derive(Debug, Default)]
struct PreparationMetrics {
    preparation: Duration,
    detail: FilePreparationDiagnostics,
    insertion: Duration,
    insertion_write_bytes: Option<u64>,
    batches: usize,
    max_batch_files: usize,
    max_batch_source_bytes: u64,
}

#[derive(Debug, Default)]
struct FilePreparationDiagnostics {
    files_profiled: usize,
    total: Duration,
    read: Duration,
    text_prepare: Duration,
    hash: Duration,
    parse: Duration,
    source_token_count: Duration,
    chunk_token_count: Duration,
    projection: Duration,
}

impl FilePreparationDiagnostics {
    fn add(&mut self, other: Self) {
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

    fn report(&self) -> PreparationDiagnostics {
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

#[derive(Clone, Copy, PartialEq, Eq)]
enum StorageProfiling {
    Omit,
    Collect,
}

struct LazyWorkerPool {
    pool: OnceLock<ThreadPool>,
    init: Mutex<()>,
}

#[derive(Debug, Default)]
/// Explicit filesystem membership classification used to drive incremental work.
///
/// Only creations and deletions can change which bounded import candidate paths
/// resolve. Content-only modifications do not trigger reverse-import expansion.
struct ChangeSet {
    created: Vec<String>,
    modified: Vec<String>,
    deleted: Vec<String>,
    visibility_recomputed: bool,
}

#[derive(Debug)]
struct RelocationPlan {
    old_path: String,
    new_file: DiscoveredFile,
    expected_hash: String,
}

#[derive(Debug, Eq, Hash, PartialEq)]
struct RelocationKey {
    content_hash: String,
    size_bytes: u64,
    language: Option<String>,
}

impl ChangeSet {
    fn classify(
        existing: &HashMap<String, crate::storage::FileRecord>,
        candidates: &HashMap<String, DiscoveredFile>,
        deletions: &HashSet<String>,
        visibility_recomputed: bool,
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
            visibility_recomputed,
        }
    }

    fn membership_changes(&self) -> Vec<String> {
        let mut paths = Vec::with_capacity(self.created.len() + self.deleted.len());
        paths.extend(self.created.iter().cloned());
        paths.extend(self.deleted.iter().cloned());
        paths
    }
}

impl LazyWorkerPool {
    fn new() -> Self {
        Self {
            pool: OnceLock::new(),
            init: Mutex::new(()),
        }
    }

    fn get_or_build(&self, workers: usize) -> Result<&ThreadPool> {
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
