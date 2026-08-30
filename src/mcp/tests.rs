use rmcp::{serve_client, serve_server};

use super::*;
use crate::SearchOccurrenceOutput;

#[test]
fn request_admission_has_an_exact_fail_fast_boundary() {
    let (server, _) = LeanTokenMcp::pending();
    let permits = (0..DEFAULT_ACTIVE_TOOL_CALL_CAPACITY)
        .map(|_| {
            server
                .request_admission
                .try_admit()
                .expect("admitted tool call")
        })
        .collect::<Vec<_>>();
    assert_eq!(server.request_admission.available_permits(), 0);
    assert!(matches!(
        server.request_admission.try_admit(),
        Err(crate::Error::RetrievalOverloaded)
    ));
    drop(permits);
    assert_eq!(
        server.request_admission.available_permits(),
        DEFAULT_ACTIVE_TOOL_CALL_CAPACITY
    );
}

#[test]
fn repository_context_registry_defaults_and_fails_closed() {
    let primary = McpServices::starting_default();
    let registry = McpContextRegistry::primary(primary.clone());
    assert!(registry.resolve(None).is_ok());
    assert!(registry.resolve(Some("default")).is_ok());
    assert!(matches!(
        registry.resolve(Some("unapproved")),
        Err(crate::Error::InvalidInput {
            field: "repository_context",
            ..
        })
    ));

    registry
        .register("docs".into(), McpServices::starting_default())
        .expect("valid context name");
    assert!(registry.resolve(Some("docs")).is_ok());
}

#[test]
fn repository_context_registry_allows_the_configured_approved_context_limit() {
    let registry = McpContextRegistry::primary(McpServices::starting_default());
    for index in 0..MAX_REPOSITORY_CONTEXTS {
        registry
            .register(format!("context-{index}"), McpServices::starting_default())
            .expect("configured approved context capacity");
    }

    assert!(matches!(
        registry.register("one-too-many".into(), McpServices::starting_default()),
        Err(crate::Error::RequestLimitExceeded {
            field: "repository_contexts",
            requested,
            limit: MAX_REPOSITORY_CONTEXTS,
        }) if requested == MAX_REPOSITORY_CONTEXTS + 1
    ));
}

#[tokio::test]
async fn selecting_an_approved_context_requests_lazy_activation() {
    let (server, _) = LeanTokenMcp::pending();
    let context = McpServices::starting_default();
    server
        .contexts
        .register("docs".into(), context.clone())
        .expect("approved context");
    assert!(!context.activation_requested());

    let cancellation = CancellationToken::new();
    let call_cancellation = cancellation.clone();
    let call = tokio::spawn(async move {
        server
            .prepare_retrieval_call(call_cancellation, Some("docs"), |_| Ok(()))
            .await
    });
    for _ in 0..100 {
        if context.activation_requested() {
            break;
        }
        tokio::task::yield_now().await;
    }
    assert!(context.activation_requested());
    context
        .wait_for_activation(CancellationToken::new())
        .await
        .expect("activation signal");
    cancellation.cancel();
    let _ = call.await.expect("activation call joins");
}

#[test]
fn approved_context_activation_is_dormant_and_coalesced() {
    let registry = McpContextRegistry::primary(McpServices::starting_default());
    let contexts = (0..MAX_REPOSITORY_CONTEXTS)
        .map(|index| {
            let context = McpServices::starting_default();
            registry
                .register(format!("context-{index}"), context.clone())
                .expect("approved context");
            context
        })
        .collect::<Vec<_>>();
    assert!(
        contexts
            .iter()
            .all(|context| !context.activation_requested())
    );

    let selected = contexts[3].clone();
    let requests = (0..32)
        .map(|_| {
            let selected = selected.clone();
            std::thread::spawn(move || selected.request_activation())
        })
        .collect::<Vec<_>>();
    let first_requests = requests
        .into_iter()
        .map(|request| request.join().expect("activation request"))
        .filter(|first| *first)
        .count();

    assert_eq!(first_requests, 1);
    assert!(selected.activation_requested());
    assert!(
        contexts
            .iter()
            .enumerate()
            .all(|(index, context)| { index == 3 || !context.activation_requested() })
    );
}

#[tokio::test]
async fn prepared_retrieval_selects_the_approved_context() {
    let primary_root = tempfile::tempdir().expect("primary repository");
    let docs_root = tempfile::tempdir().expect("approved repository");
    let primary = Arc::new(
        Services::open(
            Config::discover(
                primary_root.path(),
                Some(primary_root.path().join("index.sqlite")),
            )
            .expect("primary config"),
        )
        .expect("primary services"),
    );
    let docs = Arc::new(
        Services::open(
            Config::discover(
                docs_root.path(),
                Some(docs_root.path().join("index.sqlite")),
            )
            .expect("approved config"),
        )
        .expect("approved services"),
    );
    let expected_id = docs.repository_id();
    let server = LeanTokenMcp::new(primary);
    server
        .contexts
        .register("docs".into(), McpServices::ready(docs))
        .expect("valid context name");

    let prepared = server
        .prepare_retrieval_call(CancellationToken::new(), Some("docs"), |_| Ok(()))
        .await
        .expect("approved context selection");
    let RetrievalPreparation::Ready(prepared) = prepared else {
        panic!("approved context should be ready");
    };
    assert_eq!(prepared.services.repository_id(), expected_id);
}

#[tokio::test]
async fn receipt_resource_lookup_requests_dormant_context_activation() {
    let root = tempfile::tempdir().expect("primary repository");
    let primary = Arc::new(
        Services::open(
            Config::discover(root.path(), Some(root.path().join("index.sqlite")))
                .expect("primary config"),
        )
        .expect("primary services"),
    );
    let server = LeanTokenMcp::new(primary);
    let context = McpServices::starting_default();
    server
        .contexts
        .register("docs".into(), context.clone())
        .expect("approved context");

    let call = tokio::spawn({
        let server = server.clone();
        async move {
            server
                .read_receipt_resource(
                    "leantoken://receipt/v1/r0123456789abcdef0123456789abcdef0123456789abcdef"
                        .into(),
                    None,
                )
                .await
        }
    });
    for _ in 0..100 {
        if context.activation_requested() {
            break;
        }
        tokio::task::yield_now().await;
    }
    assert!(context.activation_requested());
    context.set_failed(&crate::Error::OperationFailure(
        "test startup failure".into(),
    ));
    let error = call
        .await
        .expect("receipt lookup joins")
        .expect_err("unknown receipt");
    assert_eq!(error.code, rmcp::model::ErrorCode::RESOURCE_NOT_FOUND);
}

#[test]
fn cloned_servers_share_admission_but_separate_instances_do_not() {
    let (server, _) = LeanTokenMcp::pending();
    let clone = server.clone();
    let permits = (0..DEFAULT_ACTIVE_TOOL_CALL_CAPACITY)
        .map(|_| {
            clone
                .request_admission
                .try_admit()
                .expect("admitted clone tool call")
        })
        .collect::<Vec<_>>();

    assert!(matches!(
        server.request_admission.try_admit(),
        Err(crate::Error::RetrievalOverloaded)
    ));

    let (independent, _) = LeanTokenMcp::pending();
    let independent_permit = independent
        .request_admission
        .try_admit()
        .expect("separate server has independent capacity");
    drop(independent_permit);
    drop(permits);
}

#[test]
fn retryable_service_errors_are_structured_successful_tool_results() {
    let (server, _) = LeanTokenMcp::pending();
    for (error, reason) in [
        (
            crate::Error::RetrievalOverloaded,
            "retrieval_capacity_exhausted",
        ),
        (
            crate::Error::RetrievalQueueTimeout,
            "retrieval_queue_timeout",
        ),
        (crate::Error::IndexNotReady, "index_building"),
    ] {
        let result = server
            .service_result::<()>(Err(error))
            .expect("capacity result");
        let structured = result.structured_content.expect("structured result");
        assert_eq!(structured["status"], "retryable");
        assert_eq!(structured["reason"], reason);
        assert_eq!(structured["retry_after_ms"], 500);
        assert_eq!(result.is_error, Some(false));
    }
}

#[test]
fn detailed_index_progress_retry_payload_stays_bounded() {
    let response = RetryableToolResponse::new(
        "index_building",
        "repository index is being built; retry the same call shortly",
        500,
    )
    .with_index_progress(Some(IndexProgressSnapshot {
        cache_namespace: "ffffffffffffffffffffffffffffffff".into(),
        detail_available: true,
        active: true,
        current_generation: 0,
        attempt_id: Some("ffffffffffffffffffffffffffffffff".into()),
        phase: Some(crate::model::IndexProgressPhase::ReferenceFts),
        started_unix_ms: Some(u64::MAX),
        elapsed_ms: Some(u64::MAX),
        last_progress_unix_ms: Some(u64::MAX),
        update_sequence: Some(u64::MAX),
        walk_entries: Some(u64::MAX),
        files_discovered: Some(u64::MAX),
        discovered_source_bytes: Some(u64::MAX),
        files_prepared: Some(u64::MAX),
        files_staged: Some(u64::MAX),
        preparation_batches: Some(u64::MAX),
    }));
    let wire = serde_json::to_string(&response).expect("serialize retry payload");
    let tokens = crate::tokens::Tokenizer::Cl100kBase.count(&wire);

    assert!(
        tokens <= 256,
        "detailed retry payload must remain at most 256 cl100k tokens, observed {tokens}: {wire}"
    );
}

#[tokio::test]
async fn initialization_and_tool_listing_bypass_saturated_tool_admission() {
    let (server, _) = LeanTokenMcp::pending();
    let permits = (0..DEFAULT_ACTIVE_TOOL_CALL_CAPACITY)
        .map(|_| {
            server
                .request_admission
                .try_admit()
                .expect("saturate tool admission")
        })
        .collect::<Vec<_>>();
    let (client_stream, server_stream) = tokio::io::duplex(64 * 1024);
    let server_start = tokio::spawn(async move {
        serve_server(server, server_stream)
            .await
            .expect("start server")
    });
    let mut client = serve_client((), client_stream)
        .await
        .expect("initialize client while saturated");
    let mut server = server_start.await.expect("join server startup");

    assert_eq!(
        client
            .peer()
            .peer_info()
            .expect("initialize response")
            .server_info
            .as_ref()
            .expect("server identity")
            .name,
        "leantoken"
    );
    assert_eq!(
        client
            .peer()
            .peer_info()
            .expect("initialize response")
            .server_info
            .as_ref()
            .expect("server identity")
            .version,
        mcp_runtime_version()
    );
    assert_eq!(mcp_schema_fingerprint().len(), 32);
    assert_eq!(
        client
            .peer()
            .list_all_tools()
            .await
            .expect("list tools while saturated")
            .len(),
        9
    );

    drop(permits);
    client.close().await.expect("close client");
    server.close().await.expect("close server");
}

