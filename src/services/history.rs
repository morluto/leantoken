use std::{
    borrow::Cow,
    collections::{BTreeMap, BTreeSet},
};

use similar::TextDiff;
use tokio_util::sync::CancellationToken;

use super::change_receipt::{
    HistoricalSymbolChangeEndpoint, classify_historical_symbol_added,
    classify_historical_symbol_change, classify_historical_symbol_removed,
    classify_historical_symbol_rename,
};
use super::validation::{MAX_PATH_BYTES, MAX_PATTERN_BYTES, check_cancelled, validate_input};
use super::{ServiceCallOptions, Services, validate_positive_request_limit};
use crate::model::{
    DiffSymbolsDiagnostics, DiffSymbolsIncompleteReason, DiffSymbolsRequest, DiffSymbolsResponse,
    DiffSymbolsResult, DiffSymbolsStatus, HistoricalSymbol, HistoryOperation, HistoryRequest,
    HistoryResponse, HistoryRevisionMetadata, Symbol, SymbolHistoryCommit,
    TokenAccountingOperation,
};
use crate::repository::{
    GitBlob, GitBlobBatch, GitCommitMetadata, git_blob_at_revision, git_blobs_at_resolved_revision,
    git_commit_metadata, git_diff_identity, git_line_history, normalize_relative,
};
use crate::tokens::ResponseBudget;
use crate::{Error, Result, parser};

#[cfg(test)]
const MAX_HISTORY_RESULTS: usize = 100;
pub(crate) const MAX_DIFF_SYMBOL_TARGETS: usize = 64;
pub(crate) const MAX_DIFF_SYMBOL_RESULTS: usize = 32;
const MAX_DIFF_SYMBOL_PATHS_PER_ENDPOINT: usize = 32;
const MAX_DIFF_SYMBOL_FILE_BYTES: usize = 1024 * 1024;
const MAX_DIFF_SYMBOL_TOTAL_BYTES: usize = 8 * 1024 * 1024;
const MAX_DIFF_SYMBOL_PARSED_SYMBOLS_PER_ENDPOINT: usize = 1_024;
const MAX_DIFF_SYMBOL_BYTES: usize = 1024 * 1024;
const MAX_DIFF_SYMBOL_CURSOR_BYTES: usize = 128;

struct ResolvedHistoricalSymbol {
    symbol: HistoricalSymbol,
    signature: Option<String>,
    content: String,
}

struct ParsedHistoricalFile {
    content: String,
    symbols: Vec<Symbol>,
}

struct ParsedHistoricalBatch {
    revision: String,
    files: BTreeMap<String, ParsedHistoricalFile>,
    unavailable: BTreeMap<String, String>,
    blob_bytes: usize,
    parsed_symbols: usize,
}

fn char_boundaries(value: &str) -> Vec<usize> {
    let mut boundaries = value
        .char_indices()
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    boundaries.push(value.len());
    boundaries
}

fn symbol_diff_buffer(content: &str) -> Cow<'_, str> {
    if content.is_empty() || content.ends_with('\n') {
        Cow::Borrowed(content)
    } else {
        Cow::Owned(format!("{content}\n"))
    }
}

#[cfg(test)]
fn validate_history_request(request: &HistoryRequest) -> Result<()> {
    validate_history_request_with_limit(request, MAX_HISTORY_RESULTS)
}

fn validate_history_request_with_limit(
    request: &HistoryRequest,
    max_results_limit: usize,
) -> Result<()> {
    fn validate_non_empty(value: &str, field: &'static str) -> Result<()> {
        if value.trim().is_empty() {
            return Err(Error::InvalidInput {
                field,
                reason: "must not be empty",
            });
        }
        Ok(())
    }

    match &request.operation {
        HistoryOperation::ReadSymbol {
            path,
            symbol,
            revision,
        }
        | HistoryOperation::SymbolLog {
            path,
            symbol,
            revision: Some(revision),
        } => {
            validate_input(path, "path", MAX_PATH_BYTES)?;
            validate_input(symbol, "symbol", MAX_PATTERN_BYTES)?;
            validate_non_empty(symbol, "symbol")?;
            validate_input(revision, "revision", MAX_PATTERN_BYTES)?;
            validate_non_empty(revision, "revision")?;
        }
        HistoryOperation::SymbolLog {
            path,
            symbol,
            revision: None,
        } => {
            validate_input(path, "path", MAX_PATH_BYTES)?;
            validate_input(symbol, "symbol", MAX_PATTERN_BYTES)?;
        }
        HistoryOperation::DiffSymbol {
            path,
            symbol,
            base_revision,
            head_revision,
        } => {
            validate_input(path, "path", MAX_PATH_BYTES)?;
            validate_input(symbol, "symbol", MAX_PATTERN_BYTES)?;
            validate_non_empty(symbol, "symbol")?;
            validate_input(base_revision, "base revision", MAX_PATTERN_BYTES)?;
            validate_non_empty(base_revision, "base revision")?;
            validate_input(head_revision, "head revision", MAX_PATTERN_BYTES)?;
            validate_non_empty(head_revision, "head revision")?;
        }
    }
    if let Some(max_results) = request.max_results {
        validate_positive_request_limit("max_results", max_results, max_results_limit)?;
    }
    Ok(())
}

fn normalize_operation(operation: HistoryOperation) -> Result<HistoryOperation> {
    fn normalize_non_empty(value: String, field: &'static str) -> Result<String> {
        let value = value.trim().to_owned();
        if value.is_empty() {
            return Err(Error::InvalidInput {
                field,
                reason: "must not be empty",
            });
        }
        Ok(value)
    }

    Ok(match operation {
        HistoryOperation::ReadSymbol {
            path,
            symbol,
            revision,
        } => HistoryOperation::ReadSymbol {
            path: normalize_relative(&path)?,
            symbol: normalize_non_empty(symbol, "symbol")?,
            revision: normalize_non_empty(revision, "revision")?,
        },
        HistoryOperation::DiffSymbol {
            path,
            symbol,
            base_revision,
            head_revision,
        } => HistoryOperation::DiffSymbol {
            path: normalize_relative(&path)?,
            symbol: normalize_non_empty(symbol, "symbol")?,
            base_revision: normalize_non_empty(base_revision, "base revision")?,
            head_revision: normalize_non_empty(head_revision, "head revision")?,
        },
        HistoryOperation::SymbolLog {
            path,
            symbol,
            revision,
        } => HistoryOperation::SymbolLog {
            path: normalize_relative(&path)?,
            symbol: normalize_non_empty(symbol, "symbol")?,
            revision: revision
                .map(|revision| normalize_non_empty(revision, "revision"))
                .transpose()?,
        },
    })
}

fn normalize_history_request(mut request: HistoryRequest) -> Result<HistoryRequest> {
    request.operation = normalize_operation(request.operation)?;
    Ok(request)
}

#[cfg(test)]
fn validate_diff_symbols_request(request: &DiffSymbolsRequest) -> Result<()> {
    validate_diff_symbols_request_with_limit(request, MAX_HISTORY_RESULTS)
}

