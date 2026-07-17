#![allow(dead_code)]

use std::{
    collections::{BTreeMap, BTreeSet, HashSet},
    error::Error,
    fs::{self, OpenOptions},
    io::Write,
    path::Path,
};

use leantoken::tokens::Tokenizer;
use serde::{Deserialize, Serialize};

#[path = "swe_bench_patch.rs"]
mod patch;
#[path = "swe_bench_selection.rs"]
mod selection;

use patch::{
    extract_patch_evidence, infer_task_language, line_map_len, merge_line_maps,
    normalize_diff_path, regions_from_lines, subtract_line_map,
};
use selection::{query_contains_exact_identifier, select_candidates, selection_key};

const SCHEMA_VERSION: u32 = 1;
const DATASET_KIND: &str = "swe_bench_multilingual_development";
const DATASET_NAME: &str = "SWE-bench/SWE-bench_Multilingual";
const DATASET_LICENSE: &str = "MIT";
const MAX_QUERY_BYTES: usize = 64 * 1024;
const MAX_LABELED_LINES: usize = 10_000;
const HASH_HEX_LEN: usize = 64;

type DynError = Box<dyn Error>;
type LineMap = BTreeMap<String, BTreeSet<usize>>;

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Language {
    C,
    Cpp,
    Go,
    Java,
    Javascript,
    Typescript,
    Php,
    Ruby,
    Rust,
}

impl Language {
    pub(crate) const ALL: [Self; 9] = [
        Self::C,
        Self::Cpp,
        Self::Go,
        Self::Java,
        Self::Javascript,
        Self::Typescript,
        Self::Php,
        Self::Ruby,
        Self::Rust,
    ];

    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::C => "c",
            Self::Cpp => "cpp",
            Self::Go => "go",
            Self::Java => "java",
            Self::Javascript => "javascript",
            Self::Typescript => "typescript",
            Self::Php => "php",
            Self::Ruby => "ruby",
            Self::Rust => "rust",
        }
    }
}

