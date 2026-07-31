use super::*;

#[tokio::test]
async fn diff_scoped_context_with_explicit_changed_paths_reports_receipt() {
    let (_root, services) = fixture().await;

    let response = services
        .context(ContextRequest {
            task: "change greet caller".into(),
            token_budget: 200,
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
            changed_paths: vec!["src/lib.rs".into()],
            strict_changed_paths: false,
            explain_diagnostics: false,
        })
        .await
        .expect("diff-scoped context");

    let scope = response
        .diff_scope
        .as_ref()
        .expect("diff scope receipt present");
    assert_eq!(scope.changed_paths, vec!["src/lib.rs".to_owned()]);
    assert!(scope.base_revision.is_none());
    assert!(scope.head_revision.is_none());
    assert_eq!(scope.indexed_changed_paths, 1);
    let evidence = scope.evidence.as_ref().expect("diff evidence");
    assert!(
        evidence
            .changed_symbols
            .iter()
            .any(|symbol| symbol.name == "greet")
    );
    assert!(
        evidence
            .gaps
            .contains(&"hunk_ranges_unavailable_for_explicit_paths".to_owned())
    );
}

#[tokio::test]
async fn strict_explicit_changed_paths_do_not_expand_to_working_tree_changes() {
    require_git();

    let root = tempfile::tempdir().expect("root");
    let database = tempfile::tempdir().expect("database");
    std::fs::create_dir_all(root.path().join("src")).expect("source directory");
    std::fs::write(
        root.path().join("src/selected.rs"),
        "pub fn strict_scope_marker_selected() -> bool { false }\n",
    )
    .expect("selected base");
    std::fs::write(
        root.path().join("src/unrelated.rs"),
        "pub fn strict_scope_marker_unrelated() -> bool { false }\n",
    )
    .expect("unrelated base");
    init_git_repo(root.path());
    std::fs::write(
        root.path().join("src/selected.rs"),
        "pub fn strict_scope_marker_selected() -> bool { true }\n",
    )
    .expect("selected change");
    std::fs::write(
        root.path().join("src/unrelated.rs"),
        "pub fn strict_scope_marker_unrelated() -> bool { true }\n",
    )
    .expect("unrelated change");
    std::fs::write(
        root.path().join("private-notes.md"),
        "# strict_scope_marker private workspace state\n",
    )
    .expect("private untracked path");

    let config = Config::discover(
        root.path(),
        Some(database.path().join("strict-scope.sqlite")),
    )
    .expect("config");
    let services = Services::open(config).expect("services");
    services.index(false).await.expect("index working tree");
    let mut request = context_limit_request(500);
    request.task = "review strict_scope_marker".into();
    request.base_revision = Some("HEAD".into());
    request.changed_paths = vec!["src/selected.rs".into()];
    request.strict_changed_paths = true;

    let response = services.context(request).await.expect("strict context");

    let scope = response.diff_scope.as_ref().expect("diff scope");
    assert_eq!(scope.changed_paths, ["src/selected.rs"]);
    assert!(
        !response.fragments.is_empty()
            && response
                .fragments
                .iter()
                .all(|fragment| fragment.path == "src/selected.rs")
    );
    assert_eq!(response.coverage.path_scope_satisfied, Some(true));
    let coverage = response
        .coverage
        .changed_path_coverage
        .as_ref()
        .expect("changed path coverage");
    assert_eq!(coverage.resolved_paths, 1);
    assert_eq!(coverage.indexed_paths, 1);
    assert!(coverage.selected_fragments > 0);
    let evidence = scope.evidence.as_ref().expect("diff evidence");
    assert!(
        evidence
            .changed_symbols
            .iter()
            .all(|symbol| symbol.path == "src/selected.rs")
    );
    let serialized = serde_json::to_string(&response).expect("serialize response");
    assert!(!serialized.contains("src/unrelated.rs"));
    assert!(!serialized.contains("private-notes.md"));

    for args in [
        &["add", "src/selected.rs", "src/unrelated.rs"][..],
        &["commit", "-m", "change both tracked files"][..],
    ] {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(root.path())
            .output()
            .expect("git command");
        assert!(output.status.success());
    }
    let mut range_request = context_limit_request(500);
    range_request.task = "review strict_scope_marker".into();
    range_request.base_revision = Some("HEAD~1..HEAD".into());
    range_request.changed_paths = vec!["src/selected.rs".into()];
    range_request.strict_changed_paths = true;

    let range_response = services
        .context(range_request)
        .await
        .expect("strict immutable context");
    let range_scope = range_response.diff_scope.as_ref().expect("range diff scope");
    assert_eq!(range_scope.changed_paths, ["src/selected.rs"]);
    assert!(
        !range_response.fragments.is_empty()
            && range_response
                .fragments
                .iter()
                .all(|fragment| fragment.path == "src/selected.rs")
    );
    let range_serialized =
        serde_json::to_string(&range_response).expect("serialize range response");
    assert!(!range_serialized.contains("src/unrelated.rs"));
    assert!(!range_serialized.contains("private-notes.md"));
}

