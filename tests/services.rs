use std::time::Instant;

use leantoken::{
    Config, ContextRequest, ContextSignalPolicy, ContextWorkflow, DiffSymbolsIncompleteReason,
    DiffSymbolsRequest, DiffSymbolsStatus, DiffSymbolsTarget, Error, FileOperation, FilesRequest,
    Freshness, HandoffManifestRequest, HandoffValidation, HandoffValidationStatus,
    HandoffWorkingTreeState, HistoryOperation, HistoryRequest, IndexConsistency, IndexState,
    JsonIncompleteReason, JsonOperation, JsonProjection, JsonRequest, JsonSelector, OutlineRequest,
    ReadDeltaFallback, ReadDeltaOutcome, ReadRequest, ReadStatus, SearchMode, SearchRequest,
    TokenAccountingOperation, TokenSavingsOperation, WorkflowEvidence,
    coordination::IndexCoordination,
    services::{ServiceCallOptions, Services},
    tokens::Tokenizer,
};
use leantoken::{
    DiffConfigurationChangeKind, DiffOwnerTestStatus, DiffSymbolChangeKind, DiffSymbolModification,
};
use tokio_util::sync::CancellationToken;

macro_rules! assert_response_token_accounting {
    ($response:expr, $tokenizer:expr) => {{
        let response = &$response;
        let tokenizer = $tokenizer;
        assert_eq!(response.meta.source_tokens, response.meta.emitted_tokens);
        assert_eq!(response.meta.tokenizer, tokenizer.name());
        assert_eq!(response.meta.token_count_exact, tokenizer.is_exact());
        assert!(response.meta.protocol_tokens > 0);
        assert_eq!(
            response.meta.total_response_tokens,
            response.meta.source_tokens
                + response.meta.protocol_tokens
                + response.meta.path_and_metadata_tokens
        );
        assert_eq!(
            response.meta.payload_tokens,
            response.meta.total_response_tokens
        );
        assert!(response.meta.payload_tokens > 0);

        let final_payload =
            serde_json::to_string(response).expect("serialize final response payload");
        assert_eq!(
            tokenizer.count(&final_payload),
            response.meta.total_response_tokens,
            "accounting must include its own serialized fields"
        );
    }};
}

async fn fixture() -> (tempfile::TempDir, Services) {
    let root = tempfile::tempdir().expect("temporary repository");
    std::fs::create_dir(root.path().join("src")).expect("create src");
    std::fs::write(
        root.path().join("src/lib.rs"),
        "pub fn greet(name: &str) -> String {\n    format!(\"hello {name}\")\n}\n\npub fn caller() {\n    let _ = greet(\"agent\");\n}\n",
    )
    .expect("write fixture");
    let config =
        Config::discover(root.path(), Some(root.path().join("index.sqlite"))).expect("config");
    let services = Services::open(config).expect("services");
    services.index(false).await.expect("index fixture");
    (root, services)
}

#[tokio::test]
async fn retrieval_call_options_enforce_final_service_response_bounds() {
    let (root, services) = fixture().await;
    std::fs::write(
        root.path().join("data.json"),
        r#"{"alpha":{"escaped":"line\nvalue"},"βeta":[1,2,3]}"#,
    )
    .expect("write JSON fixture");
    let options = ServiceCallOptions::new().with_max_response_tokens(32_000);

    let files = services
        .files_with_options(
            FilesRequest {
                operation: FileOperation::Tree,
                path: None,
                query: None,
                pattern: None,
                max_results: Some(10),
                cursor: None,
                depth: Some(2),
            },
            options,
        )
        .await
        .expect("bounded files");
    let search = services
        .search_with_options(
            SearchRequest {
                query: "greet".into(),
                mode: SearchMode::Auto,
                include_paths: Vec::new(),
                exclude_paths: Vec::new(),
                focus_paths: Vec::new(),
                max_results: Some(10),
                max_tokens: None,
                context_lines: None,
                case_sensitive: false,
                all_occurrences: false,
                prefer_structural: false,
                receipt_id: None,
                cursor: None,
            },
            options,
        )
        .await
        .expect("bounded search");
    let outline = services
        .outline_with_options(
            OutlineRequest {
                paths: vec!["src/lib.rs".into()],
                symbol_name: None,
                symbol_kind: None,
                max_results: Some(20),
                max_tokens: None,
                receipt_id: None,
                cursor: None,
            },
            options,
        )
        .await
        .expect("bounded outline");
    let read = services
        .read_with_options(
            ReadRequest {
                path: "src/lib.rs".into(),
                start_line: Some(1),
                end_line: Some(6),
                symbol: None,
                heading: None,
                heading_occurrence: None,
                continuation_cursor: None,
                max_tokens: None,
                expected_hash: None,
                delta: false,
                receipt_id: None,
            },
            options,
        )
        .await
        .expect("bounded read");
    let json = services
        .json_with_options(
            JsonRequest {
                operation: JsonOperation::Query {
                    path: "data.json".into(),
                    selector: None,
                    projection: JsonProjection::Keys,
                },
                max_tokens: Some(1_000),
                max_items: Some(100),
                array_sample_size: None,
                cursor: None,
            },
            options,
        )
        .await
        .expect("bounded JSON");

    for total in [
        files.meta.total_response_tokens,
        search.meta.total_response_tokens,
        outline.meta.total_response_tokens,
        read.meta.total_response_tokens,
        json.meta.total_response_tokens,
    ] {
        assert!(total <= 32_000);
    }
    for payload in [
        serde_json::to_string(&files).expect("serialize files"),
        serde_json::to_string(&search).expect("serialize search"),
        serde_json::to_string(&outline).expect("serialize outline"),
        serde_json::to_string(&read).expect("serialize read"),
        serde_json::to_string(&json).expect("serialize JSON"),
    ] {
        assert!(Tokenizer::default().count(&payload) <= 32_000);
    }

    let validation_request = FilesRequest {
        operation: FileOperation::Tree,
        path: None,
        query: None,
        pattern: None,
        max_results: Some(1),
        cursor: None,
        depth: None,
    };
    let invalid = services
        .files_with_options(
            validation_request.clone(),
            ServiceCallOptions::new().with_max_response_tokens(0),
        )
        .await
        .expect_err("zero response limit must fail before retrieval");
    assert!(matches!(
        invalid,
        Error::InvalidInput {
            field: "max_response_tokens",
            ..
        }
    ));
    let oversized = services
        .files_with_options(
            validation_request,
            ServiceCallOptions::new().with_max_response_tokens(32_001),
        )
        .await
        .expect_err("server maximum must apply to service callers");
    assert!(matches!(
        oversized,
        Error::RequestLimitExceeded {
            field: "max_response_tokens",
            requested: 32_001,
            limit: 32_000
        }
    ));
    let too_small = services
        .files_with_options(
            FilesRequest {
                operation: FileOperation::Tree,
                path: None,
                query: None,
                pattern: None,
                max_results: Some(1),
                cursor: None,
                depth: None,
            },
            ServiceCallOptions::new().with_max_response_tokens(1),
        )
        .await
        .expect_err("mandatory files skeleton must fail loudly");
    assert!(matches!(
        too_small,
        Error::RequestLimitExceeded {
            field: "max_response_tokens",
            limit: 1,
            ..
        }
    ));
}

#[tokio::test]
async fn files_response_budget_uses_a_resumable_deterministic_prefix() {
    let (root, services) = fixture().await;
    for index in 0..24 {
        std::fs::write(
            root.path()
                .join("src")
                .join(format!("長い名前_{index:02}_escaped_quote.rs")),
            format!("pub const VALUE_{index}: usize = {index};\n"),
        )
        .expect("write path fixture");
    }
    services.index(false).await.expect("index added paths");
    let one_entry_request = FilesRequest {
        operation: FileOperation::Tree,
        path: Some("src".into()),
        query: None,
        pattern: None,
        max_results: Some(1),
        cursor: None,
        depth: Some(1),
    };
    let minimum = services
        .files(one_entry_request.clone())
        .await
        .expect("one-entry files page");
    let exact_limit = minimum.meta.total_response_tokens;
    let exact = services
        .files_with_options(
            one_entry_request.clone(),
            ServiceCallOptions::new().with_max_response_tokens(exact_limit),
        )
        .await
        .expect("exact one-entry response limit");
    assert_eq!(exact.entries.len(), 1);
    assert_eq!(exact.meta.total_response_tokens, exact_limit);
    let below_minimum = services
        .files_with_options(
            one_entry_request,
            ServiceCallOptions::new().with_max_response_tokens(exact_limit - 1),
        )
        .await
        .expect_err("one token below the resumable skeleton");
    assert!(matches!(
        below_minimum,
        Error::RequestLimitExceeded {
            field: "max_response_tokens",
            ..
        }
    ));

    let request = FilesRequest {
        operation: FileOperation::Tree,
        path: Some("src".into()),
        query: None,
        pattern: None,
        max_results: Some(100),
        cursor: None,
        depth: Some(1),
    };
    let full = services.files(request.clone()).await.expect("full files page");
    assert!(full.entries.len() > 10);
    let limit = full.meta.total_response_tokens.saturating_sub(600);
    let bounded = services
        .files_with_options(
            request.clone(),
            ServiceCallOptions::new().with_max_response_tokens(limit),
        )
        .await
        .expect("fit files prefix");
    assert!(bounded.meta.total_response_tokens <= limit);
    assert!(bounded.entries.len() < full.entries.len());
    assert!(bounded.meta.next_cursor.is_some());
    assert_eq!(
        bounded
            .entries
            .iter()
            .map(|entry| &entry.path)
            .collect::<Vec<_>>(),
        full.entries[..bounded.entries.len()]
            .iter()
            .map(|entry| &entry.path)
            .collect::<Vec<_>>(),
        "fitting must preserve the original deterministic prefix"
    );

    let continuation = services
        .files(FilesRequest {
            cursor: bounded.meta.next_cursor.clone(),
            ..request
        })
        .await
        .expect("continue fitted files page");
    assert_eq!(
        continuation.entries.first().map(|entry| &entry.path),
        full.entries
            .get(bounded.entries.len())
            .map(|entry| &entry.path)
    );
}

#[tokio::test]
async fn json_keys_response_budget_preserves_cursor_completeness() {
    let (root, services) = fixture().await;
    let object = (0..80)
        .map(|index| {
            (
                format!("escaped_長い_key_{index:03}"),
                serde_json::json!({"nested": index}),
            )
        })
        .collect::<serde_json::Map<_, _>>();
    std::fs::write(
        root.path().join("keys.json"),
        serde_json::to_vec(&serde_json::Value::Object(object)).expect("serialize fixture"),
    )
    .expect("write JSON fixture");
    let request = JsonRequest {
        operation: JsonOperation::Query {
            path: "keys.json".into(),
            selector: None,
            projection: JsonProjection::Keys,
        },
        max_tokens: Some(8_000),
        max_items: Some(1_000),
        array_sample_size: None,
        cursor: None,
    };
    let full = services.json(request.clone()).await.expect("full keys page");
    let full_items = full.returned_items.expect("keys item count");
    assert!(full_items > 50);
    let limit = full.meta.total_response_tokens.saturating_sub(600);
    let bounded = services
        .json_with_options(
            request.clone(),
            ServiceCallOptions::new().with_max_response_tokens(limit),
        )
        .await
        .expect("fit keys page");
    assert!(bounded.meta.total_response_tokens <= limit);
    assert!(bounded.returned_items.expect("bounded item count") < full_items);
    assert!(!bounded.result_complete);
    assert_eq!(
        bounded.incomplete_reason,
        Some(JsonIncompleteReason::MaxTokens)
    );
    let cursor = bounded.meta.next_cursor.clone().expect("continuation cursor");
    let continuation = services
        .json(JsonRequest {
            cursor: Some(cursor),
            ..request
        })
        .await
        .expect("continue keys page");
    assert_eq!(
        bounded
            .returned_items
            .expect("bounded item count")
            .saturating_add(continuation.remaining_items.unwrap_or_default())
            .saturating_add(continuation.returned_items.unwrap_or_default()),
        full.total_items.expect("total keys"),
    );
}

#[tokio::test]
async fn read_response_budget_reduces_source_without_skipping_continuation() {
    let (root, services) = fixture().await;
    let source = (1..=120)
        .map(|line| format!("pub const 長い名前_{line:03}: &str = \"escaped-{line}\";\n"))
        .collect::<String>();
    std::fs::write(root.path().join("src/big.rs"), source).expect("write read fixture");
    services.index(false).await.expect("index read fixture");
    let request = ReadRequest {
        path: "src/big.rs".into(),
        start_line: Some(1),
        end_line: Some(120),
        symbol: None,
        heading: None,
        heading_occurrence: None,
        continuation_cursor: None,
        max_tokens: Some(2_000),
        expected_hash: None,
        delta: false,
        receipt_id: None,
    };
    let full = services.read(request.clone()).await.expect("full read");
    let limit = full.meta.total_response_tokens.saturating_sub(500);
    let bounded = services
        .read_with_options(
            request,
            ServiceCallOptions::new().with_max_response_tokens(limit),
        )
        .await
        .expect("fit read response");
    assert!(bounded.meta.total_response_tokens <= limit);
    assert!(bounded.truncated);
    let next_start_line = bounded.next_start_line.expect("next line");
    let cursor = bounded
        .continuation_cursor
        .clone()
        .expect("continuation cursor");
    let continuation = services
        .read(ReadRequest {
            path: "src/big.rs".into(),
            start_line: None,
            end_line: None,
            symbol: None,
            heading: None,
            heading_occurrence: None,
            continuation_cursor: Some(cursor),
            max_tokens: Some(2_000),
            expected_hash: None,
            delta: false,
            receipt_id: None,
        })
        .await
        .expect("continue bounded read");
    assert_eq!(continuation.returned_start_line, next_start_line);
}

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
    assert!(matches!(
        error,
        Error::InvalidInput {
            field: "plan_only",
            ..
        }
    ));
    assert_eq!(
        services
            .status()
            .await
            .expect("status after invalid handoff")
            .repository_generation,
        generation,
        "static handoff errors must not reconcile the index"
    );

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

#[tokio::test]
async fn search_applies_path_filters_before_candidate_limits() {
    let root = tempfile::tempdir().expect("temporary repository");
    let source = "pub fn shared_target() {\n    let shared_lexical_needle = 1;\n}\npub fn caller() { shared_target(); }\n";
    for index in 0..10 {
        std::fs::write(root.path().join(format!("a{index:02}.rs")), source)
            .expect("write excluded source");
    }
    std::fs::write(root.path().join("z_included.rs"), source).expect("write included source");
    let config =
        Config::discover(root.path(), Some(root.path().join("index.sqlite"))).expect("config");
    let services = Services::open(config).expect("services");
    services.index(false).await.expect("index fixture");

    for (mode, query) in [
        (SearchMode::Symbol, "shared_target"),
        (SearchMode::Reference, "shared_target"),
        (SearchMode::Text, "shared_lexical_needle"),
    ] {
        let response = services
            .search(SearchRequest {
                query: query.into(),
                mode,
                case_sensitive: true,
                all_occurrences: false,
                prefer_structural: false,
                include_paths: vec!["z_included.rs".into()],
                exclude_paths: Vec::new(),
                focus_paths: Vec::new(),
                max_results: Some(1),
                max_tokens: Some(200),
                context_lines: Some(0),
                receipt_id: None,
                cursor: None,
            })
            .await
            .expect("filtered search");
        assert_eq!(response.hits.len(), 1, "{mode:?}");
        assert_eq!(response.hits[0].path, "z_included.rs", "{mode:?}");

        let response = services
            .search(SearchRequest {
                query: query.into(),
                mode,
                case_sensitive: true,
                all_occurrences: false,
                prefer_structural: false,
                include_paths: Vec::new(),
                exclude_paths: vec!["a*.rs".into()],
                focus_paths: Vec::new(),
                max_results: Some(1),
                max_tokens: Some(200),
                context_lines: Some(0),
                receipt_id: None,
                cursor: None,
            })
            .await
            .expect("exclusion-filtered search");
        assert_eq!(response.hits.len(), 1, "{mode:?}");
        assert_eq!(response.hits[0].path, "z_included.rs", "{mode:?}");
    }
}

#[tokio::test]
async fn exhaustive_text_search_returns_each_occurrence_with_exact_total_and_pagination() {
    let root = tempfile::tempdir().expect("temporary repository");
    let source = "const first = \"audit_key audit_key\";\nconst second = \"audit_key\";\n";
    std::fs::write(root.path().join("occurrences.js"), source).expect("write source");
    std::fs::write(root.path().join("excluded.js"), "const value = 'audit_key';\n")
        .expect("write excluded source");
    let config =
        Config::discover(root.path(), Some(root.path().join("index.sqlite"))).expect("config");
    let services = Services::open(config).expect("services");
    services.index(false).await.expect("index fixture");
    let request = SearchRequest {
        query: "audit_key".into(),
        mode: SearchMode::Text,
        include_paths: vec!["occurrences.js".into()],
        exclude_paths: Vec::new(),
        focus_paths: Vec::new(),
        max_results: Some(2),
        max_tokens: Some(1_000),
        context_lines: Some(0),
        case_sensitive: true,
        all_occurrences: true,
        prefer_structural: false,
        receipt_id: None,
        cursor: None,
    };

    let first = services
        .search(request.clone())
        .await
        .expect("first occurrence page");

    assert_eq!(first.occurrences_total, Some(3));
    assert_eq!(first.occurrences_returned, 2);
    assert_eq!(first.hits.len(), 2);
    let expected_offsets = source
        .match_indices("audit_key")
        .map(|(start, matched)| (start, start + matched.len()))
        .collect::<Vec<_>>();
    let first_offsets = first
        .hits
        .iter()
        .map(|hit| {
            let occurrence = hit.occurrence.as_ref().expect("exact occurrence");
            (occurrence.start_byte, occurrence.end_byte)
        })
        .collect::<Vec<_>>();
    assert_eq!(first_offsets, expected_offsets[..2]);

    let mut token_limited = request.clone();
    token_limited.max_tokens = Some(1);
    let limited = services
        .search(token_limited)
        .await
        .expect("token-limited occurrence page");
    assert_eq!(limited.occurrences_total, Some(3));
    assert_eq!(limited.occurrences_returned, 0);
    assert!(limited.hits.is_empty());
    assert!(limited.meta.next_cursor.is_some());

    let mut next = request;
    next.cursor = first.meta.next_cursor;
    let second = services
        .search(next)
        .await
        .expect("second occurrence page");

    assert_eq!(second.occurrences_total, Some(3));
    assert_eq!(second.occurrences_returned, 1);
    assert!(second.meta.next_cursor.is_none());
    let occurrence = second.hits[0]
        .occurrence
        .as_ref()
        .expect("exact occurrence");
    assert_eq!(
        (occurrence.start_byte, occurrence.end_byte),
        expected_offsets[2]
    );
    assert_eq!(occurrence.start_line, 2);
    assert_eq!(occurrence.end_line, 2);

    let mut short_query = search_limit_request(Some(10), Some(1_000), Some(0));
    short_query.query = "it".into();
    short_query.mode = SearchMode::Text;
    short_query.include_paths = vec!["occurrences.js".into()];
    short_query.case_sensitive = true;
    short_query.all_occurrences = true;
    let short = services
        .search(short_query)
        .await
        .expect("short substring occurrence search");
    assert_eq!(short.occurrences_total, Some(3));
    assert_eq!(short.occurrences_returned, 3);
}

#[tokio::test]
async fn exhaustive_occurrence_search_requires_text_or_regex_mode() {
    let (_root, services) = fixture().await;
    let mut request = search_limit_request(Some(20), Some(1_000), Some(0));
    request.mode = SearchMode::Auto;
    request.all_occurrences = true;

    let error = services
        .search(request)
        .await
        .expect_err("auto mode must not claim exhaustive occurrences");

    assert!(matches!(
        error,
        Error::InvalidInput {
            field: "all occurrences",
            ..
        }
    ));

    let mut prefer = search_limit_request(Some(20), Some(1_000), Some(0));
    prefer.mode = SearchMode::Text;
    prefer.prefer_structural = true;
    let error = services
        .search(prefer)
        .await
        .expect_err("text mode must not accept structural preference");
    assert!(matches!(
        error,
        Error::InvalidInput {
            field: "prefer structural",
            ..
        }
    ));
}

#[tokio::test]
async fn identifier_search_merges_definition_channels_and_reports_coverage() {
    let root = tempfile::tempdir().expect("temporary repository");
    std::fs::write(
        root.path().join("search.rs"),
        "fn shared_identifier() {}\nfn caller() { shared_identifier(); }\n",
    )
    .expect("source");
    std::fs::write(
        root.path().join("other.rs"),
        "fn other_caller() { shared_identifier(); }\n",
    )
    .expect("second source");
    let config =
        Config::discover(root.path(), Some(root.path().join("index.sqlite"))).expect("config");
    let services = Services::open(config).expect("services");
    services.index(false).await.expect("index fixture");

    let response = services
        .search(SearchRequest {
            query: "shared_identifier".into(),
            mode: SearchMode::Identifier,
            include_paths: Vec::new(),
            exclude_paths: Vec::new(),
            focus_paths: Vec::new(),
            max_results: Some(1),
            max_tokens: Some(1_000),
            context_lines: Some(1),
            case_sensitive: true,
            all_occurrences: false,
            prefer_structural: true,
            receipt_id: None,
            cursor: None,
        })
        .await
        .expect("identifier search");

    assert_eq!(response.hits.len(), 1);
    let merged = &response.hits[0];
    assert_eq!(merged.match_kind, "symbol");
    assert!(merged.match_kinds.iter().any(|kind| kind == "symbol"));
    assert!(merged.match_kinds.iter().any(|kind| kind == "text"));
    assert_eq!(merged.normalized_score, 1.0);
    assert_eq!(response.coverage.definitions.total, 1);
    assert_eq!(response.coverage.definitions.returned, 1);
    assert_eq!(response.coverage.definitions.truncated, 0);
    assert!(response.coverage.references.total >= 2);
    assert_eq!(response.coverage.references.returned, 1);
    assert_eq!(
        response.coverage.references.truncated,
        response.coverage.references.total - 1
    );
    assert!(response.coverage.text_matches.total >= 1);
    assert_eq!(response.coverage.text_matches.returned, 1);
    assert_eq!(
        response.coverage.text_matches.total,
        response.coverage.text_matches.returned + response.coverage.text_matches.truncated
    );
}

#[tokio::test]
async fn exhaustive_regex_search_counts_repeated_matches_in_one_chunk() {
    let root = tempfile::tempdir().expect("temporary repository");
    let source = "const values = ['item1', 'item22', 'item333'];\n";
    std::fs::write(root.path().join("regex.js"), source).expect("write source");
    let config =
        Config::discover(root.path(), Some(root.path().join("index.sqlite"))).expect("config");
    let services = Services::open(config).expect("services");
    services.index(false).await.expect("index fixture");
    let response = services
        .search(SearchRequest {
            query: r"item\d+".into(),
            mode: SearchMode::Regex,
            include_paths: Vec::new(),
            exclude_paths: Vec::new(),
            focus_paths: Vec::new(),
            max_results: Some(10),
            max_tokens: Some(1_000),
            context_lines: Some(0),
            case_sensitive: true,
            all_occurrences: true,
            prefer_structural: false,
            receipt_id: None,
            cursor: None,
        })
        .await
        .expect("exhaustive regex search");

    assert_eq!(response.occurrences_total, Some(3));
    assert_eq!(response.occurrences_returned, 3);
    assert_eq!(response.hits.len(), 3);
    assert!(
        response
            .hits
            .iter()
            .all(|hit| hit.occurrence.is_some() && hit.match_kind == "regex")
    );
}

#[tokio::test]
async fn regex_candidate_plans_match_full_scan_and_report_fallback_selection() {
    let root = tempfile::tempdir().expect("temporary repository");
    for (path, source) in [
        (
            "alpha.rs",
            "const needle_value: usize = 42;\nconst marker_value: usize = 7;\n",
        ),
        ("bravo.rs", "const needle_value: usize = 7;\n"),
        ("digits.rs", "const value_123: usize = 123;\n"),
        ("negative.rs", "const unrelated: usize = 0;\n"),
    ] {
        std::fs::write(root.path().join(path), source).expect("write source");
    }
    let config =
        Config::discover(root.path(), Some(root.path().join("index.sqlite"))).expect("config");
    let services = Services::open(config).expect("services");
    services.index(false).await.expect("index fixture");

    for (pattern, expected_strategy) in [
        (
            r"needle_value\s*:\s*usize\s*=\s*42",
            leantoken::RegexCandidateStrategy::Trigram,
        ),
        (
            r"(?:needle|marker)_value",
            leantoken::RegexCandidateStrategy::Trigram,
        ),
        (
            r"(?:needle|)value",
            leantoken::RegexCandidateStrategy::Trigram,
        ),
        (
            r"(?:needle)?\d+",
            leantoken::RegexCandidateStrategy::FullScan,
        ),
        (
            r"needle|value_\d+",
            leantoken::RegexCandidateStrategy::Trigram,
        ),
        (
            r"needle|\d+",
            leantoken::RegexCandidateStrategy::FullScan,
        ),
    ] {
        let request = SearchRequest {
            query: pattern.into(),
            mode: SearchMode::Regex,
            include_paths: Vec::new(),
            exclude_paths: Vec::new(),
            focus_paths: Vec::new(),
            max_results: Some(20),
            max_tokens: Some(4_000),
            context_lines: Some(0),
            case_sensitive: true,
            all_occurrences: true,
            prefer_structural: false,
            receipt_id: None,
            cursor: None,
        };
        let optimized = services
            .search_evaluation(request.clone())
            .await
            .expect("optimized regex");
        let full_scan = services
            .search_full_scan_evaluation(request)
            .await
            .expect("full-scan regex");

        let mut optimized_response = optimized.response.clone();
        optimized_response.meta.receipt_id = None;
        let mut full_scan_response = full_scan.response.clone();
        full_scan_response.meta.receipt_id = None;
        assert_eq!(
            serde_json::to_value(optimized_response).expect("optimized JSON"),
            serde_json::to_value(full_scan_response).expect("full scan JSON"),
            "{pattern}"
        );
        assert_eq!(
            optimized.phases.regex_candidate_strategy, expected_strategy,
            "{pattern}"
        );
        assert_eq!(
            full_scan.phases.regex_candidate_strategy,
            leantoken::RegexCandidateStrategy::FullScan,
            "{pattern}"
        );
        assert_eq!(
            optimized.phases.regex_chunks_verified,
            optimized
                .phases
                .regex_candidate_chunks
                .max(optimized.phases.regex_chunks_loaded),
            "{pattern}"
        );
    }
}

#[tokio::test]
async fn regex_candidate_plan_preserves_candidate_limit_errors() {
    let root = tempfile::tempdir().expect("temporary repository");
    for index in 0..21 {
        std::fs::write(
            root.path().join(format!("match_{index:02}.rs")),
            format!("const overflow_needle_{index:02}: usize = {index};\n"),
        )
        .expect("write source");
    }
    let config =
        Config::discover(root.path(), Some(root.path().join("index.sqlite"))).expect("config");
    let services = Services::open(config).expect("services");
    services.index(false).await.expect("index fixture");
    let request = SearchRequest {
        query: "overflow_needle".into(),
        mode: SearchMode::Regex,
        include_paths: Vec::new(),
        exclude_paths: Vec::new(),
        focus_paths: Vec::new(),
        max_results: Some(1),
        max_tokens: Some(1_000),
        context_lines: Some(0),
        case_sensitive: true,
        all_occurrences: false,
        prefer_structural: false,
        receipt_id: None,
        cursor: None,
    };

    let optimized = services
        .search_evaluation(request.clone())
        .await
        .expect_err("optimized candidate cap");
    let full_scan = services
        .search_full_scan_evaluation(request)
        .await
        .expect_err("full-scan candidate cap");

    assert!(matches!(optimized, Error::LimitExceeded));
    assert!(matches!(full_scan, Error::LimitExceeded));
}

#[tokio::test]
async fn regex_candidate_plan_applies_path_scope_before_candidate_limit() {
    let root = tempfile::tempdir().expect("temporary repository");
    let included = root.path().join("included");
    std::fs::create_dir(&included).expect("create included directory");
    std::fs::write(
        included.join("match.rs"),
        "const scoped_overflow_needle: usize = 42;\n",
    )
    .expect("write source");
    let database = root.path().join("index.sqlite");
    let config = Config::discover(root.path(), Some(database.clone())).expect("config");
    let services = Services::open(config).expect("services");
    services.index(false).await.expect("index fixture");

    let mut connection = rusqlite::Connection::open(database).expect("writer connection");
    let transaction = connection.transaction().expect("transaction");
    transaction
        .execute_batch(
            "WITH RECURSIVE sequence(value) AS (
                 SELECT 1
                 UNION ALL
                 SELECT value + 1 FROM sequence WHERE value < 40
             )
             INSERT INTO files(path, content_hash, generation)
             SELECT printf('excluded/%02d.rs', value), 'dummy', 1 FROM sequence;

             WITH RECURSIVE sequence(value) AS (
                 SELECT 1
                 UNION ALL
                 SELECT value + 1 FROM sequence WHERE value < 250
             )
             INSERT INTO chunks(
                 file_id, content, start_line, end_line,
                 start_byte, end_byte, token_count
             )
             SELECT f.id, 'scoped_overflow_needle', sequence.value, sequence.value,
                    0, 22, 1
             FROM files f
             CROSS JOIN sequence
             WHERE f.path GLOB 'excluded/*';",
        )
        .expect("populate excluded candidates");
    transaction.commit().expect("commit candidates");

    let request = SearchRequest {
        query: "scoped_overflow_needle".into(),
        mode: SearchMode::Regex,
        include_paths: vec!["included/**".into()],
        exclude_paths: Vec::new(),
        focus_paths: Vec::new(),
        max_results: Some(20),
        max_tokens: Some(1_000),
        context_lines: Some(0),
        case_sensitive: true,
        all_occurrences: false,
        prefer_structural: false,
        receipt_id: None,
        cursor: None,
    };
    let optimized = services
        .search_evaluation(request.clone())
        .await
        .expect("scoped candidate plan");
    let full_scan = services
        .search_full_scan_evaluation(request)
        .await
        .expect("scoped full scan");

    let mut optimized_response = optimized.response.clone();
    optimized_response.meta.receipt_id = None;
    let mut full_scan_response = full_scan.response.clone();
    full_scan_response.meta.receipt_id = None;
    assert_eq!(
        serde_json::to_value(optimized_response).expect("optimized JSON"),
        serde_json::to_value(full_scan_response).expect("full-scan JSON")
    );
    assert_eq!(optimized.phases.regex_candidate_chunks, 1);
    assert_eq!(optimized.phases.regex_chunks_verified, 1);
}

