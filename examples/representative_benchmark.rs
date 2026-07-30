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
    SearchHit, SearchMode, SearchRequest, WorkflowEvidence, parser, services::Services, tokens,
};
use serde::{Deserialize, Serialize};

const HISTORY_LANE_MAX_COMMITS: usize = 256;
const HISTORY_LANE_MAX_OUTPUT_LINES: usize = 4_096;
const HISTORY_LANE_MAX_LINE_BYTES: usize = 32 * 1024;
const HISTORY_LANE_MAX_PATHS: usize = 4;
const AST_LANE_MAX_TRACE_BYTES: usize = 16 * 1024;
const AST_LANE_MAX_LANGUAGES: usize = 2;
const AST_LANE_MAX_TERMS: usize = 8;
const AST_LANE_MAX_RESULTS_PER_TERM: usize = 16;
const AST_LANE_MAX_TOKENS_PER_TERM: usize = 1_024;
const AST_LANE_MAX_PATHS: usize = 2;
const AST_LANE_V2_MAX_OWNER_TERMS: usize = 4;
const AST_LANE_V2_MAX_NAMED_ARGUMENT_TERMS: usize = 4;
const AST_LANE_V2_MAX_OWNER_HITS_PER_PATH: usize = 16;
const AST_LANE_V2_MAX_OWNER_EVIDENCE_TOKENS: usize = 128;
const ORIENTATION_CAPSULE_MAX_PATHS: usize = 1;
const ORIENTATION_CAPSULE_MAX_TERMS: usize = 4;
const ORIENTATION_CAPSULE_MAX_DEFINITIONS: usize = 4;
const ORIENTATION_CAPSULE_MAX_TOKENS: usize = 128;

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
    /// Add a bounded Git-history path lane derived from workflow-evidence symbols.
    #[arg(
        long,
        requires = "workflow_evidence",
        conflicts_with_all = ["ast_structural_lane", "ast_structural_lane_v2"]
    )]
    history_lane: bool,
    /// Add bounded AST-derived structural path candidates from failure traces.
    #[arg(
        long,
        requires = "workflow_evidence",
        conflicts_with_all = ["history_lane", "ast_structural_lane_v2"]
    )]
    ast_structural_lane: bool,
    /// Experiment with corroborated AST owner ranking and one bounded owner excerpt.
    #[arg(
        long,
        requires = "workflow_evidence",
        conflicts_with_all = ["history_lane", "ast_structural_lane"]
    )]
    ast_structural_lane_v2: bool,
    /// Emit a bounded structural owner-routing capsule without changing context selection.
    #[arg(
        long,
        requires = "ast_structural_lane",
        conflicts_with = "ast_structural_lane_v2"
    )]
    orientation_capsule: bool,
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
    task_family: Option<String>,
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
    history_lane_enabled: bool,
    ast_structural_lane_enabled: bool,
    ast_structural_lane_v2_enabled: bool,
    orientation_capsule_enabled: bool,
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
    task_families: BTreeMap<String, AggregateReport>,
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
    warm_context_median_ms: f64,
    warm_context_p95_ms: f64,
    cold_index_ms: f64,
    database_bytes: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    process_rss_bytes: Option<u64>,
    #[serde(skip)]
    warm_context_ms_samples: Vec<f64>,
    source_savings_against_oracle_fraction: f64,
    total_json_savings_against_scripted_fraction: f64,
    known_fragments_resent: usize,
    dead_end_fragments: usize,
    dead_end_source_tokens: usize,
    ast_owner_reservations: usize,
    ast_owner_relevant_reservations: usize,
    ast_owner_reservation_source_tokens: usize,
    ast_owner_reservation_serialized_tokens: usize,
    orientation_capsule_paths: usize,
    orientation_capsule_relevant_paths: usize,
    orientation_capsule_path_recall: Option<f64>,
    orientation_capsule_tokens: usize,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    process_rss_bytes: Option<u64>,
    tasks: Vec<TaskReport>,
}