fn validate_diff_symbols_request_with_limit(
    request: &DiffSymbolsRequest,
    max_results_limit: usize,
) -> Result<()> {
    fn validate_non_empty(value: &str, field: &'static str) -> Result<()> {
        if value.trim().is_empty() {
            return Err(Error::InvalidInput {
                field,
                reason: "must not be empty",
            });
        }
        Ok(())
    }

    validate_positive_request_limit("targets", request.targets.len(), MAX_DIFF_SYMBOL_TARGETS)?;
    validate_input(&request.base_revision, "base revision", MAX_PATTERN_BYTES)?;
    validate_non_empty(&request.base_revision, "base revision")?;
    validate_input(&request.head_revision, "head revision", MAX_PATTERN_BYTES)?;
    validate_non_empty(&request.head_revision, "head revision")?;
    if let Some(max_results) = request.max_results {
        validate_positive_request_limit(
            "max_results",
            max_results,
            max_results_limit.min(MAX_DIFF_SYMBOL_RESULTS),
        )?;
    }
    if request
        .cursor
        .as_ref()
        .is_some_and(|cursor| cursor.is_empty() || cursor.len() > MAX_DIFF_SYMBOL_CURSOR_BYTES)
    {
        return Err(Error::StaleCursor);
    }
    let mut base_paths = BTreeSet::new();
    let mut head_paths = BTreeSet::new();
    for target in &request.targets {
        validate_input(&target.path, "path", MAX_PATH_BYTES)?;
        validate_input(&target.symbol, "symbol", MAX_PATTERN_BYTES)?;
        validate_non_empty(&target.symbol, "symbol")?;
        match (&target.head_path, &target.head_symbol) {
            (Some(path), Some(symbol)) => {
                validate_input(path, "head path", MAX_PATH_BYTES)?;
                validate_input(symbol, "head symbol", MAX_PATTERN_BYTES)?;
                validate_non_empty(symbol, "head symbol")?;
                head_paths.insert(path.as_str());
            }
            (None, None) => {
                head_paths.insert(target.path.as_str());
            }
            _ => {
                return Err(Error::InvalidInput {
                    field: "targets",
                    reason: "head_path and head_symbol must be supplied together",
                });
            }
        }
        base_paths.insert(target.path.as_str());
    }
    for (field, requested) in [
        ("base paths", base_paths.len()),
        ("head paths", head_paths.len()),
    ] {
        if requested > MAX_DIFF_SYMBOL_PATHS_PER_ENDPOINT {
            return Err(Error::RequestLimitExceeded {
                field,
                requested,
                limit: MAX_DIFF_SYMBOL_PATHS_PER_ENDPOINT,
            });
        }
    }
    Ok(())
}

fn normalize_diff_symbols_request(mut request: DiffSymbolsRequest) -> Result<DiffSymbolsRequest> {
    fn normalize_non_empty(value: String, field: &'static str) -> Result<String> {
        let value = value.trim().to_owned();
        if value.is_empty() {
            return Err(Error::InvalidInput {
                field,
                reason: "must not be empty",
            });
        }
        Ok(value)
    }

    request.base_revision = normalize_non_empty(request.base_revision, "base revision")?;
    request.head_revision = normalize_non_empty(request.head_revision, "head revision")?;
    let mut seen = BTreeSet::new();
    for target in &mut request.targets {
        target.path = normalize_relative(&target.path)?;
        target.symbol = normalize_non_empty(target.symbol.clone(), "symbol")?;
        if let Some(path) = &mut target.head_path {
            *path = normalize_relative(path)?;
        }
        if let Some(symbol) = &mut target.head_symbol {
            *symbol = normalize_non_empty(symbol.clone(), "head symbol")?;
        }
        let key = (
            target.path.clone(),
            target.symbol.clone(),
            target.head_path.clone(),
            target.head_symbol.clone(),
        );
        if !seen.insert(key) {
            return Err(Error::InvalidInput {
                field: "targets",
                reason: "must not contain duplicate symbol pairings",
            });
        }
    }
    Ok(request)
}

fn diff_symbols_query_hash(
    request: &DiffSymbolsRequest,
    base_revision: &str,
    head_revision: &str,
) -> String {
    fn update(hasher: &mut blake3::Hasher, value: &str) {
        hasher.update(&(value.len() as u64).to_le_bytes());
        hasher.update(value.as_bytes());
    }

    let mut hasher = blake3::Hasher::new();
    update(&mut hasher, base_revision);
    update(&mut hasher, head_revision);
    hasher.update(&(request.targets.len() as u64).to_le_bytes());
    for target in &request.targets {
        update(&mut hasher, &target.path);
        update(&mut hasher, &target.symbol);
        for value in [&target.head_path, &target.head_symbol] {
            match value {
                Some(value) => {
                    hasher.update(&[1]);
                    update(&mut hasher, value);
                }
                None => {
                    hasher.update(&[0]);
                }
            }
        }
    }
    hasher.finalize().to_hex()[..16].to_string()
}

fn make_diff_symbols_cursor(
    request: &DiffSymbolsRequest,
    base_revision: &str,
    head_revision: &str,
    offset: usize,
) -> String {
    format!(
        "history-multi:{offset}:{base_revision}:{head_revision}:{}",
        diff_symbols_query_hash(request, base_revision, head_revision)
    )
}

fn parse_diff_symbols_cursor(
    request: &DiffSymbolsRequest,
    base_revision: &str,
    head_revision: &str,
) -> Result<usize> {
    let Some(cursor) = request.cursor.as_deref() else {
        return Ok(0);
    };
    let fields = cursor.split(':').collect::<Vec<_>>();
    let [kind, offset, cursor_base, cursor_head, query_hash] = fields.as_slice() else {
        return Err(Error::StaleCursor);
    };
    if *kind != "history-multi"
        || *cursor_base != base_revision
        || *cursor_head != head_revision
        || query_hash.len() != 16
        || !query_hash.bytes().all(|byte| byte.is_ascii_hexdigit())
        || *query_hash != diff_symbols_query_hash(request, base_revision, head_revision)
    {
        return Err(Error::StaleCursor);
    }
    let offset = offset.parse::<usize>().map_err(|_| Error::StaleCursor)?;
    if offset >= request.targets.len() {
        return Err(Error::StaleCursor);
    }
    Ok(offset)
}

fn history_revision_metadata(metadata: GitCommitMetadata) -> HistoryRevisionMetadata {
    HistoryRevisionMetadata {
        revision: metadata.revision,
        authored_at: metadata.authored_at,
        subject: metadata.subject,
    }
}

fn append_batch_unavailable(
    unavailable: &mut BTreeMap<String, String>,
    paths: &[String],
    reason: &str,
) {
    for path in paths {
        unavailable
            .entry(path.clone())
            .or_insert_with(|| reason.to_owned());
    }
}

