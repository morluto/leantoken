use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::error::Error;
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use clap::Parser;
use leantoken::{
    Config, ContextCandidateEvaluation, ContextFragment, ContextRequest, ContextResponse,
    WorkflowEvidence, services::Services, tokens,
};
use serde::{Deserialize, Serialize};

#[derive(Debug, Parser)]
#[command(about = "Run a pinned LeanToken context-retrieval benchmark")]
struct Args {
    #[arg(long, default_value = "benchmarks/representative.json")]
    manifest: PathBuf,
    #[arg(long, default_value = "target/representative-repos")]
    repos_root: PathBuf,
    #[arg(long, default_value = "target/representative_benchmark_report.json")]
    output: PathBuf,
    /// Optional task-concept labels bound to this manifest and its line anchors.
    #[arg(long)]
    concept_labels: Option<PathBuf>,
    /// Exit nonzero after writing the report when frozen concept thresholds fail.
    #[arg(long, requires = "concept_labels")]
    require_concept_thresholds: bool,
    /// Validate the manifest, candidate runtime tree, and pinned checkouts without evaluating.
    #[arg(long)]
    preflight_only: bool,
    /// Re-run a consumed blind holdout for diagnostics without claiming blind evidence.
    #[arg(long)]
    consumed_diagnostic: bool,
    /// Derive typed workflow evidence from each JSON task prompt.
    #[arg(long)]
    workflow_evidence: bool,
}

#[derive(Debug, Deserialize)]
struct Manifest {
    schema_version: u32,
    #[serde(default = "default_dataset_kind")]
    dataset_kind: String,
    #[serde(default)]
    frozen_at: Option<String>,
    #[serde(default)]
    candidate_revision: Option<String>,
    #[serde(default)]
    evaluation_protocol: Option<String>,
    #[serde(default)]
    reclassification_rule: Option<String>,
    description: String,
    #[serde(default = "default_rg_max_lines")]
    rg_max_lines_per_query: usize,
    corpora: Vec<CorpusSpec>,
}

#[derive(Debug, Deserialize)]
struct CorpusSpec {
    name: String,
    url: String,
    directory: String,
    base_revision: String,
    #[serde(default)]
    fix_commit: Option<String>,
    #[serde(default)]
    issue_url: Option<String>,
    #[serde(default)]
    prompt_provenance: Option<String>,
    #[serde(default)]
    label_provenance: Option<String>,
    #[serde(default)]
    dataset_url: Option<String>,
    #[serde(default)]
    dataset_revision: Option<String>,
    #[serde(default)]
    dataset_license: Option<String>,
    #[serde(default)]
    external_limitations: Vec<String>,
    tasks: Vec<TaskSpec>,
}

#[derive(Debug, Deserialize)]
struct TaskSpec {
    id: String,
    prompt: String,
    #[serde(default)]
    languages: Vec<String>,
    #[serde(default)]
    task_shapes: Vec<String>,
    rg_queries: Vec<String>,
    relevant_files: Vec<RelevantFile>,
    token_budget: usize,
}

#[derive(Debug, Deserialize)]
struct RelevantFile {
    path: String,
    #[serde(default)]
    line_anchors: Vec<usize>,
}

#[derive(Debug, Deserialize)]
struct ConceptLabelManifest {
    schema_version: u32,
    source_manifest: String,
    source_manifest_blake3: String,
    dataset_kind: String,
    frozen_at: String,
    methodology: String,
    thresholds: ConceptThresholds,
    tasks: Vec<ConceptTaskLabels>,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize)]
struct ConceptThresholds {
    minimum_candidate_concept_recall: f64,
    minimum_selected_concept_recall: f64,
    minimum_selection_retention: f64,
    minimum_task_selected_concept_recall: f64,
}

#[derive(Debug, Deserialize)]
struct ConceptTaskLabels {
    id: String,
    concepts: Vec<ConceptLabel>,
}

#[derive(Debug, Deserialize)]
struct ConceptLabel {
    id: String,
    description: String,
    evidence: Vec<ConceptEvidence>,
}

#[derive(Debug, Deserialize)]
struct ConceptEvidence {
    path: String,
    line_anchors: Vec<usize>,
}

#[derive(Debug)]
struct LoadedConceptLabels {
    schema_version: u32,
    blake3: String,
    source_manifest: String,
    source_manifest_blake3: String,
    dataset_kind: String,
    frozen_at: String,
    methodology: String,
    thresholds: ConceptThresholds,
    tasks: BTreeMap<String, ConceptTaskLabels>,
}

#[derive(Debug, Serialize)]
struct Report {
    schema_version: u32,
    dataset_kind: String,
    manifest_blake3: String,
    frozen_at: Option<String>,
    candidate_revision: Option<String>,
    evaluation_protocol: Option<String>,
    reclassification_rule: Option<String>,
    manifest_description: String,
    leantoken_version: &'static str,
    harness_revision: String,
    harness_worktree_dirty: bool,
    candidate_runtime_tree_verified: Option<bool>,
    diagnostic_only: bool,
    workflow_evidence_enabled: bool,
    host_os: &'static str,
    host_arch: &'static str,
    rustc_version: String,
    ripgrep_version: String,
    generated_at_unix_seconds: u64,
    tokenizer: &'static str,
    token_count_exact: bool,
    methodology: Methodology,
    #[serde(skip_serializing_if = "Option::is_none")]
    concept_coverage: Option<ConceptCoverageDecision>,
    aggregate: AggregateReport,
    corpora: Vec<CorpusReport>,
    limitations: Vec<&'static str>,
}

#[derive(Debug, Serialize)]
struct Methodology {
    oracle_baseline: &'static str,
    rg_discovery_baseline: &'static str,
    scripted_baseline: &'static str,
    source_tokens: &'static str,
    serialized_tokens: &'static str,
}

#[derive(Debug, Default, Serialize)]
struct AggregateReport {
    corpus_count: usize,
    task_count: usize,
    relevant_files: usize,
    relevant_files_found: usize,
    relevant_file_recall: f64,
    candidate_relevant_files_found: usize,
    candidate_relevant_file_recall: f64,
    returned_files: usize,
    labeled_file_precision: f64,
    line_anchors: usize,
    line_anchors_found: usize,
    line_anchor_recall: Option<f64>,
    oracle_source_tokens: usize,
    rg_discovery_tokens: usize,
    scripted_baseline_total_json_tokens: usize,
    leantoken_source_tokens: usize,
    leantoken_total_json_tokens: usize,
    source_savings_against_oracle_fraction: f64,
    total_json_savings_against_scripted_fraction: f64,
    known_fragments_resent: usize,
    dead_end_fragments: usize,
    dead_end_source_tokens: usize,
    second_response_source_tokens: usize,
    estimated_repeated_range_source_tokens: usize,
    repeat_request_json_tokens: usize,
    repeat_total_json_tokens: usize,
    two_turn_context_json_tokens: usize,
    concepts: usize,
    candidate_concepts_found: usize,
    candidate_concept_recall: Option<f64>,
    selected_concepts_found: usize,
    selected_concept_recall: Option<f64>,
    concept_selection_retention: Option<f64>,
}

#[derive(Debug, Serialize)]
struct CorpusReport {
    name: String,
    url: String,
    base_revision: String,
    fix_commit: Option<String>,
    issue_url: Option<String>,
    prompt_provenance: Option<String>,
    label_provenance: Option<String>,
    dataset_url: Option<String>,
    dataset_revision: Option<String>,
    dataset_license: Option<String>,
    external_limitations: Vec<String>,
    indexed_files: usize,
    indexed_chunks: usize,
    index_warnings: Vec<String>,
    cold_index_ms: f64,
    database_bytes: u64,
    tasks: Vec<TaskReport>,
}

#[derive(Debug, Serialize)]
struct TaskReport {
    id: String,
    prompt: String,
    languages: Vec<String>,
    task_shapes: Vec<String>,
    token_budget: usize,
    workflow_evidence: WorkflowEvidenceCounts,
    relevant_files: Vec<String>,
    returned_files: Vec<String>,
    returned_evidence: Vec<EvidenceSummary>,
    candidate_files: Vec<String>,
    relevant_candidate_evidence: Vec<CandidateEvidenceSummary>,
    omitted_relevant_files: Vec<OmittedRelevantFile>,
    relevant_files_found: usize,
    relevant_file_recall: f64,
    candidate_relevant_files_found: usize,
    candidate_relevant_file_recall: f64,
    labeled_file_precision: f64,
    line_anchors: usize,
    line_anchors_found: usize,
    line_anchor_recall: Option<f64>,
    unlabeled_returned_files: Vec<String>,
    oracle_source_tokens: usize,
    oracle_minimal_read_json_tokens: usize,
    rg_discovery_tokens: usize,
    rg_discovery_json_tokens: usize,
    scripted_baseline_total_json_tokens: usize,
    leantoken_source_tokens: usize,
    leantoken_total_json_tokens: usize,
    source_savings_against_oracle_fraction: f64,
    total_json_savings_against_scripted_fraction: f64,
    first_context_ms: f64,
    warm_context_ms_samples: Vec<f64>,
    warm_context_median_ms: f64,
    warm_context_p95_ms: f64,
    second_response_source_tokens: usize,
    estimated_repeated_range_source_tokens: usize,
    repeat_request_json_tokens: usize,
    repeat_total_json_tokens: usize,
    two_turn_context_json_tokens: usize,
    known_fragments_resent: usize,
    known_hash_omission_visible: bool,
    dead_end_fragments: usize,
    dead_end_source_tokens: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    concept_coverage: Option<TaskConceptCoverage>,
}

#[derive(Debug, Default, Serialize)]
struct WorkflowEvidenceCounts {
    failure_traces: usize,
    symbols: usize,
    paths: usize,
    test_intents: usize,
    total_bytes: usize,
}

#[derive(Debug, Serialize)]
struct ConceptCoverageDecision {
    labels_schema_version: u32,
    labels_blake3: String,
    source_manifest: String,
    source_manifest_blake3: String,
    dataset_kind: String,
    frozen_at: String,
    methodology: String,
    thresholds: ConceptThresholds,
    passed: bool,
    failures: Vec<String>,
}

