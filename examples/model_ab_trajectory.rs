#[allow(dead_code)]
#[path = "support/model_ab_artifacts.rs"]
mod model_ab_artifacts;

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::error::Error;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::sync::LazyLock;

use clap::Parser;
use leantoken::tokens::Tokenizer;
use model_ab_artifacts::{
    ARTIFACT_SCHEMA_V1, PrewalkHandoff, RunBinding, ToolOutcome, ToolTrace, Trajectory,
};
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use unidiff::PatchSet;

const REPORT_SCHEMA_V1: u32 = 1;
const MANIFEST_SCHEMA_V1: u32 = 1;

type DynError = Box<dyn Error>;

#[derive(Debug, Parser)]
#[command(about = "Classify frozen model A/B retrieval and handoff trajectories")]
struct Args {
    /// Portable classifier manifest with frozen report, dataset, and artifact identities.
    #[arg(long)]
    manifest: PathBuf,
    /// Redacted classifier report. Raw commands, output, source, and paths are omitted.
    #[arg(long)]
    output: PathBuf,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    schema_version: u32,
    experiment_id: String,
    raw_report: PathBuf,
    raw_report_blake3: String,
    artifacts_root: PathBuf,
    dataset: PathBuf,
    dataset_blake3: String,
    repositories: BTreeMap<String, PathBuf>,
    tokenizer: Tokenizer,
    broad_read_min_lines: usize,
    broad_read_min_fraction: f64,
    baseline_arm: String,
    candidate_arm: String,
    handoff_arm: String,
}

#[derive(Debug, Deserialize)]
struct RawReport {
    schema_version: u32,
    experiment_id: String,
    manifest_blake3: String,
    primary_model: String,
    executor_model: String,
    repetitions: usize,
    task_definitions: Vec<TaskDefinition>,
    runs: Vec<RawRun>,
}

#[derive(Debug, Deserialize)]
struct TaskDefinition {
    id: String,
    #[allow(dead_code)]
    repository: PathBuf,
    revision: String,
}

#[derive(Debug, Deserialize)]
struct RawRun {
    task_id: String,
    repetition: usize,
    arm: String,
    duration_ms: u64,
    status: String,
    artifacts: RawArtifacts,
    result: Option<RawResult>,
}

#[derive(Debug, Deserialize)]
struct RawArtifacts {
    tool_trace: Option<ArtifactIdentity>,
    trajectory: Option<ArtifactIdentity>,
    prewalk_handoff: Option<ArtifactIdentity>,
}

#[derive(Debug, Deserialize)]
struct ArtifactIdentity {
    blake3: String,
}

#[derive(Debug, Deserialize)]
struct RawResult {
    task_success: bool,
}

#[derive(Debug, Deserialize)]
struct DatasetRecord {
    instance_id: String,
    patch: String,
    test_patch: String,
}

