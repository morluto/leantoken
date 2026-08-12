use super::coverage::{MAX_PARSER_COVERAGE_GROUPS, parser_coverage_summary, safe_extension_family};
use super::read::AdaptiveExcerptRequest;
use super::savings::signed_token_difference;
use super::startup::{INITIAL_INDEX_IDLE_GRACE, INITIAL_INDEX_PROBE_INTERVAL};
use super::*;
use std::fs;
use std::sync::{Condvar, Mutex};
use tokio_util::sync::CancellationToken;

#[derive(Default)]
struct ScanGate {
    entered: Mutex<bool>,
    open: Mutex<bool>,
    changed: Condvar,
}

struct ScanGateRelease(Arc<ScanGate>);

impl Drop for ScanGateRelease {
    fn drop(&mut self) {
        self.0.open();
    }
}

impl ScanGate {
    fn wait(&self) {
        *self.entered.lock().expect("scan gate entered") = true;
        self.changed.notify_all();
        let mut open = self.open.lock().expect("scan gate open");
        while !*open {
            open = self.changed.wait(open).expect("scan gate wait");
        }
    }

    fn entered(&self) -> bool {
        *self.entered.lock().expect("scan gate entered")
    }

    fn open(&self) {
        *self.open.lock().expect("scan gate open") = true;
        self.changed.notify_all();
    }
}

async fn wait_until(predicate: impl Fn() -> bool) {
    for _ in 0..10_000 {
        if predicate() {
            return;
        }
        tokio::task::yield_now().await;
    }
    panic!("condition was not reached");
}

async fn wait_until_with_timer(predicate: impl Fn() -> bool) {
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            if predicate() {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(1)).await;
        }
    })
    .await
    .expect("condition was not reached before timeout");
}

async fn indexed_services() -> (tempfile::TempDir, Services) {
    let root = tempfile::tempdir().expect("root");
    fs::write(root.path().join("lib.rs"), "pub fn existing() {}\n").expect("source");
    let config =
        Config::discover(root.path(), Some(root.path().join("db.sqlite"))).expect("config");
    let services = Services::open(config).expect("services");
    services
        .refresh(IndexingMode::Reconcile)
        .await
        .expect("initial index");
    services.reconciliation.reset_diagnostics();
    (root, services)
}

#[tokio::test]
async fn poisoned_reconciliation_state_returns_a_typed_error() {
    let (_root, services) = indexed_services().await;
    services.reconciliation.poison_state_for_test();

    let result = services
        .reconciliation
        .reconcile(CancellationToken::new(), None)
        .await;

    assert!(matches!(
        result,
        Err(Error::OperationFailure(message))
            if message == "reconciliation coordinator state poisoned"
    ));
}

#[tokio::test]
async fn response_accounting_reaches_an_inclusive_fixed_point_across_digit_boundaries() {
    let (_root, services) = indexed_services().await;
    let mut digit_widths = Vec::new();

    for repository_id_bytes in [1, 400, 4_000] {
        let mut response = FilesResponse {
            entries: Vec::new(),
            meta: services.meta(1, 0, None),
        };
        response.meta.repository_id = "r".repeat(repository_id_bytes);
        services
            .finalize_response(&mut response)
            .expect("fixed-point accounting");

        let serialized = serde_json::to_string(&response).expect("serialize response");
        assert_eq!(
            services.config.tokenizer.count(&serialized),
            response.meta.total_response_tokens
        );
        digit_widths.push(response.meta.total_response_tokens.to_string().len());
    }

    digit_widths.dedup();
    assert!(
        digit_widths.len() >= 2,
        "fixture must cross at least one accounting digit boundary"
    );
}

#[tokio::test]
async fn mcp_wrapper_budget_rejects_before_receipt_and_savings_side_effects() {
    let (_root, services) = indexed_services().await;
    let request: WorktreeReadRequest = ReadRequest {
        path: "lib.rs".into(),
        start_line: Some(1),
        end_line: Some(1),
        symbol: None,
        heading: None,
        heading_occurrence: None,
        continuation_cursor: None,
        max_tokens: Some(100),
        expected_hash: None,
    }
    .into();
    let shape = crate::tokens::McpResponseShape {
        mode: crate::tokens::McpResponseMode::Structured,
        protocol: crate::tokens::McpProtocolShape::Modern,
    };
    let shaped_options = ServiceCallOptions::new().with_mcp_response_shape(shape);
    let successful = services
        .read_worktree_with_options(request.clone(), shaped_options)
        .await
        .expect("unbounded MCP-shaped read");
    let visible_tokens = services
        .response_accountant
        .finalized_tokens_with_receipt_resource(&successful, Some(shape))
        .expect("model-visible receipt accounting");
    assert_eq!(successful.meta.total_response_tokens, visible_tokens);
    let savings = services
        .token_savings_report()
        .await
        .expect("accounted savings");
    assert_eq!(
        savings.response_accounting.total_response_tokens,
        successful.meta.total_response_tokens as u64
    );

    let mut prototype = successful.clone();
    prototype.meta.receipt_id = None;
    let shaped_required = services
        .response_accountant
        .finalized_tokens_with_receipt_reserve(&prototype, 1, shaped_options)
        .expect("MCP receipt reserve");
    let plain_options = ServiceCallOptions::new();
    let plain_required = services
        .response_accountant
        .finalized_tokens_with_receipt_reserve(&prototype, 1, plain_options)
        .expect("service receipt reserve");
    let limit = shaped_required - 1;
    assert!(
        plain_required <= limit,
        "fixture must isolate the MCP wrapper from the service JSON"
    );

    let tracked_before = services
        .token_savings()
        .await
        .expect("savings before rejected call")
        .tracked_requests;
    let error = services
        .read_worktree_with_options(request, shaped_options.with_max_response_tokens(limit))
        .await
        .expect_err("MCP wrapper must be reserved before receipt persistence");
    match error {
        Error::ResponseBudgetExceeded {
            provided_max_response_tokens,
            minimum_required_response_tokens,
            ..
        } => {
            assert_eq!(provided_max_response_tokens, limit);
            assert!(minimum_required_response_tokens > limit);
        }
        error => panic!("unexpected response-budget error: {error:?}"),
    }
    let tracked_after = services
        .token_savings()
        .await
        .expect("savings after rejected call")
        .tracked_requests;
    assert_eq!(tracked_after, tracked_before);
}

