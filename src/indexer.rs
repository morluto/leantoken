use std::collections::{HashMap, HashSet};
use std::fmt;
use std::fs;
use std::io::Read;
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant, UNIX_EPOCH};

use rayon::ThreadPool;
use rayon::prelude::*;
use tokio_util::sync::CancellationToken;

use crate::error::RetryableOperation;
use crate::model::{IndexReport, IndexResponse, IndexSkipReasonCounts};
use crate::parser::{self, ParseOutput};
use crate::repository::{
    DiscoveredFile, discover_files_with_limits_policy_and_filter, enforce_limit, slash_path,
    validate_relative,
};
use crate::storage::{ChunkInput, ImportInput, IndexedFile, ReferenceInput, Storage, SymbolInput};
use crate::text::{PreparedText, TextKind, hash_bytes};
use crate::{Config, Error, Result};

const INDEX_CONTENT_VERSION: u32 = 8;
#[cfg(test)]
const PREVIOUS_INDEX_CONTENT_MARKER: &str = "leantoken-index-v6-9-language";

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
    /// Import resolution and SQLite insertion time inside batch callbacks.
    pub insertion_ms: f64,
    /// Total lifetime of the generation publication transaction.
    pub publication_ms: f64,
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
    insertion: Duration,
    batches: usize,
    max_batch_files: usize,
    max_batch_source_bytes: u64,
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

impl Indexer {
    /// Construct an indexer whose dedicated worker pool is created on demand.
    pub fn new(config: Arc<Config>, storage: Storage) -> Result<Self> {
        Self::validate_config(&config)?;
        Ok(Self {
            config,
            storage,
            pool: Arc::new(LazyWorkerPool::new()),
        })
    }

    /// Reconcile filesystem state into one committed repository generation.
    pub fn reconcile(&self, rebuild: bool) -> Result<IndexResponse> {
        self.reconcile_report(rebuild)
            .map(IndexReport::into_response)
    }

    /// Reconcile filesystem state and include bounded preparation skip reasons.
    pub fn reconcile_report(&self, rebuild: bool) -> Result<IndexReport> {
        self.reconcile_profiled_report(rebuild)
            .map(|profiled| profiled.report)
    }

    /// Reconcile a full repository and return phase diagnostics for benchmarks.
    pub fn reconcile_profiled(&self, rebuild: bool) -> Result<ProfiledIndexResponse> {
        self.reconcile_profiled_report(rebuild)
            .map(|profiled| ProfiledIndexResponse {
                response: profiled.report.into_response(),
                diagnostics: profiled.diagnostics,
            })
    }

    /// Reconcile a full repository with additive details and phase diagnostics.
    pub fn reconcile_profiled_report(&self, rebuild: bool) -> Result<ProfiledIndexReport> {
        self.reconcile_cancellable_profiled_report(rebuild, &CancellationToken::new())
    }

    /// Reconcile the repository with cooperative cancellation and stale-plan retry.
    pub fn reconcile_cancellable(
        &self,
        rebuild: bool,
        cancellation: &CancellationToken,
    ) -> Result<IndexResponse> {
        self.reconcile_cancellable_report(rebuild, cancellation)
            .map(IndexReport::into_response)
    }

    /// Reconcile with cancellation and include bounded preparation skip reasons.
    pub fn reconcile_cancellable_report(
        &self,
        rebuild: bool,
        cancellation: &CancellationToken,
    ) -> Result<IndexReport> {
        self.reconcile_cancellable_profiled_report(rebuild, cancellation)
            .map(|profiled| profiled.report)
    }

    /// Reconcile a full repository with cancellation and phase diagnostics.
    pub fn reconcile_cancellable_profiled(
        &self,
        rebuild: bool,
        cancellation: &CancellationToken,
    ) -> Result<ProfiledIndexResponse> {
        self.reconcile_cancellable_profiled_report(rebuild, cancellation)
            .map(|profiled| ProfiledIndexResponse {
                response: profiled.report.into_response(),
                diagnostics: profiled.diagnostics,
            })
    }

    fn reconcile_cancellable_profiled_report(
        &self,
        rebuild: bool,
        cancellation: &CancellationToken,
    ) -> Result<ProfiledIndexReport> {
        for _ in 0..3 {
            match self.reconcile_once(rebuild, cancellation) {
                Err(Error::StaleReconciliation { .. }) => continue,
                result => return result,
            }
        }
        Err(Error::RetryableConflict(RetryableOperation::Reconciliation))
    }

    fn reconcile_once(
        &self,
        rebuild: bool,
        cancellation: &CancellationToken,
    ) -> Result<ProfiledIndexReport> {
        self.reconcile_once_with_preparation_hook(rebuild, cancellation, || {})
    }