#[derive(Debug, Serialize)]
struct TaskConceptCoverage {
    concepts: usize,
    candidate_concepts_found: usize,
    candidate_concept_recall: f64,
    selected_concepts_found: usize,
    selected_concept_recall: f64,
    selection_retention: Option<f64>,
    evidence: Vec<ConceptEvidenceCoverage>,
}

#[derive(Debug, Serialize)]
struct ConceptEvidenceCoverage {
    id: String,
    description: String,
    candidate_covered: bool,
    selected_covered: bool,
    candidate_anchors_found: Vec<MatchedAnchor>,
    selected_anchors_found: Vec<MatchedAnchor>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
struct MatchedAnchor {
    path: String,
    line: usize,
}

#[derive(Debug, Serialize)]
struct EvidenceSummary {
    path: String,
    start_line: usize,
    end_line: usize,
    representation: String,
    reason: String,
    score: f64,
    token_count: usize,
    content_hash: String,
}

#[derive(Debug, Serialize)]
struct OmittedRelevantFile {
    path: String,
    reason: &'static str,
}

#[derive(Debug, Serialize)]
struct CandidateEvidenceSummary {
    path: String,
    start_line: usize,
    end_line: usize,
    representation: String,
    match_kinds: Vec<String>,
    concepts: Vec<String>,
    concept_weight: f64,
    score: f64,
    token_count: usize,
}

#[derive(Debug, Serialize)]
struct BaselineRead<'a> {
    path: &'a str,
    content: String,
}

#[derive(Debug, Serialize)]
struct RgResult<'a> {
    query: &'a str,
    json_lines: String,
    truncated: bool,
}

#[derive(Debug, Serialize)]
struct ScriptedBaseline<'a> {
    searches: &'a [RgResult<'a>],
    reads: &'a [BaselineRead<'a>],
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let args = Args::parse();
    let manifest_json = fs::read_to_string(&args.manifest)?;
    let manifest_blake3 = blake3::hash(manifest_json.as_bytes()).to_hex().to_string();
    let manifest: Manifest = serde_json::from_str(&manifest_json)?;
    if !matches!(manifest.schema_version, 1..=4) {
        return Err(format!(
            "unsupported benchmark manifest schema version {}",
            manifest.schema_version
        )
        .into());
    }
    validate_manifest(&manifest)?;
    let mut concept_labels = args
        .concept_labels
        .as_deref()
        .map(|path| load_concept_labels(path, &args.manifest, &manifest, &manifest_blake3))
        .transpose()?;
    if args.consumed_diagnostic && manifest.dataset_kind != "blind_holdout" {
        return Err("--consumed-diagnostic requires a blind_holdout manifest".into());
    }
    let source_root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let harness_revision = git_output(source_root, &["rev-parse", "HEAD"])?
        .trim()
        .to_owned();
    let harness_worktree_dirty = !git_output(
        source_root,
        &["status", "--porcelain=v1", "--untracked-files=all"],
    )?
    .trim()
    .is_empty();
    let candidate_runtime_tree_verified = if args.consumed_diagnostic {
        None
    } else {
        verify_candidate_runtime_tree(&manifest, source_root)?
    };
    let ripgrep_version = command_version("rg")?;
    preflight(&manifest, &args.repos_root)?;
    if args.preflight_only {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "manifest_blake3": manifest_blake3,
                "dataset_kind": manifest.dataset_kind,
                "candidate_revision": manifest.candidate_revision,
                "harness_revision": harness_revision,
                "harness_worktree_dirty": harness_worktree_dirty,
                "candidate_runtime_tree_verified": candidate_runtime_tree_verified,
                "diagnostic_only": args.consumed_diagnostic,
                "concept_labels_blake3": concept_labels.as_ref().map(|labels| &labels.blake3),
                "concept_count": concept_labels.as_ref().map(|labels| {
                    labels.tasks.values().map(|task| task.concepts.len()).sum::<usize>()
                }),
                "corpus_count": manifest.corpora.len(),
                "task_count": manifest.corpora.iter().map(|corpus| corpus.tasks.len()).sum::<usize>(),
                "status": "ready"
            }))?
        );
        return Ok(());
    }

    let scratch = tempfile::tempdir()?;
    let mut corpora = Vec::new();
    let mut aggregate = AggregateReport::default();
    for corpus in manifest.corpora {
        let root = args.repos_root.join(&corpus.directory);
        verify_revision(&root, &corpus.base_revision)?;
        let database_path = scratch.path().join(format!("{}.sqlite", corpus.name));
        let config = Config::discover(&root, Some(database_path.clone()))?;
        let services = Services::open(config)?;

        let started = Instant::now();
        let indexed = services.index(true).await?;
        let cold_index_ms = elapsed_ms(started);
        let status = services.status().await?;
        let mut tasks = Vec::new();
        for task in corpus.tasks {
            let labels = concept_labels
                .as_mut()
                .and_then(|loaded| loaded.tasks.remove(&task.id));
            let report = run_task(
                &root,
                &services,
                task,
                manifest.rg_max_lines_per_query,
                labels.as_ref(),
                args.workflow_evidence,
            )
            .await?;
            accumulate(&mut aggregate, &report);
            tasks.push(report);
        }
        corpora.push(CorpusReport {
            name: corpus.name,
            url: corpus.url,
            base_revision: corpus.base_revision,
            fix_commit: corpus.fix_commit,
            issue_url: corpus.issue_url,
            prompt_provenance: corpus.prompt_provenance,
            label_provenance: corpus.label_provenance,
            dataset_url: corpus.dataset_url,
            dataset_revision: corpus.dataset_revision,
            dataset_license: corpus.dataset_license,
            external_limitations: corpus.external_limitations,
            indexed_files: status.file_count,
            indexed_chunks: status.chunk_count,
            index_warnings: indexed.warnings,
            cold_index_ms,
            database_bytes: database_footprint(&database_path)?,
            tasks,
        });
    }
    aggregate.corpus_count = corpora.len();
    aggregate.relevant_file_recall =
        ratio(aggregate.relevant_files_found, aggregate.relevant_files);
    aggregate.candidate_relevant_file_recall = ratio(
        aggregate.candidate_relevant_files_found,
        aggregate.relevant_files,
    );
    aggregate.labeled_file_precision =
        ratio(aggregate.relevant_files_found, aggregate.returned_files);
    aggregate.line_anchor_recall =
        optional_ratio(aggregate.line_anchors_found, aggregate.line_anchors);
    aggregate.candidate_concept_recall =
        optional_ratio(aggregate.candidate_concepts_found, aggregate.concepts);
    aggregate.selected_concept_recall =
        optional_ratio(aggregate.selected_concepts_found, aggregate.concepts);
    aggregate.concept_selection_retention = optional_ratio(
        aggregate.selected_concepts_found,
        aggregate.candidate_concepts_found,
    );
    aggregate.source_savings_against_oracle_fraction = savings(
        aggregate.oracle_source_tokens,
        aggregate.leantoken_source_tokens,
    );
    aggregate.total_json_savings_against_scripted_fraction = savings(
        aggregate.scripted_baseline_total_json_tokens,
        aggregate.leantoken_total_json_tokens,
    );
    if let Some(labels) = &concept_labels
        && !labels.tasks.is_empty()
    {
        return Err(format!(
            "concept labels contain tasks absent from the source manifest: {}",
            labels.tasks.keys().cloned().collect::<Vec<_>>().join(", ")
        )
        .into());
    }
    let concept_coverage = concept_labels
        .as_ref()
        .map(|labels| concept_coverage_decision(labels, &aggregate, &corpora));
    let concept_thresholds_passed = concept_coverage
        .as_ref()
        .is_none_or(|coverage| coverage.passed);

    let report = Report {
        schema_version: manifest.schema_version,
        dataset_kind: manifest.dataset_kind.clone(),
        manifest_blake3,
        frozen_at: manifest.frozen_at,
        candidate_revision: manifest.candidate_revision,
        evaluation_protocol: manifest.evaluation_protocol,
        reclassification_rule: manifest.reclassification_rule,
        manifest_description: manifest.description,
        leantoken_version: env!("CARGO_PKG_VERSION"),
        harness_revision,
        harness_worktree_dirty,
        candidate_runtime_tree_verified,
        diagnostic_only: args.consumed_diagnostic,
        workflow_evidence_enabled: args.workflow_evidence,
        host_os: std::env::consts::OS,
        host_arch: std::env::consts::ARCH,
        rustc_version: command_version("rustc")?,
        ripgrep_version,
        generated_at_unix_seconds: SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs(),
        tokenizer: tokens::Tokenizer::default().name(),
        token_count_exact: tokens::Tokenizer::default().is_exact(),
        methodology: Methodology {
            oracle_baseline: "Full contents of fix-labeled relevant files, as if an agent chose every file perfectly and paid no discovery cost.",
            rg_discovery_baseline: "Bounded, path-sorted ripgrep --json output for fixed-string queries derived from each public bug task.",
            scripted_baseline: "One JSON envelope containing the ripgrep discovery output and oracle full-file reads.",
            source_tokens: "Tokens in source content only; excludes paths, scores, reasons, receipts, and JSON syntax.",
            serialized_tokens: "Tokens in the complete serialized JSON payload, including metadata and syntax.",
        },
        concept_coverage,
        aggregate,
        corpora,
        limitations: benchmark_limitations(
            &manifest.dataset_kind,
            args.consumed_diagnostic,
            args.concept_labels.is_some(),
        ),
    };
    let json = serde_json::to_string_pretty(&report)?;
    if let Some(parent) = args
        .output
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
    {
        fs::create_dir_all(parent)?;
    }
    fs::write(&args.output, &json)?;
    println!("{json}");
    if args.require_concept_thresholds && !concept_thresholds_passed {
        return Err("frozen context concept-coverage thresholds failed".into());
    }
    Ok(())
}

fn default_dataset_kind() -> String {
    "development".to_owned()
}