#[tokio::test]
async fn concurrent_consistency_requests_share_one_waiting_wave() {
    let (_root, services) = indexed_services().await;
    let held_operation = services
        .coordination
        .acquire_operation(&CancellationToken::new())
        .expect("hold operation lock");

    let calls = (0..8)
        .map(|_| {
            let services = services.clone();
            tokio::spawn(async move {
                services
                    .apply_consistency(
                        IndexConsistency::ReconcileWorkingTree,
                        CancellationToken::new(),
                    )
                    .await
            })
        })
        .collect::<Vec<_>>();

    wait_until(|| services.reconciliation.diagnostics().requests == 8).await;
    let waiting = services.reconciliation.diagnostics();
    assert_eq!(waiting.waves_created, 1);
    assert_eq!(waiting.waves_started, 0);
    assert_eq!(waiting.coalesced_requests, 7);

    held_operation.release().expect("release operation lock");
    for call in calls {
        call.await.expect("join reconciliation").expect("reconcile");
    }

    let completed = services.reconciliation.diagnostics();
    assert_eq!(completed.requests, 8);
    assert_eq!(completed.waves_started, 1);
    assert_eq!(completed.waves_completed, 1);
    assert_eq!(completed.active_waves, 0);
}

#[test]
fn architecture_documents_runtime_retrieval_bounds() {
    let architecture = include_str!("../../docs/architecture.md");
    let expected_rows = [
        format!(
            "| Context query terms | {} (`MAX_CONTEXT_QUERIES`) |",
            context::MAX_CONTEXT_QUERIES
        ),
        format!(
            "| Context hits per term/source | {} symbols/refs, {} FTS |",
            context::MAX_CONTEXT_HITS_PER_SOURCE,
            context::MAX_CONTEXT_LEXICAL_HITS
        ),
        format!(
            "| Regex matching chunks | `min(max_results × 20, {})` |",
            search::MAX_REGEX_CANDIDATES
        ),
        format!(
            "| Trigram candidate chunks | {} |",
            search::MAX_REGEX_CANDIDATE_CHUNKS
        ),
        format!(
            "| Lightweight rows inspected for path-scoped trigram planning | {} |",
            search::MAX_SCOPED_REGEX_ROWS_SCANNED
        ),
        format!(
            "| Full-scan fallback files | {} |",
            search::MAX_REGEX_FILES_SCANNED
        ),
        format!(
            "| Full-scan fallback chunks per file | {} |",
            search::MAX_REGEX_CHUNKS_PER_FILE
        ),
        format!(
            "| File scan page size | {} for find (path projection) and globset fallback; tree/glob SQL-page `max_results + 1` projected paths |",
            files::FILE_LIST_PAGE_SIZE
        ),
    ];

    for row in expected_rows {
        assert!(
            architecture.contains(&row),
            "architecture bound drifted: {row}"
        );
    }
}

#[tokio::test]
async fn failed_wave_fans_out_one_error_without_retry_scans() {
    let (_root, services) = indexed_services().await;
    let held_operation = services
        .coordination
        .acquire_operation(&CancellationToken::new())
        .expect("hold operation lock");
    services
        .reconciliation
        .set_before_scan_hook(Some(Arc::new(|| panic!("injected shared failure"))));

    let calls = (0..8)
        .map(|_| {
            let services = services.clone();
            tokio::spawn(async move {
                services
                    .apply_consistency(
                        IndexConsistency::ReconcileWorkingTree,
                        CancellationToken::new(),
                    )
                    .await
            })
        })
        .collect::<Vec<_>>();
    wait_until(|| services.reconciliation.diagnostics().requests == 8).await;
    assert_eq!(services.reconciliation.diagnostics().waves_created, 1);
    held_operation.release().expect("release operation lock");

    let mut failures = Vec::new();
    for call in calls {
        let Err(Error::ReconciliationFailed(error)) = call.await.expect("join reconciliation")
        else {
            panic!("coalesced caller should receive the shared failure");
        };
        assert!(matches!(error.as_ref(), Error::Join(join) if join.is_panic()));
        failures.push(error);
    }
    assert!(
        failures
            .iter()
            .all(|failure| Arc::ptr_eq(failure, &failures[0]))
    );

    let diagnostics = services.reconciliation.diagnostics();
    assert_eq!(diagnostics.waves_created, 1);
    assert_eq!(diagnostics.waves_started, 1);
    assert_eq!(diagnostics.waves_failed, 1);
    assert_eq!(diagnostics.active_waves, 0);
    services.reconciliation.set_before_scan_hook(None);
}

