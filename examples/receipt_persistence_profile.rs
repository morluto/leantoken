//! Profile receipt creation and reuse against an existing repository.
//!
//! Run the same source and arguments at a base and candidate revision. Index
//! work is excluded from the timed phases and storage deltas begin after the
//! index is ready.

use std::error::Error as StdError;
use std::fs;
use std::hint::black_box;
use std::path::{Path, PathBuf};
use std::time::Instant;

use clap::Parser;
use leantoken::Config;
use leantoken::model::{SearchMode, SearchRequest};
use leantoken::services::Services;
use serde::Serialize;

type AnyResult<T> = Result<T, Box<dyn StdError>>;

#[derive(Debug, Parser)]
#[command(about = "Profile persistent retrieval receipt overhead")]
struct Args {
    /// Existing repository to index into a disposable database.
    #[arg(long)]
    repository: PathBuf,
    /// Search query shared by every sample.
    #[arg(long, default_value = "Services")]
    query: String,
    /// Samples in each create and reuse phase.
    #[arg(long, default_value_t = 100)]
    iterations: usize,
    /// Persistent database path. Omit to use and remove a temporary cache.
    #[arg(long)]
    database: Option<PathBuf>,
    /// Reuse an already indexed caller-provided database.
    #[arg(long, requires = "database")]
    skip_index: bool,
}

#[derive(Debug, Serialize)]
struct PhaseReport {
    samples: usize,
    p50_micros: u128,
    p95_micros: u128,
    mean_micros: u128,
    returned_hits: usize,
    suppressed_exact: usize,
    suppressed_overlap: usize,
    database_bytes_delta: i128,
    wal_bytes_delta: i128,
    process_write_bytes_delta: Option<u64>,
}

#[derive(Debug, Serialize)]
struct Report {
    schema_version: u32,
    repository: PathBuf,
    query: String,
    iterations: usize,
    database_bytes_after_index: u64,
    wal_bytes_after_index: u64,
    create: PhaseReport,
    reuse: PhaseReport,
    peak_rss_kib: Option<u64>,
    limitations: Vec<&'static str>,
}

#[tokio::main]
async fn main() -> AnyResult<()> {
    let args = Args::parse();
    if args.iterations == 0 {
        return Err("iterations must be positive".into());
    }
    let repository = args.repository.canonicalize()?;
    let temporary = args
        .database
        .is_none()
        .then(tempfile::tempdir)
        .transpose()?;
    let database = args.database.clone().unwrap_or_else(|| {
        temporary
            .as_ref()
            .expect("temporary database")
            .path()
            .join("index.sqlite")
    });
    let config = Config::discover(&repository, Some(database.clone()))?;
    let services = Services::open(config)?;
    if !args.skip_index {
        services.refresh(leantoken::IndexingMode::Reconcile).await?;
    }
    let database_bytes_after_index = file_bytes(&database);
    let wal_bytes_after_index = file_bytes(&wal_path(&database));

    let create = profile_create(&services, &database, &args.query, args.iterations).await?;
    let reuse = profile_reuse(&services, &database, &args.query, args.iterations).await?;
    let report = Report {
        schema_version: 1,
        repository,
        query: args.query,
        iterations: args.iterations,
        database_bytes_after_index,
        wal_bytes_after_index,
        create,
        reuse,
        peak_rss_kib: linux_status_kib("VmHWM:"),
        limitations: vec![
            "Phase timing includes complete Services::search work, not only receipt storage.",
            "Linux process write_bytes includes every write by this process during the phase.",
            "SQLite database and WAL deltas are physical artifact sizes, not logical receipt bytes.",
            "Task success and provider-visible token usage are outside this local profile.",
        ],
    };
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}

async fn profile_create(
    services: &Services,
    database: &Path,
    query: &str,
    iterations: usize,
) -> AnyResult<PhaseReport> {
    let before_database = file_bytes(database);
    let before_wal = file_bytes(&wal_path(database));
    let before_write = linux_process_write_bytes();
    let mut samples = Vec::with_capacity(iterations);
    let mut returned_hits = 0usize;
    for _ in 0..iterations {
        let started = Instant::now();
        let response = services.search(search_request(query, None)).await?;
        samples.push(started.elapsed().as_micros());
        returned_hits = returned_hits.saturating_add(response.hits.len());
        black_box(response);
    }
    Ok(phase_report(
        samples,
        returned_hits,
        0,
        0,
        artifact_delta(before_database, file_bytes(database)),
        artifact_delta(before_wal, file_bytes(&wal_path(database))),
        write_delta(before_write, linux_process_write_bytes()),
    ))
}

