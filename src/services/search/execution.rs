pub(super) struct PreparedSearch {
    pub(super) regex: Option<regex::Regex>,
    pub(super) literal_regex: Option<regex::Regex>,
    pub(super) occurrence_literal_regex: Option<regex::Regex>,
    pub(super) query_receipt: PreparedQueryReceipt,
    pub(super) limit: usize,
    pub(super) token_limit: usize,
    pub(super) context_lines: usize,
}

pub(super) struct ParsedSearchRequest {
    pub(super) request: SearchRequest,
    pub(super) prepared: PreparedSearch,
}

pub(super) enum PreparedQueryReceipt {
    None,
    Record(ExactQueryPredicate),
    Reuse {
        receipt_id: String,
        predicate: ExactQueryPredicate,
    },
}

pub(super) struct LexicalSearchBatch {
    pub(super) hits: Vec<CandidateSearchHit>,
    pub(super) phases: SearchPhaseCounters,
    pub(super) primitive_keys: Vec<RetrievalPrimitiveKey>,
}

pub(super) struct SearchSnapshotResult {
    pub(super) response: SearchResponse,
    pub(super) baseline_source_tokens: Option<usize>,
    pub(super) phases: SearchPhaseCounters,
    pub(super) primitive_keys: Vec<RetrievalPrimitiveKey>,
    pub(super) query_receipt: QueryReceiptExecution,
}

impl Services {
    pub(super) fn parse_search_request(
        &self,
        request: SearchRequest,
        output_shape: SearchOutputShape,
    ) -> Result<ParsedSearchRequest> {
        let prepared = self.prepare_search(&request, output_shape)?;
        Ok(ParsedSearchRequest { request, prepared })
    }

    pub(super) fn prepare_search(
        &self,
        request: &SearchRequest,
        output_shape: SearchOutputShape,
    ) -> Result<PreparedSearch> {
        validate_search_input(request)?;
        validate_search_output(request, output_shape)?;
        let regex = matches!(request.mode, SearchMode::Regex)
            .then(|| compile_regex(request))
            .transpose()?;
        let literal_regex = if matches!(request.mode, SearchMode::Regex) {
            None
        } else {
            compile_literal_regex(&request.query, request.case_sensitive)?
        };
        let occurrence_literal_regex = (request.all_occurrences
            && matches!(request.mode, SearchMode::Text))
        .then(|| compile_occurrence_literal_regex(request))
        .transpose()?;
        let query_receipt = match &request.query_receipt {
            None => PreparedQueryReceipt::None,
            Some(QueryReceiptAction::Record) => {
                PreparedQueryReceipt::Record(ExactQueryPredicate::from_request(request)?)
            }
            Some(QueryReceiptAction::Reuse { receipt_id }) => PreparedQueryReceipt::Reuse {
                receipt_id: receipt_id.clone(),
                predicate: ExactQueryPredicate::from_request(request)?,
            },
        };
        Ok(PreparedSearch {
            regex,
            literal_regex,
            occurrence_literal_regex,
            query_receipt,
            limit: self.result_limit(request.max_results)?,
            token_limit: self.token_limit(request.max_tokens, self.config.default_read_tokens)?,
            context_lines: self.context_line_limit(request.context_lines)?,
        })
    }

    pub(super) fn search_snapshot(
        &self,
        snapshot: SearchSnapshot<'_>,
        query: SearchQuery<'_>,
        execution: SearchExecutionOptions,
        scan: SearchScan,
    ) -> Result<SearchSnapshotResult> {
        let SearchSnapshot {
            session,
            generation,
            cancellation,
        } = snapshot;
        let SearchQuery { request, prepared } = query;
        if let PreparedQueryReceipt::Reuse {
            receipt_id,
            predicate,
        } = &prepared.query_receipt
        {
            return self.reuse_query_receipt(
                session,
                generation,
                receipt_id,
                predicate,
                cancellation,
            );
        }
        let offset = parse_cursor(request.cursor.as_deref(), generation)?;
        let mut hits = self.collect_structural_search_hits(
            session,
            request,
            prepared,
            prepared.limit,
            prepared.context_lines,
            cancellation,
        )?;
        let snapshot = SearchSnapshot {
            session,
            generation,
            cancellation,
        };
        let query = SearchQuery { request, prepared };
        let lexical = self.collect_lexical_search_hits(snapshot, query, scan)?;
        hits.extend(lexical.hits);
        let hits = order_search_hits(hits, request)?;
        let page = OrderedSearchPage { hits, offset };
        let (response, baseline_source_tokens, query_receipt) =
            self.build_search_page(snapshot, query, execution, page)?;
        Ok(SearchSnapshotResult {
            response,
            baseline_source_tokens,
            phases: lexical.phases,
            primitive_keys: lexical.primitive_keys,
            query_receipt,
        })
    }

