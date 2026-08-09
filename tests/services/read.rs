use super::*;

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
            policy: leantoken::ReadPolicy::default(),
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
            policy: leantoken::ReadPolicy::Full,
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
            policy: leantoken::ReadPolicy::Full,
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
            policy: leantoken::ReadPolicy::Full,
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
            policy: leantoken::ReadPolicy::Full,
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
    assert_eq!(receipt.delta_tokens, Some(changed.meta.source_tokens));
    assert!(receipt.full_tokens > changed.meta.source_tokens);
    assert_eq!(
        receipt.avoided_tokens,
        receipt.full_tokens - changed.meta.source_tokens
    );
    assert!(receipt.fallback_reason.is_none());
    assert_response_token_accounting!(changed, Tokenizer::Cl100kBase);
}

#[tokio::test]
async fn read_delta_restart_matches_the_process_local_oracle() {
    let source = (1..=120)
        .map(|line| format!("let value_{line} = compute_value({line});\n"))
        .collect::<String>();
    let (persistent_root, persistent_a) = indexed_source("restart.rs", source.as_bytes()).await;
    let (oracle_root, oracle) = indexed_source("restart.rs", source.as_bytes()).await;
    let request = |expected_hash: Option<String>| ReadRequest {
        path: "restart.rs".into(),
        start_line: None,
        end_line: None,
        symbol: None,
        heading: None,
        heading_occurrence: None,
        continuation_cursor: None,
        max_tokens: Some(32_000),
        expected_hash,
        delta: true,
        receipt_id: None,
        policy: leantoken::ReadPolicy::Full,
    };

    let persistent_base = persistent_a
        .read(request(None))
        .await
        .expect("persist clean base");
    let oracle_base = oracle
        .read(request(None))
        .await
        .expect("capture process-local oracle base");
    assert_eq!(persistent_base.content_hash, oracle_base.content_hash);
    let persistent_receipt = persistent_base
        .delta_receipt
        .as_ref()
        .expect("persistent base receipt");
    assert!(persistent_receipt.head_persisted);
    assert!(persistent_receipt.persistence_fallback_reason.is_none());

    let changed_source = source.replace(
        "let value_60 = compute_value(60);",
        "let value_60 = compute_updated_value(60);",
    );
    std::fs::write(persistent_root.path().join("restart.rs"), &changed_source)
        .expect("edit persistent source");
    std::fs::write(oracle_root.path().join("restart.rs"), &changed_source)
        .expect("edit oracle source");
    drop(persistent_a);
    let persistent_b = Services::open(
        Config::discover(
            persistent_root.path(),
            Some(persistent_root.path().join("index.sqlite")),
        )
        .expect("restart config"),
    )
    .expect("restart services");

    let expected_hash = persistent_base.content_hash.clone();
    let restarted = persistent_b
        .read(request(Some(expected_hash.clone())))
        .await
        .expect("read from persistent base");
    let in_memory = oracle
        .read(request(Some(expected_hash)))
        .await
        .expect("read from process-local base");
    assert_eq!(restarted.status, ReadStatus::Delta);
    assert_eq!(restarted.status, in_memory.status);
    assert_eq!(restarted.delta, in_memory.delta);
    assert_eq!(restarted.content_hash, in_memory.content_hash);
    assert_eq!(restarted.indexed_hash, in_memory.indexed_hash);
    assert_eq!(restarted.target_start_line, in_memory.target_start_line);
    assert_eq!(restarted.target_end_line, in_memory.target_end_line);
    assert_eq!(restarted.returned_start_line, in_memory.returned_start_line);
    assert_eq!(restarted.returned_end_line, in_memory.returned_end_line);
    let restarted_receipt = restarted.delta_receipt.as_ref().expect("restart receipt");
    let oracle_receipt = in_memory.delta_receipt.as_ref().expect("oracle receipt");
    assert_eq!(
        restarted_receipt.base_source,
        Some(ReadDeltaBaseSource::Persistent)
    );
    assert_eq!(
        oracle_receipt.base_source,
        Some(ReadDeltaBaseSource::ProcessLocal)
    );
    assert_eq!(
        restarted_receipt.persistence_fallback_reason,
        Some(ReadDeltaPersistenceFallback::LiveDiffersFromIndex)
    );
    assert!(!restarted_receipt.head_persisted);
    assert_eq!(restarted_receipt.outcome, oracle_receipt.outcome);
    assert_eq!(
        restarted_receipt.base_generation,
        oracle_receipt.base_generation
    );
    assert_eq!(restarted_receipt.full_tokens, oracle_receipt.full_tokens);
    assert_eq!(restarted_receipt.delta_tokens, oracle_receipt.delta_tokens);
    assert_eq!(
        restarted_receipt.avoided_tokens,
        oracle_receipt.avoided_tokens
    );
}

