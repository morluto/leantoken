//! Reproducible Linux resource profile for several stdio MCP processes.
//!
//! Build the product binary first, then run this example in release mode:
//!
//! ```text
//! cargo build --release
//! cargo run --release --example mcp_multiprocess_profile -- \
//!   --binary target/release/leantoken --output report.json
//! ```

use std::{
    error::Error,
    fs,
    io::{BufRead, BufReader, Read, Write},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, Stdio},
    sync::mpsc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use clap::Parser;
use rusqlite::{Connection, OpenFlags};
use serde::Serialize;
use serde_json::{Value, json};

const MAX_PROCESSES: usize = 16;
const MAX_FIXTURE_FILES: usize = 10_000;
const MAX_FUNCTIONS_PER_FILE: usize = 1_000;
const MAX_WARM_ITERATIONS: usize = 1_000;

#[derive(Debug, Parser)]
#[command(about = "Measure 1/2/4 stdio MCP processes against one cache")]
struct Args {
    /// Release-mode LeanToken executable to launch.
    #[arg(long, default_value = "target/release/leantoken")]
    binary: PathBuf,
    /// Comma-separated process counts. Include 1 and 4 for a decision.
    #[arg(long, default_value = "1,2,4")]
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
    /// Per-operation timeout.
    #[arg(long, default_value_t = 20)]
    timeout_seconds: u64,
    /// Write pretty JSON here in addition to stdout.
    #[arg(long)]
    output: Option<PathBuf>,
}