#[tokio::test]
async fn repository_identity_is_checked_before_tool_admission() {
    let root = tempfile::tempdir().expect("root");
    let config =
        Config::discover(root.path(), Some(root.path().join("index.sqlite"))).expect("config");
    let services = Arc::new(Services::open(config).expect("services"));
    let server = LeanTokenMcp::new(Arc::clone(&services));
    let permits = (0..DEFAULT_ACTIVE_TOOL_CALL_CAPACITY)
        .map(|_| {
            server
                .request_admission
                .try_admit()
                .expect("saturate tool admission")
        })
        .collect::<Vec<_>>();
    let called = Arc::new(AtomicBool::new(false));
    let operation_called = Arc::clone(&called);

    let error = server
        .run_admitted::<(), _, _>(
            services,
            Some("not-this-repository".into()),
            move |_| async move {
                operation_called.store(true, Ordering::SeqCst);
                Ok(())
            },
        )
        .await
        .expect("identity mismatch must be returned before overload");

    assert_eq!(
        error.structured_content,
        Some(serde_json::json!({
            "category": "repository_identity_mismatch",
            "expected_repository_id": "not-this-repository",
            "actual_repository_id": server.services(&server.services.get())
                .expect("ready services")
                .repository_id(),
            "message": "repository identity does not match this server",
            "status": "error",
        }))
    );
    assert_eq!(error.is_error, Some(true));
    assert!(!called.load(Ordering::SeqCst));
    drop(permits);
}

#[tokio::test]
async fn savings_is_covered_by_protocol_admission() {
    let root = tempfile::tempdir().expect("root");
    let config =
        Config::discover(root.path(), Some(root.path().join("index.sqlite"))).expect("config");
    let services = Arc::new(Services::open(config).expect("services"));
    let server = LeanTokenMcp::new(services);
    let permits = (0..DEFAULT_ACTIVE_TOOL_CALL_CAPACITY)
        .map(|_| {
            server
                .request_admission
                .try_admit()
                .expect("saturate tool admission")
        })
        .collect::<Vec<_>>();

    let result = server
        .leantoken_savings(Parameters(SavingsMcpRequest {
            repository_context: None,
            snapshot: None,
        }))
        .await
        .expect("retryable savings response");
    assert_eq!(
        result
            .structured_content
            .as_ref()
            .and_then(|value| value["reason"].as_str()),
        Some("retrieval_capacity_exhausted")
    );
    drop(permits);
}

#[test]
fn startup_failures_expose_only_allowlisted_guidance() {
    let marker = "/secret/repository";
    let failure = StartupFailure::from_error(&crate::Error::UnsafeRepositoryRoot(marker.into()));
    for mode in [
        McpResultMode::Dual,
        McpResultMode::Text,
        McpResultMode::Structured,
    ] {
        let result = tool_unavailable(failure.reason, failure.message, mode);
        assert_eq!(result.is_error, Some(true));
        assert_eq!(result.content.is_empty(), mode == McpResultMode::Structured);
        assert_eq!(
            result.structured_content.is_some(),
            mode != McpResultMode::Text
        );
        assert!(
            serde_json::to_string(&result)
                .unwrap()
                .contains("unsafe_repository_root")
        );
        assert!(!serde_json::to_string(&result).unwrap().contains(marker));
    }
}

#[test]
fn user_docs_list_the_exact_runtime_tool_catalog() {
    let expected = LeanTokenMcp::tool_router()
        .list_all()
        .into_iter()
        .map(|tool| format!("leantoken.{}", tool.name))
        .collect::<std::collections::BTreeSet<_>>();

    let readme = include_str!("../../README.md");
    let readme_tools = readme
        .split_once("## Available tools")
        .expect("README tool section")
        .1
        .split_once("## CLI usage")
        .expect("README tool section end")
        .0
        .lines()
        .filter_map(|line| line.strip_prefix("| `"))
        .filter_map(|line| line.split_once('`').map(|(name, _)| name.to_owned()))
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(readme_tools, expected, "README tool table drifted");

    let usage_tools = include_str!("../../docs/usage.md")
        .lines()
        .filter_map(|line| line.strip_prefix("## `"))
        .filter_map(|line| line.strip_suffix('`'))
        .filter(|name| name.starts_with("leantoken."))
        .map(str::to_owned)
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(usage_tools, expected, "usage guide tool sections drifted");
}

#[test]
fn tools_have_input_schemas_without_redundant_output_schemas() {
    let router = LeanTokenMcp::tool_router();
    let tools = router.list_all();
    for tool in tools {
        assert!(
            !tool.input_schema.is_empty(),
            "{} input_schema is empty",
            tool.name
        );
        assert!(
            tool.output_schema.is_none(),
            "{} output_schema adds catalog tokens despite structured results",
            tool.name
        );
    }
}

#[test]
fn mcp_contract_snapshot() {
    insta::assert_json_snapshot!(mcp_contract());
}

#[test]
fn result_modes_emit_only_the_selected_representations() {
    let value = serde_json::json!({"answer": 42});
    let dual = tool_result(value.clone(), McpResultMode::Dual).expect("dual");
    let text = tool_result(value.clone(), McpResultMode::Text).expect("text");
    let structured = tool_result(value, McpResultMode::Structured).expect("structured");

    assert!(!dual.content.is_empty());
    assert!(dual.structured_content.is_some());
    assert!(!text.content.is_empty());
    assert!(text.structured_content.is_none());
    assert!(structured.content.is_empty());
    assert!(structured.structured_content.is_some());
    for result in [&dual, &text, &structured] {
        assert_eq!(result.result_type, Some(rmcp::model::ResultType::COMPLETE));
        assert_eq!(result.is_error, Some(false));
    }
}

#[test]
fn semantic_failures_use_native_model_visible_tool_errors() {
    for mode in [
        McpResultMode::Dual,
        McpResultMode::Text,
        McpResultMode::Structured,
    ] {
        let result = into_tool_error(
            crate::Error::InputTooLong {
                field: "search query",
                max_bytes: 64,
            },
            mode,
        )
        .expect("semantic failure should be a tool result");
        assert_eq!(result.result_type, Some(rmcp::model::ResultType::COMPLETE));
        assert_eq!(result.is_error, Some(true));
        assert_eq!(result.content.is_empty(), mode == McpResultMode::Structured);
        assert_eq!(
            result.structured_content.is_some(),
            mode != McpResultMode::Text
        );
        assert!(
            serde_json::to_string(&result)
                .unwrap()
                .contains("input_too_long")
        );
    }

    let internal = into_tool_error(
        crate::Error::OperationFailure("failed".into()),
        McpResultMode::Dual,
    )
    .expect_err("internal failures remain protocol errors");
    assert_eq!(internal.code, rmcp::model::ErrorCode::INTERNAL_ERROR);
}

#[test]
fn structured_receipt_results_preserve_evidence_without_repeated_visible_handoff() {
    let receipt_id = "r0123456789abcdef0123456789abcdef0123456789abcdef";
    let value = serde_json::json!({
        "meta": {
            "receipt_id": receipt_id,
            "source_tokens": 6,
            "protocol_tokens": 0,
            "path_and_metadata_tokens": 0,
            "total_response_tokens": 0,
            "tokenizer": "cl100k_base"
        },
        "fragments": [{
            "path": "lib.rs",
            "content": "fn ready() {}"
        }]
    });
    let result = tool_result(value, McpResultMode::Structured).expect("receipt result");
    let structured = result
        .structured_content
        .expect("structured receipt result");
    let uri = format!("leantoken://receipt/v1/{receipt_id}");
    assert_eq!(structured["receipt_resource"]["kind"], "retrieval_receipt");
    assert_eq!(structured["receipt_resource"]["id"], receipt_id);
    assert_eq!(structured["receipt_resource"]["uri"], uri);
    assert_eq!(structured["fragments"][0]["path"], "lib.rs");
    assert_eq!(structured["fragments"][0]["content"], "fn ready() {}");
    assert!(result.content.is_empty());
    assert!(
        structured["meta"]["total_response_tokens"]
            .as_u64()
            .is_some_and(|tokens| tokens > 0)
    );

    let without_receipt = tool_result(
        serde_json::json!({"meta": {"receipt_id": null}}),
        McpResultMode::Structured,
    )
    .expect("receipt-free result");
    assert!(without_receipt.content.is_empty());
    assert!(
        without_receipt
            .structured_content
            .expect("structured receipt-free result")
            .get("receipt_resource")
            .is_none()
    );
}

#[test]
fn receipt_decoration_cannot_exceed_the_requested_response_budget() {
    let receipt_id = "r0123456789abcdef0123456789abcdef0123456789abcdef";
    let value = serde_json::json!({
        "meta": {
            "receipt_id": receipt_id,
            "source_tokens": 0,
            "protocol_tokens": 0,
            "path_and_metadata_tokens": 0,
            "total_response_tokens": 0,
            "tokenizer": "cl100k_base"
        }
    });
    let error = tool_result_with_limit(
        value,
        McpResultMode::Structured,
        Some(1),
        Some(&ProtocolVersion::V_2026_07_28),
    )
    .expect_err("receipt decoration must respect the final response budget");
    assert_eq!(error.code, rmcp::model::ErrorCode::INVALID_PARAMS);
    assert_eq!(
        error.data.as_ref().and_then(|data| data.get("limit")),
        Some(&serde_json::json!(1))
    );
    assert!(
        error
            .data
            .as_ref()
            .and_then(|data| data.get("minimum_required_response_tokens"))
            .and_then(serde_json::Value::as_u64)
            .is_some_and(|tokens| tokens > 1)
    );
    assert!(error.data.as_ref().is_some_and(|data| {
        data.pointer("/breakdown/mandatory_response_tokens")
            == data.get("minimum_required_response_tokens")
    }));
}

#[test]
fn response_accounting_matches_the_selected_model_visible_result() {
    for protocol in [ProtocolVersion::V_2025_11_25, ProtocolVersion::V_2026_07_28] {
        for mode in [
            McpResultMode::Structured,
            McpResultMode::Text,
            McpResultMode::Dual,
        ] {
            let result = tool_result_with_limit(
                serde_json::json!({
                "meta": {
                    "source_tokens": 3,
                    "protocol_tokens": 0,
                    "path_and_metadata_tokens": 0,
                    "total_response_tokens": 0,
                    "tokenizer": "cl100k_base"
                },
                "fragments": [{"path": "lib.rs", "content": "fn ready() {}"}]
                }),
                mode,
                None,
                Some(&protocol),
            )
            .expect("accounted result");
            let structured = result.structured_content.clone().unwrap_or_else(|| {
                let text = result
                    .content
                    .first()
                    .and_then(ContentBlock::as_text)
                    .expect("text result");
                serde_json::from_str(&text.text).expect("JSON text result")
            });
            let reported = structured["meta"]["total_response_tokens"]
                .as_u64()
                .expect("reported total") as usize;
            let accounted = [
                "source_tokens",
                "protocol_tokens",
                "path_and_metadata_tokens",
            ]
            .into_iter()
            .map(|field| {
                structured["meta"][field]
                    .as_u64()
                    .expect("accounting field") as usize
            })
            .sum::<usize>();
            assert_eq!(accounted, reported);
            let mut visible = rmcp::model::ServerResult::CallToolResult(result);
            if protocol < ProtocolVersion::V_2026_07_28 {
                visible.strip_result_type_for_legacy_peer();
            }
            let tokenizer = crate::tokens::Tokenizer::Cl100kBase;
            assert_eq!(
                reported,
                tokenizer.count(&serde_json::to_string(&visible).expect("wire result"))
            );
            assert!(
                tool_result_with_limit(
                    structured.clone(),
                    mode,
                    Some(reported - 1),
                    Some(&protocol),
                )
                .is_err()
            );
            assert!(
                tool_result_with_limit(structured, mode, Some(reported), Some(&protocol)).is_ok()
            );
        }
    }
}

