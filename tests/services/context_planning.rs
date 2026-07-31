use super::*;


#[tokio::test]
async fn typed_workflow_evidence_is_bounded_and_reaches_candidate_provenance() {
    let (_root, services) = fixture().await;
    let evidence = WorkflowEvidence::new()
        .with_failure_traces(["error: greet failed".into()])
        .with_symbols(["greet".into()])
        .with_paths(["src/lib.rs".into()])
        .with_test_intents(["greet regression".into()]);
    let mut request = context_limit_request(300);
    request.task = "investigate the observed failure".into();

    let evaluation = services
        .context_evaluation_with_workflow_evidence(request.clone(), evidence)
        .await
        .expect("typed workflow evidence");

    assert!(evaluation.generated_candidate_paths.iter().any(|path| path == "src/lib.rs"));
    assert!(evaluation.generated_candidates.iter().any(|candidate| {
        candidate
            .match_kinds
            .iter()
            .any(|kind| kind.starts_with("facet:symbol:"))
    }));

    let error = services
        .context_with_workflow_evidence(
            request,
            WorkflowEvidence::new().with_symbols((0..9).map(|index| format!("symbol_{index}"))),
        )
        .await
        .expect_err("per-class bound");
    assert!(matches!(
        error,
        Error::RequestLimitExceeded {
            field: "workflow_evidence items per class",
            requested: 9,
            limit: 8,
        }
    ));
}

#[tokio::test]
async fn context_plan_routes_mcp_catalog_questions_to_mcp_sources() {
    let (root, services) = fixture().await;
    std::fs::create_dir_all(root.path().join("src/mcp")).expect("create MCP source directory");
    std::fs::create_dir_all(root.path().join("src/watcher/tests"))
        .expect("create watcher test directory");
    std::fs::write(
        root.path().join("src/mcp/tools.rs"),
        "// MCP tool catalog registration and request schemas\npub fn register_tools() {}\n",
    )
    .expect("write MCP tool fixture");
    std::fs::write(
        root.path().join("src/mcp/tests.rs"),
        "// MCP catalog schema regression tests\n",
    )
    .expect("write MCP test fixture");
    std::fs::write(
        root.path().join("src/watcher/tests/support.rs"),
        "struct RegistrationFailure;\n",
    )
    .expect("write unrelated registration fixture");
    services.index(false).await.expect("index MCP routing fixture");

    let mut request = context_limit_request(600);
    request.task = "Where is MCP tool registration and catalog schema defined?".into();
    request.plan_only = true;
    request.max_fragments = Some(4);
    let response = services.context(request).await.expect("context plan");
    let plan = response.plan.expect("plan-only response");
    assert_eq!(
        plan.candidates.first().map(|candidate| candidate.path.as_str()),
        Some("src/mcp/tools.rs")
    );
    assert!(plan
        .candidates
        .iter()
        .take(2)
        .all(|candidate| candidate.path.starts_with("src/mcp")));
}

#[tokio::test]
async fn context_plan_previews_materialization_without_receipt_or_source() {
    let (_root, services) = fixture().await;
    let mut request = context_limit_request(100);
    let source_budget = request.token_budget;
    request.focus_paths = vec!["src/**".into()];
    request.strict_focus_paths = true;
    request.plan_only = true;
    let savings_before = services.token_savings().await.expect("savings before plan");

    let preview = services
        .context(request.clone())
        .await
        .expect("context plan");
    let plan = preview.plan.as_ref().expect("query plan");

    assert!(preview.fragments.is_empty());
    assert!(preview.receipt.fragment_hashes.is_empty());
    assert_eq!(preview.meta.source_tokens, 0);
    assert!(preview.meta.receipt_id.is_none());
    assert!(!plan.candidates.is_empty());
    assert!(plan.focus_coverage[0].satisfied);
    assert!(
        preview.meta.total_response_tokens > source_budget,
        "the current source-only budget must not be mistaken for a full response ceiling"
    );
    assert!(
        preview
            .coverage
            .focus_path_coverage
            .iter()
            .all(|coverage| coverage.satisfied)
    );
    assert_response_token_accounting!(preview, Tokenizer::default());
    let savings_after = services.token_savings().await.expect("savings after plan");
    assert_eq!(
        savings_after.tracked_requests,
        savings_before.tracked_requests
    );
    assert_eq!(
        savings_after.estimated_source_tokens_saved,
        savings_before.estimated_source_tokens_saved
    );
    let accounting = services
        .token_savings_report()
        .await
        .expect("response accounting");
    let plan_accounting = accounting
        .response_accounting
        .by_operation
        .iter()
        .find(|row| row.operation == TokenAccountingOperation::ContextPlan)
        .expect("context plan accounting");
    assert_eq!(plan_accounting.tracked_requests, 1);
    assert_eq!(plan_accounting.baseline_requests, 0);
    assert_eq!(
        plan_accounting.total_response_tokens,
        preview.meta.total_response_tokens as u64
    );
    assert_eq!(
        plan_accounting.estimated_net_tokens_saved,
        -(preview.meta.total_response_tokens as i64)
    );

    request.plan_only = false;
    let materialized = services.context(request).await.expect("materialized context");
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
    assert!(
        preview.meta.total_response_tokens <= materialized.meta.total_response_tokens,
        "a non-empty metadata plan should not exceed its materialized response"
    );
}

#[tokio::test]
async fn context_options_enforce_the_final_serialized_service_response_budget() {
    let (root, services) = fixture().await;
    for index in 0..6 {
        let body = (0..80)
            .map(|line| format!("    let greet_value_{line} = \"hello {index} {line}\";\n"))
            .collect::<String>();
        std::fs::write(
            root.path().join(format!("src/greet_{index}.rs")),
            format!("pub fn greet_{index}() {{\n{body}}}\n"),
        )
        .expect("write context budget fixture");
    }
    services.index(false).await.expect("reindex budget fixture");
    let request = context_limit_request(1_000);
    let source_budget = request.token_budget;
    let unrestricted = services
        .context(request.clone())
        .await
        .expect("unrestricted context");
    let removable_tokens = unrestricted
        .fragments
        .last()
        .map_or(1, |fragment| fragment.token_count.max(1));
    let max_response_tokens = unrestricted
        .meta
        .total_response_tokens
        .saturating_sub(removable_tokens);

    let bounded = services
        .context_with_options(
            request.clone(),
            ServiceCallOptions::new().with_max_response_tokens(max_response_tokens),
        )
        .await
        .expect("bounded context");
    let repeated = services
        .context_with_options(
            request,
            ServiceCallOptions::new().with_max_response_tokens(max_response_tokens),
        )
        .await
        .expect("repeated bounded context");

    assert!(bounded.meta.total_response_tokens <= max_response_tokens);
    assert!(repeated.meta.total_response_tokens <= max_response_tokens);
    assert!(bounded.meta.source_tokens <= source_budget);
    assert!(
        bounded.fragments.len() < unrestricted.fragments.len(),
        "a tighter total ceiling must remove the lowest-ranked optional source"
    );
    assert_response_token_accounting!(bounded, Tokenizer::Cl100kBase);
    assert_response_token_accounting!(repeated, Tokenizer::Cl100kBase);
    assert_eq!(
        bounded
            .fragments
            .iter()
            .map(|fragment| (&fragment.path, fragment.start_line, fragment.end_line))
            .collect::<Vec<_>>(),
        repeated
            .fragments
            .iter()
            .map(|fragment| (&fragment.path, fragment.start_line, fragment.end_line))
            .collect::<Vec<_>>()
    );

    let mut underfilled = context_limit_request(4_000);
    underfilled.task = "perform the requested audit".into();
    underfilled.focus_paths = vec!["src/lib.rs".into()];
    underfilled.focus_symbols = vec!["greet".into()];
    underfilled.strict_focus_paths = true;
    underfilled.minimum_fragments_per_focus_path = Some(2);
    underfilled.plan_only = true;
    let underfilled = services
        .context(underfilled)
        .await
        .expect("underfilled focus plan");
    assert!(!underfilled.coverage.focus_path_coverage[0].satisfied);
    assert!(
        underfilled.warnings.iter().any(|warning| {
            warning.contains("generated 1 distinct bounded candidates")
                && warning.contains("requested minimum 2")
        }),
        "unexpected underfilled warnings: {:#?}",
        underfilled.warnings
    );
}