    pub(super) fn reuse_query_receipt(
        &self,
        session: &IndexReadSnapshot,
        generation: u64,
        receipt_id: &str,
        requested_predicate: &ExactQueryPredicate,
        cancellation: &CancellationToken,
    ) -> Result<SearchSnapshotResult> {
        check_cancelled(cancellation)?;
        let stored = session.load_query_receipt(receipt_id)?;
        let Some(scope_relation) = requested_predicate.scope_relation_to(&stored.predicate) else {
            return Err(Error::QueryReceiptMismatch);
        };
        if scope_relation == QueryReceiptScopeRelation::Subset && stored.match_count != 0 {
            return Err(Error::QueryReceiptMismatch);
        }
        let current_meta = session.meta()?;
        if current_meta.config_hash != stored.config_hash {
            return Err(Error::StaleQueryReceipt {
                receipt_generation: stored.repository_generation,
                repository_generation: generation,
            });
        }
        let reused_across_generation = stored.repository_generation != generation;
        if reused_across_generation {
            let recorded_filter = PathFilter::new(
                stored.predicate.include_paths(),
                stored.predicate.exclude_paths(),
            )?;
            let current_partition = session.exact_query_partition(
                |path| recorded_filter.allows(path),
                || check_cancelled(cancellation),
            )?;
            if current_partition != stored.partition {
                return Err(Error::StaleQueryReceipt {
                    receipt_generation: stored.repository_generation,
                    repository_generation: generation,
                });
            }
        }
        let requested_predicate_blake3 = requested_predicate.digest()?;
        let outcome = QueryReceiptOutcome {
            status: QueryReceiptStatus::AlreadyCovered,
            receipt_id: Some(stored.receipt_id),
            complete: true,
            match_count: stored.match_count,
            requested_predicate_blake3,
            covered_predicate_blake3: stored.predicate_blake3,
            result_blake3: Some(stored.result_blake3),
            receipt_generation: stored.repository_generation,
            reused_across_generation,
            scope_relation,
        };
        Ok(SearchSnapshotResult {
            response: SearchResponse {
                hits: Vec::new(),
                coverage: SearchCoverage {
                    text_matches: SearchCoverageCount {
                        total: stored.match_count,
                        returned: 0,
                        truncated: stored.match_count,
                    },
                    ..SearchCoverage::default()
                },
                occurrences_returned: 0,
                occurrences_total: Some(stored.match_count),
                meta: self.meta(generation, 0, None),
            },
            baseline_source_tokens: None,
            phases: SearchPhaseCounters::default(),
            primitive_keys: Vec::new(),
            query_receipt: QueryReceiptExecution::Outcome(outcome),
        })
    }

