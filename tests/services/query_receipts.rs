use super::*;

fn exact_request(
    query: &str,
    include_paths: Vec<String>,
    exclude_paths: Vec<String>,
    action: QueryReceiptAction,
) -> SearchRequest {
    SearchRequest {
        query: query.into(),
        mode: SearchMode::Text,
        include_paths,
        exclude_paths,
        focus_paths: Vec::new(),
        max_results: Some(100),
        max_tokens: Some(10_000),
        context_lines: Some(0),
        case_sensitive: true,
        all_occurrences: true,
        prefer_structural: false,
        receipt_id: None,
        query_receipt: Some(action),
        cursor: None,
    }
}

#[tokio::test]
async fn complete_exact_query_receipt_skips_same_generation_rescan() {
    let (_root, services) = fixture().await;
    let recorded = services
        .search_occurrences(
            exact_request(
                "greet",
                Vec::new(),
                Vec::new(),
                QueryReceiptAction::Record,
            ),
            true,
        )
        .await
        .expect("record query receipt");
    let proof = recorded.query_receipt.expect("recorded proof");
    assert_eq!(proof.status, QueryReceiptStatus::Recorded);
    assert!(proof.complete);
    assert_eq!(proof.match_count, recorded.occurrences_total);
    assert_eq!(
        proof.scope_relation,
        QueryReceiptScopeRelation::Exact
    );
    let receipt_id = proof.receipt_id.expect("query receipt id");
    assert!(receipt_id.starts_with('q'));

    let reused = services
        .search_occurrences(
            exact_request(
                "greet",
                Vec::new(),
                Vec::new(),
                QueryReceiptAction::Reuse {
                    receipt_id: receipt_id.clone(),
                },
            ),
            true,
        )
        .await
        .expect("reuse query receipt");
    let reused_proof = reused.query_receipt.expect("reuse proof");
    assert_eq!(reused_proof.status, QueryReceiptStatus::AlreadyCovered);
    assert_eq!(reused_proof.receipt_id.as_deref(), Some(receipt_id.as_str()));
    assert_eq!(reused_proof.result_blake3, proof.result_blake3);
    assert_eq!(reused_proof.match_count, proof.match_count);
    assert!(!reused_proof.reused_across_generation);
    assert!(reused.groups.is_empty());
    assert_eq!(reused.occurrences_returned, 0);
    assert_eq!(reused.occurrences_total, proof.match_count);
    assert_eq!(reused.coverage.text_matches.total, proof.match_count);
    assert_eq!(reused.coverage.text_matches.returned, 0);
    assert_eq!(reused.coverage.text_matches.truncated, proof.match_count);
}

#[tokio::test]
async fn incomplete_response_and_pre_write_cancellation_never_persist_query_receipts() {
    let (root, services) = fixture().await;
    let database = root.path().join("index.sqlite");
    let mut paged = exact_request(
        "greet",
        Vec::new(),
        Vec::new(),
        QueryReceiptAction::Record,
    );
    paged.max_results = Some(1);
    let response = services
        .search_occurrences(paged, true)
        .await
        .expect("paged exhaustive response");
    let outcome = response.query_receipt.expect("incomplete outcome");
    assert_eq!(
        outcome.status,
        QueryReceiptStatus::NotRecordedIncompleteResponse
    );
    assert!(!outcome.complete);
    assert_eq!(outcome.receipt_id, None);
    assert_eq!(query_receipt_count(&database), 0);

    let cancellation = CancellationToken::new();
    cancellation.cancel();
    let error = services
        .search_occurrences_with_options_consistency_cancellable(
            exact_request(
                "greet",
                Vec::new(),
                Vec::new(),
                QueryReceiptAction::Record,
            ),
            true,
            IndexConsistency::IndexedGeneration,
            ServiceCallOptions::new(),
            cancellation,
        )
        .await
        .expect_err("cancel before execution");
    assert!(matches!(error, Error::Cancelled));
    assert_eq!(query_receipt_count(&database), 0);
}