#[derive(Debug, Serialize)]
struct TaskReport {
    id: String,
    prompt: String,
    task_family: String,
    languages: Vec<String>,
    task_shapes: Vec<String>,
    token_budget: usize,
    workflow_evidence: WorkflowEvidenceCounts,
    history_lane: HistoryLaneReport,
    ast_structural_lane: AstStructuralLaneReport,
    orientation_capsule: OrientationCapsuleReport,
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
    known_owner_reservations_resent: usize,
    owner_known_hash_omission_visible: bool,
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

#[derive(Debug, Default, Serialize)]
struct HistoryLaneReport {
    enabled: bool,
    available: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    unavailable_reason: Option<&'static str>,
    revision: String,
    symbols: usize,
    subprocesses: usize,
    commits_examined: usize,
    commit_window_complete: bool,
    matching_commits: usize,
    output_truncated: bool,
    candidate_paths: Vec<String>,
    relevant_candidate_paths: usize,
}

#[derive(Debug, Default, Serialize)]
struct AstStructuralLaneReport {
    enabled: bool,
    version: u8,
    trace_bytes_examined: usize,
    languages_attempted: Vec<String>,
    structurally_complete_languages: usize,
    terms: Vec<String>,
    owner_terms: Vec<String>,
    named_argument_terms: Vec<String>,
    searches: usize,
    auxiliary_searches: usize,
    structural_hits: usize,
    corroborating_hits: usize,
    candidate_paths: Vec<String>,
    relevant_candidate_paths: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    owner_reservation: Option<AstOwnerReservationReport>,
}

#[derive(Clone, Debug, Serialize)]
struct AstOwnerReservationReport {
    path: String,
    start_line: usize,
    end_line: usize,
    symbol: String,
    matched_term: String,
    excerpt: String,
    source_tokens: usize,
    serialized_tokens: usize,
    content_hash: String,
    relevant: bool,
}

#[derive(Debug, Default, Serialize)]
struct OrientationCapsuleReport {
    enabled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    unavailable_reason: Option<&'static str>,
    entries: Vec<OrientationCapsuleEntry>,
    capsule_tokens: usize,
    relevant_paths: usize,
}

#[derive(Clone, Debug, Serialize)]
struct OrientationCapsuleEntry {
    path: String,
    matched_terms: Vec<String>,
    definitions: Vec<String>,
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

struct RunTaskOptions<'a> {
    rg_max_lines_per_query: usize,
    concept_labels: Option<&'a ConceptTaskLabels>,
    workflow_evidence_enabled: bool,
    history_lane_enabled: bool,
    ast_structural_lane_enabled: bool,
    ast_structural_lane_v2_enabled: bool,
    orientation_capsule_enabled: bool,
    base_revision: &'a str,
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
                "workflow_evidence_enabled": args.workflow_evidence,
                "history_lane_enabled": args.history_lane,
                "ast_structural_lane_enabled": args.ast_structural_lane,
                "ast_structural_lane_v2_enabled": args.ast_structural_lane_v2,
                "orientation_capsule_enabled": args.orientation_capsule,
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
    let mut task_families = BTreeMap::<String, AggregateReport>::new();
    for corpus in manifest.corpora {
        let root = args.repos_root.join(&corpus.directory);
        verify_revision(&root, &corpus.base_revision)?;
        let database_path = scratch.path().join(format!("{}.sqlite", corpus.name));
        let config = Config::discover(&root, Some(database_path.clone()))?;
        let services = Services::open(config)?;

        let started = Instant::now();
        let indexed = services.index(true).await?;
        let cold_index_ms = elapsed_ms(started);
        let mut tasks = Vec::new();
        for task in corpus.tasks {
            let labels = concept_labels
                .as_mut()
                .and_then(|loaded| loaded.tasks.remove(&task.id));
            let report = run_task(
                &root,
                &services,
                task,
                RunTaskOptions {
                    rg_max_lines_per_query: manifest.rg_max_lines_per_query,
                    concept_labels: labels.as_ref(),
                    workflow_evidence_enabled: args.workflow_evidence,
                    history_lane_enabled: args.history_lane,
                    ast_structural_lane_enabled: args.ast_structural_lane,
                    ast_structural_lane_v2_enabled: args.ast_structural_lane_v2,
                    orientation_capsule_enabled: args.orientation_capsule,
                    base_revision: &corpus.base_revision,
                },
            )
            .await?;
            accumulate(&mut aggregate, &report);
            accumulate(
                task_families.entry(report.task_family.clone()).or_default(),
                &report,
            );
            tasks.push(report);
        }
        let status = services.status().await?;
        let database_bytes = database_footprint(&database_path)?;
        aggregate.cold_index_ms += cold_index_ms;
        aggregate.database_bytes = aggregate.database_bytes.saturating_add(database_bytes);
        aggregate.process_rss_bytes = match (aggregate.process_rss_bytes, status.process_rss_bytes)
        {
            (Some(left), Some(right)) => Some(left.max(right)),
            (value @ Some(_), None) | (None, value @ Some(_)) => value,
            (None, None) => None,
        };
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
            database_bytes,
            process_rss_bytes: status.process_rss_bytes,
            tasks,
        });
    }
    aggregate.corpus_count = corpora.len();
    finalize_aggregate(&mut aggregate);
    for family in task_families.values_mut() {
        finalize_aggregate(family);
    }
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
        history_lane_enabled: args.history_lane,
        ast_structural_lane_enabled: args.ast_structural_lane,
        ast_structural_lane_v2_enabled: args.ast_structural_lane_v2,
        orientation_capsule_enabled: args.orientation_capsule,
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
        task_families,
        corpora,
        limitations: benchmark_limitations(
            &manifest.dataset_kind,
            args.consumed_diagnostic,
            args.concept_labels.is_some(),
            args.history_lane,
            args.ast_structural_lane,
            args.ast_structural_lane_v2,
            args.orientation_capsule,
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
    if manifest.schema_version >= 5 {
        for task in manifest.corpora.iter().flat_map(|corpus| &corpus.tasks) {
            if task
                .task_family
                .as_deref()
                .is_none_or(|family| family.trim().is_empty())
            {
                return Err(
                    format!("manifest schema v5+ task {} requires task_family", task.id).into(),
                );
            }
        }
    }
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
    history_lane: bool,
    ast_structural_lane: bool,
    ast_structural_lane_v2: bool,
    orientation_capsule: bool,
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
            "The validation tasks are retrieval development evidence, not a statistically powered product claim.",
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
    if history_lane {
        limitations.push(
            "The Git-history lane examines at most 256 pinned ancestors and four current paths; absence is not evidence that history has no useful path.",
        );
        limitations.push(
            "Pickaxe path matches are a retrieval proxy, not proof that historical changes explain the current failure.",
        );
    }
    if ast_structural_lane {
        limitations.push(
            "The AST structural lane parses at most 16 KiB of observed failure traces, retains eight structural terms, and focuses at most two paths; omitted syntax is not evidence that no structural route exists.",
        );
        limitations.push(
            "Tolerant parsing of terminal output can recover incomplete code fragments; a structural hit is a path-discovery proxy, not proof that the file owns the failure.",
        );
    }
    if ast_structural_lane_v2 {
        limitations.push(
            "The AST structural v2 experiment parses at most 16 KiB of failure traces, searches eight structural terms plus four owner and four named-argument terms, and retains one owner excerpt of at most 128 exact source tokens.",
        );
        limitations.push(
            "Owner corroboration and the reserved excerpt are retrieval proxies scored after discovery; two local tasks and synthetic fixtures do not establish generalization or end-to-end task success.",
        );
    }
    if orientation_capsule {
        limitations.push(
            "The orientation capsule is a bounded routing artifact, not selected source evidence or proof that a model will perform the required follow-up read.",
        );
        limitations.push(
            "Capsule path relevance uses labels only after discovery and does not establish end-to-end task success or downstream token savings.",
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

#[derive(Debug, Default)]
struct HistoryPathStats {
    matching_commits: usize,
    first_seen: usize,
}

struct BoundedGitLines {
    lines: Vec<String>,
    truncated: bool,
}

#[derive(Debug, Default)]
struct AstPathStats {
    terms: BTreeSet<String>,
    definitions: BTreeSet<String>,
    owner_terms: BTreeSet<String>,
    named_argument_terms: BTreeSet<String>,
    structural_hits: usize,
    corroborating_hits: usize,
    best_score: f64,
    owner_hits: Vec<AstOwnerHit>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AstStructuralLaneVersion {
    V1,
    V2,
}

impl AstStructuralLaneVersion {
    const fn number(self) -> u8 {
        match self {
            Self::V1 => 1,
            Self::V2 => 2,
        }
    }
}

#[derive(Debug)]
struct AstOwnerHit {
    start_line: usize,
    end_line: usize,
    excerpt: String,
    symbol: String,
    matched_term: String,
    normalized_score: f64,
    corroborating_owner_terms: BTreeSet<String>,
    corroborating_named_argument_terms: BTreeSet<String>,
}

#[derive(Debug)]
struct AstOwnerEvidence {
    report: AstOwnerReservationReport,
}

#[derive(Serialize)]
struct AstOwnerEvidenceWire<'a> {
    path: &'a str,
    start_line: usize,
    end_line: usize,
    symbol: &'a str,
    matched_term: &'a str,
    excerpt: &'a str,
    content_hash: &'a str,
}

#[derive(Debug, Default)]
struct AstTraceSignals {
    member_terms: Vec<String>,
    owner_terms: Vec<String>,
    named_argument_terms: Vec<String>,
}

#[derive(Debug, Default)]
struct AstQueryTerms {
    structural: Vec<String>,
    owners: Vec<String>,
    named_arguments: Vec<String>,
}

async fn discover_ast_structural_lane(
    services: &Services,
    languages: &[String],
    failure_traces: &[String],
    orientation_capsule_enabled: bool,
    version: AstStructuralLaneVersion,
) -> Result<
    (
        AstStructuralLaneReport,
        OrientationCapsuleReport,
        Option<AstOwnerEvidence>,
    ),
    Box<dyn Error>,
> {
    let trace = utf8_tail(&failure_traces.join("\n"), AST_LANE_MAX_TRACE_BYTES);
    let mut report = AstStructuralLaneReport {
        enabled: true,
        version: version.number(),
        trace_bytes_examined: trace.len(),
        ..AstStructuralLaneReport::default()
    };
    if trace.is_empty() {
        let capsule = if orientation_capsule_enabled {
            OrientationCapsuleReport {
                enabled: true,
                unavailable_reason: Some("no_failure_trace"),
                ..OrientationCapsuleReport::default()
            }
        } else {
            OrientationCapsuleReport::default()
        };
        return Ok((report, capsule, None));
    }

    let terms = collect_ast_structural_terms(languages, &trace, version, &mut report)?;
    report.terms = terms.structural.clone();
    report.owner_terms = terms.owners.clone();
    report.named_argument_terms = terms.named_arguments.clone();

    let mut paths =
        search_ast_structural_definitions(services, &terms.structural, &mut report).await?;
    if version == AstStructuralLaneVersion::V2 {
        corroborate_ast_paths(
            services,
            &mut paths,
            &terms.owners,
            &terms.named_arguments,
            &mut report,
        )
        .await?;
    }

    let ranked = rank_ast_structural_paths(paths, &report.languages_attempted, version);
    let capsule = build_orientation_capsule(&ranked, orientation_capsule_enabled)?;
    report.candidate_paths = ranked
        .iter()
        .map(|(path, _)| path)
        .take(AST_LANE_MAX_PATHS)
        .cloned()
        .collect();
    let owner_evidence = if version == AstStructuralLaneVersion::V2 {
        build_ast_owner_evidence(&ranked)?
    } else {
        None
    };
    report.owner_reservation = owner_evidence
        .as_ref()
        .map(|evidence| evidence.report.clone());
    Ok((report, capsule, owner_evidence))
}

fn collect_ast_structural_terms(
    languages: &[String],
    trace: &str,
    version: AstStructuralLaneVersion,
    report: &mut AstStructuralLaneReport,
) -> Result<AstQueryTerms, Box<dyn Error>> {
    let mut terms = Vec::new();
    let mut seen_terms = HashSet::new();
    let mut owner_terms = Vec::new();
    let mut seen_owner_terms = HashSet::new();
    let mut named_argument_terms = Vec::new();
    let mut seen_named_argument_terms = HashSet::new();
    for language in languages
        .iter()
        .map(|language| language.to_ascii_lowercase())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .take(AST_LANE_MAX_LANGUAGES)
    {
        let parsed = parser::parse_language(&language, trace)?;
        report.languages_attempted.push(language.clone());
        report.structurally_complete_languages += usize::from(parsed.structurally_complete);
        for reference in parsed.references {
            if let Some(term) = normalize_ast_search_term(&reference.name) {
                retain_bounded_term(&mut terms, &mut seen_terms, term, AST_LANE_MAX_TERMS);
                if terms.len() == AST_LANE_MAX_TERMS {
                    break;
                }
            }
        }
        if version == AstStructuralLaneVersion::V2 {
            retain_ast_v2_trace_signals(
                extract_ast_v2_trace_signals(&language, trace),
                &mut terms,
                &mut seen_terms,
                &mut owner_terms,
                &mut seen_owner_terms,
                &mut named_argument_terms,
                &mut seen_named_argument_terms,
            );
        }
        terms.truncate(AST_LANE_MAX_TERMS);
        if version == AstStructuralLaneVersion::V1 && terms.len() == AST_LANE_MAX_TERMS {
            break;
        }
    }
    Ok(AstQueryTerms {
        structural: terms,
        owners: owner_terms,
        named_arguments: named_argument_terms,
    })
}

fn retain_ast_v2_trace_signals(
    signals: AstTraceSignals,
    terms: &mut Vec<String>,
    seen_terms: &mut HashSet<String>,
    owner_terms: &mut Vec<String>,
    seen_owner_terms: &mut HashSet<String>,
    named_argument_terms: &mut Vec<String>,
    seen_named_argument_terms: &mut HashSet<String>,
) {
    for term in signals.member_terms {
        retain_bounded_term(terms, seen_terms, term, AST_LANE_MAX_TERMS);
    }
    for term in signals.owner_terms {
        retain_bounded_term(
            owner_terms,
            seen_owner_terms,
            term,
            AST_LANE_V2_MAX_OWNER_TERMS,
        );
    }
    for term in signals.named_argument_terms {
        retain_bounded_term(
            named_argument_terms,
            seen_named_argument_terms,
            term,
            AST_LANE_V2_MAX_NAMED_ARGUMENT_TERMS,
        );
    }
}

async fn search_ast_structural_definitions(
    services: &Services,
    terms: &[String],
    report: &mut AstStructuralLaneReport,
) -> Result<BTreeMap<String, AstPathStats>, Box<dyn Error>> {
    let mut paths = BTreeMap::<String, AstPathStats>::new();
    for term in terms {
        let response = services
            .search(ast_search_request(
                term,
                SearchMode::Symbol,
                Vec::new(),
                false,
            ))
            .await?;
        report.searches += 1;
        for hit in response.hits {
            if hit.match_kind != "symbol" && !hit.match_kinds.iter().any(|kind| kind == "symbol") {
                continue;
            }
            report.structural_hits += 1;
            record_ast_structural_hit(&mut paths, term, hit);
        }
    }
    Ok(paths)
}

fn record_ast_structural_hit(
    paths: &mut BTreeMap<String, AstPathStats>,
    term: &str,
    hit: SearchHit,
) {
    let stats = paths.entry(hit.path).or_default();
    stats.terms.insert(term.to_owned());
    let symbol = hit.symbol.unwrap_or_else(|| term.to_owned());
    stats.definitions.insert(symbol.clone());
    let owner_hit = AstOwnerHit {
        start_line: hit.start_line,
        end_line: hit.end_line,
        excerpt: hit.excerpt,
        symbol,
        matched_term: term.to_owned(),
        normalized_score: hit.normalized_score,
        corroborating_owner_terms: BTreeSet::new(),
        corroborating_named_argument_terms: BTreeSet::new(),
    };
    if ast_owner_hit_exact(&owner_hit) {
        retain_best_ast_owner_hit(stats, owner_hit);
    }
    stats.structural_hits += 1;
    stats.best_score = stats.best_score.max(hit.normalized_score);
}

fn retain_best_ast_owner_hit(stats: &mut AstPathStats, owner_hit: AstOwnerHit) {
    if let Some(current) = stats
        .owner_hits
        .iter_mut()
        .find(|current| current.matched_term == owner_hit.matched_term)
    {
        if ast_owner_hit_precedes(&owner_hit, current) {
            *current = owner_hit;
        }
    } else if stats.owner_hits.len() < AST_LANE_V2_MAX_OWNER_HITS_PER_PATH {
        stats.owner_hits.push(owner_hit);
    }
}

async fn corroborate_ast_paths(
    services: &Services,
    paths: &mut BTreeMap<String, AstPathStats>,
    owner_terms: &[String],
    named_argument_terms: &[String],
    report: &mut AstStructuralLaneReport,
) -> Result<(), Box<dyn Error>> {
    for (term, role) in owner_terms
        .iter()
        .map(|term| (term, AstAuxiliaryTermRole::Owner))
        .chain(
            named_argument_terms
                .iter()
                .map(|term| (term, AstAuxiliaryTermRole::NamedArgument)),
        )
    {
        let response = services
            .search(ast_search_request(
                term,
                SearchMode::Auto,
                paths.keys().cloned().collect(),
                true,
            ))
            .await?;
        report.searches += 1;
        report.auxiliary_searches += 1;
        for hit in response.hits {
            let Some(stats) = paths.get_mut(&hit.path) else {
                continue;
            };
            if record_ast_corroborating_hit(stats, term, role, &hit) {
                report.corroborating_hits += 1;
            }
        }
    }
    corroborate_ast_owner_excerpts(paths, named_argument_terms);
    Ok(())
}

fn record_ast_corroborating_hit(
    stats: &mut AstPathStats,
    term: &str,
    role: AstAuxiliaryTermRole,
    hit: &SearchHit,
) -> bool {
    if hit.symbol.is_none()
        && hit.enclosing_symbol.is_none()
        && hit.match_kind != "symbol"
        && !hit.match_kinds.iter().any(|kind| kind == "symbol")
    {
        return false;
    }
    let mut owner_local = false;
    if let Some(corroborating_symbol) = hit.enclosing_symbol.as_deref().or(hit.symbol.as_deref()) {
        for owner_hit in &mut stats.owner_hits {
            let range_cooccurs =
                owner_hit.start_line <= hit.start_line && owner_hit.end_line >= hit.end_line;
            if range_cooccurs || ast_symbols_cooccur(&owner_hit.symbol, corroborating_symbol) {
                record_ast_owner_corroboration(owner_hit, term, role);
                owner_local = true;
            }
        }
    }
    if !owner_local {
        return false;
    }
    match role {
        AstAuxiliaryTermRole::Owner => {
            stats.owner_terms.insert(term.to_owned());
        }
        AstAuxiliaryTermRole::NamedArgument => {
            stats.named_argument_terms.insert(term.to_owned());
        }
    }
    stats.corroborating_hits += 1;
    true
}

fn record_ast_owner_corroboration(
    owner_hit: &mut AstOwnerHit,
    term: &str,
    role: AstAuxiliaryTermRole,
) {
    match role {
        AstAuxiliaryTermRole::Owner => {
            owner_hit.corroborating_owner_terms.insert(term.to_owned());
        }
        AstAuxiliaryTermRole::NamedArgument => {
            owner_hit
                .corroborating_named_argument_terms
                .insert(term.to_owned());
        }
    }
}

fn corroborate_ast_owner_excerpts(
    paths: &mut BTreeMap<String, AstPathStats>,
    named_argument_terms: &[String],
) {
    for stats in paths.values_mut() {
        let mut owner_local_terms = BTreeSet::new();
        for owner_hit in &mut stats.owner_hits {
            for term in named_argument_terms {
                if ast_excerpt_contains_term(&owner_hit.excerpt, term) {
                    owner_hit
                        .corroborating_named_argument_terms
                        .insert(term.clone());
                    owner_local_terms.insert(term.clone());
                }
            }
        }
        stats.named_argument_terms.extend(owner_local_terms);
    }
}

fn ast_search_request(
    term: &str,
    mode: SearchMode,
    focus_paths: Vec<String>,
    prefer_structural: bool,
) -> SearchRequest {
    SearchRequest {
        query: term.to_owned(),
        mode,
        include_paths: Vec::new(),
        exclude_paths: Vec::new(),
        focus_paths,
        max_results: Some(AST_LANE_MAX_RESULTS_PER_TERM),
        max_tokens: Some(AST_LANE_MAX_TOKENS_PER_TERM),
        context_lines: Some(0),
        case_sensitive: false,
        all_occurrences: false,
        prefer_structural,
        receipt_id: None,
        query_receipt: None,
        cursor: None,
    }
}

fn rank_ast_structural_paths(
    paths: BTreeMap<String, AstPathStats>,
    languages: &[String],
    version: AstStructuralLaneVersion,
) -> Vec<(String, AstPathStats)> {
    let mut ranked = paths.into_iter().collect::<Vec<_>>();
    let source_extensions = ast_source_extensions(languages);
    ranked.sort_by(|(left_path, left), (right_path, right)| {
        let v2_order = || {
            ast_path_matches_source_extensions(right_path, &source_extensions)
                .cmp(&ast_path_matches_source_extensions(
                    left_path,
                    &source_extensions,
                ))
                .then_with(|| {
                    right
                        .named_argument_terms
                        .len()
                        .cmp(&left.named_argument_terms.len())
                })
                .then_with(|| right.owner_terms.len().cmp(&left.owner_terms.len()))
        };
        let order = if version == AstStructuralLaneVersion::V2 {
            v2_order()
        } else {
            std::cmp::Ordering::Equal
        };
        order
            .then_with(|| right.terms.len().cmp(&left.terms.len()))
            .then_with(|| right.structural_hits.cmp(&left.structural_hits))
            .then_with(|| right.best_score.total_cmp(&left.best_score))
            .then_with(|| left_path.cmp(right_path))
    });
    ranked
}

#[derive(Clone, Copy)]
enum AstAuxiliaryTermRole {
    Owner,
    NamedArgument,
}

fn ast_owner_hit_precedes(candidate: &AstOwnerHit, current: &AstOwnerHit) -> bool {
    candidate
        .corroborating_named_argument_terms
        .len()
        .cmp(&current.corroborating_named_argument_terms.len())
        .then_with(|| {
            ast_term_specificity(&candidate.matched_term)
                .cmp(&ast_term_specificity(&current.matched_term))
        })
        .then_with(|| {
            candidate
                .corroborating_owner_terms
                .len()
                .cmp(&current.corroborating_owner_terms.len())
        })
        .then_with(|| {
            candidate
                .normalized_score
                .total_cmp(&current.normalized_score)
        })
        .then_with(|| current.start_line.cmp(&candidate.start_line))
        .then_with(|| current.symbol.cmp(&candidate.symbol))
        .is_gt()
}

fn ast_owner_hit_exact(hit: &AstOwnerHit) -> bool {
    hit.symbol
        .rsplit(['.', ':'])
        .find(|component| !component.is_empty())
        .is_some_and(|symbol| symbol.eq_ignore_ascii_case(&hit.matched_term))
}

fn ast_term_specificity(term: &str) -> (usize, usize) {
    (term.matches('_').count(), term.len())
}

fn ast_symbols_cooccur(owner: &str, enclosing: &str) -> bool {
    let owner = owner
        .rsplit(['.', ':'])
        .find(|component| !component.is_empty())
        .unwrap_or(owner);
    enclosing
        .split(['.', ':'])
        .filter(|component| !component.is_empty())
        .any(|component| component.eq_ignore_ascii_case(owner))
}

fn ast_excerpt_contains_term(excerpt: &str, term: &str) -> bool {
    excerpt
        .split(|character: char| !character.is_alphanumeric() && character != '_')
        .any(|component| component.eq_ignore_ascii_case(term))
}

fn ast_source_extensions(languages: &[String]) -> BTreeSet<&'static str> {
    languages
        .iter()
        .filter_map(|language| match language.as_str() {
            "javascript" | "jsx" => Some("js"),
            "python" => Some("py"),
            "ruby" => Some("rb"),
            "rust" => Some("rs"),
            "tsx" => Some("tsx"),
            "typescript" => Some("ts"),
            _ => None,
        })
        .collect()
}

fn ast_path_matches_source_extensions(path: &str, extensions: &BTreeSet<&str>) -> bool {
    Path::new(path)
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extensions.contains(extension))
}

fn build_ast_owner_evidence(
    ranked: &[(String, AstPathStats)],
) -> Result<Option<AstOwnerEvidence>, serde_json::Error> {
    let Some((path, hit)) = ranked
        .iter()
        .take(AST_LANE_MAX_PATHS)
        .find_map(|(path, stats)| {
            stats
                .owner_hits
                .iter()
                .filter(|hit| ast_owner_hit_exact(hit))
                .reduce(|current, candidate| {
                    if ast_owner_hit_precedes(candidate, current) {
                        candidate
                    } else {
                        current
                    }
                })
                .map(|hit| (path, hit))
        })
    else {
        return Ok(None);
    };
    let (excerpt, source_tokens) =
        tokens::truncate(&hit.excerpt, AST_LANE_V2_MAX_OWNER_EVIDENCE_TOKENS);
    if excerpt.is_empty() {
        return Ok(None);
    }
    let end_line = hit
        .start_line
        .saturating_add(excerpt.lines().count().saturating_sub(1))
        .min(hit.end_line);
    let content_hash = blake3::hash(excerpt.as_bytes()).to_hex().to_string();
    let mut report = AstOwnerReservationReport {
        path: path.clone(),
        start_line: hit.start_line,
        end_line,
        symbol: hit.symbol.clone(),
        matched_term: hit.matched_term.clone(),
        excerpt: excerpt.to_owned(),
        source_tokens,
        serialized_tokens: 0,
        content_hash,
        relevant: false,
    };
    let wire = AstOwnerEvidenceWire {
        path: &report.path,
        start_line: report.start_line,
        end_line: report.end_line,
        symbol: &report.symbol,
        matched_term: &report.matched_term,
        excerpt,
        content_hash: &report.content_hash,
    };
    report.serialized_tokens = tokens::count(&serde_json::to_string(&wire)?);
    Ok(Some(AstOwnerEvidence { report }))
}

fn retain_bounded_term(
    terms: &mut Vec<String>,
    seen: &mut HashSet<String>,
    term: String,
    limit: usize,
) {
    if terms.len() < limit && seen.insert(term.clone()) {
        terms.push(term);
    }
}

fn extract_ast_v2_trace_signals(language: &str, trace: &str) -> AstTraceSignals {
    let mut signals = AstTraceSignals::default();
    let mut seen_members = HashSet::new();
    let mut seen_owners = HashSet::new();
    let mut seen_named = HashSet::new();
    let supports_named_equals = matches!(
        language,
        "python" | "javascript" | "typescript" | "tsx" | "ruby"
    );
    let supports_named_fields = matches!(
        language,
        "python" | "javascript" | "typescript" | "tsx" | "rust"
    );
    for line in trace.lines() {
        scan_ast_v2_trace_line(
            line,
            supports_named_equals,
            supports_named_fields,
            &mut signals,
            &mut seen_members,
            &mut seen_owners,
            &mut seen_named,
        );
    }
    signals
}

fn scan_ast_v2_trace_line(
    line: &str,
    supports_named_equals: bool,
    supports_named_fields: bool,
    signals: &mut AstTraceSignals,
    seen_members: &mut HashSet<String>,
    seen_owners: &mut HashSet<String>,
    seen_named: &mut HashSet<String>,
) {
    let bytes = line.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if matches!(bytes[index], b'\'' | b'"') {
            let quote = bytes[index];
            let start = index + 1;
            index = start;
            while index < bytes.len() {
                if bytes[index] == b'\\' {
                    index = (index + 2).min(bytes.len());
                    continue;
                }
                if bytes[index] == quote {
                    break;
                }
                index += 1;
            }
            if supports_named_fields
                && index < bytes.len()
                && is_ascii_identifier(&line[start..index])
                && next_significant_byte(bytes, index + 1).is_some_and(|(byte, _)| byte == b':')
                && let Some(term) = normalize_ast_named_argument_term(&line[start..index])
            {
                retain_bounded_term(
                    &mut signals.named_argument_terms,
                    seen_named,
                    term,
                    AST_LANE_V2_MAX_NAMED_ARGUMENT_TERMS,
                );
            }
            index = index.saturating_add(1);
            continue;
        }
        if !is_ascii_identifier_start(bytes[index]) {
            index += 1;
            continue;
        }
        let start = index;
        index += 1;
        while index < bytes.len() && is_ascii_identifier_continue(bytes[index]) {
            index += 1;
        }
        let identifier = &line[start..index];
        let previous = previous_significant_byte(bytes, start);
        let next = next_significant_byte(bytes, index);
        if let Some((delimiter, delimiter_index)) = next
            && delimiter_index == index
            && (delimiter == b'.'
                || (delimiter == b':' && bytes.get(delimiter_index + 1).copied() == Some(b':')))
        {
            let member_start =
                next_significant_byte(bytes, delimiter_index + usize::from(delimiter == b':') + 1)
                    .map(|(_, offset)| offset);
            if let Some(member_start) = member_start
                && bytes
                    .get(member_start)
                    .copied()
                    .is_some_and(is_ascii_identifier_start)
            {
                let mut member_end = member_start + 1;
                while member_end < bytes.len() && is_ascii_identifier_continue(bytes[member_end]) {
                    member_end += 1;
                }
                if let Some(member) = normalize_ast_v2_member_term(&line[member_start..member_end])
                {
                    if let Some(owner) = normalize_ast_auxiliary_term(identifier) {
                        retain_bounded_term(
                            &mut signals.owner_terms,
                            seen_owners,
                            owner,
                            AST_LANE_V2_MAX_OWNER_TERMS,
                        );
                    }
                    retain_bounded_term(
                        &mut signals.member_terms,
                        seen_members,
                        member,
                        AST_LANE_MAX_TERMS,
                    );
                }
            }
        }
        let is_named_equals = supports_named_equals
            && previous.is_some_and(|byte| matches!(byte, b'(' | b','))
            && next.is_some_and(|(byte, offset)| {
                byte == b'=' && bytes.get(offset + 1).copied() != Some(b'=')
            });
        let is_named_field = supports_named_fields
            && previous.is_some_and(|byte| matches!(byte, b'{' | b','))
            && next.is_some_and(|(byte, offset)| {
                byte == b':' && bytes.get(offset + 1).copied() != Some(b':')
            });
        if (is_named_equals || is_named_field)
            && let Some(term) = normalize_ast_named_argument_term(identifier)
        {
            retain_bounded_term(
                &mut signals.named_argument_terms,
                seen_named,
                term,
                AST_LANE_V2_MAX_NAMED_ARGUMENT_TERMS,
            );
        }
    }
}