    pub(super) fn collect_structural_search_hits(
        &self,
        session: &IndexReadSnapshot,
        request: &SearchRequest,
        prepared: &PreparedSearch,
        limit: usize,
        context_lines: usize,
        cancellation: &CancellationToken,
    ) -> Result<Vec<CandidateSearchHit>> {
        let mut hits = Vec::new();
        if matches!(
            request.mode,
            SearchMode::Auto | SearchMode::Identifier | SearchMode::Symbol
        ) {
            let max_candidates = limit.saturating_mul(4);
            let unicode_full_scan = !request.case_sensitive
                && crate::symbol_identity::case_fold_literal_variants(&request.query).is_none();
            let symbol_hits = if unicode_full_scan {
                filter_materialized_hits(
                    request,
                    max_candidates,
                    cancellation,
                    session.search_symbols_page(&request.query, false, MAX_FILTER_SCAN_ROWS, 0)?,
                    |hit: &SymbolHit| &hit.path,
                )?
            } else {
                collect_filtered_hits(
                    request,
                    max_candidates,
                    cancellation,
                    |offset, page_limit| {
                        session.search_symbols_page(
                            &request.query,
                            request.case_sensitive,
                            page_limit,
                            offset,
                        )
                    },
                    |hit: &SymbolHit| &hit.path,
                )?
            };
            let excerpt_requests = symbol_hits
                .iter()
                .map(|hit| StoredExcerptRequest {
                    file_id: hit.symbol.file_id,
                    desired_start_line: hit.symbol.start_line.saturating_sub(context_lines).max(1),
                    desired_end_line: hit.symbol.end_line.saturating_add(context_lines),
                    required_start_line: hit.symbol.start_line,
                    required_end_line: hit.symbol.start_line,
                    max_lines: 30,
                })
                .collect::<Vec<_>>();
            for (hit, excerpt) in symbol_hits
                .into_iter()
                .zip(self.stored_excerpts(session, &excerpt_requests)?)
            {
                if let Some(excerpt) = excerpt {
                    let definition = DefinitionIdentity {
                        path: hit.path.clone(),
                        start_line: hit.symbol.start_line,
                        end_line: hit.symbol.end_line,
                    };
                    hits.push(CandidateSearchHit {
                        hit: self.symbol_search_hit(
                            hit,
                            &request.query,
                            request.case_sensitive,
                            prepared.literal_regex.as_ref(),
                            excerpt,
                        ),
                        definition: Some(definition),
                    });
                }
            }
        }
        if matches!(
            request.mode,
            SearchMode::Auto | SearchMode::Identifier | SearchMode::Reference
        ) {
            let max_candidates = limit.saturating_mul(4);
            let unicode_full_scan = !request.case_sensitive
                && crate::symbol_identity::case_fold_literal_variants(&request.query).is_none();
            let reference_hits = if unicode_full_scan {
                filter_materialized_hits(
                    request,
                    max_candidates,
                    cancellation,
                    session.search_references_page(
                        &request.query,
                        false,
                        MAX_FILTER_SCAN_ROWS,
                        0,
                    )?,
                    |hit: &ReferenceHit| &hit.path,
                )?
            } else {
                collect_filtered_hits(
                    request,
                    max_candidates,
                    cancellation,
                    |offset, page_limit| {
                        session.search_references_page(
                            &request.query,
                            request.case_sensitive,
                            page_limit,
                            offset,
                        )
                    },
                    |hit: &ReferenceHit| &hit.path,
                )?
            };
            let excerpt_requests = reference_hits
                .iter()
                .map(|hit| StoredExcerptRequest {
                    file_id: hit.reference.file_id,
                    desired_start_line: hit
                        .reference
                        .start_line
                        .saturating_sub(context_lines)
                        .max(1),
                    desired_end_line: hit.reference.end_line.saturating_add(context_lines),
                    required_start_line: hit.reference.start_line,
                    required_end_line: hit.reference.end_line,
                    max_lines: 12,
                })
                .collect::<Vec<_>>();
            for (hit, excerpt) in reference_hits
                .into_iter()
                .zip(self.stored_excerpts(session, &excerpt_requests)?)
            {
                if let Some(excerpt) = excerpt {
                    hits.push(CandidateSearchHit {
                        hit: self.reference_search_hit(
                            hit,
                            &request.query,
                            request.case_sensitive,
                            prepared.literal_regex.as_ref(),
                            excerpt,
                        ),
                        definition: None,
                    });
                }
            }
        }
        Ok(hits)
    }