#[tokio::test]
async fn receipt_resource_reads_fail_fast_at_the_reader_pool_bound() {
    let (server, _state) = LeanTokenMcp::pending();
    let permits = (0..default_receipt_resource_read_capacity())
        .map(|_| {
            server
                .resource_read_admission
                .try_admit()
                .expect("resource read permit")
        })
        .collect::<Vec<_>>();
    let error = server
        .read_receipt_resource(
            "leantoken://receipt/v1/r0123456789abcdef0123456789abcdef0123456789abcdef".into(),
            None,
        )
        .await
        .expect_err("resource read must fail before waiting for storage");
    assert_eq!(
        error.data.as_ref().and_then(|data| data.get("category")),
        Some(&serde_json::json!("retrieval_capacity_exhausted"))
    );
    drop(permits);
    assert_eq!(
        server.resource_read_admission.available_permits(),
        default_receipt_resource_read_capacity()
    );
}

#[test]
fn retryable_conflicts_are_successful_structured_results() {
    let (server, _state) = LeanTokenMcp::pending();
    for error in [
        crate::Error::StaleReconciliation {
            expected: 12,
            actual: 13,
        },
        crate::Error::ReconciliationFailed(Arc::new(crate::Error::StaleReconciliation {
            expected: 12,
            actual: 13,
        })),
        crate::Error::RetryableConflict(crate::error::RetryableOperation::Retrieval),
        crate::Error::ReconciliationFailed(Arc::new(crate::Error::RetryableConflict(
            crate::error::RetryableOperation::Retrieval,
        ))),
    ] {
        let result = server
            .service_result::<()>(Err(error))
            .expect("tool result");

        assert_eq!(result.is_error, Some(false));
        let structured = result.structured_content.expect("structured retry result");
        assert_eq!(structured["status"], "retryable");
        assert_eq!(structured["reason"], "repository_changed");
        assert_eq!(structured["retry_after_ms"], 100);
    }
}

#[tokio::test]
async fn ready_operation_is_not_retried() {
    let (_server, mcp_services) = LeanTokenMcp::pending();
    let calls = std::sync::atomic::AtomicUsize::new(0);
    let waits = std::sync::atomic::AtomicUsize::new(0);

    let result = retry_after_initial_index_with_policy(
        "files",
        &mcp_services,
        CancellationToken::new(),
        Duration::from_secs(30),
        |_| {
            waits.fetch_add(1, Ordering::AcqRel);
            std::future::ready(Ok::<(), crate::Error>(()))
        },
        || {
            calls.fetch_add(1, Ordering::AcqRel);
            std::future::ready(Ok::<_, crate::Error>(42))
        },
    )
    .await
    .expect("ready operation");

    assert_eq!(result, 42);
    assert_eq!(calls.load(Ordering::Acquire), 1);
    assert_eq!(waits.load(Ordering::Acquire), 0);
}

#[tokio::test(start_paused = true)]
async fn initial_index_retry_is_bounded() {
    let (_server, mcp_services) = LeanTokenMcp::pending();
    let calls = std::sync::atomic::AtomicUsize::new(0);
    let waits = std::sync::atomic::AtomicUsize::new(0);

    let error = retry_after_initial_index_with_policy(
        "files",
        &mcp_services,
        CancellationToken::new(),
        Duration::from_millis(250),
        |cancellation| {
            waits.fetch_add(1, Ordering::AcqRel);
            async move {
                cancellation.cancelled().await;
                Err(crate::Error::Cancelled)
            }
        },
        || {
            calls.fetch_add(1, Ordering::AcqRel);
            std::future::ready(Err::<(), _>(crate::Error::IndexNotReady))
        },
    )
    .await
    .expect_err("generation-zero operation must time out");

    assert!(matches!(error, crate::Error::IndexNotReady));
    assert_eq!(calls.load(Ordering::Acquire), 1);
    assert_eq!(waits.load(Ordering::Acquire), 1);
}

#[tokio::test(start_paused = true)]
async fn initial_operation_does_not_restart_the_readiness_deadline() {
    let (_server, mcp_services) = LeanTokenMcp::pending();
    let waits = std::sync::atomic::AtomicUsize::new(0);

    let error = retry_after_initial_index_with_policy(
        "files",
        &mcp_services,
        CancellationToken::new(),
        Duration::from_secs(30),
        |_| {
            waits.fetch_add(1, Ordering::AcqRel);
            std::future::ready(Ok(()))
        },
        || async {
            tokio::time::sleep(Duration::from_secs(30)).await;
            Err::<(), _>(crate::Error::IndexNotReady)
        },
    )
    .await
    .expect_err("the first operation consumed the complete readiness budget");

    assert!(matches!(error, crate::Error::IndexNotReady));
    assert_eq!(waits.load(Ordering::Acquire), 0);
}

#[tokio::test(start_paused = true)]
async fn initial_index_retry_returns_first_published_result() {
    let (_server, mcp_services) = LeanTokenMcp::pending();
    let ready = Arc::new(AtomicBool::new(false));
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let operation_ready = Arc::clone(&ready);
    let operation_calls = Arc::clone(&calls);
    let waiting = tokio::spawn(async move {
        retry_after_initial_index_with_policy(
            "files",
            &mcp_services,
            CancellationToken::new(),
            Duration::from_secs(1),
            |_| async {
                tokio::time::sleep(Duration::from_millis(100)).await;
                Ok(())
            },
            move || {
                operation_calls.fetch_add(1, Ordering::AcqRel);
                let result = if operation_ready.load(Ordering::Acquire) {
                    Ok(42)
                } else {
                    Err(crate::Error::IndexNotReady)
                };
                std::future::ready(result)
            },
        )
        .await
    });
    tokio::task::yield_now().await;
    assert_eq!(calls.load(Ordering::Acquire), 1);

    ready.store(true, Ordering::Release);
    tokio::time::advance(Duration::from_millis(100)).await;

    assert_eq!(
        waiting
            .await
            .expect("join readiness retry")
            .expect("published result"),
        42
    );
    assert_eq!(calls.load(Ordering::Acquire), 2);
}

#[tokio::test]
async fn initial_index_retry_honors_cancellation() {
    let (_server, mcp_services) = LeanTokenMcp::pending();
    let cancellation = CancellationToken::new();
    cancellation.cancel();

    let error = retry_after_initial_index_with_policy(
        "files",
        &mcp_services,
        cancellation,
        Duration::from_secs(30),
        |_| std::future::pending::<crate::Result<()>>(),
        || std::future::ready(Err::<(), _>(crate::Error::IndexNotReady)),
    )
    .await
    .expect_err("cancelled retry must stop");

    assert!(matches!(error, crate::Error::Cancelled));
}

#[tokio::test]
async fn initial_index_retry_stops_when_runtime_fails() {
    let (_server, mcp_services) = LeanTokenMcp::pending();
    let waiting_services = mcp_services.clone();
    let waiting = tokio::spawn(async move {
        retry_after_initial_index_with_policy(
            "files",
            &waiting_services,
            CancellationToken::new(),
            Duration::from_secs(30),
            |_| std::future::pending::<crate::Result<()>>(),
            || std::future::ready(Err::<(), _>(crate::Error::IndexNotReady)),
        )
        .await
    });
    tokio::task::yield_now().await;
    assert!(!waiting.is_finished());

    mcp_services.set_failed(&crate::Error::McpRuntimeStopped);

    let error = waiting
        .await
        .expect("join initial-index retry")
        .expect_err("runtime failure must interrupt readiness retry");
    assert!(matches!(error, crate::Error::McpRuntimeStopped));
}

#[tokio::test]
async fn initial_index_retry_prefers_runtime_failure_over_readiness_error() {
    let (_server, mcp_services) = LeanTokenMcp::pending();
    let failed_services = mcp_services.clone();

    let error = retry_after_initial_index_with_policy(
        "files",
        &mcp_services,
        CancellationToken::new(),
        Duration::from_secs(30),
        move |_| async move {
            failed_services.set_failed(&crate::Error::McpRuntimeStopped);
            Err(crate::Error::IndexNotReady)
        },
        || std::future::ready(Err::<(), _>(crate::Error::IndexNotReady)),
    )
    .await
    .expect_err("terminal runtime failure must supersede readiness error");

    assert!(matches!(error, crate::Error::McpRuntimeStopped));
}

#[tokio::test]
async fn protocol_initialization_wait_observes_transition() {
    let (_server, services) = LeanTokenMcp::pending();
    let waiting_services = services.clone();
    let waiting = tokio::spawn(async move {
        waiting_services.wait_initialized().await;
    });
    tokio::task::yield_now().await;
    assert!(!waiting.is_finished());

    services.mark_protocol_initialized();

    tokio::time::timeout(Duration::from_secs(1), waiting)
        .await
        .expect("initialization wait must wake")
        .expect("join initialization wait");
}

#[tokio::test(start_paused = true)]
async fn starting_service_wait_is_bounded() {
    let (_server, services) = LeanTokenMcp::pending();

    let state = services
        .wait_for_services(
            services.get(),
            CancellationToken::new(),
            tokio::time::Instant::now() + Duration::from_millis(250),
        )
        .await
        .expect("bounded wait");

    assert!(matches!(state, McpServiceState::Starting(_)));
}

#[tokio::test]
async fn starting_service_wait_observes_terminal_transition() {
    let (_server, services) = LeanTokenMcp::pending();
    let waiting_services = services.clone();
    let waiting = tokio::spawn(async move {
        waiting_services
            .wait_for_services(
                waiting_services.get(),
                CancellationToken::new(),
                tokio::time::Instant::now() + Duration::from_secs(1),
            )
            .await
    });
    tokio::task::yield_now().await;
    assert!(!waiting.is_finished());

    services.set_failed(&crate::Error::McpRuntimeStopped);

    let state = waiting
        .await
        .expect("join service wait")
        .expect("terminal service state");
    assert!(matches!(state, McpServiceState::Failed { .. }));
}

#[tokio::test]
async fn starting_service_wait_honors_cancellation() {
    let (_server, services) = LeanTokenMcp::pending();
    let cancellation = CancellationToken::new();
    cancellation.cancel();

    let error = services
        .wait_for_services(
            services.get(),
            cancellation,
            tokio::time::Instant::now() + Duration::from_secs(30),
        )
        .await
        .expect_err("cancelled startup wait must stop");

    assert!(matches!(error, crate::Error::Cancelled));
}

