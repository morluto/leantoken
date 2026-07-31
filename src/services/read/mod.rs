//! Bounded live reads and index-backed excerpts.

use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};

use tokio_util::sync::CancellationToken;

use super::execution_options::RetrievalExecution;
use super::read_delta::ReadDeltaInput;
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

pub(super) const MIN_CONTEXT_RANGE_LINES: usize = 12;
pub(super) const MAX_CONTEXT_RANGE_LINES: usize = 128;

mod cursor;
mod excerpts;
mod live;
mod types;

use cursor::*;
pub(super) use live::open_live_file;
use live::*;
use types::*;
pub(super) use types::{AdaptiveExcerptRequest, StoredExcerpt, StoredExcerptRequest};

pub(super) fn validate_read_input(request: &ReadRequest) -> Result<()> {
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
                .apply_consistency_with_initial_deadline(
                    consistency,
                    cancellation.clone(),
                    options.initial_reconciliation_deadline(),
                )
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

    pub(super) fn read_sync(
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
            let evaluation = self.read_deltas.evaluate(ReadDeltaInput {
                repository_id: &response.meta.repository_id,
                storage: &self.storage,
                request: &request,
                response: &response,
                current_content: &materialized.current_content,
                full_tokens: materialized.current_tokens,
                tokenizer: self.config.tokenizer,
            })?;
            if evaluation.receipt.outcome == ReadDeltaOutcome::NotModified {
                response.status = ReadStatus::NotModified;
                response.not_modified = true;
                response.content = None;
                response.delta = None;
                response.meta.source_tokens = 0;
            } else if let Some(delta) = evaluation.delta {
                let emitted_tokens = evaluation
                    .receipt
                    .delta_tokens
                    .expect("delta evaluation reports its token count");
                response.status = ReadStatus::Delta;
                response.content = None;
                response.delta = Some(delta);
                response.meta.source_tokens = emitted_tokens;
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
                return Err(self.response_budget_error_with_receipt_reserve(
                    &response,
                    returned_items,
                    limit,
                )?);
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
            response.meta.source_tokens = 0;
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
        Err(self.response_budget_error_with_receipt_reserve(
            &minimum.response,
            usize::from(!minimum.response.not_modified),
            max_response_tokens,
        )?)
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

pub(super) fn prefer_full_if_delta_payload_not_smaller(
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
    full.meta.source_tokens = current_tokens;
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

pub(super) fn finalized_serialized_read_tokens(
    response: &ReadResponse,
    tokenizer: crate::tokens::Tokenizer,
) -> Result<usize> {
    let mut finalized = response.clone();
    finalized.meta.protocol_tokens = 0;
    finalized.meta.path_and_metadata_tokens = 0;
    finalized.meta.total_response_tokens = 0;
    finalized.meta.total_response_tokens = 0;
    let accounting = crate::tokens::response_token_accounting(
        &finalized,
        finalized.meta.source_tokens,
        &tokenizer,
    )?;
    finalized.meta.protocol_tokens = accounting.protocol_tokens;
    finalized.meta.path_and_metadata_tokens = accounting.path_and_metadata_tokens;
    finalized.meta.total_response_tokens = accounting.total_response_tokens;
    finalized.meta.total_response_tokens = accounting.total_response_tokens;
    Ok(tokenizer.count(&serde_json::to_string(&finalized)?))
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
