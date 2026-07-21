use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

use clap::Parser;
use leantoken::{
    Config, ContextEvaluation, ContextRequest, ContextResponse, ContextSignalPolicy,
    services::Services, tokens,
};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};

const REPORT_SCHEMA_VERSION: u32 = 1;
const EXPECTED_ARMS: [&str; 4] = [
    "lexical_syntax",
    "import_neighbor",
    "reverse_dependency",
    "high_confidence_caller",
];

type DynError = Box<dyn Error>;

#[derive(Debug, Parser)]
#[command(about = "Run a frozen additive dependency/caller signal ablation")]
struct Args {
    /// Ablation manifest with preregistered task labels and acceptance thresholds.
    #[arg(long, default_value = "benchmarks/graph_signal_ablation_v1.json")]
    manifest: PathBuf,
    /// Root containing repositories at the source manifest's pinned revisions.
    #[arg(long, default_value = "target/representative-repos")]
    repos_root: PathBuf,
    /// Redacted JSON report path.
    #[arg(long, default_value = "target/graph-signal-ablation-v1.json")]
    output: PathBuf,
    /// Validate identities, labels, revisions, and clean worktrees without indexing.
    #[arg(long)]
    preflight_only: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AblationManifest {
    schema_version: u32,
    experiment_id: String,
    description: String,
    source_manifest: PathBuf,
    source_manifest_blake3: String,
    repetitions: usize,
    arms: Vec<String>,
    thresholds: Thresholds,
    task_labels: Vec<TaskLabel>,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Thresholds {
    minimum_signal_candidate_precision: f64,
    minimum_relevant_file_gain: usize,
    minimum_line_anchor_gain: usize,
    maximum_dead_end_source_token_increase: usize,
    maximum_complete_response_token_increase: usize,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct TaskLabel {
    task_id: String,
    applicable_signals: Vec<String>,
    rationale: String,
}

#[derive(Debug, Deserialize)]
struct SourceManifest {
    schema_version: u32,
    description: String,
    corpora: Vec<CorpusSpec>,
}

#[derive(Debug, Deserialize)]
struct CorpusSpec {
    name: String,
    directory: String,
    base_revision: String,
    tasks: Vec<TaskSpec>,
}

#[derive(Debug, Deserialize)]
struct TaskSpec {
    id: String,
    prompt: String,
    relevant_files: Vec<RelevantFile>,
    token_budget: usize,
}

#[derive(Debug, Deserialize)]
struct RelevantFile {
    path: String,
    line_anchors: Vec<usize>,
}

#[derive(Debug, Serialize)]
struct Report {
    schema_version: u32,
    experiment_id: String,
    manifest_blake3: String,
    source_manifest_blake3: String,
    manifest_description: String,
    source_manifest_description: String,
    source_manifest_schema_version: u32,
    harness_revision: String,
    harness_source_blake3: String,
    harness_worktree_dirty: bool,
    leantoken_version: &'static str,
    host_os: &'static str,
    host_arch: &'static str,
    rustc_version: String,
    tokenizer: &'static str,
    token_count_exact: bool,
    methodology: Methodology,
    thresholds: Thresholds,
    graph_index: GraphIndexAggregate,
    arms: Vec<ArmAggregate>,
    runs: Vec<RunReport>,
    decision: Decision,
    limitations: Vec<&'static str>,
}

#[derive(Debug, Serialize)]
struct Methodology {
    baseline: &'static str,
    arms: &'static str,
    additive_invariant: &'static str,
    edge_precision: &'static str,
    signal_precision: &'static str,
    dead_end_source: &'static str,
    complete_response_tokens: &'static str,
    index_size: &'static str,
    timing: &'static str,
}

#[derive(Debug, Default, Serialize)]
struct GraphIndexAggregate {
    corpus_count: usize,
    total_database_bytes: u64,
    total_import_edges: usize,
    resolved_import_edges: usize,
    resolved_existing_import_edges: usize,
    unresolved_import_edges: usize,
    false_resolved_import_edges: usize,
    import_edge_resolution_precision: Option<f64>,
    unresolved_import_rate: Option<f64>,
    parsed_reference_edges: usize,
    corpora: Vec<CorpusIndexReport>,
}

#[derive(Debug, Serialize)]
struct CorpusIndexReport {
    name: String,
    revision: String,
    indexed_files: usize,
    database_bytes: u64,
    cold_index_ms: f64,
    noop_reconcile_ms: Vec<f64>,
    noop_reconcile_median_ms: f64,
    imports: ImportStats,
    parsed_reference_edges: usize,
}

#[derive(Debug, Default, Clone, Copy, Serialize)]
struct ImportStats {
    total: usize,
    resolved: usize,
    resolved_existing: usize,
    unresolved: usize,
    false_resolved: usize,
    resolution_precision: Option<f64>,
    unresolved_rate: Option<f64>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
struct RetrievalTotals {
    relevant_files: usize,
    relevant_files_found: usize,
    line_anchors: usize,
    line_anchors_found: usize,
    returned_files: usize,
    source_tokens: usize,
    dead_end_fragments: usize,
    dead_end_source_tokens: usize,
    complete_response_tokens: usize,
    signal_candidate_files: usize,
    relevant_signal_candidate_files: usize,
    false_positive_signal_candidate_files: usize,
    signal_selected_files: usize,
    relevant_signal_selected_files: usize,
    applicable_signal_tasks: usize,
    applicable_signal_tasks_without_relevant_candidate: usize,
    additive_violations: usize,
}

#[derive(Debug, Serialize)]
struct ArmAggregate {
    arm: String,
    repetitions: usize,
    per_repetition: Vec<RetrievalTotals>,
    deterministic_metrics_repeat: bool,
    deterministic_task_results_repeat: bool,
    relevant_file_recall: f64,
    line_anchor_recall: f64,
    signal_candidate_precision: Option<f64>,
    signal_candidate_false_positive_rate: Option<f64>,
    signal_selected_precision: Option<f64>,
    applicable_signal_unresolved_rate: Option<f64>,
    mean_dead_end_source_tokens: f64,
    mean_complete_response_tokens: f64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct RunReport {
    corpus: String,
    task_id: String,
    repetition: usize,
    arm: String,
    signal_applicable: bool,
    applicable_signal_rationale: Option<String>,
    additive_baseline_preserved: bool,
    missing_baseline_candidates: usize,
    relevant_files: usize,
    relevant_files_found: usize,
    returned_files: Vec<String>,
    line_anchors: usize,
    line_anchors_found: usize,
    source_tokens: usize,
    dead_end_fragments: usize,
    dead_end_source_tokens: usize,
    complete_response_tokens: usize,
    signal_candidate_files: Vec<String>,
    relevant_signal_candidate_files: usize,
    false_positive_signal_candidate_files: usize,
    signal_selected_files: Vec<String>,
    relevant_signal_selected_files: usize,
    applicable_signal_unresolved: bool,
}

#[derive(Debug, Serialize)]
struct Decision {
    ranking_signal_decisions: Vec<SignalDecision>,
    retained_ranking_signals: Vec<String>,
    expose_graph_metadata: bool,
    metadata_decision: &'static str,
    issue_outcome: &'static str,
}

#[derive(Debug, Serialize)]
struct SignalDecision {
    arm: String,
    repeatable: bool,
    additive: bool,
    recall_gate_passed_every_repetition: bool,
    dead_end_gate_passed_every_repetition: bool,
    response_cost_gate_passed_every_repetition: bool,
    precision_gate_passed: bool,
    retain_ranking_signal: bool,
    reason: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Arm {
    LexicalSyntax,
    ImportNeighbor,
    ReverseDependency,
    HighConfidenceCaller,
}

impl Arm {
    const ALL: [Self; 4] = [
        Self::LexicalSyntax,
        Self::ImportNeighbor,
        Self::ReverseDependency,
        Self::HighConfidenceCaller,
    ];

    const fn name(self) -> &'static str {
        match self {
            Self::LexicalSyntax => "lexical_syntax",
            Self::ImportNeighbor => "import_neighbor",
            Self::ReverseDependency => "reverse_dependency",
            Self::HighConfidenceCaller => "high_confidence_caller",
        }
    }

    const fn policy(self) -> ContextSignalPolicy {
        match self {
            Self::LexicalSyntax => ContextSignalPolicy::LexicalSyntax,
            Self::ImportNeighbor => ContextSignalPolicy::ImportNeighbor,
            Self::ReverseDependency => ContextSignalPolicy::ReverseDependency,
            Self::HighConfidenceCaller => ContextSignalPolicy::HighConfidenceCaller,
        }
    }
}

#[tokio::main]
async fn main() -> Result<(), DynError> {
    let args = Args::parse();
    let manifest_bytes = fs::read(&args.manifest)?;
    let manifest_blake3 = blake3::hash(&manifest_bytes).to_hex().to_string();
    let manifest: AblationManifest = serde_json::from_slice(&manifest_bytes)?;
    let source_bytes = fs::read(&manifest.source_manifest)?;
    let observed_source_hash = blake3::hash(&source_bytes).to_hex().to_string();
    if observed_source_hash != manifest.source_manifest_blake3 {
        return Err(format!(
            "source manifest hash mismatch: expected {}, observed {observed_source_hash}",
            manifest.source_manifest_blake3
        )
        .into());
    }
    let source: SourceManifest = serde_json::from_slice(&source_bytes)?;
    validate_manifest(&manifest, &source)?;
    preflight_repositories(&source, &args.repos_root)?;

    let source_root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let harness_revision = git_output(source_root, &["rev-parse", "HEAD"])?;
    let harness_dirty = !git_output(
        source_root,
        &["status", "--porcelain=v1", "--untracked-files=all"],
    )?
    .is_empty();
    let harness_source_blake3 = blake3::hash(include_bytes!("graph_signal_ablation.rs"))
        .to_hex()
        .to_string();
    if args.preflight_only {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "experiment_id": manifest.experiment_id,
                "manifest_blake3": manifest_blake3,
                "source_manifest_blake3": observed_source_hash,
                "harness_revision": harness_revision,
                "harness_worktree_dirty": harness_dirty,
                "corpus_count": source.corpora.len(),
                "task_count": source.corpora.iter().map(|corpus| corpus.tasks.len()).sum::<usize>(),
                "status": "ready"
            }))?
        );
        return Ok(());
    }
    if harness_dirty {
        return Err("formal ablation requires a clean harness worktree".into());
    }

    let scratch = tempfile::tempdir()?;
    let labels = manifest
        .task_labels
        .iter()
        .map(|label| (label.task_id.as_str(), label))
        .collect::<BTreeMap<_, _>>();
    let mut graph_index = GraphIndexAggregate::default();
    let mut runs = Vec::new();
    for corpus in source.corpora {
        let root = args.repos_root.join(&corpus.directory);
        let database = scratch.path().join(format!("{}.sqlite", corpus.name));
        let services = Services::open(Config::discover(&root, Some(database.clone()))?)?;
        let started = Instant::now();
        let indexed = services.index(true).await?;
        let cold_index_ms = elapsed_ms(started);
        if !indexed.warnings.is_empty() {
            return Err(format!(
                "{} indexing emitted warnings: {:?}",
                corpus.name, indexed.warnings
            )
            .into());
        }
        let status = services.status().await?;
        let mut noop_reconcile_ms = Vec::new();
        for _ in 0..manifest.repetitions {
            let started = Instant::now();
            let reconciled = services.index(false).await?;
            if reconciled.files_indexed != 0 || reconciled.files_removed != 0 {
                return Err(
                    format!("{} no-op reconciliation changed the index", corpus.name).into(),
                );
            }
            noop_reconcile_ms.push(elapsed_ms(started));
        }
        let imports = import_stats(&database)?;
        let parsed_reference_edges = scalar_usize(&database, "SELECT COUNT(*) FROM symbol_refs")?;
        let database_bytes = logical_database_bytes(&database)?;
        accumulate_graph_index(
            &mut graph_index,
            database_bytes,
            imports,
            parsed_reference_edges,
        );
        graph_index.corpora.push(CorpusIndexReport {
            name: corpus.name.clone(),
            revision: corpus.base_revision.clone(),
            indexed_files: status.file_count,
            database_bytes,
            cold_index_ms,
            noop_reconcile_median_ms: median(&noop_reconcile_ms),
            noop_reconcile_ms,
            imports,
            parsed_reference_edges,
        });

        for repetition in 1..=manifest.repetitions {
            for task in &corpus.tasks {
                let request = context_request(task);
                let baseline = services
                    .context_signal_evaluation(request.clone(), Arm::LexicalSyntax.policy())
                    .await?;
                let baseline_keys = candidate_keys(&baseline);
                for arm in Arm::ALL {
                    let evaluation = if arm == Arm::LexicalSyntax {
                        baseline.clone()
                    } else {
                        services
                            .context_signal_evaluation(request.clone(), arm.policy())
                            .await?
                    };
                    runs.push(run_report(
                        &corpus.name,
                        task,
                        repetition,
                        arm,
                        labels[task.id.as_str()],
                        &baseline_keys,
                        evaluation,
                    )?);
                }
            }
        }
    }
    finish_graph_index(&mut graph_index);
    let arms = aggregate_arms(&runs, manifest.repetitions);
    let decision = decide(&arms, manifest.thresholds);
    let report = Report {
        schema_version: REPORT_SCHEMA_VERSION,
        experiment_id: manifest.experiment_id,
        manifest_blake3,
        source_manifest_blake3: observed_source_hash,
        manifest_description: manifest.description,
        source_manifest_description: source.description,
        source_manifest_schema_version: source.schema_version,
        harness_revision,
        harness_source_blake3,
        harness_worktree_dirty: harness_dirty,
        leantoken_version: env!("CARGO_PKG_VERSION"),
        host_os: std::env::consts::OS,
        host_arch: std::env::consts::ARCH,
        rustc_version: command_version("rustc")?,
        tokenizer: tokens::Tokenizer::default().name(),
        token_count_exact: tokens::Tokenizer::default().is_exact(),
        methodology: Methodology {
            baseline: "Symbol and full-text candidates with dependency expansion and parsed-reference candidates disabled.",
            arms: "Each candidate arm starts from the identical baseline and enables exactly one of forward import expansion, reverse-import ranking boost, or parsed reference candidates.",
            additive_invariant: "Every baseline candidate identity (path, inclusive range, representation) must remain present in every signal arm.",
            edge_precision: "Resolved import precision counts resolved_path values that join an indexed file; unresolved rate counts imports with no resolved_path. This validates resolver integrity, not semantic usefulness.",
            signal_precision: "Unique signal-bearing candidate or selected paths are relevant only when they match the frozen relevant-file labels.",
            dead_end_source: "Selected source tokens in files outside the frozen relevant-file labels.",
            complete_response_tokens: "Exact tokenizer count of the complete serialized ContextResponse; no evaluation diagnostics are included in that payload.",
            index_size: "Logical SQLite page_count times page_size after indexing and no-op reconciliation; WAL and SHM sidecars are excluded.",
            timing: "Cold index and three no-op reconciliations run in release mode on one host. Arms share the same graph-enabled index, so timing is a cost envelope, not a causal graph-disabled comparison.",
        },
        thresholds: manifest.thresholds,
        graph_index,
        arms,
        runs,
        decision,
        limitations: vec![
            "The retrospective prompts and labels use public future fixes and are development evidence, not blind generalization evidence.",
            "Eight tasks and fifteen relevant files are not a statistically powered product claim.",
            "Parsed import resolution precision does not establish that an edge is useful to a model.",
            "The benchmark executes no model, edit, or tests, so it cannot justify exposing graph metadata to agents.",
            "Index and reconciliation timings include all current indexing work; this harness does not implement a graph-disabled indexer.",
            "Package aliases, re-exports, dynamic imports, same-package relationships, and cross-language calls can remain unresolved.",
        ],
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
    Ok(())
}

fn validate_manifest(manifest: &AblationManifest, source: &SourceManifest) -> Result<(), DynError> {
    if manifest.schema_version != 1 {
        return Err(format!("unsupported manifest schema {}", manifest.schema_version).into());
    }
    if manifest.repetitions < 3 {
        return Err("at least three repetitions are required".into());
    }
    if manifest.arms.iter().map(String::as_str).collect::<Vec<_>>() != EXPECTED_ARMS {
        return Err("manifest arms must match the frozen four-arm order".into());
    }
    let task_ids = source
        .corpora
        .iter()
        .flat_map(|corpus| corpus.tasks.iter().map(|task| task.id.as_str()))
        .collect::<BTreeSet<_>>();
    let label_ids = manifest
        .task_labels
        .iter()
        .map(|label| label.task_id.as_str())
        .collect::<BTreeSet<_>>();
    if task_ids != label_ids || label_ids.len() != manifest.task_labels.len() {
        return Err("task labels must match source tasks exactly and be unique".into());
    }
    for label in &manifest.task_labels {
        if label.rationale.trim().is_empty() || label.applicable_signals.is_empty() {
            return Err(format!("{} requires signal labels and rationale", label.task_id).into());
        }
        for signal in &label.applicable_signals {
            if !EXPECTED_ARMS[1..].contains(&signal.as_str()) {
                return Err(format!("{} names unknown signal {signal}", label.task_id).into());
            }
        }
    }
    let thresholds = manifest.thresholds;
    if !(0.0..=1.0).contains(&thresholds.minimum_signal_candidate_precision) {
        return Err("signal precision threshold must be between zero and one".into());
    }
    Ok(())
}

fn preflight_repositories(source: &SourceManifest, repos_root: &Path) -> Result<(), DynError> {
    for corpus in &source.corpora {
        let root = repos_root.join(&corpus.directory);
        let revision = git_output(&root, &["rev-parse", "HEAD"])?;
        if revision != corpus.base_revision {
            return Err(format!(
                "{} revision mismatch: expected {}, observed {revision}",
                corpus.name, corpus.base_revision
            )
            .into());
        }
        if !git_output(
            &root,
            &["status", "--porcelain=v1", "--untracked-files=all"],
        )?
        .is_empty()
        {
            return Err(format!("{} benchmark worktree is dirty", corpus.name).into());
        }
    }
    Ok(())
}

fn context_request(task: &TaskSpec) -> ContextRequest {
    ContextRequest {
        task: task.prompt.clone(),
        token_budget: task.token_budget,
        focus_paths: Vec::new(),
        focus_symbols: Vec::new(),
        exclude_paths: Vec::new(),
        known_hashes: Vec::new(),
        prior_repository_generation: None,
        base_revision: None,
        changed_paths: Vec::new(),
    }
}

fn candidate_keys(evaluation: &ContextEvaluation) -> BTreeSet<(String, usize, usize, String)> {
    evaluation
        .generated_candidates
        .iter()
        .map(|candidate| {
            (
                candidate.path.clone(),
                candidate.start_line,
                candidate.end_line,
                candidate.representation.clone(),
            )
        })
        .collect()
}

fn run_report(
    corpus: &str,
    task: &TaskSpec,
    repetition: usize,
    arm: Arm,
    label: &TaskLabel,
    baseline_keys: &BTreeSet<(String, usize, usize, String)>,
    evaluation: ContextEvaluation,
) -> Result<RunReport, DynError> {
    let relevant = task
        .relevant_files
        .iter()
        .map(|file| file.path.as_str())
        .collect::<BTreeSet<_>>();
    let candidate_keys = candidate_keys(&evaluation);
    let missing_baseline_candidates = baseline_keys.difference(&candidate_keys).count();
    let returned_files = evaluation
        .response
        .fragments
        .iter()
        .map(|fragment| fragment.path.clone())
        .collect::<BTreeSet<_>>();
    let signal_candidate_files = evaluation
        .generated_candidates
        .iter()
        .filter(|candidate| candidate_has_signal(candidate, arm))
        .map(|candidate| candidate.path.clone())
        .collect::<BTreeSet<_>>();
    let signal_selected_files = evaluation
        .response
        .fragments
        .iter()
        .filter(|fragment| selected_has_signal(&fragment.reason, arm))
        .map(|fragment| fragment.path.clone())
        .collect::<BTreeSet<_>>();
    let relevant_files_found = returned_files
        .iter()
        .filter(|path| relevant.contains(path.as_str()))
        .count();
    let relevant_signal_candidate_files = signal_candidate_files
        .iter()
        .filter(|path| relevant.contains(path.as_str()))
        .count();
    let relevant_signal_selected_files = signal_selected_files
        .iter()
        .filter(|path| relevant.contains(path.as_str()))
        .count();
    let false_positive_signal_candidate_files = signal_candidate_files
        .len()
        .saturating_sub(relevant_signal_candidate_files);
    let line_anchors = task
        .relevant_files
        .iter()
        .map(|file| file.line_anchors.len())
        .sum();
    let line_anchors_found = count_line_anchors(&evaluation.response, &task.relevant_files);
    let source_tokens = evaluation
        .response
        .fragments
        .iter()
        .map(|fragment| fragment.token_count)
        .sum();
    let dead_end_fragments = evaluation
        .response
        .fragments
        .iter()
        .filter(|fragment| !relevant.contains(fragment.path.as_str()))
        .count();
    let dead_end_source_tokens = evaluation
        .response
        .fragments
        .iter()
        .filter(|fragment| !relevant.contains(fragment.path.as_str()))
        .map(|fragment| fragment.token_count)
        .sum();
    let complete_response_tokens = tokens::count(&serde_json::to_string(&evaluation.response)?);
    let signal_applicable = label
        .applicable_signals
        .iter()
        .any(|name| name == arm.name());
    let applicable_signal_unresolved = signal_applicable && relevant_signal_candidate_files == 0;
    Ok(RunReport {
        corpus: corpus.to_owned(),
        task_id: task.id.clone(),
        repetition,
        arm: arm.name().to_owned(),
        signal_applicable,
        applicable_signal_rationale: signal_applicable.then(|| label.rationale.clone()),
        additive_baseline_preserved: missing_baseline_candidates == 0,
        missing_baseline_candidates,
        relevant_files: relevant.len(),
        relevant_files_found,
        returned_files: returned_files.into_iter().collect(),
        line_anchors,
        line_anchors_found,
        source_tokens,
        dead_end_fragments,
        dead_end_source_tokens,
        complete_response_tokens,
        signal_candidate_files: signal_candidate_files.into_iter().collect(),
        relevant_signal_candidate_files,
        false_positive_signal_candidate_files,
        signal_selected_files: signal_selected_files.into_iter().collect(),
        relevant_signal_selected_files,
        applicable_signal_unresolved,
    })
}

fn candidate_has_signal(candidate: &leantoken::ContextCandidateEvaluation, arm: Arm) -> bool {
    match arm {
        Arm::LexicalSyntax => false,
        Arm::ImportNeighbor => candidate.representation == "import_symbol",
        Arm::ReverseDependency => candidate
            .match_kinds
            .iter()
            .any(|kind| kind == "reverse-import"),
        Arm::HighConfidenceCaller => candidate.match_kinds.iter().any(|kind| kind == "reference"),
    }
}

fn selected_has_signal(reason: &str, arm: Arm) -> bool {
    let reasons = reason.split("; ").collect::<BTreeSet<_>>();
    match arm {
        Arm::LexicalSyntax => false,
        Arm::ImportNeighbor => reasons.contains("import"),
        Arm::ReverseDependency => reasons.contains("reverse-import"),
        Arm::HighConfidenceCaller => reasons.contains("reference"),
    }
}

fn count_line_anchors(response: &ContextResponse, relevant: &[RelevantFile]) -> usize {
    relevant
        .iter()
        .map(|file| {
            file.line_anchors
                .iter()
                .filter(|anchor| {
                    response.fragments.iter().any(|fragment| {
                        fragment.path == file.path
                            && fragment.start_line <= **anchor
                            && fragment.end_line >= **anchor
                    })
                })
                .count()
        })
        .sum()
}

fn aggregate_arms(runs: &[RunReport], repetitions: usize) -> Vec<ArmAggregate> {
    Arm::ALL
        .into_iter()
        .map(|arm| {
            let mut per_repetition = Vec::new();
            for repetition in 1..=repetitions {
                let mut totals = RetrievalTotals::default();
                for run in runs
                    .iter()
                    .filter(|run| run.arm == arm.name() && run.repetition == repetition)
                {
                    totals.relevant_files += run.relevant_files;
                    totals.relevant_files_found += run.relevant_files_found;
                    totals.line_anchors += run.line_anchors;
                    totals.line_anchors_found += run.line_anchors_found;
                    totals.returned_files += run.returned_files.len();
                    totals.source_tokens += run.source_tokens;
                    totals.dead_end_fragments += run.dead_end_fragments;
                    totals.dead_end_source_tokens += run.dead_end_source_tokens;
                    totals.complete_response_tokens += run.complete_response_tokens;
                    totals.signal_candidate_files += run.signal_candidate_files.len();
                    totals.relevant_signal_candidate_files += run.relevant_signal_candidate_files;
                    totals.false_positive_signal_candidate_files +=
                        run.false_positive_signal_candidate_files;
                    totals.signal_selected_files += run.signal_selected_files.len();
                    totals.relevant_signal_selected_files += run.relevant_signal_selected_files;
                    totals.applicable_signal_tasks += usize::from(run.signal_applicable);
                    totals.applicable_signal_tasks_without_relevant_candidate +=
                        usize::from(run.applicable_signal_unresolved);
                    totals.additive_violations += run.missing_baseline_candidates;
                }
                per_repetition.push(totals);
            }
            let first = per_repetition.first().cloned().unwrap_or_default();
            let deterministic_metrics_repeat = per_repetition.iter().all(|totals| totals == &first);
            let reference_runs = normalized_task_runs(runs, arm, 1);
            let deterministic_task_results_repeat = (2..=repetitions)
                .all(|repetition| normalized_task_runs(runs, arm, repetition) == reference_runs);
            ArmAggregate {
                arm: arm.name().to_owned(),
                repetitions,
                relevant_file_recall: ratio(first.relevant_files_found, first.relevant_files),
                line_anchor_recall: ratio(first.line_anchors_found, first.line_anchors),
                signal_candidate_precision: optional_ratio(
                    first.relevant_signal_candidate_files,
                    first.signal_candidate_files,
                ),
                signal_candidate_false_positive_rate: optional_ratio(
                    first.false_positive_signal_candidate_files,
                    first.signal_candidate_files,
                ),
                signal_selected_precision: optional_ratio(
                    first.relevant_signal_selected_files,
                    first.signal_selected_files,
                ),
                applicable_signal_unresolved_rate: optional_ratio(
                    first.applicable_signal_tasks_without_relevant_candidate,
                    first.applicable_signal_tasks,
                ),
                mean_dead_end_source_tokens: mean(
                    &per_repetition
                        .iter()
                        .map(|totals| totals.dead_end_source_tokens)
                        .collect::<Vec<_>>(),
                ),
                mean_complete_response_tokens: mean(
                    &per_repetition
                        .iter()
                        .map(|totals| totals.complete_response_tokens)
                        .collect::<Vec<_>>(),
                ),
                per_repetition,
                deterministic_metrics_repeat,
                deterministic_task_results_repeat,
            }
        })
        .collect()
}

fn normalized_task_runs(runs: &[RunReport], arm: Arm, repetition: usize) -> Vec<RunReport> {
    runs.iter()
        .filter(|run| run.arm == arm.name() && run.repetition == repetition)
        .cloned()
        .map(|mut run| {
            run.repetition = 0;
            run
        })
        .collect()
}

fn decide(arms: &[ArmAggregate], thresholds: Thresholds) -> Decision {
    let baseline = &arms[0];
    let mut ranking_signal_decisions = Vec::new();
    let mut retained_ranking_signals = Vec::new();
    for arm in &arms[1..] {
        let additive = arm
            .per_repetition
            .iter()
            .all(|totals| totals.additive_violations == 0);
        let recall_gate_passed_every_repetition = arm
            .per_repetition
            .iter()
            .zip(&baseline.per_repetition)
            .all(|(candidate, base)| {
                candidate.relevant_files_found
                    >= base.relevant_files_found + thresholds.minimum_relevant_file_gain
                    || candidate.line_anchors_found
                        >= base.line_anchors_found + thresholds.minimum_line_anchor_gain
            });
        let dead_end_gate_passed_every_repetition = arm
            .per_repetition
            .iter()
            .zip(&baseline.per_repetition)
            .all(|(candidate, base)| {
                candidate.dead_end_source_tokens
                    <= base.dead_end_source_tokens
                        + thresholds.maximum_dead_end_source_token_increase
            });
        let response_cost_gate_passed_every_repetition = arm
            .per_repetition
            .iter()
            .zip(&baseline.per_repetition)
            .all(|(candidate, base)| {
                candidate.complete_response_tokens
                    <= base.complete_response_tokens
                        + thresholds.maximum_complete_response_token_increase
            });
        let precision_gate_passed = arm
            .signal_candidate_precision
            .is_some_and(|precision| precision >= thresholds.minimum_signal_candidate_precision);
        let repeatable = arm.deterministic_metrics_repeat && arm.deterministic_task_results_repeat;
        let retain = repeatable
            && additive
            && recall_gate_passed_every_repetition
            && dead_end_gate_passed_every_repetition
            && response_cost_gate_passed_every_repetition
            && precision_gate_passed;
        if retain {
            retained_ranking_signals.push(arm.arm.clone());
        }
        ranking_signal_decisions.push(SignalDecision {
            arm: arm.arm.clone(),
            repeatable,
            additive,
            recall_gate_passed_every_repetition,
            dead_end_gate_passed_every_repetition,
            response_cost_gate_passed_every_repetition,
            precision_gate_passed,
            retain_ranking_signal: retain,
            reason: if retain {
                "Passed every preregistered repeatability, additivity, recall, dead-end, response-cost, and precision gate.".into()
            } else {
                "Rejected because one or more preregistered repeatability, additivity, recall, dead-end, response-cost, or precision gates failed.".into()
            },
        });
    }
    Decision {
        issue_outcome: if retained_ranking_signals.is_empty() {
            "no_go"
        } else {
            "ranking_only_go"
        },
        ranking_signal_decisions,
        retained_ranking_signals,
        expose_graph_metadata: false,
        metadata_decision: "Do not expose graph metadata: this retrieval-only ablation provides no model-visible value measurement, while complete response cost is already measured without new fields.",
    }
}

fn import_stats(database: &Path) -> Result<ImportStats, DynError> {
    let conn = Connection::open(database)?;
    let (total, resolved, resolved_existing): (i64, i64, i64) = conn.query_row(
        "SELECT COUNT(*),
                COALESCE(SUM(CASE WHEN imports.resolved_path IS NOT NULL THEN 1 ELSE 0 END), 0),
                COALESCE(SUM(CASE WHEN files.id IS NOT NULL THEN 1 ELSE 0 END), 0)
         FROM imports
         LEFT JOIN files ON files.path = imports.resolved_path",
        [],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?;
    let total = usize::try_from(total)?;
    let resolved = usize::try_from(resolved)?;
    let resolved_existing = usize::try_from(resolved_existing)?;
    let unresolved = total.saturating_sub(resolved);
    let false_resolved = resolved.saturating_sub(resolved_existing);
    Ok(ImportStats {
        total,
        resolved,
        resolved_existing,
        unresolved,
        false_resolved,
        resolution_precision: optional_ratio(resolved_existing, resolved),
        unresolved_rate: optional_ratio(unresolved, total),
    })
}

fn scalar_usize(database: &Path, sql: &str) -> Result<usize, DynError> {
    let conn = Connection::open(database)?;
    let value: i64 = conn.query_row(sql, [], |row| row.get(0))?;
    Ok(usize::try_from(value)?)
}

fn accumulate_graph_index(
    aggregate: &mut GraphIndexAggregate,
    database_bytes: u64,
    imports: ImportStats,
    references: usize,
) {
    aggregate.corpus_count += 1;
    aggregate.total_database_bytes += database_bytes;
    aggregate.total_import_edges += imports.total;
    aggregate.resolved_import_edges += imports.resolved;
    aggregate.resolved_existing_import_edges += imports.resolved_existing;
    aggregate.unresolved_import_edges += imports.unresolved;
    aggregate.false_resolved_import_edges += imports.false_resolved;
    aggregate.parsed_reference_edges += references;
}

fn finish_graph_index(aggregate: &mut GraphIndexAggregate) {
    aggregate.import_edge_resolution_precision = optional_ratio(
        aggregate.resolved_existing_import_edges,
        aggregate.resolved_import_edges,
    );
    aggregate.unresolved_import_rate = optional_ratio(
        aggregate.unresolved_import_edges,
        aggregate.total_import_edges,
    );
}

fn logical_database_bytes(database: &Path) -> Result<u64, DynError> {
    let conn = Connection::open(database)?;
    let page_count =
        u64::try_from(conn.pragma_query_value::<i64, _>(None, "page_count", |row| row.get(0))?)?;
    let page_size =
        u64::try_from(conn.pragma_query_value::<i64, _>(None, "page_size", |row| row.get(0))?)?;
    page_count
        .checked_mul(page_size)
        .ok_or_else(|| "logical SQLite size overflow".into())
}

fn git_output(root: &Path, args: &[&str]) -> Result<String, DynError> {
    let output = Command::new("git").args(args).current_dir(root).output()?;
    if !output.status.success() {
        return Err(format!(
            "git {} failed in benchmark input: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        )
        .into());
    }
    Ok(String::from_utf8(output.stdout)?.trim().to_owned())
}

fn command_version(command: &str) -> Result<String, DynError> {
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

fn elapsed_ms(started: Instant) -> f64 {
    started.elapsed().as_secs_f64() * 1_000.0
}

fn median(values: &[f64]) -> f64 {
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    sorted[sorted.len() / 2]
}

fn mean(values: &[usize]) -> f64 {
    values.iter().sum::<usize>() as f64 / values.len() as f64
}

fn ratio(numerator: usize, denominator: usize) -> f64 {
    numerator as f64 / denominator as f64
}

fn optional_ratio(numerator: usize, denominator: usize) -> Option<f64> {
    (denominator != 0).then(|| ratio(numerator, denominator))
}