#[test]
fn mcp_error_mapping_separates_invalid_input_from_internal_failures() {
    let invalid = into_mcp_error(crate::Error::InputTooLong {
        field: "search query",
        max_bytes: 64,
    });
    assert_eq!(invalid.code, rmcp::model::ErrorCode::INVALID_PARAMS);
    assert_eq!(
        invalid
            .data
            .as_ref()
            .and_then(|data| data["category"].as_str()),
        Some("input_too_long")
    );
    assert_eq!(
        invalid.data.as_ref().map(|data| &data["limit"]),
        Some(&serde_json::json!(64))
    );

    let ambiguous = into_mcp_error(crate::Error::AmbiguousSymbol {
        path: "service.rs".into(),
        symbol: "run".into(),
    });
    assert_eq!(ambiguous.code, rmcp::model::ErrorCode::INVALID_PARAMS);
    assert_eq!(
        ambiguous.data,
        Some(serde_json::json!({
            "category": "symbol_ambiguous",
        }))
    );

    let request_limit = into_mcp_error(crate::Error::RequestLimitExceeded {
        field: "max_tokens",
        requested: 32_001,
        limit: 32_000,
    });
    assert_eq!(request_limit.code, rmcp::model::ErrorCode::INVALID_PARAMS);
    assert_eq!(
        request_limit.data,
        Some(serde_json::json!({
            "category": "request_limit_exceeded",
            "field": "max_tokens",
            "requested": 32_001,
            "limit": 32_000,
        }))
    );

    let retrieval_limit = into_mcp_error(crate::Error::RetrievalLimitExceeded {
        kind: crate::RetrievalLimitKind::RegexChunksPerFile,
        observed: 264,
        limit: 256,
    });
    assert_eq!(retrieval_limit.code, rmcp::model::ErrorCode::INVALID_PARAMS);
    assert_eq!(
        retrieval_limit.message,
        "retrieval regex_chunks_per_file limit exceeded: observed 264, limit 256; exclude or narrow paths that include unusually large files"
    );
    assert_eq!(
        retrieval_limit.data,
        Some(serde_json::json!({
            "category": "request_limit_exceeded",
            "reason": "regex_chunks_per_file",
            "requested": 264,
            "limit": 256,
        }))
    );

    assert_path_retrieval_limit_error_mapping();

    let response_budget = into_mcp_error(crate::Error::ResponseBudgetExceeded {
        provided_max_response_tokens: 40,
        minimum_required_response_tokens: 73,
        retry_with_at_least: 73,
        breakdown: crate::ResponseBudgetBreakdown {
            mandatory_response_tokens: 61,
            source_tokens: 17,
            protocol_tokens: 20,
            path_and_metadata_tokens: 24,
            receipt_reserve_tokens: 12,
        },
    });
    assert_eq!(response_budget.code, rmcp::model::ErrorCode::INVALID_PARAMS);
    assert_eq!(
        response_budget.message,
        "max_response_tokens is too small; retry with at least 73"
    );
    assert_eq!(
        response_budget.data,
        Some(serde_json::json!({
            "category": "request_limit_exceeded",
            "field": "max_response_tokens",
            "requested": 73,
            "limit": 40,
            "provided_max_response_tokens": 40,
            "minimum_required_response_tokens": 73,
            "retry_with_at_least": 73,
            "breakdown": {
                "mandatory_response_tokens": 61,
                "source_tokens": 17,
                "protocol_tokens": 20,
                "path_and_metadata_tokens": 24,
                "receipt_reserve_tokens": 12,
            },
        }))
    );

    let input_constraints = into_mcp_error(crate::Error::InvalidInputConstraints(
        crate::InputViolations::new(vec![
            crate::InputViolation {
                field: "focus paths",
                reason: "must not be empty when focus path constraints are enabled",
            },
            crate::InputViolation {
                field: "plan_only",
                reason: "cannot be combined with a handoff manifest",
            },
        ]),
    ));
    assert_eq!(
        input_constraints.code,
        rmcp::model::ErrorCode::INVALID_PARAMS
    );
    assert_eq!(
        input_constraints.data,
        Some(serde_json::json!({
            "category": "invalid_input",
            "violations": [
                {
                    "field": "focus paths",
                    "reason": "must not be empty when focus path constraints are enabled",
                },
                {
                    "field": "plan_only",
                    "reason": "cannot be combined with a handoff manifest",
                }
            ],
        }))
    );

    assert_search_option_error_mapping();

    let selector = into_mcp_error(crate::Error::InvalidJsonSelector {
        stage: "evaluate",
        offset: 6,
        line: 1,
        column: 7,
        reason: "Runtime error: Argument 0 expects type array, given number".into(),
    });
    assert_eq!(selector.code, rmcp::model::ErrorCode::INVALID_PARAMS);
    assert_eq!(
        selector.data,
        Some(serde_json::json!({
            "category": "invalid_json_selector",
            "field": "JMESPath expression",
            "stage": "evaluate",
            "offset": 6,
            "line": 1,
            "column": 7,
            "reason": "Runtime error: Argument 0 expects type array, given number",
        }))
    );

    let syntax = into_mcp_error(crate::Error::InvalidJson {
        syntax_category: "syntax",
        byte_offset: 12,
        line: 1,
        column: 13,
        reason: "trailing comma at line 1 column 13".into(),
    });
    assert_eq!(syntax.code, rmcp::model::ErrorCode::INVALID_PARAMS);
    assert_eq!(
        syntax
            .data
            .as_ref()
            .and_then(|data| data["byte_offset"].as_u64()),
        Some(12)
    );

    let stale_receipt = into_mcp_error(crate::Error::StaleReceipt {
        receipt_generation: 4,
        repository_generation: 5,
    });
    assert_eq!(stale_receipt.code, rmcp::model::ErrorCode::INVALID_PARAMS);
    assert_eq!(
        stale_receipt
            .data
            .as_ref()
            .and_then(|data| data["category"].as_str()),
        Some("stale_receipt")
    );

    let internal = [
        crate::Error::InvalidConfiguration("chunk size must be positive".into()),
        crate::Error::OperationFailure("parser returned None".into()),
        crate::Error::RuntimeCapabilityUnavailable {
            capability: "SQLite FTS5",
            source: None,
        },
    ];
    for error in internal {
        assert_eq!(
            into_mcp_error(error).code,
            rmcp::model::ErrorCode::INTERNAL_ERROR
        );
    }
}

fn assert_path_retrieval_limit_error_mapping() {
    let path_limit = into_mcp_error(crate::Error::RetrievalPathLimitExceeded {
        kind: crate::RetrievalLimitKind::RegexChunksPerFile,
        path: "generated/large.rs".into(),
        observed: 264,
        limit: 256,
    });
    assert_eq!(path_limit.code, rmcp::model::ErrorCode::INVALID_PARAMS);
    assert_eq!(
        path_limit.message,
        "retrieval regex_chunks_per_file limit exceeded for generated/large.rs: observed 264, limit 256; exclude or narrow paths that include unusually large files"
    );
    assert_eq!(
        path_limit.data,
        Some(serde_json::json!({
            "category": "request_limit_exceeded",
            "reason": "regex_chunks_per_file",
            "blocking_path": "generated/large.rs",
            "requested": 264,
            "limit": 256,
        }))
    );
}

fn assert_search_option_error_mapping() {
    let search_options = into_mcp_error(crate::incompatible_occurrence_options(
        crate::SearchMode::Symbol,
        vec![
            "all_occurrences=true".into(),
            "projection=occurrences".into(),
        ],
    ));
    assert_eq!(search_options.code, rmcp::model::ErrorCode::INVALID_PARAMS);
    assert_eq!(
        search_options.data,
        Some(serde_json::json!({
            "category": "invalid_input",
            "field": "all_occurrences",
            "allowed_modes": ["text", "regex"],
            "conflicting_options": [
                "mode=symbol",
                "all_occurrences=true",
                "projection=occurrences",
            ],
            "examples": {
                "ranked_symbol": "{\"operation\":{\"kind\":\"symbol\",\"query\":\"Services\"}}",
                "exhaustive_text": "{\"operation\":{\"kind\":\"text\",\"query\":\"Services\",\"all_occurrences\":true,\"projection\":\"occurrences\"}}",
            },
        }))
    );

    let exhausted = into_mcp_error(crate::Error::RegexWorkBudgetExceeded {
        dimension: crate::RegexWorkDimension::CandidateChunks,
        candidate_files: 10,
        candidate_chunks: 20_511,
        candidate_bytes: 1_024,
        limit: 20_510,
    });
    assert_eq!(exhausted.code, rmcp::model::ErrorCode::INVALID_PARAMS);
    assert_eq!(
        exhausted.message,
        "exhaustive search stopped before complete coverage at its bounded candidate-work budget; narrow the search scope or make the query more selective"
    );
    assert_eq!(
        exhausted.data,
        Some(serde_json::json!({
            "category": "incomplete_work",
            "complete": false,
            "recovery": {
                "action": "partition_scope",
                "message": "Narrow include_paths or make the query more selective; increasing max_tokens or max_results cannot make one request unbounded.",
                "required_fields": ["include_paths"]
            },
            "limiting_dimension": "candidate_chunks",
            "candidate_files": 10,
            "candidate_chunks": 20_511,
            "candidate_bytes": 1_024,
            "limit": 20_510,
        }))
    );
}

#[test]
fn mcp_error_mapping_never_serializes_internal_or_input_paths() {
    let unix_marker = "/home/example/sensitive-marker/external.sqlite";
    let windows_marker = r"C:\Users\example\sensitive-marker\external.sqlite";
    let invalid_regex = ["(?P<", "sensitive-marker", ">"].concat();
    let errors = [
        crate::Error::RootNotFound(unix_marker.into()),
        crate::Error::UnsafeRepositoryRoot(unix_marker.into()),
        crate::Error::PathOutsideRoot(unix_marker.into()),
        crate::Error::PathOutsideRoot(windows_marker.into()),
        crate::Error::NotIndexed(unix_marker.into()),
        crate::Error::SymbolNotFound {
            path: unix_marker.into(),
            symbol: "sensitive-marker".into(),
        },
        crate::Error::AmbiguousSymbol {
            path: unix_marker.into(),
            symbol: "sensitive-marker".into(),
        },
        crate::Error::HeadingNotFound {
            path: unix_marker.into(),
            heading: "sensitive-marker".into(),
            occurrence: 2,
        },
        crate::Error::UnsupportedLanguage(unix_marker.into()),
        crate::Error::InvalidRequest(format!("invalid path: {unix_marker}")),
        crate::Error::OperationFailure(format!("failed at {unix_marker}")),
        crate::Error::RepositoryMismatch {
            database: windows_marker.into(),
            expected_repository: unix_marker.into(),
            actual_repository: unix_marker.into(),
        },
        crate::Error::IndexScopeMismatch {
            database: windows_marker.into(),
        },
        crate::Error::Io(std::io::Error::other(format!(
            "permission denied at {unix_marker}"
        ))),
        crate::Error::Sqlite(rusqlite::Error::InvalidPath(windows_marker.into())),
        crate::Error::Regex(regex::Regex::new(&invalid_regex).expect_err("regex")),
        crate::Error::Glob(globset::Glob::new("[sensitive-marker").expect_err("glob")),
    ];

    for error in errors {
        let response = into_mcp_error(error);
        let wire = serde_json::to_string(&response).expect("serialize public error");
        for marker in [
            unix_marker,
            windows_marker,
            "sensitive-marker",
            "external.sqlite",
            "example",
        ] {
            assert!(
                !wire.contains(marker),
                "public error leaked {marker}: {wire}"
            );
        }
        assert!(
            response
                .data
                .as_ref()
                .and_then(|data| data["category"].as_str())
                .is_some(),
            "public error has no stable category: {wire}"
        );
    }
}

#[test]
fn mcp_fallback_errors_preserve_their_public_category() {
    let cases = [
        (
            crate::Error::DoctorFailure {
                stage: "catalog",
                message: "tools/list returned no result".into(),
            },
            "doctor_failure",
        ),
        (
            crate::Error::StaleReconciliation {
                expected: 12,
                actual: 13,
            },
            "retryable_conflict",
        ),
        (
            crate::Error::Io(std::io::Error::other("private descriptor")),
            "internal_error",
        ),
    ];

    for (error, category) in cases {
        let response = into_mcp_error(error);
        assert_eq!(
            response
                .data
                .as_ref()
                .and_then(|data| data["category"].as_str()),
            Some(category)
        );
    }
}

