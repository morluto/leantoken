use super::*;

#[test]
fn context_plan_matches_materialized_selection_without_source() {
    let focused = Candidate::new("src/ranking.rs", 10, 12, "focused evidence")
        .match_kind("symbol")
        .exact(2.0);
    let other = Candidate::new("src/other.rs", 20, 21, "other evidence").match_kind("text");
    let candidates = vec![other, focused];
    let mut request = request_focused(100, "src/ranking.rs");
    request.max_fragments = Some(1);
    request.plan_only = true;

    let preview = select(candidates.clone(), &request, 7);
    let plan = preview.plan.as_ref().expect("query plan");

    assert!(preview.fragments.is_empty());
    assert!(preview.receipt.fragment_hashes.is_empty());
    assert_eq!(preview.meta.source_tokens, 0);
    assert!(!plan.candidates.is_empty());
    assert_eq!(plan.candidates.len(), 1);
    assert!(!plan.result_complete);
    assert!(
        plan.candidates
            .iter()
            .all(|candidate| candidate.score >= 0.0)
    );
    assert!(
        plan.candidates
            .iter()
            .all(|candidate| !candidate.reasons.is_empty())
    );
    assert_eq!(
        plan.estimated_source_tokens,
        plan.candidates
            .iter()
            .map(|candidate| candidate.estimated_tokens)
            .sum::<usize>()
    );
    assert_eq!(plan.focus_coverage.len(), 1);
    assert!(plan.focus_coverage[0].satisfied);

    request.plan_only = false;
    let materialized = select(candidates, &request, 7);
    assert!(materialized.plan.is_none());
    assert_eq!(
        plan.candidates
            .iter()
            .map(|candidate| (&candidate.path, candidate.start_line, candidate.end_line))
            .collect::<Vec<_>>(),
        materialized
            .fragments
            .iter()
            .map(|fragment| (&fragment.path, fragment.start_line, fragment.end_line))
            .collect::<Vec<_>>()
    );
    assert_eq!(
        plan.estimated_source_tokens,
        materialized.meta.source_tokens
    );
}

#[test]
fn context_plan_warns_when_generated_defaults_match() {
    let generated =
        Candidate::new("artifacts/runtime_reports/latest.json", 1, 2, "generated").exact(10.0);
    let source = Candidate::new("src/runtime.rs", 1, 2, "source").exact(0.5);
    let mut request = request_with_budget(20);
    request.plan_only = true;

    let response = select(vec![generated, source], &request, 1);
    let plan = response.plan.expect("query plan");

    assert!(plan.generated_artifact_warning);
    assert!(
        response
            .warnings
            .iter()
            .any(|warning| warning.contains("generated-artifact"))
    );
    assert!(
        plan.candidates
            .iter()
            .all(|candidate| candidate.path != "artifacts/runtime_reports/latest.json")
    );
}
