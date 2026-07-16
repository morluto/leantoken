use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    error::Error,
    fs,
    path::{Component, Path, PathBuf},
};

use clap::{Parser, Subcommand, ValueEnum};
use serde::{Deserialize, Serialize, de::DeserializeOwned};

const CONTRACT_SCHEMA_VERSION: u32 = 1;
const REPORT_SCHEMA_VERSION: u32 = 1;
const MAX_BUDGET: usize = 1_000_000;
const MAX_RELEVANT_LINES: usize = 5_000_000;

type RankedLine = (usize, String, usize);

#[derive(Debug, Parser)]
#[command(about = "Convert, validate, evaluate, and compare ranked code-region data")]
struct Args {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Convert a SWE-Explore JSONL file into the LeanToken evaluator contract.
    ConvertSweExplore {
        #[arg(long)]
        dataset: PathBuf,
        /// Optional JSON object mapping instance IDs to issue text.
        #[arg(long)]
        issue_map: Option<PathBuf>,
        /// Optional JSON object mapping instance IDs to pinned base commits.
        #[arg(long)]
        commit_map: Option<PathBuf>,
        #[arg(long)]
        output: PathBuf,
        #[arg(long, default_value_t = 2_000)]
        line_budget: usize,
        #[arg(long)]
        limit: Option<usize>,
    },
    /// Convert an internal representative manifest/report pair into ranked regions.
    ImportRepresentative {
        #[arg(long)]
        manifest: PathBuf,
        #[arg(long)]
        report: PathBuf,
        #[arg(long)]
        manifest_output: PathBuf,
        #[arg(long)]
        predictions_output: PathBuf,
    },
    /// Evaluate ranked predictions against evaluator-side labels.
    Evaluate {
        #[arg(long)]
        manifest: PathBuf,
        #[arg(long)]
        predictions: PathBuf,
        #[arg(long)]
        output: Option<PathBuf>,
    },
    /// Compare two reports produced from the same frozen manifest.
    Compare {
        #[arg(long)]
        baseline: PathBuf,
        #[arg(long)]
        candidate: PathBuf,
        #[arg(long)]
        output: Option<PathBuf>,
    },
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct RankedTask {
    schema_version: u32,
    dataset_kind: String,
    task_id: String,
    repository: RepositorySpec,
    query: String,
    language: String,
    strata: Strata,
    budget: Budget,
    relevant_files: Vec<String>,
    core_regions: Vec<Region>,
    #[serde(default)]
    optional_regions: Vec<Region>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct RepositorySpec {
    url: String,
    revision: String,
    #[serde(default)]
    path_style: PathStyle,
}

#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum PathStyle {
    #[default]
    Posix,
    Windows,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct Strata {
    repo_size_bucket: String,
    exact_identifier: bool,
    lexical_overlap_bucket: String,
    #[serde(default)]
    tags: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
struct Budget {
    kind: BudgetKind,
    amount: usize,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq, ValueEnum)]
#[serde(rename_all = "snake_case")]
enum BudgetKind {
    Lines,
    SourceTokens,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct Region {
    path: String,
    start_line: usize,
    end_line: usize,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct Prediction {
    schema_version: u32,
    task_id: String,
    manifest_blake3: String,
    repository_revision: String,
    budget: Budget,
    #[serde(default)]
    tokenizer: Option<String>,
    #[serde(default)]
    complete_response_tokens: Option<usize>,
    #[serde(default)]
    source_tokens: Option<usize>,
    #[serde(default)]
    latency_ms: Option<f64>,
    #[serde(default)]
    index_generation: Option<u64>,
    regions: Vec<PredictedRegion>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct PredictedRegion {
    path: String,
    start_line: usize,
    end_line: usize,
    rank: usize,
    #[serde(default)]
    source: Option<String>,
    #[serde(default)]
    facet: Option<String>,
    #[serde(default)]
    score: Option<f64>,
    #[serde(default)]
    token_count: Option<usize>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct EvaluationReport {
    schema_version: u32,
    dataset_kind: String,
    manifest_blake3: String,
    predictions_blake3: String,
    budget_kind: BudgetKind,
    tokenizer: Option<String>,
    aggregate: AggregateMetrics,
    strata: BTreeMap<String, AggregateMetrics>,
    tasks: Vec<TaskMetrics>,
    limitations: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct TaskMetrics {
    task_id: String,
    repository_revision: String,
    budget: Budget,
    language: String,
    strata: Strata,
    relevant_files: usize,
    relevant_files_hit: usize,
    core_lines: usize,
    core_lines_hit: usize,
    returned_lines: usize,
    core_returned_lines: usize,
    relevant_returned_lines: usize,
    core_regions: usize,
    core_regions_hit: usize,
    predicted_regions: usize,
    noise_regions: usize,
    file_recall: f64,
    line_recall: f64,
    line_precision: f64,
    line_f1: f64,
    context_efficiency: f64,
    hit_region_rate: f64,
    noise_region_rate: f64,
    ndcg_at_budget: f64,
    first_useful_hit_score: f64,
    complete_response_tokens: Option<usize>,
    source_tokens: Option<usize>,
    latency_ms: Option<f64>,
    index_generation: Option<u64>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
struct AggregateMetrics {
    task_count: usize,
    relevant_files: usize,
    relevant_files_hit: usize,
    core_lines: usize,
    core_lines_hit: usize,
    returned_lines: usize,
    core_returned_lines: usize,
    relevant_returned_lines: usize,
    core_regions: usize,
    core_regions_hit: usize,
    predicted_regions: usize,
    noise_regions: usize,
    file_recall_macro: f64,
    line_recall_macro: f64,
    line_precision_macro: f64,
    line_f1_macro: f64,
    context_efficiency_macro: f64,
    hit_region_rate_macro: f64,
    noise_region_rate_macro: f64,
    ndcg_at_budget_macro: f64,
    first_useful_hit_score_macro: f64,
    file_recall_micro: f64,
    line_recall_micro: f64,
    line_precision_micro: f64,
    line_f1_micro: f64,
    context_efficiency_micro: f64,
    hit_region_rate_micro: f64,
    noise_region_rate_micro: f64,
    complete_response_tokens: Option<usize>,
    source_tokens: Option<usize>,
    latency_ms: Option<f64>,
    complete_tokens_per_relevant_line: Option<f64>,
    source_tokens_per_relevant_line: Option<f64>,
}

#[derive(Debug, Serialize)]
struct ComparisonReport {
    schema_version: u32,
    dataset_kind: String,
    manifest_blake3: String,
    budget_kind: BudgetKind,
    tokenizer: Option<String>,
    task_count: usize,
    metrics: BTreeMap<String, MetricComparison>,
    tradeoffs: Vec<String>,
}

#[derive(Debug, Serialize)]
struct MetricComparison {
    baseline: f64,
    candidate: f64,
    absolute_delta: f64,
    relative_delta: Option<f64>,
}

#[derive(Debug, Deserialize)]
struct InternalManifest {
    #[serde(default = "development_dataset")]
    dataset_kind: String,
    corpora: Vec<InternalCorpus>,
}

#[derive(Debug, Deserialize)]
struct InternalCorpus {
    name: String,
    url: String,
    base_revision: String,
    tasks: Vec<InternalTask>,
}

#[derive(Debug, Deserialize)]
struct InternalTask {
    id: String,
    prompt: String,
    #[serde(default)]
    languages: Vec<String>,
    #[serde(default)]
    task_shapes: Vec<String>,
    relevant_files: Vec<InternalRelevantFile>,
    token_budget: usize,
}

#[derive(Debug, Deserialize)]
struct InternalRelevantFile {
    path: String,
    #[serde(default)]
    line_anchors: Vec<usize>,
}

#[derive(Debug, Deserialize)]
struct InternalReport {
    manifest_blake3: String,
    tokenizer: String,
    corpora: Vec<InternalCorpusReport>,
}

#[derive(Debug, Deserialize)]
struct InternalCorpusReport {
    name: String,
    base_revision: String,
    indexed_files: usize,
    tasks: Vec<InternalTaskReport>,
}

#[derive(Debug, Deserialize)]
struct InternalTaskReport {
    id: String,
    leantoken_source_tokens: usize,
    leantoken_total_json_tokens: usize,
    first_context_ms: f64,
    returned_evidence: Vec<InternalEvidence>,
}

#[derive(Debug, Deserialize)]
struct InternalEvidence {
    path: String,
    start_line: usize,
    end_line: usize,
    representation: String,
    score: f64,
    token_count: usize,
}

fn main() -> Result<(), Box<dyn Error>> {
    match Args::parse().command {
        Command::ConvertSweExplore {
            dataset,
            issue_map,
            commit_map,
            output,
            line_budget,
            limit,
        } => convert_swe_explore(
            &dataset,
            issue_map.as_deref(),
            commit_map.as_deref(),
            &output,
            line_budget,
            limit,
        )?,
        Command::ImportRepresentative {
            manifest,
            report,
            manifest_output,
            predictions_output,
        } => import_representative(&manifest, &report, &manifest_output, &predictions_output)?,
        Command::Evaluate {
            manifest,
            predictions,
            output,
        } => {
            let report = evaluate_files(&manifest, &predictions)?;
            write_json_report(&report, output.as_deref())?;
        }
        Command::Compare {
            baseline,
            candidate,
            output,
        } => {
            let report = compare_files(&baseline, &candidate)?;
            write_json_report(&report, output.as_deref())?;
        }
    }
    Ok(())
}

fn convert_swe_explore(
    dataset: &Path,
    issue_map_path: Option<&Path>,
    commit_map_path: Option<&Path>,
    output: &Path,
    line_budget: usize,
    limit: Option<usize>,
) -> Result<(), Box<dyn Error>> {
    validate_budget(line_budget)?;
    let issue_map = read_optional_string_map(issue_map_path)?;
    let commit_map = read_optional_string_map(commit_map_path)?;
    let records = read_jsonl::<serde_json::Value>(dataset)?;
    let tasks = records
        .into_iter()
        .take(limit.unwrap_or(usize::MAX))
        .map(|record| convert_swe_record(&record, line_budget, &issue_map, &commit_map))
        .collect::<Result<Vec<_>, _>>()?;
    validate_task_ids(&tasks)?;
    write_jsonl(output, &tasks)?;
    let manifest_blake3 = blake3::hash(&fs::read(output)?).to_hex().to_string();
    eprintln!(
        "converted {} SWE-Explore tasks to {} with manifest {}",
        tasks.len(),
        output.display(),
        manifest_blake3
    );
    Ok(())
}

fn convert_swe_record(
    record: &serde_json::Value,
    line_budget: usize,
    issue_map: &BTreeMap<String, String>,
    commit_map: &BTreeMap<String, String>,
) -> Result<RankedTask, Box<dyn Error>> {
    let task_id = required_string(record, &["/instance_id"])?;
    let query = optional_string(record, &["/problem_statement", "/issue"])
        .or_else(|| issue_map.get(&task_id).cloned())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            format!("{task_id} has no issue text; provide problem_statement or --issue-map")
        })?;
    let repository_name = optional_string(record, &["/repo", "/repository"])
        .or_else(|| repository_from_instance_id(&task_id))
        .ok_or_else(|| format!("{task_id} has no repository identity"))?;
    let revision = optional_string(record, &["/base_commit", "/meta/base_commit", "/revision"])
        .or_else(|| commit_map.get(&task_id).cloned())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            format!("{task_id} has no pinned revision; provide base_commit or --commit-map")
        })?;
    validate_revision(&revision)?;
    let language = optional_string(record, &["/language", "/meta/language"])
        .unwrap_or_else(|| "unknown".to_owned());
    let core_regions = required_regions(record, "/ground_truth/read_core_regions")?;
    let relevant_files = optional_strings(record, "/ground_truth/read_core_files")
        .unwrap_or_else(|| unique_paths(&core_regions));
    let optional_regions =
        flatten_optional_regions(record.pointer("/ground_truth/read_optional_regions_map"))?;
    let lexical_overlap_bucket = optional_string(record, &["/meta/lexical_overlap_bucket"])
        .unwrap_or_else(|| "unknown".into());
    let repo_size_bucket =
        optional_string(record, &["/meta/repo_size_bucket"]).unwrap_or_else(|| "unknown".into());
    Ok(RankedTask {
        schema_version: CONTRACT_SCHEMA_VERSION,
        dataset_kind: "swe_explore".into(),
        task_id,
        repository: RepositorySpec {
            url: format!(
                "https://github.com/{}.git",
                repository_name.trim_end_matches(".git")
            ),
            revision,
            path_style: PathStyle::Posix,
        },
        query: query.clone(),
        language,
        strata: Strata {
            repo_size_bucket,
            exact_identifier: has_exact_identifier(&query),
            lexical_overlap_bucket,
            tags: Vec::new(),
        },
        budget: Budget {
            kind: BudgetKind::Lines,
            amount: line_budget,
        },
        relevant_files,
        core_regions,
        optional_regions,
    })
}

fn import_representative(
    manifest_path: &Path,
    report_path: &Path,
    manifest_output: &Path,
    predictions_output: &Path,
) -> Result<(), Box<dyn Error>> {
    let manifest_json = fs::read_to_string(manifest_path)?;
    let internal_manifest: InternalManifest = serde_json::from_str(&manifest_json)?;
    let report_json = fs::read_to_string(report_path)?;
    let internal_report: InternalReport = serde_json::from_str(&report_json)?;
    let source_manifest_hash = blake3::hash(manifest_json.as_bytes()).to_hex().to_string();
    if source_manifest_hash != internal_report.manifest_blake3 {
        return Err("representative report does not match its source manifest".into());
    }
    let report_corpora = internal_report
        .corpora
        .iter()
        .map(|corpus| (corpus.name.as_str(), corpus))
        .collect::<HashMap<_, _>>();
    let mut tasks = Vec::new();
    let mut pending_predictions = Vec::new();
    for corpus in &internal_manifest.corpora {
        let corpus_report = report_corpora
            .get(corpus.name.as_str())
            .ok_or_else(|| format!("report is missing corpus {}", corpus.name))?;
        if corpus_report.base_revision != corpus.base_revision {
            return Err(format!("revision mismatch for corpus {}", corpus.name).into());
        }
        let task_reports = corpus_report
            .tasks
            .iter()
            .map(|task| (task.id.as_str(), task))
            .collect::<HashMap<_, _>>();
        for task in &corpus.tasks {
            let task_report = task_reports
                .get(task.id.as_str())
                .ok_or_else(|| format!("report is missing task {}", task.id))?;
            let language = task
                .languages
                .first()
                .cloned()
                .unwrap_or_else(|| infer_language(&task.relevant_files));
            let ranked_task = RankedTask {
                schema_version: CONTRACT_SCHEMA_VERSION,
                dataset_kind: internal_manifest.dataset_kind.clone(),
                task_id: task.id.clone(),
                repository: RepositorySpec {
                    url: corpus.url.clone(),
                    revision: corpus.base_revision.clone(),
                    path_style: PathStyle::Posix,
                },
                query: task.prompt.clone(),
                language,
                strata: Strata {
                    repo_size_bucket: repo_size_bucket(corpus_report.indexed_files).into(),
                    exact_identifier: has_exact_identifier(&task.prompt),
                    lexical_overlap_bucket: "unknown".into(),
                    tags: task.task_shapes.clone(),
                },
                budget: Budget {
                    kind: BudgetKind::SourceTokens,
                    amount: task.token_budget,
                },
                relevant_files: task
                    .relevant_files
                    .iter()
                    .map(|file| file.path.clone())
                    .collect(),
                core_regions: task
                    .relevant_files
                    .iter()
                    .flat_map(|file| {
                        file.line_anchors.iter().map(|line| Region {
                            path: file.path.clone(),
                            start_line: *line,
                            end_line: *line,
                        })
                    })
                    .collect(),
                optional_regions: Vec::new(),
            };
            let prediction = Prediction {
                schema_version: CONTRACT_SCHEMA_VERSION,
                task_id: task.id.clone(),
                manifest_blake3: String::new(),
                repository_revision: corpus.base_revision.clone(),
                budget: ranked_task.budget.clone(),
                tokenizer: Some(internal_report.tokenizer.clone()),
                complete_response_tokens: Some(task_report.leantoken_total_json_tokens),
                source_tokens: Some(task_report.leantoken_source_tokens),
                latency_ms: Some(task_report.first_context_ms),
                index_generation: None,
                regions: task_report
                    .returned_evidence
                    .iter()
                    .enumerate()
                    .map(|(index, evidence)| PredictedRegion {
                        path: evidence.path.clone(),
                        start_line: evidence.start_line,
                        end_line: evidence.end_line,
                        rank: index + 1,
                        source: Some(evidence.representation.clone()),
                        facet: None,
                        score: Some(evidence.score),
                        token_count: Some(evidence.token_count),
                    })
                    .collect(),
            };
            tasks.push(ranked_task);
            pending_predictions.push(prediction);
        }
    }
    validate_task_ids(&tasks)?;
    write_jsonl(manifest_output, &tasks)?;
    let manifest_bytes = fs::read(manifest_output)?;
    let manifest_blake3 = blake3::hash(&manifest_bytes).to_hex().to_string();
    for prediction in &mut pending_predictions {
        prediction.manifest_blake3.clone_from(&manifest_blake3);
    }
    write_jsonl(predictions_output, &pending_predictions)?;
    eprintln!(
        "imported {} tasks with ranked manifest {}",
        tasks.len(),
        manifest_blake3
    );
    Ok(())
}

fn evaluate_files(
    manifest_path: &Path,
    predictions_path: &Path,
) -> Result<EvaluationReport, Box<dyn Error>> {
    let manifest_bytes = fs::read(manifest_path)?;
    let manifest_blake3 = blake3::hash(&manifest_bytes).to_hex().to_string();
    let prediction_bytes = fs::read(predictions_path)?;
    let predictions_blake3 = blake3::hash(&prediction_bytes).to_hex().to_string();
    let tasks = parse_jsonl::<RankedTask>(&manifest_bytes)?;
    let predictions = parse_jsonl::<Prediction>(&prediction_bytes)?;
    evaluate(tasks, predictions, manifest_blake3, predictions_blake3)
}

fn evaluate(
    tasks: Vec<RankedTask>,
    predictions: Vec<Prediction>,
    manifest_blake3: String,
    predictions_blake3: String,
) -> Result<EvaluationReport, Box<dyn Error>> {
    validate_task_ids(&tasks)?;
    let dataset_kind = uniform_value(
        tasks.iter().map(|task| task.dataset_kind.clone()),
        "dataset kind",
    )?;
    let budget_kind = uniform_value(tasks.iter().map(|task| task.budget.kind), "budget kind")?;
    let prediction_map = prediction_map(predictions)?;
    if prediction_map.len() != tasks.len() {
        return Err("manifest and prediction task counts differ".into());
    }
    let mut task_metrics = Vec::with_capacity(tasks.len());
    let mut tokenizer_values = Vec::new();
    for task in &tasks {
        validate_task(task)?;
        let prediction = prediction_map
            .get(task.task_id.as_str())
            .ok_or_else(|| format!("missing prediction for {}", task.task_id))?;
        validate_prediction(task, prediction, &manifest_blake3)?;
        tokenizer_values.push(prediction.tokenizer.clone());
        task_metrics.push(evaluate_task(task, prediction)?);
    }
    let tokenizer = uniform_value(tokenizer_values.into_iter(), "tokenizer")?;
    let aggregate = aggregate_metrics(task_metrics.iter());
    let strata = stratified_metrics(&task_metrics);
    Ok(EvaluationReport {
        schema_version: REPORT_SCHEMA_VERSION,
        dataset_kind,
        manifest_blake3,
        predictions_blake3,
        budget_kind,
        tokenizer,
        aggregate,
        strata,
        tasks: task_metrics,
        limitations: [
            "Relevant regions are evaluator labels; they do not prove that every labeled line is necessary or that every unlabeled line is useless.",
            "Line NDCG ranks newly emitted lines, assigns gain 2 to core lines and 1 to optional lines, and ignores overlapping repeated lines.",
            "Source-token budgets use the prediction's authoritative emitted source-token total; line budgets are enforced directly by unique returned lines.",
            "This evaluator measures retrieval, not patch correctness or model task success.",
        ]
        .map(str::to_owned)
        .to_vec(),
    })
}

fn prediction_map(
    predictions: Vec<Prediction>,
) -> Result<HashMap<String, Prediction>, Box<dyn Error>> {
    let mut result = HashMap::new();
    for prediction in predictions {
        if result
            .insert(prediction.task_id.clone(), prediction)
            .is_some()
        {
            return Err("prediction task IDs must be unique".into());
        }
    }
    Ok(result)
}

fn validate_task(task: &RankedTask) -> Result<(), Box<dyn Error>> {
    if task.schema_version != CONTRACT_SCHEMA_VERSION {
        return Err(format!(
            "{} uses unsupported task schema {}",
            task.task_id, task.schema_version
        )
        .into());
    }
    if task.task_id.trim().is_empty()
        || task.dataset_kind.trim().is_empty()
        || task.query.trim().is_empty()
        || task.language.trim().is_empty()
        || task.repository.url.trim().is_empty()
    {
        return Err(format!("{} has an empty required field", task.task_id).into());
    }
    validate_revision(&task.repository.revision)?;
    validate_budget(task.budget.amount)?;
    for path in &task.relevant_files {
        normalize_path(path, task.repository.path_style)?;
    }
    validate_regions(&task.core_regions, task.repository.path_style)?;
    validate_regions(&task.optional_regions, task.repository.path_style)?;
    let relevant_lines = task
        .core_regions
        .iter()
        .chain(&task.optional_regions)
        .try_fold(0usize, |total, region| {
            total
                .checked_add(region.end_line - region.start_line + 1)
                .ok_or("relevant line count overflow")
        })?;
    if relevant_lines > MAX_RELEVANT_LINES {
        return Err(format!("{} has too many relevant lines", task.task_id).into());
    }
    Ok(())
}

fn validate_prediction(
    task: &RankedTask,
    prediction: &Prediction,
    manifest_blake3: &str,
) -> Result<(), Box<dyn Error>> {
    if prediction.schema_version != CONTRACT_SCHEMA_VERSION {
        return Err(format!(
            "{} uses unsupported prediction schema {}",
            prediction.task_id, prediction.schema_version
        )
        .into());
    }
    if prediction.manifest_blake3 != manifest_blake3 {
        return Err(format!(
            "{} prediction manifest hash does not match",
            prediction.task_id
        )
        .into());
    }
    if prediction.repository_revision != task.repository.revision {
        return Err(format!("{} prediction revision does not match", task.task_id).into());
    }
    if prediction.budget != task.budget {
        return Err(format!("{} prediction budget does not match", task.task_id).into());
    }
    let mut ranks = BTreeSet::new();
    for region in &prediction.regions {
        validate_region(
            &Region {
                path: region.path.clone(),
                start_line: region.start_line,
                end_line: region.end_line,
            },
            task.repository.path_style,
        )?;
        if region.rank == 0 || !ranks.insert(region.rank) {
            return Err(format!("{} has invalid or duplicate ranks", task.task_id).into());
        }
        if region.score.is_some_and(|score| !score.is_finite()) {
            return Err(format!("{} has a non-finite score", task.task_id).into());
        }
    }
    if !ranks.iter().copied().eq(1..=ranks.len()) {
        return Err(format!("{} ranks must be contiguous from one", task.task_id).into());
    }
    if task.budget.kind == BudgetKind::SourceTokens {
        let used = prediction.source_tokens.ok_or_else(|| {
            format!(
                "{} source-token prediction lacks authoritative source_tokens",
                task.task_id
            )
        })?;
        if used > task.budget.amount {
            return Err(format!(
                "{} spends {used} source tokens over budget {}",
                task.task_id, task.budget.amount
            )
            .into());
        }
    }
    Ok(())
}

fn evaluate_task(
    task: &RankedTask,
    prediction: &Prediction,
) -> Result<TaskMetrics, Box<dyn Error>> {
    let style = task.repository.path_style;
    let core = region_lines(&task.core_regions, style)?;
    let optional = region_lines(&task.optional_regions, style)?;
    let relevant = core.union(&optional).cloned().collect::<HashSet<_>>();
    let relevant_files = task
        .relevant_files
        .iter()
        .map(|path| normalize_path(path, style))
        .collect::<Result<HashSet<_>, _>>()?;
    let ranked_lines = prediction_lines(task, prediction)?;
    let returned = ranked_lines
        .iter()
        .map(|(_, path, line)| (path.clone(), *line))
        .collect::<HashSet<_>>();
    let returned_files = returned
        .iter()
        .map(|(path, _)| path.clone())
        .collect::<HashSet<_>>();
    let relevant_files_hit = relevant_files.intersection(&returned_files).count();
    let core_lines_hit = core.intersection(&returned).count();
    let core_returned_lines = core_lines_hit;
    let relevant_returned_lines = relevant.intersection(&returned).count();
    let core_regions_hit = task
        .core_regions
        .iter()
        .filter(|region| region_hits(region, style, &returned))
        .count();
    let contributing_ranks = ranked_lines
        .iter()
        .map(|(rank, _, _)| *rank)
        .collect::<HashSet<_>>();
    let useful_ranks = ranked_lines
        .iter()
        .filter(|(_, path, line)| relevant.contains(&(path.clone(), *line)))
        .map(|(rank, _, _)| *rank)
        .collect::<HashSet<_>>();
    let predicted_regions = contributing_ranks.len();
    let noise_regions = predicted_regions.saturating_sub(useful_ranks.len());
    let first_useful_hit_score = useful_ranks
        .iter()
        .min()
        .map_or(0.0, |rank| 1.0 / *rank as f64);
    let ndcg_at_budget = line_ndcg(&ranked_lines, &core, &optional);

    Ok(TaskMetrics {
        task_id: task.task_id.clone(),
        repository_revision: task.repository.revision.clone(),
        budget: task.budget.clone(),
        language: task.language.clone(),
        strata: task.strata.clone(),
        relevant_files: relevant_files.len(),
        relevant_files_hit,
        core_lines: core.len(),
        core_lines_hit,
        returned_lines: returned.len(),
        core_returned_lines,
        relevant_returned_lines,
        core_regions: task.core_regions.len(),
        core_regions_hit,
        predicted_regions,
        noise_regions,
        file_recall: ratio(relevant_files_hit, relevant_files.len()),
        line_recall: ratio(core_lines_hit, core.len()),
        line_precision: ratio(core_returned_lines, returned.len()),
        line_f1: f1(
            ratio(core_returned_lines, returned.len()),
            ratio(core_lines_hit, core.len()),
        ),
        context_efficiency: ratio(relevant_returned_lines, returned.len()),
        hit_region_rate: ratio(core_regions_hit, task.core_regions.len()),
        noise_region_rate: ratio(noise_regions, predicted_regions),
        ndcg_at_budget,
        first_useful_hit_score,
        complete_response_tokens: prediction.complete_response_tokens,
        source_tokens: prediction.source_tokens,
        latency_ms: prediction.latency_ms,
        index_generation: prediction.index_generation,
    })
}

fn prediction_lines(
    task: &RankedTask,
    prediction: &Prediction,
) -> Result<Vec<RankedLine>, Box<dyn Error>> {
    let mut regions = prediction.regions.iter().collect::<Vec<_>>();
    regions.sort_by_key(|region| region.rank);
    let mut seen = HashSet::new();
    let mut ranked_lines = Vec::new();
    let mut line_budget_used = 0usize;
    for region in regions {
        let path = normalize_path(&region.path, task.repository.path_style)?;
        for line in region.start_line..=region.end_line {
            if !seen.insert((path.clone(), line)) {
                continue;
            }
            if task.budget.kind == BudgetKind::Lines && line_budget_used == task.budget.amount {
                return Ok(ranked_lines);
            }
            ranked_lines.push((region.rank, path.clone(), line));
            line_budget_used += 1;
        }
    }
    Ok(ranked_lines)
}

fn line_ndcg(
    ranked_lines: &[RankedLine],
    core: &HashSet<(String, usize)>,
    optional: &HashSet<(String, usize)>,
) -> f64 {
    let evaluated = ranked_lines.len();
    let dcg = ranked_lines
        .iter()
        .take(evaluated)
        .enumerate()
        .map(|(index, (_, path, line))| {
            let key = (path.clone(), *line);
            let gain = if core.contains(&key) {
                2.0
            } else if optional.contains(&key) {
                1.0
            } else {
                0.0
            };
            gain / ((index + 2) as f64).log2()
        })
        .sum::<f64>();
    let core_lines = core.len().min(evaluated);
    let optional_lines = optional
        .difference(core)
        .count()
        .min(evaluated.saturating_sub(core_lines));
    let idcg = (0..core_lines)
        .map(|index| 2.0 / ((index + 2) as f64).log2())
        .chain(
            (core_lines..core_lines + optional_lines)
                .map(|index| 1.0 / ((index + 2) as f64).log2()),
        )
        .sum::<f64>();
    if idcg == 0.0 { 0.0 } else { dcg / idcg }
}

fn aggregate_metrics<'a>(metrics: impl Iterator<Item = &'a TaskMetrics>) -> AggregateMetrics {
    let metrics = metrics.collect::<Vec<_>>();
    let mut aggregate = AggregateMetrics {
        task_count: metrics.len(),
        ..AggregateMetrics::default()
    };
    for task in &metrics {
        aggregate.relevant_files += task.relevant_files;
        aggregate.relevant_files_hit += task.relevant_files_hit;
        aggregate.core_lines += task.core_lines;
        aggregate.core_lines_hit += task.core_lines_hit;
        aggregate.returned_lines += task.returned_lines;
        aggregate.core_returned_lines += task.core_returned_lines;
        aggregate.relevant_returned_lines += task.relevant_returned_lines;
        aggregate.core_regions += task.core_regions;
        aggregate.core_regions_hit += task.core_regions_hit;
        aggregate.predicted_regions += task.predicted_regions;
        aggregate.noise_regions += task.noise_regions;
        aggregate.file_recall_macro += task.file_recall;
        aggregate.line_recall_macro += task.line_recall;
        aggregate.line_precision_macro += task.line_precision;
        aggregate.line_f1_macro += task.line_f1;
        aggregate.context_efficiency_macro += task.context_efficiency;
        aggregate.hit_region_rate_macro += task.hit_region_rate;
        aggregate.noise_region_rate_macro += task.noise_region_rate;
        aggregate.ndcg_at_budget_macro += task.ndcg_at_budget;
        aggregate.first_useful_hit_score_macro += task.first_useful_hit_score;
    }
    if aggregate.task_count > 0 {
        let count = aggregate.task_count as f64;
        aggregate.file_recall_macro /= count;
        aggregate.line_recall_macro /= count;
        aggregate.line_precision_macro /= count;
        aggregate.line_f1_macro /= count;
        aggregate.context_efficiency_macro /= count;
        aggregate.hit_region_rate_macro /= count;
        aggregate.noise_region_rate_macro /= count;
        aggregate.ndcg_at_budget_macro /= count;
        aggregate.first_useful_hit_score_macro /= count;
    }
    aggregate.file_recall_micro = ratio(aggregate.relevant_files_hit, aggregate.relevant_files);
    aggregate.line_recall_micro = ratio(aggregate.core_lines_hit, aggregate.core_lines);
    aggregate.line_precision_micro = ratio(aggregate.core_returned_lines, aggregate.returned_lines);
    aggregate.line_f1_micro = f1(aggregate.line_precision_micro, aggregate.line_recall_micro);
    aggregate.context_efficiency_micro =
        ratio(aggregate.relevant_returned_lines, aggregate.returned_lines);
    aggregate.hit_region_rate_micro = ratio(aggregate.core_regions_hit, aggregate.core_regions);
    aggregate.noise_region_rate_micro = ratio(aggregate.noise_regions, aggregate.predicted_regions);
    aggregate.complete_response_tokens =
        complete_sum(metrics.iter().map(|task| task.complete_response_tokens));
    aggregate.source_tokens = complete_sum(metrics.iter().map(|task| task.source_tokens));
    aggregate.latency_ms = complete_float_sum(metrics.iter().map(|task| task.latency_ms));
    aggregate.complete_tokens_per_relevant_line = aggregate
        .complete_response_tokens
        .filter(|_| aggregate.relevant_returned_lines > 0)
        .map(|tokens| tokens as f64 / aggregate.relevant_returned_lines as f64);
    aggregate.source_tokens_per_relevant_line = aggregate
        .source_tokens
        .filter(|_| aggregate.relevant_returned_lines > 0)
        .map(|tokens| tokens as f64 / aggregate.relevant_returned_lines as f64);
    aggregate
}

fn stratified_metrics(tasks: &[TaskMetrics]) -> BTreeMap<String, AggregateMetrics> {
    let mut groups = BTreeMap::<String, Vec<&TaskMetrics>>::new();
    for task in tasks {
        for key in [
            format!("language:{}", task.language),
            format!("repo_size:{}", task.strata.repo_size_bucket),
            format!("exact_identifier:{}", task.strata.exact_identifier),
            format!("lexical_overlap:{}", task.strata.lexical_overlap_bucket),
        ]
        .into_iter()
        .chain(task.strata.tags.iter().map(|tag| format!("tag:{tag}")))
        {
            groups.entry(key).or_default().push(task);
        }
    }
    groups
        .into_iter()
        .map(|(key, tasks)| (key, aggregate_metrics(tasks.into_iter())))
        .collect()
}

fn compare_files(
    baseline_path: &Path,
    candidate_path: &Path,
) -> Result<ComparisonReport, Box<dyn Error>> {
    let baseline: EvaluationReport = serde_json::from_str(&fs::read_to_string(baseline_path)?)?;
    let candidate: EvaluationReport = serde_json::from_str(&fs::read_to_string(candidate_path)?)?;
    compare_reports(&baseline, &candidate)
}

fn compare_reports(
    baseline: &EvaluationReport,
    candidate: &EvaluationReport,
) -> Result<ComparisonReport, Box<dyn Error>> {
    if baseline.schema_version != REPORT_SCHEMA_VERSION
        || candidate.schema_version != REPORT_SCHEMA_VERSION
    {
        return Err("reports use an unsupported schema version".into());
    }
    if baseline.manifest_blake3 != candidate.manifest_blake3 {
        return Err("reports use different manifests".into());
    }
    if baseline.dataset_kind != candidate.dataset_kind {
        return Err("reports use different dataset kinds".into());
    }
    if baseline.budget_kind != candidate.budget_kind {
        return Err("reports use different budget kinds".into());
    }
    if baseline.tokenizer != candidate.tokenizer {
        return Err("reports use different tokenizers".into());
    }
    if baseline.aggregate.task_count != candidate.aggregate.task_count {
        return Err("reports contain different task counts".into());
    }
    if baseline.tasks.len() != baseline.aggregate.task_count
        || candidate.tasks.len() != candidate.aggregate.task_count
    {
        return Err("report task details do not match aggregate task counts".into());
    }
    let baseline_tasks = report_task_map(&baseline.tasks)?;
    let candidate_tasks = report_task_map(&candidate.tasks)?;
    if baseline_tasks.len() != candidate_tasks.len()
        || baseline_tasks.keys().ne(candidate_tasks.keys())
    {
        return Err("reports contain different task IDs".into());
    }
    for (task_id, baseline_task) in baseline_tasks {
        let candidate_task = candidate_tasks
            .get(task_id)
            .expect("task keys were checked above");
        if baseline_task.repository_revision != candidate_task.repository_revision {
            return Err(format!("reports use different revisions for {task_id}").into());
        }
        if baseline_task.budget != candidate_task.budget {
            return Err(format!("reports use different budgets for {task_id}").into());
        }
    }
    let mut metrics = BTreeMap::new();
    for (name, baseline_value, candidate_value) in [
        (
            "file_recall_macro",
            baseline.aggregate.file_recall_macro,
            candidate.aggregate.file_recall_macro,
        ),
        (
            "line_recall_macro",
            baseline.aggregate.line_recall_macro,
            candidate.aggregate.line_recall_macro,
        ),
        (
            "line_precision_macro",
            baseline.aggregate.line_precision_macro,
            candidate.aggregate.line_precision_macro,
        ),
        (
            "line_f1_macro",
            baseline.aggregate.line_f1_macro,
            candidate.aggregate.line_f1_macro,
        ),
        (
            "context_efficiency_macro",
            baseline.aggregate.context_efficiency_macro,
            candidate.aggregate.context_efficiency_macro,
        ),
        (
            "ndcg_at_budget_macro",
            baseline.aggregate.ndcg_at_budget_macro,
            candidate.aggregate.ndcg_at_budget_macro,
        ),
        (
            "noise_region_rate_macro",
            baseline.aggregate.noise_region_rate_macro,
            candidate.aggregate.noise_region_rate_macro,
        ),
    ] {
        metrics.insert(
            name.to_owned(),
            metric_comparison(baseline_value, candidate_value),
        );
    }
    if let (Some(baseline_tokens), Some(candidate_tokens)) = (
        baseline.aggregate.complete_response_tokens,
        candidate.aggregate.complete_response_tokens,
    ) {
        metrics.insert(
            "complete_response_tokens".into(),
            metric_comparison(baseline_tokens as f64, candidate_tokens as f64),
        );
    }
    if let (Some(baseline_tokens), Some(candidate_tokens)) = (
        baseline.aggregate.source_tokens,
        candidate.aggregate.source_tokens,
    ) {
        metrics.insert(
            "source_tokens".into(),
            metric_comparison(baseline_tokens as f64, candidate_tokens as f64),
        );
    }
    if let (Some(baseline_cost), Some(candidate_cost)) = (
        baseline.aggregate.complete_tokens_per_relevant_line,
        candidate.aggregate.complete_tokens_per_relevant_line,
    ) {
        metrics.insert(
            "complete_tokens_per_relevant_line".into(),
            metric_comparison(baseline_cost, candidate_cost),
        );
    }
    if let (Some(baseline_cost), Some(candidate_cost)) = (
        baseline.aggregate.source_tokens_per_relevant_line,
        candidate.aggregate.source_tokens_per_relevant_line,
    ) {
        metrics.insert(
            "source_tokens_per_relevant_line".into(),
            metric_comparison(baseline_cost, candidate_cost),
        );
    }
    let mut tradeoffs = Vec::new();
    if candidate.aggregate.line_recall_macro > baseline.aggregate.line_recall_macro
        && candidate.aggregate.line_precision_macro < baseline.aggregate.line_precision_macro
    {
        tradeoffs.push("line recall improved while line precision declined".into());
    }
    if candidate.aggregate.ndcg_at_budget_macro > baseline.aggregate.ndcg_at_budget_macro
        && candidate.aggregate.complete_response_tokens
            > baseline.aggregate.complete_response_tokens
    {
        tradeoffs.push("rank quality improved while complete response tokens increased".into());
    }
    if tradeoffs.is_empty() {
        tradeoffs.push(
            "No predefined tradeoff trigger fired; inspect every metric instead of treating this report as a single winner score."
                .into(),
        );
    }
    Ok(ComparisonReport {
        schema_version: REPORT_SCHEMA_VERSION,
        dataset_kind: baseline.dataset_kind.clone(),
        manifest_blake3: baseline.manifest_blake3.clone(),
        budget_kind: baseline.budget_kind,
        tokenizer: baseline.tokenizer.clone(),
        task_count: baseline.aggregate.task_count,
        metrics,
        tradeoffs,
    })
}

fn report_task_map(tasks: &[TaskMetrics]) -> Result<BTreeMap<&str, &TaskMetrics>, Box<dyn Error>> {
    let mut result = BTreeMap::new();
    for task in tasks {
        if result.insert(task.task_id.as_str(), task).is_some() {
            return Err(format!("report contains duplicate task ID {}", task.task_id).into());
        }
    }
    Ok(result)
}

fn metric_comparison(baseline: f64, candidate: f64) -> MetricComparison {
    let absolute_delta = candidate - baseline;
    MetricComparison {
        baseline,
        candidate,
        absolute_delta,
        relative_delta: (baseline != 0.0).then_some(absolute_delta / baseline.abs()),
    }
}

fn region_lines(
    regions: &[Region],
    style: PathStyle,
) -> Result<HashSet<(String, usize)>, Box<dyn Error>> {
    let mut lines = HashSet::new();
    for region in regions {
        let path = normalize_path(&region.path, style)?;
        for line in region.start_line..=region.end_line {
            lines.insert((path.clone(), line));
        }
    }
    Ok(lines)
}

fn region_hits(region: &Region, style: PathStyle, returned: &HashSet<(String, usize)>) -> bool {
    normalize_path(&region.path, style).is_ok_and(|path| {
        (region.start_line..=region.end_line).any(|line| returned.contains(&(path.clone(), line)))
    })
}

fn validate_regions(regions: &[Region], style: PathStyle) -> Result<(), Box<dyn Error>> {
    for region in regions {
        validate_region(region, style)?;
    }
    Ok(())
}

fn validate_region(region: &Region, style: PathStyle) -> Result<(), Box<dyn Error>> {
    normalize_path(&region.path, style)?;
    if region.start_line == 0 || region.end_line < region.start_line {
        return Err(format!(
            "invalid line range {}:{}-{}",
            region.path, region.start_line, region.end_line
        )
        .into());
    }
    if region.end_line - region.start_line + 1 > MAX_BUDGET {
        return Err(format!("range is too large: {}", region.path).into());
    }
    Ok(())
}

fn normalize_path(path: &str, style: PathStyle) -> Result<String, Box<dyn Error>> {
    if path.is_empty() || path.contains('\0') {
        return Err("repository path must be non-empty and contain no NUL".into());
    }
    let normalized = match style {
        PathStyle::Posix => path.to_owned(),
        PathStyle::Windows => path.replace('\\', "/"),
    };
    if normalized.starts_with('/')
        || normalized.starts_with('\\')
        || normalized
            .as_bytes()
            .get(1)
            .is_some_and(|value| *value == b':')
    {
        return Err(format!("repository path must be relative: {path}").into());
    }
    if normalized
        .split('/')
        .any(|part| part.is_empty() || matches!(part, "." | ".."))
    {
        return Err(format!("repository path is not normalized: {path}").into());
    }
    if Path::new(&normalized).components().any(|component| {
        matches!(
            component,
            Component::ParentDir | Component::RootDir | Component::Prefix(_)
        )
    }) {
        return Err(format!("repository path escapes its root: {path}").into());
    }
    Ok(normalized)
}

fn validate_revision(revision: &str) -> Result<(), Box<dyn Error>> {
    if revision.len() != 40 || !revision.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(
            format!("revision must be a pinned 40-character Git object ID: {revision}").into(),
        );
    }
    Ok(())
}

fn validate_budget(budget: usize) -> Result<(), Box<dyn Error>> {
    if !(1..=MAX_BUDGET).contains(&budget) {
        return Err(format!("budget must be between 1 and {MAX_BUDGET}").into());
    }
    Ok(())
}

fn validate_task_ids(tasks: &[RankedTask]) -> Result<(), Box<dyn Error>> {
    let mut ids = HashSet::new();
    for task in tasks {
        if !ids.insert(task.task_id.as_str()) {
            return Err(format!("duplicate task ID {}", task.task_id).into());
        }
    }
    Ok(())
}

fn read_jsonl<T: DeserializeOwned>(path: &Path) -> Result<Vec<T>, Box<dyn Error>> {
    parse_jsonl(&fs::read(path)?)
}

fn parse_jsonl<T: DeserializeOwned>(bytes: &[u8]) -> Result<Vec<T>, Box<dyn Error>> {
    std::str::from_utf8(bytes)?
        .lines()
        .enumerate()
        .filter(|(_, line)| !line.trim().is_empty())
        .map(|(index, line)| {
            serde_json::from_str(line).map_err(|error| {
                format!("invalid JSONL record on line {}: {error}", index + 1).into()
            })
        })
        .collect()
}

fn write_jsonl<T: Serialize>(path: &Path, values: &[T]) -> Result<(), Box<dyn Error>> {
    if let Some(parent) = path.parent().filter(|path| !path.as_os_str().is_empty()) {
        fs::create_dir_all(parent)?;
    }
    let mut output = String::new();
    for value in values {
        output.push_str(&serde_json::to_string(value)?);
        output.push('\n');
    }
    fs::write(path, output)?;
    Ok(())
}

fn write_json_report<T: Serialize>(
    report: &T,
    output: Option<&Path>,
) -> Result<(), Box<dyn Error>> {
    let json = serde_json::to_string_pretty(report)?;
    if let Some(output) = output {
        if let Some(parent) = output.parent().filter(|path| !path.as_os_str().is_empty()) {
            fs::create_dir_all(parent)?;
        }
        fs::write(output, &json)?;
    }
    println!("{json}");
    Ok(())
}

fn required_string(value: &serde_json::Value, pointers: &[&str]) -> Result<String, Box<dyn Error>> {
    optional_string(value, pointers)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| format!("missing required string at one of {pointers:?}").into())
}

fn optional_string(value: &serde_json::Value, pointers: &[&str]) -> Option<String> {
    pointers.iter().find_map(|pointer| {
        value
            .pointer(pointer)
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned)
    })
}

