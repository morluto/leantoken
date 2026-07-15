use std::error::Error;
use std::fs::{self, OpenOptions};
use std::hint::black_box;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use clap::Parser;
use leantoken::Config;
use leantoken::indexer::Indexer;
use leantoken::model::IndexResponse;
use leantoken::storage::Storage;
use serde::Serialize;

type AnyResult<T> = Result<T, Box<dyn Error>>;

#[derive(Debug, Parser)]
#[command(about = "Profile full and changed-path indexing plus warm file reads")]
struct Args {
    /// Number of synthetic Rust source files.
    #[arg(long, default_value_t = 2_000)]
    files: usize,
    /// Approximate bytes in each synthetic source file.
    #[arg(long, default_value_t = 8 * 1024)]
    file_bytes: usize,
    /// Samples for each indexing measurement.
    #[arg(long, default_value_t = 10)]
    iterations: usize,
    /// Samples for each file-read measurement.
    #[arg(long, default_value_t = 2_000)]
    read_samples: usize,
    /// Number of files in the repeated-read working set.
    #[arg(long, default_value_t = 8)]
    hot_set: usize,
    /// JSON report destination.
    #[arg(long, default_value = "target/indexing_profile_report.json")]
    output: PathBuf,
}

#[derive(Debug, Serialize)]
struct Report {
    schema_version: u32,
    host_os: &'static str,
    host_arch: &'static str,
    release_build: bool,
    corpus: CorpusReport,
    initial_index: IndexSample,
    full_noop: IndexMeasurement,
    full_changed: IndexMeasurement,
    targeted_changed: IndexMeasurement,
    warm_hot_file_reads: ReadMeasurement,
    warm_spread_file_reads: ReadMeasurement,
    memory_hot_file_copies: ReadMeasurement,
    limitations: Vec<&'static str>,
}

#[derive(Debug, Serialize)]
struct CorpusReport {
    files: usize,
    approximate_file_bytes: usize,
    indexing_iterations: usize,
    read_samples: usize,
    hot_set_files: usize,
}

#[derive(Debug, Serialize)]
struct IndexSample {
    elapsed_ms: f64,
    response: IndexResponse,
}

#[derive(Debug, Serialize)]
struct IndexMeasurement {
    timing: TimingStats,
    files_seen_per_sample: usize,
    files_indexed_per_sample: usize,
}

#[derive(Debug, Serialize)]
struct ReadMeasurement {
    timing: TimingStats,
    total_bytes: u64,
    mean_bytes: f64,
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

fn main() -> AnyResult<()> {
    let args = Args::parse();
    validate_args(&args)?;
    let report = run_profile(&args)?;
    let json = serde_json::to_string_pretty(&report)?;
    if let Some(parent) = args.output.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&args.output, format!("{json}\n"))?;
    println!("{json}");
    Ok(())
}

fn validate_args(args: &Args) -> AnyResult<()> {
    if args.files < 2 {
        return Err(invalid_input("--files must be at least 2"));
    }
    if args.file_bytes < 128 {
        return Err(invalid_input("--file-bytes must be at least 128"));
    }
    if args.iterations == 0 {
        return Err(invalid_input("--iterations must be positive"));
    }
    if args.read_samples == 0 {
        return Err(invalid_input("--read-samples must be positive"));
    }
    if args.hot_set == 0 {
        return Err(invalid_input("--hot-set must be positive"));
    }
    Ok(())
}

fn invalid_input(message: &str) -> Box<dyn Error> {
    Box::new(io::Error::new(io::ErrorKind::InvalidInput, message))
}