#[derive(Debug, Clone, Serialize)]
struct RunClassification {
    task_id: String,
    repetition: usize,
    arm: String,
    status: String,
    official_success: bool,
    duration_ms: u64,
    evidence_scope: EvidenceScope,
    discovery_order: Vec<DiscoveryKind>,
    retrieval_calls: usize,
    source_tokens: u64,
    whole_file_reads: usize,
    whole_file_read_tokens: u64,
    broad_reads: usize,
    broad_read_tokens: u64,
    exact_rereads: usize,
    exact_reread_tokens: u64,
    overlap_rereads: usize,
    overlap_reread_tokens: u64,
    known_hash_inputs: usize,
    known_hash_reuses: usize,
    known_hash_resends: usize,
    failed_searches: usize,
    failed_discovery_calls: usize,
    failed_discovery_recoveries: usize,
    unrecovered_failed_discovery: bool,
    retryable_results: usize,
    dead_end_reads: usize,
    dead_end_source_tokens: u64,
    dead_end_recoveries: usize,
    mixed_native_read_tokens: u64,
    repository_generation_changes: usize,
    stale_index_results: usize,
    native_read_commands: usize,
    parsed_native_read_commands: usize,
    first_edit_sequence: Option<usize>,
    first_validated_edit_sequence: Option<usize>,
    handoff: Option<HandoffClassification>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum DiscoveryKind {
    Files,
    Outline,
    Search,
    Read,
    Context,
    NativeSearch,
    NativeRead,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum EvidenceScope {
    Unavailable,
    ExactTrace,
    HandoffPrimary,
}

#[derive(Debug, Clone, Serialize)]
struct HandoffClassification {
    transferred_evidence_calls: usize,
    transferred_source_tokens: u64,
    transferred_ranges: usize,
    todo_events: usize,
    executor_retrieval_calls: Option<usize>,
    first_validated_edit_sequence: usize,
    validation_sequence: usize,
}

#[derive(Debug, Default)]
struct Metrics {
    discovery_order: Vec<DiscoveryKind>,
    retrieval_calls: usize,
    source_tokens: u64,
    whole_file_reads: usize,
    whole_file_read_tokens: u64,
    broad_reads: usize,
    broad_read_tokens: u64,
    exact_rereads: usize,
    exact_reread_tokens: u64,
    overlap_rereads: usize,
    overlap_reread_tokens: u64,
    known_hash_inputs: usize,
    known_hash_reuses: usize,
    known_hash_resends: usize,
    failed_searches: usize,
    failed_discovery_calls: usize,
    failed_discovery_recoveries: usize,
    pending_failed_discovery: bool,
    retryable_results: usize,
    dead_end_reads: usize,
    dead_end_source_tokens: u64,
    dead_end_recoveries: usize,
    pending_dead_end: bool,
    active_range_call: Option<usize>,
    active_call_has_dead_end: bool,
    active_call_has_relevant: bool,
    mixed_native_read_tokens: u64,
    repository_generation_changes: usize,
    stale_index_results: usize,
    native_read_commands: usize,
    parsed_native_read_commands: usize,
    first_edit_sequence: Option<usize>,
    generations: Vec<u64>,
    ranges: Vec<ObservedRange>,
}

#[derive(Debug, Clone)]
struct ObservedRange {
    call_sequence: usize,
    path: String,
    start_line: usize,
    end_line: usize,
    source_tokens: u64,
    full_content_eligible: bool,
}

#[derive(Debug, Clone)]
struct NativeRead {
    path: String,
    start_line: usize,
    end_line: usize,
}

#[derive(Debug, Serialize)]
struct Report {
    schema_version: u32,
    report_kind: &'static str,
    status: &'static str,
    experiment_id: String,
    source: SourceReceipt,
    controls: ControlReceipt,
    arm_summaries: BTreeMap<String, ArmSummary>,
    task_arm_summaries: BTreeMap<String, BTreeMap<String, ArmSummary>>,
    runs: Vec<RunClassification>,
    decision: Decision,
    limitations: Vec<&'static str>,
}

#[derive(Debug, Serialize)]
struct SourceReceipt {
    classifier_binary_blake3: String,
    classifier_source_blake3: String,
    classifier_manifest_blake3: String,
    raw_report_blake3: String,
    model_ab_manifest_blake3: String,
    dataset_blake3: String,
    raw_report_schema_version: u32,
    verified_artifacts: usize,
}

#[derive(Debug, Serialize)]
struct ControlReceipt {
    primary_model: String,
    executor_model: String,
    tasks: usize,
    repetitions: usize,
    runs: usize,
    tokenizer: String,
    broad_read_min_lines: usize,
    broad_read_min_fraction: f64,
    baseline_arm: String,
    candidate_arm: String,
    handoff_arm: String,
}

#[derive(Debug, Default, Clone, Serialize)]
struct ArmSummary {
    runs: usize,
    classified_runs: usize,
    successes: usize,
    success_rate: f64,
    retrieval_calls: Distribution,
    source_tokens: Distribution,
    whole_file_reads: Distribution,
    broad_reads: Distribution,
    exact_rereads: Distribution,
    overlap_rereads: Distribution,
    failed_searches: Distribution,
    failed_discovery_calls: Distribution,
    failed_discovery_recoveries: Distribution,
    unrecovered_failed_discovery_runs: usize,
    retryable_results: Distribution,
    dead_end_source_tokens: Distribution,
    dead_end_recoveries: Distribution,
    duration_ms: Distribution,
    known_hash_inputs: usize,
    known_hash_reuses: usize,
    known_hash_resends: usize,
    repository_generation_changes: usize,
    stale_index_results: usize,
    handoff_artifacts: usize,
    observed_executor_handoffs: usize,
    executor_retrieval_calls: usize,
}

#[derive(Debug, Default, Clone, Serialize)]
struct Distribution {
    samples: usize,
    minimum: Option<f64>,
    median: Option<f64>,
    mean: Option<f64>,
    maximum: Option<f64>,
    sample_variance: Option<f64>,
}

#[derive(Debug, Serialize)]
struct Decision {
    result: &'static str,
    reason: String,
    baseline_successes: usize,
    candidate_successes: usize,
    baseline_median_dead_end_source_tokens: Option<f64>,
    candidate_median_dead_end_source_tokens: Option<f64>,
    baseline_median_overlap_rereads: Option<f64>,
    candidate_median_overlap_rereads: Option<f64>,
    change_tool_descriptions: bool,
    add_receipt_fields: bool,
    add_next_useful_action: bool,
    add_server_session_state: bool,
}

fn main() -> Result<(), DynError> {
    let args = Args::parse();
    let manifest_bytes = fs::read(&args.manifest)?;
    let manifest: Manifest = serde_json::from_slice(&manifest_bytes)?;
    validate_manifest(&manifest)?;
    let root = args.manifest.parent().unwrap_or_else(|| Path::new("."));
    let raw_report_path = resolve(root, &manifest.raw_report);
    let artifacts_root = resolve(root, &manifest.artifacts_root);
    let dataset_path = resolve(root, &manifest.dataset);
    let raw_report_bytes = read_verified(&raw_report_path, &manifest.raw_report_blake3)?;
    let dataset_bytes = read_verified(&dataset_path, &manifest.dataset_blake3)?;
    let raw_report: RawReport = serde_json::from_slice(&raw_report_bytes)?;
    if raw_report.experiment_id != manifest.experiment_id {
        return Err("classifier and model A/B experiment IDs differ".into());
    }
    let task_ids = raw_report
        .task_definitions
        .iter()
        .map(|task| task.id.clone())
        .collect::<BTreeSet<_>>();
    let labels = dataset_labels(&dataset_bytes, &task_ids)?;
    if manifest.repositories.keys().collect::<BTreeSet<_>>()
        != task_ids.iter().collect::<BTreeSet<_>>()
    {
        return Err("classifier repository set differs from the frozen task set".into());
    }
    let repositories = raw_report
        .task_definitions
        .iter()
        .map(|task| {
            let configured = manifest
                .repositories
                .get(&task.id)
                .ok_or("classifier manifest is missing a task repository")?;
            let repository = resolve(root, configured);
            verify_repository(&repository, &task.revision)?;
            Ok((task.id.clone(), repository))
        })
        .collect::<Result<BTreeMap<_, _>, DynError>>()?;
    let mut line_counts = HashMap::new();
    let mut verified_artifacts = 0usize;
    let mut runs = Vec::with_capacity(raw_report.runs.len());
    for run in &raw_report.runs {
        runs.push(classify_run(
            run,
            &raw_report.experiment_id,
            &raw_report.manifest_blake3,
            &artifacts_root,
            &labels,
            &repositories,
            &manifest,
            &mut line_counts,
            &mut verified_artifacts,
        )?);
    }
    let arm_summaries = aggregate(&runs);
    let task_arm_summaries = aggregate_by_task(&runs);
    let baseline = arm_summaries
        .get(&manifest.baseline_arm)
        .ok_or("baseline arm is absent")?;
    let candidate = arm_summaries
        .get(&manifest.candidate_arm)
        .ok_or("candidate arm is absent")?;
    let negative = candidate.successes < baseline.successes;
    let decision = Decision {
        result: if negative { "no_go" } else { "inconclusive" },
        reason: if negative {
            format!(
                "{} had fewer validated successes than {} ({} versus {}), so retrieval-efficiency diagnostics cannot authorize a behavior or protocol change",
                manifest.candidate_arm,
                manifest.baseline_arm,
                candidate.successes,
                baseline.successes
            )
        } else {
            "The classified traces do not satisfy a pre-registered controlled-win rule".to_owned()
        },
        baseline_successes: baseline.successes,
        candidate_successes: candidate.successes,
        baseline_median_dead_end_source_tokens: baseline.dead_end_source_tokens.median,
        candidate_median_dead_end_source_tokens: candidate.dead_end_source_tokens.median,
        baseline_median_overlap_rereads: baseline.overlap_rereads.median,
        candidate_median_overlap_rereads: candidate.overlap_rereads.median,
        change_tool_descriptions: false,
        add_receipt_fields: false,
        add_next_useful_action: false,
        add_server_session_state: false,
    };
    let report = Report {
        schema_version: REPORT_SCHEMA_V1,
        report_kind: "model_ab_trajectory_classification",
        status: if negative {
            "completed_post_hoc_no_go"
        } else {
            "completed_post_hoc_inconclusive"
        },
        experiment_id: raw_report.experiment_id,
        source: SourceReceipt {
            classifier_binary_blake3: hash_file(&std::env::current_exe()?)?,
            classifier_source_blake3: hash_bytes(include_bytes!("model_ab_trajectory.rs")),
            classifier_manifest_blake3: hash_bytes(&manifest_bytes),
            raw_report_blake3: hash_bytes(&raw_report_bytes),
            model_ab_manifest_blake3: raw_report.manifest_blake3,
            dataset_blake3: hash_bytes(&dataset_bytes),
            raw_report_schema_version: raw_report.schema_version,
            verified_artifacts,
        },
        controls: ControlReceipt {
            primary_model: raw_report.primary_model,
            executor_model: raw_report.executor_model,
            tasks: raw_report.task_definitions.len(),
            repetitions: raw_report.repetitions,
            runs: runs.len(),
            tokenizer: manifest.tokenizer.name().to_owned(),
            broad_read_min_lines: manifest.broad_read_min_lines,
            broad_read_min_fraction: manifest.broad_read_min_fraction,
            baseline_arm: manifest.baseline_arm,
            candidate_arm: manifest.candidate_arm,
            handoff_arm: manifest.handoff_arm,
        },
        arm_summaries,
        task_arm_summaries,
        runs,
        decision,
        limitations: vec![
            "This classifier was defined after the underlying public-task model runs and is diagnostic, not blinded confirmation.",
            "Native command parsing recognizes common rg/grep/find/ls, sed, cat, head, and tail forms; mixed shell pipelines retain unattributed read tokens instead of inventing range precision.",
            "Dead-end labels are paths touched by the official patch or test patch; adjacent files may still be useful even when labeled dead-end.",
            "Dead-end recovery requires a later retrieval call to touch a labeled path after a call whose ranges were all unlabeled; discovery recovery closes a consecutive failure episode on the next successful retrieval.",
            "Native reread attribution is a lower bound when a command form cannot be parsed into repository-relative ranges.",
            "The prewalk arm changes executor model as well as handoff structure and is mechanism-only evidence.",
            "Three prewalk runs retain only the primary handoff after executor adapter failure; their executor retrieval behavior is unknown and represented as null.",
            "No tool description, MCP receipt, next-action field, runtime session state, or server behavior changed as a result.",
        ],
    };
    let mut report_bytes = serde_json::to_vec_pretty(&report)?;
    report_bytes.push(b'\n');
    fs::write(args.output, report_bytes)?;
    Ok(())
}

fn validate_manifest(manifest: &Manifest) -> Result<(), DynError> {
    if manifest.schema_version != MANIFEST_SCHEMA_V1 {
        return Err("unsupported trajectory manifest schema".into());
    }
    if manifest.experiment_id.trim().is_empty()
        || manifest.baseline_arm == manifest.candidate_arm
        || manifest.repositories.is_empty()
        || manifest.broad_read_min_lines == 0
        || !(0.0..=1.0).contains(&manifest.broad_read_min_fraction)
        || manifest.broad_read_min_fraction == 0.0
    {
        return Err("invalid trajectory classifier controls".into());
    }
    validate_hash(&manifest.raw_report_blake3)?;
    validate_hash(&manifest.dataset_blake3)?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn classify_run(
    run: &RawRun,
    experiment_id: &str,
    model_ab_manifest_blake3: &str,
    artifacts_root: &Path,
    labels: &BTreeMap<String, BTreeSet<String>>,
    repositories: &BTreeMap<String, PathBuf>,
    manifest: &Manifest,
    line_counts: &mut HashMap<(String, String), usize>,
    verified_artifacts: &mut usize,
) -> Result<RunClassification, DynError> {
    let mut metrics = Metrics::default();
    let directory = artifacts_root
        .join(experiment_id)
        .join(&run.task_id)
        .join(format!("repetition-{}", run.repetition))
        .join(&run.arm);
    let relevant = labels
        .get(&run.task_id)
        .ok_or("task has no dataset labels")?;
    let repository = repositories
        .get(&run.task_id)
        .ok_or("task has no frozen repository")?;
    let mut evidence_scope = EvidenceScope::Unavailable;
    let mut handoff = None;
    let mut first_validated_edit_sequence = None;

    if run.artifacts.tool_trace.is_some() != run.artifacts.trajectory.is_some()
        || (run.artifacts.prewalk_handoff.is_some() && run.arm != manifest.handoff_arm)
    {
        return Err("run has an inconsistent trajectory artifact set".into());
    }

    if let (Some(trace_identity), Some(trajectory_identity)) =
        (&run.artifacts.tool_trace, &run.artifacts.trajectory)
    {
        let trace: ToolTrace = read_artifact(
            &directory.join("tool-trace.json"),
            trace_identity,
            verified_artifacts,
        )?;
        let trajectory: Trajectory = read_artifact(
            &directory.join("trajectory.json"),
            trajectory_identity,
            verified_artifacts,
        )?;
        validate_artifact_schema(trace.schema_version)?;
        validate_artifact_schema(trajectory.schema_version)?;
        validate_binding(&trace.binding, run, experiment_id, model_ab_manifest_blake3)?;
        validate_binding(
            &trajectory.binding,
            run,
            experiment_id,
            model_ab_manifest_blake3,
        )?;
        let call_tools = mcp_tools_by_call_id(&trajectory)?;
        classify_trace(
            &trace,
            &call_tools,
            repository,
            &run.task_id,
            relevant,
            manifest,
            line_counts,
            &mut metrics,
        )?;
        classify_trajectory(
            &trajectory,
            repository,
            &run.task_id,
            relevant,
            manifest,
            line_counts,
            &mut metrics,
        )?;
        evidence_scope = EvidenceScope::ExactTrace;

        if let Some(identity) = &run.artifacts.prewalk_handoff {
            let value: PrewalkHandoff = read_artifact(
                &directory.join("prewalk-handoff.json"),
                identity,
                verified_artifacts,
            )?;
            validate_artifact_schema(value.schema_version)?;
            validate_binding(&value.binding, run, experiment_id, model_ab_manifest_blake3)?;
            validate_handoff(&value, &trace)?;
            let phase_boundary = trajectory
                .events
                .iter()
                .position(|event| event["type"].as_str() == Some("leantoken.phase_boundary"))
                .ok_or("completed prewalk trajectory has no phase boundary")?;
            let executor_retrieval_calls = {
                trajectory.events[phase_boundary + 1..]
                    .iter()
                    .filter(|event| is_retrieval_event(event))
                    .count()
            };
            first_validated_edit_sequence = Some(value.first_validated_edit.edit_sequence);
            handoff = Some(summarize_handoff(&value, Some(executor_retrieval_calls)));
        }
    } else if let Some(identity) = &run.artifacts.prewalk_handoff {
        let value: PrewalkHandoff = read_artifact(
            &directory.join("prewalk-handoff.json"),
            identity,
            verified_artifacts,
        )?;
        validate_artifact_schema(value.schema_version)?;
        validate_binding(&value.binding, run, experiment_id, model_ab_manifest_blake3)?;
        validate_standalone_handoff(&value)?;
        let trajectory = Trajectory {
            schema_version: value.schema_version,
            binding: value.binding.clone(),
            events: value.trajectory_events.clone(),
        };
        let trace = ToolTrace {
            schema_version: value.schema_version,
            binding: value.binding.clone(),
            calls: value.evidence_calls.clone(),
        };
        let call_tools = mcp_tools_by_call_id(&trajectory)?;
        classify_trace(
            &trace,
            &call_tools,
            repository,
            &run.task_id,
            relevant,
            manifest,
            line_counts,
            &mut metrics,
        )?;
        classify_trajectory(
            &trajectory,
            repository,
            &run.task_id,
            relevant,
            manifest,
            line_counts,
            &mut metrics,
        )?;
        metrics.first_edit_sequence = Some(value.first_validated_edit.edit_sequence);
        first_validated_edit_sequence = Some(value.first_validated_edit.edit_sequence);
        handoff = Some(summarize_handoff(&value, None));
        evidence_scope = EvidenceScope::HandoffPrimary;
    }

    Ok(RunClassification {
        task_id: run.task_id.clone(),
        repetition: run.repetition,
        arm: run.arm.clone(),
        status: run.status.clone(),
        official_success: run
            .result
            .as_ref()
            .is_some_and(|result| result.task_success),
        duration_ms: run.duration_ms,
        evidence_scope,
        discovery_order: metrics.discovery_order,
        retrieval_calls: metrics.retrieval_calls,
        source_tokens: metrics.source_tokens,
        whole_file_reads: metrics.whole_file_reads,
        whole_file_read_tokens: metrics.whole_file_read_tokens,
        broad_reads: metrics.broad_reads,
        broad_read_tokens: metrics.broad_read_tokens,
        exact_rereads: metrics.exact_rereads,
        exact_reread_tokens: metrics.exact_reread_tokens,
        overlap_rereads: metrics.overlap_rereads,
        overlap_reread_tokens: metrics.overlap_reread_tokens,
        known_hash_inputs: metrics.known_hash_inputs,
        known_hash_reuses: metrics.known_hash_reuses,
        known_hash_resends: metrics.known_hash_resends,
        failed_searches: metrics.failed_searches,
        failed_discovery_calls: metrics.failed_discovery_calls,
        failed_discovery_recoveries: metrics.failed_discovery_recoveries,
        unrecovered_failed_discovery: metrics.pending_failed_discovery,
        retryable_results: metrics.retryable_results,
        dead_end_reads: metrics.dead_end_reads,
        dead_end_source_tokens: metrics.dead_end_source_tokens,
        dead_end_recoveries: metrics.dead_end_recoveries,
        mixed_native_read_tokens: metrics.mixed_native_read_tokens,
        repository_generation_changes: metrics.repository_generation_changes,
        stale_index_results: metrics.stale_index_results,
        native_read_commands: metrics.native_read_commands,
        parsed_native_read_commands: metrics.parsed_native_read_commands,
        first_edit_sequence: metrics.first_edit_sequence,
        first_validated_edit_sequence,
        handoff,
    })
}

#[allow(clippy::too_many_arguments)]
fn classify_trace(
    trace: &ToolTrace,
    call_tools: &HashMap<String, String>,
    repository: &Path,
    task_id: &str,
    relevant: &BTreeSet<String>,
    manifest: &Manifest,
    line_counts: &mut HashMap<(String, String), usize>,
    metrics: &mut Metrics,
) -> Result<(), DynError> {
    for call in &trace.calls {
        if call.tool_name == "leantoken" {
            metrics.source_tokens = metrics
                .source_tokens
                .checked_add(call.result_source_tokens)
                .ok_or("source-token overflow")?;
        }
        if call.tool_name == "edit"
            && call.outcome == ToolOutcome::Success
            && metrics.first_edit_sequence.is_none()
        {
            metrics.first_edit_sequence = Some(call.sequence);
        }
        for range in &call.ranges {
            let tool = call_tools
                .get(&call.call_id)
                .or_else(|| {
                    call.call_id
                        .strip_prefix("prewalk:")
                        .and_then(|id| call_tools.get(id))
                })
                .ok_or("MCP trace call is missing from the exact trajectory")?;
            let source_tokens = u64::try_from(range.source_tokens.unwrap_or(0))?;
            observe_range(
                ObservedRange {
                    call_sequence: call.sequence,
                    path: normalize_path(&range.path)?,
                    start_line: range.start_line,
                    end_line: range.end_line,
                    source_tokens,
                    full_content_eligible: matches!(
                        tool.as_str(),
                        "leantoken_read" | "leantoken_context"
                    ),
                },
                repository,
                task_id,
                relevant,
                manifest,
                line_counts,
                metrics,
            )?;
        }
    }
    Ok(())
}

fn classify_trajectory(
    trajectory: &Trajectory,
    repository: &Path,
    task_id: &str,
    relevant: &BTreeSet<String>,
    manifest: &Manifest,
    line_counts: &mut HashMap<(String, String), usize>,
    metrics: &mut Metrics,
) -> Result<(), DynError> {
    let mut event_sequence = 1_000_000usize;
    for event in &trajectory.events {
        if event["type"].as_str() != Some("item.completed") {
            continue;
        }
        let item = &event["item"];
        match item["type"].as_str() {
            Some("mcp_tool_call") => {
                let tool = item["tool"].as_str().unwrap_or_default();
                if let Some(kind) = mcp_discovery_kind(tool) {
                    metrics.discovery_order.push(kind);
                    metrics.retrieval_calls += 1;
                    classify_hash_and_generation(tool, item, metrics)?;
                }
            }
            Some("command_execution") => {
                let command = item["command"].as_str().unwrap_or_default();
                let output = item["aggregated_output"].as_str().unwrap_or_default();
                let exit_code = item["exit_code"].as_i64();
                let has_search = is_native_search(command);
                let has_read = is_native_read(command);
                let failed =
                    (has_search || has_read) && (exit_code != Some(0) || output.trim().is_empty());
                if has_search || has_read {
                    observe_discovery_outcome(metrics, failed);
                }
                if has_search {
                    metrics.discovery_order.push(DiscoveryKind::NativeSearch);
                    metrics.retrieval_calls += 1;
                    if failed {
                        metrics.failed_searches += 1;
                    }
                }
                if has_read {
                    metrics.discovery_order.push(DiscoveryKind::NativeRead);
                    metrics.retrieval_calls += 1;
                    metrics.native_read_commands += 1;
                    let reads = native_reads(command, repository, task_id, line_counts)?;
                    if !reads.is_empty() {
                        metrics.parsed_native_read_commands += 1;
                    }
                    let output_tokens = u64::try_from(manifest.tokenizer.count(output))?;
                    metrics.source_tokens = metrics
                        .source_tokens
                        .checked_add(output_tokens)
                        .ok_or("source-token overflow")?;
                    if has_search || reads.len() != 1 {
                        metrics.mixed_native_read_tokens = metrics
                            .mixed_native_read_tokens
                            .checked_add(output_tokens)
                            .ok_or("mixed read-token overflow")?;
                    }
                    let attribute_tokens = !has_search && reads.len() == 1;
                    for read in reads {
                        let source_tokens = if attribute_tokens { output_tokens } else { 0 };
                        observe_range(
                            ObservedRange {
                                call_sequence: event_sequence,
                                path: read.path.clone(),
                                start_line: read.start_line,
                                end_line: read.end_line,
                                source_tokens,
                                full_content_eligible: true,
                            },
                            repository,
                            task_id,
                            relevant,
                            manifest,
                            line_counts,
                            metrics,
                        )?;
                    }
                } else if has_search {
                    metrics.source_tokens = metrics
                        .source_tokens
                        .checked_add(u64::try_from(manifest.tokenizer.count(output))?)
                        .ok_or("source-token overflow")?;
                }
            }
            _ => {}
        }
        event_sequence = event_sequence.saturating_add(1);
    }
    Ok(())
}

fn classify_hash_and_generation(
    tool: &str,
    item: &Value,
    metrics: &mut Metrics,
) -> Result<(), DynError> {
    let arguments = &item["arguments"];
    let expected_hash = arguments["expected_hash"].as_str();
    let known_hashes = arguments["known_hashes"]
        .as_array()
        .map(|values| {
            values
                .iter()
                .filter_map(Value::as_str)
                .collect::<BTreeSet<_>>()
        })
        .unwrap_or_default();
    metrics.known_hash_inputs += known_hashes.len() + usize::from(expected_hash.is_some());
    let structured = item
        .pointer("/result/structured_content")
        .or_else(|| item.pointer("/result/structuredContent"));
    let retryable = structured.and_then(|value| value["status"].as_str()) == Some("retryable");
    let search_empty = tool == "leantoken_search"
        && structured
            .and_then(|value| value["hits"].as_array())
            .is_some_and(Vec::is_empty);
    let failed = item["status"].as_str() != Some("completed") || retryable || search_empty;
    observe_discovery_outcome(metrics, failed);
    metrics.retryable_results += usize::from(retryable);
    if tool == "leantoken_search" && failed {
        metrics.failed_searches += 1;
    }
    let Some(structured) = structured else {
        return Ok(());
    };
    if structured["status"].as_str() == Some("not_modified") {
        metrics.known_hash_reuses += 1;
    }
    if structured["index_stale"].as_bool() == Some(true) {
        metrics.stale_index_results += 1;
    }
    if let Some(hashes) = structured
        .pointer("/receipt/fragment_hashes")
        .and_then(Value::as_array)
    {
        metrics.known_hash_resends += hashes
            .iter()
            .filter_map(Value::as_str)
            .filter(|hash| known_hashes.contains(hash))
            .count();
    }
    let generation = structured
        .pointer("/meta/repository_generation")
        .and_then(Value::as_u64);
    if let Some(generation) = generation
        && metrics.generations.last().copied() != Some(generation)
    {
        if !metrics.generations.is_empty() {
            metrics.repository_generation_changes += 1;
        }
        metrics.generations.push(generation);
    }
    Ok(())
}

fn observe_discovery_outcome(metrics: &mut Metrics, failed: bool) {
    if failed {
        metrics.failed_discovery_calls += 1;
        metrics.pending_failed_discovery = true;
    } else if metrics.pending_failed_discovery {
        metrics.failed_discovery_recoveries += 1;
        metrics.pending_failed_discovery = false;
    }
}

#[allow(clippy::too_many_arguments)]
fn observe_range(
    range: ObservedRange,
    repository: &Path,
    task_id: &str,
    relevant: &BTreeSet<String>,
    manifest: &Manifest,
    line_counts: &mut HashMap<(String, String), usize>,
    metrics: &mut Metrics,
) -> Result<(), DynError> {
    if range.start_line == 0 || range.end_line < range.start_line {
        return Err("trajectory range is invalid".into());
    }
    let lines = range.end_line - range.start_line + 1;
    advance_range_call(metrics, range.call_sequence);
    let file_lines = file_line_count(repository, task_id, &range.path, line_counts)?;
    let whole =
        range.full_content_eligible && range.start_line == 1 && range.end_line >= file_lines;
    let broad = range.full_content_eligible
        && (lines >= manifest.broad_read_min_lines
            || (lines >= 200
                && lines as f64 / file_lines.max(1) as f64 >= manifest.broad_read_min_fraction));
    if whole {
        metrics.whole_file_reads += 1;
        metrics.whole_file_read_tokens += range.source_tokens;
    }
    if broad {
        metrics.broad_reads += 1;
        metrics.broad_read_tokens += range.source_tokens;
    }
    if !relevant.contains(&range.path) {
        metrics.dead_end_reads += 1;
        metrics.dead_end_source_tokens += range.source_tokens;
        metrics.active_call_has_dead_end = true;
    } else {
        metrics.active_call_has_relevant = true;
        if metrics.pending_dead_end {
            metrics.dead_end_recoveries += 1;
            metrics.pending_dead_end = false;
        }
    }
    let previous = metrics
        .ranges
        .iter()
        .filter(|seen| seen.call_sequence < range.call_sequence && seen.path == range.path)
        .collect::<Vec<_>>();
    if previous
        .iter()
        .any(|seen| seen.start_line == range.start_line && seen.end_line == range.end_line)
    {
        metrics.exact_rereads += 1;
        metrics.exact_reread_tokens += range.source_tokens;
    } else if previous
        .iter()
        .any(|seen| seen.start_line <= range.end_line && range.start_line <= seen.end_line)
    {
        metrics.overlap_rereads += 1;
        metrics.overlap_reread_tokens += range.source_tokens;
    }
    metrics.ranges.push(range);
    Ok(())
}

fn advance_range_call(metrics: &mut Metrics, sequence: usize) {
    if metrics.active_range_call == Some(sequence) {
        return;
    }
    if metrics.active_range_call.is_some()
        && metrics.active_call_has_dead_end
        && !metrics.active_call_has_relevant
    {
        metrics.pending_dead_end = true;
    }
    metrics.active_range_call = Some(sequence);
    metrics.active_call_has_dead_end = false;
    metrics.active_call_has_relevant = false;
}

fn native_reads(
    command: &str,
    repository: &Path,
    task_id: &str,
    line_counts: &mut HashMap<(String, String), usize>,
) -> Result<Vec<NativeRead>, DynError> {
    static SED: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r#"sed\s+-n\s+['\"]?(\d+),(\d+)p['\"]?\s+([^\s;&|]+)"#).unwrap()
    });
    static HEAD: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"head\s+-n\s+(\d+)\s+([^\s;&|]+)").unwrap());
    static TAIL: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"tail\s+-n\s+(\d+)\s+([^\s;&|]+)").unwrap());
    static CAT: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?:^|[;&|]\s*)cat\s+([^\s;&|]+)").unwrap());
    let mut reads = Vec::new();
    for captures in SED.captures_iter(command) {
        let Some(path) = normalize_command_path(&captures[3], repository) else {
            continue;
        };
        reads.push(NativeRead {
            path,
            start_line: captures[1].parse()?,
            end_line: captures[2].parse()?,
        });
    }
    for captures in HEAD.captures_iter(command) {
        let Some(path) = normalize_command_path(&captures[2], repository) else {
            continue;
        };
        reads.push(NativeRead {
            path,
            start_line: 1,
            end_line: captures[1].parse()?,
        });
    }
    for captures in TAIL.captures_iter(command) {
        let Some(path) = normalize_command_path(&captures[2], repository) else {
            continue;
        };
        let file_lines = file_line_count(repository, task_id, &path, line_counts)?;
        let count = captures[1].parse::<usize>()?;
        reads.push(NativeRead {
            path,
            start_line: file_lines.saturating_sub(count).saturating_add(1),
            end_line: file_lines,
        });
    }
    for captures in CAT.captures_iter(command) {
        let Some(path) = normalize_command_path(&captures[1], repository) else {
            continue;
        };
        let file_lines = file_line_count(repository, task_id, &path, line_counts)?;
        reads.push(NativeRead {
            path,
            start_line: 1,
            end_line: file_lines,
        });
    }
    Ok(reads)
}

