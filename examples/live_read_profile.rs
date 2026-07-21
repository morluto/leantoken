use std::error::Error;
use std::fs::{self, OpenOptions};
use std::hint::black_box;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use clap::Parser;
use leantoken::model::ReadRequest;
use leantoken::repository::{DiscoveredFile, discover_files};
use leantoken::services::Services;
use leantoken::{Config, DiscoveryLimits};
use serde::Serialize;
use tempfile::TempDir;

type AnyResult<T> = Result<T, Box<dyn Error>>;

#[derive(Debug, Parser)]
#[command(about = "Profile live file reads without adding a process-local body cache")]
struct Args {
    /// Existing clean Git checkout copied into a disposable profile root.
    #[arg(long, value_name = "PATH")]
    repository: PathBuf,
    /// Public corpus name or URL included in the report.
    #[arg(long)]
    repository_label: String,
    /// Samples in each direct, service, and memory measurement.
    #[arg(long, default_value_t = 200)]
    iterations: usize,
    /// Files in the repeated-read working set.
    #[arg(long, default_value_t = 8)]
    hot_set: usize,
    /// Source-token ceiling for each service read.
    #[arg(long, default_value_t = 512)]
    max_tokens: usize,
    /// Touched bytes retained while measuring the pressure condition.
    #[arg(long, default_value_t = 256 * 1024 * 1024)]
    pressure_bytes: usize,
    /// JSON report destination.
    #[arg(long, default_value = "target/live-read-profile.json")]
    output: PathBuf,
}

#[derive(Debug, Serialize)]
struct Report {
    schema_version: u32,
    leantoken_version: &'static str,
    leantoken_git_revision: Option<String>,
    leantoken_worktree_dirty: Option<bool>,
    host_os: &'static str,
    host_arch: &'static str,
    release_build: bool,
    corpus: CorpusReport,
    initial_index_ms: f64,
    post_index_spread_direct: ReadMeasurement,
    warm_hot_direct: ReadMeasurement,
    warm_hot_memory_copy: ReadMeasurement,
    warm_hot_service: ServiceMeasurement,
    warm_spread_service: ServiceMeasurement,
    pressure: PressureMeasurement,
    live_change: LiveChangeCheck,
    hot_payload_bytes: u64,
    limitations: Vec<String>,
}

#[derive(Debug, Serialize)]
struct CorpusReport {
    source_repository: String,
    revision: String,
    files: usize,
    total_bytes: u64,
    text_read_targets: usize,
    iterations: usize,
    hot_set_files: usize,
    max_tokens: usize,
    pressure_bytes: usize,
}

#[derive(Debug, Serialize)]
struct ReadMeasurement {
    timing: TimingStats,
    total_bytes: u64,
    mean_bytes: f64,
    unique_files: usize,
}

#[derive(Debug, Serialize)]
struct ServiceMeasurement {
    request: TimingStats,
    serialization: TimingStats,
    complete: TimingStats,
    total_source_bytes: u64,
    total_wire_bytes: u64,
    mean_source_bytes: f64,
    mean_wire_bytes: f64,
    unique_files: usize,
}

#[derive(Debug, Serialize)]
struct PressureMeasurement {
    retained_touched_bytes: usize,
    direct_hot: ReadMeasurement,
    service_hot: ServiceMeasurement,
}

#[derive(Debug, Serialize)]
struct LiveChangeCheck {
    path: String,
    generation_before: u64,
    stale_generation: u64,
    generation_after: u64,
    stale_before_reconciliation: bool,
    current_after_reconciliation: bool,
}

#[derive(Debug, Serialize)]
struct TimingStats {
    samples: usize,
    total_ms: f64,
    mean_us: f64,
    min_us: f64,
    p50_us: f64,
    p95_us: f64,
    max_us: f64,
}

#[derive(Debug, Clone)]
struct ReadTarget {
    absolute_path: PathBuf,
    relative_path: String,
    size_bytes: u64,
}

struct CorpusSnapshot {
    _directory: TempDir,
    root: PathBuf,
    database: PathBuf,
    revision: String,
    files: usize,
    total_bytes: u64,
}

#[tokio::main]
async fn main() -> AnyResult<()> {
    let args = Args::parse();
    validate_args(&args)?;
    let report = run_profile(&args).await?;
    let json = serde_json::to_string_pretty(&report)?;
    if let Some(parent) = args.output.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&args.output, format!("{json}\n"))?;
    println!("{json}");
    Ok(())
}

