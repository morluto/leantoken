//! Bounded live reads and index-backed excerpts.

use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};

use tokio_util::sync::CancellationToken;

use super::execution_options::RetrievalExecution;
use super::receipts::{ReceiptDecision, ReceiptEvidence};
use super::validation::{
    MAX_PATH_BYTES, MAX_PATTERN_BYTES, check_cancelled, is_lower_hex, validate_input,
    validate_optional_input,
};
use super::{ServiceCallOptions, Services};
use crate::model::*;
use crate::repository::{normalize_relative, resolve_existing, validate_relative};
use crate::storage::ReadSession;
use crate::text::{anchored_line_window, hash};
use crate::tokens::ResponseBudget;
use crate::{Error, Result};

const MIN_CONTEXT_RANGE_LINES: usize = 12;
const MAX_CONTEXT_RANGE_LINES: usize = 128;
// Re-tokenize bounded candidate windows instead of guessing a byte/token ratio.
// The hard cap prevents pathological low-token inputs from growing forever.
const LIVE_READ_TOKEN_CHECK_BYTES: usize = 64 * 1024;
const MAX_LIVE_READ_BYTES: usize = 8 * 1024 * 1024;

#[derive(Clone)]
pub(super) struct StoredExcerpt {
    pub(super) content: String,
    pub(super) start_line: usize,
    pub(super) end_line: usize,
}

pub(super) struct StoredExcerptRequest {
    pub file_id: i64,
    pub desired_start_line: usize,
    pub desired_end_line: usize,
    pub required_start_line: usize,
    pub required_end_line: usize,
    pub max_lines: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct ResolvedStoredExcerptRequest {
    file_id: i64,
    start_line: usize,
    end_line: usize,
}

impl StoredExcerptRequest {
    fn resolve(&self, file_end_line: Option<usize>) -> Option<ResolvedStoredExcerptRequest> {
        let file_end_line = file_end_line?;
        let required_start = self.required_start_line.max(1);
        if required_start > file_end_line {
            return None;
        }
        let required_end = self
            .required_end_line
            .max(required_start)
            .min(file_end_line);
        let desired_start = self.desired_start_line.max(1).min(file_end_line);
        let desired_end = self.desired_end_line.max(desired_start).min(file_end_line);
        let (start_line, end_line) = anchored_line_window(
            desired_start,
            desired_end,
            required_start,
            required_end,
            self.max_lines,
        );
        Some(ResolvedStoredExcerptRequest {
            file_id: self.file_id,
            start_line,
            end_line,
        })
    }
}

pub(super) struct AdaptiveExcerptRequest {
    pub file_id: i64,
    pub declaration_start: usize,
    pub declaration_end: usize,
    pub matched_line: usize,
    pub token_budget: usize,
}

#[derive(Debug, Clone)]
struct ResolvedReadTarget {
    target_start_line: usize,
    target_end_line: Option<usize>,
    page_start_line: usize,
    page_start_byte: usize,
    expected_full_hash: Option<String>,
}

#[derive(Debug)]
struct LiveReadRange {
    content: String,
    page_start_line: usize,
    target_bytes: usize,
}

struct LiveFileSnapshot {
    content_hash: String,
    end_line: usize,
}

struct LiveReadObservation {
    snapshot: LiveFileSnapshot,
    range: LiveReadRange,
}

struct MaterializedRead {
    response: ReadResponse,
    baseline_source_tokens: usize,
    current_content: String,
    current_tokens: usize,
}

#[derive(Debug)]
struct ReadCursor {
    generation: u64,
    target_start_line: usize,
    target_end_line: usize,
    next_start_line: usize,
    next_byte: usize,
    full_hash: String,
    path_hash: String,
}

impl ReadCursor {
    fn encode(&self) -> String {
        format!(
            "{}:read:{}:{}:{}:{}:{}:{}",
            self.generation,
            self.target_start_line,
            self.target_end_line,
            self.next_start_line,
            self.next_byte,
            self.full_hash,
            self.path_hash,
        )
    }
}

fn assemble_stored_excerpt(
    request: ResolvedStoredExcerptRequest,
    selected: &[crate::storage::ChunkRecord],
) -> Option<StoredExcerpt> {
    let first_chunk = selected.first()?;
    let base_line = first_chunk.start_line;
    let mut combined = String::new();
    for chunk in selected {
        combined.push_str(&chunk.content);
    }
    let local_start = request.start_line.saturating_sub(base_line) + 1;
    let local_end = request.end_line.saturating_sub(base_line) + 1;
    Some(StoredExcerpt {
        content: crate::text::excerpt(&combined, local_start, local_end),
        start_line: request.start_line,
        end_line: request.end_line,
    })
}

fn validate_read_input(request: &ReadRequest) -> Result<()> {
    validate_input(&request.path, "path", MAX_PATH_BYTES)?;
    if request.symbol.as_deref().is_some_and(str::is_empty) {
        return Err(Error::InvalidInput {
            field: "symbol",
            reason: "must not be empty",
        });
    }
    validate_optional_input(request.symbol.as_deref(), "symbol", MAX_PATTERN_BYTES)?;
    if request
        .heading
        .as_deref()
        .is_some_and(|heading| heading.trim().is_empty())
    {
        return Err(Error::InvalidInput {
            field: "heading",
            reason: "must not be empty",
        });
    }
    validate_optional_input(request.heading.as_deref(), "heading", MAX_PATTERN_BYTES)?;
    if request.heading_occurrence == Some(0) {
        return Err(Error::InvalidInput {
            field: "heading occurrence",
            reason: "must be one-based",
        });
    }
    if request.heading_occurrence.is_some() && request.heading.is_none() {
        return Err(Error::InvalidInput {
            field: "heading occurrence",
            reason: "requires a heading target",
        });
    }
    validate_optional_input(request.expected_hash.as_deref(), "expected hash", 128)?;
    validate_optional_input(
        request.continuation_cursor.as_deref(),
        "continuation cursor",
        256,
    )?;
    if let Some(cursor) = request.continuation_cursor.as_deref() {
        decode_read_cursor(cursor)?;
    }
    validate_relative(&request.path)?;
    let has_line_target = request.start_line.is_some() || request.end_line.is_some();
    if request.symbol.is_some() && has_line_target {
        return Err(Error::InvalidInput {
            field: "read target",
            reason: "must use either a symbol or line range, not both",
        });
    }
    if request.heading.is_some() && (request.symbol.is_some() || has_line_target) {
        return Err(Error::InvalidInput {
            field: "read target",
            reason: "must use either a heading, symbol, or line range",
        });
    }
    if request.continuation_cursor.is_some()
        && (request.symbol.is_some() || request.heading.is_some() || has_line_target)
    {
        return Err(Error::InvalidInput {
            field: "read target",
            reason: "must use either a continuation cursor or a new target, not both",
        });
    }
    if request.delta && request.continuation_cursor.is_some() {
        return Err(Error::InvalidInput {
            field: "delta",
            reason: "is supported only for a new line, symbol, or heading target",
        });
    }
    if request.symbol.is_none()
        && request.heading.is_none()
        && request.continuation_cursor.is_none()
    {
        let start_line = request.start_line.unwrap_or(1);
        if start_line == 0
            || request
                .end_line
                .is_some_and(|end_line| end_line < start_line)
        {
            return Err(invalid_line_range());
        }
    }
    Ok(())
}

impl Services {
    pub async fn read(&self, request: ReadRequest) -> Result<ReadResponse> {
        self.read_with_options(request, ServiceCallOptions::new())
            .await
    }

