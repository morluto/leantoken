use super::*;

#[test]
fn dedup_keeps_content_identical_highest_score() {
    let a = Candidate::new("a.rs", 1, 2, "same body")
        .exact(1.0)
        .match_kind("exact");
    let b = Candidate::new("a.rs", 10, 11, "same body")
        .exact(0.5)
        .match_kind("reference");

    let ranked = rank(vec![a, b], &Weights::default());
    let deduped = deduplicate(ranked);

    assert_eq!(deduped.len(), 1);
    assert!(deduped[0].score > 0.9);
}

#[test]
fn dedup_keeps_content_identical_candidates_at_distinct_paths() {
    let implementation = Candidate::new("src/lib.rs", 1, 1, "same body").exact(1.0);
    let contract = Candidate::new("examples/lib.rs", 1, 1, "same body").exact(0.5);

    let deduped = deduplicate(rank(vec![implementation, contract], &Weights::default()));

    assert_eq!(deduped.len(), 2);
}

#[test]
fn dedup_merges_multi_channel_provenance_and_recomputes_score() {
    let symbol = Candidate::new("src/lib.rs", 1, 2, "fn target() {}")
        .concept("target", 2.0)
        .match_kind("symbol")
        .facet("exact_atom", "target")
        .channel("symbol", 0)
        .symbol(1.0);
    let text = Candidate::new("src/lib.rs", 1, 2, "fn target() {}")
        .concept("behavior", 0.8)
        .match_kind("text")
        .facet("behavior", "behavior")
        .channel("text", 2)
        .exact(1.0)
        .bm25(1_000_000.0);
    let best_single = rank(vec![symbol.clone(), text.clone()], &Weights::default())
        .into_iter()
        .map(|candidate| candidate.score)
        .fold(0.0, f64::max);

    let deduped = deduplicate(rank(vec![symbol, text], &Weights::default()));

    assert_eq!(deduped.len(), 1);
    let merged = &deduped[0];
    assert!(merged.score > best_single);
    assert!(
        merged
            .candidate
            .concepts
            .iter()
            .any(|value| value == "target")
    );
    assert!(
        merged
            .candidate
            .concepts
            .iter()
            .any(|value| value == "behavior")
    );
    assert!(
        merged
            .candidate
            .match_kinds
            .iter()
            .any(|value| value == "symbol")
    );
    assert!(
        merged
            .candidate
            .match_kinds
            .iter()
            .any(|value| value == "text")
    );
}

#[test]
fn dedup_preserves_exact_range_when_a_broader_candidate_overlaps() {
    let broad = Candidate::new("a.rs", 1, 10, "broad")
        .bm25(1_000_000.0)
        .path_score(2.0);
    let exact = Candidate::new("a.rs", 5, 15, "exact").exact(1.0);

    let deduped = deduplicate(rank(vec![broad, exact], &Weights::default()));

    assert_eq!(deduped.len(), 1);
    assert_eq!(deduped[0].candidate.start_line, 5);
}

#[test]
fn dedup_keeps_overlapping_highest_score() {
    let a = Candidate::new("a.rs", 1, 10, "first").exact(1.0);
    let b = Candidate::new("a.rs", 5, 15, "second").exact(0.5);

    let ranked = rank(vec![a, b], &Weights::default());
    let deduped = deduplicate(ranked);

    // 6 of 10 lines overlap, exceeding the 0.5 threshold.
    assert_eq!(deduped.len(), 1);
}

#[test]
fn dedup_keeps_overlapping_content_with_distinct_required_evidence() {
    let marker = required_evidence_marker(0, 0);
    let retained =
        Candidate::new("paper.tex", 1, 10, "stronger content without the literal").exact(2.0);
    let verified = Candidate::new("paper.tex", 5, 15, "REQUIRED_LITERAL appears here")
        .match_kind(marker.clone())
        .exact(1.0);

    let deduped = deduplicate(rank(vec![retained, verified], &Weights::default()));

    assert_eq!(deduped.len(), 2);
    let retained = deduped
        .iter()
        .find(|candidate| !candidate.candidate.content.contains("REQUIRED_LITERAL"))
        .expect("stronger overlapping candidate");
    assert!(!retained.candidate.match_kinds.contains(&marker));
    assert!(deduped.iter().any(|candidate| {
        candidate.candidate.content.contains("REQUIRED_LITERAL")
            && candidate.candidate.match_kinds.contains(&marker)
    }));
}

#[test]
fn dedup_merges_required_evidence_for_content_identical_candidates() {
    let marker = required_evidence_marker(0, 0);
    let retained = Candidate::new("paper.tex", 1, 1, "REQUIRED_LITERAL").exact(2.0);
    let verified = Candidate::new("paper.tex", 10, 10, "REQUIRED_LITERAL")
        .match_kind(marker.clone())
        .exact(1.0);

    let deduped = deduplicate(rank(vec![retained, verified], &Weights::default()));

    assert_eq!(deduped.len(), 1);
    assert!(deduped[0].candidate.match_kinds.contains(&marker));
}

#[test]
fn dedup_keeps_non_overlapping_same_file() {
    let a = Candidate::new("a.rs", 1, 5, "first").exact(1.0);
    let b = Candidate::new("a.rs", 7, 10, "second").exact(0.9);

    let ranked = rank(vec![a, b], &Weights::default());
    let deduped = deduplicate(ranked);

    assert_eq!(deduped.len(), 2);
}

#[test]
fn rank_orders_by_score() {
    let a = Candidate::new("a.rs", 1, 1, "x").exact(1.0);
    let b = Candidate::new("b.rs", 1, 1, "x").exact(0.5);
    let c = Candidate::new("c.rs", 1, 1, "x").exact(0.0);

    let ranked = rank(vec![c, b, a], &Weights::default());

    assert!(ranked[0].score > ranked[1].score);
    assert!(ranked[1].score > ranked[2].score);
}