fn validate_manifest(manifest: &Manifest) -> Result<(), Box<dyn Error>> {
    if manifest.dataset_kind == "external_retrieval_corpus" {
        if manifest.schema_version < 4 {
            return Err("external retrieval corpora require manifest schema v4".into());
        }
        if manifest.frozen_at.as_deref().is_none_or(str::is_empty) {
            return Err("external retrieval corpora require frozen_at".into());
        }
        for (field, value) in [
            (
                "evaluation_protocol",
                manifest.evaluation_protocol.as_deref(),
            ),
            (
                "reclassification_rule",
                manifest.reclassification_rule.as_deref(),
            ),
        ] {
            if value.is_none_or(str::is_empty) {
                return Err(format!("external retrieval corpora require {field}").into());
            }
        }
        for corpus in &manifest.corpora {
            if corpus.fix_commit.is_some() {
                return Err(
                    format!("external corpus {} must not name a future fix", corpus.name).into(),
                );
            }
            for (field, value) in [
                ("dataset_url", corpus.dataset_url.as_deref()),
                ("dataset_revision", corpus.dataset_revision.as_deref()),
                ("dataset_license", corpus.dataset_license.as_deref()),
                ("prompt_provenance", corpus.prompt_provenance.as_deref()),
                ("label_provenance", corpus.label_provenance.as_deref()),
            ] {
                if value.is_none_or(str::is_empty) {
                    return Err(format!("external corpus {} requires {field}", corpus.name).into());
                }
            }
            let revision = corpus
                .dataset_revision
                .as_deref()
                .expect("validated dataset revision");
            if revision.len() != 40 || !revision.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                return Err(format!(
                    "external corpus {} dataset revision is not a full Git object ID",
                    corpus.name
                )
                .into());
            }
            if corpus.tasks.is_empty() {
                return Err(format!("external corpus {} has no tasks", corpus.name).into());
            }
            if corpus.external_limitations.is_empty()
                || corpus
                    .external_limitations
                    .iter()
                    .any(|limitation| limitation.trim().is_empty())
            {
                return Err(format!(
                    "external corpus {} requires explicit limitations",
                    corpus.name
                )
                .into());
            }
            for task in &corpus.tasks {
                if task.languages.is_empty()
                    || task
                        .languages
                        .iter()
                        .any(|language| language.trim().is_empty())
                    || task.task_shapes.is_empty()
                    || task.task_shapes.iter().any(|shape| shape.trim().is_empty())
                {
                    return Err(format!(
                        "external task {} requires language and task-shape strata",
                        task.id
                    )
                    .into());
                }
            }
        }
    }
    if is_patch_free_dataset(&manifest.dataset_kind) {
        if manifest.frozen_at.as_deref().is_none_or(str::is_empty) {
            return Err(format!("{} set requires frozen_at", manifest.dataset_kind).into());
        }
        for corpus in &manifest.corpora {
            if corpus.fix_commit.is_some() {
                return Err(format!(
                    "{} corpus {} must not name a future fix",
                    manifest.dataset_kind, corpus.name
                )
                .into());
            }
            for (field, value) in [
                ("issue_url", corpus.issue_url.as_deref()),
                ("prompt_provenance", corpus.prompt_provenance.as_deref()),
                ("label_provenance", corpus.label_provenance.as_deref()),
            ] {
                if value.is_none_or(str::is_empty) {
                    return Err(format!(
                        "{} corpus {} requires {field}",
                        manifest.dataset_kind, corpus.name
                    )
                    .into());
                }
            }
        }
    }
    if manifest.schema_version >= 3 && manifest.dataset_kind == "blind_holdout" {
        for (field, value) in [
            ("candidate_revision", manifest.candidate_revision.as_deref()),
            (
                "evaluation_protocol",
                manifest.evaluation_protocol.as_deref(),
            ),
            (
                "reclassification_rule",
                manifest.reclassification_rule.as_deref(),
            ),
        ] {
            if value.is_none_or(str::is_empty) {
                return Err(format!("blind_holdout schema v3 requires {field}").into());
            }
        }

        let tasks = manifest
            .corpora
            .iter()
            .flat_map(|corpus| &corpus.tasks)
            .collect::<Vec<_>>();
        if tasks.len() < 8 {
            return Err("blind_holdout schema v3 requires at least eight tasks".into());
        }
        let mut languages = HashSet::new();
        let mut task_shapes = HashSet::new();
        let allowed_shapes = HashSet::from([
            "configuration",
            "cross_file_behavior",
            "definition_discovery",
            "framework_behavior",
            "regression_test_discovery",
        ]);
        for task in tasks {
            if task.languages.is_empty() || task.task_shapes.is_empty() {
                return Err(format!("{} requires language and task-shape tags", task.id).into());
            }
            for language in &task.languages {
                if language.trim().is_empty() {
                    return Err(format!("{} has an empty language tag", task.id).into());
                }
                languages.insert(language.as_str());
            }
            for shape in &task.task_shapes {
                if !allowed_shapes.contains(shape.as_str()) {
                    return Err(format!("{} has unsupported task shape {shape}", task.id).into());
                }
                task_shapes.insert(shape.as_str());
            }
        }
        if languages.len() < 6 {
            return Err("blind_holdout schema v3 requires at least six languages".into());
        }
        if task_shapes.len() < 4 {
            return Err("blind_holdout schema v3 requires at least four task shapes".into());
        }
    }
    Ok(())
}

fn load_concept_labels(
    path: &Path,
    source_manifest_path: &Path,
    manifest: &Manifest,
    manifest_blake3: &str,
) -> Result<LoadedConceptLabels, Box<dyn Error>> {
    let json = fs::read_to_string(path)?;
    let blake3 = blake3::hash(json.as_bytes()).to_hex().to_string();
    let labels: ConceptLabelManifest = serde_json::from_str(&json)?;
    if labels.schema_version != 1 {
        return Err(format!(
            "unsupported concept-label schema version {}",
            labels.schema_version
        )
        .into());
    }
    if labels.source_manifest.trim().is_empty()
        || labels
            .source_manifest
            .rsplit('/')
            .next()
            .is_none_or(|name| {
                Some(name) != source_manifest_path.file_name().and_then(|v| v.to_str())
            })
    {
        return Err("concept labels name a different source manifest".into());
    }
    if labels.source_manifest_blake3 != manifest_blake3 {
        return Err("concept labels do not match the source manifest BLAKE3".into());
    }
    if labels.dataset_kind != manifest.dataset_kind {
        return Err("concept labels do not match the source dataset kind".into());
    }
    if labels.frozen_at.trim().is_empty() || labels.methodology.trim().is_empty() {
        return Err("concept labels require frozen_at and methodology".into());
    }
    for (name, value) in [
        (
            "minimum_candidate_concept_recall",
            labels.thresholds.minimum_candidate_concept_recall,
        ),
        (
            "minimum_selected_concept_recall",
            labels.thresholds.minimum_selected_concept_recall,
        ),
        (
            "minimum_selection_retention",
            labels.thresholds.minimum_selection_retention,
        ),
        (
            "minimum_task_selected_concept_recall",
            labels.thresholds.minimum_task_selected_concept_recall,
        ),
    ] {
        if !value.is_finite() || !(0.0..=1.0).contains(&value) {
            return Err(format!("concept threshold {name} must be between zero and one").into());
        }
    }

    let mut source_tasks = BTreeMap::new();
    for task in manifest.corpora.iter().flat_map(|corpus| &corpus.tasks) {
        if source_tasks.insert(task.id.as_str(), task).is_some() {
            return Err(format!("source manifest repeats task id {}", task.id).into());
        }
    }
    let mut tasks = BTreeMap::new();
    for task_labels in labels.tasks {
        let Some(source_task) = source_tasks.get(task_labels.id.as_str()) else {
            return Err(format!("concept labels contain unknown task {}", task_labels.id).into());
        };
        if task_labels.concepts.is_empty() {
            return Err(format!("concept task {} has no concepts", task_labels.id).into());
        }

        let source_anchors = source_task
            .relevant_files
            .iter()
            .flat_map(|file| {
                file.line_anchors
                    .iter()
                    .map(|line| (file.path.clone(), *line))
            })
            .collect::<BTreeSet<_>>();
        if source_anchors.is_empty() {
            return Err(format!(
                "concept task {} requires source line-anchor labels",
                task_labels.id
            )
            .into());
        }
        let mut concept_ids = BTreeSet::new();
        let mut labeled_anchors = BTreeSet::new();
        for concept in &task_labels.concepts {
            if concept.id.trim().is_empty()
                || concept.description.trim().is_empty()
                || concept.evidence.is_empty()
            {
                return Err(format!(
                    "concepts for task {} require an id, description, and evidence",
                    task_labels.id
                )
                .into());
            }
            if !concept_ids.insert(concept.id.as_str()) {
                return Err(
                    format!("task {} repeats concept id {}", task_labels.id, concept.id).into(),
                );
            }
            for evidence in &concept.evidence {
                validate_benchmark_path(&evidence.path)?;
                if evidence.line_anchors.is_empty() {
                    return Err(format!(
                        "concept {} in task {} has no line anchors",
                        concept.id, task_labels.id
                    )
                    .into());
                }
                for &line in &evidence.line_anchors {
                    let anchor = (evidence.path.clone(), line);
                    if line == 0 || !source_anchors.contains(&anchor) {
                        return Err(format!(
                            "concept {} in task {} uses an anchor absent from the source manifest",
                            concept.id, task_labels.id
                        )
                        .into());
                    }
                    if !labeled_anchors.insert(anchor) {
                        return Err(format!(
                            "task {} assigns one source anchor to multiple concepts",
                            task_labels.id
                        )
                        .into());
                    }
                }
            }
        }
        if labeled_anchors != source_anchors {
            return Err(format!(
                "task {} concept labels must partition every source-manifest anchor exactly once",
                task_labels.id
            )
            .into());
        }
        let task_id = task_labels.id.clone();
        if tasks.insert(task_id.clone(), task_labels).is_some() {
            return Err(format!("concept labels repeat task {task_id}").into());
        }
    }
    if tasks.len() != source_tasks.len()
        || source_tasks
            .keys()
            .any(|task_id| !tasks.contains_key(*task_id))
    {
        return Err("concept labels must cover every source-manifest task exactly once".into());
    }

    Ok(LoadedConceptLabels {
        schema_version: labels.schema_version,
        blake3,
        source_manifest: labels.source_manifest,
        source_manifest_blake3: labels.source_manifest_blake3,
        dataset_kind: labels.dataset_kind,
        frozen_at: labels.frozen_at,
        methodology: labels.methodology,
        thresholds: labels.thresholds,
        tasks,
    })
}