fn run_profile(args: &Args) -> AnyResult<Report> {
    let root = tempfile::tempdir()?;
    let paths = create_corpus(root.path(), args.files, args.file_bytes)?;
    let config = Arc::new(Config::discover(
        root.path(),
        Some(root.path().join("index.sqlite")),
    )?);
    let storage = Storage::open(&config.database_path)?;
    let indexer = Indexer::new(config, storage);

    let start = Instant::now();
    let initial_response = indexer.reconcile(false)?;
    let initial_index = IndexSample {
        elapsed_ms: milliseconds(start.elapsed()),
        response: initial_response,
    };

    let mut full_noop_durations = Vec::with_capacity(args.iterations);
    for _ in 0..args.iterations {
        let start = Instant::now();
        let response = indexer.reconcile(false)?;
        full_noop_durations.push(start.elapsed());
        require_index_counts(&response, args.files, 0, "full no-op")?;
    }

    let full_changed = measure_changed_indexing(
        args.iterations,
        &paths[0],
        || indexer.reconcile(false),
        args.files,
        "full changed",
    )?;
    let targeted_changed = measure_changed_indexing(
        args.iterations,
        &paths[1],
        || indexer.reconcile_paths(&["src/file_00001.rs".to_owned()]),
        1,
        "targeted changed",
    )?;

    let hot_set = args.hot_set.min(paths.len());
    let hot_paths = &paths[..hot_set];
    let hot_contents = hot_paths
        .iter()
        .map(fs::read)
        .collect::<Result<Vec<_>, _>>()?;
    for path in hot_paths {
        black_box(fs::read(path)?);
    }

    let warm_hot_file_reads = measure_file_reads(hot_paths, args.read_samples)?;
    let warm_spread_file_reads = measure_file_reads(&paths, args.read_samples)?;
    let memory_hot_file_copies = measure_memory_copies(&hot_contents, args.read_samples);

    Ok(Report {
        schema_version: 1,
        host_os: std::env::consts::OS,
        host_arch: std::env::consts::ARCH,
        release_build: !cfg!(debug_assertions),
        corpus: CorpusReport {
            files: args.files,
            approximate_file_bytes: args.file_bytes,
            indexing_iterations: args.iterations,
            read_samples: args.read_samples,
            hot_set_files: hot_set,
        },
        initial_index,
        full_noop: IndexMeasurement {
            timing: TimingStats::from_durations(full_noop_durations),
            files_seen_per_sample: args.files,
            files_indexed_per_sample: 0,
        },
        full_changed,
        targeted_changed,
        warm_hot_file_reads,
        warm_spread_file_reads,
        memory_hot_file_copies,
        limitations: vec![
            "The generated Rust corpus controls file count and size but does not model a real repository's language mix or directory shape.",
            "File-read measurements use the operating system's warm page cache; they do not represent cold, remote, encrypted, or heavily contended filesystems.",
            "The in-memory comparison copies bytes but excludes cache lookup, eviction, synchronization, invalidation, and memory-pressure costs.",
            "Timing is machine-specific. Compare runs only on the same host and build profile, and use release builds for decisions.",
        ],
    })
}

fn measure_changed_indexing<F>(
    iterations: usize,
    path: &Path,
    mut reconcile: F,
    files_seen: usize,
    measurement: &str,
) -> AnyResult<IndexMeasurement>
where
    F: FnMut() -> leantoken::Result<IndexResponse>,
{
    let mut durations = Vec::with_capacity(iterations);
    for iteration in 0..iterations {
        let mut file = OpenOptions::new().append(true).open(path)?;
        writeln!(file, "// profile mutation {iteration}")?;
        drop(file);

        let start = Instant::now();
        let response = reconcile()?;
        durations.push(start.elapsed());
        require_index_counts(&response, files_seen, 1, measurement)?;
    }
    Ok(IndexMeasurement {
        timing: TimingStats::from_durations(durations),
        files_seen_per_sample: files_seen,
        files_indexed_per_sample: 1,
    })
}

fn require_index_counts(
    response: &IndexResponse,
    files_seen: usize,
    files_indexed: usize,
    measurement: &str,
) -> AnyResult<()> {
    if response.files_seen != files_seen || response.files_indexed != files_indexed {
        return Err(Box::new(io::Error::other(format!(
            "{measurement} expected files_seen={files_seen}, files_indexed={files_indexed}; got files_seen={}, files_indexed={}",
            response.files_seen, response.files_indexed
        ))));
    }
    Ok(())
}