    /// Read live source under explicit serialized-response controls.
    pub async fn read_with_options(
        &self,
        request: ReadRequest,
        options: ServiceCallOptions,
    ) -> Result<ReadResponse> {
        self.read_execute(
            request,
            RetrievalExecution::direct(options, CancellationToken::new()),
        )
        .await
    }

    /// Read source after applying the requested index consistency boundary.
    pub async fn read_with_consistency_cancellable(
        &self,
        request: ReadRequest,
        consistency: IndexConsistency,
        cancellation: CancellationToken,
    ) -> Result<ReadResponse> {
        self.read_execute(
            request,
            RetrievalExecution::consistent(consistency, ServiceCallOptions::new(), cancellation),
        )
        .await
    }

    /// Read source under consistency and serialized-response controls.
    pub async fn read_with_options_consistency_cancellable(
        &self,
        request: ReadRequest,
        consistency: IndexConsistency,
        options: ServiceCallOptions,
        cancellation: CancellationToken,
    ) -> Result<ReadResponse> {
        self.read_execute(
            request,
            RetrievalExecution::consistent(consistency, options, cancellation),
        )
        .await
    }

    pub async fn read_cancellable(
        &self,
        request: ReadRequest,
        cancellation: CancellationToken,
    ) -> Result<ReadResponse> {
        self.read_execute(
            request,
            RetrievalExecution::direct(ServiceCallOptions::new(), cancellation),
        )
        .await
    }

    async fn read_execute(
        &self,
        request: ReadRequest,
        execution: RetrievalExecution,
    ) -> Result<ReadResponse> {
        let operation = TokenAccountingOperation::Read;
        let RetrievalExecution {
            consistency,
            options,
            cancellation,
        } = execution;
        self.observe_service_result(operation, self.validate_call_options(options))?;
        if let Some(consistency) = consistency {
            self.observe_service_result(operation, validate_read_input(&request))?;
            self.observe_service_result(
                operation,
                self.token_limit(request.max_tokens, self.config.default_read_tokens),
            )?;
            let consistency_result = self
                .apply_consistency(consistency, cancellation.clone())
                .await;
            self.observe_service_result(operation, consistency_result)?;
        }
        let this = self.clone();
        let result = self
            .blocking_executor
            .run(cancellation, move |cancellation| {
                this.read_sync(request, options, cancellation)
            })
            .await;
        self.observe_service_result(operation, result)
    }

