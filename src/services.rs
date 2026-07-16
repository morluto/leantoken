use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fs;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use std::thread;
use std::time::{Duration, Instant};

use globset::Glob;
use nucleo_matcher::pattern::{AtomKind, CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config as MatcherConfig, Matcher};
use tokio_util::sync::CancellationToken;

use crate::coordination::{IndexCoordination, IndexLeadership};
use crate::error::RetryableOperation;
use crate::indexer::Indexer;
use crate::model::*;
use crate::ranking::{self, Candidate};
use crate::repository::{git_changed_paths, resolve_existing, validate_relative};
use crate::storage::{ChunkHit, FileRecord, ReferenceHit, Storage, SymbolHit};
use crate::text::{byte_range_to_line_range, excerpt, expand_terms, hash, identifier_words};
use crate::{Config, Error, Result};

const MAX_QUERY_BYTES: usize = 64 * 1024;
const MAX_PATTERN_BYTES: usize = 4 * 1024;
const MAX_PATH_BYTES: usize = 4 * 1024;
const MAX_INPUT_ITEMS: usize = 256;
const GIT_CHANGED_PATHS_MAX: usize = 512;
const MIN_CONTEXT_RANGE_LINES: usize = 12;
const MAX_CONTEXT_RANGE_LINES: usize = 128;
const STARTUP_BUSY_TIMEOUT: Duration = Duration::from_millis(250);
const STARTUP_RETRY_INITIAL_DELAY: Duration = Duration::from_millis(25);
const STARTUP_RETRY_MAX_DELAY: Duration = Duration::from_millis(500);
const CANCELLATION_POLL_INTERVAL: Duration = Duration::from_millis(25);

#[derive(Debug, Clone)]
/// Shared application services used by both CLI and MCP adapters.
///
/// Blocking filesystem and SQLite work runs on Tokio's blocking pool. Index
/// reconciliations are serialized across processes, while reads use committed
/// SQLite WAL generations and retry if a generation changes mid-response.
pub struct Services {
    config: Arc<Config>,
    storage: Storage,
    indexer: Indexer,
    coordination: IndexCoordination,
    active_reconciliations: Arc<AtomicUsize>,
}

struct StoredExcerpt {
    content: String,
    start_line: usize,
    end_line: usize,
}

impl Services {
    /// Open the SQLite index and construct retrieval services.
    pub fn open(config: Config) -> Result<Self> {
        let coordination = IndexCoordination::for_database(&config.database_path);
        let cancellation = CancellationToken::new();
        let _initialization = coordination.acquire_initialization(&cancellation)?;
        Self::open_once(&config, None)
    }

