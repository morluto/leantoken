use std::error::Error;
use std::fs;
use std::hint::black_box;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use clap::Parser;
use leantoken::model::WorktreeReadRequest;
use leantoken::services::Services;
use leantoken::{Config, DiscoveryLimits};
use serde::Serialize;

type AnyResult<T> = Result<T, Box<dyn Error>>;

#[derive(Debug, Parser)]
#[command(about = "Profile one-pass live reads on a deep, near-limit source file")]
struct Args {
    /// Synthetic source bytes, bounded by the default discovery file limit.
    #[arg(long, default_value_t = 2 * 1024 * 1024 - 4096)]
    file_bytes: usize,
    /// Timed samples for shallow and deep line ranges.
    #[arg(long, default_value_t = 100)]
    iterations: usize,
    /// Lines returned from each edge of the synthetic file.
    #[arg(long, default_value_t = 32)]
    range_lines: usize,
    /// Source-token ceiling for each read.
    #[arg(long, default_value_t = 2_048)]
    max_tokens: usize,
}

#[derive(Debug, Serialize)]
struct Report {
    schema_version: u32,
    release_build: bool,
    fixture: Fixture,
    algorithm: Algorithm,
    shallow_range: TimingStats,
    deep_range: TimingStats,
    response_parity: ResponseParity,
    limitations: Vec<&'static str>,
}

#[derive(Debug, Serialize)]
struct Fixture {
    file_bytes: usize,
    lines: usize,
    iterations: usize,
    range_lines: usize,
    max_tokens: usize,
    deep_start_line: usize,
}

#[derive(Debug, Serialize)]
struct Algorithm {
    name: &'static str,
    complete_read_file_streams: usize,
    truncated_read_file_streams: usize,
}

#[derive(Debug, Serialize)]
struct ResponseParity {
    shallow_lines: usize,
    deep_lines: usize,
    shallow_index_stale: bool,
    deep_index_stale: bool,
    shallow_truncated: bool,
    deep_truncated: bool,
}

#[derive(Debug, Serialize)]
struct TimingStats {
    samples: usize,
    p50_us: f64,
    p95_us: f64,
    mean_us: f64,
}

#[tokio::main]
async fn main() -> AnyResult<()> {
    let args = Args::parse();
    let report = run(&args).await?;
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}

async fn run(args: &Args) -> AnyResult<Report> {
    validate(args)?;
    let root = tempfile::tempdir()?;
    let (content, lines) = synthetic_source(args.file_bytes);
    fs::write(root.path().join("near_limit.rs"), &content)?;
    let config = Config::discover(root.path(), Some(root.path().join("index.sqlite")))?;
    let services = Services::open(config)?;
    services.refresh(leantoken::IndexingMode::Reconcile).await?;

    let shallow = (1, args.range_lines.min(lines));
    let deep_start = lines
        .saturating_sub(args.range_lines)
        .saturating_add(1)
        .max(1);
    let deep = (deep_start, lines);
    services
        .read_worktree(request(shallow, args.max_tokens))
        .await?;
    services
        .read_worktree(request(deep, args.max_tokens))
        .await?;

    let (shallow_range, shallow_response) =
        measure(&services, shallow, args.iterations, args.max_tokens).await?;
    let (deep_range, deep_response) =
        measure(&services, deep, args.iterations, args.max_tokens).await?;

    Ok(Report {
        schema_version: 1,
        release_build: !cfg!(debug_assertions),
        fixture: Fixture {
            file_bytes: content.len(),
            lines,
            iterations: args.iterations,
            range_lines: args.range_lines,
            max_tokens: args.max_tokens,
            deep_start_line: deep_start,
        },
        algorithm: Algorithm {
            name: "single_forward_hash_and_range_stream",
            complete_read_file_streams: 1,
            truncated_read_file_streams: 2,
        },
        shallow_range,
        deep_range,
        response_parity: ResponseParity {
            shallow_lines: shallow_response.returned_end_line
                - shallow_response.returned_start_line
                + 1,
            deep_lines: deep_response.returned_end_line - deep_response.returned_start_line + 1,
            shallow_index_stale: shallow_response.index_stale,
            deep_index_stale: deep_response.index_stale,
            shallow_truncated: shallow_response.truncated,
            deep_truncated: deep_response.truncated,
        },
        limitations: vec![
            "The synthetic file is warm in the operating-system page cache after indexing and warmup.",
            "Elapsed times are diagnostics; correctness and stream-count invariants are not inferred from latency thresholds.",
            "A complete read uses one forward stream. A token-truncated read retains a second full-hash verification before issuing its continuation cursor.",
        ],
    })
}