#[tokio::test]
async fn regex_candidate_plan_bypasses_only_the_full_scan_file_bound() {
    let root = tempfile::tempdir().expect("temporary repository");
    let database = root.path().join("index.sqlite");
    std::fs::write(
        root.path().join("match.rs"),
        "const openclaw_scale_needle: usize = 42;\n",
    )
    .expect("write source");
    let config = Config::discover(root.path(), Some(database.clone())).expect("config");
    let services = Services::open(config).expect("services");
    services.index(false).await.expect("index fixture");

    // Populate the relational file inventory without creating 10,000 physical
    // files. Only the indexed source owns a chunk, which isolates whether a
    // sound candidate query is incorrectly gated by the fallback scan bound.
    let mut connection = rusqlite::Connection::open(database).expect("writer connection");
    let transaction = connection.transaction().expect("transaction");
    transaction
        .execute_batch(
            "WITH RECURSIVE sequence(value) AS (
                 SELECT 1
                 UNION ALL
                 SELECT value + 1 FROM sequence WHERE value < 10000
             )
             INSERT INTO files(path, content_hash, generation)
             SELECT printf('dummy/%05d.rs', value), 'dummy', 1 FROM sequence;",
        )
        .expect("populate large file inventory");
    transaction.commit().expect("commit inventory");

    let planned_request = SearchRequest {
        query: "openclaw_scale_needle".into(),
        mode: SearchMode::Regex,
        include_paths: Vec::new(),
        exclude_paths: Vec::new(),
        focus_paths: Vec::new(),
        max_results: Some(20),
        max_tokens: Some(1_000),
        context_lines: Some(0),
        case_sensitive: true,
        all_occurrences: false,
        prefer_structural: false,
        receipt_id: None,
        cursor: None,
    };
    let optimized = services
        .search_evaluation(planned_request.clone())
        .await
        .expect("sound candidate plan should not scan the file inventory");
    assert_eq!(optimized.response.hits.len(), 1);
    assert_eq!(
        optimized.phases.regex_candidate_strategy,
        leantoken::RegexCandidateStrategy::Trigram
    );
    assert_eq!(optimized.phases.regex_files_considered, 10_001);
    assert_eq!(optimized.phases.regex_candidate_chunks, 1);
    assert_eq!(optimized.phases.regex_chunks_verified, 1);
    assert_eq!(optimized.phases.regex_chunks_loaded, 0);

    let full_scan = services
        .search_full_scan_evaluation(planned_request)
        .await
        .expect_err("full scan remains bounded by the file inventory");
    assert!(matches!(full_scan, Error::LimitExceeded));

    let fallback = services
        .search_evaluation(SearchRequest {
            query: "openclaw_scale_needle".into(),
            mode: SearchMode::Regex,
            include_paths: Vec::new(),
            exclude_paths: Vec::new(),
            focus_paths: Vec::new(),
            max_results: Some(20),
            max_tokens: Some(1_000),
            context_lines: Some(0),
            case_sensitive: false,
            all_occurrences: false,
            prefer_structural: false,
            receipt_id: None,
            cursor: None,
        })
        .await
        .expect_err("case-insensitive fallback remains bounded");
    assert!(matches!(fallback, Error::LimitExceeded));

    let mut connection =
        rusqlite::Connection::open(root.path().join("index.sqlite")).expect("writer connection");
    let transaction = connection.transaction().expect("transaction");
    transaction
        .execute_batch(
            "WITH RECURSIVE sequence(value) AS (
                 SELECT 1
                 UNION ALL
                 SELECT value + 1 FROM sequence WHERE value < 10000
             )
             INSERT INTO chunks(
                 file_id, content, start_line, end_line, start_byte, end_byte, token_count
             )
             SELECT files.id, 'openclaw_scale_needle', value, value, 0, 22, 1
             FROM sequence
             JOIN files ON files.path = 'dummy/00001.rs';",
        )
        .expect("populate candidate overflow");
    transaction.commit().expect("commit candidate overflow");
    let candidate_overflow = services
        .search_evaluation(SearchRequest {
            query: "openclaw_scale_needle".into(),
            mode: SearchMode::Regex,
            include_paths: Vec::new(),
            exclude_paths: Vec::new(),
            focus_paths: Vec::new(),
            max_results: Some(100),
            max_tokens: Some(1_000),
            context_lines: Some(0),
            case_sensitive: true,
            all_occurrences: true,
            prefer_structural: false,
            receipt_id: None,
            cursor: None,
        })
        .await
        .expect_err("planned candidate query remains bounded");
    assert!(matches!(candidate_overflow, Error::LimitExceeded));
}

#[cfg(unix)]
#[tokio::test]
async fn live_read_cannot_escape_through_replaced_path_components() {
    use std::os::unix::fs::symlink;

    let root = tempfile::tempdir().expect("root");
    let outside = tempfile::tempdir().expect("outside");
    std::fs::create_dir(root.path().join("src")).expect("source directory");
    std::fs::write(
        root.path().join("src/module.rs"),
        "pub fn contained_source() {}\n",
    )
    .expect("contained source");
    std::fs::write(
        outside.path().join("module.rs"),
        "pub fn external_marker_needle() {}\n",
    )
    .expect("external source");
    let config =
        Config::discover(root.path(), Some(root.path().join("index.sqlite"))).expect("config");
    let services = Services::open(config).expect("services");
    services.index(false).await.expect("index");

    std::fs::rename(root.path().join("src"), root.path().join("src.original"))
        .expect("move indexed directory");
    symlink(outside.path(), root.path().join("src")).expect("replace directory with symlink");

    assert!(
        services
            .read(ReadRequest {
                path: "src/module.rs".into(),
                symbol: None,
                heading: None,
                heading_occurrence: None,
                start_line: None,
                end_line: None,
                continuation_cursor: None,
                max_tokens: Some(100),
                expected_hash: None,
                delta: false,
                receipt_id: None,
            })
            .await
            .is_err()
    );
}

#[cfg(windows)]
#[tokio::test]
async fn live_read_cannot_escape_through_replaced_path_components() {
    let root = tempfile::tempdir().expect("root");
    let outside = tempfile::tempdir().expect("outside");
    std::fs::create_dir(root.path().join("src")).expect("source directory");
    std::fs::write(
        root.path().join("src/module.rs"),
        "pub fn contained_source() {}\n",
    )
    .expect("contained source");
    std::fs::write(
        outside.path().join("module.rs"),
        "pub fn external_marker_needle() {}\n",
    )
    .expect("external source");
    let config =
        Config::discover(root.path(), Some(root.path().join("index.sqlite"))).expect("config");
    let services = Services::open(config).expect("services");
    services.index(false).await.expect("index");

    std::fs::rename(root.path().join("src"), root.path().join("src.original"))
        .expect("move indexed directory");
    let junction = std::process::Command::new("cmd")
        .args(["/C", "mklink", "/J"])
        .arg(root.path().join("src"))
        .arg(outside.path())
        .output()
        .expect("create junction");
    assert!(
        junction.status.success(),
        "junction creation failed: {}",
        String::from_utf8_lossy(&junction.stderr)
    );

    assert!(
        services
            .read(ReadRequest {
                path: "src/module.rs".into(),
                symbol: None,
                heading: None,
                heading_occurrence: None,
                start_line: None,
                end_line: None,
                continuation_cursor: None,
                max_tokens: Some(100),
                expected_hash: None,
                delta: false,
                receipt_id: None,
            })
            .await
            .is_err()
    );
}

#[tokio::test]
async fn repository_identity_distinguishes_linked_worktrees_before_empty_search_is_evidence() {
    if !git_available() {
        return;
    }

    let parent = tempfile::tempdir().expect("parent");
    let base = parent.path().join("base");
    let linked = parent.path().join("linked");
    std::fs::create_dir(&base).expect("base");
    std::fs::write(base.join("base.rs"), "pub fn base_only() {}\n").expect("base source");
    init_git_repo(&base);
    let worktree = std::process::Command::new("git")
        .args(["worktree", "add", "-b", "holdout-worktree"])
        .arg(&linked)
        .current_dir(&base)
        .output()
        .expect("git worktree add");
    assert!(
        worktree.status.success(),
        "git worktree add failed: {}",
        String::from_utf8_lossy(&worktree.stderr)
    );
    std::fs::write(
        linked.join("holdout.rs"),
        "pub fn linked_worktree_holdout_symbol() {}\n",
    )
    .expect("holdout source");

    let base_services = Services::open(
        Config::discover(&base, Some(parent.path().join("base.sqlite"))).expect("base config"),
    )
    .expect("base services");
    let linked_services = Services::open(
        Config::discover(&linked, Some(parent.path().join("linked.sqlite"))).expect("linked config"),
    )
    .expect("linked services");
    base_services.index(false).await.expect("index base");
    linked_services.index(false).await.expect("index linked");

    let base_id = base_services.repository_id();
    let linked_id = linked_services.repository_id();
    assert_ne!(base_id, linked_id);
    assert!(matches!(
        base_services.validate_repository_id(Some(&linked_id)),
        Err(Error::RepositoryIdentityMismatch { expected, actual })
            if expected == linked_id && actual == base_id
    ));
    let response = linked_services
        .search(SearchRequest {
            query: "linked_worktree_holdout_symbol".into(),
            mode: SearchMode::Symbol,
            case_sensitive: true,
            all_occurrences: false,
            prefer_structural: false,
            include_paths: Vec::new(),
            exclude_paths: Vec::new(),
            focus_paths: Vec::new(),
            max_results: Some(10),
            max_tokens: Some(200),
            context_lines: Some(0),
            receipt_id: None,
            cursor: None,
        })
        .await
        .expect("linked search");
    assert_eq!(response.meta.repository_id, linked_id);
    assert_eq!(response.hits.len(), 1);
}

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
    services.index(false).await.expect("index");

    let response = services
        .context_with_workflow_consistency_cancellable(
            ContextRequest {
                task: "prepare a contribution for parse_contribution_target".into(),
                token_budget: 1_000,
                include_paths: Vec::new(),
                must_include_paths: Vec::new(),
                must_include_symbols: Vec::new(),
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
                verbose_diagnostics: false,
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
        relationship.related_path == "tests/parser.rs"
            && relationship.signal == "test_name_match"
    }));
    assert!(paths.contains("AGENTS.md"));
    assert!(paths.contains("docs/development.md"));
    assert!(paths.contains(".github/workflows/ci.yml"));
    assert!(paths.contains("tests/parser.rs"));
}

fn assert_zero_limit(error: Error, expected_field: &'static str) {
    assert!(
        matches!(
            error,
            Error::InvalidInput {
                field,
                reason: "must be greater than zero"
            } if field == expected_field
        ),
        "unexpected zero-limit error: {error:?}"
    );
}

fn assert_limit_exceeded(
    error: Error,
    expected_field: &'static str,
    expected_requested: usize,
    expected_limit: usize,
) {
    assert!(
        matches!(
            error,
            Error::RequestLimitExceeded {
                field,
                requested,
                limit,
            } if field == expected_field
                && requested == expected_requested
                && limit == expected_limit
        ),
        "unexpected request-limit error: {error:?}"
    );
}

fn files_limit_request(max_results: Option<usize>) -> FilesRequest {
    FilesRequest {
        operation: FileOperation::Tree,
        path: None,
        query: None,
        pattern: None,
        max_results,
        cursor: None,
        depth: Some(0),
    }
}

fn search_limit_request(
    max_results: Option<usize>,
    max_tokens: Option<usize>,
    context_lines: Option<usize>,
) -> SearchRequest {
    SearchRequest {
        query: "greet".into(),
        mode: SearchMode::Text,
        include_paths: Vec::new(),
        exclude_paths: Vec::new(),
        focus_paths: Vec::new(),
        max_results,
        max_tokens,
        context_lines,
        case_sensitive: false,
        all_occurrences: false,
        prefer_structural: false,
        receipt_id: None,
        cursor: None,
    }
}

fn outline_limit_request(
    max_results: Option<usize>,
    max_tokens: Option<usize>,
) -> OutlineRequest {
    OutlineRequest {
        paths: vec!["src/lib.rs".into()],
        symbol_name: None,
        symbol_kind: None,
        max_results,
        max_tokens,
        receipt_id: None,
        cursor: None,
    }
}

fn read_limit_request(max_tokens: Option<usize>) -> ReadRequest {
    ReadRequest {
        path: "src/lib.rs".into(),
        start_line: Some(1),
        end_line: Some(1),
        symbol: None,
        heading: None,
        heading_occurrence: None,
        continuation_cursor: None,
        max_tokens,
        expected_hash: None,
        delta: false,
        receipt_id: None,
    }
}

