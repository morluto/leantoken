#![allow(dead_code)]

use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    env,
    error::Error,
    fs::{self, OpenOptions},
    io::{self, Read, Write},
    path::{Component, Path, PathBuf},
    process::{Command, Output, Stdio},
    time::{Duration, Instant},
};

use clap::{Args, Parser, Subcommand};
use leantoken::{ContextResponse, IndexResponse, tokens::Tokenizer};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use wait_timeout::ChildExt;

const CONTRACT_SCHEMA_VERSION: u32 = 1;
const RECEIPT_SCHEMA_VERSION: u32 = 1;
const DECISION_SCHEMA_VERSION: u32 = 1;
const FROZEN_DATASET_KIND: &str = "swe_bench_multilingual_development";
const FROZEN_LABEL_METHOD: &str = "base_diff_removed_or_insertion_context_v1";
const EXPECTED_TASKS: usize = 54;
const EXPECTED_LANGUAGES: [&str; 9] = [
    "c",
    "cpp",
    "go",
    "java",
    "javascript",
    "typescript",
    "php",
    "ruby",
    "rust",
];
const EXPECTED_TASKS_PER_LANGUAGE: usize = 6;
const EXPECTED_EXACT_TASKS: usize = 27;
const FROZEN_SOURCE_TOKEN_BUDGET: usize = 2_000;
const FROZEN_TOKENIZER: Tokenizer = Tokenizer::Cl100kBase;
const FROZEN_MINIMUM_STRATUM_TASKS: usize = 6;
const FROZEN_REQUIRED_IMPROVED_STRATA: usize = 2;
const FROZEN_MAXIMUM_COMPLETE_COST_REGRESSION: f64 = 0.05;
const HASH_HEX_LEN: usize = 64;
const MAX_QUERY_BYTES: usize = 64 * 1024;
const MAX_COMMAND_OUTPUT_BYTES: usize = 4 * 1024 * 1024;

type DynError = Box<dyn Error + Send + Sync>;

#[derive(Debug, Parser)]
#[command(about = "Materialize and run the sealed multilingual retrieval development gate")]
struct Cli {
    #[command(subcommand)]
    command: GateCommand,
}

#[derive(Debug, Subcommand)]
enum GateCommand {
    /// Join public tasks to sealed labels and create a private ranked-region manifest.
    Materialize(MaterializeArgs),
    /// Run one frozen external LeanToken binary twice per public task.
    Predict(PredictArgs),
    /// Apply the preregistered external Gate A retrieval and cost criteria.
    Decide(DecideArgs),
}

#[derive(Debug, Args)]
struct MaterializeArgs {
    #[arg(long)]
    tasks: PathBuf,
    #[arg(long)]
    labels: PathBuf,
    #[arg(long)]
    expected_tasks_blake3: String,
    #[arg(long)]
    expected_labels_blake3: String,
    #[arg(long)]
    output: PathBuf,
    #[arg(long)]
    receipt_output: PathBuf,
    #[command(flatten)]
    evaluator: EvaluatorIdentityArgs,
}

#[derive(Debug, Args)]
struct PredictArgs {
    #[arg(long)]
    tasks: PathBuf,
    #[arg(long)]
    expected_tasks_blake3: String,
    /// Opaque ranked manifest whose hash binds predictions to evaluator labels.
    #[arg(long)]
    manifest: PathBuf,
    #[arg(long)]
    runtime_binary: PathBuf,
    #[arg(long)]
    runtime_binary_blake3: String,
    #[arg(long)]
    runtime_repository: PathBuf,
    #[arg(long)]
    runtime_revision: String,
    #[arg(long)]
    arm_id: String,
    /// Persistent bare partial-clone cache shared by frozen arms.
    #[arg(long)]
    repository_cache: PathBuf,
    /// New arm-specific scratch root for worktrees and SQLite indexes.
    #[arg(long)]
    work_root: PathBuf,
    #[arg(long)]
    output: PathBuf,
    #[arg(long)]
    receipt_output: PathBuf,
    #[arg(long, default_value_t = 2)]
    repetitions: usize,
    #[arg(long, default_value_t = 1_800)]
    command_timeout_seconds: u64,
    #[command(flatten)]
    evaluator: EvaluatorIdentityArgs,
}

#[derive(Debug, Args)]
struct DecideArgs {
    #[arg(long)]
    manifest: PathBuf,
    #[arg(long)]
    baseline_predictions: PathBuf,
    #[arg(long)]
    candidate_predictions: PathBuf,
    #[arg(long)]
    ranked_evaluator_binary: PathBuf,
    #[arg(long)]
    ranked_evaluator_binary_blake3: String,
    #[arg(long)]
    baseline_report_output: PathBuf,
    #[arg(long)]
    candidate_report_output: PathBuf,
    #[arg(long)]
    output: PathBuf,
    #[arg(long, default_value_t = 300)]
    command_timeout_seconds: u64,
    #[command(flatten)]
    evaluator: EvaluatorIdentityArgs,
}