    fn read_sync(
        &self,
        mut request: ReadRequest,
        options: ServiceCallOptions,
        cancellation: &CancellationToken,
    ) -> Result<ReadResponse> {
        check_cancelled(cancellation)?;
        validate_read_input(&request)?;
        request.path = normalize_relative(&request.path)?;
        let max_tokens = self.token_limit(request.max_tokens, self.config.default_read_tokens)?;
        let materialized = self.consistent(|session, generation| {
            check_cancelled(cancellation)?;
            self.read_at_generation_with_options(session, &request, generation, max_tokens, options)
        })?;
        let mut response = materialized.response;
        let direct_response = response.clone();
        if request.delta {
            let evaluation = self.read_deltas.evaluate(
                &response.meta.repository_id,
                &request,
                &response,
                &materialized.current_content,
                materialized.current_tokens,
                self.config.tokenizer,
            )?;
            if evaluation.receipt.outcome == ReadDeltaOutcome::NotModified {
                response.status = ReadStatus::NotModified;
                response.not_modified = true;
                response.content = None;
                response.delta = None;
                response.meta.source_tokens = 0;
                response.meta.emitted_tokens = 0;
            } else if let Some(delta) = evaluation.delta {
                let emitted_tokens = evaluation
                    .receipt
                    .delta_tokens
                    .expect("delta evaluation reports its token count");
                response.status = ReadStatus::Delta;
                response.content = None;
                response.delta = Some(delta);
                response.meta.source_tokens = emitted_tokens;
                response.meta.emitted_tokens = emitted_tokens;
            }
            response.delta_receipt = Some(evaluation.receipt);
            prefer_full_if_delta_payload_not_smaller(
                &mut response,
                &materialized.current_content,
                materialized.current_tokens,
                self.config.tokenizer,
            )?;
        }
        let mut returned_items = usize::from(!response.not_modified);
        if let Some(limit) = options.max_response_tokens() {
            let mut reserved =
                self.finalized_response_tokens_with_receipt_reserve(&response, returned_items)?;
            if reserved > limit && request.delta {
                response = direct_response;
                returned_items = usize::from(!response.not_modified);
                reserved =
                    self.finalized_response_tokens_with_receipt_reserve(&response, returned_items)?;
            }
            if reserved > limit {
                return Err(Error::RequestLimitExceeded {
                    field: "max_response_tokens",
                    requested: reserved,
                    limit,
                });
            }
        }
        let receipt_candidates = if response.not_modified {
            Vec::new()
        } else {
            vec![ReceiptEvidence::new(
                response.path.clone(),
                response.returned_start_line,
                response.returned_end_line,
                response.content_hash.clone(),
                Some(&materialized.current_content),
            )]
        };
        let receipt = self.evaluate_read_receipt(
            request.receipt_id.as_deref(),
            response.meta.repository_generation,
            &receipt_candidates,
        )?;
        if receipt
            .decisions
            .first()
            .is_some_and(|decision| *decision == ReceiptDecision::SuppressExact)
        {
            response.content = None;
            response.delta = None;
            response.status = ReadStatus::ReceiptSuppressed;
            response.not_modified = false;
            response.meta.source_tokens = 0;
            response.meta.emitted_tokens = 0;
            if let Some(delta_receipt) = response.delta_receipt.as_mut() {
                delta_receipt.outcome = ReadDeltaOutcome::ReceiptSuppressed;
                delta_receipt.delta_tokens = Some(0);
                delta_receipt.avoided_tokens = delta_receipt.full_tokens;
                delta_receipt.fallback_reason = None;
            }
        }
        receipt.apply_meta(&mut response.meta);
        self.finalize_bounded_response(&mut response, options)?;
        let expected_hash_not_modified = request.expected_hash.is_some() && response.not_modified;
        self.record_token_savings_with_expected_hash(
            TokenAccountingOperation::Read,
            Some(materialized.baseline_source_tokens),
            &response.meta,
            if response.not_modified {
                TokenSavingsRequestClass::HashSuppressed
            } else if matches!(
                &response.status,
                ReadStatus::Truncated | ReadStatus::ReceiptSuppressed
            ) {
                TokenSavingsRequestClass::Incomplete
            } else {
                TokenSavingsRequestClass::Useful
            },
            expected_hash_not_modified,
            if expected_hash_not_modified {
                materialized.baseline_source_tokens
            } else {
                0
            },
        );
        Ok(response)
    }

