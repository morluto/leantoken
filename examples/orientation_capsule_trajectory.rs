#[allow(dead_code)]
#[path = "support/model_ab_artifacts.rs"]
mod model_ab_artifacts;

use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};

use clap::Parser;
use leantoken::tokens::Tokenizer;
use model_ab_artifacts::{
    OrientationCapsule, PrewalkHandoff, RunBinding, ToolTrace, Trajectory,
    orientation_capsule_prompt, validate_orientation_capsule,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

type DynError = Box<dyn Error>;

const REPORT_SCHEMA_V1: u32 = 1;
const BASELINE_ARM: &str = "prewalk";
const CANDIDATE_ARM: &str = "prewalk_capsule";
const CLASSIFIER_SOURCE: &[u8] = include_bytes!("orientation_capsule_trajectory.rs");

#[derive(Debug, Parser)]
#[command(about = "Classify bounded orientation-capsule model A/B trajectories")]
struct Args {
    /// Raw model_ab JSON report.
    #[arg(long)]
    report: PathBuf,
    /// Root containing the report's immutable per-run artifacts.
    #[arg(long)]
    artifacts_dir: PathBuf,
    /// Classifier report path.
    #[arg(long)]
    output: PathBuf,
}

#[derive(Debug, Deserialize)]
struct RawReport {
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
    orientation_capsule: Option<OrientationCapsule>,
    #[serde(default)]
    relevant_files: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct RawRun {
    task_id: String,
    repetition: usize,
    arm: String,
    status: String,
    duration_ms: u128,
    artifacts: RawArtifacts,
    result: Option<RawResult>,
    error: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RawArtifacts {
    tool_trace: Option<ArtifactIdentity>,
    trajectory: Option<ArtifactIdentity>,
    prewalk_handoff: Option<ArtifactIdentity>,
}

#[derive(Debug, Deserialize)]
struct ArtifactIdentity {
    bytes: u64,
    blake3: String,
}

#[derive(Debug, Deserialize)]
struct RawResult {
    task_success: bool,
    total_input_tokens: Option<u64>,
    total_output_tokens: Option<u64>,
}

#[derive(Debug, Clone, Serialize)]
struct RunClassification {
    task_id: String,
    repetition: usize,
    arm: String,
    status: String,
    official_success: bool,
    duration_ms: u128,
    retrieval_calls: usize,
    retrieval_source_tokens: u64,
    transferred_source_tokens: u64,
    transferred_ranges: usize,
    dead_end_source_tokens: u64,
    rereads: usize,
    reread_tokens: u64,
    executor_retrieval_calls: usize,
    owner_path_followed: bool,
    first_owner_evidence_sequence: Option<usize>,
    capsule_tokens: usize,
    capsule_prompt_tokens: usize,
    first_validated_edit_sequence: usize,
    validation_sequence: usize,
    total_input_tokens: Option<u64>,
    total_output_tokens: Option<u64>,
}

#[derive(Debug, Serialize)]
struct UnclassifiedRun {
    task_id: String,
    repetition: usize,
    arm: String,
    status: String,
    official_success: bool,
    duration_ms: u128,
    verified_artifacts: usize,
    missing_artifacts: Vec<&'static str>,
    error: Option<String>,
}

#[derive(Debug, Default, Serialize)]
struct ArmSummary {
    runs: usize,
    successes: usize,
    retrieval_calls: usize,
    retrieval_source_tokens: u64,
    transferred_source_tokens: u64,
    transferred_ranges: usize,
    dead_end_source_tokens: u64,
    rereads: usize,
    reread_tokens: u64,
    executor_retrieval_calls: usize,
    owner_path_followed_runs: usize,
    capsule_tokens: usize,
    capsule_prompt_tokens: usize,
    total_input_tokens: Option<u64>,
    total_output_tokens: Option<u64>,
}

#[derive(Debug, Serialize)]
struct Decision {
    result: &'static str,
    reason: String,
    paired_runs: usize,
    unclassified_runs: usize,
    success_regressions: usize,
    candidate_owner_misses: usize,
    retrieval_calls_saved: Option<i64>,
    retrieval_source_tokens_saved: Option<i64>,
    capsule_prompt_tokens: usize,
    net_tokens_saved_after_prompt: Option<i64>,
    dead_end_source_token_delta: Option<i64>,
    reread_token_delta: Option<i64>,
    production_change_authorized: bool,
}

#[derive(Debug, Serialize)]
struct Report {
    schema_version: u32,
    report_kind: &'static str,
    experiment_id: String,
    raw_report_blake3: String,
    classifier_source_blake3: String,
    classifier_binary_blake3: String,
    primary_model: String,
    executor_model: String,
    tokenizer: &'static str,
    tasks: usize,
    repetitions: usize,
    verified_artifacts: usize,
    arms: BTreeMap<String, ArmSummary>,
    runs: Vec<RunClassification>,
    unclassified_runs: Vec<UnclassifiedRun>,
    decision: Decision,
    limitations: Vec<&'static str>,
}

fn main() -> Result<(), DynError> {
    let args = Args::parse();
    let report_bytes = fs::read(&args.report)?;
    let classifier_binary = fs::read(std::env::current_exe()?)?;
    let raw: RawReport = serde_json::from_slice(&report_bytes)?;
    let task_map = validate_report(&raw)?;
    let mut verified_artifacts = 0usize;
    let mut runs = Vec::new();
    let mut unclassified_runs = Vec::new();
    for run in raw
        .runs
        .iter()
        .filter(|run| matches!(run.arm.as_str(), BASELINE_ARM | CANDIDATE_ARM))
    {
        let task = task_map
            .get(run.task_id.as_str())
            .ok_or("run references an unknown task")?;
        if run.artifacts.tool_trace.is_some()
            && run.artifacts.trajectory.is_some()
            && run.artifacts.prewalk_handoff.is_some()
        {
            runs.push(classify_run(
                &raw,
                run,
                task,
                &args.artifacts_dir,
                &mut verified_artifacts,
            )?);
        } else {
            unclassified_runs.push(verify_partial_run(
                &raw,
                run,
                task,
                &args.artifacts_dir,
                &mut verified_artifacts,
            )?);
        }
    }
    let arms = summarize_arms(&runs);
    let decision = decide(&raw.runs, &runs, &task_map)?;
    let report = Report {
        schema_version: REPORT_SCHEMA_V1,
        report_kind: "orientation_capsule_trajectory_ab",
        experiment_id: raw.experiment_id,
        raw_report_blake3: hash_bytes(&report_bytes),
        classifier_source_blake3: hash_bytes(CLASSIFIER_SOURCE),
        classifier_binary_blake3: hash_bytes(&classifier_binary),
        primary_model: raw.primary_model,
        executor_model: raw.executor_model,
        tokenizer: Tokenizer::Cl100kBase.name(),
        tasks: raw.task_definitions.len(),
        repetitions: raw.repetitions,
        verified_artifacts,
        arms,
        runs,
        unclassified_runs,
        decision,
        limitations: vec![
            "This small local experiment is directional evidence, not a production rollout authorization.",
            "Capsule prompt tokens include the complete fixed instruction and JSON wrapper; retrieval source tokens are LeanToken source accounting, not provider billing tokens.",
            "Dead-end source counts only retrieval calls with explicit ranges whose paths are all outside the frozen relevant-file labels.",
            "Runs missing a trace, trajectory, or handoff remain explicit unclassified failures; partial artifact identities are still verified, token deltas become null, and no positive decision is possible.",
            "Provider and model nondeterminism remain even with alternating deterministic arm order.",
        ],
    };
    if let Some(parent) = args
        .output
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
    {
        fs::create_dir_all(parent)?;
    }
    let mut output = serde_json::to_vec_pretty(&report)?;
    output.push(b'\n');
    fs::write(args.output, output)?;
    Ok(())
}

fn validate_report(raw: &RawReport) -> Result<BTreeMap<&str, &TaskDefinition>, DynError> {
    if raw.experiment_id.trim().is_empty()
        || raw.manifest_blake3.len() != 64
        || raw.repetitions == 0
        || raw.task_definitions.is_empty()
    {
        return Err("raw orientation model A/B report is incomplete".into());
    }
    let mut tasks = BTreeMap::new();
    for task in &raw.task_definitions {
        let capsule = task
            .orientation_capsule
            .as_ref()
            .ok_or("orientation task has no frozen capsule")?;
        validate_orientation_capsule(capsule, Tokenizer::Cl100kBase)?;
        if task.relevant_files.is_empty()
            || capsule
                .entries
                .iter()
                .any(|entry| !task.relevant_files.contains(&entry.path))
            || tasks.insert(task.id.as_str(), task).is_some()
        {
            return Err("orientation task labels are empty, inconsistent, or duplicated".into());
        }
    }
    Ok(tasks)
}

fn classify_run(
    raw: &RawReport,
    run: &RawRun,
    task: &TaskDefinition,
    artifacts_root: &Path,
    verified_artifacts: &mut usize,
) -> Result<RunClassification, DynError> {
    let directory = artifacts_root
        .join(&raw.experiment_id)
        .join(&run.task_id)
        .join(format!("repetition-{}", run.repetition))
        .join(&run.arm);
    let trace: ToolTrace = read_artifact(
        &directory.join("tool-trace.json"),
        run.artifacts
            .tool_trace
            .as_ref()
            .ok_or("run has no tool trace identity")?,
        verified_artifacts,
    )?;
    let trajectory: Trajectory = read_artifact(
        &directory.join("trajectory.json"),
        run.artifacts
            .trajectory
            .as_ref()
            .ok_or("run has no trajectory identity")?,
        verified_artifacts,
    )?;
    let handoff: PrewalkHandoff = read_artifact(
        &directory.join("prewalk-handoff.json"),
        run.artifacts
            .prewalk_handoff
            .as_ref()
            .ok_or("orientation run has no prewalk handoff identity")?,
        verified_artifacts,
    )?;
    let expected = RunBinding {
        experiment_id: raw.experiment_id.clone(),
        manifest_blake3: raw.manifest_blake3.clone(),
        task_id: run.task_id.clone(),
        repetition: run.repetition,
        arm: run.arm.clone(),
    };
    for binding in [&trace.binding, &trajectory.binding, &handoff.binding] {
        if binding != &expected {
            return Err("orientation artifact run binding mismatch".into());
        }
    }
    let expected_capsule = (run.arm == CANDIDATE_ARM).then_some(
        task.orientation_capsule
            .as_ref()
            .expect("validated capsule"),
    );
    if handoff.orientation_capsule.as_ref() != expected_capsule {
        return Err("orientation handoff capsule differs from the frozen arm".into());
    }
    validate_handoff_trace(&handoff, &trace)?;

    let relevant = task.relevant_files.iter().collect::<BTreeSet<_>>();
    let owner_paths = task
        .orientation_capsule
        .as_ref()
        .expect("validated capsule")
        .entries
        .iter()
        .map(|entry| entry.path.as_str())
        .collect::<BTreeSet<_>>();
    let retrieval = trace
        .calls
        .iter()
        .filter(|call| call.tool_name == "leantoken")
        .collect::<Vec<_>>();
    let owner_sequences = handoff.evidence_calls.iter().filter_map(|call| {
        call.ranges
            .iter()
            .any(|range| owner_paths.contains(range.path.as_str()))
            .then_some(call.sequence)
    });
    let first_owner_evidence_sequence = owner_sequences.min();
    let dead_end_source_tokens = retrieval
        .iter()
        .filter(|call| {
            !call.ranges.is_empty()
                && call
                    .ranges
                    .iter()
                    .all(|range| !relevant.contains(&range.path))
        })
        .map(|call| call.result_source_tokens)
        .sum();
    let capsule_prompt_tokens = handoff
        .orientation_capsule
        .as_ref()
        .map(orientation_capsule_prompt)
        .transpose()?
        .map(|prompt| Tokenizer::Cl100kBase.count(&prompt))
        .unwrap_or(0);
    let result = run.result.as_ref();
    Ok(RunClassification {
        task_id: run.task_id.clone(),
        repetition: run.repetition,
        arm: run.arm.clone(),
        status: run.status.clone(),
        official_success: result.is_some_and(|result| result.task_success),
        duration_ms: run.duration_ms,
        retrieval_calls: retrieval.len(),
        retrieval_source_tokens: retrieval.iter().map(|call| call.result_source_tokens).sum(),
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
        dead_end_source_tokens,
        rereads: trace.calls.iter().filter(|call| call.reread).count(),
        reread_tokens: trace
            .calls
            .iter()
            .filter(|call| call.reread)
            .map(|call| call.result_source_tokens)
            .sum(),
        executor_retrieval_calls: executor_retrieval_calls(&trajectory),
        owner_path_followed: first_owner_evidence_sequence.is_some(),
        first_owner_evidence_sequence,
        capsule_tokens: handoff
            .orientation_capsule
            .as_ref()
            .map(|capsule| capsule.capsule_tokens)
            .unwrap_or(0),
        capsule_prompt_tokens,
        first_validated_edit_sequence: handoff.first_validated_edit.edit_sequence,
        validation_sequence: handoff.first_validated_edit.validation_sequence,
        total_input_tokens: result.and_then(|result| result.total_input_tokens),
        total_output_tokens: result.and_then(|result| result.total_output_tokens),
    })
}

fn verify_partial_run(
    raw: &RawReport,
    run: &RawRun,
    task: &TaskDefinition,
    artifacts_root: &Path,
    verified_artifacts: &mut usize,
) -> Result<UnclassifiedRun, DynError> {
    let directory = artifacts_root
        .join(&raw.experiment_id)
        .join(&run.task_id)
        .join(format!("repetition-{}", run.repetition))
        .join(&run.arm);
    let before = *verified_artifacts;
    let trace: Option<ToolTrace> = run
        .artifacts
        .tool_trace
        .as_ref()
        .map(|identity| {
            read_artifact(
                &directory.join("tool-trace.json"),
                identity,
                verified_artifacts,
            )
        })
        .transpose()?;
    let trajectory: Option<Trajectory> = run
        .artifacts
        .trajectory
        .as_ref()
        .map(|identity| {
            read_artifact(
                &directory.join("trajectory.json"),
                identity,
                verified_artifacts,
            )
        })
        .transpose()?;
    let handoff: Option<PrewalkHandoff> = run
        .artifacts
        .prewalk_handoff
        .as_ref()
        .map(|identity| {
            read_artifact(
                &directory.join("prewalk-handoff.json"),
                identity,
                verified_artifacts,
            )
        })
        .transpose()?;
    let expected = RunBinding {
        experiment_id: raw.experiment_id.clone(),
        manifest_blake3: raw.manifest_blake3.clone(),
        task_id: run.task_id.clone(),
        repetition: run.repetition,
        arm: run.arm.clone(),
    };
    for binding in [
        trace.as_ref().map(|artifact| &artifact.binding),
        trajectory.as_ref().map(|artifact| &artifact.binding),
        handoff.as_ref().map(|artifact| &artifact.binding),
    ]
    .into_iter()
    .flatten()
    {
        if binding != &expected {
            return Err("partial orientation artifact run binding mismatch".into());
        }
    }
    if let Some(handoff) = &handoff {
        let expected_capsule = (run.arm == CANDIDATE_ARM).then_some(
            task.orientation_capsule
                .as_ref()
                .expect("validated capsule"),
        );
        if handoff.orientation_capsule.as_ref() != expected_capsule {
            return Err("partial orientation handoff capsule differs from the frozen arm".into());
        }
        if let Some(trace) = &trace {
            validate_handoff_trace(handoff, trace)?;
        }
    }
    let mut missing_artifacts = Vec::new();
    if trace.is_none() {
        missing_artifacts.push("tool-trace.json");
    }
    if trajectory.is_none() {
        missing_artifacts.push("trajectory.json");
    }
    if handoff.is_none() {
        missing_artifacts.push("prewalk-handoff.json");
    }
    if missing_artifacts.is_empty() {
        return Err("complete run was routed to partial classification".into());
    }
    Ok(UnclassifiedRun {
        task_id: run.task_id.clone(),
        repetition: run.repetition,
        arm: run.arm.clone(),
        status: run.status.clone(),
        official_success: run
            .result
            .as_ref()
            .is_some_and(|result| result.task_success),
        duration_ms: run.duration_ms,
        verified_artifacts: *verified_artifacts - before,
        missing_artifacts,
        error: run.error.clone(),
    })
}

fn validate_handoff_trace(handoff: &PrewalkHandoff, trace: &ToolTrace) -> Result<(), DynError> {
    if handoff.evidence_calls.is_empty()
        || handoff.first_validated_edit.edit_sequence
            >= handoff.first_validated_edit.validation_sequence
    {
        return Err("orientation handoff has no ordered validated evidence".into());
    }
    for evidence in &handoff.evidence_calls {
        let exact = trace
            .calls
            .get(evidence.sequence)
            .ok_or("orientation evidence sequence is outside the trace")?;
        if exact.call_id != evidence.call_id
            || exact.result_id != evidence.result_id
            || exact.result_source_tokens != evidence.result_source_tokens
            || exact.ranges.len() != evidence.ranges.len()
        {
            return Err("orientation handoff evidence differs from the trace".into());
        }
    }
    Ok(())
}

fn executor_retrieval_calls(trajectory: &Trajectory) -> usize {
    let Some(boundary) = trajectory
        .events
        .iter()
        .position(|event| event["type"].as_str() == Some("leantoken.phase_boundary"))
    else {
        return 0;
    };
    trajectory.events[boundary + 1..]
        .iter()
        .filter(|event| is_retrieval_event(event))
        .count()
}

fn is_retrieval_event(event: &Value) -> bool {
    if event["type"].as_str() != Some("item.completed") {
        return false;
    }
    match event.pointer("/item/type").and_then(Value::as_str) {
        Some("mcp_tool_call") => true,
        Some("command_execution") => event
            .pointer("/item/command")
            .and_then(Value::as_str)
            .is_some_and(|command| {
                command.split_whitespace().next().is_some_and(|program| {
                    matches!(
                        program.rsplit('/').next().unwrap_or(program),
                        "rg" | "grep" | "find" | "ls" | "sed" | "cat" | "head" | "tail"
                    )
                })
            }),
        _ => false,
    }
}

fn summarize_arms(runs: &[RunClassification]) -> BTreeMap<String, ArmSummary> {
    let mut arms = BTreeMap::<String, ArmSummary>::new();
    for run in runs {
        let arm = arms.entry(run.arm.clone()).or_default();
        arm.runs += 1;
        arm.successes += usize::from(run.official_success);
        arm.retrieval_calls += run.retrieval_calls;
        arm.retrieval_source_tokens += run.retrieval_source_tokens;
        arm.transferred_source_tokens += run.transferred_source_tokens;
        arm.transferred_ranges += run.transferred_ranges;
        arm.dead_end_source_tokens += run.dead_end_source_tokens;
        arm.rereads += run.rereads;
        arm.reread_tokens += run.reread_tokens;
        arm.executor_retrieval_calls += run.executor_retrieval_calls;
        arm.owner_path_followed_runs += usize::from(run.owner_path_followed);
        arm.capsule_tokens += run.capsule_tokens;
        arm.capsule_prompt_tokens += run.capsule_prompt_tokens;
    }
    for (name, arm) in &mut arms {
        let arm_runs = runs.iter().filter(|run| run.arm == *name);
        arm.total_input_tokens = complete_sum(arm_runs.clone().map(|run| run.total_input_tokens));
        arm.total_output_tokens = complete_sum(arm_runs.map(|run| run.total_output_tokens));
    }
    arms
}

fn decide(
    raw_runs: &[RawRun],
    runs: &[RunClassification],
    tasks: &BTreeMap<&str, &TaskDefinition>,
) -> Result<Decision, DynError> {
    let mut pairs = BTreeMap::<(String, usize), BTreeMap<String, &RawRun>>::new();
    for run in raw_runs
        .iter()
        .filter(|run| matches!(run.arm.as_str(), BASELINE_ARM | CANDIDATE_ARM))
    {
        if pairs
            .entry((run.task_id.clone(), run.repetition))
            .or_default()
            .insert(run.arm.clone(), run)
            .is_some()
        {
            return Err("paired experiment contains a duplicate run".into());
        }
    }
    let mut baselines = Vec::new();
    let mut candidates = Vec::new();
    for arms in pairs.values() {
        baselines.push(
            *arms
                .get(BASELINE_ARM)
                .ok_or("paired experiment is missing a baseline run")?,
        );
        candidates.push(
            *arms
                .get(CANDIDATE_ARM)
                .ok_or("paired experiment is missing a capsule run")?,
        );
    }
    let official_success = |run: &RawRun| {
        run.result
            .as_ref()
            .is_some_and(|result| result.task_success)
    };
    let success_regressions = baselines
        .iter()
        .zip(&candidates)
        .filter(|(baseline, candidate)| official_success(baseline) && !official_success(candidate))
        .count();
    let classified = runs
        .iter()
        .map(|run| {
            (
                (run.task_id.as_str(), run.repetition, run.arm.as_str()),
                run,
            )
        })
        .collect::<BTreeMap<_, _>>();
    let unclassified_runs = baselines.len() * 2 - runs.len();
    let candidate_owner_misses = candidates
        .iter()
        .filter(|candidate| {
            !classified
                .get(&(
                    candidate.task_id.as_str(),
                    candidate.repetition,
                    CANDIDATE_ARM,
                ))
                .is_some_and(|run| run.owner_path_followed)
        })
        .count();
    let capsule_prompt_tokens = candidates
        .iter()
        .map(|run| {
            let capsule = tasks
                .get(run.task_id.as_str())
                .and_then(|task| task.orientation_capsule.as_ref())
                .ok_or("candidate task has no frozen capsule")?;
            let prompt = orientation_capsule_prompt(capsule)?;
            Ok(Tokenizer::Cl100kBase.count(&prompt))
        })
        .sum::<Result<usize, DynError>>()?;

    let metrics = if unclassified_runs == 0 {
        let classified_baselines = baselines
            .iter()
            .map(|run| {
                classified
                    .get(&(run.task_id.as_str(), run.repetition, BASELINE_ARM))
                    .copied()
                    .ok_or("baseline trajectory classification is missing")
            })
            .collect::<Result<Vec<_>, _>>()?;
        let classified_candidates = candidates
            .iter()
            .map(|run| {
                classified
                    .get(&(run.task_id.as_str(), run.repetition, CANDIDATE_ARM))
                    .copied()
                    .ok_or("candidate trajectory classification is missing")
            })
            .collect::<Result<Vec<_>, _>>()?;
        let retrieval_calls_saved = signed_delta(
            classified_baselines
                .iter()
                .map(|run| run.retrieval_calls as u64)
                .sum(),
            classified_candidates
                .iter()
                .map(|run| run.retrieval_calls as u64)
                .sum(),
        );
        let retrieval_source_tokens_saved = signed_delta(
            classified_baselines
                .iter()
                .map(|run| run.retrieval_source_tokens)
                .sum(),
            classified_candidates
                .iter()
                .map(|run| run.retrieval_source_tokens)
                .sum(),
        );
        let dead_end_source_token_delta = signed_delta(
            classified_candidates
                .iter()
                .map(|run| run.dead_end_source_tokens)
                .sum(),
            classified_baselines
                .iter()
                .map(|run| run.dead_end_source_tokens)
                .sum(),
        );
        let reread_token_delta = signed_delta(
            classified_candidates
                .iter()
                .map(|run| run.reread_tokens)
                .sum(),
            classified_baselines
                .iter()
                .map(|run| run.reread_tokens)
                .sum(),
        );
        Some((
            retrieval_calls_saved,
            retrieval_source_tokens_saved,
            retrieval_source_tokens_saved - capsule_prompt_tokens as i64,
            dead_end_source_token_delta,
            reread_token_delta,
        ))
    } else {
        None
    };
    let (
        retrieval_calls_saved,
        retrieval_source_tokens_saved,
        net_tokens_saved_after_prompt,
        dead_end_source_token_delta,
        reread_token_delta,
    ) = metrics
        .map(|(calls, source, net, dead_end, reread)| {
            (
                Some(calls),
                Some(source),
                Some(net),
                Some(dead_end),
                Some(reread),
            )
        })
        .unwrap_or((None, None, None, None, None));
    let promising = success_regressions == 0
        && unclassified_runs == 0
        && candidate_owner_misses == 0
        && net_tokens_saved_after_prompt.is_some_and(|value| value > 0)
        && dead_end_source_token_delta.is_some_and(|value| value <= 0)
        && reread_token_delta.is_some_and(|value| value <= 0);
    let result = if success_regressions > 0 {
        "reject"
    } else if promising {
        "promising_small_sample"
    } else {
        "no_measured_win"
    };
    Ok(Decision {
        result,
        reason: if success_regressions > 0 {
            "At least one baseline success became a candidate failure, which fails the accuracy-first gate.".to_owned()
        } else if unclassified_runs > 0 {
            "At least one adapter failure lacks a complete trajectory, so retrieval deltas remain unknown and the fail-closed gate forbids a positive claim.".to_owned()
        } else if promising {
            "The capsule preserved validated success, routed every candidate to its owner, and saved more retrieval source tokens than the full injected prompt cost without increasing dead ends or rereads. Repeat before production use.".to_owned()
        } else {
            "The small sample preserved success but did not clear every pre-registered downstream-work gate.".to_owned()
        },
        paired_runs: pairs.len(),
        unclassified_runs,
        success_regressions,
        candidate_owner_misses,
        retrieval_calls_saved,
        retrieval_source_tokens_saved,
        capsule_prompt_tokens,
        net_tokens_saved_after_prompt,
        dead_end_source_token_delta,
        reread_token_delta,
        production_change_authorized: false,
    })
}

fn complete_sum(values: impl IntoIterator<Item = Option<u64>>) -> Option<u64> {
    values
        .into_iter()
        .try_fold(0_u64, |total, value| total.checked_add(value?))
}

fn signed_delta(left: u64, right: u64) -> i64 {
    i128::from(left)
        .saturating_sub(i128::from(right))
        .clamp(i128::from(i64::MIN), i128::from(i64::MAX)) as i64
}

fn read_artifact<T: for<'de> Deserialize<'de>>(
    path: &Path,
    identity: &ArtifactIdentity,
    verified_artifacts: &mut usize,
) -> Result<T, DynError> {
    let bytes = fs::read(path)?;
    if bytes.len() as u64 != identity.bytes || hash_bytes(&bytes) != identity.blake3 {
        return Err(format!("artifact identity mismatch: {}", path.display()).into());
    }
    *verified_artifacts += 1;
    Ok(serde_json::from_slice(&bytes)?)
}

fn hash_bytes(bytes: &[u8]) -> String {
    blake3::hash(bytes).to_hex().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn capsule() -> OrientationCapsule {
        let entries = vec![model_ab_artifacts::OrientationCapsuleEntry {
            path: "src/owner.rs".to_owned(),
            matched_terms: vec!["owner".to_owned()],
            definitions: vec!["Owner".to_owned()],
        }];
        OrientationCapsule {
            capsule_tokens: Tokenizer::Cl100kBase
                .count(&serde_json::to_string(&entries).expect("capsule JSON")),
            entries,
        }
    }

    fn run(arm: &str, source_tokens: u64, prompt_tokens: usize) -> RunClassification {
        RunClassification {
            task_id: "task".to_owned(),
            repetition: 1,
            arm: arm.to_owned(),
            status: "completed".to_owned(),
            official_success: true,
            duration_ms: 1,
            retrieval_calls: if arm == BASELINE_ARM { 3 } else { 1 },
            retrieval_source_tokens: source_tokens,
            transferred_source_tokens: source_tokens,
            transferred_ranges: 1,
            dead_end_source_tokens: 0,
            rereads: 0,
            reread_tokens: 0,
            executor_retrieval_calls: 0,
            owner_path_followed: true,
            first_owner_evidence_sequence: Some(0),
            capsule_tokens: if arm == CANDIDATE_ARM { 20 } else { 0 },
            capsule_prompt_tokens: prompt_tokens,
            first_validated_edit_sequence: 1,
            validation_sequence: 2,
            total_input_tokens: Some(10),
            total_output_tokens: Some(5),
        }
    }

    fn raw_run(run: &RunClassification) -> RawRun {
        RawRun {
            task_id: run.task_id.clone(),
            repetition: run.repetition,
            arm: run.arm.clone(),
            status: run.status.clone(),
            duration_ms: run.duration_ms,
            artifacts: RawArtifacts {
                tool_trace: None,
                trajectory: None,
                prewalk_handoff: None,
            },
            result: Some(RawResult {
                task_success: run.official_success,
                total_input_tokens: run.total_input_tokens,
                total_output_tokens: run.total_output_tokens,
            }),
            error: None,
        }
    }

    fn classify_decision(runs: &[RunClassification]) -> Decision {
        let raw = runs.iter().map(raw_run).collect::<Vec<_>>();
        let task = TaskDefinition {
            id: "task".to_owned(),
            orientation_capsule: Some(capsule()),
            relevant_files: vec!["src/owner.rs".to_owned()],
        };
        let tasks = BTreeMap::from([("task", &task)]);
        decide(&raw, runs, &tasks).expect("paired decision")
    }

    #[test]
    fn decision_charges_the_complete_capsule_prompt() {
        let prompt_tokens = Tokenizer::Cl100kBase
            .count(&orientation_capsule_prompt(&capsule()).expect("capsule prompt"));
        let decision = classify_decision(&[
            run(BASELINE_ARM, 300, 0),
            run(CANDIDATE_ARM, 100, prompt_tokens),
        ]);
        assert_eq!(decision.result, "promising_small_sample");
        assert_eq!(
            decision.net_tokens_saved_after_prompt,
            Some(200 - prompt_tokens as i64)
        );

        let decision = classify_decision(&[
            run(BASELINE_ARM, 100 + prompt_tokens as u64 - 1, 0),
            run(CANDIDATE_ARM, 100, prompt_tokens),
        ]);
        assert_eq!(decision.result, "no_measured_win");
        assert_eq!(decision.net_tokens_saved_after_prompt, Some(-1));
    }

    #[test]
    fn decision_rejects_a_success_regression() {
        let baseline = run(BASELINE_ARM, 300, 0);
        let mut candidate = run(CANDIDATE_ARM, 50, 120);
        candidate.official_success = false;
        let decision = classify_decision(&[baseline, candidate]);
        assert_eq!(decision.result, "reject");
        assert_eq!(decision.success_regressions, 1);
        assert!(!decision.production_change_authorized);
    }

    #[test]
    fn decision_fails_closed_when_a_trajectory_is_missing() {
        let baseline = run(BASELINE_ARM, 300, 0);
        let candidate = run(CANDIDATE_ARM, 50, 120);
        let raw = vec![raw_run(&baseline), raw_run(&candidate)];
        let task = TaskDefinition {
            id: "task".to_owned(),
            orientation_capsule: Some(capsule()),
            relevant_files: vec!["src/owner.rs".to_owned()],
        };
        let tasks = BTreeMap::from([("task", &task)]);
        let decision = decide(&raw, &[baseline], &tasks).expect("fail-closed decision");
        assert_eq!(decision.result, "no_measured_win");
        assert_eq!(decision.unclassified_runs, 1);
        assert_eq!(decision.retrieval_source_tokens_saved, None);
        assert_eq!(decision.net_tokens_saved_after_prompt, None);
    }
}
