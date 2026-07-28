/// Select the highest-relevance candidates that fit within the token budget
/// while preserving file diversity and bounding protocol metadata.
#[must_use]
pub fn select(
    candidates: Vec<Candidate>,
    request: &ContextRequest,
    repository_generation: u64,
) -> ContextResponse {
    select_with_tokenizer(
        candidates,
        request,
        repository_generation,
        tokens::Tokenizer::default(),
    )
}

/// Select candidates using an explicit tokenizer for budgets and metadata.
#[must_use]
pub fn select_with_tokenizer(
    candidates: Vec<Candidate>,
    request: &ContextRequest,
    repository_generation: u64,
    tokenizer: tokens::Tokenizer,
) -> ContextResponse {
    select_with_tokenizer_and_context_exclusions(
        candidates,
        request,
        repository_generation,
        tokenizer,
        &default_context_exclude_paths(),
        &[],
    )
}

pub(crate) fn select_with_tokenizer_and_context_exclusions(
    candidates: Vec<Candidate>,
    request: &ContextRequest,
    repository_generation: u64,
    tokenizer: tokens::Tokenizer,
    context_exclude_paths: &[String],
    prefiltered_path_omissions: &[String],
) -> ContextResponse {
    select_with_options(
        candidates,
        request,
        repository_generation,
        &Weights::default(),
        tokenizer,
        context_exclude_paths,
        prefiltered_path_omissions,
    )
}

/// Same as [`select`] but with explicit [`Weights`].
#[must_use]
pub fn select_with_weights(
    candidates: Vec<Candidate>,
    request: &ContextRequest,
    repository_generation: u64,
    weights: &Weights,
) -> ContextResponse {
    select_with_weights_and_tokenizer(
        candidates,
        request,
        repository_generation,
        weights,
        tokens::Tokenizer::default(),
    )
}

/// Select candidates with explicit ranking weights and tokenizer.
#[must_use]
pub fn select_with_weights_and_tokenizer(
    candidates: Vec<Candidate>,
    request: &ContextRequest,
    repository_generation: u64,
    weights: &Weights,
    tokenizer: tokens::Tokenizer,
) -> ContextResponse {
    select_with_options(
        candidates,
        request,
        repository_generation,
        weights,
        tokenizer,
        &default_context_exclude_paths(),
        &[],
    )
}

