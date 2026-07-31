use std::{collections::BTreeMap, error::Error, fs, path::PathBuf};

use clap::{Parser, ValueEnum};
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
    /// Enforce the default-path promotion policy and fail closed on regression.
    #[arg(long, value_enum)]
    promotion_track: Option<PromotionTrack>,
    /// Baseline end-to-end task success rate from the paired agent evaluation.
    #[arg(long, requires = "promotion_track")]
    baseline_task_success_rate: Option<f64>,
    /// Candidate end-to-end task success rate from the paired agent evaluation.
    #[arg(long, requires = "promotion_track")]
    candidate_task_success_rate: Option<f64>,
    /// Baseline complete two-turn provider input from the paired agent evaluation.
    #[arg(long, requires = "promotion_track")]
    baseline_two_turn_provider_input_tokens: Option<u64>,
    /// Candidate complete two-turn provider input from the paired agent evaluation.
    #[arg(long, requires = "promotion_track")]
    candidate_two_turn_provider_input_tokens: Option<u64>,
    /// Baseline follow-up native source reads from the paired agent evaluation.
    #[arg(long, requires = "promotion_track")]
    baseline_follow_up_native_reads: Option<u64>,
    /// Candidate follow-up native source reads from the paired agent evaluation.
    #[arg(long, requires = "promotion_track")]
    candidate_follow_up_native_reads: Option<u64>,
    /// Baseline tool calls from the paired agent evaluation.
    #[arg(long, requires = "promotion_track")]
    baseline_tool_calls: Option<u64>,
    /// Candidate tool calls from the paired agent evaluation.
    #[arg(long, requires = "promotion_track")]
    candidate_tool_calls: Option<u64>,
}

#[derive(Debug, Clone, Copy, Serialize, ValueEnum)]
#[serde(rename_all = "snake_case")]
enum PromotionTrack {
    Quality,
    Cost,
}

const PROMOTION_POLICY: PromotionPolicy = PromotionPolicy {
    minimum_quality_improvement: 0.02,
    minimum_cost_improvement_fraction: 0.05,
    maximum_quality_cost_increase_fraction: 0.05,
    maximum_resource_increase_fraction: 0.05,
};

#[derive(Debug, Deserialize)]
struct BenchmarkReport {
    dataset_kind: String,
    manifest_blake3: String,
    host_os: String,
    host_arch: String,
    tokenizer: String,
    token_count_exact: bool,
    #[serde(default)]
    concept_coverage: Option<ConceptCoverageIdentity>,
    aggregate: Aggregate,
    #[serde(default)]
    task_families: BTreeMap<String, Aggregate>,
}

#[derive(Debug, Deserialize)]
struct ConceptCoverageIdentity {
    labels_blake3: String,
}

#[derive(Debug, Clone, Deserialize)]
struct Aggregate {
    task_count: usize,
    relevant_files: usize,
    candidate_relevant_files_found: usize,
    relevant_files_found: usize,
    line_anchors: usize,
    line_anchors_found: usize,
    leantoken_source_tokens: usize,
    leantoken_total_json_tokens: usize,
    #[serde(default)]
    warm_context_median_ms: f64,
    #[serde(default)]
    warm_context_p95_ms: f64,
    #[serde(default)]
    cold_index_ms: f64,
    #[serde(default)]
    database_bytes: u64,
    #[serde(default)]
    process_rss_bytes: Option<u64>,
    dead_end_fragments: usize,
    dead_end_source_tokens: usize,
    #[serde(default)]
    orientation_capsule_paths: usize,
    #[serde(default)]
    orientation_capsule_relevant_paths: usize,
    #[serde(default)]
    orientation_capsule_tokens: usize,
    known_fragments_resent: usize,
    estimated_repeated_range_source_tokens: usize,
    two_turn_context_json_tokens: usize,
    #[serde(default)]
    concepts: usize,
    #[serde(default)]
    candidate_concepts_found: usize,
    #[serde(default)]
    selected_concepts_found: usize,
}

#[derive(Debug, Serialize)]
struct Comparison {
    dataset_kind: String,
    manifest_blake3: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    concept_labels_blake3: Option<String>,
    task_count: usize,
    baseline: Metrics,
    candidate: Metrics,
    delta: MetricDelta,
    task_families: BTreeMap<String, FamilyComparison>,
    #[serde(skip_serializing_if = "Option::is_none")]
    promotion: Option<PromotionReceipt>,
}

#[derive(Debug, Serialize)]
struct FamilyComparison {
    baseline: Metrics,
    candidate: Metrics,
    delta: MetricDelta,
}

