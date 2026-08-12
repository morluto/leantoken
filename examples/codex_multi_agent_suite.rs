//! Aggregate a randomized multi-agent receipt suite without reading private rollouts.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::error::Error;
use std::fs;
use std::path::{Component, Path, PathBuf};

use clap::Parser;
use serde::{Deserialize, Serialize};

type DynError = Box<dyn Error>;

#[derive(Debug, Parser)]
#[command(about = "Aggregate redacted Codex multi-agent suite receipts")]
struct Args {
    /// Frozen suite manifest.
    #[arg(long)]
    manifest: PathBuf,
    /// Suite output root containing runs/*/run.json and redacted receipts.
    #[arg(long)]
    runs_root: PathBuf,
    /// Publishable aggregate report.
    #[arg(long)]
    output: PathBuf,
}

#[derive(Debug, Deserialize)]
struct SuiteManifest {
    schema_version: u32,
    experiment_id: String,
    controls: Controls,
    arms: Vec<Arm>,
    tasks: Vec<Task>,
    analysis: Analysis,
    quality_gates: QualityGateConfig,
}

#[derive(Debug, Deserialize)]
struct Controls {
    repetitions: usize,
}

#[derive(Debug, Deserialize)]
struct Arm {
    name: String,
    repository_reads: String,
    maximum_mcp_calls: Option<usize>,
}

#[derive(Debug, Deserialize)]
struct Analysis {
    full_baseline_arm: String,
    retrieval_baseline_arm: String,
    candidate_arm: String,
}

#[derive(Debug, Deserialize)]
struct Task {
    id: String,
    language: String,
    repository_revision: String,
}

#[derive(Debug, Deserialize)]
struct QualityGateConfig {
    minimum_candidate_success_rate: f64,
    minimum_candidate_successes_per_task: Option<usize>,
    maximum_per_task_success_regression: Option<usize>,
    minimum_combined_total_input_savings_fraction: f64,
    minimum_combined_savings_ci_lower_bound: f64,
    minimum_retrieval_total_input_savings_fraction: f64,
    minimum_retrieval_savings_ci_lower_bound: f64,
    maximum_success_rate_regression: f64,
}

#[derive(Debug, Deserialize)]
struct RunMetadata {
    schema_version: u32,
    schedule_index: usize,
    schedule_key: String,
    repetition: usize,
    task_id: String,
    corpus: String,
    arm: String,
    receipt: String,
}

#[derive(Debug, Deserialize)]
struct Receipt {
    schema_version: u32,
    experiment_id: String,
    arm: String,
    source_family_blake3: String,
    receipt_binary_blake3: String,
    topology: Topology,
    provider_usage: Usage,
    task_evaluation: TaskEvaluation,
    thread_receipts: Vec<ThreadReceipt>,
}

#[derive(Debug, Deserialize)]
struct Topology {
    thread_count: usize,
    child_thread_count: usize,
    maximum_depth: usize,
    spawn_calls: usize,
    matched_spawn_calls: usize,
}

#[derive(Debug, Clone, Copy, Default, Deserialize)]
struct Usage {
    total_input_tokens: u64,
    uncached_input_tokens: u64,
    cache_read_input_tokens: u64,
    output_tokens: u64,
}

#[derive(Debug, Deserialize)]
struct TaskEvaluation {
    task_id: String,
    repository_revision: String,
    evaluation_mode: String,
    expected_evidence_count: usize,
    matched_path_count: usize,
    unexpected_evidence_count: usize,
    task_success: bool,
}

#[derive(Debug, Deserialize)]
struct ThreadReceipt {
    role: String,
    host_version: String,
    model: String,
    reasoning_effort: Option<String>,
    provider_request_count: usize,
    provider_usage: Usage,
    tool_calls: ToolCalls,
}

#[derive(Debug, Deserialize)]
struct ToolCalls {
    mcp: usize,
    failed_mcp: usize,
    mcp_result_json_bytes: u64,
    mcp_emitted_source_tokens: u64,
    shell: usize,
}

#[derive(Debug)]
struct Run {
    schedule_index: usize,
    repetition: usize,
    task_id: String,
    arm: String,
    source_family_blake3: String,
    success: bool,
    usage: Usage,
    child_usage: Usage,
    child_requests: usize,
    child_tools: ToolCalls,
}

#[derive(Debug, Serialize)]
struct Report {
    schema_version: u32,
    experiment_id: String,
    suite_manifest_blake3: String,
    aggregate_binary_blake3: String,
    receipt_set_blake3: String,
    run_count: usize,
    task_count: usize,
    repetitions: usize,
    arms: Vec<String>,
    languages: Vec<String>,
    consistency: Consistency,
    arm_summaries: Vec<ArmSummary>,
    task_arm_summaries: Vec<TaskArmSummary>,
    comparisons: Vec<Comparison>,
    quality_gate: QualityGateReport,
    run_samples: Vec<RunSample>,
    limitations: Vec<&'static str>,
}

