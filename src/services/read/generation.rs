//! Canonical reads from one atomically published repository generation.

use super::*;
use crate::services::cursor::request_digest;
use crate::text::{excerpt, line_starts};

#[derive(serde::Serialize)]
struct PublishedReadCursorRequest<'a> {
    path: &'a str,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct PublishedReadPosition {
    #[serde(rename = "a")]
    target_start_line: usize,
    #[serde(rename = "b")]
    target_end_line: Option<usize>,
    #[serde(rename = "c")]
    next_start_line: usize,
    #[serde(rename = "d")]
    next_byte: usize,
}

impl Services {
    /// Read source from one published repository generation.
    pub async fn read(&self, request: ReadRequest) -> Result<ReadResponse> {
        self.read_with_options(request, ServiceCallOptions::new())
            .await
    }

    /// Read published source under explicit serialized-response controls.
    pub async fn read_with_options(
        &self,
        request: ReadRequest,
        options: ServiceCallOptions,
    ) -> Result<ReadResponse> {
        self.read_generation_execute(
            request,
            RetrievalExecution::direct(options, CancellationToken::new()),
        )
        .await
    }

    /// Read published source under response controls and cancellation.
    pub async fn read_with_options_cancellable(
        &self,
        request: ReadRequest,
        options: ServiceCallOptions,
        cancellation: CancellationToken,
    ) -> Result<ReadResponse> {
        self.read_generation_execute(request, RetrievalExecution::direct(options, cancellation))
            .await
    }

    pub async fn read_cancellable(
        &self,
        request: ReadRequest,
        cancellation: CancellationToken,
    ) -> Result<ReadResponse> {
        self.read_generation_execute(
            request,
            RetrievalExecution::direct(ServiceCallOptions::new(), cancellation),
        )
        .await
    }

    async fn read_generation_execute(
        &self,
        request: ReadRequest,
        execution: RetrievalExecution,
    ) -> Result<ReadResponse> {
        let operation = TokenAccountingOperation::Read;
        let request = self.observe_service_result(operation, parse_read_request(request))?;
        let RetrievalExecution {
            consistency: _,
            options,
            cancellation,
        } = execution;
        self.observe_service_result(operation, self.validate_call_options(options))?;
        let this = self.clone();
        let result = self
            .blocking_executor
            .run(cancellation, move |cancellation| {
                this.read_generation_sync(request, options, cancellation)
            })
            .await;
        self.observe_service_result(operation, result)
    }

    fn read_generation_sync(
        &self,
        request: ReadInput,
        options: ServiceCallOptions,
        cancellation: &CancellationToken,
    ) -> Result<ReadResponse> {
        check_cancelled(cancellation)?;
        let max_tokens = self.token_limit(request.max_tokens, self.config.default_read_tokens)?;
        let (mut response, baseline_source_tokens) = self.consistent(|generation| {
            check_cancelled(cancellation)?;
            let baseline_source_tokens = self
                .published_read_budget_estimate(generation, &request)?
                .map(|estimate| estimate.target_source_tokens);
            let response =
                self.read_published_with_options(generation, &request, max_tokens, options)?;
            Ok((response, baseline_source_tokens))
        })?;
        self.finalize_bounded_response(&mut response, options)?;
        let expected_hash_not_modified = request.expected_hash.is_some() && response.not_modified;
        self.record_token_savings_with_expected_hash(
            TokenAccountingOperation::Read,
            baseline_source_tokens,
            &response.meta,
            if response.not_modified {
                TokenSavingsRequestClass::HashSuppressed
            } else if response.truncated {
                TokenSavingsRequestClass::Incomplete
            } else {
                TokenSavingsRequestClass::Useful
            },
            expected_hash_not_modified,
            if expected_hash_not_modified {
                baseline_source_tokens.unwrap_or(0)
            } else {
                0
            },
        );
        Ok(response)
    }

