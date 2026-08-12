use super::*;

#[tokio::test]
async fn contribution_context_routes_to_guidance_validation_and_owner_tests() {
    let root = tempfile::tempdir().expect("root");
    std::fs::create_dir_all(root.path().join("src")).expect("src");
    std::fs::create_dir_all(root.path().join("tests")).expect("tests");
    std::fs::create_dir_all(root.path().join("docs")).expect("docs");
    std::fs::create_dir_all(root.path().join(".github/workflows")).expect("workflows");
    for (path, content) in [
        (
            "src/parser.rs",
            "pub fn parse_contribution_target() -> bool { true }\n",
        ),
        (
            "tests/parser.rs",
            "#[test]\nfn parser_contract() { assert!(true); }\n",
        ),
        (
            "AGENTS.md",
            "# Contribution rules\nRun focused tests before full validation.\n",
        ),
        (
            "docs/development.md",
            "# Development\nUse cargo fmt, clippy, and test.\n",
        ),
        (
            ".github/workflows/ci.yml",
            "name: CI\njobs: { test: { runs-on: ubuntu-latest } }\n",
        ),
        (
            "docs/release.md",
            "# Parser contribution release archive\nUnrelated historical release notes.\n",
        ),
    ] {
        std::fs::write(root.path().join(path), content).expect("fixture");
    }
    let config =
        Config::discover(root.path(), Some(root.path().join("index.sqlite"))).expect("config");
    let services = Services::open(config).expect("services");
    services
        .refresh(leantoken::IndexingMode::Reconcile)
        .await
        .expect("index");

    let response = services
        .context_with_workflow_consistency_cancellable(
            ContextRequest {
                task: "prepare a contribution for parse_contribution_target".into(),
                token_budget: 1_000,
                include_paths: Vec::new(),
                must_include_paths: Vec::new(),
                must_include_symbols: Vec::new(),
                required_evidence: Vec::new(),
                max_fragments: None,
                plan_only: false,
                focus_paths: Vec::new(),
                strict_focus_paths: false,
                minimum_fragments_per_focus_path: None,
                focus_symbols: vec!["parse_contribution_target".into()],
                exclude_paths: Vec::new(),
                known_hashes: Vec::new(),
                receipt_id: None,
                prior_repository_generation: None,
                base_revision: None,
                changed_paths: vec!["src/parser.rs".into()],
                strict_changed_paths: false,
                explain_diagnostics: false,
            },
            ContextWorkflow::Contribution,
            IndexConsistency::IndexedGeneration,
            CancellationToken::new(),
        )
        .await
        .expect("contribution context");

    let paths = response
        .fragments
        .iter()
        .map(|fragment| fragment.path.as_str())
        .collect::<std::collections::HashSet<_>>();
    assert_eq!(response.workflow, ContextWorkflow::Contribution);
    let receipt = response
        .workflow_receipt
        .as_ref()
        .expect("workflow receipt");
    assert_eq!(receipt.guidance_candidates, 2);
    assert_eq!(receipt.validation_candidates, 1);
    assert_eq!(receipt.owner_test_candidates, 1);
    assert_eq!(receipt.missing_families, vec!["template"]);
    let diff_evidence = response
        .diff_scope
        .as_ref()
        .and_then(|scope| scope.evidence.as_ref())
        .expect("diff evidence");
    assert!(
        diff_evidence
            .changed_symbols
            .iter()
            .any(|symbol| symbol.name == "parse_contribution_target")
    );
    assert!(diff_evidence.related_paths.iter().any(|relationship| {
        relationship.related_path == "tests/parser.rs" && relationship.signal == "test_name_match"
    }));
    assert!(paths.contains("AGENTS.md"));
    assert!(paths.contains("docs/development.md"));
    assert!(paths.contains(".github/workflows/ci.yml"));
    assert!(paths.contains("tests/parser.rs"));
}
