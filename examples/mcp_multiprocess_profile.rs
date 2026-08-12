//! Reproducible Linux resource profile for several stdio MCP processes.
//!
//! Build the product binary first, then run this example in release mode:
//!
//! ```text
//! cargo build --release
//! cargo run --release --package leantoken-benchmarks --bin mcp_multiprocess_profile -- \
//!   --binary target/release/leantoken --output report.json
//! ```

use std::{
    collections::HashMap,
    error::Error,
    fs,
    io::{BufRead, BufReader, Read, Write},
    path::{Path, PathBuf},
    process::{Child, ChildStderr, ChildStdin, Stdio},
    sync::{Arc, Mutex, mpsc},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use clap::Parser;
use rusqlite::{Connection, OpenFlags};
use serde::Serialize;
use serde_json::{Value, json};

const MAX_PROCESSES: usize = 16;
const MAX_INDEX_WORKERS: usize = 64;
const MAX_FIXTURE_FILES: usize = 10_000;
const MAX_FUNCTIONS_PER_FILE: usize = 1_000;
const MAX_WARM_ITERATIONS: usize = 1_000;
const MAX_IDLE_SECONDS: u64 = 60;
const MAX_POLLING_DIRECTORIES: usize = 60_000;
const MAX_POLLING_OBSERVATION_SECONDS: u64 = 120;
const MAX_PARITY_MISMATCH_PATHS: usize = 32;
const WORKLOADS: [Workload; 4] = [
    Workload::Files,
    Workload::Search,
    Workload::Read,
    Workload::Context,
];

#[derive(Debug, Parser)]
#[command(about = "Measure 1/4/8 stdio MCP processes across cache topologies")]
struct Args {
    /// Release-mode LeanToken executable to launch.
    #[arg(long, default_value = "target/release/leantoken")]
    binary: PathBuf,
    /// Explicit file-preparation worker limit passed to every MCP process.
    #[arg(long, default_value_t = 1)]
    max_index_workers: usize,
    /// Comma-separated process counts. Include 1, 4, and 8 for a decision.
    #[arg(long, default_value = "1,4,8")]
    process_counts: String,
    /// Deterministic Rust fixture files generated per run.
    #[arg(long, default_value_t = 200)]
    files: usize,
    /// Functions generated in every fixture file.
    #[arg(long, default_value_t = 40)]
    functions_per_file: usize,
    /// Concurrent warm query rounds per process.
    #[arg(long, default_value_t = 10)]
    warm_iterations: usize,
    /// Idle CPU observation window after retrieval rounds.
    #[arg(long, default_value_t = 5)]
    idle_seconds: u64,
    /// Empty directories used to force the bounded periodic-polling fallback.
    #[arg(long, default_value_t = 50_001)]
    polling_directories: usize,
    /// CPU and reconciliation observation window for the polling fallback.
    #[arg(long, default_value_t = 31)]
    polling_observation_seconds: u64,
    /// Skip the dedicated periodic-polling process probe.
    #[arg(long)]
    skip_polling_probe: bool,
    /// Per-operation timeout.
    #[arg(long, default_value_t = 20)]
    timeout_seconds: u64,
    /// Write pretty JSON here in addition to stdout.
    #[arg(long)]
    output: Option<PathBuf>,
}

#[derive(Debug, Clone, Copy)]
struct RunConfig<'a> {
    binary: &'a Path,
    max_index_workers: usize,
    files: usize,
    functions_per_file: usize,
    warm_iterations: usize,
    idle_duration: Duration,
    timeout: Duration,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
enum Topology {
    SharedCache,
    IndependentCaches,
}

impl Topology {
    fn run_order() -> [Self; 4] {
        [
            Self::SharedCache,
            Self::IndependentCaches,
            Self::IndependentCaches,
            Self::SharedCache,
        ]
    }

    fn expected_leaders(self, process_count: usize) -> usize {
        match self {
            Self::SharedCache => 1,
            Self::IndependentCaches => process_count,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
enum Workload {
    Files,
    Search,
    Read,
    Context,
}

#[derive(Debug, Clone, Copy, Serialize)]
struct DecisionThresholds {
    max_incremental_rss_mib_per_follower: f64,
    max_startup_p95_ratio: f64,
    max_warm_p95_ratio: f64,
    max_normalized_wal_bytes_per_query_ratio: f64,
    max_established_read_connections_per_process: usize,
    max_takeover_ms: f64,
    max_eight_process_cpu_per_query_ratio: f64,
    max_independent_cold_cpu_per_repository_ratio: f64,
}

impl Default for DecisionThresholds {
    fn default() -> Self {
        Self {
            max_incremental_rss_mib_per_follower: 128.0,
            max_startup_p95_ratio: 3.0,
            max_warm_p95_ratio: 3.0,
            max_normalized_wal_bytes_per_query_ratio: 3.0,
            max_established_read_connections_per_process: 8,
            max_takeover_ms: 5_000.0,
            max_eight_process_cpu_per_query_ratio: 2.0,
            max_independent_cold_cpu_per_repository_ratio: 2.0,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize)]
struct LatencySummary {
    samples: usize,
    p50_ms: f64,
    p95_ms: f64,
    max_ms: f64,
}

impl LatencySummary {
    fn from_values(values: &[f64]) -> Self {
        Self {
            samples: values.len(),
            p50_ms: percentile(values, 0.50),
            p95_ms: percentile(values, 0.95),
            max_ms: values.iter().copied().fold(0.0, f64::max),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
struct ProcessResources {
    pid: u32,
    role: &'static str,
    rss_kib: usize,
    peak_rss_kib: usize,
    threads: usize,
    file_descriptors: usize,
    database_file_descriptors: usize,
    sqlite_artifact_file_descriptors: usize,
    estimated_established_read_connections: usize,
    inotify_file_descriptors: usize,
}

#[derive(Debug, Clone, Copy, Serialize)]
struct CpuSummary {
    cpu_milliseconds: f64,
    wall_milliseconds: f64,
    average_utilization_percent: f64,
    cpu_milliseconds_per_operation: Option<f64>,
}

impl CpuSummary {
    fn from_measurement(cpu_milliseconds: f64, wall: Duration, operations: usize) -> Self {
        let wall_milliseconds = wall.as_secs_f64() * 1_000.0;
        Self {
            cpu_milliseconds,
            wall_milliseconds,
            average_utilization_percent: safe_ratio(cpu_milliseconds, wall_milliseconds) * 100.0,
            cpu_milliseconds_per_operation: (operations > 0)
                .then(|| cpu_milliseconds / operations as f64),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
struct WorkloadMeasurement {
    workload: Workload,
    requests: usize,
    complete_request: LatencySummary,
    cpu: CpuSummary,
    baseline_response_blake3: Vec<String>,
    parity_checked: usize,
    parity_mismatches: usize,
    parity_mismatch_paths: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
struct WatcherObservation {
    pid: u32,
    backend: &'static str,
    admission_entries: Option<usize>,
    admission_directories: Option<usize>,
    admission_complete: Option<bool>,
    fallback_reason: Option<String>,
    poll_ticks: Option<u64>,
    changed_path_deliveries: Option<u64>,
    full_reconciliation_deliveries: Option<u64>,
}

#[derive(Debug, Clone, Serialize)]
struct PeriodicPollMeasurement {
    directories_created: usize,
    watcher: WatcherObservation,
    reconciliations_at_ready: usize,
    reconciliations_during_observation: usize,
    cpu: CpuSummary,
}

#[derive(Debug, Clone, Serialize)]
struct ProcessMeasurement {
    resources: ProcessResources,
    startup_to_ready_ms: f64,
    cold_query_ms: f64,
    watcher: WatcherObservation,
}

#[derive(Debug, Clone, Copy, Serialize)]
struct StorageSnapshot {
    repository_generation: i64,
    response_accounting_updates: i64,
    tracked_baseline_requests: i64,
    database_bytes: u64,
    wal_bytes: u64,
    shm_bytes: u64,
}

#[derive(Debug, Clone, Serialize)]
struct TakeoverMeasurement {
    killed_leader_pid: u32,
    successor_leader_pid: u32,
    takeover_ms: f64,
    repository_generation: i64,
    watcher_processes_after_takeover: usize,
}

#[derive(Debug, Clone, Serialize)]
struct RunMeasurement {
    topology: Topology,
    order_index: usize,
    process_count: usize,
    repository_count: usize,
    leader_pids: Vec<u32>,
    leader_lock_owners: usize,
    watcher_processes: usize,
    aggregate_rss_kib: usize,
    aggregate_peak_rss_kib: usize,
    aggregate_threads: usize,
    aggregate_file_descriptors: usize,
    aggregate_estimated_read_connections: usize,
    startup_to_ready: LatencySummary,
    cold_startup_cpu: CpuSummary,
    cold_query: LatencySummary,
    warm_query: LatencySummary,
    workloads: Vec<WorkloadMeasurement>,
    idle_cpu: CpuSummary,
    storage_before_queries: StorageSnapshot,
    storage_after_queries: StorageSnapshot,
    generation_publications: i64,
    expected_response_accounting_updates: usize,
    observed_response_accounting_updates: i64,
    parity_checked: usize,
    parity_mismatches: usize,
    processes: Vec<ProcessMeasurement>,
    takeover: Option<TakeoverMeasurement>,
    #[serde(skip)]
    parity_responses: HashMap<Workload, Value>,
}

#[derive(Debug, Clone, Serialize)]
struct Decision {
    recommendation: &'static str,
    reasons: Vec<String>,
    incremental_rss_mib_per_follower: Option<f64>,
    startup_p95_ratio: Option<f64>,
    warm_p95_ratio: Option<f64>,
    normalized_wal_bytes_per_query_ratio: Option<f64>,
    eight_process_cpu_per_query_ratio: Option<f64>,
    independent_cold_cpu_per_repository_ratio: Option<f64>,
}

#[derive(Debug, Serialize)]
struct Report {
    schema_version: u32,
    generated_at_unix_seconds: u64,
    platform: &'static str,
    kernel_release: String,
    logical_cpus: usize,
    binary: String,
    binary_blake3: String,
    max_index_workers: usize,
    fixture_files: usize,
    functions_per_file: usize,
    warm_iterations_per_process: usize,
    idle_seconds: u64,
    topology_order: [Topology; 4],
    thresholds: DecisionThresholds,
    runs: Vec<RunMeasurement>,
    periodic_poll: Option<PeriodicPollMeasurement>,
    decision: Decision,
    observation_limits: Vec<&'static str>,
}

struct McpProcess {
    child: Child,
    stdin: Option<ChildStdin>,
    lines: mpsc::Receiver<String>,
    diagnostics: Arc<Mutex<Vec<String>>>,
    diagnostics_thread: Option<std::thread::JoinHandle<()>>,
    next_id: u64,
    stopped: bool,
}

impl McpProcess {
    fn spawn(
        binary: &Path,
        root: &Path,
        database: &Path,
        max_index_workers: usize,
    ) -> Result<Self, Box<dyn Error>> {
        let mut child = std::process::Command::new(binary)
            .args(["--root", path_str(root)?, "--database", path_str(database)?])
            .arg("--max-index-workers")
            .arg(max_index_workers.to_string())
            .arg("mcp")
            .env("RUST_LOG", "leantoken=info")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        let stdin = child.stdin.take().ok_or("MCP stdin unavailable")?;
        let stdout = child.stdout.take().ok_or("MCP stdout unavailable")?;
        let stderr = child.stderr.take().ok_or("MCP stderr unavailable")?;
        let (sender, lines) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                if sender.send(line).is_err() {
                    break;
                }
            }
        });
        let diagnostics = Arc::new(Mutex::new(Vec::new()));
        let diagnostics_thread = collect_diagnostics(stderr, Arc::clone(&diagnostics));
        Ok(Self {
            child,
            stdin: Some(stdin),
            lines,
            diagnostics,
            diagnostics_thread: Some(diagnostics_thread),
            next_id: 1,
            stopped: false,
        })
    }

    fn pid(&self) -> u32 {
        self.child.id()
    }

    fn send_initialize(&mut self) -> Result<u64, Box<dyn Error>> {
        let id = self.take_id();
        self.send(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-11-25",
                "capabilities": {},
                "clientInfo": {
                    "name": "leantoken-multiprocess-profile",
                    "version": "1"
                }
            }
        }))?;
        Ok(id)
    }

    fn send_initialized(&mut self) -> Result<(), Box<dyn Error>> {
        self.send(&json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized"
        }))
    }

    fn send_files_query(&mut self) -> Result<(u64, Instant), Box<dyn Error>> {
        self.send_workload_query(Workload::Files)
    }

    fn send_workload_query(
        &mut self,
        workload: Workload,
    ) -> Result<(u64, Instant), Box<dyn Error>> {
        let id = self.take_id();
        let started = Instant::now();
        let (name, arguments) = workload_request(workload);
        self.send(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "tools/call",
            "params": {
                "name": name,
                "arguments": arguments
            }
        }))?;
        Ok((id, started))
    }

    fn receive(&self, id: u64, timeout: Duration) -> Result<Value, Box<dyn Error>> {
        let deadline = Instant::now() + timeout;
        loop {
            let line = self
                .lines
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))?;
            let response: Value = serde_json::from_str(&line)?;
            if response.get("id").and_then(Value::as_u64) == Some(id) {
                return Ok(response);
            }
        }
    }

    fn wait_until_ready(&mut self, timeout: Duration) -> Result<(), Box<dyn Error>> {
        let deadline = Instant::now() + timeout;
        loop {
            let (id, _) = self.send_files_query()?;
            let response = self.receive(id, deadline.saturating_duration_since(Instant::now()))?;
            if successful_tool_response(&response) {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err("MCP process did not become ready before the deadline".into());
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    fn kill_now(&mut self) -> Result<(), Box<dyn Error>> {
        if !self.stopped {
            self.stdin.take();
            self.child.kill()?;
            self.child.wait()?;
            self.stopped = true;
            self.join_diagnostics();
        }
        Ok(())
    }

    fn stop(&mut self) {
        if self.stopped {
            return;
        }
        self.stdin.take();
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            if self.child.try_wait().ok().flatten().is_some() {
                self.stopped = true;
                self.join_diagnostics();
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
        self.stopped = true;
        self.join_diagnostics();
    }

    fn send(&mut self, message: &Value) -> Result<(), Box<dyn Error>> {
        let stdin = self.stdin.as_mut().ok_or("MCP process is stopped")?;
        serde_json::to_writer(&mut *stdin, message)?;
        stdin.write_all(b"\n")?;
        stdin.flush()?;
        Ok(())
    }

    fn take_id(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    fn diagnostic_lines(&self) -> Vec<String> {
        self.diagnostics
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    fn join_diagnostics(&mut self) {
        if let Some(thread) = self.diagnostics_thread.take() {
            let _ = thread.join();
        }
    }
}

fn collect_diagnostics(
    stderr: ChildStderr,
    diagnostics: Arc<Mutex<Vec<String>>>,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            if line.contains("watcher") {
                diagnostics
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .push(line);
            }
        }
    })
}

impl Drop for McpProcess {
    fn drop(&mut self) {
        self.stop();
    }
}

fn main() -> Result<(), Box<dyn Error>> {
    if !cfg!(target_os = "linux") {
        return Err("mcp_multiprocess_profile requires Linux /proc".into());
    }
    let args = Args::parse();
    validate_args(&args)?;
    let process_counts = parse_process_counts(&args.process_counts)?;
    let binary = fs::canonicalize(&args.binary)?;
    let timeout = Duration::from_secs(args.timeout_seconds);
    let thresholds = DecisionThresholds::default();
    let run_config = RunConfig {
        binary: &binary,
        max_index_workers: args.max_index_workers,
        files: args.files,
        functions_per_file: args.functions_per_file,
        warm_iterations: args.warm_iterations,
        idle_duration: Duration::from_secs(args.idle_seconds),
        timeout,
    };
    let mut runs = Vec::with_capacity(process_counts.len() * Topology::run_order().len());
    for process_count in process_counts {
        for (order_index, topology) in Topology::run_order().into_iter().enumerate() {
            runs.push(run_measurement(
                &run_config,
                topology,
                order_index,
                process_count,
            )?);
        }
    }
    apply_cross_run_response_parity(&mut runs);
    let periodic_poll = if args.skip_polling_probe {
        None
    } else {
        Some(run_periodic_poll_probe(
            &binary,
            args.max_index_workers,
            args.polling_directories,
            Duration::from_secs(args.polling_observation_seconds),
            timeout,
        )?)
    };
    let decision = make_decision(&runs, thresholds);
    let report = Report {
        schema_version: 3,
        generated_at_unix_seconds: SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs(),
        platform: "linux-procfs",
        kernel_release: fs::read_to_string("/proc/sys/kernel/osrelease")?
            .trim()
            .to_owned(),
        logical_cpus: std::thread::available_parallelism()?.get(),
        binary: binary.display().to_string(),
        binary_blake3: hash_file(&binary)?,
        max_index_workers: args.max_index_workers,
        fixture_files: args.files,
        functions_per_file: args.functions_per_file,
        warm_iterations_per_process: args.warm_iterations,
        idle_seconds: args.idle_seconds,
        topology_order: Topology::run_order(),
        thresholds,
        runs,
        periodic_poll,
        decision,
        observation_limits: vec![
            "RSS, HWM, threads, file descriptors, lock ownership, and watcher ownership require Linux /proc.",
            "Established read connections are inferred as main-database file descriptors minus the one writer connection.",
            "SQLite does not expose cross-process statement counts; successful response-accounting updates and generation publications are reported as observed writes.",
            "Latency is host-local wall time from one orchestrator and is comparable only on the same host and release build.",
            "Startup readiness and concurrent-query responses are observed by one orchestrator in process order, so later processes can include bounded client-side receipt delay.",
            "Watcher backend is confirmed with Linux inotify descriptors; admission counters are parsed from the product's structured tracing fields.",
            "Complete response parity removes only JSON-RPC request ids, generated receipt_id/repository_id values, instantaneous freshness, and their derived path_and_metadata_tokens/total_response_tokens accounting (the independent topology uses distinct canonical roots and concurrent freshness is a liveness observation), then compares every other observable result field across processes, workloads, topologies, and ABBA repetitions.",
            "The explicit max_index_workers value applies to every indexing attempt in this profiler. A two-worker run is a cold-start contention probe, not evidence that warm reconciliation should use two workers.",
        ],
    };
    let serialized = serde_json::to_string_pretty(&report)?;
    if let Some(output) = args.output {
        if let Some(parent) = output.parent()
            && !parent.as_os_str().is_empty()
        {
            fs::create_dir_all(parent)?;
        }
        fs::write(output, format!("{serialized}\n"))?;
    }
    println!("{serialized}");
    Ok(())
}

fn validate_args(args: &Args) -> Result<(), Box<dyn Error>> {
    validate_max_index_workers(args.max_index_workers)?;
    if args.files == 0 || args.files > MAX_FIXTURE_FILES {
        return Err(format!("--files must be within 1..={MAX_FIXTURE_FILES}").into());
    }
    if args.functions_per_file == 0 || args.functions_per_file > MAX_FUNCTIONS_PER_FILE {
        return Err(
            format!("--functions-per-file must be within 1..={MAX_FUNCTIONS_PER_FILE}").into(),
        );
    }
    if args.warm_iterations == 0 || args.warm_iterations > MAX_WARM_ITERATIONS {
        return Err(format!("--warm-iterations must be within 1..={MAX_WARM_ITERATIONS}").into());
    }
    if args.idle_seconds == 0 || args.idle_seconds > MAX_IDLE_SECONDS {
        return Err(format!("--idle-seconds must be within 1..={MAX_IDLE_SECONDS}").into());
    }
    if args.polling_directories <= 50_000 || args.polling_directories > MAX_POLLING_DIRECTORIES {
        return Err(format!(
            "--polling-directories must be within 50001..={MAX_POLLING_DIRECTORIES}"
        )
        .into());
    }
    if args.polling_observation_seconds < 31
        || args.polling_observation_seconds > MAX_POLLING_OBSERVATION_SECONDS
    {
        return Err(format!(
            "--polling-observation-seconds must be within 31..={MAX_POLLING_OBSERVATION_SECONDS}"
        )
        .into());
    }
    if args.timeout_seconds == 0 || args.timeout_seconds > 300 {
        return Err("--timeout-seconds must be within 1..=300".into());
    }
    Ok(())
}

fn validate_max_index_workers(value: usize) -> Result<(), Box<dyn Error>> {
    if !(1..=MAX_INDEX_WORKERS).contains(&value) {
        return Err(format!("--max-index-workers must be within 1..={MAX_INDEX_WORKERS}").into());
    }
    Ok(())
}

fn parse_process_counts(value: &str) -> Result<Vec<usize>, Box<dyn Error>> {
    let mut counts = value
        .split(',')
        .map(|part| part.trim().parse::<usize>())
        .collect::<Result<Vec<_>, _>>()?;
    if counts.is_empty()
        || counts
            .iter()
            .any(|count| *count == 0 || *count > MAX_PROCESSES)
    {
        return Err(format!("process counts must be within 1..={MAX_PROCESSES}").into());
    }
    counts.sort_unstable();
    counts.dedup();
    Ok(counts)
}

fn run_periodic_poll_probe(
    binary: &Path,
    max_index_workers: usize,
    directories: usize,
    observation: Duration,
    timeout: Duration,
) -> Result<PeriodicPollMeasurement, Box<dyn Error>> {
    let workspace = tempfile::tempdir()?;
    let repository = workspace.path().join("polling-repository");
    fs::create_dir(&repository)?;
    write_fixture(&repository, 1, 1)?;
    for index in 0..directories {
        fs::create_dir(repository.join(format!("poll-{index:05}")))?;
    }
    let database = workspace.path().join("polling-cache").join("index.sqlite");
    fs::create_dir_all(database.parent().ok_or("database parent missing")?)?;
    let mut process = McpProcess::spawn(binary, &repository, &database, max_index_workers)?;
    let initialize = process.send_initialize()?;
    let response = process.receive(initialize, timeout)?;
    if response.get("result").is_none() {
        return Err(format!("MCP polling-probe initialize failed: {response}").into());
    }
    process.send_initialized()?;
    process.wait_until_ready(timeout)?;
    wait_for_generation(&database, 1, timeout)?;
    let reconciliations_at_ready = count_full_reconciliations(&process.diagnostic_lines());
    if reconciliations_at_ready != 0 {
        return Err("periodic polling reconciled before its first interval".into());
    }

    let cpu_before = aggregate_cpu_ticks(std::slice::from_ref(&process))?;
    let started = Instant::now();
    std::thread::sleep(observation);
    let cpu = CpuSummary::from_measurement(
        cpu_milliseconds(
            cpu_before,
            aggregate_cpu_ticks(std::slice::from_ref(&process))?,
        ),
        started.elapsed(),
        1,
    );
    let resources = sample_process(process.pid(), &database, true)?;
    process.stop();
    let lines = process.diagnostic_lines();
    let reconciliations_during_observation =
        count_full_reconciliations(&lines).saturating_sub(reconciliations_at_ready);
    if reconciliations_during_observation == 0 {
        return Err("periodic polling did not reconcile after its interval".into());
    }
    let watcher = parse_watcher_observation(process.pid(), &resources, &lines);
    if watcher.backend != "periodic_polling" {
        return Err(format!(
            "polling probe did not select periodic polling: {:?}",
            watcher.backend
        )
        .into());
    }
    Ok(PeriodicPollMeasurement {
        directories_created: directories,
        watcher,
        reconciliations_at_ready,
        reconciliations_during_observation,
        cpu,
    })
}

fn count_full_reconciliations(lines: &[String]) -> usize {
    lines
        .iter()
        .filter(|line| line.contains("watcher scheduled bounded full reconciliation"))
        .count()
}

fn run_measurement(
    config: &RunConfig<'_>,
    topology: Topology,
    order_index: usize,
    process_count: usize,
) -> Result<RunMeasurement, Box<dyn Error>> {
    let RunConfig {
        binary,
        max_index_workers,
        files,
        functions_per_file,
        warm_iterations,
        idle_duration,
        timeout,
    } = *config;
    let workspace = tempfile::tempdir()?;
    let mut repositories = Vec::with_capacity(process_count);
    let mut databases = Vec::with_capacity(process_count);
    match topology {
        Topology::SharedCache => {
            let repository = workspace.path().join("repository");
            fs::create_dir(&repository)?;
            write_fixture(&repository, files, functions_per_file)?;
            let database = workspace.path().join("cache").join("index.sqlite");
            fs::create_dir_all(database.parent().ok_or("database parent missing")?)?;
            repositories.resize(process_count, repository);
            databases.resize(process_count, database);
        }
        Topology::IndependentCaches => {
            for index in 0..process_count {
                let repository = workspace.path().join(format!("repository-{index:02}"));
                fs::create_dir(&repository)?;
                write_fixture(&repository, files, functions_per_file)?;
                let database = workspace
                    .path()
                    .join(format!("cache-{index:02}"))
                    .join("index.sqlite");
                fs::create_dir_all(database.parent().ok_or("database parent missing")?)?;
                repositories.push(repository);
                databases.push(database);
            }
        }
    }

    let started = Instant::now();
    let mut processes = repositories
        .iter()
        .zip(&databases)
        .map(|(repository, database)| {
            McpProcess::spawn(binary, repository, database, max_index_workers)
        })
        .collect::<Result<Vec<_>, _>>()?;
    let startup_cpu_before = aggregate_cpu_ticks(&processes)?;
    let initialize_ids = processes
        .iter_mut()
        .map(McpProcess::send_initialize)
        .collect::<Result<Vec<_>, _>>()?;
    for (process, id) in processes.iter().zip(initialize_ids) {
        let response = process.receive(id, timeout)?;
        if response.get("result").is_none() {
            return Err(format!("MCP initialize failed: {response}").into());
        }
    }
    for process in &mut processes {
        process.send_initialized()?;
    }

    let mut startup_ms = Vec::with_capacity(process_count);
    for process in &mut processes {
        process.wait_until_ready(timeout)?;
        startup_ms.push(started.elapsed().as_secs_f64() * 1_000.0);
    }
    for database in unique_paths(&databases) {
        wait_for_generation(database, 1, timeout)?;
    }
    let cold_startup_cpu = CpuSummary::from_measurement(
        cpu_milliseconds(startup_cpu_before, aggregate_cpu_ticks(&processes)?),
        started.elapsed(),
        process_count,
    );
    let storage_before_queries = aggregate_storage_snapshot(&databases)?;

    let cold_values = measure_query_round(&mut processes, timeout)?;
    let mut baselines = vec![HashMap::new(); process_count];
    for workload in WORKLOADS {
        for (index, process) in processes.iter_mut().enumerate() {
            let (id, _) = process.send_workload_query(workload)?;
            let response = process.receive(id, timeout)?;
            if !successful_tool_response(&response) {
                return Err(format!("MCP baseline query did not succeed: {response}").into());
            }
            baselines[index].insert(workload, normalize_response(response));
        }
    }
    let mut workloads = Vec::with_capacity(WORKLOADS.len());
    for workload in WORKLOADS {
        workloads.push(measure_workload(
            &mut processes,
            workload,
            &baselines,
            warm_iterations,
            timeout,
        )?);
    }
    std::thread::sleep(Duration::from_millis(100));
    let idle_cpu_before = aggregate_cpu_ticks(&processes)?;
    let idle_started = Instant::now();
    std::thread::sleep(idle_duration);
    let idle_cpu = CpuSummary::from_measurement(
        cpu_milliseconds(idle_cpu_before, aggregate_cpu_ticks(&processes)?),
        idle_started.elapsed(),
        0,
    );

    let pids = processes.iter().map(McpProcess::pid).collect::<Vec<_>>();
    let leader_pids = match topology {
        Topology::SharedCache => {
            let leadership_path = PathBuf::from(format!("{}.leader.lock", databases[0].display()));
            let owners = lock_owner_pids(&leadership_path, &pids)?;
            if owners.len() != 1 {
                return Err(format!("expected one leadership lock owner, found {owners:?}").into());
            }
            owners
        }
        Topology::IndependentCaches => {
            let mut owners = Vec::with_capacity(process_count);
            for (pid, database) in pids.iter().zip(&databases) {
                let leadership_path = PathBuf::from(format!("{}.leader.lock", database.display()));
                let owner = lock_owner_pids(&leadership_path, &[*pid])?;
                if owner != [*pid] {
                    return Err(format!(
                        "independent cache {} did not retain leader {pid}: {owner:?}",
                        database.display()
                    )
                    .into());
                }
                owners.push(*pid);
            }
            owners
        }
    };
    let mut resources = pids
        .iter()
        .zip(&databases)
        .map(|(pid, database)| sample_process(*pid, database, leader_pids.contains(pid)))
        .collect::<Result<Vec<_>, _>>()?;
    resources.sort_by_key(|sample| sample.pid);
    let watcher_processes = resources
        .iter()
        .filter(|sample| sample.inotify_file_descriptors > 0)
        .count();

    let mut process_measurements = Vec::with_capacity(process_count);
    for resource in resources.iter().cloned() {
        let source_index = pids
            .iter()
            .position(|pid| *pid == resource.pid)
            .ok_or("sampled PID was not launched")?;
        process_measurements.push(ProcessMeasurement {
            watcher: parse_watcher_observation(
                resource.pid,
                &resource,
                &processes[source_index].diagnostic_lines(),
            ),
            resources: resource,
            startup_to_ready_ms: startup_ms[source_index],
            cold_query_ms: cold_values[source_index],
        });
    }
    let warm_query = workloads
        .iter()
        .find(|measurement| measurement.workload == Workload::Files)
        .map_or_else(
            || LatencySummary::from_values(&[]),
            |measurement| measurement.complete_request,
        );
    let storage_after_queries = aggregate_storage_snapshot(&databases)?;
    let expected_generations = topology.expected_leaders(process_count) as i64;
    if storage_after_queries.repository_generation != expected_generations {
        return Err(format!(
            "cold startup published {} aggregate generations instead of {expected_generations}",
            storage_after_queries.repository_generation,
        )
        .into());
    }

    let takeover = if topology == Topology::SharedCache && process_count > 1 {
        Some(measure_takeover(
            &repositories[0],
            &databases[0],
            &mut processes,
            leader_pids[0],
            timeout,
        )?)
    } else {
        None
    };
    for process in &mut processes {
        process.stop();
    }
    std::thread::sleep(Duration::from_millis(20));
    for (measurement, process) in process_measurements.iter_mut().zip(&processes) {
        measurement.watcher = parse_watcher_observation(
            measurement.resources.pid,
            &measurement.resources,
            &process.diagnostic_lines(),
        );
    }
    let parity_checked = workloads
        .iter()
        .map(|measurement| measurement.parity_checked)
        .sum();
    let parity_mismatches = workloads
        .iter()
        .map(|measurement| measurement.parity_mismatches)
        .sum();
    let expected_response_accounting_updates =
        process_count * (1 + WORKLOADS.len() * (1 + warm_iterations));

    Ok(RunMeasurement {
        topology,
        order_index,
        process_count,
        repository_count: topology.expected_leaders(process_count),
        leader_pids: leader_pids.clone(),
        leader_lock_owners: leader_pids.len(),
        watcher_processes,
        aggregate_rss_kib: resources.iter().map(|sample| sample.rss_kib).sum(),
        aggregate_peak_rss_kib: resources.iter().map(|sample| sample.peak_rss_kib).sum(),
        aggregate_threads: resources.iter().map(|sample| sample.threads).sum(),
        aggregate_file_descriptors: resources.iter().map(|sample| sample.file_descriptors).sum(),
        aggregate_estimated_read_connections: resources
            .iter()
            .map(|sample| sample.estimated_established_read_connections)
            .sum(),
        startup_to_ready: LatencySummary::from_values(&startup_ms),
        cold_startup_cpu,
        cold_query: LatencySummary::from_values(&cold_values),
        warm_query,
        workloads,
        idle_cpu,
        storage_before_queries,
        storage_after_queries,
        generation_publications: expected_generations,
        expected_response_accounting_updates,
        observed_response_accounting_updates: storage_after_queries
            .response_accounting_updates
            .saturating_sub(storage_before_queries.response_accounting_updates),
        parity_checked,
        parity_mismatches,
        processes: process_measurements,
        takeover,
        parity_responses: baselines[0].clone(),
    })
}

fn measure_query_round(
    processes: &mut [McpProcess],
    timeout: Duration,
) -> Result<Vec<f64>, Box<dyn Error>> {
    let tickets = processes
        .iter_mut()
        .map(McpProcess::send_files_query)
        .collect::<Result<Vec<_>, _>>()?;
    let mut latencies = Vec::with_capacity(processes.len());
    for (process, (id, started)) in processes.iter().zip(tickets) {
        let response = process.receive(id, timeout)?;
        if !successful_tool_response(&response) {
            return Err(format!("MCP query did not succeed: {response}").into());
        }
        latencies.push(started.elapsed().as_secs_f64() * 1_000.0);
    }
    Ok(latencies)
}

fn measure_workload(
    processes: &mut [McpProcess],
    workload: Workload,
    baselines: &[HashMap<Workload, Value>],
    iterations: usize,
    timeout: Duration,
) -> Result<WorkloadMeasurement, Box<dyn Error>> {
    let cpu_before = aggregate_cpu_ticks(processes)?;
    let started = Instant::now();
    let mut latencies = Vec::with_capacity(processes.len() * iterations);
    let reference = &baselines[0][&workload];
    let mut parity_checked = 0usize;
    let mut parity_mismatches = 0usize;
    let mut parity_mismatch_paths = Vec::new();
    for (index, baseline) in baselines.iter().enumerate().skip(1) {
        compare_response_parity(
            reference,
            &baseline[&workload],
            &format!("cross_process_{index}"),
            &mut parity_checked,
            &mut parity_mismatches,
            &mut parity_mismatch_paths,
        );
    }
    for _ in 0..iterations {
        let tickets = processes
            .iter_mut()
            .map(|process| process.send_workload_query(workload))
            .collect::<Result<Vec<_>, _>>()?;
        for (index, (process, (id, query_started))) in processes.iter().zip(tickets).enumerate() {
            let response = process.receive(id, timeout)?;
            if !successful_tool_response(&response) {
                return Err(format!("MCP {workload:?} query did not succeed: {response}").into());
            }
            compare_response_parity(
                &baselines[index][&workload],
                &normalize_response(response),
                &format!("warm_process_{index}"),
                &mut parity_checked,
                &mut parity_mismatches,
                &mut parity_mismatch_paths,
            );
            latencies.push(query_started.elapsed().as_secs_f64() * 1_000.0);
        }
    }
    let requests = processes.len() * iterations;
    let elapsed = started.elapsed();
    let cpu = CpuSummary::from_measurement(
        cpu_milliseconds(cpu_before, aggregate_cpu_ticks(processes)?),
        elapsed,
        requests,
    );
    Ok(WorkloadMeasurement {
        workload,
        requests,
        complete_request: LatencySummary::from_values(&latencies),
        cpu,
        baseline_response_blake3: baselines
            .iter()
            .map(|baseline| response_fingerprint(&baseline[&workload]))
            .collect(),
        parity_checked,
        parity_mismatches,
        parity_mismatch_paths,
    })
}

fn apply_cross_run_response_parity(runs: &mut [RunMeasurement]) {
    let Some(reference) = runs.first().map(|run| run.parity_responses.clone()) else {
        return;
    };
    for (run_index, run) in runs.iter_mut().enumerate().skip(1) {
        for measurement in &mut run.workloads {
            compare_response_parity(
                &reference[&measurement.workload],
                &run.parity_responses[&measurement.workload],
                &format!("cross_run_{run_index}"),
                &mut measurement.parity_checked,
                &mut measurement.parity_mismatches,
                &mut measurement.parity_mismatch_paths,
            );
        }
    }
    for run in runs {
        run.parity_checked = run
            .workloads
            .iter()
            .map(|measurement| measurement.parity_checked)
            .sum();
        run.parity_mismatches = run
            .workloads
            .iter()
            .map(|measurement| measurement.parity_mismatches)
            .sum();
    }
}

fn compare_response_parity(
    expected: &Value,
    actual: &Value,
    scope: &str,
    checked: &mut usize,
    mismatches: &mut usize,
    mismatch_paths: &mut Vec<String>,
) {
    *checked += 1;
    if expected == actual {
        return;
    }
    *mismatches += 1;
    let mut paths = Vec::new();
    collect_json_diff_paths(expected, actual, "", &mut paths);
    for path in paths {
        let scoped = format!("{scope}:{path}");
        if !mismatch_paths.contains(&scoped) {
            mismatch_paths.push(scoped);
            if mismatch_paths.len() == MAX_PARITY_MISMATCH_PATHS {
                break;
            }
        }
    }
}

fn collect_json_diff_paths(expected: &Value, actual: &Value, path: &str, paths: &mut Vec<String>) {
    if expected == actual || paths.len() == MAX_PARITY_MISMATCH_PATHS {
        return;
    }
    match (expected, actual) {
        (Value::Object(expected), Value::Object(actual)) => {
            let mut keys = expected.keys().chain(actual.keys()).collect::<Vec<_>>();
            keys.sort_unstable();
            keys.dedup();
            for key in keys {
                let child_path = format!("{path}/{}", json_pointer_segment(key));
                match (expected.get(key), actual.get(key)) {
                    (Some(expected), Some(actual)) => {
                        collect_json_diff_paths(expected, actual, &child_path, paths);
                    }
                    _ => paths.push(child_path),
                }
                if paths.len() == MAX_PARITY_MISMATCH_PATHS {
                    break;
                }
            }
        }
        (Value::Array(expected), Value::Array(actual)) => {
            for index in 0..expected.len().max(actual.len()) {
                let child_path = format!("{path}/{index}");
                match (expected.get(index), actual.get(index)) {
                    (Some(expected), Some(actual)) => {
                        collect_json_diff_paths(expected, actual, &child_path, paths);
                    }
                    _ => paths.push(child_path),
                }
                if paths.len() == MAX_PARITY_MISMATCH_PATHS {
                    break;
                }
            }
        }
        _ => paths.push(if path.is_empty() {
            "/".into()
        } else {
            path.into()
        }),
    }
}

fn json_pointer_segment(value: &str) -> String {
    value.replace('~', "~0").replace('/', "~1")
}

fn successful_tool_response(response: &Value) -> bool {
    response["result"]["isError"] != true
        && response["result"]["structuredContent"]["status"] != "retryable"
        && response.get("error").is_none()
}

fn workload_request(workload: Workload) -> (&'static str, Value) {
    match workload {
        Workload::Files => (
            "files",
            json!({
                "operation": {"kind": "tree", "max_results": 10}
            }),
        ),
        Workload::Search => (
            "search",
            json!({
                "operation": {
                    "kind": "auto",
                    "query": "item_00010_1",
                    "max_results": 20,
                    "max_tokens": 1_000
                }
            }),
        ),
        Workload::Read => (
            "read",
            json!({
                "path": "file_00000.rs",
                "target": {"kind": "lines", "start": 1, "end": 80},
                "max_tokens": 1_000
            }),
        ),
        Workload::Context => (
            "context",
            json!({
                "task": "Investigate item_00010_1 handling and relevant tests",
                "token_budget": 1_000,
                "max_fragments": 8,
                "plan_only": false
            }),
        ),
    }
}

fn normalize_response(mut response: Value) -> Value {
    if let Value::Object(object) = &mut response {
        object.remove("id");
    }
    remove_generated_identifiers(&mut response);
    response
}

fn response_fingerprint(response: &Value) -> String {
    let canonical = canonicalize_json(response);
    let encoded = serde_json::to_vec(&canonical).expect("JSON values are serializable");
    blake3::hash(&encoded).to_hex().to_string()
}

fn canonicalize_json(value: &Value) -> Value {
    match value {
        Value::Object(object) => {
            let mut keys = object.keys().collect::<Vec<_>>();
            keys.sort_unstable();
            let mut canonical = serde_json::Map::new();
            for key in keys {
                canonical.insert(key.clone(), canonicalize_json(&object[key]));
            }
            Value::Object(canonical)
        }
        Value::Array(values) => Value::Array(values.iter().map(canonicalize_json).collect()),
        value => value.clone(),
    }
}

fn remove_generated_identifiers(value: &mut Value) {
    match value {
        Value::Object(object) => {
            object.remove("receipt_id");
            object.remove("repository_id");
            object.remove("freshness");
            object.remove("path_and_metadata_tokens");
            object.remove("total_response_tokens");
            object.remove("total_response_tokens");
            for value in object.values_mut() {
                remove_generated_identifiers(value);
            }
        }
        Value::Array(values) => {
            for value in values {
                remove_generated_identifiers(value);
            }
        }
        Value::String(text) if text.starts_with('{') => {
            if let Ok(mut nested) = serde_json::from_str::<Value>(text) {
                remove_generated_identifiers(&mut nested);
                if let Ok(normalized) = serde_json::to_string(&nested) {
                    *text = normalized;
                }
            }
        }
        _ => {}
    }
}

fn aggregate_cpu_ticks(processes: &[McpProcess]) -> Result<u64, Box<dyn Error>> {
    processes.iter().try_fold(0u64, |total, process| {
        Ok(total.saturating_add(process_cpu_ticks(process.pid())?))
    })
}

fn process_cpu_ticks(pid: u32) -> Result<u64, Box<dyn Error>> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat"))?;
    let fields = stat
        .rsplit_once(") ")
        .ok_or("malformed /proc stat")?
        .1
        .split_whitespace()
        .collect::<Vec<_>>();
    let user = fields
        .get(11)
        .ok_or("user CPU ticks missing")?
        .parse::<u64>()?;
    let system = fields
        .get(12)
        .ok_or("system CPU ticks missing")?
        .parse::<u64>()?;
    Ok(user.saturating_add(system))
}

fn cpu_milliseconds(before: u64, after: u64) -> f64 {
    let ticks_per_second = std::process::Command::new("getconf")
        .arg("CLK_TCK")
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .and_then(|value| value.trim().parse::<u64>().ok())
        .unwrap_or(100);
    after.saturating_sub(before) as f64 * 1_000.0 / ticks_per_second as f64
}

fn unique_paths(paths: &[PathBuf]) -> Vec<&Path> {
    let mut unique = Vec::new();
    for path in paths {
        if !unique.contains(&path.as_path()) {
            unique.push(path.as_path());
        }
    }
    unique
}

fn aggregate_storage_snapshot(paths: &[PathBuf]) -> Result<StorageSnapshot, Box<dyn Error>> {
    unique_paths(paths).into_iter().try_fold(
        StorageSnapshot {
            repository_generation: 0,
            response_accounting_updates: 0,
            tracked_baseline_requests: 0,
            database_bytes: 0,
            wal_bytes: 0,
            shm_bytes: 0,
        },
        |mut aggregate, path| {
            let snapshot = storage_snapshot(path)?;
            aggregate.repository_generation += snapshot.repository_generation;
            aggregate.response_accounting_updates += snapshot.response_accounting_updates;
            aggregate.tracked_baseline_requests += snapshot.tracked_baseline_requests;
            aggregate.database_bytes = aggregate
                .database_bytes
                .saturating_add(snapshot.database_bytes);
            aggregate.wal_bytes = aggregate.wal_bytes.saturating_add(snapshot.wal_bytes);
            aggregate.shm_bytes = aggregate.shm_bytes.saturating_add(snapshot.shm_bytes);
            Ok(aggregate)
        },
    )
}

fn parse_watcher_observation(
    pid: u32,
    resources: &ProcessResources,
    lines: &[String],
) -> WatcherObservation {
    let initialized = lines
        .iter()
        .find(|line| line.contains("repository watcher initialized"));
    let stopped = lines
        .iter()
        .find(|line| line.contains("repository watcher stopped"));
    let backend = if initialized.is_some_and(|line| line.contains("backend=PeriodicPolling")) {
        "periodic_polling"
    } else if initialized.is_some_and(|line| line.contains("backend=Native"))
        || resources.inotify_file_descriptors > 0
    {
        "native"
    } else {
        "unobserved"
    };
    WatcherObservation {
        pid,
        backend,
        admission_entries: initialized.and_then(|line| diagnostic_usize(line, "admission_entries")),
        admission_directories: initialized
            .and_then(|line| diagnostic_usize(line, "admission_directories")),
        admission_complete: initialized
            .and_then(|line| diagnostic_bool(line, "admission_complete")),
        fallback_reason: initialized
            .and_then(|line| diagnostic_field(line, "fallback_reason"))
            .and_then(|value| normalize_fallback_reason(&value)),
        poll_ticks: stopped.and_then(|line| diagnostic_u64(line, "poll_ticks")),
        changed_path_deliveries: stopped
            .and_then(|line| diagnostic_u64(line, "changed_path_deliveries")),
        full_reconciliation_deliveries: stopped
            .and_then(|line| diagnostic_u64(line, "full_reconciliation_deliveries")),
    }
}

fn diagnostic_usize(line: &str, field: &str) -> Option<usize> {
    diagnostic_field(line, field)?.parse().ok()
}

fn diagnostic_u64(line: &str, field: &str) -> Option<u64> {
    diagnostic_field(line, field)?.parse().ok()
}

fn diagnostic_bool(line: &str, field: &str) -> Option<bool> {
    diagnostic_field(line, field)?.parse().ok()
}

fn diagnostic_field(line: &str, field: &str) -> Option<String> {
    let prefix = format!("{field}=");
    line.split_whitespace()
        .find_map(|part| part.strip_prefix(&prefix))
        .map(|value| value.trim_matches(',').to_owned())
}

fn normalize_fallback_reason(value: &str) -> Option<String> {
    let value = value.strip_prefix("Some(")?.strip_suffix(')')?;
    let mut normalized = String::with_capacity(value.len() + 4);
    for (index, character) in value.chars().enumerate() {
        if character.is_ascii_uppercase() {
            if index > 0 {
                normalized.push('_');
            }
            normalized.push(character.to_ascii_lowercase());
        } else {
            normalized.push(character);
        }
    }
    Some(normalized)
}

fn measure_takeover(
    repository: &Path,
    database: &Path,
    processes: &mut [McpProcess],
    leader_pid: u32,
    timeout: Duration,
) -> Result<TakeoverMeasurement, Box<dyn Error>> {
    let leader_index = processes
        .iter()
        .position(|process| process.pid() == leader_pid)
        .ok_or("leader process missing")?;
    let started = Instant::now();
    processes[leader_index].kill_now()?;
    fs::write(
        repository.join("file_00000.rs"),
        "pub fn changed_after_leader_crash() -> usize { 2 }\n",
    )?;
    wait_for_generation(database, 2, timeout)?;
    let live_pids = processes
        .iter()
        .filter(|process| !process.stopped)
        .map(McpProcess::pid)
        .collect::<Vec<_>>();
    let leadership_path = PathBuf::from(format!("{}.leader.lock", database.display()));
    let successor_owners = wait_for_lock_owner(&leadership_path, &live_pids, timeout)?;
    if successor_owners.len() != 1 || successor_owners[0] == leader_pid {
        return Err(format!("invalid successor leadership: {successor_owners:?}").into());
    }
    let watcher_processes_after_takeover = live_pids
        .iter()
        .map(|pid| sample_process(*pid, database, *pid == successor_owners[0]))
        .collect::<Result<Vec<_>, _>>()?
        .iter()
        .filter(|sample| sample.inotify_file_descriptors > 0)
        .count();
    if watcher_processes_after_takeover != 1 {
        return Err("follower takeover did not restore exactly one watcher".into());
    }
    Ok(TakeoverMeasurement {
        killed_leader_pid: leader_pid,
        successor_leader_pid: successor_owners[0],
        takeover_ms: started.elapsed().as_secs_f64() * 1_000.0,
        repository_generation: 2,
        watcher_processes_after_takeover,
    })
}

fn write_fixture(
    repository: &Path,
    files: usize,
    functions_per_file: usize,
) -> Result<(), Box<dyn Error>> {
    for file_index in 0..files {
        let mut source = String::new();
        for function_index in 0..functions_per_file {
            source.push_str(&format!(
                "pub fn item_{file_index}_{function_index}() -> usize {{ {} }}\n",
                file_index + function_index
            ));
        }
        fs::write(repository.join(format!("file_{file_index:05}.rs")), source)?;
    }
    Ok(())
}

fn sample_process(
    pid: u32,
    database: &Path,
    leader: bool,
) -> Result<ProcessResources, Box<dyn Error>> {
    let status = fs::read_to_string(format!("/proc/{pid}/status"))?;
    let fd_root = PathBuf::from(format!("/proc/{pid}/fd"));
    let mut file_descriptors = 0usize;
    let mut database_file_descriptors = 0usize;
    let mut sqlite_artifact_file_descriptors = 0usize;
    let mut inotify_file_descriptors = 0usize;
    let database_text = database.display().to_string();
    for entry in fs::read_dir(fd_root)? {
        let target = fs::read_link(entry?.path())?;
        let target_text = target.to_string_lossy();
        file_descriptors += 1;
        if target_text == database_text {
            database_file_descriptors += 1;
        }
        if target_text == database_text
            || target_text == format!("{database_text}-wal")
            || target_text == format!("{database_text}-shm")
        {
            sqlite_artifact_file_descriptors += 1;
        }
        if target_text == "anon_inode:inotify" {
            inotify_file_descriptors += 1;
        }
    }
    Ok(ProcessResources {
        pid,
        role: if leader { "leader" } else { "follower" },
        rss_kib: status_kib(&status, "VmRSS:")?,
        peak_rss_kib: status_kib(&status, "VmHWM:")?,
        threads: status_number(&status, "Threads:")?,
        file_descriptors,
        database_file_descriptors,
        sqlite_artifact_file_descriptors,
        estimated_established_read_connections: database_file_descriptors.saturating_sub(1),
        inotify_file_descriptors,
    })
}

fn status_kib(status: &str, key: &str) -> Result<usize, Box<dyn Error>> {
    status_number(status, key)
}

fn status_number(status: &str, key: &str) -> Result<usize, Box<dyn Error>> {
    status
        .lines()
        .find(|line| line.starts_with(key))
        .and_then(|line| line.split_whitespace().nth(1))
        .ok_or_else(|| format!("{key} missing from /proc status").into())
        .and_then(|value| Ok(value.parse()?))
}

fn lock_owner_pids(path: &Path, candidates: &[u32]) -> Result<Vec<u32>, Box<dyn Error>> {
    let inode = file_inode(path)?;
    Ok(parse_lock_owner_pids(
        &fs::read_to_string("/proc/locks")?,
        inode,
        candidates,
    ))
}

#[cfg(unix)]
fn file_inode(path: &Path) -> Result<u64, Box<dyn Error>> {
    Ok(fs::metadata(path)?.ino())
}

#[cfg(not(unix))]
fn file_inode(_path: &Path) -> Result<u64, Box<dyn Error>> {
    Err("leadership-lock observation requires Unix file metadata".into())
}

fn parse_lock_owner_pids(contents: &str, inode: u64, candidates: &[u32]) -> Vec<u32> {
    let mut owners = contents
        .lines()
        .filter_map(|line| {
            let fields = line.split_whitespace().collect::<Vec<_>>();
            if fields.len() < 6 {
                return None;
            }
            let pid = fields[4].parse::<u32>().ok()?;
            let locked_inode = fields[5].rsplit(':').next()?.parse::<u64>().ok()?;
            (locked_inode == inode && candidates.contains(&pid)).then_some(pid)
        })
        .collect::<Vec<_>>();
    owners.sort_unstable();
    owners.dedup();
    owners
}

fn wait_for_lock_owner(
    path: &Path,
    candidates: &[u32],
    timeout: Duration,
) -> Result<Vec<u32>, Box<dyn Error>> {
    let deadline = Instant::now() + timeout;
    loop {
        let owners = lock_owner_pids(path, candidates)?;
        if owners.len() == 1 {
            return Ok(owners);
        }
        if Instant::now() >= deadline {
            return Err(format!("leadership lock did not settle: {owners:?}").into());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn storage_snapshot(database: &Path) -> Result<StorageSnapshot, Box<dyn Error>> {
    let connection = Connection::open_with_flags(database, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    connection.busy_timeout(Duration::from_millis(500))?;
    Ok(StorageSnapshot {
        repository_generation: connection.query_row(
            "SELECT repository_generation FROM meta WHERE id = 1",
            [],
            |row| row.get(0),
        )?,
        response_accounting_updates: connection.query_row(
            "SELECT COALESCE(SUM(response_tracked_requests), 0) FROM token_savings",
            [],
            |row| row.get(0),
        )?,
        tracked_baseline_requests: connection.query_row(
            "SELECT COALESCE(SUM(tracked_requests), 0) FROM token_savings",
            [],
            |row| row.get(0),
        )?,
        database_bytes: file_size(database),
        wal_bytes: file_size(PathBuf::from(format!("{}-wal", database.display()))),
        shm_bytes: file_size(PathBuf::from(format!("{}-shm", database.display()))),
    })
}

fn wait_for_generation(
    database: &Path,
    generation: i64,
    timeout: Duration,
) -> Result<(), Box<dyn Error>> {
    let deadline = Instant::now() + timeout;
    loop {
        if storage_snapshot(database)
            .is_ok_and(|snapshot| snapshot.repository_generation >= generation)
        {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(format!("repository generation did not reach {generation}").into());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn file_size(path: impl AsRef<Path>) -> u64 {
    fs::metadata(path).map_or(0, |metadata| metadata.len())
}

fn make_decision(runs: &[RunMeasurement], thresholds: DecisionThresholds) -> Decision {
    let one_runs = runs
        .iter()
        .filter(|run| run.process_count == 1 && run.topology == Topology::SharedCache)
        .collect::<Vec<_>>();
    if one_runs.is_empty() {
        return insufficient_decision("missing one-process baseline");
    }
    let four_runs = runs
        .iter()
        .filter(|run| run.process_count == 4 && run.topology == Topology::SharedCache)
        .collect::<Vec<_>>();
    if four_runs.is_empty() {
        return insufficient_decision("missing four-process comparison");
    }
    if !runs
        .iter()
        .any(|run| run.process_count == 8 && run.topology == Topology::SharedCache)
    {
        return insufficient_decision("missing eight-process shared-cache comparison");
    }
    if !runs
        .iter()
        .any(|run| run.process_count == 1 && run.topology == Topology::IndependentCaches)
        || !runs
            .iter()
            .any(|run| run.process_count == 8 && run.topology == Topology::IndependentCaches)
    {
        return insufficient_decision("missing independent-cache CPU comparison");
    }
    let one_rss = average_by(&one_runs, |run| run.aggregate_rss_kib as f64);
    let four_rss = average_by(&four_runs, |run| run.aggregate_rss_kib as f64);
    let incremental_rss = (four_rss - one_rss).max(0.0) / 3.0 / 1_024.0;
    let startup_ratio = safe_ratio(
        average_by(&four_runs, |run| run.startup_to_ready.p95_ms),
        average_by(&one_runs, |run| run.startup_to_ready.p95_ms),
    );
    let warm_ratio = safe_ratio(
        average_by(&four_runs, |run| run.warm_query.p95_ms),
        average_by(&one_runs, |run| run.warm_query.p95_ms),
    );
    let one_wal_per_query = average_by(&one_runs, wal_bytes_per_query);
    let four_wal_per_query = average_by(&four_runs, wal_bytes_per_query);
    let wal_ratio = safe_ratio(four_wal_per_query, one_wal_per_query);
    let eight_process_cpu_ratio =
        workload_cpu_ratio(runs, Topology::SharedCache, Workload::Context, 8, 1);
    let independent_cold_cpu_ratio =
        cold_cpu_per_repository_ratio(runs, Topology::IndependentCaches, 8, 1);
    let mut reasons = Vec::new();
    if incremental_rss > thresholds.max_incremental_rss_mib_per_follower {
        reasons.push(format!(
            "incremental follower RSS {incremental_rss:.2} MiB exceeded {:.2} MiB",
            thresholds.max_incremental_rss_mib_per_follower
        ));
    }
    if startup_ratio > thresholds.max_startup_p95_ratio {
        reasons.push(format!(
            "startup p95 ratio {startup_ratio:.2} exceeded {:.2}",
            thresholds.max_startup_p95_ratio
        ));
    }
    if warm_ratio > thresholds.max_warm_p95_ratio {
        reasons.push(format!(
            "warm p95 ratio {warm_ratio:.2} exceeded {:.2}",
            thresholds.max_warm_p95_ratio
        ));
    }
    if wal_ratio > thresholds.max_normalized_wal_bytes_per_query_ratio {
        reasons.push(format!(
            "normalized WAL/query ratio {wal_ratio:.2} exceeded {:.2}",
            thresholds.max_normalized_wal_bytes_per_query_ratio
        ));
    }
    if eight_process_cpu_ratio
        .is_some_and(|ratio| ratio > thresholds.max_eight_process_cpu_per_query_ratio)
    {
        reasons.push(format!(
            "eight-process context CPU/query ratio {:.2} exceeded {:.2}",
            eight_process_cpu_ratio.unwrap_or_default(),
            thresholds.max_eight_process_cpu_per_query_ratio
        ));
    }
    if independent_cold_cpu_ratio
        .is_some_and(|ratio| ratio > thresholds.max_independent_cold_cpu_per_repository_ratio)
    {
        reasons.push(format!(
            "independent cold CPU/repository ratio {:.2} exceeded {:.2}",
            independent_cold_cpu_ratio.unwrap_or_default(),
            thresholds.max_independent_cold_cpu_per_repository_ratio
        ));
    }
    let parity_mismatches = runs.iter().map(|run| run.parity_mismatches).sum::<usize>();
    if parity_mismatches > 0 {
        reasons.push(format!(
            "complete observable response parity had {parity_mismatches} mismatches"
        ));
    }
    for run in runs {
        let expected_owners = run.topology.expected_leaders(run.process_count);
        if run.leader_lock_owners != expected_owners
            || run.generation_publications != expected_owners as i64
        {
            reasons.push(format!(
                "{:?} with {} processes violated repository-owner publication invariants",
                run.topology, run.process_count
            ));
        }
        if run.processes.iter().any(|process| {
            process.resources.estimated_established_read_connections
                > thresholds.max_established_read_connections_per_process
        }) {
            reasons.push(format!(
                "{} processes exceeded the per-process read-connection bound",
                run.process_count
            ));
        }
        if run
            .takeover
            .as_ref()
            .is_some_and(|takeover| takeover.takeover_ms > thresholds.max_takeover_ms)
        {
            reasons.push(format!(
                "{}-process {:?} order {} takeover exceeded {:.0} ms",
                run.process_count, run.topology, run.order_index, thresholds.max_takeover_ms
            ));
        }
    }
    Decision {
        recommendation: if parity_mismatches > 0 {
            "invalid_measurement"
        } else if reasons.is_empty() {
            "retain_stdio"
        } else {
            "investigate_host_wide_admission"
        },
        reasons,
        incremental_rss_mib_per_follower: Some(incremental_rss),
        startup_p95_ratio: Some(startup_ratio),
        warm_p95_ratio: Some(warm_ratio),
        normalized_wal_bytes_per_query_ratio: Some(wal_ratio),
        eight_process_cpu_per_query_ratio: eight_process_cpu_ratio,
        independent_cold_cpu_per_repository_ratio: independent_cold_cpu_ratio,
    }
}

fn average_by<F>(runs: &[&RunMeasurement], value: F) -> f64
where
    F: Fn(&RunMeasurement) -> f64,
{
    runs.iter().map(|run| value(run)).sum::<f64>() / runs.len() as f64
}

fn insufficient_decision(reason: &str) -> Decision {
    Decision {
        recommendation: "insufficient_evidence",
        reasons: vec![reason.into()],
        incremental_rss_mib_per_follower: None,
        startup_p95_ratio: None,
        warm_p95_ratio: None,
        normalized_wal_bytes_per_query_ratio: None,
        eight_process_cpu_per_query_ratio: None,
        independent_cold_cpu_per_repository_ratio: None,
    }
}

fn workload_cpu_ratio(
    runs: &[RunMeasurement],
    topology: Topology,
    workload: Workload,
    numerator_processes: usize,
    denominator_processes: usize,
) -> Option<f64> {
    let average = |process_count| {
        let samples = runs
            .iter()
            .filter(|run| run.topology == topology && run.process_count == process_count)
            .filter_map(|run| {
                run.workloads
                    .iter()
                    .find(|measurement| measurement.workload == workload)
                    .and_then(|measurement| measurement.cpu.cpu_milliseconds_per_operation)
            })
            .collect::<Vec<_>>();
        (!samples.is_empty()).then(|| samples.iter().sum::<f64>() / samples.len() as f64)
    };
    Some(safe_ratio(
        average(numerator_processes)?,
        average(denominator_processes)?,
    ))
}

fn cold_cpu_per_repository_ratio(
    runs: &[RunMeasurement],
    topology: Topology,
    numerator_processes: usize,
    denominator_processes: usize,
) -> Option<f64> {
    let average = |process_count| {
        let samples = runs
            .iter()
            .filter(|run| run.topology == topology && run.process_count == process_count)
            .map(|run| run.cold_startup_cpu.cpu_milliseconds / run.repository_count.max(1) as f64)
            .collect::<Vec<_>>();
        (!samples.is_empty()).then(|| samples.iter().sum::<f64>() / samples.len() as f64)
    };
    Some(safe_ratio(
        average(numerator_processes)?,
        average(denominator_processes)?,
    ))
}

fn wal_bytes_per_query(run: &RunMeasurement) -> f64 {
    run.storage_after_queries
        .wal_bytes
        .saturating_sub(run.storage_before_queries.wal_bytes) as f64
        / run.expected_response_accounting_updates.max(1) as f64
}

fn safe_ratio(numerator: f64, denominator: f64) -> f64 {
    if denominator <= f64::EPSILON {
        if numerator <= f64::EPSILON {
            1.0
        } else {
            f64::INFINITY
        }
    } else {
        numerator / denominator
    }
}

fn percentile(values: &[f64], quantile: f64) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    let rank = (sorted.len() - 1) as f64 * quantile.clamp(0.0, 1.0);
    let lower = rank.floor() as usize;
    let upper = rank.ceil() as usize;
    let weight = rank - lower as f64;
    sorted[lower] * (1.0 - weight) + sorted[upper] * weight
}

fn hash_file(path: &Path) -> Result<String, Box<dyn Error>> {
    let mut file = fs::File::open(path)?;
    let mut hasher = blake3::Hasher::new();
    let mut buffer = [0u8; 1024 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hasher.finalize().to_hex().to_string())
}

fn path_str(path: &Path) -> Result<&str, Box<dyn Error>> {
    path.to_str().ok_or_else(|| "path is not UTF-8".into())
}

#[cfg(unix)]
use std::os::unix::fs::MetadataExt;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentile_interpolates_deterministically() {
        let values = [40.0, 10.0, 30.0, 20.0];
        assert_eq!(percentile(&values, 0.0), 10.0);
        assert_eq!(percentile(&values, 0.5), 25.0);
        assert_eq!(percentile(&values, 1.0), 40.0);
    }

    #[test]
    fn process_counts_are_sorted_deduplicated_and_bounded() {
        assert_eq!(parse_process_counts("4, 1,2,4").unwrap(), vec![1, 2, 4]);
        assert!(parse_process_counts("0,1").is_err());
        assert!(parse_process_counts("1,17").is_err());
        validate_max_index_workers(1).expect("one worker");
        validate_max_index_workers(MAX_INDEX_WORKERS).expect("maximum workers");
        assert!(validate_max_index_workers(0).is_err());
        assert!(validate_max_index_workers(MAX_INDEX_WORKERS + 1).is_err());
    }

    #[test]
    fn topology_order_is_abba() {
        assert_eq!(
            Topology::run_order(),
            [
                Topology::SharedCache,
                Topology::IndependentCaches,
                Topology::IndependentCaches,
                Topology::SharedCache,
            ]
        );
    }

    #[test]
    fn response_parity_ignores_only_generated_identifiers() {
        let first = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": {
                "structuredContent": {
                    "receipt_id": "first",
                    "meta": {
                        "repository_id": "repository-first",
                        "freshness": "current",
                        "path_and_metadata_tokens": 10,
                        "total_response_tokens": 20,
                        "total_response_tokens": 30
                    },
                    "files": ["src/lib.rs"]
                },
                "content": [{
                    "type": "text",
                    "text": "{\"receipt_id\":\"nested-first\",\"repository_id\":\"repository-first\",\"count\":1}"
                }]
            }
        });
        let second = json!({
            "jsonrpc": "2.0",
            "id": 99,
            "result": {
                "structuredContent": {
                    "receipt_id": "second",
                    "meta": {
                        "repository_id": "repository-second",
                        "freshness": "reconciling",
                        "path_and_metadata_tokens": 11,
                        "total_response_tokens": 21,
                        "total_response_tokens": 32
                    },
                    "files": ["src/lib.rs"]
                },
                "content": [{
                    "type": "text",
                    "text": "{\"receipt_id\":\"nested-second\",\"repository_id\":\"repository-second\",\"count\":1}"
                }]
            }
        });
        let drifted = json!({
            "jsonrpc": "2.0",
            "id": 100,
            "result": {
                "structuredContent": {
                    "receipt_id": "third",
                    "meta": {"repository_id": "repository-third"},
                    "files": ["src/main.rs"]
                },
                "content": [{
                    "type": "text",
                    "text": "{\"receipt_id\":\"nested-third\",\"repository_id\":\"repository-third\",\"count\":1}"
                }]
            }
        });

        assert_eq!(
            normalize_response(first.clone()),
            normalize_response(second)
        );
        assert_ne!(normalize_response(first), normalize_response(drifted));
    }

    #[test]
    fn watcher_diagnostics_include_backend_admission_and_delivery_counts() {
        let resources = ProcessResources {
            pid: 42,
            role: "leader",
            rss_kib: 1,
            peak_rss_kib: 1,
            threads: 1,
            file_descriptors: 1,
            database_file_descriptors: 1,
            sqlite_artifact_file_descriptors: 1,
            estimated_established_read_connections: 0,
            inotify_file_descriptors: 0,
        };
        let lines = vec![
            "repository watcher initialized backend=PeriodicPolling \
             fallback_reason=Some(AdmissionDirectoryLimit) admission_entries=50002 \
             admission_directories=50001 admission_complete=false"
                .to_owned(),
            "repository watcher stopped backend=PeriodicPolling poll_ticks=1 \
             changed_path_deliveries=0 full_reconciliation_deliveries=1"
                .to_owned(),
        ];

        let observation = parse_watcher_observation(42, &resources, &lines);
        assert_eq!(observation.backend, "periodic_polling");
        assert_eq!(
            observation.fallback_reason.as_deref(),
            Some("admission_directory_limit")
        );
        assert_eq!(observation.admission_entries, Some(50_002));
        assert_eq!(observation.admission_directories, Some(50_001));
        assert_eq!(observation.admission_complete, Some(false));
        assert_eq!(observation.poll_ticks, Some(1));
        assert_eq!(observation.changed_path_deliveries, Some(0));
        assert_eq!(observation.full_reconciliation_deliveries, Some(1));
    }

    #[test]
    fn parity_fingerprints_are_canonical_and_diffs_are_bounded_paths() {
        let first: Value = serde_json::from_str(r#"{"b":1,"a":{"x":1}}"#).unwrap();
        let reordered: Value = serde_json::from_str(r#"{"a":{"x":1},"b":1}"#).unwrap();
        let changed: Value = serde_json::from_str(r#"{"a":{"x":2},"b":1}"#).unwrap();

        assert_eq!(
            response_fingerprint(&first),
            response_fingerprint(&reordered)
        );
        let mut paths = Vec::new();
        collect_json_diff_paths(&first, &changed, "", &mut paths);
        assert_eq!(paths, ["/a/x"]);
    }

    #[test]
    fn proc_lock_parser_filters_inode_and_candidate_pid() {
        let locks = "\
12: FLOCK  ADVISORY  WRITE 123 00:44:9001 0 EOF\n\
13: FLOCK  ADVISORY  WRITE 456 00:44:9002 0 EOF\n";
        assert_eq!(parse_lock_owner_pids(locks, 9001, &[123, 456]), vec![123]);
        assert!(parse_lock_owner_pids(locks, 9001, &[456]).is_empty());
    }
}