fn mcp_discovery_kind(tool: &str) -> Option<DiscoveryKind> {
    match tool {
        "leantoken_files" => Some(DiscoveryKind::Files),
        "leantoken_outline" => Some(DiscoveryKind::Outline),
        "leantoken_search" => Some(DiscoveryKind::Search),
        "leantoken_read" => Some(DiscoveryKind::Read),
        "leantoken_context" => Some(DiscoveryKind::Context),
        _ => None,
    }
}

fn mcp_tools_by_call_id(trajectory: &Trajectory) -> Result<HashMap<String, String>, DynError> {
    let mut tools = HashMap::new();
    for event in &trajectory.events {
        if event["type"].as_str() != Some("item.completed")
            || event.pointer("/item/type").and_then(Value::as_str) != Some("mcp_tool_call")
        {
            continue;
        }
        let id = event
            .pointer("/item/id")
            .and_then(Value::as_str)
            .ok_or("completed MCP trajectory item has no ID")?;
        let tool = event
            .pointer("/item/tool")
            .and_then(Value::as_str)
            .ok_or("completed MCP trajectory item has no tool")?;
        if tools.insert(id.to_owned(), tool.to_owned()).is_some() {
            return Err("trajectory contains a duplicate MCP call ID".into());
        }
    }
    Ok(tools)
}