    fn read_published_with_options(
        &self,
        generation: &RepositoryGeneration,
        request: &ReadInput,
        max_tokens: usize,
        options: ServiceCallOptions,
    ) -> Result<ReadResponse> {
        let (mut response, minimum_progress_tokens) =
            self.read_published(generation, request, max_tokens)?;
        if self.response_fits(&response, options)? && !response.truncated {
            return Ok(response);
        }

        self.apply_published_read_guidance(generation, request, &mut response, max_tokens)?;
        if self.response_fits(&response, options)? {
            return Ok(response);
        }

        let max_response_tokens = options
            .max_response_tokens()
            .expect("fitting only runs with a response limit");
        let budget = ResponseBudget::new(max_response_tokens);
        let additional_tokens = max_tokens.saturating_sub(minimum_progress_tokens);
        let keep = budget.largest_fitting_prefix(additional_tokens, |additional_tokens| {
            let candidate_limit = minimum_progress_tokens.saturating_add(additional_tokens);
            let (mut candidate, _) = self.read_published(generation, request, candidate_limit)?;
            self.apply_published_read_guidance(
                generation,
                request,
                &mut candidate,
                candidate_limit,
            )?;
            self.finalized_response_tokens(&candidate, options)
        })?;
        if let Some(additional_tokens) = keep {
            let candidate_limit = minimum_progress_tokens.saturating_add(additional_tokens);
            let (mut candidate, _) = self.read_published(generation, request, candidate_limit)?;
            self.apply_published_read_guidance(
                generation,
                request,
                &mut candidate,
                candidate_limit,
            )?;
            return Ok(candidate);
        }

        let (mut minimum, _) = self.read_published(generation, request, minimum_progress_tokens)?;
        self.apply_published_read_guidance(
            generation,
            request,
            &mut minimum,
            minimum_progress_tokens,
        )?;
        Err(self.response_budget_error(&minimum, max_response_tokens, options)?)
    }

    fn apply_published_read_guidance(
        &self,
        generation: &RepositoryGeneration,
        request: &ReadInput,
        response: &mut ReadResponse,
        current_max_tokens: usize,
    ) -> Result<()> {
        response.truncation_guidance = None;
        if !response.truncated {
            return Ok(());
        }
        let Some(estimate) = self.published_read_budget_estimate(generation, request)? else {
            return Ok(());
        };
        let progress_bytes = estimate
            .page_start_byte
            .saturating_add(response.content.as_deref().map_or(0, str::len));
        let Some(remaining) = estimate.indexed_content.get(progress_bytes..) else {
            return Ok(());
        };
        let remaining_source_tokens = self.config.tokenizer.count(remaining);
        if remaining_source_tokens == 0 {
            return Ok(());
        }
        response.truncation_guidance = Some(ReadTruncationGuidance {
            basis: ReadTruncationGuidanceBasis::PublishedGeneration,
            target_source_tokens: estimate.target_source_tokens,
            remaining_source_tokens,
            remaining_pages_at_current_budget: remaining_source_tokens.div_ceil(current_max_tokens),
            recommended_next_max_tokens: remaining_source_tokens.min(self.config.max_output_tokens),
            minimum_remaining_pages: remaining_source_tokens
                .div_ceil(self.config.max_output_tokens),
        });
        Ok(())
    }

    fn published_read_budget_estimate(
        &self,
        generation: &RepositoryGeneration,
        request: &ReadInput,
    ) -> Result<Option<ReadBudgetEstimate>> {
        let Some(indexed) = generation.find_file(&request.path)? else {
            return Ok(None);
        };
        let target = resolve_published_read_target(generation, indexed.id, request)?;
        let expected_size = usize::try_from(indexed.size_bytes).map_err(|_| {
            Error::OperationFailure("indexed file size exceeds this platform".into())
        })?;
        let content = generation.file_content(indexed.id, expected_size)?;
        let end_line = target
            .target_end_line
            .unwrap_or_else(|| line_starts(&content).len().max(1));
        let indexed_content = excerpt(&content, target.target_start_line, end_line);
        Ok(Some(ReadBudgetEstimate {
            target_source_tokens: self.config.tokenizer.count(&indexed_content),
            indexed_content: indexed_content.to_owned(),
            page_start_byte: target.page_start_byte,
        }))
    }

