use std::collections::HashMap;
use std::fs;
use std::sync::{
    Arc,
    atomic::{AtomicU64, AtomicUsize, Ordering},
};
use std::thread;
use std::time::{Duration, Instant};

use cap_std::fs::Dir;
use serde::Serialize;
use tokio_util::sync::CancellationToken;

use crate::coordination::{CacheLease, IndexCoordination, IndexLeadership};
use crate::error::RetryableOperation;
use crate::indexer::Indexer;
use crate::model::*;
use crate::storage::{ReadSession, Storage, StorageCounts, TokenSavingsRecord};
use crate::tokens::response_token_accounting;
use crate::{Config, Error, Result};

mod change_receipt;
#[cfg(test)]
mod concurrency_profile;
mod context;
mod executor;
mod files;
mod handoff;
mod history;
mod json;
mod read;
mod read_delta;
mod receipts;
mod reconciliation;
mod search;
pub(crate) mod validation;

const STARTUP_BUSY_TIMEOUT: Duration = Duration::from_millis(250);
const STARTUP_RETRY_INITIAL_DELAY: Duration = Duration::from_millis(25);
const STARTUP_RETRY_MAX_DELAY: Duration = Duration::from_millis(500);
const CANCELLATION_POLL_INTERVAL: Duration = Duration::from_millis(25);
const INITIAL_INDEX_IDLE_GRACE: Duration = Duration::from_secs(1);
const INITIAL_INDEX_PROBE_INTERVAL: Duration = Duration::from_millis(100);
pub(crate) const MAX_EXPECTED_REPOSITORY_ID_BYTES: usize = 128;
const TOKEN_SAVINGS_ESTIMATE_BASIS: &str =
    "requested read ranges or whole source files represented in each response";
const RESPONSE_ACCOUNTING_SCOPE: &str = "successful repository retrieval responses recorded after \
    full-response accounting was enabled; includes successful retries as separate requests but \
    excludes pre-response failures, tool discovery, task success, and native-tool costs";
const RESPONSE_ACCOUNTING_ESTIMATE_BASIS: &str =
    "represented-source baseline minus complete serialized response tokens";

fn signed_token_difference(baseline: u64, response: u64) -> i64 {
    let difference = i128::from(baseline) - i128::from(response);
    difference.clamp(i128::from(i64::MIN), i128::from(i64::MAX)) as i64
}

pub(crate) fn retrieval_primitive_key(
    generation: u64,
    kind: &str,
    normalized_inputs: &str,
) -> RetrievalPrimitiveKey {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"leantoken-retrieval-primitive-v1\0");
    hasher.update(&generation.to_le_bytes());
    hasher.update(&(kind.len() as u64).to_le_bytes());
    hasher.update(kind.as_bytes());
    hasher.update(&(normalized_inputs.len() as u64).to_le_bytes());
    hasher.update(normalized_inputs.as_bytes());
    RetrievalPrimitiveKey {
        kind: kind.to_owned(),
        key_blake3: hasher.finalize().to_hex().to_string(),
    }
}

pub(crate) fn validate_positive_request_limit(
    field: &'static str,
    requested: usize,
    limit: usize,
) -> Result<usize> {
    if requested == 0 {
        return Err(Error::InvalidInput {
            field,
            reason: "must be greater than zero",
        });
    }
    validate_request_limit(field, requested, limit)
}

pub(crate) fn validate_request_limit(
    field: &'static str,
    requested: usize,
    limit: usize,
) -> Result<usize> {
    if requested > limit {
        return Err(Error::RequestLimitExceeded {
            field,
            requested,
            limit,
        });
    }
    Ok(requested)
}

#[derive(Debug, Clone)]
/// Shared application services used by both CLI and MCP adapters.
///
/// Blocking filesystem and SQLite work runs on Tokio's blocking pool. Index
/// reconciliations are serialized across processes, while reads use committed
/// SQLite WAL snapshots so every query in one response sees the same generation.
pub struct Services {
    config: Arc<Config>,
    storage: Storage,
    indexer: Indexer,
    repository_root: Arc<Dir>,
    coordination: IndexCoordination,
    _cache_lease: CacheLease,
    active_reconciliations: Arc<AtomicUsize>,
    reconciliation_changed: Arc<tokio::sync::Notify>,
    receipts: Arc<receipts::ReceiptRegistry>,
    read_deltas: Arc<read_delta::ReadDeltaRegistry>,
    next_receipt_id: Arc<AtomicU64>,
    blocking_executor: executor::BlockingExecutor,
    reconciliation: reconciliation::ReconciliationCoordinator,
}

trait RetrievalResponse: Serialize {
    fn meta_mut(&mut self) -> &mut ResponseMeta;
}

macro_rules! impl_retrieval_response {
    ($($response:ty),+ $(,)?) => {
        $(
            impl RetrievalResponse for $response {
                fn meta_mut(&mut self) -> &mut ResponseMeta {
                    &mut self.meta
                }
            }
        )+
    };
}

impl_retrieval_response!(
    FilesResponse,
    HistoryResponse,
    JsonResponse,
    SearchResponse,
    OutlineResponse,
    ReadResponse,
    ContextResponse,
);

impl Services {
    /// Open the SQLite index and construct retrieval services.
    pub fn open(config: Config) -> Result<Self> {
        config.validate()?;
        let coordination = IndexCoordination::for_database(&config.database_path);
        let cancellation = CancellationToken::new();
        let cache_lease = coordination.acquire_cache_lease(&cancellation)?;
        let _initialization = coordination.acquire_initialization(&cancellation)?;
        Self::open_once(&config, None, cache_lease)
    }

    /// Open services under exclusive cache initialization ownership, retrying
    /// transient SQLite contention until the caller cancels.
    pub fn open_cancellable(config: Config, cancellation: &CancellationToken) -> Result<Self> {
        config.validate()?;
        let coordination = IndexCoordination::for_database(&config.database_path);
        let cache_lease = coordination.acquire_cache_lease(cancellation)?;
        let _initialization = coordination.acquire_initialization(cancellation)?;
        let mut delay = STARTUP_RETRY_INITIAL_DELAY;
        let mut attempt = 0u32;

        loop {
            validation::check_cancelled(cancellation)?;
            match Self::open_once(&config, Some(STARTUP_BUSY_TIMEOUT), cache_lease.clone()) {
                Ok(services) => return Ok(services),
                Err(error) if is_database_contention(&error) => {
                    attempt = attempt.saturating_add(1);
                    if attempt == 1 || attempt.is_multiple_of(20) {
                        tracing::warn!(
                            attempt,
                            retry_delay_ms = delay.as_millis(),
                            database = %config.database_path.display(),
                            %error,
                            "cache initialization is waiting for SQLite contention"
                        );
                    }
                    wait_cancellable(cancellation, delay)?;
                    delay = delay.saturating_mul(2).min(STARTUP_RETRY_MAX_DELAY);
                }
                Err(error) => return Err(error),
            }
        }
    }

    fn open_once(
        config: &Config,
        startup_timeout: Option<Duration>,
        cache_lease: CacheLease,
    ) -> Result<Self> {
        let open_storage = || match startup_timeout {
            Some(timeout) => Storage::open_for_repository_with_startup_timeout(
                &config.database_path,
                &config.root,
                timeout,
            ),
            None => Storage::open_for_repository(&config.database_path, &config.root),
        };
        let storage = match open_storage() {
            Ok(storage) => storage,
            Err(error) if config.database_is_managed_cache && is_database_corruption(&error) => {
                tracing::warn!(database = %config.database_path.display(), "rebuilding corrupt managed index");
                remove_database_artifacts(&config.database_path)?;
                open_storage()?
            }
            Err(error) => return Err(error),
        };
        Self::from_parts(Arc::new(config.clone()), storage, cache_lease)
    }

