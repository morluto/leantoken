use super::*;
#[derive(Debug, Clone, Copy)]
pub(in crate::ranking) struct GeneratedFocusFacts<'matcher> {
    pub(in crate::ranking) matcher: &'matcher PathMatcher,
    pub(in crate::ranking) fragments: usize,
    pub(in crate::ranking) symbol_fragments: usize,
}

pub(in crate::ranking) fn unique_focus_candidates(
    candidates: &[ScoredCandidate],
    matcher: &PathMatcher,
) -> HashSet<(String, usize, usize)> {
    candidates
        .iter()
        .filter(|candidate| matcher.is_match(&candidate.candidate.path))
        .map(|candidate| {
            (
                candidate.candidate.path.clone(),
                candidate.candidate.start_line,
                candidate.candidate.end_line,
            )
        })
        .collect()
}

pub(in crate::ranking) fn generated_focus_facts<'matcher>(
    candidates: &[Candidate],
    matchers: &'matcher [PathMatcher],
) -> Vec<GeneratedFocusFacts<'matcher>> {
    matchers
        .iter()
        .map(|matcher| {
            let mut generated = HashSet::new();
            let mut symbols = HashSet::new();
            for candidate in candidates
                .iter()
                .filter(|candidate| matcher.is_match(&candidate.path))
            {
                let key = (
                    candidate.path.clone(),
                    candidate.start_line,
                    candidate.end_line,
                );
                generated.insert(key.clone());
                if candidate.target_range.is_some() {
                    symbols.insert(key);
                }
            }
            GeneratedFocusFacts {
                matcher,
                fragments: generated.len(),
                symbol_fragments: symbols.len(),
            }
        })
        .collect()
}

pub(in crate::ranking) fn push_focus_suppression(
    suppressions: &mut Vec<ContextFocusSuppression>,
    boundary: ContextFocusSuppressionBoundary,
    fragments: usize,
) {
    if fragments > 0 {
        suppressions.push(ContextFocusSuppression {
            boundary,
            fragments,
        });
    }
}

pub(in crate::ranking) fn focus_limit_suppressions(
    omitted: &[ScoredCandidate],
    selected: &[ScoredCandidate],
    matcher: &PathMatcher,
    request: &ContextRequest,
    policy: ContextSelectionPolicy,
) -> [usize; 4] {
    let selected_tokens = selected
        .iter()
        .map(|candidate| candidate.token_count)
        .sum::<usize>();
    let remaining_tokens = request.token_budget.saturating_sub(selected_tokens);
    let selected_per_file = selected
        .iter()
        .fold(HashMap::new(), |mut counts, candidate| {
            *counts
                .entry(candidate.candidate.path.as_str())
                .or_insert(0usize) += 1;
            counts
        });
    let max_per_file = (request.token_budget / DIVERSITY_DIVISOR).clamp(1, 3);
    let max_fragments = request.max_fragments.unwrap_or(DEFAULT_CONTEXT_FRAGMENTS);
    let enforced_focus_minimum = policy.focus_minimum().is_some();
    let mut counts = [0usize; 4];
    for candidate in omitted
        .iter()
        .filter(|candidate| matcher.is_match(&candidate.candidate.path))
    {
        let boundary = if enforced_focus_minimum && selected.len() >= max_fragments {
            1
        } else if candidate.token_count > remaining_tokens {
            0
        } else if selected_per_file
            .get(candidate.candidate.path.as_str())
            .copied()
            .unwrap_or_default()
            >= max_per_file
        {
            2
        } else {
            3
        };
        counts[boundary] = counts[boundary].saturating_add(1);
    }
    counts
}