fn is_retrieval_event(event: &Value) -> bool {
    let item = &event["item"];
    if event["type"].as_str() != Some("item.completed") {
        return false;
    }
    match item["type"].as_str() {
        Some("mcp_tool_call") => {
            mcp_discovery_kind(item["tool"].as_str().unwrap_or_default()).is_some()
        }
        Some("command_execution") => item["command"]
            .as_str()
            .is_some_and(|command| is_native_search(command) || is_native_read(command)),
        _ => false,
    }
}

fn is_native_search(command: &str) -> bool {
    static SEARCH: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?:^|[\s;&|])(rg|grep|find|fd|fdfind|ls|tree)(?:[\s;&|]|$)").unwrap()
    });
    SEARCH.is_match(command)
        || command.contains("git grep")
        || command.contains("git ls-files")
        || command.contains("git log")
        || command.contains("git show")
}

fn is_native_read(command: &str) -> bool {
    static READ: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?:^|[\s;&|])(cat|bat|sed|head|tail|awk|nl|less|more)(?:[\s;&|]|$)").unwrap()
    });
    READ.is_match(command) || command.contains("git show") || command.contains("git blame")
}

fn aggregate(runs: &[RunClassification]) -> BTreeMap<String, ArmSummary> {
    let mut grouped = BTreeMap::<String, Vec<&RunClassification>>::new();
    for run in runs {
        grouped.entry(run.arm.clone()).or_default().push(run);
    }
    grouped
        .into_iter()
        .map(|(arm, runs)| (arm, summarize(&runs)))
        .collect()
}