#[tokio::test]
async fn diff_scoped_context_maps_base_hunks_cross_language_changes_and_untracked_owner_tests() {
    require_git();

    let root = tempfile::tempdir().expect("root");
    let database = tempfile::tempdir().expect("database");
    std::fs::create_dir_all(root.path().join("src")).expect("src");
    std::fs::create_dir_all(root.path().join("tests")).expect("tests");
    std::fs::write(
        root.path().join("src/service.py"),
        "def compute(value):\n    return value + 1\n",
    )
    .expect("python source");
    std::fs::write(
        root.path().join("src/lib.rs"),
        "pub fn rust_changed(value: i32) -> i32 {\n    value + 1\n}\n",
    )
    .expect("rust source");
    std::fs::write(
        root.path().join("src/obsolete.py"),
        "def obsolete():\n    return True\n",
    )
    .expect("deleted source");
    init_git_repo(root.path());
    let base_revision = String::from_utf8(
        std::process::Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(root.path())
            .output()
            .expect("git rev-parse")
            .stdout,
    )
    .expect("UTF-8 revision")
    .trim()
    .to_owned();

    std::fs::write(
        root.path().join("src/service.py"),
        "def compute(value):\n    return value + 2\n",
    )
    .expect("modify python source");
    std::fs::write(
        root.path().join("src/lib.rs"),
        "pub fn rust_changed(value: i32) -> i32 {\n    value + 2\n}\n",
    )
    .expect("modify rust source");
    std::fs::remove_file(root.path().join("src/obsolete.py")).expect("delete source");
    std::fs::write(
        root.path().join("tests/service_test.py"),
        "from src.service import compute\n\ndef test_compute():\n    assert compute(1) == 3\n",
    )
    .expect("untracked owner test");

    let config = Config::discover(
        root.path(),
        Some(database.path().join("index.sqlite")),
    )
    .expect("config");
    let services = Services::open(config).expect("services");
    services.index(false).await.expect("index working tree");
    let response = services
        .context_with_handoff_workflow_consistency_cancellable(
            ContextRequest {
                task: "review compute and rust_changed with owning tests".into(),
                token_budget: 1_500,
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
                base_revision: Some(base_revision),
                changed_paths: Vec::new(),
                strict_changed_paths: true,
                explain_diagnostics: false,
            },
            HandoffManifestRequest::default(),
            ContextWorkflow::Review,
            IndexConsistency::IndexedGeneration,
            CancellationToken::new(),
        )
        .await
        .expect("base-revision context");

    assert!(
        response
            .coverage
            .changed_path_coverage
            .as_ref()
            .is_some_and(|coverage| coverage.satisfied)
    );
    let fragment_paths = response
        .fragments
        .iter()
        .map(|fragment| fragment.path.clone())
        .collect::<Vec<_>>();
    let manifest = response
        .handoff_manifest
        .as_ref()
        .expect("diff handoff manifest");
    assert_eq!(manifest.working_tree_state, HandoffWorkingTreeState::Dirty);
    assert!(manifest.commit_revision.is_some());
    assert!(
        manifest
            .changed_paths
            .iter()
            .any(|path| path == "src/service.py")
    );
    assert!(
        manifest
            .related_paths
            .iter()
            .any(|path| path == "tests/service_test.py")
    );
    assert!(
        manifest
            .test_paths
            .iter()
            .any(|path| path == "tests/service_test.py")
    );
    let scope = response.diff_scope.expect("diff scope");
    assert!(
        fragment_paths
            .iter()
            .all(|path| scope.changed_paths.contains(path))
    );
    for path in [
        "src/lib.rs",
        "src/obsolete.py",
        "src/service.py",
        "tests/service_test.py",
    ] {
        assert!(
            scope.changed_paths.iter().any(|changed| changed == path),
            "missing changed path {path}: {:?}",
            scope.changed_paths
        );
    }
    let evidence = scope.evidence.expect("diff evidence");
    assert!(
        evidence
            .changed_hunks
            .iter()
            .any(|hunk| hunk.path == "src/service.py" && hunk.start_line <= 2)
    );
    for symbol in ["compute", "rust_changed"] {
        assert!(
            evidence
                .changed_symbols
                .iter()
                .any(|changed| changed.name == symbol),
            "missing changed symbol {symbol}: {:?}",
            evidence.changed_symbols
        );
    }
    assert!(evidence.gaps.contains(
        &"src/obsolete.py:not_indexed_or_deleted".to_owned()
    ));
    assert!(evidence.related_paths.iter().any(|relationship| {
        relationship.changed_path == "src/service.py"
            && relationship.related_path == "tests/service_test.py"
            && relationship.signal == "test_name_match"
    }));
    assert!(
        evidence
            .semantic_change
            .as_ref()
            .is_some_and(|semantic| semantic
                .gaps
                .contains(&"semantic_change_requires_immutable_range".to_owned()))
    );

    let mut working_tree_request = context_limit_request(1_000);
    working_tree_request.task = "review compute and rust_changed".into();
    working_tree_request.strict_changed_paths = true;
    let working_tree = services
        .context(working_tree_request)
        .await
        .expect("strict working-tree scope");
    let working_tree_scope = working_tree.diff_scope.expect("working-tree scope");
    assert!(
        working_tree
            .fragments
            .iter()
            .all(|fragment| working_tree_scope.changed_paths.contains(&fragment.path))
    );
}

