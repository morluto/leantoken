use super::*;
pub(in crate::ranking) fn select_required_candidates(
    mut candidates: Vec<ScoredCandidate>,
    request: &ContextRequest,
    policy: ContextSelectionPolicy,
    budget: usize,
    max_fragments: usize,
) -> (Vec<ScoredCandidate>, Vec<ScoredCandidate>) {
    let mut selected = Vec::new();
    let mut used_tokens = 0usize;

    for (requirement_index, requirement) in request.required_evidence.iter().enumerate() {
        let mut matched_queries = HashSet::new();
        loop {
            for query_index in 0..requirement.queries.len() {
                if selected.iter().any(|candidate: &ScoredCandidate| {
                    required_evidence_query(&candidate.candidate, requirement_index, query_index)
                }) {
                    matched_queries.insert(query_index);
                }
            }
            if matched_queries.len() >= requirement.minimum_query_matches
                || selected.len() == max_fragments
            {
                break;
            }
            let remaining = budget.saturating_sub(used_tokens);
            let Some(index) = candidates.iter().position(|candidate| {
                candidate.token_count <= remaining
                    && (0..requirement.queries.len()).any(|query_index| {
                        !matched_queries.contains(&query_index)
                            && required_evidence_query(
                                &candidate.candidate,
                                requirement_index,
                                query_index,
                            )
                    })
            }) else {
                break;
            };
            let candidate = candidates.remove(index);
            used_tokens = used_tokens.saturating_add(candidate.token_count);
            selected.push(candidate);
        }
    }

    for pattern in &request.must_include_paths {
        if selected
            .iter()
            .any(|candidate: &ScoredCandidate| required_path_matches(&candidate.candidate, pattern))
        {
            continue;
        }
        let remaining = budget.saturating_sub(used_tokens);
        let Some(index) = candidates.iter().position(|candidate| {
            required_path_matches(&candidate.candidate, pattern)
                && candidate.token_count <= remaining
        }) else {
            continue;
        };
        if selected.len() == max_fragments {
            break;
        }
        let candidate = candidates.remove(index);
        used_tokens = used_tokens.saturating_add(candidate.token_count);
        selected.push(candidate);
    }

    for symbol in &request.must_include_symbols {
        if selected
            .iter()
            .any(|candidate| required_symbol_matches(&candidate.candidate, symbol))
        {
            continue;
        }
        let remaining = budget.saturating_sub(used_tokens);
        let Some(index) = candidates.iter().position(|candidate| {
            required_symbol_matches(&candidate.candidate, symbol)
                && candidate.token_count <= remaining
        }) else {
            continue;
        };
        if selected.len() == max_fragments {
            break;
        }
        let candidate = candidates.remove(index);
        used_tokens = used_tokens.saturating_add(candidate.token_count);
        selected.push(candidate);
    }

    if let Some(minimum_focus_fragments) = policy.focus_minimum().filter(|minimum| *minimum > 0) {
        for pattern in &request.focus_paths {
            while selected
                .iter()
                .filter(|candidate| required_path_matches(&candidate.candidate, pattern))
                .count()
                < minimum_focus_fragments
            {
                if selected.len() == max_fragments {
                    break;
                }
                let remaining = budget.saturating_sub(used_tokens);
                let Some(index) = candidates.iter().position(|candidate| {
                    required_path_matches(&candidate.candidate, pattern)
                        && candidate.token_count <= remaining
                }) else {
                    break;
                };
                let candidate = candidates.remove(index);
                used_tokens = used_tokens.saturating_add(candidate.token_count);
                selected.push(candidate);
            }
        }
    }

    (selected, candidates)
}

pub(in crate::ranking) fn required_path_matches(candidate: &Candidate, pattern: &str) -> bool {
    path_matches(&candidate.path, pattern).unwrap_or(false)
}

pub(in crate::ranking) fn required_symbol_matches(candidate: &Candidate, symbol: &str) -> bool {
    candidate
        .symbol_name
        .as_deref()
        .is_some_and(|name| name == symbol)
        && candidate.target_range.is_some()
}

pub(in crate::ranking) fn required_symbol_satisfied(candidate: &Candidate, symbol: &str) -> bool {
    required_symbol_matches(candidate, symbol) && !candidate.target_truncated()
}

pub(in crate::ranking) fn apply_request_signals(
    candidates: &mut [Candidate],
    request: &ContextRequest,
    focus_paths: &PathMatcher,
) {
    for candidate in candidates {
        if focus_paths.is_match(&candidate.path) {
            candidate.focus_boost += 1.0;
        }

        if let Some(ref name) = candidate.symbol_name {
            for focus_symbol in &request.focus_symbols {
                if focus_symbol == name {
                    candidate.focus_boost += 1.0;
                    break;
                }
            }
        }
    }
}
