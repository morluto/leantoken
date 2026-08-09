use super::*;
use leantoken::{ObservedTokenSavingsReport, TaskSavingsObservationStatus};

#[tokio::test]
async fn token_savings_tracks_successful_source_retrievals_by_operation() {
    let (root, services) = fixture().await;
    let initial = services.token_savings().await.expect("initial savings");
    assert_eq!(initial.tracked_requests, 0);
    assert_eq!(initial.estimated_source_tokens_saved, 0);
    assert_eq!(initial.by_operation.len(), 4);
    let initial_snapshot = services
        .observed_token_savings_snapshot(None)
        .await
        .expect("initial snapshot");
    assert_eq!(initial_snapshot.window, TokenSavingsWindow::Lifetime);

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
            policy: leantoken::ReadPolicy::default(),
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
            policy: leantoken::ReadPolicy::default(),
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
    assert_eq!(report.tracked_requests, 4);
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
            (TokenSavingsOperation::Read, 1),
            (TokenSavingsOperation::Context, 1),
        ]
    );
    assert_eq!(
        report.emitted_source_tokens,
        search.meta.source_tokens as u64
            + outline.meta.source_tokens as u64
            + first_read.meta.source_tokens as u64
            + context.meta.source_tokens as u64
    );
    assert!(report.baseline_source_tokens >= report.emitted_source_tokens);
    assert!(report.estimated_source_tokens_saved > 0);
    let effective = services
        .token_savings_report()
        .await
        .expect("effective savings");
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
    assert_eq!(accounting.by_operation.len(), 9);
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
            (TokenAccountingOperation::ReceiptRebase, 0),
        ]
    );
    let observed = services
        .observed_token_savings_report()
        .await
        .expect("observed accounting");
    assert_eq!(observed.report, effective);
    assert_eq!(observed.observations.successful_response_records, 5);
    assert_eq!(observed.observations.responses_with_baseline, 5);
    assert_eq!(observed.observations.failed_service_requests, 1);
    assert_eq!(
        observed.observations.expected_hash_not_modified_responses,
        1
    );
    assert!(observed.observations.expected_hash_suppressed_source_tokens > 0);
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
    assert_eq!(observed.observations.request_classification.useful, 4);
    assert_eq!(
        observed.observations.request_classification.hash_suppressed,
        1
    );
    assert_eq!(observed.observations.request_classification.failed, 1);
    assert_eq!(
        observed.observed_task_savings.status,
        TaskSavingsObservationStatus::Unavailable
    );
    assert_eq!(observed.observed_task_savings.observed_tasks, 0);
    assert_eq!(
        observed.observed_task_savings.unknown_relevance_responses,
        5
    );
    assert_eq!(observed.observed_task_savings.failed_retrieval_calls, 1);
    assert_eq!(observed.observed_task_savings.failure_response_tokens, None);
    assert_eq!(observed.observed_task_savings.retry_calls, None);
    assert_eq!(observed.observed_task_savings.superseded_calls, None);
    assert_eq!(observed.observed_task_savings.net_tokens_saved, None);
    assert_eq!(
        observed.observed_task_savings.savings_rate_basis_points,
        None
    );
    let delta = services
        .observed_token_savings_snapshot(Some(initial_snapshot.snapshot))
        .await
        .expect("snapshot delta");
    assert_eq!(delta.window, TokenSavingsWindow::Delta);
    assert_eq!(delta.observed, observed);
    assert!(delta.snapshot.len() < 4_096);
    let zero_delta = services
        .observed_token_savings_snapshot(Some(delta.snapshot.clone()))
        .await
        .expect("empty snapshot delta");
    assert_eq!(
        zero_delta.observed.observations.successful_response_records,
        0
    );
    assert_eq!(zero_delta.observed.observations.failed_service_requests, 0);
    assert_eq!(
        zero_delta
            .observed
            .report
            .response_accounting
            .total_response_tokens,
        0
    );
    let (_other_root, other_services) = fixture().await;
    assert!(matches!(
        other_services
            .observed_token_savings_snapshot(Some(delta.snapshot.clone()))
            .await,
        Err(Error::InvalidInput {
            field: "snapshot",
            ..
        })
    ));
    let mut invalid_snapshot = delta.snapshot;
    invalid_snapshot.push('x');
    assert!(matches!(
        services
            .observed_token_savings_snapshot(Some(invalid_snapshot))
            .await,
        Err(Error::InvalidInput {
            field: "snapshot",
            ..
        })
    ));
    let serialized = serde_json::to_value(&observed).expect("serialize observed accounting");
    assert!(
        serialized
            .pointer("/response_accounting/estimate_basis")
            .is_some()
    );
    assert!(
        serialized
            .pointer("/response_accounting/estimated_net_tokens_saved")
            .is_some()
    );
    assert!(serialized.get("response_accounting").is_some());
    assert!(serialized.get("observed_task_savings").is_some());
    assert!(
        serialized
            .pointer("/observed_task_savings/retry_calls")
            .is_some_and(serde_json::Value::is_null)
    );
    assert!(
        serialized
            .pointer("/observed_task_savings/superseded_calls")
            .is_some_and(serde_json::Value::is_null)
    );
    assert!(
        serialized
            .pointer("/observed_task_savings/net_tokens_saved")
            .is_some_and(serde_json::Value::is_null)
    );
    assert!(serialized.get("observations").is_some());
    assert!(serialized.get("report").is_none());
    let mut legacy = serialized.clone();
    let object = legacy.as_object_mut().expect("observed report object");
    object.remove("observed_task_savings");
    let legacy: ObservedTokenSavingsReport =
        serde_json::from_value(legacy).expect("deserialize legacy observed report");
    assert_eq!(
        legacy.observed_task_savings.status,
        TaskSavingsObservationStatus::Unavailable
    );
    assert_eq!(legacy.observed_task_savings.retry_calls, None);
    assert_eq!(legacy.observations.request_classification.useful, 4);

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
async fn receipt_rebase_records_success_and_failure_accounting() {
    let (root, services) = fixture().await;
    let source = services
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
            policy: leantoken::ReadPolicy::default(),
        })
        .await
        .expect("source read");
    let source_receipt = source.meta.receipt_id.expect("source receipt");
    std::fs::write(root.path().join("src/rebase-added.rs"), "fn added() {}\n")
        .expect("generation edit");
    services.index(false).await.expect("publish generation");

    services
        .rebase_receipt(ReceiptRebaseRequest {
            receipt_id: source_receipt.clone(),
            max_samples_per_outcome: Some(0),
        })
        .await
        .expect("successful rebase");
    assert!(matches!(
        services
            .rebase_receipt(ReceiptRebaseRequest {
                receipt_id: source_receipt,
                max_samples_per_outcome: Some(17),
            })
            .await,
        Err(Error::RequestLimitExceeded {
            field: "max_samples_per_outcome",
            requested: 17,
            limit: 16,
        })
    ));

    let observed = services
        .observed_token_savings_report()
        .await
        .expect("observed accounting");
    let rebase = observed
        .report
        .response_accounting
        .by_operation
        .iter()
        .find(|row| row.operation == TokenAccountingOperation::ReceiptRebase)
        .expect("receipt rebase row");
    assert_eq!(rebase.tracked_requests, 1);
    assert_eq!(
        observed
            .observations
            .failed_by_operation_and_category
            .iter()
            .filter(|failure| failure.operation == TokenAccountingOperation::ReceiptRebase)
            .map(|failure| (failure.error_category.as_str(), failure.failed_requests))
            .collect::<Vec<_>>(),
        vec![("request_limit_exceeded", 1)]
    );
}

