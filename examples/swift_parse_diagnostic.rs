//! Build a bounded, source-free diagnostic for a pinned Swift corpus.

use std::collections::BTreeMap;
use std::error::Error;
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::ops::ControlFlow;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use clap::Parser;
use serde::Serialize;
use tempfile::NamedTempFile;
use tree_sitter::{Node, ParseOptions, Parser as TreeParser, Tree};

const SCHEMA_VERSION: u32 = 1;
const MAX_FILES: usize = 100_000;
const MAX_PATH_BYTES: usize = 4_096;
const MAX_FILE_BYTES: u64 = 8 << 20;
const MAX_TOTAL_SOURCE_BYTES: u64 = 8 << 30;
const MAX_NODES_PER_FILE: u64 = 1_000_000;
const MAX_TOTAL_NODES: u64 = 100_000_000;
const MAX_RECOVERY_NODES: u64 = 1_000_000;
const MAX_RECOVERY_CATEGORIES: usize = 512;
const MAX_REPORTED_RECOVERY_CATEGORIES: usize = 32;
const MAX_KIND_BYTES: usize = 64;
const MAX_GIT_STDOUT_BYTES: usize = 64 << 20;
const MAX_GIT_STDERR_BYTES: usize = 64 << 10;
const GIT_TIMEOUT: Duration = Duration::from_secs(60);
const FILE_PARSE_TIMEOUT: Duration = Duration::from_secs(30);
const LOCKFILE: &str = include_str!("../Cargo.lock");

type AnyError = Box<dyn Error + Send + Sync>;
type AnyResult<T> = Result<T, AnyError>;

#[derive(Debug, Parser)]
#[command(about = "Diagnose bounded Swift parser recovery at an exact Git revision")]
struct Args {
    #[arg(long)]
    repository: PathBuf,
    #[arg(long)]
    revision: String,
    #[arg(long)]
    output: PathBuf,
}

#[derive(Debug, Serialize)]
struct DiagnosticReport {
    schema_version: u32,
    parser_dependencies: ParserDependencies,
    corpus: CorpusReport,
    limits: LimitReport,
    summary: Counts,
    strata: Vec<StratumReport>,
    recovery_category_count: u64,
    recovery_categories: Vec<RecoveryCategory>,
    other_recovery_nodes: u64,
}

#[derive(Debug, Serialize)]
struct ParserDependencies {
    tree_sitter: String,
    tree_sitter_swift: String,
}

#[derive(Debug, Serialize)]
struct CorpusReport {
    revision: String,
    content_blake3: String,
    files: u64,
    source_bytes: u64,
}

#[derive(Debug, Serialize)]
struct LimitReport {
    max_files: u64,
    max_path_bytes: u64,
    max_file_bytes: u64,
    max_total_source_bytes: u64,
    max_nodes_per_file: u64,
    max_total_nodes: u64,
    max_recovery_nodes: u64,
    max_recovery_categories: u64,
    max_reported_recovery_categories: u64,
    file_parse_timeout_seconds: u64,
}

#[derive(Debug, Clone, Default, Serialize)]
struct Counts {
    files: u64,
    incomplete_files: u64,
    source_bytes: u64,
    nodes_visited: u64,
    error_nodes: u64,
    missing_nodes: u64,
    extracted: ExtractionCounts,
    retained_in_incomplete_files: ExtractionCounts,
}

#[derive(Debug, Clone, Default, Serialize)]
struct ExtractionCounts {
    definitions: u64,
    nested_definitions: u64,
    imports: u64,
    calls: u64,
    calls_with_owner: u64,
    owner_ranges: u64,
}

#[derive(Debug, Serialize)]
struct StratumReport {
    source_shape: &'static str,
    counts: Counts,
}