fn aggregate_by_task(runs: &[RunClassification]) -> BTreeMap<String, BTreeMap<String, ArmSummary>> {
    let mut grouped = BTreeMap::<String, BTreeMap<String, Vec<&RunClassification>>>::new();
    for run in runs {
        grouped
            .entry(run.task_id.clone())
            .or_default()
            .entry(run.arm.clone())
            .or_default()
            .push(run);
    }
    grouped
        .into_iter()
        .map(|(task, arms)| {
            (
                task,
                arms.into_iter()
                    .map(|(arm, runs)| (arm, summarize(&runs)))
                    .collect(),
            )
        })
        .collect()
}

fn summarize(runs: &[&RunClassification]) -> ArmSummary {
    let classified = runs
        .iter()
        .copied()
        .filter(|run| run.evidence_scope != EvidenceScope::Unavailable)
        .collect::<Vec<_>>();
    let successes = runs.iter().filter(|run| run.official_success).count();
    ArmSummary {
        runs: runs.len(),
        classified_runs: classified.len(),
        successes,
        success_rate: successes as f64 / runs.len().max(1) as f64,
        retrieval_calls: distribution(&classified, |run| run.retrieval_calls as f64),
        source_tokens: distribution(&classified, |run| run.source_tokens as f64),
        whole_file_reads: distribution(&classified, |run| run.whole_file_reads as f64),
        broad_reads: distribution(&classified, |run| run.broad_reads as f64),
        exact_rereads: distribution(&classified, |run| run.exact_rereads as f64),
        overlap_rereads: distribution(&classified, |run| run.overlap_rereads as f64),
        failed_searches: distribution(&classified, |run| run.failed_searches as f64),
        failed_discovery_calls: distribution(&classified, |run| run.failed_discovery_calls as f64),
        failed_discovery_recoveries: distribution(&classified, |run| {
            run.failed_discovery_recoveries as f64
        }),
        unrecovered_failed_discovery_runs: classified
            .iter()
            .filter(|run| run.unrecovered_failed_discovery)
            .count(),
        retryable_results: distribution(&classified, |run| run.retryable_results as f64),
        dead_end_source_tokens: distribution(&classified, |run| run.dead_end_source_tokens as f64),
        dead_end_recoveries: distribution(&classified, |run| run.dead_end_recoveries as f64),
        duration_ms: distribution(runs, |run| run.duration_ms as f64),
        known_hash_inputs: classified.iter().map(|run| run.known_hash_inputs).sum(),
        known_hash_reuses: classified.iter().map(|run| run.known_hash_reuses).sum(),
        known_hash_resends: classified.iter().map(|run| run.known_hash_resends).sum(),
        repository_generation_changes: classified
            .iter()
            .map(|run| run.repository_generation_changes)
            .sum(),
        stale_index_results: classified.iter().map(|run| run.stale_index_results).sum(),
        handoff_artifacts: classified
            .iter()
            .filter(|run| run.handoff.is_some())
            .count(),
        observed_executor_handoffs: classified
            .iter()
            .filter_map(|run| run.handoff.as_ref())
            .filter(|handoff| handoff.executor_retrieval_calls.is_some())
            .count(),
        executor_retrieval_calls: classified
            .iter()
            .filter_map(|run| run.handoff.as_ref())
            .filter_map(|handoff| handoff.executor_retrieval_calls)
            .sum(),
    }
}