#[derive(Debug, Serialize)]
struct RunSample {
    schedule_index: usize,
    repetition: usize,
    task_id: String,
    arm: String,
    source_family_blake3: String,
    success: bool,
    family_usage: UsageSample,
    child_usage: UsageSample,
    child_provider_requests: usize,
    child_mcp_calls: usize,
    child_failed_mcp_calls: usize,
    child_mcp_result_bytes: u64,
    child_mcp_source_tokens: u64,
    child_shell_calls: usize,
}

#[derive(Debug, Clone, Copy, Serialize)]
struct UsageSample {
    total_input_tokens: u64,
    uncached_input_tokens: u64,
    cache_read_input_tokens: u64,
    output_tokens: u64,
}

impl From<Usage> for UsageSample {
    fn from(usage: Usage) -> Self {
        Self {
            total_input_tokens: usage.total_input_tokens,
            uncached_input_tokens: usage.uncached_input_tokens,
            cache_read_input_tokens: usage.cache_read_input_tokens,
            output_tokens: usage.output_tokens,
        }
    }
}

#[derive(Debug, Serialize)]
struct Consistency {
    complete_schedule: bool,
    host_versions: Vec<String>,
    models: Vec<String>,
    reasoning_efforts: Vec<String>,
    receipt_binary_blake3: Vec<String>,
    contract_violation_count: usize,
}

#[derive(Debug, Serialize)]
struct ArmSummary {
    arm: String,
    runs: usize,
    successes: usize,
    success_rate: f64,
    success_rate_wilson_95: Interval,
    family_total_input: NumericSummary,
    family_uncached_input: NumericSummary,
    family_cache_read_input: NumericSummary,
    family_output: NumericSummary,
    child_total_input: NumericSummary,
    child_provider_requests: NumericSummary,
    child_mcp_result_bytes: NumericSummary,
    child_mcp_source_tokens: NumericSummary,
    contract_violations: usize,
}

#[derive(Debug, Serialize)]
struct TaskArmSummary {
    task_id: String,
    language: String,
    arm: String,
    runs: usize,
    successes: usize,
    success_rate: f64,
    family_total_input: NumericSummary,
}

#[derive(Debug, Clone, Copy, Serialize)]
struct Interval {
    lower: f64,
    upper: f64,
}

#[derive(Debug, Serialize)]
struct NumericSummary {
    count: usize,
    sum: u64,
    mean: f64,
    median: f64,
    minimum: u64,
    maximum: u64,
    sample_standard_deviation: f64,
    coefficient_of_variation: f64,
}

#[derive(Debug, Serialize)]
struct Comparison {
    baseline: String,
    candidate: String,
    paired_runs: usize,
    both_successful_runs: usize,
    candidate_wins: usize,
    equal_token_runs: usize,
    weighted_total_input_savings_fraction: f64,
    paired_savings_median: f64,
    paired_savings_mean: f64,
    stratified_bootstrap_weighted_savings_95: Interval,
    weighted_uncached_input_savings_fraction: f64,
    weighted_child_input_savings_fraction: f64,
    baseline_success_rate: f64,
    candidate_success_rate: f64,
    success_rate_difference: f64,
    per_task: Vec<TaskComparison>,
}

#[derive(Debug, Serialize)]
struct TaskComparison {
    task_id: String,
    paired_runs: usize,
    weighted_total_input_savings_fraction: f64,
    median_paired_savings_fraction: f64,
}

#[derive(Debug, Serialize)]
struct QualityGateReport {
    passed: bool,
    checks: BTreeMap<String, GateCheck>,
}

#[derive(Debug, Serialize)]
struct GateCheck {
    passed: bool,
    observed: f64,
    required: String,
}

fn main() -> Result<(), DynError> {
    let args = Args::parse();
    let manifest_bytes = fs::read(&args.manifest)?;
    let manifest: SuiteManifest = serde_json::from_slice(&manifest_bytes)?;
    validate_manifest(&manifest)?;
    let (runs, consistency) = load_runs(&manifest, &args.runs_root)?;
    let report = build_report(
        &manifest,
        &runs,
        consistency,
        blake3::hash(&manifest_bytes).to_hex().to_string(),
        blake3::hash(&fs::read(std::env::current_exe()?)?)
            .to_hex()
            .to_string(),
    )?;
    fs::write(args.output, serde_json::to_vec_pretty(&report)?)?;
    Ok(())
}