fn optional_strings(value: &serde_json::Value, pointer: &str) -> Option<Vec<String>> {
    value.pointer(pointer)?.as_array().map(|values| {
        values
            .iter()
            .filter_map(serde_json::Value::as_str)
            .map(str::to_owned)
            .collect()
    })
}

fn required_regions(
    value: &serde_json::Value,
    pointer: &str,
) -> Result<Vec<Region>, Box<dyn Error>> {
    let values = value
        .pointer(pointer)
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| format!("missing region array at {pointer}"))?;
    values.iter().map(parse_region).collect()
}

fn parse_region(value: &serde_json::Value) -> Result<Region, Box<dyn Error>> {
    let path = value
        .get("path")
        .and_then(serde_json::Value::as_str)
        .ok_or("region path is missing")?
        .to_owned();
    let start_line = value
        .get("start")
        .or_else(|| value.get("start_line"))
        .and_then(serde_json::Value::as_u64)
        .and_then(|line| usize::try_from(line).ok())
        .ok_or("region start line is missing or invalid")?;
    let end_line = value
        .get("end")
        .or_else(|| value.get("end_line"))
        .and_then(serde_json::Value::as_u64)
        .and_then(|line| usize::try_from(line).ok())
        .ok_or("region end line is missing or invalid")?;
    let region = Region {
        path,
        start_line,
        end_line,
    };
    validate_region(&region, PathStyle::Posix)?;
    Ok(region)
}

