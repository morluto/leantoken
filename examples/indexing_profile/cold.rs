use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use clap::{Args, ValueEnum};
use leantoken::Config;
use leantoken::indexer::{Indexer, IndexingDiagnostics, ProfiledIndexResponse};
use leantoken::model::{IndexProgressPhase, IndexResponse};
use leantoken::repository::{DiscoveredFile, discover_files};
use leantoken::storage::{Storage, StorageCounts};
use rusqlite::{Connection, types::ValueRef};
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

use super::{
    AnyResult, StorageFootprint, git_output, invalid_input, leantoken_source_identity,
    snapshot_repository, storage_footprint,
};

const SCHEMA_VERSION: u32 = 2;
const MAX_MATRIX_RUNS: usize = 16;
const MAX_WORKERS: usize = 64;
const MAX_PARITY_QUERIES: usize = 16;
const MAX_PARITY_QUERY_BYTES: usize = 256;
const MAX_TIMEOUT_SECONDS: u64 = 4 * 60 * 60;
const MAX_SAMPLE_INTERVAL_MS: u64 = 1_000;
const CANCELLATION_GRACE_SECONDS: u64 = 10 * 60;
const MAX_WORKER_REPORT_BYTES: u64 = 16 * 1024 * 1024;
const MAX_BASELINE_BYTES: u64 = 64 * 1024;
const REQUIRED_CANCELLATION_PHASES: [IndexProgressPhase; 7] = [
    IndexProgressPhase::Preparation,
    IndexProgressPhase::RelationalWrite,
    IndexProgressPhase::ChunkWordFts,
    IndexProgressPhase::ChunkTrigramFts,
    IndexProgressPhase::SymbolFts,
    IndexProgressPhase::ReferenceFts,
    IndexProgressPhase::CommitAndCheckpoint,
];

#[derive(Debug, Args)]
pub(super) struct ColdMatrixArgs {
    /// Existing clean Git checkout to profile through one isolated snapshot.
    #[arg(long, value_name = "PATH")]
    repository: PathBuf,
    /// Public repository name or URL recorded in the report.
    #[arg(long)]
    repository_label: String,
    /// Exact Git revision required before the snapshot is created.
    #[arg(long)]
    expected_revision: String,
    /// Screening matrix or the guarded one-versus-two-worker follow-up.
    #[arg(long, value_enum, default_value_t = ColdMatrixKind::Screening)]
    matrix_kind: ColdMatrixKind,
    /// Counterbalanced fresh-cache worker order. Defaults depend on matrix kind.
    #[arg(long)]
    worker_order: Option<String>,
    /// Retrieval parity queries replayed against every complete index.
    #[arg(
        long = "parity-query",
        value_delimiter = ',',
        default_value = "TODO,class"
    )]
    parity_queries: Vec<String>,
    /// Initial-index phases to cancel and rebuild from a fresh cache, or `none`.
    #[arg(
        long,
        default_value = "preparation,relational_write,chunk_word_fts,chunk_trigram_fts,symbol_fts,reference_fts,commit_and_checkpoint"
    )]
    cancellation_phases: String,
    /// Poll interval for process and phase resource high-water sampling.
    #[arg(long, default_value_t = 25)]
    sample_interval_ms: u64,
    /// Per-index wall-time bound before cooperative cancellation is requested.
    #[arg(long, default_value_t = 7_200)]
    timeout_seconds: u64,
    /// JSON report destination.
    #[arg(long, default_value = "target/dependency-heavy-cold-index-v2.json")]
    output: PathBuf,
    /// Permit a debug build for deterministic mechanical smoke tests only.
    #[arg(long, hide = true)]
    allow_debug: bool,
    /// Permit a dirty LeanToken source tree for deterministic smoke tests only.
    #[arg(long, hide = true)]
    allow_dirty: bool,
    /// Permit an incomplete/non-counterbalanced worker order for smoke tests.
    #[arg(long, hide = true)]
    allow_incomplete_matrix: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ValueEnum)]
#[serde(rename_all = "snake_case")]
enum ColdMatrixKind {
    Screening,
    TwoWorkerFollowUp,
}

impl ColdMatrixKind {
    fn default_worker_order(self) -> &'static str {
        match self {
            Self::Screening => "1,2,4,4,2,1",
            Self::TwoWorkerFollowUp => "1,2,2,1,2,1,1,2",
        }
    }

    fn minimum_samples_per_worker(self) -> usize {
        match self {
            Self::Screening => 2,
            Self::TwoWorkerFollowUp => 4,
        }
    }
}

#[derive(Debug, Args)]
pub(super) struct ColdWorkerArgs {
    #[arg(long, value_name = "PATH")]
    repository: PathBuf,
    #[arg(long, value_name = "PATH")]
    database: PathBuf,
    #[arg(long)]
    sequence: usize,
    #[arg(long)]
    workers: usize,
    #[arg(long = "parity-query", value_delimiter = ',')]
    parity_queries: Vec<String>,
    #[arg(long)]
    sample_interval_ms: u64,
    #[arg(long)]
    timeout_seconds: u64,
    #[arg(long)]
    target_phase: Option<String>,
    #[arg(long, value_name = "PATH")]
    baseline: Option<PathBuf>,
    #[arg(long)]
    allow_missed_phase: bool,
    #[arg(long, value_name = "PATH")]
    output: PathBuf,
}

#[derive(Debug, Serialize)]
struct ColdMatrixReport {
    schema_version: u32,
    generated_unix_ms: u64,
    leantoken_version: &'static str,
    leantoken_git_revision: String,
    leantoken_worktree_dirty: bool,
    release_build: bool,
    host: HostReport,
    corpus: ColdCorpusReport,
    measurement_policy: MeasurementPolicy,
    runs: Vec<ColdRunReport>,
    worker_summaries: Vec<WorkerSummary>,
    cancellation_probes: Vec<CancellationProbeReport>,
    parity: ParityReport,
    decision: DecisionReport,
    limitations: Vec<String>,
}

#[derive(Debug, Serialize)]
struct HostReport {
    os: &'static str,
    arch: &'static str,
    available_parallelism: usize,
    kernel: Option<String>,
    rustc: Option<String>,
    clock_ticks_per_second: Option<u64>,
    executable_blake3: Option<String>,
}

#[derive(Debug, Serialize)]
struct ColdCorpusReport {
    source_kind: &'static str,
    source_repository: String,
    revision: String,
    files: usize,
    source_bytes: u64,
    mean_file_bytes: f64,
    max_directory_depth: usize,
    extensions: BTreeMap<String, usize>,
}

#[derive(Debug, Serialize)]
struct MeasurementPolicy {
    matrix_kind: ColdMatrixKind,
    worker_order: Vec<usize>,
    minimum_samples_per_worker: usize,
    sample_interval_ms: u64,
    timeout_seconds: u64,
    cancellation_grace_seconds: u64,
    cancellation_phases: Vec<IndexProgressPhase>,
    parity_queries: Vec<String>,
    minimum_wall_reduction: f64,
    minimum_wall_p95_reduction: Option<f64>,
    maximum_cpu_increase: f64,
    maximum_peak_rss_increase: f64,
    maximum_write_increase: f64,
    maximum_footprint_increase: f64,
    preparation_owner_threshold: f64,
    require_cancellation_observation: bool,
}