    fn read_at_generation_with_options(
        &self,
        session: &ReadSession,
        request: &ReadRequest,
        generation: u64,
        max_tokens: usize,
        options: ServiceCallOptions,
    ) -> Result<MaterializedRead> {
        let materialized = self.read_at_generation(session, request, generation, max_tokens)?;
        let returned_items = usize::from(!materialized.response.not_modified);
        if self.response_fits_with_receipt_reserve(
            &materialized.response,
            returned_items,
            options,
        )? {
            return Ok(materialized);
        }

        let max_response_tokens = options
            .max_response_tokens()
            .expect("fitting only runs with a response limit");
        let budget = ResponseBudget::new(&self.config.tokenizer, max_response_tokens);
        let keep = budget.largest_fitting_prefix(max_tokens, |candidate_limit| {
            let candidate_limit = candidate_limit.max(1);
            let candidate =
                self.read_at_generation(session, request, generation, candidate_limit)?;
            let returned_items = usize::from(!candidate.response.not_modified);
            self.finalized_response_tokens_with_receipt_reserve(&candidate.response, returned_items)
        })?;
        if let Some(candidate_limit) = keep.filter(|keep| *keep > 0) {
            return self.read_at_generation(session, request, generation, candidate_limit);
        }

        let minimum = self.read_at_generation(session, request, generation, 1)?;
        let requested = self.finalized_response_tokens_with_receipt_reserve(
            &minimum.response,
            usize::from(!minimum.response.not_modified),
        )?;
        Err(Error::RequestLimitExceeded {
            field: "max_response_tokens",
            requested,
            limit: max_response_tokens,
        })
    }