#[tokio::test]
async fn review_context_classifies_semantic_changes_without_exposing_configuration_values() {
    require_git();

    let root = tempfile::tempdir().expect("root");
    let database = tempfile::tempdir().expect("database");
    std::fs::create_dir_all(root.path().join("src")).expect("src");
    std::fs::create_dir_all(root.path().join("tests")).expect("tests");
    std::fs::write(
        root.path().join("src/lib.rs"),
        "pub fn contract(value: i32) -> i32 {\n    value + 1\n}\n\nfn body_only(value: i32) -> i32 {\n    value + 1\n}\n\npub fn old_name(value: i32) -> i32 {\n    value * 2\n}\n\nfn removed() -> bool {\n    true\n}\n",
    )
    .expect("base source");
    std::fs::write(
        root.path().join("tests/lib_test.rs"),
        "#[test]\nfn contract_works() {\n    assert_eq!(crate::contract(1), 2);\n}\n",
    )
    .expect("owner test");
    std::fs::write(
        root.path().join("src/deleted.rs"),
        "pub fn deleted_file_symbol() -> bool {\n    true\n}\n",
    )
    .expect("deleted source");
    std::fs::write(
        root.path().join("package.json"),
        r#"{"secret":"base-secret-value","removed":true,"nested":{"stable":1}}"#,
    )
    .expect("base configuration");
    init_git_repo(root.path());
    let revision = |name: &str| {
        String::from_utf8(
            std::process::Command::new("git")
                .args(["rev-parse", name])
                .current_dir(root.path())
                .output()
                .expect("resolve revision")
                .stdout,
        )
        .expect("UTF-8 revision")
        .trim()
        .to_owned()
    };
    let base = revision("HEAD");

    std::fs::write(
        root.path().join("src/lib.rs"),
        "pub fn contract(value: i64) -> i64 {\n    value + 1\n}\n\nfn body_only(value: i32) -> i32 {\n    value + 2\n}\n\npub fn new_name(value: i32) -> i32 {\n    value * 2\n}\n\npub fn added() -> bool {\n    false\n}\n",
    )
    .expect("head source");
    std::fs::write(
        root.path().join("package.json"),
        r#"{"secret":"head-secret-value","added":false,"nested":{"stable":1}}"#,
    )
    .expect("head configuration");
    std::fs::remove_file(root.path().join("src/deleted.rs")).expect("remove deleted source");
    std::fs::write(
        root.path().join("src/created.rs"),
        "pub fn created_file_symbol() -> bool {\n    true\n}\n",
    )
    .expect("created source");
    let commit = std::process::Command::new("git")
        .args(["add", "."])
        .current_dir(root.path())
        .output()
        .expect("git add");
    assert!(commit.status.success());
    let commit = std::process::Command::new("git")
        .args(["commit", "-m", "semantic changes"])
        .current_dir(root.path())
        .output()
        .expect("git commit");
    assert!(commit.status.success());
    let head = revision("HEAD");

    let config = Config::discover(
        root.path(),
        Some(database.path().join("index.sqlite")),
    )
    .expect("config");
    let services = Services::open(config).expect("services");
    services.index(false).await.expect("index head");
    let mut request = context_limit_request(2_000);
    request.task = "review public contracts, configuration, and owner tests".into();
    request.base_revision = Some(format!("{base}..{head}"));
    request.strict_changed_paths = true;
    let implementation_request = request.clone();
    let response = services
        .context_with_workflow_consistency_cancellable(
            request,
            ContextWorkflow::Review,
            IndexConsistency::IndexedGeneration,
            CancellationToken::new(),
        )
        .await
        .expect("review context");
    assert_response_token_accounting!(response, Tokenizer::default());
    let implementation = services
        .context_with_workflow_consistency_cancellable(
            implementation_request,
            ContextWorkflow::Implementation,
            IndexConsistency::IndexedGeneration,
            CancellationToken::new(),
        )
        .await
        .expect("implementation context");
    assert!(
        implementation
            .diff_scope
            .as_ref()
            .and_then(|scope| scope.evidence.as_ref())
            .is_some_and(|evidence| evidence.semantic_change.is_none())
    );

    let evidence = response
        .diff_scope
        .as_ref()
        .and_then(|scope| scope.evidence.as_ref())
        .expect("diff evidence");
    let semantic = evidence.semantic_change.as_ref().expect("semantic change");
    let symbol_change = |name: &str| {
        semantic
            .symbol_changes
            .iter()
            .find(|change| {
                change
                    .after
                    .as_ref()
                    .or(change.before.as_ref())
                    .is_some_and(|symbol| symbol.name == name)
            })
            .unwrap_or_else(|| panic!("missing symbol change {name}: {semantic:?}"))
    };
    let contract = symbol_change("contract");
    assert_eq!(contract.kind, DiffSymbolChangeKind::Modified);
    assert_eq!(
        contract.modification,
        Some(DiffSymbolModification::SignatureChanged)
    );
    assert!(contract.public_contract_changed);
    let body_only = symbol_change("body_only");
    assert_eq!(body_only.kind, DiffSymbolChangeKind::Modified);
    assert_eq!(
        body_only.modification,
        Some(DiffSymbolModification::BodyOnly)
    );
    assert!(!body_only.public_contract_changed);
    let renamed = symbol_change("new_name");
    assert_eq!(renamed.kind, DiffSymbolChangeKind::Renamed);
    assert_eq!(
        renamed.before.as_ref().map(|symbol| symbol.name.as_str()),
        Some("old_name")
    );
    assert!(renamed.public_contract_changed);
    assert_eq!(
        symbol_change("removed").kind,
        DiffSymbolChangeKind::Removed
    );
    assert_eq!(symbol_change("added").kind, DiffSymbolChangeKind::Added);
    assert_eq!(
        symbol_change("deleted_file_symbol").kind,
        DiffSymbolChangeKind::Removed
    );
    assert_eq!(
        symbol_change("created_file_symbol").kind,
        DiffSymbolChangeKind::Added
    );

    for (key_path, kind) in [
        ("/added", DiffConfigurationChangeKind::Added),
        ("/removed", DiffConfigurationChangeKind::Removed),
        ("/secret", DiffConfigurationChangeKind::Modified),
    ] {
        assert!(
            semantic
                .configuration_changes
                .iter()
                .any(|change| change.key_path == key_path && change.kind == kind),
            "missing configuration change {key_path}: {semantic:?}"
        );
    }
    let serialized = serde_json::to_string(semantic).expect("serialize semantic receipt");
    assert!(!serialized.contains("base-secret-value"));
    assert!(!serialized.contains("head-secret-value"));
    assert!(!serialized.contains("risk"));

    let source_tests = semantic
        .owner_tests
        .iter()
        .find(|coverage| coverage.changed_path == "src/lib.rs")
        .expect("source owner-test coverage");
    assert_eq!(source_tests.status, DiffOwnerTestStatus::Found);
    assert_eq!(source_tests.paths, ["tests/lib_test.rs"]);
    let config_tests = semantic
        .owner_tests
        .iter()
        .find(|coverage| coverage.changed_path == "package.json")
        .expect("configuration owner-test coverage");
    assert_eq!(config_tests.status, DiffOwnerTestStatus::Missing);

    let history = services
        .history(HistoryRequest {
            operation: HistoryOperation::DiffSymbol {
                path: "src/lib.rs".into(),
                symbol: "contract".into(),
                base_revision: base,
                head_revision: head,
            },
            max_results: None,
            max_tokens: Some(200),
        })
        .await
        .expect("historical semantic change");
    assert_response_token_accounting!(history, Tokenizer::default());
    let history_change = history.semantic_change.expect("history semantic change");
    assert_eq!(history_change.kind, DiffSymbolChangeKind::Modified);
    assert_eq!(
        history_change.modification,
        Some(DiffSymbolModification::SignatureChanged)
    );
    assert!(history_change.public_contract_changed);
}

