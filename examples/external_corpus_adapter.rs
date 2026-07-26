use std::{
    collections::{BTreeMap, HashSet},
    error::Error,
    fs,
    path::{Component, Path, PathBuf},
    process::Command,
};

use clap::{Parser, Subcommand};
use serde::{Deserialize, Serialize};

const LOCK_SCHEMA_VERSION: u32 = 1;
const MANIFEST_SCHEMA_VERSION: u32 = 4;
const DEFAULT_TOKEN_BUDGET: usize = 2_000;

#[derive(Debug, Parser)]
#[command(about = "Convert pinned public retrieval corpora into LeanToken manifests")]
struct Args {
    /// Pinned corpus metadata and accepted task families.
    #[arg(long, default_value = "benchmarks/external_corpora.json")]
    lock: PathBuf,
    #[command(subcommand)]
    command: AdapterCommand,
}

#[derive(Debug, Subcommand)]
enum AdapterCommand {
    /// Convert Semble repository annotations.
    Semble {
        /// Checkout of the pinned Semble dataset revision.
        #[arg(long)]
        source: PathBuf,
        #[arg(long)]
        output: PathBuf,
        /// Include only these repository names.
        #[arg(long)]
        repository: Vec<String>,
        /// Bound the total converted tasks for a diagnostic run.
        #[arg(long)]
        limit: Option<usize>,
        #[arg(long, default_value_t = DEFAULT_TOKEN_BUDGET)]
        token_budget: usize,
    },
    /// Convert supported Sverklo primitive tasks.
    Sverklo {
        /// Checkout of the pinned sverklo-bench dataset revision.
        #[arg(long)]
        source: PathBuf,
        #[arg(long)]
        output: PathBuf,
        /// Bound the total converted tasks for a diagnostic run.
        #[arg(long)]
        limit: Option<usize>,
        #[arg(long, default_value_t = DEFAULT_TOKEN_BUDGET)]
        token_budget: usize,
    },
}

#[derive(Debug, Deserialize)]
struct CorpusLock {
    schema_version: u32,
    frozen_at: String,
    semble: SembleLock,
    sverklo: SverkloLock,
}