#[derive(Debug, Serialize, Deserialize)]
struct ColdRunReport {
    sequence: usize,
    workers: usize,
    wall_ms: f64,
    response: IndexResponse,
    diagnostics: IndexingDiagnostics,
    resources: ResourceReport,
    shape: IndexShape,
    final_storage_footprint: StorageFootprint,
    logical_index_blake3: String,
    retrieval_blake3: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct IndexShape {
    files: usize,
    chunks: usize,
    symbols: usize,
    references: usize,
    imports: usize,
    source_bytes: u64,
    languages: BTreeMap<String, usize>,
}

#[derive(Debug, Serialize, Deserialize)]
struct ResourceReport {
    sample_interval_ms: u64,
    samples: u64,
    cpu_ms: Option<f64>,
    process_user_cpu_ms: Option<f64>,
    process_system_cpu_ms: Option<f64>,
    process_write_bytes: Option<u64>,
    peak_rss_bytes: Option<u64>,
    peak_storage_footprint: StorageFootprint,
    by_phase: BTreeMap<String, PhaseResourceReport>,
    timeout_triggered: bool,
    cancellation_grace_exceeded: bool,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct PhaseResourceReport {
    samples: u64,
    approximate_cpu_ms: Option<f64>,
    approximate_process_write_bytes: Option<u64>,
    peak_rss_bytes: Option<u64>,
    peak_database_bytes: u64,
    peak_wal_bytes: u64,
    peak_shm_bytes: u64,
    peak_total_storage_bytes: u64,
}

#[derive(Debug, Serialize)]
struct WorkerSummary {
    workers: usize,
    samples: usize,
    wall_p50_ms: f64,
    wall_p95_ms: f64,
    mean_cpu_ms: Option<f64>,
    peak_rss_bytes: Option<u64>,
    mean_process_write_bytes: Option<f64>,
    max_final_storage_bytes: u64,
}

#[derive(Debug, Serialize, Deserialize)]
struct ParityReport {
    logical_index_blake3: String,
    retrieval_blake3: String,
    shape: IndexShape,
    complete: bool,
}

#[derive(Debug, Serialize)]
struct DecisionReport {
    baseline_workers: usize,
    dominant_phase: String,
    dominant_phase_share: f64,
    candidate_workers: Option<usize>,
    outcome: &'static str,
    rationale: String,
    comparisons: Vec<WorkerComparison>,
}

#[derive(Debug, Serialize)]
struct WorkerComparison {
    workers: usize,
    wall_reduction: f64,
    wall_p95_reduction: f64,
    cpu_increase: Option<f64>,
    peak_rss_increase: Option<f64>,
    write_increase: Option<f64>,
    footprint_increase: f64,
    passes: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MonitoredTarget {
    Phase(IndexProgressPhase),
}

#[derive(Debug, Serialize, Deserialize)]
struct CancellationProbeReport {
    target_phase: IndexProgressPhase,
    workers: usize,
    phase_observed: bool,
    cancellation_requested: bool,
    cancellation_to_return_ms: Option<f64>,
    result: String,
    attempt_wall_ms: f64,
    resources: ResourceReport,
    generation_after_attempt: u64,
    footprint_after_attempt: StorageFootprint,
    restart_wall_ms: f64,
    restart_generation: u64,
    restart_logical_index_blake3: String,
    restart_retrieval_blake3: String,
    restart_matches_baseline: bool,
}

struct PreparedColdCorpus {
    _temporary_root: tempfile::TempDir,
    root: PathBuf,
    source_repository: String,
    revision: String,
    files: Vec<DiscoveredFile>,
}

struct MonitoredProfile {
    result: leantoken::Result<ProfiledIndexResponse>,
    wall: Duration,
    resources: ResourceReport,
    target_observed: bool,
    cancellation_requested_at: Option<Instant>,
}

struct MonitorSettings {
    started: Instant,
    baseline: ProcessResourceBaseline,
    target: Option<MonitoredTarget>,
    sample_interval_ms: u64,
    timeout_seconds: u64,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "kind", content = "report", rename_all = "snake_case")]
enum ColdWorkerOutput {
    Matrix(Box<ColdRunReport>),
    Cancellation(Box<CancellationProbeReport>),
}

struct ResourceSampler {
    database: PathBuf,
    sample_interval_ms: u64,
    clock_ticks_per_second: Option<u64>,
    initial_cpu_ticks: Option<ProcessCpuTicks>,
    previous_cpu_ticks: Option<ProcessCpuTicks>,
    initial_write_bytes: Option<u64>,
    previous_write_bytes: Option<u64>,
    samples: u64,
    peak_rss_bytes: Option<u64>,
    peak_storage: StorageFootprint,
    phases: BTreeMap<String, PhaseResourceAccumulator>,
}

#[derive(Clone, Copy)]
struct ProcessResourceBaseline {
    cpu_ticks: Option<ProcessCpuTicks>,
    write_bytes: Option<u64>,
    rss_bytes: Option<u64>,
}

#[derive(Default)]
struct PhaseResourceAccumulator {
    samples: u64,
    cpu_ticks: Option<u64>,
    write_bytes: Option<u64>,
    peak_rss_bytes: Option<u64>,
    peak_database_bytes: u64,
    peak_wal_bytes: u64,
    peak_shm_bytes: u64,
    peak_total_storage_bytes: u64,
}

#[derive(Clone, Copy)]
struct ProcessCpuTicks {
    user: u64,
    system: u64,
}

pub(super) fn run(args: &ColdMatrixArgs) -> AnyResult<()> {
    let policy = validate_args(args)?;
    let corpus = prepare_corpus(args)?;
    let (revision, dirty) = leantoken_source_identity();
    let revision =
        revision.ok_or_else(|| invalid_input("LeanToken Git revision is unavailable"))?;
    let dirty = dirty.ok_or_else(|| invalid_input("LeanToken worktree state is unavailable"))?;
    if dirty && !args.allow_dirty {
        return Err(invalid_input(
            "cold-matrix requires a clean LeanToken worktree; commit the harness first",
        ));
    }

    let run_root = tempfile::tempdir()?;
    let mut runs = Vec::with_capacity(policy.worker_order.len());
    let mut parity: Option<ParityReport> = None;
    for (sequence, workers) in policy.worker_order.iter().copied().enumerate() {
        let report = measure_cold_run(sequence, workers, &corpus, run_root.path(), &policy)?;
        require_parity(&mut parity, &report)?;
        runs.push(report);
    }
    let parity = parity.ok_or_else(|| invalid_input("worker matrix produced no runs"))?;
    let cancellation_probes =
        measure_cancellation_probes(&corpus, run_root.path(), &policy, &parity)?;
    let worker_summaries = summarize_workers(&runs);
    let decision = decide(&runs, &worker_summaries, &policy);
    let host = host_report();
    let report = ColdMatrixReport {
        schema_version: SCHEMA_VERSION,
        generated_unix_ms: unix_millis(),
        leantoken_version: env!("LEANTOKEN_PRODUCT_VERSION"),
        leantoken_git_revision: revision,
        leantoken_worktree_dirty: dirty,
        release_build: !cfg!(debug_assertions),
        host,
        corpus: corpus_report(&corpus),
        measurement_policy: policy,
        runs,
        worker_summaries,
        cancellation_probes,
        parity,
        decision,
        limitations: vec![
            "The report characterizes one pinned corpus on one host; worker or storage conclusions do not transfer automatically to other repositories or platforms.".into(),
            "Phase CPU attribution is sampled process CPU assigned to the latest observed process-local phase; exact IndexingDiagnostics wall times remain the phase-owner source of truth.".into(),
            "RSS and SQLite main/WAL/SHM values are sampled high-water observations and can miss peaks shorter than the configured interval.".into(),
            "The per-index deadline requests cooperative cancellation. An in-process worker cannot safely kill its indexing thread and joins it after a grace violation; the supervising parent imposes a separate hard subprocess bound and kills a worker that fails to exit. cancellation_grace_exceeded records the in-process contract violation.".into(),
            "Cancellation probes request cooperative cancellation after the target phase is observed. A single SQLite FTS statement or commit may finish before cancellation is checked; the report preserves the resulting generation instead of assuming rollback.".into(),
            "Fresh subprocesses and SQLite paths isolate process initialization and database state, but the profiler does not evict the corpus from the operating-system page cache. The mirrored worker order counterbalances order effects; this is not a cold-disk measurement.".into(),
            "The snapshot copies ignore-visible regular files and does not vendor the external corpus or preserve Git metadata.".into(),
        ],
    };
    let json = serde_json::to_string_pretty(&report)?;
    if let Some(parent) = args.output.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&args.output, format!("{json}\n"))?;
    println!("{json}");
    Ok(())
}

fn validate_args(args: &ColdMatrixArgs) -> AnyResult<MeasurementPolicy> {
    if !cfg!(target_os = "linux") {
        return Err(invalid_input(
            "cold-matrix currently requires Linux /proc resource accounting",
        ));
    }
    if cfg!(debug_assertions) && !args.allow_debug {
        return Err(invalid_input(
            "cold-matrix decisions require a release build; use --allow-debug only for smoke tests",
        ));
    }
    if !args.repository.is_dir() {
        return Err(invalid_input("--repository must name a Git checkout"));
    }
    if args.repository_label.trim().is_empty() || args.repository_label.len() > 512 {
        return Err(invalid_input(
            "--repository-label must contain 1..=512 bytes",
        ));
    }
    if args.expected_revision.trim().is_empty() || args.expected_revision.len() > 64 {
        return Err(invalid_input(
            "--expected-revision must contain 1..=64 bytes",
        ));
    }
    if !(1..=MAX_SAMPLE_INTERVAL_MS).contains(&args.sample_interval_ms) {
        return Err(invalid_input("--sample-interval-ms must be in 1..=1000"));
    }
    if !(1..=MAX_TIMEOUT_SECONDS).contains(&args.timeout_seconds) {
        return Err(invalid_input("--timeout-seconds must be in 1..=14400"));
    }
    if args.parity_queries.is_empty() || args.parity_queries.len() > MAX_PARITY_QUERIES {
        return Err(invalid_input(
            "--parity-query requires 1..=16 bounded queries",
        ));
    }
    for query in &args.parity_queries {
        if query.is_empty() || query.len() > MAX_PARITY_QUERY_BYTES {
            return Err(invalid_input(
                "each --parity-query must contain 1..=256 bytes",
            ));
        }
    }

    let cancellation_phases = parse_cancellation_phases(&args.cancellation_phases)?;
    if !args.allow_incomplete_matrix
        && (cancellation_phases.len() != REQUIRED_CANCELLATION_PHASES.len()
            || REQUIRED_CANCELLATION_PHASES
                .iter()
                .any(|phase| !cancellation_phases.contains(phase)))
    {
        return Err(invalid_input(
            "decision matrices require every cancellation phase; use --allow-incomplete-matrix only for mechanical smoke tests",
        ));
    }
    let worker_order = parse_worker_order(
        args.worker_order
            .as_deref()
            .unwrap_or_else(|| args.matrix_kind.default_worker_order()),
    )?;
    if !args.allow_incomplete_matrix {
        match args.matrix_kind {
            ColdMatrixKind::Screening => validate_screening_order(&worker_order)?,
            ColdMatrixKind::TwoWorkerFollowUp => {
                validate_two_worker_follow_up_order(&worker_order)?;
            }
        }
    }
    Ok(MeasurementPolicy {
        matrix_kind: args.matrix_kind,
        worker_order,
        minimum_samples_per_worker: args.matrix_kind.minimum_samples_per_worker(),
        sample_interval_ms: args.sample_interval_ms,
        timeout_seconds: args.timeout_seconds,
        cancellation_grace_seconds: CANCELLATION_GRACE_SECONDS,
        cancellation_phases,
        parity_queries: args.parity_queries.clone(),
        minimum_wall_reduction: 0.20,
        minimum_wall_p95_reduction: (args.matrix_kind == ColdMatrixKind::TwoWorkerFollowUp)
            .then_some(0.20),
        maximum_cpu_increase: 0.25,
        maximum_peak_rss_increase: 0.25,
        maximum_write_increase: 0.05,
        maximum_footprint_increase: 0.05,
        preparation_owner_threshold: 0.35,
        require_cancellation_observation: !args.allow_incomplete_matrix,
    })
}

fn validate_screening_order(worker_order: &[usize]) -> AnyResult<()> {
    let reverse = worker_order.iter().rev().copied().collect::<Vec<_>>();
    let workers = worker_order.iter().copied().collect::<BTreeSet<_>>();
    let balanced = [1, 2, 4].into_iter().all(|worker| {
        worker_order
            .iter()
            .filter(|actual| **actual == worker)
            .count()
            >= 2
    });
    if worker_order != reverse || !balanced || workers != BTreeSet::from([1, 2, 4]) {
        return Err(invalid_input(
            "screening --worker-order must be a mirrored counterbalance containing 1,2,4 at least twice",
        ));
    }
    Ok(())
}

fn validate_two_worker_follow_up_order(worker_order: &[usize]) -> AnyResult<()> {
    let workers = worker_order.iter().copied().collect::<BTreeSet<_>>();
    let one_samples = worker_order.iter().filter(|workers| **workers == 1).count();
    let two_samples = worker_order.iter().filter(|workers| **workers == 2).count();
    let blocks_are_counterbalanced =
        worker_order
            .chunks_exact(4)
            .enumerate()
            .all(|(index, block)| {
                let expected = if index % 2 == 0 {
                    [1, 2, 2, 1]
                } else {
                    [2, 1, 1, 2]
                };
                block == expected
            });
    if worker_order.len() < 8
        || !worker_order.len().is_multiple_of(4)
        || workers != BTreeSet::from([1, 2])
        || one_samples != two_samples
        || one_samples < ColdMatrixKind::TwoWorkerFollowUp.minimum_samples_per_worker()
        || !blocks_are_counterbalanced
    {
        return Err(invalid_input(
            "two-worker-follow-up --worker-order must contain alternating 1,2,2,1 / 2,1,1,2 blocks with at least four samples per worker",
        ));
    }
    Ok(())
}

fn parse_worker_order(value: &str) -> AnyResult<Vec<usize>> {
    let workers = value
        .split(',')
        .map(|part| {
            part.trim()
                .parse::<usize>()
                .map_err(|_| invalid_input("--worker-order must be comma-separated integers"))
        })
        .collect::<AnyResult<Vec<_>>>()?;
    if workers.is_empty() || workers.len() > MAX_MATRIX_RUNS {
        return Err(invalid_input("--worker-order must contain 1..=16 runs"));
    }
    if workers
        .iter()
        .any(|workers| !(1..=MAX_WORKERS).contains(workers))
    {
        return Err(invalid_input("each --worker-order value must be in 1..=64"));
    }
    Ok(workers)
}

fn parse_cancellation_phases(value: &str) -> AnyResult<Vec<IndexProgressPhase>> {
    if value.trim() == "none" {
        return Ok(Vec::new());
    }
    let mut phases = Vec::new();
    for part in value.split(',') {
        let phase = match part.trim() {
            "preparation" => IndexProgressPhase::Preparation,
            "relational_write" => IndexProgressPhase::RelationalWrite,
            "chunk_word_fts" => IndexProgressPhase::ChunkWordFts,
            "chunk_trigram_fts" => IndexProgressPhase::ChunkTrigramFts,
            "symbol_fts" => IndexProgressPhase::SymbolFts,
            "reference_fts" => IndexProgressPhase::ReferenceFts,
            "commit_and_checkpoint" => IndexProgressPhase::CommitAndCheckpoint,
            _ => {
                return Err(invalid_input(
                    "--cancellation-phases contains an unsupported phase",
                ));
            }
        };
        if !phases.contains(&phase) {
            phases.push(phase);
        }
    }
    Ok(phases)
}

fn prepare_corpus(args: &ColdMatrixArgs) -> AnyResult<PreparedColdCorpus> {
    let source = args.repository.canonicalize()?;
    let revision = git_output(&source, ["rev-parse", "HEAD"])?;
    if revision != args.expected_revision {
        return Err(invalid_input(
            "--repository HEAD does not match --expected-revision",
        ));
    }
    let status = git_output(&source, ["status", "--porcelain", "--untracked-files=all"])?;
    if !status.is_empty() {
        return Err(invalid_input(
            "--repository must be clean so the revision identifies the corpus",
        ));
    }
    let temporary_root = tempfile::tempdir()?;
    let root = temporary_root.path().join("repository");
    snapshot_repository(&source, &root, temporary_root.path())?;
    let config = Config::discover(
        &root,
        Some(temporary_root.path().join("corpus-probe.sqlite")),
    )?;
    let files = discover_files(&root, config.max_file_bytes)?;
    if files.is_empty() {
        return Err(invalid_input(
            "cold-matrix corpus has no ignore-visible files",
        ));
    }
    Ok(PreparedColdCorpus {
        _temporary_root: temporary_root,
        root,
        source_repository: args.repository_label.clone(),
        revision,
        files,
    })
}

fn corpus_report(corpus: &PreparedColdCorpus) -> ColdCorpusReport {
    let source_bytes = corpus.files.iter().map(|file| file.size_bytes).sum::<u64>();
    let mut extensions = BTreeMap::new();
    let mut max_directory_depth = 0usize;
    for file in &corpus.files {
        let path = Path::new(&file.relative_path);
        let extension = path
            .extension()
            .and_then(|extension| extension.to_str())
            .filter(|extension| !extension.is_empty())
            .unwrap_or("<none>")
            .to_ascii_lowercase();
        *extensions.entry(extension).or_insert(0) += 1;
        max_directory_depth = max_directory_depth.max(path.components().count().saturating_sub(1));
    }
    ColdCorpusReport {
        source_kind: "clean_git_worktree_snapshot",
        source_repository: corpus.source_repository.clone(),
        revision: corpus.revision.clone(),
        files: corpus.files.len(),
        source_bytes,
        mean_file_bytes: source_bytes as f64 / corpus.files.len() as f64,
        max_directory_depth,
        extensions,
    }
}

fn measure_cold_run(
    sequence: usize,
    workers: usize,
    corpus: &PreparedColdCorpus,
    run_root: &Path,
    policy: &MeasurementPolicy,
) -> AnyResult<ColdRunReport> {
    let database = run_root.join(format!("matrix-{sequence:02}-w{workers}.sqlite"));
    let output = run_root.join(format!("matrix-{sequence:02}-w{workers}.json"));
    let worker = ColdWorkerArgs {
        repository: corpus.root.clone(),
        database,
        sequence,
        workers,
        parity_queries: policy.parity_queries.clone(),
        sample_interval_ms: policy.sample_interval_ms,
        timeout_seconds: policy.timeout_seconds,
        target_phase: None,
        baseline: None,
        allow_missed_phase: false,
        output,
    };
    let maximum_wall = Duration::from_secs(
        policy
            .timeout_seconds
            .saturating_add(policy.cancellation_grace_seconds)
            .saturating_add(30),
    );
    match spawn_cold_worker(&worker, maximum_wall)? {
        ColdWorkerOutput::Matrix(report) => Ok(*report),
        ColdWorkerOutput::Cancellation(_) => Err(invalid_input(
            "cold worker returned a cancellation report for a matrix run",
        )),
    }
}

fn measure_cold_run_in_process(
    sequence: usize,
    workers: usize,
    repository: &Path,
    database: &Path,
    parity_queries: &[String],
    sample_interval_ms: u64,
    timeout_seconds: u64,
) -> AnyResult<ColdRunReport> {
    let started = Instant::now();
    let baseline = ProcessResourceBaseline::capture();
    let mut config = Config::discover(repository, Some(database.to_path_buf()))?;
    config.max_index_workers = workers;
    let storage = Storage::open(database)?;
    let indexer = Indexer::new(std::sync::Arc::new(config), storage.clone())?;
    let cancellation = CancellationToken::new();
    let monitored = run_profiled_monitored(
        &indexer,
        database,
        &cancellation,
        MonitorSettings {
            started,
            baseline,
            target: None,
            sample_interval_ms,
            timeout_seconds,
        },
    )?;
    if monitored.resources.timeout_triggered {
        return Err(invalid_input(
            "cold index exceeded --timeout-seconds before completing",
        ));
    }
    if monitored.resources.cancellation_grace_exceeded {
        return Err(invalid_input(
            "cold index exceeded the cancellation grace period",
        ));
    }
    let profiled = monitored.result?;
    let shape = index_shape(&storage, database)?;
    let logical_index_blake3 = logical_index_digest(database)?;
    let retrieval_blake3 = retrieval_digest(&storage, parity_queries)?;
    Ok(ColdRunReport {
        sequence,
        workers,
        wall_ms: monitored.wall.as_secs_f64() * 1_000.0,
        response: profiled.response,
        diagnostics: profiled.diagnostics,
        resources: monitored.resources,
        shape,
        final_storage_footprint: storage_footprint(database)?,
        logical_index_blake3,
        retrieval_blake3,
    })
}

fn spawn_cold_worker(args: &ColdWorkerArgs, maximum_wall: Duration) -> AnyResult<ColdWorkerOutput> {
    let executable = std::env::current_exe()?;
    let stderr_path = args.output.with_extension("stderr.log");
    let stderr = fs::File::create(&stderr_path)?;
    let mut command = Command::new(executable);
    command
        .arg("cold-worker")
        .arg("--repository")
        .arg(&args.repository)
        .arg("--database")
        .arg(&args.database)
        .arg("--sequence")
        .arg(args.sequence.to_string())
        .arg("--workers")
        .arg(args.workers.to_string())
        .arg("--sample-interval-ms")
        .arg(args.sample_interval_ms.to_string())
        .arg("--timeout-seconds")
        .arg(args.timeout_seconds.to_string())
        .arg("--output")
        .arg(&args.output)
        .stdout(Stdio::null())
        .stderr(Stdio::from(stderr));
    for query in &args.parity_queries {
        command.arg("--parity-query").arg(query);
    }
    if let Some(target_phase) = &args.target_phase {
        command.arg("--target-phase").arg(target_phase);
    }
    if let Some(baseline) = &args.baseline {
        command.arg("--baseline").arg(baseline);
    }
    if args.allow_missed_phase {
        command.arg("--allow-missed-phase");
    }

    let mut child = command.spawn()?;
    let started = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if started.elapsed() >= maximum_wall {
            let _ = child.kill();
            let status = child.wait()?;
            let stderr = bounded_stderr(&stderr_path);
            return Err(invalid_input(&format!(
                "cold worker exceeded its hard subprocess bound ({status}): {stderr}"
            )));
        }
        std::thread::sleep(Duration::from_millis(25));
    };
    if !status.success() {
        let stderr = bounded_stderr(&stderr_path);
        return Err(invalid_input(&format!(
            "cold worker failed ({status}): {stderr}"
        )));
    }
    let output = serde_json::from_slice(&read_bounded_file(
        &args.output,
        MAX_WORKER_REPORT_BYTES,
        "cold worker report",
    )?)?;
    Ok(output)
}

