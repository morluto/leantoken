use super::*;
pub(in crate::ranking) fn greedy_select(
    candidates: Vec<ScoredCandidate>,
    budget: usize,
    max_per_file: usize,
    max_fragments: usize,
    initial_file_counts: HashMap<String, usize>,
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
    let mut file_counts: HashMap<String, usize> = initial_file_counts;
    let mut covered_concepts = HashSet::new();
    let mut concept_representations = HashSet::new();
    let mut concept_paths = HashMap::new();
    let mut evidence_counts = EvidenceQuotaCounts::default();

    let primary_candidate_fits = |candidate: &ScoredCandidate, path_class| {
        context_path_class(&candidate.candidate.path) == path_class
            && evidence_counts.allows(candidate, max_fragments)
            && candidate_fits(
                candidate,
                budget,
                *file_counts.get(&candidate.candidate.path).unwrap_or(&0),
                max_per_file,
                selected.len(),
                max_fragments,
            )
    };
    let primary_position_for = |path_class| {
        let specific_baseline = pool.iter().find(|candidate| {
            primary_candidate_fits(candidate, path_class)
                && carries_specific_primary_change(&candidate.candidate)
        });
        specific_baseline
            .and_then(|baseline| {
                let owner_path = pool
                    .iter()
                    .enumerate()
                    .filter(|(_, candidate)| {
                        primary_candidate_fits(candidate, path_class)
                            && carries_specific_primary_change(&candidate.candidate)
                            && candidate.candidate.representation
                                == baseline.candidate.representation
                            && carries_all_facet_values(
                                &candidate.candidate,
                                &baseline.candidate,
                                "primary_change",
                            )
                    })
                    .max_by_key(|(position, candidate)| {
                        (
                            facet_value_count(&candidate.candidate, "primary_change"),
                            std::cmp::Reverse(*position),
                        )
                    })
                    .map(|(_, candidate)| candidate.candidate.path.as_str())?;
                pool.iter().position(|candidate| {
                    candidate.candidate.path == owner_path
                        && primary_candidate_fits(candidate, path_class)
                        && carries_specific_primary_change(&candidate.candidate)
                })
            })
            .or_else(|| {
                let baseline = pool.iter().find(|candidate| {
                    primary_candidate_fits(candidate, path_class)
                        && carries_facet(&candidate.candidate, "primary_change")
                })?;
                let owner_path = pool
                    .iter()
                    .enumerate()
                    .filter(|(_, candidate)| {
                        primary_candidate_fits(candidate, path_class)
                            && carries_facet(&candidate.candidate, "primary_change")
                            && candidate.candidate.representation
                                == baseline.candidate.representation
                            && carries_all_facet_values(
                                &candidate.candidate,
                                &baseline.candidate,
                                "primary_change",
                            )
                    })
                    .max_by_key(|(position, candidate)| {
                        (
                            facet_value_count(&candidate.candidate, "primary_change"),
                            std::cmp::Reverse(*position),
                        )
                    })
                    .map(|(_, candidate)| candidate.candidate.path.as_str())?;
                pool.iter().position(|candidate| {
                    candidate.candidate.path == owner_path
                        && primary_candidate_fits(candidate, path_class)
                        && carries_facet(&candidate.candidate, "primary_change")
                })
            })
    };
    let primary_position = primary_position_for(ContextPathClass::Production)
        .or_else(|| primary_position_for(ContextPathClass::Supporting));
    let mut primary_owner = None;
    if let Some(position) = primary_position {
        let candidate = pool.remove(position);
        primary_owner = Some((
            candidate.candidate.path.clone(),
            candidate.candidate.concepts.clone(),
        ));
        record_context_selection(
            &candidate,
            &mut covered_concepts,
            &mut concept_representations,
            &mut concept_paths,
            &mut evidence_counts,
        );
        push_selected(candidate, &mut selected, &mut used_tokens, &mut file_counts);
    }

    let eligible_tests = pool
        .iter()
        .enumerate()
        .filter(|(_, candidate)| {
            context_path_class(&candidate.candidate.path) == ContextPathClass::Test
                && (carries_specific_exact_atom(&candidate.candidate)
                    || primary_owner.as_ref().is_some_and(|(path, _)| {
                        owner_test_path_affinity(path, &candidate.candidate.path) > 0
                    })
                    || candidate
                        .candidate
                        .concepts
                        .iter()
                        .any(|concept| covered_concepts.contains(concept)))
                && evidence_counts.allows(candidate, max_fragments)
                && candidate_fits(
                    candidate,
                    budget.saturating_sub(used_tokens),
                    *file_counts.get(&candidate.candidate.path).unwrap_or(&0),
                    max_per_file,
                    selected.len(),
                    max_fragments,
                )
        })
        .collect::<Vec<_>>();
    let test_position = eligible_tests
        .iter()
        .map(|(position, candidate)| {
            let owner_affinity = primary_owner.as_ref().map_or(0, |(path, _)| {
                owner_test_path_affinity(path, &candidate.candidate.path)
            });
            (
                *position,
                owner_affinity,
                usize::from(carries_specific_exact_atom(&candidate.candidate)),
            )
        })
        .max_by_key(|(position, owner_affinity, exact)| {
            (
                usize::from(*owner_affinity >= 3),
                *exact,
                *owner_affinity,
                std::cmp::Reverse(*position),
            )
        })
        .map(|(position, _, _)| position);
    if let Some(position) = test_position {
        let candidate = pool.remove(position);
        record_context_selection(
            &candidate,
            &mut covered_concepts,
            &mut concept_representations,
            &mut concept_paths,
            &mut evidence_counts,
        );
        push_selected(candidate, &mut selected, &mut used_tokens, &mut file_counts);
    }

    if let Some(position) = pool.iter().position(|candidate| {
        context_path_class(&candidate.candidate.path) != ContextPathClass::Auxiliary
            && carries_facet(&candidate.candidate, "preserve_constraint")
            && evidence_counts.allows(candidate, max_fragments)
            && candidate_fits(
                candidate,
                budget.saturating_sub(used_tokens),
                *file_counts.get(&candidate.candidate.path).unwrap_or(&0),
                max_per_file,
                selected.len(),
                max_fragments,
            )
    }) {
        let candidate = pool.remove(position);
        record_context_selection(
            &candidate,
            &mut covered_concepts,
            &mut concept_representations,
            &mut concept_paths,
            &mut evidence_counts,
        );
        push_selected(candidate, &mut selected, &mut used_tokens, &mut file_counts);
    }

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

        if evidence_counts.allows(&candidate, max_fragments)
            && candidate_fits(
                &candidate,
                remaining,
                file_count,
                max_per_file,
                selected.len(),
                max_fragments,
            )
        {
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
            evidence_counts.record(&candidate);
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
            && evidence_counts.allows(&candidate, max_fragments)
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
            evidence_counts.record(&candidate);
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
            && evidence_counts.allows(&candidate, max_fragments)
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
            evidence_counts.record(&candidate);
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
        if evidence_counts.allows(&candidate, max_fragments)
            && candidate_fits(
                &candidate,
                remaining,
                file_count,
                max_per_file,
                selected.len(),
                max_fragments,
            )
        {
            evidence_counts.record(&candidate);
            push_selected(candidate, &mut selected, &mut used_tokens, &mut file_counts);
        } else {
            omitted.push(candidate);
        }
    }

    (selected, omitted)
}