#[tokio::test]
async fn savings_excludes_incomplete_and_zero_symbol_latex_outlines_from_source_compression() {
    let (root, services) = fixture().await;
    std::fs::write(
        root.path().join("empty.tex"),
        "Plain prose without structural LaTeX commands.\n",
    )
    .expect("write empty LaTeX fixture");
    services.index(false).await.expect("index LaTeX fixture");
    let base = services
        .observed_token_savings_snapshot(None)
        .await
        .expect("baseline snapshot");

    let unsupported = services
        .outline(OutlineRequest {
            paths: vec!["empty.tex".into()],
            symbol_name: None,
            symbol_kind: None,
            max_results: Some(10),
            max_tokens: Some(100),
            receipt_id: None,
            cursor: None,
        })
        .await
        .expect("zero-symbol LaTeX outline");
    assert_eq!(unsupported.total_symbols, 0);
    assert_eq!(unsupported.files[0].language.as_deref(), Some("latex"));

    let incomplete = services
        .outline(OutlineRequest {
            paths: vec!["src/lib.rs".into()],
            symbol_name: None,
            symbol_kind: None,
            max_results: Some(1),
            max_tokens: Some(100),
            receipt_id: None,
            cursor: None,
        })
        .await
        .expect("bounded outline");
    assert!(!incomplete.result_complete);
    let delta = services
        .observed_token_savings_snapshot(Some(base.snapshot))
        .await
        .expect("classified delta");
    assert_eq!(delta.window, TokenSavingsWindow::Delta);
    assert_eq!(
        delta.observed.report.response_accounting.tracked_requests,
        2
    );
    assert_eq!(
        delta
            .observed
            .observations
            .request_classification
            .unsupported,
        1
    );
    assert_eq!(
        delta
            .observed
            .observations
            .request_classification
            .incomplete,
        1
    );
    assert_eq!(delta.observed.observations.request_classification.failed, 0);
    assert_eq!(delta.observed.observations.failed_service_requests, 0);
}