    fn read_published(
        &self,
        generation: &RepositoryGeneration,
        request: &ReadInput,
        max_tokens: usize,
    ) -> Result<(ReadResponse, usize)> {
        let generation_id = generation.generation();
        let indexed = generation
            .find_file(&request.path)?
            .ok_or_else(|| Error::NotIndexed(request.path.clone()))?;
        let target = resolve_published_read_target(generation, indexed.id, request)?;
        let expected_size = usize::try_from(indexed.size_bytes).map_err(|_| {
            Error::OperationFailure("indexed file size exceeds this platform".into())
        })?;
        let indexed_content = generation.file_content(indexed.id, expected_size)?;
        if hash(&indexed_content) != indexed.content_hash {
            return Err(Error::OperationFailure(
                "indexed file content hash does not match its file record".into(),
            ));
        }
        if target
            .expected_file_size
            .is_some_and(|size| size != expected_size)
            || target
                .expected_full_hash
                .as_deref()
                .is_some_and(|expected| expected != indexed.content_hash)
        {
            return Err(Error::StaleCursor);
        }

        let indexed_end_line = line_starts(&indexed_content).len().max(1);
        let target_end_line = target
            .target_end_line
            .unwrap_or(indexed_end_line)
            .min(indexed_end_line);
        if target.target_start_line > target_end_line || target.page_start_line > target_end_line {
            return Err(invalid_line_range());
        }
        let target_content = excerpt(&indexed_content, target.target_start_line, target_end_line);
        if target.page_start_byte > target_content.len()
            || !target_content.is_char_boundary(target.page_start_byte)
        {
            return Err(Error::StaleCursor);
        }
        if let Some(expected_prefix) = target.expected_prefix_hash.as_deref()
            && hash(&target_content[..target.page_start_byte]) != expected_prefix
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
        let cursor_request = request_digest(&PublishedReadCursorRequest {
            path: &request.path,
        })?;
        let continuation_cursor = next_start_line
            .map(|next_start_line| {
                generation.seal_cursor(
                    "read",
                    &cursor_request,
                    PublishedReadPosition {
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
        Ok((
            ReadResponse {
                path: request.path.clone(),
                status,
                source: ReadSource::PublishedGeneration,
                target_start_line: target.target_start_line,
                target_end_line,
                returned_start_line,
                returned_end_line,
                truncated,
                next_start_line,
                continuation_cursor,
                truncation_guidance: None,
                not_modified,
                content: (!not_modified).then(|| content.to_owned()),
                delta: None,
                delta_receipt: None,
                content_hash,
                indexed_hash: Some(indexed.content_hash),
                index_stale: false,
                index_state: ReadIndexState::Unknown,
                live_bytes_read: 0,
                meta: self.meta(
                    generation_id,
                    if not_modified { 0 } else { emitted_tokens },
                    None,
                ),
            },
            minimum_progress_tokens,
        ))
    }
}

fn resolve_published_read_target(
    generation: &RepositoryGeneration,
    file_id: i64,
    request: &ReadInput,
) -> Result<ResolvedReadTarget> {
    if let ReadMode::Direct(ReadTargetInput::Continuation(token)) = &request.mode {
        let request_digest = request_digest(&PublishedReadCursorRequest {
            path: &request.path,
        })?;
        let position: PublishedReadPosition =
            generation.open_cursor(token, "read", &request_digest)?;
        if position.target_start_line == 0
            || position.next_start_line < position.target_start_line
            || position.next_byte == 0
            || position.target_end_line.is_some_and(|end| {
                end < position.target_start_line || position.next_start_line > end
            })
        {
            return Err(Error::StaleCursor);
        }
        return Ok(ResolvedReadTarget {
            target_start_line: position.target_start_line,
            target_end_line: position.target_end_line,
            page_start_line: position.next_start_line,
            page_start_byte: position.next_byte,
            expected_full_hash: None,
            expected_prefix_hash: None,
            expected_file_size: None,
            expected_modified_ns: None,
            policy: request.policy,
        });
    }
    resolve_read_target(generation, file_id, request)
}
