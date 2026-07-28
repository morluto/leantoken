struct PreparedSearch {
    regex: Option<regex::Regex>,
    literal_regex: Option<regex::Regex>,
    occurrence_literal_regex: Option<regex::Regex>,
    limit: usize,
    token_limit: usize,
    context_lines: usize,
}

struct LexicalSearchBatch {
    hits: Vec<CandidateSearchHit>,
    phases: SearchPhaseCounters,
    primitive_keys: Vec<RetrievalPrimitiveKey>,
}

struct SearchSnapshotResult {
    response: SearchResponse,
    baseline_source_tokens: Option<usize>,
    phases: SearchPhaseCounters,
    primitive_keys: Vec<RetrievalPrimitiveKey>,
}

impl Services {
    fn prepare_search(&self, request: &SearchRequest) -> Result<PreparedSearch> {
        validate_search_input(request)?;
        let regex = matches!(request.mode, SearchMode::Regex)
            .then(|| compile_regex(request))
            .transpose()?;
        let literal_regex = if matches!(request.mode, SearchMode::Regex) {
            None
        } else {
            compile_literal_regex(&request.query, request.case_sensitive)?
        };
        let occurrence_literal_regex =
            (request.all_occurrences && matches!(request.mode, SearchMode::Text))
                .then(|| compile_occurrence_literal_regex(request))
                .transpose()?;
        Ok(PreparedSearch {
            regex,
            literal_regex,
            occurrence_literal_regex,
            limit: self.result_limit(request.max_results)?,
            token_limit: self
                .token_limit(request.max_tokens, self.config.default_read_tokens)?,
            context_lines: self.context_line_limit(request.context_lines)?,
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn search_snapshot(
        &self,
        session: &ReadSession,
        generation: u64,
        request: &SearchRequest,
        prepared: &PreparedSearch,
        cancellation: &CancellationToken,
        regex_planning: RegexPlanning,
        diagnostics: SearchDiagnostics,
        execution: SearchExecutionOptions,
    ) -> Result<SearchSnapshotResult> {
        let offset = parse_cursor(request.cursor.as_deref(), generation)?;
        let mut hits = self.collect_structural_search_hits(
            session,
            request,
            prepared.limit,
            prepared.context_lines,
            cancellation,
        )?;
        let lexical = self.collect_lexical_search_hits(
            session,
            generation,
            request,
            prepared,
            cancellation,
            regex_planning,
            diagnostics,
        )?;
        hits.extend(lexical.hits);
        let hits = order_search_hits(hits, request)?;
        let (response, baseline_source_tokens) = self.build_search_page(
            session,
            generation,
            request,
            prepared,
            execution,
            cancellation,
            hits,
            offset,
        )?;
        Ok(SearchSnapshotResult {
            response,
            baseline_source_tokens,
            phases: lexical.phases,
            primitive_keys: lexical.primitive_keys,
        })
    }

    fn collect_structural_search_hits(
        &self,
        session: &ReadSession,
        request: &SearchRequest,
        limit: usize,
        context_lines: usize,
        cancellation: &CancellationToken,
    ) -> Result<Vec<CandidateSearchHit>> {
        let mut hits = Vec::new();
        if matches!(
            request.mode,
            SearchMode::Auto | SearchMode::Identifier | SearchMode::Symbol
        ) {
            let symbol_hits = collect_filtered_hits(
                request,
                limit.saturating_mul(4),
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
            )?;
            let excerpt_requests = symbol_hits
                .iter()
                .map(|hit| StoredExcerptRequest {
                    file_id: hit.symbol.file_id,
                    desired_start_line: hit
                        .symbol
                        .start_line
                        .saturating_sub(context_lines)
                        .max(1),
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
                        hit: self.symbol_search_hit(hit, &request.query, excerpt),
                        definition: Some(definition),
                    });
                }
            }
        }
        if matches!(
            request.mode,
            SearchMode::Auto | SearchMode::Identifier | SearchMode::Reference
        ) {
            let reference_hits = collect_filtered_hits(
                request,
                limit.saturating_mul(4),
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
            )?;
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
                        hit: self.reference_search_hit(hit, &request.query, excerpt),
                        definition: None,
                    });
                }
            }
        }
        Ok(hits)
    }

    #[allow(clippy::too_many_arguments)]
    fn collect_lexical_search_hits(
        &self,
        session: &ReadSession,
        generation: u64,
        request: &SearchRequest,
        prepared: &PreparedSearch,
        cancellation: &CancellationToken,
        regex_planning: RegexPlanning,
        diagnostics: SearchDiagnostics,
    ) -> Result<LexicalSearchBatch> {
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
                let primitive_kind = match phases.regex_candidate_strategy {
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
                    RegexPlanning::Disabled,
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
            SearchMode::Text | SearchMode::Auto | SearchMode::Identifier => {
                let fetch_page = |offset, page_limit| {
                    if matches!(request.mode, SearchMode::Identifier) {
                        session.search_word_page(&fts_quote(&request.query), page_limit, offset)
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
        let hits = self.hydrate_lexical_search_hits(
            session,
            request,
            prepared,
            cancellation,
            lexical,
        )?;
        Ok(LexicalSearchBatch {
            hits,
            phases,
            primitive_keys,
        })
    }

    fn hydrate_lexical_search_hits(
        &self,
        session: &ReadSession,
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
                    MAX_EXHAUSTIVE_OCCURRENCES.saturating_sub(lexical_hits.len()),
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
                    return Err(Error::LimitExceeded);
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

    #[allow(clippy::too_many_arguments)]
    fn build_search_page(
        &self,
        session: &ReadSession,
        generation: u64,
        request: &SearchRequest,
        prepared: &PreparedSearch,
        execution: SearchExecutionOptions,
        cancellation: &CancellationToken,
        hits: Vec<CandidateSearchHit>,
        offset: usize,
    ) -> Result<(SearchResponse, Option<usize>)> {
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
        Ok((response, baseline_source_tokens))
    }
}

fn order_search_hits(
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
