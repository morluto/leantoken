//! Profile cross-restart read-delta reuse against a disposable repository.
//!
//! Run this source with identical arguments at a base and candidate revision.
//! Indexing is excluded from timed phases. Every measured request opens a fresh
//! `Services` instance so process-local read-delta state cannot carry over.

use std::error::Error as StdError;
use std::fs;
use std::hint::black_box;
use std::path::{Path, PathBuf};
use std::time::Instant;

use clap::Parser;
use leantoken::Config;
use leantoken::model::{ReadDeltaOutcome, ReadResponse, ReadStatus, WorktreeReadRequest};
use leantoken::services::Services;
use serde::Serialize;

type AnyResult<T> = Result<T, Box<dyn StdError>>;

#[derive(Debug, Parser)]
#[command(about = "Profile persistent read-delta base overhead and reuse")]
struct Args {
    /// Samples in each restart-reuse phase.
    #[arg(long, default_value_t = 100)]
    iterations: usize,
    /// Generated source lines in the complete read target.
    #[arg(long, default_value_t = 1_200)]
    lines: usize,
    /// Persistent database path. Omit to use and remove a temporary repository.
    #[arg(long)]
    database: Option<PathBuf>,
}

#[derive(Debug, Serialize)]
struct PhaseReport {
    samples: usize,
    p50_micros: u128,
    p95_micros: u128,
    mean_micros: u128,
    full_responses: usize,
    delta_responses: usize,
    not_modified_responses: usize,
    emitted_source_tokens: usize,
    total_response_tokens: usize,
    receipt_avoided_tokens: usize,
    database_bytes_delta: i128,
    wal_bytes_delta: i128,
    process_write_bytes_delta: Option<u64>,
}

#[derive(Debug, Serialize)]
struct Report {
    schema_version: u32,
    iterations: usize,
    lines: usize,
    source_bytes: usize,
    database_bytes_after_index: u64,
    wal_bytes_after_index: u64,
    seed: PhaseReport,
    unchanged_restart_reuse: PhaseReport,
    edited_restart_reuse: PhaseReport,
    final_database_bytes: u64,
    final_wal_bytes: u64,
    peak_rss_kib: Option<u64>,
    limitations: Vec<&'static str>,
}

#[derive(Default)]
struct OutcomeTotals {
    full: usize,
    delta: usize,
    not_modified: usize,
    emitted_source_tokens: usize,
    total_response_tokens: usize,
    receipt_avoided_tokens: usize,
}

#[tokio::main]
async fn main() -> AnyResult<()> {
    let args = Args::parse();
    if args.iterations == 0 || args.lines < 20 {
        return Err("iterations must be positive and lines must be at least 20".into());
    }
    let temporary = tempfile::tempdir()?;
    let root = temporary.path();
    let database = args
        .database
        .clone()
        .unwrap_or_else(|| root.join("index.sqlite"));
    let source_path = root.join("profile.rs");
    let source = generated_source(args.lines);
    fs::write(&source_path, &source)?;
    let services = open_services(root, &database)?;
    services.refresh(leantoken::IndexingMode::Reconcile).await?;
    drop(services);
    let database_bytes_after_index = file_bytes(&database);
    let wal_bytes_after_index = file_bytes(&wal_path(&database));

    let (seed, base_hash) = profile_seed(root, &database).await?;
    let unchanged_restart_reuse = profile_restarts(root, &database, args.iterations, None).await?;
    let changed = source.replacen("compute_value(10)", "compute_updated_value(10)", 1);
    fs::write(&source_path, changed)?;
    let edited_restart_reuse =
        profile_restarts(root, &database, args.iterations, Some(&base_hash)).await?;

    let report = Report {
        schema_version: 1,
        iterations: args.iterations,
        lines: args.lines,
        source_bytes: source.len(),
        database_bytes_after_index,
        wal_bytes_after_index,
        seed,
        unchanged_restart_reuse,
        edited_restart_reuse,
        final_database_bytes: file_bytes(&database),
        final_wal_bytes: file_bytes(&wal_path(&database)),
        peak_rss_kib: linux_status_kib("VmHWM:"),
        limitations: vec![
            "Fresh Services instances model restart isolation inside one benchmark process.",
            "Phase timing includes complete Services::read work, not only delta-base storage.",
            "Linux process write_bytes includes every write by this benchmark process.",
            "SQLite database and WAL deltas are physical artifact sizes, not logical base bytes.",
            "Provider task success is invariant here because candidate responses are checked by status and hash.",
        ],
    };
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}