#[derive(Default)]
struct EvidenceQuotaCounts {
    auxiliary: usize,
    failures: usize,
    tests: usize,
    preserve: usize,
}

fn record_context_selection(
    candidate: &ScoredCandidate,
    covered_concepts: &mut HashSet<String>,
    concept_representations: &mut HashSet<(String, String)>,
    concept_paths: &mut HashMap<String, String>,
    evidence_counts: &mut EvidenceQuotaCounts,
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
    evidence_counts.record(candidate);
}

impl EvidenceQuotaCounts {
    fn allows(&self, candidate: &ScoredCandidate, max_fragments: usize) -> bool {
        let auxiliary_limit = usize::from(max_fragments > 0);
        let failure_limit = max_fragments.min(2);
        let test_limit = max_fragments.min(2);
        let preserve_limit = max_fragments.min(2);
        let path_class = context_path_class(&candidate.candidate.path);
        (path_class != ContextPathClass::Auxiliary || self.auxiliary < auxiliary_limit)
            && (!carries_facet(&candidate.candidate, "failure_trace")
                || self.failures < failure_limit)
            && (path_class != ContextPathClass::Test || self.tests < test_limit)
            && (!carries_facet(&candidate.candidate, "preserve_constraint")
                || self.preserve < preserve_limit)
    }

    fn record(&mut self, candidate: &ScoredCandidate) {
        match context_path_class(&candidate.candidate.path) {
            ContextPathClass::Auxiliary => self.auxiliary += 1,
            ContextPathClass::Test => self.tests += 1,
            ContextPathClass::Production | ContextPathClass::Supporting => {}
        }
        if carries_facet(&candidate.candidate, "preserve_constraint") {
            self.preserve += 1;
        }
        if carries_facet(&candidate.candidate, "failure_trace") {
            self.failures += 1;
        }
    }
}

pub(in crate::ranking) fn candidate_fits(
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

pub(in crate::ranking) fn push_selected(
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

pub(in crate::ranking) fn compare_utility(a: &ScoredCandidate, b: &ScoredCandidate) -> Ordering {
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