fn bounded_stderr(path: &Path) -> String {
    let Ok(mut file) = fs::File::open(path) else {
        return String::new();
    };
    let length = file.metadata().map(|metadata| metadata.len()).unwrap_or(0);
    let start = length.saturating_sub(8 * 1024);
    if file.seek(SeekFrom::Start(start)).is_err() {
        return String::new();
    }
    let mut bytes = Vec::with_capacity(usize::try_from(length - start).unwrap_or(0));
    if file.take(8 * 1024).read_to_end(&mut bytes).is_err() {
        return String::new();
    }
    String::from_utf8_lossy(&bytes).trim().to_owned()
}

fn read_bounded_file(path: &Path, maximum_bytes: u64, label: &str) -> AnyResult<Vec<u8>> {
    let file = fs::File::open(path)?;
    let length = file.metadata()?.len();
    if length > maximum_bytes {
        return Err(invalid_input(&format!(
            "{label} exceeds its {maximum_bytes}-byte bound"
        )));
    }
    let mut bytes = Vec::with_capacity(usize::try_from(length)?);
    file.take(maximum_bytes.saturating_add(1))
        .read_to_end(&mut bytes)?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > maximum_bytes {
        return Err(invalid_input(&format!(
            "{label} exceeds its {maximum_bytes}-byte bound"
        )));
    }
    Ok(bytes)
}

