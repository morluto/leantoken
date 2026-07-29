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

struct SelectionScope<'request> {
    focus_paths: PathMatcher,
    include_paths: PathMatcher,
    exclude_paths: PathMatcher,
    context_exclude_paths: PathMatcher,
    changed_paths: HashSet<&'request str>,
}

impl<'request> SelectionScope<'request> {
    fn new(request: &'request ContextRequest, context_exclude_paths: &[String]) -> Self {
        Self {
            focus_paths: PathMatcher::new_lossy(&request.focus_paths),
            include_paths: PathMatcher::new_lossy(&request.include_paths),
            exclude_paths: PathMatcher::new_lossy(&request.exclude_paths),
            context_exclude_paths: PathMatcher::new_lossy(context_exclude_paths),
            changed_paths: request.changed_paths.iter().map(String::as_str).collect(),
        }
    }
}

struct CandidatePartition {
    eligible: Vec<Candidate>,
    path_omitted: Vec<ScoredCandidate>,
    known_omitted: Vec<ScoredCandidate>,
    generated_artifact_warning: bool,
}

fn partition_candidates(
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

struct CandidateSelection {
    selected: Vec<ScoredCandidate>,
    omitted: Vec<ScoredCandidate>,
    candidate_paths_total: usize,
    result_complete: bool,
}

fn select_candidates(
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
    let (additional, omitted) = greedy_select(
        remaining,
        budget.saturating_sub(required_tokens),
        max_per_file,
        max_fragments.saturating_sub(selected.len()),
    );
    selected.extend(additional);
    CandidateSelection {
        result_complete: omitted.is_empty(),
        selected,
        omitted,
        candidate_paths_total,
    }
}

fn select_with_options(
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
    let generated_focus =
        request
            .verbose_diagnostics
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
    let (omission_summary, omitted, warnings) = build_context_omissions(
        request,
        partition.path_omitted,
        partition.known_omitted,
        selection.omitted,
        prefiltered_path_omissions,
        &scope.focus_paths,
        &scope.changed_paths,
        partition.generated_artifact_warning,
    );
    finalize_context_response(
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
    )
}
