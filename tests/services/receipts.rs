use super::*;

#[tokio::test]
async fn retrieval_receipt_identifies_bound_repository_and_rejects_mismatch() {
    let (_root, services) = fixture().await;
    let expected_repository = services.repository_id();
    let response = services
        .files(FilesRequest {
            operation: FileOperation::Tree,
            path: None,
            query: None,
            pattern: None,
            max_results: Some(10),
            cursor: None,
            depth: Some(1),
        })
        .await
        .expect("files");

    assert_eq!(response.meta.repository_id, expected_repository);
    services
        .validate_repository_id(Some(&expected_repository))
        .expect("matching repository");
    assert!(matches!(
        services.validate_repository_id(Some("different-repository")),
        Err(Error::RepositoryIdentityMismatch { expected, actual })
            if expected == "different-repository" && actual == expected_repository
    ));
    assert!(matches!(
        services.validate_repository_id(Some(&"x".repeat(128))),
        Err(Error::RepositoryIdentityMismatch { expected, .. }) if expected.len() == 128
    ));
    assert!(matches!(
        services.validate_repository_id(Some(&"x".repeat(129))),
        Err(Error::InputTooLong {
            field: "expected_repository_id",
            max_bytes: 128
        })
    ));
    assert!(matches!(
        services.validate_repository_id(Some(&"é".repeat(64))),
        Err(Error::RepositoryIdentityMismatch { expected, .. })
            if expected.len() == 128
    ));
    assert!(matches!(
        services.validate_repository_id(Some(&"é".repeat(65))),
        Err(Error::InputTooLong {
            field: "expected_repository_id",
            max_bytes: 128
        })
    ));
}

#[tokio::test]
async fn server_managed_receipt_suppresses_repeated_search_and_context_evidence() {
    let (_root, services) = fixture().await;

    let first_search = services
        .search(search_limit_request(Some(100), Some(2_000), Some(1)))
        .await
        .expect("first search");
    assert!(!first_search.hits.is_empty());
    let search_receipt = first_search
        .meta
        .receipt_id
        .clone()
        .expect("search receipt");
    let mut repeated_search_request = search_limit_request(Some(100), Some(2_000), Some(1));
    repeated_search_request.receipt_id = Some(search_receipt.clone());
    let repeated_search = services
        .search(repeated_search_request)
        .await
        .expect("repeated search");

    assert_eq!(
        repeated_search.meta.receipt_id.as_deref(),
        Some(search_receipt.as_str())
    );
    assert!(repeated_search.hits.is_empty());
    assert!(
        repeated_search.meta.receipt_suppressed_exact
            + repeated_search.meta.receipt_suppressed_overlap
            > 0
    );
    assert_eq!(repeated_search.meta.source_tokens, 0);

    let first_context = services
        .context(context_limit_request(1_000))
        .await
        .expect("first context");
    assert!(!first_context.fragments.is_empty());
    let context_receipt = first_context
        .meta
        .receipt_id
        .clone()
        .expect("context receipt");
    let mut repeated_context_request = context_limit_request(1_000);
    repeated_context_request.receipt_id = Some(context_receipt.clone());
    let repeated_context = services
        .context(repeated_context_request)
        .await
        .expect("repeated context");

    assert_eq!(
        repeated_context.meta.receipt_id.as_deref(),
        Some(context_receipt.as_str())
    );
    assert!(repeated_context.fragments.is_empty());
    assert!(
        repeated_context.meta.receipt_suppressed_exact
            + repeated_context.meta.receipt_suppressed_overlap
            > 0
    );
    assert!(repeated_context.receipt.fragment_hashes.is_empty());
    assert_eq!(repeated_context.meta.source_tokens, 0);
}

#[tokio::test]
async fn server_managed_receipt_survives_service_restart() {
    let (_root, services) = fixture().await;
    let config = services.config().clone();
    let first = services
        .search(search_limit_request(Some(100), Some(2_000), Some(1)))
        .await
        .expect("first search");
    let receipt_id = first.meta.receipt_id.expect("receipt");
    drop(services);

    let reopened = Services::open(config).expect("reopen services");
    let mut request = search_limit_request(Some(100), Some(2_000), Some(1));
    request.receipt_id = Some(receipt_id.clone());
    let repeated = reopened.search(request).await.expect("reuse after restart");
    assert_eq!(
        repeated.meta.receipt_id.as_deref(),
        Some(receipt_id.as_str())
    );
    assert!(repeated.hits.is_empty());
    assert!(repeated.meta.receipt_suppressed_exact + repeated.meta.receipt_suppressed_overlap > 0);
}