    fn from_parts(config: Arc<Config>, storage: Storage, cache_lease: CacheLease) -> Result<Self> {
        let indexer = Indexer::new(Arc::clone(&config), storage.clone())?;
        let repository_root = indexer.repository_root();
        let coordination = IndexCoordination::for_database(&config.database_path);
        let active_reconciliations = Arc::new(AtomicUsize::new(0));
        let reconciliation_changed = Arc::new(tokio::sync::Notify::new());
        let reconciliation = reconciliation::ReconciliationCoordinator::new(
            indexer.clone(),
            coordination.clone(),
            Arc::clone(&active_reconciliations),
            Arc::clone(&reconciliation_changed),
        );
        Ok(Self {
            config,
            storage,
            indexer,
            repository_root,
            coordination,
            _cache_lease: cache_lease,
            active_reconciliations,
            reconciliation_changed,
            receipts: Arc::new(receipts::ReceiptRegistry::default()),
            read_deltas: Arc::new(read_delta::ReadDeltaRegistry::default()),
            next_receipt_id: Arc::new(AtomicU64::new(1)),
            blocking_executor: executor::BlockingExecutor::default(),
            reconciliation,
        })
    }

    #[must_use]
    /// Return the resolved repository configuration.
    pub fn config(&self) -> &Config {
        &self.config
    }

    fn finalize_response<T: RetrievalResponse>(&self, response: &mut T) -> Result<()> {
        let source_tokens = {
            let meta = response.meta_mut();
            meta.protocol_tokens = 0;
            meta.path_and_metadata_tokens = 0;
            meta.total_response_tokens = 0;
            meta.payload_tokens = 0;
            meta.source_tokens
        };
        let accounting =
            response_token_accounting(&*response, source_tokens, &self.config.tokenizer)?;
        let meta = response.meta_mut();
        meta.protocol_tokens = accounting.protocol_tokens;
        meta.path_and_metadata_tokens = accounting.path_and_metadata_tokens;
        meta.total_response_tokens = accounting.total_response_tokens;
        meta.payload_tokens = accounting.total_response_tokens;
        Ok(())
    }

    /// Reconcile repository files into one committed index generation.
    pub async fn index(&self, rebuild: bool) -> Result<IndexResponse> {
        self.index_report(rebuild)
            .await
            .map(IndexReport::into_response)
    }

    /// Reconcile repository files and include bounded preparation skip reasons.
    pub async fn index_report(&self, rebuild: bool) -> Result<IndexReport> {
        self.index_cancellable_report(rebuild, CancellationToken::new())
            .await
    }

    /// Reconcile repository files while honoring caller-owned cancellation.
    pub async fn index_cancellable(
        &self,
        rebuild: bool,
        cancellation: CancellationToken,
    ) -> Result<IndexResponse> {
        self.index_cancellable_report(rebuild, cancellation)
            .await
            .map(IndexReport::into_response)
    }

    /// Reconcile with cancellation and include bounded preparation skip reasons.
    pub async fn index_cancellable_report(
        &self,
        rebuild: bool,
        cancellation: CancellationToken,
    ) -> Result<IndexReport> {
        let this = self.clone();
        let active_reconciliations = Arc::clone(&self.active_reconciliations);
        let reconciliation_changed = Arc::clone(&self.reconciliation_changed);
        active_reconciliations.fetch_add(1, Ordering::AcqRel);
        tokio::task::spawn_blocking(move || {
            let _active = ActiveReconciliation {
                count: active_reconciliations,
                changed: reconciliation_changed,
            };
            let operation = this.coordination.acquire_operation(&cancellation)?;
            let result = this
                .indexer
                .reconcile_cancellable_report(rebuild, &cancellation);
            operation.release()?;
            result
        })
        .await?
    }

    /// Reconcile watcher-reported paths, falling back internally when a
    /// repository-wide scan is required for correctness.
    pub async fn index_paths(&self, paths: Vec<String>) -> Result<IndexResponse> {
        self.index_paths_report(paths)
            .await
            .map(IndexReport::into_response)
    }

    /// Reconcile watcher paths and include bounded preparation skip reasons.
    pub async fn index_paths_report(&self, paths: Vec<String>) -> Result<IndexReport> {
        self.index_paths_cancellable_report(paths, CancellationToken::new())
            .await
    }

    /// Reconcile watcher-reported paths while honoring caller-owned cancellation.
    pub async fn index_paths_cancellable(
        &self,
        paths: Vec<String>,
        cancellation: CancellationToken,
    ) -> Result<IndexResponse> {
        self.index_paths_cancellable_report(paths, cancellation)
            .await
            .map(IndexReport::into_response)
    }

    /// Reconcile watcher paths with cancellation and preparation skip reasons.
    pub async fn index_paths_cancellable_report(
        &self,
        paths: Vec<String>,
        cancellation: CancellationToken,
    ) -> Result<IndexReport> {
        let this = self.clone();
        let active_reconciliations = Arc::clone(&self.active_reconciliations);
        let reconciliation_changed = Arc::clone(&self.reconciliation_changed);
        active_reconciliations.fetch_add(1, Ordering::AcqRel);
        tokio::task::spawn_blocking(move || {
            let _active = ActiveReconciliation {
                count: active_reconciliations,
                changed: reconciliation_changed,
            };
            let operation = this.coordination.acquire_operation(&cancellation)?;
            let result = this
                .indexer
                .reconcile_paths_cancellable_report(&paths, &cancellation);
            operation.release()?;
            result
        })
        .await?
    }

    /// Wait until the first committed generation is no longer being published.
    pub(crate) async fn wait_for_initial_index_cancellable(
        &self,
        cancellation: CancellationToken,
    ) -> Result<()> {
        let mut idle_deadline = None;
        loop {
            validation::check_cancelled(&cancellation)?;

            let changed = self.reconciliation_changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            if self.active_reconciliations.load(Ordering::Acquire) > 0 {
                idle_deadline = None;
                tokio::select! {
                    _ = cancellation.cancelled() => return Err(Error::Cancelled),
                    _ = &mut changed => {}
                }
                continue;
            }

            let this = self.clone();
            let probe = tokio::task::spawn_blocking(move || {
                let Some(operation) = this.coordination.try_acquire_operation()? else {
                    return Ok(None);
                };
                let generation = this.storage.repository_generation();
                operation.release()?;
                generation.map(Some)
            });
            let generation = tokio::select! {
                _ = cancellation.cancelled() => return Err(Error::Cancelled),
                _ = &mut changed => {
                    idle_deadline = None;
                    continue;
                },
                result = probe => result??,
            };
            if generation.is_some_and(|generation| generation > 0) {
                return Ok(());
            }
            let delay = if generation.is_none() {
                idle_deadline = None;
                INITIAL_INDEX_PROBE_INTERVAL
            } else {
                let deadline = idle_deadline
                    .get_or_insert_with(|| tokio::time::Instant::now() + INITIAL_INDEX_IDLE_GRACE);
                let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
                if remaining.is_zero() {
                    return Err(Error::IndexNotReady);
                }
                remaining.min(INITIAL_INDEX_PROBE_INTERVAL)
            };

            tokio::select! {
                _ = cancellation.cancelled() => return Err(Error::Cancelled),
                _ = &mut changed => {
                    idle_deadline = None;
                },
                _ = tokio::time::sleep(delay) => {}
            }
        }
    }