fn evaluate_concept_coverage(
    labels: &ConceptTaskLabels,
    candidates: &[ContextCandidateEvaluation],
    selected: &[ContextFragment],
) -> Result<TaskConceptCoverage, Box<dyn Error>> {
    let mut evidence = Vec::with_capacity(labels.concepts.len());
    for concept in &labels.concepts {
        let anchors = concept
            .evidence
            .iter()
            .flat_map(|item| {
                item.line_anchors.iter().map(|line| MatchedAnchor {
                    path: item.path.clone(),
                    line: *line,
                })
            })
            .collect::<BTreeSet<_>>();
        let candidate_anchors_found = anchors
            .iter()
            .filter(|anchor| {
                candidates.iter().any(|candidate| {
                    candidate.path == anchor.path
                        && candidate.start_line <= anchor.line
                        && candidate.end_line >= anchor.line
                })
            })
            .cloned()
            .collect::<Vec<_>>();
        let selected_anchors_found = anchors
            .iter()
            .filter(|anchor| {
                selected.iter().any(|fragment| {
                    fragment.path == anchor.path
                        && fragment.start_line <= anchor.line
                        && fragment.end_line >= anchor.line
                })
            })
            .cloned()
            .collect::<Vec<_>>();
        let candidate_covered = !candidate_anchors_found.is_empty();
        let selected_covered = !selected_anchors_found.is_empty();
        if selected_covered && !candidate_covered {
            return Err(format!(
                "selected concept {} for task {} was absent from candidate diagnostics",
                concept.id, labels.id
            )
            .into());
        }
        evidence.push(ConceptEvidenceCoverage {
            id: concept.id.clone(),
            description: concept.description.clone(),
            candidate_covered,
            selected_covered,
            candidate_anchors_found,
            selected_anchors_found,
        });
    }
    evidence.sort_by(|left, right| left.id.cmp(&right.id));
    let candidate_concepts_found = evidence
        .iter()
        .filter(|concept| concept.candidate_covered)
        .count();
    let selected_concepts_found = evidence
        .iter()
        .filter(|concept| concept.selected_covered)
        .count();
    let concepts = evidence.len();
    Ok(TaskConceptCoverage {
        concepts,
        candidate_concepts_found,
        candidate_concept_recall: ratio(candidate_concepts_found, concepts),
        selected_concepts_found,
        selected_concept_recall: ratio(selected_concepts_found, concepts),
        selection_retention: optional_ratio(selected_concepts_found, candidate_concepts_found),
        evidence,
    })
}

fn concept_coverage_decision(
    labels: &LoadedConceptLabels,
    aggregate: &AggregateReport,
    corpora: &[CorpusReport],
) -> ConceptCoverageDecision {
    let mut failures = Vec::new();
    let candidate_recall = aggregate.candidate_concept_recall.unwrap_or(0.0);
    let selected_recall = aggregate.selected_concept_recall.unwrap_or(0.0);
    let selection_retention = aggregate.concept_selection_retention.unwrap_or(0.0);
    for (name, actual, minimum) in [
        (
            "candidate concept recall",
            candidate_recall,
            labels.thresholds.minimum_candidate_concept_recall,
        ),
        (
            "selected concept recall",
            selected_recall,
            labels.thresholds.minimum_selected_concept_recall,
        ),
        (
            "concept selection retention",
            selection_retention,
            labels.thresholds.minimum_selection_retention,
        ),
    ] {
        if actual < minimum {
            failures.push(format!("{name} {actual:.4} is below minimum {minimum:.4}"));
        }
    }
    for task in corpora.iter().flat_map(|corpus| &corpus.tasks) {
        let Some(coverage) = &task.concept_coverage else {
            failures.push(format!("task {} has no concept coverage", task.id));
            continue;
        };
        if coverage.selected_concept_recall < labels.thresholds.minimum_task_selected_concept_recall
        {
            failures.push(format!(
                "task {} selected concept recall {:.4} is below minimum {:.4}",
                task.id,
                coverage.selected_concept_recall,
                labels.thresholds.minimum_task_selected_concept_recall
            ));
        }
    }
    ConceptCoverageDecision {
        labels_schema_version: labels.schema_version,
        labels_blake3: labels.blake3.clone(),
        source_manifest: labels.source_manifest.clone(),
        source_manifest_blake3: labels.source_manifest_blake3.clone(),
        dataset_kind: labels.dataset_kind.clone(),
        frozen_at: labels.frozen_at.clone(),
        methodology: labels.methodology.clone(),
        thresholds: labels.thresholds,
        passed: failures.is_empty(),
        failures,
    }
}

fn benchmark_limitations(
    dataset_kind: &str,
    consumed_diagnostic: bool,
    concept_labels: bool,
) -> Vec<&'static str> {
    let mut limitations = vec![
        "The oracle baseline assumes perfect file selection and reads whole files rather than exact decisive ranges.",
        "The scripted ripgrep baseline uses fixed queries supplied by the manifest and is not an autonomous agent trajectory.",
        "No model executes an edit, so this runner does not measure pass rate, prewalk handoff quality, or end-to-end task cost.",
        "Cold indexing and warm latency depend on host hardware and filesystem cache state.",
    ];
    if dataset_kind == "blind_holdout" {
        limitations.push(
            "Holdout prompts and labels were frozen before evaluation from issue reports and pinned source inspection; relevance labels remain human judgments, not proof that every labeled range is required.",
        );
        limitations.push(
            "A holdout result is evaluation evidence, not permission to tune against the same dataset while continuing to call it blind.",
        );
        if consumed_diagnostic {
            limitations.push(
                "This explicitly diagnostic rerun used a consumed holdout and an unverified runtime tree; it is not blind or generalization evidence.",
            );
        }
    } else if dataset_kind == "prospective_validation" {
        limitations.push(
            "Validation prompts and labels were frozen from open issue reports and pinned source inspection, then used during retrieval tuning; this is not blind holdout evidence.",
        );
        limitations.push(
            "Four validation tasks are retrieval development evidence, not a statistically powered product claim.",
        );
    } else if dataset_kind == "external_retrieval_corpus" {
        limitations.push(
            "External labels retain their source methodology and limitations; this diagnostic does not make them blind or independently validated.",
        );
        limitations.push(
            "File-only tasks do not contribute line-anchor recall, and unsupported task families remain excluded rather than inferred.",
        );
        limitations.push(
            "External-corpus results are comparison evidence for retrieval experiments, not permission to change production ranking without a separately frozen promotion gate.",
        );
    } else {
        limitations.push(
            "Development prompts and labels were derived retrospectively from public future fixes and must not be reported as blind generalization evidence.",
        );
        limitations.push(
            "Eight development tasks are retrieval smoke evidence, not a statistically powered product claim.",
        );
    }
    if concept_labels {
        limitations.push(
            "Concept coverage credits a frozen concept when one labeled anchor is present; it does not prove that the complete implementation, test, or explanation was retrieved.",
        );
        limitations.push(
            "The concept overlay partitions labels from a consumed development set. Its thresholds are regression floors, not promotion or generalization evidence.",
        );
    }
    limitations
}

fn is_patch_free_dataset(dataset_kind: &str) -> bool {
    matches!(dataset_kind, "prospective_validation" | "blind_holdout")
}

fn workflow_evidence_from_json_prompt(prompt: &str) -> Result<WorkflowEvidence, Box<dyn Error>> {
    let query: serde_json::Value = serde_json::from_str(prompt)?;
    let object = query
        .as_object()
        .ok_or("workflow-evidence prompt must be a JSON object")?;
    let failure_trace = object
        .get("failure_excerpt")
        .and_then(serde_json::Value::as_str)
        .ok_or("workflow-evidence prompt has no failure_excerpt")?;
    let failure_trace = utf8_tail(failure_trace, 8 * 1024);
    let mut test_intents = Vec::new();
    let mut seen_test_intents = HashSet::new();
    for line in failure_trace.lines() {
        let trimmed = line.trim();
        if let Some(failed) = trimmed.strip_prefix("FAILED ") {
            retain_observed_value(
                &mut test_intents,
                &mut seen_test_intents,
                failed.trim().to_owned(),
            );
        } else if let Some(test_name) = trimmed.strip_prefix("def test_") {
            let suffix = test_name
                .split(|character: char| !character.is_alphanumeric() && character != '_')
                .next()
                .unwrap_or_default();
            if !suffix.is_empty() {
                retain_observed_value(
                    &mut test_intents,
                    &mut seen_test_intents,
                    format!("test_{suffix}"),
                );
            }
        }
    }
    if let Some(command) = object.get("command").and_then(serde_json::Value::as_str)
        && !command.trim().is_empty()
    {
        retain_observed_value(
            &mut test_intents,
            &mut seen_test_intents,
            command.trim().to_owned(),
        );
    }
    Ok(WorkflowEvidence::new()
        .with_failure_traces([failure_trace.clone()])
        .with_symbols(observed_trace_symbols(&failure_trace))
        .with_paths(observed_trace_paths(&failure_trace))
        .with_test_intents(test_intents.into_iter().take(8)))
}

fn utf8_tail(value: &str, max_bytes: usize) -> String {
    if value.len() <= max_bytes {
        return value.to_owned();
    }
    let mut start = value.len() - max_bytes;
    while !value.is_char_boundary(start) {
        start += 1;
    }
    value[start..].to_owned()
}

fn observed_trace_paths(trace: &str) -> Vec<String> {
    let mut paths = BTreeSet::new();
    for token in trace.split_whitespace() {
        let token = token.trim_matches(|character: char| {
            !character.is_alphanumeric() && !matches!(character, '/' | '.' | '_' | '-' | ':' | '\\')
        });
        let path = token.split_once("::").map_or(token, |(path, _)| path);
        let path = path
            .split_once(':')
            .filter(|(_, suffix)| {
                suffix
                    .chars()
                    .next()
                    .is_some_and(|character| character.is_ascii_digit())
            })
            .map_or(path, |(path, _)| path)
            .trim_start_matches("./");
        if path.contains('/') && path.contains('.') && validate_benchmark_path(path).is_ok() {
            paths.insert(path.to_owned());
        }
    }
    paths.into_iter().take(8).collect()
}