fn parse_historical_batch(
    batch: GitBlobBatch,
    side: &str,
    cancellation: &CancellationToken,
) -> Result<ParsedHistoricalBatch> {
    let mut unavailable = BTreeMap::new();
    append_batch_unavailable(
        &mut unavailable,
        &batch.oversized_paths,
        &format!("{side}_file_exceeds_byte_limit"),
    );
    append_batch_unavailable(
        &mut unavailable,
        &batch.total_limit_paths,
        &format!("{side}_total_blob_bytes_limit"),
    );
    append_batch_unavailable(
        &mut unavailable,
        &batch.invalid_utf8_paths,
        &format!("{side}_file_not_utf8"),
    );
    append_batch_unavailable(
        &mut unavailable,
        &batch.unsupported_paths,
        &format!("{side}_git_entry_unsupported"),
    );
    let blob_bytes = batch
        .blobs
        .values()
        .map(String::len)
        .fold(0usize, usize::saturating_add);
    let mut files = BTreeMap::new();
    let mut parsed_symbols = 0usize;
    for (path, content) in batch.blobs {
        check_cancelled(cancellation)?;
        let parsed = match parser::parse_with_cancellation(&path, &content, cancellation) {
            Ok(parsed) => parsed,
            Err(Error::Cancelled) => return Err(Error::Cancelled),
            Err(_) => {
                unavailable.insert(path, format!("{side}_parse_failed"));
                continue;
            }
        };
        if parsed_symbols.saturating_add(parsed.symbols.len())
            > MAX_DIFF_SYMBOL_PARSED_SYMBOLS_PER_ENDPOINT
        {
            unavailable.insert(path, format!("{side}_parsed_symbol_limit"));
            continue;
        }
        parsed_symbols = parsed_symbols.saturating_add(parsed.symbols.len());
        files.insert(
            path,
            ParsedHistoricalFile {
                content,
                symbols: parsed.symbols,
            },
        );
    }
    Ok(ParsedHistoricalBatch {
        revision: batch.revision,
        files,
        unavailable,
        blob_bytes,
        parsed_symbols,
    })
}

fn resolve_parsed_historical_symbol(
    batch: &ParsedHistoricalBatch,
    path: &str,
    symbol_name: &str,
) -> Result<Option<ResolvedHistoricalSymbol>> {
    let Some(file) = batch.files.get(path) else {
        return Ok(None);
    };
    let symbol =
        match crate::symbol_identity::resolve_symbol_matches(file.symbols.iter().filter(|symbol| {
            crate::symbol_identity::symbol_identity_matches(
                symbol_name,
                &symbol.name,
                symbol.parent.as_deref(),
            )
        })) {
            crate::symbol_identity::SymbolResolution::NotFound => return Ok(None),
            crate::symbol_identity::SymbolResolution::Unique(symbol) => symbol,
            crate::symbol_identity::SymbolResolution::Ambiguous => {
                return Err(Error::AmbiguousSymbol {
                    path: format!("{path}@{}", batch.revision),
                    symbol: symbol_name.to_owned(),
                });
            }
        };
    let content = file
        .content
        .get(symbol.start_byte..symbol.end_byte)
        .ok_or_else(|| Error::OperationFailure("invalid historical symbol range".into()))?
        .to_owned();
    Ok(Some(ResolvedHistoricalSymbol {
        symbol: HistoricalSymbol {
            revision: batch.revision.clone(),
            path: path.to_owned(),
            name: symbol.name.clone(),
            kind: symbol.kind.clone(),
            parent: symbol.parent.clone(),
            target_start_line: symbol.start_line,
            target_end_line: symbol.end_line,
            returned_end_line: symbol.end_line,
            truncated: false,
            content: None,
            content_hash: crate::text::hash(&content),
        },
        signature: symbol.signature.clone(),
        content,
    }))
}

fn historical_metadata(mut resolved: ResolvedHistoricalSymbol) -> HistoricalSymbol {
    resolved.symbol.content = None;
    resolved.symbol.returned_end_line = 0;
    resolved.symbol
}

fn utf8_prefix(value: &str, max_bytes: usize) -> &str {
    let mut end = value.len().min(max_bytes);
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    &value[..end]
}

fn refresh_diff_symbols_accounting(
    tokenizer: &crate::tokens::Tokenizer,
    response: &mut DiffSymbolsResponse,
) {
    let source_tokens = response
        .results
        .iter()
        .filter_map(|result| result.diff.as_deref())
        .map(|diff| tokenizer.count(diff))
        .fold(0usize, usize::saturating_add);
    response.meta.source_tokens = source_tokens;
    response.diagnostics.retained_diff_bytes = response
        .results
        .iter()
        .filter_map(|result| result.diff.as_ref())
        .map(String::len)
        .fold(0usize, usize::saturating_add);
}

impl Services {
    /// Retrieve symbol-aware evidence from immutable Git revisions.
    pub async fn history(&self, request: HistoryRequest) -> Result<HistoryResponse> {
        self.history_with_options(request, ServiceCallOptions::new())
            .await
    }

    /// Retrieve symbol-aware Git evidence under serialized-response controls.
    pub async fn history_with_options(
        &self,
        request: HistoryRequest,
        options: ServiceCallOptions,
    ) -> Result<HistoryResponse> {
        self.history_cancellable_with_options(request, options, CancellationToken::new())
            .await
    }

    /// Retrieve symbol-aware Git evidence with cooperative cancellation.
    pub async fn history_cancellable(
        &self,
        request: HistoryRequest,
        cancellation: CancellationToken,
    ) -> Result<HistoryResponse> {
        self.history_cancellable_with_options(request, ServiceCallOptions::new(), cancellation)
            .await
    }

    /// Retrieve symbol-aware Git evidence under response controls and cancellation.
    pub async fn history_cancellable_with_options(
        &self,
        request: HistoryRequest,
        options: ServiceCallOptions,
        cancellation: CancellationToken,
    ) -> Result<HistoryResponse> {
        let operation = TokenAccountingOperation::History;
        self.observe_service_result(operation, self.validate_call_options(options))?;
        self.observe_service_result(
            operation,
            validate_history_request_with_limit(&request, self.config.max_results),
        )?;
        let request = self.observe_service_result(operation, normalize_history_request(request))?;
        self.observe_service_result(
            operation,
            validate_history_request_with_limit(&request, self.config.max_results),
        )?;
        let this = self.clone();
        let result = self
            .blocking_executor
            .run(cancellation, move |cancellation| {
                this.history_sync(request, options, cancellation)
            })
            .await;
        self.observe_service_result(operation, result)
    }

    /// Diff an ordered, bounded set of parsed symbols across one immutable range.
    pub async fn history_diff_symbols(
        &self,
        request: DiffSymbolsRequest,
    ) -> Result<DiffSymbolsResponse> {
        self.history_diff_symbols_cancellable_with_options(
            request,
            ServiceCallOptions::default(),
            CancellationToken::new(),
        )
        .await
    }

    /// Diff multiple symbols with a hard final serialized-response boundary.
    pub async fn history_diff_symbols_with_options(
        &self,
        request: DiffSymbolsRequest,
        options: ServiceCallOptions,
    ) -> Result<DiffSymbolsResponse> {
        self.history_diff_symbols_cancellable_with_options(
            request,
            options,
            CancellationToken::new(),
        )
        .await
    }

