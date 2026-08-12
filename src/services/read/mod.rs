//! Bounded reads from one immutable indexed generation.

use std::collections::HashMap;

use tokio_util::sync::CancellationToken;

use super::execution_options::RetrievalExecution;
use super::index_read::IndexReadSnapshot;
use super::validation::{
    MAX_PATH_BYTES, MAX_PATTERN_BYTES, check_cancelled, validate_input, validate_optional_input,
};
use super::{ServiceCallOptions, Services};
use crate::model::*;
use crate::repository::{normalize_relative, validate_relative};
use crate::text::{anchored_line_window, excerpt, hash, line_starts};
use crate::tokens::ResponseBudget;
use crate::{Error, Result};

pub(super) const MIN_CONTEXT_RANGE_LINES: usize = 12;
pub(super) const MAX_CONTEXT_RANGE_LINES: usize = 128;

mod excerpts;
mod live;
mod types;

use live::{invalid_line_range, resolve_read_target};
use types::*;
pub(super) use types::{
    AdaptiveExcerptRequest, NewReadTarget, StoredExcerpt, StoredExcerptRequest,
};

fn parse_read_request(mut request: ReadRequest) -> Result<ReadInput> {
    validate_input(&request.path, "path", MAX_PATH_BYTES)?;
    validate_relative(&request.path)?;
    // Bound caller-owned input before normalization so a large whitespace
    // prefix cannot disappear before the byte limit is applied.
    validate_optional_input(request.symbol.as_deref(), "symbol", MAX_PATTERN_BYTES)?;
    if let Some(symbol) = request.symbol.take() {
        let symbol = symbol.trim().to_owned();
        if symbol.is_empty() {
            return Err(Error::InvalidInput {
                field: "symbol",
                reason: "must not be empty",
            });
        }
        request.symbol = Some(symbol);
    }
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
    validate_optional_input(request.expected_hash.as_deref(), "expected hash", 128)?;
    validate_optional_input(
        request.continuation_cursor.as_deref(),
        "continuation cursor",
        2 * 1024,
    )?;
    let mode = parse_read_mode(&request)?;
    request.path = normalize_relative(&request.path)?;
    Ok(ReadInput {
        path: request.path,
        mode,
        max_tokens: request.max_tokens,
        expected_hash: request.expected_hash,
        policy: request.policy,
    })
}

fn parse_read_mode(request: &ReadRequest) -> Result<ReadMode> {
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
    if request.delta {
        return Err(Error::InvalidInput {
            field: "delta",
            reason: "has been removed; clients should send known content hashes",
        });
    }
    if let Some(cursor) = &request.continuation_cursor {
        return Ok(ReadMode::Direct(ReadTargetInput::Continuation(
            cursor.clone(),
        )));
    }
    let target = if let Some(symbol) = &request.symbol {
        NewReadTarget::Symbol(symbol.clone())
    } else if let Some(name) = &request.heading {
        let occurrence = std::num::NonZeroUsize::new(request.heading_occurrence.unwrap_or(1))
            .ok_or_else(|| Error::InvalidInput {
                field: "heading occurrence",
                reason: "must be one-based",
            })?;
        NewReadTarget::Heading {
            name: name.clone(),
            occurrence,
        }
    } else {
        let start = std::num::NonZeroUsize::new(request.start_line.unwrap_or(1))
            .ok_or_else(invalid_line_range)?;
        if request.end_line.is_some_and(|end| end < start.get()) {
            return Err(invalid_line_range());
        }
        NewReadTarget::Lines {
            start,
            end: request.end_line,
        }
    };
    Ok(ReadMode::Direct(ReadTargetInput::New(target)))
}

impl Services {
    pub async fn read(&self, request: ReadRequest) -> Result<ReadResponse> {
        self.read_with_options(request, ServiceCallOptions::new())
            .await
    }

    /// Read source from one immutable indexed generation under explicit
    /// serialized-response controls.
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
        let request = self.observe_service_result(operation, parse_read_request(request))?;
        let RetrievalExecution {
            consistency,
            options,
            cancellation,
        } = execution;
        self.observe_service_result(operation, self.validate_call_options(options))?;
        if let Some(consistency) = consistency {
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
            .process_budget
            .run(cancellation, move |cancellation| {
                this.read_sync(request, options, cancellation)
            })
            .await;
        self.observe_service_result(operation, result)
    }

    fn read_sync(
        &self,
        request: ReadInput,
        options: ServiceCallOptions,
        cancellation: &CancellationToken,
    ) -> Result<ReadResponse> {
        check_cancelled(cancellation)?;
        let max_tokens = self.token_limit(request.max_tokens, self.config.default_read_tokens)?;
        let materialized = self.consistent(|session| {
            let generation = session.generation();
            check_cancelled(cancellation)?;
            self.read_at_generation_with_options(session, &request, generation, max_tokens, options)
        })?;
        let mut response = materialized.response;
        let returned_items = usize::from(!response.not_modified);
        if let Some(limit) = options.max_response_tokens()
            && self.finalized_response_tokens(&response, options)? > limit
        {
            return Err(self.response_budget_error(&response, limit, options)?);
        }
        self.finalize_bounded_response(&mut response, options)?;
        let _ = returned_items;
        Ok(response)
    }