fn observed_trace_symbols(trace: &str) -> Vec<String> {
    let mut symbols = Vec::new();
    let mut seen = HashSet::new();
    for (index, value) in trace.split('`').enumerate() {
        if index % 2 == 1 {
            retain_trace_symbol(&mut symbols, &mut seen, value);
        }
    }
    for token in trace.split_whitespace() {
        let token = token.trim_matches(|character: char| {
            !character.is_alphanumeric() && !matches!(character, '_' | '.' | ':')
        });
        retain_trace_symbol(&mut symbols, &mut seen, token);
    }
    symbols.into_iter().take(8).collect()
}

fn retain_trace_symbol(symbols: &mut Vec<String>, seen: &mut HashSet<String>, value: &str) {
    if (3..=128).contains(&value.len())
        && !value.contains('/')
        && !value.chars().any(char::is_whitespace)
        && value.chars().any(char::is_alphabetic)
        && (value.contains('_')
            || value.contains('.')
            || value.contains("::")
            || value.chars().any(char::is_uppercase))
        && seen.insert(value.to_owned())
    {
        symbols.push(value.to_owned());
    }
}

fn retain_observed_value(values: &mut Vec<String>, seen: &mut HashSet<String>, value: String) {
    if !value.is_empty() && seen.insert(value.clone()) {
        values.push(value);
    }
}

fn workflow_evidence_counts(evidence: &WorkflowEvidence) -> WorkflowEvidenceCounts {
    WorkflowEvidenceCounts {
        failure_traces: evidence.failure_traces.len(),
        symbols: evidence.symbols.len(),
        paths: evidence.paths.len(),
        test_intents: evidence.test_intents.len(),
        total_bytes: evidence
            .failure_traces
            .iter()
            .chain(&evidence.symbols)
            .chain(&evidence.paths)
            .chain(&evidence.test_intents)
            .map(String::len)
            .sum(),
    }
}

async fn run_task(
    root: &Path,
    services: &Services,
    task: TaskSpec,
    rg_max_lines_per_query: usize,
    concept_labels: Option<&ConceptTaskLabels>,
    workflow_evidence_enabled: bool,
) -> Result<TaskReport, Box<dyn Error>> {
    let relevant_paths = task
        .relevant_files
        .iter()
        .map(|file| file.path.clone())
        .collect::<HashSet<_>>();
    let reads = task
        .relevant_files
        .iter()
        .map(|file| {
            validate_benchmark_path(&file.path)?;
            Ok(BaselineRead {
                path: &file.path,
                content: fs::read_to_string(root.join(&file.path))?,
            })
        })
        .collect::<Result<Vec<_>, Box<dyn Error>>>()?;
    let oracle_source_tokens = reads.iter().map(|read| tokens::count(&read.content)).sum();
    let oracle_json = serde_json::to_string(&reads)?;
    let rg_results = task
        .rg_queries
        .iter()
        .map(|query| run_rg(root, query, rg_max_lines_per_query))
        .collect::<Result<Vec<_>, Box<dyn Error>>>()?;
    let rg_discovery_tokens = rg_results
        .iter()
        .map(|result| tokens::count(&result.json_lines))
        .sum();
    let rg_json = serde_json::to_string(&rg_results)?;
    let scripted_json = serde_json::to_string(&ScriptedBaseline {
        searches: &rg_results,
        reads: &reads,
    })?;

    let request = ContextRequest {
        task: task.prompt.clone(),
        token_budget: task.token_budget,
        include_paths: Vec::new(),
        must_include_paths: Vec::new(),
        must_include_symbols: Vec::new(),
        max_fragments: None,
        plan_only: false,
        focus_paths: Vec::new(),
        strict_focus_paths: false,
        minimum_fragments_per_focus_path: None,
        focus_symbols: Vec::new(),
        exclude_paths: Vec::new(),
        known_hashes: Vec::new(),
        receipt_id: None,
        prior_repository_generation: None,
        base_revision: None,
        changed_paths: Vec::new(),
        strict_changed_paths: false,
        verbose_diagnostics: false,
    };
    let workflow_evidence = if workflow_evidence_enabled {
        workflow_evidence_from_json_prompt(&task.prompt)?
    } else {
        WorkflowEvidence::default()
    };
    let workflow_evidence_counts = workflow_evidence_counts(&workflow_evidence);
    let started = Instant::now();
    let evaluation = services
        .context_evaluation_with_workflow_evidence(request.clone(), workflow_evidence.clone())
        .await?;
    let concept_coverage = concept_labels
        .map(|labels| {
            evaluate_concept_coverage(
                labels,
                &evaluation.generated_candidates,
                &evaluation.response.fragments,
            )
        })
        .transpose()?;
    let response = evaluation.response;
    let first_context_ms = elapsed_ms(started);
    verify_token_accounting(&response)?;
    let canonical_response = deterministic_context_json(&response)?;
    let mut warm_context_ms_samples = Vec::with_capacity(3);
    for _ in 0..3 {
        let started = Instant::now();
        let warm = services
            .context_with_workflow_evidence(request.clone(), workflow_evidence.clone())
            .await?;
        warm_context_ms_samples.push(elapsed_ms(started));
        verify_token_accounting(&warm)?;
        if deterministic_context_json(&warm)? != canonical_response {
            return Err(format!("{} returned nondeterministic context", task.id).into());
        }
    }
    let returned_files = sorted_unique(response.fragments.iter().map(|item| item.path.clone()));
    let candidate_files = evaluation.generated_candidate_paths;
    let relevant_candidate_evidence = evaluation
        .generated_candidates
        .into_iter()
        .filter(|candidate| relevant_paths.contains(&candidate.path))
        .map(|candidate| CandidateEvidenceSummary {
            path: candidate.path,
            start_line: candidate.start_line,
            end_line: candidate.end_line,
            representation: candidate.representation,
            match_kinds: candidate.match_kinds,
            concepts: candidate.concepts,
            concept_weight: candidate.concept_weight,
            score: candidate.score,
            token_count: candidate.token_count,
        })
        .collect();
    let returned_evidence = response
        .fragments
        .iter()
        .map(|fragment| EvidenceSummary {
            path: fragment.path.clone(),
            start_line: fragment.start_line,
            end_line: fragment.end_line,
            representation: fragment.representation.clone(),
            reason: fragment.reason.clone(),
            score: fragment.score,
            token_count: fragment.token_count,
            content_hash: fragment.content_hash.clone(),
        })
        .collect();
    let relevant_files_found = returned_files
        .iter()
        .filter(|path| relevant_paths.contains(*path))
        .count();
    let candidate_relevant_files_found = candidate_files
        .iter()
        .filter(|path| relevant_paths.contains(*path))
        .count();
    let omitted_relevant_files = task
        .relevant_files
        .iter()
        .filter_map(|file| {
            if returned_files.binary_search(&file.path).is_ok() {
                return None;
            }
            candidate_files
                .binary_search(&file.path)
                .is_ok()
                .then(|| OmittedRelevantFile {
                    path: file.path.clone(),
                    reason: "generated but not selected",
                })
        })
        .collect();
    let labeled_file_precision = ratio(relevant_files_found, returned_files.len());
    let unlabeled_returned_files = returned_files
        .iter()
        .filter(|path| !relevant_paths.contains(*path))
        .cloned()
        .collect::<Vec<_>>();
    let dead_end_fragments = response
        .fragments
        .iter()
        .filter(|fragment| !relevant_paths.contains(&fragment.path))
        .count();
    let dead_end_source_tokens = response
        .fragments
        .iter()
        .filter(|fragment| !relevant_paths.contains(&fragment.path))
        .map(|fragment| fragment.token_count)
        .sum();
    let line_anchors = task
        .relevant_files
        .iter()
        .map(|file| file.line_anchors.len())
        .sum();
    let line_anchors_found = count_line_anchors(&response, &task.relevant_files);
    let leantoken_total_json_tokens = tokens::count(&serde_json::to_string(&response)?);

    let known = response
        .fragments
        .iter()
        .map(|fragment| fragment.content_hash.clone())
        .collect::<Vec<_>>();
    let known_set = known.iter().cloned().collect::<HashSet<_>>();
    let repeat_request = ContextRequest {
        known_hashes: known,
        receipt_id: None,
        prior_repository_generation: Some(response.meta.repository_generation),
        ..request
    };
    let repeat_request_json_tokens = tokens::count(&serde_json::to_string(&repeat_request)?);
    let repeat = services
        .context_with_workflow_evidence(repeat_request, workflow_evidence)
        .await?;
    let known_fragments_resent = repeat
        .fragments
        .iter()
        .filter(|fragment| known_set.contains(&fragment.content_hash))
        .count();
    if known_fragments_resent != 0 {
        return Err(format!(
            "{} resent {known_fragments_resent} fragments whose hashes were known",
            task.id
        )
        .into());
    }
    let repeat_total_json_tokens = tokens::count(&serde_json::to_string(&repeat)?);
    let estimated_repeated_range_source_tokens = repeat
        .fragments
        .iter()
        .map(|fragment| {
            let prior_ranges = response
                .fragments
                .iter()
                .filter(|prior| prior.path == fragment.path)
                .map(|prior| (prior.start_line, prior.end_line))
                .collect::<Vec<_>>();
            repeated_range_token_estimate(
                fragment.start_line,
                fragment.end_line,
                fragment.token_count,
                &prior_ranges,
            )
        })
        .sum();
    let two_turn_context_json_tokens = leantoken_total_json_tokens
        .saturating_add(repeat_request_json_tokens)
        .saturating_add(repeat_total_json_tokens);
    let known_hash_omission_visible = reports_known_hash_omission(&repeat);
    if !known_set.is_empty() && !known_hash_omission_visible {
        return Err(format!("{} hid all known-hash omissions", task.id).into());
    }

    Ok(TaskReport {
        id: task.id,
        prompt: task.prompt,
        languages: task.languages,
        task_shapes: task.task_shapes,
        token_budget: task.token_budget,
        workflow_evidence: workflow_evidence_counts,
        relevant_files: task
            .relevant_files
            .into_iter()
            .map(|file| file.path)
            .collect(),
        returned_files,
        returned_evidence,
        candidate_files,
        relevant_candidate_evidence,
        omitted_relevant_files,
        relevant_files_found,
        relevant_file_recall: ratio(relevant_files_found, relevant_paths.len()),
        candidate_relevant_files_found,
        candidate_relevant_file_recall: ratio(candidate_relevant_files_found, relevant_paths.len()),
        labeled_file_precision,
        line_anchors,
        line_anchors_found,
        line_anchor_recall: optional_ratio(line_anchors_found, line_anchors),
        unlabeled_returned_files,
        oracle_source_tokens,
        oracle_minimal_read_json_tokens: tokens::count(&oracle_json),
        rg_discovery_tokens,
        rg_discovery_json_tokens: tokens::count(&rg_json),
        scripted_baseline_total_json_tokens: tokens::count(&scripted_json),
        leantoken_source_tokens: response.meta.emitted_tokens,
        leantoken_total_json_tokens,
        source_savings_against_oracle_fraction: savings(
            oracle_source_tokens,
            response.meta.emitted_tokens,
        ),
        total_json_savings_against_scripted_fraction: savings(
            tokens::count(&scripted_json),
            leantoken_total_json_tokens,
        ),
        first_context_ms,
        warm_context_median_ms: percentile(&warm_context_ms_samples, 0.50),
        warm_context_p95_ms: percentile(&warm_context_ms_samples, 0.95),
        warm_context_ms_samples,
        second_response_source_tokens: repeat.meta.emitted_tokens,
        estimated_repeated_range_source_tokens,
        repeat_request_json_tokens,
        repeat_total_json_tokens,
        two_turn_context_json_tokens,
        known_fragments_resent,
        known_hash_omission_visible,
        dead_end_fragments,
        dead_end_source_tokens,
        concept_coverage,
    })
}