fn distribution<T>(values: &[T], project: impl Fn(&T) -> f64) -> Distribution {
    let mut samples = values.iter().map(project).collect::<Vec<_>>();
    samples.sort_by(f64::total_cmp);
    let count = samples.len();
    if count == 0 {
        return Distribution::default();
    }
    let mean = samples.iter().sum::<f64>() / count as f64;
    let median = if count % 2 == 1 {
        samples[count / 2]
    } else {
        (samples[count / 2 - 1] + samples[count / 2]) / 2.0
    };
    let variance = (count > 1).then(|| {
        samples
            .iter()
            .map(|sample| (sample - mean).powi(2))
            .sum::<f64>()
            / (count - 1) as f64
    });
    Distribution {
        samples: count,
        minimum: samples.first().copied(),
        median: Some(median),
        mean: Some(mean),
        maximum: samples.last().copied(),
        sample_variance: variance,
    }
}

fn dataset_labels(
    bytes: &[u8],
    task_ids: &BTreeSet<String>,
) -> Result<BTreeMap<String, BTreeSet<String>>, DynError> {
    let mut labels = BTreeMap::new();
    for line in std::str::from_utf8(bytes)?.lines() {
        let record: DatasetRecord = serde_json::from_str(line)?;
        if !task_ids.contains(&record.instance_id) {
            continue;
        }
        let mut paths = patch_paths(&record.patch)?;
        paths.extend(patch_paths(&record.test_patch)?);
        if paths.is_empty() || labels.insert(record.instance_id, paths).is_some() {
            return Err("dataset task labels are empty or duplicated".into());
        }
    }
    if labels.len() != task_ids.len() {
        return Err("dataset is missing a frozen model A/B task".into());
    }
    Ok(labels)
}