#[tokio::test]
async fn response_budget_and_invalid_regex_fail_before_query_receipt_write() {
    let (root, services) = fixture().await;
    let database = root.path().join("index.sqlite");
    let request = exact_request(
        "greet",
        Vec::new(),
        Vec::new(),
        QueryReceiptAction::Record,
    );
    let error = services
        .search_occurrences_with_options(
            request.clone(),
            true,
            ServiceCallOptions::new().with_max_response_tokens(1),
        )
        .await
        .expect_err("undersized response budget");
    let Error::ResponseBudgetExceeded {
        minimum_required_response_tokens,
        ..
    } = error
    else {
        panic!("unexpected response error: {error:?}");
    };
    assert_eq!(query_receipt_count(&database), 0);
    let retry = services
        .search_occurrences_with_options(
            request,
            true,
            ServiceCallOptions::new()
                .with_max_response_tokens(minimum_required_response_tokens),
        )
        .await
        .expect("exact retry minimum");
    assert_eq!(
        retry.query_receipt.expect("recorded proof").status,
        QueryReceiptStatus::Recorded
    );
    assert_eq!(query_receipt_count(&database), 1);

    let mut invalid_regex = exact_request(
        "(",
        Vec::new(),
        Vec::new(),
        QueryReceiptAction::Record,
    );
    invalid_regex.mode = SearchMode::Regex;
    assert!(matches!(
        services.search_occurrences(invalid_regex, true).await,
        Err(Error::Regex(_))
    ));
    assert_eq!(query_receipt_count(&database), 1);
}

#[tokio::test]
async fn regex_receipts_are_exact_and_ranked_modes_are_rejected() {
    let (_root, services) = fixture().await;
    let mut regex = exact_request(
        "gr(?:ee)t",
        Vec::new(),
        Vec::new(),
        QueryReceiptAction::Record,
    );
    regex.mode = SearchMode::Regex;
    let recorded = services
        .search_occurrences(regex.clone(), true)
        .await
        .expect("record exhaustive regex");
    let receipt_id = recorded
        .query_receipt
        .expect("regex proof")
        .receipt_id
        .expect("regex receipt id");
    regex.query_receipt = Some(QueryReceiptAction::Reuse { receipt_id });
    let reused = services
        .search_occurrences(regex, true)
        .await
        .expect("reuse regex proof");
    assert_eq!(
        reused.query_receipt.expect("reuse proof").status,
        QueryReceiptStatus::AlreadyCovered
    );

    let mut ranked = exact_request(
        "greet",
        Vec::new(),
        Vec::new(),
        QueryReceiptAction::Record,
    );
    ranked.mode = SearchMode::Identifier;
    let error = services
        .search_occurrences(ranked, true)
        .await
        .expect_err("ranked identifier cannot issue coverage");
    assert!(matches!(
        error,
        Error::InvalidSearchOptions {
            field: "all_occurrences",
            ..
        } | Error::InvalidInput {
            field: "query_receipt",
            ..
        }
    ));

    let error = services
        .search(exact_request(
            "greet",
            Vec::new(),
            Vec::new(),
            QueryReceiptAction::Record,
        ))
        .await
        .expect_err("ranked response projection cannot issue coverage");
    assert!(matches!(
        error,
        Error::InvalidInput {
            field: "query_receipt",
            reason: "requires the occurrences projection"
        }
    ));
}

#[tokio::test]
async fn zero_match_superset_covers_subset_but_nonempty_results_do_not() {
    let (_root, services) = fixture().await;
    let absent = services
        .search_occurrences(
            exact_request(
                "definitely_absent",
                Vec::new(),
                Vec::new(),
                QueryReceiptAction::Record,
            ),
            true,
        )
        .await
        .expect("record absence");
    let absence_receipt = absent
        .query_receipt
        .expect("absence proof")
        .receipt_id
        .expect("absence receipt id");
    let subset = services
        .search_occurrences(
            exact_request(
                "definitely_absent",
                vec!["./src/".into()],
                Vec::new(),
                QueryReceiptAction::Reuse {
                    receipt_id: absence_receipt,
                },
            ),
            true,
        )
        .await
        .expect("reuse absence over subset");
    let subset_proof = subset.query_receipt.expect("subset proof");
    assert_eq!(
        subset_proof.scope_relation,
        QueryReceiptScopeRelation::Subset
    );
    assert_eq!(subset_proof.match_count, 0);

    let present = services
        .search_occurrences(
            exact_request(
                "greet",
                Vec::new(),
                Vec::new(),
                QueryReceiptAction::Record,
            ),
            true,
        )
        .await
        .expect("record present result");
    let present_receipt = present
        .query_receipt
        .expect("present proof")
        .receipt_id
        .expect("present receipt id");
    let error = services
        .search_occurrences(
            exact_request(
                "greet",
                vec!["src".into()],
                Vec::new(),
                QueryReceiptAction::Reuse {
                    receipt_id: present_receipt,
                },
            ),
            true,
        )
        .await
        .expect_err("nonempty superset cannot derive subset count");
    assert!(matches!(error, Error::QueryReceiptMismatch));
}

