//! Standalone concurrency profile linking the ordinary release library.

#[path = "../crates/benchmarks/src/build_identity.rs"]
mod build_identity;

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

use leantoken::{services::Services, *};

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

struct ProfileArm {
    scenario: Scenario,
    concurrency: usize,
    resource_sampling: bool,
}

#[derive(Debug, Serialize)]
struct ProfileReport {
    schema_version: u32,
    generated_at_unix_seconds: u64,
    leantoken_revision: String,
    checkout_revision: String,
    product_source_blake3: String,
    rustflags: &'static str,
    rustc: String,
    target: &'static str,
    profile: &'static str,
    source_dirty: bool,
    performance_eligible: bool,
    cfg_test: bool,
    observation: &'static str,
    cargo_lock_blake3: String,
    harness_blake3: String,
    concurrency_levels: Vec<usize>,
    repositories: Vec<RepositoryReport>,
}

#[derive(Debug, Serialize)]
struct RepositoryReport {
    name: String,
    path: String,
    revision: String,
    read_path: String,
    query: String,
    indexed_files: usize,
    index_milliseconds: u128,
    database_bytes: u64,
    scenarios: Vec<ScenarioReport>,
    calibrations: Vec<Calibration>,
}

#[derive(Debug, Serialize)]
struct Calibration {
    scenario: Scenario,
    concurrency: usize,
    sampled_p95_over_baseline: Option<f64>,
    outcome_categories_equal: bool,
    eligible_for_performance_conclusions: bool,
}

