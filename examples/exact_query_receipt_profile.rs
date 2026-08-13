//! Profile exhaustive lexical scans against exact query-receipt reuse.
//!
//! Indexing and the one-time receipt record are excluded from the timed
//! comparison. Both timed arms still issue a Services call; this profile does
//! not claim that a host or model turn was avoided.

use std::error::Error as StdError;
use std::hint::black_box;
use std::path::PathBuf;
use std::time::Instant;

use clap::Parser;
use leantoken::Config;
use leantoken::model::{QueryReceiptAction, SearchMode, SearchOccurrenceOutput, SearchRequest};
use leantoken::services::Services;
use serde::Serialize;

type AnyResult<T> = Result<T, Box<dyn StdError>>;

#[derive(Debug, Parser)]
#[command(about = "Profile exact exhaustive-query receipt reuse")]
struct Args {
    /// Existing repository to index into a disposable database.
    #[arg(long)]
    repository: PathBuf,
    /// Absent or low-cardinality literal used by exhaustive text search.
    #[arg(
        long,
        default_value = "__leantoken_exact_query_receipt_profile_absent__"
    )]
    query: String,
    /// Timed samples in each arm; executed in mirrored control/reuse order.
    #[arg(long, default_value_t = 12)]
    iterations: usize,
    /// Persistent database path. Omit to use and remove a temporary cache.
    #[arg(long)]
    database: Option<PathBuf>,
    /// Reuse an already indexed caller-provided database.
    #[arg(long, requires = "database")]
    skip_index: bool,
}

#[derive(Debug, Serialize)]
struct Phase {
    samples: usize,
    p50_micros: u128,
    p95_micros: u128,
    mean_micros: u128,
}

#[derive(Debug, Serialize)]
struct Report {
    schema_version: u32,
    repository: PathBuf,
    indexed_generation: u64,
    query_blake3: String,
    occurrences_total: usize,
    iterations_per_arm: usize,
    exhaustive_control: Phase,
    query_receipt_reuse: Phase,
    p50_wall_reduction_percent: f64,
    server_scan_avoided: bool,
    server_result_payload_avoided: bool,
    host_tool_call_avoided: bool,
    limitations: Vec<&'static str>,
}

#[tokio::main]
async fn main() -> AnyResult<()> {
    let args = Args::parse();
    if args.iterations < 4 {
        return Err("iterations must be at least four".into());
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
            .expect("temporary database owner")
            .path()
            .join("index.sqlite")
    });
    let services = Services::open(Config::discover(&repository, Some(database))?)?;
    if !args.skip_index {
        services.index(leantoken::IndexingMode::Reconcile).await?;
    }

    let recorded = services
        .search_occurrences(
            request(&args.query, Some(QueryReceiptAction::Record)),
            SearchOccurrenceOutput::Coordinates,
        )
        .await?;
    let proof = recorded
        .query_receipt
        .as_ref()
        .filter(|proof| proof.complete)
        .ok_or("query did not fit one complete receipt-bearing response")?;
    let receipt_id = proof
        .receipt_id
        .clone()
        .ok_or("complete query proof omitted receipt ID")?;
    let generation = recorded.meta.repository_generation;
    let occurrences_total = recorded.occurrences_total;

    let mut control = Vec::with_capacity(args.iterations);
    let mut reuse = Vec::with_capacity(args.iterations);
    for index in 0..args.iterations.div_ceil(2) {
        for arm in if index % 2 == 0 {
            [false, true, true, false]
        } else {
            [true, false, false, true]
        } {
            if control.len() == args.iterations && reuse.len() == args.iterations {
                break;
            }
            if arm && reuse.len() < args.iterations {
                reuse.push(
                    timed(
                        &services,
                        request(
                            &args.query,
                            Some(QueryReceiptAction::Reuse {
                                receipt_id: receipt_id.clone(),
                            }),
                        ),
                        occurrences_total,
                        true,
                    )
                    .await?,
                );
            } else if !arm && control.len() < args.iterations {
                control.push(
                    timed(
                        &services,
                        request(&args.query, None),
                        occurrences_total,
                        false,
                    )
                    .await?,
                );
            }
        }
    }
    let control = phase(control);
    let reuse = phase(reuse);
    let reduction = if control.p50_micros == 0 {
        0.0
    } else {
        (control.p50_micros.saturating_sub(reuse.p50_micros) as f64 / control.p50_micros as f64)
            * 100.0
    };
    println!(
        "{}",
        serde_json::to_string_pretty(&Report {
            schema_version: 1,
            repository,
            indexed_generation: generation,
            query_blake3: blake3::hash(args.query.as_bytes()).to_hex().to_string(),
            occurrences_total,
            iterations_per_arm: args.iterations,
            exhaustive_control: control,
            query_receipt_reuse: reuse,
            p50_wall_reduction_percent: reduction,
            server_scan_avoided: true,
            server_result_payload_avoided: true,
            host_tool_call_avoided: false,
            limitations: vec![
                "One local repository and one low-cardinality exhaustive-text predicate.",
                "Wall time is mechanism evidence, not a task-success or provider-cost result.",
                "Both arms issue a Services call; host/model turn avoidance is unmeasured.",
            ],
        })?
    );
    Ok(())
}

async fn timed(
    services: &Services,
    request: SearchRequest,
    expected_total: usize,
    expect_reuse: bool,
) -> AnyResult<u128> {
    let started = Instant::now();
    let response = services
        .search_occurrences(request, SearchOccurrenceOutput::Coordinates)
        .await?;
    let elapsed = started.elapsed().as_micros();
    if response.occurrences_total != expected_total {
        return Err("occurrence parity changed between profile arms".into());
    }
    if expect_reuse
        && !response
            .query_receipt
            .as_ref()
            .is_some_and(|proof| proof.complete && response.groups.is_empty())
    {
        return Err("reuse arm did not return a complete payload-suppressed proof".into());
    }
    black_box(response);
    Ok(elapsed)
}

fn request(query: &str, query_receipt: Option<QueryReceiptAction>) -> SearchRequest {
    SearchRequest {
        query: query.into(),
        mode: SearchMode::Text,
        include_paths: Vec::new(),
        exclude_paths: Vec::new(),
        focus_paths: Vec::new(),
        max_results: Some(100),
        max_tokens: Some(32_000),
        context_lines: Some(0),
        case_sensitive: true,
        all_occurrences: true,
        prefer_structural: false,
        receipt_id: None,
        query_receipt,
        cursor: None,
    }
}

fn phase(mut samples: Vec<u128>) -> Phase {
    samples.sort_unstable();
    let total = samples.iter().copied().sum::<u128>();
    Phase {
        samples: samples.len(),
        p50_micros: percentile(&samples, 50),
        p95_micros: percentile(&samples, 95),
        mean_micros: total / samples.len().max(1) as u128,
    }
}

fn percentile(samples: &[u128], percentile: usize) -> u128 {
    let index = samples
        .len()
        .saturating_mul(percentile)
        .div_ceil(100)
        .saturating_sub(1)
        .min(samples.len().saturating_sub(1));
    samples.get(index).copied().unwrap_or_default()
}
