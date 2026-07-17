//! Plumbing-only adapter for validating the model A/B harness without credentials.

use std::error::Error;
use std::io::{self, Read};

use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize)]
struct AdapterRequest {
    schema_version: u32,
    experiment_id: String,
    manifest_blake3: String,
    random_seed: u64,
    repetition: usize,
    arm_order_index: usize,
    arm: String,
    task_id: String,
}

#[derive(Debug, Serialize)]
struct AdapterResult {
    task_success: bool,
    total_input_tokens: u64,
    total_output_tokens: u64,
    provider_reported_cost_usd: Option<f64>,
    tool_calls: usize,
    rereads: usize,
    reread_tokens: u64,
    failed_searches: usize,
    dead_end_reads: usize,
    provider_usage: ProviderUsage,
    evidence_receipt: Option<serde_json::Value>,
    repository_generation: Option<u64>,
}

#[derive(Debug, Serialize)]
struct ProviderUsage {
    kind: &'static str,
    request_schema_version: u32,
    experiment_id: String,
    manifest_blake3: String,
    random_seed: u64,
    repetition: usize,
    arm_order_index: usize,
    arm: String,
    task_id: String,
}

fn main() -> Result<(), Box<dyn Error>> {
    let mut input = String::new();
    io::stdin().read_to_string(&mut input)?;
    let request: AdapterRequest = serde_json::from_str(&input)?;
    let result = AdapterResult {
        task_success: false,
        total_input_tokens: 0,
        total_output_tokens: 0,
        provider_reported_cost_usd: None,
        tool_calls: 0,
        rereads: 0,
        reread_tokens: 0,
        failed_searches: 0,
        dead_end_reads: 0,
        provider_usage: ProviderUsage {
            kind: "dry_run",
            request_schema_version: request.schema_version,
            experiment_id: request.experiment_id,
            manifest_blake3: request.manifest_blake3,
            random_seed: request.random_seed,
            repetition: request.repetition,
            arm_order_index: request.arm_order_index,
            arm: request.arm,
            task_id: request.task_id,
        },
        evidence_receipt: None,
        repository_generation: None,
    };
    serde_json::to_writer(io::stdout(), &result)?;
    Ok(())
}
