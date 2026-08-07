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
    services.index(false).await.expect("index fixture");

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