fn validate_manifest(manifest: &SuiteManifest) -> Result<(), DynError> {
    if manifest.schema_version != 1 || manifest.tasks.len() < 3 || manifest.controls.repetitions < 5
    {
        return Err("suite requires schema 1, at least three tasks, and five repetitions".into());
    }
    let arms = manifest
        .arms
        .iter()
        .map(|arm| arm.name.as_str())
        .collect::<HashSet<_>>();
    if arms.len() != manifest.arms.len()
        || !arms.contains(manifest.analysis.full_baseline_arm.as_str())
        || !arms.contains(manifest.analysis.retrieval_baseline_arm.as_str())
        || !arms.contains(manifest.analysis.candidate_arm.as_str())
    {
        return Err("suite arms are missing, duplicated, or unsupported".into());
    }
    let tasks = manifest
        .tasks
        .iter()
        .map(|task| task.id.as_str())
        .collect::<HashSet<_>>();
    if tasks.len() != manifest.tasks.len() {
        return Err("suite task IDs must be unique".into());
    }
    Ok(())
}

fn load_runs(manifest: &SuiteManifest, root: &Path) -> Result<(Vec<Run>, Consistency), DynError> {
    let task_by_id = manifest
        .tasks
        .iter()
        .map(|task| (task.id.as_str(), task))
        .collect::<HashMap<_, _>>();
    let arm_by_name = manifest
        .arms
        .iter()
        .map(|arm| (arm.name.as_str(), arm))
        .collect::<HashMap<_, _>>();
    let runs_directory = root.join("runs");
    let mut metadata_paths = fs::read_dir(&runs_directory)?
        .map(|entry| entry.map(|entry| entry.path().join("run.json")))
        .collect::<Result<Vec<_>, _>>()?;
    metadata_paths.sort();

    let expected_count = manifest.tasks.len() * manifest.arms.len() * manifest.controls.repetitions;
    if metadata_paths.len() != expected_count {
        return Err(format!(
            "expected {expected_count} run metadata files, found {}",
            metadata_paths.len()
        )
        .into());
    }

    let mut seen = HashSet::new();
    let mut schedule_indices = BTreeSet::new();
    let mut host_versions = BTreeSet::new();
    let mut models = BTreeSet::new();
    let mut reasoning_efforts = BTreeSet::new();
    let mut receipt_binary_hashes = BTreeSet::new();
    let mut contract_violation_count = 0usize;
    let mut runs = Vec::with_capacity(expected_count);

    for metadata_path in metadata_paths {
        let metadata: RunMetadata = serde_json::from_slice(&fs::read(&metadata_path)?)?;
        if metadata.schema_version != 1
            || metadata.schedule_key.len() != 64
            || metadata.repetition == 0
            || metadata.repetition > manifest.controls.repetitions
            || metadata.corpus.is_empty()
        {
            return Err("invalid run metadata".into());
        }
        let task = task_by_id
            .get(metadata.task_id.as_str())
            .ok_or("run references an unknown task")?;
        let arm = arm_by_name
            .get(metadata.arm.as_str())
            .ok_or("run references an unknown arm")?;
        if !seen.insert((
            metadata.task_id.clone(),
            metadata.arm.clone(),
            metadata.repetition,
        )) || !schedule_indices.insert(metadata.schedule_index)
        {
            return Err("duplicate task/arm/repetition or schedule index".into());
        }
        let receipt_relative = Path::new(&metadata.receipt);
        if receipt_relative.is_absolute()
            || receipt_relative
                .components()
                .any(|component| !matches!(component, Component::Normal(_)))
        {
            return Err("run receipt path must be a safe relative path".into());
        }
        let receipt: Receipt = serde_json::from_slice(&fs::read(root.join(receipt_relative))?)?;
        validate_receipt(manifest, task, arm, &receipt)?;
        let child = receipt
            .thread_receipts
            .iter()
            .find(|thread| thread.role == "subagent")
            .ok_or("receipt has no child thread")?;
        host_versions.insert(child.host_version.clone());
        models.insert(child.model.clone());
        reasoning_efforts.insert(
            child
                .reasoning_effort
                .clone()
                .unwrap_or_else(|| "unknown".to_owned()),
        );
        receipt_binary_hashes.insert(receipt.receipt_binary_blake3.clone());
        let violation = if arm.repository_reads == "leantoken" {
            child.tool_calls.shell > 0
                || child.tool_calls.failed_mcp > 0
                || arm
                    .maximum_mcp_calls
                    .is_some_and(|maximum| child.tool_calls.mcp > maximum)
        } else {
            child.tool_calls.mcp > 0
        };
        contract_violation_count += usize::from(violation);
        runs.push(Run {
            schedule_index: metadata.schedule_index,
            repetition: metadata.repetition,
            task_id: metadata.task_id,
            arm: metadata.arm,
            source_family_blake3: receipt.source_family_blake3,
            success: receipt.task_evaluation.task_success,
            usage: receipt.provider_usage,
            child_usage: child.provider_usage,
            child_requests: child.provider_request_count,
            child_tools: ToolCalls {
                mcp: child.tool_calls.mcp,
                failed_mcp: child.tool_calls.failed_mcp,
                mcp_result_json_bytes: child.tool_calls.mcp_result_json_bytes,
                mcp_emitted_source_tokens: child.tool_calls.mcp_emitted_source_tokens,
                shell: child.tool_calls.shell,
            },
        });
    }
    runs.sort_by_key(|run| run.schedule_index);
    let expected_indices = (1..=expected_count).collect::<BTreeSet<_>>();
    if schedule_indices != expected_indices {
        return Err("run schedule indices are incomplete".into());
    }

    Ok((
        runs,
        Consistency {
            complete_schedule: true,
            host_versions: host_versions.into_iter().collect(),
            models: models.into_iter().collect(),
            reasoning_efforts: reasoning_efforts.into_iter().collect(),
            receipt_binary_blake3: receipt_binary_hashes.into_iter().collect(),
            contract_violation_count,
        },
    ))
}