#[tokio::test]
async fn context_response_budget_fails_loudly_when_the_mandatory_skeleton_cannot_fit() {
    let (root, services) = fixture().await;
    let generation = services
        .status()
        .await
        .expect("status before invalid limit")
        .repository_generation;
    std::fs::write(
        root.path().join("src/pending.rs"),
        "pub fn pending_response_budget_change() {}\n",
    )
    .expect("write pending change");
    let mut conflicting_profile = context_limit_request(100);
    conflicting_profile.verbose_diagnostics = true;
    let invalid = services
        .context_with_workflow_evidence_options_consistency_cancellable(
            leantoken::services::ContextWorkflowOptions {
                request: conflicting_profile,
                handoff: None,
                workflow: ContextWorkflow::Auto,
                workflow_evidence: WorkflowEvidence::default(),
                consistency: IndexConsistency::ReconcileWorkingTree,
                options: ServiceCallOptions::new()
                    .with_context_response_profile(ContextResponseProfile::Balanced),
                cancellation: CancellationToken::new(),
            },
        )
        .await
        .expect_err("explicit balanced profile must reject legacy verbose diagnostics");
    assert!(matches!(
        invalid,
        Error::InvalidInput {
            field: "response_profile",
            reason: "verbose_diagnostics=true requires response_profile=explain",
        }
    ));
    assert_eq!(
        services
            .status()
            .await
            .expect("status after conflicting response profile")
            .repository_generation,
        generation
    );

    let invalid = services
        .context_with_workflow_evidence_options_consistency_cancellable(
            leantoken::services::ContextWorkflowOptions {
                request: context_limit_request(100),
                handoff: None,
                workflow: ContextWorkflow::Auto,
                workflow_evidence: WorkflowEvidence::default(),
                consistency: IndexConsistency::ReconcileWorkingTree,
                options: ServiceCallOptions::new().with_max_response_tokens(0),
                cancellation: CancellationToken::new(),
            },
        )
        .await
        .expect_err("zero response limit must fail before reconciliation");
    assert!(matches!(
        invalid,
        Error::InvalidInput {
            field: "max_response_tokens",
            ..
        }
    ));
    assert_eq!(
        services
            .status()
            .await
            .expect("status after invalid limit")
            .repository_generation,
        generation
    );

    let mut request = context_limit_request(100);
    request.focus_paths = vec!["src/**".into()];
    request.strict_focus_paths = true;

    let error = services
        .context_with_options(
            request,
            ServiceCallOptions::new().with_max_response_tokens(1),
        )
        .await
        .expect_err("one token cannot fit the correctness skeleton");

    let (minimum, breakdown) = assert_response_budget_error(error, 1);
    assert!(minimum > 1);
    assert!(breakdown.mandatory_response_tokens > 0);
}

#[tokio::test]
async fn context_response_budget_details_are_exact_for_plan_and_materialization() {
    let (_root, services) = fixture().await;
    for plan_only in [true, false] {
        let mut request = context_limit_request(200);
        request.task = "inspect greet".into();
        request.focus_paths = vec!["src/lib.rs".into()];
        request.strict_focus_paths = true;
        request.plan_only = plan_only;

        let (minimum, breakdown) = assert_response_budget_error(
            services
                .context_with_options(
                    request.clone(),
                    ServiceCallOptions::new().with_max_response_tokens(1),
                )
                .await
                .expect_err("one token cannot fit context"),
            1,
        );
        if plan_only {
            assert_eq!(breakdown.receipt_reserve_tokens, 0);
        } else {
            assert!(breakdown.receipt_reserve_tokens > 0);
        }

        let exact = services
            .context_with_options(
                request.clone(),
                ServiceCallOptions::new().with_max_response_tokens(minimum),
            )
            .await
            .expect("reported context minimum must be directly retryable");
        assert!(exact.meta.total_response_tokens <= minimum);
        let (repeated_minimum, repeated_breakdown) = assert_response_budget_error(
            services
                .context_with_options(
                    request.clone(),
                    ServiceCallOptions::new().with_max_response_tokens(minimum - 1),
                )
                .await
                .expect_err("one token below context minimum must fail"),
            minimum - 1,
        );
        assert_eq!(repeated_minimum, minimum);
        assert_eq!(repeated_breakdown, breakdown);

        services
            .context_with_options(
                request,
                ServiceCallOptions::new().with_max_response_tokens(32_000),
            )
            .await
            .expect("configured response maximum must remain accepted");
    }
}

#[tokio::test]
async fn context_plan_diff_evidence_is_opt_in_and_never_smaller_when_expanded() {
    let (_root, services) = fixture().await;
    let mut request = context_limit_request(200);
    request.plan_only = true;
    request.changed_paths = vec!["src/lib.rs".into()];

    let compact = services
        .context(request.clone())
        .await
        .expect("compact diff plan");
    assert_eq!(
        compact.effective_response_profile,
        ContextResponseProfile::Balanced
    );
    assert!(
        compact
            .diff_scope
            .as_ref()
            .expect("diff scope")
            .evidence
            .is_none()
    );

    request.verbose_diagnostics = true;
    let expanded = services.context(request).await.expect("expanded diff plan");
    assert_eq!(
        expanded.effective_response_profile,
        ContextResponseProfile::Explain
    );
    assert!(
        expanded
            .diff_scope
            .as_ref()
            .expect("diff scope")
            .evidence
            .is_some()
    );
    assert!(expanded.meta.total_response_tokens >= compact.meta.total_response_tokens);
}