    /// Attempt to own automatic indexing and watching for this cache.
    pub fn try_acquire_index_leadership(&self) -> Result<Option<IndexLeadership>> {
        self.coordination.try_acquire_leadership()
    }

    /// Return index counts, generation, and freshness.
    pub async fn status(&self) -> Result<StatusResponse> {
        let this = self.clone();
        self.blocking_executor
            .run(CancellationToken::new(), move |_| this.status_sync())
            .await
    }

    /// Return status without initializing an existing SQLite cache.
    ///
    /// This keeps a read-only status request responsive while another process
    /// is creating, migrating, or indexing the cache. A missing cache still
    /// follows the normal open path so cold status reports an uninitialized
    /// repository and creates the cache as it did previously.
    pub fn status_without_initializing(config: Config) -> Result<StatusResponse> {
        config.validate()?;
        if !config.database_path.exists() {
            return Self::open(config)?.status_sync();
        }

        let coordination = IndexCoordination::for_database(&config.database_path);
        let operation = coordination.try_acquire_operation()?;
        let freshness = operation.is_none();
        let snapshot = Storage::read_only_status(&config.database_path, &config.root);
        if let Some(operation) = operation {
            operation.release()?;
        }
        let snapshot = snapshot?;
        Ok(status_response(
            &config,
            snapshot.generation,
            snapshot.counts,
            if freshness {
                Freshness::Reconciling
            } else {
                Freshness::Current
            },
        ))
    }

    fn status_sync(&self) -> Result<StatusResponse> {
        self.consistent_allow_empty(|session, generation| {
            let counts = session.counts()?;
            Ok(status_response(
                &self.config,
                generation,
                counts,
                self.freshness(),
            ))
        })
    }

    /// Return cumulative source-token savings estimates for this repository and tokenizer.
    pub async fn token_savings(&self) -> Result<TokenSavingsResponse> {
        let this = self.clone();
        self.blocking_executor
            .run(CancellationToken::new(), move |_| this.token_savings_sync())
            .await
    }

    fn token_savings_sync(&self) -> Result<TokenSavingsResponse> {
        let tokenizer = self.config.tokenizer.name();
        let stored = self.storage.token_savings(tokenizer)?;
        Ok(self.source_savings_from_records(&stored))
    }

    /// Return source-only savings plus complete successful-response accounting.
    pub async fn token_savings_report(&self) -> Result<TokenSavingsReport> {
        let this = self.clone();
        tokio::task::spawn_blocking(move || this.token_savings_report_sync()).await?
    }

    fn token_savings_report_sync(&self) -> Result<TokenSavingsReport> {
        let tokenizer = self.config.tokenizer.name();
        let stored = self.storage.token_savings(tokenizer)?;
        let source_savings = self.source_savings_from_records(&stored);
        let mut tracked_requests = 0u64;
        let mut baseline_requests = 0u64;
        let mut baseline_source_tokens = 0u64;
        let mut response_source_tokens = 0u64;
        let mut path_and_metadata_tokens = 0u64;
        let mut protocol_tokens = 0u64;
        let mut total_response_tokens = 0u64;
        let mut receipt_suppressed_exact = 0u64;
        let mut receipt_suppressed_overlap = 0u64;
        let by_operation = TokenAccountingOperation::ALL
            .into_iter()
            .map(|operation| {
                let record = stored.get(operation.as_str()).cloned().unwrap_or_default();
                tracked_requests =
                    tracked_requests.saturating_add(record.response_tracked_requests);
                baseline_requests =
                    baseline_requests.saturating_add(record.response_baseline_requests);
                baseline_source_tokens =
                    baseline_source_tokens.saturating_add(record.response_baseline_source_tokens);
                response_source_tokens =
                    response_source_tokens.saturating_add(record.response_source_tokens);
                path_and_metadata_tokens =
                    path_and_metadata_tokens.saturating_add(record.path_and_metadata_tokens);
                protocol_tokens = protocol_tokens.saturating_add(record.protocol_tokens);
                total_response_tokens =
                    total_response_tokens.saturating_add(record.total_response_tokens);
                receipt_suppressed_exact =
                    receipt_suppressed_exact.saturating_add(record.receipt_suppressed_exact);
                receipt_suppressed_overlap =
                    receipt_suppressed_overlap.saturating_add(record.receipt_suppressed_overlap);
                ResponseTokenAccountingByOperation {
                    operation,
                    tracked_requests: record.response_tracked_requests,
                    baseline_requests: record.response_baseline_requests,
                    baseline_source_tokens: record.response_baseline_source_tokens,
                    response_source_tokens: record.response_source_tokens,
                    path_and_metadata_tokens: record.path_and_metadata_tokens,
                    protocol_tokens: record.protocol_tokens,
                    total_response_tokens: record.total_response_tokens,
                    estimated_net_tokens_saved: signed_token_difference(
                        record.response_baseline_source_tokens,
                        record.total_response_tokens,
                    ),
                    receipt_suppressed_exact: record.receipt_suppressed_exact,
                    receipt_suppressed_overlap: record.receipt_suppressed_overlap,
                }
            })
            .collect();
        Ok(TokenSavingsReport {
            source_savings,
            response_accounting: ResponseTokenAccounting {
                accounting_scope: RESPONSE_ACCOUNTING_SCOPE.to_owned(),
                estimate_basis: RESPONSE_ACCOUNTING_ESTIMATE_BASIS.to_owned(),
                tracked_requests,
                baseline_requests,
                baseline_source_tokens,
                response_source_tokens,
                path_and_metadata_tokens,
                protocol_tokens,
                total_response_tokens,
                estimated_net_tokens_saved: signed_token_difference(
                    baseline_source_tokens,
                    total_response_tokens,
                ),
                receipt_suppressed_exact,
                receipt_suppressed_overlap,
                by_operation,
            },
        })
    }

    fn source_savings_from_records(
        &self,
        stored: &HashMap<String, TokenSavingsRecord>,
    ) -> TokenSavingsResponse {
        let tokenizer = self.config.tokenizer.name();
        let mut tracked_requests = 0u64;
        let mut baseline_source_tokens = 0u64;
        let mut emitted_source_tokens = 0u64;
        let mut estimated_source_tokens_saved = 0u64;
        let by_operation = TokenSavingsOperation::ALL
            .into_iter()
            .map(|operation| {
                let record = stored.get(operation.as_str()).cloned().unwrap_or_default();
                tracked_requests = tracked_requests.saturating_add(record.tracked_requests);
                baseline_source_tokens =
                    baseline_source_tokens.saturating_add(record.baseline_source_tokens);
                emitted_source_tokens =
                    emitted_source_tokens.saturating_add(record.emitted_source_tokens);
                estimated_source_tokens_saved = estimated_source_tokens_saved
                    .saturating_add(record.estimated_source_tokens_saved);
                TokenSavingsByOperation {
                    operation,
                    tracked_requests: record.tracked_requests,
                    baseline_source_tokens: record.baseline_source_tokens,
                    emitted_source_tokens: record.emitted_source_tokens,
                    estimated_source_tokens_saved: record.estimated_source_tokens_saved,
                }
            })
            .collect();
        TokenSavingsResponse {
            tokenizer: tokenizer.to_owned(),
            token_count_exact: self.config.tokenizer.is_exact(),
            estimate_basis: TOKEN_SAVINGS_ESTIMATE_BASIS.to_owned(),
            tracked_requests,
            baseline_source_tokens,
            emitted_source_tokens,
            estimated_source_tokens_saved,
            by_operation,
        }
    }

