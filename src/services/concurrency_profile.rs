//! Opt-in release-mode concurrency profile for the process-local retrieval governor.

use std::collections::HashMap;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant};

use ignore::WalkBuilder;
use rusqlite::Connection;
use serde::Serialize;
use serde_json::Value;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

use super::executor::{
    DEFAULT_BLOCKING_QUEUE_TIMEOUT, default_blocking_active_capacity,
    default_blocking_execution_capacity,
};
use super::*;

const CONCURRENCY_LEVELS: [usize; 6] = [1, 2, 4, 8, 16, 32];
const LARGE_REPOSITORY_ENV: &str = "LEANTOKEN_CONCURRENCY_PROFILE_LARGE_REPOSITORY";
const LARGE_REVISION_ENV: &str = "LEANTOKEN_CONCURRENCY_PROFILE_LARGE_REVISION";
const OUTPUT_ENV: &str = "LEANTOKEN_CONCURRENCY_PROFILE_OUTPUT";

#[derive(Debug, Clone, Copy, Hash, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum Workload {
    Files,
    Search,
    Read,
    Context,
    Status,
    Savings,
}

const WORKLOADS: [Workload; 6] = [
    Workload::Files,
    Workload::Search,
    Workload::Read,
    Workload::Context,
    Workload::Status,
    Workload::Savings,
];

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
enum Scenario {
    Mixed,
    CancellationStorm,
    ConcurrentIndexing,
}

#[derive(Debug, Serialize)]
struct ProfileReport {
    schema_version: u32,
    generated_at_unix_seconds: u64,
    leantoken_revision: String,
    rustc: String,
    execution_capacity: usize,
    active_capacity: usize,
    queue_timeout_ms: u128,
    concurrency_levels: Vec<usize>,
    repositories: Vec<RepositoryReport>,
    phase_five_decision: PhaseFiveDecision,
}

#[derive(Debug, Serialize)]
struct RepositoryReport {
    name: String,
    path: String,
    revision: String,
    indexed_files: usize,
    index_milliseconds: u128,
    database_bytes: u64,
    scenarios: Vec<ScenarioReport>,
    reconciliation_wave: ReconciliationWaveReport,
}

#[derive(Debug, Serialize)]
struct ReconciliationWaveReport {
    requests: usize,
    elapsed_micros: u64,
    errors: u64,
    rejected_requests: u64,
    waves_created: u64,
    waves_started: u64,
    waves_completed: u64,
    waves_failed: u64,
    coalesced_requests: u64,
    peak_active_waves: usize,
    peak_pending_waiters: usize,
}

#[derive(Debug, Serialize)]
struct ScenarioReport {
    scenario: Scenario,
    concurrency: usize,
    requests: usize,
    complete_request_micros: Percentiles,
    queue_wait_micros: Percentiles,
    reader_checkout_wait_micros: Percentiles,
    accepted: u64,
    rejected: u64,
    timed_out: u64,
    cancelled: u64,
    errors: u64,
    succeeded: u64,
    submitted_blocking_closures: u64,
    started_blocking_closures: u64,
    finished_blocking_closures: u64,
    peak_active_blocking_requests: usize,
    peak_running_blocking_closures: usize,
    distinct_blocking_threads: usize,
    peak_active_sqlite_snapshots: usize,
    active_sqlite_snapshots_after: usize,
    cpu_milliseconds: Option<u64>,
    peak_rss_bytes: Option<u64>,
    wal_bytes_before: u64,
    wal_bytes_peak: u64,
    wal_bytes_after: u64,
    checkpoint: CheckpointReport,
    parity_checked: u64,
    parity_mismatches: u64,
    order_mismatches: u64,
    generation_mismatches: u64,
    token_accounting_mismatches: u64,
    indexing_milliseconds: Option<u128>,
    indexing_error: Option<String>,
}

#[derive(Debug, Default, Serialize)]
struct Percentiles {
    samples: usize,
    p50: Option<u64>,
    p95: Option<u64>,
    max: Option<u64>,
}