    pub(super) fn collect_lexical_search_hits(
        &self,
        snapshot: SearchSnapshot<'_>,
        query: SearchQuery<'_>,
        scan: SearchScan,
    ) -> Result<LexicalSearchBatch> {
        let SearchSnapshot {
            session,
            generation,
            cancellation,
        } = snapshot;
        let SearchQuery { request, prepared } = query;
        let SearchScan {
            regex_planning,
            diagnostics,
        } = scan;
        let mut phases = SearchPhaseCounters::default();
        let mut primitive_keys = Vec::new();
        let lexical = match request.mode {
            SearchMode::Regex => {
                let scan = self.regex_hits(
                    session,
                    request,
                    prepared
                        .regex
                        .as_ref()
                        .expect("regex mode compiles a pattern"),
                    (!request.all_occurrences).then_some(prepared.limit.saturating_mul(20)),
                    cancellation,
                    regex_planning,
                )?;
                phases = scan.phases;
                let primitive_kind = match phases.regex_planning.strategy() {
                    RegexCandidateStrategy::Trigram => "regex_trigram_candidates",
                    RegexCandidateStrategy::FullScan => "regex_full_scan",
                };
                if diagnostics == SearchDiagnostics::Collect {
                    primitive_keys.push(retrieval_primitive_key(
                        generation,
                        primitive_kind,
                        &format!(
                            "case_sensitive:{}:include:{:?}:exclude:{:?}:query:{}",
                            request.case_sensitive,
                            request.include_paths,
                            request.exclude_paths,
                            request.query
                        ),
                    ));
                }
                scan.hits
            }
            SearchMode::Text if request.all_occurrences => {
                let scan = self.regex_hits(
                    session,
                    request,
                    prepared
                        .occurrence_literal_regex
                        .as_ref()
                        .expect("exhaustive text mode compiles a literal pattern"),
                    None,
                    cancellation,
                    regex_planning,
                )?;
                phases = scan.phases;
                scan.hits
            }
            SearchMode::Text | SearchMode::Auto | SearchMode::Identifier
                if request.query.chars().count() < 3 =>
            {
                // Word FTS cannot match substrings shorter than three
                // characters; reuse the literal regex lexical path so
                // Identifier stays aligned with Text/Auto short queries.
                let short_literal_regex = compile_occurrence_literal_regex(request)?;
                let scan = self.regex_hits(
                    session,
                    request,
                    &short_literal_regex,
                    Some(prepared.limit.saturating_mul(20)),
                    cancellation,
                    RegexPlanning::Disabled,
                )?;
                phases = scan.phases;
                scan.hits
            }
            SearchMode::Text | SearchMode::Auto | SearchMode::Identifier
                if !request.case_sensitive
                    && crate::symbol_identity::case_fold_literal_variants(&request.query)
                        .is_none() =>
            {
                let scan = self.regex_hits(
                    session,
                    request,
                    prepared
                        .literal_regex
                        .as_ref()
                        .expect("case-insensitive literal search compiles a matcher"),
                    Some(prepared.limit.saturating_mul(20)),
                    cancellation,
                    RegexPlanning::Disabled,
                )?;
                phases = scan.phases;
                scan.hits
            }
            SearchMode::Text | SearchMode::Auto | SearchMode::Identifier => {
                let folded = (!request.case_sensitive)
                    .then(|| crate::symbol_identity::case_fold_literal_variants(&request.query))
                    .flatten()
                    .filter(|variants| variants.expanded);
                let indexed_query = folded
                    .as_ref()
                    .map(crate::symbol_identity::case_fold_fts_query)
                    .unwrap_or_else(|| fts_quote(&request.query));
                let fetch_page = |offset, page_limit| {
                    if matches!(request.mode, SearchMode::Identifier) {
                        session.search_word_page(&indexed_query, page_limit, offset)
                    } else if folded.is_some() {
                        session.search_trigram_expression_page(&indexed_query, page_limit, offset)
                    } else {
                        session.search_trigram_page(&request.query, page_limit, offset)
                    }
                };
                collect_filtered_hits(
                    request,
                    prepared.limit.saturating_mul(8),
                    cancellation,
                    fetch_page,
                    |hit: &ChunkHit| &hit.path,
                )?
            }
            SearchMode::Symbol | SearchMode::Reference => Vec::new(),
        };
        let hits =
            self.hydrate_lexical_search_hits(session, request, prepared, cancellation, lexical)?;
        Ok(LexicalSearchBatch {
            hits,
            phases,
            primitive_keys,
        })
    }