#[test]
fn explicit_null_limits_are_equivalent_to_omission() {
    let files = serde_json::from_value::<FilesMcpRequest>(serde_json::json!({
        "operation": {"kind": "tree", "max_results": null}
    }))
    .expect("null files limit");
    assert!(matches!(
        files.operation,
        FilesMcpOperation::Tree {
            max_results: None,
            ..
        }
    ));

    let search = serde_json::from_value::<SearchMcpRequest>(serde_json::json!({
        "operation": {
            "kind": "text",
            "query": "answer",
            "max_results": null,
            "max_tokens": null,
            "context_lines": null
        }
    }))
    .expect("null search limits");
    let options = match search.operation {
        SearchMcpOperation::Text { options } => options,
        _ => panic!("expected text operation"),
    };
    assert_eq!(options.max_results, None);
    assert_eq!(options.max_tokens, None);
    assert_eq!(options.context_lines, None);

    let outline = serde_json::from_value::<OutlineMcpRequest>(serde_json::json!({
        "paths": ["lib.rs"],
        "max_results": null,
        "max_tokens": null
    }))
    .expect("null outline limits");
    assert_eq!(outline.max_results, None);
    assert_eq!(outline.max_tokens, None);

    let read = serde_json::from_value::<ReadMcpRequest>(serde_json::json!({
        "path": "lib.rs",
        "target": {"kind": "lines", "start": 1, "end": 1},
        "max_tokens": null
    }))
    .expect("null read limit");
    assert_eq!(read.max_tokens, None);

    let context = serde_json::from_value::<ContextMcpRequest>(serde_json::json!({
        "task": "find answer",
        "token_budget": null,
        "minimum_fragments_per_focus_path": null
    }))
    .expect("null context limits");
    assert_eq!(context.token_budget, None);
    assert_eq!(context.minimum_fragments_per_focus_path, None);
}

#[test]
fn search_operation_rejects_unknown_fields() {
    let error = serde_json::from_value::<SearchMcpRequest>(serde_json::json!({
        "operation": {
            "kind": "text",
            "query": "answer",
            "max_token": 1
        }
    }))
    .expect_err("misspelled search limit");
    assert!(error.to_string().contains("unknown field"));
}

#[test]
fn omitted_context_budget_uses_the_runtime_default() {
    let request = serde_json::from_value::<ContextMcpRequest>(serde_json::json!({
        "task": "find answer"
    }))
    .expect("context request without a budget");
    let (request, _, _, _, _, _, _) = request.into_parts(37);
    assert_eq!(request.token_budget, 37);
    let null_limit = serde_json::from_value::<ContextMcpRequest>(serde_json::json!({
        "task": "find answer",
        "max_response_tokens": null
    }))
    .expect("null response limit is equivalent to omission");
    let (_, _, _, _, options, _, _) = null_limit.into_parts(37);
    assert_eq!(options.max_response_tokens(), None);
    assert_eq!(options.context_response_profile(), None);

    let request = serde_json::from_value::<ContextMcpRequest>(serde_json::json!({
        "task": "find answer",
        "token_budget": 23,
        "max_response_tokens": 47,
        "focus_paths": ["src/**"],
        "strict_focus_paths": true,
        "minimum_fragments_per_focus_path": 2,
        "required_evidence": [{
            "path": "paper/**",
            "queries": ["failure boundary", "disclosure"],
            "minimum_query_matches": 2
        }],
        "changed_paths": ["src/lib.rs"],
        "strict_changed_paths": true,
        "response_profile": "explain",
        "workflow_evidence": {
            "failure_traces": ["error[E0001]"],
            "symbols": ["answer"],
            "paths": ["src/lib.rs"],
            "test_intents": ["answer regression"]
        }
    }))
    .expect("context request with a budget");
    let (request, _, workflow_evidence, _, options, _, _) = request.into_parts(37);
    assert_eq!(request.token_budget, 23);
    assert_eq!(options.max_response_tokens(), Some(47));
    assert_eq!(
        options.context_response_profile(),
        Some(ContextResponseProfile::Explain)
    );
    assert_eq!(request.focus_paths, ["src/**"]);
    assert!(request.strict_focus_paths);
    assert_eq!(request.minimum_fragments_per_focus_path, Some(2));
    assert_eq!(request.required_evidence.len(), 1);
    assert_eq!(request.required_evidence[0].path, "paper/**");
    assert_eq!(
        request.required_evidence[0].queries,
        ["failure boundary", "disclosure"]
    );
    assert_eq!(request.required_evidence[0].minimum_query_matches, 2);
    assert_eq!(request.changed_paths, ["src/lib.rs"]);
    assert!(request.strict_changed_paths);
    assert_eq!(workflow_evidence.failure_traces, ["error[E0001]"]);
    assert_eq!(workflow_evidence.symbols, ["answer"]);
    assert_eq!(workflow_evidence.paths, ["src/lib.rs"]);
    assert_eq!(workflow_evidence.test_intents, ["answer regression"]);

    for invalid in [0, MAX_OUTPUT_TOKENS + 1] {
        let request = serde_json::from_value::<ContextMcpRequest>(serde_json::json!({
            "task": "find answer",
            "max_response_tokens": invalid
        }))
        .expect("syntactically valid context response limit");
        assert!(request.validate_limits(McpLimitPolicy::DEFAULT).is_err());
    }
    assert!(
        serde_json::from_value::<ContextMcpRequest>(serde_json::json!({
            "task": "find answer",
            "max_response_tokens": -1
        }))
        .is_err()
    );
}

#[test]
fn context_mcp_rejects_unknown_response_profiles() {
    let result = serde_json::from_value::<ContextMcpRequest>(serde_json::json!({
        "task": "find answer",
        "response_profile": "verbose"
    }));
    assert!(result.is_err());
}

#[test]
fn context_mcp_maps_bounded_handoff_state() {
    let request = serde_json::from_value::<ContextMcpRequest>(serde_json::json!({
        "task": "continue implementation",
        "handoff": {
            "summary": "executor state",
            "validations": [{
                "command": "cargo test",
                "status": "passed",
                "summary": "all tests passed"
            }],
            "assumptions": ["public API remains stable"],
            "open_questions": ["is another fixture required?"],
            "negative_evidence": ["no alternate owner found"],
            "avoid_rules": ["do not copy source bodies"]
        }
    }))
    .expect("context handoff request");
    let (_, _, _, _, _, _, handoff) = request.into_parts(37);
    let handoff = handoff.expect("handoff");
    assert_eq!(handoff.summary.as_deref(), Some("executor state"));
    assert_eq!(handoff.validations.len(), 1);
    assert_eq!(handoff.assumptions, ["public API remains stable"]);

    assert!(
        serde_json::from_value::<ContextMcpRequest>(serde_json::json!({
            "task": "continue implementation",
            "handoff": {"unexpected": true}
        }))
        .is_err()
    );
}

#[test]
fn tool_input_fields_are_documented() {
    for tool in LeanTokenMcp::tool_router().list_all() {
        // Savings accepts an optional snapshot and follows the same field contract.
        let properties = tool
            .input_schema
            .get("properties")
            .and_then(serde_json::Value::as_object);
        let properties =
            properties.unwrap_or_else(|| panic!("{} input properties missing", tool.name));
        for (field, schema) in properties {
            assert!(
                schema
                    .get("description")
                    .and_then(serde_json::Value::as_str)
                    .is_some_and(|description| !description.trim().is_empty()),
                "{}.{} is missing a schema description",
                tool.name,
                field
            );
        }
    }
}

#[test]
fn tool_required_fields_match_the_wire_contract() {
    let expected = [
        ("context", Some(serde_json::json!(["task"]))),
        ("files", Some(serde_json::json!(["operation"]))),
        ("history", Some(serde_json::json!(["operation"]))),
        ("json", Some(serde_json::json!(["operation"]))),
        ("outline", Some(serde_json::json!(["paths"]))),
        ("read", Some(serde_json::json!(["path", "target"]))),
        ("receipt_rebase", Some(serde_json::json!(["receipt_id"]))),
        ("savings", None),
        ("search", Some(serde_json::json!(["operation"]))),
    ]
    .into_iter()
    .collect::<std::collections::HashMap<_, _>>();

    for tool in LeanTokenMcp::tool_router().list_all() {
        let expected = expected.get(tool.name.as_ref()).expect("known tool");
        match expected {
            Some(required) => assert_eq!(
                tool.input_schema.get("required"),
                Some(required),
                "{} required fields changed",
                tool.name
            ),
            None => assert!(
                tool.input_schema.get("required").is_none(),
                "{} should not advertise required fields",
                tool.name
            ),
        }
    }
}

#[test]
fn files_schema_matches_operation_specific_runtime_requirements() {
    let tool = LeanTokenMcp::tool_router()
        .list_all()
        .into_iter()
        .find(|tool| tool.name == "files")
        .expect("files tool");
    let schema = serde_json::Value::Object((*tool.input_schema).clone());
    let variants = schema["$defs"]["FilesMcpOperation"]["oneOf"]
        .as_array()
        .expect("operation variants");
    assert_eq!(variants.len(), 3);
    assert_eq!(variants[0]["properties"]["kind"]["const"], "tree");
    assert_eq!(variants[1]["properties"]["kind"]["const"], "find");
    assert_eq!(variants[1]["properties"]["query"]["type"], "string");
    assert_eq!(variants[1]["properties"]["query"]["minLength"], 1);
    assert_eq!(
        variants[1]["required"],
        serde_json::json!(["kind", "query"])
    );
    assert_eq!(variants[2]["properties"]["kind"]["const"], "glob");
    assert_eq!(variants[2]["properties"]["pattern"]["type"], "string");
    assert_eq!(
        variants[2]["required"],
        serde_json::json!(["kind", "pattern"])
    );
    for variant in variants {
        assert_eq!(
            variant["properties"]["cursor"]["maxLength"],
            crate::services::MAX_FILES_CURSOR_ENCODED_BYTES,
            "files must accept every cursor emitted by the service"
        );
    }
}

#[test]
fn history_schema_matches_operation_specific_result_limits() {
    let tool = LeanTokenMcp::tool_router()
        .list_all()
        .into_iter()
        .find(|tool| tool.name == "history")
        .expect("history tool");
    let schema = serde_json::Value::Object((*tool.input_schema).clone());
    let variants = schema["$defs"]["HistoryMcpOperation"]["oneOf"]
        .as_array()
        .expect("history operation variants");
    let maximum_for = |kind: &str| {
        variants
            .iter()
            .find(|variant| variant["properties"]["kind"]["const"] == kind)
            .unwrap_or_else(|| panic!("{kind} history variant"))["properties"]["max_results"]
            ["maximum"]
            .as_u64()
            .expect("max_results maximum")
    };
    assert_eq!(
        maximum_for("diff_symbols"),
        crate::services::MAX_DIFF_SYMBOL_RESULTS as u64
    );
    assert_eq!(maximum_for("symbol_log"), MAX_RESULTS as u64);
}