#[derive(Debug, Default, Serialize)]
struct CheckpointReport {
    busy: Option<i64>,
    log_frames: Option<i64>,
    checkpointed_frames: Option<i64>,
    error: Option<String>,
}

#[derive(Debug, Serialize)]
struct PhaseFiveDecision {
    split_execution_lanes: bool,
    reconciliation_coalescing: bool,
    dedicated_blocking_runtime: bool,
    adaptive_limiter: bool,
    shared_daemon: bool,
    rationale: String,
}

struct RequestOutcome {
    workload: Workload,
    elapsed_micros: u64,
    result: Result<Value>,
}

#[derive(Default)]
struct ResourceSample {
    peak_rss_bytes: Option<u64>,
    peak_wal_bytes: u64,
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
#[ignore = "run explicitly in release mode with a pinned large repository"]
async fn release_concurrency_matrix() {
    let large_repository = PathBuf::from(
        env::var(LARGE_REPOSITORY_ENV)
            .unwrap_or_else(|_| panic!("{LARGE_REPOSITORY_ENV} must name a large checkout")),
    );
    let expected_large_revision = env::var(LARGE_REVISION_ENV)
        .unwrap_or_else(|_| panic!("{LARGE_REVISION_ENV} must pin the large checkout"));
    let actual_large_revision = git_revision(&large_repository);
    assert_eq!(
        actual_large_revision, expected_large_revision,
        "large checkout is not at the pinned revision"
    );

    let output = PathBuf::from(
        env::var(OUTPUT_ENV).unwrap_or_else(|_| "target/concurrency-profile.json".to_owned()),
    );
    if let Some(parent) = output.parent() {
        fs::create_dir_all(parent).expect("create report directory");
    }

    let small = tempfile::tempdir().expect("small repository");
    create_small_repository(small.path());
    let small_revision = "generated-concurrency-fixture-v1".to_owned();

    let small_report =
        profile_repository("small_generated", small.path(), &small_revision, "needle").await;
    let large_report = profile_repository(
        "large_pinned",
        &large_repository,
        &actual_large_revision,
        "request",
    )
    .await;

    let report = ProfileReport {
        schema_version: 2,
        generated_at_unix_seconds: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock")
            .as_secs(),
        leantoken_revision: git_revision(Path::new(env!("CARGO_MANIFEST_DIR"))),
        rustc: command_output("rustc", &["--version"]),
        execution_capacity: default_blocking_execution_capacity(),
        active_capacity: default_blocking_active_capacity(),
        queue_timeout_ms: DEFAULT_BLOCKING_QUEUE_TIMEOUT.as_millis(),
        concurrency_levels: CONCURRENCY_LEVELS.to_vec(),
        repositories: vec![small_report, large_report],
        phase_five_decision: PhaseFiveDecision {
            split_execution_lanes: false,
            reconciliation_coalescing: true,
            dedicated_blocking_runtime: false,
            adaptive_limiter: false,
            shared_daemon: false,
            rationale: "Deterministic wave admission demonstrates that concurrent reconcile_working_tree callers otherwise submit redundant serialized scans. Services now coalesces callers admitted before a scan starts and assigns later callers to one pending freshness wave. Other Phase 5 mechanisms remain disabled until a same-host comparison isolates their owner without parity, error, RSS, WAL, or determinism regressions.".into(),
        },
    };
    fs::write(
        &output,
        serde_json::to_vec_pretty(&report).expect("serialize report"),
    )
    .expect("write report");
    println!("wrote {}", output.display());
}

async fn profile_repository(
    name: &str,
    repository: &Path,
    revision: &str,
    query: &str,
) -> RepositoryReport {
    let database_dir = tempfile::tempdir().expect("database directory");
    let database = database_dir.path().join("index.sqlite");
    let config = Config::discover(repository, Some(database.clone())).expect("config");
    let services = Arc::new(Services::open(config).expect("services"));
    let index_started = Instant::now();
    let index = services
        .refresh(IndexingMode::Reconcile)
        .await
        .expect("index repository");
    let index_milliseconds = index_started.elapsed().as_millis();
    let source_path = first_source_path(repository);
    let baselines = baseline_responses(&services, &source_path, query).await;
    let mut scenarios = Vec::new();

    for concurrency in CONCURRENCY_LEVELS {
        for scenario in [
            Scenario::Mixed,
            Scenario::CancellationStorm,
            Scenario::ConcurrentIndexing,
        ] {
            scenarios.push(
                run_scenario(
                    Arc::clone(&services),
                    &database,
                    &source_path,
                    query,
                    &baselines,
                    concurrency,
                    scenario,
                )
                .await,
            );
        }
    }
    let reconciliation_wave = profile_reconciliation_wave(
        Arc::clone(&services),
        super::reconciliation::default_reconciliation_active_capacity(),
    )
    .await;

    RepositoryReport {
        name: name.to_owned(),
        path: repository.display().to_string(),
        revision: revision.to_owned(),
        indexed_files: index.files_indexed,
        index_milliseconds,
        database_bytes: fs::metadata(&database)
            .map(|metadata| metadata.len())
            .unwrap_or(0),
        scenarios,
        reconciliation_wave,
    }
}

async fn profile_reconciliation_wave(
    services: Arc<Services>,
    requests: usize,
) -> ReconciliationWaveReport {
    services.reconciliation.reset_diagnostics();
    let held_operation = services
        .coordination
        .acquire_operation(&CancellationToken::new())
        .expect("hold operation lock for reconciliation wave");
    let started = Instant::now();
    let mut tasks = JoinSet::new();
    for _ in 0..requests {
        let services = Arc::clone(&services);
        tasks.spawn(async move {
            services
                .apply_consistency(
                    IndexConsistency::ReconcileWorkingTree,
                    CancellationToken::new(),
                )
                .await
        });
    }
    for _ in 0..10_000 {
        if services.reconciliation.diagnostics().requests == requests as u64 {
            break;
        }
        tokio::task::yield_now().await;
    }
    assert_eq!(
        services.reconciliation.diagnostics().requests,
        requests as u64,
        "all reconciliation requests must reach wave admission"
    );
    held_operation
        .release()
        .expect("release reconciliation wave operation lock");

    let mut errors = 0_u64;
    while let Some(result) = tasks.join_next().await {
        if result.expect("reconciliation wave task").is_err() {
            errors = errors.saturating_add(1);
        }
    }
    let diagnostics = services.reconciliation.diagnostics();
    ReconciliationWaveReport {
        requests,
        elapsed_micros: duration_micros(started.elapsed()),
        errors,
        rejected_requests: diagnostics.rejected_requests,
        waves_created: diagnostics.waves_created,
        waves_started: diagnostics.waves_started,
        waves_completed: diagnostics.waves_completed,
        waves_failed: diagnostics.waves_failed,
        coalesced_requests: diagnostics.coalesced_requests,
        peak_active_waves: diagnostics.peak_active_waves,
        peak_pending_waiters: diagnostics.peak_pending_waiters,
    }
}

async fn baseline_responses(
    services: &Services,
    source_path: &str,
    query: &str,
) -> HashMap<Workload, Value> {
    let mut baselines = HashMap::new();
    for workload in WORKLOADS {
        let response = execute_workload(
            services,
            workload,
            source_path,
            query,
            CancellationToken::new(),
        )
        .await
        .expect("baseline response");
        baselines.insert(workload, normalize_response(response));
    }
    baselines
}

async fn run_scenario(
    services: Arc<Services>,
    database: &Path,
    source_path: &str,
    query: &str,
    baselines: &HashMap<Workload, Value>,
    concurrency: usize,
    scenario: Scenario,
) -> ScenarioReport {
    services.blocking_executor.reset_diagnostics();
    services.storage.reset_diagnostics();
    let cpu_before = process_cpu_ticks();
    let wal_before = wal_bytes(database);
    let sampler_cancellation = CancellationToken::new();
    let sampler = tokio::spawn(sample_resources(
        database.to_owned(),
        sampler_cancellation.clone(),
    ));
    let request_count = concurrency.saturating_mul(2).max(8);
    let mut outcomes = Vec::with_capacity(request_count);
    let mut indexing_milliseconds = None;
    let mut indexing_error = None;
    let mut next_request = 0;

    while next_request < request_count {
        let wave_size = concurrency.min(request_count - next_request);
        let barrier_size = wave_size
            + usize::from(matches!(scenario, Scenario::ConcurrentIndexing) && next_request == 0)
            + 1;
        let barrier = Arc::new(tokio::sync::Barrier::new(barrier_size));
        let mut tasks = JoinSet::new();
        let mut cancellations = Vec::new();

        for wave_index in 0..wave_size {
            let request_index = next_request + wave_index;
            let workload = WORKLOADS[request_index % WORKLOADS.len()];
            let services = Arc::clone(&services);
            let source_path = source_path.to_owned();
            let query = query.to_owned();
            let barrier = Arc::clone(&barrier);
            let cancellation = CancellationToken::new();
            if matches!(scenario, Scenario::CancellationStorm) && request_index % 3 == 0 {
                cancellations.push(cancellation.clone());
            }
            tasks.spawn(async move {
                barrier.wait().await;
                let started = Instant::now();
                let result =
                    execute_workload(&services, workload, &source_path, &query, cancellation).await;
                RequestOutcome {
                    workload,
                    elapsed_micros: duration_micros(started.elapsed()),
                    result,
                }
            });
        }

        let indexing = if matches!(scenario, Scenario::ConcurrentIndexing) && next_request == 0 {
            let services = Arc::clone(&services);
            let barrier = Arc::clone(&barrier);
            let source_path = source_path.to_owned();
            Some(tokio::spawn(async move {
                barrier.wait().await;
                let started = Instant::now();
                let _ = source_path;
                let result = services.refresh(IndexingMode::Reconcile).await.map(|_| ());
                (started.elapsed().as_millis(), result)
            }))
        } else {
            None
        };

        barrier.wait().await;
        if !cancellations.is_empty() {
            tokio::task::yield_now().await;
            for cancellation in cancellations {
                cancellation.cancel();
            }
        }
        while let Some(outcome) = tasks.join_next().await {
            outcomes.push(outcome.expect("retrieval task"));
        }
        if let Some(indexing) = indexing {
            let (elapsed_milliseconds, result) = indexing.await.expect("index task");
            indexing_milliseconds = Some(elapsed_milliseconds);
            if let Err(error) = result {
                indexing_error = Some(error.to_string());
            }
        }
        next_request += wave_size;
    }

    sampler_cancellation.cancel();
    let resources = sampler.await.expect("resource sampler");
    let cpu_after = process_cpu_ticks();
    let executor = services.blocking_executor.diagnostics();
    let storage = services.storage.diagnostics();
    let checkpoint = passive_checkpoint(database);
    let wal_after = wal_bytes(database);
    let mut complete = outcomes
        .iter()
        .map(|outcome| outcome.elapsed_micros)
        .collect::<Vec<_>>();
    let mut succeeded = 0_u64;
    let mut cancelled = 0_u64;
    let mut rejected = 0_u64;
    let mut timed_out = 0_u64;
    let mut errors = 0_u64;
    let mut parity_checked = 0_u64;
    let mut parity_mismatches = 0_u64;
    let mut order_mismatches = 0_u64;
    let mut generation_mismatches = 0_u64;
    let mut token_accounting_mismatches = 0_u64;

    for outcome in outcomes {
        match outcome.result {
            Ok(response) => {
                succeeded = succeeded.saturating_add(1);
                if parity_workload(outcome.workload) {
                    parity_checked = parity_checked.saturating_add(1);
                    let normalized = normalize_response(response);
                    let baseline = &baselines[&outcome.workload];
                    if normalized != *baseline {
                        parity_mismatches = parity_mismatches.saturating_add(1);
                    }
                    if ordered_arrays(&normalized) != ordered_arrays(baseline) {
                        order_mismatches = order_mismatches.saturating_add(1);
                    }
                    if normalized.pointer("/meta/repository_generation")
                        != baseline.pointer("/meta/repository_generation")
                    {
                        generation_mismatches = generation_mismatches.saturating_add(1);
                    }
                    if token_accounting(&normalized) != token_accounting(baseline) {
                        token_accounting_mismatches = token_accounting_mismatches.saturating_add(1);
                    }
                }
            }
            Err(Error::Cancelled) => cancelled = cancelled.saturating_add(1),
            Err(Error::RetrievalOverloaded) => rejected = rejected.saturating_add(1),
            Err(Error::RetrievalQueueTimeout) => timed_out = timed_out.saturating_add(1),
            Err(_) => errors = errors.saturating_add(1),
        }
    }

    ScenarioReport {
        scenario,
        concurrency,
        requests: request_count,
        complete_request_micros: percentiles(&mut complete),
        queue_wait_micros: percentiles(&mut executor.queue_wait_micros.clone()),
        reader_checkout_wait_micros: percentiles(&mut storage.reader_checkout_wait_micros.clone()),
        accepted: executor.accepted,
        rejected: rejected.max(executor.rejected),
        timed_out: timed_out.max(executor.queue_timed_out),
        cancelled,
        errors,
        succeeded,
        submitted_blocking_closures: executor.submitted,
        started_blocking_closures: executor.started,
        finished_blocking_closures: executor.finished,
        peak_active_blocking_requests: executor.peak_active,
        peak_running_blocking_closures: executor.peak_running,
        distinct_blocking_threads: executor.blocking_threads.len(),
        peak_active_sqlite_snapshots: storage.peak_active_snapshots,
        active_sqlite_snapshots_after: storage.active_snapshots,
        cpu_milliseconds: cpu_milliseconds(cpu_before, cpu_after),
        peak_rss_bytes: resources.peak_rss_bytes,
        wal_bytes_before: wal_before,
        wal_bytes_peak: resources.peak_wal_bytes.max(wal_before),
        wal_bytes_after: wal_after,
        checkpoint,
        parity_checked,
        parity_mismatches,
        order_mismatches,
        generation_mismatches,
        token_accounting_mismatches,
        indexing_milliseconds,
        indexing_error,
    }
}

async fn execute_workload(
    services: &Services,
    workload: Workload,
    source_path: &str,
    query: &str,
    cancellation: CancellationToken,
) -> Result<Value> {
    let response = match workload {
        Workload::Files => serde_json::to_value(
            services
                .files_cancellable(
                    FilesRequest {
                        operation: FileOperation::Find,
                        path: None,
                        query: Some(query.to_owned()),
                        pattern: None,
                        max_results: Some(20),
                        cursor: None,
                        depth: None,
                    },
                    cancellation,
                )
                .await?,
        )?,
        Workload::Search => serde_json::to_value(
            services
                .search_cancellable(search_request(query), cancellation)
                .await?,
        )?,
        Workload::Read => serde_json::to_value(
            services
                .read_cancellable(read_request(source_path), cancellation)
                .await?,
        )?,
        Workload::Context => serde_json::to_value(
            services
                .context_cancellable(context_request(query), cancellation)
                .await?,
        )?,
        Workload::Status => serde_json::to_value(services.status().await?)?,
        Workload::Savings => serde_json::to_value(services.token_savings().await?)?,
    };
    Ok(response)
}

fn search_request(query: &str) -> SearchRequest {
    SearchRequest {
        query: query.to_owned(),
        mode: SearchMode::Auto,
        include_paths: Vec::new(),
        exclude_paths: Vec::new(),
        focus_paths: Vec::new(),
        max_results: Some(20),
        max_tokens: Some(1_000),
        context_lines: Some(2),
        case_sensitive: false,
        all_occurrences: false,
        prefer_structural: false,
        receipt_id: None,
        query_receipt: None,
        cursor: None,
    }
}

fn read_request(source_path: &str) -> ReadRequest {
    ReadRequest {
        path: source_path.to_owned(),
        symbol: None,
        heading: None,
        heading_occurrence: None,
        start_line: Some(1),
        end_line: Some(80),
        continuation_cursor: None,
        max_tokens: Some(1_000),
        expected_hash: None,
        delta: false,
        receipt_id: None,
        policy: crate::model::ReadPolicy::default(),
    }
}

fn context_request(query: &str) -> ContextRequest {
    ContextRequest {
        task: format!("Investigate {query} handling, cancellation, and tests"),
        token_budget: 1_000,
        include_paths: Vec::new(),
        must_include_paths: Vec::new(),
        must_include_symbols: Vec::new(),
        required_evidence: Vec::new(),
        max_fragments: Some(8),
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
    }
}

fn parity_workload(workload: Workload) -> bool {
    matches!(
        workload,
        Workload::Files | Workload::Search | Workload::Read | Workload::Context
    )
}

fn normalize_response(mut value: Value) -> Value {
    remove_key_recursively(&mut value, "receipt_id");
    value
}

fn remove_key_recursively(value: &mut Value, key: &str) {
    match value {
        Value::Object(object) => {
            object.remove(key);
            for value in object.values_mut() {
                remove_key_recursively(value, key);
            }
        }
        Value::Array(values) => {
            for value in values {
                remove_key_recursively(value, key);
            }
        }
        _ => {}
    }
}

fn ordered_arrays(value: &Value) -> Vec<Value> {
    let mut arrays = Vec::new();
    collect_arrays(value, &mut arrays);
    arrays
}

fn collect_arrays(value: &Value, arrays: &mut Vec<Value>) {
    match value {
        Value::Array(values) => {
            arrays.push(value.clone());
            for value in values {
                collect_arrays(value, arrays);
            }
        }
        Value::Object(object) => {
            for value in object.values() {
                collect_arrays(value, arrays);
            }
        }
        _ => {}
    }
}

fn token_accounting(value: &Value) -> Vec<Option<Value>> {
    [
        "/meta/source_tokens",
        "/meta/total_response_tokens",
        "/meta/token_count_exact",
        "/meta/tokenizer",
    ]
    .into_iter()
    .map(|pointer| value.pointer(pointer).cloned())
    .collect()
}

fn percentiles(samples: &mut [u64]) -> Percentiles {
    if samples.is_empty() {
        return Percentiles::default();
    }
    samples.sort_unstable();
    Percentiles {
        samples: samples.len(),
        p50: Some(percentile(samples, 50)),
        p95: Some(percentile(samples, 95)),
        max: samples.last().copied(),
    }
}

fn percentile(samples: &[u64], percentile: usize) -> u64 {
    let index = (samples.len().saturating_sub(1))
        .saturating_mul(percentile)
        .div_ceil(100);
    samples[index]
}

async fn sample_resources(database: PathBuf, cancellation: CancellationToken) -> ResourceSample {
    let mut sample = ResourceSample::default();
    loop {
        sample.peak_rss_bytes = max_option(sample.peak_rss_bytes, process_rss_bytes());
        sample.peak_wal_bytes = sample.peak_wal_bytes.max(wal_bytes(&database));
        tokio::select! {
            biased;
            _ = cancellation.cancelled() => return sample,
            _ = tokio::time::sleep(Duration::from_millis(5)) => {}
        }
    }
}

fn passive_checkpoint(database: &Path) -> CheckpointReport {
    let connection = match Connection::open(database) {
        Ok(connection) => connection,
        Err(error) => {
            return CheckpointReport {
                error: Some(error.to_string()),
                ..CheckpointReport::default()
            };
        }
    };
    match connection.query_row("PRAGMA wal_checkpoint(PASSIVE)", [], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, i64>(1)?,
            row.get::<_, i64>(2)?,
        ))
    }) {
        Ok((busy, log_frames, checkpointed_frames)) => CheckpointReport {
            busy: Some(busy),
            log_frames: Some(log_frames),
            checkpointed_frames: Some(checkpointed_frames),
            error: None,
        },
        Err(error) => CheckpointReport {
            error: Some(error.to_string()),
            ..CheckpointReport::default()
        },
    }
}