fn validate_receipt(
    manifest: &SuiteManifest,
    task: &Task,
    arm: &Arm,
    receipt: &Receipt,
) -> Result<(), DynError> {
    if receipt.schema_version != 1
        || receipt.experiment_id != manifest.experiment_id
        || receipt.arm != arm.name
        || receipt.task_evaluation.task_id != task.id
        || receipt.task_evaluation.repository_revision != task.repository_revision
        || receipt.task_evaluation.evaluation_mode != "path"
        || receipt.task_evaluation.expected_evidence_count == 0
        || receipt.task_evaluation.matched_path_count
            > receipt.task_evaluation.expected_evidence_count
        || receipt.task_evaluation.unexpected_evidence_count
            > receipt
                .task_evaluation
                .expected_evidence_count
                .saturating_mul(8)
        || receipt.topology.thread_count != 2
        || receipt.topology.child_thread_count != 1
        || receipt.topology.maximum_depth != 1
        || receipt.topology.spawn_calls != 1
        || receipt.topology.matched_spawn_calls != 1
        || receipt.thread_receipts.len() != 2
    {
        return Err("receipt does not satisfy the frozen suite contract".into());
    }
    Ok(())
}

fn build_report(
    manifest: &SuiteManifest,
    runs: &[Run],
    consistency: Consistency,
    manifest_hash: String,
    aggregate_binary_hash: String,
) -> Result<Report, DynError> {
    let language_by_task = manifest
        .tasks
        .iter()
        .map(|task| (task.id.as_str(), task.language.as_str()))
        .collect::<HashMap<_, _>>();
    let mut arm_summaries = Vec::new();
    for arm in &manifest.arms {
        let selected = runs
            .iter()
            .filter(|run| run.arm == arm.name)
            .collect::<Vec<_>>();
        arm_summaries.push(summarize_arm(&arm.name, &selected, manifest));
    }

    let mut task_arm_summaries = Vec::new();
    for task in &manifest.tasks {
        for arm in &manifest.arms {
            let selected = runs
                .iter()
                .filter(|run| run.task_id == task.id && run.arm == arm.name)
                .collect::<Vec<_>>();
            task_arm_summaries.push(TaskArmSummary {
                task_id: task.id.clone(),
                language: task.language.clone(),
                arm: arm.name.clone(),
                runs: selected.len(),
                successes: selected.iter().filter(|run| run.success).count(),
                success_rate: ratio(
                    selected.iter().filter(|run| run.success).count(),
                    selected.len(),
                ),
                family_total_input: summarize_u64(
                    &selected
                        .iter()
                        .map(|run| run.usage.total_input_tokens)
                        .collect::<Vec<_>>(),
                ),
            });
        }
    }

    let combined = compare(
        runs,
        &manifest.analysis.full_baseline_arm,
        &manifest.analysis.candidate_arm,
    )?;
    let retrieval = compare(
        runs,
        &manifest.analysis.retrieval_baseline_arm,
        &manifest.analysis.candidate_arm,
    )?;
    let fork = compare(
        runs,
        &manifest.analysis.full_baseline_arm,
        &manifest.analysis.retrieval_baseline_arm,
    )?;
    let comparisons = vec![fork, retrieval, combined];
    let quality_gate = evaluate_gates(
        &manifest.quality_gates,
        &manifest.analysis,
        &arm_summaries,
        &task_arm_summaries,
        &comparisons,
        consistency.contract_violation_count,
    )?;
    let receipt_set_blake3 = blake3::hash(
        runs.iter()
            .map(|run| format!("{}:{}", run.schedule_index, run.source_family_blake3))
            .collect::<Vec<_>>()
            .join("\n")
            .as_bytes(),
    )
    .to_hex()
    .to_string();
    let run_samples = runs
        .iter()
        .map(|run| RunSample {
            schedule_index: run.schedule_index,
            repetition: run.repetition,
            task_id: run.task_id.clone(),
            arm: run.arm.clone(),
            source_family_blake3: run.source_family_blake3.clone(),
            success: run.success,
            family_usage: run.usage.into(),
            child_usage: run.child_usage.into(),
            child_provider_requests: run.child_requests,
            child_mcp_calls: run.child_tools.mcp,
            child_failed_mcp_calls: run.child_tools.failed_mcp,
            child_mcp_result_bytes: run.child_tools.mcp_result_json_bytes,
            child_mcp_source_tokens: run.child_tools.mcp_emitted_source_tokens,
            child_shell_calls: run.child_tools.shell,
        })
        .collect();
    let languages = language_by_task
        .values()
        .copied()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .map(str::to_owned)
        .collect();

    Ok(Report {
        schema_version: 1,
        experiment_id: manifest.experiment_id.clone(),
        suite_manifest_blake3: manifest_hash,
        aggregate_binary_blake3: aggregate_binary_hash,
        receipt_set_blake3,
        run_count: runs.len(),
        task_count: manifest.tasks.len(),
        repetitions: manifest.controls.repetitions,
        arms: manifest.arms.iter().map(|arm| arm.name.clone()).collect(),
        languages,
        consistency,
        arm_summaries,
        task_arm_summaries,
        comparisons,
        quality_gate,
        run_samples,
        limitations: vec![
            "The confidence interval resamples repetitions within the four fixed tasks; it does not generalize to unseen tasks.",
            "Total input measures context volume. Uncached input is reported separately and depends on provider cache state.",
            "Path-set success follows the pre-existing validation labels and does not establish patch correctness.",
            "The suite uses one Codex CLI version, its account-selected default model, one reasoning effort, and one root-plus-child topology.",
        ],
    })
}

