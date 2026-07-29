use std::{
    env,
    error::Error,
    fs,
    path::{Path, PathBuf},
};

use leantoken::{
    Config, FileOperation, FilesRequest, OutlineRequest, SearchMode, SearchRequest,
    services::Services,
};
use serde::{Deserialize, Serialize};

type AnyResult<T> = Result<T, Box<dyn Error>>;

#[derive(Debug, Deserialize)]
struct Manifest {
    schema_version: usize,
    experiment: String,
    fixture: Fixture,
    tasks: Tasks,
    acceptance: Acceptance,
}

#[derive(Debug, Deserialize)]
struct Fixture {
    root: PathBuf,
    tree_blake3: String,
    tokenizer: String,
    generated_reference_callers: usize,
}

#[derive(Debug, Deserialize)]
struct Tasks {
    files: FilesTask,
    outline: OutlineTask,
    search: SearchTask,
}

#[derive(Debug, Deserialize)]
struct FilesTask {
    depth: usize,
    max_results: usize,
}

#[derive(Debug, Deserialize)]
struct OutlineTask {
    paths: Vec<String>,
    max_results: usize,
    max_tokens: usize,
}

#[derive(Debug, Deserialize)]
struct SearchTask {
    query: String,
    max_results: usize,
    max_tokens: usize,
    context_lines: usize,
}

#[derive(Debug, Deserialize, Serialize)]
struct Acceptance {
    require_default_membership_parity: bool,
    require_verifiable_path_range_hash: bool,
    require_zero_retry_proxy_delta: bool,
    require_each_complete_response_token_delta_negative: bool,
}

#[derive(Debug, Serialize)]
struct ProjectionResult {
    baseline_total_response_tokens: usize,
    compact_total_response_tokens: usize,
    complete_response_token_delta: isize,
    baseline_source_tokens: usize,
    compact_source_tokens: usize,
    membership_or_concept_parity: bool,
    verifiable: bool,
    baseline_retry_proxy: usize,
    compact_retry_proxy: usize,
}

#[derive(Debug, Serialize)]
struct Report {
    schema_version: usize,
    experiment: String,
    source_revision: String,
    source_dirty: bool,
    build_profile: &'static str,
    operating_system: &'static str,
    architecture: &'static str,
    manifest_blake3: String,
    fixture_tree_blake3: String,
    tokenizer: String,
    token_count_exact: bool,
    visible_entries: usize,
    generated_reference_callers: usize,
    files: ProjectionResult,
    outline: ProjectionResult,
    search: ProjectionResult,
    aggregate_baseline_tokens: usize,
    aggregate_compact_tokens: usize,
    aggregate_token_delta: isize,
    retry_proxy_delta: isize,
    acceptance: Acceptance,
    passed: bool,
    limitations: Vec<&'static str>,
}