#[tokio::test]
async fn dirty_unindexed_and_ignored_delta_bases_never_persist() {
    let source = (1..=80)
        .map(|line| format!("let value_{line} = compute_value({line});\n"))
        .collect::<String>();
    let (root, services) = indexed_source("dirty.rs", source.as_bytes()).await;
    let dirty_source = source.replace(
        "let value_40 = compute_value(40);",
        "let value_40 = compute_dirty_value(40);",
    );
    std::fs::write(root.path().join("dirty.rs"), &dirty_source).expect("dirty source");
    let dirty = services
        .read(ReadRequest {
            path: "dirty.rs".into(),
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
            policy: leantoken::ReadPolicy::Full,
        })
        .await
        .expect("capture process-local dirty base");
    let dirty_receipt = dirty.delta_receipt.as_ref().expect("dirty receipt");
    assert!(!dirty_receipt.head_persisted);
    assert_eq!(
        dirty_receipt.persistence_fallback_reason,
        Some(ReadDeltaPersistenceFallback::LiveDiffersFromIndex)
    );
    let connection =
        rusqlite::Connection::open(root.path().join("index.sqlite")).expect("inspect database");
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM read_delta_bases", [], |row| {
                row.get::<_, i64>(0)
            })
            .expect("persistent base count"),
        0
    );
    drop(connection);

    drop(services);
    std::fs::write(
        root.path().join("dirty.rs"),
        dirty_source.replace("compute_dirty_value", "compute_dirtier_value"),
    )
    .expect("edit dirty source again");
    let reopened = Services::open(
        Config::discover(root.path(), Some(root.path().join("index.sqlite")))
            .expect("restart config"),
    )
    .expect("restart services");
    let after_restart = reopened
        .read(ReadRequest {
            path: "dirty.rs".into(),
            start_line: None,
            end_line: None,
            symbol: None,
            heading: None,
            heading_occurrence: None,
            continuation_cursor: None,
            max_tokens: Some(32_000),
            expected_hash: Some(dirty.content_hash),
            delta: true,
            receipt_id: None,
            policy: leantoken::ReadPolicy::Full,
        })
        .await
        .expect("dirty base unavailable after restart");
    let restart_receipt = after_restart.delta_receipt.expect("restart receipt");
    assert_eq!(
        restart_receipt.fallback_reason,
        Some(ReadDeltaFallback::BaseUnavailable)
    );
    assert!(restart_receipt.base_source.is_none());

    let isolated = tempfile::tempdir().expect("isolated repository");
    std::fs::create_dir(isolated.path().join(".git")).expect("git marker");
    std::fs::write(isolated.path().join(".gitignore"), "ignored.rs\n").expect("ignore file");
    std::fs::write(isolated.path().join("tracked.rs"), "fn tracked() {}\n").expect("tracked file");
    std::fs::write(isolated.path().join("ignored.rs"), "fn secret() {}\n").expect("ignored file");
    let isolated_config =
        Config::discover(isolated.path(), Some(isolated.path().join("index.sqlite")))
            .expect("isolated config");
    let isolated_services = Services::open(isolated_config).expect("isolated services");
    isolated_services
        .index(false)
        .await
        .expect("index isolated repository");
    std::fs::write(isolated.path().join("unindexed.rs"), "fn new_file() {}\n")
        .expect("unindexed file");
    for path in ["ignored.rs", "unindexed.rs"] {
        assert!(
            isolated_services
                .read(ReadRequest {
                    path: path.into(),
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
                    policy: leantoken::ReadPolicy::Full,
                })
                .await
                .is_err(),
            "{path} must not become a delta base"
        );
    }
    let connection = rusqlite::Connection::open(isolated.path().join("index.sqlite"))
        .expect("inspect isolated database");
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM read_delta_bases", [], |row| {
                row.get::<_, i64>(0)
            })
            .expect("isolated persistent base count"),
        0
    );
}

