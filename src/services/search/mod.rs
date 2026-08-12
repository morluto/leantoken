//! Lexical and structural search over a request-scoped snapshot.

use std::collections::{HashMap, HashSet};

use regex_syntax::hir::{
    Hir, HirKind,
    literal::{ExtractKind, Extractor},
};
use tokio_util::sync::CancellationToken;

use super::cursor::request_digest;
use super::execution_options::RetrievalExecution;
use super::index_read::{ChunkHit, ReferenceHit, RepositoryGeneration, SymbolHit};
use super::read::{StoredExcerpt, StoredExcerptRequest};
use super::receipts::{ReceiptDecision, ReceiptEvidence};
use super::validation::{
    MAX_CURSOR_BYTES, MAX_QUERY_BYTES, PathFilter, PathMatcher, check_cancelled,
    validate_glob_patterns, validate_input, validate_optional_input,
};
use super::{ServiceCallOptions, Services, retrieval_primitive_key};
use crate::model::*;
use crate::query_receipt::{
    ExactQueryPredicate, QUERY_RECEIPT_ID_RESPONSE_RESERVE, QueryReceiptRecord,
    exhaustive_result_digest,
};
use crate::text::{
    anchored_line_window, byte_range_to_line_range, byte_to_line, excerpt, hash, line_starts,
};
use crate::{Error, RegexWorkDimension, Result, RetrievalLimitKind};

mod hits;
mod projection;
mod regex_plan;
mod types;
mod validation;

use execution::*;
use hits::*;
pub(super) use hits::{OccurrenceMetadata, chunk_search_hit_for_range, fts_quote};
use projection::*;
use regex_plan::*;
pub(super) use regex_plan::{LiteralFullScan, compile_literal_regex};
use types::*;
pub(super) use types::{
    LexicalMatchKind, MAX_REGEX_CANDIDATE_CHUNKS, MAX_REGEX_CANDIDATES, MAX_REGEX_CHUNKS_PER_FILE,
    MAX_REGEX_FILES_SCANNED, MAX_SCOPED_REGEX_ROWS_SCANNED,
};
use validation::*;

impl Services {
    fn ensure_search_page_fits(
        &self,
        selected: &mut [CandidateSearchHit],
        shape: SearchResponseShape<'_>,
        options: ServiceCallOptions,
    ) -> Result<()> {
        let provisional = |selected: &[CandidateSearchHit]| -> Result<SearchResponse> {
            Ok(SearchResponse {
                hits: selected
                    .iter()
                    .map(|candidate| candidate.hit.clone())
                    .collect(),
                coverage: search_coverage(shape.all, selected),
                occurrences_returned: selected.len(),
                occurrences_total: shape
                    .request
                    .kind
                    .is_exhaustive()
                    .then_some(shape.total_candidates),
                meta: self.meta(
                    shape.generation.generation(),
                    selected
                        .iter()
                        .map(|candidate| self.config.tokenizer.count(&candidate.hit.excerpt))
                        .sum(),
                    shape
                        .has_more
                        .then(|| {
                            shape.generation.seal_cursor(
                                "search",
                                shape.cursor_digest,
                                SearchPosition {
                                    offset: shape.offset + shape.consumed,
                                },
                            )
                        })
                        .transpose()?,
                ),
            })
        };
        let mut sized = provisional(selected)?;
        if self.response_fits_with_receipt_reserve(&sized, selected.len(), options)? {
            return Ok(());
        }
        for candidate in selected.iter_mut() {
            candidate.hit.score_reasons.clear();
        }
        sized = provisional(selected)?;
        if self.response_fits_with_receipt_reserve(&sized, selected.len(), options)? {
            return Ok(());
        }
        Err(self.response_budget_error_with_receipt_reserve(
            &sized,
            selected.len(),
            options
                .max_response_tokens()
                .expect("fitting only runs with a response limit"),
            options,
        )?)
    }

    /// Search indexed lexical and structural evidence.
    pub async fn search(&self, request: SearchRequest) -> Result<SearchResponse> {
        self.search_with_options(request, ServiceCallOptions::new())
            .await
    }