fn context_limit_request(token_budget: usize) -> ContextRequest {
    ContextRequest {
        task: "find greet".into(),
        token_budget,
        include_paths: Vec::new(),
        must_include_paths: Vec::new(),
        must_include_symbols: Vec::new(),
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
        verbose_diagnostics: false,
    }
}

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
    assert_eq!(preview.meta.emitted_tokens, 0);
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
    let invalid = services
        .context_with_options_workflow_consistency_cancellable(
            context_limit_request(100),
            None,
            ContextWorkflow::Auto,
            IndexConsistency::ReconcileWorkingTree,
            ServiceCallOptions::new().with_max_response_tokens(0),
            CancellationToken::new(),
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

    assert!(matches!(
        error,
        Error::RequestLimitExceeded {
            field: "max_response_tokens",
            requested,
            limit: 1,
        } if requested > 1
    ));
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
    let underfilled = services
        .context(underfilled)
        .await
        .expect("underfilled focus context");
    assert_eq!(underfilled.fragments.len(), 3);
    assert_eq!(underfilled.coverage.strict_scope_satisfied, Some(false));
    assert_eq!(
        underfilled
            .coverage
            .focus_path_coverage
            .iter()
            .filter(|focus| focus.satisfied)
            .count(),
        1
    );

    let mut missing = context_limit_request(400);
    missing.task = "change shared_scope_target".into();
    missing.focus_paths = vec!["src/missing/**".into()];
    missing.strict_focus_paths = true;
    let missing = services.context(missing).await.expect("missing strict focus");
    assert!(missing.fragments.is_empty());
    assert_eq!(missing.coverage.strict_scope_satisfied, Some(false));
    assert_eq!(missing.coverage.unmatched_focus_paths, ["src/missing/**"]);
    assert_eq!(missing.coverage.focus_path_coverage[0].indexed_paths, 0);
    assert!(!missing.coverage.focus_path_coverage[0].satisfied);
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

    let mut excluded = context_limit_request(1_000);
    excluded.task = "change buried_focus_target".into();
    excluded.focus_paths = vec!["focus/owner_0.rs".into()];
    excluded.exclude_paths = vec!["focus/owner_0.rs".into()];
    excluded.strict_focus_paths = true;
    let excluded = services
        .context(excluded)
        .await
        .expect("policy-empty focus scope");
    assert!(excluded.fragments.is_empty());
    assert_eq!(excluded.coverage.focus_path_coverage[0].indexed_paths, 1);
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

#[tokio::test]
async fn files_enforces_result_limit_contract() {
    let (_root, services) = fixture().await;
    let limit = services.config().max_results;

    services
        .files(files_limit_request(None))
        .await
        .expect("default result limit");
    for requested in [1, limit] {
        services
            .files(files_limit_request(Some(requested)))
            .await
            .expect("valid result limit");
    }
    let error = services
        .files(files_limit_request(Some(0)))
        .await
        .expect_err("zero result limit");
    assert_zero_limit(error, "max_results");
    let error = services
        .files(files_limit_request(Some(limit + 1)))
        .await
        .expect_err("oversized result limit");
    assert_limit_exceeded(error, "max_results", limit + 1, limit);
}

#[tokio::test]
async fn search_enforces_all_limit_contracts() {
    let (_root, services) = fixture().await;
    let result_limit = services.config().max_results;
    let token_limit = services.config().max_output_tokens;

    services
        .search(search_limit_request(None, None, None))
        .await
        .expect("default search limits");
    for requested in [1, result_limit] {
        services
            .search(search_limit_request(Some(requested), Some(1), Some(0)))
            .await
            .expect("valid result limit");
    }
    let error = services
        .search(search_limit_request(Some(0), Some(1), Some(0)))
        .await
        .expect_err("zero result limit");
    assert_zero_limit(error, "max_results");
    let error = services
        .search(search_limit_request(
            Some(result_limit + 1),
            Some(1),
            Some(0),
        ))
        .await
        .expect_err("oversized result limit");
    assert_limit_exceeded(error, "max_results", result_limit + 1, result_limit);

    for requested in [1, token_limit] {
        services
            .search(search_limit_request(Some(1), Some(requested), Some(0)))
            .await
            .expect("valid token limit");
    }
    let error = services
        .search(search_limit_request(Some(1), Some(0), Some(0)))
        .await
        .expect_err("zero token limit");
    assert_zero_limit(error, "max_tokens");
    let error = services
        .search(search_limit_request(
            Some(1),
            Some(token_limit + 1),
            Some(0),
        ))
        .await
        .expect_err("oversized token limit");
    assert_limit_exceeded(error, "max_tokens", token_limit + 1, token_limit);

    for requested in [0, 1, 20] {
        services
            .search(search_limit_request(Some(1), Some(1), Some(requested)))
            .await
            .expect("valid context-line limit");
    }
    let error = services
        .search(search_limit_request(Some(1), Some(1), Some(21)))
        .await
        .expect_err("oversized context-line limit");
    assert_limit_exceeded(error, "context_lines", 21, 20);
}

#[tokio::test]
async fn outline_enforces_result_and_token_limit_contracts() {
    let (_root, services) = fixture().await;
    let result_limit = services.config().max_results;
    let token_limit = services.config().max_output_tokens;

    services
        .outline(outline_limit_request(None, None))
        .await
        .expect("default outline limits");
    for requested in [1, result_limit] {
        services
            .outline(outline_limit_request(Some(requested), Some(1)))
            .await
            .expect("valid result limit");
    }
    let error = services
        .outline(outline_limit_request(Some(0), Some(1)))
        .await
        .expect_err("zero result limit");
    assert_zero_limit(error, "max_results");
    let error = services
        .outline(outline_limit_request(Some(result_limit + 1), Some(1)))
        .await
        .expect_err("oversized result limit");
    assert_limit_exceeded(error, "max_results", result_limit + 1, result_limit);

    for requested in [1, token_limit] {
        services
            .outline(outline_limit_request(Some(1), Some(requested)))
            .await
            .expect("valid token limit");
    }
    let error = services
        .outline(outline_limit_request(Some(1), Some(0)))
        .await
        .expect_err("zero token limit");
    assert_zero_limit(error, "max_tokens");
    let error = services
        .outline(outline_limit_request(Some(1), Some(token_limit + 1)))
        .await
        .expect_err("oversized token limit");
    assert_limit_exceeded(error, "max_tokens", token_limit + 1, token_limit);
}

#[tokio::test]
async fn read_enforces_token_limit_contract() {
    let (_root, services) = fixture().await;
    let limit = services.config().max_output_tokens;

    services
        .read(read_limit_request(None))
        .await
        .expect("default token limit");
    for requested in [1, limit] {
        services
            .read(read_limit_request(Some(requested)))
            .await
            .expect("valid token limit");
    }
    let error = services
        .read(read_limit_request(Some(0)))
        .await
        .expect_err("zero token limit");
    assert_zero_limit(error, "max_tokens");
    let error = services
        .read(read_limit_request(Some(limit + 1)))
        .await
        .expect_err("oversized token limit");
    assert_limit_exceeded(error, "max_tokens", limit + 1, limit);
}

#[tokio::test]
async fn context_enforces_token_budget_contract() {
    let (_root, services) = fixture().await;
    let limit = services.config().max_output_tokens;

    for requested in [1, limit] {
        services
            .context(context_limit_request(requested))
            .await
            .expect("valid token budget");
    }
    let error = services
        .context(context_limit_request(0))
        .await
        .expect_err("zero token budget");
    assert_zero_limit(error, "token_budget");
    let error = services
        .context(context_limit_request(limit + 1))
        .await
        .expect_err("oversized token budget");
    assert_limit_exceeded(error, "token_budget", limit + 1, limit);
}

#[tokio::test]
async fn context_tiny_budget_does_not_claim_candidates_are_missing() {
    let (_root, services) = fixture().await;

    let response = services
        .context(context_limit_request(1))
        .await
        .expect("tiny valid token budget");

    assert!(response.fragments.is_empty());
    assert!(response.omission_summary.budget_or_result_limit > 0);
    assert!(
        !response
            .warnings
            .iter()
            .any(|warning| warning == "no relevant indexed evidence found")
    );
}

#[tokio::test]
async fn reconcile_working_tree_limit_errors_do_not_reconcile_the_index() {
    let (root, services) = fixture().await;
    let generation = services
        .status()
        .await
        .expect("initial status")
        .repository_generation;
    std::fs::write(
        root.path().join("src/unreconciled.rs"),
        "pub fn unreconciled() {}\n",
    )
    .expect("write unindexed source");

    let error = services
        .files_with_consistency_cancellable(
            files_limit_request(Some(0)),
            IndexConsistency::ReconcileWorkingTree,
            CancellationToken::new(),
        )
        .await
        .expect_err("invalid files limit");
    assert_zero_limit(error, "max_results");

    for (request, field) in [
        (search_limit_request(Some(0), Some(1), Some(0)), "max_results"),
        (search_limit_request(Some(1), Some(0), Some(0)), "max_tokens"),
    ] {
        let error = services
            .search_with_consistency_cancellable(
                request,
                IndexConsistency::ReconcileWorkingTree,
                CancellationToken::new(),
            )
            .await
            .expect_err("invalid search limit");
        assert_zero_limit(error, field);
    }
    let error = services
        .search_with_consistency_cancellable(
            search_limit_request(Some(1), Some(1), Some(21)),
            IndexConsistency::ReconcileWorkingTree,
            CancellationToken::new(),
        )
        .await
        .expect_err("invalid search context limit");
    assert_limit_exceeded(error, "context_lines", 21, 20);

    for (request, field) in [
        (outline_limit_request(Some(0), Some(1)), "max_results"),
        (outline_limit_request(Some(1), Some(0)), "max_tokens"),
    ] {
        let error = services
            .outline_with_consistency_cancellable(
                request,
                IndexConsistency::ReconcileWorkingTree,
                CancellationToken::new(),
            )
            .await
            .expect_err("invalid outline limit");
        assert_zero_limit(error, field);
    }

    let error = services
        .read_with_consistency_cancellable(
            read_limit_request(Some(0)),
            IndexConsistency::ReconcileWorkingTree,
            CancellationToken::new(),
        )
        .await
        .expect_err("invalid read limit");
    assert_zero_limit(error, "max_tokens");
    let error = services
        .context_with_consistency_cancellable(
            context_limit_request(0),
            IndexConsistency::ReconcileWorkingTree,
            CancellationToken::new(),
        )
        .await
        .expect_err("invalid context limit");
    assert_zero_limit(error, "token_budget");

    let after = services.status().await.expect("status after invalid requests");
    assert_eq!(after.repository_generation, generation);
    let committed = services
        .files(FilesRequest {
            operation: FileOperation::Find,
            path: None,
            query: Some("unreconciled".into()),
            pattern: None,
            max_results: Some(1),
            cursor: None,
            depth: None,
        })
        .await
        .expect("committed lookup");
    assert!(committed.entries.is_empty());
}

#[tokio::test]
async fn reconcile_working_tree_static_input_errors_do_not_reconcile_the_index() {
    let (root, services) = fixture().await;
    let generation = services
        .status()
        .await
        .expect("initial status")
        .repository_generation;
    std::fs::write(
        root.path().join("src/unreconciled.rs"),
        "pub fn unreconciled() {}\n",
    )
    .expect("write unindexed source");
    let mut expected_failures = 0u64;

    macro_rules! assert_static_error {
        ($future:expr, $case:literal) => {{
            assert!($future.await.is_err(), concat!($case, " must fail"));
            expected_failures += 1;
            let current = services.status().await.expect("status after static error");
            assert_eq!(
                current.repository_generation, generation,
                concat!($case, " must not reconcile")
            );
        }};
    }

    assert_static_error!(
        services.files_with_consistency_cancellable(
            FilesRequest {
                operation: FileOperation::Find,
                path: None,
                query: None,
                pattern: None,
                max_results: Some(1),
                cursor: None,
                depth: None,
            },
            IndexConsistency::ReconcileWorkingTree,
            CancellationToken::new(),
        ),
        "missing find query"
    );
    assert_static_error!(
        services.files_with_consistency_cancellable(
            FilesRequest {
                operation: FileOperation::Tree,
                path: Some("../outside.rs".into()),
                query: None,
                pattern: None,
                max_results: Some(1),
                cursor: None,
                depth: None,
            },
            IndexConsistency::ReconcileWorkingTree,
            CancellationToken::new(),
        ),
        "unsafe tree root"
    );
    assert_static_error!(
        services.files_with_consistency_cancellable(
            FilesRequest {
                operation: FileOperation::Glob,
                path: None,
                query: None,
                pattern: Some("[".into()),
                max_results: Some(1),
                cursor: None,
                depth: None,
            },
            IndexConsistency::ReconcileWorkingTree,
            CancellationToken::new(),
        ),
        "invalid files glob"
    );
    let mut files = files_limit_request(Some(1));
    files.cursor = Some("invalid".into());
    assert_static_error!(
        services.files_with_consistency_cancellable(
            files,
            IndexConsistency::ReconcileWorkingTree,
            CancellationToken::new(),
        ),
        "malformed files cursor"
    );

    let mut search = search_limit_request(Some(1), Some(1), Some(0));
    search.query = " ".into();
    assert_static_error!(
        services.search_with_consistency_cancellable(
            search,
            IndexConsistency::ReconcileWorkingTree,
            CancellationToken::new(),
        ),
        "empty search query"
    );
    let mut search = search_limit_request(Some(1), Some(1), Some(0));
    search.query = "[".into();
    search.mode = SearchMode::Regex;
    assert_static_error!(
        services.search_with_consistency_cancellable(
            search,
            IndexConsistency::ReconcileWorkingTree,
            CancellationToken::new(),
        ),
        "invalid search regex"
    );
    let mut search = search_limit_request(Some(1), Some(1), Some(0));
    search.focus_paths = vec!["[".into()];
    assert_static_error!(
        services.search_with_consistency_cancellable(
            search,
            IndexConsistency::ReconcileWorkingTree,
            CancellationToken::new(),
        ),
        "invalid search path glob"
    );
    let mut search = search_limit_request(Some(1), Some(1), Some(0));
    search.query = "x".repeat(64 * 1024 + 1);
    assert_static_error!(
        services.search_with_consistency_cancellable(
            search,
            IndexConsistency::ReconcileWorkingTree,
            CancellationToken::new(),
        ),
        "oversized search query"
    );
    let mut search = search_limit_request(Some(1), Some(1), Some(0));
    search.cursor = Some("invalid".into());
    assert_static_error!(
        services.search_with_consistency_cancellable(
            search,
            IndexConsistency::ReconcileWorkingTree,
            CancellationToken::new(),
        ),
        "malformed search cursor"
    );

    let mut outline = outline_limit_request(Some(1), Some(1));
    outline.paths = Vec::new();
    assert_static_error!(
        services.outline_with_consistency_cancellable(
            outline,
            IndexConsistency::ReconcileWorkingTree,
            CancellationToken::new(),
        ),
        "empty outline paths"
    );
    let mut outline = outline_limit_request(Some(1), Some(1));
    outline.paths = (0..257).map(|index| format!("src/{index}.rs")).collect();
    assert_static_error!(
        services.outline_with_consistency_cancellable(
            outline,
            IndexConsistency::ReconcileWorkingTree,
            CancellationToken::new(),
        ),
        "excessive outline paths"
    );
    let mut outline = outline_limit_request(Some(1), Some(1));
    outline.paths = vec!["../outside.rs".into()];
    assert_static_error!(
        services.outline_with_consistency_cancellable(
            outline,
            IndexConsistency::ReconcileWorkingTree,
            CancellationToken::new(),
        ),
        "unsafe outline path"
    );

    let mut read = read_limit_request(Some(1));
    read.start_line = Some(0);
    assert_static_error!(
        services.read_with_consistency_cancellable(
            read,
            IndexConsistency::ReconcileWorkingTree,
            CancellationToken::new(),
        ),
        "invalid read range"
    );
    let mut read = read_limit_request(Some(1));
    read.symbol = Some("greet".into());
    assert_static_error!(
        services.read_with_consistency_cancellable(
            read,
            IndexConsistency::ReconcileWorkingTree,
            CancellationToken::new(),
        ),
        "conflicting read target"
    );
    let mut read = read_limit_request(Some(1));
    read.start_line = None;
    read.end_line = None;
    read.symbol = Some(String::new());
    let error = services
        .read_with_consistency_cancellable(
            read,
            IndexConsistency::ReconcileWorkingTree,
            CancellationToken::new(),
        )
        .await
        .expect_err("empty read symbol must fail");
    let current = services
        .status()
        .await
        .expect("status after empty read symbol");
    assert_eq!(
        current.repository_generation, generation,
        "empty read symbol must not reconcile"
    );
    assert!(
        matches!(
            error,
            Error::InvalidInput {
                field: "symbol",
                reason: "must not be empty"
            }
        ),
        "unexpected empty read symbol error: {error:?}"
    );
    expected_failures += 1;

    let mut context = context_limit_request(1);
    context.task = " ".into();
    assert_static_error!(
        services.context_with_consistency_cancellable(
            context,
            IndexConsistency::ReconcileWorkingTree,
            CancellationToken::new(),
        ),
        "empty context task"
    );
    let mut context = context_limit_request(1);
    context.focus_paths = vec!["[".into()];
    assert_static_error!(
        services.context_with_consistency_cancellable(
            context,
            IndexConsistency::ReconcileWorkingTree,
            CancellationToken::new(),
        ),
        "invalid context path glob"
    );
    let mut context = context_limit_request(1);
    context.focus_symbols = vec!["symbol".into(); 257];
    assert_static_error!(
        services.context_with_consistency_cancellable(
            context,
            IndexConsistency::ReconcileWorkingTree,
            CancellationToken::new(),
        ),
        "excessive context symbols"
    );
    let mut context = context_limit_request(1);
    context.changed_paths = vec!["../outside.rs".into()];
    assert_static_error!(
        services.context_with_consistency_cancellable(
            context,
            IndexConsistency::ReconcileWorkingTree,
            CancellationToken::new(),
        ),
        "unsafe context changed path"
    );
    let mut context = context_limit_request(1);
    context.base_revision = Some("r".repeat(257));
    assert_static_error!(
        services.context_with_consistency_cancellable(
            context,
            IndexConsistency::ReconcileWorkingTree,
            CancellationToken::new(),
        ),
        "oversized context base revision"
    );
    let mut context = context_limit_request(1);
    context.changed_paths = (0..513).map(|index| format!("src/{index}.rs")).collect();
    assert_static_error!(
        services.context_with_consistency_cancellable(
            context,
            IndexConsistency::ReconcileWorkingTree,
            CancellationToken::new(),
        ),
        "excessive context changed paths"
    );
    let mut context = context_limit_request(1);
    context.task = "a_".repeat(30_000);
    assert_static_error!(
        services.context_with_consistency_cancellable(
            context,
            IndexConsistency::ReconcileWorkingTree,
            CancellationToken::new(),
        ),
        "oversized derived context matcher"
    );

    let committed = services
        .files(FilesRequest {
            operation: FileOperation::Find,
            path: None,
            query: Some("unreconciled".into()),
            pattern: None,
            max_results: Some(1),
            cursor: None,
            depth: None,
        })
        .await
        .expect("committed lookup");
    assert!(committed.entries.is_empty());
    let observed = services
        .observed_token_savings_report()
        .await
        .expect("observed static failures");
    assert_eq!(
        observed.observations.failed_service_requests,
        expected_failures,
        "each failed public service request must be observed exactly once"
    );
    assert_eq!(
        observed
            .observations
            .failed_by_operation_and_category
            .iter()
            .map(|failure| failure.failed_requests)
            .sum::<u64>(),
        expected_failures
    );
}

#[tokio::test]
async fn reconcile_working_tree_generation_checks_run_after_reconciliation() {
    let (root, services) = fixture().await;
    let generation = services
        .status()
        .await
        .expect("initial status")
        .repository_generation;
    std::fs::write(
        root.path().join("src/reconciled.rs"),
        "pub fn reconciled() {}\n",
    )
    .expect("write unindexed source");

    let mut request = search_limit_request(Some(1), Some(1), Some(0));
    request.cursor = Some(format!("{generation}:0"));
    let error = services
        .search_with_consistency_cancellable(
            request,
            IndexConsistency::ReconcileWorkingTree,
            CancellationToken::new(),
        )
        .await
        .expect_err("cursor from the pre-reconciliation generation must be stale");
    assert!(matches!(error, Error::StaleCursor));

    let after = services.status().await.expect("status after reconciliation");
    assert!(after.repository_generation > generation);
    let committed = services
        .files(FilesRequest {
            operation: FileOperation::Find,
            path: None,
            query: Some("reconciled".into()),
            pattern: None,
            max_results: Some(1),
            cursor: None,
            depth: None,
        })
        .await
        .expect("committed lookup");
    assert_eq!(committed.entries.len(), 1);
}

async fn tree_pages(
    services: &Services,
    path: Option<&str>,
) -> Vec<(serde_json::Value, Option<String>)> {
    let mut cursor = None;
    let mut pages = Vec::new();
    loop {
        let response = services
            .files(FilesRequest {
                operation: FileOperation::Tree,
                path: path.map(str::to_owned),
                query: None,
                pattern: None,
                max_results: Some(2),
                cursor,
                depth: Some(2),
            })
            .await
            .expect("tree page");
        let next = response.meta.next_cursor;
        pages.push((
            serde_json::to_value(response.entries).expect("serialize tree entries"),
            next.clone(),
        ));
        let Some(next) = next else {
            break;
        };
        cursor = Some(next);
    }
    pages
}

async fn indexed_source(path: &str, content: &[u8]) -> (tempfile::TempDir, Services) {
    let root = tempfile::tempdir().expect("temporary repository");
    let source_path = root.path().join(path);
    if let Some(parent) = source_path.parent() {
        std::fs::create_dir_all(parent).expect("create source parent");
    }
    std::fs::write(source_path, content).expect("write source");
    let config =
        Config::discover(root.path(), Some(root.path().join("index.sqlite"))).expect("config");
    let services = Services::open(config).expect("services");
    services.index(false).await.expect("index source");
    (root, services)
}

#[test]
fn services_reject_database_owned_by_another_repository() {
    let first_root = tempfile::tempdir().expect("first root");
    let second_root = tempfile::tempdir().expect("second root");
    let cache = tempfile::tempdir().expect("cache");
    let database = cache.path().join("shared.sqlite");

    let first_config =
        Config::discover(first_root.path(), Some(database.clone())).expect("first config");
    let first = Services::open(first_config).expect("claim database");
    let second_config =
        Config::discover(second_root.path(), Some(database.clone())).expect("second config");
    let error = Services::open(second_config).expect_err("different root must be rejected");

    assert!(matches!(error, Error::RepositoryMismatch { .. }));
    drop(first);
    Services::open(
        Config::discover(first_root.path(), Some(database)).expect("same-root config"),
    )
    .expect("same root may share database");
}

#[tokio::test]
async fn same_repository_services_share_committed_generations() {
    let root = tempfile::tempdir().expect("root");
    std::fs::write(root.path().join("lib.rs"), "fn shared() {}\n").expect("source");
    let database = root.path().join("index.sqlite");
    let first = Services::open(
        Config::discover(root.path(), Some(database.clone())).expect("first config"),
    )
    .expect("first services");
    let second = Services::open(
        Config::discover(root.path(), Some(database)).expect("second config"),
    )
    .expect("second services");

    let indexed = first.index(false).await.expect("index");
    let observed = second.status().await.expect("follower status");

    assert_eq!(observed.repository_generation, indexed.repository_generation);
}

#[tokio::test]
async fn independent_repositories_index_concurrently_without_result_leakage() {
    let first_root = tempfile::tempdir().expect("first root");
    let second_root = tempfile::tempdir().expect("second root");
    let cache = tempfile::tempdir().expect("cache");
    std::fs::write(first_root.path().join("first.rs"), "fn alpha_only() {}\n")
        .expect("first source");
    std::fs::write(second_root.path().join("second.rs"), "fn beta_only() {}\n")
        .expect("second source");
    let first = Services::open(
        Config::discover(first_root.path(), Some(cache.path().join("first.sqlite")))
            .expect("first config"),
    )
    .expect("first services");
    let second = Services::open(
        Config::discover(second_root.path(), Some(cache.path().join("second.sqlite")))
            .expect("second config"),
    )
    .expect("second services");

    let (first_index, second_index) = tokio::join!(first.index(false), second.index(false));
    first_index.expect("first index");
    second_index.expect("second index");
    let first_status = first.status().await.expect("first status");
    let second_status = second.status().await.expect("second status");

    assert_eq!(first_status.file_count, 1);
    assert_eq!(second_status.file_count, 1);
    assert_ne!(first.config().database_path, second.config().database_path);
    assert_ne!(first.repository_id(), second.repository_id());
}

#[cfg(unix)]
#[tokio::test]
async fn repository_identity_is_stable_across_symlink_aliases() {
    let root = tempfile::tempdir().expect("root");
    let aliases = tempfile::tempdir().expect("aliases");
    let alias = aliases.path().join("repository");
    std::os::unix::fs::symlink(root.path(), &alias).expect("symlink root");
    let first = Services::open(
        Config::discover(root.path(), Some(root.path().join("first.sqlite"))).expect("root config"),
    )
    .expect("root services");
    let second = Services::open(
        Config::discover(&alias, Some(root.path().join("second.sqlite"))).expect("alias config"),
    )
    .expect("alias services");

    assert_eq!(first.repository_id(), second.repository_id());
}

#[cfg(unix)]
#[tokio::test]
async fn index_excludes_database_below_missing_symlinked_parent() {
    let root = tempfile::tempdir().expect("root");
    let aliases = tempfile::tempdir().expect("aliases");
    let alias = aliases.path().join("repository");
    std::os::unix::fs::symlink(root.path(), &alias).expect("symlink root");
    std::fs::write(root.path().join("lib.rs"), "fn source() {}\n").expect("source");

    let config = Config::discover(
        root.path(),
        Some(alias.join("missing/cache/index.sqlite")),
    )
    .expect("config");
    let services = Services::open(config).expect("services");
    services.index(false).await.expect("index");

    let files = services
        .files(FilesRequest {
            operation: FileOperation::Tree,
            path: None,
            query: None,
            pattern: None,
            max_results: Some(100),
            cursor: None,
            depth: Some(8),
        })
        .await
        .expect("files");
    assert!(files.entries.iter().any(|entry| entry.path == "lib.rs"));
    assert!(
        files
            .entries
            .iter()
            .all(|entry| !entry.path.starts_with("missing/cache/index.sqlite")),
        "database artifacts leaked into the index: {:?}",
        files.entries
    );
}

#[tokio::test]
async fn database_artifact_notifications_do_not_publish_a_generation() {
    let (_root, services) = fixture().await;
    let before = services
        .status()
        .await
        .expect("status before artifacts")
        .repository_generation;

    let response = services
        .index_paths_report(vec![
            "index.sqlite".into(),
            "index.sqlite-wal".into(),
            "index.sqlite-shm".into(),
        ])
        .await
        .expect("ignore database artifacts");

    assert_eq!(response.repository_generation, before);
    assert_eq!(response.files_indexed, 0);
    assert_eq!(response.files_removed, 0);
    assert_eq!(response.files_unchanged, 0);
    assert_eq!(response.files_skipped, 0);
    assert_eq!(
        response
            .skip_reasons
            .as_ref()
            .expect("current skip reasons")
            .total(),
        0
    );
    assert!(response.warnings.is_empty());
    assert_eq!(
        services
            .status()
            .await
            .expect("status after artifacts")
            .repository_generation,
        before
    );
}

#[tokio::test]
async fn five_services_return_bounded_grounded_responses() {
    let (_root, services) = fixture().await;

    let files = services
        .files(FilesRequest {
            operation: FileOperation::Tree,
            path: None,
            query: None,
            pattern: None,
            max_results: Some(10),
            cursor: None,
            depth: Some(3),
        })
        .await
        .expect("files");
    assert!(files.entries.iter().any(|entry| entry.path == "src/lib.rs"));
    assert_eq!(files.meta.source_tokens, 0);
    assert_response_token_accounting!(files, Tokenizer::Cl100kBase);
    assert!(files.meta.path_and_metadata_tokens > 0);

    let search = services
        .search(SearchRequest {
            query: "greet".into(),
            mode: SearchMode::Auto,
            include_paths: Vec::new(),
            exclude_paths: Vec::new(),
            focus_paths: Vec::new(),
            max_results: Some(5),
            max_tokens: Some(200),
            context_lines: Some(1),
            case_sensitive: false,
            all_occurrences: false,
            prefer_structural: false,
            receipt_id: None,
            cursor: None,
        })
        .await
        .expect("search");
    assert!(!search.hits.is_empty());
    assert!(search.meta.emitted_tokens <= 200);
    assert!(search.hits.iter().all(|hit| hit.start_line <= hit.end_line));
    assert_response_token_accounting!(search, Tokenizer::Cl100kBase);

    let outline = services
        .outline(OutlineRequest {
            paths: vec!["src/lib.rs".into()],
            symbol_name: None,
            symbol_kind: None,
            max_results: Some(10),
            max_tokens: Some(100),
            receipt_id: None,
            cursor: None,
        })
        .await
        .expect("outline");
    assert!(
        outline.files[0]
            .symbols
            .iter()
            .any(|symbol| symbol.name == "greet")
    );
    assert!(outline.meta.emitted_tokens <= 100);
    assert_response_token_accounting!(outline, Tokenizer::Cl100kBase);

    let first = services
        .read(ReadRequest {
            path: "src/lib.rs".into(),
            start_line: Some(1),
            end_line: Some(3),
            symbol: None,
            heading: None,
            heading_occurrence: None,
            continuation_cursor: None,
            max_tokens: Some(100),
            expected_hash: None,
            delta: false,
            receipt_id: None,
        })
        .await
        .expect("first read");
    assert_response_token_accounting!(first, Tokenizer::Cl100kBase);
    let second = services
        .read(ReadRequest {
            path: "src/lib.rs".into(),
            start_line: Some(1),
            end_line: Some(3),
            symbol: None,
            heading: None,
            heading_occurrence: None,
            continuation_cursor: None,
            max_tokens: Some(100),
            expected_hash: Some(first.content_hash.clone()),
            delta: false,
            receipt_id: None,
        })
        .await
        .expect("conditional read");
    assert_eq!(second.status, ReadStatus::NotModified);
    assert!(second.content.is_none());
    assert_eq!(second.meta.emitted_tokens, 0);
    assert_response_token_accounting!(second, Tokenizer::Cl100kBase);

    let context = services
        .context(ContextRequest {
            task: "change greet caller".into(),
            token_budget: 200,
            include_paths: Vec::new(),
            must_include_paths: Vec::new(),
            must_include_symbols: Vec::new(),
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
        verbose_diagnostics: false,
        })
        .await
        .expect("context");
    assert!(!context.fragments.is_empty());
    assert!(context.meta.emitted_tokens <= 200);
    assert_response_token_accounting!(context, Tokenizer::Cl100kBase);
    assert_eq!(
        context.receipt.fragment_hashes.len(),
        context.fragments.len()
    );
    let repeated_context = services
        .context(ContextRequest {
            task: "change greet caller".into(),
            token_budget: 200,
            include_paths: Vec::new(),
            must_include_paths: Vec::new(),
            must_include_symbols: Vec::new(),
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
        verbose_diagnostics: false,
        })
        .await
        .expect("repeated context");
    let mut deterministic_context = context.clone();
    deterministic_context.meta.receipt_id = None;
    let mut deterministic_repeat = repeated_context.clone();
    deterministic_repeat.meta.receipt_id = None;
    assert_eq!(
        serde_json::to_string(&deterministic_repeat).expect("serialize repeated context"),
        serde_json::to_string(&deterministic_context).expect("serialize context"),
        "the same repository generation and request must be deterministic"
    );

    let known = context.fragments[0].content_hash.clone();
    let delta = services
        .context(ContextRequest {
            task: "change greet caller".into(),
            token_budget: 200,
            include_paths: Vec::new(),
            must_include_paths: Vec::new(),
            must_include_symbols: Vec::new(),
            max_fragments: None,
            plan_only: false,
            focus_paths: Vec::new(),
            strict_focus_paths: false,
            minimum_fragments_per_focus_path: None,
            focus_symbols: Vec::new(),
            exclude_paths: Vec::new(),
            known_hashes: vec![known.clone()],
            receipt_id: None,
            prior_repository_generation: Some(context.meta.repository_generation),
        base_revision: None,
        changed_paths: Vec::new(),
        strict_changed_paths: false,
        verbose_diagnostics: false,
        })
        .await
        .expect("context delta");
    assert!(
        delta
            .fragments
            .iter()
            .all(|fragment| fragment.content_hash != known)
    );
    let report = services
        .token_savings_report()
        .await
        .expect("full response accounting");
    let files_accounting = report
        .response_accounting
        .by_operation
        .iter()
        .find(|row| row.operation == TokenAccountingOperation::Files)
        .expect("files accounting");
    assert_eq!(files_accounting.tracked_requests, 1);
    assert_eq!(files_accounting.baseline_requests, 0);
    assert_eq!(
        files_accounting.total_response_tokens,
        files.meta.total_response_tokens as u64
    );
    assert_eq!(
        files_accounting.estimated_net_tokens_saved,
        -(files.meta.total_response_tokens as i64)
    );
}

#[tokio::test]
async fn repository_path_inputs_normalize_before_index_lookup_and_matching() {
    let (_root, services) = fixture().await;

    let read = services
        .read(ReadRequest {
            path: r".\src\lib.rs".into(),
            start_line: Some(1),
            end_line: Some(1),
            symbol: None,
            heading: None,
            heading_occurrence: None,
            continuation_cursor: None,
            max_tokens: Some(100),
            expected_hash: None,
            delta: false,
            receipt_id: None,
        })
        .await
        .expect("normalized read");
    assert_eq!(read.path, "src/lib.rs");

    let outline = services
        .outline(OutlineRequest {
            paths: vec!["./src/lib.rs".into()],
            symbol_name: None,
            symbol_kind: None,
            max_results: Some(10),
            max_tokens: Some(100),
            receipt_id: None,
            cursor: None,
        })
        .await
        .expect("normalized outline");
    assert_eq!(outline.files[0].path, "src/lib.rs");

    let files = services
        .files(FilesRequest {
            operation: FileOperation::Glob,
            path: None,
            query: None,
            pattern: Some(r"src\*.rs".into()),
            max_results: Some(10),
            cursor: None,
            depth: None,
        })
        .await
        .expect("normalized files glob");
    assert_eq!(files.entries[0].path, "src/lib.rs");

    let search = services
        .search(SearchRequest {
            query: "greet".into(),
            mode: SearchMode::Auto,
            include_paths: vec![r"src\*.rs".into()],
            exclude_paths: Vec::new(),
            focus_paths: vec![r"src\lib.rs".into()],
            max_results: Some(10),
            max_tokens: Some(100),
            context_lines: Some(1),
            case_sensitive: false,
            all_occurrences: false,
            prefer_structural: false,
            receipt_id: None,
            cursor: None,
        })
        .await
        .expect("normalized search paths");
    assert!(search.hits.iter().any(|hit| hit.path == "src/lib.rs"));
    assert!(
        search
            .hits
            .iter()
            .any(|hit| hit.score_reasons.contains(&"focus path".to_owned()))
    );

    let context = services
        .context(ContextRequest {
            task: "find greet".into(),
            token_budget: 200,
            include_paths: Vec::new(),
            must_include_paths: Vec::new(),
            must_include_symbols: Vec::new(),
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
            changed_paths: vec![r".\src\lib.rs".into()],
            strict_changed_paths: false,
            verbose_diagnostics: false,
        })
        .await
        .expect("normalized context path");
    let scope = context.diff_scope.expect("explicit diff scope");
    assert_eq!(scope.changed_paths, vec!["src/lib.rs"]);
    assert_eq!(scope.indexed_changed_paths, 1);
}

#[tokio::test]
async fn token_savings_tracks_successful_source_retrievals_by_operation() {
    let (root, services) = fixture().await;
    let initial = services.token_savings().await.expect("initial savings");
    assert_eq!(initial.tracked_requests, 0);
    assert_eq!(initial.estimated_source_tokens_saved, 0);
    assert_eq!(initial.by_operation.len(), 4);

    let search = services
        .search(search_limit_request(Some(5), Some(100), Some(1)))
        .await
        .expect("search");
    let outline = services
        .outline(outline_limit_request(Some(10), Some(100)))
        .await
        .expect("outline");
    let first_read = services
        .read(ReadRequest {
            path: "src/lib.rs".into(),
            start_line: Some(1),
            end_line: Some(3),
            symbol: None,
            heading: None,
            heading_occurrence: None,
            continuation_cursor: None,
            max_tokens: Some(100),
            expected_hash: None,
            delta: false,
            receipt_id: None,
        })
        .await
        .expect("first read");
    let repeated_read = services
        .read(ReadRequest {
            path: "src/lib.rs".into(),
            start_line: Some(1),
            end_line: Some(3),
            symbol: None,
            heading: None,
            heading_occurrence: None,
            continuation_cursor: None,
            max_tokens: Some(100),
            expected_hash: Some(first_read.content_hash),
            delta: false,
            receipt_id: None,
        })
        .await
        .expect("conditional read");
    let context = services
        .context(context_limit_request(200))
        .await
        .expect("context");
    services
        .search(search_limit_request(Some(0), Some(100), Some(1)))
        .await
        .expect_err("zero max_results must fail");

    assert_eq!(repeated_read.status, ReadStatus::NotModified);
    let report = services.token_savings().await.expect("tracked savings");
    assert_eq!(report.tokenizer, services.config().tokenizer.name());
    assert_eq!(report.tracked_requests, 5);
    assert_eq!(report.by_operation.len(), 4);
    assert_eq!(
        report
            .by_operation
            .iter()
            .map(|row| (row.operation, row.tracked_requests))
            .collect::<Vec<_>>(),
        vec![
            (TokenSavingsOperation::Search, 1),
            (TokenSavingsOperation::Outline, 1),
            (TokenSavingsOperation::Read, 2),
            (TokenSavingsOperation::Context, 1),
        ]
    );
    assert_eq!(
        report.emitted_source_tokens,
        search.meta.emitted_tokens as u64
            + outline.meta.emitted_tokens as u64
            + first_read.meta.emitted_tokens as u64
            + context.meta.emitted_tokens as u64
    );
    assert!(report.baseline_source_tokens >= report.emitted_source_tokens);
    assert!(report.estimated_source_tokens_saved > 0);
    let effective = services
        .token_savings_report()
        .await
        .expect("effective savings");
    assert_eq!(effective.source_savings, report);
    let accounting = &effective.response_accounting;
    assert_eq!(accounting.tracked_requests, 5);
    assert_eq!(accounting.baseline_requests, 5);
    assert_eq!(
        accounting.total_response_tokens,
        [
            &search.meta,
            &outline.meta,
            &first_read.meta,
            &repeated_read.meta,
            &context.meta,
        ]
        .into_iter()
        .map(|meta| meta.total_response_tokens as u64)
        .sum::<u64>()
    );
    assert_eq!(
        accounting.path_and_metadata_tokens,
        [
            &search.meta,
            &outline.meta,
            &first_read.meta,
            &repeated_read.meta,
            &context.meta,
        ]
        .into_iter()
        .map(|meta| meta.path_and_metadata_tokens as u64)
        .sum::<u64>()
    );
    assert_eq!(
        accounting.protocol_tokens,
        [
            &search.meta,
            &outline.meta,
            &first_read.meta,
            &repeated_read.meta,
            &context.meta,
        ]
        .into_iter()
        .map(|meta| meta.protocol_tokens as u64)
        .sum::<u64>()
    );
    assert_eq!(
        accounting.estimated_net_tokens_saved,
        i64::try_from(accounting.baseline_source_tokens).expect("small baseline")
            - i64::try_from(accounting.total_response_tokens).expect("small response")
    );
    assert_eq!(
        accounting.response_source_tokens
            + accounting.path_and_metadata_tokens
            + accounting.protocol_tokens,
        accounting.total_response_tokens
    );
    assert_eq!(accounting.by_operation.len(), 8);
    assert_eq!(
        accounting
            .by_operation
            .iter()
            .map(|row| (row.operation, row.tracked_requests))
            .collect::<Vec<_>>(),
        vec![
            (TokenAccountingOperation::Files, 0),
            (TokenAccountingOperation::Search, 1),
            (TokenAccountingOperation::Outline, 1),
            (TokenAccountingOperation::Read, 2),
            (TokenAccountingOperation::ContextPlan, 0),
            (TokenAccountingOperation::Context, 1),
            (TokenAccountingOperation::Json, 0),
            (TokenAccountingOperation::History, 0),
        ]
    );
    let observed = services
        .observed_token_savings_report()
        .await
        .expect("observed accounting");
    assert_eq!(observed.report, effective);
    assert_eq!(observed.observations.successful_response_records, 5);
    assert_eq!(observed.observations.responses_with_baseline, 5);
    assert_eq!(observed.observations.source_compression_requests, 5);
    assert_eq!(observed.observations.failed_service_requests, 1);
    assert_eq!(
        observed.observations.expected_hash_not_modified_responses,
        1
    );
    assert!(
        observed
            .observations
            .expected_hash_suppressed_source_tokens
            > 0
    );
    assert_eq!(
        observed
            .observations
            .failed_by_operation_and_category
            .iter()
            .map(|failure| (
                failure.operation,
                failure.error_category.as_str(),
                failure.failed_requests
            ))
            .collect::<Vec<_>>(),
        vec![(TokenAccountingOperation::Search, "invalid_input", 1)]
    );
    assert!(observed.observations.unobserved.iter().any(|outcome| {
        outcome.contains("retry chains") && outcome.contains("task/outcome identifier")
    }));
    let serialized = serde_json::to_value(&observed).expect("serialize observed accounting");
    assert!(serialized.get("tokenizer").is_some());
    assert!(serialized.get("estimated_source_tokens_saved").is_some());
    assert!(serialized.get("response_accounting").is_some());
    assert!(serialized.get("observations").is_some());
    assert!(serialized.get("report").is_none());

    let config = Config::discover(root.path(), Some(root.path().join("index.sqlite")))
        .expect("reopen config");
    let reopened = Services::open(config).expect("reopen services");
    assert_eq!(
        reopened.token_savings().await.expect("persisted savings"),
        report
    );
    assert_eq!(
        reopened
            .token_savings_report()
            .await
            .expect("persisted effective savings"),
        effective
    );
    assert_eq!(
        reopened
            .observed_token_savings_report()
            .await
            .expect("persisted observed accounting"),
        observed
    );

    let mut alternate_config =
        Config::discover(root.path(), Some(root.path().join("index.sqlite")))
            .expect("alternate tokenizer config");
    alternate_config.tokenizer = Tokenizer::O200kBase;
    let alternate = Services::open(alternate_config).expect("alternate tokenizer services");
    alternate
        .outline(outline_limit_request(Some(10), Some(100)))
        .await
        .expect("outline against stale tokenizer index");
    assert_eq!(
        alternate
            .token_savings()
            .await
            .expect("alternate tokenizer savings")
            .tracked_requests,
        0
    );
}

#[tokio::test]
async fn multilingual_structural_indexing_returns_new_language_symbol_bodies() {
    let root = tempfile::tempdir().expect("root");
    for (path, source) in [
        (
            "target.c",
            "int c_target(int value) {\n    return value + 11;\n}\n",
        ),
        (
            "CSharpTarget.cs",
            "class CSharpTarget {\n    int CsharpTarget() {\n        return 66;\n    }\n}\n",
        ),
        (
            "target.cpp",
            "class CppTarget {\npublic:\n    int cpp_target() { return 22; }\n};\n",
        ),
        (
            "JavaTarget.java",
            "class JavaTarget {\n    int javaTarget() {\n        return 33;\n    }\n}\n",
        ),
        (
            "target.php",
            "<?php\nfunction phpTarget() {\n    return 44;\n}\n",
        ),
        (
            "target.rb",
            "def ruby_target\n  55\nend\n",
        ),
    ] {
        std::fs::write(root.path().join(path), source).expect("source");
    }
    let config =
        Config::discover(root.path(), Some(root.path().join("index.sqlite"))).expect("config");
    let services = Services::open(config).expect("services");
    services.index(false).await.expect("index");

    for (path, symbol, marker) in [
        ("target.c", "c_target", "return value + 11"),
        ("CSharpTarget.cs", "CsharpTarget", "return 66"),
        ("target.cpp", "cpp_target", "return 22"),
        ("JavaTarget.java", "javaTarget", "return 33"),
        ("target.php", "phpTarget", "return 44"),
        ("target.rb", "ruby_target", "55"),
    ] {
        let outline = services
            .outline(OutlineRequest {
                paths: vec![path.into()],
                symbol_name: Some(symbol.into()),
                symbol_kind: None,
                max_results: Some(10),
                max_tokens: Some(200),
                receipt_id: None,
                cursor: None,
            })
            .await
            .expect("outline");
        assert!(
            outline.files[0]
                .symbols
                .iter()
                .any(|item| item.name == symbol && item.end_line >= item.start_line),
            "missing {symbol} in {path}: {:?}",
            outline.files[0].symbols
        );

        let context = services
            .context(ContextRequest {
                task: format!("Fix {symbol}"),
                token_budget: 300,
                include_paths: Vec::new(),
                must_include_paths: Vec::new(),
                must_include_symbols: Vec::new(),
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
            verbose_diagnostics: false,
            })
            .await
            .expect("context");
        assert!(
            context
                .fragments
                .iter()
                .any(|fragment| fragment.path == path && fragment.content.contains(marker)),
            "missing body for {symbol}: {:?}",
            context.fragments
        );
    }
}

#[tokio::test]
async fn csharp_structure_supports_outline_search_reference_read_and_context() {
    let root = tempfile::tempdir().expect("root");
    std::fs::write(
        root.path().join("Worker.cs"),
        r#"using System.Text;

namespace Clinic.Core;

public sealed class Worker {
    public string Run(int value) {
        var builder = new StringBuilder();
        return Normalize(builder.Append(value).ToString());
    }

    private string Normalize(string value) => value.Trim();
}
"#,
    )
    .expect("C# source");
    let config =
        Config::discover(root.path(), Some(root.path().join("index.sqlite"))).expect("config");
    let services = Services::open(config).expect("services");
    services.index(false).await.expect("index");

    let outline = services
        .outline(OutlineRequest {
            paths: vec!["Worker.cs".into()],
            symbol_name: None,
            symbol_kind: None,
            max_results: Some(20),
            max_tokens: Some(2_000),
            receipt_id: None,
            cursor: None,
        })
        .await
        .expect("C# outline");
    assert_eq!(outline.files[0].language.as_deref(), Some("csharp"));
    assert!(
        outline.files[0].symbols.iter().any(|symbol| {
            symbol.name == "Run"
                && symbol.kind == "method"
                && symbol.parent.as_deref() == Some("Worker")
        }),
        "{:?}",
        outline.files[0].symbols
    );

    let symbol_search = services
        .search(SearchRequest {
            query: "Run".into(),
            mode: SearchMode::Symbol,
            include_paths: vec!["Worker.cs".into()],
            exclude_paths: Vec::new(),
            focus_paths: Vec::new(),
            max_results: Some(10),
            max_tokens: Some(2_000),
            context_lines: Some(0),
            case_sensitive: true,
            all_occurrences: false,
            prefer_structural: false,
            receipt_id: None,
            cursor: None,
        })
        .await
        .expect("C# symbol search");
    assert!(
        symbol_search
            .hits
            .iter()
            .any(|hit| hit.symbol.as_deref() == Some("Run"))
    );

    let reference_search = services
        .search(SearchRequest {
            query: "Normalize".into(),
            mode: SearchMode::Reference,
            include_paths: vec!["Worker.cs".into()],
            exclude_paths: Vec::new(),
            focus_paths: Vec::new(),
            max_results: Some(10),
            max_tokens: Some(2_000),
            context_lines: Some(0),
            case_sensitive: true,
            all_occurrences: false,
            prefer_structural: false,
            receipt_id: None,
            cursor: None,
        })
        .await
        .expect("C# reference search");
    assert!(reference_search.hits.iter().any(|hit| {
        hit.symbol.as_deref() == Some("Normalize")
            && hit.enclosing_symbol.as_deref() == Some("Run")
    }));

    let read = services
        .read(ReadRequest {
            path: "Worker.cs".into(),
            start_line: None,
            end_line: None,
            symbol: Some("Worker.Run".into()),
            heading: None,
            heading_occurrence: None,
            continuation_cursor: None,
            max_tokens: Some(2_000),
            expected_hash: None,
            delta: false,
            receipt_id: None,
        })
        .await
        .expect("qualified C# symbol read");
    assert!(
        read.content
            .as_deref()
            .is_some_and(|content| content.contains("return Normalize"))
    );

    let context = services
        .context(ContextRequest {
            task: "Fix the Run method".into(),
            token_budget: 500,
            include_paths: vec!["Worker.cs".into()],
            must_include_paths: Vec::new(),
            must_include_symbols: vec!["Run".into()],
            max_fragments: None,
            plan_only: false,
            focus_paths: Vec::new(),
            strict_focus_paths: false,
            minimum_fragments_per_focus_path: None,
            focus_symbols: vec!["Run".into()],
            exclude_paths: Vec::new(),
            known_hashes: Vec::new(),
            receipt_id: None,
            prior_repository_generation: None,
            base_revision: None,
            changed_paths: Vec::new(),
            strict_changed_paths: false,
            verbose_diagnostics: false,
        })
        .await
        .expect("C# context");
    assert!(
        context.fragments.iter().any(|fragment| {
            fragment.path == "Worker.cs" && fragment.content.contains("return Normalize")
        }),
        "{:?}",
        context.fragments
    );
}

#[tokio::test]
async fn javascript_and_typescript_data_bindings_support_outline_search_and_read() {
    let root = tempfile::tempdir().expect("root");
    std::fs::write(
        root.path().join("clinic.js"),
        r#"export const clinicMedicines = [
  { id: "moon-rabbit-saline", labels: { en: "Saline", ja: "生理食塩水" } },
  { id: "boundary-anchor-patch", labels: { en: "Patch", zh: "貼片" } }
];
function helper() {
  const localOnly = { hidden: true };
  return localOnly;
}
"#,
    )
    .expect("JavaScript data");
    std::fs::write(
        root.path().join("copy.ts"),
        r#"export const copy: Record<string, string> = {
  en: "Campus clinic",
  ja: "キャンパス診療所",
  zh: "校園診所"
} satisfies Record<string, string>;
"#,
    )
    .expect("TypeScript data");
    let config =
        Config::discover(root.path(), Some(root.path().join("index.sqlite"))).expect("config");
    let services = Services::open(config).expect("services");
    services.index(false).await.expect("index");

    for (path, symbol, marker) in [
        ("clinic.js", "clinicMedicines", "boundary-anchor-patch"),
        ("copy.ts", "copy", "校園診所"),
    ] {
        let outline = services
            .outline(OutlineRequest {
                paths: vec![path.into()],
                symbol_name: None,
                symbol_kind: None,
                max_results: Some(20),
                max_tokens: Some(2_000),
                receipt_id: None,
                cursor: None,
            })
            .await
            .expect("outline data file");
        assert!(
            outline.files[0]
                .symbols
                .iter()
                .any(|item| item.name == symbol && item.kind == "constant"),
            "missing {symbol}: {:?}",
            outline.files[0].symbols
        );
        assert!(
            !outline.files[0]
                .symbols
                .iter()
                .any(|item| item.name == "localOnly")
        );

        let search = services
            .search(SearchRequest {
                query: symbol.into(),
                mode: SearchMode::Symbol,
                include_paths: vec![path.into()],
                exclude_paths: Vec::new(),
                focus_paths: Vec::new(),
                max_results: Some(10),
                max_tokens: Some(2_000),
                context_lines: Some(0),
                case_sensitive: true,
                all_occurrences: false,
                prefer_structural: false,
                receipt_id: None,
                cursor: None,
            })
            .await
            .expect("symbol search");
        assert!(
            search
                .hits
                .iter()
                .any(|hit| hit.symbol.as_deref() == Some(symbol))
        );

        let read = services
            .read(ReadRequest {
                path: path.into(),
                start_line: None,
                end_line: None,
                symbol: Some(symbol.into()),
                heading: None,
                heading_occurrence: None,
                continuation_cursor: None,
                max_tokens: Some(2_000),
                expected_hash: None,
                delta: false,
                receipt_id: None,
            })
            .await
            .expect("symbol read");
        assert!(
            read.content
                .as_deref()
                .is_some_and(|content| content.contains(marker)),
            "missing {marker} in {symbol} read: {:?}",
            read.content
        );
    }
}

