use super::*;

#[tokio::test]
async fn context_structural_scan_limit_does_not_prevent_scoped_lexical_recovery() {
    let root = tempfile::tempdir().expect("temporary repository");
    let source = (0..10_001)
        .map(|index| format!("fn f{index}() {{}}\n"))
        .collect::<String>();
    std::fs::write(root.path().join("unrelated.rs"), source).expect("write structural distractors");
    std::fs::write(root.path().join("target.txt"), "ssssss\n").expect("write lexical evidence");
    let config =
        Config::discover(root.path(), Some(root.path().join("index.sqlite"))).expect("config");
    let services = Services::open(config).expect("services");
    services
        .index(leantoken::IndexingMode::Reconcile)
        .await
        .expect("index");
    let mut request = context_limit_request(2_000);
    request.task = "ssssss".into();
    request.include_paths = vec!["target.txt".into()];
    let response = services
        .context(request)
        .await
        .expect("lexical source remains available");
    assert_eq!(response.fragments.len(), 1);
    assert_eq!(response.fragments[0].path, "target.txt");
    assert!(response.fragments[0].content.contains("ssssss"));
    assert!(
        response
            .warnings
            .iter()
            .any(|warning| warning.contains("unicode_case_fold_rows"))
    );
}

#[tokio::test]
async fn context_scope_is_applied_before_each_ranked_candidate_limit() {
    let root = tempfile::tempdir().expect("temporary repository");
    for index in 0..45 {
        std::fs::write(
            root.path().join(format!("a{index:02}.rs")),
            "pub fn unique_primary_needle() { unique_primary_needle(); }\n",
        )
        .expect("write distractor");
    }
    std::fs::write(
        root.path().join("z_owner.rs"),
        "pub fn unique_primary_needle() { unique_primary_needle(); }\n",
    )
    .expect("write owner");
    let config =
        Config::discover(root.path(), Some(root.path().join("index.sqlite"))).expect("config");
    let services = Services::open(config).expect("services");
    services
        .index(leantoken::IndexingMode::Reconcile)
        .await
        .expect("index");
    for scope in 0..3 {
        let mut request = context_limit_request(2_000);
        request.task = "unique_primary_needle".into();
        match scope {
            0 => request.include_paths = vec!["z_owner.rs".into()],
            1 => request.exclude_paths = vec!["a*.rs".into()],
            _ => {
                request.changed_paths = vec!["z_owner.rs".into()];
                request.strict_changed_paths = true;
            }
        }
        let response = services.context(request).await.expect("scoped context");
        assert!(
            !response.fragments.is_empty(),
            "scope {scope} must recover the owner"
        );
        assert!(
            response
                .fragments
                .iter()
                .all(|fragment| fragment.path == "z_owner.rs")
        );
        assert!(response.meta.source_tokens <= 2_000);
    }
}

#[tokio::test]
async fn context_retains_other_evidence_when_a_unicode_fallback_exceeds_its_limit() {
    let root = tempfile::tempdir().expect("temporary repository");
    std::fs::write(
        root.path().join("owner.rs"),
        "pub fn unique_primary_needle() {}\n",
    )
    .expect("write owner");
    for index in 0..35 {
        std::fs::write(root.path().join(format!("text{index:02}.txt")), "ssssss\n")
            .expect("write Unicode-fold fallback match");
    }
    let config =
        Config::discover(root.path(), Some(root.path().join("index.sqlite"))).expect("config");
    let services = Services::open(config).expect("services");
    services
        .index(leantoken::IndexingMode::Reconcile)
        .await
        .expect("index");
    for plan_only in [false, true] {
        let mut request = context_limit_request(2_000);
        request.task = "unique_primary_needle ssssss".into();
        request.plan_only = plan_only;
        let response = services
            .context(request)
            .await
            .expect("bounded partial context");
        assert!(
            response
                .warnings
                .iter()
                .any(|warning| warning.contains("candidate generation incomplete"))
        );
        if let Some(plan) = response.plan {
            assert!(!plan.result_complete);
            assert!(
                plan.candidates
                    .iter()
                    .any(|candidate| candidate.path == "owner.rs")
            );
        } else {
            assert!(
                response
                    .fragments
                    .iter()
                    .any(|fragment| fragment.path == "owner.rs")
            );
        }
    }

    let mut request = context_limit_request(2_000);
    request.task = "ssssss".into();
    let response = services
        .context(request.clone())
        .await
        .expect("partial lexical evidence");
    assert!(
        response
            .fragments
            .iter()
            .any(|fragment| fragment.content.contains("ssssss"))
    );
    assert!(
        response
            .warnings
            .iter()
            .any(|warning| warning.contains("candidate generation incomplete"))
    );

    request.changed_paths = vec!["text34.txt".into()];
    request.strict_changed_paths = true;
    let response = services
        .context(request)
        .await
        .expect("late scoped literal match");
    assert_eq!(response.fragments.len(), 1);
    assert_eq!(response.fragments[0].path, "text34.txt");
    assert!(
        !response
            .warnings
            .iter()
            .any(|warning| warning.contains("candidate generation incomplete"))
    );
}

