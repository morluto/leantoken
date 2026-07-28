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
    let manifest = response.handoff_manifest.as_ref().expect("handoff manifest");
    assert_eq!(manifest.schema_version, 1);
    assert_eq!(manifest.summary, "Implement the greeting change");
    assert_eq!(
        manifest.task_fingerprint,
        response.receipt.task_fingerprint
    );
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
    assert_eq!(
        manifest.held_fragment_hashes,
        vec!["held-fragment-hash"]
    );
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
    assert_eq!(repeated.meta.receipt_id.as_deref(), Some(receipt_id.as_str()));
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
    if !git_available() {
        return;
    }
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
    let indexed = services.index(false).await.expect("reindex");
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