#[tokio::test]
async fn read_delta_automatically_uses_the_latest_exact_target_base() {
    let source = (1..=80)
        .map(|line| format!("let value_{line} = compute_value({line});\n"))
        .collect::<String>();
    let (root, services) = indexed_source("latest.rs", source.as_bytes()).await;
    let savings_base = services
        .observed_token_savings_snapshot(None)
        .await
        .expect("savings base");
    let request = || ReadRequest {
        path: "latest.rs".into(),
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
        policy: leantoken::ReadPolicy::Full,
    };

    let first = services.read(request()).await.expect("capture latest base");
    let first_receipt = first.delta_receipt.as_ref().expect("first receipt");
    let first_generation = first_receipt.head_generation;
    assert_eq!(first_receipt.outcome, ReadDeltaOutcome::Full);
    assert_eq!(
        first_receipt.fallback_reason,
        Some(ReadDeltaFallback::BaseUnavailable)
    );

    let unchanged = services
        .read(request())
        .await
        .expect("automatic unchanged read");
    assert_eq!(unchanged.status, ReadStatus::NotModified);
    assert!(unchanged.not_modified);
    assert!(unchanged.content.is_none());
    assert_eq!(unchanged.meta.source_tokens, 0);
    let unchanged_receipt = unchanged.delta_receipt.as_ref().expect("unchanged receipt");
    assert_eq!(unchanged_receipt.outcome, ReadDeltaOutcome::NotModified);
    assert_eq!(
        unchanged_receipt.base_hash.as_deref(),
        Some(first.content_hash.as_str())
    );
    assert_eq!(unchanged_receipt.base_generation, Some(first_generation));

    let changed_source = source.replace(
        "let value_40 = compute_value(40);",
        "let value_40 = compute_updated_value(40);",
    );
    std::fs::write(root.path().join("latest.rs"), &changed_source).expect("first edit");
    let changed = services
        .read(request())
        .await
        .expect("automatic changed read");
    assert_eq!(changed.status, ReadStatus::Delta);
    assert_eq!(
        changed
            .delta_receipt
            .as_ref()
            .and_then(|receipt| receipt.base_hash.as_deref()),
        Some(first.content_hash.as_str())
    );
    assert!(
        changed
            .delta
            .as_deref()
            .is_some_and(|delta| delta.contains("compute_updated_value"))
    );

    let latest_hash = changed.content_hash.clone();
    let changed_again_source = changed_source.replace(
        "let value_60 = compute_value(60);",
        "let value_60 = compute_updated_value(60);",
    );
    std::fs::write(root.path().join("latest.rs"), &changed_again_source).expect("second edit");
    let changed_again = services.read(request()).await.expect("latest changed read");
    assert_eq!(changed_again.status, ReadStatus::Delta);
    assert_eq!(
        changed_again
            .delta_receipt
            .as_ref()
            .and_then(|receipt| receipt.base_hash.as_deref()),
        Some(latest_hash.as_str()),
        "the second edit must use the most recently captured head"
    );

    let ordinary_source = changed_again_source.replace(
        "let value_70 = compute_value(70);",
        "let value_70 = compute_updated_value(70);",
    );
    std::fs::write(root.path().join("latest.rs"), ordinary_source).expect("ordinary edit");
    let mut ordinary_request = request();
    ordinary_request.delta = false;
    let ordinary = services
        .read(ordinary_request)
        .await
        .expect("ordinary read");
    assert_eq!(ordinary.status, ReadStatus::Content);
    assert!(ordinary.content.is_some());
    assert!(ordinary.delta_receipt.is_none());

    let after_ordinary = services
        .read(request())
        .await
        .expect("delta read after ordinary read");
    assert_eq!(after_ordinary.status, ReadStatus::Delta);
    assert_eq!(
        after_ordinary
            .delta_receipt
            .as_ref()
            .and_then(|receipt| receipt.base_hash.as_deref()),
        Some(changed_again.content_hash.as_str()),
        "an ordinary read must not replace the latest opt-in delta base"
    );

    let savings = services
        .observed_token_savings_snapshot(Some(savings_base.snapshot))
        .await
        .expect("read delta savings");
    assert_eq!(
        savings
            .observed
            .observations
            .expected_hash_not_modified_responses,
        0,
        "automatic base selection is not an expected_hash match"
    );
    assert_eq!(
        savings
            .observed
            .observations
            .request_classification
            .hash_suppressed,
        1
    );
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
            policy: leantoken::ReadPolicy::Full,
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
    assert!(!receipt.head_persisted);
    assert_eq!(
        receipt.persistence_fallback_reason,
        Some(ReadDeltaPersistenceFallback::CurrentTruncated)
    );
    assert_eq!(receipt.avoided_tokens, 0);
}