    pub(super) fn hydrate_lexical_search_hits(
        &self,
        session: &IndexReadSnapshot,
        request: &SearchRequest,
        prepared: &PreparedSearch,
        cancellation: &CancellationToken,
        lexical: Vec<ChunkHit>,
    ) -> Result<Vec<CandidateSearchHit>> {
        let mut lexical_hits = Vec::new();
        for hit in lexical {
            check_cancelled(cancellation)?;
            let chunk_hits = if request.all_occurrences {
                chunk_search_hits(
                    &hit,
                    &request.query,
                    request.case_sensitive,
                    prepared.context_lines,
                    prepared
                        .regex
                        .as_ref()
                        .or(prepared.occurrence_literal_regex.as_ref())
                        .or(prepared.literal_regex.as_ref()),
                    matches!(request.mode, SearchMode::Regex),
                    OccurrenceMaterializationLimit {
                        existing_hits: lexical_hits.len(),
                        max_hits: MAX_EXHAUSTIVE_OCCURRENCES,
                    },
                )?
            } else {
                chunk_search_hit(
                    &hit,
                    &request.query,
                    request.case_sensitive,
                    prepared.context_lines,
                    prepared.regex.as_ref().or(prepared.literal_regex.as_ref()),
                    matches!(request.mode, SearchMode::Regex),
                )?
                .into_iter()
                .collect()
            };
            for search_hit in chunk_hits {
                if request.all_occurrences && lexical_hits.len() == MAX_EXHAUSTIVE_OCCURRENCES {
                    return Err(Error::RetrievalLimitExceeded {
                        kind: RetrievalLimitKind::ExhaustiveOccurrences,
                        observed: lexical_hits.len().saturating_add(1),
                        limit: MAX_EXHAUSTIVE_OCCURRENCES,
                    });
                }
                let matched_line = search_hit
                    .occurrence
                    .as_ref()
                    .map(|occurrence| occurrence.start_line)
                    .or_else(|| {
                        matching_line(
                            &hit,
                            &request.query,
                            request.case_sensitive,
                            prepared.regex.as_ref().or(prepared.literal_regex.as_ref()),
                        )
                    })
                    .unwrap_or(search_hit.start_line);
                lexical_hits.push((hit.file_id, search_hit, matched_line));
            }
        }
        let lexical_locations = lexical_hits
            .iter()
            .map(|(file_id, _, matched_line)| (*file_id, *matched_line))
            .collect::<Vec<_>>();
        let mut enclosing = Vec::with_capacity(lexical_locations.len());
        for locations in lexical_locations.chunks(512) {
            check_cancelled(cancellation)?;
            enclosing.extend(session.find_enclosing_symbols_batch(locations)?);
        }
        Ok(lexical_hits
            .into_iter()
            .zip(enclosing)
            .map(|((_, mut hit, _), symbol)| {
                let definition = symbol.map(|symbol| {
                    hit.enclosing_symbol = Some(symbol.name);
                    DefinitionIdentity {
                        path: hit.path.clone(),
                        start_line: symbol.start_line,
                        end_line: symbol.end_line,
                    }
                });
                CandidateSearchHit { hit, definition }
            })
            .collect())
    }

