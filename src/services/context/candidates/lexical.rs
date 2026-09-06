impl Services {
    pub(in crate::services::context) fn append_query_candidates(
        &self,
        expansion: QueryCandidateExpansion<'_>,
        batch: &mut CandidateBatch,
        phases: &mut ContextPhaseTracker,
    ) -> Result<()> {
        let QueryCandidateExpansion {
            session,
            request,
            query,
            path_filter,
            strict_changed_paths,
            changed_paths,
            path_scorer,
            cancellation,
            signals,
        } = expansion;
        let CandidateBatch {
            candidates,
            path_excluded_candidates,
            query_fusion,
            incomplete_scan_warnings: warnings,
            ..
        } = batch;
        let term = &query.value;
        let concept = query.fusion_key.as_str();
        let term_regex = compile_literal_regex(term, false)?;
        check_cancelled(cancellation)?;
        let symbol_results = phases.measure(ContextTimedPhase::SymbolSearch, || {
            context_ranked_hits(
                expansion,
                MAX_CONTEXT_HITS_PER_SOURCE,
                |offset, limit| session.search_symbols_page(term, false, limit, offset),
                |hit: &SymbolHit| &hit.path,
                warnings,
                path_excluded_candidates,
            )
        })?;
        phases.record_primitive("symbol_search", || {
            format!("case_sensitive:false:limit:{MAX_CONTEXT_HITS_PER_SOURCE}:query:{term}")
        });
        phases.counters.symbol_candidates = phases
            .counters
            .symbol_candidates
            .saturating_add(symbol_results.len());
        let symbol_hits = symbol_results.into_iter().enumerate().collect::<Vec<_>>();
        let symbol_excerpt_requests = symbol_hits
            .iter()
            .map(|(_, hit)| AdaptiveExcerptRequest {
                file_id: hit.symbol.file_id,
                declaration_start: hit.symbol.start_line,
                declaration_end: hit.symbol.end_line,
                matched_line: hit.symbol.start_line,
                token_budget: excerpt_budget(request.token_budget, ContextExcerptKind::Symbol),
                selection_budget: request.token_budget,
            })
            .collect::<Vec<_>>();
        phases.record_adaptive_excerpts(&symbol_excerpt_requests);
        let symbol_excerpts = phases.measure(ContextTimedPhase::AdaptiveExcerpt, || {
            self.adaptive_context_excerpts(session, &symbol_excerpt_requests)
        })?;
        for ((rank, hit), excerpt) in symbol_hits.into_iter().zip(symbol_excerpts) {
            check_cancelled(cancellation)?;
            let Some(excerpt) = excerpt else { continue };
            let exact = f64::from(term_regex.as_ref().is_some_and(|matcher| {
                crate::symbol_identity::symbol_identity_matches_case_fold(
                    matcher,
                    &hit.symbol.name,
                    hit.symbol.parent.as_deref(),
                )
            }));
            let qualified = qualified_symbol_match(
                concept,
                &hit.symbol.name,
                hit.symbol.parent.as_deref(),
                hit.symbol.signature.as_deref(),
            );
            if query.fuse {
                record_query_hit(
                    query_fusion,
                    &hit.path,
                    &query.fusion_key,
                    query.weight,
                    rank,
                );
            }
            let change_boost = Self::file_change_boost(
                Some(hit.generation),
                &hit.path,
                changed_paths,
                request.prior_repository_generation,
            );
            let candidate = Candidate::new(
                &hit.path,
                excerpt.start_line,
                excerpt.end_line,
                excerpt.content,
            )
            .match_kind("symbol")
            .concept(concept, query.concept_weight)
            .representation("symbol")
            .symbol_name(hit.symbol.name)
            .exact(exact + qualified * 1.5)
            .symbol(1.0)
            .path_score(path_scorer.score(&hit.path))
            .change_boost(change_boost);
            candidates.push(annotate_candidate(candidate, query, "symbol", rank));
        }
        let reference_results = if signals.caller {
            phases.measure(ContextTimedPhase::ReferenceSearch, || {
                context_ranked_hits(
                    expansion,
                    MAX_CONTEXT_HITS_PER_SOURCE,
                    |offset, limit| session.search_references_page(term, false, limit, offset),
                    |hit: &ReferenceHit| &hit.path,
                    warnings,
                    path_excluded_candidates,
                )
            })?
        } else {
            Vec::new()
        };
        if signals.caller {
            phases.record_primitive("reference_search", || {
                format!("case_sensitive:false:limit:{MAX_CONTEXT_HITS_PER_SOURCE}:query:{term}")
            });
        }
        phases.counters.reference_candidates = phases
            .counters
            .reference_candidates
            .saturating_add(reference_results.len());
        let reference_hits = reference_results
            .into_iter()
            .enumerate()
            .collect::<Vec<_>>();
        let reference_locations = reference_hits
            .iter()
            .map(|(_, hit)| (hit.reference.file_id, hit.reference.start_line))
            .collect::<Vec<_>>();
        phases.record_enclosing_locations(&reference_locations);
        let enclosing = phases.measure(ContextTimedPhase::EnclosingLookup, || {
            session.find_enclosing_symbols_batch(&reference_locations)
        })?;
        let mut adaptive_indices = Vec::new();
        let mut adaptive_requests = Vec::new();
        for (index, ((_, hit), symbol)) in reference_hits.iter().zip(enclosing).enumerate() {
            if let Some(symbol) = symbol {
                adaptive_indices.push(index);
                adaptive_requests.push(AdaptiveExcerptRequest {
                    file_id: hit.reference.file_id,
                    declaration_start: symbol.start_line,
                    declaration_end: symbol.end_line,
                    matched_line: hit.reference.start_line,
                    token_budget: excerpt_budget(
                        request.token_budget,
                        ContextExcerptKind::Reference,
                    ),
                    selection_budget: request.token_budget,
                });
            }
        }
        phases.record_adaptive_excerpts(&adaptive_requests);
        let mut adaptive_excerpts = vec![None; reference_hits.len()];
        let hydrated_adaptive = phases.measure(ContextTimedPhase::AdaptiveExcerpt, || {
            self.adaptive_context_excerpts(session, &adaptive_requests)
        })?;
        for (index, excerpt) in adaptive_indices.into_iter().zip(hydrated_adaptive) {
            adaptive_excerpts[index] = excerpt;
        }
        let mut fallback_indices = Vec::new();
        let mut fallback_requests = Vec::new();
        for (index, ((_, hit), adaptive)) in
            reference_hits.iter().zip(&adaptive_excerpts).enumerate()
        {
            if adaptive.is_some() {
                continue;
            }
            fallback_indices.push(index);
            fallback_requests.push(StoredExcerptRequest {
                file_id: hit.reference.file_id,
                desired_start_line: hit.reference.start_line.saturating_sub(2).max(1),
                desired_end_line: hit.reference.end_line.saturating_add(2),
                required_start_line: hit.reference.start_line,
                required_end_line: hit.reference.end_line,
                max_lines: 12,
            });
        }
        phases.record_stored_excerpts(&fallback_requests);
        let mut fallback_excerpts = vec![None; reference_hits.len()];
        let hydrated_fallback = phases.measure(ContextTimedPhase::StoredExcerpt, || {
            self.stored_excerpts(session, &fallback_requests)
        })?;
        for (index, excerpt) in fallback_indices.into_iter().zip(hydrated_fallback) {
            fallback_excerpts[index] = excerpt;
        }
        for (((rank, hit), adaptive), fallback) in reference_hits
            .into_iter()
            .zip(adaptive_excerpts)
            .zip(fallback_excerpts)
        {
            check_cancelled(cancellation)?;
            let excerpt = adaptive.or_else(|| {
                fallback.and_then(|excerpt| {
                    self.fit_context_excerpt(
                        excerpt,
                        hit.reference.start_line,
                        excerpt_budget(request.token_budget, ContextExcerptKind::Reference),
                        request.token_budget,
                    )
                })
            });
            let Some(excerpt) = excerpt else {
                continue;
            };
            if query.fuse {
                record_query_hit(
                    query_fusion,
                    &hit.path,
                    &query.fusion_key,
                    query.weight,
                    rank,
                );
            }
            let change_boost = Self::file_change_boost(
                Some(hit.generation),
                &hit.path,
                changed_paths,
                request.prior_repository_generation,
            );
            let candidate = Candidate::new(
                &hit.path,
                excerpt.start_line,
                excerpt.end_line,
                excerpt.content,
            )
            .match_kind("reference")
            .concept(concept, query.concept_weight)
            .symbol_name(hit.reference.name)
            .reference(1.0)
            .path_score(path_scorer.score(&hit.path))
            .change_boost(change_boost);
            candidates.push(annotate_candidate(candidate, query, "reference", rank));
        }
        let (lexical, lexical_kind) = phases.measure(ContextTimedPhase::LexicalSearch, || {
            self.context_lexical_hits(
                expansion,
                term_regex
                    .as_ref()
                    .expect("case-insensitive context term compiles a matcher"),
                warnings,
                path_excluded_candidates,
            )
        })?;
        phases.record_primitive(lexical_kind, || {
            format!("limit:{MAX_CONTEXT_LEXICAL_HITS}:query:{term}")
        });
        phases.counters.lexical_candidate_chunks = phases
            .counters
            .lexical_candidate_chunks
            .saturating_add(lexical.len());
        let mut lexical_hits = Vec::new();
        let lexical_verify_started = phases.timer();
        for (rank, hit) in lexical.into_iter().enumerate() {
            check_cancelled(cancellation)?;
            if !path_filter.allows(&hit.path)
                || strict_changed_paths
                    .as_ref()
                    .is_some_and(|paths| !paths.contains(hit.path.as_str()))
            {
                path_excluded_candidates.push(hit.path);
                continue;
            }
            phases.counters.lexical_chunks_verified =
                phases.counters.lexical_chunks_verified.saturating_add(1);
            let Some(facts) = term_regex
                .as_ref()
                .and_then(|matcher| analyze_lexical_match(&hit, matcher, 2))
            else {
                continue;
            };
            phases.counters.lexical_matches = phases.counters.lexical_matches.saturating_add(1);
            lexical_hits.push((rank, hit, facts));
        }
        phases.record_elapsed(ContextTimedPhase::LexicalVerify, lexical_verify_started);
        let lexical_locations = lexical_hits
            .iter()
            .map(|(_, hit, facts)| (hit.file_id, facts.matched_line))
            .collect::<Vec<_>>();
        phases.record_enclosing_locations(&lexical_locations);
        let enclosing = phases.measure(ContextTimedPhase::EnclosingLookup, || {
            session.find_enclosing_symbols_batch(&lexical_locations)
        })?;
        let mut adaptive_indices = Vec::new();
        let mut adaptive_requests = Vec::new();
        for (index, ((_, hit, facts), symbol)) in lexical_hits.iter().zip(enclosing).enumerate() {
            if let Some(symbol) = symbol {
                adaptive_indices.push(index);
                adaptive_requests.push(AdaptiveExcerptRequest {
                    file_id: hit.file_id,
                    declaration_start: symbol.start_line,
                    declaration_end: symbol.end_line,
                    matched_line: facts.matched_line,
                    token_budget: excerpt_budget(request.token_budget, ContextExcerptKind::Text),
                    selection_budget: request.token_budget,
                });
            }
        }
        phases.record_adaptive_excerpts(&adaptive_requests);
        let mut adaptive_excerpts = vec![None; lexical_hits.len()];
        let hydrated_adaptive = phases.measure(ContextTimedPhase::AdaptiveExcerpt, || {
            self.adaptive_context_excerpts(session, &adaptive_requests)
        })?;
        for (index, excerpt) in adaptive_indices.into_iter().zip(hydrated_adaptive) {
            adaptive_excerpts[index] = excerpt;
        }
        for ((rank, hit, facts), adaptive) in lexical_hits.into_iter().zip(adaptive_excerpts) {
            check_cancelled(cancellation)?;
            let excerpt = adaptive.or_else(|| {
                self.fit_context_excerpt(
                    StoredExcerpt {
                        content: facts.search_hit.excerpt.clone(),
                        start_line: facts.search_hit.start_line,
                        end_line: facts.search_hit.end_line,
                    },
                    facts.matched_line,
                    excerpt_budget(request.token_budget, ContextExcerptKind::Text),
                    request.token_budget,
                )
            });
            let Some(excerpt) = excerpt else { continue };
            if query.fuse {
                record_query_hit(
                    query_fusion,
                    &facts.search_hit.path,
                    &query.fusion_key,
                    query.weight,
                    rank,
                );
            }
            let change_boost = Self::file_change_boost(
                Some(hit.generation),
                &facts.search_hit.path,
                changed_paths,
                request.prior_repository_generation,
            );
            let candidate = Candidate::new(
                &facts.search_hit.path,
                excerpt.start_line,
                excerpt.end_line,
                excerpt.content,
            )
            .match_kind("text")
            .concept(concept, query.concept_weight)
            .exact(query.weight)
            .bm25((-hit.score).max(0.0) * 1_000_000.0)
            .path_score(path_scorer.score(&facts.search_hit.path))
            .lexical_frequency_penalty((facts.occurrences.saturating_sub(5) as f64 / 20.0).min(1.0))
            .change_boost(change_boost);
            candidates.push(annotate_candidate(candidate, query, "text", rank));
        }
        Ok(())
    }

    fn context_lexical_hits(
        &self,
        expansion: QueryCandidateExpansion<'_>,
        term_regex: &regex::Regex,
        warnings: &mut Vec<String>,
        path_excluded_candidates: &mut Vec<String>,
    ) -> Result<(Vec<ChunkHit>, &'static str)> {
        let QueryCandidateExpansion {
            session,
            request,
            query,
            cancellation,
            ..
        } = expansion;
        let term = &query.value;
        let folded = crate::symbol_identity::case_fold_literal_variants(term);
        let folded_query = folded
            .as_ref()
            .filter(|variants| variants.expanded)
            .map(crate::symbol_identity::case_fold_fts_query);
        if folded.is_none() {
            let scan = self.full_scan_literal_hits(LiteralFullScan {
                session,
                query: term,
                matcher: term_regex,
                include_paths: &request.include_paths,
                exclude_paths: &request.exclude_paths,
                max_candidates: MAX_CONTEXT_LEXICAL_HITS,
                allows_path: &|path| {
                    expansion
                        .strict_changed_paths
                        .is_none_or(|paths| paths.contains(path))
                },
                cancellation,
            })?;
            if let Some(error) = scan.limitation {
                record_candidate_scan_limit(warnings, error)?;
            }
            return Ok((scan.hits, "unicode_case_fold_full_scan"));
        }
        if term.chars().count() < 3 {
            let query = folded_query.unwrap_or_else(|| fts_quote(term));
            return Ok((
                context_ranked_hits(
                    expansion,
                    MAX_CONTEXT_LEXICAL_HITS,
                    |offset, limit| session.search_word_page(&query, limit, offset),
                    |hit: &ChunkHit| &hit.path,
                    warnings,
                    path_excluded_candidates,
                )?,
                if folded.is_some_and(|variants| variants.expanded) {
                    "unicode_case_fold_word"
                } else {
                    "word"
                },
            ));
        }
        if let Some(expression) = folded_query {
            return Ok((
                context_ranked_hits(
                    expansion,
                    MAX_CONTEXT_LEXICAL_HITS,
                    |offset, limit| {
                        session.search_trigram_expression_page(&expression, limit, offset)
                    },
                    |hit: &ChunkHit| &hit.path,
                    warnings,
                    path_excluded_candidates,
                )?,
                "unicode_case_fold_trigram",
            ));
        }
        Ok((
            context_ranked_hits(
                expansion,
                MAX_CONTEXT_LEXICAL_HITS,
                |offset, limit| session.search_trigram_page(term, limit, offset),
                |hit: &ChunkHit| &hit.path,
                warnings,
                path_excluded_candidates,
            )?,
            "trigram",
        ))
    }
}
use super::*;