#[tokio::test]
async fn reconciliation_waiter_admission_has_an_exact_boundary() {
    let (_root, services) = indexed_services().await;
    let held_operation = services
        .coordination
        .acquire_operation(&CancellationToken::new())
        .expect("hold operation lock");

    let calls = (0..reconciliation::default_reconciliation_active_capacity())
        .map(|_| {
            let services = services.clone();
            tokio::spawn(async move {
                services
                    .apply_consistency(
                        IndexConsistency::ReconcileWorkingTree,
                        CancellationToken::new(),
                    )
                    .await
            })
        })
        .collect::<Vec<_>>();
    wait_until(|| {
        services.reconciliation.diagnostics().requests
            == reconciliation::default_reconciliation_active_capacity() as u64
    })
    .await;

    assert!(matches!(
        services
            .apply_consistency(
                IndexConsistency::ReconcileWorkingTree,
                CancellationToken::new(),
            )
            .await,
        Err(Error::RetrievalOverloaded)
    ));
    assert_eq!(services.reconciliation.diagnostics().rejected_requests, 1);

    held_operation.release().expect("release operation lock");
    for call in calls {
        call.await.expect("join reconciliation").expect("reconcile");
    }
}

#[tokio::test]
async fn caller_after_scan_start_waits_for_the_next_wave() {
    let (root, services) = indexed_services().await;
    let gate = Arc::new(ScanGate::default());
    let _gate_release = ScanGateRelease(Arc::clone(&gate));
    let hook_gate = Arc::clone(&gate);
    services
        .reconciliation
        .set_before_scan_hook(Some(Arc::new(move || hook_gate.wait())));

    let first_services = services.clone();
    let first = tokio::spawn(async move {
        first_services
            .apply_consistency(
                IndexConsistency::ReconcileWorkingTree,
                CancellationToken::new(),
            )
            .await
    });
    wait_until_with_timer(|| gate.entered()).await;

    fs::write(
        root.path().join("later.rs"),
        "pub fn created_after_wave_started() {}\n",
    )
    .expect("later source");
    let second_services = services.clone();
    let second = tokio::spawn(async move {
        second_services
            .apply_consistency(
                IndexConsistency::ReconcileWorkingTree,
                CancellationToken::new(),
            )
            .await
    });
    wait_until_with_timer(|| services.reconciliation.diagnostics().pending_waiters == 1).await;
    gate.open();

    first.await.expect("join first").expect("first wave");
    second.await.expect("join second").expect("second wave");
    services.reconciliation.set_before_scan_hook(None);

    let diagnostics = services.reconciliation.diagnostics();
    assert_eq!(diagnostics.requests, 2);
    assert_eq!(diagnostics.waves_started, 2);
    assert_eq!(diagnostics.waves_completed, 2);
    let search = services
        .search(SearchRequest {
            query: "created_after_wave_started".into(),
            mode: SearchMode::Identifier,
            include_paths: Vec::new(),
            exclude_paths: Vec::new(),
            focus_paths: Vec::new(),
            max_results: Some(5),
            max_tokens: Some(100),
            context_lines: Some(1),
            case_sensitive: false,
            all_occurrences: false,
            prefer_structural: false,
            receipt_id: None,
            query_receipt: None,
            cursor: None,
        })
        .await
        .expect("search later source");
    assert!(search.hits.iter().any(|hit| hit.path == "later.rs"));
}

#[tokio::test]
async fn cancelling_the_only_pending_waiter_never_starts_its_wave() {
    let (_root, services) = indexed_services().await;
    let gate = Arc::new(ScanGate::default());
    let _gate_release = ScanGateRelease(Arc::clone(&gate));
    let hook_gate = Arc::clone(&gate);
    services
        .reconciliation
        .set_before_scan_hook(Some(Arc::new(move || hook_gate.wait())));

    let first_services = services.clone();
    let first = tokio::spawn(async move {
        first_services
            .apply_consistency(
                IndexConsistency::ReconcileWorkingTree,
                CancellationToken::new(),
            )
            .await
    });
    wait_until_with_timer(|| gate.entered()).await;

    let cancellation = CancellationToken::new();
    let second_services = services.clone();
    let second_cancellation = cancellation.clone();
    let second = tokio::spawn(async move {
        second_services
            .apply_consistency(IndexConsistency::ReconcileWorkingTree, second_cancellation)
            .await
    });
    wait_until_with_timer(|| services.reconciliation.diagnostics().pending_waiters == 1).await;
    cancellation.cancel();
    assert!(matches!(
        second.await.expect("join cancelled waiter"),
        Err(Error::Cancelled)
    ));

    gate.open();
    first.await.expect("join first").expect("first wave");
    services.reconciliation.set_before_scan_hook(None);
    let diagnostics = services.reconciliation.diagnostics();
    assert_eq!(diagnostics.waves_started, 1);
    assert_eq!(diagnostics.waves_completed, 1);
    assert_eq!(diagnostics.waves_cancelled_before_start, 1);
    assert_eq!(diagnostics.cancelled_waiters, 1);
}

