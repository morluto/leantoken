use std::collections::HashSet;
use std::error::Error;
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use clap::Parser;
use leantoken::{Config, ContextRequest, ContextResponse, services::Services, tokens};
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
}

#[derive(Debug, Deserialize)]
struct Manifest {
    schema_version: u32,
    #[serde(default = "default_dataset_kind")]
    dataset_kind: String,
    #[serde(default)]
    frozen_at: Option<String>,
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
    tasks: Vec<TaskSpec>,
}

#[derive(Debug, Deserialize)]
struct TaskSpec {
    id: String,
    prompt: String,
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

#[derive(Debug, Serialize)]
struct Report {
    schema_version: u32,
    dataset_kind: String,
    manifest_blake3: String,
    frozen_at: Option<String>,
    manifest_description: String,
    leantoken_version: &'static str,
    host_os: &'static str,
    host_arch: &'static str,
    rustc_version: String,
    ripgrep_version: String,
    generated_at_unix_seconds: u64,
    tokenizer: &'static str,
    token_count_exact: bool,
    methodology: Methodology,
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
    token_budget: usize,
    relevant_files: Vec<String>,
    returned_files: Vec<String>,
    returned_evidence: Vec<EvidenceSummary>,
    relevant_files_found: usize,
    relevant_file_recall: f64,
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
    if !matches!(manifest.schema_version, 1 | 2) {
        return Err(format!(
            "unsupported benchmark manifest schema version {}",
            manifest.schema_version
        )
        .into());
    }
    validate_manifest(&manifest)?;
    let ripgrep_version = command_version("rg")?;
    preflight(&manifest, &args.repos_root)?;

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
            let report = run_task(&root, &services, task, manifest.rg_max_lines_per_query).await?;
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
    aggregate.labeled_file_precision =
        ratio(aggregate.relevant_files_found, aggregate.returned_files);
    aggregate.line_anchor_recall =
        optional_ratio(aggregate.line_anchors_found, aggregate.line_anchors);
    aggregate.source_savings_against_oracle_fraction = savings(
        aggregate.oracle_source_tokens,
        aggregate.leantoken_source_tokens,
    );
    aggregate.total_json_savings_against_scripted_fraction = savings(
        aggregate.scripted_baseline_total_json_tokens,
        aggregate.leantoken_total_json_tokens,
    );

    let report = Report {
        schema_version: manifest.schema_version,
        dataset_kind: manifest.dataset_kind.clone(),
        manifest_blake3,
        frozen_at: manifest.frozen_at,
        manifest_description: manifest.description,
        leantoken_version: env!("CARGO_PKG_VERSION"),
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
        aggregate,
        corpora,
        limitations: benchmark_limitations(&manifest.dataset_kind),
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

fn default_dataset_kind() -> String {
    "development".to_owned()
}

fn validate_manifest(manifest: &Manifest) -> Result<(), Box<dyn Error>> {
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
    Ok(())
}

fn benchmark_limitations(dataset_kind: &str) -> Vec<&'static str> {
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
    } else if dataset_kind == "prospective_validation" {
        limitations.push(
            "Validation prompts and labels were frozen from open issue reports and pinned source inspection, then used during retrieval tuning; this is not blind holdout evidence.",
        );
        limitations.push(
            "Four validation tasks are retrieval development evidence, not a statistically powered product claim.",
        );
    } else {
        limitations.push(
            "Development prompts and labels were derived retrospectively from public future fixes and must not be reported as blind generalization evidence.",
        );
        limitations.push(
            "Eight development tasks are retrieval smoke evidence, not a statistically powered product claim.",
        );
    }
    limitations
}

fn is_patch_free_dataset(dataset_kind: &str) -> bool {
    matches!(dataset_kind, "prospective_validation" | "blind_holdout")
}

async fn run_task(
    root: &Path,
    services: &Services,
    task: TaskSpec,
    rg_max_lines_per_query: usize,
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
        focus_paths: Vec::new(),
        focus_symbols: Vec::new(),
        exclude_paths: Vec::new(),
        known_hashes: Vec::new(),
        prior_repository_generation: None,
    };
    let started = Instant::now();
    let response = services.context(request.clone()).await?;
    let first_context_ms = elapsed_ms(started);
    verify_token_accounting(&response)?;
    let canonical_response = serde_json::to_string(&response)?;
    let mut warm_context_ms_samples = Vec::with_capacity(3);
    for _ in 0..3 {
        let started = Instant::now();
        let warm = services.context(request.clone()).await?;
        warm_context_ms_samples.push(elapsed_ms(started));
        verify_token_accounting(&warm)?;
        if serde_json::to_string(&warm)? != canonical_response {
            return Err(format!("{} returned nondeterministic context", task.id).into());
        }
    }
    let returned_files = sorted_unique(response.fragments.iter().map(|item| item.path.clone()));
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
        prior_repository_generation: Some(response.meta.repository_generation),
        ..request
    };
    let repeat_request_json_tokens = tokens::count(&serde_json::to_string(&repeat_request)?);
    let repeat = services.context(repeat_request).await?;
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
    let known_hash_omission_visible = repeat
        .omitted
        .iter()
        .any(|candidate| candidate.reason == "known hash");
    if !known_set.is_empty() && !known_hash_omission_visible {
        return Err(format!("{} hid all known-hash omissions", task.id).into());
    }

    Ok(TaskReport {
        id: task.id,
        prompt: task.prompt,
        token_budget: task.token_budget,
        relevant_files: task
            .relevant_files
            .into_iter()
            .map(|file| file.path)
            .collect(),
        returned_files,
        returned_evidence,
        relevant_files_found,
        relevant_file_recall: ratio(relevant_files_found, relevant_paths.len()),
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
    })
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
        } else if !is_patch_free_dataset(&manifest.dataset_kind) {
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
}

#[cfg(test)]
mod tests {
    use super::*;

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

        manifest.corpora[0].fix_commit = Some("future".into());
        assert!(validate_manifest(&manifest).is_err());
    }
}