    /// Search under explicit serialized-response controls.
    pub async fn search_with_options(
        &self,
        request: SearchRequest,
        options: ServiceCallOptions,
    ) -> Result<SearchResponse> {
        self.search_execute(
            request,
            RetrievalExecution::direct(options, CancellationToken::new()),
        )
        .await
    }

    pub async fn search_with_options_cancellable(
        &self,
        request: SearchRequest,
        options: ServiceCallOptions,
        cancellation: CancellationToken,
    ) -> Result<SearchResponse> {
        self.search_execute(request, RetrievalExecution::direct(options, cancellation))
            .await
    }

    pub async fn search_cancellable(
        &self,
        request: SearchRequest,
        cancellation: CancellationToken,
    ) -> Result<SearchResponse> {
        self.search_execute(
            request,
            RetrievalExecution::direct(ServiceCallOptions::new(), cancellation),
        )
        .await
    }

    async fn search_execute(
        &self,
        request: SearchRequest,
        execution: RetrievalExecution,
    ) -> Result<SearchResponse> {
        let operation = TokenAccountingOperation::Search;
        let RetrievalExecution {
            consistency: _,
            options,
            cancellation,
        } = execution;
        let options = options.with_receipt_resource_reserve();
        self.observe_service_result(operation, self.validate_call_options(options))?;
        let output_shape = SearchOutputShape::Full;
        let request = self
            .observe_service_result(operation, self.parse_search_request(request, output_shape))?;
        let this = self.clone();
        let result = self
            .blocking_executor
            .run(cancellation, move |cancellation| {
                this.search_sync(
                    request,
                    cancellation,
                    RegexPlanning::Enabled,
                    SearchDiagnostics::Omit,
                    SearchExecutionOptions {
                        response_options: options,
                        accounting: SearchAccounting::Record,
                    },
                )
                .map(|snapshot| snapshot.response)
            })
            .await;
        self.observe_service_result(operation, result)
    }

    /// Search with source-free ranked hits.
    pub async fn search_compact(&self, request: SearchRequest) -> Result<SearchCompactResponse> {
        self.search_compact_with_options(request, ServiceCallOptions::new())
            .await
    }

    /// Search with source-free ranked hits under an exact serialized-response bound.
    pub async fn search_compact_with_options(
        &self,
        request: SearchRequest,
        options: ServiceCallOptions,
    ) -> Result<SearchCompactResponse> {
        self.search_compact_execute(
            request,
            RetrievalExecution::direct(options, CancellationToken::new()),
        )
        .await
    }

    pub async fn search_compact_with_options_cancellable(
        &self,
        request: SearchRequest,
        options: ServiceCallOptions,
        cancellation: CancellationToken,
    ) -> Result<SearchCompactResponse> {
        self.search_compact_execute(request, RetrievalExecution::direct(options, cancellation))
            .await
    }

    async fn search_compact_execute(
        &self,
        request: SearchRequest,
        execution: RetrievalExecution,
    ) -> Result<SearchCompactResponse> {
        let operation = TokenAccountingOperation::Search;
        let RetrievalExecution {
            consistency: _,
            options,
            cancellation,
        } = execution;
        let options = options.with_receipt_resource_reserve();
        self.observe_service_result(operation, self.validate_call_options(options))?;
        let output_shape = SearchOutputShape::Compact;
        let request = self
            .observe_service_result(operation, self.parse_search_request(request, output_shape))?;
        let this = self.clone();
        let result = self
            .blocking_executor
            .run(cancellation, move |cancellation| {
                let response = this
                    .search_sync(
                        request,
                        cancellation,
                        RegexPlanning::Enabled,
                        SearchDiagnostics::Omit,
                        SearchExecutionOptions {
                            response_options: ServiceCallOptions::new(),
                            accounting: SearchAccounting::Omit,
                        },
                    )?
                    .response;
                let hits = response
                    .hits
                    .iter()
                    .map(compact_search_hit)
                    .collect::<Vec<_>>();
                let mut compact = SearchCompactResponse {
                    hits_returned: hits.len(),
                    hits,
                    coverage: response.coverage,
                    occurrences_total: response.occurrences_total,
                    meta: response.meta,
                };
                this.finalize_bounded_response(&mut compact, options)?;
                this.record_token_savings(TokenAccountingOperation::Search, None, &compact.meta);
                Ok(compact)
            })
            .await;
        self.observe_service_result(operation, result)
    }

