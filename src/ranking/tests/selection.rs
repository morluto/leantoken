    #[test]
    fn selection_skips_a_higher_scored_candidate_that_does_not_fit() {
        let cheap = Candidate::new("cheap.rs", 1, 1, "alpha").exact(0.5);
        let expensive = Candidate::new("expensive.rs", 1, 1, "alpha ".repeat(20)).exact(1.0);

        let req = request_with_budget(1);
        let resp = select(vec![expensive, cheap], &req, 1);

        assert_eq!(resp.fragments.len(), 1);
        assert_eq!(resp.fragments[0].path, "cheap.rs");
    }

    #[test]
    fn file_diversity_caps_same_file_selection() {
        let a1 = Candidate::new("a.rs", 1, 2, "alpha beta").exact(1.0);
        let a2 = Candidate::new("a.rs", 10, 11, "gamma delta").exact(0.95);
        let b1 = Candidate::new("b.rs", 1, 2, "epsilon zeta").exact(0.9);

        // Budget is enough for two 2-token fragments.
        let req = request_with_budget(10);
        let resp = select(vec![a1, a2, b1], &req, 1);

        let a_count = resp.fragments.iter().filter(|f| f.path == "a.rs").count();
        let b_count = resp.fragments.iter().filter(|f| f.path == "b.rs").count();

        assert_eq!(a_count, 1);
        assert_eq!(b_count, 1);
    }

    #[test]
    fn context_uses_short_fragments_without_underfilling_result_cap() {
        let mut candidates = (0..8)
            .map(|index| {
                Candidate::new(format!("file{index}.rs"), 1, 1, format!("evidence_{index}"))
                    .exact(1.0)
            })
            .collect::<Vec<_>>();
        candidates.push(Candidate::new("file0.rs", 20, 20, "second_region").exact(2.0));

        let response = select(candidates, &request_with_budget(1_200), 1);

        assert_eq!(response.fragments.len(), DEFAULT_CONTEXT_FRAGMENTS);
        assert_eq!(
            response
                .fragments
                .iter()
                .filter(|fragment| fragment.path == "file0.rs")
                .count(),
            2
        );
        assert!(response.meta.emitted_tokens < 1_200);
    }

    #[test]
    fn context_honors_caller_fragment_limit_above_the_default() {
        let candidates = (0..12)
            .map(|index| {
                Candidate::new(format!("file{index}.rs"), 1, 1, format!("evidence_{index}"))
                    .concept(format!("concept_{index}"), 1.0)
                    .exact(1.0)
            })
            .collect::<Vec<_>>();
        let mut request = request_with_budget(1_200);
        request.max_fragments = Some(12);

        let response = select(candidates, &request, 1);

        assert_eq!(response.fragments.len(), 12);
    }

    #[test]
    fn must_cover_candidate_precedes_higher_scored_general_evidence() {
        let required = Candidate::new("src/required.rs", 1, 1, "required")
            .symbol_name("required_symbol")
            .target_range(1, 1)
            .exact(0.1);
        let general = Candidate::new("src/general.rs", 1, 1, "general").exact(10.0);
        let mut request = request_with_budget(100);
        request.must_include_paths = vec!["src/required.rs".into()];
        request.must_include_symbols = vec!["required_symbol".into()];
        request.max_fragments = Some(1);

        let response = select(vec![general, required], &request, 1);

        assert_eq!(response.fragments[0].path, "src/required.rs");
        assert_eq!(
            response.coverage.covered_must_include_paths,
            vec!["src/required.rs"]
        );
        assert_eq!(
            response.coverage.covered_must_include_symbols,
            vec!["required_symbol"]
        );
        assert!(response.coverage.uncovered_must_include_paths.is_empty());
        assert!(response.coverage.uncovered_must_include_symbols.is_empty());
    }

    #[test]
    fn required_evidence_selects_distinct_query_facets_before_general_evidence() {
        let first = Candidate::new("paper.tex", 90, 95, "first evidence")
            .match_kind(required_evidence_marker(0, 0))
            .exact(1.0);
        let second = Candidate::new("paper.tex", 140, 145, "second evidence")
            .match_kind(required_evidence_marker(0, 1))
            .exact(1.0);
        let general = Candidate::new("general.rs", 1, 1, "general").exact(10.0);
        let mut request = request_with_budget(100);
        request.required_evidence = vec![crate::model::ContextRequiredEvidence {
            path: "paper.tex".into(),
            queries: vec!["first".into(), "second".into()],
            minimum_query_matches: 2,
        }];
        request.max_fragments = Some(2);

        let response = select(vec![general, first, second], &request, 1);

        assert_eq!(response.fragments.len(), 2);
        assert!(
            response
                .fragments
                .iter()
                .all(|fragment| fragment.path == "paper.tex")
        );
        assert_eq!(response.coverage.evidence_scope_satisfied, Some(true));
        assert_eq!(
            response.coverage.required_evidence[0].matched_queries,
            ["first", "second"]
        );
    }

    #[test]
    fn uncovered_must_cover_requirements_are_explicit() {
        let mut request = request_with_budget(100);
        request.must_include_paths = vec!["src/missing.rs".into()];
        request.must_include_symbols = vec!["missing_symbol".into()];

        let response = select(
            vec![Candidate::new("src/general.rs", 1, 1, "general").exact(1.0)],
            &request,
            1,
        );

        assert_eq!(
            response.coverage.uncovered_must_include_paths,
            vec!["src/missing.rs"]
        );
        assert_eq!(
            response.coverage.uncovered_must_include_symbols,
            vec!["missing_symbol"]
        );
    }

    #[test]
    fn known_hash_satisfies_must_cover_without_resending_source() {
        let required = Candidate::new("src/required.rs", 1, 1, "required")
            .symbol_name("required_symbol")
            .target_range(1, 1)
            .exact(1.0);
        let known_hash = required.content_hash();
        let mut request = request_with_budget(100);
        request.must_include_paths = vec!["src/required.rs".into()];
        request.must_include_symbols = vec!["required_symbol".into()];
        request.known_hashes = vec![known_hash];

        let response = select(vec![required], &request, 1);

        assert!(response.fragments.is_empty());
        assert_eq!(response.omission_summary.known_hash, 1);
        assert_eq!(
            response.coverage.covered_must_include_paths,
            vec!["src/required.rs"]
        );
        assert_eq!(
            response.coverage.covered_must_include_symbols,
            vec!["required_symbol"]
        );
        assert!(response.coverage.uncovered_must_include_paths.is_empty());
        assert!(response.coverage.uncovered_must_include_symbols.is_empty());
    }

    #[test]
    fn partial_required_symbol_is_selected_but_not_reported_as_complete() {
        let required = Candidate::new("src/required.rs", 10, 20, "partial definition")
            .symbol_name("required_symbol")
            .target_range(10, 40)
            .exact(1.0);
        let mut request = request_with_budget(100);
        request.must_include_symbols = vec!["required_symbol".into()];

        let response = select(vec![required], &request, 1);

        assert_eq!(response.fragments.len(), 1);
        assert_eq!(response.fragments[0].target_start_line, Some(10));
        assert_eq!(response.fragments[0].target_end_line, Some(40));
        assert!(response.fragments[0].truncated);
        assert!(response.coverage.covered_must_include_symbols.is_empty());
        assert_eq!(
            response.coverage.partial_must_include_symbols,
            vec!["required_symbol"]
        );
        assert!(response.coverage.uncovered_must_include_symbols.is_empty());
    }

    #[test]
    fn dedup_preserves_required_symbol_target_metadata() {
        let general = Candidate::new("src/required.rs", 10, 20, "same excerpt")
            .symbol_name("other_symbol")
            .exact(10.0);
        let required = Candidate::new("src/required.rs", 10, 20, "same excerpt")
            .representation("required_symbol")
            .symbol_name("required_symbol")
            .target_range(10, 40)
            .exact(1.0);
        let mut request = request_with_budget(100);
        request.must_include_symbols = vec!["required_symbol".into()];

        let response = select(vec![general, required], &request, 1);

        assert_eq!(response.fragments.len(), 1);
        assert_eq!(response.fragments[0].representation, "required_symbol");
        assert_eq!(response.fragments[0].target_start_line, Some(10));
        assert_eq!(response.fragments[0].target_end_line, Some(40));
        assert!(response.fragments[0].truncated);
        assert_eq!(
            response.coverage.partial_must_include_symbols,
            vec!["required_symbol"]
        );
    }

    #[test]
    fn concept_allocation_keeps_independent_task_evidence() {
        let alpha_best = Candidate::new("alpha.rs", 1, 1, "alpha evidence")
            .concept("alpha", 1.0)
            .exact(2.0);
        let alpha_duplicate = Candidate::new("alpha_other.rs", 1, 1, "more alpha")
            .concept("alpha", 1.0)
            .exact(1.5);
        let beta = Candidate::new("beta.rs", 1, 1, "beta evidence")
            .concept("beta", 1.0)
            .exact(0.1);

        let response = select(
            vec![alpha_duplicate, beta, alpha_best],
            &request_with_budget(6),
            1,
        );

        assert!(
            response
                .fragments
                .iter()
                .any(|fragment| fragment.path == "alpha.rs")
        );
        assert!(
            response
                .fragments
                .iter()
                .any(|fragment| fragment.path == "beta.rs")
        );
    }

    #[test]
    fn decisive_second_view_prefers_the_definition_path() {
        let definition = Candidate::new("owner.rs", 1, 1, "definition")
            .concept("handle", 2.0)
            .representation("symbol")
            .exact(10.0);
        let owner_source = Candidate::new("owner.rs", 10, 10, "owner_source")
            .concept("handle", 2.0)
            .exact(0.5);
        let unrelated_source = Candidate::new("other.rs", 1, 1, "other ".repeat(3_000))
            .concept("handle", 2.0)
            .exact(1.0);

        let response = select(
            vec![unrelated_source, owner_source, definition],
            &request_with_budget(1_200),
            1,
        );

        assert_eq!(response.fragments.len(), 2);
        assert_eq!(response.fragments[0].path, "owner.rs");
        assert_eq!(response.fragments[1].path, "owner.rs");
    }

    #[test]
    fn weak_non_code_fill_is_omitted_by_relative_confidence() {
        let strong = Candidate::new("strong.rs", 1, 1, "strong")
            .concept("explicit", 1.0)
            .exact(10.0);
        let weak = Candidate::new("weak.rs", 1, 1, "weak").exact(0.0);

        let response = select(vec![weak, strong], &request_with_budget(100), 1);

        assert_eq!(response.fragments.len(), 1);
        assert_eq!(response.fragments[0].path, "strong.rs");
        assert!(
            response
                .warnings
                .iter()
                .any(|warning| warning.contains("omitted"))
        );
    }