#[tokio::main]
async fn main() -> AnyResult<()> {
    let manifest_path = option_value("--manifest")
        .unwrap_or_else(|| PathBuf::from("benchmarks/compact_projection_tasks.json"));
    let output_path = option_value("--output")
        .unwrap_or_else(|| PathBuf::from("target/compact_projection_report.json"));
    let repository_root = option_value("--repository-root").unwrap_or_else(|| PathBuf::from("."));
    let source_revision =
        option_string("--source-revision").unwrap_or_else(|| "working-tree".into());
    let source_dirty = env::args().any(|argument| argument == "--source-dirty");

    let manifest_bytes = fs::read(&manifest_path)?;
    let manifest: Manifest = serde_json::from_slice(&manifest_bytes)?;
    if manifest.schema_version != 1
        || manifest.experiment != "compact-projections-v1"
        || manifest.fixture.generated_reference_callers == 0
    {
        return Err("invalid compact projection manifest".into());
    }

    let fixture_source = repository_root.join(&manifest.fixture.root);
    let fixture_hash = fixture_manifest_hash(&canonical_fixture_files(&fixture_source)?);
    if fixture_hash != manifest.fixture.tree_blake3 {
        return Err("fixture tree commitment mismatch".into());
    }

    let temp = tempfile::tempdir()?;
    let root = temp.path().join("repo");
    copy_tree(&fixture_source, &root)?;
    let generated_path = root.join("src/compact_projection_fixture.rs");
    let mut generated = String::from("pub fn compact_target() -> usize {\n    42\n}\n\n");
    for index in 0..manifest.fixture.generated_reference_callers {
        generated.push_str(&format!(
            "pub fn compact_caller_{index:03}() -> usize {{\n    compact_target()\n}}\n\n"
        ));
    }
    fs::write(generated_path, generated)?;

    let config = Config::discover(&root, Some(temp.path().join("index.sqlite")))?;
    if config.tokenizer.name() != manifest.fixture.tokenizer {
        return Err("tokenizer commitment mismatch".into());
    }
    let tokenizer = config.tokenizer;
    let services = Services::open(config)?;
    services.index(true).await?;

    let files_request = FilesRequest {
        operation: FileOperation::Tree,
        path: None,
        query: None,
        pattern: None,
        max_results: Some(manifest.tasks.files.max_results),
        cursor: None,
        depth: Some(manifest.tasks.files.depth),
    };
    let files_full = services.files(files_request.clone()).await?;
    let files_compact = services.files_paths(files_request).await?;
    let files_parity = files_compact.paths
        == files_full
            .entries
            .iter()
            .map(|entry| entry.path.clone())
            .collect::<Vec<_>>();
    let files_verifiable = files_compact.paths.iter().all(|path| !path.is_empty());
    let files = projection_result(
        files_full.meta.total_response_tokens,
        files_compact.meta.total_response_tokens,
        files_full.meta.source_tokens,
        files_compact.meta.source_tokens,
        files_parity,
        files_verifiable,
    );

    let outline_request = OutlineRequest {
        paths: manifest.tasks.outline.paths.clone(),
        symbol_name: None,
        symbol_kind: None,
        max_results: Some(manifest.tasks.outline.max_results),
        max_tokens: Some(manifest.tasks.outline.max_tokens),
        receipt_id: None,
        cursor: None,
    };
    let outline_full = services.outline(outline_request.clone()).await?;
    let outline_compact = services.outline_signatures(outline_request).await?;
    let full_symbols = outline_full
        .files
        .iter()
        .flat_map(|file| {
            file.symbols.iter().map(|symbol| {
                (
                    file.path.as_str(),
                    symbol.name.as_str(),
                    symbol.start_line,
                    symbol.end_line,
                )
            })
        })
        .collect::<Vec<_>>();
    let compact_symbols = outline_compact
        .files
        .iter()
        .flat_map(|file| {
            file.signatures.iter().map(|symbol| {
                (
                    file.path.as_str(),
                    symbol.name.as_str(),
                    symbol.start_line,
                    symbol.end_line,
                )
            })
        })
        .collect::<Vec<_>>();
    let outline_parity = compact_symbols == full_symbols;
    let outline_verifiable = outline_compact.files.iter().all(|file| {
        file.content_hash.len() == 32
            && file
                .signatures
                .iter()
                .all(|symbol| symbol.start_line > 0 && symbol.end_line >= symbol.start_line)
    });
    let outline = projection_result(
        outline_full.meta.total_response_tokens,
        outline_compact.meta.total_response_tokens,
        outline_full.meta.source_tokens,
        outline_compact.meta.source_tokens,
        outline_parity,
        outline_verifiable,
    );

    let search_request = SearchRequest {
        query: manifest.tasks.search.query.clone(),
        mode: SearchMode::Auto,
        include_paths: Vec::new(),
        exclude_paths: Vec::new(),
        focus_paths: Vec::new(),
        max_results: Some(manifest.tasks.search.max_results),
        max_tokens: Some(manifest.tasks.search.max_tokens),
        context_lines: Some(manifest.tasks.search.context_lines),
        case_sensitive: false,
        all_occurrences: false,
        prefer_structural: true,
        receipt_id: None,
        query_receipt: None,
        cursor: None,
    };
    let search_full = services.search(search_request.clone()).await?;
    let search_compact = services.search_grouped(search_request).await?;
    let represented_hits = search_compact
        .groups
        .iter()
        .map(|group| group.total_hits)
        .sum::<usize>();
    let full_references = search_full
        .hits
        .iter()
        .filter(|hit| {
            hit.role == Some(leantoken::ReferenceRole::Reference)
                || hit.match_kinds.iter().any(|kind| kind == "reference")
        })
        .count();
    let compact_references = search_compact
        .groups
        .iter()
        .flat_map(|group| &group.references)
        .map(|references| references.count)
        .sum::<usize>();
    let search_parity =
        represented_hits == search_full.hits.len() && compact_references == full_references;
    let search_verifiable = search_compact
        .groups
        .iter()
        .any(|group| group.definition.is_some())
        && search_compact.groups.iter().all(|group| {
            group
                .definition
                .as_ref()
                .or(group.representative.as_ref())
                .is_some_and(|evidence| {
                    !evidence.path.is_empty()
                        && evidence.start_line > 0
                        && evidence.end_line >= evidence.start_line
                        && evidence.content_hash.len() == 32
                })
        });
    let search = projection_result(
        search_full.meta.total_response_tokens,
        search_compact.meta.total_response_tokens,
        search_full.meta.source_tokens,
        search_compact.meta.source_tokens,
        search_parity,
        search_verifiable,
    );

    let aggregate_baseline_tokens = files
        .baseline_total_response_tokens
        .saturating_add(outline.baseline_total_response_tokens)
        .saturating_add(search.baseline_total_response_tokens);
    let aggregate_compact_tokens = files
        .compact_total_response_tokens
        .saturating_add(outline.compact_total_response_tokens)
        .saturating_add(search.compact_total_response_tokens);
    let retry_proxy_delta = files.compact_retry_proxy as isize
        + outline.compact_retry_proxy as isize
        + search.compact_retry_proxy as isize;
    let each_delta_negative = [
        files.complete_response_token_delta,
        outline.complete_response_token_delta,
        search.complete_response_token_delta,
    ]
    .into_iter()
    .all(|delta| delta < 0);
    let passed = (!manifest.acceptance.require_default_membership_parity
        || (files.membership_or_concept_parity
            && outline.membership_or_concept_parity
            && search.membership_or_concept_parity))
        && (!manifest.acceptance.require_verifiable_path_range_hash
            || (files.verifiable && outline.verifiable && search.verifiable))
        && (!manifest.acceptance.require_zero_retry_proxy_delta || retry_proxy_delta == 0)
        && (!manifest
            .acceptance
            .require_each_complete_response_token_delta_negative
            || each_delta_negative);

    let report = Report {
        schema_version: manifest.schema_version,
        experiment: manifest.experiment,
        source_revision,
        source_dirty,
        build_profile: if cfg!(debug_assertions) {
            "debug"
        } else {
            "release"
        },
        operating_system: env::consts::OS,
        architecture: env::consts::ARCH,
        manifest_blake3: blake3::hash(&manifest_bytes).to_hex().to_string(),
        fixture_tree_blake3: fixture_hash,
        tokenizer: tokenizer.name().into(),
        token_count_exact: tokenizer.is_exact(),
        visible_entries: files_full.entries.len(),
        generated_reference_callers: manifest.fixture.generated_reference_callers,
        aggregate_baseline_tokens,
        aggregate_compact_tokens,
        aggregate_token_delta: aggregate_compact_tokens as isize
            - aggregate_baseline_tokens as isize,
        retry_proxy_delta,
        files,
        outline,
        search,
        acceptance: manifest.acceptance,
        passed,
        limitations: vec![
            "Frozen multilingual fixture plus one generated broad-reference file; this is not a population estimate.",
            "Retry proxy is zero only when labeled path/symbol coverage and verification coordinates remain available.",
            "No model executes a maintenance task, so task success outside the declared proxy is unobserved.",
            "The experiment measures exact service DTO tokens, not provider billing or host UI framing.",
        ],
    };
    if let Some(parent) = output_path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&output_path, serde_json::to_vec_pretty(&report)?)?;
    println!("{}", serde_json::to_string_pretty(&report)?);
    if !report.passed {
        return Err("compact projection acceptance gate failed".into());
    }
    Ok(())
}