pub(super) fn run_worker(args: &ColdWorkerArgs) -> AnyResult<()> {
    if !args.repository.is_dir() {
        return Err(invalid_input("cold worker repository does not exist"));
    }
    if !(1..=MAX_WORKERS).contains(&args.workers) {
        return Err(invalid_input("cold worker count is out of bounds"));
    }
    if !(1..=MAX_SAMPLE_INTERVAL_MS).contains(&args.sample_interval_ms)
        || !(1..=MAX_TIMEOUT_SECONDS).contains(&args.timeout_seconds)
    {
        return Err(invalid_input("cold worker timing bounds are invalid"));
    }
    if args.parity_queries.is_empty() || args.parity_queries.len() > MAX_PARITY_QUERIES {
        return Err(invalid_input("cold worker parity query count is invalid"));
    }
    if args
        .parity_queries
        .iter()
        .any(|query| query.is_empty() || query.len() > MAX_PARITY_QUERY_BYTES)
    {
        return Err(invalid_input("cold worker parity query size is invalid"));
    }
    if args.sequence >= MAX_MATRIX_RUNS {
        return Err(invalid_input("cold worker sequence is out of bounds"));
    }
    if storage_footprint(&args.database)?.total_bytes != 0 {
        return Err(invalid_input("cold worker database must be fresh"));
    }
    let output = if let Some(target) = &args.target_phase {
        let phases = parse_cancellation_phases(target)?;
        let phase = phases
            .first()
            .copied()
            .ok_or_else(|| invalid_input("cold worker target phase is missing"))?;
        if phases.len() != 1 {
            return Err(invalid_input(
                "cold worker accepts exactly one cancellation phase",
            ));
        }
        let baseline_path = args
            .baseline
            .as_deref()
            .ok_or_else(|| invalid_input("cancellation worker requires --baseline"))?;
        let baseline: ParityReport = serde_json::from_slice(&read_bounded_file(
            baseline_path,
            MAX_BASELINE_BYTES,
            "cold worker baseline",
        )?)?;
        ColdWorkerOutput::Cancellation(Box::new(measure_cancellation_probe_in_process(
            phase,
            CancellationProbeInput {
                workers: args.workers,
                repository: &args.repository,
                database: &args.database,
                parity_queries: &args.parity_queries,
                sample_interval_ms: args.sample_interval_ms,
                timeout_seconds: args.timeout_seconds,
                require_observation: !args.allow_missed_phase,
                parity: &baseline,
            },
        )?))
    } else {
        if args.baseline.is_some() {
            return Err(invalid_input(
                "matrix cold worker does not accept --baseline",
            ));
        }
        ColdWorkerOutput::Matrix(Box::new(measure_cold_run_in_process(
            args.sequence,
            args.workers,
            &args.repository,
            &args.database,
            &args.parity_queries,
            args.sample_interval_ms,
            args.timeout_seconds,
        )?))
    };
    let json = serde_json::to_string_pretty(&output)?;
    if u64::try_from(json.len()).unwrap_or(u64::MAX) > MAX_WORKER_REPORT_BYTES {
        return Err(invalid_input("cold worker report exceeds its byte bound"));
    }
    if let Some(parent) = args.output.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&args.output, format!("{json}\n"))?;
    Ok(())
}

fn run_profiled_monitored(
    indexer: &Indexer,
    database: &Path,
    cancellation: &CancellationToken,
    settings: MonitorSettings,
) -> AnyResult<MonitoredProfile> {
    let MonitorSettings {
        started,
        baseline,
        target,
        sample_interval_ms,
        timeout_seconds,
    } = settings;
    let deadline = started + Duration::from_secs(timeout_seconds);
    let grace_deadline = deadline + Duration::from_secs(CANCELLATION_GRACE_SECONDS);
    let mut sampler = ResourceSampler::new(database, sample_interval_ms, baseline)?;
    sampler.sample(
        indexer
            .progress_snapshot()
            .and_then(|snapshot| snapshot.phase),
    )?;

    let (sender, receiver) = mpsc::sync_channel(1);
    let worker = indexer.clone();
    let worker_cancellation = cancellation.clone();
    let handle = std::thread::spawn(move || {
        let result = worker.reconcile_cancellable_profiled(false, &worker_cancellation);
        let _ = sender.send(result);
    });
    let interval = Duration::from_millis(sample_interval_ms);
    let mut timeout_triggered = false;
    let mut cancellation_grace_exceeded = false;
    let mut target_observed = false;
    let mut cancellation_requested_at = None;
    let result = loop {
        let phase = indexer
            .progress_snapshot()
            .and_then(|snapshot| snapshot.phase);
        sampler.sample(phase)?;
        if let Some(MonitoredTarget::Phase(target_phase)) = target
            && phase == Some(target_phase)
            && cancellation_requested_at.is_none()
        {
            target_observed = true;
            cancellation_requested_at = Some(Instant::now());
            cancellation.cancel();
        }
        if Instant::now() >= deadline && !timeout_triggered {
            timeout_triggered = true;
            cancellation.cancel();
        }
        if Instant::now() >= grace_deadline && !handle.is_finished() {
            cancellation_grace_exceeded = true;
        }
        match receiver.recv_timeout(interval) {
            Ok(result) => break result,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return Err(invalid_input("index worker stopped without a result"));
            }
        }
    };
    handle
        .join()
        .map_err(|_| invalid_input("index worker panicked"))?;
    sampler.sample(
        indexer
            .progress_snapshot()
            .and_then(|snapshot| snapshot.phase),
    )?;
    let resources = sampler.finish(timeout_triggered, cancellation_grace_exceeded)?;
    Ok(MonitoredProfile {
        result,
        wall: started.elapsed(),
        resources,
        target_observed,
        cancellation_requested_at,
    })
}

impl ResourceSampler {
    fn new(
        database: &Path,
        sample_interval_ms: u64,
        baseline: ProcessResourceBaseline,
    ) -> AnyResult<Self> {
        let database = database.to_path_buf();
        Ok(Self {
            peak_storage: storage_footprint(&database)?,
            database,
            sample_interval_ms,
            clock_ticks_per_second: clock_ticks_per_second(),
            initial_cpu_ticks: baseline.cpu_ticks,
            previous_cpu_ticks: baseline.cpu_ticks,
            initial_write_bytes: baseline.write_bytes,
            previous_write_bytes: baseline.write_bytes,
            samples: 0,
            peak_rss_bytes: max_option(baseline.rss_bytes, process_rss_bytes()),
            phases: BTreeMap::new(),
        })
    }