#[tokio::test]
async fn caller_after_a_cancelled_waiting_wave_uses_a_fresh_wave() {
    let (_root, services) = indexed_services().await;
    let held_operation = services
        .coordination
        .acquire_operation(&CancellationToken::new())
        .expect("hold operation lock");

    let cancellation = CancellationToken::new();
    let first_services = services.clone();
    let first_cancellation = cancellation.clone();
    let first = tokio::spawn(async move {
        first_services
            .apply_consistency(IndexConsistency::ReconcileWorkingTree, first_cancellation)
            .await
    });
    wait_until(|| services.reconciliation.diagnostics().requests == 1).await;
    cancellation.cancel();
    assert!(matches!(
        first.await.expect("join cancelled first waiter"),
        Err(Error::Cancelled)
    ));

    let second_services = services.clone();
    let second = tokio::spawn(async move {
        second_services
            .apply_consistency(
                IndexConsistency::ReconcileWorkingTree,
                CancellationToken::new(),
            )
            .await
    });
    wait_until(|| {
        let diagnostics = services.reconciliation.diagnostics();
        diagnostics.requests == 2 && diagnostics.waves_created == 2
    })
    .await;
    held_operation.release().expect("release operation lock");

    second.await.expect("join second").expect("fresh wave");
    let diagnostics = services.reconciliation.diagnostics();
    assert_eq!(diagnostics.waves_created, 2);
    assert_eq!(diagnostics.waves_started, 1);
    assert_eq!(diagnostics.waves_completed, 1);
    assert_eq!(diagnostics.waves_cancelled_before_start, 1);
}

#[tokio::test]
async fn committed_generation_reconciliation_keeps_waiting_past_cold_deadline() {
    let (_root, services) = indexed_services().await;
    let held_operation = services
        .coordination
        .acquire_operation(&CancellationToken::new())
        .expect("hold operation lock");
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(30);
    let call_services = services.clone();
    let call = tokio::spawn(async move {
        call_services
            .apply_consistency_with_initial_deadline(
                IndexConsistency::ReconcileWorkingTree,
                CancellationToken::new(),
                Some(deadline),
            )
            .await
    });
    wait_until_with_timer(|| {
        let diagnostics = services.reconciliation.diagnostics();
        diagnostics.requests == 1 && diagnostics.active_waves == 1
    })
    .await;

    tokio::time::pause();
    tokio::time::advance(std::time::Duration::from_secs(60)).await;
    tokio::task::yield_now().await;
    assert!(!call.is_finished());

    held_operation.release().expect("release operation lock");
    call.await
        .expect("join warm reconciliation")
        .expect("warm reconciliation");
    let diagnostics = services.reconciliation.diagnostics();
    assert_eq!(diagnostics.timed_out_waiters, 0);
}

#[tokio::test]
async fn cancellation_precedes_initial_reconciliation_deadline_and_removes_waiter() {
    let root = tempfile::tempdir().expect("root");
    let config =
        Config::discover(root.path(), Some(root.path().join("db.sqlite"))).expect("config");
    let services = Services::open(config).expect("services");
    let held_operation = services
        .coordination
        .acquire_operation(&CancellationToken::new())
        .expect("hold operation lock");
    let cancellation = CancellationToken::new();
    let call_cancellation = cancellation.clone();
    let call_services = services.clone();
    let call = tokio::spawn(async move {
        call_services
            .apply_consistency_with_initial_deadline(
                IndexConsistency::ReconcileWorkingTree,
                call_cancellation,
                Some(tokio::time::Instant::now() + std::time::Duration::from_secs(3_600)),
            )
            .await
    });
    wait_until_with_timer(|| {
        let diagnostics = services.reconciliation.diagnostics();
        diagnostics.requests == 1 && diagnostics.active_waves == 1
    })
    .await;

    cancellation.cancel();
    assert!(matches!(
        call.await.expect("join cancelled reconciliation"),
        Err(Error::Cancelled)
    ));
    held_operation.release().expect("release operation lock");
    wait_until_with_timer(|| services.reconciliation.diagnostics().active_waves == 0).await;
    let diagnostics = services.reconciliation.diagnostics();
    assert_eq!(diagnostics.pending_waiters, 0);
    assert_eq!(diagnostics.cancelled_waiters, 1);
    assert_eq!(diagnostics.timed_out_waiters, 0);
}

#[tokio::test]
async fn aborting_a_running_waiter_does_not_cancel_its_wave() {
    let (_root, services) = indexed_services().await;
    let gate = Arc::new(ScanGate::default());
    let _gate_release = ScanGateRelease(Arc::clone(&gate));
    let hook_gate = Arc::clone(&gate);
    services
        .reconciliation
        .set_before_scan_hook(Some(Arc::new(move || hook_gate.wait())));

    let first_services = services.clone();
    let first = tokio::spawn(async move {
        first_services
            .apply_consistency(
                IndexConsistency::ReconcileWorkingTree,
                CancellationToken::new(),
            )
            .await
    });
    wait_until_with_timer(|| gate.entered()).await;
    first.abort();
    assert!(first.await.expect_err("aborted waiter").is_cancelled());

    let second_services = services.clone();
    let second = tokio::spawn(async move {
        second_services
            .apply_consistency(
                IndexConsistency::ReconcileWorkingTree,
                CancellationToken::new(),
            )
            .await
    });
    wait_until_with_timer(|| services.reconciliation.diagnostics().pending_waiters == 1).await;
    gate.open();

    second.await.expect("join second").expect("second wave");
    services.reconciliation.set_before_scan_hook(None);
    let diagnostics = services.reconciliation.diagnostics();
    assert_eq!(diagnostics.waves_started, 2);
    assert_eq!(diagnostics.waves_completed, 2);
    assert_eq!(diagnostics.cancelled_waiters, 1);
}

