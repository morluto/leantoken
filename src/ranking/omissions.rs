const OVERLAP_THRESHOLD: f64 = 0.5;

/// Divisor for the per-file diversity cap. A 1,200-token context may include
/// two non-overlapping regions from one file, while tiny budgets still prefer
/// breadth.
const DIVERSITY_DIVISOR: usize = 600;
const MAX_OMITTED_DETAILS: usize = 1;
const MAX_OMISSION_FACETS: usize = 12;
const MIN_RELATIVE_CONTEXT_SCORE: f64 = 0.25;

fn increment_facet(counts: &mut HashMap<String, usize>, value: impl Into<String>) {
    let count = counts.entry(value.into()).or_default();
    *count = count.saturating_add(1);
}

fn bounded_facets(counts: HashMap<String, usize>) -> Vec<ContextOmissionFacet> {
    let mut facets = counts
        .into_iter()
        .map(|(value, count)| ContextOmissionFacet { value, count })
        .collect::<Vec<_>>();
    facets.sort_by(|left, right| {
        right
            .count
            .cmp(&left.count)
            .then_with(|| left.value.cmp(&right.value))
    });
    if facets.len() > MAX_OMISSION_FACETS {
        let other = facets
            .drain(MAX_OMISSION_FACETS - 1..)
            .map(|facet| facet.count)
            .sum();
        facets.push(ContextOmissionFacet {
            value: "[other]".into(),
            count: other,
        });
    }
    facets
}

fn candidate_file_type(path: &str) -> String {
    Path::new(path)
        .extension()
        .and_then(|extension| extension.to_str())
        .filter(|extension| !extension.is_empty())
        .map_or_else(
            || "[no extension]".into(),
            |extension| format!(".{}", extension.to_ascii_lowercase()),
        )
}

fn score_band(score: f64) -> &'static str {
    if score >= 1.0 {
        "score >= 1.0"
    } else if score >= 0.5 {
        "0.5 <= score < 1.0"
    } else if score > 0.0 {
        "0 < score < 0.5"
    } else {
        "score = 0"
    }
}

fn summarize_omissions(
    path_omitted: &[ScoredCandidate],
    known_omitted: &[ScoredCandidate],
    limit_omitted: &[ScoredCandidate],
    prefiltered_path_omissions: &[String],
    focus_paths: &PathMatcher,
    changed_paths: &HashSet<&str>,
    verbose_diagnostics: bool,
) -> ContextOmissionSummary {
    let path_excluded = path_omitted
        .len()
        .saturating_add(prefiltered_path_omissions.len());
    let known_hash = known_omitted.len();
    let budget_or_result_limit = limit_omitted.len();
    if !verbose_diagnostics {
        return ContextOmissionSummary {
            path_excluded,
            known_hash,
            budget_or_result_limit,
            ..ContextOmissionSummary::default()
        };
    }

    let mut paths = HashMap::new();
    let mut file_types = HashMap::new();
    let mut score_bands = HashMap::new();
    let mut focused = 0usize;
    let mut changed = 0usize;

    let mut record = |path: &str, score: Option<f64>| {
        increment_facet(&mut paths, path);
        increment_facet(&mut file_types, candidate_file_type(path));
        increment_facet(&mut score_bands, score.map_or("not scored", score_band));
        focused = focused.saturating_add(usize::from(focus_paths.is_match(path)));
        changed = changed.saturating_add(usize::from(changed_paths.contains(path)));
    };
    for candidate in path_omitted
        .iter()
        .chain(known_omitted)
        .chain(limit_omitted)
    {
        record(&candidate.candidate.path, Some(candidate.score));
    }
    for path in prefiltered_path_omissions {
        record(path, None);
    }

    let total = path_excluded
        .saturating_add(known_hash)
        .saturating_add(budget_or_result_limit);
    let mut reasons = HashMap::new();
    if path_excluded > 0 {
        reasons.insert("path_excluded".into(), path_excluded);
    }
    if known_hash > 0 {
        reasons.insert("known_hash".into(), known_hash);
    }
    if budget_or_result_limit > 0 {
        reasons.insert("budget_or_result_limit".into(), budget_or_result_limit);
    }

    ContextOmissionSummary {
        path_excluded,
        known_hash,
        budget_or_result_limit,
        by_path: bounded_facets(paths),
        by_language_or_file_type: bounded_facets(file_types),
        by_reason: bounded_facets(reasons),
        by_score_band: bounded_facets(score_bands),
        focused,
        not_focused: total.saturating_sub(focused),
        changed,
        not_changed: total.saturating_sub(changed),
    }
}