fn summarize_arm(name: &str, runs: &[&Run], manifest: &SuiteManifest) -> ArmSummary {
    let successes = runs.iter().filter(|run| run.success).count();
    let arm = manifest
        .arms
        .iter()
        .find(|arm| arm.name == name)
        .expect("validated arm");
    let contract_violations = runs
        .iter()
        .filter(|run| {
            if arm.repository_reads == "leantoken" {
                run.child_tools.shell > 0
                    || run.child_tools.failed_mcp > 0
                    || arm
                        .maximum_mcp_calls
                        .is_some_and(|maximum| run.child_tools.mcp > maximum)
            } else {
                run.child_tools.mcp > 0
            }
        })
        .count();
    ArmSummary {
        arm: name.to_owned(),
        runs: runs.len(),
        successes,
        success_rate: ratio(successes, runs.len()),
        success_rate_wilson_95: wilson_interval(successes, runs.len()),
        family_total_input: summarize_u64(
            &runs
                .iter()
                .map(|run| run.usage.total_input_tokens)
                .collect::<Vec<_>>(),
        ),
        family_uncached_input: summarize_u64(
            &runs
                .iter()
                .map(|run| run.usage.uncached_input_tokens)
                .collect::<Vec<_>>(),
        ),
        family_cache_read_input: summarize_u64(
            &runs
                .iter()
                .map(|run| run.usage.cache_read_input_tokens)
                .collect::<Vec<_>>(),
        ),
        family_output: summarize_u64(
            &runs
                .iter()
                .map(|run| run.usage.output_tokens)
                .collect::<Vec<_>>(),
        ),
        child_total_input: summarize_u64(
            &runs
                .iter()
                .map(|run| run.child_usage.total_input_tokens)
                .collect::<Vec<_>>(),
        ),
        child_provider_requests: summarize_u64(
            &runs
                .iter()
                .map(|run| run.child_requests as u64)
                .collect::<Vec<_>>(),
        ),
        child_mcp_result_bytes: summarize_u64(
            &runs
                .iter()
                .map(|run| run.child_tools.mcp_result_json_bytes)
                .collect::<Vec<_>>(),
        ),
        child_mcp_source_tokens: summarize_u64(
            &runs
                .iter()
                .map(|run| run.child_tools.mcp_emitted_source_tokens)
                .collect::<Vec<_>>(),
        ),
        contract_violations,
    }
}