    fn sample(&mut self, phase: Option<IndexProgressPhase>) -> AnyResult<()> {
        let key = phase_name(phase).to_owned();
        let cpu_ticks = process_cpu_ticks();
        let write_bytes = process_write_bytes();
        let rss = process_rss_bytes();
        let footprint = storage_footprint(&self.database)?;
        self.samples = self.samples.saturating_add(1);
        self.peak_rss_bytes = max_option(self.peak_rss_bytes, rss);
        merge_footprint(&mut self.peak_storage, &footprint);

        let current = self.phases.entry(key).or_default();
        current.samples = current.samples.saturating_add(1);
        if let (Some(previous), Some(current_ticks)) = (self.previous_cpu_ticks, cpu_ticks) {
            current.cpu_ticks = Some(
                current
                    .cpu_ticks
                    .unwrap_or(0)
                    .saturating_add(current_ticks.total().saturating_sub(previous.total())),
            );
        }
        if let (Some(previous), Some(current_bytes)) = (self.previous_write_bytes, write_bytes) {
            current.write_bytes = Some(
                current
                    .write_bytes
                    .unwrap_or(0)
                    .saturating_add(current_bytes.saturating_sub(previous)),
            );
        }
        current.peak_rss_bytes = max_option(current.peak_rss_bytes, rss);
        current.peak_database_bytes = current.peak_database_bytes.max(footprint.database_bytes);
        current.peak_wal_bytes = current.peak_wal_bytes.max(footprint.wal_bytes);
        current.peak_shm_bytes = current.peak_shm_bytes.max(footprint.shm_bytes);
        current.peak_total_storage_bytes =
            current.peak_total_storage_bytes.max(footprint.total_bytes);
        self.previous_cpu_ticks = cpu_ticks;
        self.previous_write_bytes = write_bytes;
        Ok(())
    }

    fn finish(
        self,
        timeout_triggered: bool,
        cancellation_grace_exceeded: bool,
    ) -> AnyResult<ResourceReport> {
        let user_cpu_ticks = self
            .previous_cpu_ticks
            .zip(self.initial_cpu_ticks)
            .map(|(after, before)| after.user.saturating_sub(before.user));
        let system_cpu_ticks = self
            .previous_cpu_ticks
            .zip(self.initial_cpu_ticks)
            .map(|(after, before)| after.system.saturating_sub(before.system));
        let cpu_ticks = user_cpu_ticks
            .zip(system_cpu_ticks)
            .map(|(user, system)| user.saturating_add(system));
        let process_write_bytes = self
            .previous_write_bytes
            .zip(self.initial_write_bytes)
            .map(|(after, before)| after.saturating_sub(before));
        let by_phase = self
            .phases
            .into_iter()
            .map(|(phase, value)| {
                (
                    phase,
                    PhaseResourceReport {
                        samples: value.samples,
                        approximate_cpu_ms: ticks_to_milliseconds(
                            value.cpu_ticks,
                            self.clock_ticks_per_second,
                        ),
                        approximate_process_write_bytes: value.write_bytes,
                        peak_rss_bytes: value.peak_rss_bytes,
                        peak_database_bytes: value.peak_database_bytes,
                        peak_wal_bytes: value.peak_wal_bytes,
                        peak_shm_bytes: value.peak_shm_bytes,
                        peak_total_storage_bytes: value.peak_total_storage_bytes,
                    },
                )
            })
            .collect();
        Ok(ResourceReport {
            sample_interval_ms: self.sample_interval_ms,
            samples: self.samples,
            cpu_ms: ticks_to_milliseconds(cpu_ticks, self.clock_ticks_per_second),
            process_user_cpu_ms: ticks_to_milliseconds(user_cpu_ticks, self.clock_ticks_per_second),
            process_system_cpu_ms: ticks_to_milliseconds(
                system_cpu_ticks,
                self.clock_ticks_per_second,
            ),
            process_write_bytes,
            peak_rss_bytes: self.peak_rss_bytes,
            peak_storage_footprint: self.peak_storage,
            by_phase,
            timeout_triggered,
            cancellation_grace_exceeded,
        })
    }
}

fn phase_name(phase: Option<IndexProgressPhase>) -> &'static str {
    match phase {
        None => "startup",
        Some(IndexProgressPhase::Discovery) => "discovery",
        Some(IndexProgressPhase::HashAndPlan) => "hash_and_plan",
        Some(IndexProgressPhase::Preparation) => "preparation",
        Some(IndexProgressPhase::RelationalWrite) => "relational_write",
        Some(IndexProgressPhase::ChunkWordFts) => "chunk_word_fts",
        Some(IndexProgressPhase::ChunkTrigramFts) => "chunk_trigram_fts",
        Some(IndexProgressPhase::SymbolFts) => "symbol_fts",
        Some(IndexProgressPhase::ReferenceFts) => "reference_fts",
        Some(IndexProgressPhase::CommitAndCheckpoint) => "commit_and_checkpoint",
        Some(IndexProgressPhase::Completed) => "completed",
        Some(IndexProgressPhase::Failed) => "failed",
        Some(IndexProgressPhase::Cancelled) => "cancelled",
    }
}

fn merge_footprint(target: &mut StorageFootprint, value: &StorageFootprint) {
    target.database_bytes = target.database_bytes.max(value.database_bytes);
    target.wal_bytes = target.wal_bytes.max(value.wal_bytes);
    target.shm_bytes = target.shm_bytes.max(value.shm_bytes);
    target.total_bytes = target.total_bytes.max(value.total_bytes);
}

fn index_shape(storage: &Storage, database: &Path) -> AnyResult<IndexShape> {
    let StorageCounts {
        files,
        chunks,
        symbols,
        source_bytes,
        languages,
    } = storage.counts()?;
    let connection = open_read_only(database)?;
    let references = query_count(&connection, "SELECT COUNT(*) FROM symbol_refs")?;
    let imports = query_count(&connection, "SELECT COUNT(*) FROM imports")?;
    Ok(IndexShape {
        files,
        chunks,
        symbols,
        references,
        imports,
        source_bytes,
        languages: languages.into_iter().collect(),
    })
}

fn query_count(connection: &Connection, sql: &str) -> AnyResult<usize> {
    let value = connection.query_row(sql, [], |row| row.get::<_, i64>(0))?;
    usize::try_from(value).map_err(|_| invalid_input("negative or oversized SQLite count"))
}

fn logical_index_digest(database: &Path) -> AnyResult<String> {
    let connection = open_read_only(database)?;
    let mut hasher = blake3::Hasher::new();
    for (label, sql) in [
        (
            "files",
            "SELECT path, language, structurally_complete, size_bytes, modified_ns, content_hash, source_token_count, source_tokenizer FROM files ORDER BY path",
        ),
        (
            "chunks",
            "SELECT files.path, chunks.content, chunks.start_line, chunks.end_line, chunks.start_byte, chunks.end_byte, chunks.token_count FROM chunks JOIN files ON files.id = chunks.file_id ORDER BY files.path, chunks.start_byte, chunks.end_byte, chunks.id",
        ),
        (
            "symbols",
            "SELECT files.path, symbols.name, symbols.kind, symbols.parent, symbols.signature, symbols.start_line, symbols.end_line, symbols.start_byte, symbols.end_byte FROM symbols JOIN files ON files.id = symbols.file_id ORDER BY files.path, symbols.start_byte, symbols.end_byte, symbols.name, symbols.kind",
        ),
        (
            "references",
            "SELECT files.path, symbol_refs.name, symbol_refs.kind, symbol_refs.role, symbol_refs.enclosing_symbol, symbol_refs.start_line, symbol_refs.end_line, symbol_refs.start_byte, symbol_refs.end_byte FROM symbol_refs JOIN files ON files.id = symbol_refs.file_id ORDER BY files.path, symbol_refs.start_byte, symbol_refs.end_byte, symbol_refs.name, symbol_refs.role",
        ),
        (
            "imports",
            "SELECT files.path, imports.raw_target, imports.resolved_path, imports.line FROM imports JOIN files ON files.id = imports.file_id ORDER BY files.path, imports.line, imports.raw_target, imports.resolved_path",
        ),
        (
            "import_candidates",
            "SELECT files.path, imports.raw_target, imports.line, import_candidates.candidate_path FROM import_candidates JOIN imports ON imports.id = import_candidates.import_id JOIN files ON files.id = imports.file_id ORDER BY files.path, imports.line, imports.raw_target, import_candidates.candidate_path",
        ),
        (
            "path_entries",
            "SELECT path_entries.path, path_entries.depth, files.path FROM path_entries LEFT JOIN files ON files.id = path_entries.file_id ORDER BY path_entries.path",
        ),
    ] {
        update_query_digest(&connection, &mut hasher, label, sql)?;
    }
    Ok(hasher.finalize().to_hex().to_string())
}

fn update_query_digest(
    connection: &Connection,
    hasher: &mut blake3::Hasher,
    label: &str,
    sql: &str,
) -> AnyResult<()> {
    update_bytes(hasher, label.as_bytes());
    let mut statement = connection.prepare(sql)?;
    let column_count = statement.column_count();
    let mut rows = statement.query([])?;
    let mut row_count = 0u64;
    while let Some(row) = rows.next()? {
        row_count = row_count.saturating_add(1);
        hasher.update(b"row\0");
        for column in 0..column_count {
            match row.get_ref(column)? {
                ValueRef::Null => {
                    hasher.update(b"null\0");
                }
                ValueRef::Integer(value) => {
                    hasher.update(b"integer\0");
                    hasher.update(&value.to_le_bytes());
                }
                ValueRef::Real(value) => {
                    hasher.update(b"real\0");
                    hasher.update(&value.to_bits().to_le_bytes());
                }
                ValueRef::Text(value) => {
                    hasher.update(b"text\0");
                    update_bytes(hasher, value);
                }
                ValueRef::Blob(value) => {
                    hasher.update(b"blob\0");
                    update_bytes(hasher, value);
                }
            }
        }
    }
    hasher.update(&row_count.to_le_bytes());
    Ok(())
}