#[derive(Debug, Serialize)]
struct Metrics {
    candidate_file_recall: f64,
    file_recall: f64,
    line_recall: f64,
    source_tokens: usize,
    response_json_tokens: usize,
    dead_end_fragments: usize,
    dead_end_source_tokens: usize,
    orientation_capsule_paths: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    orientation_capsule_path_recall: Option<f64>,
    orientation_capsule_tokens: usize,
    exact_hash_resends: usize,
    estimated_repeated_range_source_tokens: usize,
    two_turn_json_tokens: usize,
    warm_context_median_ms: f64,
    warm_context_p95_ms: f64,
    cold_index_ms: f64,
    database_bytes: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    process_rss_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    candidate_concept_recall: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    selected_concept_recall: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    concept_selection_retention: Option<f64>,
}

#[derive(Debug, Serialize)]
struct MetricDelta {
    candidate_file_recall: f64,
    file_recall: f64,
    line_recall: f64,
    source_tokens: i64,
    response_json_tokens: i64,
    dead_end_fragments: i64,
    dead_end_source_tokens: i64,
    orientation_capsule_paths: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    orientation_capsule_path_recall: Option<f64>,
    orientation_capsule_tokens: i64,
    exact_hash_resends: i64,
    estimated_repeated_range_source_tokens: i64,
    two_turn_json_tokens: i64,
    warm_context_median_ms: f64,
    warm_context_p95_ms: f64,
    cold_index_ms: f64,
    database_bytes: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    process_rss_bytes: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    candidate_concept_recall: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    selected_concept_recall: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    concept_selection_retention: Option<f64>,
}

#[derive(Debug, Serialize)]
struct PromotionReceipt {
    schema_version: u32,
    track: PromotionTrack,
    passed: bool,
    policy: PromotionPolicy,
    baseline_agent: PairedAgentMetrics,
    candidate_agent: PairedAgentMetrics,
    checks: Vec<PromotionCheck>,
}

#[derive(Debug, Clone, Copy, Serialize)]
struct PromotionPolicy {
    minimum_quality_improvement: f64,
    minimum_cost_improvement_fraction: f64,
    maximum_quality_cost_increase_fraction: f64,
    maximum_resource_increase_fraction: f64,
}

#[derive(Debug, Clone, Serialize)]
struct PairedAgentMetrics {
    task_success_rate: f64,
    two_turn_provider_input_tokens: u64,
    follow_up_native_reads: u64,
    tool_calls: u64,
}

#[derive(Debug, Serialize)]
struct PromotionCheck {
    metric: String,
    passed: bool,
    baseline: f64,
    candidate: f64,
    requirement: String,
}

fn main() -> Result<(), Box<dyn Error>> {
    let args = Args::parse();
    let baseline: BenchmarkReport = serde_json::from_str(&fs::read_to_string(&args.baseline)?)?;
    let candidate: BenchmarkReport = serde_json::from_str(&fs::read_to_string(&args.candidate)?)?;
    validate_compatible_reports(&baseline, &candidate)?;

    let baseline_labels = baseline
        .concept_coverage
        .as_ref()
        .map(|coverage| coverage.labels_blake3.as_str());
    let baseline_metrics = Metrics::from(&baseline.aggregate);
    let candidate_metrics = Metrics::from(&candidate.aggregate);
    let task_families = compare_task_families(&baseline, &candidate)?;
    let promotion = args
        .promotion_track
        .map(|track| {
            promotion_receipt(
                track,
                &baseline_metrics,
                &candidate_metrics,
                &task_families,
                PromotionAgentInputs {
                    baseline: OptionalAgentMetrics {
                        task_success_rate: args.baseline_task_success_rate,
                        two_turn_provider_input_tokens: args
                            .baseline_two_turn_provider_input_tokens,
                        follow_up_native_reads: args.baseline_follow_up_native_reads,
                        tool_calls: args.baseline_tool_calls,
                    },
                    candidate: OptionalAgentMetrics {
                        task_success_rate: args.candidate_task_success_rate,
                        two_turn_provider_input_tokens: args
                            .candidate_two_turn_provider_input_tokens,
                        follow_up_native_reads: args.candidate_follow_up_native_reads,
                        tool_calls: args.candidate_tool_calls,
                    },
                },
                PROMOTION_POLICY,
            )
        })
        .transpose()?;
    let promotion_passed = promotion.as_ref().is_none_or(|receipt| receipt.passed);
    let comparison = Comparison {
        dataset_kind: baseline.dataset_kind,
        manifest_blake3: baseline.manifest_blake3,
        concept_labels_blake3: baseline_labels.map(str::to_owned),
        task_count: baseline.aggregate.task_count,
        delta: MetricDelta::between(&baseline_metrics, &candidate_metrics),
        baseline: baseline_metrics,
        candidate: candidate_metrics,
        task_families,
        promotion,
    };
    let json = serde_json::to_string_pretty(&comparison)?;
    if let Some(output) = args.output {
        if let Some(parent) = output.parent().filter(|path| !path.as_os_str().is_empty()) {
            fs::create_dir_all(parent)?;
        }
        fs::write(output, &json)?;
    }
    println!("{json}");
    if !promotion_passed {
        return Err("retrieval promotion gate failed; inspect the emitted receipt".into());
    }
    Ok(())
}