fn create_small_repository(root: &Path) {
    fs::create_dir_all(root.join("src")).expect("small src");
    for file_index in 0..64 {
        let mut source = format!("pub fn needle_{file_index}() -> usize {{\n");
        for line in 0..80 {
            source.push_str(&format!(
                "    let value_{line} = {line} + {file_index}; // needle cancellation request\n"
            ));
        }
        source.push_str("    value_79\n}\n");
        fs::write(
            root.join("src").join(format!("module_{file_index}.rs")),
            source,
        )
        .expect("small source");
    }
}

fn first_source_path(repository: &Path) -> String {
    WalkBuilder::new(repository)
        .hidden(false)
        .git_ignore(true)
        .git_exclude(true)
        .parents(true)
        .build()
        .filter_map(std::result::Result::ok)
        .find_map(|entry| {
            let path = entry.path();
            if !entry.file_type().is_some_and(|kind| kind.is_file())
                || !matches!(
                    path.extension().and_then(|extension| extension.to_str()),
                    Some("rs" | "ts" | "tsx" | "js" | "jsx" | "py" | "go" | "java")
                )
            {
                return None;
            }
            path.strip_prefix(repository)
                .ok()
                .map(|path| path.to_string_lossy().replace('\\', "/"))
        })
        .expect("repository contains a supported source file")
}