#[tokio::test]
async fn html_and_css_structure_support_outline_search_reference_and_read() {
    let root = tempfile::tempdir().expect("root");
    std::fs::create_dir(root.path().join("styles")).expect("styles directory");
    std::fs::create_dir(root.path().join("js")).expect("JavaScript directory");
    std::fs::write(
        root.path().join("styles/clinic.css"),
        r#":root {
  --clinic-accent: #087;
}
.clinic-hero {
  color: var(--clinic-accent);
}
.clinic-card, #clinic-panel > .clinic-title {
  display: grid;
}
@media (max-width: 720px) {
  .clinic-hero { display: block; }
}
"#,
    )
    .expect("CSS source");
    std::fs::write(
        root.path().join("index.html"),
        r##"<!doctype html>
<html>
<head>
  <link rel="stylesheet" href="./styles/clinic.css">
</head>
<body>
  <nav id="mobile-nav" data-action="toggle-nav">
    <a href="#clinic">Clinic</a>
  </nav>
  <section id="clinic">
    <form id="clinic-form">
      <button data-action="book-therapy">Book</button>
    </form>
  </section>
  <script type="module" src="./js/clinic.js"></script>
</body>
</html>
"##,
    )
    .expect("HTML source");
    std::fs::write(root.path().join("js/clinic.js"), "export const clinic = {};\n")
        .expect("JavaScript source");
    let config =
        Config::discover(root.path(), Some(root.path().join("index.sqlite"))).expect("config");
    let services = Services::open(config).expect("services");
    services.index(false).await.expect("index");

    let outline = services
        .outline(OutlineRequest {
            paths: vec!["styles/clinic.css".into(), "index.html".into()],
            symbol_name: None,
            symbol_kind: None,
            max_results: Some(100),
            max_tokens: Some(4_000),
            receipt_id: None,
            cursor: None,
        })
        .await
        .expect("frontend outlines");
    let css = outline
        .files
        .iter()
        .find(|file| file.path == "styles/clinic.css")
        .expect("CSS outline");
    assert!(css.structurally_complete);
    assert!(
        css.symbols
            .iter()
            .any(|symbol| symbol.name == ".clinic-hero" && symbol.kind == "css_selector")
    );
    assert!(
        css.symbols.iter().any(
            |symbol| symbol.name == "--clinic-accent" && symbol.kind == "css_custom_property"
        )
    );
    let html = outline
        .files
        .iter()
        .find(|file| file.path == "index.html")
        .expect("HTML outline");
    assert!(html.structurally_complete);
    assert!(
        html.symbols
            .iter()
            .any(|symbol| symbol.name == "#clinic" && symbol.kind == "html_id")
    );
    assert_eq!(
        html.imports
            .iter()
            .map(|import| (
                import.raw_target.as_str(),
                import.resolved_path.as_deref()
            ))
            .collect::<Vec<_>>(),
        vec![
            ("./styles/clinic.css", Some("styles/clinic.css")),
            ("./js/clinic.js", Some("js/clinic.js"))
        ]
    );

    for (query, mode, path) in [
        (".clinic-hero", SearchMode::Symbol, "styles/clinic.css"),
        (".clinic-title", SearchMode::Reference, "styles/clinic.css"),
        ("#clinic", SearchMode::Reference, "index.html"),
        (
            "data-action=book-therapy",
            SearchMode::Reference,
            "index.html",
        ),
    ] {
        let search = services
            .search(SearchRequest {
                query: query.into(),
                mode,
                include_paths: vec![path.into()],
                exclude_paths: Vec::new(),
                focus_paths: Vec::new(),
                max_results: Some(10),
                max_tokens: Some(2_000),
                context_lines: Some(0),
                case_sensitive: true,
                all_occurrences: false,
                prefer_structural: false,
                receipt_id: None,
                cursor: None,
            })
            .await
            .expect("structural search");
        assert!(!search.hits.is_empty(), "missing {mode:?} search for {query}");
    }

    for (path, symbol, marker) in [
        (
            "styles/clinic.css",
            ".clinic-hero",
            "color: var(--clinic-accent)",
        ),
        ("index.html", "#clinic", "data-action=\"book-therapy\""),
    ] {
        let read = services
            .read(ReadRequest {
                path: path.into(),
                start_line: None,
                end_line: None,
                symbol: Some(symbol.into()),
                heading: None,
                heading_occurrence: None,
                continuation_cursor: None,
                max_tokens: Some(2_000),
                expected_hash: None,
                delta: false,
                receipt_id: None,
            })
            .await
            .expect("structural symbol read");
        assert!(
            read.content
                .as_deref()
                .is_some_and(|content| content.contains(marker)),
            "missing {marker} from {symbol} read: {:?}",
            read.content
        );
    }
}