#[tokio::test]
async fn reconciliation_panic_releases_wave_state_and_keeps_services_usable() {
    let (_root, services) = indexed_services().await;
    services
        .reconciliation
        .set_before_scan_hook(Some(Arc::new(|| panic!("injected reconciliation panic"))));

    assert!(matches!(
        services
            .apply_consistency(
                IndexConsistency::ReconcileWorkingTree,
                CancellationToken::new(),
            )
            .await,
        Err(Error::ReconciliationFailed(error))
            if matches!(error.as_ref(), Error::Join(join) if join.is_panic())
    ));
    let failed = services.reconciliation.diagnostics();
    assert_eq!(failed.waves_started, 1);
    assert_eq!(failed.waves_failed, 1);
    assert_eq!(failed.active_waves, 0);

    services.reconciliation.set_before_scan_hook(None);
    services
        .apply_consistency(
            IndexConsistency::ReconcileWorkingTree,
            CancellationToken::new(),
        )
        .await
        .expect("later reconciliation");
    let recovered = services.reconciliation.diagnostics();
    assert_eq!(recovered.waves_started, 2);
    assert_eq!(recovered.waves_completed, 1);
    assert_eq!(recovered.active_waves, 0);
}

#[test]
fn signed_token_difference_preserves_cost_and_saturates_public_range() {
    assert_eq!(signed_token_difference(10, 3), 7);
    assert_eq!(signed_token_difference(3, 10), -7);
    assert_eq!(signed_token_difference(u64::MAX, 0), i64::MAX);
    assert_eq!(signed_token_difference(0, u64::MAX), i64::MIN);
}

#[tokio::test]
async fn initial_index_wait_returns_after_publication_lock_releases() {
    let root = tempfile::tempdir().expect("root");
    let config =
        Config::discover(root.path(), Some(root.path().join("db.sqlite"))).expect("config");
    let services = Services::open(config).expect("services");
    let operation = services
        .coordination
        .acquire_operation(&CancellationToken::new())
        .expect("operation lock");
    let publisher_services = services.clone();
    let (published_tx, published_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let publisher = tokio::task::spawn_blocking(move || {
        publisher_services
            .storage
            .full_reconcile("published", Vec::new())
            .expect("publish generation");
        published_tx.send(()).expect("announce publication");
        release_rx.recv().expect("release permission");
        operation.release().expect("release operation lock");
    });
    published_rx.await.expect("publication");

    let waiting_services = services.clone();
    let waiting = tokio::spawn(async move {
        waiting_services
            .wait_for_initial_index_cancellable(CancellationToken::new())
            .await
    });
    tokio::task::yield_now().await;
    assert!(
        !waiting.is_finished(),
        "publication is not settled until the operation lock releases"
    );

    release_tx.send(()).expect("allow release");
    publisher.await.expect("join publisher");
    waiting
        .await
        .expect("join initial index wait")
        .expect("settled generation");
    let status = services.status().await.expect("status");
    assert_eq!(status.repository_generation, 1);
    assert_eq!(status.freshness, Freshness::Current);
}

#[tokio::test]
async fn initial_index_wait_honors_cancellation_before_publication() {
    let root = tempfile::tempdir().expect("root");
    let config =
        Config::discover(root.path(), Some(root.path().join("db.sqlite"))).expect("config");
    let services = Services::open(config).expect("services");
    let cancellation = CancellationToken::new();
    let waiting_cancellation = cancellation.clone();
    let waiting = tokio::spawn(async move {
        services
            .wait_for_initial_index_cancellable(waiting_cancellation)
            .await
    });
    tokio::task::yield_now().await;
    assert!(!waiting.is_finished());

    cancellation.cancel();
    let error = waiting
        .await
        .expect("join initial index wait")
        .expect_err("generation-zero wait must cancel");
    assert!(matches!(error, Error::Cancelled));
}

#[tokio::test(start_paused = true)]
async fn initial_index_wait_bounds_generation_zero_without_an_owner() {
    let root = tempfile::tempdir().expect("root");
    let config =
        Config::discover(root.path(), Some(root.path().join("db.sqlite"))).expect("config");
    let services = Services::open(config).expect("services");

    let result = tokio::time::timeout(
        INITIAL_INDEX_IDLE_GRACE + INITIAL_INDEX_PROBE_INTERVAL,
        services.wait_for_initial_index_cancellable(CancellationToken::new()),
    )
    .await
    .expect("idle generation-zero wait must be bounded")
    .expect_err("idle generation zero remains unready");
    assert!(matches!(result, Error::IndexNotReady));
}

#[tokio::test]
async fn index_search_read_and_hash_delta() {
    let root = tempfile::tempdir().expect("root");
    fs::write(
        root.path().join("lib.rs"),
        "pub fn handle_request() { helper(); }\nfn helper() {}\n",
    )
    .expect("source");
    let config =
        Config::discover(root.path(), Some(root.path().join("db.sqlite"))).expect("config");
    let services = Services::open(config).expect("services");
    services
        .refresh(IndexingMode::Reconcile)
        .await
        .expect("index");

    let search = services
        .search(SearchRequest {
            query: "handle_request".into(),
            mode: SearchMode::Auto,
            include_paths: Vec::new(),
            exclude_paths: Vec::new(),
            focus_paths: Vec::new(),
            max_results: Some(5),
            max_tokens: Some(100),
            context_lines: Some(1),
            case_sensitive: false,
            all_occurrences: false,
            prefer_structural: false,
            receipt_id: None,
            query_receipt: None,
            cursor: None,
        })
        .await
        .expect("search");
    assert!(!search.hits.is_empty());
    assert!(search.meta.source_tokens <= 100);

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
        })
        .await
        .expect("read");
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
            expected_hash: Some(first.content_hash),
        })
        .await
        .expect("read delta");
    assert_eq!(second.status, ReadStatus::NotModified);
    assert!(second.content.is_none());
    assert_eq!(second.meta.source_tokens, 0);
}

