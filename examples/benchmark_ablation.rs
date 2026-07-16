use std::{error::Error, fs, path::PathBuf};

use clap::Parser;
use serde::{Deserialize, Serialize};

#[derive(Debug, Parser)]
#[command(about = "Compare two LeanToken retrieval reports over one frozen manifest")]
struct Args {
    /// Report produced before the retrieval change.
    #[arg(long)]
    baseline: PathBuf,
    /// Report produced after the retrieval change.
    #[arg(long)]
    candidate: PathBuf,
    /// Optional JSON output path.
    #[arg(long)]
    output: Option<PathBuf>,
}

#[derive(Debug, Deserialize)]
struct BenchmarkReport {
    dataset_kind: String,
    manifest_blake3: String,
    aggregate: Aggregate,
}

#[derive(Debug, Deserialize)]
struct Aggregate {
    task_count: usize,
    relevant_files: usize,
    relevant_files_found: usize,
    line_anchors: usize,
    line_anchors_found: usize,
    leantoken_source_tokens: usize,
    leantoken_total_json_tokens: usize,
    dead_end_fragments: usize,
    dead_end_source_tokens: usize,
    known_fragments_resent: usize,
    estimated_repeated_range_source_tokens: usize,
    two_turn_context_json_tokens: usize,
}

#[derive(Debug, Serialize)]
struct Comparison {
    dataset_kind: String,
    manifest_blake3: String,
    task_count: usize,
    baseline: Metrics,
    candidate: Metrics,
    delta: MetricDelta,
}

#[derive(Debug, Serialize)]
struct Metrics {
    file_recall: f64,
    line_recall: f64,
    source_tokens: usize,
    response_json_tokens: usize,
    dead_end_fragments: usize,
    dead_end_source_tokens: usize,
    exact_hash_resends: usize,
    estimated_repeated_range_source_tokens: usize,
    two_turn_json_tokens: usize,
}

#[derive(Debug, Serialize)]
struct MetricDelta {
    file_recall: f64,
    line_recall: f64,
    source_tokens: i64,
    response_json_tokens: i64,
    dead_end_fragments: i64,
    dead_end_source_tokens: i64,
    exact_hash_resends: i64,
    estimated_repeated_range_source_tokens: i64,
    two_turn_json_tokens: i64,
}

fn main() -> Result<(), Box<dyn Error>> {
    let args = Args::parse();
    let baseline: BenchmarkReport = serde_json::from_str(&fs::read_to_string(&args.baseline)?)?;
    let candidate: BenchmarkReport = serde_json::from_str(&fs::read_to_string(&args.candidate)?)?;
    if baseline.manifest_blake3 != candidate.manifest_blake3 {
        return Err("reports use different manifests; refusing an invalid ablation".into());
    }
    if baseline.dataset_kind != candidate.dataset_kind {
        return Err("reports use different dataset kinds".into());
    }
    if baseline.aggregate.task_count != candidate.aggregate.task_count {
        return Err("reports contain different task counts".into());
    }

    let baseline_metrics = Metrics::from(&baseline.aggregate);
    let candidate_metrics = Metrics::from(&candidate.aggregate);
    let comparison = Comparison {
        dataset_kind: baseline.dataset_kind,
        manifest_blake3: baseline.manifest_blake3,
        task_count: baseline.aggregate.task_count,
        delta: MetricDelta::between(&baseline_metrics, &candidate_metrics),
        baseline: baseline_metrics,
        candidate: candidate_metrics,
    };
    let json = serde_json::to_string_pretty(&comparison)?;
    if let Some(output) = args.output {
        if let Some(parent) = output.parent().filter(|path| !path.as_os_str().is_empty()) {
            fs::create_dir_all(parent)?;
        }
        fs::write(output, &json)?;
    }
    println!("{json}");
    Ok(())
}

impl From<&Aggregate> for Metrics {
    fn from(value: &Aggregate) -> Self {
        Self {
            file_recall: ratio(value.relevant_files_found, value.relevant_files),
            line_recall: ratio(value.line_anchors_found, value.line_anchors),
            source_tokens: value.leantoken_source_tokens,
            response_json_tokens: value.leantoken_total_json_tokens,
            dead_end_fragments: value.dead_end_fragments,
            dead_end_source_tokens: value.dead_end_source_tokens,
            exact_hash_resends: value.known_fragments_resent,
            estimated_repeated_range_source_tokens: value.estimated_repeated_range_source_tokens,
            two_turn_json_tokens: value.two_turn_context_json_tokens,
        }
    }
}

impl MetricDelta {
    fn between(baseline: &Metrics, candidate: &Metrics) -> Self {
        Self {
            file_recall: candidate.file_recall - baseline.file_recall,
            line_recall: candidate.line_recall - baseline.line_recall,
            source_tokens: signed_delta(baseline.source_tokens, candidate.source_tokens),
            response_json_tokens: signed_delta(
                baseline.response_json_tokens,
                candidate.response_json_tokens,
            ),
            dead_end_fragments: signed_delta(
                baseline.dead_end_fragments,
                candidate.dead_end_fragments,
            ),
            dead_end_source_tokens: signed_delta(
                baseline.dead_end_source_tokens,
                candidate.dead_end_source_tokens,
            ),
            exact_hash_resends: signed_delta(
                baseline.exact_hash_resends,
                candidate.exact_hash_resends,
            ),
            estimated_repeated_range_source_tokens: signed_delta(
                baseline.estimated_repeated_range_source_tokens,
                candidate.estimated_repeated_range_source_tokens,
            ),
            two_turn_json_tokens: signed_delta(
                baseline.two_turn_json_tokens,
                candidate.two_turn_json_tokens,
            ),
        }
    }
}

fn ratio(numerator: usize, denominator: usize) -> f64 {
    if denominator == 0 {
        0.0
    } else {
        numerator as f64 / denominator as f64
    }
}

fn signed_delta(baseline: usize, candidate: usize) -> i64 {
    i64::try_from(candidate).unwrap_or(i64::MAX) - i64::try_from(baseline).unwrap_or(i64::MAX)
}