#[derive(Debug, Deserialize)]
struct SembleLock {
    dataset_url: String,
    dataset_revision: String,
    dataset_license: String,
    repositories_file: String,
    annotations_directory: String,
    prompt_provenance: String,
    label_provenance: String,
    limitations: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct SverkloLock {
    dataset_url: String,
    dataset_revision: String,
    dataset_license: String,
    tasks_file: String,
    repository: LockedRepository,
    supported_categories: Vec<String>,
    prompt_provenance: String,
    label_provenance: String,
    limitations: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct LockedRepository {
    name: String,
    url: String,
    directory: String,
    revision: String,
    language: String,
}

#[derive(Debug, Deserialize)]
struct SembleRepository {
    name: String,
    language: String,
    url: String,
    revision: String,
}

#[derive(Debug, Deserialize)]
struct SembleAnnotation {
    query: String,
    relevant: Vec<SembleLabel>,
    #[serde(default)]
    secondary: Vec<SembleLabel>,
    category: String,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum SembleLabel {
    File(String),
    Region {
        path: String,
        start_line: usize,
        end_line: usize,
    },
}

#[derive(Debug, Deserialize)]
struct SverkloTask {
    id: String,
    category: String,
    dataset: String,
    query: String,
    expected: SverkloExpected,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum SverkloExpected {
    Locations {
        locations: Vec<SverkloLocation>,
    },
    Deps {
        imports: Vec<String>,
        importers: Vec<String>,
    },
    Names {
        names: Vec<String>,
    },
}

#[derive(Debug, Deserialize)]
struct SverkloLocation {
    file: String,
    line: usize,
}

#[derive(Debug, Serialize)]
struct Manifest {
    schema_version: u32,
    dataset_kind: &'static str,
    frozen_at: String,
    candidate_revision: Option<String>,
    evaluation_protocol: &'static str,
    reclassification_rule: &'static str,
    description: String,
    rg_max_lines_per_query: usize,
    corpora: Vec<Corpus>,
}

#[derive(Debug, Serialize)]
struct Corpus {
    name: String,
    url: String,
    directory: String,
    base_revision: String,
    fix_commit: Option<String>,
    issue_url: Option<String>,
    prompt_provenance: String,
    label_provenance: String,
    dataset_url: String,
    dataset_revision: String,
    dataset_license: String,
    external_limitations: Vec<String>,
    tasks: Vec<Task>,
}

#[derive(Debug, Serialize)]
struct Task {
    id: String,
    prompt: String,
    languages: Vec<String>,
    task_shapes: Vec<String>,
    rg_queries: Vec<String>,
    relevant_files: Vec<RelevantFile>,
    token_budget: usize,
}

#[derive(Debug, Serialize)]
struct RelevantFile {
    path: String,
    line_anchors: Vec<usize>,
}

fn main() -> Result<(), Box<dyn Error>> {
    let args = Args::parse();
    let lock: CorpusLock = serde_json::from_slice(&fs::read(&args.lock)?)?;
    validate_lock(&lock)?;

    let (manifest, skipped) = match args.command {
        AdapterCommand::Semble {
            source,
            output,
            repository,
            limit,
            token_budget,
        } => {
            verify_source_revision(&source, &lock.semble.dataset_revision)?;
            let manifest = convert_semble(
                &source,
                &lock,
                &repository.into_iter().collect(),
                limit,
                token_budget,
            )?;
            write_manifest(&output, &manifest)?;
            (manifest, 0)
        }
        AdapterCommand::Sverklo {
            source,
            output,
            limit,
            token_budget,
        } => {
            verify_source_revision(&source, &lock.sverklo.dataset_revision)?;
            let (manifest, skipped) = convert_sverklo(&source, &lock, limit, token_budget)?;
            write_manifest(&output, &manifest)?;
            (manifest, skipped)
        }
    };

    eprintln!(
        "converted {} corpora and {} tasks; skipped {skipped} unsupported tasks",
        manifest.corpora.len(),
        manifest
            .corpora
            .iter()
            .map(|corpus| corpus.tasks.len())
            .sum::<usize>()
    );
    Ok(())
}

fn validate_lock(lock: &CorpusLock) -> Result<(), Box<dyn Error>> {
    if lock.schema_version != LOCK_SCHEMA_VERSION {
        return Err(format!(
            "unsupported external corpus lock schema {}",
            lock.schema_version
        )
        .into());
    }
    if lock.frozen_at.trim().is_empty() {
        return Err("external corpus lock requires frozen_at".into());
    }
    for (name, url, revision, license, prompt, labels) in [
        (
            "semble",
            lock.semble.dataset_url.as_str(),
            lock.semble.dataset_revision.as_str(),
            lock.semble.dataset_license.as_str(),
            lock.semble.prompt_provenance.as_str(),
            lock.semble.label_provenance.as_str(),
        ),
        (
            "sverklo",
            lock.sverklo.dataset_url.as_str(),
            lock.sverklo.dataset_revision.as_str(),
            lock.sverklo.dataset_license.as_str(),
            lock.sverklo.prompt_provenance.as_str(),
            lock.sverklo.label_provenance.as_str(),
        ),
    ] {
        if [url, license, prompt, labels]
            .into_iter()
            .any(str::is_empty)
        {
            return Err(format!("{name} corpus lock has an empty required field").into());
        }
        validate_revision(revision)?;
    }
    validate_repository(&lock.sverklo.repository)?;
    if lock.sverklo.supported_categories.is_empty() {
        return Err("sverklo corpus lock requires supported_categories".into());
    }
    for path in [
        lock.semble.repositories_file.as_str(),
        lock.semble.annotations_directory.as_str(),
        lock.sverklo.tasks_file.as_str(),
    ] {
        validate_relative_path(path)?;
    }
    Ok(())
}

fn convert_semble(
    source: &Path,
    lock: &CorpusLock,
    repositories: &HashSet<String>,
    limit: Option<usize>,
    token_budget: usize,
) -> Result<Manifest, Box<dyn Error>> {
    validate_budget(token_budget)?;
    let repository_path = source.join(&lock.semble.repositories_file);
    let repository_specs: Vec<SembleRepository> =
        serde_json::from_slice(&fs::read(repository_path)?)?;
    let annotations_root = source.join(&lock.semble.annotations_directory);
    let mut corpora = Vec::new();
    let mut remaining = limit.unwrap_or(usize::MAX);

    for repository in repository_specs {
        if !repositories.is_empty() && !repositories.contains(&repository.name) {
            continue;
        }
        if remaining == 0 {
            break;
        }
        validate_repository_fields(
            &repository.name,
            &repository.url,
            &repository.name,
            &repository.revision,
        )?;
        let annotation_path = annotations_root.join(format!("{}.json", repository.name));
        let annotations: Vec<SembleAnnotation> =
            serde_json::from_slice(&fs::read(&annotation_path)?)?;
        let mut tasks = Vec::new();
        for (index, annotation) in annotations.into_iter().enumerate() {
            if remaining == 0 {
                break;
            }
            let task = convert_semble_annotation(&repository, annotation, index, token_budget)?;
            tasks.push(task);
            remaining -= 1;
        }
        if tasks.is_empty() {
            continue;
        }
        corpora.push(Corpus {
            name: repository.name.clone(),
            url: repository.url,
            directory: repository.name,
            base_revision: repository.revision,
            fix_commit: None,
            issue_url: None,
            prompt_provenance: lock.semble.prompt_provenance.clone(),
            label_provenance: lock.semble.label_provenance.clone(),
            dataset_url: lock.semble.dataset_url.clone(),
            dataset_revision: lock.semble.dataset_revision.clone(),
            dataset_license: lock.semble.dataset_license.clone(),
            external_limitations: lock.semble.limitations.clone(),
            tasks,
        });
    }
    if corpora.is_empty() {
        return Err("Semble filters selected no annotated repositories".into());
    }
    if !repositories.is_empty() {
        let found = corpora
            .iter()
            .map(|corpus| corpus.name.as_str())
            .collect::<HashSet<_>>();
        let mut missing = repositories
            .iter()
            .filter(|name| !found.contains(name.as_str()))
            .cloned()
            .collect::<Vec<_>>();
        missing.sort();
        if !missing.is_empty() {
            return Err(format!(
                "unknown or empty Semble repositories: {}",
                missing.join(", ")
            )
            .into());
        }
    }
    Ok(external_manifest(
        &lock.frozen_at,
        "semble",
        "Pinned Semble public retrieval annotations converted without changing labels.",
        corpora,
    ))
}

fn convert_semble_annotation(
    repository: &SembleRepository,
    annotation: SembleAnnotation,
    index: usize,
    token_budget: usize,
) -> Result<Task, Box<dyn Error>> {
    if annotation.query.trim().is_empty() || annotation.category.trim().is_empty() {
        return Err(format!("{} annotation {index} has an empty field", repository.name).into());
    }
    if annotation.relevant.is_empty() {
        return Err(format!(
            "{} annotation {index} has no primary labels",
            repository.name
        )
        .into());
    }
    let _secondary_labels_are_intentionally_not_promoted = annotation.secondary.len();
    let relevant_files = labels_to_relevant_files(annotation.relevant)?;
    Ok(Task {
        id: format!("semble:{}:{index:03}", repository.name),
        prompt: annotation.query.clone(),
        languages: vec![repository.language.clone()],
        task_shapes: vec![format!("semble_{}", annotation.category)],
        rg_queries: vec![annotation.query],
        relevant_files,
        token_budget,
    })
}

fn labels_to_relevant_files(labels: Vec<SembleLabel>) -> Result<Vec<RelevantFile>, Box<dyn Error>> {
    let mut by_path = BTreeMap::<String, Vec<usize>>::new();
    for label in labels {
        let (path, anchor) = match label {
            SembleLabel::File(path) => (path, None),
            SembleLabel::Region {
                path,
                start_line,
                end_line,
            } => {
                if start_line == 0 || end_line < start_line {
                    return Err(
                        format!("invalid Semble region {path}:{start_line}-{end_line}").into(),
                    );
                }
                (path, Some(start_line))
            }
        };
        validate_relative_path(&path)?;
        if let Some(anchor) = anchor {
            by_path.entry(path).or_default().push(anchor);
        } else {
            by_path.entry(path).or_default();
        }
    }
    Ok(by_path
        .into_iter()
        .map(|(path, mut line_anchors)| {
            line_anchors.sort_unstable();
            line_anchors.dedup();
            RelevantFile { path, line_anchors }
        })
        .collect())
}

fn convert_sverklo(
    source: &Path,
    lock: &CorpusLock,
    limit: Option<usize>,
    token_budget: usize,
) -> Result<(Manifest, usize), Box<dyn Error>> {
    validate_budget(token_budget)?;
    let tasks = read_jsonl::<SverkloTask>(&source.join(&lock.sverklo.tasks_file))?;
    let supported = lock
        .sverklo
        .supported_categories
        .iter()
        .map(String::as_str)
        .collect::<HashSet<_>>();
    let mut converted = Vec::new();
    let mut skipped = 0;
    for task in tasks {
        if !supported.contains(task.category.as_str()) {
            skipped += 1;
            continue;
        }
        if converted.len() == limit.unwrap_or(usize::MAX) {
            break;
        }
        converted.push(convert_sverklo_task(
            task,
            &lock.sverklo.repository,
            token_budget,
        )?);
    }
    if converted.is_empty() {
        return Err("Sverklo lock selected no coordinate-bearing tasks".into());
    }
    let repository = &lock.sverklo.repository;
    let corpus = Corpus {
        name: repository.name.clone(),
        url: repository.url.clone(),
        directory: repository.directory.clone(),
        base_revision: repository.revision.clone(),
        fix_commit: None,
        issue_url: None,
        prompt_provenance: lock.sverklo.prompt_provenance.clone(),
        label_provenance: lock.sverklo.label_provenance.clone(),
        dataset_url: lock.sverklo.dataset_url.clone(),
        dataset_revision: lock.sverklo.dataset_revision.clone(),
        dataset_license: lock.sverklo.dataset_license.clone(),
        external_limitations: lock.sverklo.limitations.clone(),
        tasks: converted,
    };
    Ok((
        external_manifest(
            &lock.frozen_at,
            "sverklo",
            "Pinned Sverklo coordinate-bearing primitive tasks; unsupported name-only tasks are excluded.",
            vec![corpus],
        ),
        skipped,
    ))
}

fn convert_sverklo_task(
    task: SverkloTask,
    repository: &LockedRepository,
    token_budget: usize,
) -> Result<Task, Box<dyn Error>> {
    if task.id.trim().is_empty()
        || task.query.trim().is_empty()
        || task.category.trim().is_empty()
        || task.dataset.trim().is_empty()
    {
        return Err("supported Sverklo task has an empty required field".into());
    }
    let relevant_files = match task.expected {
        SverkloExpected::Locations { locations } => {
            let mut by_path = BTreeMap::<String, Vec<usize>>::new();
            for location in locations {
                validate_relative_path(&location.file)?;
                if location.line == 0 {
                    return Err(format!("{} has a zero line label", task.id).into());
                }
                by_path
                    .entry(location.file)
                    .or_default()
                    .push(location.line);
            }
            by_path
                .into_iter()
                .map(|(path, mut line_anchors)| {
                    line_anchors.sort_unstable();
                    line_anchors.dedup();
                    RelevantFile { path, line_anchors }
                })
                .collect()
        }
        SverkloExpected::Deps { imports, importers } => labels_to_relevant_files(
            imports
                .into_iter()
                .chain(importers)
                .map(SembleLabel::File)
                .collect(),
        )?,
        SverkloExpected::Names { names } => {
            return Err(format!(
                "{} unexpectedly selected a name-only task with {} labels",
                task.id,
                names.len()
            )
            .into());
        }
    };
    if relevant_files.is_empty() {
        return Err(format!("{} has no coordinate-bearing labels", task.id).into());
    }
    Ok(Task {
        id: format!("sverklo:{}", task.id),
        prompt: task.query.clone(),
        languages: vec![repository.language.clone()],
        task_shapes: vec![format!("sverklo_{}", task.category.to_ascii_lowercase())],
        rg_queries: vec![task.query],
        relevant_files,
        token_budget,
    })
}

fn external_manifest(
    frozen_at: &str,
    corpus_name: &str,
    description: &str,
    corpora: Vec<Corpus>,
) -> Manifest {
    Manifest {
        schema_version: MANIFEST_SCHEMA_VERSION,
        dataset_kind: "external_retrieval_corpus",
        frozen_at: frozen_at.to_owned(),
        candidate_revision: None,
        evaluation_protocol: "External labels are converted deterministically. Production ranking is unchanged, and reports are diagnostic until a separate promotion commitment is frozen.",
        reclassification_rule: "Do not edit converted labels after observing LeanToken output; update the pinned dataset revision and record a new corpus lock instead.",
        description: format!("{corpus_name}: {description}"),
        rg_max_lines_per_query: 200,
        corpora,
    }
}

fn validate_repository(repository: &LockedRepository) -> Result<(), Box<dyn Error>> {
    if repository.language.trim().is_empty() {
        return Err("locked repository requires language".into());
    }
    validate_repository_fields(
        &repository.name,
        &repository.url,
        &repository.directory,
        &repository.revision,
    )
}

fn validate_repository_fields(
    name: &str,
    url: &str,
    directory: &str,
    revision: &str,
) -> Result<(), Box<dyn Error>> {
    if name.trim().is_empty() || url.trim().is_empty() || directory.trim().is_empty() {
        return Err("repository has an empty required field".into());
    }
    validate_relative_path(directory)?;
    validate_revision(revision)
}

fn validate_revision(revision: &str) -> Result<(), Box<dyn Error>> {
    if revision.len() != 40 || !revision.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(
            format!("revision must be a full 40-character Git object ID: {revision}").into(),
        );
    }
    Ok(())
}

fn validate_budget(token_budget: usize) -> Result<(), Box<dyn Error>> {
    if token_budget == 0 || token_budget > 1_000_000 {
        return Err(format!("token budget must be within 1..=1000000: {token_budget}").into());
    }
    Ok(())
}

fn validate_relative_path(path: &str) -> Result<(), Box<dyn Error>> {
    let parsed = Path::new(path);
    if path.trim().is_empty()
        || path.contains('\\')
        || parsed.is_absolute()
        || parsed
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(format!("path must be normalized and repository-relative: {path}").into());
    }
    Ok(())
}

fn verify_source_revision(source: &Path, expected: &str) -> Result<(), Box<dyn Error>> {
    let output = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(source)
        .output()?;
    if !output.status.success() {
        return Err(format!("cannot resolve dataset revision in {}", source.display()).into());
    }
    let actual = String::from_utf8(output.stdout)?.trim().to_owned();
    if actual != expected {
        return Err(format!(
            "dataset checkout {} is at {actual}, expected {expected}",
            source.display()
        )
        .into());
    }
    Ok(())
}

fn read_jsonl<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<Vec<T>, Box<dyn Error>> {
    let bytes = fs::read(path)?;
    bytes
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.iter().all(u8::is_ascii_whitespace))
        .enumerate()
        .map(|(index, line)| {
            serde_json::from_slice(line)
                .map_err(|error| format!("{} line {}: {error}", path.display(), index + 1).into())
        })
        .collect()
}

fn write_manifest(path: &Path, manifest: &Manifest) -> Result<(), Box<dyn Error>> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent)?;
    }
    let mut json = serde_json::to_string_pretty(manifest)?;
    json.push('\n');
    fs::write(path, json)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repository() -> SembleRepository {
        SembleRepository {
            name: "demo".into(),
            language: "rust".into(),
            url: "https://example.invalid/demo.git".into(),
            revision: "0123456789abcdef0123456789abcdef01234567".into(),
        }
    }

    #[test]
    fn semble_primary_labels_preserve_file_and_region_evidence() {
        let task = convert_semble_annotation(
            &repository(),
            SembleAnnotation {
                query: "DemoType".into(),
                relevant: vec![
                    SembleLabel::File("src/lib.rs".into()),
                    SembleLabel::Region {
                        path: "src/model.rs".into(),
                        start_line: 10,
                        end_line: 20,
                    },
                ],
                secondary: vec![SembleLabel::File("tests/demo.rs".into())],
                category: "symbol".into(),
            },
            2,
            2_000,
        )
        .expect("convert");

        assert_eq!(task.id, "semble:demo:002");
        assert_eq!(task.relevant_files.len(), 2);
        assert_eq!(task.relevant_files[0].path, "src/lib.rs");
        assert!(task.relevant_files[0].line_anchors.is_empty());
        assert_eq!(task.relevant_files[1].line_anchors, vec![10]);
    }

    #[test]
    fn sverklo_locations_group_and_deduplicate_line_anchors() {
        let task = convert_sverklo_task(
            SverkloTask {
                id: "sv-p2-01".into(),
                category: "P2".into(),
                dataset: "sverklo".into(),
                query: "discoverFiles".into(),
                expected: SverkloExpected::Locations {
                    locations: vec![
                        SverkloLocation {
                            file: "src/indexer.ts".into(),
                            line: 12,
                        },
                        SverkloLocation {
                            file: "src/indexer.ts".into(),
                            line: 12,
                        },
                    ],
                },
            },
            &LockedRepository {
                name: "sverklo".into(),
                url: "https://example.invalid/sverklo.git".into(),
                directory: "sverklo".into(),
                revision: "0123456789abcdef0123456789abcdef01234567".into(),
                language: "typescript".into(),
            },
            2_000,
        )
        .expect("convert");

        assert_eq!(task.relevant_files.len(), 1);
        assert_eq!(task.relevant_files[0].line_anchors, vec![12]);
    }

    #[test]
    fn invalid_paths_and_unpinned_revisions_are_rejected() {
        assert!(validate_relative_path("../secret").is_err());
        assert!(validate_relative_path(r"src\\lib.rs").is_err());
        assert!(validate_revision("main").is_err());
        assert!(validate_revision("0123456789abcdef0123456789abcdef01234567").is_ok());
    }
}