#[tokio::test]
async fn context_response_profiles_only_change_bounded_presentation() {
    let root = tempfile::tempdir().expect("temporary repository");
    std::fs::create_dir_all(root.path().join("src/browser")).expect("browser directory");
    std::fs::create_dir_all(root.path().join("src/managed")).expect("managed directory");
    let source = "pub fn shared_capture_target() -> bool { true }\n";
    std::fs::write(root.path().join("src/browser/capture.rs"), source)
        .expect("browser source");
    std::fs::write(
        root.path().join("src/browser/secondary.rs"),
        "pub fn shared_capture_target_secondary() -> bool { true }\n",
    )
    .expect("secondary browser source");
    std::fs::write(root.path().join("src/managed/evidence.rs"), source)
        .expect("managed source");
    let config =
        Config::discover(root.path(), Some(root.path().join("index.sqlite"))).expect("config");
    let services = Services::open(config).expect("services");
    services.index(false).await.expect("index fixture");

    let mut request = context_limit_request(200);
    request.task = "find shared_capture_target".into();
    request.include_paths = vec!["src/browser/**".into()];
    request.focus_paths = vec!["src/browser/**".into()];
    request.changed_paths = vec!["src/browser/capture.rs".into()];
    request.max_fragments = Some(1);

    let balanced = services
        .context_with_options(
            request.clone(),
            ServiceCallOptions::new()
                .with_context_response_profile(ContextResponseProfile::Balanced),
        )
        .await
        .expect("balanced response");
    let compact = services
        .context_with_options(
            request.clone(),
            ServiceCallOptions::new()
                .with_context_response_profile(ContextResponseProfile::Compact),
        )
        .await
        .expect("compact response");
    let explain = services
        .context_with_options(
            request.clone(),
            ServiceCallOptions::new()
                .with_context_response_profile(ContextResponseProfile::Explain),
        )
        .await
        .expect("explain response");
    let default = services
        .context(request.clone())
        .await
        .expect("default balanced response");
    request.verbose_diagnostics = true;
    let explicit_legacy_explain = services
        .context_with_options(
            request.clone(),
            ServiceCallOptions::new()
                .with_context_response_profile(ContextResponseProfile::Explain),
        )
        .await
        .expect("explicit explain accepts legacy verbose diagnostics");
    let legacy_explain = services
        .context(request)
        .await
        .expect("legacy verbose response");

    assert_eq!(
        balanced.effective_response_profile,
        ContextResponseProfile::Balanced
    );
    assert_eq!(
        compact.effective_response_profile,
        ContextResponseProfile::Compact
    );
    assert_eq!(
        explain.effective_response_profile,
        ContextResponseProfile::Explain
    );
    assert_eq!(
        default.effective_response_profile,
        ContextResponseProfile::Balanced
    );
    assert_eq!(
        legacy_explain.effective_response_profile,
        ContextResponseProfile::Explain
    );
    assert_eq!(
        explicit_legacy_explain.effective_response_profile,
        ContextResponseProfile::Explain
    );
    assert!(
        balanced.coverage.focus_path_coverage[0]
            .diagnostics
            .is_none()
    );
    assert!(
        compact.coverage.focus_path_coverage[0]
            .diagnostics
            .is_none()
    );
    assert!(
        explain.coverage.focus_path_coverage[0]
            .diagnostics
            .is_some()
    );
    assert!(
        legacy_explain.coverage.focus_path_coverage[0]
            .diagnostics
            .is_some()
    );

    let identities = |response: &leantoken::ContextResponse| {
        response
            .fragments
            .iter()
            .map(|fragment| {
                (
                    fragment.path.clone(),
                    fragment.start_line,
                    fragment.end_line,
                    fragment.content_hash.clone(),
                    fragment.score.to_bits(),
                    fragment.reason.clone(),
                    fragment.token_count,
                )
            })
            .collect::<Vec<_>>()
    };
    let balanced_identities = identities(&balanced);
    let coverage_without_focus_diagnostics = |response: &leantoken::ContextResponse| {
        let mut coverage = response.coverage.clone();
        for focus in &mut coverage.focus_path_coverage {
            focus.diagnostics = None;
        }
        coverage
    };
    let balanced_coverage = coverage_without_focus_diagnostics(&balanced);
    for response in [
        &compact,
        &explain,
        &default,
        &legacy_explain,
        &explicit_legacy_explain,
    ] {
        assert_eq!(identities(response), balanced_identities);
        assert_eq!(
            response.receipt.task_fingerprint,
            balanced.receipt.task_fingerprint
        );
        assert_eq!(
            response.receipt.fragment_hashes,
            balanced.receipt.fragment_hashes
        );
        assert_eq!(response.meta.source_tokens, balanced.meta.source_tokens);
        assert_eq!(
            coverage_without_focus_diagnostics(response),
            balanced_coverage
        );
        assert_eq!(response.workflow, balanced.workflow);
        assert_eq!(response.routing, balanced.routing);
        assert_eq!(response.warnings, balanced.warnings);
        assert_eq!(
            response.omission_summary.path_excluded,
            balanced.omission_summary.path_excluded
        );
        assert_eq!(
            response.omission_summary.known_hash,
            balanced.omission_summary.known_hash
        );
        assert_eq!(
            response.omission_summary.budget_or_result_limit,
            balanced.omission_summary.budget_or_result_limit
        );
    }

    assert!(!balanced.fragments.is_empty());
    assert!(balanced.omission_summary.path_excluded > 0);
    assert!(compact.omitted.is_empty());
    assert!(compact.omission_summary.by_path.is_empty());
    assert!(compact.omission_summary.by_reason.is_empty());
    assert!(
        compact
            .diff_scope
            .as_ref()
            .expect("compact diff scope")
            .evidence
            .is_none()
    );
    assert!(
        balanced
            .diff_scope
            .as_ref()
            .expect("balanced diff scope")
            .evidence
            .is_some()
    );
    assert!(!explain.omitted.is_empty());
    assert!(!explain.omission_summary.by_path.is_empty());
    assert!(!explain.omission_summary.by_reason.is_empty());
    assert!(
        explain
            .diff_scope
            .as_ref()
            .expect("explain diff scope")
            .evidence
            .is_some()
    );
    assert_eq!(
        serde_json::to_value(&legacy_explain.omitted).expect("legacy omission details"),
        serde_json::to_value(&explain.omitted).expect("explicit omission details")
    );
    assert_eq!(
        legacy_explain.omission_summary,
        explain.omission_summary
    );

    assert!(
        compact.meta.total_response_tokens < balanced.meta.total_response_tokens,
        "compact={} balanced={}",
        compact.meta.total_response_tokens,
        balanced.meta.total_response_tokens
    );
    assert!(
        compact.meta.total_response_tokens < explain.meta.total_response_tokens,
        "compact={} explain={}",
        compact.meta.total_response_tokens,
        explain.meta.total_response_tokens
    );
    assert_response_token_accounting!(compact, Tokenizer::Cl100kBase);
    assert_response_token_accounting!(balanced, Tokenizer::Cl100kBase);
    assert_response_token_accounting!(explain, Tokenizer::Cl100kBase);
}

#[tokio::test]
async fn context_plan_only_respects_the_serialized_response_budget() {
    let (_root, services) = fixture().await;
    let mut request = context_limit_request(200);
    request.plan_only = true;
    let unrestricted = services
        .context(request.clone())
        .await
        .expect("unrestricted plan");
    let max_response_tokens = unrestricted.meta.total_response_tokens.saturating_sub(1);

    let bounded = services
        .context_with_options(
            request,
            ServiceCallOptions::new().with_max_response_tokens(max_response_tokens),
        )
        .await
        .expect("bounded plan");

    assert!(bounded.plan.is_some());
    assert!(bounded.fragments.is_empty());
    assert!(bounded.meta.receipt_id.is_none());
    assert!(bounded.meta.total_response_tokens <= max_response_tokens);
    assert_response_token_accounting!(bounded, Tokenizer::Cl100kBase);
}

#[tokio::test]
async fn context_rejects_empty_include_patterns() {
    let (_root, services) = fixture().await;
    let mut request = context_limit_request(100);
    request.include_paths = vec![String::new()];

    let error = services
        .context(request)
        .await
        .expect_err("empty include pattern");

    assert!(matches!(
        error,
        Error::InvalidInput {
            field: "include paths",
            reason: "must not contain empty patterns"
        }
    ));

    let mut empty_exclude = context_limit_request(100);
    empty_exclude.exclude_paths = vec![String::new()];
    let error = services
        .context(empty_exclude)
        .await
        .expect_err("empty exclude pattern");
    assert!(matches!(
        error,
        Error::InvalidInput {
            field: "exclude paths",
            reason: "must not contain empty patterns"
        }
    ));

    let mut empty_focus_symbol = context_limit_request(100);
    empty_focus_symbol.focus_symbols = vec!["   ".into()];
    let error = services
        .context(empty_focus_symbol)
        .await
        .expect_err("empty focus symbol");
    assert!(matches!(
        error,
        Error::InvalidInput {
            field: "focus symbols",
            reason: "must not contain empty symbols"
        }
    ));

    let mut plan_with_receipt = context_limit_request(100);
    plan_with_receipt.plan_only = true;
    plan_with_receipt.receipt_id = Some("existing".into());
    let error = services
        .context(plan_with_receipt)
        .await
        .expect_err("plan receipt mutation");
    assert!(matches!(
        error,
        Error::InvalidInput {
            field: "receipt_id",
            reason: "must be omitted when plan_only is true"
        }
    ));

    let mut strict = context_limit_request(100);
    strict.strict_focus_paths = true;
    let error = services
        .context(strict)
        .await
        .expect_err("strict focus without paths");
    assert!(matches!(
        error,
        Error::InvalidInput {
            field: "focus paths",
            reason: "must not be empty when focus path constraints are enabled"
        }
    ));
}

#[tokio::test]
async fn context_include_paths_constrain_fragments_and_report_path_omissions() {
    let root = tempfile::tempdir().expect("temporary repository");
    std::fs::create_dir_all(root.path().join("src/browser")).expect("browser directory");
    std::fs::create_dir_all(root.path().join("src/managed")).expect("managed directory");
    let source = "pub fn shared_capture_target() -> bool { true }\n";
    std::fs::write(root.path().join("src/browser/capture.rs"), source).expect("browser source");
    std::fs::write(root.path().join("src/managed/evidence.rs"), source).expect("managed source");
    let config =
        Config::discover(root.path(), Some(root.path().join("index.sqlite"))).expect("config");
    let services = Services::open(config).expect("services");
    services.index(false).await.expect("index fixture");
    let mut request = context_limit_request(200);
    request.task = "find shared_capture_target".into();
    request.include_paths = vec!["src/browser/**".into()];
    let compact = services
        .context(request.clone())
        .await
        .expect("compact constrained context");
    assert!(compact.omitted.is_empty());
    assert!(compact.omission_summary.path_excluded > 0);
    assert!(compact.omission_summary.by_path.is_empty());
    assert!(compact.omission_summary.by_reason.is_empty());

    request.verbose_diagnostics = true;

    let response = services.context(request).await.expect("constrained context");

    assert!(!response.fragments.is_empty());
    assert!(
        response
            .fragments
            .iter()
            .all(|fragment| fragment.path.starts_with("src/browser/"))
    );
    assert!(response.omission_summary.path_excluded > 0);
    assert!(
        response
            .omission_summary
            .by_reason
            .iter()
            .any(|facet| facet.value == "path_excluded"
                && facet.count == response.omission_summary.path_excluded)
    );
    assert!(
        response
            .omission_summary
            .by_path
            .iter()
            .any(|facet| facet.value == "src/managed/evidence.rs")
    );
    assert!(
        response
            .warnings
            .iter()
            .any(|warning| warning.contains("omitted"))
    );
}