fn retrieval_digest(storage: &Storage, queries: &[String]) -> AnyResult<String> {
    let mut hasher = blake3::Hasher::new();
    for query in queries {
        update_bytes(&mut hasher, query.as_bytes());
        for hit in storage.search_word(query, 100)? {
            hasher.update(b"word\0");
            update_chunk_hit(&mut hasher, &hit);
        }
        for hit in storage.search_trigram(query, 100)? {
            hasher.update(b"trigram\0");
            update_chunk_hit(&mut hasher, &hit);
        }
        for hit in storage.search_symbols(query, false, 100)? {
            hasher.update(b"symbol\0");
            update_bytes(&mut hasher, hit.path.as_bytes());
            update_bytes(&mut hasher, hit.content_hash.as_bytes());
            update_bytes(&mut hasher, hit.symbol.name.as_bytes());
            update_bytes(&mut hasher, hit.symbol.kind.as_bytes());
            update_optional_string(&mut hasher, hit.symbol.parent.as_deref());
            update_optional_string(&mut hasher, hit.symbol.signature.as_deref());
            update_usize(&mut hasher, hit.symbol.start_line);
            update_usize(&mut hasher, hit.symbol.end_line);
            update_usize(&mut hasher, hit.symbol.start_byte);
            update_usize(&mut hasher, hit.symbol.end_byte);
        }
        for hit in storage.search_references(query, false, 100)? {
            hasher.update(b"reference\0");
            update_bytes(&mut hasher, hit.path.as_bytes());
            update_bytes(&mut hasher, hit.content_hash.as_bytes());
            update_bytes(&mut hasher, hit.reference.name.as_bytes());
            update_bytes(&mut hasher, hit.reference.kind.as_bytes());
            update_bytes(
                &mut hasher,
                serde_json::to_string(&hit.reference.role)?.as_bytes(),
            );
            update_optional_string(&mut hasher, hit.reference.enclosing_symbol.as_deref());
            update_usize(&mut hasher, hit.reference.start_line);
            update_usize(&mut hasher, hit.reference.end_line);
            update_usize(&mut hasher, hit.reference.start_byte);
            update_usize(&mut hasher, hit.reference.end_byte);
        }
    }
    Ok(hasher.finalize().to_hex().to_string())
}

fn update_chunk_hit(hasher: &mut blake3::Hasher, hit: &leantoken::storage::ChunkHit) {
    update_bytes(hasher, hit.path.as_bytes());
    update_bytes(hasher, hit.content.as_bytes());
    update_usize(hasher, hit.start_line);
    update_usize(hasher, hit.end_line);
    update_usize(hasher, hit.start_byte);
    update_usize(hasher, hit.end_byte);
    update_usize(hasher, hit.token_count);
    hasher.update(&hit.score.to_bits().to_le_bytes());
}

fn update_optional_string(hasher: &mut blake3::Hasher, value: Option<&str>) {
    if let Some(value) = value {
        hasher.update(b"some\0");
        update_bytes(hasher, value.as_bytes());
    } else {
        hasher.update(b"none\0");
    }
}

fn update_usize(hasher: &mut blake3::Hasher, value: usize) {
    hasher.update(&u64::try_from(value).unwrap_or(u64::MAX).to_le_bytes());
}

fn update_bytes(hasher: &mut blake3::Hasher, value: &[u8]) {
    hasher.update(&u64::try_from(value.len()).unwrap_or(u64::MAX).to_le_bytes());
    hasher.update(value);
}

fn open_read_only(database: &Path) -> AnyResult<Connection> {
    Ok(Connection::open_with_flags(
        database,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?)
}

fn require_parity(parity: &mut Option<ParityReport>, run: &ColdRunReport) -> AnyResult<()> {
    if run.response.repository_generation == 0 {
        return Err(invalid_input(
            "cold run did not publish a complete generation",
        ));
    }
    let observed = ParityReport {
        logical_index_blake3: run.logical_index_blake3.clone(),
        retrieval_blake3: run.retrieval_blake3.clone(),
        shape: run.shape.clone(),
        complete: true,
    };
    if let Some(expected) = parity {
        if expected.logical_index_blake3 != observed.logical_index_blake3
            || expected.retrieval_blake3 != observed.retrieval_blake3
            || expected.shape != observed.shape
        {
            return Err(invalid_input(
                "worker matrix changed logical index or retrieval output",
            ));
        }
    } else {
        *parity = Some(observed);
    }
    Ok(())
}

fn measure_cancellation_probes(
    corpus: &PreparedColdCorpus,
    run_root: &Path,
    policy: &MeasurementPolicy,
    parity: &ParityReport,
) -> AnyResult<Vec<CancellationProbeReport>> {
    let workers = policy.worker_order.iter().copied().max().unwrap_or(1);
    let baseline = run_root.join("parity-baseline.json");
    fs::write(&baseline, serde_json::to_vec_pretty(parity)?)?;
    let mut reports = Vec::with_capacity(policy.cancellation_phases.len());
    for (sequence, phase) in policy.cancellation_phases.iter().copied().enumerate() {
        let database = run_root.join(format!(
            "cancel-{sequence:02}-{}.sqlite",
            phase_name(Some(phase))
        ));
        let output = run_root.join(format!(
            "cancel-{sequence:02}-{}.json",
            phase_name(Some(phase))
        ));
        let worker = ColdWorkerArgs {
            repository: corpus.root.clone(),
            database,
            sequence,
            workers,
            parity_queries: policy.parity_queries.clone(),
            sample_interval_ms: policy.sample_interval_ms,
            timeout_seconds: policy.timeout_seconds,
            target_phase: Some(phase_name(Some(phase)).into()),
            baseline: Some(baseline.clone()),
            allow_missed_phase: !policy.require_cancellation_observation,
            output,
        };
        let one_attempt = policy
            .timeout_seconds
            .saturating_add(policy.cancellation_grace_seconds);
        let maximum_wall = Duration::from_secs(one_attempt.saturating_mul(2).saturating_add(30));
        match spawn_cold_worker(&worker, maximum_wall)? {
            ColdWorkerOutput::Cancellation(report) => reports.push(*report),
            ColdWorkerOutput::Matrix(_) => {
                return Err(invalid_input(
                    "cold worker returned a matrix report for a cancellation probe",
                ));
            }
        }
    }
    Ok(reports)
}

struct CancellationProbeInput<'a> {
    workers: usize,
    repository: &'a Path,
    database: &'a Path,
    parity_queries: &'a [String],
    sample_interval_ms: u64,
    timeout_seconds: u64,
    require_observation: bool,
    parity: &'a ParityReport,
}

fn measure_cancellation_probe_in_process(
    phase: IndexProgressPhase,
    input: CancellationProbeInput<'_>,
) -> AnyResult<CancellationProbeReport> {
    let CancellationProbeInput {
        workers,
        repository,
        database,
        parity_queries,
        sample_interval_ms,
        timeout_seconds,
        require_observation,
        parity,
    } = input;
    let started = Instant::now();
    let baseline = ProcessResourceBaseline::capture();
    let mut config = Config::discover(repository, Some(database.to_path_buf()))?;
    config.max_index_workers = workers;
    let storage = Storage::open(database)?;
    let indexer = Indexer::new(std::sync::Arc::new(config), storage.clone())?;
    let cancellation = CancellationToken::new();
    let monitored = run_profiled_monitored(
        &indexer,
        database,
        &cancellation,
        MonitorSettings {
            started,
            baseline,
            target: Some(MonitoredTarget::Phase(phase)),
            sample_interval_ms,
            timeout_seconds,
        },
    )?;
    if monitored.resources.timeout_triggered {
        return Err(invalid_input(
            "cancellation probe exceeded --timeout-seconds",
        ));
    }
    if monitored.resources.cancellation_grace_exceeded {
        return Err(invalid_input(
            "cancellation probe exceeded the cancellation grace period",
        ));
    }
    if require_observation && !monitored.target_observed {
        return Err(invalid_input(
            "requested cancellation phase was too short to observe; reduce --sample-interval-ms or report the incomplete probe explicitly",
        ));
    }

    let cancellation_to_return_ms = monitored
        .cancellation_requested_at
        .map(|requested| requested.elapsed().as_secs_f64() * 1_000.0);
    let result = match &monitored.result {
        Ok(_) if monitored.cancellation_requested_at.is_some() => "completed_after_cancellation",
        Ok(_) => "completed_without_observation",
        Err(leantoken::Error::Cancelled) => "cancelled",
        Err(error) => {
            return Err(invalid_input(&format!(
                "cancellation probe failed unexpectedly: {error}"
            )));
        }
    }
    .to_owned();
    let generation_after_attempt = storage.repository_generation()?;
    let footprint_after_attempt = storage_footprint(database)?;

    let restart_started = Instant::now();
    let restart_baseline = ProcessResourceBaseline::capture();
    let restart_cancellation = CancellationToken::new();
    let restart = run_profiled_monitored(
        &indexer,
        database,
        &restart_cancellation,
        MonitorSettings {
            started: restart_started,
            baseline: restart_baseline,
            target: None,
            sample_interval_ms,
            timeout_seconds,
        },
    )?;
    if restart.resources.timeout_triggered {
        return Err(invalid_input(
            "restart after cancellation exceeded --timeout-seconds",
        ));
    }
    if restart.resources.cancellation_grace_exceeded {
        return Err(invalid_input(
            "restart after cancellation exceeded the cancellation grace period",
        ));
    }
    restart.result?;
    let restart_generation = storage.repository_generation()?;
    let restart_logical_index_blake3 = logical_index_digest(database)?;
    let restart_retrieval_blake3 = retrieval_digest(&storage, parity_queries)?;
    let restart_matches_baseline = restart_generation > 0
        && restart_logical_index_blake3 == parity.logical_index_blake3
        && restart_retrieval_blake3 == parity.retrieval_blake3
        && index_shape(&storage, database)? == parity.shape;
    if !restart_matches_baseline {
        return Err(invalid_input(
            "restart after cancellation changed logical index or retrieval output",
        ));
    }

    Ok(CancellationProbeReport {
        target_phase: phase,
        workers,
        phase_observed: monitored.target_observed,
        cancellation_requested: monitored.cancellation_requested_at.is_some(),
        cancellation_to_return_ms,
        result,
        attempt_wall_ms: monitored.wall.as_secs_f64() * 1_000.0,
        resources: monitored.resources,
        generation_after_attempt,
        footprint_after_attempt,
        restart_wall_ms: restart.wall.as_secs_f64() * 1_000.0,
        restart_generation,
        restart_logical_index_blake3,
        restart_retrieval_blake3,
        restart_matches_baseline,
    })
}