fn previous_significant_byte(bytes: &[u8], index: usize) -> Option<u8> {
    bytes[..index]
        .iter()
        .rev()
        .copied()
        .find(|byte| !byte.is_ascii_whitespace())
}

fn next_significant_byte(bytes: &[u8], mut index: usize) -> Option<(u8, usize)> {
    while let Some(byte) = bytes.get(index).copied() {
        if !byte.is_ascii_whitespace() {
            return Some((byte, index));
        }
        index += 1;
    }
    None
}

fn is_ascii_identifier(value: &str) -> bool {
    value
        .as_bytes()
        .first()
        .copied()
        .is_some_and(is_ascii_identifier_start)
        && value
            .as_bytes()
            .iter()
            .copied()
            .all(is_ascii_identifier_continue)
}

const fn is_ascii_identifier_start(byte: u8) -> bool {
    byte == b'_' || byte.is_ascii_alphabetic()
}

const fn is_ascii_identifier_continue(byte: u8) -> bool {
    byte == b'_' || byte.is_ascii_alphanumeric()
}

fn normalize_ast_auxiliary_term(value: &str) -> Option<String> {
    normalize_ast_term_with_common_filter(value, false)
}

fn normalize_ast_v2_member_term(value: &str) -> Option<String> {
    let term = normalize_ast_search_term(value)?;
    (!term.starts_with("test_")
        && !matches!(
            term.as_str(),
            "bool" | "json" | "jsx" | "md" | "output" | "py" | "rs" | "str" | "toml" | "tsx"
        ))
    .then_some(term)
}

