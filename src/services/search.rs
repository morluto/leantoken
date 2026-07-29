//! Lexical and structural search over a request-scoped snapshot.

use std::collections::{HashMap, HashSet};

use regex_syntax::hir::{
    Hir, HirKind,
    literal::{ExtractKind, Extractor},
};
use tokio_util::sync::CancellationToken;

use super::execution_options::RetrievalExecution;
use super::read::{StoredExcerpt, StoredExcerptRequest};
use super::receipts::{ReceiptDecision, ReceiptEvidence};
use super::validation::{
    MAX_QUERY_BYTES, PathFilter, PathMatcher, check_cancelled, make_cursor, parse_cursor,
    validate_cursor, validate_glob_patterns, validate_input,
};
use super::{ServiceCallOptions, Services, retrieval_primitive_key};
use crate::model::*;
use crate::query_receipt::{
    ExactQueryPredicate, QUERY_RECEIPT_ID_RESPONSE_RESERVE, QueryReceiptRecord,
    exhaustive_result_digest,
};
use crate::storage::{ChunkHit, ReadSession, ReferenceHit, SymbolHit};
use crate::text::{
    anchored_line_window, byte_range_to_line_range, byte_to_line, excerpt, hash, line_starts,
};
use crate::{Error, Result, RetrievalLimitKind};

include!("search/types.rs");
include!("search/regex_plan.rs");
include!("search/hits.rs");
include!("search/projection.rs");
include!("search/validation.rs");

impl Services {
    fn ensure_search_page_fits(
        &self,
        selected: &mut [CandidateSearchHit],
        shape: SearchResponseShape<'_>,
        options: ServiceCallOptions,
    ) -> Result<()> {
        let provisional = |selected: &[CandidateSearchHit]| SearchResponse {
            hits: selected
                .iter()
                .map(|candidate| candidate.hit.clone())
                .collect(),
            coverage: search_coverage(shape.all, selected),
            occurrences_returned: selected.len(),
            occurrences_total: shape
                .request
                .all_occurrences
                .then_some(shape.total_candidates),
            meta: self.meta(
                shape.generation,
                selected
                    .iter()
                    .map(|candidate| self.config.tokenizer.count(&candidate.hit.excerpt))
                    .sum(),
                shape
                    .has_more
                    .then(|| make_cursor(shape.generation, shape.offset + shape.consumed)),
            ),
        };
        let mut sized = provisional(selected);
        if self.response_fits_with_receipt_reserve(&sized, selected.len(), options)? {
            return Ok(());
        }
        for candidate in selected.iter_mut() {
            candidate.hit.score_reasons.clear();
        }
        sized = provisional(selected);
        if self.response_fits_with_receipt_reserve(&sized, selected.len(), options)? {
            return Ok(());
        }
        Err(self.response_budget_error_with_receipt_reserve(
            &sized,
            selected.len(),
            options
                .max_response_tokens()
                .expect("fitting only runs with a response limit"),
        )?)
    }