#[derive(Debug, Args)]
struct EvaluatorIdentityArgs {
    #[arg(long)]
    evaluator_repository: PathBuf,
    #[arg(long)]
    evaluator_revision: String,
    #[arg(long)]
    evaluator_binary_blake3: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct DevelopmentTask {
    schema_version: u32,
    dataset_kind: String,
    task_id: String,
    repository: RepositorySpec,
    query: String,
    language: String,
    strata: DevelopmentStrata,
    budget: DevelopmentBudget,
    source_record_blake3: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct RepositorySpec {
    url: String,
    revision: String,
    path_style: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct DevelopmentStrata {
    exact_identifier: bool,
    task_shape: String,
    tags: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct DevelopmentBudget {
    kind: String,
    amount: usize,
    tokenizer: Tokenizer,
    token_count_exact: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct SealedLabel {
    schema_version: u32,
    task_id: String,
    source_record_blake3: String,
    label_method: String,
    core_regions: Vec<Region>,
    optional_regions: Vec<Region>,
    gold_patch_blake3: String,
    test_patch_blake3: String,
    unobservable_added_files: usize,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Region {
    path: String,
    start_line: usize,
    end_line: usize,
}

#[derive(Debug, Serialize)]
struct RankedTask {
    schema_version: u32,
    dataset_kind: String,
    task_id: String,
    repository: RepositorySpec,
    query: String,
    language: String,
    strata: RankedStrata,
    budget: RankedBudget,
    relevant_files: Vec<String>,
    core_regions: Vec<Region>,
    optional_regions: Vec<Region>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
struct RankedBudget {
    kind: String,
    amount: usize,
}

#[derive(Debug, Serialize)]
struct RankedStrata {
    repo_size_bucket: String,
    exact_identifier: bool,
    lexical_overlap_bucket: String,
    tags: Vec<String>,
}

#[derive(Debug, Serialize)]
struct MaterializeReceipt {
    schema_version: u32,
    operation: &'static str,
    evaluator: ArtifactIdentity,
    tasks_blake3: String,
    sealed_labels_blake3: String,
    ranked_manifest_blake3: String,
    tasks: usize,
    languages: BTreeMap<String, usize>,
    exact_identifier_tasks: usize,
    behavioral_tasks: usize,
    core_regions: usize,
    optional_regions: usize,
    label_method: String,
    output_private: bool,
}

#[derive(Debug, Clone, Serialize)]
struct ArtifactIdentity {
    revision: String,
    path: PathBuf,
    bytes: u64,
    blake3: String,
}

#[derive(Debug, Serialize)]
struct Prediction {
    schema_version: u32,
    task_id: String,
    manifest_blake3: String,
    repository_revision: String,
    budget: RankedBudget,
    tokenizer: Option<String>,
    complete_response_tokens: Option<usize>,
    source_tokens: Option<usize>,
    latency_ms: Option<f64>,
    index_generation: Option<u64>,
    regions: Vec<PredictedRegion>,
}

#[derive(Debug, Serialize)]
struct PredictedRegion {
    path: String,
    start_line: usize,
    end_line: usize,
    rank: usize,
    source: Option<String>,
    facet: Option<String>,
    score: Option<f64>,
    token_count: Option<usize>,
}

#[derive(Debug, Serialize)]
struct PredictionReceipt {
    schema_version: u32,
    operation: &'static str,
    arm_id: String,
    evaluator: ArtifactIdentity,
    runtime: RuntimeIdentity,
    tasks_blake3: String,
    manifest_blake3: String,
    predictions_blake3: String,
    repetitions: usize,
    command_timeout_seconds: u64,
    tokenizer: String,
    source_token_budget: usize,
    deterministic_responses: bool,
    tasks: Vec<TaskRunReceipt>,
}

#[derive(Debug, Serialize)]
struct RuntimeIdentity {
    repository: PathBuf,
    revision: String,
    binary: ArtifactIdentity,
}

#[derive(Debug, Serialize)]
struct TaskRunReceipt {
    task_id: String,
    repository_url: String,
    repository_revision: String,
    worktree_head: String,
    index_generation: u64,
    files_indexed: usize,
    files_skipped: usize,
    index_warning_count: usize,
    index_elapsed_ms: u128,
    index_response_blake3: String,
    index_artifact: PathBuf,
    context_elapsed_ms: Vec<u128>,
    response_blake3: String,
    context_artifact: PathBuf,
    complete_response_tokens: usize,
    source_tokens: usize,
    regions: usize,
    repetitions_identical: bool,
}

pub(crate) fn run() -> Result<(), DynError> {
    match Cli::parse().command {
        GateCommand::Materialize(args) => materialize(&args),
        GateCommand::Predict(args) => predict(&args),
        GateCommand::Decide(args) => decide(&args),
    }
}

fn materialize(args: &MaterializeArgs) -> Result<(), DynError> {
    validate_blake3(&args.expected_tasks_blake3, "expected tasks BLAKE3")?;
    validate_blake3(&args.expected_labels_blake3, "expected labels BLAKE3")?;
    let evaluator = verify_evaluator(&args.evaluator, Duration::from_secs(30))?;
    materialize_with_evaluator(args, evaluator)
}

fn materialize_with_evaluator(
    args: &MaterializeArgs,
    evaluator: ArtifactIdentity,
) -> Result<(), DynError> {
    ensure_output_absent(&args.output)?;
    ensure_output_absent(&args.receipt_output)?;

    let task_bytes = fs::read(&args.tasks)?;
    let label_bytes = fs::read(&args.labels)?;
    let tasks_blake3 = blake3_hex(&task_bytes);
    let labels_blake3 = blake3_hex(&label_bytes);
    require_hash(&tasks_blake3, &args.expected_tasks_blake3, "tasks")?;
    require_hash(
        &labels_blake3,
        &args.expected_labels_blake3,
        "sealed labels",
    )?;

    let tasks = parse_jsonl::<DevelopmentTask>(&task_bytes)?;
    let labels = parse_jsonl::<SealedLabel>(&label_bytes)?;
    let selection = validate_development_tasks(&tasks)?;
    let label_map = validate_labels(&labels)?;
    if task_bytes.len() > MAX_COMMAND_OUTPUT_BYTES || label_bytes.len() > MAX_COMMAND_OUTPUT_BYTES {
        return Err("development inputs exceed the frozen evaluator size bound".into());
    }

    let mut ranked = Vec::with_capacity(tasks.len());
    let mut core_regions = 0usize;
    let mut optional_regions = 0usize;
    let mut label_methods = BTreeSet::new();
    for task in tasks {
        let label = label_map
            .get(task.task_id.as_str())
            .ok_or_else(|| format!("missing sealed label for {}", task.task_id))?;
        if task.source_record_blake3 != label.source_record_blake3 {
            return Err(format!("task/label source binding differs for {}", task.task_id).into());
        }
        label_methods.insert(label.label_method.clone());
        core_regions += label.core_regions.len();
        optional_regions += label.optional_regions.len();
        let relevant_files = label
            .core_regions
            .iter()
            .map(|region| region.path.clone())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        let mut tags = task.strata.tags.clone();
        tags.push(format!("task_shape:{}", task.strata.task_shape));
        tags.sort();
        tags.dedup();
        ranked.push(RankedTask {
            schema_version: CONTRACT_SCHEMA_VERSION,
            dataset_kind: task.dataset_kind,
            task_id: task.task_id,
            repository: task.repository,
            query: task.query,
            language: task.language,
            strata: RankedStrata {
                repo_size_bucket: "unmeasured".into(),
                exact_identifier: task.strata.exact_identifier,
                lexical_overlap_bucket: "unmeasured".into(),
                tags,
            },
            budget: RankedBudget {
                kind: task.budget.kind,
                amount: task.budget.amount,
            },
            relevant_files,
            core_regions: label.core_regions.clone(),
            optional_regions: label.optional_regions.clone(),
        });
    }
    if label_map.len() != ranked.len() {
        return Err("sealed labels contain task IDs outside the public selection".into());
    }
    let label_method = one_value(label_methods, "label method")?;
    let manifest_bytes = serialize_jsonl(&ranked)?;
    let manifest_blake3 = blake3_hex(&manifest_bytes);
    let receipt = MaterializeReceipt {
        schema_version: RECEIPT_SCHEMA_VERSION,
        operation: "materialize",
        evaluator,
        tasks_blake3,
        sealed_labels_blake3: labels_blake3,
        ranked_manifest_blake3: manifest_blake3,
        tasks: ranked.len(),
        languages: selection.languages,
        exact_identifier_tasks: selection.exact,
        behavioral_tasks: selection.behavioral,
        core_regions,
        optional_regions,
        label_method,
        output_private: true,
    };
    let receipt_bytes = serde_json::to_vec_pretty(&receipt)?;
    write_new(&args.output, &manifest_bytes, true)?;
    write_new(&args.receipt_output, &receipt_bytes, true)?;
    Ok(())
}

struct SelectionSummary {
    languages: BTreeMap<String, usize>,
    exact: usize,
    behavioral: usize,
}

fn validate_development_tasks(tasks: &[DevelopmentTask]) -> Result<SelectionSummary, DynError> {
    if tasks.len() != EXPECTED_TASKS {
        return Err(format!(
            "expected {EXPECTED_TASKS} public tasks, got {}",
            tasks.len()
        )
        .into());
    }
    let mut ids = BTreeSet::new();
    let mut languages = BTreeMap::new();
    let mut exact = 0usize;
    for task in tasks {
        if task.schema_version != CONTRACT_SCHEMA_VERSION {
            return Err(format!("{} has unsupported task schema", task.task_id).into());
        }
        if task.dataset_kind != FROZEN_DATASET_KIND {
            return Err(format!("{} has an unexpected dataset kind", task.task_id).into());
        }
        if !ids.insert(task.task_id.as_str()) {
            return Err(format!("duplicate public task ID {}", task.task_id).into());
        }
        validate_task_id(&task.task_id)?;
        validate_repository(&task.repository)?;
        validate_blake3(&task.source_record_blake3, "source record BLAKE3")?;
        if task.query.trim().is_empty() || task.query.len() > MAX_QUERY_BYTES {
            return Err(format!("{} has an invalid query size", task.task_id).into());
        }
        if !EXPECTED_LANGUAGES.contains(&task.language.as_str()) {
            return Err(
                format!("{} has unexpected language {}", task.task_id, task.language).into(),
            );
        }
        *languages.entry(task.language.clone()).or_insert(0) += 1;
        exact += usize::from(task.strata.exact_identifier);
        let expected_shape = if task.strata.exact_identifier {
            "exact_identifier"
        } else {
            "behavioral"
        };
        if task.strata.task_shape != expected_shape {
            return Err(format!("{} task shape disagrees with exact stratum", task.task_id).into());
        }
        if task.strata.tags != ["external", "patch_ground_truth"] {
            return Err(format!("{} differs from the frozen task tags", task.task_id).into());
        }
        if task.budget.kind != "source_tokens"
            || task.budget.amount != FROZEN_SOURCE_TOKEN_BUDGET
            || task.budget.tokenizer != FROZEN_TOKENIZER
            || !task.budget.token_count_exact
        {
            return Err(format!("{} differs from the frozen token budget", task.task_id).into());
        }
    }
    if languages.len() != EXPECTED_LANGUAGES.len()
        || EXPECTED_LANGUAGES
            .iter()
            .any(|language| languages.get(*language) != Some(&EXPECTED_TASKS_PER_LANGUAGE))
    {
        return Err(format!("public task language balance differs: {languages:?}").into());
    }
    if exact != EXPECTED_EXACT_TASKS {
        return Err(format!("expected {EXPECTED_EXACT_TASKS} exact tasks, got {exact}").into());
    }
    Ok(SelectionSummary {
        languages,
        exact,
        behavioral: tasks.len() - exact,
    })
}

fn validate_labels(labels: &[SealedLabel]) -> Result<HashMap<&str, &SealedLabel>, DynError> {
    if labels.len() != EXPECTED_TASKS {
        return Err(format!(
            "expected {EXPECTED_TASKS} sealed labels, got {}",
            labels.len()
        )
        .into());
    }
    let mut result = HashMap::new();
    for label in labels {
        if label.schema_version != CONTRACT_SCHEMA_VERSION {
            return Err(format!("{} has unsupported label schema", label.task_id).into());
        }
        validate_task_id(&label.task_id)?;
        validate_blake3(&label.source_record_blake3, "label source record BLAKE3")?;
        validate_blake3(&label.gold_patch_blake3, "gold patch BLAKE3")?;
        validate_blake3(&label.test_patch_blake3, "test patch BLAKE3")?;
        if label.label_method != FROZEN_LABEL_METHOD || label.core_regions.is_empty() {
            return Err(format!("{} differs from the frozen label contract", label.task_id).into());
        }
        for region in label.core_regions.iter().chain(&label.optional_regions) {
            validate_region(region)?;
        }
        if result.insert(label.task_id.as_str(), label).is_some() {
            return Err(format!("duplicate sealed label ID {}", label.task_id).into());
        }
    }
    Ok(result)
}

fn validate_task_id(value: &str) -> Result<(), DynError> {
    if value.is_empty()
        || value.len() > 256
        || !value.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '_' | '-' | '.')
        })
    {
        return Err(format!("invalid task ID {value:?}").into());
    }
    Ok(())
}

fn validate_repository(repository: &RepositorySpec) -> Result<(), DynError> {
    if !repository.url.starts_with("https://github.com/") || !repository.url.ends_with(".git") {
        return Err(format!("unsupported repository URL {}", repository.url).into());
    }
    validate_revision(&repository.revision, "repository revision")?;
    if repository.path_style != "posix" {
        return Err("multilingual development repositories must use POSIX paths".into());
    }
    Ok(())
}

fn validate_region(region: &Region) -> Result<(), DynError> {
    validate_relative_path(&region.path)?;
    if region.start_line == 0 || region.end_line < region.start_line {
        return Err(format!(
            "invalid region {}:{}-{}",
            region.path, region.start_line, region.end_line
        )
        .into());
    }
    Ok(())
}

fn validate_relative_path(value: &str) -> Result<(), DynError> {
    let path = Path::new(value);
    if value.is_empty()
        || value.contains('\\')
        || path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(format!("invalid repository-relative path {value:?}").into());
    }
    Ok(())
}

fn parse_jsonl<T: DeserializeOwned>(bytes: &[u8]) -> Result<Vec<T>, DynError> {
    let text = std::str::from_utf8(bytes)?;
    let mut values = Vec::new();
    for (index, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            return Err(format!("blank JSONL line {}", index + 1).into());
        }
        values.push(
            serde_json::from_str(line)
                .map_err(|error| format!("invalid JSONL line {}: {error}", index + 1))?,
        );
    }
    if values.is_empty() {
        return Err("JSONL input is empty".into());
    }
    Ok(values)
}

fn serialize_jsonl<T: Serialize>(values: &[T]) -> Result<Vec<u8>, DynError> {
    let mut bytes = Vec::new();
    for value in values {
        serde_json::to_writer(&mut bytes, value)?;
        bytes.push(b'\n');
    }
    Ok(bytes)
}

fn one_value<T: Ord>(values: BTreeSet<T>, description: &str) -> Result<T, DynError> {
    if values.len() != 1 {
        return Err(format!("expected one {description}, got {}", values.len()).into());
    }
    values
        .into_iter()
        .next()
        .ok_or_else(|| format!("missing {description}").into())
}

fn validate_revision(value: &str, description: &str) -> Result<(), DynError> {
    if value.len() != 40 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(format!("{description} must be a 40-character Git revision").into());
    }
    Ok(())
}

fn validate_blake3(value: &str, description: &str) -> Result<(), DynError> {
    if value.len() != HASH_HEX_LEN
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        return Err(format!("{description} must be lowercase 64-character hex").into());
    }
    Ok(())
}