    pub(super) fn consistent<T>(
        &self,
        operation: impl Fn(&ReadSession, u64) -> Result<T>,
    ) -> Result<T> {
        self.consistent_inner(false, operation)
    }

    fn consistent_allow_empty<T>(
        &self,
        operation: impl Fn(&ReadSession, u64) -> Result<T>,
    ) -> Result<T> {
        self.consistent_inner(true, operation)
    }

    /// Assemble a response against one WAL snapshot (DEFERRED read transaction).
    /// Concurrent writers cannot mix generations inside a single assembly. If
    /// opening the snapshot fails transiently or the index is still empty,
    /// returns a typed retryable error rather than a partial response.
    fn consistent_inner<T>(
        &self,
        allow_empty: bool,
        operation: impl Fn(&ReadSession, u64) -> Result<T>,
    ) -> Result<T> {
        for attempt in 0..3 {
            let snapshot = self.storage.begin_read().and_then(|session| {
                let generation = session.repository_generation()?;
                Ok((session, generation))
            });
            let (session, generation) = match snapshot {
                Ok(snapshot) => snapshot,
                Err(error) if is_database_contention(&error) => {
                    if attempt + 1 < 3 {
                        thread::sleep(CANCELLATION_POLL_INTERVAL);
                    }
                    continue;
                }
                Err(error) => return Err(error),
            };
            if generation == 0 && !allow_empty {
                return Err(Error::IndexNotReady);
            }
            // Do not retry operation errors: after the first read, this session
            // is pinned and concurrent publication cannot have caused them.
            return operation(&session, generation);
        }
        Err(Error::RetryableConflict(RetryableOperation::Retrieval))
    }

    pub(super) fn result_limit(&self, requested: Option<usize>) -> Result<usize> {
        validate_positive_request_limit(
            "max_results",
            requested.unwrap_or(self.config.default_results),
            self.config.max_results,
        )
    }

    pub(super) fn token_limit(&self, requested: Option<usize>, default: usize) -> Result<usize> {
        validate_positive_request_limit(
            "max_tokens",
            requested.unwrap_or(default),
            self.config.max_output_tokens,
        )
    }

    pub(super) fn token_budget_limit(&self, requested: usize) -> Result<usize> {
        validate_positive_request_limit("token_budget", requested, self.config.max_output_tokens)
    }

    pub(super) fn context_line_limit(&self, requested: Option<usize>) -> Result<usize> {
        validate_request_limit(
            "context_lines",
            requested.unwrap_or(self.config.context_lines),
            crate::config::MAX_CONTEXT_LINES,
        )
    }

    pub(super) async fn apply_consistency(
        &self,
        consistency: IndexConsistency,
        cancellation: CancellationToken,
    ) -> Result<()> {
        if consistency == IndexConsistency::ReconcileWorkingTree {
            self.reconciliation.reconcile(cancellation).await?;
        }
        Ok(())
    }

    pub(super) fn freshness(&self) -> Freshness {
        let local = self.active_reconciliations.load(Ordering::Acquire) > 0;
        let shared = self.coordination.is_reconciling().unwrap_or(true);
        if local || shared {
            Freshness::Reconciling
        } else {
            Freshness::Current
        }
    }

    pub(super) fn meta(
        &self,
        generation: u64,
        emitted_tokens: usize,
        next_cursor: Option<String>,
    ) -> ResponseMeta {
        ResponseMeta {
            repository_id: self.repository_id(),
            repository_generation: generation,
            freshness: self.freshness(),
            source_tokens: emitted_tokens,
            protocol_tokens: 0,
            path_and_metadata_tokens: 0,
            total_response_tokens: 0,
            payload_tokens: 0,
            tokenizer: self.config.tokenizer.name().into(),
            emitted_tokens,
            token_count_exact: self.config.tokenizer.is_exact(),
            receipt_id: None,
            receipt_suppressed_exact: 0,
            receipt_suppressed_overlap: 0,
            receipt_near_duplicates: 0,
            next_cursor,
        }
    }

    /// Returns a stable opaque identity for the canonical repository root.
    pub fn repository_id(&self) -> String {
        let mut input = b"leantoken-repository-root-v1\0".to_vec();
        input.extend_from_slice(self.config.root.as_os_str().as_encoded_bytes());
        blake3::hash(&input).to_hex()[..32].to_string()
    }

    /// Rejects retrieval bound to a different repository/worktree.
    pub fn validate_repository_id(&self, expected: Option<&str>) -> Result<()> {
        if let Some(expected) = expected {
            validation::validate_input(
                expected,
                "expected_repository_id",
                MAX_EXPECTED_REPOSITORY_ID_BYTES,
            )?;
        }
        let actual = self.repository_id();
        if expected.is_none_or(|expected| expected == actual) {
            return Ok(());
        }
        Err(Error::RepositoryIdentityMismatch {
            expected: expected.unwrap_or_default().to_owned(),
            actual,
        })
    }

    pub(super) fn record_token_savings(
        &self,
        operation: TokenAccountingOperation,
        baseline_source_tokens: Option<usize>,
        meta: &ResponseMeta,
    ) {
        match self.storage.record_token_savings(
            self.config.tokenizer.name(),
            operation,
            baseline_source_tokens,
            meta,
        ) {
            Ok(true) => {}
            Ok(false) => tracing::debug!(
                operation = operation.as_str(),
                "token-savings accounting skipped a busy writer"
            ),
            Err(error) => tracing::warn!(
                %error,
                operation = operation.as_str(),
                "token-savings accounting was skipped"
            ),
        }
    }
}

fn is_database_corruption(error: &Error) -> bool {
    matches!(
        sqlite_error_code(error),
        Some(rusqlite::ErrorCode::DatabaseCorrupt | rusqlite::ErrorCode::NotADatabase)
    )
}

fn status_response(
    config: &Config,
    generation: u64,
    counts: StorageCounts,
    freshness: Freshness,
) -> StatusResponse {
    let index_storage_bytes = sqlite_storage_bytes(&config.database_path);
    let index_amplification_ratio =
        (counts.source_bytes > 0).then(|| index_storage_bytes as f64 / counts.source_bytes as f64);
    StatusResponse {
        repository_root: config.root.display().to_string(),
        database_path: config.database_path.display().to_string(),
        repository_generation: generation,
        index_state: if generation == 0 {
            IndexState::Uninitialized
        } else {
            IndexState::Ready
        },
        freshness,
        file_count: counts.files,
        chunk_count: counts.chunks,
        symbol_count: counts.symbols,
        index_storage_bytes,
        indexed_source_bytes: counts.source_bytes,
        index_amplification_ratio,
        process_rss_bytes: process_rss_bytes(),
        languages: counts
            .languages
            .into_iter()
            .map(|(language, files)| LanguageCount { language, files })
            .collect(),
        warnings: Vec::new(),
    }
}

fn sqlite_storage_bytes(path: &std::path::Path) -> u64 {
    ["", "-wal", "-shm"]
        .into_iter()
        .map(|suffix| {
            let mut candidate = path.as_os_str().to_os_string();
            candidate.push(suffix);
            fs::metadata(candidate).map_or(0, |metadata| metadata.len())
        })
        .fold(0, u64::saturating_add)
}

#[cfg(target_os = "linux")]
fn process_rss_bytes() -> Option<u64> {
    fs::read_to_string("/proc/self/status")
        .ok()?
        .lines()
        .find_map(|line| {
            let value = line.strip_prefix("VmRSS:")?.trim();
            let kibibytes = value.strip_suffix("kB")?.trim().parse::<u64>().ok()?;
            kibibytes.checked_mul(1024)
        })
}