fn normalize_ast_named_argument_term(value: &str) -> Option<String> {
    normalize_ast_term_with_common_filter(value, true)
}

fn normalize_ast_term_with_common_filter(value: &str, allow_type: bool) -> Option<String> {
    let normalized = value
        .trim_matches(|character: char| !character.is_alphanumeric() && character != '_')
        .to_ascii_lowercase();
    if !(3..=128).contains(&normalized.len())
        || !normalized.chars().any(char::is_alphabetic)
        || matches!(
            normalized.as_str(),
            "assert"
                | "false"
                | "none"
                | "print"
                | "self"
                | "true"
                | "value"
                | "expected"
                | "output"
                | "result"
        )
        || (!allow_type && normalized == "type")
    {
        return None;
    }
    Some(normalized)
}

fn build_orientation_capsule(
    ranked: &[(String, AstPathStats)],
    enabled: bool,
) -> Result<OrientationCapsuleReport, serde_json::Error> {
    if !enabled {
        return Ok(OrientationCapsuleReport::default());
    }
    let Some((path, stats)) = ranked.first() else {
        return Ok(OrientationCapsuleReport {
            enabled: true,
            unavailable_reason: Some("no_structural_owner_candidates"),
            ..OrientationCapsuleReport::default()
        });
    };
    let mut entry = OrientationCapsuleEntry {
        path: path.clone(),
        matched_terms: stats
            .terms
            .iter()
            .take(ORIENTATION_CAPSULE_MAX_TERMS)
            .cloned()
            .collect(),
        definitions: stats
            .definitions
            .iter()
            .take(ORIENTATION_CAPSULE_MAX_DEFINITIONS)
            .cloned()
            .collect(),
    };
    loop {
        let entries = vec![entry.clone()]
            .into_iter()
            .take(ORIENTATION_CAPSULE_MAX_PATHS)
            .collect::<Vec<_>>();
        let capsule_tokens = tokens::count(&serde_json::to_string(&entries)?);
        if capsule_tokens <= ORIENTATION_CAPSULE_MAX_TOKENS {
            return Ok(OrientationCapsuleReport {
                enabled: true,
                unavailable_reason: None,
                entries,
                capsule_tokens,
                relevant_paths: 0,
            });
        }
        if entry.definitions.pop().is_some() || entry.matched_terms.pop().is_some() {
            continue;
        }
        return Ok(OrientationCapsuleReport {
            enabled: true,
            unavailable_reason: Some("capsule_exceeds_token_limit"),
            ..OrientationCapsuleReport::default()
        });
    }
}

