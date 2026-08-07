use super::*;
/// Remove content-identical candidates and candidates whose line ranges
/// overlap the same file by at least the module's overlap threshold. The higher-scored
/// copy is kept.
#[must_use]
pub fn deduplicate(candidates: Vec<ScoredCandidate>) -> Vec<ScoredCandidate> {
    deduplicate_with_options(candidates, &Weights::default())
}

pub(crate) fn deduplicate_with_options(
    candidates: Vec<ScoredCandidate>,
    weights: &Weights,
) -> Vec<ScoredCandidate> {
    let mut sorted = candidates;
    sorted.sort_by(|a, b| {
        let ord = b.candidate.exact.total_cmp(&a.candidate.exact);
        if ord != Ordering::Equal {
            return ord;
        }
        let ord = b.score.total_cmp(&a.score);
        if ord != Ordering::Equal {
            return ord;
        }
        let ord = a.candidate.path.cmp(&b.candidate.path);
        if ord != Ordering::Equal {
            return ord;
        }
        a.candidate.start_line.cmp(&b.candidate.start_line)
    });

    let mut kept: Vec<ScoredCandidate> = Vec::with_capacity(sorted.len());
    let mut seen_hashes: HashMap<(String, String), usize> = HashMap::new();
    let mut kept_by_path: HashMap<String, Vec<usize>> = HashMap::new();

    for candidate in sorted {
        let hash_key = (
            candidate.candidate.path.clone(),
            candidate.content_hash.clone(),
        );
        if let Some(existing) = seen_hashes.get(&hash_key).copied() {
            merge_scored_candidate(&mut kept[existing], &candidate, weights);
            continue;
        }

        let candidate_lines = candidate.candidate.line_count();
        let duplicate = kept_by_path
            .get(&candidate.candidate.path)
            .and_then(|indices| {
                indices.iter().copied().find(|&index| {
                    let existing = &kept[index];

                    // Non-overlapping ranges cannot be duplicates.
                    if candidate.candidate.end_line < existing.candidate.start_line
                        || candidate.candidate.start_line > existing.candidate.end_line
                    {
                        return false;
                    }

                    let overlap_start = candidate
                        .candidate
                        .start_line
                        .max(existing.candidate.start_line);
                    let overlap_end = candidate
                        .candidate
                        .end_line
                        .min(existing.candidate.end_line);
                    let overlap_lines = overlap_end - overlap_start + 1;
                    let min_lines = candidate_lines.min(existing.candidate.line_count());

                    overlap_lines >= min_lines.div_ceil(2)
                        && overlapping_provenance_is_compatible(
                            &existing.candidate,
                            &candidate.candidate,
                        )
                })
            });
        if let Some(existing) = duplicate {
            merge_scored_candidate(&mut kept[existing], &candidate, weights);
            continue;
        }

        let kept_index = kept.len();
        seen_hashes.insert(hash_key, kept_index);
        kept_by_path
            .entry(candidate.candidate.path.clone())
            .or_default()
            .push(kept_index);
        kept.push(candidate);
    }

    kept.sort_by(|a, b| {
        b.score
            .total_cmp(&a.score)
            .then_with(|| a.candidate.path.cmp(&b.candidate.path))
            .then_with(|| a.candidate.start_line.cmp(&b.candidate.start_line))
    });
    kept
}

fn overlapping_provenance_is_compatible(retained: &Candidate, secondary: &Candidate) -> bool {
    // Required-evidence markers attest to literals in one fragment's content.
    // Keep both overlapping fragments rather than transferring an unverified marker.
    secondary
        .match_kinds
        .iter()
        .filter(|kind| kind.starts_with(REQUIRED_EVIDENCE_PREFIX))
        .all(|kind| retained.match_kinds.contains(kind))
}

pub(in crate::ranking) fn merge_scored_candidate(
    existing: &mut ScoredCandidate,
    duplicate: &ScoredCandidate,
    weights: &Weights,
) {
    merge_candidate_signals(&mut existing.candidate, &duplicate.candidate);
    existing.score = existing.candidate.score(weights, existing.token_count);
    existing.marginal_score = existing.score / bounded_count_f64(existing.token_count);
}

pub(in crate::ranking) fn merge_candidate_signals(existing: &mut Candidate, duplicate: &Candidate) {
    for kind in &duplicate.match_kinds {
        if !existing.match_kinds.contains(kind) {
            existing.match_kinds.push(kind.clone());
        }
    }
    for concept in &duplicate.concepts {
        if !existing.concepts.contains(concept) {
            existing.concepts.push(concept.clone());
        }
    }
    existing.concept_weight = existing.concept_weight.max(duplicate.concept_weight);
    if existing.target_start_line.is_none() && duplicate.target_start_line.is_some() {
        existing.symbol_name.clone_from(&duplicate.symbol_name);
        existing.target_start_line = duplicate.target_start_line;
        existing.target_end_line = duplicate.target_end_line;
        existing
            .representation
            .clone_from(&duplicate.representation);
    } else if existing.symbol_name.is_none() {
        existing.symbol_name.clone_from(&duplicate.symbol_name);
    }
    existing.exact = existing.exact.max(duplicate.exact);
    existing.symbol = existing.symbol.max(duplicate.symbol);
    existing.reference = existing.reference.max(duplicate.reference);
    existing.bm25 = existing.bm25.max(duplicate.bm25);
    existing.path_score = existing.path_score.max(duplicate.path_score);
    existing.lexical_frequency_penalty = existing
        .lexical_frequency_penalty
        .min(duplicate.lexical_frequency_penalty);
    existing.size_score = existing.size_score.max(duplicate.size_score);
    existing.focus_boost = existing.focus_boost.max(duplicate.focus_boost);
    existing.import_boost = existing.import_boost.max(duplicate.import_boost);
    existing.change_boost = existing.change_boost.max(duplicate.change_boost);
}