fn select_with_options(
    candidates: Vec<Candidate>,
    request: &ContextRequest,
    repository_generation: u64,
    weights: &Weights,
    tokenizer: tokens::Tokenizer,
    context_exclude_paths: &[String],
    prefiltered_path_omissions: &[String],
) -> ContextResponse {
    let mut candidates = candidates;
    let focus_paths = PathMatcher::new_lossy(&request.focus_paths);
    let include_paths = PathMatcher::new_lossy(&request.include_paths);
    let exclude_paths = PathMatcher::new_lossy(&request.exclude_paths);
    let context_exclude_paths = PathMatcher::new_lossy(context_exclude_paths);
    let changed_paths = request
        .changed_paths
        .iter()
        .map(String::as_str)
        .collect::<HashSet<_>>();
    apply_request_signals(&mut candidates, request, &focus_paths);

    let known_hashes: HashSet<String> = request.known_hashes.iter().cloned().collect();

    let mut path_omitted: Vec<ScoredCandidate> = Vec::new();
    let mut known_omitted: Vec<ScoredCandidate> = Vec::new();
    let mut eligible: Vec<Candidate> = Vec::with_capacity(candidates.len());
    let mut generated_artifact_warning = false;

    for candidate in candidates {
        let explicitly_included =
            !request.include_paths.is_empty() && include_paths.is_match(&candidate.path);
        generated_artifact_warning |= context_exclude_paths.is_match(&candidate.path);
        if (!request.include_paths.is_empty() && !include_paths.is_match(&candidate.path))
            || exclude_paths.is_match(&candidate.path)
            || (context_exclude_paths.is_match(&candidate.path) && !explicitly_included)
            || (request.strict_focus_paths && !focus_paths.is_match(&candidate.path))
            || (request.strict_changed_paths && !changed_paths.contains(candidate.path.as_str()))
        {
            path_omitted.push(ScoredCandidate::new_with_tokenizer(
                candidate, weights, tokenizer,
            ));
            continue;
        }

        let hash = candidate.content_hash();
        if known_hashes.contains(&hash) {
            known_omitted.push(ScoredCandidate::new_with_tokenizer(
                candidate, weights, tokenizer,
            ));
        } else {
            eligible.push(candidate);
        }
    }

    let ranked = rank_with_tokenizer(eligible, weights, tokenizer);
    let deduped = deduplicate_with_options(ranked, weights);
    let candidate_paths_total = deduped
        .iter()
        .map(|candidate| candidate.candidate.path.as_str())
        .collect::<HashSet<_>>()
        .len();

    let budget = request.token_budget;
    let max_per_file = (budget / DIVERSITY_DIVISOR).clamp(1, 3);
    // Candidate excerpts vary from a few tokens to hundreds. A token-derived
    // fragment estimate underfilled budgets when high-value evidence happened
    // to be short. The caller-bounded cap limits metadata; the token budget
    // remains the authoritative content bound.
    let max_fragments = request.max_fragments.unwrap_or(DEFAULT_CONTEXT_FRAGMENTS);
    let (mut selected, remaining) =
        select_required_candidates(deduped, request, budget, max_fragments);
    let required_tokens = selected
        .iter()
        .map(|candidate| candidate.token_count)
        .sum::<usize>();
    let (additional, mut omitted) = greedy_select(
        remaining,
        budget.saturating_sub(required_tokens),
        max_per_file,
        max_fragments.saturating_sub(selected.len()),
    );
    selected.extend(additional);
    let result_complete = omitted.is_empty();

    let covered_candidates = selected.iter().chain(&known_omitted);
    let mut coverage = ContextCoverageReceipt::default();
    for pattern in &request.must_include_paths {
        if covered_candidates
            .clone()
            .any(|candidate| required_path_matches(&candidate.candidate, pattern))
        {
            coverage.covered_must_include_paths.push(pattern.clone());
        } else {
            coverage.uncovered_must_include_paths.push(pattern.clone());
        }
    }
    for symbol in &request.must_include_symbols {
        if covered_candidates
            .clone()
            .any(|candidate| required_symbol_satisfied(&candidate.candidate, symbol))
        {
            coverage.covered_must_include_symbols.push(symbol.clone());
        } else if covered_candidates
            .clone()
            .any(|candidate| required_symbol_matches(&candidate.candidate, symbol))
        {
            coverage.partial_must_include_symbols.push(symbol.clone());
        } else {
            coverage.uncovered_must_include_symbols.push(symbol.clone());
        }
    }
    coverage.required_evidence = request
        .required_evidence
        .iter()
        .enumerate()
        .map(|(requirement_index, requirement)| {
            let matched_queries = requirement
                .queries
                .iter()
                .enumerate()
                .filter(|(query_index, _)| {
                    covered_candidates.clone().any(|candidate| {
                        required_evidence_query(
                            &candidate.candidate,
                            requirement_index,
                            *query_index,
                        )
                    })
                })
                .map(|(_, query)| query.clone())
                .collect::<Vec<_>>();
            let unmatched_queries = requirement
                .queries
                .iter()
                .filter(|query| !matched_queries.contains(query))
                .cloned()
                .collect::<Vec<_>>();
            let selected_fragments = covered_candidates
                .clone()
                .filter(|candidate| {
                    carries_required_evidence(&candidate.candidate, requirement_index)
                })
                .count();
            ContextRequiredEvidenceCoverage {
                path: requirement.path.clone(),
                indexed_paths: 0,
                inspected_paths: 0,
                minimum_query_matches: requirement.minimum_query_matches,
                satisfied: matched_queries.len() >= requirement.minimum_query_matches,
                matched_queries,
                unmatched_queries,
                selected_fragments,
            }
        })
        .collect();
    if !request.required_evidence.is_empty() {
        coverage.evidence_scope_satisfied =
            Some(coverage.required_evidence.iter().all(|item| item.satisfied));
    }

    let estimated_source_tokens = selected.iter().map(|candidate| candidate.token_count).sum();
    let plan = request.plan_only.then(|| {
        let minimum_fragments = request.minimum_fragments_per_focus_path.unwrap_or(1);
        let focus_coverage = request
            .focus_paths
            .iter()
            .map(|pattern| {
                let matcher = PathMatcher::new_lossy(std::slice::from_ref(pattern));
                let candidate_fragments = selected
                    .iter()
                    .filter(|candidate| matcher.is_match(&candidate.candidate.path))
                    .count();
                ContextPlanFocusCoverage {
                    pattern: pattern.clone(),
                    candidate_fragments,
                    minimum_fragments,
                    satisfied: candidate_fragments >= minimum_fragments,
                }
            })
            .collect();
        let candidates = selected
            .iter()
            .map(|scored| ContextPlanCandidate {
                path: scored.candidate.path.clone(),
                start_line: scored.candidate.start_line,
                end_line: scored.candidate.end_line,
                target_start_line: scored.candidate.target_start_line,
                target_end_line: scored.candidate.target_end_line,
                truncated: scored.candidate.target_truncated(),
                representation: scored.candidate.representation.clone(),
                score: (scored.score * 10_000.0).round() / 10_000.0,
                reasons: scored
                    .candidate
                    .reason()
                    .split("; ")
                    .map(str::to_owned)
                    .collect(),
                estimated_tokens: scored.token_count,
            })
            .collect();
        ContextQueryPlan {
            candidates,
            candidate_paths_total,
            estimated_source_tokens,
            focus_coverage,
            generated_artifact_warning,
            result_complete,
        }
    });

    // Materialized responses carry source; plans carry only the same selection's metadata.
    let mut fragments = Vec::with_capacity(selected.len());
    let mut fragment_hashes = Vec::with_capacity(selected.len());
    if !request.plan_only {
        for scored in &selected {
            fragments.push(ContextFragment {
                path: scored.candidate.path.clone(),
                start_line: scored.candidate.start_line,
                end_line: scored.candidate.end_line,
                target_start_line: scored.candidate.target_start_line,
                target_end_line: scored.candidate.target_end_line,
                truncated: scored.candidate.target_truncated(),
                representation: scored.candidate.representation.clone(),
                content: scored.candidate.content.clone(),
                content_hash: scored.content_hash.clone(),
                score: (scored.score * 10_000.0).round() / 10_000.0,
                reason: scored.candidate.reason(),
                token_count: scored.token_count,
            });
            fragment_hashes.push(scored.content_hash.clone());
        }
    }
    let emitted_tokens = if request.plan_only {
        0
    } else {
        estimated_source_tokens
    };

    let omission_summary = summarize_omissions(
        &path_omitted,
        &known_omitted,
        &omitted,
        prefiltered_path_omissions,
        &focus_paths,
        &changed_paths,
        request.verbose_diagnostics,
    );
    let mut omitted_dto: Vec<OmittedCandidate> = path_omitted
        .into_iter()
        .map(|scored| OmittedCandidate {
            path: scored.candidate.path,
            start_line: scored.candidate.start_line,
            end_line: scored.candidate.end_line,
            reason: "path excluded".to_string(),
        })
        .chain(known_omitted.into_iter().map(|scored| OmittedCandidate {
            path: scored.candidate.path,
            start_line: scored.candidate.start_line,
            end_line: scored.candidate.end_line,
            reason: "known hash".to_string(),
        }))
        .collect();

    omitted_dto.extend(omitted.drain(..).map(|scored| OmittedCandidate {
        path: scored.candidate.path,
        start_line: scored.candidate.start_line,
        end_line: scored.candidate.end_line,
        reason: "budget or result limit".to_string(),
    }));

    if !request.verbose_diagnostics {
        omitted_dto.clear();
    }
    let omitted_count = omission_summary
        .path_excluded
        .saturating_add(omission_summary.known_hash)
        .saturating_add(omission_summary.budget_or_result_limit);
    omitted_dto.truncate(MAX_OMITTED_DETAILS);
    let mut warnings = Vec::new();
    if omitted_count > 0 {
        warnings.push(format!("{omitted_count} omitted"));
    }
    if request.plan_only && generated_artifact_warning {
        warnings.push(
            "generated-artifact candidates matched context exclusion defaults; review their explicit inclusion before materializing source"
                .into(),
        );
    }

    let task_hash = blake3::hash(request.task.as_bytes()).to_hex().to_string();
    let task_fingerprint = task_hash[..32].to_string();

    let receipt = EvidenceReceipt {
        task_fingerprint,
        fragment_hashes,
    };

    let meta = ResponseMeta {
        repository_id: String::new(),
        repository_generation,
        freshness: Freshness::Current,
        source_tokens: emitted_tokens,
        protocol_tokens: 0,
        path_and_metadata_tokens: 0,
        total_response_tokens: 0,
        payload_tokens: 0,
        tokenizer: tokenizer.name().into(),
        emitted_tokens,
        token_count_exact: tokenizer.is_exact(),
        receipt_id: None,
        receipt_suppressed_exact: 0,
        receipt_suppressed_overlap: 0,
        receipt_near_duplicates: 0,
        next_cursor: None,
    };

    let mut response = ContextResponse {
        workflow: crate::model::ContextWorkflow::Implementation,
        workflow_receipt: None,
        plan,
        fragments,
        receipt,
        diff_scope: None,
        omitted: omitted_dto,
        omission_summary,
        coverage,
        routing: None,
        handoff_manifest: None,
        warnings,
        meta,
    };
    let accounting = tokens::response_token_accounting(&response, emitted_tokens, &tokenizer)
        .expect("context response metadata is serializable");
    response.meta.protocol_tokens = accounting.protocol_tokens;
    response.meta.path_and_metadata_tokens = accounting.path_and_metadata_tokens;
    response.meta.total_response_tokens = accounting.total_response_tokens;
    response.meta.payload_tokens = accounting.total_response_tokens;
    response
}