#[tokio::test]
async fn adaptive_context_ranges_keep_the_match_and_complete_small_declarations() {
    let root = tempfile::tempdir().expect("root");
    let mut source = String::from("fn large() {\n");
    for index in 0..180 {
        source.push_str(&format!("    let value_{index} = {index};\n"));
    }
    source.push_str("}\n\nfn small() { answer(); }\n");
    fs::write(root.path().join("lib.rs"), source).expect("source");
    let config =
        Config::discover(root.path(), Some(root.path().join("db.sqlite"))).expect("config");
    let services = Services::open(config).expect("services");
    services
        .refresh(IndexingMode::Reconcile)
        .await
        .expect("index");
    let file = services
        .storage
        .find_file("lib.rs")
        .expect("find file")
        .expect("indexed file");
    let session = services
        .storage
        .begin_generation_read()
        .expect("read session");
    let crate::symbol_identity::SymbolResolution::Unique(large) =
        session.find_symbol(file.id, "large").expect("find symbol")
    else {
        panic!("large symbol must resolve uniquely");
    };
    let matched_line = 151;
    let enclosing = session
        .find_enclosing_symbols_batch(&[(file.id, matched_line)])
        .expect("find enclosing symbol")
        .into_iter()
        .next()
        .expect("one enclosing lookup")
        .expect("enclosing symbol");
    assert_eq!(enclosing.name, "large");

    let session = crate::services::index_read::RepositoryGeneration::open(&services.storage)
        .expect("read snapshot");
    let bounded = services
        .adaptive_context_excerpts(
            &session,
            &[AdaptiveExcerptRequest {
                file_id: file.id,
                declaration_start: large.start_line,
                declaration_end: large.end_line,
                matched_line,
                token_budget: 60,
            }],
        )
        .expect("bounded excerpt")
        .into_iter()
        .next()
        .expect("one bounded request")
        .expect("bounded declaration");
    assert!(bounded.start_line <= matched_line);
    assert!(bounded.end_line >= matched_line);
    assert!(bounded.start_line > large.start_line);
    assert!(bounded.end_line <= large.end_line);

    let crate::symbol_identity::SymbolResolution::Unique(small) =
        session.find_symbol(file.id, "small").expect("find symbol")
    else {
        panic!("small symbol must resolve uniquely");
    };
    let complete = services
        .adaptive_context_excerpts(
            &session,
            &[AdaptiveExcerptRequest {
                file_id: file.id,
                declaration_start: small.start_line,
                declaration_end: small.end_line,
                matched_line: small.start_line,
                token_budget: 1_000,
            }],
        )
        .expect("complete excerpt")
        .into_iter()
        .next()
        .expect("one complete request")
        .expect("complete declaration");
    assert_eq!(complete.start_line, small.start_line);
    assert_eq!(complete.end_line, small.end_line);
}

#[tokio::test]
async fn search_cursor_defers_candidates_that_do_not_fit_the_current_token_page() {
    let root = tempfile::tempdir().expect("root");
    for name in ["a.rs", "b.rs", "c.rs"] {
        fs::write(
            root.path().join(name),
            "const NEEDLE: &str = \"needle with an excerpt too large for one token\";\n",
        )
        .expect("source");
    }
    let config =
        Config::discover(root.path(), Some(root.path().join("db.sqlite"))).expect("config");
    let services = Services::open(config).expect("services");
    services
        .refresh(IndexingMode::Reconcile)
        .await
        .expect("index");

    let mut request = SearchRequest {
        query: "needle".into(),
        mode: SearchMode::Text,
        include_paths: Vec::new(),
        exclude_paths: Vec::new(),
        focus_paths: Vec::new(),
        max_results: Some(2),
        max_tokens: Some(1_000),
        context_lines: Some(0),
        case_sensitive: false,
        all_occurrences: false,
        prefer_structural: false,
        receipt_id: None,
        query_receipt: None,
        cursor: None,
    };
    let unbounded = services
        .search(request.clone())
        .await
        .expect("unbounded search");
    let one_hit_tokens = services
        .config()
        .tokenizer
        .count(&unbounded.hits[0].excerpt);
    request.max_tokens = Some(one_hit_tokens);

    let first_page = services.search(request.clone()).await.expect("first page");
    assert_eq!(
        first_page
            .hits
            .iter()
            .map(|hit| hit.path.as_str())
            .collect::<Vec<_>>(),
        vec!["a.rs"]
    );
    let second_cursor = first_page
        .meta
        .next_cursor
        .expect("second candidate must remain on a later page");

    let second_page = services
        .search(SearchRequest {
            cursor: Some(second_cursor),
            ..request.clone()
        })
        .await
        .expect("second page");
    assert_eq!(
        second_page
            .hits
            .iter()
            .map(|hit| hit.path.as_str())
            .collect::<Vec<_>>(),
        vec!["b.rs"]
    );
    let third_cursor = second_page
        .meta
        .next_cursor
        .expect("third candidate must remain on a later page");

    let final_page = services
        .search(SearchRequest {
            cursor: Some(third_cursor),
            ..request
        })
        .await
        .expect("final page");
    assert_eq!(
        final_page
            .hits
            .iter()
            .map(|hit| hit.path.as_str())
            .collect::<Vec<_>>(),
        vec!["c.rs"]
    );
    assert!(final_page.meta.next_cursor.is_none());
}

