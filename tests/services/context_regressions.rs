use super::*;

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
        .refresh(leantoken::IndexingMode::Reconcile)
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
        .refresh(leantoken::IndexingMode::Reconcile)
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

#[tokio::test]
async fn context_declaration_excerpt_retains_long_body_across_chunks() {
    let root = tempfile::tempdir().expect("root");
    let body = (1..=48)
        .map(|line| format!("    let value_{line} = {line};\n"))
        .collect::<String>();
    std::fs::write(
        root.path().join("lib.rs"),
        format!("fn target_symbol() {{\n{body}    consume(value_48);\n}}\n"),
    )
    .expect("source");
    let mut config =
        Config::discover(root.path(), Some(root.path().join("index.sqlite"))).expect("config");
    config.chunk_lines = 3;
    let services = Services::open(config).expect("services");
    services
        .refresh(leantoken::IndexingMode::Reconcile)
        .await
        .expect("refresh");

    let response = services
        .context(ContextRequest {
            task: "fix target_symbol".into(),
            token_budget: 600,
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
        })
        .await
        .expect("context");
    let declaration = response
        .fragments
        .iter()
        .find(|fragment| fragment.path == "lib.rs" && fragment.start_line == 1)
        .expect("declaration fragment");

    assert_eq!(declaration.end_line, 51);
    assert!(declaration.content.contains("consume(value_48)"));
}

#[tokio::test]
async fn context_text_hits_use_bounded_declaration_excerpts() {
    let root = tempfile::tempdir().expect("root");
    let body = (1..=160)
        .map(|line| format!("    let filler_{line} = {line};\n"))
        .collect::<String>();
    std::fs::write(
        root.path().join("lib.rs"),
        format!(
            "fn very_large_handler() {{\n{body}    let rare_runtime_marker = filler_160;\n    consume(rare_runtime_marker);\n}}\n"
        ),
    )
    .expect("source");
    let config =
        Config::discover(root.path(), Some(root.path().join("index.sqlite"))).expect("config");
    let services = Services::open(config).expect("services");
    services
        .refresh(leantoken::IndexingMode::Reconcile)
        .await
        .expect("refresh");

    let response = services
        .context(ContextRequest {
            task: "fix rare_runtime_marker behavior".into(),
            token_budget: 1200,
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
        })
        .await
        .expect("context");
    let text_fragment = response
        .fragments
        .iter()
        .find(|fragment| fragment.path == "lib.rs" && fragment.reason.contains("text"))
        .expect("text fragment");

    assert!(
        text_fragment.token_count <= 320,
        "oversized text fragment: {text_fragment:?}"
    );
    assert!(text_fragment.content.contains("rare_runtime_marker"));
}
