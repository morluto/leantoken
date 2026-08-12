use super::*;

#[tokio::test]
async fn import_expansion_is_exact_safe_and_requires_corroborated_symbols() {
    let root = tempfile::tempdir().expect("root");
    std::fs::create_dir(root.path().join("src")).expect("src");
    std::fs::write(
        root.path().join("src/seed.js"),
        "import { OwnerAlpha } from './target.js';\nexport function useOwner() { return new OwnerAlpha(); }\n",
    )
    .expect("seed");
    std::fs::write(
        root.path().join("src/target.js"),
        format!(
            "export class OwnerAlpha {{\n  run(input) {{\n    let total = input;\n{}    return total;\n  }}\n}}\n",
            (1..=44)
                .map(|index| format!("    total += input + {index};\n"))
                .collect::<String>()
        ),
    )
    .expect("target");
    let config =
        Config::discover(root.path(), Some(root.path().join("index.sqlite"))).expect("config");
    let services = Services::open(config).expect("services");
    services
        .refresh(leantoken::IndexingMode::Reconcile)
        .await
        .expect("index");

    let exact = services
        .context_evaluation(ContextRequest {
            task: "Fix OwnerAlpha".into(),
            token_budget: 400,
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
        .expect("exact evaluation");
    assert!(
        exact
            .generated_candidates
            .iter()
            .all(|candidate| candidate.representation != "import_symbol")
    );

    let multi = services
        .context_evaluation(ContextRequest {
            task: "Fix OwnerAlpha and OtherSignal".into(),
            token_budget: 400,
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
        .expect("multi-concept evaluation");
    assert!(
        multi.generated_candidates.iter().any(|candidate| {
            candidate.path == "src/target.js" && candidate.representation == "import_symbol"
        }),
        "candidates: {:?}",
        multi.generated_candidates
    );
    let import_symbol = multi
        .generated_candidates
        .iter()
        .find(|candidate| {
            candidate.path == "src/target.js" && candidate.representation == "import_symbol"
        })
        .expect("import symbol candidate");
    assert_eq!(import_symbol.end_line, 50);
    assert!(
        import_symbol.token_count > 256,
        "import symbol fixture must cover the old cap: {import_symbol:?}"
    );
    assert!(
        multi
            .generated_candidates
            .iter()
            .all(|candidate| candidate.representation != "import_neighbor")
    );
}
#[tokio::test]
async fn context_signal_evaluation_keeps_graph_arms_additive_and_isolated() {
    let root = tempfile::tempdir().expect("root");
    std::fs::create_dir(root.path().join("src")).expect("src");
    std::fs::write(
        root.path().join("src/seed.js"),
        "import { OwnerAlpha } from './target.js';\nexport function useOwner() { return new OwnerAlpha(); }\n",
    )
    .expect("seed");
    std::fs::write(
        root.path().join("src/target.js"),
        "export class OwnerAlpha { run(input) { return input + OtherSignal; } }\n",
    )
    .expect("target");
    let config =
        Config::discover(root.path(), Some(root.path().join("index.sqlite"))).expect("config");
    let services = Services::open(config).expect("services");
    services
        .refresh(leantoken::IndexingMode::Reconcile)
        .await
        .expect("index");
    let request = ContextRequest {
        task: "Fix OwnerAlpha and OtherSignal".into(),
        token_budget: 400,
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
    };

    let baseline = services
        .context_signal_evaluation(request.clone(), ContextSignalPolicy::LexicalSyntax)
        .await
        .expect("baseline");
    let imports = services
        .context_signal_evaluation(request.clone(), ContextSignalPolicy::ImportNeighbor)
        .await
        .expect("imports");
    let reverse = services
        .context_signal_evaluation(request.clone(), ContextSignalPolicy::ReverseDependency)
        .await
        .expect("reverse dependency");
    let callers = services
        .context_signal_evaluation(request, ContextSignalPolicy::HighConfidenceCaller)
        .await
        .expect("callers");

    let candidate_keys = |evaluation: &leantoken::ContextEvaluation| {
        evaluation
            .generated_candidates
            .iter()
            .map(|candidate| {
                (
                    candidate.path.clone(),
                    candidate.start_line,
                    candidate.end_line,
                    candidate.representation.clone(),
                )
            })
            .collect::<std::collections::BTreeSet<_>>()
    };
    let baseline_keys = candidate_keys(&baseline);
    for evaluation in [&imports, &reverse, &callers] {
        assert!(baseline_keys.is_subset(&candidate_keys(evaluation)));
    }
    assert!(baseline.generated_candidates.iter().all(|candidate| {
        candidate.representation != "import_symbol"
            && !candidate.match_kinds.iter().any(|kind| kind == "reference")
            && !candidate
                .match_kinds
                .iter()
                .any(|kind| kind == "reverse-import")
    }));
    assert!(imports.generated_candidates.iter().any(|candidate| {
        candidate.representation == "import_symbol"
            && candidate.match_kinds.iter().any(|kind| kind == "import")
    }));
    assert!(
        callers
            .generated_candidates
            .iter()
            .any(|candidate| candidate.match_kinds.iter().any(|kind| kind == "reference"))
    );
    assert!(reverse.generated_candidates.iter().any(|candidate| {
        candidate.path == "src/seed.js"
            && candidate
                .match_kinds
                .iter()
                .any(|kind| kind == "reverse-import")
    }));
    assert!(
        imports
            .generated_candidates
            .iter()
            .all(|candidate| !candidate.match_kinds.iter().any(|kind| kind == "reference"))
    );
    assert!(callers.generated_candidates.iter().all(|candidate| {
        candidate.representation != "import_symbol"
            && !candidate
                .match_kinds
                .iter()
                .any(|kind| kind == "reverse-import")
    }));
}

#[tokio::test]
async fn working_tree_diff_boosts_changed_files() {
    require_git();

    let root = tempfile::tempdir().expect("root");
    std::fs::create_dir(root.path().join("src")).unwrap();
    std::fs::write(root.path().join("src/a.rs"), "fn shared() {}\n").unwrap();
    std::fs::write(root.path().join("src/b.rs"), "fn shared() {}\n").unwrap();
    init_git_repo(root.path());

    let config = Config::discover(root.path(), Some(root.path().join("index.sqlite"))).unwrap();
    let services = Services::open(config).unwrap();
    services
        .refresh(leantoken::IndexingMode::Reconcile)
        .await
        .unwrap();

    // Modify b.rs after refresh; do not refresh so the diff signal is tested.
    std::fs::write(root.path().join("src/b.rs"), "fn shared() { let x = 1; }\n").unwrap();

    let response = services
        .context(ContextRequest {
            task: "update shared implementation".into(),
            token_budget: 500,
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
        .unwrap();

    assert!(!response.fragments.is_empty());
    assert_eq!(response.fragments[0].path, "src/b.rs");
    assert!(
        response
            .fragments
            .iter()
            .any(|fragment| fragment.path == "src/b.rs" && fragment.reason.contains("changed"))
    );
}