#[tokio::test]
async fn cancellable_service_stops_before_blocking_work() {
    let root = tempfile::tempdir().expect("root");
    fs::write(root.path().join("lib.rs"), "fn answer() -> u8 { 42 }\n").expect("source");
    let config =
        Config::discover(root.path(), Some(root.path().join("db.sqlite"))).expect("config");
    let services = Services::open(config).expect("services");
    services
        .refresh(IndexingMode::Reconcile)
        .await
        .expect("index");

    let cancellation = CancellationToken::new();
    cancellation.cancel();
    let error = services
        .files_cancellable(
            FilesRequest {
                operation: FileOperation::Tree,
                path: None,
                query: None,
                pattern: None,
                max_results: Some(10),
                cursor: None,
                depth: Some(2),
            },
            cancellation,
        )
        .await
        .expect_err("pre-cancelled request should stop");
    assert!(matches!(error, Error::Cancelled));
}

#[tokio::test]
async fn files_find_rejects_whitespace_only_queries() {
    let (_root, services) = indexed_services().await;

    let error = services
        .files(FilesRequest {
            operation: FileOperation::Find,
            path: None,
            query: Some("   ".into()),
            pattern: None,
            max_results: Some(10),
            cursor: None,
            depth: None,
        })
        .await
        .expect_err("whitespace-only query must be rejected");

    assert!(matches!(
        error,
        Error::InvalidInput {
            field: "query",
            reason: "is required for find"
        }
    ));
}

#[tokio::test]
async fn token_savings_rejects_work_when_blocking_capacity_is_saturated() {
    let root = tempfile::tempdir().expect("root");
    let config =
        Config::discover(root.path(), Some(root.path().join("db.sqlite"))).expect("config");
    let mut services = Services::open(config).expect("services");
    services.blocking_executor = executor::BlockingExecutor::new(1, 1, Duration::from_secs(30));

    let gate = Arc::new((std::sync::Mutex::new(false), std::sync::Condvar::new()));
    let started = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let blocker = {
        let executor = services.blocking_executor.clone();
        let gate = Arc::clone(&gate);
        let started = Arc::clone(&started);
        tokio::spawn(async move {
            executor
                .run(CancellationToken::new(), move |_| {
                    started.store(true, Ordering::SeqCst);
                    let (open, changed) = &*gate;
                    let mut open = open.lock().expect("gate lock");
                    while !*open {
                        open = changed.wait(open).expect("gate wait");
                    }
                    Ok(())
                })
                .await
        })
    };
    while !started.load(Ordering::SeqCst) {
        tokio::task::yield_now().await;
    }

    assert!(matches!(
        services.token_savings().await,
        Err(Error::RetrievalOverloaded)
    ));

    let (open, changed) = &*gate;
    *open.lock().expect("gate lock") = true;
    changed.notify_all();
    blocker
        .await
        .expect("blocker task")
        .expect("blocker result");
}

#[test]
fn request_snapshot_ignores_concurrent_generation_publish() {
    let root = tempfile::tempdir().expect("root");
    let config =
        Config::discover(root.path(), Some(root.path().join("db.sqlite"))).expect("config");
    let services = Services::open(config).expect("services");
    let first = services
        .storage
        .full_reconcile("hash-a", Vec::new())
        .expect("initial generation");
    assert_eq!(first, 1);

    // One snapshot assembly must report the generation pinned at open, even
    // if a concurrent publish advances the committed generation mid-request.
    let observed = services
        .consistent(|session| {
            let generation = session.generation();
            assert_eq!(generation, first);
            assert_eq!(session.generation(), first);
            services
                .storage
                .full_reconcile("hash-b", Vec::new())
                .expect("concurrent publish");
            assert_eq!(
                session.generation(),
                first,
                "DEFERRED snapshot must not observe the concurrent publish"
            );
            Ok(generation)
        })
        .expect("snapshot assembly");
    assert_eq!(observed, first);
    assert_eq!(
        services
            .storage
            .repository_generation()
            .expect("latest generation"),
        first + 1
    );
}

#[test]
fn pinned_snapshot_operation_errors_are_not_retried() {
    use std::cell::Cell;

    let root = tempfile::tempdir().expect("root");
    let config =
        Config::discover(root.path(), Some(root.path().join("db.sqlite"))).expect("config");
    let services = Services::open(config).expect("services");
    services
        .storage
        .full_reconcile("hash-a", Vec::new())
        .expect("initial generation");
    let calls = Cell::new(0);

    let error = services
        .consistent(|_| {
            calls.set(calls.get() + 1);
            Err::<(), _>(Error::Io(std::io::Error::other("live read failed")))
        })
        .expect_err("operation error");

    assert!(matches!(error, Error::Io(_)));
    assert_eq!(calls.get(), 1);
}

