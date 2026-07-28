impl Services {
    fn append_query_candidates(
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
            ..
        } = batch;
        let term = &query.value;
        let concept = query.fusion_key.as_str();
        check_cancelled(cancellation)?;
        let symbol_results = phases.measure(ContextTimedPhase::SymbolSearch, || {
            session.search_symbols(term, false, MAX_CONTEXT_HITS_PER_SOURCE)
        })?;
        phases.record_primitive("symbol_search", || {
            format!("case_sensitive:false:limit:{MAX_CONTEXT_HITS_PER_SOURCE}:query:{term}")
        });
        phases.counters.symbol_candidates = phases
            .counters
            .symbol_candidates
            .saturating_add(symbol_results.len());
        let mut symbol_hits = Vec::new();
        for (rank, hit) in symbol_results.into_iter().enumerate() {
            check_cancelled(cancellation)?;
            if path_filter.allows(&hit.path)
                && strict_changed_paths
                    .as_ref()
                    .is_none_or(|paths| paths.contains(hit.path.as_str()))
            {
                symbol_hits.push((rank, hit));
            } else {
                path_excluded_candidates.push(hit.path);
            }
        }
        let symbol_excerpt_requests = symbol_hits
            .iter()
            .map(|(_, hit)| AdaptiveExcerptRequest {
                file_id: hit.symbol.file_id,
                declaration_start: hit.symbol.start_line,
                declaration_end: hit.symbol.end_line,
                matched_line: hit.symbol.start_line,
                token_budget: excerpt_budget(request.token_budget, ContextExcerptKind::Symbol),
            })
            .collect::<Vec<_>>();
        phases.record_adaptive_excerpts(&symbol_excerpt_requests);
        let symbol_excerpts = phases.measure(ContextTimedPhase::AdaptiveExcerpt, || {
            self.adaptive_context_excerpts(session, &symbol_excerpt_requests)
        })?;
        for ((rank, hit), excerpt) in symbol_hits.into_iter().zip(symbol_excerpts) {
            check_cancelled(cancellation)?;
            let Some(excerpt) = excerpt else { continue };
            let exact = f64::from(hit.symbol.name.eq_ignore_ascii_case(term));
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
                &changed_paths,
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
                session.search_references(term, false, MAX_CONTEXT_HITS_PER_SOURCE)
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
        let mut reference_hits = Vec::new();
        for (rank, hit) in reference_results.into_iter().enumerate() {
            check_cancelled(cancellation)?;
            if path_filter.allows(&hit.path)
                && strict_changed_paths
                    .as_ref()
                    .is_none_or(|paths| paths.contains(hit.path.as_str()))
            {
                reference_hits.push((rank, hit));
            } else {
                path_excluded_candidates.push(hit.path);
            }
        }
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
            let excerpt = adaptive.or(fallback);
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
                &changed_paths,
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
        let term_regex = compile_literal_regex(term, false)?;
        let lexical = phases.measure(ContextTimedPhase::LexicalSearch, || {
            if term.chars().count() >= 3 {
                session.search_trigram(term, MAX_CONTEXT_LEXICAL_HITS)
            } else {
                session.search_word(&fts_quote(term), MAX_CONTEXT_LEXICAL_HITS)
            }
        })?;
        let lexical_kind = if term.chars().count() >= 3 {
            "trigram"
        } else {
            "word"
        };
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
            let excerpt = adaptive.unwrap_or(StoredExcerpt {
                content: facts.search_hit.excerpt.clone(),
                start_line: facts.search_hit.start_line,
                end_line: facts.search_hit.end_line,
            });
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
                &changed_paths,
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

}