fn flatten_optional_regions(
    value: Option<&serde_json::Value>,
) -> Result<Vec<Region>, Box<dyn Error>> {
    let Some(map) = value.and_then(serde_json::Value::as_object) else {
        return Ok(Vec::new());
    };
    map.values()
        .filter_map(serde_json::Value::as_array)
        .flatten()
        .map(parse_region)
        .collect()
}

fn repository_from_instance_id(instance_id: &str) -> Option<String> {
    let repository = instance_id
        .rsplit_once('-')
        .map_or(instance_id, |(prefix, _)| prefix);
    repository
        .contains("__")
        .then(|| repository.replace("__", "/"))
}

fn unique_paths(regions: &[Region]) -> Vec<String> {
    regions
        .iter()
        .map(|region| region.path.clone())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn uniform_value<T: PartialEq + Clone>(
    mut values: impl Iterator<Item = T>,
    label: &str,
) -> Result<T, Box<dyn Error>> {
    let first = values
        .next()
        .ok_or_else(|| format!("no values available for {label}"))?;
    if values.any(|value| value != first) {
        return Err(format!("mixed {label} values are not comparable").into());
    }
    Ok(first)
}

fn complete_sum(mut values: impl Iterator<Item = Option<usize>>) -> Option<usize> {
    values.try_fold(0usize, |total, value| {
        value.and_then(|value| total.checked_add(value))
    })
}

fn complete_float_sum(mut values: impl Iterator<Item = Option<f64>>) -> Option<f64> {
    values.try_fold(0.0, |total, value| value.map(|value| total + value))
}

fn ratio(numerator: usize, denominator: usize) -> f64 {
    if denominator == 0 {
        0.0
    } else {
        numerator as f64 / denominator as f64
    }
}

fn f1(precision: f64, recall: f64) -> f64 {
    if precision + recall == 0.0 {
        0.0
    } else {
        2.0 * precision * recall / (precision + recall)
    }
}

fn read_optional_string_map(
    path: Option<&Path>,
) -> Result<BTreeMap<String, String>, Box<dyn Error>> {
    path.map_or_else(
        || Ok(BTreeMap::new()),
        |path| {
            serde_json::from_str(&fs::read_to_string(path)?)
                .map_err(|error| format!("invalid string map {}: {error}", path.display()).into())
        },
    )
}

fn development_dataset() -> String {
    "development".into()
}

fn repo_size_bucket(indexed_files: usize) -> &'static str {
    match indexed_files {
        0..=499 => "small",
        500..=4_999 => "medium",
        _ => "large",
    }
}