fn require_hash(actual: &str, expected: &str, description: &str) -> Result<(), DynError> {
    if actual != expected {
        return Err(
            format!("{description} BLAKE3 differs: expected {expected}, got {actual}").into(),
        );
    }
    Ok(())
}

fn blake3_hex(bytes: &[u8]) -> String {
    blake3::hash(bytes).to_hex().to_string()
}

fn hash_file(path: &Path) -> Result<String, DynError> {
    let mut file = fs::File::open(path)?;
    let mut hasher = blake3::Hasher::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hasher.finalize().to_hex().to_string())
}

fn current_artifact(revision: &str) -> Result<ArtifactIdentity, DynError> {
    let path = env::current_exe()?.canonicalize()?;
    artifact_identity(path, revision)
}

fn artifact_identity(path: PathBuf, revision: &str) -> Result<ArtifactIdentity, DynError> {
    let metadata = fs::metadata(&path)?;
    if !metadata.is_file() {
        return Err(format!("artifact is not a file: {}", path.display()).into());
    }
    Ok(ArtifactIdentity {
        revision: revision.to_owned(),
        blake3: hash_file(&path)?,
        bytes: metadata.len(),
        path,
    })
}

fn ensure_output_absent(path: &Path) -> Result<(), DynError> {
    if path.exists() {
        return Err(format!("refusing to overwrite {}", path.display()).into());
    }
    Ok(())
}

fn write_new(path: &Path, bytes: &[u8], private: bool) -> Result<(), DynError> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent)?;
    }
    let mut options = OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    if private {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    #[cfg(not(unix))]
    let _ = private;
    let mut file = options.open(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}

fn predict(args: &PredictArgs) -> Result<(), DynError> {
    validate_blake3(&args.expected_tasks_blake3, "expected tasks BLAKE3")?;
    validate_blake3(&args.runtime_binary_blake3, "runtime binary BLAKE3")?;
    validate_revision(&args.runtime_revision, "runtime revision")?;
    if args.repetitions != 2 {
        return Err("the frozen development predictor requires exactly two repetitions".into());
    }
    if args.command_timeout_seconds == 0 {
        return Err("command timeout must be positive".into());
    }
    validate_arm_id(&args.arm_id)?;

    let task_bytes = fs::read(&args.tasks)?;
    let tasks_blake3 = blake3_hex(&task_bytes);
    require_hash(&tasks_blake3, &args.expected_tasks_blake3, "tasks")?;
    let tasks = parse_jsonl::<DevelopmentTask>(&task_bytes)?;
    validate_development_tasks(&tasks)?;
    let manifest_blake3 = hash_file(&args.manifest)?;
    validate_blake3(&manifest_blake3, "manifest BLAKE3")?;

    let timeout = Duration::from_secs(args.command_timeout_seconds);
    let runtime = verify_runtime(args, timeout)?;
    let evaluator = verify_evaluator(&args.evaluator, timeout)?;
    ensure_output_absent(&args.output)?;
    ensure_output_absent(&args.receipt_output)?;
    if args.work_root.exists() {
        return Err(format!("refusing to reuse work root {}", args.work_root.display()).into());
    }
    fs::create_dir_all(&args.work_root)?;
    fs::create_dir_all(&args.repository_cache)?;
    let mut predictions = Vec::with_capacity(tasks.len());
    let mut task_receipts = Vec::with_capacity(tasks.len());
    for (index, task) in tasks.iter().enumerate() {
        let (prediction, receipt) = run_prediction_task(
            args,
            &runtime.binary.path,
            task,
            index,
            &manifest_blake3,
            timeout,
        )?;
        predictions.push(prediction);
        task_receipts.push(receipt);
    }

    let prediction_bytes = serialize_jsonl(&predictions)?;
    let predictions_blake3 = blake3_hex(&prediction_bytes);
    let receipt = PredictionReceipt {
        schema_version: RECEIPT_SCHEMA_VERSION,
        operation: "predict",
        arm_id: args.arm_id.clone(),
        evaluator,
        runtime,
        tasks_blake3,
        manifest_blake3,
        predictions_blake3,
        repetitions: args.repetitions,
        command_timeout_seconds: args.command_timeout_seconds,
        tokenizer: FROZEN_TOKENIZER.name().into(),
        source_token_budget: FROZEN_SOURCE_TOKEN_BUDGET,
        deterministic_responses: task_receipts.iter().all(|task| task.repetitions_identical),
        tasks: task_receipts,
    };
    let receipt_bytes = serde_json::to_vec_pretty(&receipt)?;
    write_new(&args.output, &prediction_bytes, false)?;
    write_new(&args.receipt_output, &receipt_bytes, false)?;
    Ok(())
}

fn validate_arm_id(value: &str) -> Result<(), DynError> {
    if value.is_empty()
        || value.len() > 64
        || !value
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '_' | '-'))
    {
        return Err("arm ID must be bounded ASCII alphanumeric, underscore, or hyphen".into());
    }
    Ok(())
}

fn verify_runtime(args: &PredictArgs, timeout: Duration) -> Result<RuntimeIdentity, DynError> {
    let repository = args.runtime_repository.canonicalize()?;
    let binary = args.runtime_binary.canonicalize()?;
    let actual_revision = git_text(
        ["-C", path_arg(&repository)?, "rev-parse", "HEAD"],
        timeout,
        "read runtime revision",
    )?;
    if actual_revision != args.runtime_revision {
        return Err(format!(
            "runtime repository is at {actual_revision}, expected {}",
            args.runtime_revision
        )
        .into());
    }
    let status = git_text(
        [
            "-C",
            path_arg(&repository)?,
            "status",
            "--porcelain=v1",
            "--untracked-files=no",
        ],
        timeout,
        "check runtime worktree",
    )?;
    if !status.is_empty() {
        return Err("runtime repository has tracked worktree changes".into());
    }
    let identity = artifact_identity(binary, &args.runtime_revision)?;
    require_hash(
        &identity.blake3,
        &args.runtime_binary_blake3,
        "runtime binary",
    )?;
    Ok(RuntimeIdentity {
        repository,
        revision: args.runtime_revision.clone(),
        binary: identity,
    })
}