fn validate_args(args: &Args) -> AnyResult<()> {
    if !args.repository.is_dir() {
        return Err(invalid_input("--repository must name a Git checkout"));
    }
    if args.repository_label.trim().is_empty() {
        return Err(invalid_input("--repository-label must not be empty"));
    }
    if args.iterations == 0 || args.hot_set == 0 || args.max_tokens == 0 {
        return Err(invalid_input(
            "--iterations, --hot-set, and --max-tokens must be positive",
        ));
    }
    if args.pressure_bytes > 1024 * 1024 * 1024 {
        return Err(invalid_input("--pressure-bytes must not exceed 1 GiB"));
    }
    Ok(())
}

async fn run_profile(args: &Args) -> AnyResult<Report> {
    let snapshot = snapshot_repository(&args.repository)?;
    let config = Config::discover(&snapshot.root, Some(snapshot.database.clone()))?;
    let services = Services::open(config)?;
    let index_start = Instant::now();
    services.index(true).await?;
    let initial_index_ms = index_start.elapsed().as_secs_f64() * 1_000.0;

    let targets = read_targets(&snapshot.root)?;
    if targets.is_empty() {
        return Err(invalid_data("repository contains no readable UTF-8 files"));
    }
    let hot_targets = evenly_spaced_targets(&targets, args.hot_set.min(targets.len()));
    let hot_contents = hot_targets
        .iter()
        .map(|target| fs::read(&target.absolute_path))
        .collect::<Result<Vec<_>, _>>()?;
    let hot_payload_bytes = hot_contents.iter().try_fold(0u64, |total, content| {
        total
            .checked_add(content.len() as u64)
            .ok_or_else(|| io::Error::other("hot payload byte count overflow"))
    })?;

    let post_index_spread_direct = measure_direct_reads(&targets, args.iterations)?;
    prewarm_direct(&hot_targets)?;
    prewarm_service(&services, &hot_targets, args.max_tokens).await?;
    let warm_hot_direct = measure_direct_reads(&hot_targets, args.iterations)?;
    let warm_hot_memory_copy = measure_memory_copies(&hot_contents, args.iterations);
    let warm_hot_service =
        measure_service_reads(&services, &hot_targets, args.iterations, args.max_tokens).await?;
    let warm_spread_service =
        measure_service_reads(&services, &targets, args.iterations, args.max_tokens).await?;

    let pressure_buffer = touched_pressure(args.pressure_bytes);
    let pressure = PressureMeasurement {
        retained_touched_bytes: pressure_buffer.len(),
        direct_hot: measure_direct_reads(&hot_targets, args.iterations)?,
        service_hot: measure_service_reads(
            &services,
            &hot_targets,
            args.iterations,
            args.max_tokens,
        )
        .await?,
    };
    black_box(&pressure_buffer);

    let live_target = targets
        .iter()
        .find(|target| !target.relative_path.contains(".gitignore"))
        .unwrap_or(&targets[0]);
    let live_change = verify_live_change(&services, live_target, args.max_tokens).await?;
    let (leantoken_git_revision, leantoken_worktree_dirty) = leantoken_source_identity();

    Ok(Report {
        schema_version: 1,
        leantoken_version: env!("CARGO_PKG_VERSION"),
        leantoken_git_revision,
        leantoken_worktree_dirty,
        host_os: std::env::consts::OS,
        host_arch: std::env::consts::ARCH,
        release_build: !cfg!(debug_assertions),
        corpus: CorpusReport {
            source_repository: args.repository_label.clone(),
            revision: snapshot.revision,
            files: snapshot.files,
            total_bytes: snapshot.total_bytes,
            text_read_targets: targets.len(),
            iterations: args.iterations,
            hot_set_files: hot_targets.len(),
            max_tokens: args.max_tokens,
            pressure_bytes: args.pressure_bytes,
        },
        initial_index_ms,
        post_index_spread_direct,
        warm_hot_direct,
        warm_hot_memory_copy,
        warm_hot_service,
        warm_spread_service,
        pressure,
        live_change,
        hot_payload_bytes,
        limitations: vec![
            "The checkout copy and initial index touch corpus files before measurement; post_index_spread_direct is a first profile pass, not proof of a cold operating-system page cache.".into(),
            "The pressure condition retains and touches process memory but cannot force or verify cross-platform page-cache eviction.".into(),
            "The direct-read p95 is treated as a deliberately generous upper bound in the adoption decision; a real cache would still pay lookup, synchronization, cloning, eviction, and invalidation costs.".into(),
            "Service timing includes SQLite snapshot lookup, full live-file read and hash, UTF-8 validation, range extraction, token truncation, and response construction. Serialization is measured separately and in the complete request total.".into(),
            "No model or provider call runs in this local profile. Comparing live reads only with the local service-plus-serialization request makes their end-to-end share an upper bound for agent workflows.".into(),
            "GitHub-hosted local filesystems do not represent remote, encrypted, antivirus-heavy, or contended deployments; those environments require an in-situ frozen profile before a scoped cache decision.".into(),
        ],
    })
}