fn wal_bytes(database: &Path) -> u64 {
    fs::metadata(format!("{}-wal", database.display()))
        .map(|metadata| metadata.len())
        .unwrap_or(0)
}

fn process_rss_bytes() -> Option<u64> {
    let status = fs::read_to_string("/proc/self/status").ok()?;
    let kibibytes = status
        .lines()
        .find_map(|line| line.strip_prefix("VmRSS:"))?
        .split_whitespace()
        .next()?
        .parse::<u64>()
        .ok()?;
    kibibytes.checked_mul(1024)
}

fn process_cpu_ticks() -> Option<u64> {
    let stat = fs::read_to_string("/proc/self/stat").ok()?;
    let fields = stat
        .rsplit_once(") ")?
        .1
        .split_whitespace()
        .collect::<Vec<_>>();
    let user = fields.get(11)?.parse::<u64>().ok()?;
    let system = fields.get(12)?.parse::<u64>().ok()?;
    user.checked_add(system)
}

fn cpu_milliseconds(before: Option<u64>, after: Option<u64>) -> Option<u64> {
    let ticks = after?.saturating_sub(before?);
    let ticks_per_second = command_output("getconf", &["CLK_TCK"])
        .parse::<u64>()
        .ok()?;
    ticks.checked_mul(1_000)?.checked_div(ticks_per_second)
}

fn max_option(left: Option<u64>, right: Option<u64>) -> Option<u64> {
    match (left, right) {
        (Some(left), Some(right)) => Some(left.max(right)),
        (left, right) => left.or(right),
    }
}

fn duration_micros(duration: Duration) -> u64 {
    duration.as_micros().min(u128::from(u64::MAX)) as u64
}

fn git_revision(repository: &Path) -> String {
    command_output_in(repository, "git", &["rev-parse", "HEAD"])
}

fn command_output(command: &str, arguments: &[&str]) -> String {
    command_output_in(Path::new(env!("CARGO_MANIFEST_DIR")), command, arguments)
}

fn command_output_in(directory: &Path, command: &str, arguments: &[&str]) -> String {
    let output = Command::new(command)
        .args(arguments)
        .current_dir(directory)
        .output()
        .unwrap_or_else(|error| panic!("run {command}: {error}"));
    assert!(
        output.status.success(),
        "{command} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .expect("command output is UTF-8")
        .trim()
        .to_owned()
}