#[derive(Debug, Serialize)]
struct ScenarioReport {
    scenario: Scenario,
    resource_sampling: bool,
    concurrency: usize,
    requests: usize,
    complete_request_micros: Percentiles,
    rejected: u64,
    timed_out: u64,
    cancelled: u64,
    errors: u64,
    succeeded: u64,
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
    accounting_conservative_ceilings: u64,
    mismatch_examples: Vec<Value>,
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

struct RequestOutcome {
    workload: Workload,
    elapsed_micros: u64,
    result: Result<ObservedResponse>,
}

struct ObservedResponse {
    value: Value,
    serialized: String,
}

fn observe<T: Serialize>(response: T) -> Result<ObservedResponse> {
    let serialized = serde_json::to_string(&response)?;
    Ok(ObservedResponse {
        value: serde_json::to_value(response)?,
        serialized,
    })
}

#[derive(Default)]
struct ResourceSample {
    peak_rss_bytes: Option<u64>,
    peak_wal_bytes: u64,
}

#[tokio::main(flavor = "multi_thread", worker_threads = 8)]
async fn main() {
    if cfg!(debug_assertions) || cfg!(test) {
        panic!("run the ordinary profiler binary built with --release");
    }
    let source_root = Path::new(env!("LEANTOKEN_REPOSITORY_ROOT"));
    let source_identity =
        build_identity::product_sources(source_root).expect("runtime source identity");
    assert_eq!(
        source_identity,
        env!("LEANTOKEN_BUILD_SOURCE_BLAKE3"),
        "checkout source differs from the compiled product; rebuild the profiler"
    );
    let checkout_revision = git_revision(source_root);
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

    let small = profile_directory();
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

    let source_dirty = !command_output("git", &["status", "--porcelain"]).is_empty();
    let corpus_dirty =
        !command_output_in(&large_repository, "git", &["status", "--porcelain"]).is_empty();
    let corpus_revision_unchanged =
        pinned_revision_unchanged(&large_repository, &actual_large_revision);
    let source_unchanged = build_identity::product_sources(source_root)
        .expect("final source identity")
        == source_identity;
    let performance_eligible = !source_dirty
        && !corpus_dirty
        && corpus_revision_unchanged
        && source_unchanged
        && checkout_revision == env!("LEANTOKEN_BUILD_REVISION")
        && small_report
            .calibrations
            .iter()
            .chain(&large_report.calibrations)
            .all(|arm| arm.eligible_for_performance_conclusions);
    let report = ProfileReport {
        schema_version: 3,
        generated_at_unix_seconds: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock")
            .as_secs(),
        leantoken_revision: env!("LEANTOKEN_BUILD_REVISION").into(),
        checkout_revision,
        product_source_blake3: source_identity,
        rustflags: env!("LEANTOKEN_BUILD_RUSTFLAGS"),
        rustc: env!("LEANTOKEN_BUILD_RUSTC").into(),
        target: env!("LEANTOKEN_BUILD_TARGET"),
        profile: env!("LEANTOKEN_BUILD_PROFILE"),
        source_dirty,
        performance_eligible,
        cfg_test: cfg!(test),
        observation: "external request timers and 5ms process RSS/WAL polling; no test-only diagnostics",
        cargo_lock_blake3: blake3::hash(include_bytes!("../Cargo.lock"))
            .to_hex()
            .to_string(),
        harness_blake3: blake3::hash(include_bytes!("concurrency_profile.rs"))
            .to_hex()
            .to_string(),
        concurrency_levels: CONCURRENCY_LEVELS.to_vec(),
        repositories: vec![small_report, large_report],
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
    let database_dir = profile_directory();
    let database = database_dir.path().join("index.sqlite");
    let config = Config::discover(repository, Some(database.clone())).expect("config");
    let services = Arc::new(Services::open(config).expect("services"));
    let index_started = Instant::now();
    let index = services
        .index(IndexingMode::Reconcile)
        .await
        .expect("index repository");
    let index_milliseconds = index_started.elapsed().as_millis();
    let source_path = first_source_path(repository);
    let baselines = baseline_responses(&services, &source_path, query).await;
    let mut scenarios = Vec::new();
    let mut calibrations = Vec::new();

    for concurrency in CONCURRENCY_LEVELS {
        for scenario in [
            Scenario::Mixed,
            Scenario::CancellationStorm,
            Scenario::ConcurrentIndexing,
        ] {
            let first = scenarios.len();
            // ABBA bounds order effects; both arms use the same production
            // library and bounded request timers. Only process polling differs.
            for resource_sampling in [false, true, true, false] {
                scenarios.push(
                    run_scenario(
                        Arc::clone(&services),
                        &database,
                        &source_path,
                        query,
                        &baselines,
                        ProfileArm {
                            concurrency,
                            scenario,
                            resource_sampling,
                        },
                    )
                    .await,
                );
            }
            let arms = &scenarios[first..];
            let baseline = arms[0].complete_request_micros.p95.unwrap_or(0)
                + arms[3].complete_request_micros.p95.unwrap_or(0);
            let sampled = arms[1].complete_request_micros.p95.unwrap_or(0)
                + arms[2].complete_request_micros.p95.unwrap_or(0);
            let ratio = (baseline > 0).then_some(sampled as f64 / baseline as f64);
            let categories = |arm: &ScenarioReport| {
                [
                    arm.succeeded,
                    arm.rejected,
                    arm.timed_out,
                    arm.cancelled,
                    arm.errors,
                ]
            };
            let equal = arms
                .iter()
                .all(|arm| categories(arm) == categories(&arms[0]));
            let parity = arms.iter().all(|arm| {
                arm.errors == 0
                    && arm.parity_mismatches == 0
                    && arm.order_mismatches == 0
                    && arm.generation_mismatches == 0
                    && arm.token_accounting_mismatches == 0
                    && arm.accounting_conservative_ceilings == 0
                    && arm.indexing_error.is_none()
            });
            calibrations.push(Calibration {
                scenario,
                concurrency,
                sampled_p95_over_baseline: ratio,
                outcome_categories_equal: equal,
                eligible_for_performance_conclusions: equal
                    && parity
                    && ratio.is_some_and(|value| value <= 1.10),
            });
        }
    }

    RepositoryReport {
        name: name.to_owned(),
        path: repository.display().to_string(),
        revision: revision.to_owned(),
        read_path: source_path,
        query: query.to_owned(),
        indexed_files: index.files_indexed,
        index_milliseconds,
        database_bytes: fs::metadata(&database)
            .map(|metadata| metadata.len())
            .unwrap_or(0),
        scenarios,
        calibrations,
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
        baselines.insert(workload, normalize_response(response.value));
    }
    baselines
}

async fn run_scenario(
    services: Arc<Services>,
    database: &Path,
    source_path: &str,
    query: &str,
    baselines: &HashMap<Workload, Value>,
    arm: ProfileArm,
) -> ScenarioReport {
    let ProfileArm {
        concurrency,
        scenario,
        resource_sampling,
    } = arm;
    let cpu_before = process_cpu_ticks();
    let wal_before = wal_bytes(database);
    let sampler_cancellation = CancellationToken::new();
    let sampler = resource_sampling.then(|| {
        tokio::spawn(sample_resources(
            database.to_owned(),
            sampler_cancellation.clone(),
        ))
    });
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
                let result = services.index_paths(vec![source_path]).await.map(|_| ());
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
    let resources = match sampler {
        Some(sampler) => sampler.await.expect("resource sampler"),
        None => ResourceSample::default(),
    };
    let cpu_after = process_cpu_ticks();
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
    let mut accounting_conservative_ceilings = 0_u64;
    let mut mismatch_examples = Vec::new();

    for outcome in outcomes {
        match outcome.result {
            Ok(response) => {
                succeeded = succeeded.saturating_add(1);
                if parity_workload(outcome.workload) {
                    parity_checked = parity_checked.saturating_add(1);
                    let meta = &response.value["meta"];
                    let reported = meta["total_response_tokens"].as_u64();
                    let source = meta["source_tokens"].as_u64();
                    let protocol = meta["protocol_tokens"].as_u64().unwrap_or(0);
                    let overhead = meta["path_and_metadata_tokens"].as_u64().unwrap_or(0);
                    let count = services.config().tokenizer.count(&response.serialized) as u64;
                    let valid = accounting_valid(meta, count, services.config().tokenizer);
                    if !valid {
                        token_accounting_mismatches += 1;
                    }
                    // Accounting permits conservative ceilings for BPE fixed-point
                    // cycles. A positive ceiling is inconclusive for this observer:
                    // do not silently accept arbitrary over-reporting as exact.
                    let ceiling = reported.is_some_and(|reported| reported > count);
                    if ceiling {
                        accounting_conservative_ceilings += 1;
                    }
                    if (!valid || ceiling) && mismatch_examples.len() < 8 {
                        mismatch_examples.push(serde_json::json!({"workload": outcome.workload, "kind": "accounting", "reported": reported, "counted": count, "source": source, "protocol": protocol, "metadata": overhead}));
                    }
                    let normalized = normalize_response(response.value);
                    let baseline = &baselines[&outcome.workload];
                    if normalized != *baseline {
                        parity_mismatches = parity_mismatches.saturating_add(1);
                        if mismatch_examples.len() < 8 {
                            mismatch_examples.push(serde_json::json!({"workload": outcome.workload, "kind": "semantic_parity", "changed_top_level_fields": normalized.as_object().map(|object| object.iter().filter(|(key, value)| baseline.get(*key) != Some(*value)).map(|(key, _)| key).take(16).collect::<Vec<_>>()), "baseline_provenance": baseline.get("provenance"), "actual_provenance": normalized.get("provenance")}));
                        }
                    }
                    if ordered_arrays(&normalized) != ordered_arrays(baseline) {
                        order_mismatches = order_mismatches.saturating_add(1);
                    }
                    if normalized.pointer("/meta/repository_generation")
                        != baseline.pointer("/meta/repository_generation")
                    {
                        generation_mismatches = generation_mismatches.saturating_add(1);
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
        resource_sampling,
        concurrency,
        requests: request_count,
        complete_request_micros: percentiles(&mut complete),
        rejected,
        timed_out,
        cancelled,
        errors,
        succeeded,
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
        accounting_conservative_ceilings,
        mismatch_examples,
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
) -> Result<ObservedResponse> {
    let response = match workload {
        Workload::Files => observe(
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
        Workload::Search => observe(
            services
                .search_cancellable(search_request(query), cancellation)
                .await?,
        )?,
        Workload::Read => observe(
            services
                .read_cancellable(read_request(source_path), cancellation)
                .await?,
        )?,
        Workload::Context => observe(
            services
                .context_cancellable(context_request(query), cancellation)
                .await?,
        )?,
        Workload::Status => observe(services.status().await?)?,
        Workload::Savings => observe(services.token_savings().await?)?,
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
        policy: ReadPolicy::default(),
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

fn accounting_valid(meta: &Value, count: u64, tokenizer: leantoken::tokens::Tokenizer) -> bool {
    let reported = meta["total_response_tokens"].as_u64();
    reported.is_some_and(|total| total >= count)
        && meta["source_tokens"]
            .as_u64()
            .and_then(|source| source.checked_add(meta["protocol_tokens"].as_u64().unwrap_or(0)))
            .and_then(|sum| sum.checked_add(meta["path_and_metadata_tokens"].as_u64().unwrap_or(0)))
            == reported
        && meta["tokenizer"].as_str() == Some(tokenizer.name())
        && meta["token_count_exact"].as_bool() == Some(tokenizer.is_exact())
}

fn normalize_response(mut value: Value) -> Value {
    remove_key_recursively(&mut value, "receipt_id");
    if let Some(provenance) = value.get_mut("provenance").and_then(Value::as_object_mut) {
        provenance.remove("freshness");
    }
    if let Some(meta) = value.get_mut("meta").and_then(Value::as_object_mut) {
        for key in [
            "freshness",
            "protocol_tokens",
            "path_and_metadata_tokens",
            "total_response_tokens",
        ] {
            meta.remove(key);
        }
    }
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

fn pinned_revision_unchanged(repository: &Path, pinned_revision: &str) -> bool {
    git_revision(repository) == pinned_revision
}

fn git_revision(repository: &Path) -> String {
    command_output_in(repository, "git", &["rev-parse", "HEAD"])
}

fn profile_directory() -> tempfile::TempDir {
    let parent = Path::new(env!("LEANTOKEN_REPOSITORY_ROOT")).join("target/concurrency-work");
    fs::create_dir_all(&parent).expect("profile scratch directory");
    tempfile::tempdir_in(parent).expect("bounded lifetime profile fixture")
}

fn command_output(command: &str, arguments: &[&str]) -> String {
    command_output_in(
        Path::new(env!("LEANTOKEN_REPOSITORY_ROOT")),
        command,
        arguments,
    )
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn final_corpus_check_rejects_a_different_revision() {
        let repository = Path::new(env!("LEANTOKEN_REPOSITORY_ROOT"));
        let current = git_revision(repository);
        assert!(pinned_revision_unchanged(repository, &current));
        assert!(!pinned_revision_unchanged(
            repository,
            &"0".repeat(current.len())
        ));
    }

    #[test]
    fn accounting_checks_original_payload_count_and_category_sum() {
        let tokenizer = leantoken::tokens::Tokenizer::Cl100kBase;
        let mut meta = serde_json::json!({"source_tokens": 2, "protocol_tokens": 3,
            "path_and_metadata_tokens": 5, "total_response_tokens": 10,
            "tokenizer": "cl100k_base", "token_count_exact": true});
        assert!(accounting_valid(&meta, 10, tokenizer));
        assert!(!accounting_valid(&meta, 11, tokenizer));
        meta["total_response_tokens"] = 9.into();
        assert!(!accounting_valid(&meta, 9, tokenizer));
        meta["total_response_tokens"] = 10.into();
        meta["tokenizer"] = "estimate".into();
        assert!(!accounting_valid(&meta, 10, tokenizer));
    }

    #[test]
    fn semantic_comparison_preserves_order_generation_and_source_accounting() {
        let first = serde_json::json!({"entries": ["a", "b"], "provenance": {"freshness": "current", "repository_generation": 7, "commit_revision": "abc"}, "meta": {
            "receipt_id": "first", "freshness": "current", "repository_generation": 7,
            "source_tokens": 10, "total_response_tokens": 60}});
        let mut other = first.clone();
        other["meta"]["receipt_id"] = "second".into();
        other["meta"]["total_response_tokens"] = 61.into();
        other["provenance"]["freshness"] = "reconciling".into();
        assert_eq!(
            normalize_response(first.clone()),
            normalize_response(other.clone())
        );
        for (pointer, value) in [
            ("/entries", serde_json::json!(["b", "a"])),
            ("/meta/repository_generation", 8.into()),
            ("/meta/source_tokens", 11.into()),
            ("/provenance/repository_generation", 8.into()),
            ("/provenance/commit_revision", "def".into()),
        ] {
            let mut changed = other.clone();
            *changed.pointer_mut(pointer).unwrap() = value;
            assert_ne!(
                normalize_response(first.clone()),
                normalize_response(changed)
            );
        }
    }

    #[test]
    fn compiled_source_fingerprint_changes_with_product_input() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir(root.path().join("src")).unwrap();
        fs::create_dir(root.path().join(".cargo")).unwrap();
        fs::create_dir_all(root.path().join("crates/benchmarks")).unwrap();
        for name in [
            "Cargo.toml",
            "Cargo.lock",
            ".cargo/config.toml",
            "crates/benchmarks/Cargo.toml",
        ] {
            fs::write(root.path().join(name), "").unwrap();
        }
        fs::write(root.path().join("src/lib.rs"), "pub fn first() {}\n").unwrap();
        let before = build_identity::product_sources(root.path()).unwrap();
        fs::write(root.path().join("src/lib.rs"), "pub fn second() {}\n").unwrap();
        assert_ne!(
            before,
            build_identity::product_sources(root.path()).unwrap()
        );
    }
}