    fn reconcile_once_with_preparation_hook(
        &self,
        rebuild: bool,
        cancellation: &CancellationToken,
        before_preparation: impl FnOnce(),
    ) -> Result<ProfiledIndexReport> {
        let total_started = Instant::now();
        check_cancelled(cancellation)?;
        let baseline = self.storage.meta()?;

        let discovery_started = Instant::now();
        let discovery = discover_files_with_limits_policy_and_filter(
            &self.config.root,
            self.config.discovery_limits(),
            self.config.discovery_policy(),
            cancellation,
            |path| !self.config.is_database_artifact_path(path),
        )?;
        let discovery_elapsed = discovery_started.elapsed();
        let discovery_stats = discovery.stats;
        tracing::debug!(
            walk_entries = discovery.stats.walk_entries,
            files = discovery.stats.files,
            total_source_bytes = discovery.stats.total_source_bytes,
            max_depth = discovery.stats.max_depth,
            "repository discovery completed"
        );
        let discovered = discovery.files;
        let planning_started = Instant::now();
        check_cancelled(cancellation)?;
        let existing = self.existing_files(cancellation)?;
        let config_hash = self.config_hash();
        let force = rebuild || baseline.config_hash != config_hash;

        let mut repository_paths = HashSet::with_capacity(discovered.len());
        for file in &discovered {
            check_cancelled(cancellation)?;
            repository_paths.insert(file.relative_path.clone());
        }
        let mut deletions = Vec::new();
        for path in existing.keys() {
            check_cancelled(cancellation)?;
            if !repository_paths.contains(path) {
                deletions.push(path.clone());
            }
        }

        let mut unchanged = 0usize;
        let mut candidates = Vec::new();
        for file in discovered {
            check_cancelled(cancellation)?;
            // mtime+size alone cannot prove content identity (bind mounts, copy
            // tools that preserve mtime, some network filesystems). Content-hash
            // before skipping so silent overwrites still reindex.
            if !force
                && let Some(record) = existing.get(&file.relative_path)
                && record.size_bytes == file.size_bytes
                && record.modified_ns == file.modified_ns
                && content_unchanged(
                    &file.absolute_path,
                    &record.content_hash,
                    self.config.max_file_bytes,
                )
            {
                unchanged += 1;
                continue;
            }
            candidates.push(file);
        }
        let planning_elapsed = planning_started.elapsed();

        let mut removed_paths = deletions.into_iter().collect::<HashSet<_>>();
        let mut warnings = Vec::new();
        let mut skip_reasons = IndexSkipReasonCounts::default();
        let mut files_indexed = 0usize;
        before_preparation();
        let publication_started = Instant::now();
        let (generation, preparation) =
            self.storage
                .publish_reconciliation_at(&baseline, &config_hash, rebuild, |writer| {
                    for path in &removed_paths {
                        writer.delete(path)?;
                    }
                    self.prepare_candidate_batches(&candidates, cancellation, |prepared| {
                        let mut indexed = Vec::with_capacity(prepared.len());
                        let mut source_token_counts = HashMap::with_capacity(prepared.len());
                        for result in prepared {
                            check_cancelled(cancellation)?;
                            match result {
                                PreparedFile::Indexed(file, source_token_count, warning) => {
                                    source_token_counts
                                        .insert(file.path.clone(), source_token_count);
                                    indexed.push(*file);
                                    if let Some(warning) = warning {
                                        push_warning(&mut warnings, warning);
                                    }
                                }
                                PreparedFile::Binary(path) => {
                                    skip_reasons.binary = skip_reasons.binary.saturating_add(1);
                                    if existing.contains_key(&path)
                                        && removed_paths.insert(path.clone())
                                    {
                                        writer.delete(&path)?;
                                    }
                                }
                                PreparedFile::Oversized(path) => {
                                    skip_reasons.oversized_during_read =
                                        skip_reasons.oversized_during_read.saturating_add(1);
                                    if existing.contains_key(&path)
                                        && removed_paths.insert(path.clone())
                                    {
                                        writer.delete(&path)?;
                                    }
                                }
                                PreparedFile::Failed(path, error) => {
                                    skip_reasons.failed = skip_reasons.failed.saturating_add(1);
                                    push_warning(&mut warnings, format!("{path}: {error}"));
                                }
                            }
                        }
                        resolve_imports(&mut indexed, &repository_paths, cancellation)?;
                        files_indexed = files_indexed.saturating_add(indexed.len());
                        for file in indexed {
                            check_cancelled(cancellation)?;
                            let source_token_count = source_token_counts
                                .remove(&file.path)
                                .expect("prepared file has a source token count");
                            writer.replace_with_source_tokens(
                                file,
                                self.config.tokenizer.name(),
                                source_token_count,
                            )?;
                        }
                        Ok(())
                    })
                })?;
        let publication_elapsed = publication_started.elapsed();

        check_cancelled(cancellation)?;
        let files_seen = unchanged + candidates.len();
        let files_removed = removed_paths.len();
        let files_skipped = skip_reasons.total();

        let response = IndexResponse {
            repository_generation: generation,
            files_seen,
            files_indexed,
            files_unchanged: unchanged,
            files_removed,
            files_skipped,
            warnings,
        };
        let report = IndexReport::with_skip_reasons(response, skip_reasons);
        let diagnostics = IndexingDiagnostics {
            total_ms: duration_ms(total_started.elapsed()),
            discovery_ms: duration_ms(discovery_elapsed),
            hash_and_plan_ms: duration_ms(planning_elapsed),
            preparation_ms: duration_ms(preparation.preparation),
            insertion_ms: duration_ms(preparation.insertion),
            publication_ms: duration_ms(publication_elapsed),
            preparation_batches: preparation.batches,
            max_batch_files: preparation.max_batch_files,
            max_batch_source_bytes: preparation.max_batch_source_bytes,
            walk_entries: discovery_stats.walk_entries,
            discovered_files: discovery_stats.files,
            discovered_source_bytes: discovery_stats.total_source_bytes,
        };
        tracing::debug!(
            total_ms = diagnostics.total_ms,
            discovery_ms = diagnostics.discovery_ms,
            hash_and_plan_ms = diagnostics.hash_and_plan_ms,
            preparation_ms = diagnostics.preparation_ms,
            insertion_ms = diagnostics.insertion_ms,
            publication_ms = diagnostics.publication_ms,
            preparation_batches = diagnostics.preparation_batches,
            max_batch_files = diagnostics.max_batch_files,
            max_batch_source_bytes = diagnostics.max_batch_source_bytes,
            "repository reconciliation profile"
        );
        Ok(ProfiledIndexReport {
            report,
            diagnostics,
        })
    }

    /// Reconcile watcher-reported paths without walking the full repository.
    ///
    /// Existing regular files and deletions are safe to apply directly. New
    /// paths, directories, symlinks, and ignore-rule changes fall back to a
    /// full reconciliation because they can affect files beyond the reported
    /// path.
    pub fn reconcile_paths(&self, paths: &[String]) -> Result<IndexResponse> {
        self.reconcile_paths_report(paths)
            .map(IndexReport::into_response)
    }

    /// Reconcile watcher paths and include bounded preparation skip reasons.
    pub fn reconcile_paths_report(&self, paths: &[String]) -> Result<IndexReport> {
        self.reconcile_paths_cancellable_report(paths, &CancellationToken::new())
    }

    /// Reconcile watcher paths with cooperative cancellation and stale-plan retry.
    pub fn reconcile_paths_cancellable(
        &self,
        paths: &[String],
        cancellation: &CancellationToken,
    ) -> Result<IndexResponse> {
        self.reconcile_paths_cancellable_report(paths, cancellation)
            .map(IndexReport::into_response)
    }

    /// Reconcile watcher paths with cancellation and preparation skip reasons.
    pub fn reconcile_paths_cancellable_report(
        &self,
        paths: &[String],
        cancellation: &CancellationToken,
    ) -> Result<IndexReport> {
        for _ in 0..3 {
            match self.reconcile_paths_once(paths, cancellation) {
                Err(Error::StaleReconciliation { .. }) => continue,
                result => return result,
            }
        }
        Err(Error::RetryableConflict(RetryableOperation::Reconciliation))
    }

    fn reconcile_paths_once(
        &self,
        paths: &[String],
        cancellation: &CancellationToken,
    ) -> Result<IndexReport> {
        self.reconcile_paths_once_with_preparation_hook(paths, cancellation, || {})
    }

    fn reconcile_paths_once_with_preparation_hook(
        &self,
        paths: &[String],
        cancellation: &CancellationToken,
        before_preparation: impl FnOnce(),
    ) -> Result<IndexReport> {
        self.reconcile_paths_once_with_hooks(paths, cancellation, || {}, before_preparation)
    }

