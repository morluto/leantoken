impl Services {
    pub(in crate::services::context) fn append_focus_candidates(
        &self,
        expansion: FocusExpansion<'_>,
        candidates: &mut Vec<Candidate>,
        phases: &mut ContextPhaseTracker,
    ) -> Result<Vec<String>> {
        let FocusExpansion {
            session,
            request,
            queries,
            path_scorer,
            resolutions,
            cancellation,
        } = expansion;
        let mut warnings = Vec::new();
        if !request.strict_focus_paths && request.minimum_fragments_per_focus_path.is_none() {
            return Ok(warnings);
        }
        for ((pattern, resolution), pattern_index) in
            request.focus_paths.iter().zip(resolutions).zip(0usize..)
        {
            check_cancelled(cancellation)?;
            if resolution.eligible_paths > MAX_CONTEXT_FOCUS_FILES_PER_PATTERN {
                warnings.push(format!(
                    "focus pattern `{pattern}` matched {} eligible indexed paths; \
                     candidate generation inspected the first {} paths",
                    resolution.eligible_paths, MAX_CONTEXT_FOCUS_FILES_PER_PATTERN
                ));
            }
            if resolution.files.is_empty() {
                if resolution.indexed_paths > 0 {
                    warnings.push(format!(
                        "focus pattern `{pattern}` has no candidate-eligible indexed path after \
                         include, exclude, generated-artifact, and strict changed-scope policy"
                    ));
                }
                continue;
            }

            let mut semantic = Vec::new();
            let mut fallback = Vec::new();
            for file in &resolution.files {
                check_cancelled(cancellation)?;
                phases.record_primitive("focus_file_chunks", || {
                    format!(
                        "file_id:{}:limit:{MAX_CONTEXT_FOCUS_CHUNKS_PER_FILE}",
                        file.id
                    )
                });
                let chunks =
                    session.get_chunks_for_file(file.id, MAX_CONTEXT_FOCUS_CHUNKS_PER_FILE)?;
                phases.record_primitive("focus_file_symbols", || {
                    format!(
                        "file_id:{}:limit:{MAX_CONTEXT_FOCUS_SYMBOLS_PER_FILE}",
                        file.id
                    )
                });
                let symbols =
                    session.get_symbols_for_file(file.id, MAX_CONTEXT_FOCUS_SYMBOLS_PER_FILE)?;

                for symbol in symbols {
                    let Some(chunk) = chunks.iter().find(|chunk| {
                        chunk.start_line <= symbol.start_line && chunk.end_line >= symbol.start_line
                    }) else {
                        continue;
                    };
                    let symbol_start = symbol.start_line.saturating_sub(chunk.start_line);
                    let symbol_lines = symbol
                        .end_line
                        .min(chunk.end_line)
                        .saturating_sub(symbol.start_line)
                        .saturating_add(1);
                    let symbol_content = chunk
                        .content
                        .lines()
                        .skip(symbol_start)
                        .take(symbol_lines)
                        .collect::<Vec<_>>()
                        .join("\n");
                    let searchable = format!(
                        "{} {} {} {} {}",
                        symbol.name,
                        symbol.kind,
                        symbol.parent.as_deref().unwrap_or_default(),
                        symbol.signature.as_deref().unwrap_or_default(),
                        symbol_content
                    );
                    let relevance = focus_text_relevance(&searchable, queries);
                    let exact_focus_symbol = request
                        .focus_symbols
                        .iter()
                        .any(|focus_symbol| focus_symbol == &symbol.name);
                    if relevance == 0.0 && !exact_focus_symbol {
                        continue;
                    }
                    let exact_score = if exact_focus_symbol {
                        4.0
                    } else {
                        relevance.min(4.0)
                    };
                    let candidate = Candidate::new(
                        &file.path,
                        chunk.start_line,
                        chunk.end_line,
                        &chunk.content,
                    )
                    .match_kind("focus_symbol")
                    .concept(format!("focus:path:{pattern}"), 2.0)
                    .representation("focus_symbol")
                    .symbol_name(symbol.name)
                    .target_range(symbol.start_line, symbol.end_line)
                    .exact(exact_score)
                    .symbol(if exact_focus_symbol { 2.0 } else { 1.0 })
                    .path_score(path_scorer.score(&file.path))
                    .focus_boost(2.0);
                    retain_ranked_focus_candidate(
                        &mut semantic,
                        FocusCandidate {
                            relevance: relevance + if exact_focus_symbol { 5.0 } else { 1.0 },
                            path: file.path.clone(),
                            start_line: chunk.start_line,
                            end_line: chunk.end_line,
                            candidate,
                        },
                    );
                }

                let mut file_fallback = None;
                for chunk in chunks {
                    let relevance = focus_text_relevance(&chunk.content, queries);
                    let candidate =
                        Candidate::new(&file.path, chunk.start_line, chunk.end_line, chunk.content)
                            .match_kind(if relevance > 0.0 {
                                "focus_text"
                            } else {
                                "focus_fallback"
                            })
                            .concept(format!("focus:path:{pattern}"), 2.0)
                            .representation(if relevance > 0.0 {
                                "focus_text"
                            } else {
                                "focus_fallback"
                            })
                            .exact(relevance.min(4.0))
                            .path_score(path_scorer.score(&file.path))
                            .focus_boost(2.0);
                    let candidate = FocusCandidate {
                        relevance,
                        path: file.path.clone(),
                        start_line: chunk.start_line,
                        end_line: chunk.end_line,
                        candidate,
                    };
                    if relevance > 0.0 {
                        retain_ranked_focus_candidate(&mut semantic, candidate);
                    } else if file_fallback
                        .as_ref()
                        .is_none_or(|best: &FocusCandidate| candidate.start_line < best.start_line)
                    {
                        file_fallback = Some(candidate);
                    }
                }
                fallback.extend(file_fallback);
            }

            let selected = if semantic.is_empty() {
                &mut fallback
            } else {
                &mut semantic
            };
            selected.sort_by(|left, right| {
                right
                    .relevance
                    .total_cmp(&left.relevance)
                    .then_with(|| left.path.cmp(&right.path))
                    .then_with(|| left.start_line.cmp(&right.start_line))
                    .then_with(|| left.end_line.cmp(&right.end_line))
            });
            let mut retained_ranges = BTreeSet::new();
            let mut retained = 0usize;
            for mut candidate in selected.drain(..) {
                if !retained_ranges.insert((
                    candidate.path.clone(),
                    candidate.start_line,
                    candidate.end_line,
                )) {
                    continue;
                }
                candidate.candidate = candidate.candidate.channel(
                    "focus_local",
                    pattern_index
                        .saturating_mul(MAX_CONTEXT_FOCUS_CANDIDATES_PER_PATTERN)
                        .saturating_add(retained),
                );
                candidates.push(candidate.candidate);
                retained = retained.saturating_add(1);
                if retained == MAX_CONTEXT_FOCUS_CANDIDATES_PER_PATTERN {
                    break;
                }
            }
            let requested_minimum = request
                .minimum_fragments_per_focus_path
                .unwrap_or(usize::from(request.strict_focus_paths));
            if retained == 0 {
                warnings.push(format!(
                    "focus pattern `{pattern}` matched indexed files without bounded chunk evidence"
                ));
            } else if retained < requested_minimum {
                warnings.push(format!(
                    "focus pattern `{pattern}` generated {retained} distinct bounded candidates \
                     for requested minimum {requested_minimum}"
                ));
            }
        }
        Ok(warnings)
    }

    pub(in crate::services::context) fn finalize_strict_scope_coverage(
        &self,
        session: &IndexReadSnapshot,
        request: &ContextRequest,
        selected_paths: &[String],
        coverage: &mut ContextCoverageReceipt,
    ) -> Result<()> {
        for focus in &mut coverage.focus_path_coverage {
            let matcher = PathMatcher::new(std::slice::from_ref(&focus.pattern))?;
            focus.selected_fragments = selected_paths
                .iter()
                .filter(|path| matcher.is_match(path))
                .count();
            focus.satisfied =
                focus.indexed_paths > 0 && focus.selected_fragments >= focus.minimum_fragments;
            if let Some(diagnostics) = &mut focus.diagnostics {
                diagnostics.capacity_blocker = if focus.satisfied {
                    None
                } else if focus.indexed_paths == 0 {
                    Some(ContextFocusCapacityBlocker::NoIndexedPaths)
                } else if diagnostics.eligible_paths == 0 {
                    Some(ContextFocusCapacityBlocker::PathPolicy)
                } else if diagnostics.eligible_paths > MAX_CONTEXT_FOCUS_FILES_PER_PATTERN
                    && diagnostics.generated_fragments < focus.minimum_fragments
                {
                    Some(ContextFocusCapacityBlocker::CandidateFanoutLimit)
                } else {
                    diagnostics
                        .capacity_blocker
                        .or(Some(ContextFocusCapacityBlocker::CandidateGeneration))
                };
            }
        }

        if request.strict_changed_paths {
            let changed_paths = request
                .changed_paths
                .iter()
                .map(String::as_str)
                .collect::<HashSet<_>>();
            let mut indexed_paths = 0usize;
            for path in &request.changed_paths {
                if session.find_file(path)?.is_some() {
                    indexed_paths = indexed_paths.saturating_add(1);
                }
            }
            let selected_fragments = selected_paths
                .iter()
                .filter(|path| changed_paths.contains(path.as_str()))
                .count();
            coverage.changed_path_coverage = Some(ContextChangedPathCoverage {
                resolved_paths: changed_paths.len(),
                indexed_paths,
                selected_fragments,
                satisfied: !changed_paths.is_empty() && indexed_paths > 0 && selected_fragments > 0,
            });
        }

        let focus_coverage_is_required =
            request.strict_focus_paths || request.minimum_fragments_per_focus_path.is_some();
        if focus_coverage_is_required || request.strict_changed_paths {
            let satisfied = (!focus_coverage_is_required
                || coverage
                    .focus_path_coverage
                    .iter()
                    .all(|focus| focus.satisfied))
                && coverage
                    .changed_path_coverage
                    .as_ref()
                    .is_none_or(|changed| changed.satisfied);
            coverage.path_scope_satisfied = Some(satisfied);
        }
        Ok(())
    }
}
use super::*;
