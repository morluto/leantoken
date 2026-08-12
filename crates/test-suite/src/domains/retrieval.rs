use leantoken::model::{ContextRequest, Freshness};
use leantoken::ranking::{Candidate, Weights, deduplicate, rank, select};
use leantoken::tokens::{Tokenizer, count, truncate};

fn request_with_budget(budget: usize) -> ContextRequest {
    ContextRequest {
        task: "rank source evidence for a task".into(),
        token_budget: budget,
        include_paths: Vec::new(),
        must_include_paths: Vec::new(),
        must_include_symbols: Vec::new(),
        required_evidence: Vec::new(),
        max_fragments: None,
        plan_only: false,
        focus_paths: Vec::new(),
        strict_focus_paths: false,
        minimum_fragments_per_focus_path: None,
        focus_symbols: Vec::new(),
        exclude_paths: Vec::new(),
        known_hashes: Vec::new(),
        receipt_id: None,
        prior_repository_generation: None,
        base_revision: None,
        changed_paths: Vec::new(),
        strict_changed_paths: false,
        explain_diagnostics: false,
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
fn public_retrieval_primitives_preserve_external_contracts() {
    let source = "fn café() { println!(\"hello\"); }\n".repeat(20);
    let tokenizer = Tokenizer::default();
    let (prefix, tokens) = truncate(&source, 12);
    assert_eq!(tokenizer.name(), "cl100k_base");
    assert!(count(&source) > 12);
    assert!(source.starts_with(prefix));
    assert!(tokens <= 12);
    assert_eq!(tokens, count(prefix));
    assert!(std::str::from_utf8(prefix.as_bytes()).is_ok());

    let exact = Candidate::new("a.rs", 1, 1, "fn a() {}")
        .exact(1.0)
        .bm25(0.1);
    let lexical = Candidate::new("b.rs", 1, 1, "fn b() {}")
        .exact(0.1)
        .bm25(10.0);
    let exact_weights = Weights {
        exact: 1.0,
        bm25: 0.0,
        ..Weights::default()
    };
    let lexical_weights = Weights {
        exact: 0.0,
        bm25: 1.0,
        ..Weights::default()
    };
    assert_eq!(
        rank(vec![exact.clone(), lexical.clone()], &exact_weights)[0]
            .candidate
            .path,
        "a.rs"
    );
    assert_eq!(
        rank(vec![exact, lexical], &lexical_weights)[0]
            .candidate
            .path,
        "b.rs"
    );

    let deduped = deduplicate(rank(
        vec![
            candidate("same.rs", "fn duplicate() {}", 1.0),
            candidate("same.rs", "fn duplicate() {}", 0.5),
        ],
        &Weights::default(),
    ));
    assert_eq!(deduped.len(), 1);
    assert_eq!(deduped[0].candidate.path, "same.rs");
}

#[test]
fn select_composes_budget_scope_omissions_and_receipt() {
    let known_content = "fn known() {}";
    let candidates = vec![
        candidate("known.rs", known_content, 1.1),
        candidate("src/lib.rs", "fn selected() {}", 0.5).symbol_name("Selected"),
        candidate("src/mainly.rs", "fn mainly() {}", 0.5).symbol_name("Mainly"),
        candidate("dist/generated.rs", "fn generated() {}", 1.2),
    ];
    let mut request = request_with_budget(50);
    request.focus_paths = vec![r"src\**\*.rs".into()];
    request.focus_symbols = vec!["Selected".into()];
    request.exclude_paths = vec!["dist/**".into()];
    request.known_hashes = vec![leantoken::text::hash(known_content)];
    request.explain_diagnostics = true;

    let response = select(candidates, &request, 7);
    let total: usize = response.fragments.iter().map(|f| f.token_count).sum();
    assert!(total <= request.token_budget);
    assert!(response.meta.source_tokens <= request.token_budget);
    assert_eq!(response.meta.repository_generation, 7);
    assert!(matches!(response.meta.freshness, Freshness::Current));
    assert_eq!(
        response
            .fragments
            .first()
            .map(|fragment| fragment.path.as_str()),
        Some("src/lib.rs")
    );
    assert!(
        response
            .fragments
            .iter()
            .all(|item| item.path != "known.rs" && !item.path.starts_with("dist/"))
    );
    assert_eq!(response.omission_summary.known_hash, 1);
    assert_eq!(response.omission_summary.path_excluded, 1);
    assert!(!response.receipt.task_fingerprint.is_empty());
    assert_eq!(
        response.receipt.fragment_hashes.len(),
        response.fragments.len()
    );
    for (fragment, content_hash) in response
        .fragments
        .iter()
        .zip(response.receipt.fragment_hashes.iter())
    {
        assert_eq!(&fragment.content_hash, content_hash);
    }
}

#[test]
fn select_does_not_focus_substring_path_matches() {
    let candidates = vec![
        candidate("src/main.rs", "fn main() {}", 0.5),
        candidate("src/mainly.rs", "fn mainly() {}", 0.6),
    ];
    let mut request = request_with_budget(50);
    request.focus_paths = vec!["main".into()];
    let response = select(candidates, &request, 1);
    assert_eq!(response.fragments[0].path, "src/mainly.rs");
}
