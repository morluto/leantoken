use std::fs;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use std::thread;
use std::time::{Duration, Instant};

use tokio_util::sync::CancellationToken;

use crate::coordination::{CacheLease, IndexCoordination, IndexLeadership};
use crate::error::RetryableOperation;
use crate::indexer::Indexer;
use crate::model::*;
use crate::storage::{ReadSession, Storage, StorageCounts};
use crate::{Config, Error, Result};

mod context;
mod files;
mod read;
mod search;
mod validation;

const STARTUP_BUSY_TIMEOUT: Duration = Duration::from_millis(250);
const STARTUP_RETRY_INITIAL_DELAY: Duration = Duration::from_millis(25);
const STARTUP_RETRY_MAX_DELAY: Duration = Duration::from_millis(500);
const CANCELLATION_POLL_INTERVAL: Duration = Duration::from_millis(25);
const TOKEN_SAVINGS_ESTIMATE_BASIS: &str =
    "requested read ranges or whole source files represented in each response";

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
    coordination: IndexCoordination,
    _cache_lease: CacheLease,
    active_reconciliations: Arc<AtomicUsize>,
}

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
        let coordination = IndexCoordination::for_database(&config.database_path);
        Ok(Self {
            config,
            storage,
            indexer,
            coordination,
            _cache_lease: cache_lease,
            active_reconciliations: Arc::new(AtomicUsize::new(0)),
        })
    }

    #[must_use]
    /// Return the resolved repository configuration.
    pub fn config(&self) -> &Config {
        &self.config
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
        active_reconciliations.fetch_add(1, Ordering::AcqRel);
        tokio::task::spawn_blocking(move || {
            let _active = ActiveReconciliation(active_reconciliations);
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
        active_reconciliations.fetch_add(1, Ordering::AcqRel);
        tokio::task::spawn_blocking(move || {
            let _active = ActiveReconciliation(active_reconciliations);
            let operation = this.coordination.acquire_operation(&cancellation)?;
            let result = this
                .indexer
                .reconcile_paths_cancellable_report(&paths, &cancellation);
            operation.release()?;
            result
        })
        .await?
    }

    /// Attempt to own automatic indexing and watching for this cache.
    pub fn try_acquire_index_leadership(&self) -> Result<Option<IndexLeadership>> {
        self.coordination.try_acquire_leadership()
    }

    /// Return index counts, generation, and freshness.
    pub async fn status(&self) -> Result<StatusResponse> {
        let this = self.clone();
        tokio::task::spawn_blocking(move || this.status_sync()).await?
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
        tokio::task::spawn_blocking(move || this.token_savings_sync()).await?
    }

    fn token_savings_sync(&self) -> Result<TokenSavingsResponse> {
        let tokenizer = self.config.tokenizer.name();
        let mut stored = self.storage.token_savings(tokenizer)?;
        let mut tracked_requests = 0u64;
        let mut baseline_source_tokens = 0u64;
        let mut emitted_source_tokens = 0u64;
        let mut estimated_source_tokens_saved = 0u64;
        let by_operation = TokenSavingsOperation::ALL
            .into_iter()
            .map(|operation| {
                let record = stored.remove(operation.as_str()).unwrap_or_default();
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
        Ok(TokenSavingsResponse {
            tokenizer: tokenizer.to_owned(),
            token_count_exact: self.config.tokenizer.is_exact(),
            estimate_basis: TOKEN_SAVINGS_ESTIMATE_BASIS.to_owned(),
            tracked_requests,
            baseline_source_tokens,
            emitted_source_tokens,
            estimated_source_tokens_saved,
            by_operation,
        })
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
        if consistency == IndexConsistency::WorkingTree {
            self.index_cancellable(false, cancellation).await?;
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
            repository_generation: generation,
            freshness: self.freshness(),
            emitted_tokens,
            token_count_exact: self.config.tokenizer.is_exact(),
            next_cursor,
        }
    }

    pub(super) fn record_token_savings(
        &self,
        operation: TokenSavingsOperation,
        baseline_source_tokens: usize,
        emitted_source_tokens: usize,
    ) {
        match self.storage.record_token_savings(
            self.config.tokenizer.name(),
            operation,
            baseline_source_tokens,
            emitted_source_tokens,
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
        languages: counts
            .languages
            .into_iter()
            .map(|(language, files)| LanguageCount { language, files })
            .collect(),
        warnings: Vec::new(),
    }
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

struct ActiveReconciliation(Arc<AtomicUsize>);

impl Drop for ActiveReconciliation {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
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
    use tokio_util::sync::CancellationToken;

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
                max_tokens: Some(100),
                expected_hash: None,
            })
            .await
            .expect("read");
        let second = services
            .read(ReadRequest {
                path: "lib.rs".into(),
                start_line: Some(1),
                end_line: Some(1),
                symbol: None,
                max_tokens: Some(100),
                expected_hash: Some(first.content_hash),
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
                cursor: None,
            })
            .await
            .expect_err("candidate overflow must be explicit");

        assert!(matches!(error, Error::LimitExceeded));
    }
}