#[tokio::test]
async fn repository_context_exclusions_preserve_exact_artifact_access() {
    let root = tempfile::tempdir().expect("temporary repository");
    std::fs::create_dir(root.path().join("src")).expect("source directory");
    std::fs::create_dir(root.path().join("generated")).expect("generated directory");
    std::fs::write(
        root.path().join(".leantoken.toml"),
        "[context]\nexclude_paths = [\"generated/**\"]\n",
    )
    .expect("repository config");
    std::fs::write(
        root.path().join("src/lib.rs"),
        "pub fn active_contract() -> bool { true }\n",
    )
    .expect("source");
    std::fs::write(
        root.path().join("generated/report.rs"),
        "pub fn generated_only_target() -> bool { true }\n",
    )
    .expect("generated artifact");
    let config =
        Config::discover(root.path(), Some(root.path().join("index.sqlite"))).expect("config");
    let services = Services::open(config).expect("services");
    services.index(false).await.expect("index fixture");

    let files = services
        .files(FilesRequest {
            operation: FileOperation::Find,
            path: None,
            query: Some("generated/report".into()),
            pattern: None,
            max_results: Some(10),
            cursor: None,
            depth: None,
        })
        .await
        .expect("exact files");
    assert!(
        files
            .entries
            .iter()
            .any(|entry| entry.path == "generated/report.rs")
    );

    let search = services
        .search(SearchRequest {
            query: "generated_only_target".into(),
            mode: SearchMode::Identifier,
            include_paths: Vec::new(),
            exclude_paths: Vec::new(),
            focus_paths: Vec::new(),
            max_results: Some(10),
            max_tokens: Some(200),
            context_lines: Some(1),
            case_sensitive: true,
            all_occurrences: false,
            prefer_structural: false,
            receipt_id: None,
            query_receipt: None,
            cursor: None,
        })
        .await
        .expect("exact search");
    assert!(
        search
            .hits
            .iter()
            .any(|hit| hit.path == "generated/report.rs")
    );

    let read = services
        .read(ReadRequest {
            path: "generated/report.rs".into(),
            start_line: None,
            end_line: None,
            symbol: Some("generated_only_target".into()),
            heading: None,
            heading_occurrence: None,
            continuation_cursor: None,
            max_tokens: Some(200),
            expected_hash: None,
            delta: false,
            receipt_id: None,
        })
        .await
        .expect("exact read");
    assert!(
        read.content
            .expect("read content")
            .contains("generated_only_target")
    );

    let mut default_request = context_limit_request(200);
    default_request.task = "change generated_only_target".into();
    let default_context = services
        .context(default_request)
        .await
        .expect("default context");
    assert!(
        default_context
            .fragments
            .iter()
            .all(|fragment| fragment.path != "generated/report.rs")
    );
    assert!(default_context.omission_summary.path_excluded > 0);

    let mut included_request = context_limit_request(200);
    included_request.task = "change generated_only_target".into();
    included_request.include_paths = vec!["generated/**".into()];
    let included_context = services
        .context(included_request)
        .await
        .expect("included context");
    assert!(
        included_context
            .fragments
            .iter()
            .any(|fragment| fragment.path == "generated/report.rs")
    );
}

#[tokio::test]
async fn strict_focus_paths_enforce_minimum_coverage_and_fail_loud() {
    let root = tempfile::tempdir().expect("temporary repository");
    for (path, symbol) in [
        ("src/alpha/one.rs", "shared_scope_target_alpha_one"),
        ("src/alpha/two.rs", "shared_scope_target_alpha_two"),
        ("src/beta/one.rs", "shared_scope_target_beta_one"),
        ("src/beta/two.rs", "shared_scope_target_beta_two"),
        ("artifacts/noise.rs", "shared_scope_target_noise"),
    ] {
        let path = root.path().join(path);
        std::fs::create_dir_all(path.parent().expect("fixture parent")).expect("fixture directory");
        std::fs::write(path, format!("pub fn {symbol}() -> bool {{ true }}\n"))
            .expect("fixture source");
    }
    let services = Services::open(
        Config::discover(root.path(), Some(root.path().join("index.sqlite"))).expect("config"),
    )
    .expect("services");
    services.index(false).await.expect("index fixture");

    let mut ordinary_focus = context_limit_request(1_000);
    ordinary_focus.task = "change shared_scope_target".into();
    ordinary_focus.focus_paths = vec!["src/alpha/**".into(), "src/beta/**".into()];
    ordinary_focus.max_fragments = Some(1);
    let ordinary_focus = services
        .context(ordinary_focus)
        .await
        .expect("ordinary focus context");
    assert_eq!(ordinary_focus.coverage.strict_scope_satisfied, None);
    assert_eq!(ordinary_focus.coverage.path_scope_satisfied, None);
    assert_eq!(ordinary_focus.coverage.focus_path_coverage.len(), 2);
    assert!(
        ordinary_focus
            .coverage
            .focus_path_coverage
            .iter()
            .all(|focus| focus.indexed_paths == 2 && focus.minimum_fragments == 1)
    );
    assert_eq!(
        ordinary_focus
            .coverage
            .focus_path_coverage
            .iter()
            .filter(|focus| focus.satisfied)
            .count(),
        1
    );

    let mut request = context_limit_request(1_000);
    request.task = "change shared_scope_target".into();
    request.focus_paths = vec!["src/alpha/**".into(), "src/beta/**".into()];
    request.strict_focus_paths = true;
    request.minimum_fragments_per_focus_path = Some(2);
    request.max_fragments = Some(4);
    let response = services.context(request).await.expect("strict focus context");

    assert_eq!(response.fragments.len(), 4);
    assert!(
        response
            .fragments
            .iter()
            .all(|fragment| fragment.path.starts_with("src/alpha/")
                || fragment.path.starts_with("src/beta/"))
    );
    assert_eq!(response.coverage.strict_scope_satisfied, Some(true));
    assert_eq!(response.coverage.path_scope_satisfied, Some(true));
    assert_eq!(response.coverage.focus_path_coverage.len(), 2);
    assert!(
        response
            .coverage
            .focus_path_coverage
            .iter()
            .all(|focus| focus.indexed_paths == 2
                && focus.minimum_fragments == 2
                && focus.selected_fragments == 2
                && focus.satisfied)
    );
    assert!(response.omission_summary.path_excluded > 0);

    let mut soft_minimum = context_limit_request(1_000);
    soft_minimum.task = "change shared_scope_target".into();
    soft_minimum.focus_paths = vec!["src/alpha/**".into()];
    soft_minimum.minimum_fragments_per_focus_path = Some(2);
    soft_minimum.max_fragments = Some(3);
    let soft_minimum = services
        .context(soft_minimum)
        .await
        .expect("soft focus minimum");
    assert_eq!(soft_minimum.fragments.len(), 3);
    assert_eq!(soft_minimum.coverage.strict_scope_satisfied, Some(true));
    assert_eq!(soft_minimum.coverage.path_scope_satisfied, Some(true));
    assert_eq!(
        soft_minimum.coverage.focus_path_coverage[0].selected_fragments,
        2
    );
    assert!(
        soft_minimum
            .fragments
            .iter()
            .any(|fragment| !fragment.path.starts_with("src/alpha/"))
    );

    let mut underfilled = context_limit_request(1_000);
    underfilled.task = "change shared_scope_target".into();
    underfilled.focus_paths = vec!["src/alpha/**".into(), "src/beta/**".into()];
    underfilled.strict_focus_paths = true;
    underfilled.minimum_fragments_per_focus_path = Some(2);
    underfilled.max_fragments = Some(3);
    underfilled.verbose_diagnostics = true;
    let underfilled = services
        .context(underfilled)
        .await
        .expect("underfilled focus context");
    assert_eq!(underfilled.fragments.len(), 3);
    assert_eq!(underfilled.coverage.strict_scope_satisfied, Some(false));
    assert_eq!(underfilled.coverage.path_scope_satisfied, Some(false));
    assert_eq!(
        underfilled
            .coverage
            .focus_path_coverage
            .iter()
            .filter(|focus| focus.satisfied)
            .count(),
        1
    );
    let satisfied = underfilled
        .coverage
        .focus_path_coverage
        .iter()
        .find(|focus| focus.satisfied)
        .expect("satisfied focus diagnostics");
    let satisfied_diagnostics = satisfied
        .diagnostics
        .as_ref()
        .expect("satisfied focus diagnostics");
    assert!(satisfied_diagnostics.generated_fragments >= 2);
    assert_eq!(satisfied_diagnostics.reserved_fragments, 2);
    assert!(satisfied_diagnostics.selected_source_tokens > 0);
    assert_eq!(satisfied_diagnostics.capacity_blocker, None);
    let underfilled_focus = underfilled
        .coverage
        .focus_path_coverage
        .iter()
        .find(|focus| !focus.satisfied)
        .expect("underfilled focus diagnostics");
    let underfilled_diagnostics = underfilled_focus
        .diagnostics
        .as_ref()
        .expect("underfilled focus diagnostics");
    assert!(underfilled_diagnostics.generated_fragments >= 2);
    assert_eq!(underfilled_diagnostics.reserved_fragments, 1);
    assert!(
        underfilled_diagnostics
            .suppressed_by
            .iter()
            .any(|suppression| {
                suppression.boundary == ContextFocusSuppressionBoundary::MaxFragments
                    && suppression.fragments > 0
            })
    );
    assert_eq!(
        underfilled_diagnostics.capacity_blocker,
        Some(ContextFocusCapacityBlocker::MaxFragments)
    );

    let mut missing = context_limit_request(400);
    missing.task = "change shared_scope_target".into();
    missing.focus_paths = vec!["src/missing/**".into()];
    missing.strict_focus_paths = true;
    missing.verbose_diagnostics = true;
    let missing = services.context(missing).await.expect("missing strict focus");
    assert!(missing.fragments.is_empty());
    assert_eq!(missing.coverage.strict_scope_satisfied, Some(false));
    assert_eq!(missing.coverage.path_scope_satisfied, Some(false));
    assert_eq!(missing.coverage.unmatched_focus_paths, ["src/missing/**"]);
    assert_eq!(missing.coverage.focus_path_coverage[0].indexed_paths, 0);
    assert!(!missing.coverage.focus_path_coverage[0].satisfied);
    assert_eq!(
        missing.coverage.focus_path_coverage[0]
            .diagnostics
            .as_ref()
            .expect("missing focus diagnostics")
            .capacity_blocker,
        Some(ContextFocusCapacityBlocker::NoIndexedPaths)
    );
    assert!(missing.warnings.iter().any(|warning| {
        warning.contains("focus path constraints did not meet minimum fragment coverage")
    }));
}