fn summarize_workers(runs: &[ColdRunReport]) -> Vec<WorkerSummary> {
    let workers = runs.iter().map(|run| run.workers).collect::<BTreeSet<_>>();
    workers
        .into_iter()
        .map(|workers| {
            let matching = runs
                .iter()
                .filter(|run| run.workers == workers)
                .collect::<Vec<_>>();
            let mut wall = matching.iter().map(|run| run.wall_ms).collect::<Vec<_>>();
            wall.sort_by(f64::total_cmp);
            WorkerSummary {
                workers,
                samples: matching.len(),
                wall_p50_ms: percentile(&wall, 0.50),
                wall_p95_ms: percentile(&wall, 0.95),
                mean_cpu_ms: mean_option(matching.iter().map(|run| run.resources.cpu_ms)),
                peak_rss_bytes: matching
                    .iter()
                    .filter_map(|run| run.resources.peak_rss_bytes)
                    .max(),
                mean_process_write_bytes: mean_option(
                    matching
                        .iter()
                        .map(|run| run.resources.process_write_bytes.map(|value| value as f64)),
                ),
                max_final_storage_bytes: matching
                    .iter()
                    .map(|run| run.final_storage_footprint.total_bytes)
                    .max()
                    .unwrap_or(0),
            }
        })
        .collect()
}

fn decide(
    runs: &[ColdRunReport],
    summaries: &[WorkerSummary],
    policy: &MeasurementPolicy,
) -> DecisionReport {
    let baseline_workers = 1;
    let baseline_runs = runs
        .iter()
        .filter(|run| run.workers == baseline_workers)
        .collect::<Vec<_>>();
    let phase_totals = diagnostic_phase_totals(&baseline_runs);
    let total_ms = phase_totals.values().copied().sum::<f64>();
    let (dominant_phase, dominant_ms) = phase_totals
        .iter()
        .max_by(|left, right| left.1.total_cmp(right.1))
        .map(|(phase, milliseconds)| (phase.clone(), *milliseconds))
        .unwrap_or_else(|| ("unavailable".into(), 0.0));
    let dominant_phase_share = if total_ms > 0.0 {
        dominant_ms / total_ms
    } else {
        0.0
    };
    let baseline = summaries
        .iter()
        .find(|summary| summary.workers == baseline_workers);
    let mut comparisons = Vec::new();
    if let Some(baseline) = baseline {
        for candidate in summaries
            .iter()
            .filter(|candidate| candidate.workers != baseline_workers)
        {
            let wall_reduction = relative_reduction(baseline.wall_p50_ms, candidate.wall_p50_ms);
            let wall_p95_reduction =
                relative_reduction(baseline.wall_p95_ms, candidate.wall_p95_ms);
            let cpu_increase =
                relative_increase_option(baseline.mean_cpu_ms, candidate.mean_cpu_ms);
            let peak_rss_increase = relative_increase_option(
                baseline.peak_rss_bytes.map(|value| value as f64),
                candidate.peak_rss_bytes.map(|value| value as f64),
            );
            let write_increase = relative_increase_option(
                baseline.mean_process_write_bytes,
                candidate.mean_process_write_bytes,
            );
            let footprint_increase = relative_increase(
                baseline.max_final_storage_bytes as f64,
                candidate.max_final_storage_bytes as f64,
            );
            let passes = wall_reduction >= policy.minimum_wall_reduction
                && policy
                    .minimum_wall_p95_reduction
                    .is_none_or(|minimum| wall_p95_reduction >= minimum)
                && cpu_increase.is_some_and(|value| value <= policy.maximum_cpu_increase)
                && peak_rss_increase.is_some_and(|value| value <= policy.maximum_peak_rss_increase)
                && write_increase.is_some_and(|value| value <= policy.maximum_write_increase)
                && footprint_increase <= policy.maximum_footprint_increase;
            comparisons.push(WorkerComparison {
                workers: candidate.workers,
                wall_reduction,
                wall_p95_reduction,
                cpu_increase,
                peak_rss_increase,
                write_increase,
                footprint_increase,
                passes,
            });
        }
    }
    let preparation_owns_cost = dominant_phase == "preparation"
        && dominant_phase_share >= policy.preparation_owner_threshold;
    let sample_counts_complete = summaries
        .iter()
        .all(|summary| summary.samples >= policy.minimum_samples_per_worker);
    let candidate_workers = (preparation_owns_cost && sample_counts_complete)
        .then(|| {
            comparisons
                .iter()
                .filter(|comparison| comparison.passes)
                .max_by(|left, right| left.wall_reduction.total_cmp(&right.wall_reduction))
                .map(|comparison| comparison.workers)
        })
        .flatten();
    let (outcome, rationale) = if baseline.is_none() {
        (
            "insufficient_evidence",
            "The matrix did not contain a one-worker baseline.".into(),
        )
    } else if !sample_counts_complete {
        (
            "insufficient_evidence",
            format!(
                "Every worker arm requires at least {} samples before this matrix can make a decision.",
                policy.minimum_samples_per_worker
            ),
        )
    } else if !preparation_owns_cost {
        (
            "optimize_measured_owner",
            format!(
                "{dominant_phase} owns {:.1}% of measured leaf-phase time, so changing preparation workers is not justified.",
                dominant_phase_share * 100.0
            ),
        )
    } else if let Some(workers) = candidate_workers {
        match policy.matrix_kind {
            ColdMatrixKind::Screening => (
                "candidate_for_follow_up",
                format!(
                    "{workers} workers passed the preregistered wall, CPU, RSS, write, and footprint thresholds; this measurement PR does not change production defaults."
                ),
            ),
            ColdMatrixKind::TwoWorkerFollowUp => (
                "candidate_for_mcp_contention_measurement",
                format!(
                    "{workers} workers passed the preregistered p50, p95, CPU, RSS, write, and footprint thresholds; production defaults remain unchanged until MCP contention and multi-process evidence also passes."
                ),
            ),
        }
    } else {
        (
            "keep_current_worker_default",
            "No candidate passed every preregistered resource and wall-time threshold.".into(),
        )
    };
    DecisionReport {
        baseline_workers,
        dominant_phase,
        dominant_phase_share,
        candidate_workers,
        outcome,
        rationale,
        comparisons,
    }
}

fn diagnostic_phase_totals(runs: &[&ColdRunReport]) -> BTreeMap<String, f64> {
    let mut totals = BTreeMap::new();
    for run in runs {
        let publication = &run.diagnostics.publication_detail;
        for (phase, milliseconds) in [
            ("discovery", run.diagnostics.discovery_ms),
            ("hash_and_plan", run.diagnostics.hash_and_plan_ms),
            ("preparation", run.diagnostics.preparation_ms),
            ("relational_write", publication.relational_write_ms),
            ("chunk_word_fts", publication.chunk_word_fts_rebuild_ms),
            (
                "chunk_trigram_fts",
                publication.chunk_trigram_fts_rebuild_ms,
            ),
            ("symbol_fts", publication.symbol_fts_rebuild_ms),
            ("reference_fts", publication.reference_fts_rebuild_ms),
            (
                "commit_and_checkpoint",
                publication.commit_ms + publication.checkpoint_ms,
            ),
        ] {
            *totals.entry(phase.to_owned()).or_insert(0.0) += milliseconds;
        }
    }
    totals
}

fn percentile(sorted: &[f64], percentile: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let rank = (sorted.len() as f64 * percentile).ceil() as usize;
    sorted[rank.saturating_sub(1).min(sorted.len() - 1)]
}

fn mean_option(values: impl Iterator<Item = Option<f64>>) -> Option<f64> {
    let values = values.collect::<Option<Vec<_>>>()?;
    (!values.is_empty()).then(|| values.iter().sum::<f64>() / values.len() as f64)
}

fn relative_reduction(baseline: f64, candidate: f64) -> f64 {
    if baseline > 0.0 {
        (baseline - candidate) / baseline
    } else {
        0.0
    }
}

fn relative_increase(baseline: f64, candidate: f64) -> f64 {
    if baseline > 0.0 {
        (candidate - baseline) / baseline
    } else if candidate == 0.0 {
        0.0
    } else {
        f64::INFINITY
    }
}

fn relative_increase_option(baseline: Option<f64>, candidate: Option<f64>) -> Option<f64> {
    Some(relative_increase(baseline?, candidate?))
}

fn host_report() -> HostReport {
    HostReport {
        os: std::env::consts::OS,
        arch: std::env::consts::ARCH,
        available_parallelism: std::thread::available_parallelism()
            .map(usize::from)
            .unwrap_or(1),
        kernel: command_output("uname", &["-srvm"]),
        rustc: command_output("rustc", &["--version"]),
        clock_ticks_per_second: clock_ticks_per_second(),
        executable_blake3: current_executable_blake3(),
    }
}

fn command_output(program: &str, args: &[&str]) -> Option<String> {
    let output = Command::new(program).args(args).output().ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

fn current_executable_blake3() -> Option<String> {
    let executable = std::env::current_exe().ok()?;
    let mut file = fs::File::open(executable).ok()?;
    let mut hasher = blake3::Hasher::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer).ok()?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Some(hasher.finalize().to_hex().to_string())
}

