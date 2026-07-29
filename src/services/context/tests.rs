use super::*;

#[test]
fn lexical_match_facts_share_first_match_and_saturate_frequency_count() {
    let content = (0..100)
        .map(|index| format!("needle_{index}"))
        .collect::<Vec<_>>()
        .join("\n");
    let hit = ChunkHit {
        chunk_id: 1,
        file_id: 1,
        path: "src/lib.rs".into(),
        content,
        start_line: 10,
        end_line: 109,
        start_byte: 100,
        end_byte: 1_000,
        token_count: 100,
        generation: 1,
        score: 0.0,
    };
    let matcher = regex::RegexBuilder::new("needle")
        .case_insensitive(true)
        .build()
        .expect("matcher");

    let facts = analyze_lexical_match(&hit, &matcher, 2).expect("match facts");

    assert_eq!(facts.matched_line, 10);
    assert_eq!(facts.search_hit.start_line, 10);
    assert_eq!(facts.occurrences, LEXICAL_OCCURRENCE_SATURATION);
}

#[test]
fn revision_ranges_require_two_explicit_endpoints() {
    assert_eq!(
        parse_revision_range("main~1..main").expect("valid range"),
        Some(("main~1", "main"))
    );
    assert_eq!(
        parse_revision_range("origin/main").expect("single revision"),
        None
    );
    for invalid in ["..main", "main..", "main...head"] {
        assert!(parse_revision_range(invalid).is_err(), "{invalid}");
    }
}

#[test]
fn workflow_auto_detection_requires_high_confidence_language() {
    assert_eq!(
        resolve_context_workflow(ContextWorkflow::Auto, "prepare this pull request"),
        ContextWorkflow::Contribution
    );
    assert_eq!(
        resolve_context_workflow(ContextWorkflow::Auto, "review this parser change"),
        ContextWorkflow::Review
    );
    assert_eq!(
        resolve_context_workflow(ContextWorkflow::Auto, "implement parser review comments"),
        ContextWorkflow::Implementation
    );
}

fn context_queries(task: &str, limit: usize) -> Vec<ContextQuery> {
    facets::plan(task, limit).queries
}

#[test]
fn language_scope_does_not_treat_lowercase_go_as_golang() {
    assert!(!task_mentions_language("go fix the parser", "go"));
    assert!(task_mentions_language("fix the Go parser", "go"));
    assert!(task_mentions_language("fix the golang parser", "go"));
    assert!(task_mentions_language(
        "fix TypeScript parsing",
        "typescript"
    ));
}

#[test]
fn language_scope_boosts_common_source_file_extensions() {
    assert_eq!(
        context_path_score("src/main.rs", &[], "Fix this Rust bug"),
        12.0
    );
    assert_eq!(
        context_path_score("lib/parser.py", &[], "Fix this Python parser"),
        12.0
    );
    assert_eq!(
        context_path_score("src/main.rs", &[], "Fix this Python parser"),
        0.0
    );
}

#[test]
fn mcp_repository_questions_prioritize_mcp_implementation_paths() {
    for (task, preferred, distractor) in [
        (
            "Where is MCP tool registration and catalog schema defined?",
            "src/mcp/tools.rs",
            "src/watcher/tests/support.rs",
        ),
        (
            "Which repository file defines the MCP server catalog and tool schemas?",
            "src/mcp.rs",
            "benchmarks/reports/mcp-profile.md",
        ),
        (
            "Which test suite verifies the MCP server catalog and tool schemas?",
            "crates/test-suite/src/domains/protocol.rs",
            "tests/services/search.rs",
        ),
    ] {
        assert!(
            context_path_score(preferred, &[], task) > context_path_score(distractor, &[], task),
            "routing regression for {task:?}"
        );
    }
}

#[test]
fn owner_test_matching_requires_filename_token_boundaries() {
    let mut request = ContextRequest {
        task: "fix core".into(),
        token_budget: 100,
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
        changed_paths: vec!["src/core.rs".into()],
        strict_changed_paths: false,
        verbose_diagnostics: false,
    };

    assert_eq!(
        owner_test_changed_path("tests/core_tests.rs", &request),
        Some("src/core.rs".into())
    );
    assert_eq!(
        owner_test_changed_path("tests/hardcore_tests.rs", &request),
        None
    );
    assert_eq!(
        owner_test_changed_path("tests/core/unrelated_tests.rs", &request),
        None
    );
    request.changed_paths = vec!["src/my_core.rs".into()];
    assert_eq!(
        owner_test_changed_path("tests/my_core_spec.rs", &request),
        Some("src/my_core.rs".into())
    );
}

#[test]
fn context_queries_keep_identifiers_and_late_test_signals() {
    let terms = context_queries(
        "copy_current_request_context reuses one copied request context so calling the decorated function concurrently can corrupt state; add a regression test",
        12,
    );

    assert!(
        terms
            .iter()
            .any(|term| term.value == "copy_current_request_context")
    );
    assert!(terms.iter().any(|term| term.value == "test"));
    assert!(!terms.iter().any(|term| term.value == "one"));
}