#[tokio::test]
async fn read_delta_falls_back_when_the_diff_is_not_smaller() {
    let (root, services) = indexed_source("small.txt", b"alpha\n").await;
    let _first = services
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
            policy: leantoken::ReadPolicy::Full,
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
            expected_hash: None,
            delta: true,
            receipt_id: None,
            policy: leantoken::ReadPolicy::Full,
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
            policy: leantoken::ReadPolicy::Full,
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
            expected_hash: None,
            delta: true,
            receipt_id: None,
            policy: leantoken::ReadPolicy::Full,
        })
        .await
        .expect("fall back after target movement");

    assert_eq!(changed.status, ReadStatus::Content);
    assert!(changed.content.as_deref().is_some_and(|content| {
        content.contains("new_behavior") && !content.contains("old_behavior")
    }));
    let receipt = changed.delta_receipt.expect("coordinate fallback");
    assert_eq!(
        receipt.base_hash.as_deref(),
        Some(first.content_hash.as_str())
    );
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
            policy: leantoken::ReadPolicy::default(),
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
            policy: leantoken::ReadPolicy::default(),
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
            policy: leantoken::ReadPolicy::default(),
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
            policy: leantoken::ReadPolicy::default(),
        })
        .await
        .expect("receipt-suppressed read");

    assert_eq!(repeated.status, ReadStatus::ReceiptSuppressed);
    assert!(!repeated.not_modified);
    assert!(repeated.content.is_none());
    assert_eq!(repeated.meta.receipt_suppressed_exact, 1);
    assert_eq!(repeated.meta.source_tokens, 0);
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
            policy: leantoken::ReadPolicy::default(),
        })
        .await
        .expect("exact range");
    assert_eq!((exact.returned_start_line, exact.returned_end_line), (2, 3));
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
            policy: leantoken::ReadPolicy::default(),
        })
        .await
        .expect("conditional exact range");
    assert_eq!(unchanged.status, ReadStatus::NotModified);
    assert!(unchanged.content.is_none());
    assert_eq!(unchanged.meta.source_tokens, 0);

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
            policy: leantoken::ReadPolicy::default(),
        })
        .await
        .expect("open-ended range");
    assert_eq!(
        (
            from_second.returned_start_line,
            from_second.returned_end_line
        ),
        (2, 5)
    );
    assert_eq!(
        from_second.content.as_deref(),
        Some("two\nthree\nfour\nfive\n")
    );

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
            policy: leantoken::ReadPolicy::default(),
        })
        .await
        .expect("open-start range");
    assert_eq!(
        (
            through_third.returned_start_line,
            through_third.returned_end_line
        ),
        (1, 3)
    );
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
            policy: leantoken::ReadPolicy::default(),
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
            policy: leantoken::ReadPolicy::default(),
        })
        .await
        .expect("exact whole file");
    assert_eq!(
        whole.content.as_deref(),
        Some("one\ntwo\nthree\nfour\nfive\n")
    );
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
            policy: leantoken::ReadPolicy::default(),
        })
        .await
        .expect("range through EOF");
    assert_eq!(
        (
            through_eof.returned_start_line,
            through_eof.returned_end_line
        ),
        (4, 5)
    );
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
            policy: leantoken::ReadPolicy::Full,
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
            policy: leantoken::ReadPolicy::default(),
        })
        .await
        .expect("symbol range");

    assert_eq!(
        (response.returned_start_line, response.returned_end_line),
        (3, 6)
    );
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
            policy: leantoken::ReadPolicy::default(),
        })
        .await
        .expect("bounded open-ended read");

    let content = response.content.as_deref().expect("content");
    assert!(content.len() <= 12 * 32);
    assert!(content.contains("generated_5000"));
    assert!(response.returned_start_line >= 5_000);
    assert!(response.meta.source_tokens <= 12);
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
            policy: leantoken::ReadPolicy::default(),
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
            policy: leantoken::ReadPolicy::default(),
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
            policy: leantoken::ReadPolicy::default(),
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
            policy: leantoken::ReadPolicy::default(),
        })
        .await
        .expect("open CRLF range");

    assert_eq!((exact.returned_start_line, exact.returned_end_line), (2, 3));
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
            policy: leantoken::ReadPolicy::default(),
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
            policy: leantoken::ReadPolicy::default(),
        })
        .await
        .expect("empty file");
    assert_eq!((empty.returned_start_line, empty.returned_end_line), (1, 1));
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
                policy: leantoken::ReadPolicy::default(),
            })
            .await
            .expect_err("invalid range");
        assert!(matches!(
            error,
            Error::InvalidInput {
                field: "line range",
                ..
            }
        ));
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
            policy: leantoken::ReadPolicy::default(),
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
                "1:read:v4:1:1:1:1:f:00000000000000000000000000000000:-:0000000000000000:0:-"
                    .into(),
            ),
            max_tokens: Some(100),
            expected_hash: None,
            delta: false,
            receipt_id: None,
            policy: leantoken::ReadPolicy::default(),
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
            policy: leantoken::ReadPolicy::default(),
        })
        .await
        .expect("token-truncated range");
    let content = response.content.as_deref().expect("content");
    let returned_lines = content
        .lines()
        .count()
        .max(usize::from(!content.is_empty()));

    assert!(!content.is_empty());
    assert_eq!(response.status, ReadStatus::Truncated);
    assert!(response.truncated);
    assert_eq!(
        (response.target_start_line, response.target_end_line),
        (2, 4)
    );
    assert!(response.next_start_line.is_some());
    assert!(response.continuation_cursor.is_some());
    assert_eq!(response.returned_start_line, 2);
    assert_eq!(
        response.returned_end_line,
        response.returned_start_line + returned_lines - 1
    );
    assert!(response.returned_end_line <= 4);
    assert!(response.meta.source_tokens <= 3);
}