fn context_ranked_hits<T>(
    expansion: QueryCandidateExpansion<'_>,
    limit: usize,
    mut fetch: impl FnMut(usize, usize) -> Result<Vec<T>>,
    path: impl Fn(&T) -> &str,
    warnings: &mut Vec<String>,
    path_excluded_candidates: &mut Vec<String>,
) -> Result<Vec<T>> {
    // Structural Unicode fallbacks verify a bounded materialized set. Do not
    // repeat that whole scan for each page discarded by the scope filter.
    let mut materialized =
        if crate::symbol_identity::case_fold_literal_variants(&expansion.query.value).is_none() {
            match fetch(0, crate::services::candidate_scan::MAX_FILTER_SCAN_ROWS) {
                Ok(hits) => Some(hits.into_iter()),
                Err(error) => {
                    record_candidate_scan_limit(warnings, error)?;
                    return Ok(Vec::new());
                }
            }
        } else {
            None
        };
    let mut recorded_exclusions = 0usize;
    let result = crate::services::candidate_scan::collect_ranked_candidates(
        limit,
        expansion.cancellation,
        |offset, page_limit| match &mut materialized {
            Some(hits) => Ok(hits.by_ref().take(page_limit).collect()),
            None => fetch(offset, page_limit),
        },
        |hit| {
            let path = path(hit);
            let allowed = expansion.path_filter.allows(path)
                && expansion
                    .strict_changed_paths
                    .is_none_or(|paths| paths.contains(path));
            // Omission diagnostics retain the old per-source bound even when
            // scope filtering scans further to fill the candidate slots.
            if !allowed && recorded_exclusions < limit {
                path_excluded_candidates.push(path.to_owned());
                recorded_exclusions += 1;
            }
            allowed
        },
    )?;
    if result.scan_limited {
        let warning = format!(
            "candidate generation incomplete: ranked scan reached {} rows; narrow include_paths",
            crate::services::candidate_scan::MAX_FILTER_SCAN_ROWS
        );
        if !warnings.contains(&warning) {
            warnings.push(warning);
        }
    }
    Ok(result.hits)
}