    /// Cancellable bounded multi-symbol revision diff.
    pub async fn history_diff_symbols_cancellable(
        &self,
        request: DiffSymbolsRequest,
        cancellation: CancellationToken,
    ) -> Result<DiffSymbolsResponse> {
        self.history_diff_symbols_cancellable_with_options(
            request,
            ServiceCallOptions::default(),
            cancellation,
        )
        .await
    }

    /// Cancellable multi-symbol revision diff with final response options.
    pub async fn history_diff_symbols_cancellable_with_options(
        &self,
        request: DiffSymbolsRequest,
        options: ServiceCallOptions,
        cancellation: CancellationToken,
    ) -> Result<DiffSymbolsResponse> {
        let operation = TokenAccountingOperation::History;
        self.observe_service_result(operation, self.validate_call_options(options))?;
        self.observe_service_result(
            operation,
            validate_diff_symbols_request_with_limit(&request, self.config.max_results),
        )?;
        let request =
            self.observe_service_result(operation, normalize_diff_symbols_request(request))?;
        self.observe_service_result(
            operation,
            validate_diff_symbols_request_with_limit(&request, self.config.max_results),
        )?;
        let this = self.clone();
        let result = self
            .blocking_executor
            .run(cancellation, move |cancellation| {
                this.history_diff_symbols_sync(request, options, cancellation)
            })
            .await;
        self.observe_service_result(operation, result)
    }