fn validate_compatible_reports(
    baseline: &BenchmarkReport,
    candidate: &BenchmarkReport,
) -> Result<(), Box<dyn Error>> {
    if baseline.manifest_blake3 != candidate.manifest_blake3 {
        return Err("reports use different manifests; refusing an invalid ablation".into());
    }
    if baseline.dataset_kind != candidate.dataset_kind {
        return Err("reports use different dataset kinds".into());
    }
    if baseline.host_os != candidate.host_os || baseline.host_arch != candidate.host_arch {
        return Err("reports were produced on different host platforms".into());
    }
    if baseline.tokenizer != candidate.tokenizer
        || baseline.token_count_exact != candidate.token_count_exact
    {
        return Err("reports use different tokenizer accounting".into());
    }
    if baseline.aggregate.task_count != candidate.aggregate.task_count {
        return Err("reports contain different task counts".into());
    }
    validate_aggregate("baseline aggregate", &baseline.aggregate)?;
    validate_aggregate("candidate aggregate", &candidate.aggregate)?;
    let baseline_labels = baseline
        .concept_coverage
        .as_ref()
        .map(|coverage| coverage.labels_blake3.as_str());
    let candidate_labels = candidate
        .concept_coverage
        .as_ref()
        .map(|coverage| coverage.labels_blake3.as_str());
    if baseline_labels != candidate_labels {
        return Err("reports use different concept-label overlays".into());
    }
    validate_frozen_denominators("aggregate", &baseline.aggregate, &candidate.aggregate)?;
    if baseline.task_families.keys().collect::<Vec<_>>()
        != candidate.task_families.keys().collect::<Vec<_>>()
    {
        return Err("reports contain different task-family strata".into());
    }
    for (family, baseline_aggregate) in &baseline.task_families {
        validate_aggregate(
            &format!("baseline task family {family}"),
            baseline_aggregate,
        )?;
        validate_aggregate(
            &format!("candidate task family {family}"),
            &candidate.task_families[family],
        )?;
        validate_frozen_denominators(
            &format!("task family {family}"),
            baseline_aggregate,
            &candidate.task_families[family],
        )?;
    }
    for (name, report) in [("baseline", baseline), ("candidate", candidate)] {
        if report.task_families.is_empty() {
            continue;
        }
        let stratified_tasks = report
            .task_families
            .values()
            .map(|aggregate| aggregate.task_count)
            .sum::<usize>();
        if stratified_tasks != report.aggregate.task_count {
            return Err(format!(
                "{name} task-family strata cover {stratified_tasks} tasks, expected {}",
                report.aggregate.task_count
            )
            .into());
        }
    }
    Ok(())
}

fn validate_aggregate(scope: &str, aggregate: &Aggregate) -> Result<(), Box<dyn Error>> {
    if aggregate.task_count == 0 {
        return Err(format!("{scope} has no tasks").into());
    }
    if aggregate.candidate_relevant_files_found > aggregate.relevant_files
        || aggregate.relevant_files_found > aggregate.relevant_files
        || aggregate.line_anchors_found > aggregate.line_anchors
        || aggregate.candidate_concepts_found > aggregate.concepts
        || aggregate.selected_concepts_found > aggregate.concepts
        || aggregate.selected_concepts_found > aggregate.candidate_concepts_found
    {
        return Err(format!("{scope} has impossible recall counters").into());
    }
    for (metric, value) in [
        ("warm_context_median_ms", aggregate.warm_context_median_ms),
        ("warm_context_p95_ms", aggregate.warm_context_p95_ms),
        ("cold_index_ms", aggregate.cold_index_ms),
    ] {
        if !value.is_finite() || value < 0.0 {
            return Err(format!("{scope} has invalid {metric}").into());
        }
    }
    if aggregate.warm_context_median_ms > aggregate.warm_context_p95_ms {
        return Err(format!("{scope} has warm p50 above warm p95").into());
    }
    Ok(())
}