async fn profile_seed(root: &Path, database: &Path) -> AnyResult<(PhaseReport, String)> {
    let before_database = file_bytes(database);
    let before_wal = file_bytes(&wal_path(database));
    let before_write = linux_process_write_bytes();
    let services = open_services(root, database)?;
    let started = Instant::now();
    let response = services.read_worktree(request(None)).await?;
    let elapsed = started.elapsed().as_micros();
    if response.truncated || response.status != ReadStatus::Content {
        return Err("seed must return complete full content".into());
    }
    let hash = response.content_hash.clone();
    let mut totals = OutcomeTotals::default();
    observe(&response, &mut totals);
    black_box(response);
    Ok((
        phase_report(
            vec![elapsed],
            totals,
            artifact_delta(before_database, file_bytes(database)),
            artifact_delta(before_wal, file_bytes(&wal_path(database))),
            write_delta(before_write, linux_process_write_bytes()),
        ),
        hash,
    ))
}

async fn profile_restarts(
    root: &Path,
    database: &Path,
    iterations: usize,
    expected_hash: Option<&str>,
) -> AnyResult<PhaseReport> {
    let before_database = file_bytes(database);
    let before_wal = file_bytes(&wal_path(database));
    let before_write = linux_process_write_bytes();
    let mut samples = Vec::with_capacity(iterations);
    let mut totals = OutcomeTotals::default();
    for _ in 0..iterations {
        let services = open_services(root, database)?;
        let started = Instant::now();
        let response = services
            .read_worktree(request(expected_hash.map(str::to_owned)))
            .await?;
        samples.push(started.elapsed().as_micros());
        observe(&response, &mut totals);
        black_box(response);
    }
    Ok(phase_report(
        samples,
        totals,
        artifact_delta(before_database, file_bytes(database)),
        artifact_delta(before_wal, file_bytes(&wal_path(database))),
        write_delta(before_write, linux_process_write_bytes()),
    ))
}

fn open_services(root: &Path, database: &Path) -> AnyResult<Services> {
    Ok(Services::open(Config::discover(
        root,
        Some(database.to_owned()),
    )?)?)
}

fn request(expected_hash: Option<String>) -> WorktreeReadRequest {
    WorktreeReadRequest {
        path: "profile.rs".into(),
        start_line: None,
        end_line: None,
        symbol: None,
        heading: None,
        heading_occurrence: None,
        continuation_cursor: None,
        max_tokens: Some(32_000),
        expected_hash,
        delta: true,
        delta_base_artifact_id: None,
        receipt_id: None,
        policy: leantoken::model::ReadPolicy::Full,
    }
}

fn generated_source(lines: usize) -> String {
    (1..=lines)
        .map(|line| format!("let value_{line} = compute_value({line});\n"))
        .collect()
}

fn observe(response: &ReadResponse, totals: &mut OutcomeTotals) {
    match response
        .delta_receipt
        .as_ref()
        .map(|receipt| &receipt.outcome)
    {
        Some(ReadDeltaOutcome::Delta) => totals.delta = totals.delta.saturating_add(1),
        Some(ReadDeltaOutcome::NotModified) => {
            totals.not_modified = totals.not_modified.saturating_add(1);
        }
        _ => totals.full = totals.full.saturating_add(1),
    }
    totals.emitted_source_tokens = totals
        .emitted_source_tokens
        .saturating_add(response.meta.source_tokens);
    totals.total_response_tokens = totals
        .total_response_tokens
        .saturating_add(response.meta.total_response_tokens);
    totals.receipt_avoided_tokens = totals.receipt_avoided_tokens.saturating_add(
        response
            .delta_receipt
            .as_ref()
            .map_or(0, |receipt| receipt.avoided_tokens),
    );
}

fn phase_report(
    mut samples: Vec<u128>,
    totals: OutcomeTotals,
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
        full_responses: totals.full,
        delta_responses: totals.delta,
        not_modified_responses: totals.not_modified,
        emitted_source_tokens: totals.emitted_source_tokens,
        total_response_tokens: totals.total_response_tokens,
        receipt_avoided_tokens: totals.receipt_avoided_tokens,
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