async fn profile_reuse(
    services: &Services,
    database: &Path,
    query: &str,
    iterations: usize,
) -> AnyResult<PhaseReport> {
    let seed = services.search(search_request(query, None)).await?;
    if seed.hits.is_empty() {
        return Err("profile query returned no seed hits".into());
    }
    let receipt_id = seed
        .meta
        .receipt_id
        .ok_or("seed search omitted receipt id")?;
    let before_database = file_bytes(database);
    let before_wal = file_bytes(&wal_path(database));
    let before_write = linux_process_write_bytes();
    let mut samples = Vec::with_capacity(iterations);
    let mut returned_hits = 0usize;
    let mut suppressed_exact = 0usize;
    let mut suppressed_overlap = 0usize;
    for _ in 0..iterations {
        let started = Instant::now();
        let response = services
            .search(search_request(query, Some(receipt_id.clone())))
            .await?;
        samples.push(started.elapsed().as_micros());
        returned_hits = returned_hits.saturating_add(response.hits.len());
        suppressed_exact = suppressed_exact.saturating_add(response.meta.receipt_suppressed_exact);
        suppressed_overlap =
            suppressed_overlap.saturating_add(response.meta.receipt_suppressed_overlap);
        black_box(response);
    }
    Ok(phase_report(
        samples,
        returned_hits,
        suppressed_exact,
        suppressed_overlap,
        artifact_delta(before_database, file_bytes(database)),
        artifact_delta(before_wal, file_bytes(&wal_path(database))),
        write_delta(before_write, linux_process_write_bytes()),
    ))
}

fn search_request(query: &str, receipt_id: Option<String>) -> SearchRequest {
    SearchRequest {
        query: query.to_owned(),
        mode: SearchMode::Identifier,
        include_paths: Vec::new(),
        exclude_paths: Vec::new(),
        focus_paths: Vec::new(),
        max_results: Some(20),
        max_tokens: Some(2_000),
        context_lines: Some(2),
        case_sensitive: false,
        all_occurrences: false,
        prefer_structural: true,
        receipt_id,
        query_receipt: None,
        cursor: None,
    }
}

fn phase_report(
    mut samples: Vec<u128>,
    returned_hits: usize,
    suppressed_exact: usize,
    suppressed_overlap: usize,
    database_bytes_delta: i128,
    wal_bytes_delta: i128,
    process_write_bytes_delta: Option<u64>,
) -> PhaseReport {
    samples.sort_unstable();
    let total = samples.iter().copied().sum::<u128>();
    PhaseReport {
        samples: samples.len(),
        p50_micros: percentile(&samples, 50),
        p95_micros: percentile(&samples, 95),
        mean_micros: total / samples.len().max(1) as u128,
        returned_hits,
        suppressed_exact,
        suppressed_overlap,
        database_bytes_delta,
        wal_bytes_delta,
        process_write_bytes_delta,
    }
}

fn percentile(sorted: &[u128], percentile: usize) -> u128 {
    let index = sorted
        .len()
        .saturating_sub(1)
        .saturating_mul(percentile)
        .saturating_add(99)
        / 100;
    sorted.get(index).copied().unwrap_or_default()
}

fn wal_path(database: &Path) -> PathBuf {
    let mut path = database.as_os_str().to_os_string();
    path.push("-wal");
    PathBuf::from(path)
}

fn file_bytes(path: &Path) -> u64 {
    fs::metadata(path).map_or(0, |metadata| metadata.len())
}

fn artifact_delta(before: u64, after: u64) -> i128 {
    i128::from(after) - i128::from(before)
}

fn write_delta(before: Option<u64>, after: Option<u64>) -> Option<u64> {
    Some(after?.saturating_sub(before?))
}

fn linux_process_write_bytes() -> Option<u64> {
    fs::read_to_string("/proc/self/io")
        .ok()?
        .lines()
        .find_map(|line| {
            line.strip_prefix("write_bytes:")
                .and_then(|value| value.trim().parse().ok())
        })
}

fn linux_status_kib(field: &str) -> Option<u64> {
    fs::read_to_string("/proc/self/status")
        .ok()?
        .lines()
        .find_map(|line| {
            line.strip_prefix(field)
                .and_then(|value| value.split_whitespace().next())
                .and_then(|value| value.parse().ok())
        })
}