    fn read_at_generation_with_options(
        &self,
        session: &IndexReadSnapshot,
        request: &ReadInput,
        generation: u64,
        max_tokens: usize,
        options: ServiceCallOptions,
    ) -> Result<MaterializedRead> {
        let (mut materialized, minimum_progress_tokens) =
            self.read_at_generation(session, request, generation, max_tokens)?;
        if self.response_fits(&materialized.response, options)? && !materialized.response.truncated
        {
            return Ok(materialized);
        }

        let budget_estimate = self.read_budget_estimate(session, request, generation)?;
        self.apply_read_budget_guidance(&mut materialized, budget_estimate.as_ref(), max_tokens);
        if self.response_fits(&materialized.response, options)? {
            return Ok(materialized);
        }

        let max_response_tokens = options
            .max_response_tokens()
            .expect("fitting only runs with a response limit");
        let budget = ResponseBudget::new(max_response_tokens);
        let additional_tokens = max_tokens.saturating_sub(minimum_progress_tokens);
        let keep = budget.largest_fitting_prefix(additional_tokens, |additional_tokens| {
            let candidate_limit = minimum_progress_tokens.saturating_add(additional_tokens);
            let (mut candidate, _) =
                self.read_at_generation(session, request, generation, candidate_limit)?;
            self.apply_read_budget_guidance(
                &mut candidate,
                budget_estimate.as_ref(),
                candidate_limit,
            );
            self.finalized_response_tokens(&candidate.response, options)
        })?;
        if let Some(additional_tokens) = keep {
            let candidate_limit = minimum_progress_tokens.saturating_add(additional_tokens);
            let (mut materialized, _) =
                self.read_at_generation(session, request, generation, candidate_limit)?;
            self.apply_read_budget_guidance(
                &mut materialized,
                budget_estimate.as_ref(),
                candidate_limit,
            );
            return Ok(materialized);
        }

        let (mut minimum, _) =
            self.read_at_generation(session, request, generation, minimum_progress_tokens)?;
        self.apply_read_budget_guidance(
            &mut minimum,
            budget_estimate.as_ref(),
            minimum_progress_tokens,
        );
        Err(self.response_budget_error(&minimum.response, max_response_tokens, options)?)
    }

    fn read_budget_estimate(
        &self,
        session: &IndexReadSnapshot,
        request: &ReadInput,
        generation: u64,
    ) -> Result<Option<ReadBudgetEstimate>> {
        let Some(indexed) = session.find_file(&request.path)? else {
            return Ok(None);
        };
        let target = resolve_read_target(self, session, indexed.id, request, generation)?;
        let target_end_line = match target.target_end_line {
            Some(end_line) => end_line,
            None => match session
                .file_end_lines_batch(&[indexed.id])?
                .into_iter()
                .next()
            {
                Some(Some(end_line)) => end_line,
                _ => return Ok(None),
            },
        };
        let Some(excerpt) = self.stored_excerpt(
            session,
            indexed.id,
            target.target_start_line,
            target_end_line,
            0,
            0,
        )?
        else {
            return Ok(None);
        };
        Ok(Some(ReadBudgetEstimate {
            target_source_tokens: self.config.tokenizer.count(&excerpt.content),
            indexed_content: excerpt.content,
            page_start_byte: target.page_start_byte,
        }))
    }

    fn apply_read_budget_guidance(
        &self,
        materialized: &mut MaterializedRead,
        estimate: Option<&ReadBudgetEstimate>,
        current_max_tokens: usize,
    ) {
        materialized.response.truncation_guidance = None;
        if !materialized.response.truncated {
            return;
        }
        let Some(estimate) = estimate else {
            return;
        };
        let progress_bytes = estimate
            .page_start_byte
            .saturating_add(materialized.current_content.len());
        let Some(remaining) = estimate.indexed_content.get(progress_bytes..) else {
            return;
        };
        let remaining_source_tokens = self.config.tokenizer.count(remaining);
        if remaining_source_tokens == 0 {
            return;
        }
        materialized.response.truncation_guidance = Some(ReadTruncationGuidance {
            basis: ReadTruncationGuidanceBasis::IndexedGenerationEstimate,
            target_source_tokens: estimate.target_source_tokens,
            remaining_source_tokens,
            remaining_pages_at_current_budget: remaining_source_tokens.div_ceil(current_max_tokens),
            recommended_next_max_tokens: remaining_source_tokens.min(self.config.max_output_tokens),
            minimum_remaining_pages: remaining_source_tokens
                .div_ceil(self.config.max_output_tokens),
        });
    }