#[tokio::test]
async fn truncated_symbol_guidance_replaces_many_tiny_pages_with_one_sized_continuation() {
    let body = (1..=80)
        .map(|line| format!("    let value_{line:03} = input + {line};\n"))
        .collect::<String>();
    let source = format!("pub fn oversized_owner(input: usize) -> usize {{\n{body}    input\n}}\n");
    let (_root, services) = indexed_source("owner.rs", source.as_bytes()).await;
    let request = |cursor: Option<String>, max_tokens, policy| ReadRequest {
        path: "owner.rs".into(),
        start_line: None,
        end_line: None,
        symbol: cursor.is_none().then(|| "oversized_owner".into()),
        heading: None,
        heading_occurrence: None,
        continuation_cursor: cursor,
        max_tokens: Some(max_tokens),
        expected_hash: None,
        delta: false,
        receipt_id: None,
        policy,
    };

    let first = services
        .read(request(None, 12, leantoken::ReadPolicy::Bounded))
        .await
        .expect("tiny first page");
    let guidance = first
        .truncation_guidance
        .as_ref()
        .expect("truncation guidance");
    let first_content = first.content.as_deref().expect("first-page source");
    let expected_remaining = Tokenizer::Cl100kBase.count(&source[first_content.len()..]);
    assert_eq!(
        guidance.basis,
        leantoken::ReadTruncationGuidanceBasis::IndexedGenerationEstimate
    );
    assert_eq!(
        guidance.target_source_tokens,
        Tokenizer::Cl100kBase.count(&source)
    );
    assert_eq!(guidance.remaining_source_tokens, expected_remaining);
    assert_eq!(
        guidance.remaining_pages_at_current_budget,
        expected_remaining.div_ceil(12)
    );
    assert_eq!(guidance.recommended_next_max_tokens, expected_remaining);
    assert_eq!(guidance.minimum_remaining_pages, 1);

    let mut naive_cursor = first.continuation_cursor.clone();
    let mut naive_pages = 1usize;
    let mut naive_response_tokens = first.meta.total_response_tokens;
    while let Some(cursor) = naive_cursor {
        let page = services
            .read(request(Some(cursor), 12, leantoken::ReadPolicy::Bounded))
            .await
            .expect("tiny continuation");
        naive_pages += 1;
        naive_response_tokens =
            naive_response_tokens.saturating_add(page.meta.total_response_tokens);
        naive_cursor = page.continuation_cursor;
        assert!(naive_pages < 100, "tiny continuations must make progress");
    }
    assert!(naive_pages >= 13, "fixture used only {naive_pages} pages");

    let recommended = services
        .read(request(
            first.continuation_cursor.clone(),
            guidance.recommended_next_max_tokens,
            leantoken::ReadPolicy::Bounded,
        ))
        .await
        .expect("sized continuation");
    assert!(!recommended.truncated);
    assert!(recommended.truncation_guidance.is_none());
    let guided_response_tokens = first
        .meta
        .total_response_tokens
        .saturating_add(recommended.meta.total_response_tokens);
    assert!(
        guided_response_tokens.saturating_mul(4) < naive_response_tokens,
        "guided={guided_response_tokens} naive={naive_response_tokens}"
    );
    assert_eq!(
        format!(
            "{first_content}{}",
            recommended.content.as_deref().expect("remaining source")
        ),
        source
    );

    let verified = services
        .read(request(None, 12, leantoken::ReadPolicy::Full))
        .await
        .expect("verified first page");
    assert_eq!(
        verified
            .truncation_guidance
            .expect("verified guidance")
            .basis,
        leantoken::ReadTruncationGuidanceBasis::VerifiedLive
    );
}