impl std::str::FromStr for Language {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.to_ascii_lowercase().as_str() {
            "c" => Ok(Self::C),
            "cpp" | "c++" => Ok(Self::Cpp),
            "go" => Ok(Self::Go),
            "java" => Ok(Self::Java),
            "javascript" | "js" => Ok(Self::Javascript),
            "typescript" | "ts" => Ok(Self::Typescript),
            "php" => Ok(Self::Php),
            "ruby" | "rb" => Ok(Self::Ruby),
            "rust" | "rs" => Ok(Self::Rust),
            _ => Err(format!("unsupported language {value}")),
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct PrepareConfig<'a> {
    pub(crate) dataset_jsonl: &'a Path,
    pub(crate) source_artifact: &'a Path,
    pub(crate) source_revision: &'a str,
    pub(crate) source_url: &'a str,
    pub(crate) seed: &'a str,
    pub(crate) harness_revision: &'a str,
    pub(crate) harness_binary: &'a Path,
    pub(crate) languages: BTreeSet<Language>,
    pub(crate) tasks_per_language: usize,
    pub(crate) non_exact_per_language: usize,
    pub(crate) max_tasks_per_repository: usize,
    pub(crate) source_token_budget: usize,
    pub(crate) tokenizer: Tokenizer,
    pub(crate) repository_license_map: Option<&'a Path>,
    pub(crate) require_license_audit: bool,
    pub(crate) tasks_output: &'a Path,
    pub(crate) labels_output: &'a Path,
    pub(crate) receipt_output: &'a Path,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct SweBenchRecord {
    repo: String,
    instance_id: String,
    base_commit: String,
    patch: String,
    test_patch: String,
    problem_statement: String,
    #[serde(default)]
    hints_text: String,
    #[serde(default)]
    created_at: String,
    #[serde(default)]
    version: String,
    #[serde(rename = "FAIL_TO_PASS", default)]
    fail_to_pass: Vec<String>,
    #[serde(rename = "PASS_TO_PASS", default)]
    pass_to_pass: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub(crate) struct DevelopmentTask {
    schema_version: u32,
    dataset_kind: String,
    task_id: String,
    repository: RepositorySpec,
    query: String,
    language: Language,
    strata: TaskStrata,
    budget: Budget,
    source_record_blake3: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
struct RepositorySpec {
    url: String,
    revision: String,
    path_style: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
struct TaskStrata {
    exact_identifier: bool,
    task_shape: String,
    tags: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
struct Budget {
    kind: String,
    amount: usize,
    tokenizer: Tokenizer,
    token_count_exact: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub(crate) struct SealedLabel {
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
struct Region {
    path: String,
    start_line: usize,
    end_line: usize,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub(crate) struct PreparationReceipt {
    schema_version: u32,
    dataset_kind: String,
    harness: HarnessReceipt,
    source: SourceReceipt,
    selection: SelectionReceipt,
    labels: LabelReceipt,
    license_audit: LicenseAuditReceipt,
    artifacts: ArtifactReceipts,
    limitations: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct HarnessReceipt {
    revision: String,
    operating_system: String,
    architecture: String,
    debug_assertions: bool,
    binary_bytes: usize,
    binary_blake3: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct SourceReceipt {
    dataset: String,
    url: String,
    revision: String,
    dataset_license: String,
    source_artifact_bytes: usize,
    source_artifact_blake3: String,
    jsonl_export_bytes: usize,
    jsonl_export_blake3: String,
    canonical_records_blake3: String,
    input_records: usize,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct SelectionReceipt {
    algorithm: String,
    seed: String,
    requested_languages: Vec<Language>,
    tasks_per_language: usize,
    non_exact_per_language: usize,
    max_tasks_per_repository: usize,
    source_token_budget: usize,
    tokenizer: Tokenizer,
    token_count_exact: bool,
    eligible_candidates: usize,
    eligible_not_selected: usize,
    selected_tasks: usize,
    selected_repositories: usize,
    max_selected_tasks_per_repository: usize,
    language_tasks: BTreeMap<Language, usize>,
    exact_identifier_tasks: usize,
    non_exact_tasks: usize,
    skipped: BTreeMap<String, usize>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct LabelReceipt {
    method: String,
    core_regions: usize,
    core_lines: usize,
    optional_regions: usize,
    optional_lines: usize,
    core_lines_per_task_p50: usize,
    core_lines_per_task_p95: usize,
    core_lines_per_task_max: usize,
    optional_lines_per_task_p50: usize,
    optional_lines_per_task_p95: usize,
    optional_lines_per_task_max: usize,
    unobservable_added_files: usize,
    tasks_with_unobservable_added_files: usize,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct LicenseAuditReceipt {
    complete: bool,
    dataset_scope: String,
    repository_source_or_patch_vendored: bool,
    audited_revisions: usize,
    non_osi_repositories: Vec<String>,
    repositories: Vec<RepositoryLicense>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct ArtifactReceipts {
    tasks: ArtifactReceipt,
    sealed_labels: ArtifactReceipt,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct ArtifactReceipt {
    bytes: usize,
    blake3: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct RepositoryLicense {
    repository: String,
    spdx_id: String,
    source_revision: String,
    license_path: String,
    license_file_blake3: String,
    source_url: String,
}

#[derive(Debug, Clone)]
struct Candidate {
    task: DevelopmentTask,
    label: SealedLabel,
    repository: String,
    language: Language,
    exact_identifier: bool,
    selection_key: String,
}

#[derive(Debug)]
enum CandidateOutcome {
    Selected(Box<Candidate>),
    Skipped(&'static str),
}

#[derive(Debug, Default)]
struct PatchEvidence {
    primary: LineMap,
    optional: LineMap,
    language_weights: BTreeMap<Language, usize>,
    unobservable_added_files: usize,
}

pub(crate) fn prepare(config: &PrepareConfig<'_>) -> Result<PreparationReceipt, DynError> {
    validate_config(config)?;
    preflight_outputs([
        config.tasks_output,
        config.labels_output,
        config.receipt_output,
    ])?;

    let dataset_bytes = fs::read(config.dataset_jsonl)?;
    let source_artifact_bytes = fs::read(config.source_artifact)?;
    let harness_binary_bytes = fs::read(config.harness_binary)?;
    let records = parse_source_records(&dataset_bytes)?;
    let input_record_count = records.len();
    let canonical_records_blake3 = canonical_records_blake3(&records)?;
    let mut seen_ids = HashSet::new();
    let mut candidates = Vec::new();
    let mut skipped = BTreeMap::new();

    for (record, source_record_blake3) in records {
        if !seen_ids.insert(record.instance_id.clone()) {
            return Err(format!("duplicate dataset instance ID {}", record.instance_id).into());
        }
        match build_candidate(&record, &source_record_blake3, config)? {
            CandidateOutcome::Selected(candidate) => candidates.push(*candidate),
            CandidateOutcome::Skipped(reason) => {
                *skipped.entry(reason.to_owned()).or_insert(0) += 1
            }
        }
    }

    let eligible_candidate_count = candidates.len();
    if eligible_candidate_count + skipped.values().sum::<usize>() != input_record_count {
        return Err("candidate accounting does not reconcile to the input dataset".into());
    }
    let selected = select_candidates(candidates, config)?;
    let tasks = selected
        .iter()
        .map(|candidate| candidate.task.clone())
        .collect::<Vec<_>>();
    let labels = selected
        .iter()
        .map(|candidate| candidate.label.clone())
        .collect::<Vec<_>>();
    validate_task_label_bindings(&tasks, &labels)?;

    let task_bytes = serialize_jsonl(&tasks)?;
    let label_bytes = serialize_jsonl(&labels)?;
    let licenses = load_license_audit(config.repository_license_map, &selected)?;
    if config.require_license_audit && licenses.is_none() {
        return Err("--require-license-audit needs a complete repository license map".into());
    }

    let receipt = build_receipt(
        config,
        &dataset_bytes,
        &source_artifact_bytes,
        &harness_binary_bytes,
        &canonical_records_blake3,
        input_record_count,
        eligible_candidate_count,
        &selected,
        &labels,
        skipped,
        licenses,
        &task_bytes,
        &label_bytes,
    )?;
    let receipt_bytes = serde_json::to_vec_pretty(&receipt)?;

    write_new(config.tasks_output, &task_bytes, false)?;
    write_new(config.labels_output, &label_bytes, true)?;
    write_new(config.receipt_output, &receipt_bytes, false)?;
    Ok(receipt)
}

fn validate_config(config: &PrepareConfig<'_>) -> Result<(), DynError> {
    validate_revision(config.source_revision, "dataset revision")?;
    validate_revision(config.harness_revision, "harness revision")?;
    if !config.source_url.starts_with("https://")
        || !config.source_url.contains(config.source_revision)
    {
        return Err("dataset source URL must use https and bind the dataset revision".into());
    }
    if config.seed.trim().is_empty() {
        return Err("selection seed must not be empty".into());
    }
    if config.languages.len() < 2 {
        return Err("at least two language strata are required".into());
    }
    if config.tasks_per_language == 0 {
        return Err("tasks per language must be positive".into());
    }
    if config.non_exact_per_language >= config.tasks_per_language {
        return Err("non-exact quota must leave at least one exact task per language".into());
    }
    if config.max_tasks_per_repository == 0 {
        return Err("repository task cap must be positive".into());
    }
    if config.source_token_budget == 0 {
        return Err("source token budget must be positive".into());
    }
    if !config.tokenizer.is_exact() {
        return Err("development-set source budgets require an exact tokenizer".into());
    }
    Ok(())
}

fn preflight_outputs<const N: usize>(paths: [&Path; N]) -> Result<(), DynError> {
    let unique = paths
        .iter()
        .map(|path| path.as_os_str())
        .collect::<HashSet<_>>();
    if unique.len() != paths.len() {
        return Err("output paths must be distinct".into());
    }
    for path in paths {
        if path.exists() {
            return Err(format!("refusing to overwrite {}", path.display()).into());
        }
    }
    Ok(())
}

fn parse_source_records(bytes: &[u8]) -> Result<Vec<(SweBenchRecord, String)>, DynError> {
    let text = std::str::from_utf8(bytes)?;
    let mut records = Vec::new();
    for (index, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let record = serde_json::from_str(line)
            .map_err(|error| format!("invalid dataset JSONL line {}: {error}", index + 1))?;
        let canonical = serde_json::to_vec(&record)?;
        records.push((record, blake3_hex(&canonical)));
    }
    if records.is_empty() {
        return Err("dataset JSONL contains no records".into());
    }
    Ok(records)
}

fn canonical_records_blake3(records: &[(SweBenchRecord, String)]) -> Result<String, DynError> {
    let mut ordered = records.iter().map(|(record, _)| record).collect::<Vec<_>>();
    ordered.sort_by(|left, right| left.instance_id.cmp(&right.instance_id));
    let mut canonical = Vec::new();
    for record in ordered {
        serde_json::to_writer(&mut canonical, record)?;
        canonical.push(b'\n');
    }
    Ok(blake3_hex(&canonical))
}

fn build_candidate(
    record: &SweBenchRecord,
    source_record_blake3: &str,
    config: &PrepareConfig<'_>,
) -> Result<CandidateOutcome, DynError> {
    validate_repository(&record.repo)?;
    validate_revision(&record.base_commit, "task base commit")?;
    if record.problem_statement.trim().is_empty() {
        return Err(format!("{} has an empty problem statement", record.instance_id).into());
    }
    if record.problem_statement.len() > MAX_QUERY_BYTES {
        return Ok(CandidateOutcome::Skipped("query_exceeds_runtime_limit"));
    }
    if record.patch.is_empty() || record.test_patch.is_empty() {
        return Err(format!("{} has an empty patch field", record.instance_id).into());
    }

    let gold = extract_patch_evidence(&record.patch, false)
        .map_err(|error| format!("{} has an invalid gold patch: {error}", record.instance_id))?;
    let tests = extract_patch_evidence(&record.test_patch, true)
        .map_err(|error| format!("{} has an invalid test patch: {error}", record.instance_id))?;
    let Some(language) = infer_task_language(&record.repo, &gold.language_weights) else {
        return Ok(CandidateOutcome::Skipped(
            "unsupported_or_ambiguous_language",
        ));
    };
    if !config.languages.contains(&language) {
        return Ok(CandidateOutcome::Skipped("language_not_requested"));
    }

    let mut core = gold.primary;
    let mut optional = gold.optional;
    merge_line_maps(&mut optional, tests.primary);
    merge_line_maps(&mut optional, tests.optional);
    subtract_line_map(&mut optional, &core);
    if core.values().all(BTreeSet::is_empty) {
        return Ok(CandidateOutcome::Skipped("no_base_revision_core_region"));
    }
    let labeled_lines = line_map_len(&core)?.saturating_add(line_map_len(&optional)?);
    if labeled_lines > MAX_LABELED_LINES {
        return Ok(CandidateOutcome::Skipped("label_line_limit_exceeded"));
    }

    let core_regions = regions_from_lines(&mut core)?;
    let optional_regions = regions_from_lines(&mut optional)?;
    let exact_identifier = query_contains_exact_identifier(&record.problem_statement);
    let task_shape = if exact_identifier {
        "exact_identifier"
    } else {
        "behavioral"
    };
    let source_record_blake3 = source_record_blake3.to_owned();
    let task = DevelopmentTask {
        schema_version: SCHEMA_VERSION,
        dataset_kind: DATASET_KIND.into(),
        task_id: record.instance_id.clone(),
        repository: RepositorySpec {
            url: format!("https://github.com/{}.git", record.repo),
            revision: record.base_commit.clone(),
            path_style: "posix".into(),
        },
        query: record.problem_statement.clone(),
        language,
        strata: TaskStrata {
            exact_identifier,
            task_shape: task_shape.into(),
            tags: vec!["external".into(), "patch_ground_truth".into()],
        },
        budget: Budget {
            kind: "source_tokens".into(),
            amount: config.source_token_budget,
            tokenizer: config.tokenizer,
            token_count_exact: config.tokenizer.is_exact(),
        },
        source_record_blake3: source_record_blake3.clone(),
    };
    let label = SealedLabel {
        schema_version: SCHEMA_VERSION,
        task_id: record.instance_id.clone(),
        source_record_blake3,
        label_method: "base_diff_removed_or_insertion_context_v1".into(),
        core_regions,
        optional_regions,
        gold_patch_blake3: blake3_hex(record.patch.as_bytes()),
        test_patch_blake3: blake3_hex(record.test_patch.as_bytes()),
        unobservable_added_files: gold.unobservable_added_files + tests.unobservable_added_files,
    };
    Ok(CandidateOutcome::Selected(Box::new(Candidate {
        task,
        label,
        repository: record.repo.clone(),
        language,
        exact_identifier,
        selection_key: selection_key(config.seed, language, &record.instance_id),
    })))
}

fn validate_task_label_bindings(
    tasks: &[DevelopmentTask],
    labels: &[SealedLabel],
) -> Result<(), DynError> {
    if tasks.len() != labels.len() {
        return Err("task and label counts differ".into());
    }
    for (task, label) in tasks.iter().zip(labels) {
        if task.task_id != label.task_id || task.source_record_blake3 != label.source_record_blake3
        {
            return Err(format!("task/label binding mismatch for {}", task.task_id).into());
        }
    }
    Ok(())
}

fn load_license_audit(
    path: Option<&Path>,
    selected: &[Candidate],
) -> Result<Option<Vec<RepositoryLicense>>, DynError> {
    let Some(path) = path else {
        return Ok(None);
    };
    let mut values: Vec<RepositoryLicense> = serde_json::from_slice(&fs::read(path)?)?;
    let expected = selected
        .iter()
        .map(|candidate| {
            (
                candidate.repository.as_str(),
                candidate.task.repository.revision.as_str(),
            )
        })
        .collect::<BTreeSet<_>>();
    let mut actual = BTreeSet::new();
    for value in &values {
        validate_repository(&value.repository)?;
        validate_revision(&value.source_revision, "license source revision")?;
        validate_hex_hash(&value.license_file_blake3, "license file BLAKE3")?;
        normalize_diff_path(&value.license_path)?;
        if !value.source_url.starts_with("https://")
            || !value.source_url.contains(&value.source_revision)
        {
            return Err(format!(
                "license source URL must use https and bind revision for {}",
                value.repository
            )
            .into());
        }
        if value.spdx_id.trim().is_empty()
            || value.spdx_id == "NOASSERTION"
            || !actual.insert((value.repository.as_str(), value.source_revision.as_str()))
        {
            return Err(format!(
                "invalid or duplicate license entry for {} at {}",
                value.repository, value.source_revision
            )
            .into());
        }
    }
    if actual != expected {
        return Err(format!(
            "license map repository revisions differ from selection: expected {}, got {}",
            expected.len(),
            actual.len()
        )
        .into());
    }
    values.sort_by(|left, right| {
        left.repository
            .cmp(&right.repository)
            .then_with(|| left.source_revision.cmp(&right.source_revision))
    });
    Ok(Some(values))
}

#[allow(clippy::too_many_arguments)]
fn build_receipt(
    config: &PrepareConfig<'_>,
    dataset_bytes: &[u8],
    source_artifact_bytes: &[u8],
    harness_binary_bytes: &[u8],
    canonical_records_blake3: &str,
    input_record_count: usize,
    eligible_candidate_count: usize,
    selected: &[Candidate],
    labels: &[SealedLabel],
    skipped: BTreeMap<String, usize>,
    licenses: Option<Vec<RepositoryLicense>>,
    task_bytes: &[u8],
    label_bytes: &[u8],
) -> Result<PreparationReceipt, DynError> {
    let mut language_tasks = BTreeMap::new();
    let mut repositories = BTreeMap::<&str, usize>::new();
    let mut exact_identifier_tasks = 0usize;
    for candidate in selected {
        *language_tasks.entry(candidate.language).or_insert(0) += 1;
        *repositories
            .entry(candidate.repository.as_str())
            .or_insert(0) += 1;
        exact_identifier_tasks += usize::from(candidate.exact_identifier);
    }
    let core_regions = labels.iter().map(|label| label.core_regions.len()).sum();
    let optional_regions = labels
        .iter()
        .map(|label| label.optional_regions.len())
        .sum();
    let core_lines = labels.iter().try_fold(0usize, |total, label| {
        total
            .checked_add(region_line_count(&label.core_regions)?)
            .ok_or_else(|| -> DynError { "core label count overflow".into() })
    })?;
    let optional_lines = labels.iter().try_fold(0usize, |total, label| {
        total
            .checked_add(region_line_count(&label.optional_regions)?)
            .ok_or_else(|| -> DynError { "optional label count overflow".into() })
    })?;
    let unobservable_added_files = labels
        .iter()
        .map(|label| label.unobservable_added_files)
        .sum();
    let tasks_with_unobservable_added_files = labels
        .iter()
        .filter(|label| label.unobservable_added_files > 0)
        .count();
    let core_lines_per_task = labels
        .iter()
        .map(|label| region_line_count(&label.core_regions))
        .collect::<Result<Vec<_>, _>>()?;
    let optional_lines_per_task = labels
        .iter()
        .map(|label| region_line_count(&label.optional_regions))
        .collect::<Result<Vec<_>, _>>()?;
    let license_complete = licenses.is_some();
    let non_osi_repositories = licenses
        .as_deref()
        .unwrap_or_default()
        .iter()
        .filter(|license| license.spdx_id == "BUSL-1.1" || license.spdx_id.contains("LicenseRef-"))
        .map(|license| license.repository.as_str())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let audited_revisions = licenses.as_deref().unwrap_or_default().len();

    Ok(PreparationReceipt {
        schema_version: SCHEMA_VERSION,
        dataset_kind: DATASET_KIND.into(),
        harness: HarnessReceipt {
            revision: config.harness_revision.into(),
            operating_system: std::env::consts::OS.into(),
            architecture: std::env::consts::ARCH.into(),
            debug_assertions: cfg!(debug_assertions),
            binary_bytes: harness_binary_bytes.len(),
            binary_blake3: blake3_hex(harness_binary_bytes),
        },
        source: SourceReceipt {
            dataset: DATASET_NAME.into(),
            url: config.source_url.into(),
            revision: config.source_revision.into(),
            dataset_license: DATASET_LICENSE.into(),
            source_artifact_bytes: source_artifact_bytes.len(),
            source_artifact_blake3: blake3_hex(source_artifact_bytes),
            jsonl_export_bytes: dataset_bytes.len(),
            jsonl_export_blake3: blake3_hex(dataset_bytes),
            canonical_records_blake3: canonical_records_blake3.into(),
            input_records: input_record_count,
        },
        selection: SelectionReceipt {
            algorithm: "blake3_seeded_language_and_exact_stratified_v1".into(),
            seed: config.seed.into(),
            requested_languages: config.languages.iter().copied().collect(),
            tasks_per_language: config.tasks_per_language,
            non_exact_per_language: config.non_exact_per_language,
            max_tasks_per_repository: config.max_tasks_per_repository,
            source_token_budget: config.source_token_budget,
            tokenizer: config.tokenizer,
            token_count_exact: config.tokenizer.is_exact(),
            eligible_candidates: eligible_candidate_count,
            eligible_not_selected: eligible_candidate_count - selected.len(),
            selected_tasks: selected.len(),
            selected_repositories: repositories.len(),
            max_selected_tasks_per_repository: repositories.values().copied().max().unwrap_or(0),
            language_tasks,
            exact_identifier_tasks,
            non_exact_tasks: selected.len() - exact_identifier_tasks,
            skipped,
        },
        labels: LabelReceipt {
            method: "base_diff_removed_or_insertion_context_v1".into(),
            core_regions,
            core_lines,
            optional_regions,
            optional_lines,
            core_lines_per_task_p50: nearest_rank(&core_lines_per_task, 50),
            core_lines_per_task_p95: nearest_rank(&core_lines_per_task, 95),
            core_lines_per_task_max: core_lines_per_task.iter().copied().max().unwrap_or(0),
            optional_lines_per_task_p50: nearest_rank(&optional_lines_per_task, 50),
            optional_lines_per_task_p95: nearest_rank(&optional_lines_per_task, 95),
            optional_lines_per_task_max: optional_lines_per_task.iter().copied().max().unwrap_or(0),
            unobservable_added_files,
            tasks_with_unobservable_added_files,
        },
        license_audit: LicenseAuditReceipt {
            complete: license_complete,
            dataset_scope: "Dataset metadata is MIT; repository source and patches remain governed by each upstream repository license and are not vendored.".into(),
            repository_source_or_patch_vendored: false,
            audited_revisions,
            non_osi_repositories,
            repositories: licenses.unwrap_or_default(),
        },
        artifacts: ArtifactReceipts {
            tasks: ArtifactReceipt {
                bytes: task_bytes.len(),
                blake3: blake3_hex(task_bytes),
            },
            sealed_labels: ArtifactReceipt {
                bytes: label_bytes.len(),
                blake3: blake3_hex(label_bytes),
            },
        },
        limitations: vec![
            "Gold-patch base lines are deterministic retrieval labels, not proof that every changed line is causally required.".into(),
            "Purely added files have no base-revision range and are counted but not labeled as retrievable evidence.".into(),
            "Test-patch and documentation ranges are optional evidence; generated, vendored, snapshot, and lock-file ranges are not core.".into(),
            "The public benchmark may be present in model training data; this development set cannot serve as a sealed product holdout.".into(),
            "The pinned dataset contains 300 tasks from 41 repository identities although the project overview describes 42 repositories.".into(),
            "The JSONL export is independently hashed because the preparation tool does not parse the source Parquet artifact.".into(),
        ],
    })
}

fn region_line_count(regions: &[Region]) -> Result<usize, DynError> {
    regions.iter().try_fold(0usize, |total, region| {
        let length = region
            .end_line
            .checked_sub(region.start_line)
            .and_then(|length| length.checked_add(1))
            .ok_or_else(|| -> DynError { "region line count overflow".into() })?;
        total
            .checked_add(length)
            .ok_or_else(|| "region line count overflow".into())
    })
}

fn nearest_rank(values: &[usize], percentile: usize) -> usize {
    if values.is_empty() || percentile == 0 {
        return 0;
    }
    let mut sorted = values.to_vec();
    sorted.sort_unstable();
    let rank = percentile.saturating_mul(sorted.len()).saturating_add(99) / 100;
    sorted[rank.saturating_sub(1).min(sorted.len() - 1)]
}

fn validate_repository(repository: &str) -> Result<(), DynError> {
    let mut parts = repository.split('/');
    let owner = parts.next().unwrap_or_default();
    let name = parts.next().unwrap_or_default();
    if owner.is_empty()
        || name.is_empty()
        || parts.next().is_some()
        || !repository
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'-' | b'_' | b'.'))
    {
        return Err(format!("invalid GitHub repository identity {repository}").into());
    }
    Ok(())
}

fn validate_revision(revision: &str, label: &str) -> Result<(), DynError> {
    if revision.len() != 40 || !revision.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(format!("{label} must be a 40-character Git object ID").into());
    }
    Ok(())
}

fn validate_hex_hash(value: &str, label: &str) -> Result<(), DynError> {
    if value.len() != HASH_HEX_LEN || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(format!("{label} must be a {HASH_HEX_LEN}-character hexadecimal hash").into());
    }
    Ok(())
}

fn serialize_jsonl<T: Serialize>(values: &[T]) -> Result<Vec<u8>, DynError> {
    let mut output = Vec::new();
    for value in values {
        serde_json::to_writer(&mut output, value)?;
        output.push(b'\n');
    }
    Ok(output)
}

fn write_new(path: &Path, bytes: &[u8], private: bool) -> Result<(), DynError> {
    #[cfg(not(unix))]
    let _ = private;
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent)?;
    }
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(if private { 0o600 } else { 0o644 });
    }
    let mut file = options.open(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}

fn blake3_hex(bytes: &[u8]) -> String {
    blake3::hash(bytes).to_hex().to_string()
}

#[cfg(test)]
#[path = "swe_bench_multilingual_tests.rs"]
mod tests;