    fn history_diff_symbols_sync(
        &self,
        request: DiffSymbolsRequest,
        options: ServiceCallOptions,
        cancellation: &CancellationToken,
    ) -> Result<DiffSymbolsResponse> {
        check_cancelled(cancellation)?;
        validate_diff_symbols_request_with_limit(&request, self.config.max_results)?;
        let request = normalize_diff_symbols_request(request)?;
        validate_diff_symbols_request_with_limit(&request, self.config.max_results)?;
        let max_results = request
            .max_results
            .unwrap_or(self.config.default_results)
            .min(MAX_DIFF_SYMBOL_RESULTS);
        let max_tokens = self.token_limit(request.max_tokens, self.config.default_read_tokens)?;
        let generation = self.consistent(|snapshot| Ok(snapshot.generation()))?;
        let revisions = git_diff_identity(
            &self.config.root,
            &request.base_revision,
            Some(&request.head_revision),
        )?;
        let page_start = parse_diff_symbols_cursor(
            &request,
            &revisions.base_revision,
            &revisions.head_revision,
        )?;
        let page_end = page_start
            .saturating_add(max_results)
            .min(request.targets.len());
        let page_targets = &request.targets[page_start..page_end];
        let base_paths = page_targets
            .iter()
            .map(|target| target.path.clone())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        let head_paths = page_targets
            .iter()
            .map(|target| {
                target
                    .head_path
                    .clone()
                    .unwrap_or_else(|| target.path.clone())
            })
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        check_cancelled(cancellation)?;
        let mut commit_metadata = git_commit_metadata(
            &self.config.root,
            &[
                revisions.base_revision.clone(),
                revisions.head_revision.clone(),
            ],
        )?;
        let base_metadata = commit_metadata
            .remove(&revisions.base_revision)
            .ok_or_else(|| Error::OperationFailure("missing base commit metadata".into()))?;
        let head_metadata = commit_metadata
            .remove(&revisions.head_revision)
            .or_else(|| {
                (revisions.base_revision == revisions.head_revision).then(|| base_metadata.clone())
            })
            .ok_or_else(|| Error::OperationFailure("missing head commit metadata".into()))?;
        check_cancelled(cancellation)?;
        let base_blobs = git_blobs_at_resolved_revision(
            &self.config.root,
            &revisions.base_revision,
            &base_paths,
            MAX_DIFF_SYMBOL_FILE_BYTES,
            MAX_DIFF_SYMBOL_TOTAL_BYTES,
        )?;
        let base_cat_file = usize::from(!base_blobs.blobs.is_empty());
        check_cancelled(cancellation)?;
        let head_blobs = git_blobs_at_resolved_revision(
            &self.config.root,
            &revisions.head_revision,
            &head_paths,
            MAX_DIFF_SYMBOL_FILE_BYTES,
            MAX_DIFF_SYMBOL_TOTAL_BYTES,
        )?;
        let head_cat_file = usize::from(!head_blobs.blobs.is_empty());
        let base = parse_historical_batch(base_blobs, "base", cancellation)?;
        let head = parse_historical_batch(head_blobs, "head", cancellation)?;

        let mut remaining_tokens = max_tokens;
        let mut remaining_diff_bytes = MAX_DIFF_SYMBOL_BYTES;
        let mut retained_diff_bytes = 0usize;
        let mut emitted_tokens = 0usize;
        let mut results = Vec::with_capacity(page_targets.len());
        for (page_index, target) in page_targets.iter().enumerate() {
            check_cancelled(cancellation)?;
            let request_index = page_start + page_index;
            let head_path = target.head_path.as_deref().unwrap_or(&target.path);
            let head_symbol = target.head_symbol.as_deref().unwrap_or(&target.symbol);
            let unavailable_reason = base
                .unavailable
                .get(&target.path)
                .or_else(|| head.unavailable.get(head_path))
                .cloned();
            if let Some(reason) = unavailable_reason {
                results.push(DiffSymbolsResult {
                    request_index,
                    target: target.clone(),
                    status: DiffSymbolsStatus::Unavailable,
                    before: None,
                    after: None,
                    diff: None,
                    diff_truncated: false,
                    semantic_change: None,
                    reason: Some(reason),
                    incomplete_reason: None,
                });
                continue;
            }
            let before = match resolve_parsed_historical_symbol(&base, &target.path, &target.symbol)
            {
                Ok(symbol) => symbol,
                Err(Error::AmbiguousSymbol { .. }) => {
                    results.push(DiffSymbolsResult {
                        request_index,
                        target: target.clone(),
                        status: DiffSymbolsStatus::Unavailable,
                        before: None,
                        after: None,
                        diff: None,
                        diff_truncated: false,
                        semantic_change: None,
                        reason: Some("ambiguous_base_symbol".into()),
                        incomplete_reason: None,
                    });
                    continue;
                }
                Err(error) => return Err(error),
            };
            let after = match resolve_parsed_historical_symbol(&head, head_path, head_symbol) {
                Ok(symbol) => symbol,
                Err(Error::AmbiguousSymbol { .. }) => {
                    results.push(DiffSymbolsResult {
                        request_index,
                        target: target.clone(),
                        status: DiffSymbolsStatus::Unavailable,
                        before: None,
                        after: None,
                        diff: None,
                        diff_truncated: false,
                        semantic_change: None,
                        reason: Some("ambiguous_head_symbol".into()),
                        incomplete_reason: None,
                    });
                    continue;
                }
                Err(error) => return Err(error),
            };
            let identity_changed = target.path != head_path || target.symbol != head_symbol;
            let (status, semantic_change) = match (&before, &after) {
                (Some(before), Some(after)) if identity_changed => (
                    DiffSymbolsStatus::Renamed,
                    Some(classify_historical_symbol_rename(
                        HistoricalSymbolChangeEndpoint {
                            path: &target.path,
                            symbol: &before.symbol,
                            signature: before.signature.as_deref(),
                            content: &before.content,
                        },
                        HistoricalSymbolChangeEndpoint {
                            path: head_path,
                            symbol: &after.symbol,
                            signature: after.signature.as_deref(),
                            content: &after.content,
                        },
                    )),
                ),
                (Some(before), Some(after)) => {
                    let change = classify_historical_symbol_change(
                        &target.path,
                        &before.symbol,
                        before.signature.as_deref(),
                        &before.content,
                        &after.symbol,
                        after.signature.as_deref(),
                        &after.content,
                    );
                    (
                        if change.is_some() {
                            DiffSymbolsStatus::Modified
                        } else {
                            DiffSymbolsStatus::Unchanged
                        },
                        change,
                    )
                }
                (None, Some(after)) => (
                    DiffSymbolsStatus::Added,
                    Some(classify_historical_symbol_added(
                        head_path,
                        &after.symbol,
                        after.signature.as_deref(),
                    )),
                ),
                (Some(before), None) => (
                    DiffSymbolsStatus::Removed,
                    Some(classify_historical_symbol_removed(
                        &target.path,
                        &before.symbol,
                        before.signature.as_deref(),
                    )),
                ),
                (None, None) => (DiffSymbolsStatus::NotFound, None),
            };
            let mut diff = None;
            let mut diff_truncated = false;
            let mut incomplete_reason = None;
            if !matches!(
                status,
                DiffSymbolsStatus::Unchanged | DiffSymbolsStatus::NotFound
            ) {
                let before_content = before
                    .as_ref()
                    .map_or("", |resolved| resolved.content.as_str());
                let after_content = after
                    .as_ref()
                    .map_or("", |resolved| resolved.content.as_str());
                if remaining_diff_bytes == 0 {
                    diff_truncated = true;
                    incomplete_reason = Some(DiffSymbolsIncompleteReason::MaxDiffBytes);
                } else if remaining_tokens == 0 {
                    diff_truncated = true;
                    incomplete_reason = Some(DiffSymbolsIncompleteReason::MaxTokens);
                } else {
                    let before_diff = symbol_diff_buffer(before_content);
                    let after_diff = symbol_diff_buffer(after_content);
                    let full_diff = TextDiff::from_lines(&before_diff, &after_diff)
                        .unified_diff()
                        .context_radius(3)
                        .header(
                            &format!("{}:{}", base.revision, target.path),
                            &format!("{}:{head_path}", head.revision),
                        )
                        .to_string();
                    let admitted = utf8_prefix(&full_diff, remaining_diff_bytes);
                    let byte_truncated = admitted.len() < full_diff.len();
                    remaining_diff_bytes = remaining_diff_bytes.saturating_sub(admitted.len());
                    let (emitted, tokens) =
                        self.config.tokenizer.truncate(admitted, remaining_tokens);
                    let token_truncated = emitted.len() < admitted.len();
                    retained_diff_bytes = retained_diff_bytes.saturating_add(emitted.len());
                    remaining_tokens = remaining_tokens.saturating_sub(tokens);
                    emitted_tokens = emitted_tokens.saturating_add(tokens);
                    diff = (!emitted.is_empty()).then(|| emitted.to_owned());
                    diff_truncated = byte_truncated || token_truncated;
                    incomplete_reason = if byte_truncated {
                        Some(DiffSymbolsIncompleteReason::MaxDiffBytes)
                    } else if token_truncated {
                        Some(DiffSymbolsIncompleteReason::MaxTokens)
                    } else {
                        None
                    };
                }
            }
            let reason = if status == DiffSymbolsStatus::NotFound {
                Some("symbol_not_found_at_both_revisions".into())
            } else if diff_truncated {
                Some("diff_truncated".into())
            } else {
                None
            };
            results.push(DiffSymbolsResult {
                request_index,
                target: target.clone(),
                status,
                before: before.map(historical_metadata),
                after: after.map(historical_metadata),
                diff,
                diff_truncated,
                semantic_change,
                reason,
                incomplete_reason,
            });
        }
        let page_complete = page_end == request.targets.len();
        let content_complete = results.iter().all(|result| {
            !result.diff_truncated && result.status != DiffSymbolsStatus::Unavailable
        });
        let mut meta = self.meta(generation, emitted_tokens, None);
        if !page_complete {
            meta.next_cursor = Some(make_diff_symbols_cursor(
                &request,
                &revisions.base_revision,
                &revisions.head_revision,
                page_end,
            ));
        }
        let parsed_symbols = base.parsed_symbols.saturating_add(head.parsed_symbols);
        let mut response = DiffSymbolsResponse {
            kind: "diff_symbols".into(),
            base: history_revision_metadata(base_metadata),
            head: history_revision_metadata(head_metadata),
            results,
            result_complete: page_complete && content_complete,
            diagnostics: DiffSymbolsDiagnostics {
                // Two revision resolutions, one commit-metadata command, one
                // tree command per endpoint, and optional batched blob reads.
                git_subprocesses: 5 + base_cat_file + head_cat_file,
                base_paths_requested: base_paths.len(),
                head_paths_requested: head_paths.len(),
                base_blob_bytes: base.blob_bytes,
                head_blob_bytes: head.blob_bytes,
                parsed_symbols,
                retained_diff_bytes,
            },
            meta,
        };
        self.fit_diff_symbols_response(&mut response, &request, page_start, options)?;
        self.finalize_bounded_response(&mut response, options)?;
        self.record_token_savings(TokenAccountingOperation::History, None, &response.meta);
        Ok(response)
    }