fn verify_evaluator(
    args: &EvaluatorIdentityArgs,
    timeout: Duration,
) -> Result<ArtifactIdentity, DynError> {
    validate_revision(&args.evaluator_revision, "evaluator revision")?;
    validate_blake3(&args.evaluator_binary_blake3, "evaluator binary BLAKE3")?;
    let repository = args.evaluator_repository.canonicalize()?;
    let actual_revision = git_text(
        ["-C", path_arg(&repository)?, "rev-parse", "HEAD"],
        timeout,
        "read evaluator revision",
    )?;
    if actual_revision != args.evaluator_revision {
        return Err(format!(
            "evaluator repository is at {actual_revision}, expected {}",
            args.evaluator_revision
        )
        .into());
    }
    let status = git_text(
        [
            "-C",
            path_arg(&repository)?,
            "status",
            "--porcelain=v1",
            "--untracked-files=no",
        ],
        timeout,
        "check evaluator worktree",
    )?;
    if !status.is_empty() {
        return Err("evaluator repository has tracked worktree changes".into());
    }
    let identity = current_artifact(&args.evaluator_revision)?;
    require_hash(
        &identity.blake3,
        &args.evaluator_binary_blake3,
        "evaluator binary",
    )?;
    Ok(identity)
}

fn run_prediction_task(
    args: &PredictArgs,
    runtime_binary: &Path,
    task: &DevelopmentTask,
    task_index: usize,
    manifest_blake3: &str,
    timeout: Duration,
) -> Result<(Prediction, TaskRunReceipt), DynError> {
    let mirror = ensure_mirror(&args.repository_cache, &task.repository, timeout)?;
    let task_key = format!(
        "{task_index:03}-{}",
        &blake3_hex(task.task_id.as_bytes())[..16]
    );
    let worktree_path = args.work_root.join("worktrees").join(&task_key);
    if let Some(parent) = worktree_path.parent() {
        fs::create_dir_all(parent)?;
    }
    let worktree = Worktree::create(mirror, worktree_path, &task.repository.revision, timeout)?;
    let worktree_head = git_text(
        ["-C", path_arg(worktree.path())?, "rev-parse", "HEAD"],
        timeout,
        "verify task worktree revision",
    )?;
    if worktree_head != task.repository.revision {
        return Err(format!("{} worktree revision differs", task.task_id).into());
    }

    let database = args
        .work_root
        .join("indexes")
        .join(format!("{task_key}.sqlite"));
    if let Some(parent) = database.parent() {
        fs::create_dir_all(parent)?;
    }
    let artifact_dir = args.work_root.join("artifacts").join(&task_key);
    fs::create_dir_all(&artifact_dir)?;

    let index_started = Instant::now();
    let index_output = run_runtime(
        runtime_binary,
        worktree.path(),
        &database,
        ["index", "--rebuild"],
        timeout,
        "index task repository",
    )?;
    let index_elapsed_ms = index_started.elapsed().as_millis();
    let index_response: IndexResponse = serde_json::from_slice(&index_output.stdout)?;
    let index_artifact = artifact_dir.join("index.json");
    write_new(&index_artifact, &index_output.stdout, false)?;

    let mut outputs = Vec::with_capacity(args.repetitions);
    let mut context_elapsed_ms = Vec::with_capacity(args.repetitions);
    for _ in 0..args.repetitions {
        let started = Instant::now();
        let output = run_runtime_context(
            runtime_binary,
            worktree.path(),
            &database,
            &task.query,
            timeout,
        )?;
        context_elapsed_ms.push(started.elapsed().as_millis());
        outputs.push(output.stdout);
    }
    let repetitions_identical = outputs.windows(2).all(|pair| pair[0] == pair[1]);
    if !repetitions_identical {
        return Err(format!(
            "{} context response is not byte deterministic",
            task.task_id
        )
        .into());
    }
    let response_bytes = outputs
        .first()
        .ok_or_else(|| format!("{} produced no context response", task.task_id))?;
    let response: ContextResponse = serde_json::from_slice(response_bytes)?;
    if response.meta.repository_generation != index_response.repository_generation {
        return Err(format!("{} context/index generation mismatch", task.task_id).into());
    }
    if response.meta.emitted_tokens > FROZEN_SOURCE_TOKEN_BUDGET {
        return Err(format!("{} exceeded the source-token budget", task.task_id).into());
    }
    let counted_source = response
        .fragments
        .iter()
        .map(|fragment| FROZEN_TOKENIZER.count(&fragment.content))
        .sum::<usize>();
    if counted_source != response.meta.emitted_tokens {
        return Err(format!("{} source token accounting differs", task.task_id).into());
    }
    let response_text = std::str::from_utf8(response_bytes)?;
    let complete_response_tokens = FROZEN_TOKENIZER.count(response_text);
    let context_artifact = artifact_dir.join("context.json");
    write_new(&context_artifact, response_bytes, false)?;

    let regions = response
        .fragments
        .iter()
        .enumerate()
        .map(|(index, fragment)| PredictedRegion {
            path: fragment.path.clone(),
            start_line: fragment.start_line,
            end_line: fragment.end_line,
            rank: index + 1,
            source: Some(fragment.reason.clone()),
            facet: None,
            score: None,
            token_count: Some(FROZEN_TOKENIZER.count(&fragment.content)),
        })
        .collect::<Vec<_>>();
    let prediction = Prediction {
        schema_version: CONTRACT_SCHEMA_VERSION,
        task_id: task.task_id.clone(),
        manifest_blake3: manifest_blake3.to_owned(),
        repository_revision: task.repository.revision.clone(),
        budget: RankedBudget {
            kind: task.budget.kind.clone(),
            amount: task.budget.amount,
        },
        tokenizer: Some(FROZEN_TOKENIZER.name().into()),
        complete_response_tokens: Some(complete_response_tokens),
        source_tokens: Some(response.meta.emitted_tokens),
        latency_ms: None,
        index_generation: Some(response.meta.repository_generation),
        regions,
    };
    let receipt = TaskRunReceipt {
        task_id: task.task_id.clone(),
        repository_url: task.repository.url.clone(),
        repository_revision: task.repository.revision.clone(),
        worktree_head,
        index_generation: index_response.repository_generation,
        files_indexed: index_response.files_indexed,
        files_skipped: index_response.files_skipped,
        index_warning_count: index_response.warnings.len(),
        index_elapsed_ms,
        index_response_blake3: blake3_hex(&index_output.stdout),
        index_artifact: index_artifact.strip_prefix(&args.work_root)?.to_path_buf(),
        context_elapsed_ms,
        response_blake3: blake3_hex(response_bytes),
        context_artifact: context_artifact
            .strip_prefix(&args.work_root)?
            .to_path_buf(),
        complete_response_tokens,
        source_tokens: response.meta.emitted_tokens,
        regions: response.fragments.len(),
        repetitions_identical,
    };
    worktree.remove()?;
    Ok((prediction, receipt))
}

fn ensure_mirror(
    cache: &Path,
    repository: &RepositorySpec,
    timeout: Duration,
) -> Result<PathBuf, DynError> {
    let key = &blake3_hex(repository.url.as_bytes())[..24];
    let mirror = cache.join("mirrors").join(format!("{key}.git"));
    if !mirror.exists() {
        if let Some(parent) = mirror.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut command = Command::new("git");
        command.args([
            "clone",
            "--bare",
            "--filter=blob:none",
            "--no-tags",
            "--quiet",
        ]);
        command.arg(&repository.url).arg(&mirror);
        run_checked(command, timeout, "clone repository mirror")?;
    }
    if !mirror.is_dir() {
        return Err(format!("repository mirror is not a directory: {}", mirror.display()).into());
    }
    let origin = git_text(
        [
            "--git-dir",
            path_arg(&mirror)?,
            "config",
            "--get",
            "remote.origin.url",
        ],
        timeout,
        "read repository mirror origin",
    )?;
    if origin != repository.url {
        return Err(format!("repository mirror origin differs for {}", repository.url).into());
    }
    let object = format!("{}^{{commit}}", repository.revision);
    let mut probe = Command::new("git");
    probe
        .arg("--git-dir")
        .arg(&mirror)
        .args(["cat-file", "-e", &object]);
    let probe = run_captured(probe, timeout, "probe repository revision")?;
    if !probe.status.success() {
        let mut fetch = Command::new("git");
        fetch
            .arg("--git-dir")
            .arg(&mirror)
            .args([
                "fetch",
                "--filter=blob:none",
                "--no-tags",
                "--quiet",
                "origin",
            ])
            .arg(&repository.revision);
        run_checked(fetch, timeout, "fetch pinned repository revision")?;
        let mut verify = Command::new("git");
        verify
            .arg("--git-dir")
            .arg(&mirror)
            .args(["cat-file", "-e", &object]);
        run_checked(verify, timeout, "verify fetched repository revision")?;
    }
    Ok(mirror)
}