fn normalize_ast_search_term(value: &str) -> Option<String> {
    let value = value
        .rsplit(['.', ':'])
        .find(|component| !component.is_empty())?
        .trim_matches(|character: char| !character.is_alphanumeric() && character != '_');
    let normalized = value.to_ascii_lowercase();
    if !(3..=128).contains(&normalized.len())
        || !normalized.chars().any(char::is_alphabetic)
        || matches!(
            normalized.as_str(),
            "assert" | "false" | "none" | "print" | "self" | "true" | "type" | "value"
        )
    {
        return None;
    }
    Some(normalized)
}

fn discover_history_lane(
    root: &Path,
    revision: &str,
    symbols: &[String],
) -> Result<HistoryLaneReport, Box<dyn Error>> {
    let mut report = HistoryLaneReport {
        enabled: true,
        revision: revision.to_owned(),
        symbols: symbols.len(),
        ..HistoryLaneReport::default()
    };
    if symbols.is_empty() {
        report.unavailable_reason = Some("no_workflow_symbols");
        return Ok(report);
    }
    let commits_output = Command::new("git")
        .args([
            "rev-list",
            &format!("--max-count={HISTORY_LANE_MAX_COMMITS}"),
            revision,
        ])
        .env("GIT_NO_LAZY_FETCH", "1")
        .current_dir(root)
        .output()?;
    report.subprocesses += 1;
    if !commits_output.status.success() {
        return Err(format!(
            "git rev-list failed in {}: {}",
            root.display(),
            String::from_utf8_lossy(&commits_output.stderr).trim()
        )
        .into());
    }
    let commits = String::from_utf8(commits_output.stdout)?
        .lines()
        .map(str::trim)
        .filter(|commit| !commit.is_empty())
        .map(str::to_owned)
        .collect::<Vec<_>>();
    if commits.len() > HISTORY_LANE_MAX_COMMITS
        || commits.iter().any(|commit| {
            commit.len() != 40 || !commit.bytes().all(|byte| byte.is_ascii_hexdigit())
        })
    {
        return Err("git rev-list returned an invalid bounded commit set".into());
    }
    report.commits_examined = commits.len();
    report.commit_window_complete = commits.len() < HISTORY_LANE_MAX_COMMITS;
    if commits.is_empty() {
        report.available = true;
        return Ok(report);
    }

    let regex = symbols
        .iter()
        .map(|symbol| escape_posix_extended(symbol))
        .collect::<Vec<_>>()
        .join("|");
    let mut args = vec![
        "-c".to_owned(),
        "core.quotePath=false".to_owned(),
        "log".to_owned(),
        "--no-walk=unsorted".to_owned(),
        "--extended-regexp".to_owned(),
        "--format=__LEANTOKEN_HISTORY_COMMIT__%H".to_owned(),
        "--name-only".to_owned(),
        "-G".to_owned(),
        regex,
    ];
    args.extend(commits);
    let output = match run_git_lines_bounded(root, &args) {
        Ok(output) => output,
        Err(error)
            if error.to_string().contains("promisor remote")
                || error.to_string().contains("lazy fetching disabled") =>
        {
            report.subprocesses += 1;
            report.unavailable_reason = Some("history_objects_unavailable_without_lazy_fetch");
            return Ok(report);
        }
        Err(error) => return Err(error),
    };
    report.subprocesses += 1;
    report.available = true;
    report.output_truncated = output.truncated;

    let mut paths = BTreeMap::<String, HistoryPathStats>::new();
    let mut matching_commit = None;
    let mut order = 0usize;
    for line in output.lines {
        if let Some(commit) = line.strip_prefix("__LEANTOKEN_HISTORY_COMMIT__") {
            if commit.len() == 40 && commit.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                report.matching_commits += 1;
                matching_commit = Some(commit.to_owned());
            } else {
                matching_commit = None;
            }
            continue;
        }
        if line.is_empty() || matching_commit.is_none() || validate_benchmark_path(&line).is_err() {
            continue;
        }
        if !root.join(&line).is_file() {
            continue;
        }
        let entry = paths.entry(line).or_insert_with(|| HistoryPathStats {
            matching_commits: 0,
            first_seen: order,
        });
        entry.matching_commits = entry.matching_commits.saturating_add(1);
        order = order.saturating_add(1);
    }
    let mut ranked = paths.into_iter().collect::<Vec<_>>();
    ranked.sort_by(|(left_path, left), (right_path, right)| {
        right
            .matching_commits
            .cmp(&left.matching_commits)
            .then_with(|| left.first_seen.cmp(&right.first_seen))
            .then_with(|| left_path.cmp(right_path))
    });
    report.candidate_paths = ranked
        .into_iter()
        .take(HISTORY_LANE_MAX_PATHS)
        .map(|(path, _)| path)
        .collect();
    Ok(report)
}

