use leantoken::model::{ContextRequest, Freshness};
use leantoken::ranking::{Candidate, Weights, select};
use leantoken::tokens::Tokenizer;

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
fn explicit_selection_weights_and_tokenizer_control_the_public_response() {
    let exact = Candidate::new("exact.rs", 1, 1, "exact evidence").exact(1.0);
    let lexical = Candidate::new("lexical.rs", 1, 1, "lexical evidence").bm25(10.0);
    let mut request = request_with_budget(100);
    request.max_fragments = Some(1);
    let weights = Weights {
        exact: 0.0,
        bm25: 1.0,
        ..Weights::default()
    };

    let weighted = leantoken::ranking::select_with_weights(
        vec![exact.clone(), lexical.clone()],
        &request,
        11,
        &weights,
    );
    assert_eq!(weighted.fragments[0].path, "lexical.rs");
    assert!(weighted.meta.token_count_exact);

    let estimated = leantoken::ranking::select_with_tokenizer(
        vec![exact.clone(), lexical.clone()],
        &request,
        11,
        Tokenizer::Estimate,
    );
    assert_eq!(estimated.fragments[0].path, "exact.rs");
    assert_eq!(estimated.meta.tokenizer, Tokenizer::Estimate.name());
    assert!(!estimated.meta.token_count_exact);

    let response = leantoken::ranking::select_with_weights_and_tokenizer(
        vec![exact, lexical],
        &request,
        11,
        &weights,
        Tokenizer::Estimate,
    );

    assert_eq!(response.fragments.len(), 1);
    assert_eq!(response.fragments[0].path, "lexical.rs");
    assert_eq!(response.meta.repository_generation, 11);
    assert_eq!(response.meta.tokenizer, Tokenizer::Estimate.name());
    assert!(!response.meta.token_count_exact);
    assert!(response.meta.source_tokens <= request.token_budget);

    let content = &response.fragments[0].content;
    let estimated_tokens = leantoken::tokens::count_with(content, Tokenizer::Estimate);
    assert_eq!(estimated_tokens, response.fragments[0].token_count);
    let (estimated_prefix, estimated_prefix_tokens) = leantoken::tokens::truncate_with(
        content,
        estimated_tokens.saturating_sub(1),
        Tokenizer::Estimate,
    );
    assert!(content.starts_with(estimated_prefix));
    assert!(content.is_char_boundary(estimated_prefix.len()));
    assert!(estimated_prefix_tokens < estimated_tokens);

    let (exact_prefix, exact_prefix_tokens) = leantoken::tokens::truncate(content, 1);
    assert!(content.starts_with(exact_prefix));
    assert!(content.is_char_boundary(exact_prefix.len()));
    assert!(exact_prefix_tokens <= 1);
    assert_eq!(leantoken::tokens::count(exact_prefix), exact_prefix_tokens);
}

#[test]
fn source_chunking_preserves_multibyte_scalars_at_byte_caps() {
    let source = "const label = \"abcé\";\nconst marker = \"🧭\";\n";

    for max_chunk_bytes in [19, 1] {
        let prepared =
            leantoken::text::PreparedText::from_bytes(source.as_bytes(), 8, max_chunk_bytes);
        assert!(matches!(prepared.kind, leantoken::text::TextKind::Text));
        assert_eq!(
            prepared
                .chunks
                .iter()
                .fold(String::new(), |mut combined, chunk| {
                    combined.push_str(&chunk.content);
                    combined
                }),
            source
        );
        for chunk in &prepared.chunks {
            assert!(source.is_char_boundary(chunk.start_byte));
            assert!(source.is_char_boundary(chunk.end_byte));
            assert_eq!(chunk.content, source[chunk.start_byte..chunk.end_byte]);
            assert!(
                chunk.content.len() <= max_chunk_bytes || chunk.content.chars().count() == 1,
                "oversized chunk must contain one indivisible UTF-8 scalar: {chunk:?}"
            );
        }
    }
}

#[test]
fn source_excerpt_keeps_a_real_match_and_context_after_multibyte_text() {
    let source = concat!(
        "fn initialize() {\n",
        "    let marker = \"🧭\";\n",
        "    let generation = next_generation();\n",
        "    println!(\"{marker} {generation}\");\n",
        "}\n",
    );
    let matched = source.find("next_generation()").expect("source match");
    let excerpt =
        leantoken::text::excerpt_around(source, matched, matched + "next_generation()".len(), 1);

    assert_eq!(
        excerpt,
        concat!(
            "    let marker = \"🧭\";\n",
            "    let generation = next_generation();\n",
            "    println!(\"{marker} {generation}\");\n",
        )
    );
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
    request.focus_paths = vec!["src/main.rs".into()];
    request.max_fragments = Some(1);
    let response = select(candidates, &request, 1);
    assert_eq!(response.fragments[0].path, "src/main.rs");
}

#[test]
fn direct_selection_remains_safe_with_invalid_path_patterns() {
    let candidates = vec![candidate("src/main.rs", "fn main() {}", 0.5)];

    let mut invalid_include = request_with_budget(50);
    invalid_include.include_paths = vec!["[".into()];
    assert!(
        select(candidates.clone(), &invalid_include, 1)
            .fragments
            .is_empty()
    );

    let mut invalid_strict_focus = request_with_budget(50);
    invalid_strict_focus.focus_paths = vec!["[".into()];
    invalid_strict_focus.strict_focus_paths = true;
    assert!(
        select(candidates.clone(), &invalid_strict_focus, 1)
            .fragments
            .is_empty()
    );

    let mut invalid_exclude = request_with_budget(50);
    invalid_exclude.exclude_paths = vec!["[".into()];
    let response = select(candidates, &invalid_exclude, 1);
    assert_eq!(response.fragments.len(), 1);
    assert_eq!(response.fragments[0].path, "src/main.rs");
}