fn validate_frozen_denominators(
    scope: &str,
    baseline: &Aggregate,
    candidate: &Aggregate,
) -> Result<(), Box<dyn Error>> {
    if baseline.task_count != candidate.task_count
        || baseline.relevant_files != candidate.relevant_files
        || baseline.line_anchors != candidate.line_anchors
        || baseline.concepts != candidate.concepts
    {
        return Err(format!("{scope} has different frozen label denominators").into());
    }
    if baseline.process_rss_bytes.is_some() != candidate.process_rss_bytes.is_some() {
        return Err(format!("{scope} has inconsistent RSS availability").into());
    }
    Ok(())
}

impl From<&Aggregate> for Metrics {
    fn from(value: &Aggregate) -> Self {
        Self {
            candidate_file_recall: ratio(
                value.candidate_relevant_files_found,
                value.relevant_files,
            ),
            file_recall: ratio(value.relevant_files_found, value.relevant_files),
            line_recall: ratio(value.line_anchors_found, value.line_anchors),
            source_tokens: value.leantoken_source_tokens,
            response_json_tokens: value.leantoken_total_json_tokens,
            dead_end_fragments: value.dead_end_fragments,
            dead_end_source_tokens: value.dead_end_source_tokens,
            orientation_capsule_paths: value.orientation_capsule_paths,
            orientation_capsule_path_recall: optional_ratio(
                value.orientation_capsule_relevant_paths,
                value.orientation_capsule_paths,
            ),
            orientation_capsule_tokens: value.orientation_capsule_tokens,
            exact_hash_resends: value.known_fragments_resent,
            estimated_repeated_range_source_tokens: value.estimated_repeated_range_source_tokens,
            two_turn_json_tokens: value.two_turn_context_json_tokens,
            warm_context_median_ms: value.warm_context_median_ms,
            warm_context_p95_ms: value.warm_context_p95_ms,
            cold_index_ms: value.cold_index_ms,
            database_bytes: value.database_bytes,
            process_rss_bytes: value.process_rss_bytes,
            candidate_concept_recall: optional_ratio(
                value.candidate_concepts_found,
                value.concepts,
            ),
            selected_concept_recall: optional_ratio(value.selected_concepts_found, value.concepts),
            concept_selection_retention: optional_ratio(
                value.selected_concepts_found,
                value.candidate_concepts_found,
            ),
        }
    }
}