    fn read_at_generation(
        &self,
        session: &IndexReadSnapshot,
        request: &ReadInput,
        generation: u64,
        max_tokens: usize,
    ) -> Result<(MaterializedRead, usize)> {
        let indexed = session
            .find_file(&request.path)?
            .ok_or_else(|| Error::NotIndexed(request.path.clone()))?;
        let target = resolve_read_target(self, session, indexed.id, request, generation)?;
        let expected_size = usize::try_from(indexed.size_bytes).map_err(|_| {
            Error::OperationFailure("indexed file size exceeds this platform".into())
        })?;
        let indexed_content = session.file_content(indexed.id, expected_size)?;
        if hash(&indexed_content) != indexed.content_hash {
            return Err(Error::OperationFailure(
                "indexed file content hash does not match its file record".into(),
            ));
        }
        let indexed_end_line = line_starts(&indexed_content).len().max(1);
        let observed_target_end_line = target
            .target_end_line
            .unwrap_or(indexed_end_line)
            .min(indexed_end_line);
        if target.target_start_line > observed_target_end_line
            || target.page_start_line > observed_target_end_line
        {
            return Err(invalid_line_range());
        }
        let target_content = excerpt(
            &indexed_content,
            target.target_start_line,
            observed_target_end_line,
        );
        if target.page_start_byte > target_content.len()
            || !target_content.is_char_boundary(target.page_start_byte)
        {
            return Err(Error::StaleCursor);
        }
        let remaining = &target_content[target.page_start_byte..];
        let (content, emitted_tokens, minimum_progress_tokens) = self
            .config
            .tokenizer
            .truncate_for_read(remaining, max_tokens);
        let minimum_progress_tokens = minimum_progress_tokens.unwrap_or(1);
        let next_byte = target.page_start_byte.saturating_add(content.len());
        let truncated = next_byte < target_content.len();
        // Exact BPE tokens can split a leading UTF-8 scalar. Reject a budget
        // that cannot complete it instead of issuing a cursor at the same byte.
        if truncated && next_byte == target.page_start_byte {
            return Err(Error::InvalidInput {
                field: "max_tokens",
                reason: "must fit at least one UTF-8 scalar",
            });
        }
        let returned_start_line = target.page_start_line;
        let returned_end_line = returned_end_line(returned_start_line, content);
        let next_start_line = truncated.then(|| {
            if content.ends_with('\n') {
                returned_end_line.saturating_add(1)
            } else {
                returned_end_line
            }
        });
        let continuation_cursor = next_start_line
            .map(|next_start_line| {
                self.cursor_codec.seal(
                    generation,
                    &read_request_digest(&request.path, request.policy)?,
                    &ReadPosition {
                        target_start_line: target.target_start_line,
                        target_end_line: target.target_end_line,
                        next_start_line,
                        next_byte,
                    },
                )
            })
            .transpose()?;
        let content_hash = hash(content);
        let not_modified = request.expected_hash.as_deref() == Some(content_hash.as_str());
        let status = if truncated {
            ReadStatus::Truncated
        } else if not_modified {
            ReadStatus::NotModified
        } else {
            ReadStatus::Content
        };

        let materialized = MaterializedRead {
            response: ReadResponse {
                path: request.path.clone(),
                status,
                target_start_line: target.target_start_line,
                target_end_line: observed_target_end_line,
                returned_start_line,
                returned_end_line,
                truncated,
                next_start_line,
                continuation_cursor,
                truncation_guidance: None,
                not_modified,
                content: (!not_modified).then(|| content.to_string()),
                delta: None,
                delta_receipt: None,
                content_hash,
                indexed_hash: Some(indexed.content_hash),
                index_stale: false,
                index_state: ReadIndexState::Current,
                live_bytes_read: 0,
                meta: self.meta(
                    generation,
                    if not_modified { 0 } else { emitted_tokens },
                    None,
                ),
            },
            current_content: content.to_owned(),
        };
        Ok((materialized, minimum_progress_tokens))
    }
}

fn read_request_digest(path: &str, policy: ReadPolicy) -> Result<String> {
    super::cursor::request_digest("read", &(path, policy))
}

fn returned_end_line(start_line: usize, content: &str) -> usize {
    let newline_count = content.bytes().filter(|byte| *byte == b'\n').count();
    start_line
        .saturating_add(newline_count)
        .saturating_sub(usize::from(content.ends_with('\n') && newline_count > 0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raw_symbol_bytes_are_bounded_before_normalization() {
        let request = ReadRequest {
            path: "lib.rs".into(),
            start_line: None,
            end_line: None,
            symbol: Some(format!("{}symbol", " ".repeat(MAX_PATTERN_BYTES + 1))),
            heading: None,
            heading_occurrence: None,
            continuation_cursor: None,
            max_tokens: None,
            expected_hash: None,
            delta: false,
            receipt_id: None,
            policy: ReadPolicy::Bounded,
        };

        assert!(matches!(
            parse_read_request(request),
            Err(Error::InputTooLong {
                field: "symbol",
                max_bytes: MAX_PATTERN_BYTES,
            })
        ));
    }
}