fn reports_known_hash_omission(response: &ContextResponse) -> bool {
    response.omission_summary.known_hash > 0
        || response
            .omitted
            .iter()
            .any(|candidate| candidate.reason == "known hash")
}

fn run_rg<'a>(
    root: &Path,
    query: &'a str,
    max_lines: usize,
) -> Result<RgResult<'a>, Box<dyn Error>> {
    let mut child = Command::new("rg")
        .args([
            "--no-config",
            "--sort",
            "path",
            "--path-separator",
            "/",
            "--json",
            "--line-number",
            "--fixed-strings",
            "--",
            query,
            ".",
        ])
        .current_dir(root)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let stdout = child.stdout.take().ok_or("ripgrep stdout unavailable")?;
    let mut reader = BufReader::new(stdout);
    let mut json_lines = String::new();
    let mut line = String::new();
    let mut lines = 0usize;
    let mut truncated = false;
    loop {
        line.clear();
        if reader.read_line(&mut line)? == 0 {
            break;
        }
        if lines == max_lines {
            truncated = true;
            let _ = child.kill();
            break;
        }
        json_lines.push_str(line.trim_end_matches(['\r', '\n']));
        json_lines.push('\n');
        lines += 1;
    }
    let output = child.wait_with_output()?;
    if !truncated && !success_or_no_matches(output.status) {
        return Err(format!(
            "ripgrep failed for {query:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        )
        .into());
    }
    Ok(RgResult {
        query,
        json_lines,
        truncated,
    })
}

fn command_version(command: &str) -> Result<String, Box<dyn Error>> {
    let output = Command::new(command).arg("--version").output()?;
    if !output.status.success() {
        return Err(format!("{command} --version failed").into());
    }
    Ok(String::from_utf8(output.stdout)?
        .lines()
        .next()
        .unwrap_or_default()
        .to_owned())
}

fn preflight(manifest: &Manifest, repos_root: &Path) -> Result<(), Box<dyn Error>> {
    if manifest.rg_max_lines_per_query == 0 || manifest.rg_max_lines_per_query > 10_000 {
        return Err("rg_max_lines_per_query must be between 1 and 10000".into());
    }
    let mut corpus_names = HashSet::new();
    let mut task_ids = HashSet::new();
    for corpus in &manifest.corpora {
        if !corpus_names.insert(corpus.name.as_str()) {
            return Err(format!("duplicate corpus name: {}", corpus.name).into());
        }
        validate_benchmark_path(&corpus.directory)?;
        let root = repos_root.join(&corpus.directory).canonicalize()?;
        let top_level = git_output(&root, &["rev-parse", "--show-toplevel"])?;
        if Path::new(top_level.trim()).canonicalize()? != root {
            return Err(format!("{} is not the Git top-level directory", root.display()).into());
        }
        verify_revision(&root, &corpus.base_revision)?;
        if let Some(fix_commit) = &corpus.fix_commit {
            let parent_arg = format!("{fix_commit}^");
            let fix_parent = git_output(&root, &["rev-parse", &parent_arg])?;
            if fix_parent.trim() != corpus.base_revision {
                return Err(
                    format!("{} is not the parent of {fix_commit}", corpus.base_revision).into(),
                );
            }
        } else if !is_patch_free_dataset(&manifest.dataset_kind)
            && manifest.dataset_kind != "external_retrieval_corpus"
        {
            return Err(format!(
                "{} has no fix_commit for dataset kind {}",
                corpus.name, manifest.dataset_kind
            )
            .into());
        }
        if !git_output(
            &root,
            &["status", "--porcelain=v1", "--untracked-files=all"],
        )?
        .trim()
        .is_empty()
        {
            return Err(format!("{} has uncommitted or untracked files", root.display()).into());
        }
        for task in &corpus.tasks {
            if !task_ids.insert(task.id.as_str()) {
                return Err(format!("duplicate task id: {}", task.id).into());
            }
            if task.prompt.trim().is_empty() {
                return Err(format!("{} has an empty prompt", task.id).into());
            }
            if task.token_budget == 0 || task.token_budget > 32_000 {
                return Err(format!("{} has an invalid token budget", task.id).into());
            }
            if task.rg_queries.iter().any(|query| query.trim().is_empty()) {
                return Err(format!("{} has an empty ripgrep query", task.id).into());
            }
            if task.relevant_files.is_empty() {
                return Err(format!("{} has no relevance labels", task.id).into());
            }
            let mut relevant_paths = HashSet::new();
            for file in &task.relevant_files {
                if !relevant_paths.insert(file.path.as_str()) {
                    return Err(format!("{} repeats relevant path {}", task.id, file.path).into());
                }
                validate_benchmark_path(&file.path)?;
                let content = fs::read_to_string(root.join(&file.path))?;
                let line_count = content.lines().count();
                if let Some(line) = file
                    .line_anchors
                    .iter()
                    .find(|line| **line == 0 || **line > line_count)
                {
                    return Err(format!(
                        "{} anchor {}:{} is outside 1..={line_count}",
                        task.id, file.path, line
                    )
                    .into());
                }
            }
        }
    }
    Ok(())
}

fn git_output(root: &Path, args: &[&str]) -> Result<String, Box<dyn Error>> {
    let output = Command::new("git").args(args).current_dir(root).output()?;
    if !output.status.success() {
        return Err(format!(
            "git {} failed in {}: {}",
            args.join(" "),
            root.display(),
            String::from_utf8_lossy(&output.stderr)
        )
        .into());
    }
    Ok(String::from_utf8(output.stdout)?)
}

fn verify_candidate_runtime_tree(
    manifest: &Manifest,
    source_root: &Path,
) -> Result<Option<bool>, Box<dyn Error>> {
    if manifest.dataset_kind != "blind_holdout" {
        return Ok(None);
    }
    let candidate = manifest
        .candidate_revision
        .as_deref()
        .ok_or("blind holdout has no candidate revision")?;
    if !git_output(
        source_root,
        &[
            "status",
            "--porcelain=v1",
            "--untracked-files=all",
            "--",
            "src",
            "Cargo.toml",
            "Cargo.lock",
        ],
    )?
    .trim()
    .is_empty()
    {
        return Err("LeanToken runtime tree has uncommitted changes".into());
    }
    let output = Command::new("git")
        .args([
            "diff",
            "--quiet",
            candidate,
            "--",
            "src",
            "Cargo.toml",
            "Cargo.lock",
        ])
        .current_dir(source_root)
        .output()?;
    match output.status.code() {
        Some(0) => Ok(Some(true)),
        Some(1) => {
            Err(format!("LeanToken runtime tree differs from frozen candidate {candidate}").into())
        }
        _ => Err(format!(
            "could not compare LeanToken runtime tree with {candidate}: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )
        .into()),
    }
}

fn verify_revision(root: &Path, expected: &str) -> Result<(), Box<dyn Error>> {
    let output = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(root)
        .output()?;
    if !output.status.success() {
        return Err(format!("{} is not a readable Git checkout", root.display()).into());
    }
    let actual = String::from_utf8(output.stdout)?.trim().to_owned();
    if actual != expected {
        return Err(format!("{} is at {actual}, expected {expected}", root.display()).into());
    }
    Ok(())
}

fn validate_benchmark_path(path: &str) -> Result<(), Box<dyn Error>> {
    let path = Path::new(path);
    if path.is_absolute()
        || path
            .components()
            .any(|part| matches!(part, std::path::Component::ParentDir))
    {
        return Err(format!("invalid benchmark path: {}", path.display()).into());
    }
    Ok(())
}

fn count_line_anchors(response: &ContextResponse, relevant: &[RelevantFile]) -> usize {
    relevant
        .iter()
        .map(|file| {
            file.line_anchors
                .iter()
                .filter(|line| {
                    response.fragments.iter().any(|fragment| {
                        fragment.path == file.path
                            && fragment.start_line <= **line
                            && fragment.end_line >= **line
                    })
                })
                .count()
        })
        .sum()
}

fn verify_token_accounting(response: &ContextResponse) -> Result<(), Box<dyn Error>> {
    let declared = response
        .fragments
        .iter()
        .map(|fragment| fragment.token_count)
        .sum::<usize>();
    if declared != response.meta.emitted_tokens {
        return Err(format!(
            "context token mismatch: fragment fields={declared}, meta={}",
            response.meta.emitted_tokens
        )
        .into());
    }
    if !response.meta.token_count_exact {
        // Estimate tokenizers do not promise byte-for-byte equality with a
        // re-count, but the stored fragment counts must still be consistent.
        return Ok(());
    }
    let counted = response
        .fragments
        .iter()
        .map(|fragment| tokens::count(&fragment.content))
        .sum::<usize>();
    if declared != counted {
        return Err(format!(
            "context token mismatch: fragment fields={declared}, counted={counted}"
        )
        .into());
    }
    Ok(())
}

fn deterministic_context_json(response: &ContextResponse) -> Result<String, serde_json::Error> {
    let mut deterministic = response.clone();
    deterministic.meta.receipt_id = None;
    serde_json::to_string(&deterministic)
}

fn database_footprint(database: &Path) -> Result<u64, Box<dyn Error>> {
    let Some(file_name) = database.file_name().and_then(|name| name.to_str()) else {
        return Ok(0);
    };
    let Some(parent) = database.parent() else {
        return Ok(0);
    };
    let mut bytes = 0;
    for entry in fs::read_dir(parent)? {
        let entry = entry?;
        let name = entry.file_name();
        if name.to_string_lossy().starts_with(file_name) {
            bytes += entry.metadata()?.len();
        }
    }
    Ok(bytes)
}

fn success_or_no_matches(status: ExitStatus) -> bool {
    status.success() || status.code() == Some(1)
}

fn sorted_unique(values: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut values = values.into_iter().collect::<Vec<_>>();
    values.sort_unstable();
    values.dedup();
    values
}

fn elapsed_ms(started: Instant) -> f64 {
    started.elapsed().as_secs_f64() * 1_000.0
}

fn percentile(values: &[f64], quantile: f64) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    let index = ((sorted.len() - 1) as f64 * quantile).ceil() as usize;
    sorted[index]
}