#[tokio::test]
async fn required_evidence_does_not_transfer_to_overlapping_content() {
    let root = tempfile::tempdir().expect("temporary repository");
    let evidence_path = root.path().join("paper/evidence.txt");
    std::fs::create_dir_all(evidence_path.parent().expect("evidence parent"))
        .expect("evidence directory");
    let content = (1..=160)
        .map(|line| match line {
            80 => "EVIDENCE_ONLY_LITERAL appears only at the chunk boundary.".to_owned(),
            100 => "retained_overlap_alpha is discussed in the retained chunk.".to_owned(),
            101 => "retained_overlap_beta is also discussed in the retained chunk.".to_owned(),
            _ => format!("ordinary background line {line}"),
        })
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(&evidence_path, content).expect("write evidence fixture");
    let config =
        Config::discover(root.path(), Some(root.path().join("index.sqlite"))).expect("config");
    let services = Services::open(config).expect("services");
    services
        .index(leantoken::IndexingMode::Reconcile)
        .await
        .expect("index fixture");

    let response = services
        .context(ContextRequest {
            task: "inspect retained_overlap_alpha and retained_overlap_beta".into(),
            token_budget: 1_000,
            include_paths: Vec::new(),
            must_include_paths: vec!["paper/evidence.txt".into()],
            must_include_symbols: Vec::new(),
            required_evidence: vec![ContextRequiredEvidence {
                path: "paper/evidence.txt".into(),
                queries: vec!["EVIDENCE_ONLY_LITERAL".into()],
                minimum_query_matches: 1,
            }],
            max_fragments: Some(1),
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
        })
        .await
        .expect("required evidence context");

    assert_eq!(response.fragments.len(), 1);
    assert!(
        response.fragments[0]
            .content
            .contains("EVIDENCE_ONLY_LITERAL")
    );
    assert_eq!(response.coverage.evidence_scope_satisfied, Some(true));
    assert_eq!(
        response.coverage.required_evidence[0].matched_queries,
        ["EVIDENCE_ONLY_LITERAL"]
    );
}