#[derive(Debug, Clone, Copy, Serialize)]
struct DecisionThresholds {
    max_incremental_rss_mib_per_follower: f64,
    max_startup_p95_ratio: f64,
    max_warm_p95_ratio: f64,
    max_normalized_wal_bytes_per_query_ratio: f64,
    max_established_read_connections_per_process: usize,
    max_takeover_ms: f64,
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

#[derive(Debug, Clone, Serialize)]
struct ProcessMeasurement {
    resources: ProcessResources,
    startup_to_ready_ms: f64,
    cold_query_ms: f64,
    warm_query: LatencySummary,
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
    process_count: usize,
    leader_pid: u32,
    leader_lock_owners: usize,
    watcher_processes: usize,
    aggregate_rss_kib: usize,
    aggregate_peak_rss_kib: usize,
    aggregate_threads: usize,
    aggregate_file_descriptors: usize,
    aggregate_estimated_read_connections: usize,
    startup_to_ready: LatencySummary,
    cold_query: LatencySummary,
    warm_query: LatencySummary,
    storage_before_queries: StorageSnapshot,
    storage_after_queries: StorageSnapshot,
    generation_publications: i64,
    expected_response_accounting_updates: usize,
    observed_response_accounting_updates: i64,
    processes: Vec<ProcessMeasurement>,
    takeover: Option<TakeoverMeasurement>,
}

#[derive(Debug, Clone, Serialize)]
struct Decision {
    recommendation: &'static str,
    reasons: Vec<String>,
    incremental_rss_mib_per_follower: Option<f64>,
    startup_p95_ratio: Option<f64>,
    warm_p95_ratio: Option<f64>,
    normalized_wal_bytes_per_query_ratio: Option<f64>,
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
    fixture_files: usize,
    functions_per_file: usize,
    warm_iterations_per_process: usize,
    thresholds: DecisionThresholds,
    runs: Vec<RunMeasurement>,
    decision: Decision,
    observation_limits: Vec<&'static str>,
}

struct McpProcess {
    child: Child,
    stdin: Option<ChildStdin>,
    lines: mpsc::Receiver<String>,
    next_id: u64,
    stopped: bool,
}

impl McpProcess {
    fn spawn(binary: &Path, root: &Path, database: &Path) -> Result<Self, Box<dyn Error>> {
        let mut child = std::process::Command::new(binary)
            .args(["--root", path_str(root)?, "--database", path_str(database)?])
            .arg("mcp")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;
        let stdin = child.stdin.take().ok_or("MCP stdin unavailable")?;
        let stdout = child.stdout.take().ok_or("MCP stdout unavailable")?;
        let (sender, lines) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                if sender.send(line).is_err() {
                    break;
                }
            }
        });
        Ok(Self {
            child,
            stdin: Some(stdin),
            lines,
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
        let id = self.take_id();
        let started = Instant::now();
        self.send(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "tools/call",
            "params": {
                "name": "files",
                "arguments": {
                    "operation": {"kind": "tree"},
                    "max_results": 10
                }
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
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
        self.stopped = true;
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
    let mut runs = Vec::with_capacity(process_counts.len());
    for process_count in process_counts {
        runs.push(run_measurement(
            &binary,
            process_count,
            args.files,
            args.functions_per_file,
            args.warm_iterations,
            timeout,
        )?);
    }
    let decision = make_decision(&runs, thresholds);
    let report = Report {
        schema_version: 1,
        generated_at_unix_seconds: SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs(),
        platform: "linux-procfs",
        kernel_release: fs::read_to_string("/proc/sys/kernel/osrelease")?
            .trim()
            .to_owned(),
        logical_cpus: std::thread::available_parallelism()?.get(),
        binary: binary.display().to_string(),
        binary_blake3: hash_file(&binary)?,
        fixture_files: args.files,
        functions_per_file: args.functions_per_file,
        warm_iterations_per_process: args.warm_iterations,
        thresholds,
        runs,
        decision,
        observation_limits: vec![
            "RSS, HWM, threads, file descriptors, lock ownership, and watcher ownership require Linux /proc.",
            "Established read connections are inferred as main-database file descriptors minus the one writer connection.",
            "SQLite does not expose cross-process statement counts; successful response-accounting updates and generation publications are reported as observed writes.",
            "Latency is host-local wall time from one orchestrator and is comparable only on the same host and release build.",
            "Startup readiness and concurrent-query responses are observed by one orchestrator in process order, so later processes can include bounded client-side receipt delay.",
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
    if args.timeout_seconds == 0 || args.timeout_seconds > 300 {
        return Err("--timeout-seconds must be within 1..=300".into());
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

fn run_measurement(
    binary: &Path,
    process_count: usize,
    files: usize,
    functions_per_file: usize,
    warm_iterations: usize,
    timeout: Duration,
) -> Result<RunMeasurement, Box<dyn Error>> {
    let workspace = tempfile::tempdir()?;
    let repository = workspace.path().join("repository");
    fs::create_dir(&repository)?;
    write_fixture(&repository, files, functions_per_file)?;
    let database = workspace.path().join("cache").join("index.sqlite");
    fs::create_dir_all(database.parent().ok_or("database parent missing")?)?;

    let started = Instant::now();
    let mut processes = (0..process_count)
        .map(|_| McpProcess::spawn(binary, &repository, &database))
        .collect::<Result<Vec<_>, _>>()?;
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
    wait_for_generation(&database, 1, timeout)?;
    let storage_before_queries = storage_snapshot(&database)?;

    let cold_values = measure_query_round(&mut processes, timeout)?;
    let mut warm_by_process = vec![Vec::with_capacity(warm_iterations); process_count];
    for _ in 0..warm_iterations {
        for (index, latency) in measure_query_round(&mut processes, timeout)?
            .into_iter()
            .enumerate()
        {
            warm_by_process[index].push(latency);
        }
    }
    std::thread::sleep(Duration::from_millis(100));

    let pids = processes.iter().map(McpProcess::pid).collect::<Vec<_>>();
    let leadership_path = PathBuf::from(format!("{}.leader.lock", database.display()));
    let lock_owners = lock_owner_pids(&leadership_path, &pids)?;
    if lock_owners.len() != 1 {
        return Err(format!("expected one leadership lock owner, found {lock_owners:?}").into());
    }
    let leader_pid = lock_owners[0];
    let mut resources = pids
        .iter()
        .map(|pid| sample_process(*pid, &database, *pid == leader_pid))
        .collect::<Result<Vec<_>, _>>()?;
    resources.sort_by_key(|sample| sample.pid);
    let watcher_processes = resources
        .iter()
        .filter(|sample| sample.inotify_file_descriptors > 0)
        .count();
    if watcher_processes != 1
        || resources
            .iter()
            .find(|sample| sample.pid == leader_pid)
            .is_none_or(|sample| sample.inotify_file_descriptors == 0)
    {
        return Err(
            format!("watcher ownership did not match leader {leader_pid}: {resources:?}").into(),
        );
    }

    let mut process_measurements = Vec::with_capacity(process_count);
    for resource in resources.iter().cloned() {
        let source_index = pids
            .iter()
            .position(|pid| *pid == resource.pid)
            .ok_or("sampled PID was not launched")?;
        process_measurements.push(ProcessMeasurement {
            resources: resource,
            startup_to_ready_ms: startup_ms[source_index],
            cold_query_ms: cold_values[source_index],
            warm_query: LatencySummary::from_values(&warm_by_process[source_index]),
        });
    }
    let warm_values = warm_by_process
        .iter()
        .flatten()
        .copied()
        .collect::<Vec<_>>();
    let storage_after_queries = storage_snapshot(&database)?;
    if storage_after_queries.repository_generation != 1 {
        return Err(format!(
            "cold startup published {} generations instead of one",
            storage_after_queries.repository_generation
        )
        .into());
    }

    let takeover = if process_count > 1 {
        Some(measure_takeover(
            &repository,
            &database,
            &mut processes,
            leader_pid,
            timeout,
        )?)
    } else {
        None
    };
    for process in &mut processes {
        process.stop();
    }

    Ok(RunMeasurement {
        process_count,
        leader_pid,
        leader_lock_owners: lock_owners.len(),
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
        cold_query: LatencySummary::from_values(&cold_values),
        warm_query: LatencySummary::from_values(&warm_values),
        storage_before_queries,
        storage_after_queries,
        generation_publications: storage_after_queries.repository_generation,
        expected_response_accounting_updates: process_count * (warm_iterations + 1),
        observed_response_accounting_updates: storage_after_queries
            .response_accounting_updates
            .saturating_sub(storage_before_queries.response_accounting_updates),
        processes: process_measurements,
        takeover,
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

fn successful_tool_response(response: &Value) -> bool {
    response["result"]["isError"] != true
        && response["result"]["structuredContent"]["status"] != "retryable"
        && response.get("error").is_none()
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
    let Some(one) = runs.iter().find(|run| run.process_count == 1) else {
        return insufficient_decision("missing one-process baseline");
    };
    let Some(four) = runs.iter().find(|run| run.process_count == 4) else {
        return insufficient_decision("missing four-process comparison");
    };
    let incremental_rss =
        four.aggregate_rss_kib.saturating_sub(one.aggregate_rss_kib) as f64 / 3.0 / 1_024.0;
    let startup_ratio = safe_ratio(four.startup_to_ready.p95_ms, one.startup_to_ready.p95_ms);
    let warm_ratio = safe_ratio(four.warm_query.p95_ms, one.warm_query.p95_ms);
    let one_wal_per_query = wal_bytes_per_query(one);
    let four_wal_per_query = wal_bytes_per_query(four);
    let wal_ratio = safe_ratio(four_wal_per_query, one_wal_per_query);
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
    for run in runs {
        if run.leader_lock_owners != 1
            || run.watcher_processes != 1
            || run.generation_publications != 1
        {
            reasons.push(format!(
                "{} processes violated single-owner publication invariants",
                run.process_count
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
                "{}-process takeover exceeded {:.0} ms",
                run.process_count, thresholds.max_takeover_ms
            ));
        }
    }
    Decision {
        recommendation: if reasons.is_empty() {
            "retain_stdio"
        } else {
            "investigate_shared_daemon"
        },
        reasons,
        incremental_rss_mib_per_follower: Some(incremental_rss),
        startup_p95_ratio: Some(startup_ratio),
        warm_p95_ratio: Some(warm_ratio),
        normalized_wal_bytes_per_query_ratio: Some(wal_ratio),
    }
}

fn insufficient_decision(reason: &str) -> Decision {
    Decision {
        recommendation: "insufficient_evidence",
        reasons: vec![reason.into()],
        incremental_rss_mib_per_follower: None,
        startup_p95_ratio: None,
        warm_p95_ratio: None,
        normalized_wal_bytes_per_query_ratio: None,
    }
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
