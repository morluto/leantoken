pub(super) struct FocusPathResolution {
    pub(super) files: Vec<FileRecord>,
    pub(super) indexed_paths: usize,
    pub(super) eligible_paths: usize,
}

pub(super) struct ContextConstraintExpansion {
    pub(super) coverage: ContextCoverageReceipt,
    pub(super) focus_paths: Vec<FocusPathResolution>,
}

#[derive(Clone, Copy)]
pub(super) struct ConstraintCandidateExpansion<'a> {
    pub(super) session: &'a IndexReadSnapshot,
    pub(super) request: &'a ContextRequest,
    pub(super) queries: &'a [ContextQuery],
    pub(super) path_scorer: &'a ContextPathScorer,
    pub(super) cancellation: &'a CancellationToken,
    pub(super) focus: ContextFocusPolicy,
}

pub(super) struct FocusCandidate {
    pub(super) relevance: f64,
    pub(super) path: String,
    pub(super) start_line: usize,
    pub(super) end_line: usize,
    pub(super) candidate: Candidate,
}

pub(super) struct RequiredEvidenceExcerptPlan {
    pub(super) relevance: f64,
    pub(super) path: String,
    pub(super) file_id: i64,
    pub(super) matched_line: usize,
    pub(super) requirement_index: usize,
}

pub(super) fn retain_required_evidence_plan(
    plans: &mut Vec<RequiredEvidenceExcerptPlan>,
    plan: RequiredEvidenceExcerptPlan,
) {
    if plans
        .iter()
        .any(|existing| existing.path == plan.path && existing.matched_line == plan.matched_line)
    {
        return;
    }
    let insertion = plans
        .binary_search_by(|existing| {
            plan.relevance
                .total_cmp(&existing.relevance)
                .then_with(|| existing.path.cmp(&plan.path))
                .then_with(|| existing.matched_line.cmp(&plan.matched_line))
        })
        .unwrap_or_else(|index| index);
    if insertion < MAX_CONTEXT_FOCUS_CANDIDATES_PER_PATTERN {
        plans.insert(insertion, plan);
        plans.truncate(MAX_CONTEXT_FOCUS_CANDIDATES_PER_PATTERN);
    }
}

pub(super) struct FocusExpansion<'a> {
    pub(super) session: &'a IndexReadSnapshot,
    pub(super) request: &'a ContextRequest,
    pub(super) queries: &'a [ContextQuery],
    pub(super) path_scorer: &'a ContextPathScorer,
    pub(super) resolutions: &'a [FocusPathResolution],
    pub(super) cancellation: &'a CancellationToken,
    pub(super) focus: ContextFocusPolicy,
}

pub(super) fn retain_focus_file(files: &mut Vec<FileRecord>, file: &FileRecord) {
    let insertion = files
        .binary_search_by(|candidate| candidate.path.cmp(&file.path))
        .unwrap_or_else(|index| index);
    if insertion < MAX_CONTEXT_FOCUS_FILES_PER_PATTERN {
        files.insert(insertion, file.clone());
        files.truncate(MAX_CONTEXT_FOCUS_FILES_PER_PATTERN);
    }
}

pub(super) fn retain_required_evidence_files(
    file: &FileRecord,
    matchers: &[PathMatcher],
    path_filter: &PathFilter,
    strict_changed_paths: Option<&HashSet<&str>>,
    path_matches: &mut [usize],
    path_files: &mut [Vec<FileRecord>],
) {
    for ((matcher, matches), files) in matchers
        .iter()
        .zip(path_matches.iter_mut())
        .zip(path_files.iter_mut())
    {
        if !matcher.is_match(&file.path) {
            continue;
        }
        *matches = matches.saturating_add(1);
        if path_filter.allows(&file.path)
            && strict_changed_paths.is_none_or(|paths| paths.contains(file.path.as_str()))
        {
            retain_focus_file(files, file);
        }
    }
}

pub(super) fn focus_text_relevance(text: &str, queries: &[ContextQuery]) -> f64 {
    let normalized = text.to_lowercase();
    queries
        .iter()
        .filter(|query| !query.has_facet(FacetKind::TestIntent))
        .filter_map(|query| {
            normalized
                .contains(&query.value.to_lowercase())
                .then_some(query.weight)
        })
        .sum()
}

pub(super) fn required_evidence_query_matches(text: &str, queries: &[String]) -> Vec<usize> {
    let normalized = text.to_lowercase();
    let mut seen = HashSet::new();
    queries
        .iter()
        .enumerate()
        .filter_map(|(index, query)| {
            let lower = query.to_lowercase();
            if normalized.contains(&lower) && seen.insert(lower) {
                Some(index)
            } else {
                None
            }
        })
        .collect()
}

pub(super) fn retain_ranked_focus_candidate(
    candidates: &mut Vec<FocusCandidate>,
    candidate: FocusCandidate,
) {
    if let Some(existing) = candidates.iter_mut().find(|existing| {
        existing.path == candidate.path
            && existing.start_line == candidate.start_line
            && existing.end_line == candidate.end_line
    }) {
        if candidate.relevance > existing.relevance {
            *existing = candidate;
        }
    } else {
        candidates.push(candidate);
    }
    candidates.sort_by(|left, right| {
        right
            .relevance
            .total_cmp(&left.relevance)
            .then_with(|| left.path.cmp(&right.path))
            .then_with(|| left.start_line.cmp(&right.start_line))
            .then_with(|| left.end_line.cmp(&right.end_line))
    });
    candidates.truncate(MAX_CONTEXT_FOCUS_CANDIDATES_PER_PATTERN);
}
use super::*;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn required_evidence_query_matches_deduplicates_case_equivalent_queries() {
        let text = "ROLLBACK the transaction";
        let queries = vec!["rollback".to_string(), "ROLLBACK".to_string()];
        assert_eq!(required_evidence_query_matches(text, &queries), vec![0],);
    }

    #[test]
    fn required_evidence_query_matches_deduplicates_exact_duplicates() {
        let text = "begin the transaction";
        let queries = vec![
            "begin".to_string(),
            "BEGIN".to_string(),
            "begin".to_string(),
        ];
        assert_eq!(required_evidence_query_matches(text, &queries), vec![0],);
    }

    #[test]
    fn required_evidence_query_matches_preserves_distinct_queries() {
        let text = "begin and rollback the transaction";
        let queries = vec!["begin".to_string(), "rollback".to_string()];
        assert_eq!(required_evidence_query_matches(text, &queries), vec![0, 1],);
    }

    #[test]
    fn required_evidence_query_matches_returns_empty_when_nothing_matches() {
        let text = "select all the rows";
        let queries = vec!["drop".to_string(), "DROP".to_string()];
        assert!(required_evidence_query_matches(text, &queries).is_empty());
    }
}
