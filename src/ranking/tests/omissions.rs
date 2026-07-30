use super::*;

#[test]
fn known_hash_omitted_and_reported() {
    let c = Candidate::new("known.rs", 1, 2, "alpha beta").exact(1.0);
    let hash = c.content_hash();

    let mut req = request_with_budget(10);
    req.known_hashes.push(hash);
    req.verbose_diagnostics = true;

    let resp = select(vec![c], &req, 1);

    assert!(resp.fragments.is_empty());
    assert_eq!(resp.omitted.len(), 1);
    assert_eq!(resp.omitted[0].reason, "known hash");
}

#[test]
fn exclude_paths_filter_candidates() {
    let kept = Candidate::new("src/lib.rs", 1, 2, "alpha").exact(1.0);
    let excluded = Candidate::new("test/ranking.rs", 1, 2, "beta").exact(1.0);

    let mut req = request_excluding(10, "test");
    req.verbose_diagnostics = true;
    let resp = select(vec![kept, excluded], &req, 1);

    assert_eq!(resp.fragments.len(), 1);
    assert_eq!(resp.fragments[0].path, "src/lib.rs");
    assert_eq!(resp.omission_summary.path_excluded, 1);
    assert_eq!(resp.omitted[0].reason, "path excluded");
}

#[test]
fn omission_summary_reports_bounded_coverage_facets() {
    let selected = Candidate::new("src/selected.rs", 1, 2, "selected").exact(3.0);
    let limited = Candidate::new("src/changed.rs", 1, 2, "limited").exact(2.0);
    let known = Candidate::new("src/known.md", 1, 2, "known").exact(1.0);
    let known_hash = known.content_hash();
    let excluded = Candidate::new("tests/helper.ts", 1, 2, "excluded").exact(1.0);
    let mut request = request_with_budget(100);
    request.max_fragments = Some(1);
    request.focus_paths = vec!["src/**".into()];
    request.changed_paths = vec!["src/changed.rs".into()];
    request.exclude_paths = vec!["tests/**".into()];
    request.known_hashes = vec![known_hash];
    request.verbose_diagnostics = true;

    let candidates = vec![selected, limited, known, excluded];
    let mut compact_request = request.clone();
    compact_request.verbose_diagnostics = false;
    let compact = select_with_tokenizer_and_context_exclusions(
        candidates.clone(),
        &compact_request,
        1,
        tokens::Tokenizer::default(),
        &[],
        &["generated/tool.js".into()],
    );
    assert!(compact.omitted.is_empty());
    assert!(compact.omission_summary.by_reason.is_empty());
    assert_eq!(compact.omission_summary.path_excluded, 2);

    let response = select_with_tokenizer_and_context_exclusions(
        candidates,
        &request,
        1,
        tokens::Tokenizer::default(),
        &[],
        &["generated/tool.js".into()],
    );

    assert_eq!(response.omitted.len(), MAX_OMITTED_DETAILS);
    let summary = response.omission_summary;
    assert_eq!(summary.path_excluded, 2);
    assert_eq!(summary.known_hash, 1);
    assert_eq!(summary.budget_or_result_limit, 1);
    assert_eq!(summary.focused, 2);
    assert_eq!(summary.not_focused, 2);
    assert_eq!(summary.changed, 1);
    assert_eq!(summary.not_changed, 3);
    assert!(summary.by_reason.contains(&ContextOmissionFacet {
        value: "path_excluded".into(),
        count: 2,
    }));
    assert!(
        summary
            .by_language_or_file_type
            .iter()
            .any(|facet| facet.value == ".js" && facet.count == 1)
    );
    assert!(
        summary
            .by_score_band
            .iter()
            .any(|facet| facet.value == "not scored" && facet.count == 1)
    );
    assert_eq!(
        summary
            .by_path
            .iter()
            .map(|facet| facet.count)
            .sum::<usize>(),
        4
    );
}