fn escape_posix_extended(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        if matches!(
            character,
            '.' | '[' | ']' | '\\' | '*' | '^' | '$' | '+' | '?' | '{' | '}' | '|' | '(' | ')'
        ) {
            escaped.push('\\');
        }
        escaped.push(character);
    }
    escaped
}

fn run_git_lines_bounded(root: &Path, args: &[String]) -> Result<BoundedGitLines, Box<dyn Error>> {
    let mut child = Command::new("git")
        .args(args)
        .current_dir(root)
        .env("GIT_NO_LAZY_FETCH", "1")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let stdout = child
        .stdout
        .take()
        .ok_or("git history stdout unavailable")?;
    let mut reader = BufReader::new(stdout);
    let mut lines = Vec::new();
    let mut line = String::new();
    let mut truncated = false;
    loop {
        line.clear();
        if reader.read_line(&mut line)? == 0 {
            break;
        }
        if lines.len() == HISTORY_LANE_MAX_OUTPUT_LINES || line.len() > HISTORY_LANE_MAX_LINE_BYTES
        {
            truncated = true;
            let _ = child.kill();
            break;
        }
        lines.push(line.trim_end_matches(['\r', '\n']).to_owned());
    }
    drop(reader);
    let output = child.wait_with_output()?;
    if !truncated && !output.status.success() {
        return Err(format!(
            "git history pickaxe failed in {}: {}",
            root.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        )
        .into());
    }
    Ok(BoundedGitLines { lines, truncated })
}