pub(in crate::ranking) fn focus_capacity_blocker(
    selected_fragments: usize,
    minimum_fragments: usize,
    generated_fragments: usize,
    suppressions: &[ContextFocusSuppression],
) -> Option<ContextFocusCapacityBlocker> {
    if selected_fragments >= minimum_fragments {
        return None;
    }
    if generated_fragments == 0 {
        return Some(ContextFocusCapacityBlocker::CandidateGeneration);
    }
    for boundary in [
        ContextFocusSuppressionBoundary::MaxFragments,
        ContextFocusSuppressionBoundary::TokenBudget,
        ContextFocusSuppressionBoundary::KnownHash,
        ContextFocusSuppressionBoundary::FileDiversity,
        ContextFocusSuppressionBoundary::GlobalRanking,
        ContextFocusSuppressionBoundary::Deduplicated,
        ContextFocusSuppressionBoundary::PathPolicy,
    ] {
        if suppressions
            .iter()
            .any(|suppression| suppression.boundary == boundary)
        {
            return Some(match boundary {
                ContextFocusSuppressionBoundary::PathPolicy => {
                    ContextFocusCapacityBlocker::PathPolicy
                }
                ContextFocusSuppressionBoundary::KnownHash => {
                    ContextFocusCapacityBlocker::KnownHash
                }
                ContextFocusSuppressionBoundary::Deduplicated => {
                    ContextFocusCapacityBlocker::Deduplicated
                }
                ContextFocusSuppressionBoundary::GlobalRanking => {
                    ContextFocusCapacityBlocker::GlobalRanking
                }
                ContextFocusSuppressionBoundary::TokenBudget => {
                    ContextFocusCapacityBlocker::TokenBudget
                }
                ContextFocusSuppressionBoundary::MaxFragments => {
                    ContextFocusCapacityBlocker::MaxFragments
                }
                ContextFocusSuppressionBoundary::FileDiversity => {
                    ContextFocusCapacityBlocker::FileDiversity
                }
            });
        }
    }
    Some(ContextFocusCapacityBlocker::CandidateGeneration)
}

pub(in crate::ranking) fn build_focus_path_coverage(
    request: &ContextRequest,
    generated: &[GeneratedFocusFacts<'_>],
    path_omitted: &[ScoredCandidate],
    known_omitted: &[ScoredCandidate],
    selected: &[ScoredCandidate],
    limit_omitted: &[ScoredCandidate],
    policy: ContextSelectionPolicy,
) -> Vec<ContextFocusPathCoverage> {
    let minimum_fragments = policy.focus_minimum().unwrap_or(0);
    request
        .focus_paths
        .iter()
        .zip(generated)
        .map(|(pattern, generated)| {
            let matcher = generated.matcher;
            let path_omitted = unique_focus_candidates(path_omitted, matcher).len();
            let known_omitted = unique_focus_candidates(known_omitted, matcher).len();
            let selected_ranges = unique_focus_candidates(selected, matcher);
            let selected_fragments = selected_ranges.len();
            let omitted_ranges = unique_focus_candidates(limit_omitted, matcher);
            let classified_fragments = path_omitted
                .saturating_add(known_omitted)
                .saturating_add(selected_fragments)
                .saturating_add(omitted_ranges.len());
            let deduplicated = generated.fragments.saturating_sub(classified_fragments);
            let selected_source_tokens = selected
                .iter()
                .filter(|candidate| matcher.is_match(&candidate.candidate.path))
                .map(|candidate| candidate.token_count)
                .sum();
            let enforced_focus_minimum = policy.focus_minimum().is_some();
            let reserved_fragments = if enforced_focus_minimum {
                selected_fragments.min(minimum_fragments)
            } else {
                0
            };
            let [token_budget, max_fragments, file_diversity, global_ranking] =
                focus_limit_suppressions(limit_omitted, selected, matcher, request, policy);
            let mut suppressed_by = Vec::new();
            push_focus_suppression(
                &mut suppressed_by,
                ContextFocusSuppressionBoundary::PathPolicy,
                path_omitted,
            );
            push_focus_suppression(
                &mut suppressed_by,
                ContextFocusSuppressionBoundary::KnownHash,
                known_omitted,
            );
            push_focus_suppression(
                &mut suppressed_by,
                ContextFocusSuppressionBoundary::Deduplicated,
                deduplicated,
            );
            push_focus_suppression(
                &mut suppressed_by,
                ContextFocusSuppressionBoundary::TokenBudget,
                token_budget,
            );
            push_focus_suppression(
                &mut suppressed_by,
                ContextFocusSuppressionBoundary::MaxFragments,
                max_fragments,
            );
            push_focus_suppression(
                &mut suppressed_by,
                ContextFocusSuppressionBoundary::FileDiversity,
                file_diversity,
            );
            push_focus_suppression(
                &mut suppressed_by,
                ContextFocusSuppressionBoundary::GlobalRanking,
                global_ranking,
            );
            let satisfied = selected_fragments >= minimum_fragments;
            let diagnostics = Some(ContextFocusPathDiagnostics {
                eligible_paths: 0,
                generated_fragments: generated.fragments,
                generated_symbol_fragments: generated.symbol_fragments,
                reserved_fragments,
                selected_source_tokens,
                capacity_blocker: focus_capacity_blocker(
                    selected_fragments,
                    minimum_fragments,
                    generated.fragments,
                    &suppressed_by,
                ),
                suppressed_by,
            });
            ContextFocusPathCoverage {
                pattern: pattern.clone(),
                indexed_paths: 0,
                minimum_fragments,
                selected_fragments,
                satisfied,
                diagnostics,
            }
        })
        .collect()
}