#[tokio::test]
async fn strict_focus_paths_generate_candidates_before_global_top_n_truncation() {
    let root = tempfile::tempdir().expect("temporary repository");
    std::fs::create_dir_all(root.path().join("aaa_noise")).expect("noise directory");
    std::fs::create_dir_all(root.path().join("focus")).expect("focus directory");
    for index in 0..64 {
        std::fs::write(
            root.path().join(format!("aaa_noise/noise_{index:02}.rs")),
            format!("pub fn buried_focus_target() -> usize {{ {index} }}\n"),
        )
        .expect("noise source");
    }
    let focus_paths = (0..9)
        .map(|index| format!("focus/owner_{index}.rs"))
        .collect::<Vec<_>>();
    for (index, path) in focus_paths.iter().enumerate() {
        std::fs::write(
            root.path().join(path),
            format!("pub fn buried_focus_target_owner_{index}() -> usize {{ {index} }}\n"),
        )
        .expect("focus source");
    }
    let services = Services::open(
        Config::discover(root.path(), Some(root.path().join("index.sqlite"))).expect("config"),
    )
    .expect("services");
    services.index(false).await.expect("index fixture");

    let request = {
        let mut request = context_limit_request(4_000);
        request.task = "change buried_focus_target".into();
        request.focus_paths.clone_from(&focus_paths);
        request.strict_focus_paths = true;
        request.minimum_fragments_per_focus_path = Some(1);
        request.max_fragments = Some(focus_paths.len());
        request
    };
    let first = services
        .context(request.clone())
        .await
        .expect("focused context");
    let second = services
        .context(request.clone())
        .await
        .expect("deterministic focused context");

    assert_eq!(first.fragments.len(), focus_paths.len());
    assert_eq!(first.coverage.strict_scope_satisfied, Some(true));
    assert!(first.coverage.focus_path_coverage.iter().all(
        |focus| focus.indexed_paths == 1
            && focus.selected_fragments == 1
            && focus.satisfied
    ));
    assert_eq!(
        first
            .fragments
            .iter()
            .map(|fragment| (&fragment.path, &fragment.content_hash))
            .collect::<Vec<_>>(),
        second
            .fragments
            .iter()
            .map(|fragment| (&fragment.path, &fragment.content_hash))
            .collect::<Vec<_>>()
    );

    let evaluation = services
        .context_evaluation(request.clone())
        .await
        .expect("focus candidate evaluation");
    assert_eq!(
        evaluation
            .primitive_keys
            .iter()
            .filter(|primitive| {
                matches!(
                    primitive.kind.as_str(),
                    "focus_file_chunks" | "focus_file_symbols"
                )
            })
            .count(),
        focus_paths.len() * 2
    );

    let mut plan_request = request;
    plan_request.plan_only = true;
    let plan = services
        .context(plan_request)
        .await
        .expect("focused plan")
        .plan
        .expect("plan");
    assert_eq!(
        plan.candidates
            .iter()
            .map(|candidate| candidate.path.as_str())
            .collect::<Vec<_>>(),
        first
            .fragments
            .iter()
            .map(|fragment| fragment.path.as_str())
            .collect::<Vec<_>>()
    );

    let mut overlapping = context_limit_request(1_000);
    overlapping.task = "change buried_focus_target".into();
    overlapping.focus_paths =
        vec!["focus/owner_0.rs".into(), "focus/owner_*.rs".into()];
    overlapping.strict_focus_paths = true;
    overlapping.max_fragments = Some(1);
    let overlapping = services
        .context(overlapping)
        .await
        .expect("overlapping exact and glob focus");
    assert_eq!(overlapping.fragments.len(), 1);
    assert_eq!(overlapping.fragments[0].path, "focus/owner_0.rs");
    assert!(overlapping.coverage.focus_path_coverage.iter().all(
        |focus| focus.selected_fragments == 1 && focus.satisfied
    ));

    let mut broad = context_limit_request(2_000);
    broad.task = "change buried_focus_target".into();
    broad.focus_paths = vec!["focus/**".into()];
    broad.strict_focus_paths = true;
    broad.max_fragments = Some(4);
    broad.plan_only = true;
    let broad = services.context(broad).await.expect("bounded broad focus");
    assert_eq!(broad.coverage.focus_path_coverage[0].indexed_paths, 9);
    assert!(broad.warnings.iter().any(|warning| {
        warning.contains("matched 9 eligible indexed paths")
            && warning.contains("inspected the first 4 paths")
    }));
    assert_eq!(
        broad
            .plan
            .expect("broad plan")
            .candidates
            .iter()
            .map(|candidate| candidate.path.as_str())
            .collect::<Vec<_>>(),
        vec![
            "focus/owner_0.rs",
            "focus/owner_1.rs",
            "focus/owner_2.rs",
            "focus/owner_3.rs",
        ]
    );

    let mut fanout_limited = context_limit_request(4_000);
    fanout_limited.task = "perform the requested audit".into();
    fanout_limited.focus_paths = vec!["focus/**".into()];
    fanout_limited.minimum_fragments_per_focus_path = Some(8);
    fanout_limited.max_fragments = Some(8);
    fanout_limited.plan_only = true;
    fanout_limited.verbose_diagnostics = true;
    let fanout_limited = services
        .context(fanout_limited)
        .await
        .expect("fan-out-limited focus plan");
    let fanout_diagnostics = fanout_limited.coverage.focus_path_coverage[0]
        .diagnostics
        .as_ref()
        .expect("fan-out-limited diagnostics");
    assert_eq!(fanout_diagnostics.eligible_paths, 9);
    assert!(fanout_diagnostics.generated_fragments < 8);
    assert_eq!(
        fanout_diagnostics.capacity_blocker,
        Some(ContextFocusCapacityBlocker::CandidateFanoutLimit)
    );

    let mut excluded = context_limit_request(1_000);
    excluded.task = "change buried_focus_target".into();
    excluded.focus_paths = vec!["focus/owner_0.rs".into()];
    excluded.exclude_paths = vec!["focus/owner_0.rs".into()];
    excluded.strict_focus_paths = true;
    excluded.verbose_diagnostics = true;
    let excluded = services
        .context(excluded)
        .await
        .expect("policy-empty focus scope");
    assert!(excluded.fragments.is_empty());
    assert_eq!(excluded.coverage.focus_path_coverage[0].indexed_paths, 1);
    assert_eq!(
        excluded.coverage.focus_path_coverage[0]
            .diagnostics
            .as_ref()
            .expect("excluded focus diagnostics")
            .capacity_blocker,
        Some(ContextFocusCapacityBlocker::PathPolicy)
    );
    assert!(excluded.warnings.iter().any(|warning| {
        warning.contains("focus pattern `focus/owner_0.rs`")
            && warning.contains("no candidate-eligible indexed path")
    }));

    let mut too_many_patterns = context_limit_request(1_000);
    too_many_patterns.task = "change buried_focus_target".into();
    too_many_patterns.focus_paths = (0..33)
        .map(|index| format!("focus/owner_{index}.rs"))
        .collect();
    let error = services
        .context(too_many_patterns)
        .await
        .expect_err("focus pattern fan-out must be bounded");
    assert!(matches!(
        error,
        Error::RequestLimitExceeded {
            field: "focus_paths",
            requested: 33,
            limit: 32
        }
    ));

    let mut excessive_minimum = context_limit_request(1_000);
    excessive_minimum.task = "change buried_focus_target".into();
    excessive_minimum.focus_paths = vec!["focus/**".into()];
    excessive_minimum.minimum_fragments_per_focus_path = Some(9);
    let error = services
        .context(excessive_minimum)
        .await
        .expect_err("per-pattern candidate fan-out must be bounded");
    assert!(matches!(
        error,
        Error::RequestLimitExceeded {
            field: "minimum_fragments_per_focus_path",
            requested: 9,
            limit: 8
        }
    ));
}