#[test]
fn context_queries_preserve_dotted_and_header_tokens() {
    let terms = context_queries(
        "Fix res.send adding Content-Length when Transfer-Encoding is present and add coverage",
        12,
    );

    assert!(terms.iter().any(|term| term.value == "res.send"));
    assert!(terms.iter().any(|term| term.value == "Content-Length"));
    assert!(terms.iter().any(|term| term.value == "Transfer-Encoding"));
    assert_eq!(terms.last().map(|term| term.value.as_str()), Some("test"));
}

#[test]
fn context_queries_keep_early_domain_nouns_over_later_long_words() {
    let terms = context_queries(
        "Fix app.render and res.render for a view name ending in a dot. The callback must report the normal lookup error.",
        12,
    );

    assert!(terms.iter().any(|term| term.value == "view"));
    assert!(terms.iter().any(|term| term.value == "name"));
    assert!(terms.iter().any(|term| term.value == "ending"));
    assert!(terms.iter().any(|term| term.value == "dot"));
    assert!(!terms.iter().any(|term| term.value == "callback"));
}

#[test]
fn context_queries_cover_early_domain_tail_intent_and_natural_phrases() {
    let terms = context_queries(
        "Trace how index generations are published atomically and how request snapshot consistency is preserved for concurrent readers",
        12,
    );

    assert!(terms.len() <= 10);
    assert!(terms.iter().any(|term| term.value == "index"));
    assert!(terms.iter().any(|term| term.value == "snapshot"));
    assert!(terms.iter().any(|term| term.value == "concurrent"));
    assert!(
        terms
            .iter()
            .any(|term| term.value == "snapshot consistency")
    );
    assert!(
        terms
            .iter()
            .any(|term| term.value == "published atomically")
    );
    assert!(!terms.iter().any(|term| term.value == "Trace"));
    assert!(!terms.iter().any(|term| term.value == "how"));
}

#[test]
fn context_queries_reserve_space_for_task_intent() {
    let terms = context_queries(
        "Fix Alpha::first_long_identifier Beta::second_long_identifier while preserving idempotency",
        12,
    );

    assert!(
        terms
            .iter()
            .any(|term| term.value == "Alpha::first_long_identifier")
    );
    assert!(
        terms
            .iter()
            .any(|term| term.value == "Beta::second_long_identifier")
    );
    assert!(terms.iter().any(|term| term.value == "idempotency"));
}

#[test]
fn oversized_diff_routing_is_bounded_deterministic_and_preserves_retry_inputs() {
    assert_eq!(context_path_group("README.md"), "<root>");
    let request = ContextRequest {
        task: "review the changed implementation".into(),
        token_budget: 4_000,
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
        known_hashes: vec!["held".into()],
        receipt_id: None,
        prior_repository_generation: None,
        base_revision: Some("origin/main".into()),
        changed_paths: Vec::new(),
        strict_changed_paths: false,
        verbose_diagnostics: false,
    };
    let changed_paths = (0..12)
        .flat_map(|index| {
            [
                format!("src/browser/file_{index}.rs"),
                format!("src/runtime/file_{index}.rs"),
                format!("tests/scenario_{index}.rs"),
            ]
        })
        .collect::<Vec<_>>();
    let scope = DiffScopeReceipt {
        base_revision: Some("base".into()),
        head_revision: Some("head".into()),
        changed_paths,
        indexed_changed_paths: 36,
        evidence: None,
    };
    let fragments = [
        ContextFragment {
            path: "src/browser/file_0.rs".into(),
            start_line: 1,
            end_line: 1,
            target_start_line: None,
            target_end_line: None,
            truncated: false,
            representation: "source".into(),
            content: "browser".into(),
            content_hash: "browser-hash".into(),
            score: 1.0,
            reason: "text".into(),
            token_count: 1,
        },
        ContextFragment {
            path: "src/runtime/file_0.rs".into(),
            start_line: 1,
            end_line: 1,
            target_start_line: None,
            target_end_line: None,
            truncated: false,
            representation: "source".into(),
            content: "runtime".into(),
            content_hash: "runtime-hash".into(),
            score: 1.0,
            reason: "text".into(),
            token_count: 1,
        },
    ];

    let selected_paths = fragments
        .iter()
        .map(|fragment| fragment.path.clone())
        .collect::<Vec<_>>();
    let routing =
        build_context_routing(&request, &scope, 24, &selected_paths).expect("oversized routing");

    assert_eq!(routing.changed_paths, 36);
    assert_eq!(routing.path_groups_total, 3);
    assert!(routing.weakly_concentrated);
    assert_eq!(
        routing
            .path_groups
            .iter()
            .map(|group| group.prefix.as_str())
            .collect::<Vec<_>>(),
        vec!["src/browser", "src/runtime", "tests"]
    );
    assert_eq!(routing.suggestions.len(), 3);
    assert_eq!(routing.suggestions[0].include_paths, vec!["src/browser/**"]);
    assert_eq!(routing.base_revision.as_deref(), Some("origin/main"));
    assert_eq!(routing.known_hashes, vec!["held"]);
}