struct Worktree {
    mirror: PathBuf,
    path: PathBuf,
    timeout: Duration,
    active: bool,
}

impl Worktree {
    fn create(
        mirror: PathBuf,
        path: PathBuf,
        revision: &str,
        timeout: Duration,
    ) -> Result<Self, DynError> {
        if path.exists() {
            return Err(format!("refusing to reuse worktree {}", path.display()).into());
        }
        let mut command = Command::new("git");
        command
            .arg("--git-dir")
            .arg(&mirror)
            .args(["worktree", "add", "--detach", "--force", "--quiet"])
            .arg(&path)
            .arg(revision);
        run_checked(command, timeout, "create pinned task worktree")?;
        Ok(Self {
            mirror,
            path,
            timeout,
            active: true,
        })
    }

    fn path(&self) -> &Path {
        &self.path
    }

    fn remove(mut self) -> Result<(), DynError> {
        remove_worktree(&self.mirror, &self.path, self.timeout)?;
        self.active = false;
        Ok(())
    }
}

impl Drop for Worktree {
    fn drop(&mut self) {
        if self.active {
            let _ = remove_worktree(&self.mirror, &self.path, self.timeout);
        }
    }
}

fn remove_worktree(mirror: &Path, path: &Path, timeout: Duration) -> Result<(), DynError> {
    let mut command = Command::new("git");
    command
        .arg("--git-dir")
        .arg(mirror)
        .args(["worktree", "remove", "--force"])
        .arg(path);
    run_checked(command, timeout, "remove task worktree")?;
    Ok(())
}

fn run_runtime<const N: usize>(
    binary: &Path,
    root: &Path,
    database: &Path,
    operation: [&str; N],
    timeout: Duration,
    description: &str,
) -> Result<Output, DynError> {
    let mut command = Command::new(binary);
    command
        .arg("--root")
        .arg(root)
        .arg("--database")
        .arg(database)
        .args(["--json", "--tokenizer", FROZEN_TOKENIZER.name()])
        .args(operation);
    run_checked(command, timeout, description)
}

fn run_runtime_context(
    binary: &Path,
    root: &Path,
    database: &Path,
    query: &str,
    timeout: Duration,
) -> Result<Output, DynError> {
    let mut command = Command::new(binary);
    command
        .arg("--root")
        .arg(root)
        .arg("--database")
        .arg(database)
        .args([
            "--json",
            "--tokenizer",
            FROZEN_TOKENIZER.name(),
            "context",
            "--task",
        ])
        .arg(query)
        .args(["--budget", "2000"]);
    run_checked(command, timeout, "retrieve task context")
}

fn git_text<const N: usize>(
    args: [&str; N],
    timeout: Duration,
    description: &str,
) -> Result<String, DynError> {
    let mut command = Command::new("git");
    command.args(args);
    let output = run_checked(command, timeout, description)?;
    Ok(std::str::from_utf8(&output.stdout)?.trim().to_owned())
}

fn path_arg(path: &Path) -> Result<&str, DynError> {
    path.to_str()
        .ok_or_else(|| format!("path is not valid UTF-8: {}", path.display()).into())
}

fn run_checked(command: Command, timeout: Duration, description: &str) -> Result<Output, DynError> {
    let output = run_captured(command, timeout, description)?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "{description} exited with {}: {}",
            output.status,
            stderr.trim()
        )
        .into());
    }
    Ok(output)
}

fn run_captured(
    mut command: Command,
    timeout: Duration,
    description: &str,
) -> Result<Output, DynError> {
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = command.spawn()?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| format!("{description} stdout unavailable"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| format!("{description} stderr unavailable"))?;
    let stdout_reader = std::thread::spawn(move || read_bounded(stdout));
    let stderr_reader = std::thread::spawn(move || read_bounded(stderr));
    let status = match child.wait_timeout(timeout)? {
        Some(status) => status,
        None => {
            child.kill()?;
            let _ = child.wait();
            let _ = stdout_reader.join();
            let _ = stderr_reader.join();
            return Err(format!("{description} timed out after {}s", timeout.as_secs()).into());
        }
    };
    let (stdout, stdout_exceeded) = stdout_reader
        .join()
        .map_err(|_| format!("{description} stdout reader panicked"))??;
    let (stderr, stderr_exceeded) = stderr_reader
        .join()
        .map_err(|_| format!("{description} stderr reader panicked"))??;
    if stdout_exceeded
        || stderr_exceeded
        || stdout.len().saturating_add(stderr.len()) > MAX_COMMAND_OUTPUT_BYTES
    {
        return Err(format!("{description} output exceeds the frozen size bound").into());
    }
    Ok(Output {
        status,
        stdout,
        stderr,
    })
}

fn read_bounded(mut reader: impl Read) -> io::Result<(Vec<u8>, bool)> {
    let mut output = Vec::new();
    let mut exceeded = false;
    let mut buffer = [0_u8; 16 * 1024];
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        let remaining = MAX_COMMAND_OUTPUT_BYTES.saturating_sub(output.len());
        output.extend_from_slice(&buffer[..read.min(remaining)]);
        exceeded |= read > remaining;
    }
    Ok((output, exceeded))
}