#[tokio::test]
async fn context_handoff_preserves_coordinates_provenance_and_host_state_without_source() {
    let (_root, services) = fixture().await;
    let mut request = context_limit_request(1_000);
    request.focus_paths = vec!["src".into()];
    request.focus_symbols = vec!["greet".into()];
    request.known_hashes = vec!["held-fragment-hash".into()];
    let response = services
        .context_with_handoff(
            request,
            HandoffManifestRequest {
                summary: Some("Implement the greeting change".into()),
                validations: vec![HandoffValidation {
                    command: "cargo test --test integration services".into(),
                    status: HandoffValidationStatus::Passed,
                    summary: Some("service contract passed".into()),
                }],
                assumptions: vec!["greet remains the public entrypoint".into()],
                open_questions: vec!["should caller formatting change?".into()],
                negative_evidence: vec!["no alternate greeting implementation found".into()],
                avoid_rules: vec!["do not copy complete files into the handoff".into()],
            },
        )
        .await
        .expect("context handoff");

    assert!(!response.fragments.is_empty());
    let manifest = response
        .handoff_manifest
        .as_ref()
        .expect("handoff manifest");
    assert_eq!(manifest.schema_version, 1);
    assert_eq!(manifest.summary, "Implement the greeting change");
    assert_eq!(manifest.task_fingerprint, response.receipt.task_fingerprint);
    assert_eq!(manifest.repository_id, response.meta.repository_id);
    assert_eq!(
        manifest.repository_generation,
        response.meta.repository_generation
    );
    assert_eq!(manifest.freshness, response.meta.freshness);
    assert_eq!(
        manifest.receipt_id.as_deref(),
        response.meta.receipt_id.as_deref()
    );
    assert_eq!(manifest.held_fragment_hashes, vec!["held-fragment-hash"]);
    assert_eq!(manifest.focus_paths, vec!["src"]);
    assert_eq!(manifest.focus_symbols, vec!["greet"]);
    assert_eq!(manifest.validations.len(), 1);
    assert_eq!(
        manifest.assumptions,
        vec!["greet remains the public entrypoint"]
    );
    assert_eq!(
        manifest.working_tree_state,
        HandoffWorkingTreeState::Unknown
    );
    assert!(manifest.commit_revision.is_none());
    assert!(
        manifest
            .gaps
            .iter()
            .any(|gap| gap.contains("commit identity"))
    );

    let mut expected = response
        .fragments
        .iter()
        .map(|fragment| {
            (
                fragment.path.clone(),
                fragment.start_line,
                fragment.end_line,
                fragment.content_hash.clone(),
            )
        })
        .collect::<Vec<_>>();
    expected.sort();
    let actual = manifest
        .evidence
        .iter()
        .map(|evidence| {
            (
                evidence.path.clone(),
                evidence.start_line,
                evidence.end_line,
                evidence.content_hash.clone(),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(actual, expected);
    let manifest_json = serde_json::to_string(manifest).expect("serialize manifest");
    assert!(!manifest_json.contains("pub fn greet"));
    assert!(!manifest_json.contains("\"content\""));
    assert_response_token_accounting!(response, Tokenizer::default());
}

#[tokio::test]
async fn context_handoff_retains_selected_coordinates_after_receipt_suppression() {
    let (_root, services) = fixture().await;
    let first = services
        .context_with_handoff(
            context_limit_request(1_000),
            HandoffManifestRequest::default(),
        )
        .await
        .expect("first handoff");
    let receipt_id = first.meta.receipt_id.clone().expect("receipt");
    let first_evidence = first
        .handoff_manifest
        .as_ref()
        .expect("first manifest")
        .evidence
        .clone();
    assert!(!first_evidence.is_empty());

    let mut repeated_request = context_limit_request(1_000);
    repeated_request.receipt_id = Some(receipt_id.clone());
    let repeated = services
        .context_with_handoff(repeated_request, HandoffManifestRequest::default())
        .await
        .expect("repeated handoff");

    assert!(repeated.fragments.is_empty());
    assert_eq!(repeated.meta.source_tokens, 0);
    assert_eq!(
        repeated.meta.receipt_id.as_deref(),
        Some(receipt_id.as_str())
    );
    assert_eq!(
        repeated
            .handoff_manifest
            .as_ref()
            .expect("repeated manifest")
            .evidence,
        first_evidence
    );
    assert_response_token_accounting!(repeated, Tokenizer::default());
}

#[tokio::test]
async fn context_handoff_rejects_plan_previews_and_unbounded_host_state() {
    let (_root, services) = fixture().await;
    let generation = services
        .status()
        .await
        .expect("status before invalid handoff")
        .repository_generation;
    let mut plan_request = context_limit_request(1_000);
    plan_request.plan_only = true;
    plan_request.minimum_fragments_per_focus_path = Some(2);
    plan_request.receipt_id = Some("existing".into());
    let error = services
        .context_with_handoff_workflow_consistency_cancellable(
            plan_request,
            HandoffManifestRequest::default(),
            ContextWorkflow::Auto,
            IndexConsistency::ReconcileWorkingTree,
            CancellationToken::new(),
        )
        .await
        .expect_err("plan handoff must fail");
    let Error::InvalidInputConstraints(violations) = error else {
        panic!("expected aggregated context constraints, got {error:?}");
    };
    assert_eq!(
        violations.as_slice(),
        &[
            leantoken::InputViolation {
                field: "focus paths",
                reason: "must not be empty when focus path constraints are enabled",
            },
            leantoken::InputViolation {
                field: "receipt_id",
                reason: "must be omitted when plan_only is true",
            },
            leantoken::InputViolation {
                field: "plan_only",
                reason: "cannot be combined with a handoff manifest",
            },
        ]
    );
    assert_eq!(
        services
            .status()
            .await
            .expect("status after invalid handoff")
            .repository_generation,
        generation,
        "static handoff errors must not reconcile the index"
    );

    let mut plan_request = context_limit_request(1_000);
    plan_request.plan_only = true;
    let error = services
        .context_with_handoff(plan_request, HandoffManifestRequest::default())
        .await
        .expect_err("one plan handoff conflict must preserve the single-field error");
    assert!(matches!(
        error,
        Error::InvalidInput {
            field: "plan_only",
            reason: "cannot be combined with a handoff manifest"
        }
    ));

    let error = services
        .context_with_handoff(
            context_limit_request(1_000),
            HandoffManifestRequest {
                summary: Some("x".repeat(513)),
                ..HandoffManifestRequest::default()
            },
        )
        .await
        .expect_err("oversized summary must fail");
    assert!(matches!(
        error,
        Error::InputTooLong {
            field: "handoff.summary",
            max_bytes: 512
        }
    ));
}

#[tokio::test]
async fn context_handoff_reports_clean_git_head_identity() {
    require_git();
    let (root, services) = fixture().await;
    std::fs::write(root.path().join(".gitignore"), "index.sqlite*\n").expect("write ignore");
    init_git_repo(root.path());
    let expected_head = String::from_utf8(
        std::process::Command::new("git")
            .args(["rev-parse", "--short=12", "HEAD"])
            .current_dir(root.path())
            .output()
            .expect("git head")
            .stdout,
    )
    .expect("utf-8 head")
    .trim()
    .to_owned();

    let response = services
        .context_with_handoff(
            context_limit_request(1_000),
            HandoffManifestRequest::default(),
        )
        .await
        .expect("Git context handoff");
    let manifest = response.handoff_manifest.expect("manifest");
    assert_eq!(
        manifest.commit_revision.as_deref(),
        Some(expected_head.as_str())
    );
    assert_eq!(manifest.working_tree_state, HandoffWorkingTreeState::Clean);
    assert!(
        !manifest
            .gaps
            .iter()
            .any(|gap| gap.contains("Git commit") || gap.contains("working-tree"))
    );
}

#[tokio::test]
async fn server_managed_receipt_suppresses_overlapping_evidence_across_tools() {
    let (_root, services) = fixture().await;
    let mut read_request = read_limit_request(Some(1_000));
    read_request.end_line = Some(3);
    let read = services.read(read_request).await.expect("read");
    let receipt_id = read.meta.receipt_id.clone().expect("read receipt");

    let mut outline_request = outline_limit_request(Some(100), Some(2_000));
    outline_request.receipt_id = Some(receipt_id.clone());
    let outline = services.outline(outline_request).await.expect("outline");

    assert_eq!(
        outline.meta.receipt_id.as_deref(),
        Some(receipt_id.as_str())
    );
    assert!(outline.meta.receipt_suppressed_overlap > 0);
    assert!(
        outline
            .files
            .iter()
            .flat_map(|file| &file.symbols)
            .all(|symbol| symbol.name != "greet")
    );
}

#[tokio::test]
async fn server_managed_receipt_rejects_unknown_and_stale_generations() {
    let (root, services) = fixture().await;
    let mut unknown_request = read_limit_request(Some(1_000));
    unknown_request.receipt_id = Some("missing-receipt".into());
    assert!(matches!(
        services.read(unknown_request).await,
        Err(Error::UnknownReceipt(id)) if id == "missing-receipt"
    ));

    let first = services
        .read(read_limit_request(Some(1_000)))
        .await
        .expect("first read");
    let receipt_id = first.meta.receipt_id.expect("read receipt");
    let receipt_generation = first.meta.repository_generation;
    std::fs::write(
        root.path().join("src/lib.rs"),
        "pub fn greet(name: &str) -> String {\n    format!(\"hi {name}\")\n}\n",
    )
    .expect("update fixture");
    let indexed = services
        .index(leantoken::IndexingMode::Reconcile)
        .await
        .expect("reindex");
    assert!(indexed.repository_generation > receipt_generation);

    let mut stale_request = read_limit_request(Some(1_000));
    stale_request.receipt_id = Some(receipt_id);
    assert!(matches!(
        services.read(stale_request).await,
        Err(Error::StaleReceipt {
            receipt_generation: actual_receipt,
            repository_generation
        }) if actual_receipt == receipt_generation
            && repository_generation == indexed.repository_generation
    ));
}

#[tokio::test]
async fn exact_receipt_rebase_classifies_controlled_edits_without_false_suppression() {
    let root = tempfile::tempdir().expect("temporary repository");
    let initial = [
        ("unchanged.rs", "fn unchanged() {}\n"),
        ("outside.rs", "fn outside() {}\n// old tail\n"),
        ("above.rs", "fn above() {}\n"),
        ("body.rs", "fn body() -> u8 { 1 }\n"),
        ("moved.rs", "fn moved() {}\n"),
        ("duplicate.rs", "fn duplicate() {}\n"),
        ("deleted.rs", "fn deleted() {}\n"),
        ("renamed.rs", "fn renamed() {}\n"),
        ("unmapped.rs", "fn unmapped() {}\n"),
    ];
    for (path, content) in initial {
        std::fs::write(root.path().join(path), content).expect("write initial source");
    }
    let database = root.path().join("index.sqlite");
    let config = Config::discover(root.path(), Some(database.clone())).expect("config");
    let services = Services::open(config).expect("services");
    services
        .index(leantoken::IndexingMode::Reconcile)
        .await
        .expect("initial index");

    let mut source_receipt = None;
    for path in [
        "unchanged.rs",
        "outside.rs",
        "above.rs",
        "body.rs",
        "moved.rs",
        "duplicate.rs",
        "deleted.rs",
        "renamed.rs",
        "unmapped.rs",
    ] {
        source_receipt = Some(append_line_receipt(&services, path, source_receipt).await);
    }
    let source_receipt = source_receipt.expect("source receipt");
    let source_generation = services
        .status()
        .await
        .expect("source status")
        .repository_generation;

    std::fs::write(
        root.path().join("outside.rs"),
        "fn outside() {}\n// changed tail\n",
    )
    .expect("edit outside range");
    std::fs::write(
        root.path().join("above.rs"),
        "// inserted above\nfn above() {}\n",
    )
    .expect("insert above");
    std::fs::write(root.path().join("body.rs"), "fn body() -> u8 { 2 }\n").expect("edit body");
    std::fs::write(
        root.path().join("moved.rs"),
        "// old coordinate changed\n\nfn moved() {}\n",
    )
    .expect("move function");
    std::fs::write(
        root.path().join("duplicate.rs"),
        "// old coordinate changed\n\nfn duplicate() {}\nfn duplicate() {}\n",
    )
    .expect("duplicate away from old coordinate");
    std::fs::remove_file(root.path().join("deleted.rs")).expect("delete evidence");
    std::fs::rename(
        root.path().join("renamed.rs"),
        root.path().join("renamed-new.rs"),
    )
    .expect("rename evidence");
    std::fs::write(root.path().join("unrelated.rs"), "fn unrelated() {}\n")
        .expect("unrelated edit");
    services
        .index(leantoken::IndexingMode::Reconcile)
        .await
        .expect("publish edits");
    let current_generation = services
        .status()
        .await
        .expect("current status")
        .repository_generation;
    assert!(current_generation > source_generation);
    std::fs::write(
        root.path().join("unmapped.rs"),
        "fn unmapped() {}\n// dirty after publication\n",
    )
    .expect("make live file differ from pinned generation");

    let before = receipt_header_count(&database);
    let cancellation = CancellationToken::new();
    cancellation.cancel();
    let cancelled = services
        .rebase_receipt_with_options_consistency_cancellable(
            ReceiptRebaseRequest {
                receipt_id: source_receipt.clone(),
                max_samples_per_outcome: Some(4),
            },
            IndexConsistency::IndexedGeneration,
            ServiceCallOptions::new(),
            cancellation,
        )
        .await
        .expect_err("cancelled rebase");
    assert!(matches!(cancelled, Error::Cancelled));
    assert_eq!(receipt_header_count(&database), before);

    let budget_error = services
        .rebase_receipt_with_options(
            ReceiptRebaseRequest {
                receipt_id: source_receipt.clone(),
                max_samples_per_outcome: Some(0),
            },
            ServiceCallOptions::new()
                .with_max_response_tokens(1)
                .with_receipt_resource_reserve(),
        )
        .await
        .expect_err("one token cannot fit");
    let minimum_response_tokens = match budget_error {
        Error::ResponseBudgetExceeded {
            minimum_required_response_tokens,
            ..
        } => minimum_required_response_tokens,
        other => panic!("expected response budget error, got {other}"),
    };
    assert_eq!(receipt_header_count(&database), before);

    let below_boundary = services
        .rebase_receipt_with_options(
            ReceiptRebaseRequest {
                receipt_id: source_receipt.clone(),
                max_samples_per_outcome: Some(0),
            },
            ServiceCallOptions::new()
                .with_max_response_tokens(minimum_response_tokens - 1)
                .with_receipt_resource_reserve(),
        )
        .await
        .expect_err("receipt decoration must fit before persistence");
    assert!(matches!(
        below_boundary,
        Error::ResponseBudgetExceeded {
            minimum_required_response_tokens: minimum,
            ..
        } if minimum == minimum_response_tokens
    ));
    assert_eq!(receipt_header_count(&database), before);

    let boundary_response = services
        .rebase_receipt_with_options(
            ReceiptRebaseRequest {
                receipt_id: source_receipt.clone(),
                max_samples_per_outcome: Some(0),
            },
            ServiceCallOptions::new()
                .with_max_response_tokens(minimum_response_tokens)
                .with_receipt_resource_reserve(),
        )
        .await
        .expect("exact receipt-decoration boundary");
    assert!(boundary_response.meta.receipt_id.is_some());
    assert_eq!(receipt_header_count(&database), before + 1);

    let response = services
        .rebase_receipt(ReceiptRebaseRequest {
            receipt_id: source_receipt.clone(),
            max_samples_per_outcome: Some(4),
        })
        .await
        .expect("exact rebase");
    assert_eq!(response.source_receipt_id, source_receipt);
    assert_eq!(response.source_repository_generation, source_generation);
    assert_eq!(response.meta.repository_generation, current_generation);
    assert_eq!(response.counts.carried, 2);
    assert_eq!(response.counts.changed, 4);
    assert_eq!(response.counts.missing, 2);
    assert_eq!(response.counts.unmapped, 1);
    assert_eq!(response.counts.total(), 9);
    assert!(response.samples_complete);
    assert_eq!(response.outcomes_blake3.len(), 64);
    assert_response_token_accounting!(response, services.config().tokenizer);
    assert_eq!(receipt_header_count(&database), before + 2);
    let rebased_receipt = response.meta.receipt_id.clone().expect("rebased receipt");

    let unchanged = services
        .read(line_read_request(
            "unchanged.rs",
            Some(rebased_receipt.clone()),
        ))
        .await
        .expect("reuse unchanged");
    assert_eq!(unchanged.status, ReadStatus::ReceiptSuppressed);
    assert_eq!(unchanged.meta.receipt_suppressed_exact, 1);
    let changed = services
        .read(line_read_request("body.rs", Some(rebased_receipt)))
        .await
        .expect("changed evidence is returned");
    assert_eq!(changed.status, ReadStatus::Content);
    assert_eq!(changed.meta.receipt_suppressed_exact, 0);
    assert_eq!(changed.content.as_deref(), Some("fn body() -> u8 { 2 }\n"));

    let repeated = services
        .rebase_receipt(ReceiptRebaseRequest {
            receipt_id: source_receipt,
            max_samples_per_outcome: Some(0),
        })
        .await
        .expect("deterministic repeated classification");
    assert_eq!(repeated.counts, response.counts);
    assert_eq!(repeated.outcomes_blake3, response.outcomes_blake3);
    assert!(!repeated.samples_complete);
    assert!(repeated.samples.carried.is_empty());
    assert!(repeated.samples.changed.is_empty());
    assert!(repeated.samples.missing.is_empty());
    assert!(repeated.samples.unmapped.is_empty());
}

#[tokio::test]
async fn exact_receipt_rebase_survives_restart_and_branch_switches_fail_closed() {
    require_git();
    let root = tempfile::tempdir().expect("temporary repository");
    std::fs::write(
        root.path().join("branch.rs"),
        "fn branch_value() -> u8 { 1 }\n",
    )
    .expect("write branch source");
    init_git_repo(root.path());
    let original_branch = std::process::Command::new("git")
        .args(["branch", "--show-current"])
        .current_dir(root.path())
        .output()
        .expect("current branch");
    let original_branch = String::from_utf8(original_branch.stdout)
        .expect("UTF-8 branch")
        .trim()
        .to_owned();
    let database = root.path().join("index.sqlite");
    let config = Config::discover(root.path(), Some(database.clone())).expect("config");
    let services = Services::open(config).expect("services");
    services
        .index(leantoken::IndexingMode::Reconcile)
        .await
        .expect("initial index");
    let source_receipt = append_line_receipt(&services, "branch.rs", None).await;
    drop(services);

    let switched = std::process::Command::new("git")
        .args(["switch", "-c", "receipt-rebase-alternate"])
        .current_dir(root.path())
        .status()
        .expect("switch branch");
    assert!(switched.success());
    std::fs::write(
        root.path().join("branch.rs"),
        "fn branch_value() -> u8 { 2 }\n",
    )
    .expect("write alternate branch");
    let committed = std::process::Command::new("git")
        .args(["add", "branch.rs"])
        .current_dir(root.path())
        .status()
        .expect("stage alternate branch");
    assert!(committed.success());
    let committed = std::process::Command::new("git")
        .args(["commit", "-m", "alternate"])
        .current_dir(root.path())
        .status()
        .expect("commit alternate branch");
    assert!(committed.success());
    let config = Config::discover(root.path(), Some(database.clone())).expect("reopen config");
    let services = Services::open(config).expect("reopened services");
    services
        .index(leantoken::IndexingMode::Reconcile)
        .await
        .expect("index branch switch");
    let response = services
        .rebase_receipt(ReceiptRebaseRequest {
            receipt_id: source_receipt,
            max_samples_per_outcome: None,
        })
        .await
        .expect("rebase after branch switch");
    assert_eq!(response.counts.carried, 0);
    assert_eq!(response.counts.changed, 1);
    let rebased = response.meta.receipt_id.expect("new receipt");
    drop(services);

    let returned = std::process::Command::new("git")
        .args(["switch", &original_branch])
        .current_dir(root.path())
        .status()
        .expect("restore branch");
    assert!(returned.success());
    let config = Config::discover(root.path(), Some(database)).expect("third config");
    let services = Services::open(config).expect("third services");
    services
        .index(leantoken::IndexingMode::Reconcile)
        .await
        .expect("index restored branch");
    let error = services
        .read(line_read_request("branch.rs", Some(rebased)))
        .await
        .expect_err("rebased receipt is generation bound after restart");
    assert!(matches!(error, Error::StaleReceipt { .. }));
}

#[tokio::test]
async fn exact_receipt_rebase_validates_outline_signature_evidence() {
    let root = tempfile::tempdir().expect("temporary repository");
    std::fs::write(
        root.path().join("outline.rs"),
        "pub fn stable(value: u8) -> u8 {\n    value\n}\n",
    )
    .expect("write outline source");
    let config =
        Config::discover(root.path(), Some(root.path().join("index.sqlite"))).expect("config");
    let services = Services::open(config).expect("services");
    services
        .index(leantoken::IndexingMode::Reconcile)
        .await
        .expect("initial index");
    let request = OutlineRequest {
        paths: vec!["outline.rs".into()],
        symbol_name: Some("stable".into()),
        symbol_kind: None,
        max_results: Some(10),
        max_tokens: Some(1_000),
        receipt_id: None,
        cursor: None,
    };
    let first = services
        .outline(request.clone())
        .await
        .expect("initial outline");
    assert_eq!(first.files[0].symbols.len(), 1);
    let source_receipt = first.meta.receipt_id.expect("source receipt");
    std::fs::write(
        root.path().join("outline.rs"),
        "pub fn stable(value: u8) -> u8 {\n    value.saturating_add(1)\n}\n",
    )
    .expect("change body without changing the outline signature");
    services
        .index(leantoken::IndexingMode::Reconcile)
        .await
        .expect("publish body edit");

    let rebased = services
        .rebase_receipt(ReceiptRebaseRequest {
            receipt_id: source_receipt,
            max_samples_per_outcome: None,
        })
        .await
        .expect("rebase outline receipt");
    assert_eq!(rebased.counts.carried, 1);
    assert_eq!(rebased.counts.changed, 0);
    let rebased_receipt = rebased.meta.receipt_id.expect("rebased receipt");
    let mut repeated_request = request;
    repeated_request.receipt_id = Some(rebased_receipt.clone());
    let repeated = services
        .outline(repeated_request)
        .await
        .expect("reuse rebased outline receipt");
    assert!(repeated.files[0].symbols.is_empty());
    assert_eq!(repeated.meta.receipt_suppressed_exact, 1);

    let mut search_request = search_limit_request(Some(10), Some(1_000), Some(2));
    search_request.query = "stable".into();
    search_request.mode = SearchMode::Symbol;
    search_request.receipt_id = Some(rebased_receipt);
    let search = services
        .search(search_request)
        .await
        .expect("search changed function body with rebased receipt");
    assert!(
        search
            .hits
            .iter()
            .any(|hit| hit.excerpt.contains("saturating_add"))
    );
    assert_eq!(search.meta.receipt_suppressed_overlap, 0);
}

fn line_read_request(path: &str, receipt_id: Option<String>) -> ReadRequest {
    ReadRequest {
        path: path.into(),
        start_line: Some(1),
        end_line: Some(1),
        symbol: None,
        heading: None,
        heading_occurrence: None,
        continuation_cursor: None,
        max_tokens: Some(100),
        expected_hash: None,
        delta: false,
        policy: leantoken::ReadPolicy::default(),
        receipt_id,
    }
}

async fn append_line_receipt(
    services: &Services,
    path: &str,
    receipt_id: Option<String>,
) -> String {
    services
        .read(line_read_request(path, receipt_id))
        .await
        .expect("append line evidence")
        .meta
        .receipt_id
        .expect("receipt id")
}

fn receipt_header_count(database: &std::path::Path) -> usize {
    let connection = rusqlite::Connection::open(database).expect("inspect receipt headers");
    let count: i64 = connection
        .query_row("SELECT COUNT(*) FROM retrieval_receipts", [], |row| {
            row.get(0)
        })
        .expect("receipt header count");
    usize::try_from(count).expect("non-negative receipt count")
}
