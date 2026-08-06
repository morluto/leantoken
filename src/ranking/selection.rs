use super::*;
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

pub(in crate::ranking) struct SelectionScope<'request> {
    pub(in crate::ranking) focus_paths: PathMatcher,
    pub(in crate::ranking) include_paths: PathMatcher,
    pub(in crate::ranking) exclude_paths: PathMatcher,
    pub(in crate::ranking) context_exclude_paths: PathMatcher,
    pub(in crate::ranking) changed_paths: HashSet<&'request str>,
}

impl<'request> SelectionScope<'request> {
    fn new(request: &'request ContextRequest, context_exclude_paths: &[String]) -> Self {
        Self {
            // The public ranking API returns a response rather than `Result`.
            // It must remain safe when called directly with a malformed
            // request, independently of adapter admission validation.
            focus_paths: PathMatcher::new(&request.focus_paths)
                .unwrap_or_else(|_| PathMatcher::empty()),
            include_paths: PathMatcher::new(&request.include_paths)
                .unwrap_or_else(|_| PathMatcher::empty()),
            exclude_paths: PathMatcher::new(&request.exclude_paths)
                .unwrap_or_else(|_| PathMatcher::empty()),
            context_exclude_paths: PathMatcher::new(context_exclude_paths)
                .unwrap_or_else(|_| PathMatcher::empty()),
            changed_paths: request.changed_paths.iter().map(String::as_str).collect(),
        }
    }
}

pub(in crate::ranking) struct CandidatePartition {
    pub(in crate::ranking) eligible: Vec<Candidate>,
    pub(in crate::ranking) path_omitted: Vec<ScoredCandidate>,
    pub(in crate::ranking) known_omitted: Vec<ScoredCandidate>,
    pub(in crate::ranking) generated_artifact_warning: bool,
}

pub(in crate::ranking) fn partition_candidates(
    candidates: Vec<Candidate>,
    request: &ContextRequest,
    scope: &SelectionScope<'_>,
    weights: &Weights,
    tokenizer: tokens::Tokenizer,
) -> CandidatePartition {
    let known_hashes = request.known_hashes.iter().collect::<HashSet<_>>();
    let mut partition = CandidatePartition {
        eligible: Vec::with_capacity(candidates.len()),
        path_omitted: Vec::new(),
        known_omitted: Vec::new(),
        generated_artifact_warning: false,
    };
    for candidate in candidates {
        let explicitly_included =
            !request.include_paths.is_empty() && scope.include_paths.is_match(&candidate.path);
        partition.generated_artifact_warning |=
            scope.context_exclude_paths.is_match(&candidate.path);
        let path_excluded = (!request.include_paths.is_empty()
            && !scope.include_paths.is_match(&candidate.path))
            || scope.exclude_paths.is_match(&candidate.path)
            || (scope.context_exclude_paths.is_match(&candidate.path) && !explicitly_included)
            || (request.strict_focus_paths && !scope.focus_paths.is_match(&candidate.path))
            || (request.strict_changed_paths
                && !scope.changed_paths.contains(candidate.path.as_str()));
        if path_excluded {
            partition
                .path_omitted
                .push(ScoredCandidate::new_with_tokenizer(
                    candidate, weights, tokenizer,
                ));
        } else if known_hashes.contains(&candidate.content_hash()) {
            partition
                .known_omitted
                .push(ScoredCandidate::new_with_tokenizer(
                    candidate, weights, tokenizer,
                ));
        } else {
            partition.eligible.push(candidate);
        }
    }
    partition
}

pub(in crate::ranking) struct CandidateSelection {
    pub(in crate::ranking) selected: Vec<ScoredCandidate>,
    pub(in crate::ranking) omitted: Vec<ScoredCandidate>,
    pub(in crate::ranking) candidate_paths_total: usize,
    pub(in crate::ranking) result_complete: bool,
}

pub(in crate::ranking) fn select_candidates(
    candidates: Vec<Candidate>,
    request: &ContextRequest,
    weights: &Weights,
    tokenizer: tokens::Tokenizer,
) -> CandidateSelection {
    let ranked = rank_with_tokenizer(candidates, weights, tokenizer);
    let deduped = deduplicate_with_options(ranked, weights);
    let candidate_paths_total = deduped
        .iter()
        .map(|candidate| candidate.candidate.path.as_str())
        .collect::<HashSet<_>>()
        .len();
    let budget = request.token_budget;
    let max_per_file = (budget / DIVERSITY_DIVISOR).clamp(1, 3);
    let max_fragments = request.max_fragments.unwrap_or(DEFAULT_CONTEXT_FRAGMENTS);
    let (mut selected, remaining) =
        select_required_candidates(deduped, request, budget, max_fragments);
    let required_tokens = selected
        .iter()
        .map(|candidate| candidate.token_count)
        .sum::<usize>();
    let mut initial_file_counts: HashMap<String, usize> = HashMap::new();
    for candidate in &selected {
        *initial_file_counts
            .entry(candidate.candidate.path.clone())
            .or_default() += 1;
    }
    let (additional, omitted) = greedy_select(
        remaining,
        budget.saturating_sub(required_tokens),
        max_per_file,
        max_fragments.saturating_sub(selected.len()),
        initial_file_counts,
    );
    selected.extend(additional);
    CandidateSelection {
        result_complete: omitted.is_empty(),
        selected,
        omitted,
        candidate_paths_total,
    }
}

pub(in crate::ranking) fn select_with_options(
    mut candidates: Vec<Candidate>,
    request: &ContextRequest,
    repository_generation: u64,
    weights: &Weights,
    tokenizer: tokens::Tokenizer,
    context_exclude_paths: &[String],
    prefiltered_path_omissions: &[String],
) -> ContextResponse {
    let scope = SelectionScope::new(request, context_exclude_paths);
    apply_request_signals(&mut candidates, request, &scope.focus_paths);
    let generated_focus = request
        .explain_diagnostics
        .then(|| generated_focus_facts(&candidates, request));
    let partition = partition_candidates(candidates, request, &scope, weights, tokenizer);
    let selection = select_candidates(partition.eligible, request, weights, tokenizer);
    let mut coverage =
        build_context_coverage(request, &selection.selected, &partition.known_omitted);
    if let Some(generated_focus) = &generated_focus {
        coverage.focus_path_coverage = build_focus_path_coverage(
            request,
            generated_focus,
            &partition.path_omitted,
            &partition.known_omitted,
            &selection.selected,
            &selection.omitted,
        );
    }
    let estimated_source_tokens = selection
        .selected
        .iter()
        .map(|candidate| candidate.token_count)
        .sum();
    let plan = build_context_plan(
        request,
        &selection.selected,
        selection.candidate_paths_total,
        estimated_source_tokens,
        partition.generated_artifact_warning,
        selection.result_complete,
    );
    let (fragments, fragment_hashes, emitted_tokens) =
        materialize_context_fragments(request, &selection.selected, estimated_source_tokens);
    let (omission_summary, omitted, warnings) =
        build_context_omissions(super::omissions::BuildOmissionsParams {
            request,
            path_omitted: partition.path_omitted,
            known_omitted: partition.known_omitted,
            limit_omitted: selection.omitted,
            prefiltered_path_omissions,
            focus_paths: &scope.focus_paths,
            changed_paths: &scope.changed_paths,
            generated_artifact_warning: partition.generated_artifact_warning,
        });
    finalize_context_response(super::response::FinalizeContextParams {
        request,
        repository_generation,
        tokenizer,
        plan,
        fragments,
        fragment_hashes,
        emitted_tokens,
        omitted,
        omission_summary,
        coverage,
        warnings,
    })
}