#[derive(Debug, Deserialize, Serialize)]
struct EvaluationReport {
    schema_version: u32,
    dataset_kind: String,
    manifest_blake3: String,
    predictions_blake3: String,
    budget_kind: String,
    tokenizer: Option<String>,
    aggregate: EvaluationMetrics,
    strata: BTreeMap<String, EvaluationMetrics>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct EvaluationMetrics {
    task_count: usize,
    file_recall_macro: f64,
    line_recall_macro: f64,
    line_precision_macro: f64,
    line_f1_macro: f64,
    ndcg_at_budget_macro: f64,
    noise_region_rate_macro: f64,
    complete_response_tokens: Option<usize>,
    source_tokens: Option<usize>,
    complete_tokens_per_relevant_line: Option<f64>,
    source_tokens_per_relevant_line: Option<f64>,
}

#[derive(Debug, Serialize)]
struct ExternalGateDecision {
    schema_version: u32,
    decision_kind: &'static str,
    evaluator: ArtifactIdentity,
    ranked_region_evaluator: ArtifactIdentity,
    baseline_report_blake3: String,
    candidate_report_blake3: String,
    manifest_blake3: String,
    baseline_predictions_blake3: String,
    candidate_predictions_blake3: String,
    dataset_kind: String,
    task_count: usize,
    thresholds: DecisionThresholds,
    aggregate: AggregateDecision,
    eligible_strata: Vec<StratumDecision>,
    improved_strata: Vec<String>,
    improved_evidence_groups: Vec<String>,
    conflicting_strata: Vec<String>,
    retrieval_strata_passed: bool,
    complete_cost_passed: bool,
    external_gate_a_passed: bool,
    integration_gate_a_passed: bool,
    remaining_integration_gates: Vec<&'static str>,
}

#[derive(Debug, Serialize)]
struct DecisionThresholds {
    minimum_stratum_tasks: usize,
    required_improved_strata: usize,
    maximum_complete_cost_regression: f64,
    improvement_rule: &'static str,
    equivalent_strata_rule: &'static str,
    predefined_strata: [&'static str; 3],
}

#[derive(Debug, Serialize)]
struct AggregateDecision {
    baseline: EvaluationMetrics,
    candidate: EvaluationMetrics,
    complete_cost_relative_delta: Option<f64>,
}

#[derive(Debug, Serialize)]
struct StratumDecision {
    name: String,
    tasks: usize,
    baseline_line_recall: f64,
    candidate_line_recall: f64,
    baseline_ndcg: f64,
    candidate_ndcg: f64,
    line_recall_delta: f64,
    ndcg_delta: f64,
    improved: bool,
    conflicts: bool,
}

fn decide(args: &DecideArgs) -> Result<(), DynError> {
    if args.command_timeout_seconds == 0 {
        return Err("command timeout must be positive".into());
    }
    validate_blake3(
        &args.ranked_evaluator_binary_blake3,
        "ranked evaluator binary BLAKE3",
    )?;
    let timeout = Duration::from_secs(args.command_timeout_seconds);
    let evaluator = verify_evaluator(&args.evaluator, timeout)?;
    let ranked_region_evaluator = artifact_identity(
        args.ranked_evaluator_binary.canonicalize()?,
        &args.evaluator.evaluator_revision,
    )?;
    require_hash(
        &ranked_region_evaluator.blake3,
        &args.ranked_evaluator_binary_blake3,
        "ranked evaluator binary",
    )?;
    let manifest_blake3 = hash_file(&args.manifest)?;
    let baseline_predictions_blake3 = hash_file(&args.baseline_predictions)?;
    let candidate_predictions_blake3 = hash_file(&args.candidate_predictions)?;
    ensure_output_absent(&args.output)?;
    ensure_output_absent(&args.baseline_report_output)?;
    ensure_output_absent(&args.candidate_report_output)?;
    run_ranked_evaluation(
        &ranked_region_evaluator.path,
        &args.manifest,
        &args.baseline_predictions,
        &args.baseline_report_output,
        timeout,
    )?;
    run_ranked_evaluation(
        &ranked_region_evaluator.path,
        &args.manifest,
        &args.candidate_predictions,
        &args.candidate_report_output,
        timeout,
    )?;
    let baseline_bytes = fs::read(&args.baseline_report_output)?;
    let candidate_bytes = fs::read(&args.candidate_report_output)?;
    decide_report_pair(
        args,
        evaluator,
        ranked_region_evaluator,
        &baseline_bytes,
        &candidate_bytes,
        &manifest_blake3,
        &baseline_predictions_blake3,
        &candidate_predictions_blake3,
    )
}

fn run_ranked_evaluation(
    binary: &Path,
    manifest: &Path,
    predictions: &Path,
    output: &Path,
    timeout: Duration,
) -> Result<(), DynError> {
    let mut command = Command::new(binary);
    command
        .arg("evaluate")
        .arg("--manifest")
        .arg(manifest)
        .arg("--predictions")
        .arg(predictions)
        .arg("--output")
        .arg(output);
    run_checked(command, timeout, "evaluate ranked retrieval predictions")?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn decide_report_pair(
    args: &DecideArgs,
    evaluator: ArtifactIdentity,
    ranked_region_evaluator: ArtifactIdentity,
    baseline_bytes: &[u8],
    candidate_bytes: &[u8],
    manifest_blake3: &str,
    baseline_predictions_blake3: &str,
    candidate_predictions_blake3: &str,
) -> Result<(), DynError> {
    let baseline: EvaluationReport = serde_json::from_slice(baseline_bytes)?;
    let candidate: EvaluationReport = serde_json::from_slice(candidate_bytes)?;
    validate_report_pair(&baseline, &candidate)?;
    require_hash(
        &baseline.manifest_blake3,
        manifest_blake3,
        "evaluated manifest",
    )?;
    require_hash(
        &baseline.predictions_blake3,
        baseline_predictions_blake3,
        "baseline predictions",
    )?;
    require_hash(
        &candidate.predictions_blake3,
        candidate_predictions_blake3,
        "candidate predictions",
    )?;

    let mut eligible_strata = Vec::new();
    for (name, baseline_metrics) in &baseline.strata {
        if !predefined_gate_stratum(name)
            || baseline_metrics.task_count < FROZEN_MINIMUM_STRATUM_TASKS
        {
            continue;
        }
        let candidate_metrics = candidate
            .strata
            .get(name)
            .ok_or_else(|| format!("candidate report is missing stratum {name}"))?;
        if candidate_metrics.task_count != baseline_metrics.task_count {
            return Err(format!("stratum {name} task counts differ").into());
        }
        let line_recall_delta =
            candidate_metrics.line_recall_macro - baseline_metrics.line_recall_macro;
        let ndcg_delta =
            candidate_metrics.ndcg_at_budget_macro - baseline_metrics.ndcg_at_budget_macro;
        eligible_strata.push(StratumDecision {
            name: name.clone(),
            tasks: baseline_metrics.task_count,
            baseline_line_recall: baseline_metrics.line_recall_macro,
            candidate_line_recall: candidate_metrics.line_recall_macro,
            baseline_ndcg: baseline_metrics.ndcg_at_budget_macro,
            candidate_ndcg: candidate_metrics.ndcg_at_budget_macro,
            line_recall_delta,
            ndcg_delta,
            improved: line_recall_delta > 0.0 || ndcg_delta > 0.0,
            conflicts: line_recall_delta < 0.0 || ndcg_delta < 0.0,
        });
    }
    eligible_strata.sort_by(|left, right| left.name.cmp(&right.name));
    let improved_strata = eligible_strata
        .iter()
        .filter(|stratum| stratum.improved)
        .map(|stratum| stratum.name.clone())
        .collect::<Vec<_>>();
    let improved_evidence_groups = improved_strata
        .iter()
        .map(|name| evidence_group(name))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let conflicting_strata = eligible_strata
        .iter()
        .filter(|stratum| stratum.conflicts)
        .map(|stratum| stratum.name.clone())
        .collect::<Vec<_>>();
    let retrieval_strata_passed = improved_evidence_groups.len() >= FROZEN_REQUIRED_IMPROVED_STRATA;
    let complete_cost_relative_delta = relative_delta(
        baseline.aggregate.complete_tokens_per_relevant_line,
        candidate.aggregate.complete_tokens_per_relevant_line,
    );
    let complete_cost_passed = complete_cost_relative_delta
        .is_some_and(|delta| delta <= FROZEN_MAXIMUM_COMPLETE_COST_REGRESSION);
    let external_gate_a_passed = retrieval_strata_passed && complete_cost_passed;
    let decision = ExternalGateDecision {
        schema_version: DECISION_SCHEMA_VERSION,
        decision_kind: "external_development_gate_a",
        evaluator,
        ranked_region_evaluator,
        baseline_report_blake3: blake3_hex(baseline_bytes),
        candidate_report_blake3: blake3_hex(candidate_bytes),
        manifest_blake3: baseline.manifest_blake3.clone(),
        baseline_predictions_blake3: baseline.predictions_blake3.clone(),
        candidate_predictions_blake3: candidate.predictions_blake3.clone(),
        dataset_kind: baseline.dataset_kind.clone(),
        task_count: baseline.aggregate.task_count,
        thresholds: DecisionThresholds {
            minimum_stratum_tasks: FROZEN_MINIMUM_STRATUM_TASKS,
            required_improved_strata: FROZEN_REQUIRED_IMPROVED_STRATA,
            maximum_complete_cost_regression: FROZEN_MAXIMUM_COMPLETE_COST_REGRESSION,
            improvement_rule: "candidate macro relevant-line recall or NDCG is strictly greater than baseline",
            equivalent_strata_rule: "exact_identifier and its task_shape alias count as one evidence group",
            predefined_strata: ["language:*", "exact_identifier:*", "tag:task_shape:*"],
        },
        aggregate: AggregateDecision {
            baseline: baseline.aggregate,
            candidate: candidate.aggregate,
            complete_cost_relative_delta,
        },
        eligible_strata,
        improved_strata,
        improved_evidence_groups,
        conflicting_strata,
        retrieval_strata_passed,
        complete_cost_passed,
        external_gate_a_passed,
        integration_gate_a_passed: false,
        remaining_integration_gates: vec![
            "stable/MSRV/cross-platform correctness and deterministic output",
            "consumed internal smoke floor",
            "exact low-cardinality, freshness, budget, and recovery regression checks",
            "complete source/wire/dead-end/two-turn tradeoff disclosure",
        ],
    };
    write_new(&args.output, &serde_json::to_vec_pretty(&decision)?, false)?;
    Ok(())
}

fn validate_report_pair(
    baseline: &EvaluationReport,
    candidate: &EvaluationReport,
) -> Result<(), DynError> {
    if baseline.schema_version != CONTRACT_SCHEMA_VERSION
        || candidate.schema_version != CONTRACT_SCHEMA_VERSION
    {
        return Err("evaluation reports use an unsupported schema".into());
    }
    if baseline.manifest_blake3 != candidate.manifest_blake3
        || baseline.dataset_kind != candidate.dataset_kind
        || baseline.budget_kind != candidate.budget_kind
        || baseline.tokenizer != candidate.tokenizer
        || baseline.aggregate.task_count != candidate.aggregate.task_count
        || baseline.strata.keys().ne(candidate.strata.keys())
    {
        return Err("baseline and candidate evaluation identities differ".into());
    }
    validate_blake3(&baseline.manifest_blake3, "evaluation manifest BLAKE3")?;
    validate_blake3(&baseline.predictions_blake3, "baseline predictions BLAKE3")?;
    validate_blake3(
        &candidate.predictions_blake3,
        "candidate predictions BLAKE3",
    )?;
    if baseline.budget_kind != "source_tokens"
        || baseline.tokenizer.as_deref() != Some(FROZEN_TOKENIZER.name())
        || baseline.dataset_kind != FROZEN_DATASET_KIND
        || baseline.aggregate.task_count != EXPECTED_TASKS
    {
        return Err("evaluation report differs from the frozen development contract".into());
    }
    validate_evaluation_metrics(&baseline.aggregate, "baseline aggregate")?;
    validate_evaluation_metrics(&candidate.aggregate, "candidate aggregate")?;
    for (name, metrics) in &baseline.strata {
        validate_evaluation_metrics(metrics, &format!("baseline stratum {name}"))?;
        validate_evaluation_metrics(
            candidate
                .strata
                .get(name)
                .expect("strata keys were compared"),
            &format!("candidate stratum {name}"),
        )?;
    }
    if baseline.aggregate.complete_response_tokens.is_none()
        || candidate.aggregate.complete_response_tokens.is_none()
        || baseline.aggregate.source_tokens.is_none()
        || candidate.aggregate.source_tokens.is_none()
        || baseline
            .aggregate
            .complete_tokens_per_relevant_line
            .is_none()
        || candidate
            .aggregate
            .complete_tokens_per_relevant_line
            .is_none()
    {
        return Err("evaluation aggregate lacks complete frozen cost accounting".into());
    }
    let maximum_source_tokens = EXPECTED_TASKS * FROZEN_SOURCE_TOKEN_BUDGET;
    if baseline
        .aggregate
        .source_tokens
        .is_some_and(|tokens| tokens > maximum_source_tokens)
        || candidate
            .aggregate
            .source_tokens
            .is_some_and(|tokens| tokens > maximum_source_tokens)
    {
        return Err("evaluation aggregate exceeds the frozen source-token budget".into());
    }
    Ok(())
}

fn validate_evaluation_metrics(
    metrics: &EvaluationMetrics,
    description: &str,
) -> Result<(), DynError> {
    if metrics.task_count == 0 || metrics.task_count > EXPECTED_TASKS {
        return Err(format!("{description} has an invalid task count").into());
    }
    for value in [
        metrics.file_recall_macro,
        metrics.line_recall_macro,
        metrics.line_precision_macro,
        metrics.line_f1_macro,
        metrics.ndcg_at_budget_macro,
        metrics.noise_region_rate_macro,
    ] {
        if !value.is_finite() || !(0.0..=1.0).contains(&value) {
            return Err(format!("{description} has an invalid ratio metric").into());
        }
    }
    for value in [
        metrics.complete_tokens_per_relevant_line,
        metrics.source_tokens_per_relevant_line,
    ]
    .into_iter()
    .flatten()
    {
        if !value.is_finite() || value < 0.0 {
            return Err(format!("{description} has an invalid cost metric").into());
        }
    }
    Ok(())
}

fn predefined_gate_stratum(name: &str) -> bool {
    name.strip_prefix("language:")
        .is_some_and(|language| EXPECTED_LANGUAGES.contains(&language))
        || matches!(
            name,
            "exact_identifier:true"
                | "exact_identifier:false"
                | "tag:task_shape:exact_identifier"
                | "tag:task_shape:behavioral"
        )
}

fn evidence_group(name: &str) -> String {
    match name {
        "tag:task_shape:exact_identifier" => "exact_identifier:true".into(),
        "tag:task_shape:behavioral" => "exact_identifier:false".into(),
        _ => name.to_owned(),
    }
}

fn relative_delta(baseline: Option<f64>, candidate: Option<f64>) -> Option<f64> {
    let baseline = baseline?;
    let candidate = candidate?;
    if !baseline.is_finite() || !candidate.is_finite() || baseline <= 0.0 {
        return None;
    }
    Some((candidate - baseline) / baseline)
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_REVISION: &str = "1111111111111111111111111111111111111111";

    #[test]
    fn materialize_is_deterministic_and_binds_all_frozen_tasks() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let (task_bytes, label_bytes) = development_fixture();
        let tasks = directory.path().join("tasks.jsonl");
        let labels = directory.path().join("labels.jsonl");
        fs::write(&tasks, &task_bytes).expect("write tasks");
        fs::write(&labels, &label_bytes).expect("write labels");

        let first = materialize_args(
            directory.path(),
            "first",
            &tasks,
            &labels,
            &task_bytes,
            &label_bytes,
        );
        let second = materialize_args(
            directory.path(),
            "second",
            &tasks,
            &labels,
            &task_bytes,
            &label_bytes,
        );
        let evaluator = test_artifact();
        materialize_with_evaluator(&first, evaluator.clone()).expect("first materialization");
        materialize_with_evaluator(&second, evaluator).expect("second materialization");

        let first_manifest = fs::read(&first.output).expect("read first manifest");
        let second_manifest = fs::read(&second.output).expect("read second manifest");
        assert_eq!(first_manifest, second_manifest);
        assert_eq!(
            fs::read(&first.receipt_output).expect("read first receipt"),
            fs::read(&second.receipt_output).expect("read second receipt")
        );
        let ranked: Vec<serde_json::Value> =
            parse_jsonl(&first_manifest).expect("parse ranked manifest");
        assert_eq!(ranked.len(), EXPECTED_TASKS);
        assert_eq!(ranked[0]["relevant_files"][0], "c/file0.txt");
        assert_eq!(
            ranked[0]["strata"]["tags"],
            serde_json::json!([
                "external",
                "patch_ground_truth",
                "task_shape:exact_identifier"
            ])
        );

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            let manifest_mode = fs::metadata(&first.output)
                .expect("manifest metadata")
                .permissions()
                .mode()
                & 0o777;
            let receipt_mode = fs::metadata(&first.receipt_output)
                .expect("receipt metadata")
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(manifest_mode, 0o600);
            assert_eq!(receipt_mode, 0o600);
        }
    }

