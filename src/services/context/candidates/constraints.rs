impl Services {
    pub(in crate::services::context) fn append_constraint_candidates(
        &self,
        expansion: ConstraintCandidateExpansion<'_>,
        candidates: &mut Vec<Candidate>,
        phases: &mut ContextPhaseTracker,
    ) -> Result<ContextConstraintExpansion> {
        let ConstraintCandidateExpansion {
            session,
            request,
            queries: _,
            path_scorer: _,
            cancellation,
        } = expansion;
        let mut coverage = ContextCoverageReceipt::default();
        let mut focus_path_matches = vec![0usize; request.focus_paths.len()];
        let mut focus_path_files = vec![Vec::new(); request.focus_paths.len()];
        let mut focus_path_eligible = vec![0usize; request.focus_paths.len()];
        let mut include_path_matches = vec![false; request.include_paths.len()];
        let mut required_path_matches = vec![false; request.must_include_paths.len()];
        let mut required_path_files = vec![None::<FileRecord>; request.must_include_paths.len()];
        let mut evidence_path_matches = vec![0usize; request.required_evidence.len()];
        let mut evidence_path_files = vec![Vec::new(); request.required_evidence.len()];
        let focus_matchers = request
            .focus_paths
            .iter()
            .map(|pattern| PathMatcher::new(std::slice::from_ref(pattern)))
            .collect::<Result<Vec<_>>>()?;
        let include_matchers = request
            .include_paths
            .iter()
            .map(|pattern| PathMatcher::new(std::slice::from_ref(pattern)))
            .collect::<Result<Vec<_>>>()?;
        let required_matchers = request
            .must_include_paths
            .iter()
            .map(|pattern| PathMatcher::new(std::slice::from_ref(pattern)))
            .collect::<Result<Vec<_>>>()?;
        let evidence_matchers = request
            .required_evidence
            .iter()
            .map(|requirement| PathMatcher::new(std::slice::from_ref(&requirement.path)))
            .collect::<Result<Vec<_>>>()?;
        let path_filter = PathFilter::new(&request.include_paths, &request.exclude_paths)?;
        let context_exclude_paths = PathMatcher::new_lossy(&self.config.context_exclude_paths);
        let strict_changed_paths = request.strict_changed_paths.then(|| {
            request
                .changed_paths
                .iter()
                .map(String::as_str)
                .collect::<HashSet<_>>()
        });

        if !request.focus_paths.is_empty()
            || !request.include_paths.is_empty()
            || !request.must_include_paths.is_empty()
            || !request.required_evidence.is_empty()
        {
            let mut cursor = None;
            loop {
                check_cancelled(cancellation)?;
                let page = session.list_files(512, cursor)?;
                let Some(last) = page.last() else {
                    break;
                };
                cursor = Some(last.id);
                for file in page {
                    let focus_eligible = if focus_matchers.is_empty() {
                        false
                    } else {
                        let explicitly_included = !request.include_paths.is_empty()
                            && include_matchers
                                .iter()
                                .any(|matcher| matcher.is_match(&file.path));
                        path_filter.allows(&file.path)
                            && strict_changed_paths
                                .as_ref()
                                .is_none_or(|paths| paths.contains(file.path.as_str()))
                            && (!context_exclude_paths.is_match(&file.path) || explicitly_included)
                    };
                    for (index, matcher) in focus_matchers.iter().enumerate() {
                        if matcher.is_match(&file.path) {
                            focus_path_matches[index] = focus_path_matches[index].saturating_add(1);
                            if focus_eligible {
                                focus_path_eligible[index] =
                                    focus_path_eligible[index].saturating_add(1);
                                retain_focus_file(&mut focus_path_files[index], &file);
                            }
                        }
                    }
                    for (index, matcher) in include_matchers.iter().enumerate() {
                        include_path_matches[index] |= matcher.is_match(&file.path);
                    }
                    for (index, matcher) in required_matchers.iter().enumerate() {
                        if !matcher.is_match(&file.path) {
                            continue;
                        }
                        required_path_matches[index] = true;
                        if required_path_files[index].is_none() && path_filter.allows(&file.path) {
                            required_path_files[index] = Some(file.clone());
                        }
                    }
                    retain_required_evidence_files(
                        &file,
                        &evidence_matchers,
                        &path_filter,
                        strict_changed_paths.as_ref(),
                        &mut evidence_path_matches,
                        &mut evidence_path_files,
                    );
                }
            }
        }

        coverage.unmatched_focus_paths = request
            .focus_paths
            .iter()
            .zip(&focus_path_matches)
            .filter(|(_, matched)| **matched == 0)
            .map(|(pattern, _)| pattern.clone())
            .collect();
        let minimum_focus_fragments = request.minimum_fragments_per_focus_path.unwrap_or(1);
        if !request.focus_paths.is_empty() {
            coverage.focus_path_coverage = request
                .focus_paths
                .iter()
                .zip(&focus_path_matches)
                .zip(&focus_path_eligible)
                .map(
                    |((pattern, indexed_paths), eligible_paths)| ContextFocusPathCoverage {
                        pattern: pattern.clone(),
                        indexed_paths: *indexed_paths,
                        minimum_fragments: minimum_focus_fragments,
                        selected_fragments: 0,
                        satisfied: false,
                        diagnostics: request.verbose_diagnostics.then(|| {
                            ContextFocusPathDiagnostics {
                                eligible_paths: *eligible_paths,
                                ..ContextFocusPathDiagnostics::default()
                            }
                        }),
                    },
                )
                .collect();
        }
        coverage.unmatched_include_paths = request
            .include_paths
            .iter()
            .zip(include_path_matches)
            .filter(|(_, matched)| !matched)
            .map(|(pattern, _)| pattern.clone())
            .collect();
        coverage.unmatched_must_include_paths = request
            .must_include_paths
            .iter()
            .zip(&required_path_matches)
            .filter(|(_, matched)| !**matched)
            .map(|(pattern, _)| pattern.clone())
            .collect();
        coverage.required_evidence = request
            .required_evidence
            .iter()
            .zip(&evidence_path_matches)
            .zip(&evidence_path_files)
            .map(|((requirement, indexed_paths), inspected_files)| {
                ContextRequiredEvidenceCoverage {
                    path: requirement.path.clone(),
                    indexed_paths: *indexed_paths,
                    inspected_paths: inspected_files.len(),
                    minimum_query_matches: requirement.minimum_query_matches,
                    matched_queries: Vec::new(),
                    unmatched_queries: requirement.queries.clone(),
                    selected_fragments: 0,
                    satisfied: false,
                }
            })
            .collect();

        self.append_required_path_candidates(expansion, required_path_files, candidates, phases)?;

        let mut exact_names = Vec::new();
        let mut seen_exact_names = HashSet::new();
        for name in request
            .focus_symbols
            .iter()
            .chain(&request.must_include_symbols)
        {
            if seen_exact_names.insert(name.clone()) {
                exact_names.push(name.clone());
            }
        }
        phases.counters.exact_symbol_names = exact_names.len();
        let required_names = request
            .must_include_symbols
            .iter()
            .map(String::as_str)
            .collect::<HashSet<_>>();
        let mut exact_presence = HashSet::new();
        let mut allowed_required_hits = HashMap::<String, SymbolHit>::new();
        for names in exact_names.chunks(MAX_EXACT_SYMBOL_BATCH_NAMES) {
            check_cancelled(cancellation)?;
            phases.counters.exact_symbol_batches =
                phases.counters.exact_symbol_batches.saturating_add(1);
            let results = phases.measure(ContextTimedPhase::ExactSymbolLookup, || {
                session.find_symbols_exact_batch(names, MAX_IMPORT_SYMBOLS)
            })?;
            for (name, hits) in names.iter().zip(results) {
                phases.record_primitive("exact_symbol", || {
                    format!("case_sensitive:true:limit:{MAX_IMPORT_SYMBOLS}:name:{name}")
                });
                phases.counters.exact_symbol_hits =
                    phases.counters.exact_symbol_hits.saturating_add(hits.len());
                if hits.is_empty() {
                    continue;
                }
                exact_presence.insert(name.clone());
                if required_names.contains(name.as_str())
                    && let Some(hit) = hits.into_iter().find(|hit| path_filter.allows(&hit.path))
                {
                    allowed_required_hits.insert(name.clone(), hit);
                }
            }
        }
        for symbol in &request.focus_symbols {
            check_cancelled(cancellation)?;
            if !exact_presence.contains(symbol) {
                coverage.unmatched_focus_symbols.push(symbol.clone());
            }
        }
        let mut required_symbol_hits = Vec::<(String, SymbolHit)>::new();
        for symbol in &request.must_include_symbols {
            check_cancelled(cancellation)?;
            if !exact_presence.contains(symbol) {
                coverage.unmatched_must_include_symbols.push(symbol.clone());
                continue;
            }
            if let Some(hit) = allowed_required_hits.get(symbol).cloned() {
                required_symbol_hits.push((symbol.clone(), hit));
            }
        }
        let required_symbol_budget = request
            .token_budget
            .saturating_div(required_symbol_hits.len().max(1))
            .max(1);
        let symbol_excerpt_requests = required_symbol_hits
            .iter()
            .map(|(_, hit)| AdaptiveExcerptRequest {
                file_id: hit.symbol.file_id,
                declaration_start: hit.symbol.start_line,
                declaration_end: hit.symbol.end_line,
                matched_line: hit.symbol.start_line,
                token_budget: required_symbol_budget,
            })
            .collect::<Vec<_>>();
        phases.record_adaptive_excerpts(&symbol_excerpt_requests);
        let symbol_excerpts = phases.measure(ContextTimedPhase::AdaptiveExcerpt, || {
            self.adaptive_context_excerpts(session, &symbol_excerpt_requests)
        })?;
        for (((symbol, hit), excerpt), rank) in required_symbol_hits
            .into_iter()
            .zip(symbol_excerpts)
            .zip(0usize..)
        {
            let Some(excerpt) = excerpt else { continue };
            candidates.push(
                Candidate::new(
                    hit.path,
                    excerpt.start_line,
                    excerpt.end_line,
                    excerpt.content,
                )
                .match_kind("must_symbol")
                .concept(format!("must:symbol:{symbol}"), 2.0)
                .representation("required_symbol")
                .symbol_name(hit.symbol.name)
                .target_range(hit.symbol.start_line, hit.symbol.end_line)
                .exact(2.0)
                .symbol(2.0)
                .focus_boost(2.0)
                .channel("must_symbol", rank),
            );
        }

        self.append_required_evidence_candidates(
            expansion,
            &evidence_path_files,
            candidates,
            phases,
        )?;

        Ok(ContextConstraintExpansion {
            coverage,
            focus_paths: focus_path_files
                .into_iter()
                .zip(focus_path_matches)
                .zip(focus_path_eligible)
                .map(
                    |((files, indexed_paths), eligible_paths)| FocusPathResolution {
                        files,
                        indexed_paths,
                        eligible_paths,
                    },
                )
                .collect(),
        })
    }

    pub(in crate::services::context) fn append_required_path_candidates(
        &self,
        expansion: ConstraintCandidateExpansion<'_>,
        required_path_files: Vec<Option<FileRecord>>,
        candidates: &mut Vec<Candidate>,
        phases: &mut ContextPhaseTracker,
    ) -> Result<()> {
        let ConstraintCandidateExpansion {
            session,
            request,
            queries,
            path_scorer,
            cancellation,
        } = expansion;
        let required_path_entries = request
            .must_include_paths
            .iter()
            .zip(required_path_files)
            .filter_map(|(pattern, file)| file.map(|file| (pattern, file)))
            .collect::<Vec<_>>();
        let mut fallback_path_entries = Vec::new();
        for (pattern, file) in &required_path_entries {
            check_cancelled(cancellation)?;
            phases.record_primitive("required_path_chunks", || {
                format!(
                    "file_id:{}:limit:{MAX_CONTEXT_FOCUS_CHUNKS_PER_FILE}",
                    file.id
                )
            });
            let chunks = session.get_chunks_for_file(file.id, MAX_CONTEXT_FOCUS_CHUNKS_PER_FILE)?;
            let best = chunks
                .into_iter()
                .filter_map(|chunk| {
                    let relevance = focus_text_relevance(&chunk.content, queries);
                    (relevance > 0.0).then_some((relevance, chunk))
                })
                .max_by(|(left_score, left), (right_score, right)| {
                    left_score
                        .total_cmp(right_score)
                        .then_with(|| right.start_line.cmp(&left.start_line))
                });
            if let Some((relevance, chunk)) = best {
                candidates.push(
                    Candidate::new(&file.path, chunk.start_line, chunk.end_line, chunk.content)
                        .match_kind("must_path")
                        .concept(format!("must:path:{pattern}"), 2.0)
                        .representation("required_path")
                        .exact(relevance.min(4.0))
                        .path_score(path_scorer.score(&file.path))
                        .focus_boost(2.0),
                );
            } else {
                fallback_path_entries.push((*pattern, file.clone()));
            }
        }
        let path_excerpt_requests = fallback_path_entries
            .iter()
            .map(|(_, file)| StoredExcerptRequest {
                file_id: file.id,
                desired_start_line: 1,
                desired_end_line: 40,
                required_start_line: 1,
                required_end_line: 1,
                max_lines: 40,
            })
            .collect::<Vec<_>>();
        phases.record_stored_excerpts(&path_excerpt_requests);
        let path_excerpts = phases.measure(ContextTimedPhase::StoredExcerpt, || {
            self.stored_excerpts(session, &path_excerpt_requests)
        })?;
        for ((pattern, file), excerpt) in fallback_path_entries.into_iter().zip(path_excerpts) {
            let Some(excerpt) = excerpt else { continue };
            candidates.push(
                Candidate::new(
                    file.path,
                    excerpt.start_line,
                    excerpt.end_line,
                    excerpt.content,
                )
                .match_kind("must_path_fallback")
                .concept(format!("must:path:{pattern}"), 2.0)
                .representation("required_path_fallback")
                .exact(2.0)
                .focus_boost(2.0),
            );
        }
        Ok(())
    }

    pub(in crate::services::context) fn append_required_evidence_candidates(
        &self,
        expansion: ConstraintCandidateExpansion<'_>,
        evidence_path_files: &[Vec<FileRecord>],
        candidates: &mut Vec<Candidate>,
        phases: &mut ContextPhaseTracker,
    ) -> Result<()> {
        let ConstraintCandidateExpansion {
            session,
            request,
            queries: _,
            path_scorer,
            cancellation,
        } = expansion;
        for (requirement_index, (requirement, files)) in request
            .required_evidence
            .iter()
            .zip(evidence_path_files)
            .enumerate()
        {
            let mut retained = Vec::<RequiredEvidenceExcerptPlan>::new();
            let normalized_queries = requirement
                .queries
                .iter()
                .map(|query| query.to_lowercase())
                .collect::<Vec<_>>();
            for file in files {
                check_cancelled(cancellation)?;
                phases.record_primitive("required_evidence_chunks", || {
                    format!(
                        "file_id:{}:limit:{MAX_CONTEXT_FOCUS_CHUNKS_PER_FILE}",
                        file.id
                    )
                });
                for chunk in
                    session.get_chunks_for_file(file.id, MAX_CONTEXT_FOCUS_CHUNKS_PER_FILE)?
                {
                    let relevance =
                        required_evidence_query_matches(&chunk.content, &requirement.queries).len()
                            as f64;
                    for (line_offset, line) in chunk.content.lines().enumerate() {
                        let normalized_line = line.to_lowercase();
                        if !normalized_queries
                            .iter()
                            .any(|query| normalized_line.contains(query))
                        {
                            continue;
                        }
                        let matched_line = chunk.start_line.saturating_add(line_offset);
                        retain_required_evidence_plan(
                            &mut retained,
                            RequiredEvidenceExcerptPlan {
                                relevance,
                                path: file.path.clone(),
                                file_id: file.id,
                                matched_line,
                                requirement_index,
                            },
                        );
                    }
                }
            }
            let excerpt_requests = retained
                .iter()
                .map(|plan| StoredExcerptRequest {
                    file_id: plan.file_id,
                    desired_start_line: plan.matched_line.saturating_sub(19).max(1),
                    desired_end_line: plan.matched_line.saturating_add(20),
                    required_start_line: plan.matched_line,
                    required_end_line: plan.matched_line,
                    max_lines: 40,
                })
                .collect::<Vec<_>>();
            phases.record_stored_excerpts(&excerpt_requests);
            let excerpts = phases.measure(ContextTimedPhase::StoredExcerpt, || {
                self.stored_excerpts(session, &excerpt_requests)
            })?;
            for (rank, (plan, excerpt)) in retained.into_iter().zip(excerpts).enumerate() {
                let Some(excerpt) = excerpt else { continue };
                let matched_queries =
                    required_evidence_query_matches(&excerpt.content, &requirement.queries);
                if matched_queries.is_empty() {
                    continue;
                }
                let path_score = path_scorer.score(&plan.path);
                let mut candidate = Candidate::new(
                    plan.path,
                    excerpt.start_line,
                    excerpt.end_line,
                    excerpt.content,
                )
                .match_kind("required_evidence")
                .concept(format!("required:evidence:{}", plan.requirement_index), 2.0)
                .representation("required_evidence")
                .exact((matched_queries.len() as f64).min(4.0))
                .path_score(path_score)
                .focus_boost(2.0);
                for query_index in matched_queries {
                    candidate = candidate.match_kind(ranking::required_evidence_marker(
                        plan.requirement_index,
                        query_index,
                    ));
                }
                candidates.push(candidate.channel("required_evidence", rank));
            }
        }
        Ok(())
    }
}
use super::*;