fn unix_millis() -> u64 {
    let milliseconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    u64::try_from(milliseconds).unwrap_or(u64::MAX)
}

impl ProcessCpuTicks {
    fn total(self) -> u64 {
        self.user.saturating_add(self.system)
    }
}

impl ProcessResourceBaseline {
    fn capture() -> Self {
        Self {
            cpu_ticks: process_cpu_ticks(),
            write_bytes: process_write_bytes(),
            rss_bytes: process_rss_bytes(),
        }
    }
}

fn process_cpu_ticks() -> Option<ProcessCpuTicks> {
    let stat = fs::read_to_string("/proc/self/stat").ok()?;
    let after_command = stat.rsplit_once(')')?.1;
    let fields = after_command.split_ascii_whitespace().collect::<Vec<_>>();
    let user = fields.get(11)?.parse::<u64>().ok()?;
    let system = fields.get(12)?.parse::<u64>().ok()?;
    Some(ProcessCpuTicks { user, system })
}

fn process_write_bytes() -> Option<u64> {
    proc_value("/proc/self/io", "write_bytes:").and_then(|value| value.parse().ok())
}

fn process_rss_bytes() -> Option<u64> {
    let kibibytes = proc_value("/proc/self/status", "VmRSS:")?
        .split_ascii_whitespace()
        .next()?
        .parse::<u64>()
        .ok()?;
    kibibytes.checked_mul(1024)
}

fn proc_value(path: &str, key: &str) -> Option<String> {
    fs::read_to_string(path)
        .ok()?
        .lines()
        .find_map(|line| line.strip_prefix(key).map(str::trim).map(str::to_owned))
}

fn clock_ticks_per_second() -> Option<u64> {
    command_output("getconf", &["CLK_TCK"])?.parse().ok()
}

fn ticks_to_milliseconds(ticks: Option<u64>, ticks_per_second: Option<u64>) -> Option<f64> {
    let ticks = ticks?;
    let ticks_per_second = ticks_per_second?;
    (ticks_per_second > 0).then(|| ticks as f64 * 1_000.0 / ticks_per_second as f64)
}

fn max_option(left: Option<u64>, right: Option<u64>) -> Option<u64> {
    match (left, right) {
        (Some(left), Some(right)) => Some(left.max(right)),
        (Some(value), None) | (None, Some(value)) => Some(value),
        (None, None) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run_git(repository: &Path, args: &[&str]) {
        let status = Command::new("git")
            .arg("-C")
            .arg(repository)
            .args(args)
            .status()
            .expect("run git");
        assert!(status.success(), "git {args:?} failed with {status}");
    }

    fn fixture_repository() -> (tempfile::TempDir, String) {
        let repository = tempfile::tempdir().expect("repository");
        run_git(repository.path(), &["init", "--quiet"]);
        run_git(
            repository.path(),
            &["config", "user.email", "test@example.com"],
        );
        run_git(
            repository.path(),
            &["config", "user.name", "LeanToken Test"],
        );
        for index in 0..12 {
            fs::write(
                repository.path().join(format!("file_{index}.rs")),
                format!("pub fn item_{index}(value: usize) -> usize {{ value + {index} }}\n"),
            )
            .expect("fixture source");
        }
        fs::write(
            repository.path().join("module.py"),
            "class Example:\n    def value(self):\n        return 1\n",
        )
        .expect("Python fixture");
        run_git(repository.path(), &["add", "-A"]);
        run_git(repository.path(), &["commit", "--quiet", "-m", "fixture"]);
        let revision =
            git_output(repository.path(), ["rev-parse", "HEAD"]).expect("fixture revision");
        (repository, revision)
    }

    #[test]
    fn parsers_enforce_worker_and_cancellation_bounds() {
        assert_eq!(
            parse_worker_order("1,2,4,4,2,1").expect("workers"),
            vec![1, 2, 4, 4, 2, 1]
        );
        assert!(parse_worker_order("0").is_err());
        assert!(parse_worker_order("65").is_err());
        assert!(parse_worker_order("").is_err());
        assert!(parse_worker_order(&vec!["1"; 17].join(",")).is_err());
        validate_screening_order(&[1, 2, 4, 4, 2, 1]).expect("screening order");
        assert!(validate_screening_order(&[1, 2, 4, 1, 2, 4]).is_err());
        validate_two_worker_follow_up_order(&[1, 2, 2, 1, 2, 1, 1, 2])
            .expect("two-worker follow-up order");
        assert!(validate_two_worker_follow_up_order(&[1, 2, 2, 1, 1, 2]).is_err());
        assert!(validate_two_worker_follow_up_order(&[1, 2, 2, 1, 1, 2, 2, 1]).is_err());

        assert!(
            parse_cancellation_phases("none")
                .expect("no cancellation")
                .is_empty()
        );
        assert_eq!(
            parse_cancellation_phases("preparation,reference_fts,preparation").expect("phases"),
            vec![
                IndexProgressPhase::Preparation,
                IndexProgressPhase::ReferenceFts
            ]
        );
        assert!(parse_cancellation_phases("unknown").is_err());
        assert_eq!(percentile(&[10.0, 20.0], 0.50), 10.0);
        assert_eq!(percentile(&[10.0, 20.0], 0.95), 20.0);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_process_accounting_is_available() {
        assert!(process_cpu_ticks().is_some());
        assert!(process_write_bytes().is_some());
        assert!(process_rss_bytes().is_some());
        assert!(clock_ticks_per_second().is_some());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn cold_matrix_smoke_preserves_parity_and_restartability() {
        let git = Command::new("git")
            .arg("--version")
            .status()
            .expect("Git is required for the cold-matrix test");
        assert!(git.success(), "Git is required for the cold-matrix test");
        let (repository, revision) = fixture_repository();
        let output = tempfile::tempdir().expect("output");
        let mut args = ColdMatrixArgs {
            repository: repository.path().to_path_buf(),
            repository_label: "local-fixture".into(),
            expected_revision: revision,
            matrix_kind: ColdMatrixKind::Screening,
            worker_order: Some("1,2".into()),
            parity_queries: vec!["item".into(), "class".into()],
            cancellation_phases: "preparation,relational_write,chunk_word_fts,chunk_trigram_fts,symbol_fts,reference_fts,commit_and_checkpoint".into(),
            sample_interval_ms: 1,
            timeout_seconds: 30,
            output: output.path().join("cold.json"),
            allow_debug: true,
            allow_dirty: true,
            allow_incomplete_matrix: true,
        };
        args.worker_order = Some("1,2,4,4,2,1".into());
        args.allow_incomplete_matrix = false;
        validate_args(&args).expect("strict counterbalance");
        let required_cancellation_phases = args.cancellation_phases.clone();
        args.cancellation_phases = "none".into();
        assert!(validate_args(&args).is_err());
        args.cancellation_phases = required_cancellation_phases;
        args.worker_order = Some("1,2".into());
        assert!(validate_args(&args).is_err());
        args.matrix_kind = ColdMatrixKind::TwoWorkerFollowUp;
        args.worker_order = None;
        let follow_up = validate_args(&args).expect("strict two-worker follow-up");
        assert_eq!(follow_up.worker_order, vec![1, 2, 2, 1, 2, 1, 1, 2]);
        assert_eq!(follow_up.minimum_samples_per_worker, 4);
        assert_eq!(follow_up.minimum_wall_p95_reduction, Some(0.20));
        args.matrix_kind = ColdMatrixKind::Screening;
        args.worker_order = Some("1,2".into());
        args.allow_incomplete_matrix = true;
        args.cancellation_phases = "preparation".into();
        let policy = validate_args(&args).expect("policy");
        let corpus = prepare_corpus(&args).expect("corpus");
        let first = measure_cold_run_in_process(
            0,
            1,
            &corpus.root,
            &output.path().join("first.sqlite"),
            &policy.parity_queries,
            policy.sample_interval_ms,
            policy.timeout_seconds,
        )
        .expect("first cold run");
        let second = measure_cold_run_in_process(
            1,
            2,
            &corpus.root,
            &output.path().join("second.sqlite"),
            &policy.parity_queries,
            policy.sample_interval_ms,
            policy.timeout_seconds,
        )
        .expect("second cold run");
        let mut parity = None;
        require_parity(&mut parity, &first).expect("first parity");
        require_parity(&mut parity, &second).expect("second parity");
        let parity = parity.expect("parity report");
        let cancellation = measure_cancellation_probe_in_process(
            IndexProgressPhase::Preparation,
            CancellationProbeInput {
                workers: 2,
                repository: &corpus.root,
                database: &output.path().join("cancel.sqlite"),
                parity_queries: &policy.parity_queries,
                sample_interval_ms: policy.sample_interval_ms,
                timeout_seconds: policy.timeout_seconds,
                require_observation: false,
                parity: &parity,
            },
        )
        .expect("cancellation probe");

        assert!(parity.complete);
        assert_eq!(first.logical_index_blake3, second.logical_index_blake3);
        assert_eq!(first.retrieval_blake3, second.retrieval_blake3);
        assert!(cancellation.restart_matches_baseline);
        let runs = vec![first, second];
        let summaries = summarize_workers(&runs);
        let decision = decide(&runs, &summaries, &policy);
        assert_eq!(decision.baseline_workers, 1);
        assert_eq!(decision.comparisons.len(), 1);
        let encoded = serde_json::to_vec(&ColdWorkerOutput::Cancellation(Box::new(cancellation)))
            .expect("serialize worker output");
        assert!(matches!(
            serde_json::from_slice::<ColdWorkerOutput>(&encoded)
                .expect("deserialize worker output"),
            ColdWorkerOutput::Cancellation(_)
        ));
    }
}