    fn history_sync(
        &self,
        request: HistoryRequest,
        options: ServiceCallOptions,
        cancellation: &CancellationToken,
    ) -> Result<HistoryResponse> {
        check_cancelled(cancellation)?;
        validate_history_request_with_limit(&request, self.config.max_results)?;
        let request = normalize_history_request(request)?;
        validate_history_request_with_limit(&request, self.config.max_results)?;
        let max_results = request.max_results.unwrap_or(self.config.default_results);
        let max_tokens = self.token_limit(request.max_tokens, self.config.default_read_tokens)?;
        let operation = request.operation;
        let generation = self.consistent(|snapshot| Ok(snapshot.generation()))?;
        let mut response = match operation {
            HistoryOperation::ReadSymbol {
                path,
                symbol,
                revision,
            } => {
                let mut resolved = self.historical_symbol(&path, &symbol, &revision)?;
                let (content, emitted_tokens) = self
                    .config
                    .tokenizer
                    .truncate(&resolved.content, max_tokens);
                let truncated = content.len() < resolved.content.len();
                resolved.symbol.returned_end_line =
                    returned_end_line(resolved.symbol.target_start_line, content);
                resolved.symbol.truncated = truncated;
                resolved.symbol.content = Some(content.to_owned());
                HistoryResponse {
                    kind: "read_symbol".into(),
                    symbol: Some(resolved.symbol),
                    before: None,
                    after: None,
                    diff: None,
                    diff_truncated: false,
                    semantic_change: None,
                    commits: Vec::new(),
                    result_complete: !truncated,
                    meta: self.meta(generation, emitted_tokens, None),
                }
            }
            HistoryOperation::DiffSymbol {
                path,
                symbol,
                base_revision,
                head_revision,
            } => {
                let revisions =
                    git_diff_identity(&self.config.root, &base_revision, Some(&head_revision))?;
                check_cancelled(cancellation)?;
                let before =
                    self.historical_symbol_optional(&path, &symbol, &revisions.base_revision)?;
                check_cancelled(cancellation)?;
                let after =
                    self.historical_symbol_optional(&path, &symbol, &revisions.head_revision)?;
                if before.is_none() && after.is_none() {
                    return Err(Error::SymbolNotFound {
                        path: format!(
                            "{path}@{}..{}",
                            revisions.base_revision, revisions.head_revision
                        ),
                        symbol,
                    });
                }
                let before_content = before
                    .as_ref()
                    .map_or("", |resolved| resolved.content.as_str());
                let after_content = after
                    .as_ref()
                    .map_or("", |resolved| resolved.content.as_str());
                let before_diff = symbol_diff_buffer(before_content);
                let after_diff = symbol_diff_buffer(after_content);
                let full_diff = TextDiff::from_lines(&before_diff, &after_diff)
                    .unified_diff()
                    .context_radius(3)
                    .header(
                        &format!("{}:{path}", revisions.base_revision),
                        &format!("{}:{path}", revisions.head_revision),
                    )
                    .to_string();
                let total_diff_tokens = self.config.tokenizer.count(&full_diff);
                let (diff, emitted_tokens) = self.config.tokenizer.truncate(&full_diff, max_tokens);
                let diff_truncated = emitted_tokens < total_diff_tokens;
                let semantic_change = match (&before, &after) {
                    (Some(before), Some(after)) => classify_historical_symbol_change(
                        &path,
                        &before.symbol,
                        before.signature.as_deref(),
                        &before.content,
                        &after.symbol,
                        after.signature.as_deref(),
                        &after.content,
                    ),
                    (None, Some(after)) => Some(classify_historical_symbol_added(
                        &path,
                        &after.symbol,
                        after.signature.as_deref(),
                    )),
                    (Some(before), None) => Some(classify_historical_symbol_removed(
                        &path,
                        &before.symbol,
                        before.signature.as_deref(),
                    )),
                    (None, None) => unreachable!("both absent endpoints returned above"),
                };
                let before = before.map(|mut resolved| {
                    resolved.symbol.content = None;
                    resolved.symbol.returned_end_line = 0;
                    resolved.symbol
                });
                let after = after.map(|mut resolved| {
                    resolved.symbol.content = None;
                    resolved.symbol.returned_end_line = 0;
                    resolved.symbol
                });
                HistoryResponse {
                    kind: "diff_symbol".into(),
                    symbol: None,
                    before,
                    after,
                    diff: Some(diff.to_owned()),
                    diff_truncated,
                    semantic_change,
                    commits: Vec::new(),
                    result_complete: !diff_truncated,
                    meta: self.meta(generation, emitted_tokens, None),
                }
            }
            HistoryOperation::SymbolLog {
                path,
                symbol,
                revision,
            } => {
                let revision = revision.as_deref().unwrap_or("HEAD");
                let resolved = self.historical_symbol(&path, &symbol, revision)?;
                check_cancelled(cancellation)?;
                let mut commits = git_line_history(
                    &self.config.root,
                    revision,
                    &path,
                    resolved.symbol.target_start_line,
                    resolved.symbol.target_end_line,
                    max_results.saturating_add(1),
                )?;
                let result_complete = commits.len() <= max_results;
                commits.truncate(max_results);
                HistoryResponse {
                    kind: "symbol_log".into(),
                    symbol: Some(HistoricalSymbol {
                        content: None,
                        returned_end_line: 0,
                        ..resolved.symbol
                    }),
                    before: None,
                    after: None,
                    diff: None,
                    diff_truncated: false,
                    semantic_change: None,
                    commits: commits
                        .into_iter()
                        .map(|commit| SymbolHistoryCommit {
                            commit: commit.commit,
                            authored_at: commit.authored_at,
                            subject: commit.subject,
                        })
                        .collect(),
                    result_complete,
                    meta: self.meta(generation, 0, None),
                }
            }
        };
        self.fit_history_response(&mut response, options)?;
        self.finalize_bounded_response(&mut response, options)?;
        self.record_token_savings(TokenAccountingOperation::History, None, &response.meta);
        Ok(response)
    }

    fn fit_history_response(
        &self,
        response: &mut HistoryResponse,
        options: ServiceCallOptions,
    ) -> Result<()> {
        if self.response_fits(response, options)? {
            return Ok(());
        }

        let mut minimum = None;
        if let Some(content) = response
            .symbol
            .as_ref()
            .and_then(|symbol| symbol.content.as_ref())
            .cloned()
        {
            let boundaries = char_boundaries(&content);
            minimum = Some(self.history_text_prefix_candidate(
                response,
                &content,
                &boundaries,
                usize::from(boundaries.len() > 1),
                false,
            ));
            if let Some(candidate) =
                self.fit_history_text_prefix(response, &content, &boundaries, options, false)?
            {
                *response = candidate;
                return Ok(());
            }
        } else if let Some(diff) = response.diff.clone() {
            let boundaries = char_boundaries(&diff);
            minimum = Some(self.history_text_prefix_candidate(
                response,
                &diff,
                &boundaries,
                usize::from(boundaries.len() > 1),
                true,
            ));
            if let Some(candidate) =
                self.fit_history_text_prefix(response, &diff, &boundaries, options, true)?
            {
                *response = candidate;
                return Ok(());
            }
        } else if !response.commits.is_empty() {
            let original = response.clone();
            let max_response_tokens = options
                .max_response_tokens()
                .expect("fitting only runs with a response limit");
            let budget = ResponseBudget::new(&self.config.tokenizer, max_response_tokens);
            let keep = budget.largest_fitting_prefix(original.commits.len(), |keep| {
                let mut candidate = original.clone();
                candidate.commits.truncate(keep);
                candidate.result_complete = false;
                self.finalized_response_tokens(&candidate)
            })?;
            if let Some(keep) = keep.filter(|keep| *keep > 0) {
                response.commits.truncate(keep);
                response.result_complete = false;
                return Ok(());
            }
            let mut candidate = original;
            candidate.commits.truncate(1);
            if response.commits.len() > 1 {
                candidate.result_complete = false;
            }
            minimum = Some(candidate);
        }

        Err(self.response_budget_error(
            minimum.as_ref().unwrap_or(response),
            options
                .max_response_tokens()
                .expect("fitting only runs with a response limit"),
        )?)
    }