    async fn apply_search_consistency(
        &self,
        request: &SearchRequest,
        occurrence_groups: bool,
        consistency: Option<IndexConsistency>,
        deadline: Option<tokio::time::Instant>,
        cancellation: &CancellationToken,
    ) -> Result<()> {
        let Some(consistency) = consistency else {
            return Ok(());
        };
        let operation = TokenAccountingOperation::Search;
        if occurrence_groups {
            self.observe_service_result(operation, validate_occurrence_group_input(request))?;
        } else {
            self.observe_service_result(operation, validate_search_input(request))?;
        }
        self.observe_service_result(operation, self.result_limit(request.max_results))?;
        self.observe_service_result(
            operation,
            self.token_limit(request.max_tokens, self.config.default_read_tokens),
        )?;
        self.observe_service_result(operation, self.context_line_limit(request.context_lines))?;
        let consistency_result = self
            .apply_consistency_with_initial_deadline(consistency, cancellation.clone(), deadline)
            .await;
        self.observe_service_result(operation, consistency_result)
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

    /// Search after applying a cancellable index consistency boundary.
    pub async fn search_with_consistency_cancellable(
        &self,
        request: SearchRequest,
        consistency: IndexConsistency,
        cancellation: CancellationToken,
    ) -> Result<SearchResponse> {
        self.search_execute(
            request,
            RetrievalExecution::consistent(consistency, ServiceCallOptions::new(), cancellation),
        )
        .await
    }

    /// Search under consistency and serialized-response controls.
    pub async fn search_with_options_consistency_cancellable(
        &self,
        request: SearchRequest,
        consistency: IndexConsistency,
        options: ServiceCallOptions,
        cancellation: CancellationToken,
    ) -> Result<SearchResponse> {
        self.search_execute(
            request,
            RetrievalExecution::consistent(consistency, options, cancellation),
        )
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
            consistency,
            options,
            cancellation,
        } = execution;
        self.observe_service_result(operation, self.validate_call_options(options))?;
        self.apply_search_consistency(
            &request,
            false,
            consistency,
            options.initial_reconciliation_deadline(),
            &cancellation,
        )
        .await?;
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
                        output_shape: SearchOutputShape::Full,
                        response_options: options,
                        record_savings: true,
                    },
                )
                .map(|snapshot| snapshot.response)
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

    /// Search with grouped output after applying the requested consistency boundary.
    pub async fn search_grouped_with_options_consistency_cancellable(
        &self,
        request: SearchRequest,
        consistency: IndexConsistency,
        options: ServiceCallOptions,
        cancellation: CancellationToken,
    ) -> Result<SearchGroupedResponse> {
        self.search_grouped_execute(
            request,
            RetrievalExecution::consistent(consistency, options, cancellation),
        )
        .await
    }

    async fn search_grouped_execute(
        &self,
        request: SearchRequest,
        execution: RetrievalExecution,
    ) -> Result<SearchGroupedResponse> {
        let operation = TokenAccountingOperation::Search;
        let RetrievalExecution {
            consistency,
            options,
            cancellation,
        } = execution;
        self.observe_service_result(operation, self.validate_call_options(options))?;
        self.apply_search_consistency(
            &request,
            false,
            consistency,
            options.initial_reconciliation_deadline(),
            &cancellation,
        )
        .await?;
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
                            output_shape: SearchOutputShape::Full,
                            response_options: ServiceCallOptions::new(),
                            record_savings: false,
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
                meta.emitted_tokens = source_tokens;
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
        coordinates_only: bool,
    ) -> Result<SearchOccurrencesResponse> {
        self.search_occurrences_with_options(request, coordinates_only, ServiceCallOptions::new())
            .await
    }

    /// Search every lexical occurrence under an exact serialized-response bound.
    pub async fn search_occurrences_with_options(
        &self,
        request: SearchRequest,
        coordinates_only: bool,
        options: ServiceCallOptions,
    ) -> Result<SearchOccurrencesResponse> {
        self.search_occurrences_execute(
            request,
            coordinates_only,
            RetrievalExecution::direct(options, CancellationToken::new()),
        )
        .await
    }

    /// Search grouped occurrences after applying the requested consistency boundary.
    pub async fn search_occurrences_with_options_consistency_cancellable(
        &self,
        request: SearchRequest,
        coordinates_only: bool,
        consistency: IndexConsistency,
        options: ServiceCallOptions,
        cancellation: CancellationToken,
    ) -> Result<SearchOccurrencesResponse> {
        self.search_occurrences_execute(
            request,
            coordinates_only,
            RetrievalExecution::consistent(consistency, options, cancellation),
        )
        .await
    }

    async fn search_occurrences_execute(
        &self,
        request: SearchRequest,
        coordinates_only: bool,
        execution: RetrievalExecution,
    ) -> Result<SearchOccurrencesResponse> {
        let operation = TokenAccountingOperation::Search;
        let RetrievalExecution {
            consistency,
            options,
            cancellation,
        } = execution;
        self.observe_service_result(operation, self.validate_call_options(options))?;
        self.apply_search_consistency(
            &request,
            true,
            consistency,
            options.initial_reconciliation_deadline(),
            &cancellation,
        )
        .await?;
        if consistency.is_none() {
            self.observe_service_result(operation, validate_occurrence_group_input(&request))?;
        }
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
                        output_shape: SearchOutputShape::OccurrenceGroups { coordinates_only },
                        response_options: ServiceCallOptions::new(),
                        record_savings: false,
                    },
                )?;
                let response = snapshot.response;
                let occurrences_total = response.occurrences_total.ok_or_else(|| {
                    Error::InternalFailure(
                        "grouped occurrence search omitted its exact total".into(),
                    )
                })?;
                let groups = group_occurrence_hits(&response.hits, coordinates_only)?;
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
                    coordinates_only,
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
        let this = self.clone();
        self.blocking_executor
            .run(CancellationToken::new(), move |cancellation| {
                let snapshot = this.search_sync(
                    request,
                    cancellation,
                    RegexPlanning::Enabled,
                    SearchDiagnostics::Collect,
                    SearchExecutionOptions {
                        output_shape: SearchOutputShape::Full,
                        response_options: ServiceCallOptions::new(),
                        record_savings: true,
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
    /// bounded legacy scan so tests and benchmarks can prove optimized parity.
    pub async fn search_full_scan_evaluation(
        &self,
        request: SearchRequest,
    ) -> Result<SearchEvaluation> {
        let this = self.clone();
        self.blocking_executor
            .run(CancellationToken::new(), move |cancellation| {
                let snapshot = this.search_sync(
                    request,
                    cancellation,
                    RegexPlanning::Disabled,
                    SearchDiagnostics::Collect,
                    SearchExecutionOptions {
                        output_shape: SearchOutputShape::Full,
                        response_options: ServiceCallOptions::new(),
                        record_savings: true,
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
        request: SearchRequest,
        cancellation: &CancellationToken,
        regex_planning: RegexPlanning,
        diagnostics: SearchDiagnostics,
        execution: SearchExecutionOptions,
    ) -> Result<SearchSnapshotResult> {
        check_cancelled(cancellation)?;
        let prepared = self.prepare_search(&request)?;
        let mut snapshot = self.consistent(|session, generation| {
            self.search_snapshot(
                session,
                generation,
                &request,
                &prepared,
                cancellation,
                regex_planning,
                diagnostics,
                execution,
            )
        })?;
        self.finalize_bounded_response(&mut snapshot.response, execution.response_options)?;
        if execution.record_savings {
            self.record_token_savings(
                TokenAccountingOperation::Search,
                snapshot.baseline_source_tokens,
                &snapshot.response.meta,
            );
        }
        Ok(snapshot)
    }
}

fn recorded_query_receipt_outcome(
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
include!("search/execution.rs");

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
            false,
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
