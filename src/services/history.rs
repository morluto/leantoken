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
use super::cursor::{ContinuationCursor, CursorKind, StreamId, StreamIdentityBuilder};
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

struct ResolvedHistoricalSymbol {
    symbol: HistoricalSymbol,
    signature: Option<String>,
    content: String,
}

#[derive(Clone, Copy)]
enum HistoryText<'a> {
    Symbol(&'a str),
    Diff(&'a str),
}

impl<'a> HistoryText<'a> {
    const fn content(self) -> &'a str {
        match self {
            Self::Symbol(content) | Self::Diff(content) => content,
        }
    }
}

enum HistoricalSymbolEndpoints {
    Modified {
        before: Box<ResolvedHistoricalSymbol>,
        after: Box<ResolvedHistoricalSymbol>,
    },
    Added {
        after: Box<ResolvedHistoricalSymbol>,
    },
    Removed {
        before: Box<ResolvedHistoricalSymbol>,
    },
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

#[derive(Debug)]
struct ParsedHistoryRequest {
    operation: ParsedHistoryOperation,
    max_results: Option<usize>,
    max_tokens: Option<usize>,
}

#[derive(Debug)]
enum ParsedHistoryOperation {
    ReadSymbol {
        path: String,
        symbol: String,
        revision: String,
    },
    DiffSymbol {
        path: String,
        symbol: String,
        base_revision: String,
        head_revision: String,
    },
    SymbolLog {
        path: String,
        symbol: String,
        revision: Option<String>,
    },
}

#[derive(Debug, PartialEq, Eq)]
struct HistoricalSymbolTarget {
    path: String,
    symbol: String,
}

#[derive(Debug)]
enum HistoricalHeadTarget {
    SameAsBase,
    Override(HistoricalSymbolTarget),
}

#[derive(Debug)]
struct ParsedDiffSymbolsTarget {
    base: HistoricalSymbolTarget,
    head: HistoricalHeadTarget,
}

#[derive(Debug)]
struct ParsedDiffSymbolsRequest {
    targets: Vec<ParsedDiffSymbolsTarget>,
    base_revision: String,
    head_revision: String,
    max_results: Option<usize>,
    max_tokens: Option<usize>,
    cursor: Option<ContinuationCursor>,
}

impl ParsedDiffSymbolsTarget {
    fn head(&self) -> &HistoricalSymbolTarget {
        match &self.head {
            HistoricalHeadTarget::SameAsBase => &self.base,
            HistoricalHeadTarget::Override(target) => target,
        }
    }

    fn identity_changed(&self) -> bool {
        self.base != *self.head()
    }

