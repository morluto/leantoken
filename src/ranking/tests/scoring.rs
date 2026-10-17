use super::*;

#[test]
fn candidate_target_ranges_are_complete_and_one_based() {
    assert!(CandidateTargetRange::new(0, 1).is_none());
    assert!(CandidateTargetRange::new(3, 2).is_none());
    assert_eq!(
        CandidateTargetRange::new(2, 3).map(|range| range.lines()),
        Some((2, 3))
    );
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