#[tokio::test]
async fn five_focus_diagnostics_freeze_plan_and_materialized_capacity_truth() {
    let root = tempfile::tempdir().expect("temporary repository");
    let focus_paths = [
        "src/alpha.rs",
        "src/beta.rs",
        "src/gamma.rs",
        "examples/demo.rs",
        "benchmarks/profile.rs",
    ];
    for (path_index, path) in focus_paths.iter().enumerate() {
        let path = root.path().join(path);
        std::fs::create_dir_all(path.parent().expect("fixture parent"))
            .expect("fixture directory");
        let mut source = String::new();
        for symbol_index in 0..2 {
            source.push_str(&format!(
                "pub fn allocation_owner_{path_index}_{symbol_index}() -> usize {{\n"
            ));
            for line in 0..80 {
                source.push_str(&format!(
                    "    let allocation_owner_value_{symbol_index}_{line:02} = {line}usize;\n"
                ));
            }
            source.push_str(&format!(
                "    allocation_owner_value_{symbol_index}_00\n}}\n\n"
            ));
        }
        std::fs::write(path, source).expect("fixture source");
    }
    let services = Services::open(
        Config::discover(root.path(), Some(root.path().join("index.sqlite"))).expect("config"),
    )
    .expect("services");
    services.index(false).await.expect("index fixture");

    let request = {
        let mut request = context_limit_request(12_000);
        request.task = "review allocation_owner behavior".into();
        request.focus_paths = focus_paths.iter().map(ToString::to_string).collect();
        request.strict_focus_paths = true;
        request.minimum_fragments_per_focus_path = Some(2);
        request.max_fragments = Some(9);
        request.verbose_diagnostics = true;
        request
    };
    let materialized = services
        .context(request.clone())
        .await
        .expect("five-focus materialized context");
    let mut plan_request = request;
    plan_request.plan_only = true;
    let planned = services
        .context(plan_request)
        .await
        .expect("five-focus plan");

    assert_eq!(materialized.fragments.len(), 9);
    assert_eq!(
        materialized
            .coverage
            .focus_path_coverage
            .iter()
            .filter(|focus| focus.satisfied)
            .count(),
        4
    );
    let underfilled = materialized
        .coverage
        .focus_path_coverage
        .iter()
        .find(|focus| !focus.satisfied)
        .expect("one focus must expose the hard capacity shortfall");
    let diagnostics = underfilled
        .diagnostics
        .as_ref()
        .expect("underfilled focus diagnostics");
    assert!(diagnostics.generated_fragments >= 2);
    assert_eq!(diagnostics.reserved_fragments, 1);
    assert_eq!(
        diagnostics.capacity_blocker,
        Some(ContextFocusCapacityBlocker::MaxFragments)
    );
    assert!(
        diagnostics
            .suppressed_by
            .iter()
            .any(|suppression| suppression.boundary
                == ContextFocusSuppressionBoundary::MaxFragments)
    );
    for focus in &materialized.coverage.focus_path_coverage {
        let diagnostics = focus
            .diagnostics
            .as_ref()
            .expect("materialized focus diagnostics");
        assert!(diagnostics.generated_fragments >= 2);
        assert_eq!(
            diagnostics.reserved_fragments,
            focus.selected_fragments.min(focus.minimum_fragments)
        );
        assert_eq!(
            diagnostics.selected_source_tokens,
            materialized
                .fragments
                .iter()
                .filter(|fragment| fragment.path == focus.pattern)
                .map(|fragment| fragment.token_count)
                .sum::<usize>()
        );
    }
    assert_eq!(
        planned.coverage.focus_path_coverage,
        materialized.coverage.focus_path_coverage
    );
    assert_eq!(
        planned
            .plan
            .expect("five-focus query plan")
            .candidates
            .iter()
            .map(|candidate| (&candidate.path, candidate.start_line, candidate.end_line))
            .collect::<Vec<_>>(),
        materialized
            .fragments
            .iter()
            .map(|fragment| (&fragment.path, fragment.start_line, fragment.end_line))
            .collect::<Vec<_>>()
    );
}

#[tokio::test]
async fn exact_focus_symbols_satisfy_multi_fragment_minimum_after_reconciliation() {
    let root = tempfile::tempdir().expect("temporary repository");
    std::fs::create_dir_all(root.path().join("focus")).expect("focus directory");
    let source_path = root.path().join("focus/owner.rs");
    std::fs::write(&source_path, "pub fn placeholder() {}\n").expect("initial source");
    let services = Services::open(
        Config::discover(root.path(), Some(root.path().join("index.sqlite"))).expect("config"),
    )
    .expect("services");
    services.index(false).await.expect("initial index");

    let focus_symbols = ["focus_alpha", "focus_beta", "focus_gamma"];
    let mut updated_source = String::new();
    for symbol in focus_symbols {
        updated_source.push_str(&format!("pub fn {symbol}() -> usize {{\n"));
        for index in 0..80 {
            updated_source.push_str(&format!("    let value_{index:02} = {index}usize;\n"));
        }
        updated_source.push_str("    value_00\n}\n\n");
    }
    std::fs::write(&source_path, updated_source).expect("updated source");

    let request = {
        let mut request = context_limit_request(4_000);
        request.task = "perform the requested audit".into();
        request.focus_paths = vec!["focus/owner.rs".into()];
        request.focus_symbols = focus_symbols.into_iter().map(str::to_owned).collect();
        request.strict_focus_paths = true;
        request.minimum_fragments_per_focus_path = Some(2);
        request.max_fragments = Some(3);
        request
    };
    let mut plan_request = request.clone();
    plan_request.plan_only = true;
    plan_request.verbose_diagnostics = true;
    let preview = services
        .context_with_consistency_cancellable(
            plan_request,
            IndexConsistency::ReconcileWorkingTree,
            CancellationToken::new(),
        )
        .await
        .expect("reconciled focus plan");
    let plan = preview.plan.expect("plan");

    assert!(plan.focus_coverage[0].satisfied);
    assert!(plan.focus_coverage[0].candidate_fragments >= 2);
    let plan_diagnostics = preview.coverage.focus_path_coverage[0]
        .diagnostics
        .as_ref()
        .expect("plan focus diagnostics");
    assert!(plan_diagnostics.generated_fragments >= 2);
    assert!(plan_diagnostics.generated_symbol_fragments >= 2);
    assert_eq!(plan_diagnostics.reserved_fragments, 2);
    assert!(plan_diagnostics.selected_source_tokens > 0);
    assert!(
        plan.candidates
            .iter()
            .filter(|candidate| candidate.representation == "focus_symbol")
            .count()
            >= 2
    );

    let mut materialized_request = request.clone();
    materialized_request.verbose_diagnostics = true;
    let first = services
        .context(materialized_request)
        .await
        .expect("focused context");
    let second = services
        .context(request)
        .await
        .expect("deterministic focused context");
    assert!(first.coverage.focus_path_coverage[0].satisfied);
    assert!(first.coverage.focus_path_coverage[0].selected_fragments >= 2);
    assert!(first.meta.source_tokens <= 4_000);
    assert_eq!(
        first
            .fragments
            .iter()
            .map(|fragment| (
                fragment.path.as_str(),
                fragment.start_line,
                fragment.end_line,
                fragment.content_hash.as_str(),
            ))
            .collect::<Vec<_>>(),
        second
            .fragments
            .iter()
            .map(|fragment| (
                fragment.path.as_str(),
                fragment.start_line,
                fragment.end_line,
                fragment.content_hash.as_str(),
            ))
            .collect::<Vec<_>>()
    );
}