    fn read_at_generation(
        &self,
        session: &ReadSession,
        request: &ReadRequest,
        generation: u64,
        max_tokens: usize,
    ) -> Result<MaterializedRead> {
        let indexed = session
            .find_file(&request.path)?
            .ok_or_else(|| Error::NotIndexed(request.path.clone()))?;
        let target = resolve_read_target(session, indexed.id, request, generation)?;

        // Hash the complete live file and extract the bounded target during
        // the same stream. Truncated responses retain one verification pass
        // before issuing a continuation cursor.
        let file = open_live_file(self, &request.path)?;
        let observation = observe_live_range(
            &file,
            target.target_start_line,
            target.target_end_line,
            target.page_start_byte,
            max_tokens,
            self.config.tokenizer,
        )?;
        let snapshot = observation.snapshot;
        let range = observation.range;
        if target
            .expected_full_hash
            .as_deref()
            .is_some_and(|expected| expected != snapshot.content_hash)
        {
            return Err(Error::StaleCursor);
        }
        let target_end_line = target
            .target_end_line
            .unwrap_or(snapshot.end_line)
            .min(snapshot.end_line);
        if target.target_start_line > target_end_line || target.page_start_line > target_end_line {
            return Err(invalid_line_range());
        }
        if range.page_start_line != target.page_start_line {
            return Err(Error::StaleCursor);
        }
        let baseline_source_tokens = self.config.tokenizer.count(&range.content);
        let (content, emitted_tokens) = self.config.tokenizer.truncate(&range.content, max_tokens);
        let returned_start_line = range.page_start_line;
        let returned_end_line = returned_end_line(returned_start_line, content);
        let next_byte = target.page_start_byte.saturating_add(content.len());
        let truncated = next_byte < range.target_bytes;
        let next_start_line = truncated.then(|| {
            if content.ends_with('\n') {
                returned_end_line.saturating_add(1)
            } else {
                returned_end_line
            }
        });
        if truncated {
            let after_read = stream_snapshot(&file)?;
            if after_read.content_hash != snapshot.content_hash
                || after_read.end_line != snapshot.end_line
            {
                return Err(Error::RetryableConflict(
                    crate::error::RetryableOperation::Retrieval,
                ));
            }
        }
        let continuation_cursor = next_start_line.map(|next_start_line| {
            ReadCursor {
                generation,
                target_start_line: target.target_start_line,
                target_end_line,
                next_start_line,
                next_byte,
                full_hash: snapshot.content_hash.clone(),
                path_hash: read_path_hash(&request.path),
            }
            .encode()
        });
        let content_hash = hash(content);
        let index_stale = indexed.content_hash != snapshot.content_hash;
        let indexed_hash = Some(indexed.content_hash);
        let not_modified = request.expected_hash.as_deref() == Some(content_hash.as_str());
        let status = if truncated {
            ReadStatus::Truncated
        } else if not_modified {
            ReadStatus::NotModified
        } else {
            ReadStatus::Content
        };

        Ok(MaterializedRead {
            response: ReadResponse {
                path: request.path.clone(),
                status,
                target_start_line: target.target_start_line,
                target_end_line,
                returned_start_line,
                returned_end_line,
                start_line: returned_start_line,
                end_line: returned_end_line,
                truncated,
                next_start_line,
                continuation_cursor,
                not_modified,
                content: (!not_modified).then(|| content.to_string()),
                delta: None,
                delta_receipt: None,
                content_hash,
                indexed_hash,
                index_stale,
                meta: self.meta(
                    generation,
                    if not_modified { 0 } else { emitted_tokens },
                    None,
                ),
            },
            baseline_source_tokens,
            current_content: content.to_owned(),
            current_tokens: emitted_tokens,
        })
    }
}

fn prefer_full_if_delta_payload_not_smaller(
    response: &mut ReadResponse,
    current_content: &str,
    current_tokens: usize,
    tokenizer: crate::tokens::Tokenizer,
) -> Result<()> {
    if response.status != ReadStatus::Delta {
        return Ok(());
    }
    let delta_tokens = finalized_serialized_read_tokens(response, tokenizer)?;
    let mut full = response.clone();
    full.status = ReadStatus::Content;
    full.content = Some(current_content.to_owned());
    full.delta = None;
    full.meta.source_tokens = current_tokens;
    full.meta.emitted_tokens = current_tokens;
    if let Some(receipt) = full.delta_receipt.as_mut() {
        receipt.outcome = ReadDeltaOutcome::Full;
        receipt.delta_tokens = None;
        receipt.avoided_tokens = 0;
        receipt.fallback_reason = Some(ReadDeltaFallback::DeltaNotSmaller);
    }
    if delta_tokens >= finalized_serialized_read_tokens(&full, tokenizer)? {
        *response = full;
    }
    Ok(())
}

fn finalized_serialized_read_tokens(
    response: &ReadResponse,
    tokenizer: crate::tokens::Tokenizer,
) -> Result<usize> {
    let mut finalized = response.clone();
    finalized.meta.protocol_tokens = 0;
    finalized.meta.path_and_metadata_tokens = 0;
    finalized.meta.total_response_tokens = 0;
    finalized.meta.payload_tokens = 0;
    let accounting = crate::tokens::response_token_accounting(
        &finalized,
        finalized.meta.source_tokens,
        &tokenizer,
    )?;
    finalized.meta.protocol_tokens = accounting.protocol_tokens;
    finalized.meta.path_and_metadata_tokens = accounting.path_and_metadata_tokens;
    finalized.meta.total_response_tokens = accounting.total_response_tokens;
    finalized.meta.payload_tokens = accounting.total_response_tokens;
    Ok(tokenizer.count(&serde_json::to_string(&finalized)?))
}

fn decode_read_cursor(cursor: &str) -> Result<ReadCursor> {
    let fields = cursor.split(':').collect::<Vec<_>>();
    let [
        generation,
        kind,
        target_start,
        target_end,
        next_start,
        next_byte,
        full_hash,
        path_hash,
    ] = fields.as_slice()
    else {
        return Err(Error::StaleCursor);
    };
    if *kind != "read"
        || full_hash.len() != crate::text::CONTENT_FINGERPRINT_HEX_LEN
        || path_hash.len() != 16
        || !full_hash.bytes().all(is_lower_hex)
        || !path_hash.bytes().all(is_lower_hex)
    {
        return Err(Error::StaleCursor);
    }
    let cursor = ReadCursor {
        generation: generation.parse().map_err(|_| Error::StaleCursor)?,
        target_start_line: target_start.parse().map_err(|_| Error::StaleCursor)?,
        target_end_line: target_end.parse().map_err(|_| Error::StaleCursor)?,
        next_start_line: next_start.parse().map_err(|_| Error::StaleCursor)?,
        next_byte: next_byte.parse().map_err(|_| Error::StaleCursor)?,
        full_hash: (*full_hash).into(),
        path_hash: (*path_hash).into(),
    };
    if cursor.target_start_line == 0
        || cursor.target_end_line < cursor.target_start_line
        || cursor.next_start_line < cursor.target_start_line
        || cursor.next_start_line > cursor.target_end_line
        || cursor.next_byte == 0
    {
        return Err(Error::StaleCursor);
    }
    Ok(cursor)
}

fn parse_read_cursor(cursor: &str, generation: u64, path: &str) -> Result<ReadCursor> {
    let cursor = decode_read_cursor(cursor)?;
    if cursor.generation != generation || cursor.path_hash != read_path_hash(path) {
        return Err(Error::StaleCursor);
    }
    Ok(cursor)
}

fn read_path_hash(path: &str) -> String {
    blake3::hash(path.as_bytes()).to_hex()[..16].to_string()
}

fn returned_end_line(start_line: usize, content: &str) -> usize {
    let newline_count = content.bytes().filter(|byte| *byte == b'\n').count();
    start_line
        .saturating_add(newline_count)
        .saturating_sub(usize::from(content.ends_with('\n') && newline_count > 0))
}

pub(super) fn open_live_file(services: &Services, path: &str) -> Result<File> {
    services
        .repository_root
        .open(path)
        .map(cap_std::fs::File::into_std)
        .map_err(|open_error| {
            // The capability open is authoritative for access. Canonicalization is
            // only used after refusal to preserve the public escape classification.
            match resolve_existing(&services.config.root, path) {
                Err(Error::PathOutsideRoot(external)) => Error::PathOutsideRoot(external),
                _ => Error::Io(open_error),
            }
        })
}

fn resolve_read_target(
    session: &ReadSession,
    file_id: i64,
    request: &ReadRequest,
    generation: u64,
) -> Result<ResolvedReadTarget> {
    if let Some(cursor) = request.continuation_cursor.as_deref() {
        let cursor = parse_read_cursor(cursor, generation, &request.path)?;
        return Ok(ResolvedReadTarget {
            target_start_line: cursor.target_start_line,
            target_end_line: Some(cursor.target_end_line),
            page_start_line: cursor.next_start_line,
            page_start_byte: cursor.next_byte,
            expected_full_hash: Some(cursor.full_hash),
        });
    }

    let (target_start_line, target_end_line) = if let Some(symbol_name) = &request.symbol {
        let symbol =
            session
                .find_symbol(file_id, symbol_name)?
                .ok_or_else(|| Error::SymbolNotFound {
                    path: request.path.clone(),
                    symbol: symbol_name.clone(),
                })?;
        (symbol.start_line, Some(symbol.end_line))
    } else if let Some(heading_name) = &request.heading {
        let occurrence = request.heading_occurrence.unwrap_or(1);
        let heading = session
            .find_document_heading(file_id, heading_name, occurrence)?
            .ok_or_else(|| Error::HeadingNotFound {
                path: request.path.clone(),
                heading: heading_name.clone(),
                occurrence,
            })?;
        (heading.start_line, Some(heading.end_line))
    } else {
        (request.start_line.unwrap_or(1), request.end_line)
    };

    if target_start_line == 0
        || target_end_line.is_some_and(|end_line| end_line < target_start_line)
    {
        return Err(invalid_line_range());
    }
    Ok(ResolvedReadTarget {
        target_start_line,
        target_end_line,
        page_start_line: target_start_line,
        page_start_byte: 0,
        expected_full_hash: None,
    })
}

/// Stream a file without loading it into memory and capture its live identity.
fn stream_snapshot(file: &File) -> Result<LiveFileSnapshot> {
    let mut file = file.try_clone()?;
    file.seek(SeekFrom::Start(0))?;
    let mut reader = BufReader::new(file);
    let mut hasher = blake3::Hasher::new();
    let mut buf = [0u8; 65_536];
    let mut bytes_seen = 0usize;
    let mut newline_count = 0usize;
    let mut last_byte_was_newline = false;
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        bytes_seen = bytes_seen.saturating_add(n);
        newline_count =
            newline_count.saturating_add(buf[..n].iter().filter(|byte| **byte == b'\n').count());
        last_byte_was_newline = buf[n - 1] == b'\n';
    }
    let end_line = if bytes_seen == 0 {
        1
    } else {
        newline_count.saturating_add(usize::from(!last_byte_was_newline))
    };
    Ok(LiveFileSnapshot {
        content_hash: hasher.finalize().to_hex()[..crate::text::CONTENT_FINGERPRINT_HEX_LEN]
            .to_string(),
        end_line,
    })
}