#[test]
fn context_query_expansions_share_one_fusion_concept() {
    let terms = context_queries(
        "Fix GlobSet::matches_all when one compiled strategy matches",
        12,
    );
    let qualified = terms
        .iter()
        .find(|term| term.value == "GlobSet::matches_all")
        .expect("qualified query");
    let expansion = terms
        .iter()
        .find(|term| term.value != qualified.value && term.fusion_key == qualified.fusion_key)
        .expect("expanded query");

    assert_eq!(qualified.fusion_key, expansion.fusion_key);
    assert!(!qualified.fusion_key.is_empty());
}

#[test]
fn candidate_diagnostics_retain_facet_and_ranked_channel_provenance() {
    let query = context_queries("Fix Rack::Deflater behavior", 12)
        .into_iter()
        .find(|query| query.value == "Rack::Deflater")
        .expect("exact technical query");
    let candidate = annotate_candidate(
        Candidate::new("src/lib.rs", 1, 1, "target").match_kind("symbol"),
        &query,
        "symbol",
        2,
    );

    assert!(
        candidate
            .match_kinds
            .iter()
            .any(|kind| kind == "facet:exact_atom:rack::deflater")
    );
    assert!(
        candidate
            .match_kinds
            .iter()
            .any(|kind| kind == "channel:symbol:2")
    );
    assert_eq!(candidate.reason(), "symbol");
}

#[test]
fn low_cardinality_exact_query_disables_neighbor_expansion() {
    let exact = context_queries("Fix Rack::Deflater", 12);
    let multi = context_queries("Fix Rack::Deflater and Compression::Writer", 12);

    assert!(low_cardinality_exact_query(&exact));
    assert!(!low_cardinality_exact_query(&multi));
}

#[test]
fn import_symbol_requires_the_same_seed_and_target_concept() {
    let queries = context_queries("Fix Rack::Deflater and Compression::Writer", 12);
    let deflater_query = queries
        .iter()
        .find(|query| query.fusion_key == "rack::deflater")
        .expect("deflater query");
    let symbol = SymbolRecord {
        id: 1,
        file_id: 2,
        name: "Deflater".into(),
        kind: "class".into(),
        parent: Some("Rack".into()),
        signature: Some("class Rack::Deflater".into()),
        start_line: 10,
        end_line: 20,
        start_byte: 100,
        end_byte: 200,
    };

    assert!(corroborated_import_symbol(vec![symbol.clone()], &queries, &BTreeSet::new()).is_none());
    let matched = corroborated_import_symbol(
        vec![symbol],
        &queries,
        &BTreeSet::from([deflater_query.fusion_key.clone()]),
    )
    .expect("same-concept import symbol");
    assert_eq!(matched.0.name, "Deflater");
    assert_eq!(matched.1.fusion_key, "rack::deflater");
    assert!(matched.2 > 0.0);
}

#[test]
fn qualified_symbol_match_requires_all_owner_and_name_parts() {
    assert_eq!(
        qualified_symbol_match(
            "render.AsciiJSON",
            "Render",
            None,
            Some("func (r AsciiJSON) Render() error"),
        ),
        1.0
    );
    assert_eq!(
        qualified_symbol_match(
            "render.AsciiJSON",
            "AsciiJSON",
            None,
            Some("type AsciiJSON")
        ),
        0.0
    );
    assert_eq!(
        qualified_symbol_match("Flask.run", "run", Some("Flask"), Some("def run()")),
        1.0
    );
}

#[test]
fn qualified_path_evidence_excludes_dynamic_lowercase_receivers() {
    assert_eq!(
        context_path_score(
            "test/app.render.js",
            &[],
            "Fix app.render for a trailing dot",
        ),
        0.0
    );
    assert!(context_path_score("render/json.go", &[], "Fix render.AsciiJSON escaping",) > 0.0);
    assert!(
        context_path_score(
            "tokio/src/fs/file.rs",
            &[],
            "Fix tokio::fs::File poll_write",
        ) > 0.0
    );
}

#[test]
fn fusion_requires_two_independent_query_concepts() {
    let mut fusion = HashMap::new();
    record_query_hit(&mut fusion, "one.rs", "globset::matches_all", 1.0, 0);
    record_query_hit(&mut fusion, "one.rs", "globset::matches_all", 0.95, 1);
    record_query_hit(&mut fusion, "two.rs", "content-length", 1.0, 0);
    record_query_hit(&mut fusion, "two.rs", "transfer-encoding", 1.0, 1);
    let mut candidates = vec![
        Candidate::new("one.rs", 1, 1, "one"),
        Candidate::new("two.rs", 1, 1, "two"),
    ];

    apply_query_fusion(&mut candidates, &fusion);

    assert_eq!(candidates[0].path_score, 0.0);
    assert!(
        !candidates[0]
            .match_kinds
            .iter()
            .any(|kind| kind == "multi-query")
    );
    assert!(candidates[1].path_score > 0.0);
    assert!(
        candidates[1]
            .match_kinds
            .iter()
            .any(|kind| kind == "multi-query")
    );
}
