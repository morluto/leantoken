use super::*;
pub(in crate::ranking) fn build_context_coverage(
    request: &ContextRequest,
    selected: &[ScoredCandidate],
    known_omitted: &[ScoredCandidate],
) -> ContextCoverageReceipt {
    let covered_candidates = || selected.iter().chain(known_omitted);
    let mut coverage = ContextCoverageReceipt::default();

    for pattern in &request.must_include_paths {
        if covered_candidates()
            .any(|candidate| required_path_matches(&candidate.candidate, pattern))
        {
            coverage.covered_must_include_paths.push(pattern.clone());
        } else {
            coverage.uncovered_must_include_paths.push(pattern.clone());
        }
    }
    for symbol in &request.must_include_symbols {
        if covered_candidates()
            .any(|candidate| required_symbol_satisfied(&candidate.candidate, symbol))
        {
            coverage.covered_must_include_symbols.push(symbol.clone());
        } else if covered_candidates()
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
                    covered_candidates().any(|candidate| {
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
            let selected_fragments = covered_candidates()
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
    coverage
}