fn snapshot_repository(repository: &Path) -> AnyResult<CorpusSnapshot> {
    let repository = repository.canonicalize()?;
    let revision = git_output(&repository, &["rev-parse", "HEAD"])?;
    if !git_output(
        &repository,
        &["status", "--porcelain", "--untracked-files=normal"],
    )?
    .is_empty()
    {
        return Err(invalid_data("profile repository must be clean"));
    }
    let limits = DiscoveryLimits::default();
    let discovered = discover_files(&repository, limits.max_file_bytes)?;
    let directory = tempfile::tempdir()?;
    let root = directory.path().join("repository");
    fs::create_dir_all(&root)?;
    let mut total_bytes = 0u64;
    for file in &discovered {
        let destination = root.join(&file.relative_path);
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::copy(&file.absolute_path, destination)?;
        total_bytes = total_bytes
            .checked_add(file.size_bytes)
            .ok_or_else(|| io::Error::other("corpus byte count overflow"))?;
    }
    let database = directory.path().join("index.sqlite");
    Ok(CorpusSnapshot {
        _directory: directory,
        root,
        database,
        revision,
        files: discovered.len(),
        total_bytes,
    })
}

fn read_targets(root: &Path) -> AnyResult<Vec<ReadTarget>> {
    let limits = DiscoveryLimits::default();
    let discovered = discover_files(root, limits.max_file_bytes)?;
    let mut targets = Vec::new();
    for DiscoveredFile {
        absolute_path,
        relative_path,
        size_bytes,
        ..
    } in discovered
    {
        let bytes = fs::read(&absolute_path)?;
        if !bytes.is_empty() && std::str::from_utf8(&bytes).is_ok() {
            targets.push(ReadTarget {
                absolute_path,
                relative_path,
                size_bytes,
            });
        }
    }
    targets.sort_by(|left, right| left.relative_path.cmp(&right.relative_path));
    Ok(targets)
}

fn evenly_spaced_targets(targets: &[ReadTarget], count: usize) -> Vec<ReadTarget> {
    (0..count)
        .map(|index| targets[index * targets.len() / count].clone())
        .collect()
}

fn prewarm_direct(targets: &[ReadTarget]) -> AnyResult<()> {
    for target in targets {
        black_box(fs::read(&target.absolute_path)?);
    }
    Ok(())
}

async fn prewarm_service(
    services: &Services,
    targets: &[ReadTarget],
    max_tokens: usize,
) -> AnyResult<()> {
    for target in targets {
        black_box(services.read(read_request(target, max_tokens)).await?);
    }
    Ok(())
}

fn measure_direct_reads(targets: &[ReadTarget], samples: usize) -> AnyResult<ReadMeasurement> {
    let mut durations = Vec::with_capacity(samples);
    let mut total_bytes = 0u64;
    for sample in 0..samples {
        let target = &targets[sample % targets.len()];
        let start = Instant::now();
        let bytes = fs::read(&target.absolute_path)?;
        durations.push(start.elapsed());
        total_bytes = total_bytes
            .checked_add(bytes.len() as u64)
            .ok_or_else(|| io::Error::other("read byte count overflow"))?;
        black_box(bytes);
    }
    Ok(ReadMeasurement::new(
        durations,
        total_bytes,
        samples.min(targets.len()),
    ))
}