    #[test]
    fn materialize_rejects_a_task_label_binding_mismatch() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let (task_bytes, label_bytes) = development_fixture();
        let mut labels: Vec<SealedLabel> = parse_jsonl(&label_bytes).expect("parse labels");
        labels[0].source_record_blake3 = "f".repeat(HASH_HEX_LEN);
        let changed_labels = serialize_jsonl(&labels).expect("serialize changed labels");
        let tasks = directory.path().join("tasks.jsonl");
        let labels_path = directory.path().join("labels.jsonl");
        fs::write(&tasks, &task_bytes).expect("write tasks");
        fs::write(&labels_path, &changed_labels).expect("write labels");
        let args = materialize_args(
            directory.path(),
            "mismatch",
            &tasks,
            &labels_path,
            &task_bytes,
            &changed_labels,
        );

        let error = materialize_with_evaluator(&args, test_artifact())
            .expect_err("binding mismatch must fail");
        assert!(error.to_string().contains("source binding differs"));
        assert!(!args.output.exists());
    }

    #[test]
    fn decide_passes_two_distinct_improved_evidence_groups_and_records_conflicts() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let (mut baseline, mut candidate) = report_pair();
        insert_stratum_pair(
            &mut baseline,
            &mut candidate,
            "language:c",
            0.2,
            0.3,
            0.2,
            0.2,
        );
        insert_stratum_pair(
            &mut baseline,
            &mut candidate,
            "exact_identifier:true",
            0.2,
            0.3,
            0.2,
            0.2,
        );
        insert_stratum_pair(
            &mut baseline,
            &mut candidate,
            "tag:task_shape:exact_identifier",
            0.2,
            0.3,
            0.2,
            0.2,
        );
        insert_stratum_pair(
            &mut baseline,
            &mut candidate,
            "language:cpp",
            0.3,
            0.31,
            0.3,
            0.29,
        );
        let output = run_decide(directory.path(), &baseline, &candidate);