#[tokio::test]
async fn cross_generation_reuse_requires_unchanged_relevant_partition() {
    let (root, services) = fixture().await;
    let recorded = services
        .search_occurrences(
            exact_request(
                "definitely_absent",
                vec!["src".into()],
                Vec::new(),
                QueryReceiptAction::Record,
            ),
            true,
        )
        .await
        .expect("record scoped absence");
    let proof = recorded.query_receipt.expect("recorded proof");
    let receipt_id = proof.receipt_id.expect("receipt id");

    std::fs::write(root.path().join("notes.md"), "definitely_absent\n")
        .expect("write out-of-scope file");
    services.index(false).await.expect("publish unrelated file");
    let reused = services
        .search_occurrences(
            exact_request(
                "definitely_absent",
                vec!["./src/".into()],
                Vec::new(),
                QueryReceiptAction::Reuse {
                    receipt_id: receipt_id.clone(),
                },
            ),
            true,
        )
        .await
        .expect("reuse unchanged partition");
    let reused_proof = reused.query_receipt.expect("cross-generation proof");
    assert!(reused_proof.reused_across_generation);
    assert_eq!(
        reused_proof.scope_relation,
        QueryReceiptScopeRelation::Exact
    );

    std::fs::write(
        root.path().join("src/lib.rs"),
        "pub fn changed() { let _ = \"definitely_absent\"; }\n",
    )
    .expect("change relevant source");
    services.index(false).await.expect("publish relevant edit");
    let error = services
        .search_occurrences(
            exact_request(
                "definitely_absent",
                vec!["src".into()],
                Vec::new(),
                QueryReceiptAction::Reuse { receipt_id },
            ),
            true,
        )
        .await
        .expect_err("changed partition invalidates proof");
    assert!(matches!(error, Error::StaleQueryReceipt { .. }));
}

#[tokio::test]
async fn query_receipt_survives_restart_and_fails_loud_on_predicate_mismatch() {
    let root = tempfile::tempdir().expect("root");
    std::fs::write(root.path().join("lib.rs"), "fn needle() {}\n").expect("source");
    let database = root.path().join("index.sqlite");
    let config = Config::discover(root.path(), Some(database)).expect("config");
    let services = Services::open(config.clone()).expect("services");
    services.index(false).await.expect("index");
    let recorded = services
        .search_occurrences(
            exact_request(
                "needle",
                Vec::new(),
                Vec::new(),
                QueryReceiptAction::Record,
            ),
            true,
        )
        .await
        .expect("record");
    let receipt_id = recorded
        .query_receipt
        .expect("proof")
        .receipt_id
        .expect("receipt id");
    drop(services);

    let reopened = Services::open(config).expect("reopen");
    let reused = reopened
        .search_occurrences(
            exact_request(
                "needle",
                Vec::new(),
                Vec::new(),
                QueryReceiptAction::Reuse {
                    receipt_id: receipt_id.clone(),
                },
            ),
            true,
        )
        .await
        .expect("reuse after restart");
    assert_eq!(
        reused.query_receipt.expect("reuse proof").status,
        QueryReceiptStatus::AlreadyCovered
    );

    let error = reopened
        .search_occurrences(
            exact_request(
                "different",
                Vec::new(),
                Vec::new(),
                QueryReceiptAction::Reuse { receipt_id },
            ),
            true,
        )
        .await
        .expect_err("predicate mismatch");
    assert!(matches!(error, Error::QueryReceiptMismatch));
}

fn query_receipt_count(database: &std::path::Path) -> usize {
    let connection = rusqlite::Connection::open(database).expect("open database");
    let count: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM query_coverage_receipts",
            [],
            |row| row.get(0),
        )
        .expect("query receipt count");
    usize::try_from(count).expect("non-negative query receipt count")
}