fn compare(runs: &[Run], baseline: &str, candidate: &str) -> Result<Comparison, DynError> {
    let by_key = runs
        .iter()
        .map(|run| {
            (
                (run.task_id.as_str(), run.repetition, run.arm.as_str()),
                run,
            )
        })
        .collect::<HashMap<_, _>>();
    let baseline_runs = runs.iter().filter(|run| run.arm == baseline).count();
    let candidate_runs = runs.iter().filter(|run| run.arm == candidate).count();
    if baseline_runs != candidate_runs || baseline_runs == 0 {
        return Err("comparison arms are incomplete".into());
    }
    let mut pairs = Vec::with_capacity(baseline_runs);
    for run in runs.iter().filter(|run| run.arm == baseline) {
        let candidate_run = by_key
            .get(&(run.task_id.as_str(), run.repetition, candidate))
            .ok_or("missing paired candidate run")?;
        pairs.push((run, *candidate_run));
    }
    pairs.sort_by_key(|(run, _)| (run.task_id.as_str(), run.repetition));
    let baseline_total = pairs
        .iter()
        .map(|(base, _)| base.usage.total_input_tokens)
        .sum::<u64>();
    let candidate_total = pairs
        .iter()
        .map(|(_, candidate)| candidate.usage.total_input_tokens)
        .sum::<u64>();
    let baseline_uncached = pairs
        .iter()
        .map(|(base, _)| base.usage.uncached_input_tokens)
        .sum::<u64>();
    let candidate_uncached = pairs
        .iter()
        .map(|(_, candidate)| candidate.usage.uncached_input_tokens)
        .sum::<u64>();
    let baseline_child = pairs
        .iter()
        .map(|(base, _)| base.child_usage.total_input_tokens)
        .sum::<u64>();
    let candidate_child = pairs
        .iter()
        .map(|(_, candidate)| candidate.child_usage.total_input_tokens)
        .sum::<u64>();
    let paired_savings = pairs
        .iter()
        .map(|(base, candidate)| {
            savings(
                base.usage.total_input_tokens,
                candidate.usage.total_input_tokens,
            )
        })
        .collect::<Vec<_>>();
    let mut per_task = Vec::new();
    for task in pairs
        .iter()
        .map(|(run, _)| run.task_id.as_str())
        .collect::<BTreeSet<_>>()
    {
        let selected = pairs
            .iter()
            .filter(|(run, _)| run.task_id == task)
            .copied()
            .collect::<Vec<_>>();
        let base = selected
            .iter()
            .map(|(run, _)| run.usage.total_input_tokens)
            .sum::<u64>();
        let candidate_total = selected
            .iter()
            .map(|(_, run)| run.usage.total_input_tokens)
            .sum::<u64>();
        let values = selected
            .iter()
            .map(|(base, candidate)| {
                savings(
                    base.usage.total_input_tokens,
                    candidate.usage.total_input_tokens,
                )
            })
            .collect::<Vec<_>>();
        per_task.push(TaskComparison {
            task_id: task.to_owned(),
            paired_runs: selected.len(),
            weighted_total_input_savings_fraction: savings(base, candidate_total),
            median_paired_savings_fraction: median_f64(&values),
        });
    }

    let baseline_successes = pairs.iter().filter(|(run, _)| run.success).count();
    let candidate_successes = pairs.iter().filter(|(_, run)| run.success).count();
    Ok(Comparison {
        baseline: baseline.to_owned(),
        candidate: candidate.to_owned(),
        paired_runs: pairs.len(),
        both_successful_runs: pairs
            .iter()
            .filter(|(base, candidate)| base.success && candidate.success)
            .count(),
        candidate_wins: pairs
            .iter()
            .filter(|(base, candidate)| {
                candidate.usage.total_input_tokens < base.usage.total_input_tokens
            })
            .count(),
        equal_token_runs: pairs
            .iter()
            .filter(|(base, candidate)| {
                candidate.usage.total_input_tokens == base.usage.total_input_tokens
            })
            .count(),
        weighted_total_input_savings_fraction: savings(baseline_total, candidate_total),
        paired_savings_median: median_f64(&paired_savings),
        paired_savings_mean: mean_f64(&paired_savings),
        stratified_bootstrap_weighted_savings_95: bootstrap_interval(
            &pairs,
            &format!("{baseline}|{candidate}"),
        ),
        weighted_uncached_input_savings_fraction: savings(baseline_uncached, candidate_uncached),
        weighted_child_input_savings_fraction: savings(baseline_child, candidate_child),
        baseline_success_rate: ratio(baseline_successes, pairs.len()),
        candidate_success_rate: ratio(candidate_successes, pairs.len()),
        success_rate_difference: ratio(candidate_successes, pairs.len())
            - ratio(baseline_successes, pairs.len()),
        per_task,
    })
}