        assert_eq!(output["retrieval_strata_passed"], true);
        assert_eq!(output["complete_cost_passed"], true);
        assert_eq!(output["external_gate_a_passed"], true);
        assert_eq!(output["integration_gate_a_passed"], false);
        assert_eq!(
            output["improved_evidence_groups"],
            serde_json::json!(["exact_identifier:true", "language:c", "language:cpp"])
        );
        assert_eq!(
            output["conflicting_strata"],
            serde_json::json!(["language:cpp"])
        );
    }

    #[test]
    fn decide_does_not_double_count_exact_identifier_task_shape_aliases() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let (mut baseline, mut candidate) = report_pair();
        insert_stratum_pair(
            &mut baseline,
            &mut candidate,
            "exact_identifier:true",
            0.2,
            0.3,
            0.2,
            0.2,
        );
        insert_stratum_pair(
            &mut baseline,
            &mut candidate,
            "tag:task_shape:exact_identifier",
            0.2,
            0.3,
            0.2,
            0.2,
        );
        let output = run_decide(directory.path(), &baseline, &candidate);

        assert_eq!(output["improved_strata"].as_array().map(Vec::len), Some(2));
        assert_eq!(
            output["improved_evidence_groups"],
            serde_json::json!(["exact_identifier:true"])
        );
        assert_eq!(output["retrieval_strata_passed"], false);
        assert_eq!(output["external_gate_a_passed"], false);
    }

    #[test]
    fn report_validation_rejects_invalid_metrics_and_strata_identity() {
        let (baseline, mut candidate) = report_pair();
        candidate.aggregate.line_recall_macro = 1.01;
        assert!(
            validate_report_pair(&baseline, &candidate)
                .expect_err("out-of-range metric must fail")
                .to_string()
                .contains("invalid ratio metric")
        );

        let (mut baseline, candidate) = report_pair();
        baseline
            .strata
            .insert("language:c".into(), metrics(6, 0.2, 0.2, 10.0));
        assert!(
            validate_report_pair(&baseline, &candidate)
                .expect_err("strata identity mismatch must fail")
                .to_string()
                .contains("identities differ")
        );
    }

    #[test]
    fn bounded_reader_drains_input_but_caps_retained_bytes() {
        let input = vec![b'x'; MAX_COMMAND_OUTPUT_BYTES + 123];
        let (output, exceeded) = read_bounded(input.as_slice()).expect("read bounded input");

        assert!(exceeded);
        assert_eq!(output.len(), MAX_COMMAND_OUTPUT_BYTES);
    }

    fn development_fixture() -> (Vec<u8>, Vec<u8>) {
        let mut tasks = Vec::new();
        let mut labels = Vec::new();
        for language in EXPECTED_LANGUAGES {
            for index in 0..EXPECTED_TASKS_PER_LANGUAGE {
                let task_id = format!("task-{language}-{index}");
                let source_record_blake3 = blake3_hex(task_id.as_bytes());
                let exact_identifier = index < EXPECTED_TASKS_PER_LANGUAGE / 2;
                tasks.push(DevelopmentTask {
                    schema_version: CONTRACT_SCHEMA_VERSION,
                    dataset_kind: FROZEN_DATASET_KIND.into(),
                    task_id: task_id.clone(),
                    repository: RepositorySpec {
                        url: "https://github.com/example/repository.git".into(),
                        revision: TEST_REVISION.into(),
                        path_style: "posix".into(),
                    },
                    query: format!("Fix {task_id}"),
                    language: language.into(),
                    strata: DevelopmentStrata {
                        exact_identifier,
                        task_shape: if exact_identifier {
                            "exact_identifier".into()
                        } else {
                            "behavioral".into()
                        },
                        tags: vec!["external".into(), "patch_ground_truth".into()],
                    },
                    budget: DevelopmentBudget {
                        kind: "source_tokens".into(),
                        amount: FROZEN_SOURCE_TOKEN_BUDGET,
                        tokenizer: FROZEN_TOKENIZER,
                        token_count_exact: true,
                    },
                    source_record_blake3: source_record_blake3.clone(),
                });
                labels.push(SealedLabel {
                    schema_version: CONTRACT_SCHEMA_VERSION,
                    task_id,
                    source_record_blake3,
                    label_method: FROZEN_LABEL_METHOD.into(),
                    core_regions: vec![Region {
                        path: format!("{language}/file{index}.txt"),
                        start_line: 10,
                        end_line: 12,
                    }],
                    optional_regions: Vec::new(),
                    gold_patch_blake3: "a".repeat(HASH_HEX_LEN),
                    test_patch_blake3: "b".repeat(HASH_HEX_LEN),
                    unobservable_added_files: 0,
                });
            }
        }
        (
            serialize_jsonl(&tasks).expect("serialize tasks"),
            serialize_jsonl(&labels).expect("serialize labels"),
        )
    }

    fn materialize_args(
        directory: &Path,
        stem: &str,
        tasks: &Path,
        labels: &Path,
        task_bytes: &[u8],
        label_bytes: &[u8],
    ) -> MaterializeArgs {
        MaterializeArgs {
            tasks: tasks.to_path_buf(),
            labels: labels.to_path_buf(),
            expected_tasks_blake3: blake3_hex(task_bytes),
            expected_labels_blake3: blake3_hex(label_bytes),
            output: directory.join(format!("{stem}-manifest.jsonl")),
            receipt_output: directory.join(format!("{stem}-receipt.json")),
            evaluator: EvaluatorIdentityArgs {
                evaluator_repository: directory.to_path_buf(),
                evaluator_revision: TEST_REVISION.into(),
                evaluator_binary_blake3: "d".repeat(HASH_HEX_LEN),
            },
        }
    }

    fn report_pair() -> (EvaluationReport, EvaluationReport) {
        let manifest_blake3 = "c".repeat(HASH_HEX_LEN);
        let baseline = EvaluationReport {
            schema_version: CONTRACT_SCHEMA_VERSION,
            dataset_kind: FROZEN_DATASET_KIND.into(),
            manifest_blake3: manifest_blake3.clone(),
            predictions_blake3: "e".repeat(HASH_HEX_LEN),
            budget_kind: "source_tokens".into(),
            tokenizer: Some(FROZEN_TOKENIZER.name().into()),
            aggregate: metrics(EXPECTED_TASKS, 0.2, 0.2, 10.0),
            strata: BTreeMap::new(),
        };
        let candidate = EvaluationReport {
            schema_version: CONTRACT_SCHEMA_VERSION,
            dataset_kind: FROZEN_DATASET_KIND.into(),
            manifest_blake3,
            predictions_blake3: "f".repeat(HASH_HEX_LEN),
            budget_kind: "source_tokens".into(),
            tokenizer: Some(FROZEN_TOKENIZER.name().into()),
            aggregate: metrics(EXPECTED_TASKS, 0.25, 0.25, 10.4),
            strata: BTreeMap::new(),
        };
        (baseline, candidate)
    }

    fn insert_stratum_pair(
        baseline: &mut EvaluationReport,
        candidate: &mut EvaluationReport,
        name: &str,
        baseline_line: f64,
        candidate_line: f64,
        baseline_ndcg: f64,
        candidate_ndcg: f64,
    ) {
        baseline.strata.insert(
            name.into(),
            metrics(
                EXPECTED_TASKS_PER_LANGUAGE,
                baseline_line,
                baseline_ndcg,
                10.0,
            ),
        );
        candidate.strata.insert(
            name.into(),
            metrics(
                EXPECTED_TASKS_PER_LANGUAGE,
                candidate_line,
                candidate_ndcg,
                10.0,
            ),
        );
    }

    fn metrics(task_count: usize, line_recall: f64, ndcg: f64, cost: f64) -> EvaluationMetrics {
        EvaluationMetrics {
            task_count,
            file_recall_macro: 0.5,
            line_recall_macro: line_recall,
            line_precision_macro: 0.4,
            line_f1_macro: 0.4,
            ndcg_at_budget_macro: ndcg,
            noise_region_rate_macro: 0.1,
            complete_response_tokens: Some(task_count * 100),
            source_tokens: Some(task_count * 80),
            complete_tokens_per_relevant_line: Some(cost),
            source_tokens_per_relevant_line: Some(cost * 0.8),
        }
    }

    fn run_decide(
        directory: &Path,
        baseline: &EvaluationReport,
        candidate: &EvaluationReport,
    ) -> serde_json::Value {
        let output = directory.join("decision.json");
        let baseline_bytes = serde_json::to_vec(baseline).expect("serialize baseline");
        let candidate_bytes = serde_json::to_vec(candidate).expect("serialize candidate");
        decide_report_pair(
            &DecideArgs {
                manifest: directory.join("manifest.jsonl"),
                baseline_predictions: directory.join("baseline.predictions.jsonl"),
                candidate_predictions: directory.join("candidate.predictions.jsonl"),
                ranked_evaluator_binary: directory.join("ranked-evaluator"),
                ranked_evaluator_binary_blake3: "d".repeat(HASH_HEX_LEN),
                baseline_report_output: directory.join("baseline.report.json"),
                candidate_report_output: directory.join("candidate.report.json"),
                output: output.clone(),
                command_timeout_seconds: 300,
                evaluator: EvaluatorIdentityArgs {
                    evaluator_repository: directory.to_path_buf(),
                    evaluator_revision: TEST_REVISION.into(),
                    evaluator_binary_blake3: "d".repeat(HASH_HEX_LEN),
                },
            },
            test_artifact(),
            test_artifact(),
            &baseline_bytes,
            &candidate_bytes,
            &baseline.manifest_blake3,
            &baseline.predictions_blake3,
            &candidate.predictions_blake3,
        )
        .expect("decide reports");
        serde_json::from_slice(&fs::read(output).expect("read decision")).expect("parse decision")
    }

    fn test_artifact() -> ArtifactIdentity {
        ArtifactIdentity {
            revision: TEST_REVISION.into(),
            path: PathBuf::from("test-evaluator"),
            bytes: 123,
            blake3: "d".repeat(HASH_HEX_LEN),
        }
    }
}