#[test]
fn search_schema_matches_exhaustive_occurrence_runtime_requirements() {
    let tool = LeanTokenMcp::tool_router()
        .list_all()
        .into_iter()
        .find(|tool| tool.name == "search")
        .expect("search tool");
    let schema = serde_json::Value::Object((*tool.input_schema).clone());
    let exhaustive_modes = SearchMode::EXHAUSTIVE_MODES.map(SearchMode::wire_name);
    let variants = schema["$defs"]["SearchMcpOperation"]["oneOf"]
        .as_array()
        .expect("search operation variants");
    let kinds = variants
        .iter()
        .filter_map(|variant| variant["properties"]["kind"]["const"].as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        kinds,
        ["auto", "text", "regex", "identifier", "symbol", "reference"]
    );
    assert_eq!(exhaustive_modes, ["text", "regex"]);
    for variant in variants {
        assert!(!variant["properties"]["query"].is_null());
        assert!(!variant["properties"]["all_occurrences"].is_null());
        let description = variant["description"]
            .as_str()
            .expect("search operation description");
        for rule in [
            "prefer_structural requires auto or identifier mode",
            "query_receipt requires all_occurrences=true with text or regex mode and auto or occurrences projection",
            "cannot be combined with focus_paths, receipt_id, or cursor",
        ] {
            assert!(
                description.contains(rule),
                "search operation description is missing `{rule}`"
            );
        }
    }
}

#[test]
fn retrieval_tools_expose_consistency_boundary() {
    for tool in LeanTokenMcp::tool_router()
        .list_all()
        .into_iter()
        .filter(|tool| tool.name != "savings" && tool.name != "history" && tool.name != "json")
    {
        let schema = serde_json::Value::Object((*tool.input_schema).clone());
        let consistency = schema
            .pointer("/properties/consistency")
            .or_else(|| schema.pointer("/$defs/FilesMcpOperation/oneOf/0/properties/consistency"))
            .or_else(|| schema.pointer("/$defs/SearchMcpOperation/oneOf/0/properties/consistency"))
            .unwrap_or_else(|| panic!("{} consistency schema missing", tool.name));
        assert_eq!(
            consistency.get("default"),
            Some(&serde_json::json!("indexed_generation"))
        );
        assert_eq!(
            consistency.get("enum"),
            Some(&serde_json::json!([
                "indexed_generation",
                "reconcile_working_tree"
            ]))
        );
        assert!(
            consistency
                .get("description")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|description| {
                    description.contains("reconcile_working_tree") && description.contains("edits")
                }),
            "{}.consistency must tell agents when to synchronize",
            tool.name
        );
    }
    let history = LeanTokenMcp::tool_router()
        .list_all()
        .into_iter()
        .find(|tool| tool.name == "history")
        .expect("history tool");
    assert!(
        history
            .input_schema
            .get("properties")
            .and_then(serde_json::Value::as_object)
            .is_none_or(|properties| !properties.contains_key("consistency"))
    );
}

#[test]
fn tool_descriptions_route_native_discovery_workflows() {
    let descriptions = LeanTokenMcp::tool_router()
        .list_all()
        .into_iter()
        .map(|tool| {
            (
                tool.name.into_owned(),
                tool.description.expect("tool description").into_owned(),
            )
        })
        .collect::<std::collections::HashMap<_, _>>();
    let description_bytes = descriptions.values().map(String::len).sum::<usize>();
    assert!(
        description_bytes <= 5_000,
        "tool descriptions must stay within the 5,000-byte prompt budget; got {description_bytes}"
    );
    assert!(descriptions["files"].starts_with("Preferred over native find, ls, or glob"));
    assert!(descriptions["files"].contains("Discover repository paths"));
    assert!(descriptions["files"].contains("Next: use outline or read"));
    assert!(descriptions["search"].starts_with("Preferred over native grep or rg"));
    assert!(descriptions["search"].contains("Search indexed source"));
    assert!(descriptions["search"].contains("enclosing_symbol"));
    assert!(descriptions["outline"].starts_with("Preferred before native whole-file reads"));
    assert!(descriptions["outline"].contains("without reading whole source files"));
    assert!(descriptions["outline"].contains("Next: pass"));
    assert!(descriptions["read"].starts_with("Preferred over native Read, cat, head, or sed"));
    assert!(descriptions["read"].contains("expected_hash"));
    assert!(descriptions["read"].contains("truncated reads"));
    assert!(descriptions["history"].starts_with("Preferred over native git show, diff, or log -L"));
    assert!(descriptions["json"].starts_with("Preferred over native jq or whole-file reads"));
    assert!(descriptions["context"].starts_with("DEFAULT FIRST CALL"));
    assert!(descriptions["context"].contains("Build a bounded"));
    assert!(descriptions["context"].contains("plan_only previews"));
    assert!(descriptions["savings"].contains("unobserved task outcomes"));
    assert!(descriptions["savings"].contains("not task-success claims"));
    assert!(
        descriptions
            .values()
            .all(|description| description.contains("Example:"))
    );
    assert!(descriptions["search"].contains("all_occurrences=true requires text or regex mode"));
    assert!(
        descriptions["search"]
            .contains("projection=occurrences also requires all_occurrences=true")
    );
}

#[test]
fn history_description_routes_symbol_commit_history() {
    let history = LeanTokenMcp::tool_router()
        .list_all()
        .into_iter()
        .find(|tool| tool.name == "history")
        .expect("history tool");
    let description = history.description.expect("history description");
    assert!(description.contains("symbol_log is the commit-history operation"));
    assert!(description.contains("path-wide or repository-wide commit history"));
}

#[test]
fn server_instructions_keep_shared_routing_compact() {
    assert!(
        MCP_INSTRUCTIONS.len() <= 800,
        "shared MCP instructions must stay compact; got {} bytes",
        MCP_INSTRUCTIONS.len()
    );
    let first_window = &MCP_INSTRUCTIONS[..MCP_INSTRUCTIONS
        .char_indices()
        .map(|(index, _)| index)
        .take_while(|index| *index <= 512)
        .last()
        .unwrap_or(MCP_INSTRUCTIONS.len())];
    assert!(first_window.contains("call leantoken.context once"));
    assert!(first_window.contains("For a known scope"));
    assert!(first_window.contains("Use native tools for edits, builds, tests"));
    assert!(!MCP_INSTRUCTIONS.contains("leantoken.search over"));
    assert!(!MCP_INSTRUCTIONS.contains("leantoken.files over"));
}

#[test]
fn savings_tool_is_local_and_read_only() {
    let tool = LeanTokenMcp::tool_router()
        .list_all()
        .into_iter()
        .find(|tool| tool.name == "savings")
        .expect("savings tool");
    let annotations = tool.annotations.expect("savings annotations");
    assert_eq!(annotations.read_only_hint, Some(true));
    assert_eq!(annotations.open_world_hint, Some(false));
}

#[test]
fn tool_schemas_are_closed_bounded_and_remove_ambiguous_inputs() {
    let tools = LeanTokenMcp::tool_router()
        .list_all()
        .into_iter()
        .map(|tool| {
            (
                tool.name.into_owned(),
                serde_json::Value::Object((*tool.input_schema).clone()),
            )
        })
        .collect::<std::collections::HashMap<_, _>>();

    for (name, schema) in &tools {
        assert_eq!(
            schema.get("additionalProperties"),
            Some(&serde_json::json!(false)),
            "{name} must reject unknown arguments"
        );
    }
    assert_eq!(
        tools["context"].pointer("/properties/token_budget/default"),
        Some(&serde_json::Value::Null)
    );
    assert!(tools["files"].pointer("/$defs/FilesMcpOperation").is_some());
    assert!(tools["read"].pointer("/properties/symbol").is_none());
    assert!(tools["read"].pointer("/properties/start_line").is_none());
    assert!(tools["read"].pointer("/properties/target").is_some());

    let request = serde_json::from_value::<FilesMcpRequest>(serde_json::json!({
        "operation": {"kind": "find", "query": "mcp"}
    }))
    .expect("tagged files request shape");
    assert!(request.validate_limits(McpLimitPolicy::DEFAULT).is_ok());
    assert!(
        serde_json::from_value::<FilesMcpRequest>(serde_json::json!({
            "operation": "find", "query": "mcp"
        }))
        .is_err()
    );
    assert!(
        serde_json::from_value::<ReadMcpRequest>(serde_json::json!({
            "path": "src/mcp.rs",
            "target": {"kind": "symbol", "identity": {"name": "LeanTokenMcp"}}
        }))
        .is_ok()
    );
    assert!(
        serde_json::from_value::<ReadMcpRequest>(serde_json::json!({
            "path": "src/mcp.rs",
            "target": {"kind": "symbol", "name": "LeanTokenMcp"}
        }))
        .is_err()
    );
    assert_read_target_shapes();
}

fn assert_read_target_shapes() {
    let request = serde_json::from_value::<ReadMcpRequest>(serde_json::json!({
        "path": "src/mcp.rs",
        "target": {"kind": "lines", "start": 10, "end": 20}
    }))
    .expect("canonical line-range target");
    let (request, _, _, _) = request.into_parts();
    assert_eq!(request.start_line, Some(10));
    assert_eq!(request.end_line, Some(20));
    for target in [
        serde_json::json!({"kind": "range", "start": 10, "end": 20}),
        serde_json::json!({"kind": "line_range", "start_line": 10, "end_line": 20}),
    ] {
        assert!(
            serde_json::from_value::<ReadMcpRequest>(serde_json::json!({
                "path": "src/mcp.rs",
                "target": target
            }))
            .is_err()
        );
    }
    let heading = serde_json::from_value::<ReadMcpRequest>(serde_json::json!({
        "path": "README.md",
        "target": {"kind": "heading", "name": "Installation", "occurrence": 2}
    }))
    .expect("Markdown heading target");
    assert!(heading.validate_limits(McpLimitPolicy::DEFAULT).is_ok());
    let (heading, _, _, _) = heading.into_parts();
    assert_eq!(heading.heading.as_deref(), Some("Installation"));
    assert_eq!(heading.heading_occurrence, Some(2));
    assert!(heading.symbol.is_none());
    let invalid_heading = serde_json::from_value::<ReadMcpRequest>(serde_json::json!({
        "path": "README.md",
        "target": {"kind": "heading", "name": "Installation", "occurrence": 0}
    }))
    .expect("schema validation remains a runtime boundary");
    assert!(
        invalid_heading
            .validate_limits(McpLimitPolicy::DEFAULT)
            .is_err()
    );
    let continuation = serde_json::from_value::<ReadMcpRequest>(serde_json::json!({
        "path": "src/mcp.rs",
        "target": {"kind": "continuation", "cursor": "opaque"}
    }))
    .expect("continuation target");
    let (continuation, _, _, _) = continuation.into_parts();
    assert_eq!(continuation.continuation_cursor.as_deref(), Some("opaque"));
    assert!(continuation.symbol.is_none());
    assert!(continuation.heading.is_none());
    assert!(continuation.heading_occurrence.is_none());
    assert!(continuation.start_line.is_none());
    assert!(continuation.end_line.is_none());
}