    fn observe_visibility_delta(
        &self,
        paths: &[String],
        existing: &HashMap<String, crate::storage::FileRecord>,
    ) -> (bool, HashSet<String>) {
        let mut visibility_delta = false;
        let mut observed_deletions = HashSet::new();
        for requested in paths {
            let relative = Path::new(requested);
            let relative_path = slash_path(relative);
            visibility_delta |= self
                .config
                .discovery_policy()
                .is_ignore_control_path(&relative_path);
            let absolute = self.config.root.join(relative);
            match fs::symlink_metadata(&absolute) {
                Ok(metadata) => {
                    let is_file = metadata.file_type().is_file();
                    visibility_delta |= !existing.contains_key(&relative_path) || !is_file;
                    if !is_file {
                        if existing.contains_key(&relative_path) {
                            observed_deletions.insert(relative_path.clone());
                        }
                        if !metadata.file_type().is_dir() {
                            let prefix = format!("{relative_path}/");
                            observed_deletions.extend(
                                existing
                                    .keys()
                                    .filter(|path| path.starts_with(&prefix))
                                    .cloned(),
                            );
                        }
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    let prefix = format!("{relative_path}/");
                    let observed = existing
                        .keys()
                        .filter(|path| *path == &relative_path || path.starts_with(&prefix))
                        .cloned()
                        .collect::<Vec<_>>();
                    visibility_delta |= observed.iter().any(|path| path.starts_with(&prefix));
                    observed_deletions.extend(observed);
                }
                Err(_) => {}
            }
        }
        if !visibility_delta {
            observed_deletions.clear();
        }
        (visibility_delta, observed_deletions)
    }

    fn reconcile_paths_once_with_hooks(
        &self,
        paths: &[String],
        cancellation: &CancellationToken,
        after_discovery: impl FnOnce(),
        before_preparation: impl FnOnce(),
    ) -> Result<IndexReport> {
        check_cancelled(cancellation)?;
        let baseline = self.storage.meta()?;
        let config_hash = self.config_hash();
        if baseline.config_hash != config_hash {
            return self.reconcile_cancellable_report(true, cancellation);
        }

        let existing = self.existing_files(cancellation)?;
        let mut repository_paths = HashSet::with_capacity(existing.len());
        for path in existing.keys() {
            check_cancelled(cancellation)?;
            repository_paths.insert(path.clone());
        }
        let mut unique = HashSet::with_capacity(paths.len());
        for path in paths {
            check_cancelled(cancellation)?;
            unique.insert(slash_path(&validate_relative(path)?));
        }
        let mut paths = unique.drain().collect::<Vec<_>>();
        check_cancelled(cancellation)?;
        paths.sort_unstable();
        check_cancelled(cancellation)?;

        // Preserve targeted deletion evidence from the observation that triggers discovery.
        let (visibility_delta, visibility_observed_deletions) =
            self.observe_visibility_delta(&paths, &existing);
        let discovered = visibility_delta
            .then(|| {
                discover_files_with_limits_policy_and_filter(
                    &self.config.root,
                    self.config.discovery_limits(),
                    self.config.discovery_policy(),
                    cancellation,
                    |path| !self.config.is_database_artifact_path(path),
                )
                .map(|discovery| discovery.files)
            })
            .transpose()?;
        let discovered_by_path = discovered.as_ref().map(|files| {
            files
                .iter()
                .cloned()
                .map(|file| (file.relative_path.clone(), file))
                .collect::<HashMap<_, _>>()
        });
        after_discovery();

        let mut candidates = HashMap::new();
        let mut deletions = HashSet::new();
        let mut directly_observed_deletions = visibility_observed_deletions;
        let mut unchanged = 0usize;
        if let Some(discovered) = &discovered_by_path {
            for (path, file) in discovered {
                check_cancelled(cancellation)?;
                if !existing.contains_key(path) || paths.binary_search(path).is_ok() {
                    candidates.insert(path.clone(), file.clone());
                }
            }
            for path in existing.keys() {
                check_cancelled(cancellation)?;
                if !discovered.contains_key(path) {
                    deletions.insert(path.clone());
                }
            }
        } else {
            for requested in &paths {
                check_cancelled(cancellation)?;
                let relative = validate_relative(requested)?;
                let relative_path = slash_path(&relative);
                enforce_limit(
                    crate::IndexLimitKind::Depth,
                    u64::try_from(relative.components().count()).unwrap_or(u64::MAX),
                    u64::try_from(self.config.max_depth).unwrap_or(u64::MAX),
                )?;
                let absolute_path = self.config.root.join(&relative);
                if self.config.is_database_artifact_path(&absolute_path) {
                    continue;
                }
                let metadata = match fs::symlink_metadata(&absolute_path) {
                    Ok(metadata) => metadata,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                        if existing.contains_key(&relative_path) {
                            directly_observed_deletions.insert(relative_path.clone());
                            deletions.insert(relative_path);
                        }
                        continue;
                    }
                    Err(error) => return Err(error.into()),
                };
                if metadata.len() > self.config.max_file_bytes {
                    deletions.insert(relative_path);
                    continue;
                }
                let modified_ns = metadata
                    .modified()
                    .ok()
                    .and_then(|value| value.duration_since(UNIX_EPOCH).ok())
                    .map(|duration| duration.as_nanos());
                candidates.insert(
                    relative_path.clone(),
                    DiscoveredFile {
                        absolute_path,
                        relative_path,
                        size_bytes: metadata.len(),
                        modified_ns,
                    },
                );
            }
        }

        let change_set = ChangeSet::classify(&existing, &candidates, &deletions, visibility_delta);
        debug_assert_eq!(
            change_set.modified.len() + change_set.created.len(),
            candidates.len()
        );
        debug_assert_eq!(change_set.visibility_recomputed, visibility_delta);

        for deletion in &change_set.deleted {
            repository_paths.remove(deletion);
        }
        repository_paths.extend(candidates.keys().cloned());
        let forced_importers =
            self.add_affected_importers(&mut candidates, &deletions, &change_set, cancellation)?;
        self.validate_membership_limits(&existing, &candidates, &deletions, cancellation)?;
        directly_observed_deletions.retain(|path| deletions.contains(path));
        debug_assert!(directly_observed_deletions.is_subset(&deletions));

        let files_seen = candidates.len() + directly_observed_deletions.len();
        let candidates = candidates.into_values().collect::<Vec<_>>();
        let mut warnings = Vec::new();
        let mut skip_reasons = IndexSkipReasonCounts::default();
        let mut files_indexed = 0usize;
        before_preparation();
        let (generation, _preparation) =
            self.storage
                .publish_reconciliation_at(&baseline, &config_hash, false, |writer| {
                    for path in &deletions {
                        writer.delete(path)?;
                    }
                    self.prepare_candidate_batches(&candidates, cancellation, |prepared| {
                        let mut indexed = Vec::with_capacity(prepared.len());
                        let mut source_token_counts = HashMap::with_capacity(prepared.len());
                        for result in prepared {
                            check_cancelled(cancellation)?;
                            match result {
                                PreparedFile::Indexed(file, source_token_count, warning) => {
                                    let same = existing.get(&file.path).is_some_and(|record| {
                                        record.content_hash == file.content_hash
                                            && record.size_bytes == file.size_bytes
                                            && record.modified_ns == file.modified_ns
                                    });
                                    if same && !forced_importers.contains(&file.path) {
                                        unchanged += 1;
                                        continue;
                                    }
                                    source_token_counts
                                        .insert(file.path.clone(), source_token_count);
                                    indexed.push(*file);
                                    if let Some(warning) = warning {
                                        push_warning(&mut warnings, warning);
                                    }
                                }
                                PreparedFile::Binary(path) => {
                                    skip_reasons.binary = skip_reasons.binary.saturating_add(1);
                                    if existing.contains_key(&path)
                                        && deletions.insert(path.clone())
                                    {
                                        writer.delete(&path)?;
                                    }
                                }
                                PreparedFile::Oversized(path) => {
                                    skip_reasons.oversized_during_read =
                                        skip_reasons.oversized_during_read.saturating_add(1);
                                    if existing.contains_key(&path)
                                        && deletions.insert(path.clone())
                                    {
                                        writer.delete(&path)?;
                                    }
                                }
                                PreparedFile::Failed(path, error) => {
                                    skip_reasons.failed = skip_reasons.failed.saturating_add(1);
                                    push_warning(&mut warnings, format!("{path}: {error}"));
                                }
                            }
                        }
                        resolve_imports(&mut indexed, &repository_paths, cancellation)?;
                        files_indexed = files_indexed.saturating_add(indexed.len());
                        for file in indexed {
                            check_cancelled(cancellation)?;
                            let source_token_count = source_token_counts
                                .remove(&file.path)
                                .expect("prepared file has a source token count");
                            writer.replace_with_source_tokens(
                                file,
                                self.config.tokenizer.name(),
                                source_token_count,
                            )?;
                        }
                        Ok(())
                    })
                })?;
        check_cancelled(cancellation)?;
        let files_removed = deletions.len();
        let files_skipped = skip_reasons.total();

        let response = IndexResponse {
            repository_generation: generation,
            files_seen,
            files_indexed,
            files_unchanged: unchanged,
            files_removed,
            files_skipped,
            warnings,
        };
        Ok(IndexReport::with_skip_reasons(response, skip_reasons))
    }

