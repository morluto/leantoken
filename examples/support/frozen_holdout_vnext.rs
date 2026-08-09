use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Component, Path, PathBuf};

use clap::{Args, Parser, Subcommand};
use serde::{Deserialize, Serialize, de::DeserializeOwned};

const SCHEMA_VERSION: u32 = 1;
const DATASET_KIND: &str = "frozen_holdout_vnext";
const SEALED_STATE: &str = "sealed_unconsumed";
const TOKENIZER: &str = "cl100k_base";
const MAX_ARTIFACT_BYTES: u64 = 32 * 1024 * 1024;
const MAX_RECORDS: usize = 1_000;
const MAX_PROMPT_BYTES: usize = 64 * 1024;
const MAX_VALIDATOR_COMMANDS: usize = 8;
const MAX_COMMAND_BYTES: usize = 8 * 1024;
const MAX_TOKEN_BUDGET: usize = 32_000;
const REQUIRED_FAMILIES: [&str; 11] = [
    "symptom_first_debugging",
    "framework_convention",
    "cross_package_ownership",
    "config_generated_behavior",
    "tests_as_specification",
    "refactor_impact",
    "history_root_cause",
    "monorepo_routing",
    "generated_source_mapping",
    "failure_trace_to_owner",
    "repository_orientation",
];

type DynError = Box<dyn Error + Send + Sync>;

#[derive(Debug, Parser)]
#[command(about = "Seal or verify an access-separated frozen holdout vNext")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Validate public tasks and private labels, then emit a path-free receipt.
    Seal(SealArgs),
    /// Revalidate the publishable policy, tasks, host provenance, and receipt.
    VerifyPublic(VerifyPublicArgs),
}

#[derive(Debug, Args)]
struct SealArgs {
    #[arg(long)]
    policy: PathBuf,
    #[arg(long)]
    tasks: PathBuf,
    #[arg(long)]
    labels: PathBuf,
    #[arg(long)]
    host: PathBuf,
    #[arg(long)]
    candidate_revision: String,
    #[arg(long)]
    harness_revision: String,
    #[arg(long)]
    evaluator_revision: String,
    #[arg(long)]
    toolchain: String,
    #[arg(long)]
    output: PathBuf,
}