fn evaluate_gates(
    config: &QualityGateConfig,
    analysis: &Analysis,
    arms: &[ArmSummary],
    task_arms: &[TaskArmSummary],
    comparisons: &[Comparison],
    contract_violations: usize,
) -> Result<QualityGateReport, DynError> {
    let candidate_name = analysis.candidate_arm.as_str();
    let candidate = arms
        .iter()
        .find(|summary| summary.arm == candidate_name)
        .ok_or("candidate summary is missing")?;
    let combined = comparisons
        .iter()
        .find(|comparison| {
            comparison.baseline == analysis.full_baseline_arm
                && comparison.candidate == candidate_name
        })
        .ok_or("combined comparison is missing")?;
    let retrieval = comparisons
        .iter()
        .find(|comparison| {
            comparison.baseline == analysis.retrieval_baseline_arm
                && comparison.candidate == candidate_name
        })
        .ok_or("retrieval comparison is missing")?;
    let minimum_task_successes = task_arms
        .iter()
        .filter(|summary| summary.arm == candidate_name)
        .map(|summary| summary.successes)
        .min()
        .unwrap_or(0);
    let worst_per_task_success_regression = task_arms
        .iter()
        .filter(|summary| summary.arm == analysis.retrieval_baseline_arm)
        .map(|baseline| {
            let candidate = task_arms
                .iter()
                .find(|summary| {
                    summary.arm == candidate_name && summary.task_id == baseline.task_id
                })
                .expect("validated task-arm matrix");
            baseline.successes.saturating_sub(candidate.successes)
        })
        .max()
        .unwrap_or(0);
    let worst_success_regression = comparisons
        .iter()
        .filter(|comparison| comparison.candidate == candidate_name)
        .map(|comparison| -comparison.success_rate_difference)
        .fold(f64::NEG_INFINITY, f64::max)
        .max(0.0);
    let mut checks = BTreeMap::new();
    insert_gate(
        &mut checks,
        "candidate_success_rate",
        candidate.success_rate,
        config.minimum_candidate_success_rate,
        true,
    );
    if let Some(minimum) = config.minimum_candidate_successes_per_task {
        insert_gate(
            &mut checks,
            "candidate_successes_per_task",
            minimum_task_successes as f64,
            minimum as f64,
            true,
        );
    }
    if let Some(maximum) = config.maximum_per_task_success_regression {
        insert_gate(
            &mut checks,
            "per_task_success_regression",
            worst_per_task_success_regression as f64,
            maximum as f64,
            false,
        );
    }
    insert_gate(
        &mut checks,
        "combined_total_input_savings",
        combined.weighted_total_input_savings_fraction,
        config.minimum_combined_total_input_savings_fraction,
        true,
    );
    insert_gate(
        &mut checks,
        "combined_savings_ci_lower_bound",
        combined.stratified_bootstrap_weighted_savings_95.lower,
        config.minimum_combined_savings_ci_lower_bound,
        true,
    );
    insert_gate(
        &mut checks,
        "retrieval_total_input_savings",
        retrieval.weighted_total_input_savings_fraction,
        config.minimum_retrieval_total_input_savings_fraction,
        true,
    );
    insert_gate(
        &mut checks,
        "retrieval_savings_ci_lower_bound",
        retrieval.stratified_bootstrap_weighted_savings_95.lower,
        config.minimum_retrieval_savings_ci_lower_bound,
        true,
    );
    insert_gate(
        &mut checks,
        "success_rate_regression",
        worst_success_regression,
        config.maximum_success_rate_regression,
        false,
    );
    insert_gate(
        &mut checks,
        "contract_violations",
        contract_violations as f64,
        0.0,
        false,
    );
    Ok(QualityGateReport {
        passed: checks.values().all(|check| check.passed),
        checks,
    })
}

fn insert_gate(
    checks: &mut BTreeMap<String, GateCheck>,
    name: &str,
    observed: f64,
    threshold: f64,
    minimum: bool,
) {
    checks.insert(
        name.to_owned(),
        GateCheck {
            passed: if minimum {
                observed >= threshold
            } else {
                observed <= threshold
            },
            observed,
            required: if minimum {
                format!(">= {threshold}")
            } else {
                format!("<= {threshold}")
            },
        },
    );
}

