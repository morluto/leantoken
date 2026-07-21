//! Plumbing-only adapter for validating the model A/B harness without credentials.

#[allow(dead_code)]
#[path = "support/model_ab_artifacts.rs"]
mod model_ab_artifacts;

use std::error::Error;
use std::fs;
use std::io::{self, Read};
use std::path::PathBuf;

use model_ab_artifacts::{
    ARTIFACT_SCHEMA_V1, PROVIDER_USAGE_FILE, ProviderUsage, ProviderUsageReceipt, RunBinding,
    TOOL_TRACE_FILE, TRAJECTORY_FILE, ToolTrace, Trajectory,
};
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
    artifacts_directory: PathBuf,
}

#[derive(Debug, Serialize)]
struct AdapterResult {
    schema_version: u32,
    task_success: bool,
    total_input_tokens: Option<u64>,
    total_output_tokens: Option<u64>,
    provider_reported_cost_usd: Option<f64>,
    tool_calls: usize,
    rereads: usize,
    reread_tokens: u64,
    failed_tool_calls: usize,
    failed_searches: usize,
    dead_end_reads: usize,
    provider_usage: ProviderUsage,
    evidence_receipt: Option<serde_json::Value>,
    repository_generation: Option<u64>,
}

fn main() -> Result<(), Box<dyn Error>> {
    let mut input = String::new();
    io::stdin().read_to_string(&mut input)?;
    let request: AdapterRequest = serde_json::from_str(&input)?;
    if request.schema_version != 4 {
        return Err("unsupported model A/B adapter request schema".into());
    }
    let binding = RunBinding {
        experiment_id: request.experiment_id.clone(),
        manifest_blake3: request.manifest_blake3.clone(),
        task_id: request.task_id.clone(),
        repetition: request.repetition,
        arm: request.arm.clone(),
    };
    let provider_usage = ProviderUsage {
        uncached_input_tokens: Some(0),
        cache_creation_input_tokens: Some(0),
        cache_read_input_tokens: Some(0),
        output_tokens: Some(0),
        reasoning_tokens: Some(0),
    };
    write_json(
        request.artifacts_directory.join(TOOL_TRACE_FILE),
        &ToolTrace {
            schema_version: ARTIFACT_SCHEMA_V1,
            binding: binding.clone(),
            calls: Vec::new(),
        },
    )?;
    write_json(
        request.artifacts_directory.join(TRAJECTORY_FILE),
        &Trajectory {
            schema_version: ARTIFACT_SCHEMA_V1,
            binding: binding.clone(),
            events: Vec::new(),
        },
    )?;
    write_json(
        request.artifacts_directory.join(PROVIDER_USAGE_FILE),
        &ProviderUsageReceipt {
            schema_version: ARTIFACT_SCHEMA_V1,
            binding,
            usage: provider_usage.clone(),
            raw_receipt: serde_json::json!({
                "kind": "dry_run",
                "request_schema_version": request.schema_version,
                "random_seed": request.random_seed,
                "arm_order_index": request.arm_order_index
            }),
        },
    )?;
    let result = AdapterResult {
        schema_version: 4,
        task_success: false,
        total_input_tokens: Some(0),
        total_output_tokens: Some(0),
        provider_reported_cost_usd: None,
        tool_calls: 0,
        rereads: 0,
        reread_tokens: 0,
        failed_tool_calls: 0,
        failed_searches: 0,
        dead_end_reads: 0,
        provider_usage,
        evidence_receipt: None,
        repository_generation: None,
    };
    serde_json::to_writer(io::stdout(), &result)?;
    Ok(())
}

fn write_json(path: PathBuf, value: &impl Serialize) -> Result<(), Box<dyn Error>> {
    fs::write(path, serde_json::to_vec_pretty(value)?)?;
    Ok(())
}