    fn validate_config(config: &Config) -> Result<()> {
        config.validate()
    }

    fn prepare_candidate_batches(
        &self,
        candidates: &[DiscoveredFile],
        cancellation: &CancellationToken,
        mut consume: impl FnMut(Vec<PreparedFile>) -> Result<()>,
    ) -> Result<PreparationMetrics> {
        check_cancelled(cancellation)?;
        if candidates.is_empty() {
            return Ok(PreparationMetrics::default());
        }

        // One lazy pool per Services/cache preserves that instance's
        // configured worker bound without allocating threads in followers.
        let pool = self
            .pool
            .get_or_build(self.config.max_index_workers.max(1))?;
        let chunk_lines = self.config.chunk_lines;
        let chunk_bytes = self.config.chunk_bytes;
        let tokenizer = self.config.tokenizer;
        let limits = self.config.discovery_limits();
        let mut metrics = PreparationMetrics::default();
        let mut start = 0usize;
        while start < candidates.len() {
            check_cancelled(cancellation)?;
            let end = prepare_batch_end(candidates, start, limits);
            debug_assert!(end > start, "validated limits admit at least one file");
            let batch_source_bytes = candidates[start..end]
                .iter()
                .fold(0u64, |total, file| total.saturating_add(file.size_bytes));
            metrics.batches = metrics.batches.saturating_add(1);
            metrics.max_batch_files = metrics.max_batch_files.max(end - start);
            metrics.max_batch_source_bytes = metrics.max_batch_source_bytes.max(batch_source_bytes);
            let preparation_started = Instant::now();
            let batch = pool.install(|| {
                candidates[start..end]
                    .par_iter()
                    .map(|file| {
                        check_cancelled(cancellation)?;
                        let prepared = prepare_file(
                            file,
                            chunk_lines,
                            chunk_bytes,
                            tokenizer,
                            limits.max_file_bytes,
                            cancellation,
                        )?;
                        check_cancelled(cancellation)?;
                        Ok(prepared)
                    })
                    .collect::<Result<Vec<_>>>()
            })?;
            metrics.preparation += preparation_started.elapsed();
            let insertion_started = Instant::now();
            consume(batch)?;
            metrics.insertion += insertion_started.elapsed();
            start = end;
        }
        Ok(metrics)
    }

    fn add_affected_importers(
        &self,
        candidates: &mut HashMap<String, DiscoveredFile>,
        deletions: &HashSet<String>,
        change_set: &ChangeSet,
        cancellation: &CancellationToken,
    ) -> Result<HashSet<String>> {
        let membership_changes = change_set.membership_changes();
        let mut forced_importers = HashSet::new();
        for importer_path in self.storage.affected_importers(&membership_changes)? {
            check_cancelled(cancellation)?;
            if deletions.contains(&importer_path) {
                continue;
            }
            forced_importers.insert(importer_path.clone());
            if candidates.contains_key(&importer_path) {
                continue;
            }
            let absolute_path = self.config.root.join(&importer_path);
            let metadata = fs::symlink_metadata(&absolute_path)?;
            if !metadata.file_type().is_file() || metadata.len() > self.config.max_file_bytes {
                continue;
            }
            let modified_ns = metadata
                .modified()
                .ok()
                .and_then(|value| value.duration_since(UNIX_EPOCH).ok())
                .map(|duration| duration.as_nanos());
            candidates.insert(
                importer_path.clone(),
                DiscoveredFile {
                    absolute_path,
                    relative_path: importer_path,
                    size_bytes: metadata.len(),
                    modified_ns,
                },
            );
        }
        Ok(forced_importers)
    }

    fn validate_membership_limits(
        &self,
        existing: &HashMap<String, crate::storage::FileRecord>,
        candidates: &HashMap<String, DiscoveredFile>,
        deletions: &HashSet<String>,
        cancellation: &CancellationToken,
    ) -> Result<()> {
        let limits = self.config.discovery_limits();
        let mut files = 0u64;
        let mut total_source_bytes = 0u64;
        let mut admit = |size_bytes: u64| -> Result<()> {
            files = files.saturating_add(1);
            enforce_limit(crate::IndexLimitKind::Files, files, limits.max_files)?;
            total_source_bytes = total_source_bytes.saturating_add(size_bytes);
            enforce_limit(
                crate::IndexLimitKind::TotalSourceBytes,
                total_source_bytes,
                limits.max_total_source_bytes,
            )
        };

        for (path, record) in existing {
            check_cancelled(cancellation)?;
            if !deletions.contains(path) && !candidates.contains_key(path) {
                admit(record.size_bytes)?;
            }
        }
        for candidate in candidates.values() {
            check_cancelled(cancellation)?;
            admit(candidate.size_bytes)?;
        }
        Ok(())
    }

    fn existing_files(
        &self,
        cancellation: &CancellationToken,
    ) -> Result<HashMap<String, crate::storage::FileRecord>> {
        let mut result = HashMap::new();
        let mut cursor = None;
        loop {
            check_cancelled(cancellation)?;
            let page = self.storage.list_files(1_000, cursor)?;
            if page.is_empty() {
                break;
            }
            cursor = page.last().map(|file| file.id);
            for file in page {
                check_cancelled(cancellation)?;
                result.insert(file.path.clone(), file);
            }
        }
        Ok(result)
    }

    fn config_hash(&self) -> String {
        self.config_hash_for_content_marker(&format!(
            "leantoken-index-content-v{INDEX_CONTENT_VERSION}"
        ))
    }

    fn config_hash_for_content_marker(&self, index_content_marker: &str) -> String {
        let input = format!(
            "{index_content_marker}\0{}\0{}\0{}\0{}\0{}\0{}\0{}\0{}\0{}\0{}\0{}\0{}",
            env!("CARGO_PKG_VERSION"),
            self.config.max_walk_entries,
            self.config.max_files,
            self.config.max_total_source_bytes,
            self.config.max_depth,
            self.config.max_file_bytes,
            self.config.max_prepare_batch_files,
            self.config.max_prepare_batch_bytes,
            self.config.include_generated,
            self.config.chunk_lines,
            self.config.chunk_bytes,
            self.config.tokenizer.name()
        );
        blake3::hash(input.as_bytes()).to_hex().to_string()
    }
}