/// Hash the live file and read a resolved range in one forward stream.
fn observe_live_range(
    file: &File,
    target_start_line: usize,
    target_end_line: Option<usize>,
    page_start_byte: usize,
    max_tokens: usize,
    tokenizer: crate::tokens::Tokenizer,
) -> Result<LiveReadObservation> {
    let mut file = file.try_clone()?;
    file.seek(SeekFrom::Start(0))?;
    let mut reader = BufReader::new(file);
    let mut hasher = blake3::Hasher::new();
    let mut selected = Vec::with_capacity(LIVE_READ_TOKEN_CHECK_BYTES);
    let mut current_line = 1usize;
    let mut target_finished = false;
    let mut token_bound_reached = false;
    let mut final_target_checked = false;
    let mut target_bytes = 0usize;
    let mut page_start_line = target_start_line;
    let mut next_token_check = LIVE_READ_TOKEN_CHECK_BYTES;
    let mut utf8_pending = Vec::new();
    let mut bytes_seen = 0usize;
    let mut newline_count = 0usize;
    let mut last_byte_was_newline = false;
    let requested_end_line = target_end_line.unwrap_or(usize::MAX);

    loop {
        let buffer = reader.fill_buf()?;
        if buffer.is_empty() {
            break;
        }
        hasher.update(buffer);
        bytes_seen = bytes_seen.saturating_add(buffer.len());
        newline_count =
            newline_count.saturating_add(buffer.iter().filter(|byte| **byte == b'\n').count());
        last_byte_was_newline = buffer.last() == Some(&b'\n');

        let mut validation_chunk = Vec::new();
        if !target_finished {
            for &byte in buffer {
                let in_target =
                    current_line >= target_start_line && current_line <= requested_end_line;
                if in_target {
                    validation_chunk.push(byte);
                    if target_bytes < page_start_byte {
                        if byte == b'\n' {
                            page_start_line = current_line.saturating_add(1);
                        }
                    } else if !token_bound_reached {
                        selected.push(byte);
                    }
                    target_bytes = target_bytes.saturating_add(1);
                }
                if byte == b'\n' {
                    if requested_end_line == current_line {
                        target_finished = true;
                        break;
                    }
                    current_line = current_line.saturating_add(1);
                }
            }
        }
        let consumed = buffer.len();
        reader.consume(consumed);
        validate_utf8_chunk(
            &mut utf8_pending,
            &validation_chunk,
            target_finished || consumed == 0,
        )?;

        if !token_bound_reached
            && ((target_finished && !final_target_checked)
                || selected.len() >= next_token_check
                || selected.len() >= MAX_LIVE_READ_BYTES)
        {
            match std::str::from_utf8(&selected) {
                Ok(content) if tokenizer.count(content) > max_tokens => {
                    token_bound_reached = true;
                    final_target_checked = target_finished;
                }
                Ok(_) => {
                    if selected.len() >= MAX_LIVE_READ_BYTES {
                        return Err(Error::LimitExceeded);
                    }
                    next_token_check = selected.len().saturating_add(LIVE_READ_TOKEN_CHECK_BYTES);
                    final_target_checked = target_finished;
                }
                Err(error) if error.error_len().is_none() => {
                    if target_finished || selected.len() >= MAX_LIVE_READ_BYTES {
                        return Err(Error::InvalidInput {
                            field: "path",
                            reason: "must identify UTF-8 text",
                        });
                    }
                }
                Err(_) => {
                    return Err(Error::InvalidInput {
                        field: "path",
                        reason: "must identify UTF-8 text",
                    });
                }
            }
        }
    }

    validate_utf8_chunk(&mut utf8_pending, &[], true)?;
    if !utf8_pending.is_empty() {
        return Err(Error::InvalidInput {
            field: "path",
            reason: "must identify UTF-8 text",
        });
    }

    if page_start_byte > target_bytes {
        return Err(Error::StaleCursor);
    }
    let content = String::from_utf8(selected).map_err(|_| Error::InvalidInput {
        field: "path",
        reason: "must identify UTF-8 text",
    })?;
    let end_line = if bytes_seen == 0 {
        1
    } else {
        newline_count.saturating_add(usize::from(!last_byte_was_newline))
    };
    Ok(LiveReadObservation {
        snapshot: LiveFileSnapshot {
            content_hash: hasher.finalize().to_hex()[..crate::text::CONTENT_FINGERPRINT_HEX_LEN]
                .to_string(),
            end_line,
        },
        range: LiveReadRange {
            content,
            page_start_line,
            target_bytes,
        },
    })
}