#[test]
fn retrieval_response_budget_schemas_are_optional_and_bounded() {
    fn find_field<'a>(value: &'a serde_json::Value, name: &str) -> Option<&'a serde_json::Value> {
        match value {
            serde_json::Value::Object(object) => object
                .get(name)
                .or_else(|| object.values().find_map(|value| find_field(value, name))),
            serde_json::Value::Array(values) => {
                values.iter().find_map(|value| find_field(value, name))
            }
            _ => None,
        }
    }

    let tools = LeanTokenMcp::tool_router().list_all();
    for name in [
        "context", "read", "search", "outline", "files", "history", "json",
    ] {
        let tool = tools
            .iter()
            .find(|tool| tool.name == name)
            .unwrap_or_else(|| panic!("{name} tool"));
        let schema = serde_json::Value::Object((*tool.input_schema).clone());
        let response_limit = find_field(&schema, "max_response_tokens")
            .unwrap_or_else(|| panic!("{name} response limit schema missing"));
        assert_eq!(
            response_limit.get("minimum"),
            Some(&serde_json::json!(1)),
            "{name}"
        );
        assert_eq!(
            response_limit.get("maximum"),
            Some(&serde_json::json!(32_000)),
            "{name}"
        );
        assert_eq!(
            response_limit.get("default"),
            Some(&serde_json::Value::Null),
            "{name}"
        );
    }
}

#[test]
fn context_focus_candidate_schema_exposes_generation_bounds() {
    let context = LeanTokenMcp::tool_router()
        .list_all()
        .into_iter()
        .find(|tool| tool.name == "context")
        .expect("context tool");
    let schema = serde_json::Value::Object((*context.input_schema).clone());
    assert_eq!(
        schema.pointer("/properties/focus_paths/maxItems"),
        Some(&serde_json::json!(32))
    );
    assert_eq!(
        schema.pointer("/properties/focus_paths/description"),
        Some(&serde_json::json!(
            "Softly boost matching paths; this does not filter other candidates."
        ))
    );
    assert_eq!(
        schema.pointer("/properties/strict_focus_paths/description"),
        Some(&serde_json::json!(
            "Hard-filter returned fragments to focus paths; requires non-empty\n`focus_paths`."
        ))
    );
    assert_eq!(
        schema.pointer("/properties/minimum_fragments_per_focus_path/maximum"),
        Some(&serde_json::json!(8))
    );
}

#[test]
fn context_task_schema_accepts_every_wire_valid_value() {
    let request = serde_json::from_value::<ContextMcpRequest>(serde_json::json!({"task": "x"}))
        .expect("one-character non-empty task is wire-valid");
    assert_eq!(request.task.as_str(), "x");

    let context = LeanTokenMcp::tool_router()
        .list_all()
        .into_iter()
        .find(|tool| tool.name == "context")
        .expect("context tool");
    let schema = serde_json::Value::Object((*context.input_schema).clone());
    assert_eq!(
        schema.pointer("/properties/task/minLength"),
        Some(&serde_json::json!(1)),
        "the schema must accept every non-empty task accepted on the wire"
    );
}

#[test]
fn workflow_evidence_path_schema_matches_repository_path_bound() {
    let context = LeanTokenMcp::tool_router()
        .list_all()
        .into_iter()
        .find(|tool| tool.name == "context")
        .expect("context tool");
    let schema = serde_json::Value::Object((*context.input_schema).clone());
    assert_eq!(
        schema.pointer("/$defs/WorkflowEvidence/properties/paths/items/maxLength"),
        Some(&serde_json::json!(
            crate::repository::MAX_REPOSITORY_PATH_BYTES
        )),
        "workflow evidence paths must advertise the repository path ceiling"
    );
}

#[test]
fn retrieval_response_budget_limits_are_validated_for_every_tool() {
    fn set_limit(value: &mut serde_json::Value, nested: bool, limit: usize) {
        if nested {
            value["operation"]["max_response_tokens"] = serde_json::json!(limit);
        } else {
            value["max_response_tokens"] = serde_json::json!(limit);
        }
    }

    macro_rules! assert_invalid {
        ($ty:ty, $base:expr, $nested:expr) => {
            for invalid in [0, MAX_OUTPUT_TOKENS + 1] {
                let mut value = $base;
                set_limit(&mut value, $nested, invalid);
                let request = serde_json::from_value::<$ty>(value).expect("deserialize request");
                assert!(
                    request.validate_limits(McpLimitPolicy::DEFAULT).is_err(),
                    "{} accepted {invalid}",
                    stringify!($ty)
                );
            }
        };
    }

    assert_invalid!(
        FilesMcpRequest,
        serde_json::json!({"operation": {"kind": "tree"}}),
        true
    );
    assert_invalid!(
        SearchMcpRequest,
        serde_json::json!({"operation": {"kind": "auto", "query": "needle"}}),
        true
    );
    assert_invalid!(
        OutlineMcpRequest,
        serde_json::json!({"paths": ["src/lib.rs"]}),
        false
    );
    assert_invalid!(
        ReadMcpRequest,
        serde_json::json!({
            "path": "src/lib.rs",
            "target": {"kind": "lines", "start": 1, "end": 1}
        }),
        false
    );
    assert_invalid!(
        HistoryMcpRequest,
        serde_json::json!({
            "operation": {
                "kind": "read_symbol",
                "path": "src/lib.rs",
                "symbol": {"name": "owner"},
                "revision": "HEAD"
            }
        }),
        true
    );
    assert_invalid!(
        JsonMcpRequest,
        serde_json::json!({
            "operation": {"kind": "query", "path": "data.json"}
        }),
        true
    );
}

#[test]
fn receipt_id_maps_to_the_service_request() {
    let request = serde_json::from_value::<ReadMcpRequest>(serde_json::json!({
        "path": "README.md",
        "receipt_id": "r0000000000000001",
        "target": {"kind": "lines", "start": 1, "end": 2}
    }))
    .expect("read request with receipt");
    let (request, _, _, _) = request.into_parts();
    assert_eq!(request.receipt_id.as_deref(), Some("r0000000000000001"));
}

#[test]
fn receipt_rebase_maps_explicit_exact_only_controls() {
    let request = serde_json::from_value::<ReceiptRebaseMcpRequest>(serde_json::json!({
        "receipt_id": "r0000000000000001",
        "max_samples_per_outcome": 0,
        "max_response_tokens": 1_000,
        "consistency": "reconcile_working_tree",
        "expected_repository_id": "repository"
    }))
    .expect("receipt rebase request");
    request
        .validate_limits(McpLimitPolicy::DEFAULT)
        .expect("receipt rebase limits");
    let (request, consistency, options, expected_repository_id) = request.into_parts();
    assert_eq!(request.receipt_id, "r0000000000000001");
    assert_eq!(request.max_samples_per_outcome, Some(0));
    assert_eq!(consistency, IndexConsistency::ReconcileWorkingTree);
    assert_eq!(options.max_response_tokens(), Some(1_000));
    assert_eq!(expected_repository_id.as_deref(), Some("repository"));

    let oversized = serde_json::from_value::<ReceiptRebaseMcpRequest>(serde_json::json!({
        "receipt_id": "r0000000000000001",
        "max_samples_per_outcome": 17
    }))
    .expect("schema-independent oversized request");
    assert!(matches!(
        oversized.validate_limits(McpLimitPolicy::DEFAULT),
        Err(crate::Error::RequestLimitExceeded {
            field: "max_samples_per_outcome",
            requested: 17,
            limit: MAX_RECEIPT_REBASE_SAMPLES_PER_OUTCOME,
        })
    ));
}

#[test]
fn history_operation_maps_to_the_service_request() {
    let request = serde_json::from_value::<HistoryMcpRequest>(serde_json::json!({
        "operation": {
            "kind": "diff_symbol",
            "path": "src/lib.rs",
            "symbol": {"name": " Services ", "parent": " "},
            "base_revision": "main~1",
            "head_revision": "main",
            "max_tokens": 500
        }
    }))
    .expect("history request");
    request
        .validate_limits(McpLimitPolicy::DEFAULT)
        .expect("history limits");
    let (call, _, _) = request.into_parts().expect("history parts");
    let HistoryMcpCall::Single(request) = call else {
        panic!("expected single-symbol history call");
    };
    assert_eq!(request.max_tokens, Some(500));
    assert!(matches!(
        request.operation,
        HistoryOperation::DiffSymbol {
            path,
            symbol,
            base_revision,
            head_revision,
        } if path == "src/lib.rs"
            && symbol == "Services"
            && base_revision == "main~1"
            && head_revision == "main"
    ));
}

#[test]
fn diff_symbols_history_maps_targets_cursor_and_response_budget() {
    let request = serde_json::from_value::<HistoryMcpRequest>(serde_json::json!({
        "operation": {
            "kind": "diff_symbols",
            "targets": [
                {
                    "path": "src/old.rs",
            "symbol": {"name": "old_name"},
                    "head_path": "src/new.rs",
                    "head_symbol": {"name": "new_name"}
                }
            ],
            "base_revision": "main~1",
            "head_revision": "main",
            "max_results": 1,
            "max_tokens": 500,
            "max_response_tokens": 900,
            "cursor": "history-cursor"
        }
    }))
    .expect("batched history request");
    request
        .validate_limits(McpLimitPolicy::DEFAULT)
        .expect("batched history limits");
    let (call, options, _) = request.into_parts().expect("batched history parts");
    assert_eq!(options.max_response_tokens(), Some(900));
    let HistoryMcpCall::DiffSymbols(request) = call else {
        panic!("expected batched-symbol history call");
    };
    assert_eq!(request.base_revision, "main~1");
    assert_eq!(request.head_revision, "main");
    assert_eq!(request.max_results, Some(1));
    assert_eq!(request.max_tokens, Some(500));
    assert_eq!(request.cursor.as_deref(), Some("history-cursor"));
    assert_eq!(request.targets.len(), 1);
    assert_eq!(request.targets[0].path, "src/old.rs");
    assert_eq!(request.targets[0].symbol, "old_name");
    assert_eq!(request.targets[0].head_path.as_deref(), Some("src/new.rs"));
    assert_eq!(request.targets[0].head_symbol.as_deref(), Some("new_name"));

    let oversized_page = serde_json::from_value::<HistoryMcpRequest>(serde_json::json!({
        "operation": {
            "kind": "diff_symbols",
            "targets": [{"path": "src/lib.rs", "symbol": {"name": "item"}}],
            "base_revision": "main~1",
            "head_revision": "main",
            "max_results": crate::services::MAX_DIFF_SYMBOL_RESULTS + 1
        }
    }))
    .expect("structurally valid oversized page");
    assert!(matches!(
        oversized_page.validate_limits(McpLimitPolicy::DEFAULT),
        Err(crate::Error::RequestLimitExceeded {
            field: "max_results",
            requested,
            limit: crate::services::MAX_DIFF_SYMBOL_RESULTS,
        }) if requested == crate::services::MAX_DIFF_SYMBOL_RESULTS + 1
    ));

    let single_with_cursor = serde_json::from_value::<HistoryMcpRequest>(serde_json::json!({
        "operation": {
            "kind": "read_symbol",
            "path": "src/lib.rs",
            "symbol": {"name": "item"},
            "revision": "main"
        },
        "cursor": "not-valid-here"
    }));
    assert!(single_with_cursor.is_err());
}