impl MetricDelta {
    fn between(baseline: &Metrics, candidate: &Metrics) -> Self {
        Self {
            candidate_file_recall: candidate.candidate_file_recall - baseline.candidate_file_recall,
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
            orientation_capsule_paths: signed_delta(
                baseline.orientation_capsule_paths,
                candidate.orientation_capsule_paths,
            ),
            orientation_capsule_path_recall: optional_difference(
                baseline.orientation_capsule_path_recall,
                candidate.orientation_capsule_path_recall,
            ),
            orientation_capsule_tokens: signed_delta(
                baseline.orientation_capsule_tokens,
                candidate.orientation_capsule_tokens,
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
            warm_context_median_ms: candidate.warm_context_median_ms
                - baseline.warm_context_median_ms,
            warm_context_p95_ms: candidate.warm_context_p95_ms - baseline.warm_context_p95_ms,
            cold_index_ms: candidate.cold_index_ms - baseline.cold_index_ms,
            database_bytes: signed_delta_u64(baseline.database_bytes, candidate.database_bytes),
            process_rss_bytes: optional_signed_delta_u64(
                baseline.process_rss_bytes,
                candidate.process_rss_bytes,
            ),
            candidate_concept_recall: optional_difference(
                baseline.candidate_concept_recall,
                candidate.candidate_concept_recall,
            ),
            selected_concept_recall: optional_difference(
                baseline.selected_concept_recall,
                candidate.selected_concept_recall,
            ),
            concept_selection_retention: optional_difference(
                baseline.concept_selection_retention,
                candidate.concept_selection_retention,
            ),
        }
    }
}

fn compare_task_families(
    baseline: &BenchmarkReport,
    candidate: &BenchmarkReport,
) -> Result<BTreeMap<String, FamilyComparison>, Box<dyn Error>> {
    baseline
        .task_families
        .iter()
        .map(|(family, baseline_aggregate)| {
            let candidate_aggregate = candidate
                .task_families
                .get(family)
                .ok_or_else(|| format!("candidate report is missing task family {family}"))?;
            let baseline = Metrics::from(baseline_aggregate);
            let candidate = Metrics::from(candidate_aggregate);
            Ok((
                family.clone(),
                FamilyComparison {
                    delta: MetricDelta::between(&baseline, &candidate),
                    baseline,
                    candidate,
                },
            ))
        })
        .collect()
}

struct OptionalAgentMetrics {
    task_success_rate: Option<f64>,
    two_turn_provider_input_tokens: Option<u64>,
    follow_up_native_reads: Option<u64>,
    tool_calls: Option<u64>,
}

struct PromotionAgentInputs {
    baseline: OptionalAgentMetrics,
    candidate: OptionalAgentMetrics,
}

fn promotion_receipt(
    track: PromotionTrack,
    baseline: &Metrics,
    candidate: &Metrics,
    task_families: &BTreeMap<String, FamilyComparison>,
    agent_inputs: PromotionAgentInputs,
    policy: PromotionPolicy,
) -> Result<PromotionReceipt, Box<dyn Error>> {
    let PromotionPolicy {
        minimum_quality_improvement,
        minimum_cost_improvement_fraction,
        maximum_quality_cost_increase_fraction,
        maximum_resource_increase_fraction,
    } = policy;
    let baseline_agent = PairedAgentMetrics {
        task_success_rate: agent_inputs
            .baseline
            .task_success_rate
            .ok_or("promotion requires --baseline-task-success-rate")?,
        two_turn_provider_input_tokens: agent_inputs
            .baseline
            .two_turn_provider_input_tokens
            .ok_or("promotion requires --baseline-two-turn-provider-input-tokens")?,
        follow_up_native_reads: agent_inputs
            .baseline
            .follow_up_native_reads
            .ok_or("promotion requires --baseline-follow-up-native-reads")?,
        tool_calls: agent_inputs
            .baseline
            .tool_calls
            .ok_or("promotion requires --baseline-tool-calls")?,
    };
    let candidate_agent = PairedAgentMetrics {
        task_success_rate: agent_inputs
            .candidate
            .task_success_rate
            .ok_or("promotion requires --candidate-task-success-rate")?,
        two_turn_provider_input_tokens: agent_inputs
            .candidate
            .two_turn_provider_input_tokens
            .ok_or("promotion requires --candidate-two-turn-provider-input-tokens")?,
        follow_up_native_reads: agent_inputs
            .candidate
            .follow_up_native_reads
            .ok_or("promotion requires --candidate-follow-up-native-reads")?,
        tool_calls: agent_inputs
            .candidate
            .tool_calls
            .ok_or("promotion requires --candidate-tool-calls")?,
    };
    for (name, value) in [
        (
            "baseline task success rate",
            baseline_agent.task_success_rate,
        ),
        (
            "candidate task success rate",
            candidate_agent.task_success_rate,
        ),
    ] {
        if !(0.0..=1.0).contains(&value) {
            return Err(format!("{name} must be between zero and one").into());
        }
    }
    for (name, value) in [
        ("minimum quality improvement", minimum_quality_improvement),
        (
            "minimum cost improvement fraction",
            minimum_cost_improvement_fraction,
        ),
        (
            "maximum quality cost increase fraction",
            maximum_quality_cost_increase_fraction,
        ),
        (
            "maximum resource increase fraction",
            maximum_resource_increase_fraction,
        ),
    ] {
        if !value.is_finite() || value < 0.0 {
            return Err(format!("{name} must be finite and non-negative").into());
        }
    }
    if task_families.is_empty()
        || task_families
            .keys()
            .any(|family| family.trim().is_empty() || family == "unclassified")
    {
        return Err(
            "promotion requires non-empty task-family strata with no unclassified tasks".into(),
        );
    }

    let mut checks = Vec::new();
    push_non_regression(
        &mut checks,
        "candidate_file_recall",
        baseline.candidate_file_recall,
        candidate.candidate_file_recall,
    );
    push_non_regression(
        &mut checks,
        "returned_file_recall",
        baseline.file_recall,
        candidate.file_recall,
    );
    push_non_regression(
        &mut checks,
        "line_anchor_recall",
        baseline.line_recall,
        candidate.line_recall,
    );
    push_at_most(
        &mut checks,
        "dead_end_fragments",
        baseline.dead_end_fragments as f64,
        candidate.dead_end_fragments as f64,
        0.0,
    );
    push_at_most(
        &mut checks,
        "dead_end_source_tokens",
        baseline.dead_end_source_tokens as f64,
        candidate.dead_end_source_tokens as f64,
        0.0,
    );
    push_at_most(
        &mut checks,
        "estimated_repeated_range_source_tokens",
        baseline.estimated_repeated_range_source_tokens as f64,
        candidate.estimated_repeated_range_source_tokens as f64,
        0.0,
    );
    push_at_most(
        &mut checks,
        "exact_hash_resends",
        baseline.exact_hash_resends as f64,
        candidate.exact_hash_resends as f64,
        0.0,
    );
    push_at_most(
        &mut checks,
        "follow_up_native_reads",
        baseline_agent.follow_up_native_reads as f64,
        candidate_agent.follow_up_native_reads as f64,
        0.0,
    );
    push_at_most(
        &mut checks,
        "warm_context_p95_ms",
        baseline.warm_context_p95_ms,
        candidate.warm_context_p95_ms,
        maximum_resource_increase_fraction,
    );
    push_at_most(
        &mut checks,
        "cold_index_ms",
        baseline.cold_index_ms,
        candidate.cold_index_ms,
        maximum_resource_increase_fraction,
    );
    push_at_most(
        &mut checks,
        "database_bytes",
        baseline.database_bytes as f64,
        candidate.database_bytes as f64,
        maximum_resource_increase_fraction,
    );
    push_optional_at_most(
        &mut checks,
        "process_rss_bytes",
        baseline.process_rss_bytes,
        candidate.process_rss_bytes,
        maximum_resource_increase_fraction,
    );

    match track {
        PromotionTrack::Quality => {
            checks.push(PromotionCheck {
                metric: "task_success_rate".into(),
                passed: candidate_agent.task_success_rate - baseline_agent.task_success_rate
                    >= minimum_quality_improvement,
                baseline: baseline_agent.task_success_rate,
                candidate: candidate_agent.task_success_rate,
                requirement: format!("candidate - baseline >= {minimum_quality_improvement:.6}"),
            });
            push_at_most(
                &mut checks,
                "first_response_json_tokens",
                baseline.response_json_tokens as f64,
                candidate.response_json_tokens as f64,
                maximum_quality_cost_increase_fraction,
            );
            push_at_most(
                &mut checks,
                "two_turn_json_tokens",
                baseline.two_turn_json_tokens as f64,
                candidate.two_turn_json_tokens as f64,
                maximum_quality_cost_increase_fraction,
            );
            push_at_most(
                &mut checks,
                "two_turn_provider_input_tokens",
                baseline_agent.two_turn_provider_input_tokens as f64,
                candidate_agent.two_turn_provider_input_tokens as f64,
                maximum_quality_cost_increase_fraction,
            );
            push_at_most(
                &mut checks,
                "tool_calls",
                baseline_agent.tool_calls as f64,
                candidate_agent.tool_calls as f64,
                maximum_quality_cost_increase_fraction,
            );
        }
        PromotionTrack::Cost => {
            push_non_regression(
                &mut checks,
                "task_success_rate",
                baseline_agent.task_success_rate,
                candidate_agent.task_success_rate,
            );
            push_at_most(
                &mut checks,
                "first_response_json_tokens",
                baseline.response_json_tokens as f64,
                candidate.response_json_tokens as f64,
                maximum_resource_increase_fraction,
            );
            push_at_most(
                &mut checks,
                "two_turn_json_tokens",
                baseline.two_turn_json_tokens as f64,
                candidate.two_turn_json_tokens as f64,
                maximum_resource_increase_fraction,
            );
            push_at_most(
                &mut checks,
                "two_turn_provider_input_tokens",
                baseline_agent.two_turn_provider_input_tokens as f64,
                candidate_agent.two_turn_provider_input_tokens as f64,
                maximum_resource_increase_fraction,
            );
            push_at_most(
                &mut checks,
                "tool_calls",
                baseline_agent.tool_calls as f64,
                candidate_agent.tool_calls as f64,
                maximum_resource_increase_fraction,
            );
            let best_reduction = [
                fractional_reduction(
                    baseline_agent.two_turn_provider_input_tokens as f64,
                    candidate_agent.two_turn_provider_input_tokens as f64,
                ),
                fractional_reduction(
                    baseline_agent.tool_calls as f64,
                    candidate_agent.tool_calls as f64,
                ),
                fractional_reduction(baseline.warm_context_p95_ms, candidate.warm_context_p95_ms),
            ]
            .into_iter()
            .fold(f64::NEG_INFINITY, f64::max);
            checks.push(PromotionCheck {
                metric: "material_cost_improvement".into(),
                passed: best_reduction >= minimum_cost_improvement_fraction,
                baseline: minimum_cost_improvement_fraction,
                candidate: best_reduction,
                requirement:
                    "one of complete provider input, tool calls, or warm p95 latency improves"
                        .into(),
            });
        }
    }

    for (family, comparison) in task_families {
        push_non_regression(
            &mut checks,
            &format!("task_family.{family}.candidate_file_recall"),
            comparison.baseline.candidate_file_recall,
            comparison.candidate.candidate_file_recall,
        );
        push_non_regression(
            &mut checks,
            &format!("task_family.{family}.returned_file_recall"),
            comparison.baseline.file_recall,
            comparison.candidate.file_recall,
        );
        push_non_regression(
            &mut checks,
            &format!("task_family.{family}.line_anchor_recall"),
            comparison.baseline.line_recall,
            comparison.candidate.line_recall,
        );
    }

    Ok(PromotionReceipt {
        schema_version: 1,
        track,
        passed: checks.iter().all(|check| check.passed),
        policy,
        baseline_agent,
        candidate_agent,
        checks,
    })
}

fn push_non_regression(
    checks: &mut Vec<PromotionCheck>,
    metric: &str,
    baseline: f64,
    candidate: f64,
) {
    checks.push(PromotionCheck {
        metric: metric.into(),
        passed: candidate + f64::EPSILON >= baseline,
        baseline,
        candidate,
        requirement: "candidate >= baseline".into(),
    });
}

fn push_at_most(
    checks: &mut Vec<PromotionCheck>,
    metric: &str,
    baseline: f64,
    candidate: f64,
    allowed_increase_fraction: f64,
) {
    let maximum = baseline * (1.0 + allowed_increase_fraction);
    checks.push(PromotionCheck {
        metric: metric.into(),
        passed: candidate <= maximum + f64::EPSILON,
        baseline,
        candidate,
        requirement: format!(
            "candidate <= baseline * {:.6}",
            1.0 + allowed_increase_fraction
        ),
    });
}

fn push_optional_at_most(
    checks: &mut Vec<PromotionCheck>,
    metric: &str,
    baseline: Option<u64>,
    candidate: Option<u64>,
    allowed_increase_fraction: f64,
) {
    if let (Some(baseline), Some(candidate)) = (baseline, candidate) {
        push_at_most(
            checks,
            metric,
            baseline as f64,
            candidate as f64,
            allowed_increase_fraction,
        );
    }
}

fn fractional_reduction(baseline: f64, candidate: f64) -> f64 {
    if baseline == 0.0 {
        if candidate == 0.0 { 0.0 } else { -1.0 }
    } else {
        (baseline - candidate) / baseline
    }
}

fn ratio(numerator: usize, denominator: usize) -> f64 {
    if denominator == 0 {
        0.0
    } else {
        numerator as f64 / denominator as f64
    }
}

fn optional_ratio(numerator: usize, denominator: usize) -> Option<f64> {
    (denominator != 0).then(|| ratio(numerator, denominator))
}

fn optional_difference(baseline: Option<f64>, candidate: Option<f64>) -> Option<f64> {
    Some(candidate? - baseline?)
}

fn signed_delta(baseline: usize, candidate: usize) -> i64 {
    i64::try_from(candidate).unwrap_or(i64::MAX) - i64::try_from(baseline).unwrap_or(i64::MAX)
}

fn signed_delta_u64(baseline: u64, candidate: u64) -> i64 {
    i64::try_from(candidate).unwrap_or(i64::MAX) - i64::try_from(baseline).unwrap_or(i64::MAX)
}

fn optional_signed_delta_u64(baseline: Option<u64>, candidate: Option<u64>) -> Option<i64> {
    Some(signed_delta_u64(baseline?, candidate?))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn promotion_agent_inputs() -> PromotionAgentInputs {
        PromotionAgentInputs {
            baseline: OptionalAgentMetrics {
                task_success_rate: Some(0.8),
                two_turn_provider_input_tokens: Some(100),
                follow_up_native_reads: Some(1),
                tool_calls: Some(10),
            },
            candidate: OptionalAgentMetrics {
                task_success_rate: Some(0.8),
                two_turn_provider_input_tokens: Some(90),
                follow_up_native_reads: Some(1),
                tool_calls: Some(10),
            },
        }
    }

    fn report(labels_blake3: Option<&str>) -> BenchmarkReport {
        let aggregate = Aggregate {
            task_count: 1,
            relevant_files: 1,
            candidate_relevant_files_found: 1,
            relevant_files_found: 1,
            line_anchors: 1,
            line_anchors_found: 1,
            leantoken_source_tokens: 1,
            leantoken_total_json_tokens: 1,
            warm_context_median_ms: 1.0,
            warm_context_p95_ms: 1.0,
            cold_index_ms: 1.0,
            database_bytes: 1,
            process_rss_bytes: Some(1),
            dead_end_fragments: 0,
            dead_end_source_tokens: 0,
            orientation_capsule_paths: 0,
            orientation_capsule_relevant_paths: 0,
            orientation_capsule_tokens: 0,
            known_fragments_resent: 0,
            estimated_repeated_range_source_tokens: 0,
            two_turn_context_json_tokens: 1,
            concepts: usize::from(labels_blake3.is_some()),
            candidate_concepts_found: usize::from(labels_blake3.is_some()),
            selected_concepts_found: usize::from(labels_blake3.is_some()),
        };
        BenchmarkReport {
            dataset_kind: "validation".into(),
            manifest_blake3: "manifest".into(),
            host_os: "linux".into(),
            host_arch: "x86_64".into(),
            tokenizer: "cl100k_base".into(),
            token_count_exact: true,
            concept_coverage: labels_blake3.map(|value| ConceptCoverageIdentity {
                labels_blake3: value.into(),
            }),
            task_families: BTreeMap::from([("symptom_first_debugging".into(), aggregate.clone())]),
            aggregate,
        }
    }

    #[test]
    fn comparison_requires_the_same_concept_overlay() {
        validate_compatible_reports(&report(Some("labels")), &report(Some("labels")))
            .expect("same labels");
        assert!(
            validate_compatible_reports(&report(Some("first")), &report(Some("second")))
                .expect_err("different labels")
                .to_string()
                .contains("different concept-label overlays")
        );
        assert!(
            validate_compatible_reports(&report(None), &report(Some("labels")))
                .expect_err("labeled and unlabeled")
                .to_string()
                .contains("different concept-label overlays")
        );
    }

    #[test]
    fn orientation_capsule_metrics_remain_separate_from_source_recall() {
        let baseline = report(None);
        let mut candidate = report(None);
        candidate.aggregate.orientation_capsule_paths = 2;
        candidate.aggregate.orientation_capsule_relevant_paths = 2;
        candidate.aggregate.orientation_capsule_tokens = 79;

        let baseline_metrics = Metrics::from(&baseline.aggregate);
        let candidate_metrics = Metrics::from(&candidate.aggregate);
        let delta = MetricDelta::between(&baseline_metrics, &candidate_metrics);

        assert_eq!(candidate_metrics.orientation_capsule_path_recall, Some(1.0));
        assert_eq!(delta.orientation_capsule_paths, 2);
        assert_eq!(delta.orientation_capsule_tokens, 79);
        assert_eq!(delta.file_recall, 0.0);
        assert_eq!(delta.source_tokens, 0);
    }

    #[test]
    fn cost_promotion_requires_recall_safe_family_strata() {
        let baseline = report(None);
        let mut candidate = report(None);
        let families = compare_task_families(&baseline, &candidate).expect("family comparison");

        let passing = promotion_receipt(
            PromotionTrack::Cost,
            &Metrics::from(&baseline.aggregate),
            &Metrics::from(&candidate.aggregate),
            &families,
            promotion_agent_inputs(),
            PROMOTION_POLICY,
        )
        .expect("promotion receipt");
        assert!(passing.passed);

        candidate
            .task_families
            .get_mut("symptom_first_debugging")
            .expect("candidate family")
            .line_anchors_found = 0;
        let families = compare_task_families(&baseline, &candidate).expect("family comparison");
        let failing = promotion_receipt(
            PromotionTrack::Cost,
            &Metrics::from(&baseline.aggregate),
            &Metrics::from(&candidate.aggregate),
            &families,
            promotion_agent_inputs(),
            PROMOTION_POLICY,
        )
        .expect("promotion receipt");
        assert!(!failing.passed);
        assert!(failing.checks.iter().any(|check| {
            check.metric == "task_family.symptom_first_debugging.line_anchor_recall"
                && !check.passed
        }));
    }

    #[test]
    fn compatibility_rejects_changed_family_denominators() {
        let baseline = report(None);
        let mut candidate = report(None);
        candidate
            .task_families
            .get_mut("symptom_first_debugging")
            .expect("candidate family")
            .line_anchors = 2;

        assert!(
            validate_compatible_reports(&baseline, &candidate)
                .expect_err("changed family labels")
                .to_string()
                .contains("different frozen label denominators")
        );
    }

    #[test]
    fn compatibility_rejects_impossible_recall_counters() {
        let baseline = report(None);
        let mut candidate = report(None);
        candidate.aggregate.relevant_files_found = 2;

        assert!(
            validate_compatible_reports(&baseline, &candidate)
                .expect_err("impossible recall")
                .to_string()
                .contains("impossible recall counters")
        );
    }

    #[test]
    fn promotion_rejects_resource_regressions_even_with_a_cost_win() {
        let baseline = report(None);
        let mut candidate = report(None);
        candidate.aggregate.database_bytes = 2;
        candidate
            .task_families
            .get_mut("symptom_first_debugging")
            .expect("candidate family")
            .database_bytes = 2;
        let families = compare_task_families(&baseline, &candidate).expect("family comparison");

        let receipt = promotion_receipt(
            PromotionTrack::Cost,
            &Metrics::from(&baseline.aggregate),
            &Metrics::from(&candidate.aggregate),
            &families,
            promotion_agent_inputs(),
            PROMOTION_POLICY,
        )
        .expect("promotion receipt");

        assert!(!receipt.passed);
        assert!(
            receipt
                .checks
                .iter()
                .any(|check| check.metric == "database_bytes" && !check.passed)
        );
    }
}