    /// Search with hits grouped by matched symbol or enclosing scope.
    pub async fn search_grouped(&self, request: SearchRequest) -> Result<SearchGroupedResponse> {
        self.search_grouped_with_options(request, ServiceCallOptions::new())
            .await
    }

    /// Search with grouped output under an exact serialized-response bound.
    pub async fn search_grouped_with_options(
        &self,
        request: SearchRequest,
        options: ServiceCallOptions,
    ) -> Result<SearchGroupedResponse> {
        self.search_grouped_execute(
            request,
            RetrievalExecution::direct(options, CancellationToken::new()),
        )
        .await
    }

    pub async fn search_grouped_with_options_cancellable(
        &self,
        request: SearchRequest,
        options: ServiceCallOptions,
        cancellation: CancellationToken,
    ) -> Result<SearchGroupedResponse> {
        self.search_grouped_execute(request, RetrievalExecution::direct(options, cancellation))
            .await
    }

    async fn search_grouped_execute(
        &self,
        request: SearchRequest,
        execution: RetrievalExecution,
    ) -> Result<SearchGroupedResponse> {
        let operation = TokenAccountingOperation::Search;
        let RetrievalExecution {
            consistency: _,
            options,
            cancellation,
        } = execution;
        let options = options.with_receipt_resource_reserve();
        self.observe_service_result(operation, self.validate_call_options(options))?;
        let output_shape = SearchOutputShape::Full;
        let request = self
            .observe_service_result(operation, self.parse_search_request(request, output_shape))?;
        let this = self.clone();
        let result = self
            .blocking_executor
            .run(cancellation, move |cancellation| {
                let response = this
                    .search_sync(
                        request,
                        cancellation,
                        RegexPlanning::Enabled,
                        SearchDiagnostics::Omit,
                        SearchExecutionOptions {
                            response_options: ServiceCallOptions::new(),
                            accounting: SearchAccounting::Omit,
                        },
                    )?
                    .response;
                let hits_returned = response.hits.len();
                let groups = group_search_hits(&response.hits);
                let source_tokens = groups
                    .iter()
                    .filter_map(|group| group.definition.as_ref().or(group.representative.as_ref()))
                    .filter_map(|evidence| evidence.excerpt.as_deref())
                    .map(|excerpt| this.config.tokenizer.count(excerpt))
                    .sum();
                let mut meta = response.meta;
                meta.source_tokens = source_tokens;
                let mut compact = SearchGroupedResponse {
                    groups_returned: groups.len(),
                    groups,
                    coverage: response.coverage,
                    hits_returned,
                    occurrences_total: response.occurrences_total,
                    meta,
                };
                this.finalize_bounded_response(&mut compact, options)?;
                this.record_token_savings(TokenAccountingOperation::Search, None, &compact.meta);
                Ok(compact)
            })
            .await;
        self.observe_service_result(operation, result)
    }

    /// Search every lexical occurrence while sharing repeated excerpts.
    pub async fn search_occurrences(
        &self,
        request: SearchRequest,
        output: SearchOccurrenceOutput,
    ) -> Result<SearchOccurrencesResponse> {
        self.search_occurrences_with_options(request, output, ServiceCallOptions::new())
            .await
    }

    /// Search every lexical occurrence under an exact serialized-response bound.
    pub async fn search_occurrences_with_options(
        &self,
        request: SearchRequest,
        output: SearchOccurrenceOutput,
        options: ServiceCallOptions,
    ) -> Result<SearchOccurrencesResponse> {
        self.search_occurrences_execute(
            request,
            output,
            RetrievalExecution::direct(options, CancellationToken::new()),
        )
        .await
    }

