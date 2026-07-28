    #[test]
    fn context_plan_matches_materialized_selection_without_source() {
        let focused = Candidate::new("src/ranking.rs", 10, 12, "focused evidence")
            .match_kind("symbol")
            .exact(2.0);
        let other = Candidate::new("src/other.rs", 20, 21, "other evidence").match_kind("text");
        let candidates = vec![other, focused];
        let mut request = request_focused(100, "src/ranking.rs");
        request.max_fragments = Some(1);
        request.plan_only = true;

        let preview = select(candidates.clone(), &request, 7);
        let plan = preview.plan.as_ref().expect("query plan");

        assert!(preview.fragments.is_empty());
        assert!(preview.receipt.fragment_hashes.is_empty());
        assert_eq!(preview.meta.source_tokens, 0);
        assert_eq!(preview.meta.emitted_tokens, 0);
        assert!(!plan.candidates.is_empty());
        assert_eq!(plan.candidates.len(), 1);
        assert!(!plan.result_complete);
        assert!(
            plan.candidates
                .iter()
                .all(|candidate| candidate.score >= 0.0)
        );
        assert!(
            plan.candidates
                .iter()
                .all(|candidate| !candidate.reasons.is_empty())
        );
        assert_eq!(
            plan.estimated_source_tokens,
            plan.candidates
                .iter()
                .map(|candidate| candidate.estimated_tokens)
                .sum::<usize>()
        );
        assert_eq!(plan.focus_coverage.len(), 1);
        assert!(plan.focus_coverage[0].satisfied);

        request.plan_only = false;
        let materialized = select(candidates, &request, 7);
        assert!(materialized.plan.is_none());
        assert_eq!(
            plan.candidates
                .iter()
                .map(|candidate| (&candidate.path, candidate.start_line, candidate.end_line))
                .collect::<Vec<_>>(),
            materialized
                .fragments
                .iter()
                .map(|fragment| (&fragment.path, fragment.start_line, fragment.end_line))
                .collect::<Vec<_>>()
        );
        assert_eq!(
            plan.estimated_source_tokens,
            materialized.meta.source_tokens
        );
    }

    #[test]
    fn context_plan_warns_when_generated_defaults_match() {
        let generated =
            Candidate::new("artifacts/runtime_reports/latest.json", 1, 2, "generated").exact(10.0);
        let source = Candidate::new("src/runtime.rs", 1, 2, "source").exact(0.5);
        let mut request = request_with_budget(20);
        request.plan_only = true;

        let response = select(vec![generated, source], &request, 1);
        let plan = response.plan.expect("query plan");

        assert!(plan.generated_artifact_warning);
        assert!(
            response
                .warnings
                .iter()
                .any(|warning| warning.contains("generated-artifact"))
        );
        assert!(
            plan.candidates
                .iter()
                .all(|candidate| candidate.path != "artifacts/runtime_reports/latest.json")
        );
    }

    #[test]
    fn focus_path_boosts_selection() {
        let focus = Candidate::new("src/ranking.rs", 1, 2, "alpha").exact(0.5);
        let other = Candidate::new("src/other.rs", 1, 2, "beta").exact(0.5);

        let req = request_focused(10, "src/ranking.rs");
        let resp = select(vec![other, focus], &req, 1);

        assert_eq!(resp.fragments.len(), 2);
        // Higher combined score should place the focus candidate first.
        assert_eq!(resp.fragments[0].path, "src/ranking.rs");
    }

    #[test]
    fn focus_symbol_boosts_selection() {
        let focus = Candidate::new("a.rs", 1, 2, "alpha")
            .exact(0.5)
            .symbol_name("rank_items");
        let other = Candidate::new("b.rs", 1, 2, "beta")
            .exact(0.5)
            .symbol_name("other");

        let mut req = request_with_budget(10);
        req.focus_symbols.push("rank_items".into());

        let resp = select(vec![other, focus], &req, 1);

        assert_eq!(resp.fragments[0].path, "a.rs");
    }

    #[test]
    fn budget_omits_low_value_candidates() {
        let tiny = Candidate::new("tiny.rs", 1, 1, "alpha").exact(1.0);
        let huge = Candidate::new(
            "huge.rs",
            1,
            1,
            (0..200).map(|i| format!("token{i} ")).collect::<String>(),
        )
        .exact(0.9);

        let mut req = request_with_budget(5);
        req.verbose_diagnostics = true;
        let resp = select(vec![huge, tiny], &req, 1);

        // tiny should be selected; huge should not fit in a budget of 5 tokens.
        assert_eq!(resp.fragments.len(), 1);
        assert_eq!(resp.fragments[0].path, "tiny.rs");
        assert!(!resp.omitted.is_empty());
    }

    #[test]
    fn evidence_receipt_populated() {
        let c = Candidate::new("a.rs", 1, 2, "alpha beta").exact(1.0);

        let req = request_with_budget(10);
        let resp = select(vec![c], &req, 42);

        assert_eq!(resp.meta.repository_generation, 42);
        assert!(!resp.receipt.task_fingerprint.is_empty());
        assert_eq!(resp.receipt.fragment_hashes.len(), resp.fragments.len());
        assert_eq!(
            resp.meta.emitted_tokens,
            resp.fragments.iter().map(|f| f.token_count).sum::<usize>()
        );
        assert_eq!(resp.meta.source_tokens, resp.meta.emitted_tokens);
        assert_eq!(resp.meta.tokenizer, tokens::Tokenizer::default().name());
        let mut countable = resp.clone();
        countable.meta.protocol_tokens = 0;
        countable.meta.path_and_metadata_tokens = 0;
        countable.meta.total_response_tokens = 0;
        countable.meta.payload_tokens = 0;
        let payload = serde_json::to_string(&countable).expect("serialize context response");
        assert_eq!(
            resp.meta.total_response_tokens,
            tokens::Tokenizer::default().count(&payload)
        );
        assert_eq!(resp.meta.payload_tokens, resp.meta.total_response_tokens);
        assert_eq!(
            resp.meta.total_response_tokens,
            resp.meta.source_tokens
                + resp.meta.protocol_tokens
                + resp.meta.path_and_metadata_tokens
        );
        assert!(resp.meta.token_count_exact);
    }

    #[test]
    fn explicit_weights_and_tokenizer_control_budget_metadata() {
        let candidate = Candidate::new("a.rs", 1, 1, "alpha beta gamma").exact(1.0);
        let request = request_with_budget(20);
        let response = select_with_weights_and_tokenizer(
            vec![candidate],
            &request,
            7,
            &Weights::default(),
            tokens::Tokenizer::Estimate,
        );

        assert!(!response.meta.token_count_exact);
        assert_eq!(response.meta.source_tokens, response.meta.emitted_tokens);
        assert_eq!(response.meta.tokenizer, tokens::Tokenizer::Estimate.name());
        assert_eq!(response.meta.emitted_tokens, 4);
    }

    #[test]
    fn empty_pool_returns_empty_response() {
        let req = request_with_budget(100);
        let resp = select(Vec::new(), &req, 1);

        assert!(resp.fragments.is_empty());
        assert!(resp.omitted.is_empty());
        assert!(resp.receipt.fragment_hashes.is_empty());
    }

    #[test]
    fn change_boost_increases_score() {
        let w = Weights::default();
        let base = Candidate::new("a.rs", 1, 1, "x").exact(1.0);
        let changed = Candidate::new("a.rs", 1, 1, "x")
            .exact(1.0)
            .change_boost(1.0);

        assert!(changed.score(&w, changed.token_count()) > base.score(&w, base.token_count()));
    }

    #[test]
    fn import_boost_increases_score() {
        let w = Weights::default();
        let base = Candidate::new("a.rs", 1, 1, "x").exact(1.0);
        let imported = Candidate::new("a.rs", 1, 1, "x")
            .exact(1.0)
            .import_boost(1.0);

        assert!(imported.score(&w, imported.token_count()) > base.score(&w, base.token_count()));
    }
