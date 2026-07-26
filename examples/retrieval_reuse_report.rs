use std::collections::BTreeMap;
use std::error::Error;
use std::fs;
use std::path::PathBuf;

use clap::Parser;
use serde::{Deserialize, Serialize};

type AnyResult<T> = Result<T, Box<dyn Error>>;

#[derive(Debug, Parser)]
#[command(about = "Aggregate observed retrieval reuse before considering a cross-request LRU")]
struct Args {
    /// Frozen exact-trace trajectory report.
    #[arg(long, default_value = "benchmarks/reports/model-ab-trajectory-v1.json")]
    input: PathBuf,
}

#[derive(Debug, Deserialize)]
struct Input {
    schema_version: u32,
    source: serde_json::Value,
    controls: serde_json::Value,
    runs: Vec<Run>,
}

#[derive(Debug, Deserialize)]
struct Run {
    arm: String,
    retrieval_calls: usize,
    exact_rereads: usize,
    overlap_rereads: usize,
    known_hash_inputs: usize,
    known_hash_reuses: usize,
    known_hash_resends: usize,
}

#[derive(Debug, Default, Serialize)]
struct ArmReuse {
    runs: usize,
    retrieval_calls: usize,
    exact_rereads: usize,
    exact_reread_rate: f64,
    overlap_rereads: usize,
    overlap_reread_rate: f64,
    known_hash_inputs: usize,
    known_hash_reuses: usize,
    known_hash_resends: usize,
}

#[derive(Debug, Serialize)]
struct Report {
    schema_version: u32,
    source_report: String,
    source_schema_version: u32,
    source: serde_json::Value,
    controls: serde_json::Value,
    arms: BTreeMap<String, ArmReuse>,
    decision: Decision,
    limitations: Vec<&'static str>,
}

#[derive(Debug, Serialize)]
struct Decision {
    cross_request_lru: &'static str,
    rationale: &'static str,
    next_evidence: &'static str,
}

fn main() -> AnyResult<()> {
    let args = Args::parse();
    let input: Input = serde_json::from_slice(&fs::read(&args.input)?)?;
    let report = aggregate(input, args.input.to_string_lossy().into_owned());
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}

fn aggregate(input: Input, source_report: String) -> Report {
    let mut arms = BTreeMap::<String, ArmReuse>::new();
    for run in input.runs {
        let arm = arms.entry(run.arm).or_default();
        arm.runs = arm.runs.saturating_add(1);
        arm.retrieval_calls = arm.retrieval_calls.saturating_add(run.retrieval_calls);
        arm.exact_rereads = arm.exact_rereads.saturating_add(run.exact_rereads);
        arm.overlap_rereads = arm.overlap_rereads.saturating_add(run.overlap_rereads);
        arm.known_hash_inputs = arm.known_hash_inputs.saturating_add(run.known_hash_inputs);
        arm.known_hash_reuses = arm.known_hash_reuses.saturating_add(run.known_hash_reuses);
        arm.known_hash_resends = arm
            .known_hash_resends
            .saturating_add(run.known_hash_resends);
    }
    for arm in arms.values_mut() {
        if arm.retrieval_calls > 0 {
            arm.exact_reread_rate = arm.exact_rereads as f64 / arm.retrieval_calls as f64;
            arm.overlap_reread_rate = arm.overlap_rereads as f64 / arm.retrieval_calls as f64;
        }
    }
    Report {
        schema_version: 1,
        source_report,
        source_schema_version: input.schema_version,
        source: input.source,
        controls: input.controls,
        arms,
        decision: Decision {
            cross_request_lru: "defer",
            rationale: "The progressive LeanToken arm contains little exact range reuse, while overlap reuse does not establish identical generation-scoped primitive requests. A cross-request LRU is not justified by these traces.",
            next_evidence: "Capture privacy-safe normalized primitive request keys and pinned generations in future exact traces, then report exact repeat distance and byte-weighted hit potential before prototyping an LRU.",
        },
        limitations: vec![
            "These are post-hoc diagnostics over frozen public-task exact traces, not a blinded cache experiment.",
            "Exact rereads identify repeated source ranges, not identical context responses or identical storage primitive inputs.",
            "Overlap rereads are useful evidence for request-local batching but are not cache hits.",
            "Known-hash counters are zero in this trace set, so they cannot estimate caller-side reuse.",
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aggregation_separates_exact_and_overlap_reuse() {
        let report = aggregate(
            Input {
                schema_version: 1,
                source: serde_json::json!({}),
                controls: serde_json::json!({}),
                runs: vec![
                    Run {
                        arm: "progressive".into(),
                        retrieval_calls: 10,
                        exact_rereads: 1,
                        overlap_rereads: 4,
                        known_hash_inputs: 2,
                        known_hash_reuses: 1,
                        known_hash_resends: 0,
                    },
                    Run {
                        arm: "progressive".into(),
                        retrieval_calls: 10,
                        exact_rereads: 0,
                        overlap_rereads: 2,
                        known_hash_inputs: 0,
                        known_hash_reuses: 0,
                        known_hash_resends: 0,
                    },
                ],
            },
            "fixture.json".into(),
        );
        let progressive = &report.arms["progressive"];
        assert_eq!(progressive.retrieval_calls, 20);
        assert_eq!(progressive.exact_rereads, 1);
        assert_eq!(progressive.overlap_rereads, 6);
        assert_eq!(progressive.exact_reread_rate, 0.05);
        assert_eq!(progressive.overlap_reread_rate, 0.3);
    }
}