#[tokio::test]
async fn broad_context_reserves_primary_owner_before_auxiliary_facets() {
    let root = tempfile::tempdir().expect("temporary repository");
    std::fs::write(
        root.path().join(".git"),
        "gitdir: fixture-has-no-repository\n",
    )
    .expect("create Git boundary");
    for directory in [
        "src/services",
        "src/mcp/requests",
        "src/mcp/snapshots",
        "tests",
        "fixtures",
        ".agents/skills/context-helper",
    ] {
        std::fs::create_dir_all(root.path().join(directory)).expect("create fixture directory");
    }
    std::fs::write(
        root.path().join("src/services/dispatch.rs"),
        r#"pub fn resolve_initial_context(index_state: IndexState) -> Result<Generation> {
    if index_state == IndexState::Uninitialized {
        return initialize_atomic_generation();
    }
    current_generation()
}
"#,
    )
    .expect("write production owner");
    std::fs::write(
        root.path().join("src/mcp/requests/context.rs"),
        "// context request schema preserves MCP startup snapshot consistency\n\
         pub struct ContextRequestSchema { pub index_not_ready: bool }\n",
    )
    .expect("write request schema");
    std::fs::write(
        root.path().join("src/mcp/snapshots/context.snap"),
        "context index_not_ready initial database MCP startup snapshot consistency schema\n",
    )
    .expect("write snapshot");
    std::fs::write(
        root.path().join("tests/context.rs"),
        "#[test] fn context_regression_preserves_mcp_startup() { /* index_not_ready */ }\n",
    )
    .expect("write owner test");
    std::fs::write(
        root.path().join("fixtures/context.txt"),
        "context index_not_ready initial database MCP startup snapshot consistency fixture\n",
    )
    .expect("write fixture artifact");
    std::fs::write(
        root.path().join(".agents/skills/context-helper/SKILL.md"),
        "# Context helper\nPreserve MCP startup and snapshot consistency after index_not_ready.\n",
    )
    .expect("write skill");
    std::fs::write(
        root.path().join("context_research.md"),
        "# Context research\nInitial database context index_not_ready MCP startup snapshot consistency.\n",
    )
    .expect("write root research");

    let config =
        Config::discover(root.path(), Some(root.path().join("index.sqlite"))).expect("config");
    let services = Services::open(config).expect("services");
    services
        .index(leantoken::IndexingMode::Reconcile)
        .await
        .expect("index fixture");
    let mut request = context_limit_request(1_200);
    request.max_fragments = Some(6);
    request.task = "Fix direct CLI context on an initial database so it initializes the first \
        atomic generation instead of index_not_ready. Preserve MCP startup and snapshot \
        consistency. Add a context regression test."
        .into();

    let evaluation = services
        .context_evaluation(request)
        .await
        .expect("evaluate broad context");
    let paths = evaluation
        .response
        .fragments
        .iter()
        .map(|fragment| fragment.path.as_str())
        .collect::<Vec<_>>();
    let candidate_diagnostics = evaluation
        .generated_candidates
        .iter()
        .map(|candidate| {
            (
                candidate.path.as_str(),
                candidate.start_line,
                candidate.score,
                &candidate.match_kinds,
            )
        })
        .collect::<Vec<_>>();

    assert_eq!(
        paths.first(),
        Some(&"src/services/dispatch.rs"),
        "paths={paths:?} candidates={candidate_diagnostics:#?}"
    );
    assert!(evaluation.generated_candidates.iter().any(|candidate| {
        candidate.path == "src/services/dispatch.rs"
            && candidate
                .match_kinds
                .iter()
                .any(|kind| kind.starts_with("facet:primary_change:"))
    }));
    let selected_failures = evaluation
        .response
        .fragments
        .iter()
        .filter(|fragment| {
            evaluation.generated_candidates.iter().any(|candidate| {
                candidate.path == fragment.path
                    && candidate.start_line == fragment.start_line
                    && candidate.end_line == fragment.end_line
                    && candidate.representation == fragment.representation
                    && candidate
                        .match_kinds
                        .iter()
                        .any(|kind| kind.starts_with("facet:failure_trace:"))
            })
        })
        .count();
    assert!(
        (1..=2).contains(&selected_failures),
        "failure evidence quota was not enforced: {paths:?}"
    );
    let auxiliary = paths
        .iter()
        .filter(|path| {
            path.starts_with("fixtures/")
                || path.starts_with(".agents/")
                || path.contains("/snapshots/")
                || **path == "context_research.md"
        })
        .count();
    assert!(auxiliary <= 1, "auxiliary quota exceeded: {paths:?}");
    let selected_tests = paths
        .iter()
        .filter(|path| path.starts_with("tests/"))
        .count();
    assert!(
        (1..=2).contains(&selected_tests),
        "test reservation or quota failed: {paths:?}"
    );
    let selected_preservation = evaluation
        .response
        .fragments
        .iter()
        .filter(|fragment| {
            evaluation.generated_candidates.iter().any(|candidate| {
                candidate.path == fragment.path
                    && candidate.start_line == fragment.start_line
                    && candidate.end_line == fragment.end_line
                    && candidate.representation == fragment.representation
                    && candidate
                        .match_kinds
                        .iter()
                        .any(|kind| kind.starts_with("facet:preserve_constraint:"))
            })
        })
        .count();
    assert!(
        (1..=2).contains(&selected_preservation),
        "preservation reservation or quota failed: {paths:?}"
    );
}