#[tokio::test]
async fn exact_tokenizers_reject_source_budgets_that_cannot_advance_a_page() {
    let leading_scalar = "\u{10000}";
    let first_source = format!("{leading_scalar}tail\n");
    let continuation_prefix = "ascii prefix\n";
    let continuation_source = format!("{continuation_prefix}{leading_scalar}tail\n");
    let exact_tokenizers = [
        Tokenizer::Cl100kBase,
        Tokenizer::O200kBase,
        Tokenizer::O200kHarmony,
        Tokenizer::P50kBase,
        Tokenizer::R50kBase,
        Tokenizer::Gpt2,
        Tokenizer::P50kEdit,
    ];

    for tokenizer in exact_tokenizers {
        assert!(
            tokenizer.count(leading_scalar) > 1,
            "fixture scalar must need multiple tokens for {tokenizer:?}"
        );
        let continuation_budget = (1..tokenizer.count(&continuation_source))
            .find(|budget| {
                tokenizer.truncate(&continuation_source, *budget).0 == continuation_prefix
            })
            .unwrap_or_else(|| panic!("fixture must expose a prefix boundary for {tokenizer:?}"));

        let root = tempfile::tempdir().expect("temporary repository");
        std::fs::write(root.path().join("first.txt"), &first_source).expect("write first page");
        std::fs::write(root.path().join("continuation.txt"), &continuation_source)
            .expect("write continuation page");
        let mut config =
            Config::discover(root.path(), Some(root.path().join("index.sqlite"))).expect("config");
        config.tokenizer = tokenizer;
        let services = Services::open(config).expect("services");
        services.index(false).await.expect("index source");

        let read_request =
            |path: &str, continuation_cursor: Option<String>, max_tokens: usize| ReadRequest {
                path: path.into(),
                start_line: None,
                end_line: None,
                symbol: None,
                heading: None,
                heading_occurrence: None,
                continuation_cursor,
                max_tokens: Some(max_tokens),
                expected_hash: None,
                delta: false,
                receipt_id: None,
                policy: leantoken::ReadPolicy::Bounded,
            };
        let assert_budget_error = |error: &Error, boundary: &str| {
            assert!(
                matches!(
                    error,
                    Error::InvalidInput {
                        field: "max_tokens",
                        reason: "must fit at least one UTF-8 scalar",
                    }
                ),
                "unexpected {boundary} error for {tokenizer:?}: {error:?}"
            );
        };

        let first_error = services
            .read(read_request("first.txt", None, 1))
            .await
            .expect_err("budget cannot return the first page's first UTF-8 scalar");
        assert_budget_error(&first_error, "first-page");

        let response_budget_error = services
            .read_with_options(
                read_request("first.txt", None, tokenizer.count(&first_source)),
                ServiceCallOptions::new().with_max_response_tokens(1),
            )
            .await
            .expect_err("one token cannot fit a read response");
        let (minimum_response_tokens, _) = assert_response_budget_error(response_budget_error, 1);
        let minimum_response = services
            .read_with_options(
                read_request("first.txt", None, tokenizer.count(&first_source)),
                ServiceCallOptions::new().with_max_response_tokens(minimum_response_tokens),
            )
            .await
            .unwrap_or_else(|error| {
                panic!("reported response minimum must work for {tokenizer:?}: {error:?}")
            });
        assert!(minimum_response.meta.total_response_tokens <= minimum_response_tokens);
        assert!(
            minimum_response
                .content
                .as_deref()
                .is_some_and(|content| !content.is_empty()),
            "response fitting must not return an empty page for {tokenizer:?}"
        );

        let first_page = services
            .read(read_request("continuation.txt", None, continuation_budget))
            .await
            .expect("read prefix page");
        assert_eq!(
            first_page.content.as_deref(),
            Some(continuation_prefix),
            "fixture must stop before the multi-token scalar for {tokenizer:?}"
        );
        let cursor = first_page
            .continuation_cursor
            .expect("prefix page must have a continuation cursor");
        let continuation_error = services
            .read(read_request("continuation.txt", Some(cursor), 1))
            .await
            .expect_err("budget cannot return the continuation page's first UTF-8 scalar");
        assert_budget_error(&continuation_error, "continuation-page");
    }
}