    /// Open services under exclusive cache initialization ownership, retrying
    /// transient SQLite contention until the caller cancels.
    pub fn open_cancellable(config: Config, cancellation: &CancellationToken) -> Result<Self> {
        let coordination = IndexCoordination::for_database(&config.database_path);
        let _initialization = coordination.acquire_initialization(cancellation)?;
        let mut delay = STARTUP_RETRY_INITIAL_DELAY;
        let mut attempt = 0u32;

        loop {
            check_cancelled(cancellation)?;
            match Self::open_once(&config, Some(STARTUP_BUSY_TIMEOUT)) {
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

    fn open_once(config: &Config, startup_timeout: Option<Duration>) -> Result<Self> {
        let open_storage = || match startup_timeout {
            Some(timeout) => Storage::open_with_startup_timeout(&config.database_path, timeout),
            None => Storage::open(&config.database_path),
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
        Ok(Self::from_parts(Arc::new(config.clone()), storage))
    }

    #[must_use]
    fn from_parts(config: Arc<Config>, storage: Storage) -> Self {
        let indexer = Indexer::new(Arc::clone(&config), storage.clone());
        let coordination = IndexCoordination::for_database(&config.database_path);
        Self {
            config,
            storage,
            indexer,
            coordination,
            active_reconciliations: Arc::new(AtomicUsize::new(0)),
        }
    }

    #[must_use]
    /// Return the resolved repository configuration.
    pub fn config(&self) -> &Config {
        &self.config
    }

    /// Reconcile repository files into one committed index generation.
    pub async fn index(&self, rebuild: bool) -> Result<IndexResponse> {
        self.index_cancellable(rebuild, CancellationToken::new())
            .await
    }

    /// Reconcile repository files while honoring caller-owned cancellation.
    pub async fn index_cancellable(
        &self,
        rebuild: bool,
        cancellation: CancellationToken,
    ) -> Result<IndexResponse> {
        let this = self.clone();
        let active_reconciliations = Arc::clone(&self.active_reconciliations);
        active_reconciliations.fetch_add(1, Ordering::AcqRel);
        tokio::task::spawn_blocking(move || {
            let _active = ActiveReconciliation(active_reconciliations);
            let _operation = this.coordination.acquire_operation(&cancellation)?;
            this.indexer.reconcile_cancellable(rebuild, &cancellation)
        })
        .await?
    }

    /// Reconcile watcher-reported paths, falling back internally when a
    /// repository-wide scan is required for correctness.
    pub async fn index_paths(&self, paths: Vec<String>) -> Result<IndexResponse> {
        self.index_paths_cancellable(paths, CancellationToken::new())
            .await
    }

    /// Reconcile watcher-reported paths while honoring caller-owned cancellation.
    pub async fn index_paths_cancellable(
        &self,
        paths: Vec<String>,
        cancellation: CancellationToken,
    ) -> Result<IndexResponse> {
        let this = self.clone();
        let active_reconciliations = Arc::clone(&self.active_reconciliations);
        active_reconciliations.fetch_add(1, Ordering::AcqRel);
        tokio::task::spawn_blocking(move || {
            let _active = ActiveReconciliation(active_reconciliations);
            let _operation = this.coordination.acquire_operation(&cancellation)?;
            this.indexer
                .reconcile_paths_cancellable(&paths, &cancellation)
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

    /// Discover repository paths.
    pub async fn files(&self, request: FilesRequest) -> Result<FilesResponse> {
        self.files_cancellable(request, CancellationToken::new())
            .await
    }

    pub async fn files_cancellable(
        &self,
        request: FilesRequest,
        cancellation: CancellationToken,
    ) -> Result<FilesResponse> {
        let this = self.clone();
        tokio::task::spawn_blocking(move || this.files_sync(request, &cancellation)).await?
    }

    /// Search indexed lexical and structural evidence.
    pub async fn search(&self, request: SearchRequest) -> Result<SearchResponse> {
        self.search_cancellable(request, CancellationToken::new())
            .await
    }

    pub async fn search_cancellable(
        &self,
        request: SearchRequest,
        cancellation: CancellationToken,
    ) -> Result<SearchResponse> {
        let this = self.clone();
        tokio::task::spawn_blocking(move || this.search_sync(request, &cancellation)).await?
    }

    /// Return bounded structural outlines for indexed files.
    pub async fn outline(&self, request: OutlineRequest) -> Result<OutlineResponse> {
        self.outline_cancellable(request, CancellationToken::new())
            .await
    }

    pub async fn outline_cancellable(
        &self,
        request: OutlineRequest,
        cancellation: CancellationToken,
    ) -> Result<OutlineResponse> {
        let this = self.clone();
        tokio::task::spawn_blocking(move || this.outline_sync(request, &cancellation)).await?
    }

    /// Read a bounded live source range and report index staleness.
    pub async fn read(&self, request: ReadRequest) -> Result<ReadResponse> {
        self.read_cancellable(request, CancellationToken::new())
            .await
    }

    pub async fn read_cancellable(
        &self,
        request: ReadRequest,
        cancellation: CancellationToken,
    ) -> Result<ReadResponse> {
        let this = self.clone();
        tokio::task::spawn_blocking(move || this.read_sync(request, &cancellation)).await?
    }

    /// Select ranked task evidence within an exact source-token budget.
    pub async fn context(&self, request: ContextRequest) -> Result<ContextResponse> {
        self.context_cancellable(request, CancellationToken::new())
            .await
    }

    pub async fn context_cancellable(
        &self,
        request: ContextRequest,
        cancellation: CancellationToken,
    ) -> Result<ContextResponse> {
        let this = self.clone();
        tokio::task::spawn_blocking(move || this.context_sync(request, &cancellation)).await?
    }

    fn status_sync(&self) -> Result<StatusResponse> {
        self.consistent_allow_empty(|generation| {
            let counts = self.storage.counts()?;
            Ok(StatusResponse {
                repository_root: self.config.root.display().to_string(),
                database_path: self.config.database_path.display().to_string(),
                repository_generation: generation,
                freshness: self.freshness(),
                file_count: counts.files,
                chunk_count: counts.chunks,
                symbol_count: counts.symbols,
                languages: counts
                    .languages
                    .into_iter()
                    .map(|(language, files)| LanguageCount { language, files })
                    .collect(),
                warnings: Vec::new(),
            })
        })
    }

    fn files_sync(
        &self,
        request: FilesRequest,
        cancellation: &CancellationToken,
    ) -> Result<FilesResponse> {
        check_cancelled(cancellation)?;
        validate_optional_input(request.path.as_deref(), "path", MAX_PATH_BYTES)?;
        validate_optional_input(request.query.as_deref(), "query", MAX_QUERY_BYTES)?;
        validate_optional_input(request.pattern.as_deref(), "pattern", MAX_PATTERN_BYTES)?;
        self.consistent(|generation| {
            let limit = self.result_limit(request.max_results);
            let offset = parse_cursor(request.cursor.as_deref(), generation)?;
            let files = self.all_files(cancellation)?;
            let mut entries = match request.operation {
                FileOperation::Tree => {
                    tree_entries(&files, request.path.as_deref(), request.depth)?
                }
                FileOperation::Find => fuzzy_entries(&files, request.query.as_deref())?,
                FileOperation::Glob => glob_entries(&files, request.pattern.as_deref())?,
            };
            let has_more = offset.saturating_add(limit) < entries.len();
            entries = entries.into_iter().skip(offset).take(limit).collect();
            Ok(FilesResponse {
                entries,
                meta: self.meta(
                    generation,
                    0,
                    has_more.then(|| make_cursor(generation, offset + limit)),
                ),
            })
        })
    }

    fn search_sync(
        &self,
        request: SearchRequest,
        cancellation: &CancellationToken,
    ) -> Result<SearchResponse> {
        check_cancelled(cancellation)?;
        if request.query.trim().is_empty() {
            return Err(Error::InvalidRequest(
                "search query must not be empty".into(),
            ));
        }
        validate_input(&request.query, "search query", MAX_QUERY_BYTES)?;
        validate_patterns(&request.include_paths)?;
        validate_patterns(&request.exclude_paths)?;
        validate_patterns(&request.focus_paths)?;
        self.consistent(|generation| {
            let limit = self.result_limit(request.max_results);
            let token_limit = self.token_limit(request.max_tokens, self.config.default_read_tokens);
            let offset = parse_cursor(request.cursor.as_deref(), generation)?;
            let context_lines = request
                .context_lines
                .unwrap_or(self.config.context_lines)
                .min(20);
            let mut hits = Vec::new();

            if matches!(
                request.mode,
                SearchMode::Auto | SearchMode::Identifier | SearchMode::Symbol
            ) {
                for hit in self.storage.search_symbols(
                    &request.query,
                    request.case_sensitive,
                    limit * 4,
                )? {
                    check_cancelled(cancellation)?;
                    if path_allowed(&hit.path, &request.include_paths, &request.exclude_paths)?
                        && let Some(search_hit) =
                            self.symbol_search_hit(hit, &request.query, context_lines)?
                    {
                        hits.push(search_hit);
                    }
                }
            }
            if matches!(
                request.mode,
                SearchMode::Auto | SearchMode::Identifier | SearchMode::Reference
            ) {
                for hit in self.storage.search_references(
                    &request.query,
                    request.case_sensitive,
                    limit * 4,
                )? {
                    check_cancelled(cancellation)?;
                    if path_allowed(&hit.path, &request.include_paths, &request.exclude_paths)?
                        && let Some(search_hit) =
                            self.reference_search_hit(hit, &request.query, context_lines)?
                    {
                        hits.push(search_hit);
                    }
                }
            }

            let lexical = match request.mode {
                SearchMode::Regex => self.regex_hits(&request, limit * 20, cancellation)?,
                SearchMode::Text | SearchMode::Auto => {
                    if request.query.chars().count() >= 3 {
                        self.storage.search_trigram(&request.query, limit * 8)?
                    } else {
                        self.storage
                            .search_word(&fts_quote(&request.query), limit * 8)?
                    }
                }
                SearchMode::Identifier => self
                    .storage
                    .search_word(&fts_quote(&request.query), limit * 8)?,
                SearchMode::Symbol | SearchMode::Reference => Vec::new(),
            };
            for hit in lexical {
                check_cancelled(cancellation)?;
                if path_allowed(&hit.path, &request.include_paths, &request.exclude_paths)?
                    && let Some(search_hit) = chunk_search_hit(
                        hit,
                        &request.query,
                        request.case_sensitive,
                        context_lines,
                        matches!(request.mode, SearchMode::Regex),
                    )?
                {
                    hits.push(search_hit);
                }
            }

            apply_focus(&mut hits, &request.focus_paths)?;
            hits.sort_by(|left, right| {
                right
                    .score
                    .total_cmp(&left.score)
                    .then_with(|| left.path.cmp(&right.path))
                    .then_with(|| left.start_line.cmp(&right.start_line))
            });
            let mut seen = HashSet::new();
            hits.retain(|hit| {
                seen.insert((
                    hit.path.clone(),
                    hit.start_line,
                    hit.end_line,
                    hit.content_hash.clone(),
                ))
            });

            let mut emitted_tokens = 0usize;
            let mut selected = Vec::new();
            let remaining = hits.len().saturating_sub(offset);
            let mut consumed = 0usize;
            for hit in hits.into_iter().skip(offset) {
                check_cancelled(cancellation)?;
                if selected.len() >= limit {
                    break;
                }
                consumed += 1;
                let count = self.config.tokenizer.count(&hit.excerpt);
                if emitted_tokens.saturating_add(count) > token_limit {
                    continue;
                }
                emitted_tokens += count;
                selected.push(hit);
            }
            let has_more = consumed < remaining;
            Ok(SearchResponse {
                hits: selected,
                meta: self.meta(
                    generation,
                    emitted_tokens,
                    has_more.then(|| make_cursor(generation, offset + consumed)),
                ),
            })
        })
    }

    fn outline_sync(
        &self,
        request: OutlineRequest,
        cancellation: &CancellationToken,
    ) -> Result<OutlineResponse> {
        check_cancelled(cancellation)?;
        if request.paths.is_empty() {
            return Err(Error::InvalidRequest(
                "outline requires at least one path".into(),
            ));
        }
        if request.paths.len() > MAX_INPUT_ITEMS {
            return Err(Error::LimitExceeded);
        }
        for path in &request.paths {
            validate_input(path, "path", MAX_PATH_BYTES)?;
        }
        validate_optional_input(
            request.symbol_name.as_deref(),
            "symbol name",
            MAX_PATTERN_BYTES,
        )?;
        validate_optional_input(
            request.symbol_kind.as_deref(),
            "symbol kind",
            MAX_PATTERN_BYTES,
        )?;
        self.consistent(|generation| {
            let limit = self.result_limit(request.max_results);
            let token_limit = self.token_limit(request.max_tokens, self.config.default_read_tokens);
            let mut remaining = limit;
            let mut emitted_tokens = 0usize;
            let mut files = Vec::new();
            for path in &request.paths {
                check_cancelled(cancellation)?;
                validate_relative(path)?;
                let file = self
                    .storage
                    .find_file(path)?
                    .ok_or_else(|| Error::NotIndexed(path.clone()))?;
                let mut symbols = self
                    .storage
                    .get_symbols_for_file_filtered(
                        file.id,
                        request.symbol_name.as_deref(),
                        request.symbol_kind.as_deref(),
                        remaining.max(1),
                    )?
                    .into_iter()
                    .map(storage_symbol)
                    .collect::<Vec<_>>();
                symbols.retain(|symbol| {
                    let cost = symbol
                        .signature
                        .as_deref()
                        .map_or(1, |value| self.config.tokenizer.count(value));
                    if remaining == 0 || emitted_tokens.saturating_add(cost) > token_limit {
                        false
                    } else {
                        remaining -= 1;
                        emitted_tokens += cost;
                        true
                    }
                });
                let mut imports = self
                    .storage
                    .get_imports_for_file(file.id, limit)?
                    .into_iter()
                    .map(|import| Import {
                        raw_target: import.raw_target,
                        resolved_path: import.resolved_path,
                        line: import.line,
                    })
                    .collect::<Vec<_>>();
                imports.retain(|import| {
                    let cost = self.config.tokenizer.count(&import.raw_target)
                        + import
                            .resolved_path
                            .as_deref()
                            .map_or(0, |value| self.config.tokenizer.count(value));
                    if remaining == 0 || emitted_tokens.saturating_add(cost) > token_limit {
                        false
                    } else {
                        remaining -= 1;
                        emitted_tokens += cost;
                        true
                    }
                });
                files.push(OutlineFile {
                    path: file.path,
                    language: file.language.clone(),
                    structurally_complete: file.structurally_complete,
                    symbols,
                    imports,
                });
                if remaining == 0 {
                    break;
                }
            }
            Ok(OutlineResponse {
                files,
                meta: self.meta(generation, emitted_tokens, None),
            })
        })
    }

    fn read_sync(
        &self,
        request: ReadRequest,
        cancellation: &CancellationToken,
    ) -> Result<ReadResponse> {
        check_cancelled(cancellation)?;
        validate_input(&request.path, "path", MAX_PATH_BYTES)?;
        validate_optional_input(request.symbol.as_deref(), "symbol", MAX_PATTERN_BYTES)?;
        validate_optional_input(request.expected_hash.as_deref(), "expected hash", 128)?;
        validate_relative(&request.path)?;
        if request.symbol.is_some() && (request.start_line.is_some() || request.end_line.is_some())
        {
            return Err(Error::InvalidRequest(
                "read accepts either symbol or line range, not both".into(),
            ));
        }
        self.consistent(|generation| {
            check_cancelled(cancellation)?;
            self.read_at_generation(&request, generation)
        })
    }

    fn read_at_generation(&self, request: &ReadRequest, generation: u64) -> Result<ReadResponse> {
        let indexed = self
            .storage
            .find_file(&request.path)?
            .ok_or_else(|| Error::NotIndexed(request.path.clone()))?;
        let path = resolve_existing(&self.config.root, &request.path)?;
        let bytes = fs::read(path)?;
        let source = std::str::from_utf8(&bytes)
            .map_err(|_| Error::InvalidRequest("requested file is not UTF-8 text".into()))?;
        let line_count = source.lines().count().max(1);

        let (start_line, requested_end) = if let Some(symbol_name) = &request.symbol {
            let symbol = self
                .storage
                .find_symbol(indexed.id, symbol_name)?
                .ok_or_else(|| Error::NotIndexed(format!("{}::{symbol_name}", request.path)))?;
            (symbol.start_line, symbol.end_line)
        } else {
            (
                request.start_line.unwrap_or(1),
                request.end_line.unwrap_or(line_count),
            )
        };
        if start_line == 0 || requested_end < start_line || start_line > line_count {
            return Err(Error::InvalidRequest(
                "invalid or out-of-range line range".into(),
            ));
        }
        let requested_end = requested_end.min(line_count);
        let content = crate::text::excerpt(source, start_line, requested_end);
        let max_tokens = self.token_limit(request.max_tokens, self.config.default_read_tokens);
        let (content, emitted_tokens) = self.config.tokenizer.truncate(&content, max_tokens);
        let returned_lines = content
            .lines()
            .count()
            .max(usize::from(!content.is_empty()));
        let end_line = if returned_lines == 0 {
            start_line
        } else {
            start_line + returned_lines - 1
        };
        let content_hash = hash(content);
        let full_hash = crate::text::hash_bytes(&bytes);
        let index_stale = indexed.content_hash != full_hash;
        let indexed_hash = Some(indexed.content_hash);
        let not_modified = request.expected_hash.as_deref() == Some(content_hash.as_str());

        Ok(ReadResponse {
            path: request.path.clone(),
            status: if not_modified {
                ReadStatus::NotModified
            } else {
                ReadStatus::Content
            },
            start_line,
            end_line,
            content: (!not_modified).then(|| content.to_string()),
            content_hash,
            indexed_hash,
            index_stale,
            meta: self.meta(
                generation,
                if not_modified { 0 } else { emitted_tokens },
                None,
            ),
        })
    }

    fn file_change_boost(
        file: Option<&FileRecord>,
        path: &str,
        changed_paths: &HashSet<String>,
        prior_generation: Option<u64>,
    ) -> f64 {
        let mut boost = 0.0;

        if let Some(prior) = prior_generation
            && file.is_some_and(|f| f.generation > prior)
        {
            boost += 1.0;
        }

        if changed_paths.contains(path) {
            boost += 1.0;
        }

        boost
    }

    fn context_sync(
        &self,
        request: ContextRequest,
        cancellation: &CancellationToken,
    ) -> Result<ContextResponse> {
        check_cancelled(cancellation)?;
        if request.task.trim().is_empty() || request.token_budget == 0 {
            return Err(Error::InvalidRequest(
                "context requires a task and positive token budget".into(),
            ));
        }
        validate_input(&request.task, "task", MAX_QUERY_BYTES)?;
        validate_patterns(&request.focus_paths)?;
        validate_patterns(&request.exclude_paths)?;
        if request.focus_symbols.len() > MAX_INPUT_ITEMS {
            return Err(Error::LimitExceeded);
        }
        for symbol in &request.focus_symbols {
            validate_input(symbol, "focus symbol", MAX_PATTERN_BYTES)?;
        }
        if request.token_budget > self.config.max_output_tokens {
            return Err(Error::LimitExceeded);
        }
        if request.known_hashes.len() > MAX_INPUT_ITEMS {
            return Err(Error::LimitExceeded);
        }
        for hash in &request.known_hashes {
            validate_input(hash, "known hash", 128)?;
        }
        let changed_paths = git_changed_paths(&self.config.root, GIT_CHANGED_PATHS_MAX)
            .unwrap_or_else(|error| {
                tracing::debug!(%error, "working-tree signal unavailable");
                HashSet::new()
            });
        self.consistent(|generation| {
            let queries = context_queries(&request.task, 12);
            let terms = queries
                .iter()
                .map(|query| query.value.clone())
                .collect::<Vec<_>>();
            let files = self.all_files(cancellation)?;
            let file_map: HashMap<String, FileRecord> = files
                .into_iter()
                .map(|file| (file.path.clone(), file))
                .collect();
            let mut candidates = Vec::new();
            let mut query_fusion = HashMap::<String, HashMap<String, f64>>::new();

            // Workflow words such as `test` are useful path priors but terrible
            // retrieval queries: nearly every test function becomes a high-
            // scoring symbol candidate. Keep them out of candidate generation.
            for query in queries.iter().filter(|query| query.value != "test") {
                let term = &query.value;
                let concept = query.fusion_key.as_deref().unwrap_or(term);
                check_cancelled(cancellation)?;
                for (rank, hit) in self
                    .storage
                    .search_symbols(term, false, 20)?
                    .into_iter()
                    .enumerate()
                {
                    check_cancelled(cancellation)?;
                    if !path_allowed(&hit.path, &[], &request.exclude_paths)? {
                        continue;
                    }
                    let Some(excerpt) = self.adaptive_context_excerpt(
                        hit.symbol.file_id,
                        hit.symbol.start_line,
                        hit.symbol.end_line,
                        hit.symbol.start_line,
                        request.token_budget,
                    )?
                    else {
                        continue;
                    };
                    let exact = f64::from(hit.symbol.name.eq_ignore_ascii_case(term));
                    let qualified = qualified_symbol_match(
                        concept,
                        &hit.symbol.name,
                        hit.symbol.parent.as_deref(),
                        hit.symbol.signature.as_deref(),
                    );
                    if let Some(fusion_key) = &query.fusion_key {
                        record_query_hit(
                            &mut query_fusion,
                            &hit.path,
                            fusion_key,
                            query.weight,
                            rank,
                        );
                    }
                    let change_boost = Self::file_change_boost(
                        file_map.get(&hit.path),
                        &hit.path,
                        &changed_paths,
                        request.prior_repository_generation,
                    );
                    candidates.push(
                        Candidate::new(
                            &hit.path,
                            excerpt.start_line,
                            excerpt.end_line,
                            excerpt.content,
                        )
                        .match_kind("symbol")
                        .concept(
                            concept,
                            query.weight + f64::from(query.fusion_key.is_some()),
                        )
                        .representation("symbol")
                        .symbol_name(hit.symbol.name)
                        .exact(exact + qualified * 1.5)
                        .symbol(1.0)
                        .path_score(context_path_score(&hit.path, &terms, &request.task))
                        .change_boost(change_boost),
                    );
                }
                for (rank, hit) in self
                    .storage
                    .search_references(term, false, 20)?
                    .into_iter()
                    .enumerate()
                {
                    check_cancelled(cancellation)?;
                    if !path_allowed(&hit.path, &[], &request.exclude_paths)? {
                        continue;
                    }
                    let excerpt = if let Some(symbol) = self
                        .storage
                        .find_enclosing_symbol(hit.reference.file_id, hit.reference.start_line)?
                    {
                        self.adaptive_context_excerpt(
                            hit.reference.file_id,
                            symbol.start_line,
                            symbol.end_line,
                            hit.reference.start_line,
                            request.token_budget,
                        )?
                    } else {
                        None
                    };
                    let excerpt = if excerpt.is_some() {
                        excerpt
                    } else {
                        self.stored_excerpt(
                            hit.reference.file_id,
                            hit.reference.start_line,
                            hit.reference.end_line,
                            2,
                            12,
                        )?
                    };
                    let Some(excerpt) = excerpt else {
                        continue;
                    };
                    if let Some(fusion_key) = &query.fusion_key {
                        record_query_hit(
                            &mut query_fusion,
                            &hit.path,
                            fusion_key,
                            query.weight,
                            rank,
                        );
                    }
                    let change_boost = Self::file_change_boost(
                        file_map.get(&hit.path),
                        &hit.path,
                        &changed_paths,
                        request.prior_repository_generation,
                    );
                    candidates.push(
                        Candidate::new(
                            &hit.path,
                            excerpt.start_line,
                            excerpt.end_line,
                            excerpt.content,
                        )
                        .match_kind("reference")
                        .concept(
                            concept,
                            query.weight + f64::from(query.fusion_key.is_some()),
                        )
                        .symbol_name(hit.reference.name)
                        .reference(1.0)
                        .path_score(context_path_score(&hit.path, &terms, &request.task))
                        .change_boost(change_boost),
                    );
                }
                let lexical = if term.chars().count() >= 3 {
                    self.storage.search_trigram(term, 30)?
                } else {
                    self.storage.search_word(&fts_quote(term), 30)?
                };
                for (rank, hit) in lexical.into_iter().enumerate() {
                    check_cancelled(cancellation)?;
                    if !path_allowed(&hit.path, &[], &request.exclude_paths)? {
                        continue;
                    }
                    let Some(search_hit) = chunk_search_hit(hit.clone(), term, false, 2, false)?
                    else {
                        continue;
                    };
                    let matched_line =
                        matching_line(&hit, term, false).unwrap_or(search_hit.start_line);
                    let excerpt = if let Some(symbol) = self
                        .storage
                        .find_enclosing_symbol(hit.file_id, matched_line)?
                    {
                        self.adaptive_context_excerpt(
                            hit.file_id,
                            symbol.start_line,
                            symbol.end_line,
                            matched_line,
                            request.token_budget,
                        )?
                    } else {
                        None
                    }
                    .unwrap_or(StoredExcerpt {
                        content: search_hit.excerpt.clone(),
                        start_line: search_hit.start_line,
                        end_line: search_hit.end_line,
                    });
                    if let Some(fusion_key) = &query.fusion_key {
                        record_query_hit(
                            &mut query_fusion,
                            &search_hit.path,
                            fusion_key,
                            query.weight,
                            rank,
                        );
                    }
                    let occurrences = hit
                        .content
                        .to_lowercase()
                        .matches(&term.to_lowercase())
                        .count();
                    let change_boost = Self::file_change_boost(
                        file_map.get(&search_hit.path),
                        &search_hit.path,
                        &changed_paths,
                        request.prior_repository_generation,
                    );
                    let candidate = Candidate::new(
                        &search_hit.path,
                        excerpt.start_line,
                        excerpt.end_line,
                        excerpt.content,
                    )
                    .match_kind("text")
                    .concept(
                        concept,
                        query.weight + f64::from(query.fusion_key.is_some()),
                    )
                    .exact(query.weight)
                    .bm25((-hit.score).max(0.0) * 1_000_000.0)
                    .path_score(context_path_score(&search_hit.path, &terms, &request.task))
                    .lexical_frequency_penalty(
                        (occurrences.saturating_sub(5) as f64 / 20.0).min(1.0),
                    )
                    .change_boost(change_boost);
                    candidates.push(candidate);
                }
            }

            apply_query_fusion(&mut candidates, &query_fusion);

            let seed_paths = candidates
                .iter()
                .map(|candidate| candidate.path.clone())
                .collect::<BTreeSet<_>>();
            let mut neighbor_count = 0usize;
            for seed_path in seed_paths.iter().take(24) {
                check_cancelled(cancellation)?;
                let Some(seed_file) = file_map.get(seed_path) else {
                    continue;
                };
                for import in self.storage.get_imports_for_file(seed_file.id, 32)? {
                    check_cancelled(cancellation)?;
                    let Some(target_path) = import.resolved_path else {
                        continue;
                    };
                    if !path_allowed(&target_path, &[], &request.exclude_paths)? {
                        continue;
                    }
                    let Some(target_file) = file_map.get(&target_path) else {
                        continue;
                    };
                    let Some(chunk) = self
                        .storage
                        .get_chunks_for_file(target_file.id, 1)?
                        .into_iter()
                        .next()
                    else {
                        continue;
                    };
                    let end_line = chunk.end_line.min(chunk.start_line + 29);
                    let content = crate::text::excerpt(
                        &chunk.content,
                        1,
                        end_line.saturating_sub(chunk.start_line) + 1,
                    );
                    let change_boost = Self::file_change_boost(
                        Some(target_file),
                        &target_path,
                        &changed_paths,
                        request.prior_repository_generation,
                    );
                    candidates.push(
                        Candidate::new(&target_path, chunk.start_line, end_line, content)
                            .match_kind("import")
                            .concept(seed_path, 0.2)
                            .representation("import_neighbor")
                            .path_score(context_path_score(&target_path, &terms, &request.task))
                            .import_boost(1.0)
                            .change_boost(change_boost),
                    );
                    neighbor_count += 1;
                    if neighbor_count >= 24 {
                        break;
                    }
                }
                if neighbor_count >= 24 {
                    break;
                }
            }

            let mut response = ranking::select_with_tokenizer(
                candidates,
                &request,
                generation,
                self.config.tokenizer,
            );
            response.meta.freshness = self.freshness();
            if response.fragments.is_empty() {
                response
                    .warnings
                    .push("no relevant indexed evidence found".into());
            }
            Ok(response)
        })
    }

    fn symbol_search_hit(
        &self,
        hit: SymbolHit,
        query: &str,
        context: usize,
    ) -> Result<Option<SearchHit>> {
        let Some(excerpt) = self.stored_excerpt(
            hit.symbol.file_id,
            hit.symbol.start_line,
            hit.symbol.end_line,
            context,
            30,
        )?
        else {
            return Ok(None);
        };
        let exact = hit.symbol.name == query || hit.symbol.name.eq_ignore_ascii_case(query);
        Ok(Some(SearchHit {
            path: hit.path,
            start_line: excerpt.start_line,
            end_line: excerpt.end_line,
            content_hash: hash(&excerpt.content),
            excerpt: excerpt.content,
            match_kind: "symbol".into(),
            role: Some(ReferenceRole::Definition),
            symbol: Some(hit.symbol.name),
            enclosing_symbol: hit.symbol.parent,
            score: if exact { 10.0 } else { 7.0 },
            score_reasons: vec![if exact {
                "exact symbol".into()
            } else {
                "symbol".into()
            }],
        }))
    }

    fn reference_search_hit(
        &self,
        hit: ReferenceHit,
        query: &str,
        context: usize,
    ) -> Result<Option<SearchHit>> {
        let Some(excerpt) = self.stored_excerpt(
            hit.reference.file_id,
            hit.reference.start_line,
            hit.reference.end_line,
            context,
            12,
        )?
        else {
            return Ok(None);
        };
        let exact = hit.reference.name == query || hit.reference.name.eq_ignore_ascii_case(query);
        Ok(Some(SearchHit {
            path: hit.path,
            start_line: excerpt.start_line,
            end_line: excerpt.end_line,
            content_hash: hash(&excerpt.content),
            excerpt: excerpt.content,
            match_kind: "reference".into(),
            role: Some(hit.reference.role),
            symbol: Some(hit.reference.name),
            enclosing_symbol: hit.reference.enclosing_symbol,
            score: if exact { 8.0 } else { 5.0 },
            score_reasons: vec![if exact {
                "exact reference".into()
            } else {
                "reference".into()
            }],
        }))
    }

    fn stored_excerpt(
        &self,
        file_id: i64,
        start_line: usize,
        end_line: usize,
        context: usize,
        max_lines: usize,
    ) -> Result<Option<StoredExcerpt>> {
        let first = start_line.saturating_sub(context).max(1);
        let mut last = end_line.saturating_add(context);
        if max_lines > 0 && last.saturating_sub(first).saturating_add(1) > max_lines {
            last = first + max_lines - 1;
        }
        let selected = self.storage.get_chunks_overlapping(file_id, first, last)?;
        let (Some(first_chunk), Some(last_chunk)) = (selected.first(), selected.last()) else {
            return Ok(None);
        };
        last = last.min(last_chunk.end_line);
        let base_line = first_chunk.start_line;
        let mut combined = String::new();
        for chunk in &selected {
            combined.push_str(&chunk.content);
        }
        let local_start = first.saturating_sub(base_line) + 1;
        let local_end = last.saturating_sub(base_line) + 1;
        Ok(Some(StoredExcerpt {
            content: crate::text::excerpt(&combined, local_start, local_end),
            start_line: first,
            end_line: last,
        }))
    }

    fn adaptive_context_excerpt(
        &self,
        file_id: i64,
        declaration_start: usize,
        declaration_end: usize,
        matched_line: usize,
        token_budget: usize,
    ) -> Result<Option<StoredExcerpt>> {
        let Some(full) = self.stored_excerpt(file_id, declaration_start, declaration_end, 0, 0)?
        else {
            return Ok(None);
        };
        let full_tokens = self.config.tokenizer.count(&full.content).max(1);
        if full_tokens <= token_budget {
            return Ok(Some(full));
        }

        let declaration_lines = declaration_end
            .saturating_sub(declaration_start)
            .saturating_add(1);
        let proportional_lines = declaration_lines
            .saturating_mul(token_budget)
            .saturating_div(full_tokens)
            .clamp(MIN_CONTEXT_RANGE_LINES, MAX_CONTEXT_RANGE_LINES)
            .min(declaration_lines);
        let before = proportional_lines / 3;
        let mut start = matched_line.saturating_sub(before).max(declaration_start);
        let mut end = start
            .saturating_add(proportional_lines.saturating_sub(1))
            .min(declaration_end);
        if end.saturating_sub(start).saturating_add(1) < proportional_lines {
            start = end
                .saturating_add(1)
                .saturating_sub(proportional_lines)
                .max(declaration_start);
        }
        end = start
            .saturating_add(proportional_lines.saturating_sub(1))
            .min(declaration_end);
        self.stored_excerpt(file_id, start, end, 0, 0)
    }

    fn regex_hits(
        &self,
        request: &SearchRequest,
        max_candidates: usize,
        cancellation: &CancellationToken,
    ) -> Result<Vec<ChunkHit>> {
        let regex = regex::RegexBuilder::new(&request.query)
            .case_insensitive(!request.case_sensitive)
            .build()?;
        let mut hits = Vec::new();
        for file in self.all_files(cancellation)? {
            check_cancelled(cancellation)?;
            for chunk in self.storage.get_chunks_for_file(file.id, 10_000)? {
                check_cancelled(cancellation)?;
                if regex.is_match(&chunk.content) {
                    hits.push(ChunkHit {
                        chunk_id: chunk.id,
                        file_id: chunk.file_id,
                        path: file.path.clone(),
                        content: chunk.content,
                        start_line: chunk.start_line,
                        end_line: chunk.end_line,
                        start_byte: chunk.start_byte,
                        end_byte: chunk.end_byte,
                        token_count: chunk.token_count,
                        score: 0.0,
                    });
                    if hits.len() >= max_candidates {
                        return Ok(hits);
                    }
                }
            }
        }
        Ok(hits)
    }

    fn all_files(&self, cancellation: &CancellationToken) -> Result<Vec<FileRecord>> {
        let mut files = Vec::new();
        let mut cursor = None;
        loop {
            check_cancelled(cancellation)?;
            let page = self.storage.list_files(1_000, cursor)?;
            if page.is_empty() {
                break;
            }
            cursor = page.last().map(|file| file.id);
            files.extend(page);
        }
        Ok(files)
    }

    fn consistent<T>(&self, operation: impl Fn(u64) -> Result<T>) -> Result<T> {
        self.consistent_inner(false, operation)
    }

    fn consistent_allow_empty<T>(&self, operation: impl Fn(u64) -> Result<T>) -> Result<T> {
        self.consistent_inner(true, operation)
    }

    fn consistent_inner<T>(
        &self,
        allow_empty: bool,
        operation: impl Fn(u64) -> Result<T>,
    ) -> Result<T> {
        for _ in 0..3 {
            let before = self.storage.repository_generation()?;
            if before == 0 && !allow_empty {
                return Err(Error::IndexNotReady);
            }
            let value = operation(before)?;
            if self.storage.repository_generation()? == before {
                return Ok(value);
            }
        }
        Err(Error::RetryableConflict(RetryableOperation::Retrieval))
    }

    fn result_limit(&self, requested: Option<usize>) -> usize {
        requested
            .unwrap_or(self.config.default_results)
            .max(1)
            .min(self.config.max_results)
    }

    fn token_limit(&self, requested: Option<usize>, default: usize) -> usize {
        requested
            .unwrap_or(default)
            .max(1)
            .min(self.config.max_output_tokens)
    }

    fn freshness(&self) -> Freshness {
        let local = self.active_reconciliations.load(Ordering::Acquire) > 0;
        let shared = self.coordination.is_reconciling().unwrap_or(true);
        if local || shared {
            Freshness::Reconciling
        } else {
            Freshness::Current
        }
    }

    fn meta(
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
}

fn is_database_corruption(error: &Error) -> bool {
    matches!(
        sqlite_error_code(error),
        Some(rusqlite::ErrorCode::DatabaseCorrupt | rusqlite::ErrorCode::NotADatabase)
    )
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

fn check_cancelled(cancellation: &CancellationToken) -> Result<()> {
    if cancellation.is_cancelled() {
        Err(Error::Cancelled)
    } else {
        Ok(())
    }
}

fn wait_cancellable(cancellation: &CancellationToken, duration: Duration) -> Result<()> {
    let deadline = Instant::now() + duration;
    loop {
        check_cancelled(cancellation)?;
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Ok(());
        }
        thread::sleep(remaining.min(CANCELLATION_POLL_INTERVAL));
    }
}

fn storage_symbol(symbol: crate::storage::SymbolRecord) -> Symbol {
    Symbol {
        name: symbol.name,
        kind: symbol.kind,
        parent: symbol.parent,
        signature: symbol.signature,
        start_line: symbol.start_line,
        end_line: symbol.end_line,
        start_byte: symbol.start_byte,
        end_byte: symbol.end_byte,
    }
}

fn tree_entries(
    files: &[FileRecord],
    root: Option<&str>,
    depth: Option<usize>,
) -> Result<Vec<FileEntry>> {
    let root = root.unwrap_or("");
    if !root.is_empty() {
        validate_relative(root)?;
    }
    let root = root.trim_matches('/');
    let max_depth = depth.unwrap_or(usize::MAX);
    let root_depth = root.split('/').filter(|part| !part.is_empty()).count();
    let mut entries = BTreeMap::new();
    for file in files {
        if !root.is_empty() && file.path != root && !file.path.starts_with(&format!("{root}/")) {
            continue;
        }
        let parts = file.path.split('/').collect::<Vec<_>>();
        for index in 1..parts.len() {
            let path = parts[..index].join("/");
            let relative_depth = index.saturating_sub(root_depth);
            if relative_depth <= max_depth
                && (root.is_empty() || path == root || path.starts_with(&format!("{root}/")))
            {
                entries.entry(path.clone()).or_insert(FileEntry {
                    path,
                    kind: FileEntryKind::Directory,
                    language: None,
                    size_bytes: None,
                    score: None,
                });
            }
        }
        let file_depth = parts.len().saturating_sub(root_depth);
        if file_depth <= max_depth {
            entries.insert(
                file.path.clone(),
                FileEntry {
                    path: file.path.clone(),
                    kind: FileEntryKind::File,
                    language: file.language.clone(),
                    size_bytes: Some(file.size_bytes),
                    score: None,
                },
            );
        }
    }
    Ok(entries.into_values().collect())
}

fn fuzzy_entries(files: &[FileRecord], query: Option<&str>) -> Result<Vec<FileEntry>> {
    let query = query
        .filter(|value| !value.is_empty())
        .ok_or_else(|| Error::InvalidRequest("find requires query".into()))?;
    let paths = files
        .iter()
        .map(|file| file.path.as_str())
        .collect::<Vec<_>>();
    let pattern = Pattern::new(
        query,
        CaseMatching::Ignore,
        Normalization::Smart,
        AtomKind::Fuzzy,
    );
    let mut matcher = Matcher::new(MatcherConfig::DEFAULT.match_paths());
    let matches = pattern.match_list(paths, &mut matcher);
    let by_path = files
        .iter()
        .map(|file| (file.path.as_str(), file))
        .collect::<HashMap<_, _>>();
    Ok(matches
        .into_iter()
        .filter_map(|(path, score)| {
            by_path.get(path).map(|file| FileEntry {
                path: path.to_string(),
                kind: FileEntryKind::File,
                language: file.language.clone(),
                size_bytes: Some(file.size_bytes),
                score: Some(f64::from(score)),
            })
        })
        .collect())
}

fn glob_entries(files: &[FileRecord], pattern: Option<&str>) -> Result<Vec<FileEntry>> {
    let pattern = pattern
        .filter(|value| !value.is_empty())
        .ok_or_else(|| Error::InvalidRequest("glob requires pattern".into()))?;
    let matcher = Glob::new(pattern)?.compile_matcher();
    Ok(files
        .iter()
        .filter(|file| matcher.is_match(&file.path))
        .map(|file| FileEntry {
            path: file.path.clone(),
            kind: FileEntryKind::File,
            language: file.language.clone(),
            size_bytes: Some(file.size_bytes),
            score: None,
        })
        .collect())
}

fn chunk_search_hit(
    hit: ChunkHit,
    query: &str,
    case_sensitive: bool,
    context: usize,
    is_regex: bool,
) -> Result<Option<SearchHit>> {
    let byte_range = if is_regex {
        regex::RegexBuilder::new(query)
            .case_insensitive(!case_sensitive)
            .build()?
            .find(&hit.content)
            .map(|matched| (matched.start(), matched.end()))
    } else if case_sensitive {
        hit.content
            .find(query)
            .map(|start| (start, start + query.len()))
    } else {
        regex::RegexBuilder::new(&regex::escape(query))
            .case_insensitive(true)
            .build()?
            .find(&hit.content)
            .map(|matched| (matched.start(), matched.end()))
    };
    let Some((start, end)) = byte_range else {
        return Ok(None);
    };
    let (local_start, local_end) = byte_range_to_line_range(&hit.content, start, end);
    let excerpt_start = local_start.saturating_sub(context).max(1);
    let available_lines = hit.content.lines().count().max(1);
    let mut excerpt_end = local_end.saturating_add(context).min(available_lines);
    if excerpt_end.saturating_sub(excerpt_start).saturating_add(1) > 20 {
        excerpt_end = excerpt_start + 19;
    }
    let excerpt = excerpt(&hit.content, excerpt_start, excerpt_end);
    Ok(Some(SearchHit {
        path: hit.path,
        start_line: hit.start_line + excerpt_start - 1,
        end_line: hit.start_line + excerpt_end - 1,
        content_hash: hash(&excerpt),
        excerpt,
        match_kind: if is_regex {
            "regex".into()
        } else {
            "text".into()
        },
        role: None,
        symbol: None,
        enclosing_symbol: None,
        score: 3.0 + (-hit.score).max(0.0) * 1_000_000.0,
        score_reasons: vec![if is_regex {
            "regex match".into()
        } else {
            "text match".into()
        }],
    }))
}

fn matching_line(hit: &ChunkHit, query: &str, case_sensitive: bool) -> Option<usize> {
    hit.content
        .lines()
        .position(|line| {
            if case_sensitive {
                line.contains(query)
            } else {
                line.to_lowercase().contains(&query.to_lowercase())
            }
        })
        .map(|offset| hit.start_line + offset)
}

fn path_allowed(path: &str, includes: &[String], excludes: &[String]) -> Result<bool> {
    let mut included = includes.is_empty();
    for pattern in includes {
        included |= path_matches(path, pattern)?;
    }
    let mut excluded = false;
    for pattern in excludes {
        excluded |= path_matches(path, pattern)?;
    }
    Ok(included && !excluded)
}

fn path_matches(path: &str, pattern: &str) -> Result<bool> {
    if pattern.contains(['*', '?', '[', ']']) {
        Ok(Glob::new(pattern)?.compile_matcher().is_match(path))
    } else {
        let pattern = pattern.trim_matches('/');
        Ok(path == pattern || path.starts_with(&format!("{pattern}/")))
    }
}

fn apply_focus(hits: &mut [SearchHit], focus_paths: &[String]) -> Result<()> {
    for hit in hits {
        let mut focused = false;
        for focus in focus_paths {
            focused |= path_matches(&hit.path, focus)?;
        }
        if focused {
            hit.score += 2.0;
            hit.score_reasons.push("focus path".into());
        }
    }
    Ok(())
}

fn context_path_score(path: &str, terms: &[String], task: &str) -> f64 {
    let path = path.to_lowercase();
    let mut score = terms
        .iter()
        .filter(|term| path.contains(term.to_ascii_lowercase().as_str()))
        .count() as f64;
    for code_token in code_tokens(task).into_iter().filter(|token| {
        token.contains("::")
            || token
                .split('.')
                .any(|part| part.chars().next().is_some_and(char::is_uppercase))
    }) {
        let matched_parts = expand_terms(&code_token)
            .into_iter()
            .map(|part| part.to_ascii_lowercase())
            .filter(|part| part.chars().count() >= 2 && path.contains(part))
            .collect::<HashSet<_>>()
            .len();
        if matched_parts >= 2 {
            #[allow(clippy::cast_precision_loss)]
            {
                score += (matched_parts * matched_parts) as f64;
            }
        }
    }
    for (language, component) in [
        ("javascript", "/js/"),
        ("typescript", "/ts/"),
        ("python", "/python/"),
        ("rust", "/rust/"),
        ("go", "/go/"),
    ] {
        if task_mentions_language(task, language) && format!("/{path}/").contains(component) {
            // An explicit language name in the task is strong repository-scope
            // evidence. Keep this above an exact-name match in another
            // language so common names such as `Point` do not dominate.
            score += 12.0;
        }
    }
    score
}

fn qualified_symbol_match(
    concept: &str,
    name: &str,
    parent: Option<&str>,
    signature: Option<&str>,
) -> f64 {
    if !concept.contains(['.', ':']) {
        return 0.0;
    }
    let parts = concept
        .split(|character: char| !character.is_alphanumeric() && character != '_')
        .flat_map(identifier_words)
        .map(|part| part.to_ascii_lowercase())
        .filter(|part| part.chars().count() >= 2)
        .collect::<HashSet<_>>();
    if parts.len() < 2 {
        return 0.0;
    }
    let haystack = format!(
        "{} {} {}",
        name,
        parent.unwrap_or_default(),
        signature.unwrap_or_default()
    )
    .to_ascii_lowercase();
    f64::from(parts.iter().all(|part| haystack.contains(part)))
}

#[derive(Debug, Clone, PartialEq)]
struct ContextQuery {
    value: String,
    weight: f64,
    fusion_key: Option<String>,
}

fn context_queries(task: &str, limit: usize) -> Vec<ContextQuery> {
    if limit == 0 {
        return Vec::new();
    }
    let wants_tests = task_terms(task).iter().any(|term| is_test_term(term));
    let available = limit.saturating_sub(usize::from(wants_tests));
    let code_terms = code_tokens(task);
    let code_parts = code_terms
        .iter()
        .flat_map(|term| std::iter::once(term.clone()).chain(expand_terms(term)))
        .map(|term| term.to_ascii_lowercase())
        .collect::<HashSet<_>>();
    let mut prose = task_terms(task)
        .into_iter()
        .filter(|value| {
            !is_test_term(value)
                && !is_context_stop_word(value)
                && !code_parts.contains(&value.to_ascii_lowercase())
        })
        .enumerate()
        .collect::<Vec<_>>();
    prose.sort_by(|(left_index, left), (right_index, right)| {
        context_query_weight(right, false)
            .total_cmp(&context_query_weight(left, false))
            .then_with(|| left_index.cmp(right_index))
    });

    let prose_reserve = prose.len().min(4).min(available);
    let exact_limit = available.saturating_sub(prose_reserve);
    let mut seen = HashSet::new();
    let mut terms = Vec::new();
    for code_term in code_terms.iter().take(exact_limit) {
        push_context_query(
            &mut terms,
            &mut seen,
            code_term.clone(),
            true,
            Some(code_term.to_ascii_lowercase()),
        );
    }
    for (_, value) in prose.iter().take(prose_reserve) {
        push_context_query(&mut terms, &mut seen, value.clone(), false, None);
    }

    let mut expansion_round = 0usize;
    while terms.len() < available {
        let before = terms.len();
        for code_term in &code_terms {
            let expansions = expand_terms(code_term);
            if let Some(value) = expansions.get(expansion_round) {
                push_context_query(
                    &mut terms,
                    &mut seen,
                    value.clone(),
                    true,
                    Some(code_term.to_ascii_lowercase()),
                );
                if terms.len() == available {
                    break;
                }
            }
        }
        if terms.len() == before {
            break;
        }
        expansion_round += 1;
    }
    if wants_tests {
        terms.push(ContextQuery {
            value: "test".into(),
            weight: 0.2,
            fusion_key: None,
        });
    }
    terms
}

fn push_context_query(
    terms: &mut Vec<ContextQuery>,
    seen: &mut HashSet<String>,
    value: String,
    explicit_code_token: bool,
    fusion_key: Option<String>,
) {
    if value.chars().count() < 2
        || is_context_stop_word(&value)
        || !seen.insert(value.to_ascii_lowercase())
    {
        return;
    }
    terms.push(ContextQuery {
        weight: context_query_weight(&value, explicit_code_token),
        value,
        fusion_key,
    });
}

fn task_terms(task: &str) -> Vec<String> {
    task.split(|character: char| !character.is_alphanumeric() && character != '_')
        .filter(|value| value.chars().count() >= 2)
        .map(str::to_owned)
        .collect()
}

fn is_test_term(term: &str) -> bool {
    matches!(
        term.to_ascii_lowercase().as_str(),
        "test" | "tests" | "testing" | "coverage" | "regression"
    )
}

fn context_query_weight(term: &str, explicit_code_token: bool) -> f64 {
    if explicit_code_token {
        return if term.contains(['_', ':', '.', '-']) {
            1.0
        } else {
            0.95
        };
    }
    if term.contains(['_', ':', '.', '-']) {
        return 0.9;
    }
    match term.chars().count() {
        10.. => 0.8,
        7..=9 => 0.65,
        4..=6 => 0.45,
        _ => 0.25,
    }
}

fn record_query_hit(
    fusion: &mut HashMap<String, HashMap<String, f64>>,
    path: &str,
    fusion_key: &str,
    weight: f64,
    rank: usize,
) {
    if weight < 0.65 {
        return;
    }
    const RRF_K: f64 = 60.0;
    #[allow(clippy::cast_precision_loss)]
    let score = weight * RRF_K / (RRF_K + rank as f64 + 1.0);
    fusion
        .entry(path.to_owned())
        .or_default()
        .entry(fusion_key.to_owned())
        .and_modify(|current| *current = current.max(score))
        .or_insert(score);
}

fn apply_query_fusion(
    candidates: &mut [Candidate],
    fusion: &HashMap<String, HashMap<String, f64>>,
) {
    for candidate in candidates {
        let Some(matches) = fusion.get(&candidate.path) else {
            continue;
        };
        if matches.len() > 1 {
            let total = matches.values().sum::<f64>();
            let strongest = matches.values().copied().fold(0.0, f64::max);
            candidate.path_score += (total - strongest).min(0.2);
            if !candidate
                .match_kinds
                .iter()
                .any(|kind| kind == "multi-query")
            {
                candidate.match_kinds.push("multi-query".into());
            }
        }
    }
}

fn code_tokens(task: &str) -> Vec<String> {
    task.split_whitespace()
        .map(|token| {
            token.trim_matches(|character: char| !character.is_alphanumeric() && character != '_')
        })
        .filter(|token| {
            token.contains('_')
                || token.contains("::")
                || token.contains('.')
                || (token.contains('-') && token.chars().any(char::is_uppercase))
        })
        .map(str::to_owned)
        .collect()
}

fn task_mentions_language(task: &str, language: &str) -> bool {
    task.split(|character: char| !character.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .any(|word| {
            if language == "go" {
                word == "Go" || word.eq_ignore_ascii_case("golang")
            } else {
                word.eq_ignore_ascii_case(language)
            }
        })
}

fn is_context_stop_word(term: &str) -> bool {
    matches!(
        term.to_ascii_lowercase().as_str(),
        "a" | "an"
            | "and"
            | "add"
            | "adding"
            | "are"
            | "as"
            | "be"
            | "before"
            | "both"
            | "but"
            | "by"
            | "calling"
            | "can"
            | "change"
            | "does"
            | "each"
            | "fix"
            | "for"
            | "from"
            | "if"
            | "in"
            | "into"
            | "is"
            | "it"
            | "its"
            | "make"
            | "not"
            | "of"
            | "on"
            | "one"
            | "only"
            | "or"
            | "same"
            | "so"
            | "than"
            | "then"
            | "the"
            | "this"
            | "to"
            | "update"
            | "when"
            | "while"
            | "within"
            | "without"
            | "with"
    )
}

fn fts_quote(value: &str) -> String {
    format!("\"{}\"", value.replace('"', "\"\""))
}

fn validate_patterns(patterns: &[String]) -> Result<()> {
    if patterns.len() > MAX_INPUT_ITEMS {
        return Err(Error::LimitExceeded);
    }
    for pattern in patterns {
        validate_input(pattern, "path pattern", MAX_PATTERN_BYTES)?;
    }
    Ok(())
}

fn validate_optional_input(value: Option<&str>, name: &str, max_bytes: usize) -> Result<()> {
    if let Some(value) = value {
        validate_input(value, name, max_bytes)?;
    }
    Ok(())
}

fn validate_input(value: &str, name: &str, max_bytes: usize) -> Result<()> {
    if value.len() > max_bytes {
        return Err(Error::InvalidRequest(format!(
            "{name} exceeds {max_bytes} bytes"
        )));
    }
    Ok(())
}

fn parse_cursor(cursor: Option<&str>, generation: u64) -> Result<usize> {
    let Some(cursor) = cursor else { return Ok(0) };
    let Some((cursor_generation, offset)) = cursor.split_once(':') else {
        return Err(Error::StaleCursor);
    };
    if cursor_generation.parse::<u64>().ok() != Some(generation) {
        return Err(Error::StaleCursor);
    }
    offset.parse().map_err(|_| Error::StaleCursor)
}

fn make_cursor(generation: u64, offset: usize) -> String {
    format!("{generation}:{offset}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn language_scope_does_not_treat_lowercase_go_as_golang() {
        assert!(!task_mentions_language("go fix the parser", "go"));
        assert!(task_mentions_language("fix the Go parser", "go"));
        assert!(task_mentions_language("fix the golang parser", "go"));
        assert!(task_mentions_language(
            "fix TypeScript parsing",
            "typescript"
        ));
    }

    #[test]
    fn context_queries_keep_identifiers_and_late_test_signals() {
        let terms = context_queries(
            "copy_current_request_context reuses one copied request context so calling the decorated function concurrently can corrupt state; add a regression test",
            12,
        );

        assert!(
            terms
                .iter()
                .any(|term| term.value == "copy_current_request_context")
        );
        assert!(terms.iter().any(|term| term.value == "test"));
        assert!(!terms.iter().any(|term| term.value == "one"));
    }

    #[test]
    fn context_queries_preserve_dotted_and_header_tokens() {
        let terms = context_queries(
            "Fix res.send adding Content-Length when Transfer-Encoding is present and add coverage",
            12,
        );

        assert!(terms.iter().any(|term| term.value == "res.send"));
        assert!(terms.iter().any(|term| term.value == "Content-Length"));
        assert!(terms.iter().any(|term| term.value == "Transfer-Encoding"));
        assert_eq!(terms.last().map(|term| term.value.as_str()), Some("test"));
    }

    #[test]
    fn context_queries_reserve_space_for_task_intent() {
        let terms = context_queries(
            "Fix Alpha::first_long_identifier Beta::second_long_identifier while preserving idempotency",
            12,
        );

        assert!(
            terms
                .iter()
                .any(|term| term.value == "Alpha::first_long_identifier")
        );
        assert!(
            terms
                .iter()
                .any(|term| term.value == "Beta::second_long_identifier")
        );
        assert!(terms.iter().any(|term| term.value == "idempotency"));
    }

    #[test]
    fn context_query_expansions_share_one_fusion_concept() {
        let terms = context_queries(
            "Fix GlobSet::matches_all when one compiled strategy matches",
            12,
        );
        let qualified = terms
            .iter()
            .find(|term| term.value == "GlobSet::matches_all")
            .expect("qualified query");
        let expansion = terms
            .iter()
            .find(|term| term.value != qualified.value && term.fusion_key == qualified.fusion_key)
            .expect("expanded query");

        assert_eq!(qualified.fusion_key, expansion.fusion_key);
        assert!(qualified.fusion_key.is_some());
    }

    #[test]
    fn qualified_symbol_match_requires_all_owner_and_name_parts() {
        assert_eq!(
            qualified_symbol_match(
                "render.AsciiJSON",
                "Render",
                None,
                Some("func (r AsciiJSON) Render() error"),
            ),
            1.0
        );
        assert_eq!(
            qualified_symbol_match(
                "render.AsciiJSON",
                "AsciiJSON",
                None,
                Some("type AsciiJSON")
            ),
            0.0
        );
        assert_eq!(
            qualified_symbol_match("Flask.run", "run", Some("Flask"), Some("def run()")),
            1.0
        );
    }

    #[test]
    fn qualified_path_evidence_excludes_dynamic_lowercase_receivers() {
        assert_eq!(
            context_path_score(
                "test/app.render.js",
                &[],
                "Fix app.render for a trailing dot",
            ),
            0.0
        );
        assert!(context_path_score("render/json.go", &[], "Fix render.AsciiJSON escaping",) > 0.0);
        assert!(
            context_path_score(
                "tokio/src/fs/file.rs",
                &[],
                "Fix tokio::fs::File poll_write",
            ) > 0.0
        );
    }

    #[test]
    fn fusion_requires_two_independent_query_concepts() {
        let mut fusion = HashMap::new();
        record_query_hit(&mut fusion, "one.rs", "globset::matches_all", 1.0, 0);
        record_query_hit(&mut fusion, "one.rs", "globset::matches_all", 0.95, 1);
        record_query_hit(&mut fusion, "two.rs", "content-length", 1.0, 0);
        record_query_hit(&mut fusion, "two.rs", "transfer-encoding", 1.0, 1);
        let mut candidates = vec![
            Candidate::new("one.rs", 1, 1, "one"),
            Candidate::new("two.rs", 1, 1, "two"),
        ];

        apply_query_fusion(&mut candidates, &fusion);

        assert_eq!(candidates[0].path_score, 0.0);
        assert!(
            !candidates[0]
                .match_kinds
                .iter()
                .any(|kind| kind == "multi-query")
        );
        assert!(candidates[1].path_score > 0.0);
        assert!(
            candidates[1]
                .match_kinds
                .iter()
                .any(|kind| kind == "multi-query")
        );
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
        let large = services
            .storage
            .find_symbol(file.id, "large")
            .expect("find symbol")
            .expect("large symbol");
        let matched_line = 151;
        let enclosing = services
            .storage
            .find_enclosing_symbol(file.id, matched_line)
            .expect("find enclosing symbol")
            .expect("enclosing symbol");
        assert_eq!(enclosing.name, "large");

        let bounded = services
            .adaptive_context_excerpt(file.id, large.start_line, large.end_line, matched_line, 60)
            .expect("bounded excerpt")
            .expect("bounded declaration");
        assert!(bounded.start_line <= matched_line);
        assert!(bounded.end_line >= matched_line);
        assert!(bounded.start_line > large.start_line);
        assert!(bounded.end_line <= large.end_line);

        let small = services
            .storage
            .find_symbol(file.id, "small")
            .expect("find symbol")
            .expect("small symbol");
        let complete = services
            .adaptive_context_excerpt(
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

        let response = services
            .search(SearchRequest {
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
            })
            .await
            .expect("search");

        assert!(response.hits.is_empty());
        assert!(
            response.meta.next_cursor.is_none(),
            "all candidates were examined, so no next page remains"
        );
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
    fn repeated_generation_changes_are_a_retryable_conflict() {
        let root = tempfile::tempdir().expect("root");
        let config =
            Config::discover(root.path(), Some(root.path().join("db.sqlite"))).expect("config");
        let services = Services::open(config).expect("services");
        services
            .storage
            .full_reconcile("hash", Vec::new())
            .expect("initial generation");

        let error = services
            .consistent(|_| {
                services.storage.full_reconcile("hash", Vec::new())?;
                Ok(())
            })
            .expect_err("generation must not stabilize");

        assert!(matches!(
            error,
            Error::RetryableConflict(RetryableOperation::Retrieval)
        ));
    }
}