fn validate_utf8_chunk(pending: &mut Vec<u8>, bytes: &[u8], final_chunk: bool) -> Result<()> {
    pending.extend_from_slice(bytes);
    match std::str::from_utf8(pending) {
        Ok(_) => {
            pending.clear();
            Ok(())
        }
        Err(error) if error.error_len().is_none() && !final_chunk => {
            let valid_up_to = error.valid_up_to();
            pending.drain(..valid_up_to);
            Ok(())
        }
        Err(_) => Err(Error::InvalidInput {
            field: "path",
            reason: "must identify UTF-8 text",
        }),
    }
}

fn invalid_line_range() -> Error {
    Error::InvalidInput {
        field: "line range",
        reason: "must be ordered and within the requested file",
    }
}

#[cfg(test)]
impl Services {
    pub(super) fn stored_excerpt(
        &self,
        session: &ReadSession,
        file_id: i64,
        start_line: usize,
        end_line: usize,
        context: usize,
        max_lines: usize,
    ) -> Result<Option<StoredExcerpt>> {
        let request = StoredExcerptRequest {
            file_id,
            desired_start_line: start_line.saturating_sub(context).max(1),
            desired_end_line: end_line.saturating_add(context),
            required_start_line: start_line,
            required_end_line: end_line,
            max_lines,
        };
        Ok(self
            .stored_excerpts(session, &[request])?
            .into_iter()
            .next()
            .flatten())
    }
}

