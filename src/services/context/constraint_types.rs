struct FocusPathResolution {
    files: Vec<FileRecord>,
    indexed_paths: usize,
    eligible_paths: usize,
}

struct ContextConstraintExpansion {
    coverage: ContextCoverageReceipt,
    focus_paths: Vec<FocusPathResolution>,
}

#[derive(Clone, Copy)]
struct ConstraintCandidateExpansion<'a> {
    session: &'a ReadSession,
    request: &'a ContextRequest,
    queries: &'a [ContextQuery],
    path_scorer: &'a ContextPathScorer,
    cancellation: &'a CancellationToken,
}

struct FocusCandidate {
    relevance: f64,
    path: String,
    start_line: usize,
    end_line: usize,
    candidate: Candidate,
}

struct RequiredEvidenceExcerptPlan {
    relevance: f64,
    path: String,
    file_id: i64,
    matched_line: usize,
    requirement_index: usize,
}

fn retain_required_evidence_plan(
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

struct FocusExpansion<'a> {
    session: &'a ReadSession,
    request: &'a ContextRequest,
    queries: &'a [ContextQuery],
    path_scorer: &'a ContextPathScorer,
    resolutions: &'a [FocusPathResolution],
    cancellation: &'a CancellationToken,
}

fn retain_focus_file(files: &mut Vec<FileRecord>, file: &FileRecord) {
    let insertion = files
        .binary_search_by(|candidate| candidate.path.cmp(&file.path))
        .unwrap_or_else(|index| index);
    if insertion < MAX_CONTEXT_FOCUS_FILES_PER_PATTERN {
        files.insert(insertion, file.clone());
        files.truncate(MAX_CONTEXT_FOCUS_FILES_PER_PATTERN);
    }
}

fn retain_required_evidence_files(
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

fn focus_text_relevance(text: &str, queries: &[ContextQuery]) -> f64 {
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

fn required_evidence_query_matches(text: &str, queries: &[String]) -> Vec<usize> {
    let normalized = text.to_lowercase();
    queries
        .iter()
        .enumerate()
        .filter_map(|(index, query)| normalized.contains(&query.to_lowercase()).then_some(index))
        .collect()
}

fn retain_ranked_focus_candidate(candidates: &mut Vec<FocusCandidate>, candidate: FocusCandidate) {
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