fn projection_result(
    baseline_total_response_tokens: usize,
    compact_total_response_tokens: usize,
    baseline_source_tokens: usize,
    compact_source_tokens: usize,
    membership_or_concept_parity: bool,
    verifiable: bool,
) -> ProjectionResult {
    let compact_retry_proxy = usize::from(!(membership_or_concept_parity && verifiable));
    ProjectionResult {
        baseline_total_response_tokens,
        compact_total_response_tokens,
        complete_response_token_delta: compact_total_response_tokens as isize
            - baseline_total_response_tokens as isize,
        baseline_source_tokens,
        compact_source_tokens,
        membership_or_concept_parity,
        verifiable,
        baseline_retry_proxy: 0,
        compact_retry_proxy,
    }
}

fn option_value(flag: &str) -> Option<PathBuf> {
    option_string(flag).map(PathBuf::from)
}

fn option_string(flag: &str) -> Option<String> {
    let mut arguments = env::args();
    while let Some(argument) = arguments.next() {
        if argument == flag {
            return arguments.next();
        }
    }
    None
}

fn canonical_fixture_files(root: &Path) -> AnyResult<Vec<(String, Vec<u8>)>> {
    let mut files = Vec::new();
    collect_files(root, root, &mut files)?;
    files.sort_by(|left, right| left.0.cmp(&right.0));
    Ok(files)
}

fn collect_files(
    root: &Path,
    directory: &Path,
    files: &mut Vec<(String, Vec<u8>)>,
) -> AnyResult<()> {
    let mut entries = fs::read_dir(directory)?.collect::<Result<Vec<_>, _>>()?;
    entries.sort_by_key(fs::DirEntry::file_name);
    for entry in entries {
        let path = entry.path();
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            collect_files(root, &path, files)?;
        } else if file_type.is_file() {
            let relative = path
                .strip_prefix(root)?
                .to_string_lossy()
                .replace('\\', "/");
            let text = String::from_utf8(fs::read(path)?)?;
            let normalized = text.replace("\r\n", "\n");
            if normalized.contains('\r') {
                return Err("fixture contains a lone carriage return".into());
            }
            files.push((relative, normalized.into_bytes()));
        }
    }
    Ok(())
}

fn fixture_manifest_hash(files: &[(String, Vec<u8>)]) -> String {
    let manifest = files
        .iter()
        .map(|(path, bytes)| format!("{}  {path}\n", blake3::hash(bytes).to_hex()))
        .collect::<String>();
    blake3::hash(manifest.as_bytes()).to_hex().to_string()
}

fn copy_tree(source: &Path, destination: &Path) -> AnyResult<()> {
    fs::create_dir_all(destination)?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let source_path = entry.path();
        let destination_path = destination.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_tree(&source_path, &destination_path)?;
        } else {
            fs::copy(source_path, destination_path)?;
        }
    }
    Ok(())
}
