fn greedy_select(
    candidates: Vec<ScoredCandidate>,
    budget: usize,
    max_per_file: usize,
    max_fragments: usize,
) -> (Vec<ScoredCandidate>, Vec<ScoredCandidate>) {
    let mut pool = candidates;
    pool.sort_by(compare_utility);
    let confidence_floor = pool.first().map_or(0.0, |candidate| {
        candidate.score * MIN_RELATIVE_CONTEXT_SCORE
    });

    let mut selected = Vec::new();
    let mut deferred = Vec::with_capacity(pool.len());
    let mut omitted: Vec<ScoredCandidate> = Vec::with_capacity(pool.len());
    let mut used_tokens = 0usize;
    let mut file_counts: HashMap<String, usize> = HashMap::new();
    let mut covered_concepts = HashSet::new();
    let mut concept_representations = HashSet::new();
    let mut concept_paths = HashMap::new();

    for candidate in pool {
        let adds_concept = candidate
            .candidate
            .concepts
            .iter()
            .any(|concept| !covered_concepts.contains(concept));
        if !adds_concept || candidate.candidate.concept_weight < 1.0 {
            deferred.push(candidate);
            continue;
        }
        let file_count = *file_counts.get(&candidate.candidate.path).unwrap_or(&0);
        let remaining = budget.saturating_sub(used_tokens);

        if candidate_fits(
            &candidate,
            remaining,
            file_count,
            max_per_file,
            selected.len(),
            max_fragments,
        ) {
            covered_concepts.extend(candidate.candidate.concepts.iter().cloned());
            concept_representations.extend(
                candidate
                    .candidate
                    .concepts
                    .iter()
                    .map(|concept| (concept.clone(), candidate.candidate.representation.clone())),
            );
            for concept in &candidate.candidate.concepts {
                concept_paths
                    .entry(concept.clone())
                    .or_insert_with(|| candidate.candidate.path.clone());
            }
            push_selected(candidate, &mut selected, &mut used_tokens, &mut file_counts);
        } else {
            deferred.push(candidate);
        }
    }

    deferred.sort_by(|left, right| {
        let left_same_path = left.candidate.concepts.iter().any(|concept| {
            concept_paths
                .get(concept)
                .is_some_and(|path| path == &left.candidate.path)
        });
        let right_same_path = right.candidate.concepts.iter().any(|concept| {
            concept_paths
                .get(concept)
                .is_some_and(|path| path == &right.candidate.path)
        });
        right_same_path
            .cmp(&left_same_path)
            .then_with(|| compare_utility(left, right))
    });
    let mut remaining = Vec::with_capacity(deferred.len());
    for candidate in deferred {
        let adds_decisive_view = candidate.candidate.concept_weight >= 1.8
            && candidate.candidate.concepts.iter().any(|concept| {
                covered_concepts.contains(concept)
                    && !concept_representations
                        .contains(&(concept.clone(), candidate.candidate.representation.clone()))
            });
        let file_count = *file_counts.get(&candidate.candidate.path).unwrap_or(&0);
        let remaining_tokens = budget.saturating_sub(used_tokens);
        if adds_decisive_view
            && candidate_fits(
                &candidate,
                remaining_tokens,
                file_count,
                max_per_file,
                selected.len(),
                max_fragments,
            )
        {
            concept_representations.extend(
                candidate
                    .candidate
                    .concepts
                    .iter()
                    .map(|concept| (concept.clone(), candidate.candidate.representation.clone())),
            );
            push_selected(candidate, &mut selected, &mut used_tokens, &mut file_counts);
        } else {
            remaining.push(candidate);
        }
    }

    let mut fill = Vec::with_capacity(remaining.len());
    for candidate in remaining {
        let adds_concept = candidate
            .candidate
            .concepts
            .iter()
            .any(|concept| !covered_concepts.contains(concept));
        let file_count = *file_counts.get(&candidate.candidate.path).unwrap_or(&0);
        let remaining_tokens = budget.saturating_sub(used_tokens);
        let confident =
            candidate.candidate.concept_weight >= 1.0 || candidate.score >= confidence_floor;
        if adds_concept
            && confident
            && candidate_fits(
                &candidate,
                remaining_tokens,
                file_count,
                max_per_file,
                selected.len(),
                max_fragments,
            )
        {
            covered_concepts.extend(candidate.candidate.concepts.iter().cloned());
            push_selected(candidate, &mut selected, &mut used_tokens, &mut file_counts);
        } else {
            fill.push(candidate);
        }
    }

    for candidate in fill {
        if candidate.candidate.concept_weight < 1.0 && candidate.score < confidence_floor {
            omitted.push(candidate);
            continue;
        }
        let file_count = *file_counts.get(&candidate.candidate.path).unwrap_or(&0);
        let remaining = budget.saturating_sub(used_tokens);
        if candidate_fits(
            &candidate,
            remaining,
            file_count,
            max_per_file,
            selected.len(),
            max_fragments,
        ) {
            push_selected(candidate, &mut selected, &mut used_tokens, &mut file_counts);
        } else {
            omitted.push(candidate);
        }
    }

    (selected, omitted)
}

fn candidate_fits(
    candidate: &ScoredCandidate,
    remaining_tokens: usize,
    file_count: usize,
    max_per_file: usize,
    selected_count: usize,
    max_fragments: usize,
) -> bool {
    candidate.token_count <= remaining_tokens
        && file_count < max_per_file
        && selected_count < max_fragments
}

fn push_selected(
    candidate: ScoredCandidate,
    selected: &mut Vec<ScoredCandidate>,
    used_tokens: &mut usize,
    file_counts: &mut HashMap<String, usize>,
) {
    *used_tokens += candidate.token_count;
    *file_counts
        .entry(candidate.candidate.path.clone())
        .or_insert(0) += 1;
    selected.push(candidate);
}

fn compare_utility(a: &ScoredCandidate, b: &ScoredCandidate) -> Ordering {
    let ord = b.score.total_cmp(&a.score);
    if ord != Ordering::Equal {
        return ord;
    }

    let ord = b.marginal_score.total_cmp(&a.marginal_score);
    if ord != Ordering::Equal {
        return ord;
    }

    let ord = a.token_count.cmp(&b.token_count);
    if ord != Ordering::Equal {
        return ord;
    }

    let ord = a.candidate.path.cmp(&b.candidate.path);
    if ord != Ordering::Equal {
        return ord;
    }

    a.candidate.start_line.cmp(&b.candidate.start_line)
}