fn patch_paths(patch: &str) -> Result<BTreeSet<String>, DynError> {
    let mut patch_set = PatchSet::new();
    patch_set.parse(patch)?;
    patch_set
        .files()
        .iter()
        .map(|file| normalize_path(&file.path()))
        .collect()
}

fn verify_repository(repository: &Path, revision: &str) -> Result<(), DynError> {
    let revision_output = std::process::Command::new("git")
        .args(["-C", &repository.to_string_lossy(), "rev-parse", "HEAD"])
        .output()?;
    let status_output = std::process::Command::new("git")
        .args([
            "-C",
            &repository.to_string_lossy(),
            "status",
            "--porcelain",
            "--untracked-files=no",
        ])
        .output()?;
    if !revision_output.status.success()
        || std::str::from_utf8(&revision_output.stdout)?.trim() != revision
        || !status_output.status.success()
        || !status_output.stdout.is_empty()
    {
        return Err(format!(
            "frozen repository revision mismatch: {}",
            repository.display()
        )
        .into());
    }
    Ok(())
}

fn file_line_count(
    repository: &Path,
    task_id: &str,
    path: &str,
    cache: &mut HashMap<(String, String), usize>,
) -> Result<usize, DynError> {
    let key = (task_id.to_owned(), path.to_owned());
    if let Some(lines) = cache.get(&key) {
        return Ok(*lines);
    }
    let source = fs::read_to_string(repository.join(path))?;
    let lines = source.lines().count().max(1);
    cache.insert(key, lines);
    Ok(lines)
}

fn normalize_command_path(path: &str, repository: &Path) -> Option<String> {
    let path = path.trim_matches(|character| matches!(character, '\'' | '"'));
    let path = Path::new(path);
    let relative = if path.is_absolute() {
        path.strip_prefix(repository).ok()?
    } else {
        path
    };
    let normalized = normalize_path(relative.to_str()?).ok()?;
    repository.join(&normalized).is_file().then_some(normalized)
}

fn normalize_path(path: &str) -> Result<String, DynError> {
    let path = path.strip_prefix("a/").unwrap_or(path);
    let path = path.strip_prefix("./").unwrap_or(path);
    let parsed = Path::new(path);
    if path.is_empty()
        || path.starts_with('/')
        || path.starts_with('\\')
        || parsed.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return Err(format!("invalid repository-relative trajectory path: {path}").into());
    }
    Ok(path.replace('\\', "/"))
}

fn validate_binding(
    binding: &RunBinding,
    run: &RawRun,
    experiment_id: &str,
    model_ab_manifest_blake3: &str,
) -> Result<(), DynError> {
    if binding.manifest_blake3 != model_ab_manifest_blake3
        || binding.experiment_id != experiment_id
        || binding.task_id != run.task_id
        || binding.repetition != run.repetition
        || binding.arm != run.arm
    {
        return Err("trajectory artifact binding mismatch".into());
    }
    Ok(())
}

fn validate_artifact_schema(schema_version: u32) -> Result<(), DynError> {
    if schema_version != ARTIFACT_SCHEMA_V1 {
        return Err("unsupported trajectory artifact schema".into());
    }
    Ok(())
}

fn validate_handoff(handoff: &PrewalkHandoff, trace: &ToolTrace) -> Result<(), DynError> {
    let edit = trace
        .calls
        .get(handoff.first_validated_edit.edit_sequence)
        .ok_or("handoff edit sequence is outside the exact trace")?;
    let validation = trace
        .calls
        .get(handoff.first_validated_edit.validation_sequence)
        .ok_or("handoff validation sequence is outside the exact trace")?;
    if handoff.evidence_calls.is_empty()
        || handoff.first_validated_edit.edit_sequence
            >= handoff.first_validated_edit.validation_sequence
        || edit.tool_name != "edit"
        || edit.outcome != ToolOutcome::Success
        || validation.tool_name != "shell"
        || validation.outcome != ToolOutcome::Success
    {
        return Err("handoff does not identify an ordered validated edit".into());
    }
    for evidence in &handoff.evidence_calls {
        let exact = trace
            .calls
            .get(evidence.sequence)
            .ok_or("handoff evidence sequence is outside the exact trace")?;
        if exact.call_id != evidence.call_id
            || exact.result_id != evidence.result_id
            || exact.result_source_tokens != evidence.result_source_tokens
            || exact.tool_name != evidence.tool_name
            || exact.outcome != evidence.outcome
            || exact.ranges.len() != evidence.ranges.len()
        {
            return Err("handoff evidence differs from the exact trace".into());
        }
    }
    Ok(())
}

fn validate_standalone_handoff(handoff: &PrewalkHandoff) -> Result<(), DynError> {
    let calls = handoff
        .trajectory_events
        .iter()
        .filter(|event| {
            event["type"].as_str() == Some("item.completed")
                && matches!(
                    event.pointer("/item/type").and_then(Value::as_str),
                    Some("command_execution" | "file_change" | "mcp_tool_call")
                )
        })
        .collect::<Vec<_>>();
    let edit = calls
        .get(handoff.first_validated_edit.edit_sequence)
        .ok_or("standalone handoff edit sequence is outside its trajectory")?;
    let validation = calls
        .get(handoff.first_validated_edit.validation_sequence)
        .ok_or("standalone handoff validation sequence is outside its trajectory")?;
    if handoff.evidence_calls.is_empty()
        || handoff.first_validated_edit.edit_sequence
            >= handoff.first_validated_edit.validation_sequence
        || edit.pointer("/item/type").and_then(Value::as_str) != Some("file_change")
        || edit.pointer("/item/status").and_then(Value::as_str) != Some("completed")
        || validation.pointer("/item/type").and_then(Value::as_str) != Some("command_execution")
        || validation.pointer("/item/status").and_then(Value::as_str) != Some("completed")
        || validation
            .pointer("/item/exit_code")
            .and_then(Value::as_i64)
            != Some(0)
    {
        return Err("standalone handoff does not identify an ordered validated edit".into());
    }
    for evidence in &handoff.evidence_calls {
        let event = calls
            .get(evidence.sequence)
            .ok_or("standalone handoff evidence sequence is outside its trajectory")?;
        let event_id = event
            .pointer("/item/id")
            .and_then(Value::as_str)
            .ok_or("standalone handoff evidence has no trajectory ID")?;
        if evidence.call_id.strip_prefix("prewalk:") != Some(event_id)
            || event.pointer("/item/type").and_then(Value::as_str) != Some("mcp_tool_call")
        {
            return Err("standalone handoff evidence differs from its trajectory".into());
        }
    }
    Ok(())
}