const fn default_rg_max_lines() -> usize {
    200
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

fn savings(baseline: usize, actual: usize) -> f64 {
    if baseline == 0 {
        0.0
    } else {
        1.0 - actual as f64 / baseline as f64
    }
}

fn repeated_range_token_estimate(
    start_line: usize,
    end_line: usize,
    token_count: usize,
    prior_ranges: &[(usize, usize)],
) -> usize {
    if end_line < start_line || token_count == 0 {
        return 0;
    }
    let line_count = end_line - start_line + 1;
    let mut repeated = vec![false; line_count];
    for &(prior_start, prior_end) in prior_ranges {
        let overlap_start = start_line.max(prior_start);
        let overlap_end = end_line.min(prior_end);
        if overlap_start > overlap_end {
            continue;
        }
        for line in overlap_start..=overlap_end {
            repeated[line - start_line] = true;
        }
    }
    let repeated_lines = repeated.into_iter().filter(|value| *value).count();
    token_count
        .saturating_mul(repeated_lines)
        .div_ceil(line_count)
}

fn accumulate(aggregate: &mut AggregateReport, task: &TaskReport) {
    aggregate.task_count += 1;
    aggregate.relevant_files += task.relevant_files.len();
    aggregate.relevant_files_found += task.relevant_files_found;
    aggregate.candidate_relevant_files_found += task.candidate_relevant_files_found;
    aggregate.returned_files += task.returned_files.len();
    aggregate.line_anchors += task.line_anchors;
    aggregate.line_anchors_found += task.line_anchors_found;
    aggregate.oracle_source_tokens += task.oracle_source_tokens;
    aggregate.rg_discovery_tokens += task.rg_discovery_tokens;
    aggregate.scripted_baseline_total_json_tokens += task.scripted_baseline_total_json_tokens;
    aggregate.leantoken_source_tokens += task.leantoken_source_tokens;
    aggregate.leantoken_total_json_tokens += task.leantoken_total_json_tokens;
    aggregate.known_fragments_resent += task.known_fragments_resent;
    aggregate.dead_end_fragments += task.dead_end_fragments;
    aggregate.dead_end_source_tokens += task.dead_end_source_tokens;
    aggregate.second_response_source_tokens += task.second_response_source_tokens;
    aggregate.estimated_repeated_range_source_tokens += task.estimated_repeated_range_source_tokens;
    aggregate.repeat_request_json_tokens += task.repeat_request_json_tokens;
    aggregate.repeat_total_json_tokens += task.repeat_total_json_tokens;
    aggregate.two_turn_context_json_tokens += task.two_turn_context_json_tokens;
    if let Some(coverage) = &task.concept_coverage {
        aggregate.concepts += coverage.concepts;
        aggregate.candidate_concepts_found += coverage.candidate_concepts_found;
        aggregate.selected_concepts_found += coverage.selected_concepts_found;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use leantoken::{
        ContextCoverageReceipt, ContextOmissionSummary, ContextWorkflow, EvidenceReceipt,
        Freshness, ResponseMeta,
    };

    fn external_manifest() -> Manifest {
        serde_json::from_value(serde_json::json!({
            "schema_version": 4,
            "dataset_kind": "external_retrieval_corpus",
            "frozen_at": "2026-07-26",
            "evaluation_protocol": "frozen external evaluation",
            "reclassification_rule": "freeze a new lock",
            "description": "external fixture",
            "corpora": [{
                "name": "fixture",
                "url": "https://example.com/source",
                "directory": "fixture",
                "base_revision": "1111111111111111111111111111111111111111",
                "prompt_provenance": "prompts from the pinned dataset",
                "label_provenance": "labels from the pinned dataset",
                "dataset_url": "https://example.com/dataset",
                "dataset_revision": "2222222222222222222222222222222222222222",
                "dataset_license": "MIT",
                "external_limitations": ["fixture labels are illustrative"],
                "tasks": [{
                    "id": "fixture:001",
                    "prompt": "Find the fixture.",
                    "languages": ["rust"],
                    "task_shapes": ["fixture"],
                    "rg_queries": ["fixture"],
                    "relevant_files": [{"path": "src/lib.rs"}],
                    "token_budget": 1000
                }]
            }]
        }))
        .expect("parse external manifest")
    }

    #[test]
    fn arb_prompt_derives_bounded_workflow_evidence_without_gold_labels() {
        let prompt = serde_json::json!({
            "command": "cargo test",
            "failure_excerpt": concat!(
                "error[E0599]: no method named `default_values_if`\n",
                " --> tests/builder/default_vals.rs:326:18\n",
                "FAILED tests/builder/default_vals.rs::default_values_regression\n"
            )
        })
        .to_string();

        let evidence = workflow_evidence_from_json_prompt(&prompt).expect("workflow evidence");

        assert_eq!(evidence.failure_traces.len(), 1);
        assert!(
            evidence
                .symbols
                .iter()
                .any(|value| value == "default_values_if")
        );
        assert_eq!(evidence.paths, ["tests/builder/default_vals.rs"]);
        assert!(
            evidence
                .test_intents
                .iter()
                .any(|value| value.contains("default_values_regression"))
        );
        assert!(workflow_evidence_counts(&evidence).total_bytes <= 32 * 1024);
    }

    fn context_response(receipt_id: &str) -> ContextResponse {
        ContextResponse {
            workflow: ContextWorkflow::Implementation,
            workflow_receipt: None,
            plan: None,
            fragments: Vec::new(),
            receipt: EvidenceReceipt {
                task_fingerprint: "task".into(),
                fragment_hashes: Vec::new(),
            },
            diff_scope: None,
            omitted: Vec::new(),
            omission_summary: ContextOmissionSummary::default(),
            coverage: ContextCoverageReceipt::default(),
            routing: None,
            handoff_manifest: None,
            warnings: Vec::new(),
            meta: ResponseMeta {
                repository_id: "repository".into(),
                repository_generation: 7,
                freshness: Freshness::Current,
                source_tokens: 0,
                protocol_tokens: 0,
                path_and_metadata_tokens: 0,
                total_response_tokens: 0,
                payload_tokens: 0,
                tokenizer: "cl100k_base".into(),
                emitted_tokens: 0,
                token_count_exact: true,
                receipt_id: Some(receipt_id.into()),
                receipt_suppressed_exact: 0,
                receipt_suppressed_overlap: 0,
                receipt_near_duplicates: 0,
                next_cursor: None,
            },
        }
    }

    #[test]
    fn compact_omission_summary_reports_known_hashes() {
        let mut response = context_response("receipt");
        assert!(!reports_known_hash_omission(&response));

        response.omission_summary.known_hash = 1;
        assert!(reports_known_hash_omission(&response));
    }

    fn candidate(path: &str, start_line: usize, end_line: usize) -> ContextCandidateEvaluation {
        ContextCandidateEvaluation {
            path: path.into(),
            start_line,
            end_line,
            representation: "source".into(),
            match_kinds: vec!["text".into()],
            concepts: vec!["query".into()],
            concept_weight: 1.0,
            score: 1.0,
            token_count: 10,
        }
    }

    fn fragment(path: &str, start_line: usize, end_line: usize) -> ContextFragment {
        ContextFragment {
            path: path.into(),
            start_line,
            end_line,
            target_start_line: Some(start_line),
            target_end_line: Some(end_line),
            truncated: false,
            representation: "source".into(),
            content: "evidence".into(),
            content_hash: "hash".into(),
            score: 1.0,
            reason: "text".into(),
            token_count: 10,
        }
    }

    fn concept_task_labels() -> ConceptTaskLabels {
        ConceptTaskLabels {
            id: "task".into(),
            concepts: vec![
                ConceptLabel {
                    id: "implementation".into(),
                    description: "implementation evidence".into(),
                    evidence: vec![ConceptEvidence {
                        path: "src/lib.rs".into(),
                        line_anchors: vec![10],
                    }],
                },
                ConceptLabel {
                    id: "regression".into(),
                    description: "regression evidence".into(),
                    evidence: vec![ConceptEvidence {
                        path: "tests/lib.rs".into(),
                        line_anchors: vec![20],
                    }],
                },
            ],
        }
    }

    #[test]
    fn repeated_range_tokens_include_partial_overlap_with_a_different_hash() {
        assert_eq!(
            repeated_range_token_estimate(8, 12, 50, &[(1, 10), (20, 30)]),
            30
        );
    }

    #[test]
    fn prospective_validation_requires_provenance_and_excludes_future_fixes() {
        let mut manifest: Manifest =
            serde_json::from_str(include_str!("../benchmarks/validation.json"))
                .expect("validation manifest");
        validate_manifest(&manifest).expect("valid validation manifest");

        manifest.dataset_kind = "blind_holdout".into();
        validate_manifest(&manifest).expect("same provenance is valid for a future blind set");

        manifest.schema_version = 3;
        manifest.candidate_revision = Some("frozen-candidate".into());
        manifest.evaluation_protocol = Some("frozen evaluation".into());
        manifest.reclassification_rule = Some("reclassify after inspection".into());
        assert!(
            validate_manifest(&manifest).is_err(),
            "schema v3 must reject a four-task set"
        );

        manifest.schema_version = 2;
        manifest.corpora[0].fix_commit = Some("future".into());
        assert!(validate_manifest(&manifest).is_err());
    }

    #[test]
    fn sealed_holdout_manifest_meets_schema_and_coverage_contract() {
        let manifest: Manifest = serde_json::from_str(include_str!("../benchmarks/holdout.json"))
            .expect("holdout manifest");

        validate_manifest(&manifest).expect("valid sealed holdout");
        assert_eq!(manifest.schema_version, 3);
        assert_eq!(manifest.dataset_kind, "blind_holdout");
    }

    #[test]
    fn candidate_runtime_verification_is_not_applicable_to_validation_sets() {
        let manifest: Manifest =
            serde_json::from_str(include_str!("../benchmarks/validation.json"))
                .expect("validation manifest");

        assert_eq!(
            verify_candidate_runtime_tree(&manifest, Path::new(env!("CARGO_MANIFEST_DIR")))
                .expect("verification decision"),
            None
        );
    }

    #[test]
    fn external_manifest_requires_pinned_provenance_without_future_fix() {
        let mut manifest = external_manifest();
        validate_manifest(&manifest).expect("valid external manifest");

        manifest.corpora[0].dataset_revision = Some("not-a-full-revision".into());
        assert!(
            validate_manifest(&manifest)
                .expect_err("reject unpinned dataset")
                .to_string()
                .contains("full Git object ID")
        );

        let mut manifest = external_manifest();
        manifest.corpora[0].fix_commit = Some("3333333333333333333333333333333333333333".into());
        assert!(
            validate_manifest(&manifest)
                .expect_err("reject future fix")
                .to_string()
                .contains("must not name a future fix")
        );

        let mut manifest = external_manifest();
        manifest.evaluation_protocol = None;
        assert!(
            validate_manifest(&manifest)
                .expect_err("reject missing protocol")
                .to_string()
                .contains("evaluation_protocol")
        );

        let mut manifest = external_manifest();
        manifest.corpora[0].tasks[0].task_shapes.clear();
        assert!(
            validate_manifest(&manifest)
                .expect_err("reject missing task stratum")
                .to_string()
                .contains("language and task-shape strata")
        );
    }

    #[test]
    fn deterministic_context_comparison_ignores_only_receipt_identity() {
        let first = context_response("first");
        let mut second = context_response("second");
        assert_eq!(
            deterministic_context_json(&first).expect("serialize first response"),
            deterministic_context_json(&second).expect("serialize second response")
        );

        second.meta.repository_generation += 1;
        assert_ne!(
            deterministic_context_json(&first).expect("serialize first response"),
            deterministic_context_json(&second).expect("serialize changed response")
        );
    }

    #[test]
    fn context_concept_labels_bind_and_partition_the_validation_manifest() {
        let source_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("benchmarks/validation.json");
        let source_json = fs::read_to_string(&source_path).expect("read source manifest");
        let source: Manifest = serde_json::from_str(&source_json).expect("parse source manifest");
        let labels = load_concept_labels(
            &Path::new(env!("CARGO_MANIFEST_DIR")).join("benchmarks/context_concept_coverage.json"),
            &source_path,
            &source,
            &blake3::hash(source_json.as_bytes()).to_hex().to_string(),
        )
        .expect("load concept labels");

        assert_eq!(labels.tasks.len(), 4);
        assert_eq!(
            labels
                .tasks
                .values()
                .map(|task| task.concepts.len())
                .sum::<usize>(),
            12
        );
    }

    #[test]
    fn context_feedback_regressions_freeze_paired_tasks_and_role_facets() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("benchmarks/context_feedback_regressions.json");
        let fixture: serde_json::Value = serde_json::from_str(
            &fs::read_to_string(&path).expect("read feedback regression fixture"),
        )
        .expect("parse feedback regression fixture");

        assert_eq!(fixture["schema_version"], 1);
        let revision = fixture["repository_revision"]
            .as_str()
            .expect("repository revision");
        assert_eq!(revision.len(), 40);
        assert!(revision.bytes().all(|byte| byte.is_ascii_hexdigit()));
        let tasks = fixture["tasks"].as_array().expect("tasks");
        assert!(!tasks.is_empty());

        for task in tasks {
            assert!(
                task["token_budget"]
                    .as_u64()
                    .is_some_and(|budget| budget > 0),
                "task requires a positive source budget"
            );
            assert!(
                task["minimum_fragments_per_focus_path"]
                    .as_u64()
                    .is_some_and(|minimum| minimum > 0),
                "task requires a positive per-focus minimum"
            );
            let focus_paths = task["focus_paths"].as_array().expect("focus paths");
            assert!(focus_paths.len() >= 3);
            for path in focus_paths {
                let path = path.as_str().expect("focus path");
                validate_benchmark_path(path).expect("valid focus path");
                assert!(
                    Path::new(env!("CARGO_MANIFEST_DIR")).join(path).exists(),
                    "focus path must exist: {path}"
                );
            }

            let variants = task["variants"].as_array().expect("task variants");
            assert_eq!(variants.len(), 2);
            let styles = variants
                .iter()
                .map(|variant| variant["style"].as_str().expect("variant style"))
                .collect::<BTreeSet<_>>();
            assert_eq!(
                styles,
                BTreeSet::from(["keyword_heavy", "natural_language"])
            );
            let prompts = variants
                .iter()
                .map(|variant| variant["task"].as_str().expect("variant task").trim())
                .collect::<BTreeSet<_>>();
            assert_eq!(prompts.len(), 2);
            assert!(prompts.iter().all(|prompt| !prompt.is_empty()));

            let concepts = task["concepts"].as_array().expect("task concepts");
            assert_eq!(concepts.len(), 3);
            let roles = concepts
                .iter()
                .map(|concept| concept["role"].as_str().expect("concept role"))
                .collect::<BTreeSet<_>>();
            assert_eq!(
                roles,
                BTreeSet::from([
                    "architecture_documentation",
                    "behavioral_test",
                    "owner_implementation"
                ])
            );
            for concept in concepts {
                let evidence = &concept["evidence"];
                let path = evidence["path"].as_str().expect("evidence path");
                validate_benchmark_path(path).expect("valid evidence path");
                assert!(
                    Path::new(env!("CARGO_MANIFEST_DIR")).join(path).exists(),
                    "evidence path must exist: {path}"
                );
                assert!(
                    matches!(
                        evidence["target"]["kind"].as_str(),
                        Some("symbol" | "heading")
                    ),
                    "evidence target must be a symbol or heading"
                );
                assert!(
                    evidence["target"]["name"]
                        .as_str()
                        .is_some_and(|name| !name.trim().is_empty()),
                    "evidence target requires a name"
                );
            }
        }
    }

    #[test]
    fn context_concept_labels_reject_an_incomplete_anchor_partition() {
        let source_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("benchmarks/validation.json");
        let source_json = fs::read_to_string(&source_path).expect("read source manifest");
        let source: Manifest = serde_json::from_str(&source_json).expect("parse source manifest");
        let mut labels: serde_json::Value = serde_json::from_str(
            &fs::read_to_string(
                Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("benchmarks/context_concept_coverage.json"),
            )
            .expect("read labels"),
        )
        .expect("parse labels");
        labels["tasks"][0]["concepts"][0]["evidence"][0]["line_anchors"]
            .as_array_mut()
            .expect("anchor array")
            .pop();
        let temporary = tempfile::NamedTempFile::new().expect("temporary labels");
        fs::write(
            temporary.path(),
            serde_json::to_vec(&labels).expect("serialize labels"),
        )
        .expect("write labels");

        assert!(
            load_concept_labels(
                temporary.path(),
                &source_path,
                &source,
                &blake3::hash(source_json.as_bytes()).to_hex().to_string(),
            )
            .expect_err("reject incomplete partition")
            .to_string()
            .contains("partition every source-manifest anchor")
        );
    }

    #[test]
    fn concept_coverage_distinguishes_generation_from_selection() {
        let coverage = evaluate_concept_coverage(
            &concept_task_labels(),
            &[
                candidate("src/lib.rs", 8, 12),
                candidate("tests/lib.rs", 18, 22),
            ],
            &[fragment("src/lib.rs", 8, 12)],
        )
        .expect("evaluate coverage");

        assert_eq!(coverage.concepts, 2);
        assert_eq!(coverage.candidate_concepts_found, 2);
        assert_eq!(coverage.selected_concepts_found, 1);
        assert_eq!(coverage.candidate_concept_recall, 1.0);
        assert_eq!(coverage.selected_concept_recall, 0.5);
        assert_eq!(coverage.selection_retention, Some(0.5));
        assert_eq!(
            coverage
                .evidence
                .iter()
                .find(|concept| concept.id == "regression")
                .expect("regression concept")
                .candidate_anchors_found,
            vec![MatchedAnchor {
                path: "tests/lib.rs".into(),
                line: 20
            }]
        );
    }

    #[test]
    fn concept_coverage_rejects_selected_evidence_missing_from_diagnostics() {
        assert!(
            evaluate_concept_coverage(
                &concept_task_labels(),
                &[candidate("tests/lib.rs", 18, 22)],
                &[fragment("src/lib.rs", 8, 12)],
            )
            .expect_err("selected evidence must have a candidate")
            .to_string()
            .contains("absent from candidate diagnostics")
        );
    }
}
