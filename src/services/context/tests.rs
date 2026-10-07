use super::*;

#[test]
fn working_tree_observation_ignores_changes_outside_the_scoped_root() {
    let status = crate::repository::parse_git_status_observation(
        std::io::Cursor::new(b"M  sibling.rs\0"),
        10,
        "nested/",
    );

    assert!(status.is_available());
    assert!(status.changed_paths.is_empty());
    assert_eq!(
        WorkingTreeObservation::from_status(&status),
        WorkingTreeObservation::Clean
    );
}

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
        parse_revision_range("  main~1 .. main  ").expect("trimmed valid range"),
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
fn context_revision_validation_rejects_whitespace_only_values() {
    let error = parse_context_revision(Some(" \t\n")).expect_err("whitespace-only revision");

    assert!(matches!(
        error,
        Error::InvalidInput {
            field: "base revision",
            reason: "must not be empty"
        }
    ));
}

#[test]
fn context_revision_validation_rejects_outer_whitespace() {
    let error = parse_context_revision(Some(" main~1 ")).expect_err("outer whitespace");
    assert!(matches!(
        error,
        Error::InvalidInput {
            field: "base revision",
            reason: "must not have leading or trailing whitespace"
        }
    ));
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
        explain_diagnostics: false,
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
fn low_cardinality_exact_query_disables_neighbor_expansion() {
    let exact = facets::plan("Fix Rack::Deflater", 12).queries;
    let multi = facets::plan("Fix Rack::Deflater and Compression::Writer", 12).queries;

    assert!(low_cardinality_exact_query(&exact));
    assert!(!low_cardinality_exact_query(&multi));
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
#[test]
fn partial_candidate_recovery_does_not_swallow_cancellation_or_storage_failures() {
    let mut warnings = Vec::new();
    for error in [
        Error::Cancelled,
        Error::OperationFailure("storage unavailable".into()),
    ] {
        assert!(record_candidate_scan_limit(&mut warnings, error).is_err());
        assert!(warnings.is_empty());
    }
}