#[tokio::test]
async fn strict_changed_paths_are_a_hard_boundary_and_intersect_focus_scope() {
    let root = tempfile::tempdir().expect("temporary repository");
    std::fs::create_dir_all(root.path().join("src")).expect("source directory");
    std::fs::create_dir_all(root.path().join("artifacts")).expect("artifact directory");
    std::fs::write(
        root.path().join("src/active.rs"),
        "pub fn strict_changed_target_active() -> bool { true }\n",
    )
    .expect("active source");
    std::fs::write(
        root.path().join("artifacts/report.rs"),
        "pub fn strict_changed_target_report() -> bool { false }\n",
    )
    .expect("artifact source");
    let services = Services::open(
        Config::discover(root.path(), Some(root.path().join("index.sqlite"))).expect("config"),
    )
    .expect("services");
    services.index(false).await.expect("index fixture");

    let mut request = context_limit_request(500);
    request.task = "change strict_changed_target".into();
    request.changed_paths = vec!["src/active.rs".into()];
    request.strict_changed_paths = true;
    let response = services.context(request).await.expect("strict changed scope");

    assert!(!response.fragments.is_empty());
    assert!(
        response
            .fragments
            .iter()
            .all(|fragment| fragment.path == "src/active.rs")
    );
    assert_eq!(response.coverage.strict_scope_satisfied, Some(true));
    let changed = response
        .coverage
        .changed_path_coverage
        .expect("changed path coverage");
    assert_eq!(changed.resolved_paths, 1);
    assert_eq!(changed.indexed_paths, 1);
    assert!(changed.selected_fragments > 0);
    assert!(changed.satisfied);

    let mut intersection = context_limit_request(500);
    intersection.task = "change strict_changed_target".into();
    intersection.focus_paths = vec!["artifacts/**".into()];
    intersection.strict_focus_paths = true;
    intersection.changed_paths = vec!["src/active.rs".into()];
    intersection.strict_changed_paths = true;
    let intersection = services
        .context(intersection)
        .await
        .expect("intersected hard scopes");
    assert!(intersection.fragments.is_empty());
    assert_eq!(
        intersection.coverage.strict_scope_satisfied,
        Some(false)
    );
    assert!(
        !intersection
            .coverage
            .focus_path_coverage
            .first()
            .expect("focus coverage")
            .satisfied
    );
    assert!(
        !intersection
            .coverage
            .changed_path_coverage
            .as_ref()
            .expect("changed coverage")
            .satisfied
    );

    let mut missing = context_limit_request(500);
    missing.task = "change strict_changed_target".into();
    missing.changed_paths = vec!["src/missing.rs".into()];
    missing.strict_changed_paths = true;
    let missing = services
        .context(missing)
        .await
        .expect("missing changed scope");
    assert!(missing.fragments.is_empty());
    assert_eq!(missing.coverage.strict_scope_satisfied, Some(false));
    let changed = missing
        .coverage
        .changed_path_coverage
        .expect("missing changed coverage");
    assert_eq!(changed.resolved_paths, 1);
    assert_eq!(changed.indexed_paths, 0);
    assert_eq!(changed.selected_fragments, 0);
    assert!(!changed.satisfied);
    assert!(missing.warnings.iter().any(|warning| {
        warning.contains("strict changed-path scope produced no indexed selected evidence")
    }));
}

#[tokio::test]
async fn context_must_cover_generates_evidence_and_reports_unmatched_constraints() {
    let root = tempfile::tempdir().expect("temporary repository");
    std::fs::create_dir_all(root.path().join("src")).expect("source directory");
    std::fs::write(
        root.path().join("src/required.rs"),
        "pub fn required_symbol() -> bool { true }\n",
    )
    .expect("required source");
    std::fs::write(
        root.path().join("src/general.rs"),
        "pub fn unrelated_symbol() -> bool { false }\n",
    )
    .expect("general source");
    let config =
        Config::discover(root.path(), Some(root.path().join("index.sqlite"))).expect("config");
    let services = Services::open(config).expect("services");
    services.index(false).await.expect("index fixture");
    let mut request = context_limit_request(300);
    request.task = "investigate a different subsystem".into();
    request.include_paths = vec!["src/**".into(), "absent/**".into()];
    request.focus_paths = vec!["missing-focus/**".into()];
    request.focus_symbols = vec!["missing_focus_symbol".into(), "required_symbol".into()];
    request.must_include_paths = vec!["src/required.rs".into(), "src/missing.rs".into()];
    request.must_include_symbols = vec!["required_symbol".into(), "missing_symbol".into()];
    request.max_fragments = Some(2);

    let evaluation = services
        .context_evaluation(request.clone())
        .await
        .expect("must-cover evaluation");
    let repeated = services
        .context_evaluation(request.clone())
        .await
        .expect("repeated must-cover evaluation");
    let response = services
        .context(request.clone())
        .await
        .expect("must-cover context");

    assert!(
        response
            .fragments
            .iter()
            .any(|fragment| fragment.path == "src/required.rs")
    );
    let required_fragment = response
        .fragments
        .iter()
        .find(|fragment| fragment.representation == "required_symbol")
        .expect("required symbol fragment");
    assert_eq!(required_fragment.target_start_line, Some(1));
    assert_eq!(required_fragment.target_end_line, Some(1));
    assert!(!required_fragment.truncated);
    assert_eq!(
        response.coverage.covered_must_include_paths,
        vec!["src/required.rs"]
    );
    assert_eq!(
        response.coverage.covered_must_include_symbols,
        vec!["required_symbol"]
    );
    assert_eq!(
        response.coverage.unmatched_must_include_paths,
        vec!["src/missing.rs"]
    );
    assert_eq!(
        response.coverage.unmatched_must_include_symbols,
        vec!["missing_symbol"]
    );
    assert_eq!(
        response.coverage.unmatched_include_paths,
        vec!["absent/**"]
    );
    assert_eq!(
        response.coverage.unmatched_focus_paths,
        vec!["missing-focus/**"]
    );
    assert_eq!(
        response.coverage.unmatched_focus_symbols,
        vec!["missing_focus_symbol"]
    );
    assert!(response.coverage.uncovered_must_include_paths.is_empty());
    assert!(response.coverage.uncovered_must_include_symbols.is_empty());
    assert_eq!(evaluation.phases.exact_symbol_names, 3);
    assert_eq!(evaluation.phases.exact_symbol_batches, 1);
    assert_eq!(evaluation.phases.exact_symbol_hits, 1);
    assert!(
        evaluation.phases.unique_adaptive_excerpt_requests
            <= evaluation.phases.adaptive_excerpt_requests
    );
    assert!(!evaluation.primitive_keys.is_empty());
    assert_eq!(evaluation.primitive_keys, repeated.primitive_keys);

    let mut batched_request = context_limit_request(300);
    batched_request.task = "investigate a different subsystem".into();
    batched_request.focus_symbols = (0..33)
        .map(|index| format!("missing_symbol_{index:02}"))
        .collect();
    let batched = services
        .context_evaluation(batched_request)
        .await
        .expect("bounded exact-symbol batches");
    assert_eq!(batched.phases.exact_symbol_names, 33);
    assert_eq!(batched.phases.exact_symbol_batches, 2);
    assert_eq!(batched.phases.exact_symbol_hits, 0);

    std::fs::write(
        root.path().join("src/required.rs"),
        "pub fn required_symbol() -> bool { false }\n",
    )
    .expect("change required source");
    services
        .index_paths(vec!["src/required.rs".into()])
        .await
        .expect("advance generation");
    let next_generation = services
        .context_evaluation(request)
        .await
        .expect("next-generation evaluation");
    let previous_keys = evaluation
        .primitive_keys
        .iter()
        .map(|key| key.key_blake3.as_str())
        .collect::<std::collections::HashSet<_>>();
    assert!(
        next_generation
            .primitive_keys
            .iter()
            .all(|key| !previous_keys.contains(key.key_blake3.as_str()))
    );
}