    fn fit_diff_symbols_response(
        &self,
        response: &mut DiffSymbolsResponse,
        request: &DiffSymbolsRequest,
        page_start: usize,
        options: ServiceCallOptions,
    ) -> Result<()> {
        if self.response_fits(response, options)? {
            return Ok(());
        }
        let original = response.clone();
        let max_response_tokens = options.max_response_tokens().ok_or_else(|| {
            Error::InvalidConfiguration("fitting requires a response token limit".into())
        })?;
        let budget = ResponseBudget::new(&self.config.tokenizer, max_response_tokens);
        let mut skeleton = original.clone();
        for result in &mut skeleton.results {
            if result.diff.take().is_some() {
                result.diff_truncated = true;
                result.reason = Some("diff_truncated".into());
                result.incomplete_reason = Some(DiffSymbolsIncompleteReason::MaxResponseTokens);
            }
        }
        let keep = budget.largest_fitting_prefix(skeleton.results.len(), |keep| {
            let mut candidate = skeleton.clone();
            candidate.results.truncate(keep);
            candidate.result_complete = false;
            let next_offset = page_start.saturating_add(keep);
            candidate.meta.next_cursor = (next_offset < request.targets.len()).then(|| {
                make_diff_symbols_cursor(
                    request,
                    &candidate.base.revision,
                    &candidate.head.revision,
                    next_offset,
                )
            });
            refresh_diff_symbols_accounting(&self.config.tokenizer, &mut candidate);
            self.finalized_response_tokens(&candidate)
        })?;
        let Some(keep) = keep.filter(|keep| *keep > 0) else {
            let mut minimum = skeleton.clone();
            minimum.results.truncate(1);
            minimum.result_complete = false;
            let next_offset = page_start.saturating_add(1);
            minimum.meta.next_cursor = (next_offset < request.targets.len()).then(|| {
                make_diff_symbols_cursor(
                    request,
                    &minimum.base.revision,
                    &minimum.head.revision,
                    next_offset,
                )
            });
            refresh_diff_symbols_accounting(&self.config.tokenizer, &mut minimum);
            return Err(self.response_budget_error(&minimum, max_response_tokens)?);
        };

        let mut fitted = skeleton;
        fitted.results.truncate(keep);
        fitted.result_complete = false;
        let next_offset = page_start.saturating_add(keep);
        fitted.meta.next_cursor = (next_offset < request.targets.len()).then(|| {
            make_diff_symbols_cursor(
                request,
                &fitted.base.revision,
                &fitted.head.revision,
                next_offset,
            )
        });
        refresh_diff_symbols_accounting(&self.config.tokenizer, &mut fitted);

        for result_index in 0..keep {
            let original_result = &original.results[result_index];
            let Some(diff) = original_result.diff.as_ref() else {
                continue;
            };
            let boundaries = char_boundaries(diff);
            let full_length = boundaries.len().saturating_sub(1);
            let prefix_length = budget.largest_fitting_prefix(full_length, |prefix_length| {
                let mut candidate = fitted.clone();
                let result = &mut candidate.results[result_index];
                let prefix = &diff[..boundaries[prefix_length]];
                result.diff = (!prefix.is_empty()).then(|| prefix.to_owned());
                if prefix_length == full_length {
                    result.diff_truncated = original_result.diff_truncated;
                    result.reason.clone_from(&original_result.reason);
                    result
                        .incomplete_reason
                        .clone_from(&original_result.incomplete_reason);
                }
                refresh_diff_symbols_accounting(&self.config.tokenizer, &mut candidate);
                self.finalized_response_tokens(&candidate)
            })?;
            let Some(prefix_length) = prefix_length else {
                continue;
            };
            let result = &mut fitted.results[result_index];
            let prefix = &diff[..boundaries[prefix_length]];
            result.diff = (!prefix.is_empty()).then(|| prefix.to_owned());
            if prefix_length == full_length {
                result.diff_truncated = original_result.diff_truncated;
                result.reason.clone_from(&original_result.reason);
                result
                    .incomplete_reason
                    .clone_from(&original_result.incomplete_reason);
            }
            refresh_diff_symbols_accounting(&self.config.tokenizer, &mut fitted);
        }

        fitted.result_complete = fitted.meta.next_cursor.is_none()
            && fitted.results.iter().all(|result| {
                !result.diff_truncated && result.status != DiffSymbolsStatus::Unavailable
            });
        refresh_diff_symbols_accounting(&self.config.tokenizer, &mut fitted);
        *response = fitted;
        Ok(())
    }

    fn fit_history_text_prefix(
        &self,
        response: &HistoryResponse,
        text: &str,
        boundaries: &[usize],
        options: ServiceCallOptions,
        is_diff: bool,
    ) -> Result<Option<HistoryResponse>> {
        let max_response_tokens = options.max_response_tokens().ok_or_else(|| {
            Error::InvalidConfiguration("fitting requires a response token limit".into())
        })?;
        let budget = ResponseBudget::new(&self.config.tokenizer, max_response_tokens);
        let keep = budget.largest_fitting_prefix(boundaries.len().saturating_sub(1), |keep| {
            let candidate =
                self.history_text_prefix_candidate(response, text, boundaries, keep, is_diff);
            self.finalized_response_tokens(&candidate)
        })?;
        let Some(keep) = keep.filter(|keep| *keep > 0) else {
            return Ok(None);
        };
        Ok(Some(self.history_text_prefix_candidate(
            response, text, boundaries, keep, is_diff,
        )))
    }

    fn history_text_prefix_candidate(
        &self,
        response: &HistoryResponse,
        text: &str,
        boundaries: &[usize],
        keep: usize,
        is_diff: bool,
    ) -> HistoryResponse {
        let prefix = &text[..boundaries[keep]];
        let mut candidate = response.clone();
        let source_tokens = self.config.tokenizer.count(prefix);
        candidate.meta.source_tokens = source_tokens;
        candidate.result_complete = false;
        if is_diff {
            candidate.diff = Some(prefix.to_owned());
            candidate.diff_truncated = true;
        } else if let Some(symbol) = candidate.symbol.as_mut() {
            symbol.content = Some(prefix.to_owned());
            symbol.returned_end_line = returned_end_line(symbol.target_start_line, prefix);
            symbol.truncated = true;
        }
        candidate
    }

    fn historical_symbol(
        &self,
        path: &str,
        symbol_name: &str,
        revision: &str,
    ) -> Result<ResolvedHistoricalSymbol> {
        let blob = git_blob_at_revision(
            &self.config.root,
            revision,
            path,
            usize::try_from(self.config.max_file_bytes).unwrap_or(usize::MAX),
        )?;
        let resolved_revision = blob.revision.clone();
        self.resolve_historical_symbol(path, symbol_name, blob)?
            .ok_or_else(|| Error::SymbolNotFound {
                path: format!("{path}@{resolved_revision}"),
                symbol: symbol_name.to_owned(),
            })
    }

