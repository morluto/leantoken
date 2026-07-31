use super::*;

#[test]
fn score_is_finite_and_non_negative() {
    let candidate = Candidate::new("a.rs", 1, 2, "fn main() {}")
        .exact(1.0)
        .symbol(1.0)
        .reference(1.0)
        .bm25(10.0)
        .path_score(0.8)
        .focus_boost(0.5)
        .import_boost(0.5)
        .change_boost(0.5)
        .lexical_frequency_penalty(0.2);

    let weights = Weights::default();
    let score = candidate.score(&weights, candidate.token_count());
    assert!(score.is_finite());
    assert!(score >= 0.0);
}

#[test]
fn internal_facet_provenance_does_not_expand_response_reasons() {
    let candidate = Candidate::new("src/lib.rs", 1, 1, "target")
        .match_kind("symbol")
        .facet("exact_atom", "target")
        .channel("symbol", 3);

    assert_eq!(candidate.reason(), "symbol");
    assert!(
        candidate
            .match_kinds
            .iter()
            .any(|kind| kind == "facet:exact_atom:target")
    );
    assert!(
        candidate
            .match_kinds
            .iter()
            .any(|kind| kind == "channel:symbol:3")
    );
}

#[test]
fn bm25_normalizes_and_saturates() {
    let w = Weights::default();
    let low = Candidate::new("a.rs", 1, 1, "x").bm25(0.1);
    let high = Candidate::new("a.rs", 1, 1, "x").bm25(1_000_000.0);

    let low_score = low.score(&w, low.token_count());
    let high_score = high.score(&w, high.token_count());

    assert!(high_score > low_score);
    // Saturated BM25 contribution should be bounded.
    assert!(high_score < low_score + w.bm25 * 2.0 + 1.0);
}

#[test]
fn lexical_frequency_penalty_reduces_score() {
    let w = Weights::default();
    let base = Candidate::new("a.rs", 1, 1, "x").exact(1.0);
    let penalized = Candidate::new("a.rs", 1, 1, "x")
        .exact(1.0)
        .lexical_frequency_penalty(1.0);

    let base_score = base.score(&w, base.token_count());
    let penalized_score = penalized.score(&w, penalized.token_count());

    assert!(penalized_score < base_score);
}

#[test]
fn larger_implicit_size_score_is_smaller() {
    let w = Weights::default();
    let small = Candidate::new("a.rs", 1, 1, "x").exact(1.0);
    let large = Candidate::new("a.rs", 1, 1, "word ".repeat(50)).exact(1.0);

    let small_score = small.score(&w, small.token_count());
    let large_score = large.score(&w, large.token_count());

    // Both exact, but the larger content gets an implicit size penalty.
    assert!(large_score < small_score || large.token_count() == small.token_count());
}

#[test]
fn large_token_counts_keep_monotonic_size_penalties() {
    let candidate = Candidate::new("a.rs", 1, 1, "x").exact(1.0);
    let weights = Weights::default();
    let at_u32_limit = candidate.score(&weights, u32::MAX as usize);
    let much_larger = candidate.score(&weights, (u32::MAX as usize) * 2);
    let far_larger = candidate.score(&weights, (u32::MAX as usize) * 4);

    assert!(at_u32_limit > much_larger);
    assert!(much_larger > far_larger);
}

#[test]
fn content_hash_is_deterministic() {
    let a = Candidate::new("a.rs", 1, 2, "same content");
    let b = Candidate::new("b.rs", 3, 4, "same content");
    assert_eq!(a.content_hash(), b.content_hash());
    assert_ne!(
        a.content_hash(),
        Candidate::new("a.rs", 1, 2, "different").content_hash()
    );
}