fn prepare_batch_end(
    candidates: &[DiscoveredFile],
    start: usize,
    limits: crate::DiscoveryLimits,
) -> usize {
    let mut end = start;
    let mut batch_bytes = 0u64;
    while end < candidates.len() && end - start < limits.max_prepare_batch_files {
        let observed = batch_bytes.saturating_add(candidates[end].size_bytes);
        if observed > limits.max_prepare_batch_bytes {
            break;
        }
        batch_bytes = observed;
        end += 1;
    }
    end
}

fn duration_ms(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1_000.0
}

fn check_cancelled(cancellation: &CancellationToken) -> Result<()> {
    if cancellation.is_cancelled() {
        Err(Error::Cancelled)
    } else {
        Ok(())
    }
}

enum PreparedFile {
    Indexed(Box<IndexedFile>, usize, Option<String>),
    Binary(String),
    Oversized(String),
    Failed(String, String),
}

fn prepare_file(
    file: &DiscoveredFile,
    chunk_lines: usize,
    chunk_bytes: usize,
    tokenizer: crate::tokens::Tokenizer,
    max_file_bytes: u64,
    cancellation: &CancellationToken,
) -> Result<PreparedFile> {
    let bytes = match read_bounded(&file.absolute_path, max_file_bytes) {
        Ok(Some(bytes)) => bytes,
        Ok(None) => return Ok(PreparedFile::Oversized(file.relative_path.clone())),
        Err(error) => {
            return Ok(PreparedFile::Failed(
                file.relative_path.clone(),
                error.to_string(),
            ));
        }
    };
    let prepared = PreparedText::from_bytes(&bytes, chunk_lines, chunk_bytes);
    if prepared.kind == TextKind::Binary {
        return Ok(PreparedFile::Binary(file.relative_path.clone()));
    }

    let (parsed, warning) =
        match parser::parse_with_cancellation(&file.relative_path, &prepared.content, cancellation)
        {
            Ok(parsed) => (parsed, None),
            Err(Error::Cancelled) => return Err(Error::Cancelled),
            Err(error) => (
                ParseOutput {
                    language: parser::language_by_path(&file.relative_path),
                    structurally_complete: false,
                    symbols: Vec::new(),
                    references: Vec::new(),
                    imports: Vec::new(),
                },
                Some(format!(
                    "{}: structural parse failed; text remains searchable: {error}",
                    file.relative_path
                )),
            ),
        };

    let source_token_count = tokenizer.count(&prepared.content);
    let chunks = prepared
        .chunks
        .into_iter()
        .map(|chunk| ChunkInput {
            token_count: tokenizer.count(&chunk.content),
            content: chunk.content,
            start_line: chunk.start_line,
            end_line: chunk.end_line,
            start_byte: chunk.start_byte,
            end_byte: chunk.end_byte,
        })
        .collect();
    let symbols = parsed
        .symbols
        .into_iter()
        .map(|symbol| SymbolInput {
            name: symbol.name,
            kind: symbol.kind,
            parent: symbol.parent,
            signature: symbol.signature,
            start_line: symbol.start_line,
            end_line: symbol.end_line,
            start_byte: symbol.start_byte,
            end_byte: symbol.end_byte,
        })
        .collect();
    let references = parsed
        .references
        .into_iter()
        .map(|reference| ReferenceInput {
            name: reference.name,
            kind: reference.kind,
            role: reference.role,
            enclosing_symbol: reference.enclosing_symbol,
            start_line: reference.start_line,
            end_line: reference.end_line,
            start_byte: reference.start_byte,
            end_byte: reference.end_byte,
        })
        .collect();
    let imports = parsed
        .imports
        .into_iter()
        .map(|import| ImportInput {
            raw_target: import.raw_target,
            resolved_path: import.resolved_path,
            candidate_paths: Vec::new(),
            line: import.line,
        })
        .collect();

    Ok(PreparedFile::Indexed(
        Box::new(IndexedFile {
            path: file.relative_path.clone(),
            language: parsed.language,
            structurally_complete: parsed.structurally_complete,
            size_bytes: file.size_bytes,
            modified_ns: file.modified_ns,
            content_hash: hash_bytes(&bytes),
            chunks,
            symbols,
            references,
            imports,
        }),
        source_token_count,
        warning,
    ))
}

fn read_bounded(path: &Path, max_bytes: u64) -> std::io::Result<Option<Vec<u8>>> {
    let file = fs::File::open(path)?;
    let mut bytes =
        Vec::with_capacity(usize::try_from(max_bytes.min(64 * 1024)).unwrap_or(64 * 1024));
    file.take(max_bytes.saturating_add(1))
        .read_to_end(&mut bytes)?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > max_bytes {
        Ok(None)
    } else {
        Ok(Some(bytes))
    }
}

fn push_warning(warnings: &mut Vec<String>, warning: String) {
    const MAX_WARNINGS: usize = 100;
    if warnings.len() < MAX_WARNINGS {
        warnings.push(warning);
    }
}

/// Return whether the on-disk file still matches the indexed content hash.
///
/// Used when size and mtime look unchanged so full reconcile cannot skip a
/// content rewrite that preserved those metadata fields.
fn content_unchanged(path: &Path, expected_hash: &str, max_file_bytes: u64) -> bool {
    match read_bounded(path, max_file_bytes) {
        Ok(Some(bytes)) => hash_bytes(&bytes) == expected_hash,
        Err(_) => false,
        Ok(None) => false,
    }
}

fn resolve_imports(
    files: &mut [IndexedFile],
    repository_paths: &HashSet<String>,
    cancellation: &CancellationToken,
) -> Result<()> {
    for file in files {
        check_cancelled(cancellation)?;
        for import in &mut file.imports {
            check_cancelled(cancellation)?;
            import.candidate_paths = import_candidates(&file.path, &import.raw_target);
            import.resolved_path =
                resolve_import_candidates(&import.candidate_paths, repository_paths);
        }
    }
    Ok(())
}

#[cfg(test)]
fn resolve_import(
    source_path: &str,
    raw_target: &str,
    repository_paths: &HashSet<String>,
) -> Option<String> {
    resolve_import_candidates(
        &import_candidates(source_path, raw_target),
        repository_paths,
    )
}