fn infer_language(files: &[InternalRelevantFile]) -> String {
    files
        .iter()
        .find_map(|file| {
            Path::new(&file.path)
                .extension()
                .and_then(|extension| extension.to_str())
                .and_then(|extension| match extension {
                    "rs" => Some("rust"),
                    "py" => Some("python"),
                    "js" | "mjs" | "cjs" => Some("javascript"),
                    "ts" | "tsx" => Some("typescript"),
                    "go" => Some("go"),
                    "rb" => Some("ruby"),
                    _ => None,
                })
        })
        .unwrap_or("unknown")
        .to_owned()
}

fn has_exact_identifier(query: &str) -> bool {
    query.split_whitespace().any(|token| {
        token.contains(['_', '.', ':', '/', '#'])
            || token
                .as_bytes()
                .windows(2)
                .any(|pair| pair[0].is_ascii_lowercase() && pair[1].is_ascii_uppercase())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_task(style: PathStyle, budget: Budget) -> RankedTask {
        RankedTask {
            schema_version: CONTRACT_SCHEMA_VERSION,
            dataset_kind: "fixture".into(),
            task_id: "task".into(),
            repository: RepositorySpec {
                url: "https://example.invalid/repo.git".into(),
                revision: "0000000000000000000000000000000000000000".into(),
                path_style: style,
            },
            query: "fix parseValue".into(),
            language: "rust".into(),
            strata: Strata {
                repo_size_bucket: "small".into(),
                exact_identifier: true,
                lexical_overlap_bucket: "high".into(),
                tags: vec!["fixture".into()],
            },
            budget,
            relevant_files: vec!["src/lib.rs".into()],
            core_regions: vec![Region {
                path: "src/lib.rs".into(),
                start_line: 2,
                end_line: 3,
            }],
            optional_regions: vec![Region {
                path: "tests/lib.rs".into(),
                start_line: 5,
                end_line: 5,
            }],
        }
    }

    fn fixture_prediction(hash: &str, budget: Budget) -> Prediction {
        Prediction {
            schema_version: CONTRACT_SCHEMA_VERSION,
            task_id: "task".into(),
            manifest_blake3: hash.into(),
            repository_revision: "0000000000000000000000000000000000000000".into(),
            budget,
            tokenizer: Some("cl100k_base".into()),
            complete_response_tokens: Some(100),
            source_tokens: Some(20),
            latency_ms: Some(1.0),
            index_generation: Some(1),
            regions: vec![
                PredictedRegion {
                    path: "src/lib.rs".into(),
                    start_line: 1,
                    end_line: 3,
                    rank: 1,
                    source: Some("text".into()),
                    facet: None,
                    score: Some(1.0),
                    token_count: Some(10),
                },
                PredictedRegion {
                    path: "src/lib.rs".into(),
                    start_line: 3,
                    end_line: 4,
                    rank: 2,
                    source: Some("text".into()),
                    facet: None,
                    score: Some(0.5),
                    token_count: Some(10),
                },
            ],
        }
    }

    #[test]
    fn overlapping_predictions_do_not_double_count_lines_or_budget() {
        let budget = Budget {
            kind: BudgetKind::Lines,
            amount: 4,
        };
        let task = fixture_task(PathStyle::Posix, budget.clone());
        let prediction = fixture_prediction("hash", budget);
        let metrics = evaluate_task(&task, &prediction).expect("metrics");
        assert_eq!(metrics.returned_lines, 4);
        assert_eq!(metrics.core_lines_hit, 2);
        assert_eq!(metrics.core_lines, 2);
        assert_eq!(metrics.line_recall, 1.0);
        assert_eq!(metrics.line_precision, 0.5);
        assert!((metrics.line_f1 - (2.0 / 3.0)).abs() < f64::EPSILON);
        assert_eq!(metrics.predicted_regions, 2);
    }

    #[test]
    fn ndcg_rewards_core_lines_before_noise() {
        let core = HashSet::from([("src/lib.rs".into(), 2), ("src/lib.rs".into(), 3)]);
        let optional = HashSet::new();
        let ideal = vec![
            (1, "src/lib.rs".into(), 2),
            (1, "src/lib.rs".into(), 3),
            (2, "src/lib.rs".into(), 9),
        ];
        let noisy = vec![
            (1, "src/lib.rs".into(), 9),
            (2, "src/lib.rs".into(), 2),
            (2, "src/lib.rs".into(), 3),
        ];
        assert_eq!(line_ndcg(&ideal, &core, &optional), 1.0);
        assert!(line_ndcg(&noisy, &core, &optional) < 1.0);
    }

    #[test]
    fn windows_paths_normalize_without_changing_posix_backslashes() {
        assert_eq!(
            normalize_path(r"src\lib.rs", PathStyle::Windows).expect("windows"),
            "src/lib.rs"
        );
        assert_eq!(
            normalize_path(r"src\lib.rs", PathStyle::Posix).expect("posix"),
            r"src\lib.rs"
        );
        assert_ne!(
            normalize_path(r"src\lib.rs", PathStyle::Posix).unwrap(),
            normalize_path("src/lib.rs", PathStyle::Posix).unwrap()
        );
    }

    #[test]
    fn malformed_ranges_and_unpinned_revisions_are_rejected() {
        assert!(
            validate_region(
                &Region {
                    path: "src/lib.rs".into(),
                    start_line: 3,
                    end_line: 2,
                },
                PathStyle::Posix
            )
            .is_err()
        );
        assert!(validate_revision("main").is_err());
    }

    #[test]
    fn prediction_rejects_manifest_revision_and_budget_mismatches() {
        let budget = Budget {
            kind: BudgetKind::Lines,
            amount: 4,
        };
        let task = fixture_task(PathStyle::Posix, budget.clone());
        let mut prediction = fixture_prediction("hash", budget.clone());
        assert!(validate_prediction(&task, &prediction, "other").is_err());
        prediction.manifest_blake3 = "hash".into();
        prediction.repository_revision = "1111111111111111111111111111111111111111".into();
        assert!(validate_prediction(&task, &prediction, "hash").is_err());
        prediction.repository_revision = task.repository.revision.clone();
        prediction.budget.amount += 1;
        assert!(validate_prediction(&task, &prediction, "hash").is_err());
    }

    #[test]
    fn source_token_budget_uses_authoritative_emitted_total() {
        let budget = Budget {
            kind: BudgetKind::SourceTokens,
            amount: 20,
        };
        let task = fixture_task(PathStyle::Posix, budget.clone());
        let mut prediction = fixture_prediction("hash", budget);
        prediction.regions[1].start_line = 2;
        prediction.regions[1].end_line = 3;
        prediction.source_tokens = Some(20);
        validate_prediction(&task, &prediction, "hash").expect("within source budget");
        prediction.source_tokens = Some(21);
        assert!(validate_prediction(&task, &prediction, "hash").is_err());
        prediction.source_tokens = None;
        assert!(validate_prediction(&task, &prediction, "hash").is_err());
    }

    #[test]
    fn comparison_rejects_manifest_revision_and_tokenizer_mismatches() {
        let budget = Budget {
            kind: BudgetKind::Lines,
            amount: 4,
        };
        let report = evaluate(
            vec![fixture_task(PathStyle::Posix, budget.clone())],
            vec![fixture_prediction("a", budget)],
            "a".into(),
            "p".into(),
        )
        .expect("report");
        let mut candidate = report.clone();
        candidate.manifest_blake3 = "b".into();
        assert!(compare_reports(&report, &candidate).is_err());
        candidate.manifest_blake3 = "a".into();
        candidate.tasks[0].repository_revision = "1111111111111111111111111111111111111111".into();
        assert!(compare_reports(&report, &candidate).is_err());
        candidate.tasks[0].repository_revision = report.tasks[0].repository_revision.clone();
        candidate.tokenizer = Some("o200k_base".into());
        assert!(compare_reports(&report, &candidate).is_err());
    }

    #[test]
    fn swe_explore_conversion_uses_official_companion_maps() {
        let record = serde_json::json!({
            "instance_id": "owner__repo-42",
            "ground_truth": {
                "read_core_files": ["src/lib.rs"],
                "read_core_regions": [{"path": "src/lib.rs", "start": 2, "end": 4}],
                "read_optional_regions_map": {
                    "model": [{"path": "tests/lib.rs", "start": 7, "end": 8}]
                }
            },
            "meta": {}
        });
        let issue_map =
            BTreeMap::from([("owner__repo-42".into(), "Fix parseValue handling".into())]);
        let commit_map = BTreeMap::from([(
            "owner__repo-42".into(),
            "0123456789abcdef0123456789abcdef01234567".into(),
        )]);
        let task =
            convert_swe_record(&record, 200, &issue_map, &commit_map).expect("converted task");
        assert_eq!(task.repository.url, "https://github.com/owner/repo.git");
        assert_eq!(task.query, "Fix parseValue handling");
        assert_eq!(task.core_regions.len(), 1);
        assert_eq!(task.optional_regions.len(), 1);
    }

    #[test]
    fn zero_baseline_has_no_relative_delta() {
        let comparison = metric_comparison(0.0, 1.0);
        assert_eq!(comparison.absolute_delta, 1.0);
        assert_eq!(comparison.relative_delta, None);
    }

    #[test]
    fn checked_in_ranked_region_fixture_is_deterministic() {
        let report: EvaluationReport = serde_json::from_str(include_str!(
            "../benchmarks/fixtures/ranked_regions/swe_explore.report.json"
        ))
        .expect("checked-in report");
        assert_eq!(report.aggregate.task_count, 1);
        assert_eq!(report.aggregate.core_lines_hit, 3);
        assert_eq!(report.aggregate.returned_lines, 7);
        assert_eq!(report.aggregate.complete_response_tokens, Some(120));
        assert_eq!(report.aggregate.source_tokens, Some(35));
        assert_eq!(report.aggregate.line_f1_macro, 0.5);
    }
}