fn measure_memory_copies(contents: &[Vec<u8>], samples: usize) -> ReadMeasurement {
    let mut durations = Vec::with_capacity(samples);
    let mut total_bytes = 0u64;
    for sample in 0..samples {
        let start = Instant::now();
        let copy = contents[sample % contents.len()].clone();
        durations.push(start.elapsed());
        total_bytes += copy.len() as u64;
        black_box(copy);
    }
    ReadMeasurement::new(durations, total_bytes, samples.min(contents.len()))
}

async fn measure_service_reads(
    services: &Services,
    targets: &[ReadTarget],
    samples: usize,
    max_tokens: usize,
) -> AnyResult<ServiceMeasurement> {
    let mut request_durations = Vec::with_capacity(samples);
    let mut serialization_durations = Vec::with_capacity(samples);
    let mut complete_durations = Vec::with_capacity(samples);
    let mut total_source_bytes = 0u64;
    let mut total_wire_bytes = 0u64;
    for sample in 0..samples {
        let target = &targets[sample % targets.len()];
        let request_start = Instant::now();
        let response = services.read(read_request(target, max_tokens)).await?;
        let request_duration = request_start.elapsed();
        let serialization_start = Instant::now();
        let wire = serde_json::to_vec(&response)?;
        let serialization_duration = serialization_start.elapsed();
        request_durations.push(request_duration);
        serialization_durations.push(serialization_duration);
        complete_durations.push(request_duration + serialization_duration);
        total_source_bytes = total_source_bytes
            .checked_add(target.size_bytes)
            .ok_or_else(|| io::Error::other("service source byte count overflow"))?;
        total_wire_bytes = total_wire_bytes
            .checked_add(wire.len() as u64)
            .ok_or_else(|| io::Error::other("service wire byte count overflow"))?;
        black_box((response, wire));
    }
    Ok(ServiceMeasurement {
        request: TimingStats::from_durations(request_durations),
        serialization: TimingStats::from_durations(serialization_durations),
        complete: TimingStats::from_durations(complete_durations),
        total_source_bytes,
        total_wire_bytes,
        mean_source_bytes: total_source_bytes as f64 / samples as f64,
        mean_wire_bytes: total_wire_bytes as f64 / samples as f64,
        unique_files: samples.min(targets.len()),
    })
}

fn read_request(target: &ReadTarget, max_tokens: usize) -> ReadRequest {
    ReadRequest {
        path: target.relative_path.clone(),
        start_line: None,
        end_line: None,
        symbol: None,
        max_tokens: Some(max_tokens),
        expected_hash: None,
    }
}

fn touched_pressure(bytes: usize) -> Vec<u8> {
    let mut pressure = vec![0u8; bytes];
    for (page, offset) in (0..bytes).step_by(4096).enumerate() {
        pressure[offset] = (page % 251) as u8;
    }
    pressure
}

async fn verify_live_change(
    services: &Services,
    target: &ReadTarget,
    max_tokens: usize,
) -> AnyResult<LiveChangeCheck> {
    let before = services.read(read_request(target, max_tokens)).await?;
    OpenOptions::new()
        .append(true)
        .open(&target.absolute_path)?
        .write_all(b"\n")?;
    let stale = services.read(read_request(target, max_tokens)).await?;
    if !stale.index_stale || stale.meta.repository_generation != before.meta.repository_generation {
        return Err(invalid_data(
            "live read did not preserve generation while reporting stale content",
        ));
    }
    services
        .index_paths(vec![target.relative_path.clone()])
        .await?;
    let current = services.read(read_request(target, max_tokens)).await?;
    if current.index_stale
        || current.meta.repository_generation <= before.meta.repository_generation
        || current.indexed_hash == stale.indexed_hash
    {
        return Err(invalid_data(
            "reconciliation did not publish a current newer generation",
        ));
    }
    Ok(LiveChangeCheck {
        path: target.relative_path.clone(),
        generation_before: before.meta.repository_generation,
        stale_generation: stale.meta.repository_generation,
        generation_after: current.meta.repository_generation,
        stale_before_reconciliation: stale.index_stale,
        current_after_reconciliation: !current.index_stale,
    })
}

impl ReadMeasurement {
    fn new(durations: Vec<Duration>, total_bytes: u64, unique_files: usize) -> Self {
        let samples = durations.len();
        Self {
            timing: TimingStats::from_durations(durations),
            total_bytes,
            mean_bytes: total_bytes as f64 / samples as f64,
            unique_files,
        }
    }
}