#[tokio::test]
async fn markdown_outline_and_heading_read_preserve_section_structure_and_occurrences() {
    let root = tempfile::tempdir().expect("root");
    std::fs::write(
        root.path().join("README.md"),
        "\
# Root
intro
## Repeat
first
### Child
child
## Repeat
second

Setext
------
```markdown
# hidden
```
",
    )
    .expect("Markdown source");
    let config =
        Config::discover(root.path(), Some(root.path().join("index.sqlite"))).expect("config");
    let services = Services::open(config).expect("services");
    services.index(false).await.expect("index");

    let outline = services
        .outline(OutlineRequest {
            paths: vec!["README.md".into()],
            symbol_name: None,
            symbol_kind: Some("markdown_heading".into()),
            max_results: Some(20),
            max_tokens: Some(2_000),
            receipt_id: None,
            cursor: None,
        })
        .await
        .expect("Markdown outline");
    assert!(outline.parse_complete);
    assert!(outline.result_complete);
    assert_eq!(outline.total_symbols, 5);
    assert_eq!(
        outline.symbol_counts_by_kind.get("markdown_heading"),
        Some(&5)
    );
    let markdown = &outline.files[0];
    assert_eq!(markdown.language.as_deref(), Some("markdown"));
    assert!(markdown.parse_complete);
    assert_eq!(
        markdown
            .symbols
            .iter()
            .map(|symbol| (
                symbol.name.as_str(),
                symbol.parent.as_deref(),
                symbol.start_line,
                symbol.end_line,
            ))
            .collect::<Vec<_>>(),
        vec![
            ("Root", None, 1, 14),
            ("Repeat", Some("Root"), 3, 6),
            ("Child", Some("Repeat"), 5, 6),
            ("Repeat", Some("Root"), 7, 9),
            ("Setext", Some("Root"), 10, 14),
        ]
    );
    assert!(!markdown.symbols.iter().any(|symbol| symbol.name == "hidden"));

    for (occurrence, expected_range, expected_content) in [
        (
            None,
            (3, 6),
            "## Repeat\nfirst\n### Child\nchild",
        ),
        (Some(2), (7, 9), "## Repeat\nsecond"),
    ] {
        let read = services
            .read(ReadRequest {
                path: "README.md".into(),
                start_line: None,
                end_line: None,
                symbol: None,
                heading: Some(if occurrence == Some(2) {
                    "## Repeat".into()
                } else {
                    "Repeat".into()
                }),
                heading_occurrence: occurrence,
                continuation_cursor: None,
                max_tokens: Some(2_000),
                expected_hash: None,
                delta: false,
                receipt_id: None,
            })
            .await
            .expect("Markdown heading read");
        assert_eq!(
            (read.target_start_line, read.target_end_line),
            expected_range
        );
        assert_eq!(read.content.as_deref().map(str::trim_end), Some(expected_content));
    }

    let error = services
        .read(ReadRequest {
            path: "README.md".into(),
            start_line: None,
            end_line: None,
            symbol: None,
            heading: Some("Repeat".into()),
            heading_occurrence: Some(3),
            continuation_cursor: None,
            max_tokens: Some(2_000),
            expected_hash: None,
            delta: false,
            receipt_id: None,
        })
        .await
        .expect_err("missing duplicate occurrence");
    assert!(matches!(
        error,
        Error::HeadingNotFound {
            path,
            heading,
            occurrence: 3
        } if path == "README.md" && heading == "Repeat"
    ));

    let error = services
        .read(ReadRequest {
            path: "README.md".into(),
            start_line: None,
            end_line: None,
            symbol: None,
            heading: Some("Repeat".into()),
            heading_occurrence: Some(0),
            continuation_cursor: None,
            max_tokens: Some(2_000),
            expected_hash: None,
            delta: false,
            receipt_id: None,
        })
        .await
        .expect_err("zero heading occurrence");
    assert!(matches!(
        error,
        Error::InvalidInput {
            field: "heading occurrence",
            reason: "must be one-based"
        }
    ));
}

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
    services.index(false).await.expect("index");

    let exact = services
        .context_evaluation(ContextRequest {
            task: "Fix OwnerAlpha".into(),
            token_budget: 400,
            include_paths: Vec::new(),
            must_include_paths: Vec::new(),
            must_include_symbols: Vec::new(),
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
        verbose_diagnostics: false,
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
        verbose_diagnostics: false,
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
    services.index(false).await.expect("index");
    let request = ContextRequest {
        task: "Fix OwnerAlpha and OtherSignal".into(),
        token_budget: 400,
        include_paths: Vec::new(),
        must_include_paths: Vec::new(),
        must_include_symbols: Vec::new(),
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
    verbose_diagnostics: false,
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
    assert!(callers
        .generated_candidates
        .iter()
        .any(|candidate| candidate.match_kinds.iter().any(|kind| kind == "reference")));
    assert!(reverse.generated_candidates.iter().any(|candidate| {
        candidate.path == "src/seed.js"
            && candidate
                .match_kinds
                .iter()
                .any(|kind| kind == "reverse-import")
    }));
    assert!(imports
        .generated_candidates
        .iter()
        .all(|candidate| !candidate.match_kinds.iter().any(|kind| kind == "reference")));
    assert!(callers.generated_candidates.iter().all(|candidate| {
        candidate.representation != "import_symbol"
            && !candidate
                .match_kinds
                .iter()
                .any(|kind| kind == "reverse-import")
    }));
}

#[tokio::test]
async fn file_operations_page_without_duplicates() {
    let root = tempfile::tempdir().expect("root");
    for name in ["alpha.rs", "bravo.rs", "charlie.rs", "delta.rs", "echo.rs"] {
        std::fs::write(root.path().join(name), format!("fn {}() {{}}\n", &name[..name.len() - 3]))
            .expect("source");
    }
    let config =
        Config::discover(root.path(), Some(root.path().join("index.sqlite"))).expect("config");
    let services = Services::open(config).expect("services");
    services.index(false).await.expect("index");

    for operation in [
        FileOperation::Tree,
        FileOperation::Glob,
        FileOperation::Find,
    ] {
        let mut cursor = None;
        let mut paths = Vec::new();
        loop {
            let response = services
                .files(FilesRequest {
                    operation: operation.clone(),
                    path: None,
                    query: matches!(operation, FileOperation::Find).then(|| "rs".into()),
                    pattern: matches!(operation, FileOperation::Glob).then(|| "*.rs".into()),
                    max_results: Some(2),
                    cursor,
                    depth: Some(1),
                })
                .await
                .expect("file page");
            paths.extend(response.entries.into_iter().map(|entry| entry.path));
            cursor = response.meta.next_cursor;
            if cursor.is_none() {
                break;
            }
        }
        let unique = paths.iter().collect::<std::collections::HashSet<_>>();
        assert_eq!(paths.len(), 5, "{operation:?}");
        assert_eq!(unique.len(), paths.len(), "{operation:?}");
    }

    let tree = services
        .files(FilesRequest {
            operation: FileOperation::Tree,
            path: None,
            query: None,
            pattern: None,
            max_results: Some(2),
            cursor: None,
            depth: Some(1),
        })
        .await
        .expect("tree page");
    let error = services
        .files(FilesRequest {
            operation: FileOperation::Glob,
            path: None,
            query: None,
            pattern: Some("*.rs".into()),
            max_results: Some(2),
            cursor: tree.meta.next_cursor,
            depth: None,
        })
        .await
        .expect_err("cursor from another operation");
    assert!(matches!(error, Error::StaleCursor));
}

#[tokio::test]
async fn file_tree_projection_respects_root_depth_and_removes_empty_directories() {
    let root = tempfile::tempdir().expect("root");
    std::fs::create_dir_all(root.path().join("src/deep")).expect("directories");
    std::fs::write(root.path().join("src/top.rs"), "fn top() {}\n").expect("top source");
    std::fs::write(root.path().join("src/deep/lib.rs"), "fn deep() {}\n")
        .expect("deep source");
    let services = Services::open(
        Config::discover(root.path(), Some(root.path().join("index.sqlite"))).expect("config"),
    )
    .expect("services");
    services.index(false).await.expect("index");

    let tree = services
        .files(FilesRequest {
            operation: FileOperation::Tree,
            path: Some("src".into()),
            query: None,
            pattern: None,
            max_results: Some(20),
            cursor: None,
            depth: Some(1),
        })
        .await
        .expect("tree");
    assert_eq!(
        tree.entries
            .iter()
            .map(|entry| entry.path.as_str())
            .collect::<Vec<_>>(),
        vec!["src", "src/deep", "src/top.rs"]
    );

    std::fs::remove_file(root.path().join("src/deep/lib.rs")).expect("delete deep source");
    services
        .index_paths(vec!["src/deep/lib.rs".into()])
        .await
        .expect("reconcile deletion");
    let after = services
        .files(FilesRequest {
            operation: FileOperation::Tree,
            path: Some("src".into()),
            query: None,
            pattern: None,
            max_results: Some(20),
            cursor: None,
            depth: Some(2),
        })
        .await
        .expect("tree after deletion");
    assert!(after.entries.iter().all(|entry| entry.path != "src/deep"));
}

#[tokio::test]
async fn file_tree_normalizes_equivalent_roots_before_query_and_pagination() {
    let root = tempfile::tempdir().expect("root");
    std::fs::create_dir_all(root.path().join("src/rust")).expect("directories");
    std::fs::write(root.path().join("README.md"), "fixture\n").expect("readme");
    std::fs::write(root.path().join("src/lib.rs"), "fn lib() {}\n").expect("lib source");
    std::fs::write(root.path().join("src/rust/a.rs"), "fn a() {}\n").expect("a source");
    std::fs::write(root.path().join("src/rust/b.rs"), "fn b() {}\n").expect("b source");
    let services = Services::open(
        Config::discover(root.path(), Some(root.path().join("index.sqlite"))).expect("config"),
    )
    .expect("services");
    services.index(false).await.expect("index");

    for aliases in [
        vec![None, Some(""), Some("."), Some("./")],
        vec![Some("src"), Some("./src"), Some("src/")],
        vec![
            Some("src/rust"),
            Some("./src//rust"),
            Some("src/rust/"),
        ],
    ] {
        let expected = tree_pages(&services, aliases[0]).await;
        assert!(expected.len() > 1, "fixture must exercise pagination");
        for alias in aliases.into_iter().skip(1) {
            assert_eq!(tree_pages(&services, alias).await, expected, "alias {alias:?}");
        }
    }
}

#[tokio::test]
async fn invalid_focus_glob_is_a_typed_error() {
    let (_root, services) = fixture().await;
    let error = services
        .search(SearchRequest {
            query: "greet".into(),
            mode: SearchMode::Auto,
            include_paths: Vec::new(),
            exclude_paths: Vec::new(),
            focus_paths: vec!["[".into()],
            max_results: None,
            max_tokens: None,
            context_lines: None,
            case_sensitive: false,
            all_occurrences: false,
            prefer_structural: false,
            receipt_id: None,
            cursor: None,
        })
        .await
        .expect_err("invalid glob must fail");
    assert!(error.to_string().contains("glob"));
}

#[tokio::test]
async fn file_tree_rejects_unsafe_roots() {
    let (_root, services) = fixture().await;
    for path in ["/src", "../src", "src/../rust", "src\0rust"] {
        services
            .files(FilesRequest {
                operation: FileOperation::Tree,
                path: Some(path.into()),
                query: None,
                pattern: None,
                max_results: None,
                cursor: None,
                depth: None,
            })
            .await
            .expect_err("unsafe tree root must fail");
    }
}

#[tokio::test]
async fn search_range_covers_the_returned_context_lines() {
    let (_root, services) = fixture().await;
    let response = services
        .search(SearchRequest {
            query: "agent".into(),
            mode: SearchMode::Text,
            include_paths: Vec::new(),
            exclude_paths: Vec::new(),
            focus_paths: Vec::new(),
            max_results: Some(1),
            max_tokens: Some(100),
            context_lines: Some(1),
            case_sensitive: false,
            all_occurrences: false,
            prefer_structural: false,
            receipt_id: None,
            cursor: None,
        })
        .await
        .expect("search");

    let hit = response.hits.first().expect("text hit");
    assert_eq!((hit.start_line, hit.end_line), (5, 7));
    assert_eq!(hit.excerpt.lines().count(), 3);
    assert_eq!(hit.enclosing_symbol.as_deref(), Some("caller"));
}

#[tokio::test]
async fn text_search_windows_keep_case_insensitive_matches_across_a_chunk() {
    let mut lines = (1..=60)
        .map(|line| format!("ordinary line {line}"))
        .collect::<Vec<_>>();
    let cases = [
        (30usize, "MiddleNeedle"),
        (59usize, "LateNeedle"),
        (2usize, "EarlyNeedle"),
    ];
    for (line, needle) in cases {
        lines[line - 1] = format!("{needle} is anchored here");
    }
    let source = format!("{}\n", lines.join("\n"));
    let (_root, services) = indexed_source("positions.txt", source.as_bytes()).await;

    for (match_line, needle) in cases {
        let response = services
            .search(SearchRequest {
                query: needle.to_ascii_lowercase(),
                mode: SearchMode::Text,
                include_paths: vec!["positions.txt".into()],
                exclude_paths: Vec::new(),
                focus_paths: Vec::new(),
                max_results: Some(1),
                max_tokens: Some(1_000),
                context_lines: Some(20),
                case_sensitive: false,
                all_occurrences: false,
                prefer_structural: false,
                receipt_id: None,
                cursor: None,
            })
            .await
            .expect("case-insensitive text search");

        let hit = response.hits.first().expect("text hit");
        assert!(
            hit.excerpt.contains(needle),
            "excerpt for line {match_line} omitted {needle}: {:?}",
            hit.excerpt
        );
        assert_eq!(hit.match_kind, "text");
        assert!(hit.start_line <= match_line && hit.end_line >= match_line);
        assert_eq!(
            hit.end_line - hit.start_line + 1,
            hit.excerpt.lines().count()
        );
        assert_eq!(hit.excerpt.lines().count(), 20);
    }
}

#[tokio::test]
async fn maximum_text_context_keeps_the_original_read_bounded_range_match() {
    let mut lines = (1..=50)
        .map(|line| format!("// legacy source line {line}"))
        .collect::<Vec<_>>();
    lines[29] = "fn read_bounded_range() {}".into();
    let source = format!("{}\n", lines.join("\n"));
    let (_root, services) = indexed_source("legacy.rs", source.as_bytes()).await;

    let response = services
        .search(SearchRequest {
            query: "read_bounded_range".into(),
            mode: SearchMode::Text,
            include_paths: vec!["legacy.rs".into()],
            exclude_paths: Vec::new(),
            focus_paths: Vec::new(),
            max_results: Some(1),
            max_tokens: Some(1_000),
            context_lines: Some(20),
            case_sensitive: true,
            all_occurrences: false,
            prefer_structural: false,
            receipt_id: None,
            cursor: None,
        })
        .await
        .expect("legacy reproduction search");

    let hit = response.hits.first().expect("legacy text hit");
    assert!(hit.excerpt.contains("read_bounded_range"));
    assert!(hit.start_line <= 30 && hit.end_line >= 30);
}

#[tokio::test]
async fn regex_search_keeps_a_multiline_match_that_exceeds_the_line_cap() {
    let mut lines = (1..=5)
        .map(|line| format!("prefix {line}"))
        .collect::<Vec<_>>();
    lines.push("MATCH_BEGIN".into());
    lines.extend((1..=24).map(|line| format!("matched body {line}")));
    lines.push("MATCH_END".into());
    lines.extend((1..=5).map(|line| format!("suffix {line}")));
    let source = format!("{}\n", lines.join("\n"));
    let (_root, services) = indexed_source("multiline.txt", source.as_bytes()).await;

    let response = services
        .search(SearchRequest {
            query: "(?s)MATCH_BEGIN.*?MATCH_END".into(),
            mode: SearchMode::Regex,
            include_paths: vec!["multiline.txt".into()],
            exclude_paths: Vec::new(),
            focus_paths: Vec::new(),
            max_results: Some(1),
            max_tokens: Some(5_000),
            context_lines: Some(20),
            case_sensitive: true,
            all_occurrences: false,
            prefer_structural: false,
            receipt_id: None,
            cursor: None,
        })
        .await
        .expect("multiline regex search");

    let hit = response.hits.first().expect("regex hit");
    assert!(hit.excerpt.contains("MATCH_BEGIN"));
    assert!(hit.excerpt.contains("MATCH_END"));
    assert_eq!((hit.start_line, hit.end_line), (6, 31));
    assert_eq!(
        hit.end_line - hit.start_line + 1,
        hit.excerpt.lines().count()
    );
    assert_eq!(hit.excerpt.lines().count(), 26);
}

#[tokio::test]
async fn symbol_search_caps_a_long_definition_without_losing_its_declaration() {
    let mut lines = (1..=20)
        .map(|line| format!("const PREFIX_{line}: usize = {line};"))
        .collect::<Vec<_>>();
    let declaration_line = lines.len() + 1;
    lines.push("fn long_target() -> usize {".into());
    lines.extend((1..=40).map(|line| format!("    let value_{line} = {line};")));
    lines.push("    40".into());
    lines.push("}".into());
    let source = format!("{}\n", lines.join("\n"));
    let (_root, services) = indexed_source("long_symbol.rs", source.as_bytes()).await;

    let response = services
        .search(SearchRequest {
            query: "long_target".into(),
            mode: SearchMode::Symbol,
            include_paths: vec!["long_symbol.rs".into()],
            exclude_paths: Vec::new(),
            focus_paths: Vec::new(),
            max_results: Some(1),
            max_tokens: Some(2_000),
            context_lines: Some(20),
            case_sensitive: true,
            all_occurrences: false,
            prefer_structural: false,
            receipt_id: None,
            cursor: None,
        })
        .await
        .expect("long symbol search");

    let hit = response.hits.first().expect("symbol hit");
    assert!(hit.excerpt.contains("fn long_target()"));
    assert!(hit.start_line <= declaration_line && hit.end_line >= declaration_line);
    assert_eq!(hit.excerpt.lines().count(), 30);
    assert_eq!(hit.end_line - hit.start_line + 1, 30);
}

#[tokio::test]
async fn reference_search_window_keeps_the_required_reference_span() {
    let mut lines = vec!["fn target() {}".to_string(), String::new(), "fn caller() {".into()];
    lines.extend((1..=25).map(|line| format!("    let value_{line} = {line};")));
    let reference_line = lines.len() + 1;
    lines.push("    target();".into());
    lines.push("}".into());
    let source = format!("{}\n", lines.join("\n"));
    let (_root, services) = indexed_source("reference.rs", source.as_bytes()).await;

    let response = services
        .search(SearchRequest {
            query: "target".into(),
            mode: SearchMode::Reference,
            include_paths: vec!["reference.rs".into()],
            exclude_paths: Vec::new(),
            focus_paths: Vec::new(),
            max_results: Some(1),
            max_tokens: Some(1_000),
            context_lines: Some(20),
            case_sensitive: true,
            all_occurrences: false,
            prefer_structural: false,
            receipt_id: None,
            cursor: None,
        })
        .await
        .expect("reference search");

    let hit = response.hits.first().expect("reference hit");
    assert!(hit.excerpt.contains("target();"));
    assert!(hit.start_line <= reference_line && hit.end_line >= reference_line);
    assert_eq!(
        hit.end_line - hit.start_line + 1,
        hit.excerpt.lines().count()
    );
    assert_eq!(hit.excerpt.lines().count(), 12);
}

#[tokio::test]
async fn text_search_reports_enclosing_symbols_across_languages() {
    let root = tempfile::tempdir().expect("temporary repository");
    std::fs::write(
        root.path().join("owner.rs"),
        "fn rust_owner() {\n    let known_hashes: Vec<String> = Vec::new();\n}\n",
    )
    .expect("Rust source");
    std::fs::write(
        root.path().join("owner.py"),
        "def python_owner():\n    known_hashes = []\n    return known_hashes\n",
    )
    .expect("Python source");
    std::fs::write(
        root.path().join("owner.js"),
        "function javascriptOwner() {\n  const known_hashes = [];\n  return known_hashes;\n}\n",
    )
    .expect("JavaScript source");
    let services = Services::open(
        Config::discover(root.path(), Some(root.path().join("index.sqlite"))).expect("config"),
    )
    .expect("services");
    services.index(false).await.expect("index");

    let response = services
        .search(SearchRequest {
            query: "known_hashes".into(),
            mode: SearchMode::Text,
            include_paths: Vec::new(),
            exclude_paths: Vec::new(),
            focus_paths: Vec::new(),
            max_results: Some(10),
            max_tokens: Some(1_000),
            context_lines: Some(1),
            case_sensitive: true,
            all_occurrences: false,
            prefer_structural: false,
            receipt_id: None,
            cursor: None,
        })
        .await
        .expect("search");
    let owners = response
        .hits
        .into_iter()
        .map(|hit| (hit.path, hit.enclosing_symbol))
        .collect::<std::collections::HashMap<_, _>>();

    assert_eq!(
        owners.get("owner.rs").and_then(Option::as_deref),
        Some("rust_owner")
    );
    assert_eq!(
        owners.get("owner.py").and_then(Option::as_deref),
        Some("python_owner")
    );
    assert_eq!(
        owners.get("owner.js").and_then(Option::as_deref),
        Some("javascriptOwner")
    );
}

#[tokio::test]
async fn text_search_preserves_multiline_matches_without_a_single_matching_line() {
    let root = tempfile::tempdir().expect("temporary repository");
    std::fs::write(
        root.path().join("owner.rs"),
        "fn multiline_owner() {\n    first_line();\n    second_line();\n}\n",
    )
    .expect("Rust source");
    let services = Services::open(
        Config::discover(root.path(), Some(root.path().join("index.sqlite"))).expect("config"),
    )
    .expect("services");
    services.index(false).await.expect("index");

    let response = services
        .search(SearchRequest {
            query: "first_line();\n    second_line();".into(),
            mode: SearchMode::Text,
            include_paths: Vec::new(),
            exclude_paths: Vec::new(),
            focus_paths: Vec::new(),
            max_results: Some(10),
            max_tokens: Some(1_000),
            context_lines: Some(1),
            case_sensitive: true,
            all_occurrences: false,
            prefer_structural: false,
            receipt_id: None,
            cursor: None,
        })
        .await
        .expect("search");

    let hit = response.hits.first().expect("multiline text hit");
    assert_eq!(hit.path, "owner.rs");
    assert!(hit.excerpt.contains("first_line();\n    second_line();"));
    assert_eq!(hit.enclosing_symbol.as_deref(), Some("multiline_owner"));
}

#[tokio::test]
async fn read_reports_live_content_that_differs_from_the_index() {
    let (root, services) = fixture().await;
    let first = services
        .read(ReadRequest {
            path: "src/lib.rs".into(),
            start_line: Some(1),
            end_line: Some(1),
            symbol: None,
            heading: None,
            heading_occurrence: None,
            continuation_cursor: None,
            max_tokens: Some(100),
            expected_hash: None,
            delta: false,
            receipt_id: None,
        })
        .await
        .expect("indexed read");

    std::fs::write(
        root.path().join("src/lib.rs"),
        "pub fn changed() -> bool { true }\n",
    )
    .expect("change live file");

    let changed = services
        .read(ReadRequest {
            path: "src/lib.rs".into(),
            start_line: Some(1),
            end_line: Some(1),
            symbol: None,
            heading: None,
            heading_occurrence: None,
            continuation_cursor: None,
            max_tokens: Some(100),
            expected_hash: Some(first.content_hash.clone()),
            delta: false,
            receipt_id: None,
        })
        .await
        .expect("live read");

    assert_eq!(changed.status, ReadStatus::Content);
    assert!(changed.index_stale);
    assert_ne!(changed.content_hash, first.content_hash);
    assert_eq!(
        changed.content.as_deref(),
        Some("pub fn changed() -> bool { true }\n")
    );
}

#[tokio::test]
async fn read_delta_returns_a_complete_strictly_cheaper_edit() {
    let source = (1..=80)
        .map(|line| format!("let value_{line} = compute_value({line});\n"))
        .collect::<String>();
    let (root, services) = indexed_source("delta.rs", source.as_bytes()).await;
    let first = services
        .read(ReadRequest {
            path: "delta.rs".into(),
            start_line: None,
            end_line: None,
            symbol: None,
            heading: None,
            heading_occurrence: None,
            continuation_cursor: None,
            max_tokens: Some(32_000),
            expected_hash: None,
            delta: true,
            receipt_id: None,
        })
        .await
        .expect("capture delta base");
    let first_receipt = first.delta_receipt.as_ref().expect("base receipt");
    assert_eq!(first_receipt.outcome, ReadDeltaOutcome::Full);
    assert_eq!(first_receipt.head_hash, first.content_hash);
    assert!(first_receipt.base_hash.is_none());
    let base_hash = first.content_hash.clone();

    let unchanged = services
        .read(ReadRequest {
            path: "delta.rs".into(),
            start_line: None,
            end_line: None,
            symbol: None,
            heading: None,
            heading_occurrence: None,
            continuation_cursor: None,
            max_tokens: Some(32_000),
            expected_hash: Some(base_hash.clone()),
            delta: true,
            receipt_id: None,
        })
        .await
        .expect("read unchanged delta target");
    assert_eq!(unchanged.status, ReadStatus::NotModified);
    assert!(unchanged.content.is_none());
    assert!(unchanged.delta.is_none());
    let unchanged_receipt = unchanged.delta_receipt.expect("not-modified receipt");
    assert_eq!(unchanged_receipt.outcome, ReadDeltaOutcome::NotModified);
    assert_eq!(unchanged_receipt.delta_tokens, Some(0));
    assert_eq!(
        unchanged_receipt.avoided_tokens,
        unchanged_receipt.full_tokens
    );

    let changed_source = source.replace(
        "let value_40 = compute_value(40);",
        "let value_40 = compute_updated_value(40);",
    );
    std::fs::write(root.path().join("delta.rs"), changed_source).expect("edit source");
    let changed = services
        .read(ReadRequest {
            path: "delta.rs".into(),
            start_line: None,
            end_line: None,
            symbol: None,
            heading: None,
            heading_occurrence: None,
            continuation_cursor: None,
            max_tokens: Some(32_000),
            expected_hash: Some(base_hash),
            delta: true,
            receipt_id: None,
        })
        .await
        .expect("read changed delta");

    assert_eq!(changed.status, ReadStatus::Delta);
    assert!(changed.content.is_none());
    assert!(changed.index_stale);
    let delta = changed.delta.as_deref().expect("unified diff");
    assert!(delta.contains("-let value_40 = compute_value(40);"));
    assert!(delta.contains("+let value_40 = compute_updated_value(40);"));
    let receipt = changed.delta_receipt.as_ref().expect("delta receipt");
    assert_eq!(receipt.outcome, ReadDeltaOutcome::Delta);
    assert_eq!(receipt.base_generation, Some(first_receipt.head_generation));
    assert_eq!(receipt.head_hash, changed.content_hash);
    assert_eq!(receipt.delta_tokens, Some(changed.meta.emitted_tokens));
    assert!(receipt.full_tokens > changed.meta.emitted_tokens);
    assert_eq!(
        receipt.avoided_tokens,
        receipt.full_tokens - changed.meta.emitted_tokens
    );
    assert!(receipt.fallback_reason.is_none());
    assert_response_token_accounting!(changed, Tokenizer::Cl100kBase);
}

#[tokio::test]
async fn read_delta_does_not_capture_or_diff_a_truncated_page() {
    let source = (1..=80)
        .map(|line| format!("let value_{line} = compute_value({line});\n"))
        .collect::<String>();
    let (_root, services) = indexed_source("truncated.rs", source.as_bytes()).await;

    let response = services
        .read(ReadRequest {
            path: "truncated.rs".into(),
            start_line: None,
            end_line: None,
            symbol: None,
            heading: None,
            heading_occurrence: None,
            continuation_cursor: None,
            max_tokens: Some(20),
            expected_hash: None,
            delta: true,
            receipt_id: None,
        })
        .await
        .expect("read truncated delta target");

    assert_eq!(response.status, ReadStatus::Truncated);
    assert!(response.truncated);
    assert!(response.content.is_some());
    assert!(response.delta.is_none());
    let receipt = response.delta_receipt.expect("truncation receipt");
    assert_eq!(receipt.outcome, ReadDeltaOutcome::Full);
    assert_eq!(
        receipt.fallback_reason,
        Some(ReadDeltaFallback::CurrentTruncated)
    );
    assert_eq!(receipt.avoided_tokens, 0);
}

#[tokio::test]
async fn read_delta_falls_back_when_the_diff_is_not_smaller() {
    let (root, services) = indexed_source("small.txt", b"alpha\n").await;
    let first = services
        .read(ReadRequest {
            path: "small.txt".into(),
            start_line: Some(1),
            end_line: Some(1),
            symbol: None,
            heading: None,
            heading_occurrence: None,
            continuation_cursor: None,
            max_tokens: Some(100),
            expected_hash: None,
            delta: true,
            receipt_id: None,
        })
        .await
        .expect("capture small base");
    std::fs::write(root.path().join("small.txt"), "beta\n").expect("edit small source");

    let changed = services
        .read(ReadRequest {
            path: "small.txt".into(),
            start_line: Some(1),
            end_line: Some(1),
            symbol: None,
            heading: None,
            heading_occurrence: None,
            continuation_cursor: None,
            max_tokens: Some(100),
            expected_hash: Some(first.content_hash),
            delta: true,
            receipt_id: None,
        })
        .await
        .expect("fall back to full content");

    assert_eq!(changed.status, ReadStatus::Content);
    assert_eq!(changed.content.as_deref(), Some("beta\n"));
    assert!(changed.delta.is_none());
    let receipt = changed.delta_receipt.expect("fallback receipt");
    assert_eq!(receipt.outcome, ReadDeltaOutcome::Full);
    assert_eq!(
        receipt.fallback_reason,
        Some(ReadDeltaFallback::DeltaNotSmaller)
    );
    assert_eq!(receipt.avoided_tokens, 0);
}

#[tokio::test]
async fn read_delta_falls_back_when_symbol_coordinates_change() {
    let source = b"fn target() {\n    old_behavior();\n}\n";
    let (root, services) = indexed_source("symbol.rs", source).await;
    let first = services
        .read(ReadRequest {
            path: "symbol.rs".into(),
            start_line: None,
            end_line: None,
            symbol: Some("target".into()),
            heading: None,
            heading_occurrence: None,
            continuation_cursor: None,
            max_tokens: Some(1_000),
            expected_hash: None,
            delta: true,
            receipt_id: None,
        })
        .await
        .expect("capture symbol base");
    std::fs::write(
        root.path().join("symbol.rs"),
        "\nfn target() {\n    new_behavior();\n}\n",
    )
    .expect("move and edit symbol");
    services.index(false).await.expect("reindex moved symbol");

    let changed = services
        .read(ReadRequest {
            path: "symbol.rs".into(),
            start_line: None,
            end_line: None,
            symbol: Some("target".into()),
            heading: None,
            heading_occurrence: None,
            continuation_cursor: None,
            max_tokens: Some(1_000),
            expected_hash: Some(first.content_hash),
            delta: true,
            receipt_id: None,
        })
        .await
        .expect("fall back after target movement");

    assert_eq!(changed.status, ReadStatus::Content);
    assert!(changed.content.as_deref().is_some_and(|content| {
        content.contains("new_behavior") && !content.contains("old_behavior")
    }));
    let receipt = changed.delta_receipt.expect("coordinate fallback");
    assert_eq!(
        receipt.fallback_reason,
        Some(ReadDeltaFallback::TargetChanged)
    );
    assert!(
        receipt
            .base_generation
            .is_some_and(|base| base < receipt.head_generation)
    );
}

#[tokio::test]
async fn read_receipt_does_not_suppress_changed_overlapping_content() {
    let (root, services) = indexed_source("receipt.rs", b"fn before() {}\n").await;
    let first = services
        .read(ReadRequest {
            path: "receipt.rs".into(),
            start_line: Some(1),
            end_line: Some(1),
            symbol: None,
            heading: None,
            heading_occurrence: None,
            continuation_cursor: None,
            max_tokens: Some(100),
            expected_hash: None,
            delta: false,
            receipt_id: None,
        })
        .await
        .expect("first receipt read");
    std::fs::write(root.path().join("receipt.rs"), "fn after() {}\n").expect("edit receipt");

    let changed = services
        .read(ReadRequest {
            path: "receipt.rs".into(),
            start_line: Some(1),
            end_line: Some(1),
            symbol: None,
            heading: None,
            heading_occurrence: None,
            continuation_cursor: None,
            max_tokens: Some(100),
            expected_hash: None,
            delta: false,
            receipt_id: first.meta.receipt_id,
        })
        .await
        .expect("changed overlapping read");

    assert_eq!(changed.status, ReadStatus::Content);
    assert_eq!(changed.content.as_deref(), Some("fn after() {}\n"));
    assert_eq!(changed.meta.receipt_suppressed_overlap, 0);
}

#[tokio::test]
async fn read_receipt_distinguishes_exact_suppression_from_not_modified() {
    let (_root, services) = indexed_source("receipt.rs", b"fn unchanged() {}\n").await;
    let first = services
        .read(ReadRequest {
            path: "receipt.rs".into(),
            start_line: Some(1),
            end_line: Some(1),
            symbol: None,
            heading: None,
            heading_occurrence: None,
            continuation_cursor: None,
            max_tokens: Some(100),
            expected_hash: None,
            delta: false,
            receipt_id: None,
        })
        .await
        .expect("first receipt read");
    let repeated = services
        .read(ReadRequest {
            path: "receipt.rs".into(),
            start_line: Some(1),
            end_line: Some(1),
            symbol: None,
            heading: None,
            heading_occurrence: None,
            continuation_cursor: None,
            max_tokens: Some(100),
            expected_hash: None,
            delta: false,
            receipt_id: first.meta.receipt_id,
        })
        .await
        .expect("receipt-suppressed read");

    assert_eq!(repeated.status, ReadStatus::ReceiptSuppressed);
    assert!(!repeated.not_modified);
    assert!(repeated.content.is_none());
    assert_eq!(repeated.meta.receipt_suppressed_exact, 1);
    assert_eq!(repeated.meta.emitted_tokens, 0);
    let report = services
        .token_savings_report()
        .await
        .expect("receipt accounting");
    assert_eq!(report.response_accounting.receipt_suppressed_exact, 1);
    let reads = report
        .response_accounting
        .by_operation
        .iter()
        .find(|row| row.operation == TokenAccountingOperation::Read)
        .expect("read accounting");
    assert_eq!(reads.receipt_suppressed_exact, 1);
}

#[tokio::test]
async fn exact_and_open_reads_preserve_coordinates_hashes_and_live_content() {
    let source = b"one\ntwo\nthree\nfour\nfive\n";
    let (root, services) = indexed_source("lines.txt", source).await;

    let exact = services
        .read(ReadRequest {
            path: "lines.txt".into(),
            start_line: Some(2),
            end_line: Some(3),
            symbol: None,
            heading: None,
            heading_occurrence: None,
            continuation_cursor: None,
            max_tokens: Some(100),
            expected_hash: None,
            delta: false,
            receipt_id: None,
        })
        .await
        .expect("exact range");
    assert_eq!((exact.start_line, exact.end_line), (2, 3));
    assert_eq!(exact.content.as_deref(), Some("two\nthree\n"));

    let unchanged = services
        .read(ReadRequest {
            path: "lines.txt".into(),
            start_line: Some(2),
            end_line: Some(3),
            symbol: None,
            heading: None,
            heading_occurrence: None,
            continuation_cursor: None,
            max_tokens: Some(100),
            expected_hash: Some(exact.content_hash.clone()),
            delta: false,
            receipt_id: None,
        })
        .await
        .expect("conditional exact range");
    assert_eq!(unchanged.status, ReadStatus::NotModified);
    assert!(unchanged.content.is_none());
    assert_eq!(unchanged.meta.emitted_tokens, 0);

    let from_second = services
        .read(ReadRequest {
            path: "lines.txt".into(),
            start_line: Some(2),
            end_line: None,
            symbol: None,
            heading: None,
            heading_occurrence: None,
            continuation_cursor: None,
            max_tokens: Some(100),
            expected_hash: None,
            delta: false,
            receipt_id: None,
        })
        .await
        .expect("open-ended range");
    assert_eq!((from_second.start_line, from_second.end_line), (2, 5));
    assert_eq!(from_second.content.as_deref(), Some("two\nthree\nfour\nfive\n"));

    let through_third = services
        .read(ReadRequest {
            path: "lines.txt".into(),
            start_line: None,
            end_line: Some(3),
            symbol: None,
            heading: None,
            heading_occurrence: None,
            continuation_cursor: None,
            max_tokens: Some(100),
            expected_hash: None,
            delta: false,
            receipt_id: None,
        })
        .await
        .expect("open-start range");
    assert_eq!((through_third.start_line, through_third.end_line), (1, 3));
    assert_eq!(through_third.content.as_deref(), Some("one\ntwo\nthree\n"));

    let whole = services
        .read(ReadRequest {
            path: "lines.txt".into(),
            start_line: None,
            end_line: None,
            symbol: None,
            heading: None,
            heading_occurrence: None,
            continuation_cursor: None,
            max_tokens: Some(100),
            expected_hash: None,
            delta: false,
            receipt_id: None,
        })
        .await
        .expect("whole file");
    let exact_whole = services
        .read(ReadRequest {
            path: "lines.txt".into(),
            start_line: Some(1),
            end_line: Some(5),
            symbol: None,
            heading: None,
            heading_occurrence: None,
            continuation_cursor: None,
            max_tokens: Some(100),
            expected_hash: None,
            delta: false,
            receipt_id: None,
        })
        .await
        .expect("exact whole file");
    assert_eq!(whole.content.as_deref(), Some("one\ntwo\nthree\nfour\nfive\n"));
    assert_eq!(exact_whole.content, whole.content);
    assert_eq!(exact_whole.content_hash, whole.content_hash);

    let through_eof = services
        .read(ReadRequest {
            path: "lines.txt".into(),
            start_line: Some(4),
            end_line: Some(99),
            symbol: None,
            heading: None,
            heading_occurrence: None,
            continuation_cursor: None,
            max_tokens: Some(100),
            expected_hash: None,
            delta: false,
            receipt_id: None,
        })
        .await
        .expect("range through EOF");
    assert_eq!((through_eof.start_line, through_eof.end_line), (4, 5));
    assert_eq!(through_eof.content.as_deref(), Some("four\nfive\n"));

    std::fs::write(
        root.path().join("lines.txt"),
        b"one\nchanged\nthree\nfour\nfive\n",
    )
    .expect("edit source");
    let changed = services
        .read(ReadRequest {
            path: "lines.txt".into(),
            start_line: Some(2),
            end_line: Some(3),
            symbol: None,
            heading: None,
            heading_occurrence: None,
            continuation_cursor: None,
            max_tokens: Some(100),
            expected_hash: Some(exact.content_hash.clone()),
            delta: false,
            receipt_id: None,
        })
        .await
        .expect("changed exact range");
    assert_eq!(changed.status, ReadStatus::Content);
    assert!(changed.index_stale);
    assert_ne!(changed.content_hash, exact.content_hash);
    assert_eq!(changed.content.as_deref(), Some("changed\nthree\n"));
}

#[tokio::test]
async fn symbol_read_after_first_line_returns_the_complete_definition() {
    let source = b"const PREFIX: usize = 1;\n\nfn target() -> usize {\n    let value = PREFIX + 1;\n    value\n}\n\nfn after() {}\n";
    let (_root, services) = indexed_source("symbol.rs", source).await;

    let response = services
        .read(ReadRequest {
            path: "symbol.rs".into(),
            start_line: None,
            end_line: None,
            symbol: Some("target".into()),
            heading: None,
            heading_occurrence: None,
            continuation_cursor: None,
            max_tokens: Some(100),
            expected_hash: None,
            delta: false,
            receipt_id: None,
        })
        .await
        .expect("symbol range");

    assert_eq!((response.start_line, response.end_line), (3, 6));
    assert_eq!(
        response.content.as_deref(),
        Some("fn target() -> usize {\n    let value = PREFIX + 1;\n    value\n}\n")
    );
}

#[tokio::test]
async fn open_ended_read_bounds_live_suffix_before_returning_content() {
    // Stay above the live-read token-check window while keeping this focused
    // regression test cheap enough for the normal product loop.
    let source = (0..10_000)
        .map(|line| format!("fn generated_{line}() {{}}\n"))
        .collect::<String>();
    let (_root, services) = indexed_source("large.rs", source.as_bytes()).await;

    let response = services
        .read(ReadRequest {
            path: "large.rs".into(),
            start_line: Some(5_000),
            end_line: None,
            symbol: None,
            heading: None,
            heading_occurrence: None,
            continuation_cursor: None,
            max_tokens: Some(12),
            expected_hash: None,
            delta: false,
            receipt_id: None,
        })
        .await
        .expect("bounded open-ended read");

    let content = response.content.as_deref().expect("content");
    assert!(content.len() <= 12 * 32);
    assert!(content.contains("generated_5000"));
    assert!(response.start_line >= 5_000);
    assert!(response.meta.emitted_tokens <= 12);
}

#[tokio::test]
async fn live_read_rejects_malformed_utf8_at_eof() {
    let (root, services) = indexed_source("malformed.rs", b"fn valid() {}\n").await;
    std::fs::write(root.path().join("malformed.rs"), b"a\xC3").expect("malformed edit");

    let error = services
        .read(ReadRequest {
            path: "malformed.rs".into(),
            start_line: Some(1),
            end_line: None,
            symbol: None,
            heading: None,
            heading_occurrence: None,
            continuation_cursor: None,
            max_tokens: Some(100),
            expected_hash: None,
            delta: false,
            receipt_id: None,
        })
        .await
        .expect_err("malformed UTF-8 must fail");
    assert!(matches!(
        error,
        Error::InvalidInput {
            field: "path",
            reason: "must identify UTF-8 text"
        }
    ));
}

#[tokio::test]
async fn live_read_rejects_line_after_terminal_newline() {
    let (root, services) = indexed_source("short.rs", b"a\n").await;
    std::fs::write(root.path().join("short.rs"), b"a\n").expect("short edit");

    let error = services
        .read(ReadRequest {
            path: "short.rs".into(),
            start_line: Some(2),
            end_line: None,
            symbol: None,
            heading: None,
            heading_occurrence: None,
            continuation_cursor: None,
            max_tokens: Some(100),
            expected_hash: None,
            delta: false,
            receipt_id: None,
        })
        .await
        .expect_err("line after terminal newline must fail");
    assert!(matches!(
        error,
        Error::InvalidInput {
            field: "line range",
            reason: "must be ordered and within the requested file"
        }
    ));
}

#[tokio::test]
async fn bounded_reads_preserve_crlf_and_missing_final_newline() {
    let source = b"alpha\r\nbeta\r\ngamma";
    let (_root, services) = indexed_source("endings.txt", source).await;

    let exact = services
        .read(ReadRequest {
            path: "endings.txt".into(),
            start_line: Some(2),
            end_line: Some(3),
            symbol: None,
            heading: None,
            heading_occurrence: None,
            continuation_cursor: None,
            max_tokens: Some(100),
            expected_hash: None,
            delta: false,
            receipt_id: None,
        })
        .await
        .expect("exact CRLF range");
    let open = services
        .read(ReadRequest {
            path: "endings.txt".into(),
            start_line: Some(2),
            end_line: None,
            symbol: None,
            heading: None,
            heading_occurrence: None,
            continuation_cursor: None,
            max_tokens: Some(100),
            expected_hash: None,
            delta: false,
            receipt_id: None,
        })
        .await
        .expect("open CRLF range");

    assert_eq!((exact.start_line, exact.end_line), (2, 3));
    assert_eq!(exact.content.as_deref(), Some("beta\r\ngamma"));
    assert_eq!(exact.content, open.content);
    assert_eq!(exact.content_hash, open.content_hash);

    let final_line = services
        .read(ReadRequest {
            path: "endings.txt".into(),
            start_line: Some(3),
            end_line: Some(3),
            symbol: None,
            heading: None,
            heading_occurrence: None,
            continuation_cursor: None,
            max_tokens: Some(100),
            expected_hash: None,
            delta: false,
            receipt_id: None,
        })
        .await
        .expect("final line");
    assert_eq!(final_line.content.as_deref(), Some("gamma"));
}

#[tokio::test]
async fn read_validates_ranges_and_preserves_empty_file_metadata() {
    let (_root, services) = indexed_source("empty.txt", b"").await;

    let empty = services
        .read(ReadRequest {
            path: "empty.txt".into(),
            start_line: None,
            end_line: None,
            symbol: None,
            heading: None,
            heading_occurrence: None,
            continuation_cursor: None,
            max_tokens: Some(100),
            expected_hash: None,
            delta: false,
            receipt_id: None,
        })
        .await
        .expect("empty file");
    assert_eq!((empty.start_line, empty.end_line), (1, 1));
    assert_eq!(empty.content.as_deref(), Some(""));

    for (start_line, end_line) in [(Some(0), Some(1)), (Some(3), Some(2)), (Some(2), Some(2))] {
        let error = services
            .read(ReadRequest {
                path: "empty.txt".into(),
                start_line,
                end_line,
                symbol: None,
                heading: None,
                heading_occurrence: None,
                continuation_cursor: None,
                max_tokens: Some(100),
                expected_hash: None,
                delta: false,
                receipt_id: None,
            })
            .await
            .expect_err("invalid range");
        assert!(matches!(error, Error::InvalidInput { field: "line range", .. }));
    }

    let malformed = services
        .read(ReadRequest {
            path: "empty.txt".into(),
            start_line: None,
            end_line: None,
            symbol: None,
            heading: None,
            heading_occurrence: None,
            continuation_cursor: Some("not-a-read-cursor".into()),
            max_tokens: Some(100),
            expected_hash: None,
            delta: false,
            receipt_id: None,
        })
        .await
        .expect_err("malformed cursor");
    assert!(matches!(malformed, Error::StaleCursor));

    let conflicting = services
        .read(ReadRequest {
            path: "empty.txt".into(),
            start_line: Some(1),
            end_line: Some(1),
            symbol: None,
            heading: None,
            heading_occurrence: None,
            continuation_cursor: Some(
                "1:read:1:1:1:1:00000000000000000000000000000000:0000000000000000".into(),
            ),
            max_tokens: Some(100),
            expected_hash: None,
            delta: false,
            receipt_id: None,
        })
        .await
        .expect_err("cursor and target conflict");
    assert!(matches!(
        conflicting,
        Error::InvalidInput {
            field: "read target",
            ..
        }
    ));
}

#[tokio::test]
async fn token_truncated_read_reports_the_returned_line_range() {
    let source = b"header\nalpha beta gamma delta\nsecond retained line\nthird retained line\n";
    let (_root, services) = indexed_source("tokens.txt", source).await;

    let response = services
        .read(ReadRequest {
            path: "tokens.txt".into(),
            start_line: Some(2),
            end_line: Some(4),
            symbol: None,
            heading: None,
            heading_occurrence: None,
            continuation_cursor: None,
            max_tokens: Some(3),
            expected_hash: None,
            delta: false,
            receipt_id: None,
        })
        .await
        .expect("token-truncated range");
    let content = response.content.as_deref().expect("content");
    let returned_lines = content.lines().count().max(usize::from(!content.is_empty()));

    assert!(!content.is_empty());
    assert_eq!(response.status, ReadStatus::Truncated);
    assert!(response.truncated);
    assert_eq!((response.target_start_line, response.target_end_line), (2, 4));
    assert_eq!(response.returned_start_line, response.start_line);
    assert_eq!(response.returned_end_line, response.end_line);
    assert!(response.next_start_line.is_some());
    assert!(response.continuation_cursor.is_some());
    assert_eq!(response.start_line, 2);
    assert_eq!(response.end_line, response.start_line + returned_lines - 1);
    assert!(response.end_line <= 4);
    assert!(response.meta.emitted_tokens <= 3);
}

#[tokio::test]
async fn truncated_symbol_cursor_reconstructs_partial_lines_and_rejects_live_changes() {
    let long_line = format!("    let payload = \"{}\";\n", "multibyte-\u{754c}".repeat(80));
    let source = format!("fn oversized_symbol() {{\n{long_line}    consume(payload);\n}}\n");
    let (root, services) = indexed_source("large.rs", source.as_bytes()).await;

    let mut cursor = None;
    let mut reconstructed = String::new();
    let mut pages = 0usize;
    loop {
        let response = services
            .read(ReadRequest {
                path: "large.rs".into(),
                start_line: None,
                end_line: None,
                symbol: cursor.is_none().then(|| "oversized_symbol".into()),
                heading: None,
                heading_occurrence: None,
                continuation_cursor: cursor.take(),
                max_tokens: Some(12),
                expected_hash: None,
                delta: false,
                receipt_id: None,
            })
            .await
            .expect("read symbol page");
        pages += 1;
        assert_eq!(response.target_start_line, 1);
        assert_eq!(response.target_end_line, 4);
        assert_eq!(response.returned_start_line, response.start_line);
        assert_eq!(response.returned_end_line, response.end_line);
        reconstructed.push_str(response.content.as_deref().expect("page content"));

        if response.truncated {
            assert_eq!(response.status, ReadStatus::Truncated);
            assert!(response.next_start_line.is_some());
            cursor = response.continuation_cursor;
            assert!(cursor.is_some());
        } else {
            assert_eq!(response.status, ReadStatus::Content);
            assert!(response.next_start_line.is_none());
            assert!(response.continuation_cursor.is_none());
            break;
        }
        assert!(pages < 100, "continuation cursor must make progress");
    }

    assert!(pages > 2, "fixture must exercise multiple truncated pages");
    assert_eq!(reconstructed, source);

    let first = services
        .read(ReadRequest {
            path: "large.rs".into(),
            start_line: None,
            end_line: None,
            symbol: Some("oversized_symbol".into()),
            heading: None,
            heading_occurrence: None,
            continuation_cursor: None,
            max_tokens: Some(12),
            expected_hash: None,
            delta: false,
            receipt_id: None,
        })
        .await
        .expect("first page");
    let unchanged = services
        .read(ReadRequest {
            path: "large.rs".into(),
            start_line: None,
            end_line: None,
            symbol: Some("oversized_symbol".into()),
            heading: None,
            heading_occurrence: None,
            continuation_cursor: None,
            max_tokens: Some(12),
            expected_hash: Some(first.content_hash.clone()),
            delta: false,
            receipt_id: None,
        })
        .await
        .expect("conditional first page");
    assert_eq!(unchanged.status, ReadStatus::Truncated);
    assert!(unchanged.truncated);
    assert!(unchanged.not_modified);
    assert!(unchanged.content.is_none());
    assert_eq!(unchanged.continuation_cursor, first.continuation_cursor);

    std::fs::write(root.path().join("other.rs"), "fn other() {}\n").expect("write unrelated file");
    services.index(false).await.expect("advance generation");
    let stale_generation = services
        .read(ReadRequest {
            path: "large.rs".into(),
            start_line: None,
            end_line: None,
            symbol: None,
            heading: None,
            heading_occurrence: None,
            continuation_cursor: first.continuation_cursor,
            max_tokens: Some(12),
            expected_hash: None,
            delta: false,
            receipt_id: None,
        })
        .await
        .expect_err("cursor must not cross index generations");
    assert!(matches!(stale_generation, Error::StaleCursor));

    let current = services
        .read(ReadRequest {
            path: "large.rs".into(),
            start_line: None,
            end_line: None,
            symbol: Some("oversized_symbol".into()),
            heading: None,
            heading_occurrence: None,
            continuation_cursor: None,
            max_tokens: Some(12),
            expected_hash: None,
            delta: false,
            receipt_id: None,
        })
        .await
        .expect("current first page");
    std::fs::write(root.path().join("large.rs"), source.replace("consume", "changed"))
        .expect("change live file");
    let error = services
        .read(ReadRequest {
            path: "large.rs".into(),
            start_line: None,
            end_line: None,
            symbol: None,
            heading: None,
            heading_occurrence: None,
            continuation_cursor: current.continuation_cursor,
            max_tokens: Some(12),
            expected_hash: None,
            delta: false,
            receipt_id: None,
        })
        .await
        .expect_err("cursor must not cross live file versions");
    assert!(matches!(error, Error::StaleCursor));
}

#[tokio::test]
async fn read_rejects_ignored_files() {
    let root = tempfile::tempdir().expect("temporary repository");
    std::fs::create_dir(root.path().join(".git")).expect("git marker");
    std::fs::write(root.path().join(".gitignore"), ".env\n").expect("ignore file");
    std::fs::write(root.path().join(".env"), "SECRET=do-not-return\n").expect("ignored file");
    std::fs::write(root.path().join("lib.rs"), "fn visible() {}\n").expect("indexed file");
    let services = Services::open(
        Config::discover(root.path(), Some(root.path().join("index.sqlite"))).expect("config"),
    )
    .expect("services");
    services.index(false).await.expect("index");

    let error = services
        .read(ReadRequest {
            path: ".env".into(),
            start_line: None,
            end_line: None,
            symbol: None,
            heading: None,
            heading_occurrence: None,
            continuation_cursor: None,
            max_tokens: Some(100),
            expected_hash: None,
            delta: false,
            receipt_id: None,
        })
        .await
        .expect_err("ignored file must not be readable");

    assert!(matches!(error, Error::NotIndexed(path) if path == ".env"));
}

#[tokio::test]
async fn qualified_symbol_read_uses_outline_parent_and_missing_symbol_is_typed() {
    let source = b"class Other:\n    def run(self):\n        return 0\n\nclass Service:\n    def run(self):\n        return 1\n";
    let (_root, services) = indexed_source("service.py", source).await;

    let outline = services
        .outline(OutlineRequest {
            paths: vec!["service.py".into()],
            symbol_name: Some("run".into()),
            symbol_kind: Some("function".into()),
            max_results: Some(10),
            max_tokens: Some(100),
            receipt_id: None,
            cursor: None,
        })
        .await
        .expect("outline method");
    let method = outline.files[0]
        .symbols
        .iter()
        .find(|symbol| symbol.parent.as_deref() == Some("Service"))
        .expect("Service.run outline");
    assert_eq!(method.name, "run");
    assert_eq!(method.parent.as_deref(), Some("Service"));

    let response = services
        .read(ReadRequest {
            path: "service.py".into(),
            start_line: None,
            end_line: None,
            symbol: Some("Service.run".into()),
            heading: None,
            heading_occurrence: None,
            continuation_cursor: None,
            max_tokens: Some(100),
            expected_hash: None,
            delta: false,
            receipt_id: None,
        })
        .await
        .expect("qualified symbol");
    assert_eq!((response.start_line, response.end_line), (6, 7));
    assert!(
        response
            .content
            .as_deref()
            .is_some_and(|content| content.contains("return 1") && !content.contains("return 0"))
    );

    let error = services
        .read(ReadRequest {
            path: "service.py".into(),
            start_line: None,
            end_line: None,
            symbol: Some("Service.missing".into()),
            heading: None,
            heading_occurrence: None,
            continuation_cursor: None,
            max_tokens: Some(100),
            expected_hash: None,
            delta: false,
            receipt_id: None,
        })
        .await
        .expect_err("missing qualified symbol");
    assert!(matches!(
        error,
        Error::SymbolNotFound { path, symbol }
            if path == "service.py" && symbol == "Service.missing"
    ));
}

#[tokio::test]
async fn symbol_reads_and_outline_filters_search_beyond_result_caps() {
    let root = tempfile::tempdir().expect("temporary repository");
    let source = (0..130)
        .map(|index| format!("fn symbol_{index:03}() {{}}\n"))
        .collect::<String>();
    std::fs::write(root.path().join("many.rs"), source).expect("source");
    let services = Services::open(
        Config::discover(root.path(), Some(root.path().join("index.sqlite"))).expect("config"),
    )
    .expect("services");
    services.index(false).await.expect("index");

    let read = services
        .read(ReadRequest {
            path: "many.rs".into(),
            start_line: None,
            end_line: None,
            symbol: Some("symbol_129".into()),
            heading: None,
            heading_occurrence: None,
            continuation_cursor: None,
            max_tokens: Some(100),
            expected_hash: None,
            delta: false,
            receipt_id: None,
        })
        .await
        .expect("late symbol read");
    assert_eq!(read.start_line, 130);
    assert!(
        read.content
            .as_deref()
            .is_some_and(|text| text.contains("symbol_129"))
    );

    let outline = services
        .outline(OutlineRequest {
            paths: vec!["many.rs".into()],
            symbol_name: Some("symbol_129".into()),
            symbol_kind: Some("function".into()),
            max_results: Some(1),
            max_tokens: Some(100),
            receipt_id: None,
            cursor: None,
        })
        .await
        .expect("filtered outline");
    assert_eq!(outline.files[0].symbols.len(), 1);
    assert_eq!(outline.files[0].symbols[0].name, "symbol_129");
    assert!(outline.parse_complete);
    assert!(outline.result_complete);
    assert_eq!(outline.total_symbols, 1);
    assert_eq!(outline.returned_symbols, 1);
    assert_eq!(outline.symbol_counts_by_kind.get("function"), Some(&1));
}

#[tokio::test]
async fn outline_distinguishes_parse_completeness_from_result_completeness() {
    let root = tempfile::tempdir().expect("temporary repository");
    let constants = (0..120)
        .map(|index| format!("const VALUE_{index:03}: usize = {index};\n"))
        .collect::<String>();
    let functions = (0..20)
        .map(|index| format!("fn operation_{index:03}() {{}}\n"))
        .collect::<String>();
    std::fs::write(
        root.path().join("many.rs"),
        format!("use std::fmt; use std::io;\n{constants}{functions}"),
    )
    .expect("many symbols");
    std::fs::write(root.path().join("broken.rs"), "fn broken( {\n").expect("malformed source");
    let services = Services::open(
        Config::discover(root.path(), Some(root.path().join("index.sqlite"))).expect("config"),
    )
    .expect("services");
    services.index(false).await.expect("index");

    let first = services
        .outline(OutlineRequest {
            paths: vec!["many.rs".into()],
            symbol_name: None,
            symbol_kind: None,
            max_results: Some(100),
            max_tokens: Some(32_000),
            receipt_id: None,
            cursor: None,
        })
        .await
        .expect("first outline page");
    assert!(first.parse_complete);
    assert!(first.files[0].parse_complete);
    assert!(first.files[0].structurally_complete);
    assert!(!first.result_complete);
    assert_eq!(first.total_symbols, 140);
    assert_eq!(first.returned_symbols, 100);
    assert_eq!(first.total_imports, 2);
    assert_eq!(first.returned_imports, 0);
    assert!(first.truncated_by_max_results);
    assert!(!first.truncated_by_max_tokens);
    assert_eq!(first.symbol_counts_by_kind.get("constant"), Some(&120));
    assert_eq!(first.symbol_counts_by_kind.get("function"), Some(&20));
    let cursor = first.meta.next_cursor.clone().expect("continuation cursor");

    let changed_query = services
        .outline(OutlineRequest {
            paths: vec!["many.rs".into()],
            symbol_name: None,
            symbol_kind: Some("function".into()),
            max_results: Some(100),
            max_tokens: Some(32_000),
            receipt_id: None,
            cursor: Some(cursor.clone()),
        })
        .await
        .expect_err("cursor must remain bound to the original filters");
    assert!(matches!(changed_query, Error::StaleCursor));

    let second = services
        .outline(OutlineRequest {
            paths: vec!["many.rs".into()],
            symbol_name: None,
            symbol_kind: None,
            max_results: Some(41),
            max_tokens: Some(32_000),
            receipt_id: None,
            cursor: Some(cursor),
        })
        .await
        .expect("second outline page");
    assert!(second.parse_complete);
    assert!(!second.result_complete);
    assert_eq!(second.total_symbols, 140);
    assert_eq!(second.returned_symbols, 40);
    assert_eq!(second.returned_imports, 1);
    assert!(second.truncated_by_max_results);
    assert!(!second.truncated_by_max_tokens);
    let final_cursor = second.meta.next_cursor.clone().expect("final cursor");

    let third = services
        .outline(OutlineRequest {
            paths: vec!["many.rs".into()],
            symbol_name: None,
            symbol_kind: None,
            max_results: Some(100),
            max_tokens: Some(32_000),
            receipt_id: None,
            cursor: Some(final_cursor),
        })
        .await
        .expect("third outline page");
    assert_eq!(third.returned_symbols, 0);
    assert_eq!(third.returned_imports, 1);
    assert!(!third.truncated_by_max_results);
    assert!(third.meta.next_cursor.is_none());
    let names = first.files[0]
        .symbols
        .iter()
        .chain(&second.files[0].symbols)
        .map(|symbol| symbol.name.as_str())
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(names.len(), 140);
    assert!(names.contains("VALUE_000"));
    assert!(names.contains("operation_019"));
    let imports = second.files[0]
        .imports
        .iter()
        .chain(&third.files[0].imports)
        .map(|import| import.raw_target.as_str())
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(imports, ["std::fmt", "std::io"].into_iter().collect());

    let token_limited = services
        .outline(OutlineRequest {
            paths: vec!["many.rs".into()],
            symbol_name: None,
            symbol_kind: None,
            max_results: Some(100),
            max_tokens: Some(1),
            receipt_id: None,
            cursor: None,
        })
        .await
        .expect("token-limited outline");
    assert!(token_limited.parse_complete);
    assert!(!token_limited.result_complete);
    assert!(token_limited.returned_symbols < token_limited.total_symbols);
    assert!(!token_limited.truncated_by_max_results);
    assert!(token_limited.truncated_by_max_tokens);
    assert!(token_limited.meta.next_cursor.is_none());

    let malformed = services
        .outline(OutlineRequest {
            paths: vec!["broken.rs".into()],
            symbol_name: None,
            symbol_kind: None,
            max_results: Some(100),
            max_tokens: Some(1_000),
            receipt_id: None,
            cursor: None,
        })
        .await
        .expect("malformed outline");
    assert!(!malformed.parse_complete);
    assert!(!malformed.files[0].parse_complete);
    assert!(!malformed.files[0].structurally_complete);
    assert!(malformed.result_complete);
}

#[tokio::test]
async fn fixture_outlines_deduplicate_methods_and_report_receiver_owners() {
    let root = tempfile::tempdir().expect("temporary repository");
    for (path, source) in [
        (
            "src/rust/math.rs",
            include_str!("../fixtures/sample_repo/src/rust/math.rs"),
        ),
        (
            "src/go/point.go",
            include_str!("../fixtures/sample_repo/src/go/point.go"),
        ),
    ] {
        let absolute = root.path().join(path);
        std::fs::create_dir_all(absolute.parent().expect("fixture parent"))
            .expect("create fixture parent");
        std::fs::write(absolute, source).expect("write fixture source");
    }
    let services = Services::open(
        Config::discover(root.path(), Some(root.path().join("index.sqlite"))).expect("config"),
    )
    .expect("services");
    services.index(false).await.expect("index fixtures");

    let outline = services
        .outline(OutlineRequest {
            paths: vec!["src/rust/math.rs".into(), "src/go/point.go".into()],
            symbol_name: None,
            symbol_kind: None,
            max_results: Some(100),
            max_tokens: Some(2_000),
            receipt_id: None,
            cursor: None,
        })
        .await
        .expect("fixture outline");
    let symbols = outline
        .files
        .iter()
        .flat_map(|file| file.symbols.iter())
        .collect::<Vec<_>>();

    for (name, parent) in [("distance", "Point"), ("Distance", "Point")] {
        let matching = symbols
            .iter()
            .filter(|symbol| symbol.name == name)
            .collect::<Vec<_>>();
        assert_eq!(matching.len(), 1, "symbols for {name}: {matching:?}");
        assert_eq!(matching[0].kind, "method");
        assert_eq!(matching[0].parent.as_deref(), Some(parent));
    }

    let status = services.status().await.expect("status");
    assert_eq!(status.symbol_count, symbols.len());
    assert_eq!(status.symbol_count, 6);
}

#[tokio::test]
async fn oversized_query_is_rejected_without_stopping_services() {
    let (_root, services) = fixture().await;
    let oversized = "x".repeat(64 * 1024 + 1);
    let error = services
        .search(SearchRequest {
            query: oversized,
            mode: SearchMode::Text,
            include_paths: Vec::new(),
            exclude_paths: Vec::new(),
            focus_paths: Vec::new(),
            max_results: None,
            max_tokens: None,
            context_lines: None,
            case_sensitive: false,
            all_occurrences: false,
            prefer_structural: false,
            receipt_id: None,
            cursor: None,
        })
        .await
        .expect_err("oversized query must fail");
    assert!(error.to_string().contains("exceeds"));

    let status = services.status().await.expect("service remains live");
    assert_eq!(status.file_count, 1);
}

#[tokio::test]
async fn cancelled_blocking_queries_stop_cooperatively_without_poisoning_services() {
    let (_root, services) = fixture().await;
    let cancellation = CancellationToken::new();
    cancellation.cancel();

    let search = services
        .search_cancellable(
            SearchRequest {
                query: "greet".into(),
                mode: SearchMode::Regex,
                include_paths: Vec::new(),
                exclude_paths: Vec::new(),
                focus_paths: Vec::new(),
                max_results: Some(10),
                max_tokens: Some(100),
                context_lines: Some(2),
                case_sensitive: false,
                all_occurrences: false,
                prefer_structural: false,
                receipt_id: None,
                cursor: None,
            },
            cancellation.child_token(),
        )
        .await
        .expect_err("cancelled search");
    assert!(matches!(search, Error::Cancelled));

    let context = services
        .context_cancellable(
            ContextRequest {
                task: "change greet".into(),
                token_budget: 100,
                include_paths: Vec::new(),
                must_include_paths: Vec::new(),
                must_include_symbols: Vec::new(),
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
            verbose_diagnostics: false,
            },
            cancellation,
        )
        .await
        .expect_err("cancelled context");
    assert!(matches!(context, Error::Cancelled));
    assert_eq!(services.status().await.expect("status").file_count, 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_queries_observe_one_committed_generation_during_reconciliation() {
    let (root, services) = fixture().await;
    let services = std::sync::Arc::new(services);
    let before = services.status().await.expect("before status").repository_generation;
    std::fs::write(
        root.path().join("src/lib.rs"),
        "pub fn replacement() -> u8 { 42 }\n",
    )
    .expect("replace source");

    let indexing_services = std::sync::Arc::clone(&services);
    let indexing = tokio::spawn(async move {
        indexing_services
            .index_paths(vec!["src/lib.rs".into()])
            .await
            .expect("reconcile")
    });
    let mut queries = tokio::task::JoinSet::new();
    // Stay within the documented process-local active retrieval bound. Exact
    // overload behavior is covered separately; this test isolates generation
    // consistency while reconciliation publishes a replacement snapshot.
    for index in 0..16 {
        let services = std::sync::Arc::clone(&services);
        queries.spawn(async move {
            let query = if index % 2 == 0 { "greet" } else { "replacement" };
            let response = services
                .search(SearchRequest {
                    query: query.into(),
                    mode: SearchMode::Identifier,
                    include_paths: Vec::new(),
                    exclude_paths: Vec::new(),
                    focus_paths: Vec::new(),
                    max_results: Some(10),
                    max_tokens: Some(100),
                    context_lines: Some(1),
                    case_sensitive: false,
                    all_occurrences: false,
                    prefer_structural: false,
                    receipt_id: None,
                    cursor: None,
                })
                .await
                .expect("concurrent search");
            (query, response)
        });
    }

    let after = indexing.await.expect("index task").repository_generation;
    assert!(after > before);
    while let Some(result) = queries.join_next().await {
        let (query, response) = result.expect("query task");
        assert!(matches!(response.meta.repository_generation, value if value == before || value == after));
        if response.meta.repository_generation == before {
            assert_eq!(response.hits.is_empty(), query == "replacement");
        } else {
            assert_eq!(response.hits.is_empty(), query == "greet");
        }
    }
}

#[tokio::test]
async fn managed_corrupt_index_is_deleted_and_rebuilt() {
    let root = tempfile::tempdir().expect("root");
    std::fs::write(root.path().join("lib.rs"), "fn recovered() {}\n").expect("source");
    let config = Config::discover(root.path(), None).expect("config");
    let database = config.database_path.clone();
    let database_parent = database.parent().expect("database parent").to_owned();
    std::fs::create_dir_all(&database_parent).expect("parent");
    std::fs::write(&database, b"not a sqlite database").expect("corrupt database");

    let services = Services::open(config).expect("recover managed cache");
    services.index(false).await.expect("rebuild index");
    assert_eq!(services.status().await.expect("status").file_count, 1);
    assert!(
        std::fs::metadata(&database)
            .expect("rebuilt database")
            .len()
            > 32
    );
    drop(services);
    std::fs::remove_dir_all(database_parent).expect("remove managed cache fixture");
}

#[test]
fn explicit_corrupt_database_is_not_deleted() {
    let root = tempfile::tempdir().expect("root");
    let database = root.path().join("explicit.sqlite");
    let original = b"caller-owned data";
    std::fs::write(&database, original).expect("database fixture");
    let config = Config::discover(root.path(), Some(database.clone())).expect("config");

    Services::open(config).expect_err("explicit corruption must be reported");
    assert_eq!(std::fs::read(database).expect("preserved database"), original);
}

#[tokio::test]
async fn empty_index_reports_status_but_retrieval_is_not_ready() {
    let root = tempfile::tempdir().expect("root");
    std::fs::write(root.path().join("lib.rs"), "fn pending() {}\n").expect("source");
    let config = Config::discover(root.path(), Some(root.path().join("index.sqlite"))).unwrap();
    let services = Services::open(config).unwrap();

    let status = services.status().await.expect("status");
    assert_eq!(status.repository_generation, 0);
    assert_eq!(status.index_state, IndexState::Uninitialized);
    assert_eq!(status.freshness, Freshness::Current);
    assert_eq!(status.file_count, 0);

    let error = services
        .files(FilesRequest {
            operation: FileOperation::Tree,
            path: None,
            query: None,
            pattern: None,
            max_results: Some(10),
            cursor: None,
            depth: Some(2),
        })
        .await
        .expect_err("retrieval must not report an empty success");
    assert!(matches!(error, leantoken::Error::IndexNotReady));
}

#[tokio::test]
async fn first_index_reports_uninitialized_while_reconciling() {
    let root = tempfile::tempdir().expect("root");
    std::fs::write(root.path().join("lib.rs"), "fn pending() {}\n").expect("source");
    let database = root.path().join("index.sqlite");
    let services = Services::open(
        Config::discover(root.path(), Some(database.clone())).expect("config"),
    )
    .expect("services");
    let coordination = IndexCoordination::for_database(&database);
    let operation = coordination
        .acquire_operation(&CancellationToken::new())
        .expect("hold reconciliation lock");
    let indexing_services = services.clone();
    let indexing = tokio::spawn(async move { indexing_services.index(false).await });
    tokio::task::yield_now().await;

    let during = services.status().await.expect("status during first index");
    assert_eq!(during.repository_generation, 0);
    assert_eq!(during.index_state, IndexState::Uninitialized);
    assert_eq!(during.freshness, Freshness::Reconciling);

    drop(operation);
    indexing.await.expect("join index").expect("complete index");
    let after = services.status().await.expect("status after first index");
    assert!(after.repository_generation > 0);
    assert_eq!(after.index_state, IndexState::Ready);
    assert_eq!(after.freshness, Freshness::Current);
}

fn git_available() -> bool {
    std::process::Command::new("git")
        .arg("--version")
        .output()
        .is_ok()
}

fn init_git_repo(root: &std::path::Path) {
    let run = |args: &[&str]| {
        std::process::Command::new("git")
            .args(args)
            .current_dir(root)
            .output()
            .expect("git command");
    };
    run(&["init"]);
    run(&["config", "user.email", "test@example.com"]);
    run(&["config", "user.name", "Test"]);
    run(&["add", "-A"]);
    run(&["commit", "-m", "init"]);
}

#[tokio::test]
async fn csharp_qualified_symbols_support_historical_reads_and_diffs() {
    if !git_available() {
        return;
    }

    let root = tempfile::tempdir().expect("root");
    std::fs::write(
        root.path().join("Worker.cs"),
        "class Worker {\n    int Run() {\n        return 1;\n    }\n}\n",
    )
    .expect("base C# source");
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
        root.path().join("Worker.cs"),
        "class Worker {\n    int Run() {\n        return 2;\n    }\n}\n",
    )
    .expect("updated C# source");
    for args in [
        vec!["add", "Worker.cs"],
        vec!["commit", "-m", "update C# method"],
    ] {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(root.path())
            .output()
            .expect("git commit command");
        assert!(output.status.success());
    }
    let head = revision("HEAD");

    let config =
        Config::discover(root.path(), Some(root.path().join("index.sqlite"))).expect("config");
    let services = Services::open(config).expect("services");
    services.index(false).await.expect("index fixture");

    let read = services
        .history(HistoryRequest {
            operation: HistoryOperation::ReadSymbol {
                path: "Worker.cs".into(),
                symbol: "Worker.Run".into(),
                revision: base.clone(),
            },
            max_results: None,
            max_tokens: Some(200),
        })
        .await
        .expect("historical C# read");
    assert!(
        read.symbol
            .as_ref()
            .and_then(|symbol| symbol.content.as_deref())
            .is_some_and(|content| content.contains("return 1"))
    );

    let diff = services
        .history(HistoryRequest {
            operation: HistoryOperation::DiffSymbol {
                path: "Worker.cs".into(),
                symbol: "Worker.Run".into(),
                base_revision: base,
                head_revision: head,
            },
            max_results: None,
            max_tokens: Some(200),
        })
        .await
        .expect("historical C# diff");
    let patch = diff.diff.as_deref().expect("C# method diff");
    assert!(patch.contains("-        return 1;"));
    assert!(patch.contains("+        return 2;"));
    let report = services
        .token_savings_report()
        .await
        .expect("history response accounting");
    let history = report
        .response_accounting
        .by_operation
        .iter()
        .find(|row| row.operation == TokenAccountingOperation::History)
        .expect("history accounting");
    assert_eq!(history.tracked_requests, 2);
    assert_eq!(history.baseline_requests, 0);
    assert_eq!(
        history.total_response_tokens,
        (read.meta.total_response_tokens + diff.meta.total_response_tokens) as u64
    );
}

#[tokio::test]
async fn symbol_history_reads_diffs_and_traces_immutable_revisions() {
    if !git_available() {
        return;
    }

    let root = tempfile::tempdir().expect("root");
    std::fs::create_dir(root.path().join("src")).expect("source directory");
    std::fs::write(
        root.path().join("src/lib.rs"),
        "pub fn tracked() -> u32 { 1 }\n\npub fn unrelated() -> u32 { 10 }\n",
    )
    .expect("base source");
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
    let commit = |message: &str| {
        for args in [
            vec!["add", "-A"],
            vec!["commit", "-m", message],
        ] {
            let output = std::process::Command::new("git")
                .args(args)
                .current_dir(root.path())
                .output()
                .expect("git commit command");
            assert!(output.status.success());
        }
    };
    let base = revision("HEAD");
    std::fs::write(
        root.path().join("src/lib.rs"),
        "pub fn tracked() -> u32 { 2 }\n\npub fn unrelated() -> u32 { 10 }\n",
    )
    .expect("updated symbol");
    commit("update tracked");
    let changed = revision("HEAD");
    std::fs::write(
        root.path().join("src/other.rs"),
        "pub fn later_change() -> u32 { 11 }\n",
    )
    .expect("unrelated update");
    commit("update unrelated");

    let config =
        Config::discover(root.path(), Some(root.path().join("index.sqlite"))).expect("config");
    let services = Services::open(config).expect("services");
    services.index(false).await.expect("index fixture");

    let read = services
        .history(HistoryRequest {
            operation: HistoryOperation::ReadSymbol {
                path: "src/lib.rs".into(),
                symbol: "tracked".into(),
                revision: base.clone(),
            },
            max_results: None,
            max_tokens: Some(100),
        })
        .await
        .expect("historical read");
    let historical = read.symbol.as_ref().expect("historical symbol");
    assert_eq!(
        historical.content.as_deref(),
        Some("pub fn tracked() -> u32 { 1 }")
    );
    let historical_hash = historical.content_hash.clone();
    assert!(!historical.truncated);
    assert_response_token_accounting!(read, Tokenizer::default());
    let bounded_limit = read.meta.total_response_tokens.saturating_sub(10);
    let bounded_read = services
        .history_with_options(
            HistoryRequest {
                operation: HistoryOperation::ReadSymbol {
                    path: "src/lib.rs".into(),
                    symbol: "tracked".into(),
                    revision: base.clone(),
                },
                max_results: None,
                max_tokens: Some(100),
            },
            ServiceCallOptions::new().with_max_response_tokens(bounded_limit),
        )
        .await
        .expect("response-bounded historical read");
    assert!(bounded_read.meta.total_response_tokens <= bounded_limit);
    assert!(bounded_read.symbol.as_ref().expect("symbol").truncated);
    assert_response_token_accounting!(bounded_read, Tokenizer::default());

    let truncated = services
        .history(HistoryRequest {
            operation: HistoryOperation::ReadSymbol {
                path: "src/lib.rs".into(),
                symbol: "tracked".into(),
                revision: historical.revision.clone(),
            },
            max_results: None,
            max_tokens: Some(1),
        })
        .await
        .expect("truncated historical read");
    assert!(!truncated.result_complete);
    assert!(truncated.symbol.as_ref().expect("symbol").truncated);

    let diff = services
        .history(HistoryRequest {
            operation: HistoryOperation::DiffSymbol {
                path: "src/lib.rs".into(),
                symbol: "tracked".into(),
                base_revision: base.clone(),
                head_revision: changed.clone(),
            },
            max_results: None,
            max_tokens: Some(100),
        })
        .await
        .expect("historical diff");
    let patch = diff.diff.as_deref().expect("symbol diff");
    assert!(patch.contains("-pub fn tracked() -> u32 { 1 }"));
    assert!(patch.contains("+pub fn tracked() -> u32 { 2 }"));
    assert!(!patch.contains("\\ No newline at end of file"));
    let before = diff.before.as_ref().expect("base endpoint");
    assert_eq!(before.returned_end_line, 0);
    assert_eq!(before.content_hash, historical_hash);
    let serialized_diff = serde_json::to_value(&diff).expect("serialize symbol diff");
    assert!(
        serialized_diff
            .pointer("/before/returned_end_line")
            .is_none()
    );
    assert!(
        serialized_diff
            .pointer("/after/returned_end_line")
            .is_none()
    );
    assert!(diff.result_complete);
    assert_response_token_accounting!(diff, Tokenizer::default());

    let batch_request = DiffSymbolsRequest {
        targets: vec![
            DiffSymbolsTarget {
                path: "src/lib.rs".into(),
                symbol: "tracked".into(),
                head_path: None,
                head_symbol: None,
            },
            DiffSymbolsTarget {
                path: "src/lib.rs".into(),
                symbol: "unrelated".into(),
                head_path: None,
                head_symbol: None,
            },
        ],
        base_revision: base.clone(),
        head_revision: changed.clone(),
        max_results: Some(2),
        max_tokens: Some(1_000),
        cursor: None,
    };
    let batch = services
        .history_diff_symbols(batch_request.clone())
        .await
        .expect("batched historical diff");
    assert_eq!(batch.kind, "diff_symbols");
    assert_eq!(batch.base.revision, base[..12]);
    assert_eq!(batch.head.revision, changed[..12]);
    assert!(!batch.base.authored_at.is_empty());
    assert_eq!(batch.head.subject, "update tracked");
    assert_eq!(batch.results.len(), 2);
    assert_eq!(batch.results[0].status, DiffSymbolsStatus::Modified);
    assert_eq!(batch.results[0].before, diff.before);
    assert_eq!(batch.results[0].after, diff.after);
    assert_eq!(batch.results[0].diff, diff.diff);
    assert_eq!(batch.results[0].semantic_change, diff.semantic_change);
    assert_eq!(batch.results[1].status, DiffSymbolsStatus::Unchanged);
    assert!(batch.results[1].diff.is_none());
    assert!(batch.result_complete);
    assert_eq!(batch.diagnostics.git_subprocesses, 7);
    assert_eq!(batch.diagnostics.base_paths_requested, 1);
    assert_eq!(batch.diagnostics.head_paths_requested, 1);
    assert!(batch.diagnostics.parsed_symbols >= 4);
    assert_response_token_accounting!(batch, Tokenizer::default());

    let response_limit = batch.meta.total_response_tokens.saturating_sub(1);
    let response_limited = services
        .history_diff_symbols_with_options(
            batch_request.clone(),
            ServiceCallOptions::new().with_max_response_tokens(response_limit),
        )
        .await
        .expect("response-limited batched history");
    assert_eq!(
        response_limited.results.len(),
        2,
        "response fitting must preserve symbol status coverage before diff text"
    );
    assert_eq!(
        response_limited.results[0].incomplete_reason,
        Some(DiffSymbolsIncompleteReason::MaxResponseTokens)
    );
    assert!(response_limited.results[0].diff_truncated);
    assert_eq!(
        response_limited.results[1].status,
        DiffSymbolsStatus::Unchanged
    );
    assert_eq!(
        response_limited.diagnostics.retained_diff_bytes,
        response_limited.results[0]
            .diff
            .as_ref()
            .map_or(0, String::len)
    );
    assert!(response_limited.meta.total_response_tokens <= response_limit);
    assert_response_token_accounting!(response_limited, Tokenizer::default());

    let mut first_page_request = batch_request.clone();
    first_page_request.max_results = Some(1);
    let first_page = services
        .history_diff_symbols(first_page_request.clone())
        .await
        .expect("first batched history page");
    assert_eq!(first_page.results.len(), 1);
    assert!(!first_page.result_complete);

    let fitted_page_limit = first_page.meta.total_response_tokens;
    let fitted_page = services
        .history_diff_symbols_with_options(
            batch_request.clone(),
            ServiceCallOptions::new().with_max_response_tokens(fitted_page_limit),
        )
        .await
        .expect("response fitting should page complete status records");
    assert_eq!(fitted_page.results.len(), 1);
    assert_eq!(fitted_page.results[0].request_index, 0);
    assert!(fitted_page.meta.total_response_tokens <= fitted_page_limit);
    let mut fitted_continuation_request = batch_request.clone();
    fitted_continuation_request.cursor = fitted_page.meta.next_cursor.clone();
    let fitted_continuation = services
        .history_diff_symbols(fitted_continuation_request)
        .await
        .expect("continue response-fitted history page");
    assert_eq!(fitted_continuation.results[0].request_index, 1);
    assert!(fitted_continuation.meta.next_cursor.is_none());

    let cursor = first_page.meta.next_cursor.clone().expect("history cursor");
    first_page_request.cursor = Some(cursor);
    let second_page = services
        .history_diff_symbols(first_page_request.clone())
        .await
        .expect("second batched history page");
    assert_eq!(second_page.results.len(), 1);
    assert_eq!(second_page.results[0].request_index, 1);
    assert_eq!(second_page.results[0].status, DiffSymbolsStatus::Unchanged);
    assert!(second_page.meta.next_cursor.is_none());
    assert!(second_page.result_complete);
    first_page_request.targets.swap(0, 1);
    let stale = services
        .history_diff_symbols(first_page_request)
        .await
        .expect_err("cursor must bind ordered targets");
    assert!(matches!(stale, Error::StaleCursor));

    let mut token_limited_request = batch_request;
    token_limited_request.max_tokens = Some(1);
    let token_limited = services
        .history_diff_symbols(token_limited_request)
        .await
        .expect("token-limited batched history");
    assert!(!token_limited.result_complete);
    assert!(token_limited.results[0].diff_truncated);
    assert_eq!(
        token_limited.results[0].incomplete_reason,
        Some(DiffSymbolsIncompleteReason::MaxTokens)
    );
    assert_eq!(
        token_limited.diagnostics.retained_diff_bytes,
        token_limited
            .results
            .iter()
            .filter_map(|result| result.diff.as_ref())
            .map(String::len)
            .sum::<usize>()
    );
    assert_response_token_accounting!(token_limited, Tokenizer::default());

    let log = services
        .history(HistoryRequest {
            operation: HistoryOperation::SymbolLog {
                path: "src/lib.rs".into(),
                symbol: "tracked".into(),
                revision: None,
            },
            max_results: Some(1),
            max_tokens: None,
        })
        .await
        .expect("symbol history");
    assert!(!log.result_complete);
    assert_eq!(log.commits.len(), 1);
    assert_eq!(log.commits[0].subject, "update tracked");
    assert!(
        log.commits
            .iter()
            .all(|commit| commit.subject != "update unrelated")
    );
    assert_eq!(
        log.symbol
            .as_ref()
            .expect("symbol log endpoint")
            .returned_end_line,
        0
    );
    assert!(
        serde_json::to_value(&log)
            .expect("serialize symbol log")
            .pointer("/symbol/returned_end_line")
            .is_none()
    );
    assert_response_token_accounting!(log, Tokenizer::default());

    let context = services
        .context(ContextRequest {
            task: "review tracked".into(),
            token_budget: 500,
            include_paths: Vec::new(),
            must_include_paths: Vec::new(),
            must_include_symbols: Vec::new(),
            max_fragments: None,
            plan_only: false,
            focus_paths: Vec::new(),
            strict_focus_paths: false,
            minimum_fragments_per_focus_path: None,
            focus_symbols: vec!["tracked".into()],
            exclude_paths: Vec::new(),
            known_hashes: Vec::new(),
            receipt_id: None,
            prior_repository_generation: None,
            base_revision: Some(format!("{base}..{changed}")),
            changed_paths: Vec::new(),
            strict_changed_paths: true,
            verbose_diagnostics: false,
        })
        .await
        .expect("immutable range context");
    let scope = context.diff_scope.expect("immutable range scope");
    assert_eq!(scope.base_revision.as_deref(), Some(&base[..12]));
    assert_eq!(scope.head_revision.as_deref(), Some(&changed[..12]));
    assert_eq!(scope.changed_paths, vec!["src/lib.rs"]);
    assert!(
        context
            .fragments
            .iter()
            .all(|fragment| fragment.path == "src/lib.rs")
    );
    assert!(
        scope
            .evidence
            .expect("range evidence")
            .changed_hunks
            .iter()
            .all(|hunk| hunk.path == "src/lib.rs")
    );
}

#[tokio::test]
async fn batched_symbol_history_classifies_endpoints_renames_and_request_bounds() {
    if !git_available() {
        return;
    }

    let root = tempfile::tempdir().expect("root");
    std::fs::create_dir(root.path().join("src")).expect("source directory");
    std::fs::write(
        root.path().join("src/lib.rs"),
        "pub fn modified() -> u32 { 1 }\n\npub fn removed() -> u32 { 2 }\n\npub fn old_name() -> u32 { 3 }\n\npub fn stable() -> u32 { 4 }\n\npub struct Worker;\n\nimpl Worker {\n    pub fn same() -> u32 { 10 }\n}\n\npub struct Other;\n\nimpl Other {\n    pub fn same() -> u32 { 20 }\n}\n",
    )
    .expect("base source");
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
        "pub fn modified() -> u32 { 10 }\n\npub fn added() -> u32 { 5 }\n\npub fn stable() -> u32 { 4 }\n\npub struct Worker;\n\nimpl Worker {\n    pub fn same() -> u32 { 11 }\n}\n\npub struct Other;\n\nimpl Other {\n    pub fn same() -> u32 { 20 }\n}\n",
    )
    .expect("head source");
    std::fs::write(
        root.path().join("src/moved.rs"),
        "pub fn new_name() -> u32 { 30 }\n",
    )
    .expect("renamed source");
    for args in [
        &["add", "-A"][..],
        &["commit", "-m", "change batched symbols"][..],
    ] {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(root.path())
            .output()
            .expect("git commit command");
        assert!(output.status.success());
    }
    let head = revision("HEAD");

    let config =
        Config::discover(root.path(), Some(root.path().join("index.sqlite"))).expect("config");
    let services = Services::open(config).expect("services");
    services.index(false).await.expect("index fixture");
    let ordinary_target = |symbol: &str| DiffSymbolsTarget {
        path: "src/lib.rs".into(),
        symbol: symbol.into(),
        head_path: None,
        head_symbol: None,
    };
    let request = DiffSymbolsRequest {
        targets: vec![
            ordinary_target("modified"),
            ordinary_target("removed"),
            ordinary_target("added"),
            DiffSymbolsTarget {
                path: "src/lib.rs".into(),
                symbol: "old_name".into(),
                head_path: Some("src/moved.rs".into()),
                head_symbol: Some("new_name".into()),
            },
            ordinary_target("stable"),
            ordinary_target("never_existed"),
            ordinary_target("Worker.same"),
            ordinary_target("Other.same"),
        ],
        base_revision: base.clone(),
        head_revision: head.clone(),
        max_results: Some(8),
        max_tokens: Some(4_000),
        cursor: None,
    };
    let response = services
        .history_diff_symbols(request.clone())
        .await
        .expect("classify batched symbols");
    assert_eq!(
        response
            .results
            .iter()
            .map(|result| result.status)
            .collect::<Vec<_>>(),
        vec![
            DiffSymbolsStatus::Modified,
            DiffSymbolsStatus::Removed,
            DiffSymbolsStatus::Added,
            DiffSymbolsStatus::Renamed,
            DiffSymbolsStatus::Unchanged,
            DiffSymbolsStatus::NotFound,
            DiffSymbolsStatus::Modified,
            DiffSymbolsStatus::Unchanged,
        ]
    );
    assert_eq!(
        response.results[6]
            .before
            .as_ref()
            .and_then(|symbol| symbol.parent.as_deref()),
        Some("Worker")
    );
    assert_eq!(
        response.results[7]
            .before
            .as_ref()
            .and_then(|symbol| symbol.parent.as_deref()),
        Some("Other")
    );
    let renamed = &response.results[3];
    let semantic_rename = renamed
        .semantic_change
        .as_ref()
        .expect("semantic rename");
    assert_eq!(semantic_rename.kind, DiffSymbolChangeKind::Renamed);
    assert_eq!(
        semantic_rename
            .before
            .as_ref()
            .map(|symbol| (symbol.path.as_str(), symbol.name.as_str())),
        Some(("src/lib.rs", "old_name"))
    );
    assert_eq!(
        semantic_rename
            .after
            .as_ref()
            .map(|symbol| (symbol.path.as_str(), symbol.name.as_str())),
        Some(("src/moved.rs", "new_name"))
    );
    assert_eq!(response.base.revision, base[..12]);
    assert_eq!(response.head.revision, head[..12]);
    assert_eq!(response.head.subject, "change batched symbols");
    assert_eq!(response.diagnostics.git_subprocesses, 7);
    assert_eq!(response.diagnostics.base_paths_requested, 1);
    assert_eq!(response.diagnostics.head_paths_requested, 2);
    assert!(response.diagnostics.retained_diff_bytes <= 1024 * 1024);
    assert!(response.result_complete);
    assert_response_token_accounting!(response, Tokenizer::default());

    let too_many_targets = DiffSymbolsRequest {
        targets: (0..65)
            .map(|index| ordinary_target(&format!("symbol_{index}")))
            .collect(),
        ..request.clone()
    };
    assert!(matches!(
        services
            .history_diff_symbols(too_many_targets)
            .await
            .expect_err("target bound"),
        Error::RequestLimitExceeded {
            field: "targets",
            requested: 65,
            limit: 64
        }
    ));

    let too_many_paths = DiffSymbolsRequest {
        targets: (0..33)
            .map(|index| DiffSymbolsTarget {
                path: format!("src/file_{index}.rs"),
                symbol: "item".into(),
                head_path: None,
                head_symbol: None,
            })
            .collect(),
        ..request.clone()
    };
    assert!(matches!(
        services
            .history_diff_symbols(too_many_paths)
            .await
            .expect_err("endpoint path bound"),
        Error::RequestLimitExceeded {
            field: "base paths",
            requested: 33,
            limit: 32
        }
    ));

    let duplicate = DiffSymbolsRequest {
        targets: vec![ordinary_target("stable"), ordinary_target("stable")],
        ..request.clone()
    };
    assert!(matches!(
        services
            .history_diff_symbols(duplicate)
            .await
            .expect_err("duplicate pairing"),
        Error::InvalidInput {
            field: "targets",
            reason: "must not contain duplicate symbol pairings"
        }
    ));

    let incomplete_pair = DiffSymbolsRequest {
        targets: vec![DiffSymbolsTarget {
            path: "src/lib.rs".into(),
            symbol: "stable".into(),
            head_path: Some("src/moved.rs".into()),
            head_symbol: None,
        }],
        ..request
    };
    assert!(matches!(
        services
            .history_diff_symbols(incomplete_pair)
            .await
            .expect_err("incomplete endpoint pairing"),
        Error::InvalidInput {
            field: "targets",
            reason: "head_path and head_symbol must be supplied together"
        }
    ));
}

#[tokio::test]
async fn symbol_history_resolves_qualified_names_and_absent_diff_endpoints() {
    if !git_available() {
        return;
    }

    let root = tempfile::tempdir().expect("root");
    std::fs::create_dir(root.path().join("src")).expect("source directory");
    std::fs::write(
        root.path().join("src/service.rs"),
        "pub struct Services;\n\nimpl Services {\n    pub fn existing() -> bool { true }\n}\n",
    )
    .expect("base service");
    std::fs::write(
        root.path().join("src/deleted.rs"),
        "pub fn deleted_endpoint() -> bool { true }\n",
    )
    .expect("deleted source");
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
        root.path().join("src/service.rs"),
        "pub struct Services;\n\nimpl Services {\n    pub fn existing() -> bool { true }\n\n    pub fn evaluate_read_receipt() -> bool { true }\n}\n",
    )
    .expect("head service");
    std::fs::write(
        root.path().join("src/added.rs"),
        "pub fn introduced_endpoint() -> bool { true }",
    )
    .expect("added source");
    std::fs::remove_file(root.path().join("src/deleted.rs")).expect("delete source");
    for args in [
        &["add", "-A"][..],
        &["commit", "-m", "change symbol endpoints"][..],
    ] {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(root.path())
            .output()
            .expect("git commit command");
        assert!(output.status.success());
    }
    let head = revision("HEAD");

    let config =
        Config::discover(root.path(), Some(root.path().join("index.sqlite"))).expect("config");
    let services = Services::open(config).expect("services");
    services.index(false).await.expect("index fixture");

    let qualified = services
        .history(HistoryRequest {
            operation: HistoryOperation::ReadSymbol {
                path: "src/service.rs".into(),
                symbol: "Services.evaluate_read_receipt".into(),
                revision: head.clone(),
            },
            max_results: None,
            max_tokens: Some(200),
        })
        .await
        .expect("qualified historical read");
    let qualified_symbol = qualified.symbol.expect("qualified symbol");
    assert_eq!(qualified_symbol.name, "evaluate_read_receipt");
    assert_eq!(qualified_symbol.parent.as_deref(), Some("Services"));

    let qualified_log = services
        .history(HistoryRequest {
            operation: HistoryOperation::SymbolLog {
                path: "src/service.rs".into(),
                symbol: "Services.evaluate_read_receipt".into(),
                revision: Some(head.clone()),
            },
            max_results: Some(10),
            max_tokens: None,
        })
        .await
        .expect("qualified symbol log");
    assert!(
        qualified_log
            .commits
            .iter()
            .any(|commit| commit.subject == "change symbol endpoints")
    );

    let added_read = services
        .history(HistoryRequest {
            operation: HistoryOperation::ReadSymbol {
                path: "src/added.rs".into(),
                symbol: "introduced_endpoint".into(),
                revision: head.clone(),
            },
            max_results: None,
            max_tokens: Some(500),
        })
        .await
        .expect("read symbol from file without final newline");
    let added_raw = added_read.symbol.as_ref().expect("added raw symbol");
    assert_eq!(
        added_raw.content.as_deref(),
        Some("pub fn introduced_endpoint() -> bool { true }")
    );
    let added_raw_hash = added_raw.content_hash.clone();
    assert!(
        serde_json::to_value(&added_read)
            .expect("serialize raw historical read")
            .pointer("/symbol/returned_end_line")
            .is_some()
    );

    let added_symbol = services
        .history(HistoryRequest {
            operation: HistoryOperation::DiffSymbol {
                path: "src/service.rs".into(),
                symbol: "Services.evaluate_read_receipt".into(),
                base_revision: base.clone(),
                head_revision: head.clone(),
            },
            max_results: None,
            max_tokens: Some(500),
        })
        .await
        .expect("added nested symbol diff");
    assert!(added_symbol.before.is_none());
    assert_eq!(
        added_symbol
            .after
            .as_ref()
            .and_then(|symbol| symbol.parent.as_deref()),
        Some("Services")
    );
    assert_eq!(
        added_symbol
            .semantic_change
            .as_ref()
            .map(|change| change.kind),
        Some(DiffSymbolChangeKind::Added)
    );
    assert!(
        added_symbol
            .semantic_change
            .as_ref()
            .expect("added semantic change")
            .public_contract_changed
    );
    assert!(
        added_symbol
            .diff
            .as_deref()
            .expect("added symbol patch")
            .contains("+pub fn evaluate_read_receipt() -> bool { true }")
    );
    assert!(
        !added_symbol
            .diff
            .as_deref()
            .expect("added symbol patch")
            .contains("\\ No newline at end of file")
    );
    assert!(
        serde_json::to_value(&added_symbol)
            .expect("serialize added nested symbol")
            .pointer("/after/returned_end_line")
            .is_none()
    );

    let added_file = services
        .history(HistoryRequest {
            operation: HistoryOperation::DiffSymbol {
                path: "src/added.rs".into(),
                symbol: "introduced_endpoint".into(),
                base_revision: base.clone(),
                head_revision: head.clone(),
            },
            max_results: None,
            max_tokens: Some(500),
        })
        .await
        .expect("added file symbol diff");
    assert!(added_file.before.is_none());
    assert_eq!(
        added_file
            .semantic_change
            .as_ref()
            .map(|change| change.kind),
        Some(DiffSymbolChangeKind::Added)
    );
    let added_endpoint = added_file.after.as_ref().expect("added endpoint");
    assert_eq!(added_endpoint.returned_end_line, 0);
    assert_eq!(added_endpoint.content_hash, added_raw_hash);
    assert!(
        !added_file
            .diff
            .as_deref()
            .expect("added file patch")
            .contains("\\ No newline at end of file")
    );
    assert!(
        serde_json::to_value(&added_file)
            .expect("serialize added file diff")
            .pointer("/after/returned_end_line")
            .is_none()
    );
    assert!(added_file.result_complete);

    let truncated_added_file = services
        .history(HistoryRequest {
            operation: HistoryOperation::DiffSymbol {
                path: "src/added.rs".into(),
                symbol: "introduced_endpoint".into(),
                base_revision: base.clone(),
                head_revision: head.clone(),
            },
            max_results: None,
            max_tokens: Some(1),
        })
        .await
        .expect("truncated added file symbol diff");
    assert!(truncated_added_file.diff_truncated);
    assert!(!truncated_added_file.result_complete);
    assert!(truncated_added_file.before.is_none());
    assert!(truncated_added_file.after.is_some());
    assert!(
        serde_json::to_value(&truncated_added_file)
            .expect("serialize truncated added file diff")
            .pointer("/after/returned_end_line")
            .is_none()
    );
    assert_eq!(
        truncated_added_file
            .semantic_change
            .as_ref()
            .map(|change| change.kind),
        Some(DiffSymbolChangeKind::Added)
    );

    let deleted_file = services
        .history(HistoryRequest {
            operation: HistoryOperation::DiffSymbol {
                path: "src/deleted.rs".into(),
                symbol: "deleted_endpoint".into(),
                base_revision: base.clone(),
                head_revision: head.clone(),
            },
            max_results: None,
            max_tokens: Some(500),
        })
        .await
        .expect("deleted file symbol diff");
    assert!(deleted_file.after.is_none());
    assert_eq!(
        deleted_file
            .semantic_change
            .as_ref()
            .map(|change| change.kind),
        Some(DiffSymbolChangeKind::Removed)
    );
    assert!(
        deleted_file
            .semantic_change
            .as_ref()
            .expect("removed semantic change")
            .public_contract_changed
    );
    assert!(
        deleted_file
            .diff
            .as_deref()
            .expect("deleted symbol patch")
            .contains("-pub fn deleted_endpoint() -> bool { true }")
    );
    assert!(
        !deleted_file
            .diff
            .as_deref()
            .expect("deleted symbol patch")
            .contains("\\ No newline at end of file")
    );
    assert!(
        serde_json::to_value(&deleted_file)
            .expect("serialize deleted file diff")
            .pointer("/before/returned_end_line")
            .is_none()
    );
    assert_response_token_accounting!(deleted_file, Tokenizer::default());

    let missing_file_read = services
        .history(HistoryRequest {
            operation: HistoryOperation::ReadSymbol {
                path: "src/added.rs".into(),
                symbol: "introduced_endpoint".into(),
                revision: base.clone(),
            },
            max_results: None,
            max_tokens: Some(500),
        })
        .await
        .expect_err("ordinary historical reads keep missing-file errors");
    assert!(matches!(
        missing_file_read,
        Error::InvalidInput {
            field: "path",
            reason: "file does not exist at revision"
        }
    ));

    let missing = services
        .history(HistoryRequest {
            operation: HistoryOperation::DiffSymbol {
                path: "src/service.rs".into(),
                symbol: "Services.never_existed".into(),
                base_revision: base,
                head_revision: head,
            },
            max_results: None,
            max_tokens: Some(500),
        })
        .await
        .expect_err("both absent symbol endpoints must fail");
    assert!(matches!(missing, Error::SymbolNotFound { .. }));
}

#[tokio::test]
async fn json_structural_queries_summarize_ignored_artifacts_and_diff_fields() {
    let root = tempfile::tempdir().expect("root");
    std::fs::create_dir(root.path().join("artifacts")).expect("artifact directory");
    std::fs::write(root.path().join(".gitignore"), "artifacts/\n").expect("ignore file");
    std::fs::write(
        root.path().join("artifacts/before.json"),
        r#"{"runs":[{"score":1,"name":"a"},{"score":2,"name":"b"},{"score":3,"name":"c"},{"score":100,"name":"d"}],"version":1}"#,
    )
    .expect("base JSON");
    std::fs::write(
        root.path().join("artifacts/after.json"),
        r#"{"runs":[{"score":2,"name":"a"},{"score":4,"name":"b"}],"status":"done"}"#,
    )
    .expect("head JSON");
    let config =
        Config::discover(root.path(), Some(root.path().join("index.sqlite"))).expect("config");
    let services = Services::open(config).expect("services");

    let query = services
        .json(JsonRequest {
            operation: JsonOperation::Query {
                path: "artifacts/before.json".into(),
                selector: Some(JsonSelector::Pointer {
                    pointer: "/version".into(),
                }),
                projection: JsonProjection::Value,
            },
            max_tokens: Some(100),
            max_items: None,
            array_sample_size: None,
            cursor: None,
        })
        .await
        .expect("pointer query");
    assert_eq!(query.value, Some(serde_json::json!(1)));
    assert_eq!(query.sources[0].path, "artifacts/before.json");
    assert_response_token_accounting!(query, Tokenizer::default());

    let collapsed = services
        .json(JsonRequest {
            operation: JsonOperation::Query {
                path: "artifacts/before.json".into(),
                selector: Some(JsonSelector::Jmespath {
                    expression: "runs".into(),
                }),
                projection: JsonProjection::Collapsed,
            },
            max_tokens: Some(500),
            max_items: Some(100),
            array_sample_size: Some(1),
            cursor: None,
        })
        .await
        .expect("collapsed JMESPath query");
    assert_eq!(collapsed.value.as_ref().expect("value")["$array"]["count"], 4);
    assert_eq!(
        collapsed.value.as_ref().expect("value")["$array"]["sample"]
            .as_array()
            .expect("sample")
            .len(),
        1
    );

    for projection in [JsonProjection::Keys, JsonProjection::Schema] {
        let projected = services
            .json(JsonRequest {
                operation: JsonOperation::Query {
                    path: "artifacts/before.json".into(),
                    selector: None,
                    projection,
                },
                max_tokens: Some(1_000),
                max_items: Some(100),
                array_sample_size: None,
                cursor: None,
            })
            .await
            .expect("structural projection");
        assert!(projected.value.is_some());
        assert!(projected.result_complete);
    }

    let summary = services
        .json(JsonRequest {
            operation: JsonOperation::NumericSummary {
                path: "artifacts/before.json".into(),
                selector: Some(JsonSelector::Jmespath {
                    expression: "runs[].score".into(),
                }),
            },
            max_tokens: None,
            max_items: None,
            array_sample_size: None,
            cursor: None,
        })
        .await
        .expect("numeric summary");
    let statistics = summary.numeric_summary.expect("statistics");
    assert_eq!(statistics.count, 4);
    assert_eq!(statistics.min, Some(1.0));
    assert_eq!(statistics.median, Some(2.5));
    assert_eq!(statistics.p95, Some(100.0));
    assert_eq!(statistics.max, Some(100.0));

    let diff = services
        .json(JsonRequest {
            operation: JsonOperation::DiffFields {
                base_path: "artifacts/before.json".into(),
                head_path: "artifacts/after.json".into(),
                selectors: vec![
                    JsonSelector::Pointer {
                        pointer: "/version".into(),
                    },
                    JsonSelector::Pointer {
                        pointer: "/status".into(),
                    },
                    JsonSelector::Jmespath {
                        expression: "runs[].score".into(),
                    },
                ],
                projection: JsonProjection::Collapsed,
            },
            max_tokens: Some(1_000),
            max_items: Some(100),
            array_sample_size: Some(2),
            cursor: None,
        })
        .await
        .expect("selected-field diff");
    assert_eq!(diff.differences.len(), 3);
    assert!(diff.differences.iter().all(|field| field.changed));
    assert!(diff.differences[0].before_present);
    assert!(!diff.differences[0].after_present);
    assert!(!diff.differences[1].before_present);
    assert!(diff.differences[1].after_present);
    assert_response_token_accounting!(diff, Tokenizer::default());
    let report = services
        .token_savings_report()
        .await
        .expect("JSON response accounting");
    let json = report
        .response_accounting
        .by_operation
        .iter()
        .find(|row| row.operation == TokenAccountingOperation::Json)
        .expect("JSON accounting row");
    assert_eq!(json.tracked_requests, 6);
    assert_eq!(json.baseline_requests, 6);
    assert!(json.baseline_source_tokens > json.response_source_tokens);
    assert!(json.total_response_tokens >= json.response_source_tokens);
    assert_eq!(
        json.estimated_net_tokens_saved,
        i64::try_from(json.baseline_source_tokens).expect("small JSON baseline")
            - i64::try_from(json.total_response_tokens).expect("small JSON responses")
    );
}

#[tokio::test]
async fn json_keys_paginate_by_item_and_token_limits_with_exact_diagnostics() {
    let root = tempfile::tempdir().expect("root");
    let path = root.path().join("report.json");
    std::fs::write(
        &path,
        r#"{"alpha":1,"beta":2,"nested":{"first":3,"second":4},"rows":[{"left":5},{"right":6}]}"#,
    )
    .expect("JSON fixture");
    let config =
        Config::discover(root.path(), Some(root.path().join("index.sqlite"))).expect("config");
    let services = Services::open(config).expect("services");
    let operation = JsonOperation::Query {
        path: "report.json".into(),
        selector: None,
        projection: JsonProjection::Keys,
    };

    let complete = services
        .json(JsonRequest {
            operation: operation.clone(),
            max_tokens: Some(1_000),
            max_items: Some(100),
            array_sample_size: None,
            cursor: None,
        })
        .await
        .expect("complete keys");
    let expected = complete
        .value
        .as_ref()
        .and_then(serde_json::Value::as_array)
        .expect("key array")
        .clone();
    assert!(complete.result_complete);
    assert_eq!(complete.total_items, Some(expected.len()));
    assert_eq!(complete.returned_items, Some(expected.len()));
    assert_eq!(complete.remaining_items, Some(0));
    assert_eq!(complete.incomplete_reason, None);
    assert!(complete.meta.next_cursor.is_none());

    let mut cursor = None;
    let mut observed = Vec::new();
    let mut previous_remaining = expected.len();
    loop {
        let page = services
            .json(JsonRequest {
                operation: operation.clone(),
                max_tokens: Some(1_000),
                max_items: Some(2),
                array_sample_size: None,
                cursor,
            })
            .await
            .expect("keys page");
        let page_values = page
            .value
            .as_ref()
            .and_then(serde_json::Value::as_array)
            .expect("page values");
        assert_eq!(page.total_items, Some(expected.len()));
        assert_eq!(page.returned_items, Some(page_values.len()));
        assert!(page_values.len() <= 2);
        observed.extend(page_values.iter().cloned());
        let remaining = page.remaining_items.expect("remaining count");
        assert_eq!(remaining, expected.len().saturating_sub(observed.len()));
        assert!(remaining <= previous_remaining);
        previous_remaining = remaining;
        if page.result_complete {
            assert_eq!(page.incomplete_reason, None);
            assert!(page.meta.next_cursor.is_none());
            break;
        }
        assert_eq!(
            page.incomplete_reason,
            Some(JsonIncompleteReason::MaxItems)
        );
        cursor = page.meta.next_cursor;
        assert!(cursor.is_some());
    }
    assert_eq!(observed, expected);

    let one_item = services
        .json(JsonRequest {
            operation: operation.clone(),
            max_tokens: Some(1_000),
            max_items: Some(1),
            array_sample_size: None,
            cursor: None,
        })
        .await
        .expect("one key");
    let token_limited = services
        .json(JsonRequest {
            operation,
            max_tokens: Some(one_item.meta.source_tokens),
            max_items: Some(100),
            array_sample_size: None,
            cursor: None,
        })
        .await
        .expect("token-limited key page");
    assert_eq!(token_limited.returned_items, Some(1));
    assert_eq!(
        token_limited.incomplete_reason,
        Some(JsonIncompleteReason::MaxTokens)
    );
    assert!(token_limited.meta.source_tokens <= one_item.meta.source_tokens);
    assert!(token_limited.meta.next_cursor.is_some());
    assert_response_token_accounting!(token_limited, Tokenizer::default());
}

#[tokio::test]
async fn json_schema_degrades_breadth_first_under_token_limits() {
    let root = tempfile::tempdir().expect("root");
    let mut deep = serde_json::json!(true);
    for index in (0..80).rev() {
        deep = serde_json::json!({format!("level_{index:02}"): deep});
    }
    let mut fixture = serde_json::Map::new();
    fixture.insert("deep".into(), deep);
    fixture.insert("empty_array".into(), serde_json::json!([]));
    fixture.insert("empty_object".into(), serde_json::json!({}));
    fixture.insert(
        "gate".into(),
        serde_json::json!({"enabled": true, "mode": "strict"}),
    );
    for index in 0..16 {
        fixture.insert(format!("top_{index:02}"), serde_json::json!(index));
    }
    std::fs::write(
        root.path().join("wide.json"),
        serde_json::to_vec(&serde_json::Value::Object(fixture)).expect("serialize fixture"),
    )
    .expect("write fixture");
    let config =
        Config::discover(root.path(), Some(root.path().join("index.sqlite"))).expect("config");
    let services = Services::open(config).expect("services");
    let operation = JsonOperation::Query {
        path: "wide.json".into(),
        selector: None,
        projection: JsonProjection::Schema,
    };

    let full = services
        .json(JsonRequest {
            operation: operation.clone(),
            max_tokens: Some(32_000),
            max_items: Some(10_000),
            array_sample_size: None,
            cursor: None,
        })
        .await
        .expect("complete schema");
    assert!(full.result_complete);
    assert!(
        full.value
            .as_ref()
            .expect("complete schema value")
            .get("x-leantoken-incomplete")
            .is_none()
    );
    let partial_limit = full.meta.source_tokens.saturating_sub(1).max(1);
    let partial = services
        .json(JsonRequest {
            operation,
            max_tokens: Some(partial_limit),
            max_items: Some(10_000),
            array_sample_size: None,
            cursor: None,
        })
        .await
        .expect("token-bounded schema");

    assert!(!partial.result_complete);
    assert_eq!(
        partial.incomplete_reason,
        Some(JsonIncompleteReason::MaxTokens)
    );
    assert!(partial.meta.source_tokens <= partial_limit);
    assert!(partial.meta.next_cursor.is_none());
    assert!(
        partial
            .remaining_items
            .is_some_and(|remaining| remaining > 0)
    );
    let partial_value = partial.value.as_ref().expect("partial schema value");
    let properties = partial_value["properties"]
        .as_object()
        .expect("partial top-level properties");
    for key in [
        "deep",
        "empty_array",
        "empty_object",
        "gate",
        "top_00",
        "top_15",
    ] {
        assert!(properties.contains_key(key), "missing shallow key {key}");
    }
    assert!(
        partial_value["x-leantoken-incomplete"]["omitted_subtree_count"]
            .as_u64()
            .is_some_and(|count| count > 0)
    );
    assert!(
        partial_value["x-leantoken-incomplete"]["omitted_subtree_pointers"]
            .as_array()
            .is_some_and(|pointers| !pointers.is_empty())
    );
    assert_response_token_accounting!(partial, Tokenizer::default());

    let exact = services
        .json(JsonRequest {
            operation: JsonOperation::Query {
                path: "wide.json".into(),
                selector: Some(JsonSelector::Pointer {
                    pointer: "/gate".into(),
                }),
                projection: JsonProjection::Schema,
            },
            max_tokens: Some(100),
            max_items: Some(100),
            array_sample_size: None,
            cursor: None,
        })
        .await
        .expect("exact selector schema");
    assert!(exact.result_complete);
    assert_eq!(
        exact.value.as_ref().expect("exact schema")["properties"]["enabled"]["type"],
        "boolean"
    );
}

#[tokio::test]
async fn compact_response_projections_preserve_verifiable_coverage_and_reduce_tokens() {
    let root = tempfile::tempdir().expect("root");
    std::fs::create_dir(root.path().join("src")).expect("create src");
    let callers = (0..24)
        .map(|index| {
            format!(
                "pub fn caller_{index:02}() -> usize {{\n    target()\n}}\n\n"
            )
        })
        .collect::<String>();
    std::fs::write(
        root.path().join("src/lib.rs"),
        format!(
            "pub fn target() -> usize {{\n    42\n}}\n\n{callers}"
        ),
    )
    .expect("write primary source");
    std::fs::write(
        root.path().join("src/other.rs"),
        "use crate::target;\n\npub fn indirect() -> usize {\n    target()\n}\n",
    )
    .expect("write secondary source");
    let config =
        Config::discover(root.path(), Some(root.path().join("index.sqlite"))).expect("config");
    let services = Services::open(config).expect("services");
    services.index(false).await.expect("index fixture");

    let files_request = FilesRequest {
        operation: FileOperation::Find,
        path: None,
        query: Some("src".into()),
        pattern: None,
        max_results: Some(100),
        cursor: None,
        depth: None,
    };
    let full_files = services
        .files(files_request.clone())
        .await
        .expect("full files");
    let compact_files = services
        .files_paths(files_request)
        .await
        .expect("path-only files");
    assert_eq!(
        compact_files.paths,
        full_files
            .entries
            .iter()
            .map(|entry| entry.path.clone())
            .collect::<Vec<_>>()
    );
    assert!(
        compact_files.meta.total_response_tokens < full_files.meta.total_response_tokens,
        "path-only projection must reduce the complete serialized response"
    );
    assert_response_token_accounting!(compact_files, Tokenizer::default());

    let outline_request = OutlineRequest {
        paths: vec!["src/lib.rs".into(), "src/other.rs".into()],
        symbol_name: None,
        symbol_kind: None,
        max_results: Some(100),
        max_tokens: Some(32_000),
        receipt_id: None,
        cursor: None,
    };
    let full_outline = services
        .outline(outline_request.clone())
        .await
        .expect("full outline");
    let compact_outline = services
        .outline_signatures(outline_request)
        .await
        .expect("signature-only outline");
    let full_symbols = full_outline
        .files
        .iter()
        .flat_map(|file| {
            file.symbols.iter().map(|symbol| {
                (
                    file.path.clone(),
                    symbol.name.clone(),
                    symbol.kind.clone(),
                    symbol.parent.clone(),
                    symbol.signature.clone(),
                    symbol.start_line,
                    symbol.end_line,
                )
            })
        })
        .collect::<Vec<_>>();
    let compact_symbols = compact_outline
        .files
        .iter()
        .flat_map(|file| {
            assert_eq!(
                file.content_hash,
                leantoken::text::hash(
                    &serde_json::to_string(&file.signatures)
                        .expect("serialize compact signatures")
                )
            );
            file.signatures.iter().map(|symbol| {
                (
                    file.path.clone(),
                    symbol.name.clone(),
                    symbol.kind.clone(),
                    symbol.parent.clone(),
                    symbol.signature.clone(),
                    symbol.start_line,
                    symbol.end_line,
                )
            })
        })
        .collect::<Vec<_>>();
    assert_eq!(compact_symbols, full_symbols);
    assert_eq!(compact_outline.total_symbols, full_outline.total_symbols);
    assert_eq!(
        compact_outline.returned_symbols,
        full_outline.returned_symbols
    );
    assert_eq!(compact_outline.parse_complete, full_outline.parse_complete);
    assert!(
        compact_outline.meta.total_response_tokens < full_outline.meta.total_response_tokens,
        "signature projection must reduce the complete serialized response"
    );
    let compact_outline_json =
        serde_json::to_string(&compact_outline).expect("serialize compact outline");
    assert!(!compact_outline_json.contains("start_byte"));
    assert!(!compact_outline_json.contains("\"imports\""));
    assert_response_token_accounting!(compact_outline, Tokenizer::default());

    let search_request = SearchRequest {
        query: "target".into(),
        mode: SearchMode::Auto,
        include_paths: Vec::new(),
        exclude_paths: Vec::new(),
        focus_paths: Vec::new(),
        max_results: Some(100),
        max_tokens: Some(32_000),
        context_lines: Some(0),
        case_sensitive: false,
        all_occurrences: false,
        prefer_structural: true,
        receipt_id: None,
        cursor: None,
    };
    let full_search = services
        .search(search_request.clone())
        .await
        .expect("full search");
    let compact_search = services
        .search_grouped(search_request)
        .await
        .expect("grouped search");
    assert_eq!(
        compact_search
            .groups
            .iter()
            .map(|group| group.total_hits)
            .sum::<usize>(),
        full_search.hits.len()
    );
    assert!(
        compact_search
            .groups
            .iter()
            .any(|group| group.definition.is_some()),
        "grouped search must retain the exact definition"
    );
    let expected_references = full_search
        .hits
        .iter()
        .filter(|hit| {
            hit.role == Some(leantoken::ReferenceRole::Reference)
                || hit.match_kinds.iter().any(|kind| kind == "reference")
        })
        .count();
    assert_eq!(
        compact_search
            .groups
            .iter()
            .flat_map(|group| &group.references)
            .map(|references| references.count)
            .sum::<usize>(),
        expected_references
    );
    assert_eq!(compact_search.coverage, full_search.coverage);
    assert!(
        compact_search.meta.total_response_tokens < full_search.meta.total_response_tokens,
        "grouped search must reduce the complete serialized response"
    );
    let compact_search_json =
        serde_json::to_string(&compact_search).expect("serialize grouped search");
    assert!(!compact_search_json.contains("\"score\""));
    assert!(!compact_search_json.contains("score_reasons"));
    assert_response_token_accounting!(compact_search, Tokenizer::default());

    let mut files_page_request = FilesRequest {
        operation: FileOperation::Find,
        path: None,
        query: Some("src".into()),
        pattern: None,
        max_results: Some(1),
        cursor: None,
        depth: None,
    };
    let mut paged_paths = Vec::new();
    loop {
        let page = services
            .files_paths(files_page_request.clone())
            .await
            .expect("path-only page");
        paged_paths.extend(page.paths);
        let Some(cursor) = page.meta.next_cursor else {
            break;
        };
        files_page_request.cursor = Some(cursor);
    }
    assert_eq!(paged_paths, compact_files.paths);

    let outline_page_request = OutlineRequest {
        paths: vec!["src/lib.rs".into(), "src/other.rs".into()],
        symbol_name: None,
        symbol_kind: None,
        max_results: Some(5),
        max_tokens: Some(32_000),
        receipt_id: None,
        cursor: None,
    };
    let full_outline_cursor = services
        .outline(outline_page_request.clone())
        .await
        .expect("full outline page")
        .meta
        .next_cursor
        .expect("full outline continuation");
    let stale_projection = services
        .outline_signatures(OutlineRequest {
            cursor: Some(full_outline_cursor),
            ..outline_page_request.clone()
        })
        .await
        .expect_err("projection-bound outline cursor");
    assert!(matches!(stale_projection, Error::StaleCursor));

    let mut outline_page_request = outline_page_request;
    let mut paged_signatures = Vec::new();
    loop {
        let page = services
            .outline_signatures(outline_page_request.clone())
            .await
            .expect("signature outline page");
        paged_signatures.extend(page.files.iter().flat_map(|file| {
            file.signatures.iter().map(|symbol| {
                (
                    file.path.clone(),
                    symbol.name.clone(),
                    symbol.kind.clone(),
                    symbol.parent.clone(),
                    symbol.signature.clone(),
                    symbol.start_line,
                    symbol.end_line,
                )
            })
        }));
        let Some(cursor) = page.meta.next_cursor else {
            break;
        };
        outline_page_request.cursor = Some(cursor);
    }
    assert_eq!(paged_signatures, compact_symbols);

    let paged_search_request = SearchRequest {
        query: "target".into(),
        mode: SearchMode::Auto,
        include_paths: Vec::new(),
        exclude_paths: Vec::new(),
        focus_paths: Vec::new(),
        max_results: Some(4),
        max_tokens: Some(32_000),
        context_lines: Some(0),
        case_sensitive: false,
        all_occurrences: false,
        prefer_structural: true,
        receipt_id: None,
        cursor: None,
    };
    let mut full_page_request = paged_search_request.clone();
    let mut full_paged_hits = 0usize;
    loop {
        let page = services
            .search(full_page_request.clone())
            .await
            .expect("full search page");
        full_paged_hits = full_paged_hits.saturating_add(page.hits.len());
        let Some(cursor) = page.meta.next_cursor else {
            break;
        };
        full_page_request.cursor = Some(cursor);
    }
    let mut grouped_page_request = paged_search_request;
    let mut grouped_paged_hits = 0usize;
    loop {
        let page = services
            .search_grouped(grouped_page_request.clone())
            .await
            .expect("grouped search page");
        grouped_paged_hits = grouped_paged_hits.saturating_add(
            page.groups
                .iter()
                .map(|group| group.total_hits)
                .sum::<usize>(),
        );
        let Some(cursor) = page.meta.next_cursor else {
            break;
        };
        grouped_page_request.cursor = Some(cursor);
    }
    assert_eq!(grouped_paged_hits, full_paged_hits);

    let bounded_files = services
        .files_paths_with_options(
            FilesRequest {
                operation: FileOperation::Find,
                path: None,
                query: Some("src".into()),
                pattern: None,
                max_results: Some(100),
                cursor: None,
                depth: None,
            },
            ServiceCallOptions::new()
                .with_max_response_tokens(compact_files.meta.total_response_tokens),
        )
        .await
        .expect("exact path-only response bound");
    assert!(
        bounded_files.meta.total_response_tokens
            <= compact_files.meta.total_response_tokens
    );
    let bounded_outline = services
        .outline_signatures_with_options(
            OutlineRequest {
                paths: vec!["src/lib.rs".into(), "src/other.rs".into()],
                symbol_name: None,
                symbol_kind: None,
                max_results: Some(100),
                max_tokens: Some(32_000),
                receipt_id: None,
                cursor: None,
            },
            ServiceCallOptions::new()
                .with_max_response_tokens(compact_outline.meta.total_response_tokens),
        )
        .await
        .expect("exact signature response bound");
    assert!(
        bounded_outline.meta.total_response_tokens
            <= compact_outline.meta.total_response_tokens
    );
    let bounded_search = services
        .search_grouped_with_options(
            SearchRequest {
                query: "target".into(),
                mode: SearchMode::Auto,
                include_paths: Vec::new(),
                exclude_paths: Vec::new(),
                focus_paths: Vec::new(),
                max_results: Some(100),
                max_tokens: Some(32_000),
                context_lines: Some(0),
                case_sensitive: false,
                all_occurrences: false,
                prefer_structural: true,
                receipt_id: None,
                cursor: None,
            },
            ServiceCallOptions::new()
                .with_max_response_tokens(compact_search.meta.total_response_tokens),
        )
        .await
        .expect("exact grouped response bound");
    assert!(
        bounded_search.meta.total_response_tokens
            <= compact_search.meta.total_response_tokens
    );
}

#[tokio::test]
async fn json_cursors_and_incomplete_results_fail_loud_with_typed_diagnostics() {
    let root = tempfile::tempdir().expect("root");
    let path = root.path().join("report.json");
    std::fs::write(&path, r#"{"version":1,"nested":{"answer":42},"tail":true}"#)
        .expect("JSON fixture");
    let config =
        Config::discover(root.path(), Some(root.path().join("index.sqlite"))).expect("config");
    let services = Services::open(config).expect("services");
    let operation = JsonOperation::Query {
        path: "report.json".into(),
        selector: None,
        projection: JsonProjection::Keys,
    };
    let first = services
        .json(JsonRequest {
            operation: operation.clone(),
            max_tokens: Some(1_000),
            max_items: Some(1),
            array_sample_size: None,
            cursor: None,
        })
        .await
        .expect("first page");
    let cursor = first.meta.next_cursor.expect("continuation cursor");

    let mismatched_query = services
        .json(JsonRequest {
            operation: JsonOperation::Query {
                path: "report.json".into(),
                selector: Some(JsonSelector::Pointer {
                    pointer: "/nested".into(),
                }),
                projection: JsonProjection::Keys,
            },
            max_tokens: Some(1_000),
            max_items: Some(1),
            array_sample_size: None,
            cursor: Some(cursor.clone()),
        })
        .await
        .expect_err("cursor query binding");
    assert!(matches!(mismatched_query, Error::StaleCursor));

    let unsupported_projection = services
        .json(JsonRequest {
            operation: JsonOperation::Query {
                path: "report.json".into(),
                selector: None,
                projection: JsonProjection::Schema,
            },
            max_tokens: Some(1_000),
            max_items: Some(1),
            array_sample_size: None,
            cursor: Some(cursor.clone()),
        })
        .await
        .expect_err("cursor projection boundary");
    assert!(matches!(
        unsupported_projection,
        Error::InvalidInput {
            field: "cursor",
            ..
        }
    ));

    std::fs::write(
        &path,
        r#"{"version":2,"nested":{"answer":42},"tail":true}"#,
    )
    .expect("mutated JSON fixture");
    let stale_source = services
        .json(JsonRequest {
            operation: operation.clone(),
            max_tokens: Some(1_000),
            max_items: Some(1),
            array_sample_size: None,
            cursor: Some(cursor),
        })
        .await
        .expect_err("cursor source binding");
    assert!(matches!(stale_source, Error::StaleCursor));

    let incomplete_schema = services
        .json(JsonRequest {
            operation: JsonOperation::Query {
                path: "report.json".into(),
                selector: None,
                projection: JsonProjection::Schema,
            },
            max_tokens: Some(1_000),
            max_items: Some(2),
            array_sample_size: None,
            cursor: None,
        })
        .await
        .expect("bounded schema");
    assert!(!incomplete_schema.result_complete);
    assert_eq!(incomplete_schema.returned_items, Some(2));
    assert!(
        incomplete_schema.total_items.expect("total") > incomplete_schema.returned_items.unwrap()
    );
    assert_eq!(
        incomplete_schema.remaining_items,
        Some(
            incomplete_schema.total_items.unwrap()
                - incomplete_schema.returned_items.unwrap()
        )
    );
    assert_eq!(
        incomplete_schema.incomplete_reason,
        Some(JsonIncompleteReason::MaxItems)
    );
    assert!(incomplete_schema.meta.next_cursor.is_none());

    let typed_selector = services
        .json(JsonRequest {
            operation: JsonOperation::Query {
                path: "report.json".into(),
                selector: Some(JsonSelector::Jmespath {
                    expression: "length(version)".into(),
                }),
                projection: JsonProjection::Value,
            },
            max_tokens: Some(100),
            max_items: Some(100),
            array_sample_size: None,
            cursor: None,
        })
        .await
        .expect_err("typed JMESPath error");
    assert!(matches!(
        &typed_selector,
        Error::InvalidJsonSelector {
            stage: "evaluate",
            offset: 6,
            line: 1,
            column: 7,
            reason,
            ..
        } if reason.contains("expects type") && reason.contains("given number")
    ), "{typed_selector:?}");

    let invalid_expression = services
        .json(JsonRequest {
            operation: JsonOperation::Query {
                path: "report.json".into(),
                selector: Some(JsonSelector::Jmespath {
                    expression: "length(".into(),
                }),
                projection: JsonProjection::Value,
            },
            max_tokens: Some(100),
            max_items: Some(100),
            array_sample_size: None,
            cursor: None,
        })
        .await
        .expect_err("JMESPath compile error");
    assert!(matches!(
        invalid_expression,
        Error::InvalidJsonSelector {
            stage: "compile",
            line: 1,
            ..
        }
    ));

    std::fs::write(&path, r#"{"outer":[1,]}"#).expect("invalid JSON fixture");
    let syntax = services
        .json(JsonRequest {
            operation,
            max_tokens: Some(100),
            max_items: Some(100),
            array_sample_size: None,
            cursor: None,
        })
        .await
        .expect_err("JSON syntax error");
    assert!(matches!(
        syntax,
        Error::InvalidJson {
            syntax_category: "syntax",
            byte_offset: 12,
            line: 1,
            column: 13,
            ..
        }
    ));
}

#[tokio::test]
async fn working_tree_diff_boosts_changed_files() {
    if !git_available() {
        return;
    }

    let root = tempfile::tempdir().expect("root");
    std::fs::create_dir(root.path().join("src")).unwrap();
    std::fs::write(root.path().join("src/a.rs"), "fn shared() {}\n").unwrap();
    std::fs::write(root.path().join("src/b.rs"), "fn shared() {}\n").unwrap();
    init_git_repo(root.path());

    let config = Config::discover(root.path(), Some(root.path().join("index.sqlite"))).unwrap();
    let services = Services::open(config).unwrap();
    services.index(false).await.unwrap();

    // Modify b.rs after indexing; do not reindex so the diff signal is tested.
    std::fs::write(root.path().join("src/b.rs"), "fn shared() { let x = 1; }\n").unwrap();

    let response = services
        .context(ContextRequest {
            task: "update shared implementation".into(),
            token_budget: 500,
            include_paths: Vec::new(),
            must_include_paths: Vec::new(),
            must_include_symbols: Vec::new(),
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
        verbose_diagnostics: false,
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

#[tokio::test]
async fn tokenizer_configuration_is_scoped_to_each_service() {
    let root = tempfile::tempdir().expect("root");
    std::fs::write(
        root.path().join("lib.rs"),
        "fn independent_token_budget() { println!(\"hello\"); }\n",
    )
    .expect("source");
    let mut exact_config =
        Config::discover(root.path(), Some(root.path().join("exact.sqlite"))).expect("config");
    exact_config.tokenizer = leantoken::tokens::Tokenizer::O200kBase;
    let mut estimate_config =
        Config::discover(root.path(), Some(root.path().join("estimate.sqlite"))).expect("config");
    estimate_config.tokenizer = leantoken::tokens::Tokenizer::Estimate;
    let exact = Services::open(exact_config).expect("exact services");
    let estimate = Services::open(estimate_config).expect("estimate services");
    exact.index(false).await.expect("exact index");
    estimate.index(false).await.expect("estimate index");
    let request = ContextRequest {
        task: "change independent_token_budget".into(),
        token_budget: 100,
        include_paths: Vec::new(),
        must_include_paths: Vec::new(),
        must_include_symbols: Vec::new(),
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
    verbose_diagnostics: false,
    };

    let (exact_response, estimate_response) =
        tokio::join!(exact.context(request.clone()), estimate.context(request),);

    let exact_response = exact_response.expect("exact context");
    let estimate_response = estimate_response.expect("estimate context");
    assert_response_token_accounting!(exact_response, Tokenizer::O200kBase);
    assert_response_token_accounting!(estimate_response, Tokenizer::Estimate);
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
    services.index(false).await.expect("index");

    let response = services
        .context(ContextRequest {
            task: "fix target_symbol".into(),
            token_budget: 600,
            include_paths: Vec::new(),
            must_include_paths: Vec::new(),
            must_include_symbols: Vec::new(),
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
        verbose_diagnostics: false,
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
    services.index(false).await.expect("index");

    let response = services
        .context(ContextRequest {
            task: "fix rare_runtime_marker behavior".into(),
            token_budget: 1200,
            include_paths: Vec::new(),
            must_include_paths: Vec::new(),
            must_include_symbols: Vec::new(),
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
        verbose_diagnostics: false,
        })
        .await
        .expect("context");
    let text_fragment = response
        .fragments
        .iter()
        .find(|fragment| {
            fragment.path == "lib.rs" && fragment.reason.contains("text")
        })
        .expect("text fragment");

    assert!(
        text_fragment.token_count <= 320,
        "oversized text fragment: {text_fragment:?}"
    );
    assert!(text_fragment.content.contains("rare_runtime_marker"));
}

#[tokio::test]
async fn regex_search_respects_absolute_candidate_cap() {
    let root = tempfile::tempdir().expect("root");
    // Many matching files so limit*20 alone would exceed MAX_REGEX_CANDIDATES if
    // uncapped; the hard cap must still bound results.
    for index in 0..80 {
        std::fs::write(
            root.path().join(format!("f{index}.rs")),
            "fn needle() { let needle = 1; }\n".repeat(40),
        )
        .expect("write");
    }
    let config =
        Config::discover(root.path(), Some(root.path().join("index.sqlite"))).expect("config");
    let services = Services::open(config).expect("services");
    services.index(false).await.expect("index");

    let response = services
        .search(SearchRequest {
            query: "needle".into(),
            mode: SearchMode::Regex,
            include_paths: Vec::new(),
            exclude_paths: Vec::new(),
            focus_paths: Vec::new(),
            max_results: Some(100),
            max_tokens: Some(32_000),
            context_lines: Some(0),
            case_sensitive: false,
            all_occurrences: false,
            prefer_structural: false,
            receipt_id: None,
            cursor: None,
        })
        .await
        .expect("regex search");
    assert!(!response.hits.is_empty());
    // max_results bounds the returned page, but the path must complete without
    // scanning unbounded; generation must be a committed snapshot.
    assert!(response.meta.repository_generation >= 1);
    assert!(response.hits.len() <= 100);
}

#[tokio::test]
async fn reconcile_working_tree_search_reconciles_file_created_after_index() {
    let root = tempfile::tempdir().expect("root");
    std::fs::write(root.path().join("lib.rs"), "fn existing() {}\n").expect("initial source");
    let config =
        Config::discover(root.path(), Some(root.path().join("index.sqlite"))).expect("config");
    let services = Services::open(config).expect("services");
    let initial = services.index(false).await.expect("initial index");

    std::fs::write(
        root.path().join("new_package.rs"),
        "fn newly_committed_package() {}\n",
    )
    .expect("new source");

    let response = services
        .search_with_consistency_cancellable(
            SearchRequest {
                query: "newly_committed_package".into(),
                mode: SearchMode::Identifier,
                include_paths: Vec::new(),
                exclude_paths: Vec::new(),
                focus_paths: Vec::new(),
                max_results: Some(10),
                max_tokens: Some(100),
                context_lines: Some(0),
                case_sensitive: false,
                all_occurrences: false,
                prefer_structural: false,
                receipt_id: None,
                cursor: None,
            },
            IndexConsistency::ReconcileWorkingTree,
            CancellationToken::new(),
        )
        .await
        .expect("working-tree search");

    assert_eq!(response.hits.len(), 1);
    assert_eq!(response.hits[0].path, "new_package.rs");
    assert!(response.meta.repository_generation > initial.repository_generation);
}

#[tokio::test]
async fn indexed_generation_search_does_not_reconcile_file_created_after_index() {
    let root = tempfile::tempdir().expect("root");
    std::fs::write(root.path().join("lib.rs"), "fn existing() {}\n").expect("initial source");
    let config =
        Config::discover(root.path(), Some(root.path().join("index.sqlite"))).expect("config");
    let services = Services::open(config).expect("services");
    let initial = services.index(false).await.expect("initial index");

    std::fs::write(
        root.path().join("new_package.rs"),
        "fn newly_committed_package() {}\n",
    )
    .expect("new source");

    let response = services
        .search_with_consistency_cancellable(
            SearchRequest {
                query: "newly_committed_package".into(),
                mode: SearchMode::Identifier,
                include_paths: Vec::new(),
                exclude_paths: Vec::new(),
                focus_paths: Vec::new(),
                max_results: Some(10),
                max_tokens: Some(100),
                context_lines: Some(0),
                case_sensitive: false,
                all_occurrences: false,
                prefer_structural: false,
                receipt_id: None,
                cursor: None,
            },
            IndexConsistency::IndexedGeneration,
            CancellationToken::new(),
        )
        .await
        .expect("committed search");

    assert!(response.hits.is_empty());
    assert_eq!(
        response.meta.repository_generation,
        initial.repository_generation
    );
}

#[tokio::test]
async fn reconcile_working_tree_consistency_applies_to_each_retrieval_service() {
    let root = tempfile::tempdir().expect("root");
    std::fs::write(root.path().join("lib.rs"), "fn existing() {}\n").expect("initial source");
    let config =
        Config::discover(root.path(), Some(root.path().join("index.sqlite"))).expect("config");
    let services = Services::open(config).expect("services");
    services.index(false).await.expect("initial index");

    std::fs::write(root.path().join("files_package.rs"), "fn files_package() {}\n")
        .expect("files source");
    let files = services
        .files_with_consistency_cancellable(
            FilesRequest {
                operation: FileOperation::Find,
                path: None,
                query: Some("files_package".into()),
                pattern: None,
                max_results: Some(10),
                cursor: None,
                depth: None,
            },
            IndexConsistency::ReconcileWorkingTree,
            CancellationToken::new(),
        )
        .await
        .expect("working-tree files");
    assert!(files.entries.iter().any(|entry| entry.path == "files_package.rs"));

    std::fs::write(
        root.path().join("outline_package.rs"),
        "fn outlined_package() {}\n",
    )
    .expect("outline source");
    let outline = services
        .outline_with_consistency_cancellable(
            OutlineRequest {
                paths: vec!["outline_package.rs".into()],
                symbol_name: Some("outlined_package".into()),
                symbol_kind: None,
                max_results: Some(10),
                max_tokens: Some(100),
                receipt_id: None,
                cursor: None,
            },
            IndexConsistency::ReconcileWorkingTree,
            CancellationToken::new(),
        )
        .await
        .expect("working-tree outline");
    assert_eq!(outline.files[0].symbols[0].name, "outlined_package");

    std::fs::write(
        root.path().join("read_package.rs"),
        "fn readable_package() {}\n",
    )
    .expect("read source");
    let read = services
        .read_with_consistency_cancellable(
            ReadRequest {
                path: "read_package.rs".into(),
                start_line: Some(1),
                end_line: Some(1),
                symbol: None,
                heading: None,
                heading_occurrence: None,
                continuation_cursor: None,
                max_tokens: Some(100),
                expected_hash: None,
                delta: false,
                receipt_id: None,
            },
            IndexConsistency::ReconcileWorkingTree,
            CancellationToken::new(),
        )
        .await
        .expect("working-tree read");
    assert!(read.content.as_deref().is_some_and(|value| value.contains("readable_package")));
    assert!(!read.index_stale);

    std::fs::write(
        root.path().join("context_package.rs"),
        "fn contextual_package_marker() {}\n",
    )
    .expect("context source");
    let context = services
        .context_with_consistency_cancellable(
            ContextRequest {
                task: "change contextual_package_marker".into(),
                token_budget: 200,
                include_paths: Vec::new(),
                must_include_paths: Vec::new(),
                must_include_symbols: Vec::new(),
                max_fragments: None,
                plan_only: false,
                focus_paths: vec!["context_package.rs".into()],
                strict_focus_paths: false,
                minimum_fragments_per_focus_path: None,
                focus_symbols: vec!["contextual_package_marker".into()],
                exclude_paths: Vec::new(),
                known_hashes: Vec::new(),
                receipt_id: None,
                prior_repository_generation: None,
            base_revision: None,
            changed_paths: Vec::new(),
            strict_changed_paths: false,
            verbose_diagnostics: false,
            },
            IndexConsistency::ReconcileWorkingTree,
            CancellationToken::new(),
        )
        .await
        .expect("working-tree context");
    assert!(
        context
            .fragments
            .iter()
            .any(|fragment| fragment.path == "context_package.rs")
    );
    let report = services
        .token_savings_report()
        .await
        .expect("consistent response accounting");
    let context_accounting = report
        .response_accounting
        .by_operation
        .iter()
        .find(|row| row.operation == TokenAccountingOperation::Context)
        .expect("context accounting");
    assert_eq!(context_accounting.tracked_requests, 1);
    assert_eq!(
        context_accounting.total_response_tokens,
        context.meta.total_response_tokens as u64
    );
}

#[tokio::test]
async fn read_reports_index_stale_when_live_file_diverges() {
    let root = tempfile::tempdir().expect("root");
    std::fs::write(root.path().join("lib.rs"), "fn first() { 1 }\n").expect("write");
    let config =
        Config::discover(root.path(), Some(root.path().join("index.sqlite"))).expect("config");
    let services = Services::open(config).expect("services");
    services.index(false).await.expect("index");

    std::fs::write(root.path().join("lib.rs"), "fn second() { 2 }\n").expect("edit live");
    let response = services
        .read(ReadRequest {
            path: "lib.rs".into(),
            start_line: Some(1),
            end_line: Some(1),
            symbol: None,
            heading: None,
            heading_occurrence: None,
            continuation_cursor: None,
            max_tokens: Some(100),
            expected_hash: None,
            delta: false,
            receipt_id: None,
        })
        .await
        .expect("read");
    assert!(response.index_stale, "live rewrite without reindex must set index_stale");
    assert!(response.content.as_deref().is_some_and(|c| c.contains("second")));
    assert!(response.indexed_hash.is_some());
    assert_ne!(
        response.indexed_hash.as_deref(),
        Some(response.content_hash.as_str()),
        "range hash and whole-file indexed hash differ in meaning but live file is stale"
    );
    assert_eq!(response.meta.repository_generation, 1);
    assert_eq!(response.meta.freshness, Freshness::Current);
}

#[tokio::test]
async fn read_not_modified_still_reports_index_stale_against_live_file() {
    let root = tempfile::tempdir().expect("root");
    std::fs::write(root.path().join("lib.rs"), "fn first() { 1 }\n").expect("write");
    let config =
        Config::discover(root.path(), Some(root.path().join("index.sqlite"))).expect("config");
    let services = Services::open(config).expect("services");
    services.index(false).await.expect("index");

    let first = services
        .read(ReadRequest {
            path: "lib.rs".into(),
            start_line: Some(1),
            end_line: Some(1),
            symbol: None,
            heading: None,
            heading_occurrence: None,
            continuation_cursor: None,
            max_tokens: Some(100),
            expected_hash: None,
            delta: false,
            receipt_id: None,
        })
        .await
        .expect("first read");
    assert!(!first.index_stale);
    assert_eq!(first.status, ReadStatus::Content);

    // Live body changes but the caller still presents the old range hash.
    std::fs::write(root.path().join("lib.rs"), "fn other() { 9 }\n").expect("edit");
    let second = services
        .read(ReadRequest {
            path: "lib.rs".into(),
            start_line: Some(1),
            end_line: Some(1),
            symbol: None,
            heading: None,
            heading_occurrence: None,
            continuation_cursor: None,
            max_tokens: Some(100),
            expected_hash: Some(first.content_hash.clone()),
            delta: false,
            receipt_id: None,
        })
        .await
        .expect("second read");
    // expected_hash compares against the live range hash, so a changed file is
    // Content + index_stale rather than NotModified.
    assert_eq!(second.status, ReadStatus::Content);
    assert!(second.index_stale);
    assert!(second.content.as_deref().is_some_and(|c| c.contains("other")));
}

#[tokio::test]
async fn status_reports_reconciling_when_shared_operation_lock_is_held() {
    let root = tempfile::tempdir().expect("root");
    std::fs::write(root.path().join("lib.rs"), "fn ready() {}\n").expect("write");
    let database = root.path().join("index.sqlite");
    let config = Config::discover(root.path(), Some(database.clone())).expect("config");
    let services = Services::open(config).expect("services");
    services.index(false).await.expect("index");

    let before = services.status().await.expect("status before");
    assert_eq!(before.freshness, Freshness::Current);
    assert_eq!(before.index_state, IndexState::Ready);
    assert!(before.repository_generation >= 1);

    let coordination = IndexCoordination::for_database(&database);
    let _operation = coordination
        .acquire_operation(&CancellationToken::new())
        .expect("hold shared operation lock");

    let during = services.status().await.expect("status during lock");
    assert_eq!(
        during.freshness,
        Freshness::Reconciling,
        "followers must see reconciling via the shared operation lock"
    );
    assert_eq!(during.index_state, IndexState::Ready);
    assert_eq!(during.repository_generation, before.repository_generation);
}

#[test]
fn read_only_status_does_not_wait_for_an_active_writer() {
    let root = tempfile::tempdir().expect("root");
    std::fs::write(root.path().join("lib.rs"), "fn ready() {}\n").expect("write");
    let database = root.path().join("index.sqlite");
    let config = Config::discover(root.path(), Some(database.clone())).expect("config");
    let services = Services::open(config.clone()).expect("services");

    let connection = rusqlite::Connection::open(&database).expect("writer connection");
    connection
        .execute_batch("BEGIN IMMEDIATE")
        .expect("hold writer transaction");

    let started = Instant::now();
    let status = Services::status_without_initializing(config).expect("read-only status");
    assert!(
        started.elapsed().as_secs() < 1,
        "status waited on writer for {:?}",
        started.elapsed()
    );
    assert_eq!(status.repository_generation, 0);
    assert_eq!(status.index_state, IndexState::Uninitialized);

    drop(services);
    connection
        .execute_batch("ROLLBACK")
        .expect("release writer transaction");
}

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
            verbose_diagnostics: false,
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
    if !git_available() {
        return;
    }

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
    assert_eq!(response.coverage.strict_scope_satisfied, Some(true));
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
    if !git_available() {
        return;
    }

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
                verbose_diagnostics: false,
            },
            HandoffManifestRequest::default(),
            ContextWorkflow::Review,
            IndexConsistency::IndexedGeneration,
            CancellationToken::new(),
        )
        .await
        .expect("base-revision context");

    assert_eq!(response.coverage.strict_scope_satisfied, Some(true));
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
    assert_eq!(
        working_tree.coverage.strict_scope_satisfied,
        Some(true)
    );
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
    if !git_available() {
        return;
    }

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
            verbose_diagnostics: false,
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
            verbose_diagnostics: false,
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
            verbose_diagnostics: false,
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
            verbose_diagnostics: false,
        })
        .await
        .expect("context with unindexed changed path");

    let scope = response
        .diff_scope
        .as_ref()
        .expect("diff scope receipt present");
    assert_eq!(scope.indexed_changed_paths, 0);
}