#[test]
fn json_operation_maps_to_the_service_request() {
    let request = serde_json::from_value::<JsonMcpRequest>(serde_json::json!({
        "operation": {
            "kind": "numeric_summary",
            "path": "artifacts/results.json",
            "selector": {
                "kind": "jmespath",
                "expression": "graph_index.corpora[].cold_index_ms"
            },
            "max_items": 500
        }
    }))
    .expect("JSON request");
    request
        .validate_limits(McpLimitPolicy::DEFAULT)
        .expect("JSON limits");
    let (request, _, execution, _) = request.into_parts();
    assert_eq!(request.max_items, Some(500));
    assert!(request.cursor.is_none());
    assert_eq!(execution, JsonExecutionOptions::mcp(None));
    assert!(matches!(
        request.operation,
        JsonOperation::NumericSummary {
            path,
            selector: Some(JsonSelector::Jmespath { expression }),
        } if path == "artifacts/results.json" && expression == "graph_index.corpora[].cold_index_ms"
    ));

    let request = serde_json::from_value::<JsonMcpRequest>(serde_json::json!({
        "operation": {
            "kind": "query",
            "path": "artifacts/results.json",
            "projection": "keys",
            "depth": 1,
            "cursor": "j2:source:query:2"
        }
    }))
    .expect("paged JSON request");
    let (request, _, execution, _) = request.into_parts();
    assert_eq!(request.cursor.as_deref(), Some("j2:source:query:2"));
    assert_eq!(execution, JsonExecutionOptions::mcp(Some(1)));
    assert!(matches!(
        request.operation,
        JsonOperation::Query {
            projection: JsonProjection::Keys,
            ..
        }
    ));

    let invalid = serde_json::from_value::<JsonMcpRequest>(serde_json::json!({
        "operation": {"kind": "keys", "path": "artifacts/results.json"}
    }))
    .expect_err("keys must be a query projection, not an operation");
    let message = invalid.to_string();
    assert!(message.contains("unknown variant") && message.contains("query"));

    let invalid_outer_limit = serde_json::from_value::<JsonMcpRequest>(serde_json::json!({
        "operation": {
            "kind": "numeric_summary",
            "path": "artifacts/results.json"
        },
        "max_tokens": 1
    }))
    .expect_err("operation controls must not be accepted at the outer level");
    assert!(invalid_outer_limit.to_string().contains("unknown field"));
}

#[test]
fn outline_cursor_maps_to_the_service_request() {
    let request = serde_json::from_value::<OutlineMcpRequest>(serde_json::json!({
        "paths": ["src/lib.rs"],
        "cursor": "12:outline:34:0000000000000000"
    }))
    .expect("outline request");
    let (request, _, _, _, _) = request.into_parts();

    assert_eq!(
        request.cursor.as_deref(),
        Some("12:outline:34:0000000000000000")
    );
}

#[test]
fn compact_projections_map_to_service_requests() {
    let files = serde_json::from_value::<FilesMcpRequest>(serde_json::json!({
        "operation": {"kind": "tree"}
    }))
    .expect("default files projection");
    let (_, projection, _, _, _) = files.into_parts();
    assert_eq!(projection, FilesMcpProjection::Full);

    let files = serde_json::from_value::<FilesMcpRequest>(serde_json::json!({
        "operation": {"kind": "find", "query": "service", "projection": "paths"}
    }))
    .expect("path projection");
    let (_, projection, _, _, _) = files.into_parts();
    assert_eq!(projection, FilesMcpProjection::Paths);

    let search = serde_json::from_value::<SearchMcpRequest>(serde_json::json!({
        "operation": {"kind": "auto", "query": "Services"}
    }))
    .expect("default search projection");
    let (_, output, _, _, _) = search.into_parts();
    assert_eq!(output, SearchMcpOutput::Full);

    let search = serde_json::from_value::<SearchMcpRequest>(serde_json::json!({
        "operation": {"kind": "auto", "query": "Services", "projection": "grouped"}
    }))
    .expect("grouped projection");
    let (_, output, _, _, _) = search.into_parts();
    assert_eq!(output, SearchMcpOutput::Grouped);

    let search = serde_json::from_value::<SearchMcpRequest>(serde_json::json!({
        "operation": {"kind": "identifier", "query": "Services", "projection": "compact"}
    }))
    .expect("compact projection");
    search
        .validate_limits(McpLimitPolicy::DEFAULT)
        .expect("valid compact projection");
    let (_, output, _, _, _) = search.into_parts();
    assert_eq!(output, SearchMcpOutput::Compact);

    let search = serde_json::from_value::<SearchMcpRequest>(serde_json::json!({
        "operation": {
            "kind": "text",
            "query": "Services",
            "all_occurrences": true,
            "coordinates_only": true
        }
    }))
    .expect("coordinates-only occurrence projection");
    search
        .validate_limits(McpLimitPolicy::DEFAULT)
        .expect("valid occurrence projection");
    let (_, output, _, _, _) = search.into_parts();
    assert_eq!(
        output,
        SearchMcpOutput::Occurrences(SearchOccurrenceOutput::Coordinates)
    );

    let invalid = serde_json::from_value::<SearchMcpRequest>(serde_json::json!({
        "operation": {"kind": "text", "query": "Services", "coordinates_only": true}
    }))
    .expect("structurally valid search request");
    assert!(matches!(
        invalid.validate_limits(McpLimitPolicy::DEFAULT),
        Err(crate::Error::InvalidInput {
            field: "coordinates_only",
            ..
        })
    ));

    for mode in ["auto", "identifier", "symbol", "reference"] {
        let invalid = serde_json::from_value::<SearchMcpRequest>(serde_json::json!({
            "operation": {"kind": mode, "query": "Services", "all_occurrences": true}
        }))
        .expect("structurally valid exhaustive search request");
        assert!(matches!(
            invalid.validate_limits(McpLimitPolicy::DEFAULT),
            Err(crate::Error::InvalidSearchOptions {
                field: "all_occurrences",
                ..
            })
        ));
    }
    let invalid = serde_json::from_value::<SearchMcpRequest>(serde_json::json!({
        "operation": {"kind": "auto", "query": "Services", "all_occurrences": true}
    }))
    .expect("structurally valid exhaustive search request with default mode");
    assert!(matches!(
        invalid.validate_limits(McpLimitPolicy::DEFAULT),
        Err(crate::Error::InvalidSearchOptions {
            field: "all_occurrences",
            ..
        })
    ));

    let outline = serde_json::from_value::<OutlineMcpRequest>(serde_json::json!({
        "paths": ["src/services.rs"]
    }))
    .expect("default outline projection");
    let (_, projection, _, _, _) = outline.into_parts();
    assert_eq!(projection, OutlineMcpProjection::Full);

    let outline = serde_json::from_value::<OutlineMcpRequest>(serde_json::json!({
        "paths": ["src/services.rs"],
        "projection": "signatures"
    }))
    .expect("signature projection");
    let (_, projection, _, _, _) = outline.into_parts();
    assert_eq!(projection, OutlineMcpProjection::Signatures);
}

#[test]
fn mcp_request_validation_catches_cross_field_constraints() {
    let context = serde_json::from_value::<ContextMcpRequest>(serde_json::json!({
        "task": "find answer",
        "minimum_fragments_per_focus_path": 1
    }))
    .expect("structurally valid context request");
    assert!(matches!(
        context.validate_limits(McpLimitPolicy::DEFAULT),
        Err(crate::Error::InvalidInput {
            field: "focus paths",
            ..
        })
    ));

    let search = serde_json::from_value::<SearchMcpRequest>(serde_json::json!({
        "operation": {
            "kind": "auto",
            "query": "answer",
            "query_receipt": {"kind": "record"}
        }
    }))
    .expect("structurally valid search request");
    assert!(matches!(
        search.validate_limits(McpLimitPolicy::DEFAULT),
        Err(crate::Error::InvalidInput {
            field: "query_receipt",
            ..
        })
    ));

    let read = serde_json::from_value::<ReadMcpRequest>(serde_json::json!({
        "path": "README.md",
        "target": {"kind": "lines", "start": 20, "end": 10}
    }))
    .expect("structurally valid read request");
    assert!(matches!(
        read.validate_limits(McpLimitPolicy::DEFAULT),
        Err(crate::Error::InvalidInput { field: "lines", .. })
    ));
}

#[test]
fn history_mcp_uses_configured_result_cap() {
    let request = serde_json::from_value::<HistoryMcpRequest>(serde_json::json!({
        "operation": {
            "kind": "symbol_log",
            "path": "src/lib.rs",
            "symbol": {"name": "Services"},
            "max_results": 3
        }
    }))
    .expect("history request");
    let limits = McpLimitPolicy {
        max_results: 2,
        ..McpLimitPolicy::DEFAULT
    };
    assert!(matches!(
        request.validate_limits(limits),
        Err(crate::Error::RequestLimitExceeded {
            field: "max_results",
            requested: 3,
            limit: 2
        })
    ));
}

#[test]
fn search_query_preserves_significant_whitespace() {
    let request = serde_json::from_value::<SearchMcpRequest>(serde_json::json!({
        "operation": {"kind": "text", "query": "  exact text  "}
    }))
    .expect("whitespace-surrounded search query");
    let (request, _, _, _, _) = request.into_parts();

    assert_eq!(request.query, "  exact text  ");
}

#[test]
fn read_description_example_parses_successfully() {
    let request = serde_json::from_value::<ReadMcpRequest>(serde_json::json!({
        "path": "README.md",
        "target": {"kind": "heading", "name": "Installation"}
    }))
    .expect("read description example must parse");
    let (request, _, _, _) = request.into_parts();
    assert_eq!(request.heading.as_deref(), Some("Installation"));
}

#[test]
fn history_description_example_parses_successfully() {
    let request = serde_json::from_value::<HistoryMcpRequest>(serde_json::json!({
        "operation": {
            "kind": "symbol_log",
            "path": "src/services.rs",
            "symbol": {"name": "meta", "parent": "Services"},
            "revision": "HEAD"
        }
    }))
    .expect("history description example must parse");
    assert!(request.validate_limits(McpLimitPolicy::DEFAULT).is_ok());
}

#[test]
fn read_target_lines_variant_parses_and_maps() {
    let request = serde_json::from_value::<ReadMcpRequest>(serde_json::json!({
        "path": "src/main.rs",
        "target": {"kind": "lines", "start": 1, "end": 50}
    }))
    .expect("lines target must parse");
    let (request, _, _, _) = request.into_parts();
    assert_eq!(request.start_line, Some(1));
    assert_eq!(request.end_line, Some(50));
}

#[test]
fn read_description_does_not_offer_range_target_kind() {
    let tools = LeanTokenMcp::tool_router().list_all();
    let read = tools
        .into_iter()
        .find(|tool| tool.name == "read")
        .expect("read tool");
    let description = read.description.expect("read description");
    assert!(
        !description.contains("\"range\""),
        "read description must not imply a range target kind: {description}"
    );
    assert!(
        description.contains("line range"),
        "read description should mention line range: {description}"
    );
}

#[test]
fn outline_description_does_not_offer_range_to_read() {
    let tools = LeanTokenMcp::tool_router().list_all();
    let outline = tools
        .into_iter()
        .find(|tool| tool.name == "outline")
        .expect("outline tool");
    let description = outline.description.expect("outline description");
    assert!(
        description.contains("line range to read"),
        "outline description should guide to line range: {description}"
    );
}

#[test]
fn history_description_example_uses_symbol_identity_object() {
    let tools = LeanTokenMcp::tool_router().list_all();
    let history = tools
        .into_iter()
        .find(|tool| tool.name == "history")
        .expect("history tool");
    let description = history.description.expect("history description");
    assert!(
        description.contains("\"symbol\":{\"name\":"),
        "history description example must use a SymbolIdentity object: {description}"
    );
}