#[derive(Debug, Args)]
struct VerifyPublicArgs {
    #[arg(long)]
    policy: PathBuf,
    #[arg(long)]
    tasks: PathBuf,
    #[arg(long)]
    host: PathBuf,
    #[arg(long)]
    receipt: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct Policy {
    schema_version: u32,
    dataset_kind: String,
    frozen_at: String,
    baseline_revision: String,
    tokenizer: String,
    task_families: Vec<String>,
    coverage: CoveragePolicy,
    execution: ExecutionPolicy,
    promotion: PromotionPolicy,
    required_provenance: Vec<String>,
    reclassification_rule: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct CoveragePolicy {
    minimum_tasks: usize,
    minimum_tasks_per_family: usize,
    minimum_repositories_per_family: usize,
    minimum_languages_per_family: usize,
    minimum_task_shapes_per_family: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct ExecutionPolicy {
    policy_id: String,
    attempts_per_arm: usize,
    maximum_tool_calls_per_attempt: usize,
    maximum_wall_time_seconds_per_attempt: usize,
    maximum_source_tokens_per_retrieval: usize,
    network_policy: String,
    workspace_policy: String,
    validation_policy: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct PromotionPolicy {
    confidence_level: f64,
    bootstrap_resamples: usize,
    bootstrap_seed: u64,
    minimum_task_success_delta: f64,
    maximum_global_recall_regression: f64,
    maximum_family_recall_regression: f64,
    maximum_dead_end_source_regression: f64,
    maximum_reread_source_regression: f64,
    maximum_provider_input_regression: f64,
    maximum_warm_p95_latency_regression: f64,
    maximum_peak_rss_regression: f64,
    maximum_database_size_regression: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct PublicTask {
    schema_version: u32,
    id: String,
    prompt: String,
    task_family: String,
    repository: RepositoryIdentity,
    languages: Vec<String>,
    task_shape: String,
    token_budget: usize,
    executor_policy_id: String,
    success_validator: SuccessValidator,
    provenance: TaskProvenance,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(deny_unknown_fields)]
struct RepositoryIdentity {
    url: String,
    revision: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct SuccessValidator {
    kind: String,
    commands: Vec<String>,
    timeout_seconds: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct TaskProvenance {
    task_source: String,
    source_url: String,
    source_revision: String,
    captured_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct PrivateLabel {
    schema_version: u32,
    task_id: String,
    task_blake3: String,
    label_method: String,
    relevant_files: Vec<String>,
    relevant_regions: Vec<RelevantRegion>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct RelevantRegion {
    path: String,
    start_line: usize,
    end_line: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct HostProvenance {
    schema_version: u32,
    os: String,
    architecture: String,
    runner: String,
    rustc: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct SealReceipt {
    schema_version: u32,
    dataset_kind: String,
    state: String,
    frozen_at: String,
    baseline_revision: String,
    candidate_revision: String,
    harness_revision: String,
    evaluator_revision: String,
    toolchain: String,
    tokenizer: String,
    policy_blake3: String,
    tasks_blake3: String,
    labels_blake3: String,
    host_blake3: String,
    task_count: usize,
    label_count: usize,
    task_counts_by_family: BTreeMap<String, usize>,
    task_counts_by_language: BTreeMap<String, usize>,
    repository_counts_by_family: BTreeMap<String, usize>,
    task_shape_counts_by_family: BTreeMap<String, usize>,
    limitations: Vec<String>,
}

#[derive(Debug)]
struct Loaded<T> {
    bytes: Vec<u8>,
    value: T,
}

#[derive(Debug, Default, PartialEq)]
struct CoverageSummary {
    task_counts_by_family: BTreeMap<String, usize>,
    task_counts_by_language: BTreeMap<String, usize>,
    repositories_by_family: BTreeMap<String, BTreeSet<RepositoryIdentity>>,
    languages_by_family: BTreeMap<String, BTreeSet<String>>,
    task_shapes_by_family: BTreeMap<String, BTreeSet<String>>,
}

pub(crate) fn run() -> Result<(), DynError> {
    match Cli::parse().command {
        Command::Seal(args) => seal(&args),
        Command::VerifyPublic(args) => verify_public(&args),
    }
}

fn seal(args: &SealArgs) -> Result<(), DynError> {
    ensure_output_absent(&args.output)?;
    ensure_private_file(&args.labels)?;

    let policy = load_json::<Policy>(&args.policy)?;
    validate_policy(&policy.value)?;
    let tasks = load_jsonl::<PublicTask>(&args.tasks)?;
    let summary = validate_tasks(&policy.value, &tasks.value)?;
    let labels = load_jsonl::<PrivateLabel>(&args.labels)?;
    validate_labels(&tasks.value, &labels.value)?;
    let host = load_json::<HostProvenance>(&args.host)?;
    validate_host(&host.value)?;
    validate_revision(&args.candidate_revision, "candidate_revision")?;
    if args.candidate_revision == policy.value.baseline_revision {
        return Err("candidate_revision must differ from the frozen baseline_revision".into());
    }
    validate_revision(&args.harness_revision, "harness_revision")?;
    validate_revision(&args.evaluator_revision, "evaluator_revision")?;
    validate_nonempty_bounded(&args.toolchain, "toolchain", 256)?;

    let receipt = SealReceipt {
        schema_version: SCHEMA_VERSION,
        dataset_kind: DATASET_KIND.into(),
        state: SEALED_STATE.into(),
        frozen_at: policy.value.frozen_at.clone(),
        baseline_revision: policy.value.baseline_revision.clone(),
        candidate_revision: args.candidate_revision.clone(),
        harness_revision: args.harness_revision.clone(),
        evaluator_revision: args.evaluator_revision.clone(),
        toolchain: args.toolchain.clone(),
        tokenizer: policy.value.tokenizer.clone(),
        policy_blake3: hash(&policy.bytes),
        tasks_blake3: hash(&tasks.bytes),
        labels_blake3: hash(&labels.bytes),
        host_blake3: hash(&host.bytes),
        task_count: tasks.value.len(),
        label_count: labels.value.len(),
        task_counts_by_family: summary.task_counts_by_family,
        task_counts_by_language: summary.task_counts_by_language,
        repository_counts_by_family: summary
            .repositories_by_family
            .into_iter()
            .map(|(family, repositories)| (family, repositories.len()))
            .collect(),
        task_shape_counts_by_family: summary
            .task_shapes_by_family
            .into_iter()
            .map(|(family, shapes)| (family, shapes.len()))
            .collect(),
        limitations: vec![
            "The receipt commits to private labels but contains no owner paths, line regions, prompts, or repository source.".into(),
            "Sealing proves artifact integrity and coverage strata, not retrieval quality or agent task success.".into(),
            "Blind status depends on evaluator access controls outside this repository; local file permissions are only a fail-closed hygiene check.".into(),
            policy.value.reclassification_rule.clone(),
        ],
    };
    write_new_json(&args.output, &receipt)
}

fn verify_public(args: &VerifyPublicArgs) -> Result<(), DynError> {
    let policy = load_json::<Policy>(&args.policy)?;
    validate_policy(&policy.value)?;
    let tasks = load_jsonl::<PublicTask>(&args.tasks)?;
    let summary = validate_tasks(&policy.value, &tasks.value)?;
    let host = load_json::<HostProvenance>(&args.host)?;
    validate_host(&host.value)?;
    let receipt = load_json::<SealReceipt>(&args.receipt)?;
    validate_receipt(&receipt.value, &policy, &tasks, &host, &summary)
}

fn validate_policy(policy: &Policy) -> Result<(), DynError> {
    require_schema(policy.schema_version, "policy")?;
    if policy.dataset_kind != DATASET_KIND {
        return Err(format!("policy dataset_kind must be {DATASET_KIND}").into());
    }
    validate_nonempty_bounded(&policy.frozen_at, "frozen_at", 128)?;
    validate_revision(&policy.baseline_revision, "baseline_revision")?;
    if policy.tokenizer != TOKENIZER {
        return Err(format!("policy tokenizer must be {TOKENIZER}").into());
    }
    let expected = REQUIRED_FAMILIES
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    if policy.task_families != expected {
        return Err("policy task_families must contain the frozen ordered family catalog".into());
    }

    let coverage = &policy.coverage;
    if coverage.minimum_tasks < REQUIRED_FAMILIES.len()
        || coverage.minimum_tasks > MAX_RECORDS
        || coverage.minimum_tasks_per_family == 0
        || coverage.minimum_repositories_per_family < 2
        || coverage.minimum_languages_per_family < 2
        || coverage.minimum_task_shapes_per_family < 2
    {
        return Err("policy coverage bounds are incomplete or unsafe".into());
    }
    if coverage.minimum_tasks_per_family * REQUIRED_FAMILIES.len() > coverage.minimum_tasks {
        return Err("minimum_tasks cannot be smaller than the required per-family total".into());
    }

    let execution = &policy.execution;
    for (name, value) in [
        ("attempts_per_arm", execution.attempts_per_arm),
        (
            "maximum_tool_calls_per_attempt",
            execution.maximum_tool_calls_per_attempt,
        ),
        (
            "maximum_wall_time_seconds_per_attempt",
            execution.maximum_wall_time_seconds_per_attempt,
        ),
        (
            "maximum_source_tokens_per_retrieval",
            execution.maximum_source_tokens_per_retrieval,
        ),
    ] {
        if value == 0 {
            return Err(format!("execution {name} must be positive").into());
        }
    }
    if execution.maximum_source_tokens_per_retrieval > MAX_TOKEN_BUDGET {
        return Err("execution source-token bound exceeds the harness maximum".into());
    }
    for (name, value) in [
        ("policy_id", execution.policy_id.as_str()),
        ("network_policy", execution.network_policy.as_str()),
        ("workspace_policy", execution.workspace_policy.as_str()),
        ("validation_policy", execution.validation_policy.as_str()),
    ] {
        validate_nonempty_bounded(value, name, 256)?;
    }

    validate_promotion(&policy.promotion)?;
    let required = [
        "task_source",
        "repository_revision",
        "label_method",
        "evaluator_revision",
        "harness_revision",
        "toolchain",
        "host",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect::<BTreeSet<_>>();
    if policy
        .required_provenance
        .iter()
        .cloned()
        .collect::<BTreeSet<_>>()
        != required
        || policy.required_provenance.len() != required.len()
    {
        return Err("policy required_provenance must match the frozen catalog".into());
    }
    validate_nonempty_bounded(
        &policy.reclassification_rule,
        "reclassification_rule",
        2_048,
    )
}

fn validate_promotion(promotion: &PromotionPolicy) -> Result<(), DynError> {
    if !promotion.confidence_level.is_finite()
        || !(0.5..1.0).contains(&promotion.confidence_level)
        || promotion.bootstrap_resamples < 1_000
    {
        return Err("promotion statistical policy is incomplete".into());
    }
    for (name, value, minimum, maximum) in [
        (
            "minimum_task_success_delta",
            promotion.minimum_task_success_delta,
            -1.0,
            1.0,
        ),
        (
            "maximum_global_recall_regression",
            promotion.maximum_global_recall_regression,
            0.0,
            1.0,
        ),
        (
            "maximum_family_recall_regression",
            promotion.maximum_family_recall_regression,
            0.0,
            1.0,
        ),
        (
            "maximum_dead_end_source_regression",
            promotion.maximum_dead_end_source_regression,
            0.0,
            1.0,
        ),
        (
            "maximum_reread_source_regression",
            promotion.maximum_reread_source_regression,
            0.0,
            1.0,
        ),
        (
            "maximum_provider_input_regression",
            promotion.maximum_provider_input_regression,
            0.0,
            1.0,
        ),
        (
            "maximum_warm_p95_latency_regression",
            promotion.maximum_warm_p95_latency_regression,
            0.0,
            1.0,
        ),
        (
            "maximum_peak_rss_regression",
            promotion.maximum_peak_rss_regression,
            0.0,
            1.0,
        ),
        (
            "maximum_database_size_regression",
            promotion.maximum_database_size_regression,
            0.0,
            1.0,
        ),
    ] {
        if !value.is_finite() || value < minimum || value > maximum {
            return Err(format!("promotion {name} is outside [{minimum}, {maximum}]").into());
        }
    }
    Ok(())
}

fn validate_tasks(policy: &Policy, tasks: &[PublicTask]) -> Result<CoverageSummary, DynError> {
    if tasks.len() < policy.coverage.minimum_tasks || tasks.len() > MAX_RECORDS {
        return Err(format!(
            "task count {} does not satisfy [{}, {MAX_RECORDS}]",
            tasks.len(),
            policy.coverage.minimum_tasks
        )
        .into());
    }
    let allowed_families = policy
        .task_families
        .iter()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    let mut previous_id: Option<&str> = None;
    let mut summary = CoverageSummary::default();
    for task in tasks {
        require_schema(task.schema_version, "task")?;
        validate_task_id(&task.id)?;
        if previous_id.is_some_and(|previous| previous >= task.id.as_str()) {
            return Err("public tasks must have unique IDs in ascending order".into());
        }
        previous_id = Some(&task.id);
        validate_nonempty_bounded(&task.prompt, "task prompt", MAX_PROMPT_BYTES)?;
        if !allowed_families.contains(task.task_family.as_str()) {
            return Err(format!("task {} has unknown task_family", task.id).into());
        }
        validate_repository(&task.repository)?;
        if task.languages.is_empty() || task.languages.len() > 4 {
            return Err(format!("task {} requires one to four languages", task.id).into());
        }
        let mut language_set = BTreeSet::new();
        for language in &task.languages {
            validate_tag(language, "language")?;
            if !language_set.insert(language.as_str()) {
                return Err(format!("task {} repeats a language", task.id).into());
            }
            *summary
                .task_counts_by_language
                .entry(language.clone())
                .or_default() += 1;
            summary
                .languages_by_family
                .entry(task.task_family.clone())
                .or_default()
                .insert(language.clone());
        }
        validate_tag(&task.task_shape, "task_shape")?;
        if task.token_budget == 0
            || task.token_budget > policy.execution.maximum_source_tokens_per_retrieval
        {
            return Err(format!("task {} token_budget is outside policy", task.id).into());
        }
        if task.executor_policy_id != policy.execution.policy_id {
            return Err(format!("task {} executor policy is not frozen policy", task.id).into());
        }
        validate_success_validator(&task.id, &task.success_validator)?;
        validate_task_provenance(&task.id, &task.provenance)?;

        *summary
            .task_counts_by_family
            .entry(task.task_family.clone())
            .or_default() += 1;
        summary
            .repositories_by_family
            .entry(task.task_family.clone())
            .or_default()
            .insert(task.repository.clone());
        summary
            .task_shapes_by_family
            .entry(task.task_family.clone())
            .or_default()
            .insert(task.task_shape.clone());
    }

    for family in &policy.task_families {
        let tasks = summary
            .task_counts_by_family
            .get(family)
            .copied()
            .unwrap_or_default();
        let repositories = summary
            .repositories_by_family
            .get(family)
            .map_or(0, BTreeSet::len);
        let languages = summary
            .languages_by_family
            .get(family)
            .map_or(0, BTreeSet::len);
        let shapes = summary
            .task_shapes_by_family
            .get(family)
            .map_or(0, BTreeSet::len);
        if tasks < policy.coverage.minimum_tasks_per_family
            || repositories < policy.coverage.minimum_repositories_per_family
            || languages < policy.coverage.minimum_languages_per_family
            || shapes < policy.coverage.minimum_task_shapes_per_family
        {
            return Err(format!(
                "family {family} coverage is incomplete: tasks={tasks}, repositories={repositories}, languages={languages}, task_shapes={shapes}"
            )
            .into());
        }
    }
    Ok(summary)
}

fn validate_labels(tasks: &[PublicTask], labels: &[PrivateLabel]) -> Result<(), DynError> {
    if labels.len() != tasks.len() {
        return Err("private labels must cover every public task exactly once".into());
    }
    for (task, label) in tasks.iter().zip(labels) {
        require_schema(label.schema_version, "label")?;
        if label.task_id != task.id {
            return Err("private labels must use the same ascending task order".into());
        }
        require_hash(
            &label.task_blake3,
            &hash(&serde_json::to_vec(task)?),
            "label task binding",
        )?;
        validate_nonempty_bounded(&label.label_method, "label_method", 256)?;
        if label.relevant_files.is_empty() {
            return Err(format!("label {} has no relevant files", label.task_id).into());
        }
        let mut files = BTreeSet::new();
        for path in &label.relevant_files {
            validate_relative_path(path)?;
            if !files.insert(path.as_str()) {
                return Err(format!("label {} repeats a relevant file", label.task_id).into());
            }
        }
        if label.relevant_regions.is_empty() {
            return Err(format!("label {} has no relevant regions", label.task_id).into());
        }
        let mut regions = BTreeSet::new();
        for region in &label.relevant_regions {
            validate_relative_path(&region.path)?;
            if !files.contains(region.path.as_str()) {
                return Err(
                    format!("label {} region path is not a relevant file", label.task_id).into(),
                );
            }
            if region.start_line == 0 || region.end_line < region.start_line {
                return Err(format!("label {} has an invalid line region", label.task_id).into());
            }
            if !regions.insert((&region.path, region.start_line, region.end_line)) {
                return Err(format!("label {} repeats a line region", label.task_id).into());
            }
        }
    }
    Ok(())
}

fn validate_receipt(
    receipt: &SealReceipt,
    policy: &Loaded<Policy>,
    tasks: &Loaded<Vec<PublicTask>>,
    host: &Loaded<HostProvenance>,
    summary: &CoverageSummary,
) -> Result<(), DynError> {
    require_schema(receipt.schema_version, "receipt")?;
    if receipt.dataset_kind != DATASET_KIND
        || receipt.state != SEALED_STATE
        || receipt.frozen_at != policy.value.frozen_at
        || receipt.baseline_revision != policy.value.baseline_revision
        || receipt.tokenizer != policy.value.tokenizer
    {
        return Err("receipt frozen identity does not match policy".into());
    }
    validate_revision(&receipt.candidate_revision, "receipt candidate_revision")?;
    if receipt.candidate_revision == receipt.baseline_revision {
        return Err("receipt candidate_revision must differ from baseline_revision".into());
    }
    validate_revision(&receipt.harness_revision, "receipt harness_revision")?;
    validate_revision(&receipt.evaluator_revision, "receipt evaluator_revision")?;
    validate_nonempty_bounded(&receipt.toolchain, "receipt toolchain", 256)?;
    require_hash(&receipt.policy_blake3, &hash(&policy.bytes), "policy")?;
    require_hash(&receipt.tasks_blake3, &hash(&tasks.bytes), "tasks")?;
    require_hash(&receipt.host_blake3, &hash(&host.bytes), "host")?;
    validate_blake3(&receipt.labels_blake3, "labels commitment")?;
    if receipt.task_count != tasks.value.len()
        || receipt.label_count != tasks.value.len()
        || receipt.task_counts_by_family != summary.task_counts_by_family
        || receipt.task_counts_by_language != summary.task_counts_by_language
    {
        return Err("receipt task aggregates do not match public tasks".into());
    }
    let repository_counts = summary
        .repositories_by_family
        .iter()
        .map(|(family, repositories)| (family.clone(), repositories.len()))
        .collect::<BTreeMap<_, _>>();
    let shape_counts = summary
        .task_shapes_by_family
        .iter()
        .map(|(family, shapes)| (family.clone(), shapes.len()))
        .collect::<BTreeMap<_, _>>();
    if receipt.repository_counts_by_family != repository_counts
        || receipt.task_shape_counts_by_family != shape_counts
    {
        return Err("receipt family aggregates do not match public tasks".into());
    }
    if receipt.limitations.len() < 4
        || receipt
            .limitations
            .iter()
            .any(|limitation| limitation.trim().is_empty())
        || !receipt
            .limitations
            .iter()
            .any(|limitation| limitation == &policy.value.reclassification_rule)
    {
        return Err("receipt limitations omit the reclassification boundary".into());
    }
    Ok(())
}

fn validate_repository(repository: &RepositoryIdentity) -> Result<(), DynError> {
    if !repository.url.starts_with("https://") || repository.url.len() > 2_048 {
        return Err("repository URL must be a bounded HTTPS URL".into());
    }
    validate_revision(&repository.revision, "repository revision")
}

fn validate_success_validator(task_id: &str, validator: &SuccessValidator) -> Result<(), DynError> {
    validate_tag(&validator.kind, "success validator kind")?;
    if validator.commands.is_empty() || validator.commands.len() > MAX_VALIDATOR_COMMANDS {
        return Err(format!("task {task_id} has an invalid validator command count").into());
    }
    for command in &validator.commands {
        validate_nonempty_bounded(command, "validator command", MAX_COMMAND_BYTES)?;
    }
    if validator.timeout_seconds == 0 || validator.timeout_seconds > 3_600 {
        return Err(format!("task {task_id} validator timeout is outside bounds").into());
    }
    Ok(())
}

fn validate_task_provenance(task_id: &str, provenance: &TaskProvenance) -> Result<(), DynError> {
    for (name, value, maximum) in [
        ("task_source", provenance.task_source.as_str(), 256),
        ("source_url", provenance.source_url.as_str(), 2_048),
        ("source_revision", provenance.source_revision.as_str(), 256),
        ("captured_at", provenance.captured_at.as_str(), 128),
    ] {
        validate_nonempty_bounded(value, name, maximum)
            .map_err(|error| format!("task {task_id}: {error}"))?;
    }
    if !provenance.source_url.starts_with("https://") {
        return Err(format!("task {task_id} provenance URL must use HTTPS").into());
    }
    Ok(())
}

fn validate_host(host: &HostProvenance) -> Result<(), DynError> {
    require_schema(host.schema_version, "host")?;
    for (name, value) in [
        ("host os", host.os.as_str()),
        ("host architecture", host.architecture.as_str()),
        ("host runner", host.runner.as_str()),
        ("host rustc", host.rustc.as_str()),
    ] {
        validate_nonempty_bounded(value, name, 256)?;
    }
    Ok(())
}

fn validate_task_id(value: &str) -> Result<(), DynError> {
    if value.is_empty()
        || value.len() > 128
        || !value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"-_.".contains(&byte)
        })
    {
        return Err("task id must be bounded lowercase ASCII".into());
    }
    Ok(())
}

fn validate_tag(value: &str, name: &str) -> Result<(), DynError> {
    if value.is_empty()
        || value.len() > 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
    {
        return Err(format!("{name} must be bounded snake_case ASCII").into());
    }
    Ok(())
}

fn validate_relative_path(value: &str) -> Result<(), DynError> {
    if value.is_empty() || value.len() > 4_096 || value.contains('\\') || value.contains('\0') {
        return Err("label path must be a bounded slash-relative path".into());
    }
    let path = Path::new(value);
    if path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err("label path must not contain absolute or traversal components".into());
    }
    Ok(())
}

fn require_schema(actual: u32, description: &str) -> Result<(), DynError> {
    if actual != SCHEMA_VERSION {
        return Err(format!("{description} schema_version must be {SCHEMA_VERSION}").into());
    }
    Ok(())
}

fn validate_revision(value: &str, description: &str) -> Result<(), DynError> {
    if value.len() != 40
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
    {
        return Err(format!("{description} must be a lowercase 40-character Git revision").into());
    }
    Ok(())
}

fn validate_blake3(value: &str, description: &str) -> Result<(), DynError> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
    {
        return Err(format!("{description} must be a lowercase BLAKE3 hash").into());
    }
    Ok(())
}

fn require_hash(actual: &str, expected: &str, description: &str) -> Result<(), DynError> {
    validate_blake3(actual, description)?;
    if actual != expected {
        return Err(format!("{description} hash does not match").into());
    }
    Ok(())
}

fn validate_nonempty_bounded(
    value: &str,
    description: &str,
    maximum_bytes: usize,
) -> Result<(), DynError> {
    if value.trim().is_empty() || value.len() > maximum_bytes {
        return Err(
            format!("{description} must be nonempty and at most {maximum_bytes} bytes").into(),
        );
    }
    Ok(())
}

fn load_json<T: DeserializeOwned>(path: &Path) -> Result<Loaded<T>, DynError> {
    let bytes = read_bounded(path)?;
    let value = serde_json::from_slice(&bytes)
        .map_err(|error| format!("invalid JSON {}: {error}", path.display()))?;
    Ok(Loaded { bytes, value })
}

fn load_jsonl<T: DeserializeOwned>(path: &Path) -> Result<Loaded<Vec<T>>, DynError> {
    let bytes = read_bounded(path)?;
    let mut values = Vec::new();
    for (index, line) in bytes.split(|byte| *byte == b'\n').enumerate() {
        if line.is_empty() {
            continue;
        }
        if values.len() == MAX_RECORDS {
            return Err(format!("{} exceeds {MAX_RECORDS} records", path.display()).into());
        }
        values.push(serde_json::from_slice(line).map_err(|error| {
            format!(
                "invalid JSONL {} line {}: {error}",
                path.display(),
                index + 1
            )
        })?);
    }
    if values.is_empty() {
        return Err(format!("{} contains no records", path.display()).into());
    }
    Ok(Loaded {
        bytes,
        value: values,
    })
}

fn read_bounded(path: &Path) -> Result<Vec<u8>, DynError> {
    let metadata = fs::metadata(path)?;
    if !metadata.is_file() || metadata.len() > MAX_ARTIFACT_BYTES {
        return Err(format!(
            "{} must be a file no larger than {MAX_ARTIFACT_BYTES} bytes",
            path.display()
        )
        .into());
    }
    Ok(fs::read(path)?)
}

#[cfg(unix)]
fn ensure_private_file(path: &Path) -> Result<(), DynError> {
    use std::os::unix::fs::PermissionsExt;

    let mode = fs::metadata(path)?.permissions().mode();
    if mode & 0o077 != 0 {
        return Err(format!(
            "private label file {} must not grant group or other permissions",
            path.display()
        )
        .into());
    }
    Ok(())
}

#[cfg(not(unix))]
fn ensure_private_file(path: &Path) -> Result<(), DynError> {
    if !fs::metadata(path)?.is_file() {
        return Err(format!("private label path {} is not a file", path.display()).into());
    }
    Ok(())
}

fn ensure_output_absent(path: &Path) -> Result<(), DynError> {
    if path.exists() {
        return Err(format!("refusing to overwrite {}", path.display()).into());
    }
    Ok(())
}

fn write_new_json<T: Serialize>(path: &Path, value: &T) -> Result<(), DynError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut output = OpenOptions::new().write(true).create_new(true).open(path)?;
    serde_json::to_writer_pretty(&mut output, value)?;
    output.write_all(b"\n")?;
    output.sync_all()?;
    Ok(())
}

fn hash(bytes: &[u8]) -> String {
    blake3::hash(bytes).to_hex().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    const REVISION: &str = "1111111111111111111111111111111111111111";

    fn policy(minimum_tasks: usize) -> Policy {
        Policy {
            schema_version: SCHEMA_VERSION,
            dataset_kind: DATASET_KIND.into(),
            frozen_at: "2026-07-28T00:00:00Z".into(),
            baseline_revision: REVISION.into(),
            tokenizer: TOKENIZER.into(),
            task_families: REQUIRED_FAMILIES.into_iter().map(str::to_owned).collect(),
            coverage: CoveragePolicy {
                minimum_tasks,
                minimum_tasks_per_family: 1,
                minimum_repositories_per_family: 2,
                minimum_languages_per_family: 2,
                minimum_task_shapes_per_family: 2,
            },
            execution: ExecutionPolicy {
                policy_id: "paired".into(),
                attempts_per_arm: 2,
                maximum_tool_calls_per_attempt: 10,
                maximum_wall_time_seconds_per_attempt: 60,
                maximum_source_tokens_per_retrieval: 1_000,
                network_policy: "validator_only".into(),
                workspace_policy: "fresh".into(),
                validation_policy: "commands".into(),
            },
            promotion: PromotionPolicy {
                confidence_level: 0.95,
                bootstrap_resamples: 1_000,
                bootstrap_seed: 7,
                minimum_task_success_delta: 0.0,
                maximum_global_recall_regression: 0.0,
                maximum_family_recall_regression: 0.0,
                maximum_dead_end_source_regression: 0.0,
                maximum_reread_source_regression: 0.0,
                maximum_provider_input_regression: 0.05,
                maximum_warm_p95_latency_regression: 0.1,
                maximum_peak_rss_regression: 0.1,
                maximum_database_size_regression: 0.1,
            },
            required_provenance: [
                "task_source",
                "repository_revision",
                "label_method",
                "evaluator_revision",
                "harness_revision",
                "toolchain",
                "host",
            ]
            .into_iter()
            .map(str::to_owned)
            .collect(),
            reclassification_rule: "labels consumed means diagnostic only".into(),
        }
    }

    fn task(id: usize, family: &str, variant: usize) -> PublicTask {
        PublicTask {
            schema_version: SCHEMA_VERSION,
            id: format!("task-{id:03}"),
            prompt: format!("diagnose public symptom {id}"),
            task_family: family.into(),
            repository: RepositoryIdentity {
                url: format!("https://example.com/repository-{}.git", variant % 2),
                revision: if variant.is_multiple_of(2) {
                    REVISION.into()
                } else {
                    "2222222222222222222222222222222222222222".into()
                },
            },
            languages: vec![
                if variant.is_multiple_of(2) {
                    "rust"
                } else {
                    "python"
                }
                .into(),
                if variant.is_multiple_of(2) {
                    "python"
                } else {
                    "rust"
                }
                .into(),
            ],
            task_shape: if variant.is_multiple_of(2) {
                "behavioral"
            } else {
                "structural"
            }
            .into(),
            token_budget: 1_000,
            executor_policy_id: "paired".into(),
            success_validator: SuccessValidator {
                kind: "command".into(),
                commands: vec!["cargo test-focused owner".into()],
                timeout_seconds: 60,
            },
            provenance: TaskProvenance {
                task_source: "public_issue".into(),
                source_url: format!("https://example.com/issues/{id}"),
                source_revision: REVISION.into(),
                captured_at: "2026-07-28T00:00:00Z".into(),
            },
        }
    }

    fn complete_tasks() -> Vec<PublicTask> {
        REQUIRED_FAMILIES
            .iter()
            .enumerate()
            .flat_map(|(family_index, family)| {
                [0, 1]
                    .into_iter()
                    .map(move |variant| task(family_index * 2 + variant, family, variant))
            })
            .collect()
    }

    fn labels(tasks: &[PublicTask]) -> Vec<PrivateLabel> {
        tasks
            .iter()
            .map(|task| PrivateLabel {
                schema_version: SCHEMA_VERSION,
                task_id: task.id.clone(),
                task_blake3: hash(&serde_json::to_vec(task).expect("serialize task")),
                label_method: "independent_evaluator_v1".into(),
                relevant_files: vec!["src/owner.rs".into()],
                relevant_regions: vec![RelevantRegion {
                    path: "src/owner.rs".into(),
                    start_line: 10,
                    end_line: 20,
                }],
            })
            .collect()
    }

    #[test]
    fn family_coverage_requires_independent_repository_language_and_shape_evidence() {
        let tasks = complete_tasks();
        let policy = policy(tasks.len());
        let summary = validate_tasks(&policy, &tasks).expect("complete coverage");
        assert_eq!(summary.task_counts_by_family.len(), REQUIRED_FAMILIES.len());

        let mut incomplete = tasks.clone();
        incomplete[1].repository = incomplete[0].repository.clone();
        let error = validate_tasks(&policy, &incomplete)
            .expect_err("single repository family")
            .to_string();
        assert!(error.contains("coverage is incomplete"));
    }

    #[test]
    fn labels_bind_every_task_without_publishing_paths() {
        let tasks = complete_tasks();
        let labels = labels(&tasks);
        validate_labels(&tasks, &labels).expect("bound labels");

        let mut corrupt = labels.clone();
        corrupt[0].task_blake3 = "0".repeat(64);
        assert!(
            validate_labels(&tasks, &corrupt)
                .expect_err("corrupt binding")
                .to_string()
                .contains("does not match")
        );
    }

    #[test]
    fn public_verification_rejects_changed_artifacts_and_aggregates() {
        let tasks = complete_tasks();
        let frozen_policy = policy(tasks.len());
        let summary = validate_tasks(&frozen_policy, &tasks).expect("summary");
        let policy_bytes = serde_json::to_vec(&frozen_policy).expect("policy");
        let task_bytes = tasks
            .iter()
            .map(|task| serde_json::to_string(task).expect("task"))
            .collect::<Vec<_>>()
            .join("\n")
            .into_bytes();
        let host = HostProvenance {
            schema_version: SCHEMA_VERSION,
            os: "linux".into(),
            architecture: "x86_64".into(),
            runner: "test".into(),
            rustc: "rustc 1.95".into(),
        };
        let host_bytes = serde_json::to_vec(&host).expect("host");
        let labels = labels(&tasks);
        let mut receipt = SealReceipt {
            schema_version: SCHEMA_VERSION,
            dataset_kind: DATASET_KIND.into(),
            state: SEALED_STATE.into(),
            frozen_at: frozen_policy.frozen_at.clone(),
            baseline_revision: frozen_policy.baseline_revision.clone(),
            candidate_revision: "2222222222222222222222222222222222222222".into(),
            harness_revision: REVISION.into(),
            evaluator_revision: REVISION.into(),
            toolchain: "rustc 1.95".into(),
            tokenizer: TOKENIZER.into(),
            policy_blake3: hash(&policy_bytes),
            tasks_blake3: hash(&task_bytes),
            labels_blake3: hash(&serde_json::to_vec(&labels).expect("labels")),
            host_blake3: hash(&host_bytes),
            task_count: tasks.len(),
            label_count: tasks.len(),
            task_counts_by_family: summary.task_counts_by_family.clone(),
            task_counts_by_language: summary.task_counts_by_language.clone(),
            repository_counts_by_family: summary
                .repositories_by_family
                .iter()
                .map(|(family, repositories)| (family.clone(), repositories.len()))
                .collect(),
            task_shape_counts_by_family: summary
                .task_shapes_by_family
                .iter()
                .map(|(family, shapes)| (family.clone(), shapes.len()))
                .collect(),
            limitations: vec![
                "no labels".into(),
                "no quality claim".into(),
                "external access controls".into(),
                frozen_policy.reclassification_rule.clone(),
            ],
        };
        validate_receipt(
            &receipt,
            &Loaded {
                bytes: policy_bytes,
                value: frozen_policy,
            },
            &Loaded {
                bytes: task_bytes,
                value: tasks,
            },
            &Loaded {
                bytes: host_bytes,
                value: host,
            },
            &summary,
        )
        .expect("public verification");

        receipt.task_count += 1;
        assert!(
            validate_receipt(
                &receipt,
                &Loaded {
                    bytes: Vec::new(),
                    value: policy(22),
                },
                &Loaded {
                    bytes: Vec::new(),
                    value: complete_tasks(),
                },
                &Loaded {
                    bytes: Vec::new(),
                    value: HostProvenance {
                        schema_version: SCHEMA_VERSION,
                        os: "linux".into(),
                        architecture: "x86_64".into(),
                        runner: "test".into(),
                        rustc: "rustc 1.95".into(),
                    },
                },
                &summary,
            )
            .is_err()
        );
    }

    #[test]
    fn unsafe_paths_and_duplicate_task_order_fail_closed() {
        assert!(validate_relative_path("../secret.rs").is_err());
        assert!(validate_relative_path("/absolute.rs").is_err());

        let mut tasks = complete_tasks();
        tasks[1].id = tasks[0].id.clone();
        assert!(
            validate_tasks(&policy(tasks.len()), &tasks)
                .expect_err("duplicate task")
                .to_string()
                .contains("ascending order")
        );
    }

    #[test]
    fn seal_and_public_verification_round_trip_without_gold_paths() {
        let root = tempfile::tempdir().expect("temporary directory");
        let policy_path = root.path().join("policy.json");
        let tasks_path = root.path().join("tasks.jsonl");
        let labels_path = root.path().join("labels.jsonl");
        let host_path = root.path().join("host.json");
        let receipt_path = root.path().join("receipt.json");
        let tasks = complete_tasks();
        let frozen_policy = policy(tasks.len());
        let labels = labels(&tasks);
        let host = HostProvenance {
            schema_version: SCHEMA_VERSION,
            os: "linux".into(),
            architecture: "x86_64".into(),
            runner: "test".into(),
            rustc: "rustc 1.95".into(),
        };

        fs::write(
            &policy_path,
            serde_json::to_vec(&frozen_policy).expect("policy"),
        )
        .expect("write policy");
        fs::write(
            &tasks_path,
            tasks
                .iter()
                .map(|task| serde_json::to_string(task).expect("task"))
                .collect::<Vec<_>>()
                .join("\n"),
        )
        .expect("write tasks");
        fs::write(
            &labels_path,
            labels
                .iter()
                .map(|label| serde_json::to_string(label).expect("label"))
                .collect::<Vec<_>>()
                .join("\n"),
        )
        .expect("write labels");
        fs::write(&host_path, serde_json::to_vec(&host).expect("host")).expect("write host");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            fs::set_permissions(&labels_path, fs::Permissions::from_mode(0o600))
                .expect("private labels");
        }

        seal(&SealArgs {
            policy: policy_path.clone(),
            tasks: tasks_path.clone(),
            labels: labels_path,
            host: host_path.clone(),
            candidate_revision: "2222222222222222222222222222222222222222".into(),
            harness_revision: REVISION.into(),
            evaluator_revision: REVISION.into(),
            toolchain: "rustc 1.95".into(),
            output: receipt_path.clone(),
        })
        .expect("seal");
        verify_public(&VerifyPublicArgs {
            policy: policy_path,
            tasks: tasks_path,
            host: host_path,
            receipt: receipt_path.clone(),
        })
        .expect("verify public");

        let receipt = fs::read_to_string(receipt_path).expect("receipt");
        assert!(!receipt.contains("src/owner.rs"));
        assert!(!receipt.contains("diagnose public symptom"));
        assert!(receipt.contains(SEALED_STATE));
    }
}