#[tokio::test]
async fn bounded_open_continuation_preserves_the_unbounded_target() {
    let source = (1..=1_400)
        .map(|line| format!("line_{line:04} repeated words for a large bounded read\n"))
        .collect::<String>();
    let (_root, services) = indexed_source("open.txt", source.as_bytes()).await;

    let mut cursor = None;
    let mut reconstructed = String::new();
    let mut pages = 0usize;
    loop {
        let response = services
            .read(ReadRequest {
                path: "open.txt".into(),
                start_line: None,
                end_line: None,
                symbol: None,
                heading: None,
                heading_occurrence: None,
                continuation_cursor: cursor.take(),
                max_tokens: Some(256),
                expected_hash: None,
                delta: false,
                receipt_id: None,
                policy: leantoken::ReadPolicy::Bounded,
            })
            .await
            .expect("open bounded page");
        pages += 1;
        reconstructed.push_str(response.content.as_deref().expect("page content"));
        if !response.truncated {
            assert!(response.continuation_cursor.is_none());
            break;
        }
        cursor = response.continuation_cursor;
        assert!(cursor.is_some(), "truncated page must provide a cursor");
        assert!(pages < 100, "continuation cursor must make progress");
    }

    assert!(pages > 1, "fixture must require continuation pages");
    assert_eq!(reconstructed, source);
}

#[tokio::test]
async fn truncated_symbol_cursor_reconstructs_partial_lines_and_rejects_live_changes() {
    let long_line = format!(
        "    let payload = \"{}\";\n",
        "multibyte-\u{754c}".repeat(80)
    );
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
                policy: leantoken::ReadPolicy::default(),
            })
            .await
            .expect("read symbol page");
        pages += 1;
        assert_eq!(response.target_start_line, 1);
        assert_eq!(response.target_end_line, 4);
        assert_eq!(response.returned_start_line, response.returned_start_line);
        assert_eq!(response.returned_end_line, response.returned_end_line);
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
            policy: leantoken::ReadPolicy::default(),
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
            policy: leantoken::ReadPolicy::default(),
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
            policy: leantoken::ReadPolicy::default(),
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
            policy: leantoken::ReadPolicy::default(),
        })
        .await
        .expect("current first page");
    std::fs::write(
        root.path().join("large.rs"),
        source.replace("consume", "changed"),
    )
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
            policy: leantoken::ReadPolicy::default(),
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
            policy: leantoken::ReadPolicy::default(),
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
            policy: leantoken::ReadPolicy::default(),
        })
        .await
        .expect("qualified symbol");
    assert_eq!(
        (response.returned_start_line, response.returned_end_line),
        (6, 7)
    );
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
            policy: leantoken::ReadPolicy::default(),
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
            policy: leantoken::ReadPolicy::default(),
        })
        .await
        .expect("late symbol read");
    assert_eq!(read.returned_start_line, 130);
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
async fn bounded_read_stops_early_and_reports_unknown_index_state() {
    let source = b"line one\nline two\nline three\nline four\nline five\n";
    let (_root, services) = indexed_source("bounded.txt", source).await;

    let response = services
        .read(ReadRequest {
            path: "bounded.txt".into(),
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
            policy: leantoken::ReadPolicy::Bounded,
        })
        .await
        .expect("bounded read");

    assert_eq!(response.status, ReadStatus::Content);
    // Bounded reads do not hash the complete file, so index_state is unknown
    // and indexed_hash is absent.
    assert_eq!(response.index_state, leantoken::ReadIndexState::Unknown);
    assert!(response.indexed_hash.is_none());
    assert!(!response.index_stale);
    // Bounded reads stop after the requested page; bytes_read should be less
    // than the full file size.
    assert!(response.live_bytes_read < source.len());
    assert_eq!(response.content.as_deref(), Some("line one\n"));
}

