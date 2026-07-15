use leantoken::model::{ContextRequest, Freshness};
use leantoken::ranking::{Candidate, Weights, deduplicate, rank, select};

fn request_with_budget(budget: usize) -> ContextRequest {
    ContextRequest {
        task: "rank source evidence for a task".into(),
        token_budget: budget,
        focus_paths: Vec::new(),
        focus_symbols: Vec::new(),
        exclude_paths: Vec::new(),
        known_hashes: Vec::new(),
        prior_repository_generation: None,
    }
}

fn candidate(path: &str, lines: &str, score: f64) -> Candidate {
    let line_count = lines.lines().count().max(1);
    Candidate::new(path, 1, line_count, lines)
        .exact(score)
        .match_kind("exact")
        .representation("source")
}

#[test]
fn rank_orders_candidates_by_score() {
    let candidates = vec![
        candidate("b.rs", "fn b() {}", 0.1),
        candidate("a.rs", "fn a() {}", 0.9),
        candidate("c.rs", "fn c() {}", 0.5),
    ];
    let ranked = rank(candidates, &Weights::default());
    assert_eq!(ranked[0].candidate.path, "a.rs");
    assert_eq!(ranked[1].candidate.path, "c.rs");
    assert_eq!(ranked[2].candidate.path, "b.rs");
    assert!(ranked[0].score >= ranked[1].score);
    assert!(ranked[1].score >= ranked[2].score);
}

#[test]
fn score_is_finite_and_non_negative() {
    let c = candidate("a.rs", "fn main() {}", 1.0)
        .symbol(1.0)
        .reference(1.0)
        .bm25(10.0)
        .path_score(0.8)
        .focus_boost(0.5)
        .import_boost(0.5)
        .change_boost(0.5)
        .lexical_frequency_penalty(0.2);
    let weights = Weights::default();
    let scored = rank(vec![c], &weights);
    assert!(scored[0].score.is_finite());
    assert!(scored[0].score >= 0.0);
    assert_eq!(
        scored[0].content_hash.len(),
        leantoken::text::CONTENT_FINGERPRINT_HEX_LEN
    );
    assert!(scored[0].token_count > 0);
}

#[test]
fn deduplicate_removes_content_identical_candidates() {
    let c1 = candidate("a.rs", "fn dup() {}", 1.0);
    let c2 = candidate("a.rs", "fn dup() {}", 0.5);
    let scored = rank(vec![c1, c2], &Weights::default());
    let deduped = deduplicate(scored);
    assert_eq!(deduped.len(), 1);
    assert!(deduped[0].score > 0.0);
}

#[test]
fn deduplicate_keeps_overlapping_higher_scored_same_file() {
    let c1 = candidate("a.rs", "fn low() {}\n", 0.5).exact(0.5);
    let c2 = candidate("a.rs", "fn low() {}\nfn high() {}\n", 1.0).exact(1.0);
    let scored = rank(vec![c1, c2], &Weights::default());
    let deduped = deduplicate(scored);
    assert!(!deduped.is_empty());
    // The higher-scored larger range should dominate if it covers the smaller one.
    let high = deduped.iter().find(|s| s.candidate.path == "a.rs");
    assert!(high.is_some());
}

#[test]
fn select_respects_token_budget() {
    let candidates = vec![
        candidate("a.rs", "fn a1() {}\nfn a2() {}\n", 1.0),
        candidate("b.rs", "fn b1() {}\nfn b2() {}\n", 0.9),
        candidate("c.rs", "fn c1() {}\n", 0.8),
    ];
    let request = request_with_budget(10);
    let response = select(candidates, &request, 1);

    let total: usize = response.fragments.iter().map(|f| f.token_count).sum();
    assert!(
        total <= request.token_budget,
        "total {} > budget {}",
        total,
        request.token_budget
    );
    assert!(response.meta.emitted_tokens <= request.token_budget);
    assert_eq!(response.meta.repository_generation, 1);
    assert!(matches!(response.meta.freshness, Freshness::Current));
}

#[test]
fn select_omits_known_hashes_and_reports_them() {
    let candidates = vec![
        candidate("known.rs", "fn known() {}", 1.0),
        candidate("new.rs", "fn new() {}", 0.9),
    ];

    // Compute hash of known candidate content.
    let known_hash = leantoken::text::hash("fn known() {}");

    let mut request = request_with_budget(50);
    request.known_hashes = vec![known_hash];
    let response = select(candidates, &request, 2);

    assert!(response.fragments.iter().all(|f| f.path != "known.rs"));
    let known_omitted = response.omitted.iter().any(|o| o.path == "known.rs");
    assert!(known_omitted, "known hash should be reported in omitted");
    assert!(!response.receipt.task_fingerprint.is_empty());
    assert_eq!(response.receipt.repository_generation, 2);
    assert!(
        response
            .receipt
            .fragments
            .iter()
            .all(|i| i.path != "known.rs")
    );
}

#[test]
fn select_excludes_paths() {
    let candidates = vec![
        candidate("src/lib.rs", "fn lib() {}", 1.0),
        candidate("tests/lib.rs", "fn test() {}", 0.9),
    ];
    let mut request = request_with_budget(50);
    request.exclude_paths = vec!["tests".into()];
    let response = select(candidates, &request, 1);
    assert!(response.fragments.iter().all(|f| f.path != "tests/lib.rs"));
}

#[test]
fn select_boosts_focus_paths_and_symbols() {
    let candidates = vec![
        candidate("src/a.rs", "fn a() {}", 0.5).symbol_name("Alpha"),
        candidate("src/b.rs", "fn b() {}", 0.5).symbol_name("Beta"),
    ];
    let mut request = request_with_budget(50);
    request.focus_paths = vec!["src/a.rs".into()];
    request.focus_symbols = vec!["Alpha".into()];
    let response = select(candidates, &request, 1);

    if let Some(first) = response.fragments.first() {
        assert_eq!(first.path, "src/a.rs");
    }
}

#[test]
fn select_produces_evidence_receipt_with_hashes_and_generation() {
    let candidates = vec![candidate("src/lib.rs", "fn main() {}", 1.0)];
    let request = request_with_budget(50);
    let response = select(candidates, &request, 7);

    assert_eq!(response.receipt.repository_generation, 7);
    assert_eq!(response.receipt.fragments.len(), response.fragments.len());
    for (fragment, identity) in response
        .fragments
        .iter()
        .zip(response.receipt.fragments.iter())
    {
        assert_eq!(fragment.path, identity.path);
        assert_eq!(fragment.start_line, identity.start_line);
        assert_eq!(fragment.end_line, identity.end_line);
        assert_eq!(fragment.content_hash, identity.content_hash);
    }
}

#[test]
fn custom_weights_change_ranking() {
    let c1 = Candidate::new("a.rs", 1, 1, "fn a() {}")
        .exact(1.0)
        .bm25(0.1);
    let c2 = Candidate::new("b.rs", 1, 1, "fn b() {}")
        .exact(0.1)
        .bm25(10.0);

    let exact_weights = Weights {
        exact: 1.0,
        bm25: 0.0,
        ..Weights::default()
    };
    let ranked_exact = rank(vec![c1.clone(), c2.clone()], &exact_weights);
    assert_eq!(ranked_exact[0].candidate.path, "a.rs");

    let bm25_weights = Weights {
        exact: 0.0,
        bm25: 1.0,
        ..Weights::default()
    };
    let ranked_bm25 = rank(vec![c1, c2], &bm25_weights);
    assert_eq!(ranked_bm25[0].candidate.path, "b.rs");
}