    pub async fn search_occurrences_with_options_cancellable(
        &self,
        request: SearchRequest,
        output: SearchOccurrenceOutput,
        options: ServiceCallOptions,
        cancellation: CancellationToken,
    ) -> Result<SearchOccurrencesResponse> {
        self.search_occurrences_execute(
            request,
            output,
            RetrievalExecution::direct(options, cancellation),
        )
        .await
    }

    async fn search_occurrences_execute(
        &self,
        request: SearchRequest,
        output: SearchOccurrenceOutput,
        execution: RetrievalExecution,
    ) -> Result<SearchOccurrencesResponse> {
        let operation = TokenAccountingOperation::Search;
        let RetrievalExecution {
            consistency: _,
            options,
            cancellation,
        } = execution;
        let options = options.with_receipt_resource_reserve();
        self.observe_service_result(operation, self.validate_call_options(options))?;
        let output_shape = SearchOutputShape::OccurrenceGroups(output);
        let request = self
            .observe_service_result(operation, self.parse_search_request(request, output_shape))?;
        let this = self.clone();
        let result = self
            .blocking_executor
            .run(cancellation, move |cancellation| {
                let snapshot = this.search_sync(
                    request,
                    cancellation,
                    RegexPlanning::Enabled,
                    SearchDiagnostics::Omit,
                    SearchExecutionOptions {
                        response_options: ServiceCallOptions::new(),
                        accounting: SearchAccounting::Omit,
                    },
                )?;
                let response = snapshot.response;
                let occurrences_total = response.occurrences_total.ok_or_else(|| {
                    Error::OperationFailure(
                        "grouped occurrence search omitted its exact total".into(),
                    )
                })?;
                let groups = group_occurrence_hits(&response.hits, output)?;
                let query_receipt = match &snapshot.query_receipt {
                    QueryReceiptExecution::None => None,
                    QueryReceiptExecution::Pending(record) => Some(recorded_query_receipt_outcome(
                        record,
                        QUERY_RECEIPT_ID_RESPONSE_RESERVE.to_owned(),
                    )),
                    QueryReceiptExecution::Outcome(outcome) => Some(outcome.clone()),
                };
                let mut compact = SearchOccurrencesResponse {
                    groups_returned: groups.len(),
                    groups,
                    occurrences_returned: response.occurrences_returned,
                    occurrences_total,
                    coordinates_only: output.coordinates_only(),
                    coverage: response.coverage,
                    query_receipt,
                    meta: response.meta,
                };
                this.finalize_bounded_response(&mut compact, options)?;
                if let QueryReceiptExecution::Pending(record) = snapshot.query_receipt {
                    check_cancelled(cancellation)?;
                    let receipt_id = this.storage.persist_query_receipt(&record)?;
                    compact.query_receipt =
                        Some(recorded_query_receipt_outcome(&record, receipt_id));
                    this.finalize_bounded_response(&mut compact, options)?;
                }
                this.record_token_savings(TokenAccountingOperation::Search, None, &compact.meta);
                Ok(compact)
            })
            .await;
        self.observe_service_result(operation, result)
    }

    /// Search and expose deterministic candidate-phase counts for evaluation.
    ///
    /// Production adapters should use [`Self::search`]. This method does not
    /// alter the normal response or MCP schemas.
    pub async fn search_evaluation(&self, request: SearchRequest) -> Result<SearchEvaluation> {
        let output_shape = SearchOutputShape::Full;
        let request = self.parse_search_request(request, output_shape)?;
        let this = self.clone();
        self.blocking_executor
            .run(CancellationToken::new(), move |cancellation| {
                let snapshot = this.search_sync(
                    request,
                    cancellation,
                    RegexPlanning::Enabled,
                    SearchDiagnostics::Collect,
                    SearchExecutionOptions {
                        response_options: ServiceCallOptions::new(),
                        accounting: SearchAccounting::Record,
                    },
                )?;
                Ok(SearchEvaluation {
                    response: snapshot.response,
                    phases: snapshot.phases,
                    primitive_keys: snapshot.primitive_keys,
                })
            })
            .await
    }