#[tokio::test]
async fn context_required_evidence_covers_doq_ranges_and_rejects_intro_fallbacks() {
    fn document(lines: usize, evidence_line: usize, evidence: &str) -> String {
        (1..=lines)
            .map(|line| {
                if line == evidence_line {
                    evidence.to_owned()
                } else {
                    format!(
                        "Background narrative line {line} records ordinary setup material without the required finding."
                    )
                }
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    let root = tempfile::tempdir().expect("temporary repository");
    let latex = root.path().join("paper/latex");
    std::fs::create_dir_all(&latex).expect("latex directory");
    let fixtures = [
        (
            "3-study-overview.tex",
            223,
            "THREAT_ORDERING_SENTINEL establishes the threat-model ordering criterion.",
        ),
        (
            "4-failure-families.tex",
            251,
            "F4P_CASE_SENTINEL bounds the representative failure-family claim.",
        ),
        (
            "5-cross-implementation-evaluation.tex",
            91,
            "CROSS_IMPLEMENTATION_SENTINEL records the parity evidence.",
        ),
        (
            "7-discussion-limitations.tex",
            59,
            "DISCUSSION_LIMIT_SENTINEL states the measured limitation.",
        ),
        (
            "appendix-chains.tex",
            147,
            "QLOG_COMMIT_SENTINEL binds the qlog observation to the commit.",
        ),
        (
            "appendix-disclosure-registry.tex",
            40,
            "DISCLOSURE_SENTINEL records the disclosure status.",
        ),
    ];
    for (name, line, evidence) in fixtures {
        std::fs::write(latex.join(name), document(line + 40, line, evidence))
            .expect("write DoQ-shaped fixture");
    }
    let config =
        Config::discover(root.path(), Some(root.path().join("index.sqlite"))).expect("config");
    let services = Services::open(config).expect("services");
    services.index(false).await.expect("index fixture");

    let mut request = context_limit_request(5_000);
    request.task =
        "Retrieve the exact threat, F4-P, cross-implementation, limitation, qlog, and disclosure evidence."
            .into();
    request.max_fragments = Some(fixtures.len());
    for (name, _, evidence) in fixtures {
        let path = format!("paper/latex/{name}");
        request.must_include_paths.push(path.clone());
        request.required_evidence.push(ContextRequiredEvidence {
            path,
            queries: vec![evidence.split_whitespace().next().expect("sentinel").into()],
            minimum_query_matches: 1,
        });
    }

    let response = services
        .context(request)
        .await
        .expect("required evidence context");

    assert_eq!(
        response.coverage.evidence_scope_satisfied,
        Some(true),
        "{:#?}",
        response.coverage.required_evidence
    );
    assert!(
        response
            .coverage
            .required_evidence
            .iter()
            .all(|coverage| coverage.satisfied && coverage.selected_fragments > 0)
    );
    for (name, line, evidence) in fixtures {
        let sentinel = evidence.split_whitespace().next().expect("sentinel");
        let fragment = response
            .fragments
            .iter()
            .find(|fragment| fragment.path == format!("paper/latex/{name}"))
            .expect("path-scoped evidence");
        assert!(fragment.start_line <= line && fragment.end_line >= line);
        assert!(fragment.content.contains(sentinel));
        if line > 40 {
            assert!(
                fragment.start_line > 1,
                "intro excerpt must not satisfy evidence for {name}"
            );
        }
    }

    let mut missing = context_limit_request(1_500);
    missing.task = "Find ABSENT_EVIDENCE_SENTINEL.".into();
    missing.must_include_paths = vec!["paper/latex/3-study-overview.tex".into()];
    missing.required_evidence = vec![ContextRequiredEvidence {
        path: "paper/latex/3-study-overview.tex".into(),
        queries: vec!["ABSENT_EVIDENCE_SENTINEL".into()],
        minimum_query_matches: 1,
    }];
    missing.max_fragments = Some(1);
    let missing = services
        .context(missing)
        .await
        .expect("unsatisfied evidence context");

    assert_eq!(missing.coverage.evidence_scope_satisfied, Some(false));
    assert!(!missing.coverage.required_evidence[0].satisfied);
    assert!(missing.coverage.required_evidence[0].matched_queries.is_empty());
    assert_eq!(
        missing.fragments[0].representation,
        "required_path_fallback"
    );
    assert_eq!(missing.fragments[0].start_line, 1);
    assert!(missing.fragments[0].end_line <= 40);

    let mut invalid = context_limit_request(500);
    invalid.required_evidence = vec![ContextRequiredEvidence {
        path: "paper/latex/3-study-overview.tex".into(),
        queries: vec!["one".into()],
        minimum_query_matches: 2,
    }];
    assert!(matches!(
        services.context(invalid).await,
        Err(Error::InvalidInput {
            field: "required_evidence minimum_query_matches",
            ..
        })
    ));
}

#[tokio::test]
async fn context_marks_partial_required_symbols_without_claiming_complete_coverage() {
    let root = tempfile::tempdir().expect("temporary repository");
    std::fs::create_dir_all(root.path().join("src")).expect("source directory");
    let mut source = String::from("pub fn required_long_symbol() -> usize {\n");
    for index in 0..160 {
        source.push_str(&format!("    let value_{index} = {index};\n"));
    }
    source.push_str("    value_159\n}\n");
    std::fs::write(root.path().join("src/required.rs"), source).expect("required source");
    let config =
        Config::discover(root.path(), Some(root.path().join("index.sqlite"))).expect("config");
    let services = Services::open(config).expect("services");
    services.index(false).await.expect("index fixture");
    let mut request = context_limit_request(300);
    request.task = "inspect required_long_symbol".into();
    request.must_include_symbols = vec!["required_long_symbol".into()];
    request.max_fragments = Some(1);

    let response = services
        .context(request.clone())
        .await
        .expect("partial required symbol context");

    let fragment = response.fragments.first().expect("partial fragment");
    assert_eq!(fragment.representation, "required_symbol");
    assert_eq!(fragment.target_start_line, Some(1));
    assert_eq!(fragment.target_end_line, Some(163));
    assert_eq!(fragment.start_line, 1);
    assert!(fragment.end_line < 163);
    assert!(fragment.truncated);
    assert!(response.coverage.covered_must_include_symbols.is_empty());
    assert_eq!(
        response.coverage.partial_must_include_symbols,
        vec!["required_long_symbol"]
    );
    assert!(response.coverage.uncovered_must_include_symbols.is_empty());
    assert!(
        response
            .warnings
            .iter()
            .any(|warning| warning.contains("required symbol was returned only partially"))
    );

    request.plan_only = true;
    let plan_response = services
        .context(request)
        .await
        .expect("partial required symbol plan");
    assert!(plan_response.fragments.is_empty());
    let candidate = plan_response
        .plan
        .as_ref()
        .and_then(|plan| plan.candidates.first())
        .expect("partial plan candidate");
    assert_eq!(candidate.target_start_line, Some(1));
    assert_eq!(candidate.target_end_line, Some(163));
    assert!(candidate.end_line < 163);
    assert!(candidate.truncated);
    assert_eq!(
        plan_response.coverage.partial_must_include_symbols,
        vec!["required_long_symbol"]
    );

    let mut full_request = context_limit_request(2_000);
    full_request.task = "inspect required_long_symbol".into();
    full_request.must_include_symbols = vec!["required_long_symbol".into()];
    full_request.max_fragments = Some(1);
    let full_response = services
        .context(full_request)
        .await
        .expect("complete required symbol context");
    let full_fragment = full_response.fragments.first().expect("complete fragment");
    assert_eq!(full_fragment.start_line, 1);
    assert_eq!(full_fragment.end_line, 163);
    assert_eq!(full_fragment.target_start_line, Some(1));
    assert_eq!(full_fragment.target_end_line, Some(163));
    assert!(!full_fragment.truncated);
    assert_eq!(
        full_response.coverage.covered_must_include_symbols,
        vec!["required_long_symbol"]
    );
    assert!(
        full_response
            .coverage
            .partial_must_include_symbols
            .is_empty()
    );
}

#[tokio::test]
async fn oversized_context_reports_bounded_routing_with_reconcile_working_tree_retries() {
    let (_root, services) = fixture().await;
    let changed_paths = (0..12)
        .flat_map(|index| {
            [
                format!("src/browser/file_{index}.rs"),
                format!("src/runtime/file_{index}.rs"),
                format!("tests/scenario_{index}.rs"),
            ]
        })
        .collect();
    let mut request = context_limit_request(200);
    request.changed_paths = changed_paths;

    let response = services
        .context_with_workflow_consistency_cancellable(
            request,
            ContextWorkflow::Review,
            IndexConsistency::ReconcileWorkingTree,
            tokio_util::sync::CancellationToken::new(),
        )
        .await
        .expect("oversized context");
    let routing = response.routing.expect("routing receipt");

    assert_eq!(routing.changed_paths, 36);
    assert_eq!(routing.path_groups_total, 3);
    assert!(routing.path_groups.len() <= 5);
    assert!(routing.suggestions.len() <= 3);
    assert!(
        routing.consistency == IndexConsistency::ReconcileWorkingTree
    );
    assert!(
        response
            .warnings
            .iter()
            .any(|warning| warning.contains("36 changed paths across 3 path groups"))
    );
}