fn import_candidates(source_path: &str, raw_target: &str) -> Vec<String> {
    let source = std::path::Path::new(source_path);
    let parent = source.parent().unwrap_or_else(|| std::path::Path::new(""));
    let mut bases = Vec::new();

    if raw_target.starts_with('.') {
        bases.push(parent.join(raw_target));
    } else if matches!(
        source.extension().and_then(|ext| ext.to_str()),
        Some("py" | "pyi")
    ) {
        bases.push(parent.join(raw_target.replace('.', "/")));
    } else if source.extension().and_then(|ext| ext.to_str()) == Some("rs") {
        bases.extend(rust_module_paths(raw_target));
    } else {
        return Vec::new();
    }

    let init_file = match source.extension().and_then(|ext| ext.to_str()) {
        Some("py" | "pyi") => Some("__init__"),
        Some("rs") => Some("mod"),
        _ => None,
    };

    let extensions: &[&str] = match source.extension().and_then(|ext| ext.to_str()) {
        Some("js" | "mjs" | "cjs") => &["", "js", "mjs", "cjs"],
        Some("ts" | "mts" | "cts" | "tsx") => &["", "ts", "tsx", "mts", "cts", "js"],
        Some("py" | "pyi") => &["", "py", "pyi"],
        Some("rs") => &["", "rs"],
        _ => &[""],
    };
    let mut matches = Vec::new();
    for base in bases {
        let Some(base) = normalize_relative(&base) else {
            continue;
        };
        for extension in extensions {
            let exact = if extension.is_empty() || base.extension().is_some() {
                base.clone()
            } else {
                base.with_extension(extension)
            };
            let directory_init = match init_file {
                Some(init) if extension.is_empty() => base.join(init),
                Some(init) => base.join(init).with_extension(extension),
                None if extension.is_empty() => base.join("index"),
                None => base.join("index").with_extension(extension),
            };
            for candidate in [exact, directory_init] {
                let candidate = if candidate.extension().is_some() || extension.is_empty() {
                    candidate
                } else {
                    candidate.with_extension(extension)
                };
                let candidate = candidate.to_string_lossy().replace('\\', "/");
                if !matches.contains(&candidate) {
                    matches.push(candidate);
                }
            }
        }
    }
    matches
}

/// Decompose a raw Rust `use` target into candidate module paths.
///
/// Handles grouped imports (`a::{b, c}`), aliases (`a::b as c`), and
/// leading path qualifiers (`crate`, `self`, `super`). For each concrete
/// target, all module prefixes are tried from longest to shortest so that
/// `module::symbol` resolves to `module` when the full `module/symbol`
/// path does not exist.
fn rust_module_paths(raw_target: &str) -> Vec<std::path::PathBuf> {
    let trimmed = raw_target.trim();
    let stripped = trimmed
        .strip_prefix("crate::")
        .or_else(|| trimmed.strip_prefix("self::"))
        .or_else(|| trimmed.strip_prefix("super::"))
        .unwrap_or(trimmed);

    let mut targets = Vec::new();
    let before_brace = stripped
        .split('{')
        .next()
        .unwrap_or("")
        .trim_end_matches(':');
    let group_body = stripped.find('{').and_then(|_| {
        stripped
            .split('{')
            .nth(1)
            .and_then(|rest| rest.split('}').next())
    });

    if let Some(group) = group_body {
        let prefix = before_brace.trim_end_matches(':');
        for item in group.split(',') {
            let item = item.trim();
            let item = item.split(" as ").next().unwrap_or(item).trim();
            if item.is_empty() {
                continue;
            }
            let full = if prefix.is_empty() {
                item.to_string()
            } else {
                format!("{prefix}::{item}")
            };
            targets.push(full);
        }
    } else {
        let single = stripped.split(" as ").next().unwrap_or(stripped).trim();
        if !single.is_empty() {
            targets.push(single.to_string());
        }
    }

    let mut bases = Vec::new();
    for target in targets {
        let segments: Vec<&str> = target.split("::").filter(|s| !s.is_empty()).collect();
        if segments.is_empty() {
            continue;
        }
        for prefix_len in (1..=segments.len()).rev() {
            let path_str = segments[..prefix_len].join("/");
            bases.push(std::path::PathBuf::from(&path_str));
            bases.push(std::path::PathBuf::from("src").join(path_str));
        }
    }
    bases
}

fn resolve_import_candidates(
    candidates: &[String],
    repository_paths: &HashSet<String>,
) -> Option<String> {
    // Candidates are ordered from most-specific to least-specific. Return the
    // first existing candidate; a more specific match always wins over a
    // shorter prefix fallback. This preserves the conservative contract for
    // same-priority candidates (e.g. exact file vs directory init) while
    // allowing module prefix fallback for Rust imports.
    for candidate in candidates {
        if repository_paths.contains(candidate) {
            return Some(candidate.clone());
        }
    }
    None
}