impl Services {
    pub(super) fn stored_excerpts(
        &self,
        session: &ReadSession,
        requests: &[StoredExcerptRequest],
    ) -> Result<Vec<Option<StoredExcerpt>>> {
        let file_ids = requests
            .iter()
            .map(|request| request.file_id)
            .collect::<Vec<_>>();
        let file_end_lines = session.file_end_lines_batch(&file_ids)?;
        let mut unique_indices = HashMap::new();
        let mut unique_requests = Vec::new();
        let mut request_mapping = Vec::new();
        for (index, (request, file_end_line)) in requests.iter().zip(file_end_lines).enumerate() {
            let Some(request) = request.resolve(file_end_line) else {
                continue;
            };
            let unique_index = *unique_indices.entry(request).or_insert_with(|| {
                let unique_index = unique_requests.len();
                unique_requests.push(request);
                unique_index
            });
            request_mapping.push((index, unique_index));
        }
        let ranges = unique_requests
            .iter()
            .map(|request| (request.file_id, request.start_line, request.end_line))
            .collect::<Vec<_>>();
        let chunks = session.get_chunks_overlapping_batch(&ranges)?;
        let hydrated = unique_requests
            .into_iter()
            .zip(chunks)
            .map(|(request, chunks)| assemble_stored_excerpt(request, &chunks))
            .collect::<Vec<_>>();
        let mut excerpts = vec![None; requests.len()];
        for (index, unique_index) in request_mapping {
            excerpts[index] = hydrated[unique_index].clone();
        }
        Ok(excerpts)
    }

    #[cfg(test)]
    pub(super) fn adaptive_context_excerpt(
        &self,
        session: &ReadSession,
        file_id: i64,
        declaration_start: usize,
        declaration_end: usize,
        matched_line: usize,
        token_budget: usize,
    ) -> Result<Option<StoredExcerpt>> {
        let Some(full) =
            self.stored_excerpt(session, file_id, declaration_start, declaration_end, 0, 0)?
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
        self.stored_excerpt(session, file_id, start, end, 0, 0)
    }

    pub(super) fn adaptive_context_excerpts(
        &self,
        session: &ReadSession,
        requests: &[AdaptiveExcerptRequest],
    ) -> Result<Vec<Option<StoredExcerpt>>> {
        let full_requests = requests
            .iter()
            .map(|request| StoredExcerptRequest {
                file_id: request.file_id,
                desired_start_line: request.declaration_start,
                desired_end_line: request.declaration_end,
                required_start_line: request.matched_line,
                required_end_line: request.matched_line,
                max_lines: 0,
            })
            .collect::<Vec<_>>();
        let mut excerpts = self.stored_excerpts(session, &full_requests)?;
        let mut narrowed_indices = Vec::new();
        let mut narrowed_requests = Vec::new();
        for (index, (request, excerpt)) in requests.iter().zip(&excerpts).enumerate() {
            let Some(excerpt) = excerpt else {
                continue;
            };
            let full_tokens = self.config.tokenizer.count(&excerpt.content).max(1);
            if full_tokens <= request.token_budget {
                continue;
            }
            let declaration_lines = request
                .declaration_end
                .saturating_sub(request.declaration_start)
                .saturating_add(1);
            let proportional_lines = declaration_lines
                .saturating_mul(request.token_budget)
                .saturating_div(full_tokens)
                .clamp(MIN_CONTEXT_RANGE_LINES, MAX_CONTEXT_RANGE_LINES)
                .min(declaration_lines);
            let before = proportional_lines / 3;
            let mut start = request
                .matched_line
                .saturating_sub(before)
                .max(request.declaration_start);
            let mut end = start
                .saturating_add(proportional_lines.saturating_sub(1))
                .min(request.declaration_end);
            if end.saturating_sub(start).saturating_add(1) < proportional_lines {
                start = end
                    .saturating_add(1)
                    .saturating_sub(proportional_lines)
                    .max(request.declaration_start);
            }
            end = start
                .saturating_add(proportional_lines.saturating_sub(1))
                .min(request.declaration_end);
            narrowed_indices.push(index);
            narrowed_requests.push(StoredExcerptRequest {
                file_id: request.file_id,
                desired_start_line: start,
                desired_end_line: end,
                required_start_line: request.matched_line,
                required_end_line: request.matched_line,
                max_lines: 0,
            });
        }
        let narrowed = self.stored_excerpts(session, &narrowed_requests)?;
        for (index, excerpt) in narrowed_indices.into_iter().zip(narrowed) {
            excerpts[index] = excerpt;
        }
        Ok(excerpts)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stream_snapshot_hashes_from_the_start_of_a_shared_file_cursor() {
        let mut file = tempfile::tempfile().expect("temporary file");
        std::io::Write::write_all(&mut file, b"first\nsecond\n").expect("write fixture");
        file.seek(SeekFrom::Start(6)).expect("move shared cursor");

        let snapshot = stream_snapshot(&file).expect("snapshot");

        assert_eq!(snapshot.content_hash, hash("first\nsecond\n"));
        assert_eq!(snapshot.end_line, 2);
    }
}
