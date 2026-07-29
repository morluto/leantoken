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
        assert!(response.meta.source_tokens < 1_200);
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
    fn focus_diagnostics_explain_soft_production_displacement() {
        let production =
            Candidate::new("src/owner.rs", 1, 2, "production evidence").exact(0.01);
        let example = Candidate::new("examples/demo.rs", 1, 2, "example evidence").exact(20.0);
        let mut request = request_focused(100, "src/owner.rs");
        request.max_fragments = Some(1);
        request.verbose_diagnostics = true;

        let response = select(vec![production, example], &request, 1);

        assert_eq!(response.fragments.len(), 1);
        assert_eq!(response.fragments[0].path, "examples/demo.rs");
        let coverage = &response.coverage.focus_path_coverage[0];
        assert_eq!(coverage.selected_fragments, 0);
        assert!(!coverage.satisfied);
        let diagnostics = coverage
            .diagnostics
            .as_ref()
            .expect("focus allocation diagnostics");
        assert_eq!(diagnostics.generated_fragments, 1);
        assert_eq!(diagnostics.reserved_fragments, 0);
        assert_eq!(diagnostics.selected_source_tokens, 0);
        assert_eq!(
            diagnostics.suppressed_by,
            vec![ContextFocusSuppression {
                boundary: ContextFocusSuppressionBoundary::GlobalRanking,
                fragments: 1,
            }]
        );
        assert_eq!(
            diagnostics.capacity_blocker,
            Some(ContextFocusCapacityBlocker::GlobalRanking)
        );
    }

    #[test]
    fn focus_diagnostics_distinguish_hash_token_and_dedup_suppression() {
        let known = Candidate::new("known.rs", 1, 1, "known evidence").exact(1.0);
        let mut known_request = request_focused(100, "known.rs");
        known_request.strict_focus_paths = true;
        known_request.known_hashes = vec![known.content_hash()];
        known_request.verbose_diagnostics = true;
        let known_response = select(vec![known], &known_request, 1);
        let known_diagnostics = known_response.coverage.focus_path_coverage[0]
            .diagnostics
            .as_ref()
            .expect("known-hash diagnostics");
        assert_eq!(
            known_diagnostics.suppressed_by,
            vec![ContextFocusSuppression {
                boundary: ContextFocusSuppressionBoundary::KnownHash,
                fragments: 1,
            }]
        );
        assert_eq!(
            known_diagnostics.capacity_blocker,
            Some(ContextFocusCapacityBlocker::KnownHash)
        );

        let expensive =
            Candidate::new("expensive.rs", 1, 20, "expensive ".repeat(40)).exact(1.0);
        let mut token_request = request_focused(1, "expensive.rs");
        token_request.strict_focus_paths = true;
        token_request.verbose_diagnostics = true;
        let token_response = select(vec![expensive], &token_request, 1);
        let token_diagnostics = token_response.coverage.focus_path_coverage[0]
            .diagnostics
            .as_ref()
            .expect("token diagnostics");
        assert!(
            token_diagnostics
                .suppressed_by
                .iter()
                .any(|suppression| suppression.boundary
                    == ContextFocusSuppressionBoundary::TokenBudget)
        );
        assert_eq!(
            token_diagnostics.capacity_blocker,
            Some(ContextFocusCapacityBlocker::TokenBudget)
        );

        let first = Candidate::new("overlap.rs", 1, 10, "first evidence").exact(2.0);
        let second = Candidate::new("overlap.rs", 5, 14, "second evidence").exact(1.0);
        let mut dedup_request = request_focused(100, "overlap.rs");
        dedup_request.strict_focus_paths = true;
        dedup_request.minimum_fragments_per_focus_path = Some(2);
        dedup_request.max_fragments = Some(2);
        dedup_request.verbose_diagnostics = true;
        let dedup_response = select(vec![first, second], &dedup_request, 1);
        let dedup_diagnostics = dedup_response.coverage.focus_path_coverage[0]
            .diagnostics
            .as_ref()
            .expect("dedup diagnostics");
        assert_eq!(dedup_diagnostics.generated_fragments, 2);
        assert!(dedup_diagnostics.selected_source_tokens > 0);
        assert!(
            dedup_diagnostics
                .suppressed_by
                .iter()
                .any(|suppression| suppression.boundary
                    == ContextFocusSuppressionBoundary::Deduplicated)
        );
        assert_eq!(
            dedup_diagnostics.capacity_blocker,
            Some(ContextFocusCapacityBlocker::Deduplicated)
        );

        let excluded = Candidate::new("src/excluded.rs", 1, 1, "excluded").exact(1.0);
        let mut policy_request = request_focused(100, "src/**");
        policy_request.strict_focus_paths = true;
        policy_request.exclude_paths = vec!["src/**".into()];
        policy_request.verbose_diagnostics = true;
        let policy_response = select(vec![excluded], &policy_request, 1);
        let policy_diagnostics = policy_response.coverage.focus_path_coverage[0]
            .diagnostics
            .as_ref()
            .expect("path policy diagnostics");
        assert_eq!(
            policy_diagnostics.suppressed_by,
            vec![ContextFocusSuppression {
                boundary: ContextFocusSuppressionBoundary::PathPolicy,
                fragments: 1,
            }]
        );
        assert_eq!(
            policy_diagnostics.capacity_blocker,
            Some(ContextFocusCapacityBlocker::PathPolicy)
        );

        let first = Candidate::new("src/owner.rs", 1, 1, "first").exact(2.0);
        let second = Candidate::new("src/owner.rs", 10, 10, "second").exact(1.0);
        let other = Candidate::new("examples/demo.rs", 1, 1, "other").exact(0.5);
        let mut diversity_request = request_focused(100, "src/owner.rs");
        diversity_request.verbose_diagnostics = true;
        let diversity_response = select(vec![first, second, other], &diversity_request, 1);
        let diversity_diagnostics = diversity_response.coverage.focus_path_coverage[0]
            .diagnostics
            .as_ref()
            .expect("file diversity diagnostics");
        assert!(
            diversity_diagnostics
                .suppressed_by
                .iter()
                .any(|suppression| suppression.boundary
                    == ContextFocusSuppressionBoundary::FileDiversity)
        );
        assert_eq!(diversity_diagnostics.capacity_blocker, None);
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