    /// Search with regex candidate planning disabled for differential evaluation.
    ///
    /// This API is not exposed through CLI or MCP adapters. It retains the
    /// bounded reference scan so tests and benchmarks can prove optimized parity.
    pub async fn search_full_scan_evaluation(
        &self,
        request: SearchRequest,
    ) -> Result<SearchEvaluation> {
        let output_shape = SearchOutputShape::Full;
        let request = self.parse_search_request(request, output_shape)?;
        let this = self.clone();
        self.blocking_executor
            .run(CancellationToken::new(), move |cancellation| {
                let snapshot = this.search_sync(
                    request,
                    cancellation,
                    RegexPlanning::Disabled,
                    SearchDiagnostics::Collect,
                    SearchExecutionOptions {
                        response_options: ServiceCallOptions::new(),
                        accounting: SearchAccounting::Record,
                    },
                )?;
                Ok(SearchEvaluation {
                    response: snapshot.response,
                    phases: snapshot.phases,
                    primitive_keys: snapshot.primitive_keys,
                })
            })
            .await
    }

    fn search_sync(
        &self,
        parsed: ParsedSearchRequest,
        cancellation: &CancellationToken,
        regex_planning: RegexPlanning,
        diagnostics: SearchDiagnostics,
        execution: SearchExecutionOptions,
    ) -> Result<SearchSnapshotResult> {
        check_cancelled(cancellation)?;
        let ParsedSearchRequest {
            request,
            prepared,
            output_shape,
        } = parsed;
        let mut snapshot = self.consistent(|session| {
            let generation = session.generation();
            self.search_snapshot(
                execution::SearchSnapshot {
                    session,
                    generation,
                    cancellation,
                },
                execution::SearchQuery {
                    request: &request,
                    prepared: &prepared,
                },
                output_shape,
                execution,
                execution::SearchScan {
                    regex_planning,
                    diagnostics,
                },
            )
        })?;
        self.finalize_bounded_response(&mut snapshot.response, execution.response_options)?;
        if execution.accounting == SearchAccounting::Record {
            self.record_token_savings(
                TokenAccountingOperation::Search,
                snapshot.baseline_source_tokens,
                &snapshot.response.meta,
            );
        }
        Ok(snapshot)
    }
}

pub(super) fn recorded_query_receipt_outcome(
    record: &QueryReceiptRecord,
    receipt_id: String,
) -> QueryReceiptOutcome {
    QueryReceiptOutcome {
        status: QueryReceiptStatus::Recorded,
        receipt_id: Some(receipt_id),
        complete: true,
        match_count: record.match_count,
        requested_predicate_blake3: record.predicate_blake3.clone(),
        covered_predicate_blake3: record.predicate_blake3.clone(),
        result_blake3: Some(record.result_blake3.clone()),
        receipt_generation: record.repository_generation,
        reused_across_generation: false,
        scope_relation: QueryReceiptScopeRelation::Exact,
    }
}

// Keep the public search owner focused on request routing while the synchronous
// retrieval stages remain private to this module.
mod execution;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exhaustive_chunk_hits_fail_before_exceeding_the_materialization_limit() {
        let hit = ChunkHit {
            chunk_id: 1,
            file_id: 1,
            path: "src/lib.rs".into(),
            content: "key key key".into(),
            start_line: 1,
            end_line: 1,
            start_byte: 0,
            end_byte: 11,
            token_count: 3,
            generation: 1,
            score: 0.0,
        };

        let error = chunk_search_hits(
            &hit,
            "key",
            true,
            0,
            None,
            LexicalMatchKind::Text,
            OccurrenceMaterializationLimit {
                existing_hits: 5,
                max_hits: 7,
            },
        )
        .expect_err("third occurrence exceeds the materialization limit");

        assert!(matches!(
            error,
            Error::RetrievalLimitExceeded {
                kind: RetrievalLimitKind::ExhaustiveOccurrences,
                observed: 8,
                limit: 7,
            }
        ));
    }
}