fn summarize_handoff(
    handoff: &PrewalkHandoff,
    executor_retrieval_calls: Option<usize>,
) -> HandoffClassification {
    HandoffClassification {
        transferred_evidence_calls: handoff.evidence_calls.len(),
        transferred_source_tokens: handoff
            .evidence_calls
            .iter()
            .map(|call| call.result_source_tokens)
            .sum(),
        transferred_ranges: handoff
            .evidence_calls
            .iter()
            .map(|call| call.ranges.len())
            .sum(),
        todo_events: handoff.todo_events.len(),
        executor_retrieval_calls,
        first_validated_edit_sequence: handoff.first_validated_edit.edit_sequence,
        validation_sequence: handoff.first_validated_edit.validation_sequence,
    }
}

fn read_artifact<T: for<'de> Deserialize<'de>>(
    path: &Path,
    identity: &ArtifactIdentity,
    verified: &mut usize,
) -> Result<T, DynError> {
    let bytes = read_verified(path, &identity.blake3)?;
    *verified += 1;
    Ok(serde_json::from_slice(&bytes)?)
}

fn read_verified(path: &Path, expected: &str) -> Result<Vec<u8>, DynError> {
    validate_hash(expected)?;
    let bytes = fs::read(path)?;
    if hash_bytes(&bytes) != expected {
        return Err(format!("artifact hash mismatch: {}", path.display()).into());
    }
    Ok(bytes)
}

fn hash_file(path: &Path) -> Result<String, DynError> {
    Ok(hash_bytes(&fs::read(path)?))
}

fn hash_bytes(bytes: &[u8]) -> String {
    blake3::hash(bytes).to_hex().to_string()
}

fn validate_hash(value: &str) -> Result<(), DynError> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err("expected a lowercase 64-character BLAKE3 identity".into());
    }
    Ok(())
}

fn resolve(root: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_owned()
    } else {
        root.join(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_manifest() -> Manifest {
        Manifest {
            schema_version: 1,
            experiment_id: "experiment".to_owned(),
            raw_report: PathBuf::from("report"),
            raw_report_blake3: "0".repeat(64),
            artifacts_root: PathBuf::from("artifacts"),
            dataset: PathBuf::from("dataset"),
            dataset_blake3: "0".repeat(64),
            repositories: BTreeMap::new(),
            tokenizer: Tokenizer::O200kBase,
            broad_read_min_lines: 400,
            broad_read_min_fraction: 0.5,
            baseline_arm: "filesystem".to_owned(),
            candidate_arm: "progressive".to_owned(),
            handoff_arm: "prewalk".to_owned(),
        }
    }

    #[test]
    fn native_read_parser_extracts_ranges_and_whole_files() {
        let repository = tempfile::tempdir().expect("repository");
        fs::write(repository.path().join("owner.rs"), "one\ntwo\nthree\n").expect("owner");
        let mut cache = HashMap::new();
        let reads = native_reads(
            "sed -n '1,2p' owner.rs && cat owner.rs",
            repository.path(),
            "task",
            &mut cache,
        )
        .expect("native reads");

        assert_eq!(reads.len(), 2);
        assert_eq!((reads[0].start_line, reads[0].end_line), (1, 2));
        assert_eq!(reads[1].start_line, 1);
        assert_eq!(reads[1].end_line, 3);
    }

    #[test]
    fn native_read_parser_ignores_files_outside_repository() {
        let repository = tempfile::tempdir().expect("repository");
        let external = tempfile::NamedTempFile::new().expect("external");
        let owner = repository.path().join("owner.rs");
        fs::write(&owner, "source\n").expect("owner");
        let mut cache = HashMap::new();
        let reads = native_reads(
            &format!(
                "cat {} && cat {}",
                external.path().display(),
                owner.display()
            ),
            repository.path(),
            "task",
            &mut cache,
        )
        .expect("native reads");

        assert_eq!(reads.len(), 1);
        assert_eq!(reads[0].path, "owner.rs");
    }

    #[test]
    fn range_classifier_separates_exact_overlap_broad_and_dead_end() {
        let repository = tempfile::tempdir().expect("repository");
        fs::write(repository.path().join("owner.rs"), "line\n".repeat(500)).expect("owner");
        let manifest = test_manifest();
        let mut metrics = Metrics::default();
        let mut cache = HashMap::new();
        let relevant = BTreeSet::new();
        for (sequence, start, end, tokens) in
            [(1, 1, 450, 100), (2, 1, 450, 100), (3, 400, 500, 50)]
        {
            observe_range(
                ObservedRange {
                    call_sequence: sequence,
                    path: "owner.rs".to_owned(),
                    start_line: start,
                    end_line: end,
                    source_tokens: tokens,
                    full_content_eligible: true,
                },
                repository.path(),
                "task",
                &relevant,
                &manifest,
                &mut cache,
                &mut metrics,
            )
            .expect("range");
        }

        assert_eq!(metrics.broad_reads, 2);
        assert_eq!(metrics.exact_rereads, 1);
        assert_eq!(metrics.overlap_rereads, 1);
        assert_eq!(metrics.dead_end_reads, 3);
        assert_eq!(metrics.dead_end_source_tokens, 250);
    }

    #[test]
    fn recovery_state_uses_discovery_episodes_and_range_call_boundaries() {
        let repository = tempfile::tempdir().expect("repository");
        fs::write(repository.path().join("dead.rs"), "dead\n").expect("dead");
        fs::write(repository.path().join("target.rs"), "target\n").expect("target");
        let mut metrics = Metrics::default();
        observe_discovery_outcome(&mut metrics, true);
        observe_discovery_outcome(&mut metrics, true);
        observe_discovery_outcome(&mut metrics, false);
        assert_eq!(metrics.failed_discovery_calls, 2);
        assert_eq!(metrics.failed_discovery_recoveries, 1);
        assert!(!metrics.pending_failed_discovery);

        let relevant = BTreeSet::from(["target.rs".to_owned()]);
        let mut cache = HashMap::new();
        for (sequence, path) in [
            (1, "dead.rs"),
            (2, "target.rs"),
            (3, "dead.rs"),
            (3, "target.rs"),
        ] {
            observe_range(
                ObservedRange {
                    call_sequence: sequence,
                    path: path.to_owned(),
                    start_line: 1,
                    end_line: 1,
                    source_tokens: 1,
                    full_content_eligible: true,
                },
                repository.path(),
                "task",
                &relevant,
                &test_manifest(),
                &mut cache,
                &mut metrics,
            )
            .expect("range");
        }
        assert_eq!(metrics.dead_end_recoveries, 1);
    }

    #[test]
    fn distribution_uses_sample_variance() {
        let values = [1.0, 2.0, 3.0, 4.0];
        let result = distribution(&values, |value| *value);

        assert_eq!(result.median, Some(2.5));
        assert_eq!(result.mean, Some(2.5));
        assert_eq!(result.sample_variance, Some(5.0 / 3.0));
    }
}