impl TimingStats {
    fn from_durations(durations: Vec<Duration>) -> Self {
        let mut micros = durations
            .iter()
            .map(|duration| duration.as_secs_f64() * 1_000_000.0)
            .collect::<Vec<_>>();
        micros.sort_by(f64::total_cmp);
        let total_us = micros.iter().sum::<f64>();
        Self {
            samples: micros.len(),
            total_ms: total_us / 1_000.0,
            mean_us: total_us / micros.len() as f64,
            min_us: micros[0],
            p50_us: nearest_rank(&micros, 50),
            p95_us: nearest_rank(&micros, 95),
            max_us: micros[micros.len() - 1],
        }
    }
}

fn nearest_rank(values: &[f64], percentile: usize) -> f64 {
    let rank = (percentile * values.len()).div_ceil(100);
    values[rank.saturating_sub(1).min(values.len() - 1)]
}

fn git_output(repository: &Path, args: &[&str]) -> AnyResult<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repository)
        .args(args)
        .output()?;
    if !output.status.success() {
        return Err(invalid_data("unable to resolve clean Git corpus identity"));
    }
    Ok(String::from_utf8(output.stdout)?.trim().to_string())
}

fn leantoken_source_identity() -> (Option<String>, Option<bool>) {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let revision = git_output(root, &["rev-parse", "HEAD"]).ok();
    let dirty = git_output(root, &["status", "--porcelain", "--untracked-files=normal"])
        .ok()
        .map(|status| !status.is_empty());
    (revision, dirty)
}

fn invalid_input(message: &str) -> Box<dyn Error> {
    Box::new(io::Error::new(io::ErrorKind::InvalidInput, message))
}

fn invalid_data(message: &str) -> Box<dyn Error> {
    Box::new(io::Error::new(io::ErrorKind::InvalidData, message))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentile_uses_nearest_rank() {
        let stats = TimingStats::from_durations(
            (1..=20).map(|value| Duration::from_micros(value)).collect(),
        );
        assert_eq!(stats.p50_us, 10.0);
        assert_eq!(stats.p95_us, 19.0);
    }

    #[test]
    fn evenly_spaced_working_set_is_deterministic() {
        let targets = (0..10)
            .map(|index| ReadTarget {
                absolute_path: PathBuf::from(format!("file-{index}")),
                relative_path: format!("file-{index}"),
                size_bytes: index as u64,
            })
            .collect::<Vec<_>>();
        let selected = evenly_spaced_targets(&targets, 4)
            .into_iter()
            .map(|target| target.relative_path)
            .collect::<Vec<_>>();
        assert_eq!(selected, ["file-0", "file-2", "file-5", "file-7"]);
    }

    #[test]
    fn pressure_buffer_touches_each_page() {
        let pressure = touched_pressure(8_193);
        assert_eq!(pressure.len(), 8_193);
        assert_eq!(pressure[0], 0);
        assert_eq!(pressure[4_096], 1);
        assert_eq!(pressure[8_192], 2);
    }

    #[tokio::test]
    async fn small_profile_preserves_live_generation_contract() {
        let repository = tempfile::tempdir().expect("repository");
        fs::create_dir(repository.path().join("src")).expect("source directory");
        for index in 0..4 {
            fs::write(
                repository.path().join(format!("src/file_{index}.rs")),
                format!("pub fn value_{index}() -> usize {{ {index} }}\n"),
            )
            .expect("source file");
        }
        for arguments in [
            vec!["init", "--quiet"],
            vec!["config", "user.name", "LeanToken Test"],
            vec!["config", "user.email", "test@example.invalid"],
            vec!["add", "."],
            vec!["commit", "--quiet", "-m", "fixture"],
        ] {
            let status = Command::new("git")
                .arg("-C")
                .arg(repository.path())
                .args(arguments)
                .status()
                .expect("git command");
            assert!(status.success());
        }
        let report = run_profile(&Args {
            repository: repository.path().to_path_buf(),
            repository_label: "fixture".into(),
            iterations: 4,
            hot_set: 2,
            max_tokens: 128,
            pressure_bytes: 8_193,
            output: PathBuf::from("unused.json"),
        })
        .await
        .expect("profile");
        assert_eq!(report.corpus.files, 4);
        assert_eq!(report.warm_hot_service.complete.samples, 4);
        assert!(report.live_change.stale_before_reconciliation);
        assert!(report.live_change.current_after_reconciliation);
    }
}