    fn response_target(&self) -> crate::model::DiffSymbolsTarget {
        let (head_path, head_symbol) = match &self.head {
            HistoricalHeadTarget::SameAsBase => (None, None),
            HistoricalHeadTarget::Override(target) => {
                (Some(target.path.clone()), Some(target.symbol.clone()))
            }
        };
        crate::model::DiffSymbolsTarget {
            path: self.base.path.clone(),
            symbol: self.base.symbol.clone(),
            head_path,
            head_symbol,
        }
    }
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

fn parse_history_request(
    request: HistoryRequest,
    max_results_limit: usize,
) -> Result<ParsedHistoryRequest> {
    fn parse_path(value: String) -> Result<String> {
        validate_input(&value, "path", MAX_PATH_BYTES)?;
        normalize_relative(&value)
    }

    fn parse_non_empty(value: String, field: &'static str, max_bytes: usize) -> Result<String> {
        validate_input(&value, field, max_bytes)?;
        let value = value.trim().to_owned();
        if value.is_empty() {
            return Err(Error::InvalidInput {
                field,
                reason: "must not be empty",
            });
        }
        Ok(value)
    }

    if let Some(max_results) = request.max_results {
        validate_positive_request_limit("max_results", max_results, max_results_limit)?;
    }
    let operation = match request.operation {
        HistoryOperation::ReadSymbol {
            path,
            symbol,
            revision,
        } => ParsedHistoryOperation::ReadSymbol {
            path: parse_path(path)?,
            symbol: parse_non_empty(symbol, "symbol", MAX_PATTERN_BYTES)?,
            revision: parse_non_empty(revision, "revision", MAX_PATTERN_BYTES)?,
        },
        HistoryOperation::SymbolLog {
            path,
            symbol,
            revision,
        } => ParsedHistoryOperation::SymbolLog {
            path: parse_path(path)?,
            symbol: parse_non_empty(symbol, "symbol", MAX_PATTERN_BYTES)?,
            revision: revision
                .map(|revision| parse_non_empty(revision, "revision", MAX_PATTERN_BYTES))
                .transpose()?,
        },
        HistoryOperation::DiffSymbol {
            path,
            symbol,
            base_revision,
            head_revision,
        } => ParsedHistoryOperation::DiffSymbol {
            path: parse_path(path)?,
            symbol: parse_non_empty(symbol, "symbol", MAX_PATTERN_BYTES)?,
            base_revision: parse_non_empty(base_revision, "base revision", MAX_PATTERN_BYTES)?,
            head_revision: parse_non_empty(head_revision, "head revision", MAX_PATTERN_BYTES)?,
        },
    };
    Ok(ParsedHistoryRequest {
        operation,
        max_results: request.max_results,
        max_tokens: request.max_tokens,
    })
}

fn parse_diff_symbols_request(
    request: DiffSymbolsRequest,
    max_results_limit: usize,
) -> Result<ParsedDiffSymbolsRequest> {
    fn parse_path(value: String, field: &'static str) -> Result<String> {
        validate_input(&value, field, MAX_PATH_BYTES)?;
        normalize_relative(&value)
    }

    fn parse_non_empty(value: String, field: &'static str) -> Result<String> {
        validate_input(&value, field, MAX_PATTERN_BYTES)?;
        let value = value.trim().to_owned();
        if value.is_empty() {
            return Err(Error::InvalidInput {
                field,
                reason: "must not be empty",
            });
        }
        Ok(value)
    }

    validate_positive_request_limit("targets", request.targets.len(), MAX_DIFF_SYMBOL_TARGETS)?;
    if let Some(max_results) = request.max_results {
        validate_positive_request_limit(
            "max_results",
            max_results,
            max_results_limit.min(MAX_DIFF_SYMBOL_RESULTS),
        )?;
    }
    let cursor = ContinuationCursor::parse_optional(request.cursor.as_deref())?;
    let base_revision = parse_non_empty(request.base_revision, "base revision")?;
    let head_revision = parse_non_empty(request.head_revision, "head revision")?;
    let mut base_paths = BTreeSet::new();
    let mut head_paths = BTreeSet::new();
    let mut seen = BTreeSet::new();
    let mut targets = Vec::with_capacity(request.targets.len());
    for target in request.targets {
        let base = HistoricalSymbolTarget {
            path: parse_path(target.path, "path")?,
            symbol: parse_non_empty(target.symbol, "symbol")?,
        };
        let head = match (target.head_path, target.head_symbol) {
            (Some(path), Some(symbol)) => HistoricalHeadTarget::Override(HistoricalSymbolTarget {
                path: parse_path(path, "head path")?,
                symbol: parse_non_empty(symbol, "head symbol")?,
            }),
            (None, None) => HistoricalHeadTarget::SameAsBase,
            _ => {
                return Err(Error::InvalidInput {
                    field: "targets",
                    reason: "head_path and head_symbol must be supplied together",
                });
            }
        };
        let head_target = match &head {
            HistoricalHeadTarget::SameAsBase => &base,
            HistoricalHeadTarget::Override(target) => target,
        };
        if !seen.insert((
            base.path.clone(),
            base.symbol.clone(),
            head_target.path.clone(),
            head_target.symbol.clone(),
        )) {
            return Err(Error::InvalidInput {
                field: "targets",
                reason: "must not contain duplicate symbol pairings",
            });
        }
        base_paths.insert(base.path.clone());
        head_paths.insert(head_target.path.clone());
        targets.push(ParsedDiffSymbolsTarget { base, head });
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
    Ok(ParsedDiffSymbolsRequest {
        targets,
        base_revision,
        head_revision,
        max_results: request.max_results,
        max_tokens: request.max_tokens,
        cursor,
    })
}

fn diff_symbols_stream_id(
    services: &Services,
    request: &ParsedDiffSymbolsRequest,
    base_revision: &str,
    head_revision: &str,
) -> StreamId {
    let mut stream = StreamIdentityBuilder::for_service(services, CursorKind::HistoryDiffSymbols);
    stream.field_str("base_revision", base_revision);
    stream.field_str("head_revision", head_revision);
    stream.field_usize("target_count", request.targets.len());
    for target in &request.targets {
        stream.field_str("base_path", &target.base.path);
        stream.field_str("base_symbol", &target.base.symbol);
        match &target.head {
            HistoricalHeadTarget::SameAsBase => {
                stream.field_bool("head_override", false);
            }
            HistoricalHeadTarget::Override(head) => {
                stream.field_bool("head_override", true);
                stream.field_str("head_path", &head.path);
                stream.field_str("head_symbol", &head.symbol);
            }
        }
    }
    stream.finish()
}

fn make_diff_symbols_cursor(stream_id: StreamId, offset: usize) -> Result<String> {
    ContinuationCursor::at(CursorKind::HistoryDiffSymbols, 0, stream_id, offset)
        .map(ContinuationCursor::encode)
}

fn parse_diff_symbols_cursor(
    request: &ParsedDiffSymbolsRequest,
    stream_id: StreamId,
) -> Result<usize> {
    let offset = request
        .cursor
        .map(|cursor| cursor.position_for(CursorKind::HistoryDiffSymbols, 0, stream_id))
        .transpose()?
        .unwrap_or(0);
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
        let request = self.observe_service_result(
            operation,
            parse_history_request(request, self.config.max_results),
        )?;
        let this = self.clone();
        let result = self
            .runtime
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
        let request = self.observe_service_result(
            operation,
            parse_diff_symbols_request(request, self.config.max_results),
        )?;
        let this = self.clone();
        let result = self
            .runtime
            .blocking_executor
            .run(cancellation, move |cancellation| {
                this.history_diff_symbols_sync(request, options, cancellation)
            })
            .await;
        self.observe_service_result(operation, result)
    }

    fn history_diff_symbols_sync(
        &self,
        request: ParsedDiffSymbolsRequest,
        options: ServiceCallOptions,
        cancellation: &CancellationToken,
    ) -> Result<DiffSymbolsResponse> {
        check_cancelled(cancellation)?;
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
        let stream_id = diff_symbols_stream_id(
            self,
            &request,
            &revisions.base_revision,
            &revisions.head_revision,
        );
        let page_start = parse_diff_symbols_cursor(&request, stream_id)?;
        let page_end = page_start
            .saturating_add(max_results)
            .min(request.targets.len());
        let page_targets = &request.targets[page_start..page_end];
        let base_paths = page_targets
            .iter()
            .map(|target| target.base.path.clone())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        let head_paths = page_targets
            .iter()
            .map(|target| target.head().path.clone())
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
            let head_target = target.head();
            let head_path = head_target.path.as_str();
            let head_symbol = head_target.symbol.as_str();
            let unavailable_reason = base
                .unavailable
                .get(&target.base.path)
                .or_else(|| head.unavailable.get(head_path))
                .cloned();
            if let Some(reason) = unavailable_reason {
                results.push(DiffSymbolsResult {
                    request_index,
                    target: target.response_target(),
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
            let before = match resolve_parsed_historical_symbol(
                &base,
                &target.base.path,
                &target.base.symbol,
            ) {
                Ok(symbol) => symbol,
                Err(Error::AmbiguousSymbol { .. }) => {
                    results.push(DiffSymbolsResult {
                        request_index,
                        target: target.response_target(),
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
                        target: target.response_target(),
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
            let identity_changed = target.identity_changed();
            let (status, semantic_change) = match (&before, &after) {
                (Some(before), Some(after)) if identity_changed => (
                    DiffSymbolsStatus::Renamed,
                    Some(classify_historical_symbol_rename(
                        HistoricalSymbolChangeEndpoint {
                            path: &target.base.path,
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
                        &target.base.path,
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
                        &target.base.path,
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
                            &format!("{}:{}", base.revision, target.base.path),
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
                target: target.response_target(),
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
            meta.next_cursor = Some(make_diff_symbols_cursor(stream_id, page_end)?);
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
        self.fit_diff_symbols_response(&mut response, &request, page_start, stream_id, options)?;
        self.finalize_bounded_response(&mut response, options)?;
        self.record_token_savings(TokenAccountingOperation::History, None, &response.meta);
        Ok(response)
    }

    fn history_sync(
        &self,
        request: ParsedHistoryRequest,
        options: ServiceCallOptions,
        cancellation: &CancellationToken,
    ) -> Result<HistoryResponse> {
        check_cancelled(cancellation)?;
        let max_results = request.max_results.unwrap_or(self.config.default_results);
        let max_tokens = self.token_limit(request.max_tokens, self.config.default_read_tokens)?;
        let operation = request.operation;
        let generation = self.consistent(|snapshot| Ok(snapshot.generation()))?;
        let mut response = match operation {
            ParsedHistoryOperation::ReadSymbol {
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
            ParsedHistoryOperation::DiffSymbol {
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
                let endpoints = match (before, after) {
                    (Some(before), Some(after)) => HistoricalSymbolEndpoints::Modified {
                        before: Box::new(before),
                        after: Box::new(after),
                    },
                    (None, Some(after)) => HistoricalSymbolEndpoints::Added {
                        after: Box::new(after),
                    },
                    (Some(before), None) => HistoricalSymbolEndpoints::Removed {
                        before: Box::new(before),
                    },
                    (None, None) => {
                        return Err(Error::SymbolNotFound {
                            path: format!(
                                "{path}@{}..{}",
                                revisions.base_revision, revisions.head_revision
                            ),
                            symbol,
                        });
                    }
                };
                let (before_content, after_content) = match &endpoints {
                    HistoricalSymbolEndpoints::Modified { before, after } => {
                        (before.content.as_str(), after.content.as_str())
                    }
                    HistoricalSymbolEndpoints::Added { after } => ("", after.content.as_str()),
                    HistoricalSymbolEndpoints::Removed { before } => (before.content.as_str(), ""),
                };
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
                let semantic_change = match &endpoints {
                    HistoricalSymbolEndpoints::Modified { before, after } => {
                        classify_historical_symbol_change(
                            &path,
                            &before.symbol,
                            before.signature.as_deref(),
                            &before.content,
                            &after.symbol,
                            after.signature.as_deref(),
                            &after.content,
                        )
                    }
                    HistoricalSymbolEndpoints::Added { after } => {
                        Some(classify_historical_symbol_added(
                            &path,
                            &after.symbol,
                            after.signature.as_deref(),
                        ))
                    }
                    HistoricalSymbolEndpoints::Removed { before } => {
                        Some(classify_historical_symbol_removed(
                            &path,
                            &before.symbol,
                            before.signature.as_deref(),
                        ))
                    }
                };
                let clear_content = |mut resolved: ResolvedHistoricalSymbol| {
                    resolved.symbol.content = None;
                    resolved.symbol.returned_end_line = 0;
                    resolved.symbol
                };
                let (before, after) = match endpoints {
                    HistoricalSymbolEndpoints::Modified { before, after } => {
                        (Some(clear_content(*before)), Some(clear_content(*after)))
                    }
                    HistoricalSymbolEndpoints::Added { after } => {
                        (None, Some(clear_content(*after)))
                    }
                    HistoricalSymbolEndpoints::Removed { before } => {
                        (Some(clear_content(*before)), None)
                    }
                };
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
            ParsedHistoryOperation::SymbolLog {
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
                HistoryText::Symbol(&content),
                &boundaries,
                usize::from(boundaries.len() > 1),
            ));
            if let Some(candidate) = self.fit_history_text_prefix(
                response,
                HistoryText::Symbol(&content),
                &boundaries,
                options,
            )? {
                *response = candidate;
                return Ok(());
            }
        } else if let Some(diff) = response.diff.clone() {
            let boundaries = char_boundaries(&diff);
            minimum = Some(self.history_text_prefix_candidate(
                response,
                HistoryText::Diff(&diff),
                &boundaries,
                usize::from(boundaries.len() > 1),
            ));
            if let Some(candidate) = self.fit_history_text_prefix(
                response,
                HistoryText::Diff(&diff),
                &boundaries,
                options,
            )? {
                *response = candidate;
                return Ok(());
            }
        } else if !response.commits.is_empty() {
            let original = response.clone();
            let max_response_tokens = options
                .max_response_tokens()
                .expect("fitting only runs with a response limit");
            let budget = ResponseBudget::new(max_response_tokens);
            let keep = budget.largest_fitting_prefix(original.commits.len(), |keep| {
                let mut candidate = original.clone();
                candidate.commits.truncate(keep);
                candidate.result_complete = false;
                self.finalized_response_tokens(&candidate, options)
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
            options,
        )?)
    }

    fn fit_diff_symbols_response(
        &self,
        response: &mut DiffSymbolsResponse,
        request: &ParsedDiffSymbolsRequest,
        page_start: usize,
        stream_id: StreamId,
        options: ServiceCallOptions,
    ) -> Result<()> {
        if self.response_fits(response, options)? {
            return Ok(());
        }
        let original = response.clone();
        let max_response_tokens = options.max_response_tokens().ok_or_else(|| {
            Error::InvalidConfiguration("fitting requires a response token limit".into())
        })?;
        let budget = ResponseBudget::new(max_response_tokens);
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
            candidate.meta.next_cursor = (next_offset < request.targets.len())
                .then(|| make_diff_symbols_cursor(stream_id, next_offset))
                .transpose()?;
            refresh_diff_symbols_accounting(&self.config.tokenizer, &mut candidate);
            self.finalized_response_tokens(&candidate, options)
        })?;
        let Some(keep) = keep.filter(|keep| *keep > 0) else {
            let mut minimum = skeleton.clone();
            minimum.results.truncate(1);
            minimum.result_complete = false;
            let next_offset = page_start.saturating_add(1);
            minimum.meta.next_cursor = (next_offset < request.targets.len())
                .then(|| make_diff_symbols_cursor(stream_id, next_offset))
                .transpose()?;
            refresh_diff_symbols_accounting(&self.config.tokenizer, &mut minimum);
            return Err(self.response_budget_error(&minimum, max_response_tokens, options)?);
        };

        let mut fitted = skeleton;
        fitted.results.truncate(keep);
        fitted.result_complete = false;
        let next_offset = page_start.saturating_add(keep);
        fitted.meta.next_cursor = (next_offset < request.targets.len())
            .then(|| make_diff_symbols_cursor(stream_id, next_offset))
            .transpose()?;
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
                self.finalized_response_tokens(&candidate, options)
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
        text: HistoryText<'_>,
        boundaries: &[usize],
        options: ServiceCallOptions,
    ) -> Result<Option<HistoryResponse>> {
        let max_response_tokens = options.max_response_tokens().ok_or_else(|| {
            Error::InvalidConfiguration("fitting requires a response token limit".into())
        })?;
        let budget = ResponseBudget::new(max_response_tokens);
        let keep = budget.largest_fitting_prefix(boundaries.len().saturating_sub(1), |keep| {
            let candidate = self.history_text_prefix_candidate(response, text, boundaries, keep);
            self.finalized_response_tokens(&candidate, options)
        })?;
        let Some(keep) = keep.filter(|keep| *keep > 0) else {
            return Ok(None);
        };
        Ok(Some(self.history_text_prefix_candidate(
            response, text, boundaries, keep,
        )))
    }

    fn history_text_prefix_candidate(
        &self,
        response: &HistoryResponse,
        text: HistoryText<'_>,
        boundaries: &[usize],
        keep: usize,
    ) -> HistoryResponse {
        let prefix = &text.content()[..boundaries[keep]];
        let mut candidate = response.clone();
        let source_tokens = self.config.tokenizer.count(prefix);
        candidate.meta.source_tokens = source_tokens;
        candidate.result_complete = false;
        match text {
            HistoryText::Diff(_) => {
                candidate.diff = Some(prefix.to_owned());
                candidate.diff_truncated = true;
            }
            HistoryText::Symbol(_) => {
                if let Some(symbol) = candidate.symbol.as_mut() {
                    symbol.content = Some(prefix.to_owned());
                    symbol.returned_end_line = returned_end_line(symbol.target_start_line, prefix);
                    symbol.truncated = true;
                }
            }
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
        let request = parse_history_request(
            HistoryRequest {
                operation: HistoryOperation::ReadSymbol {
                    path: "lib.rs".into(),
                    symbol: "  Services.handle_request ".into(),
                    revision: " HEAD\n".into(),
                },
                max_results: None,
                max_tokens: None,
            },
            MAX_HISTORY_RESULTS,
        )
        .expect("normalized history request");

        let ParsedHistoryOperation::ReadSymbol {
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
        let error = parse_history_request(
            HistoryRequest {
                operation: HistoryOperation::ReadSymbol {
                    path: "lib.rs".into(),
                    symbol: " \t".into(),
                    revision: "HEAD".into(),
                },
                max_results: None,
                max_tokens: None,
            },
            MAX_HISTORY_RESULTS,
        )
        .expect_err("whitespace-only symbol");
        assert!(matches!(
            error,
            Error::InvalidInput {
                field: "symbol",
                reason: "must not be empty"
            }
        ));

        let error = parse_history_request(
            HistoryRequest {
                operation: HistoryOperation::ReadSymbol {
                    path: "lib.rs".into(),
                    symbol: "handle_request".into(),
                    revision: " \n".into(),
                },
                max_results: None,
                max_tokens: None,
            },
            MAX_HISTORY_RESULTS,
        )
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
        let request = parse_diff_symbols_request(
            DiffSymbolsRequest {
                targets: vec![crate::model::DiffSymbolsTarget {
                    path: "lib.rs".into(),
                    symbol: "  handle_request ".into(),
                    head_path: Some("lib.rs".into()),
                    head_symbol: Some(" handle_request ".into()),
                }],
                base_revision: " HEAD~1 ".into(),
                head_revision: " HEAD ".into(),
                max_results: None,
                max_tokens: None,
                cursor: None,
            },
            MAX_HISTORY_RESULTS,
        )
        .expect("normalized batch history request");

        assert_eq!(request.base_revision, "HEAD~1");
        assert_eq!(request.head_revision, "HEAD");
        assert_eq!(request.targets[0].base.symbol, "handle_request");
        let HistoricalHeadTarget::Override(head) = &request.targets[0].head else {
            panic!("expected an explicit head target");
        };
        assert_eq!(head.symbol, "handle_request");
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
            parse_history_request(request, MAX_HISTORY_RESULTS),
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

        let error = parse_diff_symbols_request(request, MAX_HISTORY_RESULTS)
            .expect_err("target count limit");
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
            parse_history_request(request, 2),
            Err(Error::RequestLimitExceeded {
                field: "max_results",
                requested: 3,
                limit: 2,
            })
        ));
    }
}