#[cfg(not(target_os = "linux"))]
fn process_rss_bytes() -> Option<u64> {
    None
}

fn is_database_contention(error: &Error) -> bool {
    matches!(
        sqlite_error_code(error),
        Some(rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked)
    )
}

fn sqlite_error_code(error: &Error) -> Option<rusqlite::ErrorCode> {
    let error = match error {
        Error::Sqlite(error) => error,
        Error::Migration(rusqlite_migration::Error::RusqliteError { err, .. }) => err,
        _ => return None,
    };
    match error {
        rusqlite::Error::SqliteFailure(inner, _) => Some(inner.code),
        _ => None,
    }
}

fn remove_database_artifacts(database: &std::path::Path) -> Result<()> {
    for suffix in ["", "-wal", "-shm"] {
        let mut path = database.as_os_str().to_os_string();
        path.push(suffix);
        match fs::remove_file(std::path::PathBuf::from(path)) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

struct ActiveReconciliation {
    count: Arc<AtomicUsize>,
    changed: Arc<tokio::sync::Notify>,
}

impl ActiveReconciliation {
    fn new(counter: Arc<AtomicUsize>, changed: Arc<tokio::sync::Notify>) -> Self {
        counter.fetch_add(1, Ordering::AcqRel);
        Self {
            count: counter,
            changed,
        }
    }
}

impl Drop for ActiveReconciliation {
    fn drop(&mut self) {
        self.count.fetch_sub(1, Ordering::AcqRel);
        self.changed.notify_waiters();
    }
}

fn wait_cancellable(cancellation: &CancellationToken, duration: Duration) -> Result<()> {
    let deadline = Instant::now() + duration;
    loop {
        validation::check_cancelled(cancellation)?;
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Ok(());
        }
        thread::sleep(remaining.min(CANCELLATION_POLL_INTERVAL));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::{Condvar, Mutex};
    use tokio_util::sync::CancellationToken;

    #[derive(Default)]
    struct ScanGate {
        entered: Mutex<bool>,
        open: Mutex<bool>,
        changed: Condvar,
    }

    impl ScanGate {
        fn wait(&self) {
            *self.entered.lock().expect("scan gate entered") = true;
            self.changed.notify_all();
            let mut open = self.open.lock().expect("scan gate open");
            while !*open {
                open = self.changed.wait(open).expect("scan gate wait");
            }
        }

        fn entered(&self) -> bool {
            *self.entered.lock().expect("scan gate entered")
        }

        fn open(&self) {
            *self.open.lock().expect("scan gate open") = true;
            self.changed.notify_all();
        }
    }

    async fn wait_until(predicate: impl Fn() -> bool) {
        for _ in 0..10_000 {
            if predicate() {
                return;
            }
            tokio::task::yield_now().await;
        }
        panic!("condition was not reached");
    }

    async fn indexed_services() -> (tempfile::TempDir, Services) {
        let root = tempfile::tempdir().expect("root");
        fs::write(root.path().join("lib.rs"), "pub fn existing() {}\n").expect("source");
        let config =
            Config::discover(root.path(), Some(root.path().join("db.sqlite"))).expect("config");
        let services = Services::open(config).expect("services");
        services.index(false).await.expect("initial index");
        services.reconciliation.reset_diagnostics();
        (root, services)
    }

    #[tokio::test]
    async fn concurrent_consistency_requests_share_one_waiting_wave() {
        let (_root, services) = indexed_services().await;
        let held_operation = services
            .coordination
            .acquire_operation(&CancellationToken::new())
            .expect("hold operation lock");

        let calls = (0..8)
            .map(|_| {
                let services = services.clone();
                tokio::spawn(async move {
                    services
                        .apply_consistency(
                            IndexConsistency::ReconcileWorkingTree,
                            CancellationToken::new(),
                        )
                        .await
                })
            })
            .collect::<Vec<_>>();

        wait_until(|| services.reconciliation.diagnostics().requests == 8).await;
        let waiting = services.reconciliation.diagnostics();
        assert_eq!(waiting.waves_created, 1);
        assert_eq!(waiting.waves_started, 0);
        assert_eq!(waiting.coalesced_requests, 7);

        held_operation.release().expect("release operation lock");
        for call in calls {
            call.await.expect("join reconciliation").expect("reconcile");
        }

        let completed = services.reconciliation.diagnostics();
        assert_eq!(completed.requests, 8);
        assert_eq!(completed.waves_started, 1);
        assert_eq!(completed.waves_completed, 1);
        assert_eq!(completed.active_waves, 0);
    }

    #[tokio::test]
    async fn failed_wave_fans_out_one_error_without_retry_scans() {
        let (_root, services) = indexed_services().await;
        let held_operation = services
            .coordination
            .acquire_operation(&CancellationToken::new())
            .expect("hold operation lock");
        services
            .reconciliation
            .set_before_scan_hook(Some(Arc::new(|| panic!("injected shared failure"))));

        let calls = (0..8)
            .map(|_| {
                let services = services.clone();
                tokio::spawn(async move {
                    services
                        .apply_consistency(
                            IndexConsistency::ReconcileWorkingTree,
                            CancellationToken::new(),
                        )
                        .await
                })
            })
            .collect::<Vec<_>>();
        wait_until(|| services.reconciliation.diagnostics().requests == 8).await;
        assert_eq!(services.reconciliation.diagnostics().waves_created, 1);
        held_operation.release().expect("release operation lock");

        let mut failures = Vec::new();
        for call in calls {
            let Err(Error::ReconciliationFailed(error)) = call.await.expect("join reconciliation")
            else {
                panic!("coalesced caller should receive the shared failure");
            };
            assert!(matches!(error.as_ref(), Error::Join(join) if join.is_panic()));
            failures.push(error);
        }
        assert!(
            failures
                .iter()
                .all(|failure| Arc::ptr_eq(failure, &failures[0]))
        );

        let diagnostics = services.reconciliation.diagnostics();
        assert_eq!(diagnostics.waves_created, 1);
        assert_eq!(diagnostics.waves_started, 1);
        assert_eq!(diagnostics.waves_failed, 1);
        assert_eq!(diagnostics.active_waves, 0);
        services.reconciliation.set_before_scan_hook(None);
    }

    #[tokio::test]
    async fn reconciliation_waiter_admission_has_an_exact_boundary() {
        let (_root, services) = indexed_services().await;
        let held_operation = services
            .coordination
            .acquire_operation(&CancellationToken::new())
            .expect("hold operation lock");

        let calls = (0..reconciliation::DEFAULT_RECONCILIATION_ACTIVE_CAPACITY)
            .map(|_| {
                let services = services.clone();
                tokio::spawn(async move {
                    services
                        .apply_consistency(
                            IndexConsistency::ReconcileWorkingTree,
                            CancellationToken::new(),
                        )
                        .await
                })
            })
            .collect::<Vec<_>>();
        wait_until(|| {
            services.reconciliation.diagnostics().requests
                == reconciliation::DEFAULT_RECONCILIATION_ACTIVE_CAPACITY as u64
        })
        .await;

        assert!(matches!(
            services
                .apply_consistency(
                    IndexConsistency::ReconcileWorkingTree,
                    CancellationToken::new(),
                )
                .await,
            Err(Error::RetrievalOverloaded)
        ));
        assert_eq!(services.reconciliation.diagnostics().rejected_requests, 1);

        held_operation.release().expect("release operation lock");
        for call in calls {
            call.await.expect("join reconciliation").expect("reconcile");
        }
    }

    #[tokio::test]
    async fn caller_after_scan_start_waits_for_the_next_wave() {
        let (root, services) = indexed_services().await;
        let gate = Arc::new(ScanGate::default());
        let hook_gate = Arc::clone(&gate);
        services
            .reconciliation
            .set_before_scan_hook(Some(Arc::new(move || hook_gate.wait())));

        let first_services = services.clone();
        let first = tokio::spawn(async move {
            first_services
                .apply_consistency(
                    IndexConsistency::ReconcileWorkingTree,
                    CancellationToken::new(),
                )
                .await
        });
        wait_until(|| gate.entered()).await;

        fs::write(
            root.path().join("later.rs"),
            "pub fn created_after_wave_started() {}\n",
        )
        .expect("later source");
        let second_services = services.clone();
        let second = tokio::spawn(async move {
            second_services
                .apply_consistency(
                    IndexConsistency::ReconcileWorkingTree,
                    CancellationToken::new(),
                )
                .await
        });
        wait_until(|| services.reconciliation.diagnostics().pending_waiters == 1).await;
        gate.open();

        first.await.expect("join first").expect("first wave");
        second.await.expect("join second").expect("second wave");
        services.reconciliation.set_before_scan_hook(None);

        let diagnostics = services.reconciliation.diagnostics();
        assert_eq!(diagnostics.requests, 2);
        assert_eq!(diagnostics.waves_started, 2);
        assert_eq!(diagnostics.waves_completed, 2);
        let search = services
            .search(SearchRequest {
                query: "created_after_wave_started".into(),
                mode: SearchMode::Identifier,
                include_paths: Vec::new(),
                exclude_paths: Vec::new(),
                focus_paths: Vec::new(),
                max_results: Some(5),
                max_tokens: Some(100),
                context_lines: Some(1),
                case_sensitive: false,
                all_occurrences: false,
                prefer_structural: false,
                receipt_id: None,
                cursor: None,
            })
            .await
            .expect("search later source");
        assert!(search.hits.iter().any(|hit| hit.path == "later.rs"));
    }

    #[tokio::test]
    async fn cancelling_the_only_pending_waiter_never_starts_its_wave() {
        let (_root, services) = indexed_services().await;
        let gate = Arc::new(ScanGate::default());
        let hook_gate = Arc::clone(&gate);
        services
            .reconciliation
            .set_before_scan_hook(Some(Arc::new(move || hook_gate.wait())));

        let first_services = services.clone();
        let first = tokio::spawn(async move {
            first_services
                .apply_consistency(
                    IndexConsistency::ReconcileWorkingTree,
                    CancellationToken::new(),
                )
                .await
        });
        wait_until(|| gate.entered()).await;

        let cancellation = CancellationToken::new();
        let second_services = services.clone();
        let second_cancellation = cancellation.clone();
        let second = tokio::spawn(async move {
            second_services
                .apply_consistency(IndexConsistency::ReconcileWorkingTree, second_cancellation)
                .await
        });
        wait_until(|| services.reconciliation.diagnostics().pending_waiters == 1).await;
        cancellation.cancel();
        assert!(matches!(
            second.await.expect("join cancelled waiter"),
            Err(Error::Cancelled)
        ));

        gate.open();
        first.await.expect("join first").expect("first wave");
        services.reconciliation.set_before_scan_hook(None);
        let diagnostics = services.reconciliation.diagnostics();
        assert_eq!(diagnostics.waves_started, 1);
        assert_eq!(diagnostics.waves_completed, 1);
        assert_eq!(diagnostics.waves_cancelled_before_start, 1);
        assert_eq!(diagnostics.cancelled_waiters, 1);
    }

    #[tokio::test]
    async fn caller_after_a_cancelled_waiting_wave_uses_a_fresh_wave() {
        let (_root, services) = indexed_services().await;
        let held_operation = services
            .coordination
            .acquire_operation(&CancellationToken::new())
            .expect("hold operation lock");

        let cancellation = CancellationToken::new();
        let first_services = services.clone();
        let first_cancellation = cancellation.clone();
        let first = tokio::spawn(async move {
            first_services
                .apply_consistency(IndexConsistency::ReconcileWorkingTree, first_cancellation)
                .await
        });
        wait_until(|| services.reconciliation.diagnostics().requests == 1).await;
        cancellation.cancel();
        assert!(matches!(
            first.await.expect("join cancelled first waiter"),
            Err(Error::Cancelled)
        ));

        let second_services = services.clone();
        let second = tokio::spawn(async move {
            second_services
                .apply_consistency(
                    IndexConsistency::ReconcileWorkingTree,
                    CancellationToken::new(),
                )
                .await
        });
        wait_until(|| {
            let diagnostics = services.reconciliation.diagnostics();
            diagnostics.requests == 2 && diagnostics.waves_created == 2
        })
        .await;
        held_operation.release().expect("release operation lock");

        second.await.expect("join second").expect("fresh wave");
        let diagnostics = services.reconciliation.diagnostics();
        assert_eq!(diagnostics.waves_created, 2);
        assert_eq!(diagnostics.waves_started, 1);
        assert_eq!(diagnostics.waves_completed, 1);
        assert_eq!(diagnostics.waves_cancelled_before_start, 1);
    }

    #[tokio::test]
    async fn aborting_a_running_waiter_does_not_cancel_its_wave() {
        let (_root, services) = indexed_services().await;
        let gate = Arc::new(ScanGate::default());
        let hook_gate = Arc::clone(&gate);
        services
            .reconciliation
            .set_before_scan_hook(Some(Arc::new(move || hook_gate.wait())));

        let first_services = services.clone();
        let first = tokio::spawn(async move {
            first_services
                .apply_consistency(
                    IndexConsistency::ReconcileWorkingTree,
                    CancellationToken::new(),
                )
                .await
        });
        wait_until(|| gate.entered()).await;
        first.abort();
        assert!(first.await.expect_err("aborted waiter").is_cancelled());

        let second_services = services.clone();
        let second = tokio::spawn(async move {
            second_services
                .apply_consistency(
                    IndexConsistency::ReconcileWorkingTree,
                    CancellationToken::new(),
                )
                .await
        });
        wait_until(|| services.reconciliation.diagnostics().pending_waiters == 1).await;
        gate.open();

        second.await.expect("join second").expect("second wave");
        services.reconciliation.set_before_scan_hook(None);
        let diagnostics = services.reconciliation.diagnostics();
        assert_eq!(diagnostics.waves_started, 2);
        assert_eq!(diagnostics.waves_completed, 2);
        assert_eq!(diagnostics.cancelled_waiters, 1);
    }

    #[tokio::test]
    async fn reconciliation_panic_releases_wave_state_and_keeps_services_usable() {
        let (_root, services) = indexed_services().await;
        services
            .reconciliation
            .set_before_scan_hook(Some(Arc::new(|| panic!("injected reconciliation panic"))));

        assert!(matches!(
            services
                .apply_consistency(
                    IndexConsistency::ReconcileWorkingTree,
                    CancellationToken::new(),
                )
                .await,
            Err(Error::ReconciliationFailed(error))
                if matches!(error.as_ref(), Error::Join(join) if join.is_panic())
        ));
        let failed = services.reconciliation.diagnostics();
        assert_eq!(failed.waves_started, 1);
        assert_eq!(failed.waves_failed, 1);
        assert_eq!(failed.active_waves, 0);

        services.reconciliation.set_before_scan_hook(None);
        services
            .apply_consistency(
                IndexConsistency::ReconcileWorkingTree,
                CancellationToken::new(),
            )
            .await
            .expect("later reconciliation");
        let recovered = services.reconciliation.diagnostics();
        assert_eq!(recovered.waves_started, 2);
        assert_eq!(recovered.waves_completed, 1);
        assert_eq!(recovered.active_waves, 0);
    }

    #[test]
    fn signed_token_difference_preserves_cost_and_saturates_public_range() {
        assert_eq!(signed_token_difference(10, 3), 7);
        assert_eq!(signed_token_difference(3, 10), -7);
        assert_eq!(signed_token_difference(u64::MAX, 0), i64::MAX);
        assert_eq!(signed_token_difference(0, u64::MAX), i64::MIN);
    }

    #[tokio::test]
    async fn initial_index_wait_returns_after_publication_lock_releases() {
        let root = tempfile::tempdir().expect("root");
        let config =
            Config::discover(root.path(), Some(root.path().join("db.sqlite"))).expect("config");
        let services = Services::open(config).expect("services");
        let operation = services
            .coordination
            .acquire_operation(&CancellationToken::new())
            .expect("operation lock");
        let publisher_services = services.clone();
        let (published_tx, published_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let publisher = tokio::task::spawn_blocking(move || {
            publisher_services
                .storage
                .full_reconcile("published", Vec::new())
                .expect("publish generation");
            published_tx.send(()).expect("announce publication");
            release_rx.recv().expect("release permission");
            operation.release().expect("release operation lock");
        });
        published_rx.await.expect("publication");

        let waiting_services = services.clone();
        let waiting = tokio::spawn(async move {
            waiting_services
                .wait_for_initial_index_cancellable(CancellationToken::new())
                .await
        });
        tokio::task::yield_now().await;
        assert!(
            !waiting.is_finished(),
            "publication is not settled until the operation lock releases"
        );

        release_tx.send(()).expect("allow release");
        publisher.await.expect("join publisher");
        waiting
            .await
            .expect("join initial index wait")
            .expect("settled generation");
        let status = services.status().await.expect("status");
        assert_eq!(status.repository_generation, 1);
        assert_eq!(status.freshness, Freshness::Current);
    }

    #[tokio::test]
    async fn initial_index_wait_honors_cancellation_before_publication() {
        let root = tempfile::tempdir().expect("root");
        let config =
            Config::discover(root.path(), Some(root.path().join("db.sqlite"))).expect("config");
        let services = Services::open(config).expect("services");
        let cancellation = CancellationToken::new();
        let waiting_cancellation = cancellation.clone();
        let waiting = tokio::spawn(async move {
            services
                .wait_for_initial_index_cancellable(waiting_cancellation)
                .await
        });
        tokio::task::yield_now().await;
        assert!(!waiting.is_finished());

        cancellation.cancel();
        let error = waiting
            .await
            .expect("join initial index wait")
            .expect_err("generation-zero wait must cancel");
        assert!(matches!(error, Error::Cancelled));
    }

    #[tokio::test(start_paused = true)]
    async fn initial_index_wait_bounds_generation_zero_without_an_owner() {
        let root = tempfile::tempdir().expect("root");
        let config =
            Config::discover(root.path(), Some(root.path().join("db.sqlite"))).expect("config");
        let services = Services::open(config).expect("services");

        let result = tokio::time::timeout(
            INITIAL_INDEX_IDLE_GRACE + INITIAL_INDEX_PROBE_INTERVAL,
            services.wait_for_initial_index_cancellable(CancellationToken::new()),
        )
        .await
        .expect("idle generation-zero wait must be bounded")
        .expect_err("idle generation zero remains unready");
        assert!(matches!(result, Error::IndexNotReady));
    }

    #[tokio::test]
    async fn index_search_read_and_hash_delta() {
        let root = tempfile::tempdir().expect("root");
        fs::write(
            root.path().join("lib.rs"),
            "pub fn handle_request() { helper(); }\nfn helper() {}\n",
        )
        .expect("source");
        let config =
            Config::discover(root.path(), Some(root.path().join("db.sqlite"))).expect("config");
        let services = Services::open(config).expect("services");
        services.index(false).await.expect("index");

        let search = services
            .search(SearchRequest {
                query: "handle_request".into(),
                mode: SearchMode::Auto,
                include_paths: Vec::new(),
                exclude_paths: Vec::new(),
                focus_paths: Vec::new(),
                max_results: Some(5),
                max_tokens: Some(100),
                context_lines: Some(1),
                case_sensitive: false,
                all_occurrences: false,
                prefer_structural: false,
                receipt_id: None,
                cursor: None,
            })
            .await
            .expect("search");
        assert!(!search.hits.is_empty());
        assert!(search.meta.emitted_tokens <= 100);

        let first = services
            .read(ReadRequest {
                path: "lib.rs".into(),
                start_line: Some(1),
                end_line: Some(1),
                symbol: None,
                heading: None,
                heading_occurrence: None,
                continuation_cursor: None,
                max_tokens: Some(100),
                expected_hash: None,
                delta: false,
                receipt_id: None,
            })
            .await
            .expect("read");
        let second = services
            .read(ReadRequest {
                path: "lib.rs".into(),
                start_line: Some(1),
                end_line: Some(1),
                symbol: None,
                heading: None,
                heading_occurrence: None,
                continuation_cursor: None,
                max_tokens: Some(100),
                expected_hash: Some(first.content_hash),
                delta: false,
                receipt_id: None,
            })
            .await
            .expect("read delta");
        assert_eq!(second.status, ReadStatus::NotModified);
        assert!(second.content.is_none());
        assert_eq!(second.meta.emitted_tokens, 0);
    }

    #[tokio::test]
    async fn adaptive_context_ranges_keep_the_match_and_complete_small_declarations() {
        let root = tempfile::tempdir().expect("root");
        let mut source = String::from("fn large() {\n");
        for index in 0..180 {
            source.push_str(&format!("    let value_{index} = {index};\n"));
        }
        source.push_str("}\n\nfn small() { answer(); }\n");
        fs::write(root.path().join("lib.rs"), source).expect("source");
        let config =
            Config::discover(root.path(), Some(root.path().join("db.sqlite"))).expect("config");
        let services = Services::open(config).expect("services");
        services.index(false).await.expect("index");
        let file = services
            .storage
            .find_file("lib.rs")
            .expect("find file")
            .expect("indexed file");
        let session = services.storage.begin_read().expect("read session");
        let large = session
            .find_symbol(file.id, "large")
            .expect("find symbol")
            .expect("large symbol");
        let matched_line = 151;
        let enclosing = session
            .find_enclosing_symbol(file.id, matched_line)
            .expect("find enclosing symbol")
            .expect("enclosing symbol");
        assert_eq!(enclosing.name, "large");

        let session = services.storage.begin_read().expect("read session");
        let bounded = services
            .adaptive_context_excerpt(
                &session,
                file.id,
                large.start_line,
                large.end_line,
                matched_line,
                60,
            )
            .expect("bounded excerpt")
            .expect("bounded declaration");
        assert!(bounded.start_line <= matched_line);
        assert!(bounded.end_line >= matched_line);
        assert!(bounded.start_line > large.start_line);
        assert!(bounded.end_line <= large.end_line);

        let small = session
            .find_symbol(file.id, "small")
            .expect("find symbol")
            .expect("small symbol");
        let complete = services
            .adaptive_context_excerpt(
                &session,
                file.id,
                small.start_line,
                small.end_line,
                small.start_line,
                1_000,
            )
            .expect("complete excerpt")
            .expect("complete declaration");
        assert_eq!(complete.start_line, small.start_line);
        assert_eq!(complete.end_line, small.end_line);
    }

    #[tokio::test]
    async fn search_cursor_tracks_candidates_consumed_by_token_filter() {
        let root = tempfile::tempdir().expect("root");
        for name in ["a.rs", "b.rs", "c.rs"] {
            fs::write(
                root.path().join(name),
                "const NEEDLE: &str = \"needle with an excerpt too large for one token\";\n",
            )
            .expect("source");
        }
        let config =
            Config::discover(root.path(), Some(root.path().join("db.sqlite"))).expect("config");
        let services = Services::open(config).expect("services");
        services.index(false).await.expect("index");

        let request = SearchRequest {
            query: "needle".into(),
            mode: SearchMode::Text,
            include_paths: Vec::new(),
            exclude_paths: Vec::new(),
            focus_paths: Vec::new(),
            max_results: Some(2),
            max_tokens: Some(1),
            context_lines: Some(0),
            case_sensitive: false,
            all_occurrences: false,
            prefer_structural: false,
            receipt_id: None,
            cursor: None,
        };
        let response = services.search(request.clone()).await.expect("search");

        assert!(response.hits.is_empty());
        let cursor = response
            .meta
            .next_cursor
            .expect("unscanned candidates require another page");

        let final_page = services
            .search(SearchRequest {
                cursor: Some(cursor),
                ..request
            })
            .await
            .expect("final search page");
        assert!(final_page.hits.is_empty());
        assert!(final_page.meta.next_cursor.is_none());
    }

    #[tokio::test]
    async fn cancellable_service_stops_before_blocking_work() {
        let root = tempfile::tempdir().expect("root");
        fs::write(root.path().join("lib.rs"), "fn answer() -> u8 { 42 }\n").expect("source");
        let config =
            Config::discover(root.path(), Some(root.path().join("db.sqlite"))).expect("config");
        let services = Services::open(config).expect("services");
        services.index(false).await.expect("index");

        let cancellation = CancellationToken::new();
        cancellation.cancel();
        let error = services
            .files_cancellable(
                FilesRequest {
                    operation: FileOperation::Tree,
                    path: None,
                    query: None,
                    pattern: None,
                    max_results: Some(10),
                    cursor: None,
                    depth: Some(2),
                },
                cancellation,
            )
            .await
            .expect_err("pre-cancelled request should stop");
        assert!(matches!(error, Error::Cancelled));
    }

    #[tokio::test]
    async fn token_savings_uses_the_shared_blocking_executor() {
        let root = tempfile::tempdir().expect("root");
        let config =
            Config::discover(root.path(), Some(root.path().join("db.sqlite"))).expect("config");
        let mut services = Services::open(config).expect("services");
        services.blocking_executor = executor::BlockingExecutor::new(1, 1, Duration::from_secs(30));

        let gate = Arc::new((std::sync::Mutex::new(false), std::sync::Condvar::new()));
        let started = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let blocker = {
            let executor = services.blocking_executor.clone();
            let gate = Arc::clone(&gate);
            let started = Arc::clone(&started);
            tokio::spawn(async move {
                executor
                    .run(CancellationToken::new(), move |_| {
                        started.store(true, Ordering::SeqCst);
                        let (open, changed) = &*gate;
                        let mut open = open.lock().expect("gate lock");
                        while !*open {
                            open = changed.wait(open).expect("gate wait");
                        }
                        Ok(())
                    })
                    .await
            })
        };
        while !started.load(Ordering::SeqCst) {
            tokio::task::yield_now().await;
        }

        assert!(matches!(
            services.token_savings().await,
            Err(Error::RetrievalOverloaded)
        ));

        let (open, changed) = &*gate;
        *open.lock().expect("gate lock") = true;
        changed.notify_all();
        blocker
            .await
            .expect("blocker task")
            .expect("blocker result");
    }

    #[test]
    fn request_snapshot_ignores_concurrent_generation_publish() {
        let root = tempfile::tempdir().expect("root");
        let config =
            Config::discover(root.path(), Some(root.path().join("db.sqlite"))).expect("config");
        let services = Services::open(config).expect("services");
        let first = services
            .storage
            .full_reconcile("hash-a", Vec::new())
            .expect("initial generation");
        assert_eq!(first, 1);

        // One snapshot assembly must report the generation pinned at open, even
        // if a concurrent publish advances the committed generation mid-request.
        let observed = services
            .consistent(|session, generation| {
                assert_eq!(generation, first);
                assert_eq!(session.repository_generation()?, first);
                services
                    .storage
                    .full_reconcile("hash-b", Vec::new())
                    .expect("concurrent publish");
                assert_eq!(
                    session.repository_generation()?,
                    first,
                    "DEFERRED snapshot must not observe the concurrent publish"
                );
                Ok(generation)
            })
            .expect("snapshot assembly");
        assert_eq!(observed, first);
        assert_eq!(
            services
                .storage
                .repository_generation()
                .expect("latest generation"),
            first + 1
        );
    }

    #[test]
    fn pinned_snapshot_operation_errors_are_not_retried() {
        use std::cell::Cell;

        let root = tempfile::tempdir().expect("root");
        let config =
            Config::discover(root.path(), Some(root.path().join("db.sqlite"))).expect("config");
        let services = Services::open(config).expect("services");
        services
            .storage
            .full_reconcile("hash-a", Vec::new())
            .expect("initial generation");
        let calls = Cell::new(0);

        let error = services
            .consistent(|_, _| {
                calls.set(calls.get() + 1);
                Err::<(), _>(Error::Io(std::io::Error::other("live read failed")))
            })
            .expect_err("operation error");

        assert!(matches!(error, Error::Io(_)));
        assert_eq!(calls.get(), 1);
    }

    #[tokio::test]
    async fn regex_candidate_overflow_is_not_reported_as_complete() {
        use crate::storage::{ChunkInput, IndexedFile};

        let root = tempfile::tempdir().expect("root");
        let config =
            Config::discover(root.path(), Some(root.path().join("db.sqlite"))).expect("config");
        let services = Services::open(config).expect("services");
        let files = (0..=2_000)
            .map(|index| IndexedFile {
                path: format!("file_{index:04}.rs"),
                language: Some("rust".into()),
                structurally_complete: true,
                size_bytes: 6,
                modified_ns: None,
                content_hash: format!("hash-{index}"),
                chunks: vec![ChunkInput {
                    content: "needle".into(),
                    start_line: 1,
                    end_line: 1,
                    start_byte: 0,
                    end_byte: 6,
                    token_count: 1,
                }],
                symbols: Vec::new(),
                references: Vec::new(),
                imports: Vec::new(),
            })
            .collect();
        services
            .storage
            .full_reconcile("hash-a", files)
            .expect("indexed fixture");

        let error = services
            .search(SearchRequest {
                query: "needle".into(),
                mode: SearchMode::Regex,
                include_paths: Vec::new(),
                exclude_paths: Vec::new(),
                focus_paths: Vec::new(),
                max_results: Some(100),
                max_tokens: Some(10_000),
                context_lines: Some(0),
                case_sensitive: true,
                all_occurrences: false,
                prefer_structural: false,
                receipt_id: None,
                cursor: None,
            })
            .await
            .expect_err("candidate overflow must be explicit");

        assert!(matches!(error, Error::LimitExceeded));
    }
}