fn normalize_relative(path: &std::path::Path) -> Option<std::path::PathBuf> {
    let mut normalized = std::path::PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::Normal(part) => normalized.push(part),
            std::path::Component::ParentDir => {
                if !normalized.pop() {
                    return None;
                }
            }
            std::path::Component::RootDir | std::path::Component::Prefix(_) => return None,
        }
    }
    Some(normalized)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_skip_reasons(
        response: &IndexReport,
        binary: usize,
        oversized_during_read: usize,
        failed: usize,
    ) {
        assert_eq!(
            response.skip_reasons.as_ref(),
            Some(&IndexSkipReasonCounts {
                binary,
                oversized_during_read,
                failed,
            })
        );
        assert_eq!(
            response.files_skipped,
            response
                .skip_reasons
                .as_ref()
                .expect("current skip reasons")
                .total()
        );
    }

    #[test]
    fn visibility_reconcile_does_not_reclassify_exclusion_after_discovery() {
        let root = tempfile::tempdir().expect("root");
        fs::create_dir(root.path().join(".git")).expect("git marker");
        let excluded_path = root.path().join("excluded.rs");
        fs::write(&excluded_path, "fn excluded() {}\n").expect("source fixture");
        fs::write(root.path().join(".gitignore"), "").expect("initial ignore");
        let config = Arc::new(
            Config::discover(root.path(), Some(root.path().join("index.sqlite"))).expect("config"),
        );
        let storage = Storage::open(&config.database_path).expect("storage");
        let indexer = Indexer::new(config, storage.clone()).expect("indexer");
        indexer.reconcile(false).expect("initial reconcile");
        fs::write(root.path().join(".gitignore"), "excluded.rs\n").expect("exclude source");

        let response = indexer
            .reconcile_paths_once_with_hooks(
                &[".gitignore".into()],
                &CancellationToken::new(),
                || fs::remove_file(&excluded_path).expect("remove after discovery"),
                || {},
            )
            .expect("visibility reconcile");

        assert_eq!(response.files_seen, 1);
        assert_eq!(response.files_indexed, 1);
        assert_eq!(response.files_removed, 1);
        assert!(storage.find_file("excluded.rs").expect("lookup").is_none());
    }

    #[test]
    fn full_reconcile_counts_every_preparation_skip_reason() {
        let root = tempfile::tempdir().expect("root");
        let indexed_path = root.path().join("indexed.rs");
        let binary_path = root.path().join("binary.rs");
        let growing_path = root.path().join("growing.rs");
        let failed_path = root.path().join("failed.rs");
        fs::write(&indexed_path, "fn indexed() {}\n").expect("indexed fixture");
        fs::write(&binary_path, b"\0binary").expect("binary fixture");
        fs::write(&growing_path, "fn growing() {}\n").expect("growing fixture");
        fs::write(&failed_path, "fn failed() {}\n").expect("failed fixture");

        let mut config =
            Config::discover(root.path(), Some(root.path().join("index.sqlite"))).expect("config");
        config.max_file_bytes = 64;
        let storage = Storage::open(&config.database_path).expect("storage");
        let indexer = Indexer::new(Arc::new(config), storage.clone()).expect("indexer");

        let response = indexer
            .reconcile_once_with_preparation_hook(false, &CancellationToken::new(), move || {
                fs::write(growing_path, vec![b'x'; 65]).expect("grow after discovery");
                fs::remove_file(failed_path).expect("remove after discovery");
            })
            .expect("full reconcile")
            .report;

        assert_eq!(response.files_seen, 4);
        assert_eq!(response.files_indexed, 1);
        assert_eq!(response.files_unchanged, 0);
        assert_eq!(response.files_removed, 0);
        assert_skip_reasons(&response, 1, 1, 1);
        assert_eq!(response.warnings.len(), 1);
        assert!(response.warnings[0].starts_with("failed.rs: "));
        assert!(storage.find_file("indexed.rs").expect("indexed").is_some());
        assert!(storage.find_file("binary.rs").expect("binary").is_none());
        assert!(storage.find_file("growing.rs").expect("growing").is_none());
        assert!(storage.find_file("failed.rs").expect("failed").is_none());
    }

    #[test]
    fn incremental_reconcile_counts_every_preparation_skip_reason() {
        let root = tempfile::tempdir().expect("root");
        let indexed_path = root.path().join("indexed.rs");
        let binary_path = root.path().join("binary.rs");
        let growing_path = root.path().join("growing.rs");
        let failed_path = root.path().join("failed.rs");
        for path in [&indexed_path, &binary_path, &growing_path, &failed_path] {
            fs::write(path, "fn original() {}\n").expect("initial fixture");
        }

        let mut config =
            Config::discover(root.path(), Some(root.path().join("index.sqlite"))).expect("config");
        config.max_file_bytes = 64;
        let storage = Storage::open(&config.database_path).expect("storage");
        let indexer = Indexer::new(Arc::new(config), storage.clone()).expect("indexer");
        indexer.reconcile(false).expect("initial reconcile");

        fs::write(&indexed_path, "fn replacement() {}\n").expect("indexed replacement");
        fs::write(&binary_path, b"\0binary").expect("binary replacement");
        fs::write(&growing_path, "fn changed_growing() {}\n").expect("growing replacement");
        fs::write(&failed_path, "fn changed_failed() {}\n").expect("failed replacement");
        let paths = vec![
            "indexed.rs".into(),
            "binary.rs".into(),
            "growing.rs".into(),
            "failed.rs".into(),
        ];
        let response = indexer
            .reconcile_paths_once_with_preparation_hook(
                &paths,
                &CancellationToken::new(),
                move || {
                    fs::write(growing_path, vec![b'x'; 65]).expect("grow after admission");
                    fs::remove_file(failed_path).expect("remove after admission");
                },
            )
            .expect("incremental reconcile");

        assert_eq!(response.files_seen, 4);
        assert_eq!(response.files_indexed, 1);
        assert_eq!(response.files_unchanged, 0);
        assert_eq!(response.files_removed, 2);
        assert_skip_reasons(&response, 1, 1, 1);
        assert_eq!(response.warnings.len(), 1);
        assert!(response.warnings[0].starts_with("failed.rs: "));
        assert!(storage.find_file("indexed.rs").expect("indexed").is_some());
        assert!(storage.find_file("binary.rs").expect("binary").is_none());
        assert!(storage.find_file("growing.rs").expect("growing").is_none());
        assert!(storage.find_file("failed.rs").expect("failed").is_some());
    }

    #[test]
    fn conservative_import_resolution_requires_one_existing_file() {
        let paths = [
            "src/app.ts".to_string(),
            "src/lib.ts".to_string(),
            "src/pkg/index.ts".to_string(),
            "pkg/helpers.py".to_string(),
            "pkg/main.py".to_string(),
        ]
        .into_iter()
        .collect();
        assert_eq!(
            resolve_import("src/app.ts", "./lib", &paths).as_deref(),
            Some("src/lib.ts")
        );
        assert_eq!(
            resolve_import("pkg/main.py", "helpers", &paths).as_deref(),
            Some("pkg/helpers.py")
        );
        assert_eq!(
            resolve_import("src/app.ts", "./pkg", &paths).as_deref(),
            Some("src/pkg/index.ts")
        );
        assert!(resolve_import("src/app.ts", "external-package", &paths).is_none());
    }

    #[test]
    fn rust_module_symbol_resolves_to_module_file() {
        let paths = ["target.rs".to_string(), "consumer.rs".to_string()]
            .into_iter()
            .collect();
        assert_eq!(
            resolve_import("consumer.rs", "target::item", &paths).as_deref(),
            Some("target.rs")
        );
    }

    #[test]
    fn rust_grouped_import_resolves_to_module_file() {
        let paths = ["target.rs".to_string(), "consumer.rs".to_string()]
            .into_iter()
            .collect();
        assert_eq!(
            resolve_import("consumer.rs", "target::{foo, bar}", &paths).as_deref(),
            Some("target.rs")
        );
    }

    #[test]
    fn rust_aliased_import_resolves_to_module_file() {
        let paths = ["foo.rs".to_string(), "crate_import.rs".to_string()]
            .into_iter()
            .collect();
        assert_eq!(
            resolve_import("crate_import.rs", "crate::foo::{bar as b}", &paths).as_deref(),
            Some("foo.rs")
        );
    }

    #[test]
    fn rust_nested_module_resolves_before_symbol_fallback() {
        let paths = ["src/foo/bar.rs".to_string(), "src/foo.rs".to_string()]
            .into_iter()
            .collect();
        // Full path src/foo/bar.rs exists, so it wins over the shorter prefix.
        assert_eq!(
            resolve_import("src/app.rs", "foo::bar", &paths).as_deref(),
            Some("src/foo/bar.rs")
        );
    }

    #[test]
    fn rust_mod_rs_resolves_for_directory_module() {
        let paths = ["src/pkg/mod.rs".to_string(), "src/app.rs".to_string()]
            .into_iter()
            .collect();
        assert_eq!(
            resolve_import("src/app.rs", "pkg", &paths).as_deref(),
            Some("src/pkg/mod.rs")
        );
    }

    #[test]
    fn python_init_py_resolves_for_directory_package() {
        let paths = ["pkg/__init__.py".to_string(), "main.py".to_string()]
            .into_iter()
            .collect();
        assert_eq!(
            resolve_import("main.py", "pkg", &paths).as_deref(),
            Some("pkg/__init__.py")
        );
    }

    #[test]
    fn import_resolution_honors_cancellation() {
        let mut files = vec![IndexedFile {
            path: "src/app.ts".into(),
            language: Some("typescript".into()),
            structurally_complete: true,
            size_bytes: 1,
            modified_ns: None,
            content_hash: "hash".into(),
            chunks: Vec::new(),
            symbols: Vec::new(),
            references: Vec::new(),
            imports: vec![ImportInput {
                raw_target: "./lib".into(),
                resolved_path: None,
                candidate_paths: Vec::new(),
                line: 1,
            }],
        }];
        let paths = ["src/app.ts".to_string(), "src/lib.ts".to_string()]
            .into_iter()
            .collect();
        let cancellation = CancellationToken::new();
        cancellation.cancel();

        assert!(matches!(
            resolve_imports(&mut files, &paths, &cancellation),
            Err(Error::Cancelled)
        ));
    }

    #[test]
    fn parser_cancellation_is_not_downgraded_to_a_file_warning() {
        let root = tempfile::tempdir().expect("root");
        let absolute_path = root.path().join("cancelled.rs");
        let source = "fn cancelled() {}\n";
        std::fs::write(&absolute_path, source).expect("source");
        let file = DiscoveredFile {
            absolute_path,
            relative_path: "cancelled.rs".into(),
            size_bytes: source.len() as u64,
            modified_ns: None,
        };
        let cancellation = CancellationToken::new();
        cancellation.cancel();

        assert!(matches!(
            prepare_file(
                &file,
                80,
                32 * 1024,
                crate::tokens::Tokenizer::default(),
                2 * 1024 * 1024,
                &cancellation,
            ),
            Err(Error::Cancelled)
        ));
    }

    #[test]
    fn parser_content_version_reindexes_legacy_symbol_rows() {
        let root = tempfile::tempdir().expect("root");
        let database = root.path().join("index.sqlite");
        std::fs::write(
            root.path().join("point.rs"),
            "struct Point;\nimpl Point { fn distance(&self) {} }\n",
        )
        .expect("source");
        let config =
            Arc::new(Config::discover(root.path(), Some(database.clone())).expect("config"));
        let storage = Storage::open(&database).expect("storage");
        let indexer = Indexer::new(config, storage.clone()).expect("indexer");

        let first = indexer.reconcile(false).expect("initial reconcile");
        assert_eq!(first.repository_generation, 1);
        let legacy_hash = indexer.config_hash_for_content_marker(PREVIOUS_INDEX_CONTENT_MARKER);
        let connection = rusqlite::Connection::open(&database).expect("legacy connection");
        connection
            .execute(
                "INSERT INTO symbols(file_id, name, kind, parent, signature, start_line, end_line, start_byte, end_byte)
                 SELECT file_id, name, 'function', name, signature, start_line, end_line, start_byte, end_byte
                 FROM symbols WHERE name = 'distance'",
                [],
            )
            .expect("inject legacy duplicate");
        connection
            .execute(
                "UPDATE meta SET config_hash = ?1 WHERE id = 1",
                rusqlite::params![legacy_hash],
            )
            .expect("set legacy marker");
        drop(connection);
        assert_eq!(
            storage
                .search_symbols("distance", true, 10)
                .expect("legacy symbols")
                .len(),
            2
        );

        let reparsed = indexer.reconcile(false).expect("content-version reparse");
        assert_eq!(reparsed.repository_generation, 2);
        assert_eq!(reparsed.files_indexed, 1);
        assert_eq!(reparsed.files_unchanged, 0);
        let symbols = storage
            .search_symbols("distance", true, 10)
            .expect("reparsed symbols");
        assert_eq!(symbols.len(), 1);
        assert_eq!(symbols[0].symbol.kind, "method");
        assert_eq!(symbols[0].symbol.parent.as_deref(), Some("Point"));
        assert_eq!(
            storage.meta().expect("metadata").config_hash,
            indexer.config_hash()
        );
    }

    #[test]
    fn bounded_read_stops_at_limit_plus_one() {
        let root = tempfile::tempdir().expect("root");
        let path = root.path().join("growing.rs");
        std::fs::write(&path, "12345").expect("source");

        assert_eq!(
            read_bounded(&path, 5).expect("boundary"),
            Some(b"12345".to_vec())
        );
        assert_eq!(read_bounded(&path, 4).expect("limit plus one"), None);
    }

    #[test]
    fn preparation_batches_honor_file_and_byte_boundaries() {
        let candidates = (0..3)
            .map(|index| DiscoveredFile {
                absolute_path: format!("{index}.rs").into(),
                relative_path: format!("{index}.rs"),
                size_bytes: 2,
                modified_ns: None,
            })
            .collect::<Vec<_>>();
        let file_limited = crate::DiscoveryLimits {
            max_file_bytes: 2,
            max_prepare_batch_files: 2,
            max_prepare_batch_bytes: 10,
            ..crate::DiscoveryLimits::default()
        };
        let byte_limited = crate::DiscoveryLimits {
            max_file_bytes: 2,
            max_prepare_batch_files: 3,
            max_prepare_batch_bytes: 3,
            ..crate::DiscoveryLimits::default()
        };

        assert_eq!(prepare_batch_end(&candidates, 0, file_limited), 2);
        assert_eq!(prepare_batch_end(&candidates, 2, file_limited), 3);
        assert_eq!(prepare_batch_end(&candidates, 0, byte_limited), 1);
        assert_eq!(prepare_batch_end(&candidates, 1, byte_limited), 2);
    }

    #[test]
    fn worker_pool_is_lazy_and_threads_follow_config_per_indexer() {
        let root = tempfile::tempdir().expect("root");
        let mut config_a =
            Config::discover(root.path(), Some(root.path().join("a.sqlite"))).expect("config a");
        config_a.max_index_workers = 1;
        let storage_a = Storage::open(&config_a.database_path).expect("storage a");
        let indexer_a = Indexer::new(Arc::new(config_a), storage_a).expect("indexer a");

        let mut config_b =
            Config::discover(root.path(), Some(root.path().join("b.sqlite"))).expect("config b");
        config_b.max_index_workers = 3;
        let storage_b = Storage::open(&config_b.database_path).expect("storage b");
        let indexer_b = Indexer::new(Arc::new(config_b), storage_b).expect("indexer b");

        assert!(indexer_a.pool.pool.get().is_none());
        assert!(indexer_b.pool.pool.get().is_none());
        let mut consumed = false;
        indexer_a
            .prepare_candidate_batches(&[], &CancellationToken::new(), |_| {
                consumed = true;
                Ok(())
            })
            .expect("empty prepare");
        assert!(!consumed);
        assert!(indexer_a.pool.pool.get().is_none());

        assert_eq!(
            indexer_a
                .pool
                .get_or_build(indexer_a.config.max_index_workers)
                .expect("pool a")
                .current_num_threads(),
            1
        );
        assert_eq!(
            indexer_b
                .pool
                .get_or_build(indexer_b.config.max_index_workers)
                .expect("pool b")
                .current_num_threads(),
            3
        );
    }

    #[test]
    fn cancellation_between_preparation_batches_stops_before_the_next_batch() {
        let root = tempfile::tempdir().expect("root");
        let paths = [root.path().join("a.rs"), root.path().join("b.rs")];
        for path in &paths {
            fs::write(path, "fn item() {}\n").expect("fixture");
        }
        let mut config =
            Config::discover(root.path(), Some(root.path().join("index.sqlite"))).expect("config");
        config.max_prepare_batch_files = 1;
        let storage = Storage::open(&config.database_path).expect("storage");
        let indexer = Indexer::new(Arc::new(config), storage).expect("indexer");
        let candidates = paths
            .iter()
            .map(|path| {
                let metadata = fs::metadata(path).expect("metadata");
                DiscoveredFile {
                    absolute_path: path.clone(),
                    relative_path: path
                        .file_name()
                        .expect("file name")
                        .to_string_lossy()
                        .into_owned(),
                    size_bytes: metadata.len(),
                    modified_ns: None,
                }
            })
            .collect::<Vec<_>>();
        let cancellation = CancellationToken::new();
        let mut batches = 0usize;

        let error = indexer
            .prepare_candidate_batches(&candidates, &cancellation, |_| {
                batches += 1;
                cancellation.cancel();
                Ok(())
            })
            .expect_err("second batch must observe cancellation");

        assert!(matches!(error, Error::Cancelled));
        assert_eq!(batches, 1);
    }
}