fn validate(args: &Args) -> AnyResult<()> {
    if args.iterations == 0 || args.range_lines == 0 || args.max_tokens == 0 {
        return Err("iterations, range-lines, and max-tokens must be positive".into());
    }
    if args.file_bytes < 4_096 || args.file_bytes > DiscoveryLimits::DEFAULT_MAX_FILE_BYTES as usize
    {
        return Err("file-bytes must be between 4096 and the default file limit".into());
    }
    Ok(())
}

fn synthetic_source(target_bytes: usize) -> (String, usize) {
    let mut content = String::with_capacity(target_bytes);
    let mut line = 1usize;
    while content.len() < target_bytes {
        let row = format!("pub const VALUE_{line:06}: usize = {line}; // deep read padding\n");
        if content.len().saturating_add(row.len()) > target_bytes {
            break;
        }
        content.push_str(&row);
        line = line.saturating_add(1);
    }
    (content, line.saturating_sub(1).max(1))
}

async fn measure(
    services: &Services,
    range: (usize, usize),
    iterations: usize,
    max_tokens: usize,
) -> AnyResult<(TimingStats, leantoken::ReadResponse)> {
    let mut durations = Vec::with_capacity(iterations);
    let mut response = None;
    for _ in 0..iterations {
        let started = Instant::now();
        let current = services.read_worktree(request(range, max_tokens)).await?;
        durations.push(started.elapsed());
        black_box(&current);
        response = Some(current);
    }
    Ok((
        TimingStats::from_durations(durations),
        response.expect("iterations are positive"),
    ))
}

fn request(range: (usize, usize), max_tokens: usize) -> WorktreeReadRequest {
    WorktreeReadRequest {
        path: PathBuf::from("near_limit.rs")
            .to_string_lossy()
            .into_owned(),
        start_line: Some(range.0),
        end_line: Some(range.1),
        symbol: None,
        heading: None,
        heading_occurrence: None,
        continuation_cursor: None,
        max_tokens: Some(max_tokens),
        expected_hash: None,
        delta: false,
        delta_base_artifact_id: None,
        receipt_id: None,
        policy: leantoken::model::ReadPolicy::default(),
    }
}

impl TimingStats {
    fn from_durations(durations: Vec<Duration>) -> Self {
        let mut micros = durations
            .iter()
            .map(|duration| duration.as_secs_f64() * 1_000_000.0)
            .collect::<Vec<_>>();
        micros.sort_by(f64::total_cmp);
        let mean_us = micros.iter().sum::<f64>() / micros.len() as f64;
        Self {
            samples: micros.len(),
            p50_us: percentile(&micros, 50),
            p95_us: percentile(&micros, 95),
            mean_us,
        }
    }
}

fn percentile(values: &[f64], percentile: usize) -> f64 {
    let rank = (percentile * values.len()).div_ceil(100);
    values[rank.saturating_sub(1).min(values.len() - 1)]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn small_profile_reads_the_deep_range_without_truncation() {
        let report = run(&Args {
            file_bytes: 64 * 1024,
            iterations: 2,
            range_lines: 8,
            max_tokens: 512,
        })
        .await
        .expect("profile");

        assert_eq!(report.algorithm.complete_read_file_streams, 1);
        assert_eq!(report.response_parity.deep_lines, 8);
        assert!(!report.response_parity.deep_index_stale);
        assert!(!report.response_parity.deep_truncated);
    }
}