fn measure_file_reads(paths: &[PathBuf], samples: usize) -> AnyResult<ReadMeasurement> {
    let mut durations = Vec::with_capacity(samples);
    let mut total_bytes = 0u64;
    for sample in 0..samples {
        let start = Instant::now();
        let contents = fs::read(&paths[sample % paths.len()])?;
        durations.push(start.elapsed());
        total_bytes += contents.len() as u64;
        black_box(contents);
    }
    Ok(ReadMeasurement {
        timing: TimingStats::from_durations(durations),
        total_bytes,
        mean_bytes: total_bytes as f64 / samples as f64,
    })
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
    ReadMeasurement {
        timing: TimingStats::from_durations(durations),
        total_bytes,
        mean_bytes: total_bytes as f64 / samples as f64,
    }
}

fn create_corpus(root: &Path, files: usize, file_bytes: usize) -> AnyResult<Vec<PathBuf>> {
    let source = root.join("src");
    fs::create_dir_all(&source)?;
    let mut paths = Vec::with_capacity(files);
    for index in 0..files {
        let path = source.join(format!("file_{index:05}.rs"));
        fs::write(&path, synthetic_source(index, file_bytes))?;
        paths.push(path);
    }
    Ok(paths)
}

fn synthetic_source(index: usize, file_bytes: usize) -> String {
    let mut source = format!(
        "pub fn symbol_{index:05}() -> usize {{\n    {index}\n}}\n\n// deterministic profile padding: "
    );
    if source.len() < file_bytes {
        source.extend(std::iter::repeat_n('x', file_bytes - source.len()));
    }
    source.push('\n');
    source
}

impl TimingStats {
    fn from_durations(durations: Vec<Duration>) -> Self {
        assert!(!durations.is_empty());
        let mut micros = durations
            .into_iter()
            .map(|duration| duration.as_secs_f64() * 1_000_000.0)
            .collect::<Vec<_>>();
        micros.sort_by(f64::total_cmp);
        let total_us = micros.iter().sum::<f64>();
        Self {
            samples: micros.len(),
            total_ms: total_us / 1_000.0,
            mean_us: total_us / micros.len() as f64,
            min_us: micros[0],
            p50_us: percentile(&micros, 0.50),
            p95_us: percentile(&micros, 0.95),
            max_us: micros[micros.len() - 1],
        }
    }
}

fn percentile(sorted: &[f64], percentile: f64) -> f64 {
    let rank = (percentile * sorted.len() as f64).ceil() as usize;
    sorted[rank.saturating_sub(1).min(sorted.len() - 1)]
}

fn milliseconds(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1_000.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentile_uses_nearest_rank() {
        let samples = [1.0, 2.0, 3.0, 4.0, 5.0];
        assert_eq!(percentile(&samples, 0.50), 3.0);
        assert_eq!(percentile(&samples, 0.95), 5.0);
    }

    #[test]
    fn small_profile_exercises_each_measurement() {
        let output = tempfile::tempdir().expect("output");
        let args = Args {
            files: 6,
            file_bytes: 256,
            iterations: 2,
            read_samples: 12,
            hot_set: 2,
            output: output.path().join("report.json"),
        };

        let report = run_profile(&args).expect("profile");

        assert_eq!(report.initial_index.response.files_indexed, 6);
        assert_eq!(report.full_noop.timing.samples, 2);
        assert_eq!(report.full_changed.files_indexed_per_sample, 1);
        assert_eq!(report.targeted_changed.files_seen_per_sample, 1);
        assert_eq!(report.warm_hot_file_reads.timing.samples, 12);
        assert_eq!(report.memory_hot_file_copies.timing.samples, 12);
    }
}