#[derive(Debug, Serialize)]
struct RecoveryCategory {
    source_shape: &'static str,
    recovery_kind: String,
    parent_kind: String,
    syntax_kind: String,
    count: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum SourceShape {
    Ordinary,
    Test,
    Fixture,
    Generated,
    IntentionalInvalid,
}

impl SourceShape {
    const fn label(self) -> &'static str {
        match self {
            Self::Ordinary => "ordinary_or_declaration",
            Self::Test => "test",
            Self::Fixture => "mock_fixture_or_harness",
            Self::Generated => "generated",
            Self::IntentionalInvalid => "intentional_invalid_syntax",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct RecoveryKey {
    shape: SourceShape,
    recovery_kind: String,
    parent_kind: String,
    syntax_kind: String,
}

#[derive(Default)]
struct FileDiagnostic {
    counts: Counts,
    recovery: BTreeMap<RecoveryKey, u64>,
}

fn main() -> AnyResult<()> {
    let args = Args::parse();
    let paths = pinned_swift_paths(&args.repository, &args.revision)?;
    let report = analyze(&args.repository, &args.revision, paths)?;
    write_report(&args.output, &report)?;
    println!(
        "Swift parse diagnostic: {} files, {} incomplete, {} ERROR, {} MISSING",
        report.summary.files,
        report.summary.incomplete_files,
        report.summary.error_nodes,
        report.summary.missing_nodes
    );
    println!("Wrote immutable report to {}", args.output.display());
    Ok(())
}

fn analyze(root: &Path, revision: &str, mut paths: Vec<PathBuf>) -> AnyResult<DiagnosticReport> {
    if paths.is_empty() {
        return Err(invalid_data("diagnostic input contains no Swift files").into());
    }
    if paths.len() > MAX_FILES {
        return Err(invalid_data("diagnostic exceeded its file-count bound").into());
    }
    paths.sort_by(|left, right| left.as_os_str().cmp(right.as_os_str()));
    if paths.windows(2).any(|pair| pair[0] == pair[1]) {
        return Err(invalid_data("diagnostic input contains a duplicate path").into());
    }

    let mut parser = TreeParser::new();
    parser.set_language(&tree_sitter_swift::LANGUAGE.into())?;
    let mut corpus_hasher = blake3::Hasher::new();
    let mut summary = Counts::default();
    let mut strata = BTreeMap::<SourceShape, Counts>::new();
    let mut recovery = BTreeMap::<RecoveryKey, u64>::new();

    for relative in paths {
        let normalized = validate_swift_path(&relative)?;
        let full_path = root.join(&relative);
        let metadata = fs::symlink_metadata(&full_path)?;
        if !metadata.file_type().is_file() {
            return Err(invalid_data("diagnostic inputs must be regular files").into());
        }
        let bytes = read_bounded_file(&full_path, MAX_FILE_BYTES)?;
        let source = std::str::from_utf8(&bytes)
            .map_err(|_| invalid_data("Swift diagnostic input is not UTF-8"))?;
        update_corpus_hash(&mut corpus_hasher, normalized.as_bytes(), &bytes)?;
        let shape = classify_source_shape(&relative);
        let tree = parse_with_deadline(&mut parser, source)?;
        let file = inspect_tree(&tree, shape, bytes.len())?;
        add_counts(&mut summary, &file.counts)?;
        add_counts(strata.entry(shape).or_default(), &file.counts)?;
        for (key, count) in file.recovery {
            if !recovery.contains_key(&key) && recovery.len() >= MAX_RECOVERY_CATEGORIES {
                return Err(invalid_data("diagnostic exceeded its recovery-category bound").into());
            }
            let total = recovery.entry(key).or_default();
            *total = checked_add(*total, count)?;
        }
        if summary.source_bytes > MAX_TOTAL_SOURCE_BYTES {
            return Err(invalid_data("diagnostic exceeded its total source-byte bound").into());
        }
        if summary.nodes_visited > MAX_TOTAL_NODES {
            return Err(invalid_data("diagnostic exceeded its total syntax-node bound").into());
        }
        if checked_add(summary.error_nodes, summary.missing_nodes)? > MAX_RECOVERY_NODES {
            return Err(invalid_data("diagnostic exceeded its recovery-node bound").into());
        }
    }

    let recovery_category_count = usize_to_u64(recovery.len())?;
    let mut recovery = recovery.into_iter().collect::<Vec<_>>();
    recovery.sort_by(|(left_key, left_count), (right_key, right_count)| {
        right_count
            .cmp(left_count)
            .then_with(|| left_key.cmp(right_key))
    });
    let mut other_recovery_nodes = 0;
    let recovery_categories = recovery
        .into_iter()
        .enumerate()
        .filter_map(|(index, (key, count))| {
            if index < MAX_REPORTED_RECOVERY_CATEGORIES {
                Some(RecoveryCategory {
                    source_shape: key.shape.label(),
                    recovery_kind: key.recovery_kind,
                    parent_kind: key.parent_kind,
                    syntax_kind: key.syntax_kind,
                    count,
                })
            } else {
                other_recovery_nodes += count;
                None
            }
        })
        .collect();

    Ok(DiagnosticReport {
        schema_version: SCHEMA_VERSION,
        parser_dependencies: ParserDependencies {
            tree_sitter: locked_package_version("tree-sitter")?,
            tree_sitter_swift: locked_package_version("tree-sitter-swift")?,
        },
        corpus: CorpusReport {
            revision: revision.to_owned(),
            content_blake3: corpus_hasher.finalize().to_hex().to_string(),
            files: summary.files,
            source_bytes: summary.source_bytes,
        },
        limits: limit_report(),
        summary,
        strata: strata
            .into_iter()
            .map(|(shape, counts)| StratumReport {
                source_shape: shape.label(),
                counts,
            })
            .collect(),
        recovery_category_count,
        recovery_categories,
        other_recovery_nodes,
    })
}

fn parse_with_deadline(parser: &mut TreeParser, source: &str) -> AnyResult<Tree> {
    let deadline = Instant::now() + FILE_PARSE_TIMEOUT;
    let bytes = source.as_bytes();
    let mut input = |offset: usize, _| bytes.get(offset..).unwrap_or_default();
    let mut progress = |_: &tree_sitter::ParseState| {
        if Instant::now() >= deadline {
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(())
        }
    };
    let options = ParseOptions::new().progress_callback(&mut progress);
    parser
        .parse_with_options(&mut input, None, Some(options))
        .ok_or_else(|| io::Error::new(io::ErrorKind::TimedOut, "Swift file parse timed out").into())
}

fn inspect_tree(tree: &Tree, shape: SourceShape, source_bytes: usize) -> AnyResult<FileDiagnostic> {
    let mut diagnostic = FileDiagnostic::default();
    diagnostic.counts.files = 1;
    diagnostic.counts.source_bytes = usize_to_u64(source_bytes)?;
    let incomplete = tree.root_node().has_error();
    diagnostic.counts.incomplete_files = u64::from(incomplete);

    let mut cursor = tree.walk();
    loop {
        let node = cursor.node();
        diagnostic.counts.nodes_visited = checked_add(diagnostic.counts.nodes_visited, 1)?;
        if diagnostic.counts.nodes_visited > MAX_NODES_PER_FILE {
            return Err(invalid_data("diagnostic exceeded its per-file syntax-node bound").into());
        }

        if node.is_error() || node.is_missing() {
            let recovery_kind = if node.is_missing() {
                diagnostic.counts.missing_nodes = checked_add(diagnostic.counts.missing_nodes, 1)?;
                "missing"
            } else {
                diagnostic.counts.error_nodes = checked_add(diagnostic.counts.error_nodes, 1)?;
                "error"
            };
            let key = RecoveryKey {
                shape,
                recovery_kind: recovery_kind.to_owned(),
                parent_kind: safe_kind(node.parent().map_or("<root>", |parent| parent.kind())),
                syntax_kind: recovery_syntax_kind(node),
            };
            let count = diagnostic.recovery.entry(key).or_default();
            *count = checked_add(*count, 1)?;
        }

        if is_definition(node) {
            diagnostic.counts.extracted.definitions =
                checked_add(diagnostic.counts.extracted.definitions, 1)?;
            if has_definition_ancestor(node) {
                diagnostic.counts.extracted.nested_definitions =
                    checked_add(diagnostic.counts.extracted.nested_definitions, 1)?;
            }
            if is_owner_definition(node) {
                diagnostic.counts.extracted.owner_ranges =
                    checked_add(diagnostic.counts.extracted.owner_ranges, 1)?;
            }
        } else if node.kind() == "import_declaration" {
            diagnostic.counts.extracted.imports =
                checked_add(diagnostic.counts.extracted.imports, 1)?;
        } else if node.kind() == "call_expression" {
            diagnostic.counts.extracted.calls = checked_add(diagnostic.counts.extracted.calls, 1)?;
            if has_owner_ancestor(node) {
                diagnostic.counts.extracted.calls_with_owner =
                    checked_add(diagnostic.counts.extracted.calls_with_owner, 1)?;
            }
        }

        if cursor.goto_first_child() {
            continue;
        }
        while !cursor.goto_next_sibling() {
            if !cursor.goto_parent() {
                if incomplete {
                    diagnostic.counts.retained_in_incomplete_files =
                        diagnostic.counts.extracted.clone();
                }
                return Ok(diagnostic);
            }
        }
    }
}

fn is_definition(node: Node<'_>) -> bool {
    matches!(
        node.kind(),
        "class_declaration"
            | "protocol_declaration"
            | "function_declaration"
            | "protocol_function_declaration"
            | "protocol_property_declaration"
            | "init_declaration"
            | "deinit_declaration"
            | "subscript_declaration"
    )
}

fn is_owner_definition(node: Node<'_>) -> bool {
    matches!(
        node.kind(),
        "class_declaration"
            | "protocol_declaration"
            | "function_declaration"
            | "protocol_function_declaration"
            | "init_declaration"
            | "deinit_declaration"
            | "subscript_declaration"
    )
}

fn has_definition_ancestor(node: Node<'_>) -> bool {
    let mut parent = node.parent();
    while let Some(candidate) = parent {
        if is_definition(candidate) {
            return true;
        }
        parent = candidate.parent();
    }
    false
}

fn has_owner_ancestor(node: Node<'_>) -> bool {
    let mut parent = node.parent();
    while let Some(candidate) = parent {
        if is_owner_definition(candidate) {
            return true;
        }
        parent = candidate.parent();
    }
    false
}

fn recovery_syntax_kind(node: Node<'_>) -> String {
    if node.is_missing() {
        return safe_kind(node.kind());
    }
    let mut cursor = node.walk();
    node.named_children(&mut cursor)
        .next()
        .map_or_else(|| "<none>".to_owned(), |child| safe_kind(child.kind()))
}

fn safe_kind(kind: &str) -> String {
    if kind.is_empty()
        || kind.len() > MAX_KIND_BYTES
        || !kind.bytes().all(|byte| byte.is_ascii_graphic())
    {
        "<other>".to_owned()
    } else {
        kind.to_owned()
    }
}

fn add_counts(target: &mut Counts, other: &Counts) -> AnyResult<()> {
    target.files = checked_add(target.files, other.files)?;
    target.incomplete_files = checked_add(target.incomplete_files, other.incomplete_files)?;
    target.source_bytes = checked_add(target.source_bytes, other.source_bytes)?;
    target.nodes_visited = checked_add(target.nodes_visited, other.nodes_visited)?;
    target.error_nodes = checked_add(target.error_nodes, other.error_nodes)?;
    target.missing_nodes = checked_add(target.missing_nodes, other.missing_nodes)?;
    add_extraction(&mut target.extracted, &other.extracted)?;
    add_extraction(
        &mut target.retained_in_incomplete_files,
        &other.retained_in_incomplete_files,
    )
}

fn add_extraction(target: &mut ExtractionCounts, other: &ExtractionCounts) -> AnyResult<()> {
    target.definitions = checked_add(target.definitions, other.definitions)?;
    target.nested_definitions = checked_add(target.nested_definitions, other.nested_definitions)?;
    target.imports = checked_add(target.imports, other.imports)?;
    target.calls = checked_add(target.calls, other.calls)?;
    target.calls_with_owner = checked_add(target.calls_with_owner, other.calls_with_owner)?;
    target.owner_ranges = checked_add(target.owner_ranges, other.owner_ranges)?;
    Ok(())
}

fn classify_source_shape(path: &Path) -> SourceShape {
    let path = path.to_str().unwrap_or_default().to_ascii_lowercase();
    let filename = path.rsplit('/').next().unwrap_or(&path);
    let segments = path.split('/').collect::<Vec<_>>();
    if segments
        .iter()
        .any(|segment| matches!(*segment, "invalid" | "malformed" | "syntax-errors"))
        || filename.contains(".invalid.")
        || filename.contains(".malformed.")
    {
        SourceShape::IntentionalInvalid
    } else if segments
        .iter()
        .any(|segment| matches!(*segment, "generated" | "gen" | "codegen"))
        || filename.contains(".generated.")
    {
        SourceShape::Generated
    } else if segments.iter().any(|segment| {
        matches!(
            *segment,
            "mock" | "mocks" | "fixture" | "fixtures" | "harness"
        )
    }) || filename.contains(".mock.")
    {
        SourceShape::Fixture
    } else if segments
        .iter()
        .any(|segment| matches!(*segment, "test" | "tests" | "uitests"))
        || filename.contains("test.swift")
    {
        SourceShape::Test
    } else {
        SourceShape::Ordinary
    }
}

fn pinned_swift_paths(repository: &Path, expected_revision: &str) -> AnyResult<Vec<PathBuf>> {
    validate_revision(expected_revision)?;
    let actual = git_text(repository, &["rev-parse", "HEAD^{commit}"])?;
    if actual.trim() != expected_revision {
        return Err(invalid_data("checkout HEAD does not match the requested revision").into());
    }
    let status = git_bytes(
        repository,
        &["status", "--porcelain=v1", "-z", "--untracked-files=no"],
    )?;
    if !status.is_empty() {
        return Err(invalid_data("tracked checkout state is not clean").into());
    }
    let paths = git_bytes(repository, &["ls-files", "-z", "--", "*.swift"])?;
    paths
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
        .map(|path| {
            let path = std::str::from_utf8(path)
                .map_err(|_| invalid_data("tracked Swift path is not UTF-8"))?;
            Ok(PathBuf::from(path))
        })
        .collect()
}

fn validate_revision(revision: &str) -> AnyResult<()> {
    if revision.len() != 40
        || !revision
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        return Err(invalid_data("revision must be a full lowercase Git object ID").into());
    }
    Ok(())
}

fn validate_swift_path(path: &Path) -> AnyResult<String> {
    let text = path
        .to_str()
        .ok_or_else(|| invalid_data("diagnostic path is not UTF-8"))?;
    if text.is_empty() || text.len() > MAX_PATH_BYTES || text.contains('\\') {
        return Err(invalid_data("diagnostic path is empty, oversized, or non-portable").into());
    }
    if !path
        .components()
        .all(|component| matches!(component, Component::Normal(_)))
        || path.extension().and_then(|extension| extension.to_str()) != Some("swift")
    {
        return Err(invalid_data("diagnostic path must be a relative Swift path").into());
    }
    Ok(text.to_owned())
}

fn git_text(repository: &Path, args: &[&str]) -> AnyResult<String> {
    String::from_utf8(git_bytes(repository, args)?)
        .map_err(|_| invalid_data("Git returned non-UTF-8 text output").into())
}

fn git_bytes(repository: &Path, args: &[&str]) -> AnyResult<Vec<u8>> {
    let mut command = Command::new("git");
    command
        .arg("--no-pager")
        .args(["-c", "core.fsmonitor=false"])
        .arg("-C")
        .arg(repository)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let output = bounded_command_output(&mut command)?;
    if !output.status.success() {
        return Err(io::Error::other(format!(
            "Git command failed with {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        ))
        .into());
    }
    Ok(output.stdout)
}

struct BoundedOutput {
    status: ExitStatus,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

fn bounded_command_output(command: &mut Command) -> AnyResult<BoundedOutput> {
    let mut child = command.spawn()?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| invalid_data("Git stdout was not piped"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| invalid_data("Git stderr was not piped"))?;
    let overflow = Arc::new(AtomicBool::new(false));
    let stdout_overflow = Arc::clone(&overflow);
    let stderr_overflow = Arc::clone(&overflow);
    let stdout_reader =
        thread::spawn(move || read_stream_bounded(stdout, MAX_GIT_STDOUT_BYTES, stdout_overflow));
    let stderr_reader =
        thread::spawn(move || read_stream_bounded(stderr, MAX_GIT_STDERR_BYTES, stderr_overflow));
    let started = Instant::now();
    let mut terminal_error = None;
    let status = loop {
        if overflow.load(Ordering::Relaxed) {
            let _ = child.kill();
            terminal_error = Some(invalid_data("Git output exceeded its byte bound"));
            break child.wait()?;
        }
        if started.elapsed() >= GIT_TIMEOUT {
            let _ = child.kill();
            terminal_error = Some(io::Error::new(
                io::ErrorKind::TimedOut,
                "Git command timed out",
            ));
            break child.wait()?;
        }
        if let Some(status) = child.try_wait()? {
            break status;
        }
        thread::sleep(Duration::from_millis(10));
    };
    let stdout = stdout_reader
        .join()
        .map_err(|_| invalid_data("Git stdout reader panicked"))??;
    let stderr = stderr_reader
        .join()
        .map_err(|_| invalid_data("Git stderr reader panicked"))??;
    if let Some(error) = terminal_error {
        return Err(error.into());
    }
    if overflow.load(Ordering::Relaxed) {
        return Err(invalid_data("Git output exceeded its byte bound").into());
    }
    Ok(BoundedOutput {
        status,
        stdout,
        stderr,
    })
}

fn read_stream_bounded(
    mut stream: impl Read,
    limit: usize,
    overflow: Arc<AtomicBool>,
) -> io::Result<Vec<u8>> {
    let mut retained = Vec::new();
    let mut buffer = [0_u8; 8 << 10];
    loop {
        let read = match stream.read(&mut buffer) {
            Ok(read) => read,
            Err(error) => {
                overflow.store(true, Ordering::Relaxed);
                return Err(error);
            }
        };
        if read == 0 {
            return Ok(retained);
        }
        let remaining = limit.saturating_sub(retained.len());
        retained.extend_from_slice(&buffer[..read.min(remaining)]);
        if read > remaining {
            overflow.store(true, Ordering::Relaxed);
        }
    }
}

fn read_bounded_file(path: &Path, limit: u64) -> AnyResult<Vec<u8>> {
    let mut file = File::open(path)?;
    let mut bytes = Vec::new();
    Read::by_ref(&mut file)
        .take(
            limit
                .checked_add(1)
                .ok_or_else(|| invalid_data("file-byte bound overflowed"))?,
        )
        .read_to_end(&mut bytes)?;
    if usize_to_u64(bytes.len())? > limit {
        return Err(invalid_data("diagnostic input exceeded its per-file byte bound").into());
    }
    Ok(bytes)
}

fn update_corpus_hash(
    hasher: &mut blake3::Hasher,
    relative_path: &[u8],
    content: &[u8],
) -> AnyResult<()> {
    hasher.update(&usize_to_u64(relative_path.len())?.to_le_bytes());
    hasher.update(relative_path);
    hasher.update(&usize_to_u64(content.len())?.to_le_bytes());
    hasher.update(content);
    Ok(())
}

fn write_report(path: &Path, report: &DiagnosticReport) -> AnyResult<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let mut temporary = NamedTempFile::new_in(parent)?;
    serde_json::to_writer_pretty(&mut temporary, report)?;
    temporary.write_all(b"\n")?;
    temporary.flush()?;
    temporary
        .persist_noclobber(path)
        .map_err(|error| error.error)?;
    Ok(())
}

fn limit_report() -> LimitReport {
    LimitReport {
        max_files: MAX_FILES as u64,
        max_path_bytes: MAX_PATH_BYTES as u64,
        max_file_bytes: MAX_FILE_BYTES,
        max_total_source_bytes: MAX_TOTAL_SOURCE_BYTES,
        max_nodes_per_file: MAX_NODES_PER_FILE,
        max_total_nodes: MAX_TOTAL_NODES,
        max_recovery_nodes: MAX_RECOVERY_NODES,
        max_recovery_categories: MAX_RECOVERY_CATEGORIES as u64,
        max_reported_recovery_categories: MAX_REPORTED_RECOVERY_CATEGORIES as u64,
        file_parse_timeout_seconds: FILE_PARSE_TIMEOUT.as_secs(),
    }
}

fn locked_package_version(package_name: &str) -> AnyResult<String> {
    let mut version = None;
    for package in LOCKFILE.split("[[package]]").skip(1) {
        let name = lockfile_value(package, "name");
        if name.as_deref() != Some(package_name) {
            continue;
        }
        let candidate = lockfile_value(package, "version")
            .ok_or_else(|| invalid_data("locked parser package has no version"))?;
        if version.replace(candidate).is_some() {
            return Err(invalid_data("locked parser package is ambiguous").into());
        }
    }
    version.ok_or_else(|| invalid_data("locked parser package was not found").into())
}

fn lockfile_value(package: &str, key: &str) -> Option<String> {
    package.lines().find_map(|line| {
        let (candidate, value) = line.split_once('=')?;
        (candidate.trim() == key).then(|| value.trim().trim_matches('"').to_owned())
    })
}

fn checked_add(left: u64, right: u64) -> AnyResult<u64> {
    left.checked_add(right)
        .ok_or_else(|| invalid_data("diagnostic counter overflowed").into())
}

fn usize_to_u64(value: usize) -> AnyResult<u64> {
    u64::try_from(value).map_err(|_| invalid_data("diagnostic count overflowed").into())
}

fn invalid_data(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inspect(source: &str) -> FileDiagnostic {
        let mut parser = TreeParser::new();
        parser
            .set_language(&tree_sitter_swift::LANGUAGE.into())
            .expect("Swift language");
        let tree = parser.parse(source, None).expect("tree");
        inspect_tree(&tree, SourceShape::Ordinary, source.len()).expect("diagnostic")
    }

    #[test]
    fn realistic_source_counts_definitions_imports_calls_and_owners() {
        let diagnostic = inspect(
            "import Foundation\nstruct Store { func save() { helper() } }\nfunc helper() {}\n",
        );
        assert_eq!(diagnostic.counts.incomplete_files, 0);
        assert_eq!(diagnostic.counts.extracted.definitions, 3);
        assert_eq!(diagnostic.counts.extracted.nested_definitions, 1);
        assert_eq!(diagnostic.counts.extracted.imports, 1);
        assert_eq!(diagnostic.counts.extracted.calls, 1);
        assert_eq!(diagnostic.counts.extracted.calls_with_owner, 1);
    }

    #[test]
    fn malformed_source_retains_recoverable_facts() {
        let diagnostic = inspect("struct Store { func ready() {} func broken( { helper() } }");
        assert_eq!(diagnostic.counts.incomplete_files, 1);
        assert!(
            diagnostic.counts.error_nodes + diagnostic.counts.missing_nodes > 0,
            "malformed syntax exposed no recovery nodes"
        );
        assert!(diagnostic.counts.retained_in_incomplete_files.definitions >= 2);
    }

    #[test]
    fn source_shape_classification_is_path_only() {
        assert_eq!(
            classify_source_shape(Path::new("Sources/Store.swift")),
            SourceShape::Ordinary
        );
        assert_eq!(
            classify_source_shape(Path::new("Tests/StoreTests.swift")),
            SourceShape::Test
        );
        assert_eq!(
            classify_source_shape(Path::new("Sources/Generated/API.swift")),
            SourceShape::Generated
        );
    }

    #[test]
    fn lockfile_reports_exact_swift_grammar() {
        assert_eq!(
            locked_package_version("tree-sitter-swift").expect("locked version"),
            "0.7.3"
        );
    }
}