#[tokio::test]
async fn diff_scoped_context_preserves_task_only_behavior_without_scope() {
    let (_root, services) = fixture().await;

    let response = services
        .context(ContextRequest {
            task: "change greet caller".into(),
            token_budget: 200,
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
        .expect("task-only context");

    assert!(
        response.diff_scope.is_none(),
        "no diff scope must not produce a receipt"
    );
    assert!(!response.fragments.is_empty());
}

#[tokio::test]
async fn diff_scoped_context_rejects_path_outside_repository() {
    let (_root, services) = fixture().await;

    let error = services
        .context(ContextRequest {
            task: "change greet caller".into(),
            token_budget: 200,
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
            changed_paths: vec!["../escape.rs".into()],
            strict_changed_paths: false,
            explain_diagnostics: false,
        })
        .await
        .expect_err("path traversal rejected");

    assert!(
        matches!(error, Error::PathOutsideRoot { .. }),
        "got {error:?}"
    );
}

#[tokio::test]
async fn diff_scoped_context_rejects_excessive_changed_path_count() {
    let (_root, services) = fixture().await;

    let too_many = (0..600).map(|i| format!("src/file{i}.rs")).collect::<Vec<_>>();
    let error = services
        .context(ContextRequest {
            task: "change greet caller".into(),
            token_budget: 200,
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
            changed_paths: too_many,
            strict_changed_paths: false,
            explain_diagnostics: false,
        })
        .await
        .expect_err("too many changed paths rejected");

    assert!(matches!(error, Error::LimitExceeded), "got {error:?}");
}

#[tokio::test]
async fn diff_scoped_context_counts_zero_for_nonexistent_changed_path() {
    let (_root, services) = fixture().await;

    let response = services
        .context(ContextRequest {
            task: "change greet caller".into(),
            token_budget: 200,
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
            changed_paths: vec!["src/nonexistent.rs".into()],
            strict_changed_paths: false,
            explain_diagnostics: false,
        })
        .await
        .expect("context with unindexed changed path");

    let scope = response
        .diff_scope
        .as_ref()
        .expect("diff scope receipt present");
    assert_eq!(scope.indexed_changed_paths, 0);
}