fn summarize_u64(values: &[u64]) -> NumericSummary {
    assert!(!values.is_empty());
    let mut sorted = values.to_vec();
    sorted.sort_unstable();
    let count = sorted.len();
    let sum = sorted.iter().sum::<u64>();
    let mean = sum as f64 / count as f64;
    let median = if count.is_multiple_of(2) {
        (sorted[count / 2 - 1] as f64 + sorted[count / 2] as f64) / 2.0
    } else {
        sorted[count / 2] as f64
    };
    let variance = if count > 1 {
        sorted
            .iter()
            .map(|value| (*value as f64 - mean).powi(2))
            .sum::<f64>()
            / (count - 1) as f64
    } else {
        0.0
    };
    let sample_standard_deviation = variance.sqrt();
    NumericSummary {
        count,
        sum,
        mean,
        median,
        minimum: sorted[0],
        maximum: sorted[count - 1],
        sample_standard_deviation,
        coefficient_of_variation: if mean == 0.0 {
            0.0
        } else {
            sample_standard_deviation / mean
        },
    }
}

fn ratio(numerator: usize, denominator: usize) -> f64 {
    numerator as f64 / denominator as f64
}

fn savings(baseline: u64, candidate: u64) -> f64 {
    (baseline as f64 - candidate as f64) / baseline as f64
}

fn mean_f64(values: &[f64]) -> f64 {
    values.iter().sum::<f64>() / values.len() as f64
}

fn median_f64(values: &[f64]) -> f64 {
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    if sorted.len().is_multiple_of(2) {
        (sorted[sorted.len() / 2 - 1] + sorted[sorted.len() / 2]) / 2.0
    } else {
        sorted[sorted.len() / 2]
    }
}

fn wilson_interval(successes: usize, count: usize) -> Interval {
    let z = 1.959_963_984_540_054_f64;
    let n = count as f64;
    let p = successes as f64 / n;
    let denominator = 1.0 + z * z / n;
    let center = (p + z * z / (2.0 * n)) / denominator;
    let margin = z * ((p * (1.0 - p) + z * z / (4.0 * n)) / n).sqrt() / denominator;
    Interval {
        lower: (center - margin).max(0.0),
        upper: (center + margin).min(1.0),
    }
}

fn bootstrap_interval(pairs: &[(&Run, &Run)], label: &str) -> Interval {
    let mut groups = BTreeMap::<&str, Vec<(&Run, &Run)>>::new();
    for pair in pairs {
        groups
            .entry(pair.0.task_id.as_str())
            .or_default()
            .push(*pair);
    }
    let digest = blake3::hash(label.as_bytes());
    let mut seed_bytes = [0_u8; 8];
    seed_bytes.copy_from_slice(&digest.as_bytes()[..8]);
    let mut rng = XorShift64::new(u64::from_le_bytes(seed_bytes));
    let mut samples = Vec::with_capacity(10_000);
    for _ in 0..10_000 {
        let mut baseline = 0_u64;
        let mut candidate = 0_u64;
        for group in groups.values() {
            for _ in 0..group.len() {
                let pair = group[rng.index(group.len())];
                baseline += pair.0.usage.total_input_tokens;
                candidate += pair.1.usage.total_input_tokens;
            }
        }
        samples.push(savings(baseline, candidate));
    }
    samples.sort_by(f64::total_cmp);
    Interval {
        lower: samples[249],
        upper: samples[9_749],
    }
}

struct XorShift64(u64);

impl XorShift64 {
    fn new(seed: u64) -> Self {
        Self(if seed == 0 {
            0x9e37_79b9_7f4a_7c15
        } else {
            seed
        })
    }

    fn index(&mut self, upper: usize) -> usize {
        let mut value = self.0;
        value ^= value << 13;
        value ^= value >> 7;
        value ^= value << 17;
        self.0 = value;
        value as usize % upper
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_numeric_summary_reports_sample_variation() {
        let summary = summarize_u64(&[10, 20, 30, 40]);
        assert_eq!(summary.sum, 100);
        assert_eq!(summary.median, 25.0);
        assert!((summary.sample_standard_deviation - 12.909_944).abs() < 0.000_01);
    }

    #[test]
    fn test_wilson_interval_contains_observed_rate() {
        let interval = wilson_interval(19, 20);
        assert!(interval.lower < 0.95);
        assert!(interval.upper > 0.95);
    }

    #[test]
    fn test_savings_sign_is_positive_when_candidate_is_smaller() {
        assert_eq!(savings(100, 80), 0.2);
        assert_eq!(savings(100, 120), -0.2);
    }
}