#[test]
fn compact_omission_diagnostics_preserve_selection_with_lower_response_cost() {
    let mut candidates = (0..20)
        .map(|index| {
            Candidate::new(
                format!("src/module_{index:02}.rs"),
                1,
                2,
                format!("fn candidate_{index}() {{}}"),
            )
            .exact((20 - index) as f64)
        })
        .collect::<Vec<_>>();
    let known = Candidate::new("src/known.md", 1, 2, "known").exact(1.0);
    let known_hash = known.content_hash();
    candidates.push(known);
    candidates.push(Candidate::new("tests/excluded.rs", 1, 2, "excluded").exact(1.0));
    let mut compact_request = request_with_budget(100);
    compact_request.max_fragments = Some(1);
    compact_request.exclude_paths = vec!["tests/**".into()];
    compact_request.known_hashes = vec![known_hash];
    let mut verbose_request = compact_request.clone();
    verbose_request.verbose_diagnostics = true;

    let compact = select(candidates.clone(), &compact_request, 1);
    let verbose = select(candidates, &verbose_request, 1);

    assert_eq!(
        compact
            .fragments
            .iter()
            .map(|fragment| (&fragment.path, &fragment.content_hash))
            .collect::<Vec<_>>(),
        verbose
            .fragments
            .iter()
            .map(|fragment| (&fragment.path, &fragment.content_hash))
            .collect::<Vec<_>>()
    );
    assert_eq!(
        compact.receipt.task_fingerprint,
        verbose.receipt.task_fingerprint
    );
    assert_eq!(
        compact.receipt.fragment_hashes,
        verbose.receipt.fragment_hashes
    );
    assert_eq!(compact.coverage, verbose.coverage);
    assert_eq!(compact.warnings, verbose.warnings);
    assert!(compact.omitted.is_empty());
    assert_eq!(verbose.omitted.len(), MAX_OMITTED_DETAILS);
    let compact_counts = (
        compact.omission_summary.path_excluded,
        compact.omission_summary.known_hash,
        compact.omission_summary.budget_or_result_limit,
    );
    let verbose_counts = (
        verbose.omission_summary.path_excluded,
        verbose.omission_summary.known_hash,
        verbose.omission_summary.budget_or_result_limit,
    );
    assert_eq!(compact_counts, (1, 1, 19));
    assert_eq!(compact_counts, verbose_counts);
    assert!(compact.omission_summary.by_path.is_empty());
    assert!(compact.omission_summary.by_language_or_file_type.is_empty());
    assert!(compact.omission_summary.by_reason.is_empty());
    assert!(compact.omission_summary.by_score_band.is_empty());
    assert_eq!(compact.omission_summary.focused, 0);
    assert_eq!(compact.omission_summary.not_focused, 0);
    assert_eq!(compact.omission_summary.changed, 0);
    assert_eq!(compact.omission_summary.not_changed, 0);
    assert!(!verbose.omission_summary.by_path.is_empty());
    assert!(
        compact.meta.total_response_tokens < verbose.meta.total_response_tokens,
        "compact={} verbose={}",
        compact.meta.total_response_tokens,
        verbose.meta.total_response_tokens
    );

    let serialized =
        serde_json::to_value(&compact.omission_summary).expect("compact diagnostics JSON");
    let object = serialized.as_object().expect("diagnostics object");
    assert_eq!(
        object
            .get("budget_or_result_limit")
            .and_then(serde_json::Value::as_u64),
        Some(19)
    );
    assert!(!object.contains_key("by_path"));
    assert!(!object.contains_key("not_focused"));
}

#[test]
fn omission_facets_fold_long_tails_into_other() {
    let counts = (0..20)
        .map(|index| (format!("path-{index:02}"), 1))
        .collect();

    let facets = bounded_facets(counts);

    assert_eq!(facets.len(), MAX_OMISSION_FACETS);
    assert_eq!(facets.last().expect("other").value, "[other]");
    assert_eq!(facets.last().expect("other").count, 9);
    assert_eq!(facets.iter().map(|facet| facet.count).sum::<usize>(), 20);
}

#[test]
fn include_paths_are_a_hard_fragment_boundary() {
    let included = Candidate::new("src/browser/capture.rs", 1, 2, "alpha").exact(0.5);
    let unrelated = Candidate::new("src/managed/evidence.rs", 1, 2, "beta").exact(10.0);
    let mut request = request_with_budget(10);
    request.include_paths = vec!["src/browser/**".into()];
    request.verbose_diagnostics = true;

    let response = select(vec![unrelated, included], &request, 1);

    assert_eq!(response.fragments.len(), 1);
    assert_eq!(response.fragments[0].path, "src/browser/capture.rs");
    assert_eq!(response.omission_summary.path_excluded, 1);
    assert_eq!(response.omitted[0].reason, "path excluded");
}

#[test]
fn generated_context_defaults_require_an_explicit_include() {
    let generated =
        Candidate::new("artifacts/runtime_reports/latest.json", 1, 2, "generated").exact(10.0);
    let source = Candidate::new("src/runtime.rs", 1, 2, "source").exact(0.5);
    let request = request_with_budget(20);

    let response = select(vec![generated.clone(), source], &request, 1);

    assert_eq!(response.fragments.len(), 1);
    assert_eq!(response.fragments[0].path, "src/runtime.rs");
    assert_eq!(response.omission_summary.path_excluded, 1);

    let mut included_request = request_with_budget(20);
    included_request.include_paths = vec!["artifacts/runtime_reports/**".into()];
    let included = select(vec![generated], &included_request, 1);

    assert_eq!(included.fragments.len(), 1);
    assert_eq!(
        included.fragments[0].path,
        "artifacts/runtime_reports/latest.json"
    );
}