#[tokio::test]
async fn regex_retained_chunk_overflow_is_not_reported_as_complete() {
    use crate::storage::{ChunkInput, IndexedFile};

    let root = tempfile::tempdir().expect("root");
    let config =
        Config::discover(root.path(), Some(root.path().join("db.sqlite"))).expect("config");
    let services = Services::open(config).expect("services");
    let files = (0..=2_000)
        .map(|index| IndexedFile {
            path: format!("file_{index:04}.rs"),
            language: Some("rust".into()),
            structurally_complete: true,
            size_bytes: 6,
            modified_ns: None,
            content_hash: format!("hash-{index}"),
            chunks: vec![ChunkInput {
                content: "needle".into(),
                start_line: 1,
                end_line: 1,
                start_byte: 0,
                end_byte: 6,
                token_count: 1,
            }],
            symbols: Vec::new(),
            references: Vec::new(),
            imports: Vec::new(),
        })
        .collect();
    services
        .storage
        .full_reconcile("hash-a", files)
        .expect("indexed fixture");

    let error = services
        .search(SearchRequest {
            query: "needle".into(),
            mode: SearchMode::Regex,
            include_paths: Vec::new(),
            exclude_paths: Vec::new(),
            focus_paths: Vec::new(),
            max_results: Some(100),
            max_tokens: Some(10_000),
            context_lines: Some(0),
            case_sensitive: true,
            all_occurrences: false,
            prefer_structural: false,
            receipt_id: None,
            query_receipt: None,
            cursor: None,
        })
        .await
        .expect_err("candidate overflow must be explicit");

    assert!(
        matches!(
            error,
            Error::RetrievalLimitExceeded {
                kind: crate::RetrievalLimitKind::RegexRetainedChunks,
                observed: 2_001,
                limit: 2_000,
            }
        ),
        "unexpected candidate overflow: {error:?}"
    );
}

#[tokio::test]
async fn regex_candidate_chunk_overflow_reports_the_fts_bound() {
    use crate::storage::{ChunkInput, IndexedFile};

    let root = tempfile::tempdir().expect("root");
    let config =
        Config::discover(root.path(), Some(root.path().join("db.sqlite"))).expect("config");
    let services = Services::open(config).expect("services");
    let chunks = (0..=10_000)
        .map(|index| ChunkInput {
            content: "needle".into(),
            start_line: index + 1,
            end_line: index + 1,
            start_byte: index * 6,
            end_byte: (index + 1) * 6,
            token_count: 1,
        })
        .collect();
    services
        .storage
        .full_reconcile(
            "hash-a",
            vec![IndexedFile {
                path: "large.rs".into(),
                language: Some("rust".into()),
                structurally_complete: true,
                size_bytes: 60_006,
                modified_ns: None,
                content_hash: "hash-large".into(),
                chunks,
                symbols: Vec::new(),
                references: Vec::new(),
                imports: Vec::new(),
            }],
        )
        .expect("indexed fixture");

    let error = services
        .search(SearchRequest {
            query: "needle".into(),
            mode: SearchMode::Regex,
            include_paths: Vec::new(),
            exclude_paths: Vec::new(),
            focus_paths: Vec::new(),
            max_results: Some(100),
            max_tokens: Some(10_000),
            context_lines: Some(0),
            case_sensitive: true,
            all_occurrences: false,
            prefer_structural: false,
            receipt_id: None,
            query_receipt: None,
            cursor: None,
        })
        .await
        .expect_err("candidate overflow must be explicit");

    assert!(matches!(
        error,
        Error::RetrievalLimitExceeded {
            kind: crate::RetrievalLimitKind::RegexCandidateChunks,
            observed: 10_001,
            limit: 10_000,
        }
    ));
}

#[test]
fn parser_coverage_bounds_groups_and_sanitizes_extension_labels() {
    let mut rows = ParserCoverageRows::default();
    for index in 0..=MAX_PARSER_COVERAGE_GROUPS {
        rows.unrecognized_extensions
            .push(crate::storage::UnrecognizedExtensionCoverageRow {
                extension: format!(".ext{index:02}"),
                files: 1,
                source_bytes: u64::try_from(index + 1).expect("fixture bytes"),
            });
    }
    rows.unrecognized_extensions
        .push(crate::storage::UnrecognizedExtensionCoverageRow {
            extension: "[no_extension]".into(),
            files: 1,
            source_bytes: 2,
        });
    rows.unrecognized_extensions
        .push(crate::storage::UnrecognizedExtensionCoverageRow {
            extension: "[other_extension]".into(),
            files: 1,
            source_bytes: 3,
        });

    let summary = parser_coverage_summary(rows);

    assert_eq!(
        summary.unrecognized_extensions.len(),
        MAX_PARSER_COVERAGE_GROUPS
    );
    assert_eq!(summary.indexed.files, MAX_PARSER_COVERAGE_GROUPS + 3);
    assert_eq!(summary.unrecognized, summary.indexed);
    assert_eq!(summary.other_unrecognized_extensions.files, 3);
    assert_eq!(safe_extension_family("source.RS"), ".rs");
    assert_eq!(safe_extension_family("Makefile"), "[no_extension]");
    assert_eq!(
        safe_extension_family("data.unsupported$label"),
        "[other_extension]"
    );
}