    fn historical_symbol_optional(
        &self,
        path: &str,
        symbol_name: &str,
        revision: &str,
    ) -> Result<Option<ResolvedHistoricalSymbol>> {
        let blob = match git_blob_at_revision(
            &self.config.root,
            revision,
            path,
            usize::try_from(self.config.max_file_bytes).unwrap_or(usize::MAX),
        ) {
            Ok(blob) => blob,
            Err(Error::InvalidInput {
                field: "path",
                reason: "file does not exist at revision",
            }) => return Ok(None),
            Err(error) => return Err(error),
        };
        self.resolve_historical_symbol(path, symbol_name, blob)
    }

    fn resolve_historical_symbol(
        &self,
        path: &str,
        symbol_name: &str,
        blob: GitBlob,
    ) -> Result<Option<ResolvedHistoricalSymbol>> {
        let parsed = parser::parse(path, &blob.content)?;
        let mut symbols = parsed.symbols;
        let symbol_index = match crate::symbol_identity::resolve_symbol_matches(
            symbols
                .iter()
                .enumerate()
                .filter(|(_, symbol)| {
                    crate::symbol_identity::symbol_identity_matches(
                        symbol_name,
                        &symbol.name,
                        symbol.parent.as_deref(),
                    )
                })
                .map(|(index, _)| index),
        ) {
            crate::symbol_identity::SymbolResolution::NotFound => return Ok(None),
            crate::symbol_identity::SymbolResolution::Unique(index) => index,
            crate::symbol_identity::SymbolResolution::Ambiguous => {
                return Err(Error::AmbiguousSymbol {
                    path: format!("{path}@{}", blob.revision),
                    symbol: symbol_name.to_owned(),
                });
            }
        };
        let symbol = symbols.remove(symbol_index);
        let content = blob
            .content
            .get(symbol.start_byte..symbol.end_byte)
            .ok_or_else(|| Error::OperationFailure("invalid historical symbol range".into()))?
            .to_owned();
        let content_hash = crate::text::hash(&content);
        Ok(Some(ResolvedHistoricalSymbol {
            symbol: HistoricalSymbol {
                revision: blob.revision,
                path: path.to_owned(),
                name: symbol.name,
                kind: symbol.kind,
                parent: symbol.parent,
                target_start_line: symbol.start_line,
                target_end_line: symbol.end_line,
                returned_end_line: symbol.end_line,
                truncated: false,
                content: None,
                content_hash,
            },
            signature: symbol.signature,
            content,
        }))
    }
}

fn returned_end_line(start_line: usize, content: &str) -> usize {
    start_line.saturating_add(content.lines().count().saturating_sub(1))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn history_normalization_trims_symbols_and_revisions_before_resolution() {
        let request = normalize_history_request(HistoryRequest {
            operation: HistoryOperation::ReadSymbol {
                path: "lib.rs".into(),
                symbol: "  Services.handle_request ".into(),
                revision: " HEAD\n".into(),
            },
            max_results: None,
            max_tokens: None,
        })
        .expect("normalized history request");

        let HistoryOperation::ReadSymbol {
            symbol, revision, ..
        } = request.operation
        else {
            panic!("expected read symbol operation");
        };
        assert_eq!(symbol, "Services.handle_request");
        assert_eq!(revision, "HEAD");
    }

    #[test]
    fn history_normalization_rejects_whitespace_only_symbols_and_revisions() {
        let error = normalize_history_request(HistoryRequest {
            operation: HistoryOperation::ReadSymbol {
                path: "lib.rs".into(),
                symbol: " \t".into(),
                revision: "HEAD".into(),
            },
            max_results: None,
            max_tokens: None,
        })
        .expect_err("whitespace-only symbol");
        assert!(matches!(
            error,
            Error::InvalidInput {
                field: "symbol",
                reason: "must not be empty"
            }
        ));

        let error = normalize_history_request(HistoryRequest {
            operation: HistoryOperation::ReadSymbol {
                path: "lib.rs".into(),
                symbol: "handle_request".into(),
                revision: " \n".into(),
            },
            max_results: None,
            max_tokens: None,
        })
        .expect_err("whitespace-only revision");
        assert!(matches!(
            error,
            Error::InvalidInput {
                field: "revision",
                reason: "must not be empty"
            }
        ));
    }

    #[test]
    fn batch_history_normalization_refines_endpoint_values() {
        let request = normalize_diff_symbols_request(DiffSymbolsRequest {
            targets: vec![crate::model::DiffSymbolsTarget {
                path: "lib.rs".into(),
                symbol: "  handle_request ".into(),
                head_path: None,
                head_symbol: Some(" handle_request ".into()),
            }],
            base_revision: " HEAD~1 ".into(),
            head_revision: " HEAD ".into(),
            max_results: None,
            max_tokens: None,
            cursor: None,
        })
        .expect("normalized batch history request");

        assert_eq!(request.base_revision, "HEAD~1");
        assert_eq!(request.head_revision, "HEAD");
        assert_eq!(request.targets[0].symbol, "handle_request");
        assert_eq!(
            request.targets[0].head_symbol.as_deref(),
            Some("handle_request")
        );
    }

    #[test]
    fn raw_history_symbol_bytes_are_bounded_before_normalization() {
        let request = HistoryRequest {
            operation: HistoryOperation::ReadSymbol {
                path: "lib.rs".into(),
                symbol: format!("{}symbol", " ".repeat(MAX_PATTERN_BYTES + 1)),
                revision: "HEAD".into(),
            },
            max_results: None,
            max_tokens: None,
        };

        assert!(matches!(
            validate_history_request(&request),
            Err(Error::InputTooLong {
                field: "symbol",
                max_bytes: MAX_PATTERN_BYTES,
            })
        ));
    }

    #[test]
    fn raw_diff_symbol_target_count_is_bounded_before_normalization() {
        let target = crate::model::DiffSymbolsTarget {
            path: "lib.rs".into(),
            symbol: "answer".into(),
            head_path: None,
            head_symbol: None,
        };
        let request = DiffSymbolsRequest {
            targets: vec![target; MAX_DIFF_SYMBOL_TARGETS + 1],
            base_revision: "HEAD~1".into(),
            head_revision: "HEAD".into(),
            max_results: None,
            max_tokens: None,
            cursor: None,
        };

        let error = validate_diff_symbols_request(&request).expect_err("target count limit");
        assert!(matches!(
            error,
            Error::RequestLimitExceeded {
                field: "targets",
                requested,
                limit,
            } if requested == MAX_DIFF_SYMBOL_TARGETS + 1 && limit == MAX_DIFF_SYMBOL_TARGETS
        ));
    }

    #[test]
    fn configured_history_result_limit_is_checked_before_git_work() {
        let request = HistoryRequest {
            operation: HistoryOperation::SymbolLog {
                path: "lib.rs".into(),
                symbol: "answer".into(),
                revision: None,
            },
            max_results: Some(3),
            max_tokens: None,
        };

        assert!(matches!(
            validate_history_request_with_limit(&request, 2),
            Err(Error::RequestLimitExceeded {
                field: "max_results",
                requested: 3,
                limit: 2,
            })
        ));
    }
}