    pub(super) fn build_search_page(
        &self,
        snapshot: SearchSnapshot<'_>,
        query: SearchQuery<'_>,
        execution: SearchExecutionOptions,
        page: OrderedSearchPage,
    ) -> Result<(SearchResponse, Option<usize>, QueryReceiptExecution)> {
        let SearchSnapshot {
            session,
            generation,
            cancellation,
        } = snapshot;
        let SearchQuery { request, prepared } = query;
        let OrderedSearchPage { hits, offset } = page;
        let total_candidates = hits.len();
        let (mut selected, consumed, _) = select_search_page(
            &hits,
            offset,
            prepared.limit,
            prepared.token_limit,
            execution.output_shape,
            &self.config.tokenizer,
            cancellation,
        )?;
        let has_more = offset.saturating_add(consumed) < total_candidates;
        self.ensure_search_page_fits(
            &mut selected,
            SearchResponseShape {
                all: &hits,
                request,
                generation,
                total_candidates,
                offset,
                consumed,
                has_more,
            },
            execution.response_options,
        )?;
        let receipt_candidates = selected
            .iter()
            .map(|candidate| {
                ReceiptEvidence::new(
                    candidate.hit.path.clone(),
                    candidate.hit.start_line,
                    candidate.hit.end_line,
                    candidate.hit.content_hash.clone(),
                    Some(&candidate.hit.excerpt),
                )
            })
            .collect::<Vec<_>>();
        let receipt = self.evaluate_receipt(
            matches!(execution.output_shape, SearchOutputShape::Full)
                .then_some(request.receipt_id.as_deref())
                .flatten(),
            generation,
            &receipt_candidates,
        )?;
        selected = selected
            .into_iter()
            .zip(&receipt.decisions)
            .filter_map(|(candidate, decision)| {
                matches!(
                    decision,
                    ReceiptDecision::Return | ReceiptDecision::ReturnNearDuplicate
                )
                .then_some(candidate)
            })
            .collect();
        let emitted_tokens = selected_search_source_tokens(
            &selected,
            execution.output_shape,
            &self.config.tokenizer,
        );
        let paths = selected
            .iter()
            .map(|candidate| candidate.hit.path.clone())
            .collect::<HashSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        let occurrences_returned = selected.len();
        let coverage = search_coverage(&hits, &selected);
        let selected = selected
            .into_iter()
            .map(|candidate| candidate.hit)
            .collect();
        let baseline_source_tokens =
            session.whole_file_source_tokens(&paths, self.config.tokenizer.name())?;
        let mut response = SearchResponse {
            hits: selected,
            coverage,
            occurrences_returned,
            occurrences_total: request.all_occurrences.then_some(total_candidates),
            meta: self.meta(
                generation,
                emitted_tokens,
                has_more.then(|| make_cursor(generation, offset + consumed)),
            ),
        };
        receipt.apply_meta(&mut response.meta);
        let query_receipt = if let PreparedQueryReceipt::Record(predicate) = &prepared.query_receipt
        {
            let predicate_blake3 = predicate.digest()?;
            if !has_more && occurrences_returned == total_candidates {
                let path_filter =
                    PathFilter::new(predicate.include_paths(), predicate.exclude_paths())?;
                let partition = session.exact_query_partition(
                    |path| path_filter.allows(path),
                    || check_cancelled(cancellation),
                )?;
                let occurrences = hits
                    .iter()
                    .map(|candidate| {
                        candidate
                            .hit
                            .occurrence
                            .as_ref()
                            .map(|occurrence| (candidate.hit.path.as_str(), occurrence))
                            .ok_or_else(|| {
                                Error::OperationFailure(
                                    "exhaustive query result omitted exact coordinates".into(),
                                )
                            })
                    })
                    .collect::<Result<Vec<_>>>()?;
                QueryReceiptExecution::Pending(QueryReceiptRecord {
                    repository_generation: generation,
                    config_hash: session.meta()?.config_hash,
                    predicate: predicate.clone(),
                    predicate_blake3,
                    partition,
                    match_count: total_candidates,
                    result_blake3: exhaustive_result_digest(occurrences),
                })
            } else {
                QueryReceiptExecution::Outcome(QueryReceiptOutcome {
                    status: QueryReceiptStatus::NotRecordedIncompleteResponse,
                    receipt_id: None,
                    complete: false,
                    match_count: total_candidates,
                    requested_predicate_blake3: predicate_blake3.clone(),
                    covered_predicate_blake3: predicate_blake3,
                    result_blake3: None,
                    receipt_generation: generation,
                    reused_across_generation: false,
                    scope_relation: QueryReceiptScopeRelation::Exact,
                })
            }
        } else {
            QueryReceiptExecution::None
        };
        Ok((response, baseline_source_tokens, query_receipt))
    }
}

pub(super) fn order_search_hits(
    mut hits: Vec<CandidateSearchHit>,
    request: &SearchRequest,
) -> Result<Vec<CandidateSearchHit>> {
    apply_focus(&mut hits, &request.focus_paths)?;
    hits.sort_by(|left, right| {
        right
            .hit
            .score
            .total_cmp(&left.hit.score)
            .then_with(|| left.hit.path.cmp(&right.hit.path))
            .then_with(|| left.hit.start_line.cmp(&right.hit.start_line))
            .then_with(|| {
                left.hit
                    .occurrence
                    .as_ref()
                    .map(|occurrence| occurrence.start_byte)
                    .cmp(
                        &right
                            .hit
                            .occurrence
                            .as_ref()
                            .map(|occurrence| occurrence.start_byte),
                    )
            })
    });
    hits = deduplicate_exact_hits(hits, request.prefer_structural);
    if matches!(request.mode, SearchMode::Auto | SearchMode::Identifier) {
        hits = deduplicate_definition_channels(hits, request.prefer_structural);
    }
    normalize_search_scores(&mut hits);
    Ok(hits)
}
use super::*;
use tokio_util::sync::CancellationToken;

#[derive(Clone, Copy)]
pub(super) struct SearchSnapshot<'a> {
    pub session: &'a IndexReadSnapshot,
    pub generation: u64,
    pub cancellation: &'a CancellationToken,
}

#[derive(Clone, Copy)]
pub(super) struct SearchQuery<'a> {
    pub request: &'a SearchRequest,
    pub prepared: &'a PreparedSearch,
}

#[derive(Clone, Copy)]
pub(super) struct SearchScan {
    pub regex_planning: RegexPlanning,
    pub diagnostics: SearchDiagnostics,
}

pub(super) struct OrderedSearchPage {
    pub hits: Vec<CandidateSearchHit>,
    pub offset: usize,
}