#[tokio::test]
async fn full_read_hashes_complete_file_and_reports_index_state() {
    let source = b"line one\nline two\nline three\nline four\nline five\n";
    let (_root, services) = indexed_source("full.txt", source).await;

    let response = services
        .read(ReadRequest {
            path: "full.txt".into(),
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
            policy: leantoken::ReadPolicy::Full,
        })
        .await
        .expect("full read");

    assert_eq!(response.status, ReadStatus::Content);
    // Full reads hash the complete file and report index verification.
    assert_eq!(response.index_state, leantoken::ReadIndexState::Current);
    assert!(response.indexed_hash.is_some());
    assert!(!response.index_stale);
    // Full reads scan the complete file.
    assert_eq!(response.live_bytes_read, source.len());
    assert_eq!(response.content.as_deref(), Some("line one\n"));
}

#[tokio::test]
async fn full_read_reports_stale_index_state_when_live_file_diverges() {
    let source = b"line one\nline two\nline three\n";
    let (root, services) = indexed_source("stale.txt", source).await;

    std::fs::write(
        root.path().join("stale.txt"),
        b"line one changed\nline two\nline three\n",
    )
    .expect("edit live file");

    let response = services
        .read(ReadRequest {
            path: "stale.txt".into(),
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
            policy: leantoken::ReadPolicy::Full,
        })
        .await
        .expect("full read after edit");

    assert_eq!(response.status, ReadStatus::Content);
    assert_eq!(response.index_state, leantoken::ReadIndexState::Stale);
    assert!(response.index_stale);
    assert!(response.indexed_hash.is_some());
    assert_eq!(response.live_bytes_read, 37);
    assert_eq!(response.content.as_deref(), Some("line one changed\n"));
}

#[tokio::test]
async fn delta_request_without_full_policy_is_rejected() {
    let source = b"line one\nline two\n";
    let (_root, services) = indexed_source("delta_policy.txt", source).await;

    let error = services
        .read(ReadRequest {
            path: "delta_policy.txt".into(),
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
            policy: leantoken::ReadPolicy::Bounded,
        })
        .await
        .expect_err("delta with bounded policy must fail");

    assert!(matches!(
        error,
        Error::InvalidInput {
            field: "policy",
            ..
        }
    ));
}

#[tokio::test]
async fn bounded_continuation_cursor_rejects_full_policy_switch() {
    let source = b"line one\nline two\nline three\nline four\nline five\n";
    let (_root, services) = indexed_source("cursor_switch.txt", source).await;

    // First read with bounded policy and a tiny token limit to get a cursor.
    let first = services
        .read(ReadRequest {
            path: "cursor_switch.txt".into(),
            start_line: Some(1),
            end_line: Some(5),
            symbol: None,
            heading: None,
            heading_occurrence: None,
            continuation_cursor: None,
            max_tokens: Some(1),
            expected_hash: None,
            delta: false,
            receipt_id: None,
            policy: leantoken::ReadPolicy::Bounded,
        })
        .await
        .expect("bounded truncated read");
    assert!(first.truncated);
    let cursor = first
        .continuation_cursor
        .as_deref()
        .expect("bounded cursor");

    // Attempting to continue with Full policy must fail because the cursor
    // was issued under Bounded policy.
    let error = services
        .read(ReadRequest {
            path: "cursor_switch.txt".into(),
            start_line: None,
            end_line: None,
            symbol: None,
            heading: None,
            heading_occurrence: None,
            continuation_cursor: Some(cursor.to_string()),
            max_tokens: Some(100),
            expected_hash: None,
            delta: false,
            receipt_id: None,
            policy: leantoken::ReadPolicy::Full,
        })
        .await
        .expect_err("policy switch must fail");
    assert!(matches!(error, Error::StaleCursor));
}