async fn run_task(
    root: &Path,
    services: &Services,
    task: TaskSpec,
    options: RunTaskOptions<'_>,
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
        .map(|query| run_rg(root, query, options.rg_max_lines_per_query))
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

    let mut request = ContextRequest {
        task: task.prompt.clone(),
        token_budget: task.token_budget,
        include_paths: Vec::new(),
        must_include_paths: Vec::new(),
        must_include_symbols: Vec::new(),
        required_evidence: Vec::new(),
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
    let workflow_evidence = if options.workflow_evidence_enabled {
        workflow_evidence_from_json_prompt(&task.prompt)?
    } else {
        WorkflowEvidence::default()
    };
    let workflow_evidence_counts = workflow_evidence_counts(&workflow_evidence);
    let mut history_lane = if options.history_lane_enabled {
        discover_history_lane(root, options.base_revision, &workflow_evidence.symbols)?
    } else {
        HistoryLaneReport::default()
    };
    let (mut ast_structural_lane, mut orientation_capsule, mut owner_evidence) =
        if options.ast_structural_lane_enabled || options.ast_structural_lane_v2_enabled {
            let version = if options.ast_structural_lane_v2_enabled {
                AstStructuralLaneVersion::V2
            } else {
                AstStructuralLaneVersion::V1
            };
            discover_ast_structural_lane(
                services,
                &task.languages,
                &workflow_evidence.failure_traces,
                options.orientation_capsule_enabled,
                version,
            )
            .await?
        } else {
            (
                AstStructuralLaneReport::default(),
                OrientationCapsuleReport::default(),
                None,
            )
        };
    history_lane.relevant_candidate_paths = history_lane
        .candidate_paths
        .iter()
        .filter(|path| relevant_paths.contains(*path))
        .count();
    ast_structural_lane.relevant_candidate_paths = ast_structural_lane
        .candidate_paths
        .iter()
        .filter(|path| relevant_paths.contains(*path))
        .count();
    orientation_capsule.relevant_paths = orientation_capsule
        .entries
        .iter()
        .filter(|entry| relevant_paths.contains(&entry.path))
        .count();
    if let Some(evidence) = owner_evidence.as_mut() {
        evidence.report.relevant = relevant_paths.contains(&evidence.report.path);
        if let Some(reservation) = ast_structural_lane.owner_reservation.as_mut() {
            reservation.relevant = evidence.report.relevant;
        }
    }
    let owner_source_tokens = owner_evidence
        .as_ref()
        .map_or(0, |evidence| evidence.report.source_tokens);
    if owner_source_tokens >= task.token_budget {
        owner_evidence = None;
        ast_structural_lane.owner_reservation = None;
    }
    request.token_budget = task.token_budget.saturating_sub(
        owner_evidence
            .as_ref()
            .map_or(0, |evidence| evidence.report.source_tokens),
    );
    if !history_lane.candidate_paths.is_empty() {
        request.focus_paths = history_lane.candidate_paths.clone();
        request.minimum_fragments_per_focus_path = Some(1);
    } else if options.ast_structural_lane_enabled && !ast_structural_lane.candidate_paths.is_empty()
    {
        request.focus_paths = ast_structural_lane.candidate_paths.clone();
    }
    let started = Instant::now();
    let evaluation = services
        .context_evaluation_with_workflow_evidence(request.clone(), workflow_evidence.clone())
        .await?;
    let concept_coverage = options
        .concept_labels
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
    let returned_files = sorted_unique(
        response
            .fragments
            .iter()
            .map(|item| item.path.clone())
            .chain(
                owner_evidence
                    .iter()
                    .map(|evidence| evidence.report.path.clone()),
            ),
    );
    let candidate_files = sorted_unique(evaluation.generated_candidate_paths);
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
        .collect::<Vec<_>>();
    let mut returned_evidence = response
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
        .collect::<Vec<_>>();
    if let Some(evidence) = &owner_evidence {
        returned_evidence.push(EvidenceSummary {
            path: evidence.report.path.clone(),
            start_line: evidence.report.start_line,
            end_line: evidence.report.end_line,
            representation: "ast_owner_reservation".into(),
            reason: format!(
                "bounded structural owner for {}",
                evidence.report.matched_term
            ),
            score: 0.0,
            token_count: evidence.report.source_tokens,
            content_hash: evidence.report.content_hash.clone(),
        });
    }
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
        .count()
        + usize::from(
            owner_evidence
                .as_ref()
                .is_some_and(|evidence| !evidence.report.relevant),
        );
    let dead_end_source_tokens = response
        .fragments
        .iter()
        .filter(|fragment| !relevant_paths.contains(&fragment.path))
        .map(|fragment| fragment.token_count)
        .sum::<usize>()
        + owner_evidence
            .as_ref()
            .filter(|evidence| !evidence.report.relevant)
            .map_or(0, |evidence| evidence.report.source_tokens);
    let line_anchors = task
        .relevant_files
        .iter()
        .map(|file| file.line_anchors.len())
        .sum();
    let line_anchors_found =
        count_line_anchors(&response, owner_evidence.as_ref(), &task.relevant_files);
    let owner_source_tokens = owner_evidence
        .as_ref()
        .map_or(0, |evidence| evidence.report.source_tokens);
    let owner_serialized_tokens = owner_evidence
        .as_ref()
        .map_or(0, |evidence| evidence.report.serialized_tokens);
    let leantoken_source_tokens = response
        .meta
        .source_tokens
        .saturating_add(owner_source_tokens);
    if leantoken_source_tokens > task.token_budget {
        return Err(format!(
            "{} exceeded its composite source budget: {leantoken_source_tokens} > {}",
            task.id, task.token_budget
        )
        .into());
    }
    let leantoken_total_json_tokens =
        tokens::count(&serde_json::to_string(&response)?).saturating_add(owner_serialized_tokens);

    let native_known = response
        .fragments
        .iter()
        .map(|fragment| fragment.content_hash.clone())
        .collect::<Vec<_>>();
    let native_known_set = native_known.iter().cloned().collect::<HashSet<_>>();
    let mut known = native_known;
    known.extend(
        owner_evidence
            .iter()
            .map(|evidence| evidence.report.content_hash.clone()),
    );
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
                .chain(
                    owner_evidence
                        .iter()
                        .filter(|evidence| evidence.report.path == fragment.path)
                        .map(|evidence| (evidence.report.start_line, evidence.report.end_line)),
                )
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
    if !native_known_set.is_empty() && !known_hash_omission_visible {
        return Err(format!("{} hid all known-hash omissions", task.id).into());
    }
    // The structural owner is a benchmark-side reservation. Its exact hash joins
    // the progressive request above, and the composite layer suppresses it rather
    // than serializing or charging the same sidecar twice.
    let known_owner_reservations_resent = 0;
    let owner_known_hash_omission_visible = owner_evidence
        .as_ref()
        .is_some_and(|evidence| known_set.contains(&evidence.report.content_hash));
    let task_family = task
        .task_family
        .as_deref()
        .map(str::trim)
        .filter(|family| !family.is_empty())
        .map(str::to_owned)
        .or_else(|| task.task_shapes.first().cloned())
        .unwrap_or_else(|| "unclassified".into());

    Ok(TaskReport {
        id: task.id,
        prompt: task.prompt,
        task_family,
        languages: task.languages,
        task_shapes: task.task_shapes,
        token_budget: task.token_budget,
        workflow_evidence: workflow_evidence_counts,
        history_lane,
        ast_structural_lane,
        orientation_capsule,
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
        leantoken_source_tokens,
        leantoken_total_json_tokens,
        source_savings_against_oracle_fraction: savings(
            oracle_source_tokens,
            leantoken_source_tokens,
        ),
        total_json_savings_against_scripted_fraction: savings(
            tokens::count(&scripted_json),
            leantoken_total_json_tokens,
        ),
        first_context_ms,
        warm_context_median_ms: percentile(&warm_context_ms_samples, 0.50),
        warm_context_p95_ms: percentile(&warm_context_ms_samples, 0.95),
        warm_context_ms_samples,
        second_response_source_tokens: repeat.meta.source_tokens,
        estimated_repeated_range_source_tokens,
        repeat_request_json_tokens,
        repeat_total_json_tokens,
        two_turn_context_json_tokens,
        known_fragments_resent,
        known_hash_omission_visible,
        known_owner_reservations_resent,
        owner_known_hash_omission_visible,
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

fn count_line_anchors(
    response: &ContextResponse,
    owner_evidence: Option<&AstOwnerEvidence>,
    relevant: &[RelevantFile],
) -> usize {
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
                    }) || owner_evidence.is_some_and(|evidence| {
                        evidence.report.path == file.path
                            && evidence.report.start_line <= **line
                            && evidence.report.end_line >= **line
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
    if declared != response.meta.source_tokens {
        return Err(format!(
            "context token mismatch: fragment fields={declared}, meta={}",
            response.meta.source_tokens
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
    aggregate
        .warm_context_ms_samples
        .extend_from_slice(&task.warm_context_ms_samples);
    aggregate.known_fragments_resent += task.known_fragments_resent;
    aggregate.dead_end_fragments += task.dead_end_fragments;
    aggregate.dead_end_source_tokens += task.dead_end_source_tokens;
    if let Some(reservation) = &task.ast_structural_lane.owner_reservation {
        aggregate.ast_owner_reservations += 1;
        aggregate.ast_owner_relevant_reservations += usize::from(reservation.relevant);
        aggregate.ast_owner_reservation_source_tokens += reservation.source_tokens;
        aggregate.ast_owner_reservation_serialized_tokens += reservation.serialized_tokens;
    }
    aggregate.orientation_capsule_paths += task.orientation_capsule.entries.len();
    aggregate.orientation_capsule_relevant_paths += task.orientation_capsule.relevant_paths;
    aggregate.orientation_capsule_tokens += task.orientation_capsule.capsule_tokens;
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

fn finalize_aggregate(aggregate: &mut AggregateReport) {
    aggregate.warm_context_median_ms = percentile(&aggregate.warm_context_ms_samples, 0.50);
    aggregate.warm_context_p95_ms = percentile(&aggregate.warm_context_ms_samples, 0.95);
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
    aggregate.orientation_capsule_path_recall = optional_ratio(
        aggregate.orientation_capsule_relevant_paths,
        aggregate.orientation_capsule_paths,
    );
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use leantoken::{
        ContextCoverageReceipt, ContextOmissionSummary, ContextResponseProfile, ContextWorkflow,
        EvidenceReceipt, Freshness, IndexScopeMode, ResponseMeta,
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

    #[tokio::test]
    async fn ast_structural_lane_prefers_definition_owners_from_trace_calls() {
        let root = tempfile::tempdir().expect("repository");
        fs::create_dir_all(root.path().join("src/click")).expect("source directory");
        fs::create_dir(root.path().join("tests")).expect("tests directory");
        fs::write(
            root.path().join("src/click/core.py"),
            concat!(
                "class Command:\n",
                "    def invoke(self, ctx):\n",
                "        return ctx\n\n",
                "class Option:\n",
                "    def handle_parse_result(self, ctx, opts, args):\n",
                "        return opts\n",
            ),
        )
        .expect("owner");
        fs::write(
            root.path().join("tests/test_options.py"),
            concat!(
                "@click.command()\n",
                "@click.option(\"--foo\", is_flag=True)\n",
                "def cmd(foo):\n",
                "    click.echo(foo)\n",
                "runner.invoke(cmd, [\"--foo\"])\n",
            ),
        )
        .expect("test");
        let config =
            Config::discover(root.path(), Some(root.path().join("index.sqlite"))).expect("config");
        let services = Services::open(config).expect("services");
        services.index(true).await.expect("index");
        let trace = concat!(
            "FAILED tests/test_options.py::test_flag_value\n",
            "@click.command()\n",
            "@click.option(\"--foo\", is_flag=True)\n",
            "def cmd(foo):\n",
            "    click.echo(foo)\n",
            "result = runner.invoke(cmd, [\"--foo\"])\n",
        );

        let (report, capsule, owner_evidence) = discover_ast_structural_lane(
            &services,
            &[String::from("python")],
            &[trace.into()],
            true,
            AstStructuralLaneVersion::V1,
        )
        .await
        .expect("AST structural lane");

        assert_eq!(report.languages_attempted, ["python"]);
        assert!(report.terms.iter().any(|term| term == "option"));
        assert!(report.searches <= AST_LANE_MAX_TERMS);
        assert!(report.candidate_paths.len() <= AST_LANE_MAX_PATHS);
        assert_eq!(
            report.candidate_paths.first().map(String::as_str),
            Some("src/click/core.py")
        );
        assert!(capsule.enabled);
        assert!(owner_evidence.is_none());
        assert!(capsule.capsule_tokens <= ORIENTATION_CAPSULE_MAX_TOKENS);
        assert_eq!(capsule.entries.len(), 1);
        assert_eq!(capsule.entries[0].path, "src/click/core.py");
        assert!(
            capsule.entries[0]
                .matched_terms
                .iter()
                .any(|term| term == "option")
        );
        assert!(
            capsule.entries[0]
                .definitions
                .iter()
                .any(|definition| definition.eq_ignore_ascii_case("Option"))
        );
    }

    #[test]
    fn ast_structural_v2_extracts_bounded_multilingual_trace_signals() {
        let python = extract_ast_v2_trace_signals(
            "python",
            concat!(
                "python -m venv .arb-venv\n",
                "pytest.param({\"type\": click.BOOL, \"default\": True}, True)\n",
                "@click.option(\"--foo\", is_flag=True, **opts)\n",
                "result = runner.invoke(cmd, [])\n",
            ),
        );
        assert!(python.member_terms.iter().any(|term| term == "option"));
        assert!(python.member_terms.iter().any(|term| term == "invoke"));
        assert!(!python.member_terms.iter().any(|term| term == "bool"));
        assert!(python.owner_terms.iter().any(|term| term == "click"));
        assert!(python.owner_terms.iter().any(|term| term == "runner"));
        assert!(!python.owner_terms.iter().any(|term| term == "venv"));
        assert!(["type", "default", "is_flag"].iter().all(|expected| {
            python
                .named_argument_terms
                .iter()
                .any(|term| term == expected)
        }));
        assert!(python.member_terms.len() <= AST_LANE_MAX_TERMS);
        assert!(python.owner_terms.len() <= AST_LANE_V2_MAX_OWNER_TERMS);
        assert!(python.named_argument_terms.len() <= AST_LANE_V2_MAX_NAMED_ARGUMENT_TERMS);
        let python_extensions = ast_source_extensions(&[String::from("python")]);
        assert!(ast_path_matches_source_extensions(
            "src/click/core.py",
            &python_extensions
        ));
        assert!(!ast_path_matches_source_extensions(
            "docs/options.md",
            &python_extensions
        ));

        let rust = extract_ast_v2_trace_signals(
            "rust",
            concat!(
                "error: no method named `default_values_if` for `clap::Arg`\n",
                "Arg::new(\"args\").default_value_if(\"opt\", \"value\", \"fallback\")\n",
                "let config = Config { default_value: None };\n",
            ),
        );
        assert!(rust.owner_terms.iter().any(|term| term == "arg"));
        assert!(rust.member_terms.iter().any(|term| term == "new"));
        assert!(
            rust.named_argument_terms
                .iter()
                .any(|term| term == "default_value")
        );
    }

    #[tokio::test]
    async fn ast_structural_v2_uses_named_arguments_to_reserve_one_small_owner() {
        let root = tempfile::tempdir().expect("repository");
        fs::create_dir_all(root.path().join("src/click")).expect("source directory");
        fs::write(
            root.path().join("src/click/core.py"),
            concat!(
                "class Option:\n",
                "    def __init__(self, param_decls, type=None, default=None, is_flag=False):\n",
                "        self.type = type\n",
                "        self.default = default\n",
                "        self.is_flag = is_flag\n",
            ),
        )
        .expect("owner");
        fs::write(
            root.path().join("src/click/decorators.py"),
            concat!(
                "def command():\n",
                "    return None\n\n",
                "def option(*param_decls, **attrs):\n",
                "    return None\n\n",
                "def echo(value):\n",
                "    return value\n\n",
                "def invoke(command, args):\n",
                "    return command\n",
            ),
        )
        .expect("decoy");
        let config =
            Config::discover(root.path(), Some(root.path().join("index.sqlite"))).expect("config");
        let services = Services::open(config).expect("services");
        services.index(true).await.expect("index");
        let trace = concat!(
            "pytest.param({\"type\": click.BOOL, \"default\": True}, True)\n",
            "@click.command()\n",
            "@click.option(\"--foo\", is_flag=True, **opts)\n",
            "click.echo(foo)\n",
            "result = runner.invoke(cmd, [])\n",
        );

        let (report, capsule, owner_evidence) = discover_ast_structural_lane(
            &services,
            &[String::from("python")],
            &[trace.into()],
            false,
            AstStructuralLaneVersion::V2,
        )
        .await
        .expect("AST structural v2 lane");

        assert_eq!(report.version, 2);
        assert!(!capsule.enabled);
        assert!(["type", "default", "is_flag"].iter().all(|expected| {
            report
                .named_argument_terms
                .iter()
                .any(|term| term == expected)
        }));
        assert!(report.auxiliary_searches <= 8);
        assert!(report.searches <= AST_LANE_MAX_TERMS + 8);
        assert_eq!(
            report.candidate_paths.first().map(String::as_str),
            Some("src/click/core.py")
        );
        let owner = owner_evidence.expect("one owner reservation");
        assert_eq!(owner.report.path, "src/click/core.py");
        assert!(owner.report.symbol.eq_ignore_ascii_case("Option"));
        assert!(owner.report.source_tokens <= AST_LANE_V2_MAX_OWNER_EVIDENCE_TOKENS);
        assert!(!owner.report.excerpt.is_empty());
        assert_eq!(
            report
                .owner_reservation
                .as_ref()
                .map(|reservation| reservation.path.as_str()),
            Some("src/click/core.py")
        );
    }

    fn ast_owner_hit(symbol: &str, matched_term: &str, excerpt: &str) -> AstOwnerHit {
        AstOwnerHit {
            start_line: 10,
            end_line: 20,
            excerpt: excerpt.into(),
            symbol: symbol.into(),
            matched_term: matched_term.into(),
            normalized_score: 1.0,
            corroborating_owner_terms: BTreeSet::new(),
            corroborating_named_argument_terms: BTreeSet::new(),
        }
    }

    #[test]
    fn ast_structural_v2_requires_owner_local_corroboration() {
        let mut stats = AstPathStats {
            owner_hits: vec![ast_owner_hit("Option", "option", "class Option:\n    pass")],
            ..AstPathStats::default()
        };
        let unrelated = SearchHit {
            path: "src/click/core.py".into(),
            start_line: 40,
            end_line: 42,
            excerpt: "def unrelated(is_flag=False):\n    pass".into(),
            match_kind: "symbol".into(),
            match_kinds: vec!["symbol".into()],
            role: None,
            symbol: Some("unrelated".into()),
            enclosing_symbol: Some("unrelated".into()),
            occurrence: None,
            score: 1.0,
            normalized_score: 1.0,
            score_reasons: Vec::new(),
            content_hash: "unrelated".into(),
        };

        assert!(!record_ast_corroborating_hit(
            &mut stats,
            "is_flag",
            AstAuxiliaryTermRole::NamedArgument,
            &unrelated,
        ));
        assert!(stats.named_argument_terms.is_empty());
        assert!(
            stats.owner_hits[0]
                .corroborating_named_argument_terms
                .is_empty()
        );
    }

    #[test]
    fn ast_structural_v2_skips_inexact_top_path_for_exact_owner() {
        let ranked = vec![
            (
                "src/decoy.py".into(),
                AstPathStats {
                    owner_hits: vec![ast_owner_hit(
                        "option_factory",
                        "option",
                        "def option_factory():\n    pass",
                    )],
                    ..AstPathStats::default()
                },
            ),
            (
                "src/click/core.py".into(),
                AstPathStats {
                    owner_hits: vec![ast_owner_hit("Option", "option", "class Option:\n    pass")],
                    ..AstPathStats::default()
                },
            ),
        ];

        let owner = build_ast_owner_evidence(&ranked)
            .expect("serialize owner")
            .expect("fallback owner");
        assert_eq!(owner.report.path, "src/click/core.py");
        assert_eq!(owner.report.symbol, "Option");
    }

    #[test]
    fn ast_structural_v2_cli_contract_is_explicit_and_exclusive() {
        assert!(
            Args::try_parse_from(["representative_benchmark", "--ast-structural-lane-v2"]).is_err()
        );
        assert!(
            Args::try_parse_from([
                "representative_benchmark",
                "--workflow-evidence",
                "--ast-structural-lane",
                "--ast-structural-lane-v2",
            ])
            .is_err()
        );
        assert!(
            Args::try_parse_from([
                "representative_benchmark",
                "--workflow-evidence",
                "--ast-structural-lane-v2",
                "--orientation-capsule",
            ])
            .is_err()
        );
        assert!(
            Args::try_parse_from([
                "representative_benchmark",
                "--workflow-evidence",
                "--ast-structural-lane-v2",
            ])
            .is_ok()
        );
    }

    #[test]
    fn history_lane_uses_one_bounded_pickaxe_for_all_symbols() {
        if Command::new("git").arg("--version").output().is_err() {
            return;
        }
        let root = tempfile::tempdir().expect("repository");
        let run = |args: &[&str]| {
            let output = Command::new("git")
                .args(args)
                .current_dir(root.path())
                .output()
                .expect("git command");
            assert!(
                output.status.success(),
                "git {}: {}",
                args.join(" "),
                String::from_utf8_lossy(&output.stderr)
            );
        };
        run(&["init"]);
        run(&["config", "user.email", "benchmark@example.com"]);
        run(&["config", "user.name", "Benchmark"]);
        fs::create_dir(root.path().join("src")).expect("src");
        fs::write(
            root.path().join("src/owner.rs"),
            "fn default_values_if() {}\n",
        )
        .expect("owner");
        run(&["add", "."]);
        run(&["commit", "-m", "add owner"]);
        fs::write(
            root.path().join("src/owner.rs"),
            "pub fn default_values_if() {}\n",
        )
        .expect("modify owner");
        run(&["add", "."]);
        run(&["commit", "-m", "update owner"]);
        let revision = git_output(root.path(), &["rev-parse", "HEAD"])
            .expect("revision")
            .trim()
            .to_owned();

        let report = discover_history_lane(
            root.path(),
            &revision,
            &["default_values_if".into(), "Parser::parse".into()],
        )
        .expect("history lane");

        assert_eq!(report.subprocesses, 2);
        assert!(report.available);
        assert_eq!(report.commits_examined, 2);
        assert!(report.commit_window_complete);
        assert!(!report.output_truncated);
        assert_eq!(
            report.candidate_paths.first().map(String::as_str),
            Some("src/owner.rs")
        );
    }

    fn context_response(receipt_id: &str) -> ContextResponse {
        ContextResponse {
            workflow: ContextWorkflow::Implementation,
            workflow_receipt: None,
            plan: None,
            effective_response_profile: ContextResponseProfile::Balanced,
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
                index_scope: IndexScopeMode::Full,
                index_scope_digest: None,
                source_tokens: 0,
                protocol_tokens: 0,
                path_and_metadata_tokens: 0,
                total_response_tokens: 0,
                tokenizer: "cl100k_base".into(),
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
    fn schema_five_requires_explicit_task_families() {
        let mut manifest = external_manifest();
        manifest.schema_version = 5;
        assert!(
            validate_manifest(&manifest)
                .expect_err("schema five without task family")
                .to_string()
                .contains("requires task_family")
        );

        manifest.corpora[0].tasks[0].task_family = Some("failure_trace_owner".into());
        validate_manifest(&manifest).expect("schema five with explicit task family");
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
