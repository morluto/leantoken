//! Build a bounded, source-free diagnostic for a pinned Kotlin corpus.

use std::collections::BTreeMap;
use std::error::Error;
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

#[cfg(test)]
use std::fs;

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
const LOCKFILE: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.lock"));

type AnyError = Box<dyn Error + Send + Sync>;
type AnyResult<T> = Result<T, AnyError>;

#[derive(Debug, Parser)]
#[command(about = "Diagnose bounded Kotlin parser recovery at an exact Git revision")]
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
    extension_strata: Vec<ExtensionStratumReport>,
    recovery_category_count: u64,
    recovery_categories: Vec<RecoveryCategory>,
    other_recovery_nodes: u64,
}

#[derive(Debug, Serialize)]
struct ParserDependencies {
    tree_sitter: String,
    tree_sitter_kotlin: String,
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
    syntax_node_counts: SyntaxNodeCounts,
    syntax_nodes_retained_in_incomplete_files: SyntaxNodeCounts,
}

#[derive(Debug, Clone, Default, Serialize)]
struct SyntaxNodeCounts {
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
struct ExtensionStratumReport {
    extension: &'static str,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum SourceExtension {
    KotlinSource,
    KotlinScript,
}

impl SourceExtension {
    const fn label(self) -> &'static str {
        match self {
            Self::KotlinSource => "kt",
            Self::KotlinScript => "kts",
        }
    }
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
    let paths = pinned_kotlin_paths(&args.repository, &args.revision)?;
    let report = analyze(&args.repository, &args.revision, paths)?;
    write_report(&args.output, &report)?;
    println!(
        "Kotlin parse diagnostic: {} files, {} incomplete, {} ERROR, {} MISSING",
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
        return Err(invalid_data("diagnostic input contains no Kotlin files").into());
    }
    if paths.len() > MAX_FILES {
        return Err(invalid_data("diagnostic exceeded its file-count bound").into());
    }
    paths.sort_by(|left, right| left.as_os_str().cmp(right.as_os_str()));
    if paths.windows(2).any(|pair| pair[0] == pair[1]) {
        return Err(invalid_data("diagnostic input contains a duplicate path").into());
    }

    let mut parser = TreeParser::new();
    parser.set_language(&kotlin_diagnostic_grammar::LANGUAGE.into())?;
    let mut corpus_hasher = blake3::Hasher::new();
    let mut summary = Counts::default();
    let mut strata = BTreeMap::<SourceShape, Counts>::new();
    let mut extension_strata = BTreeMap::<SourceExtension, Counts>::new();
    let mut recovery = BTreeMap::<RecoveryKey, u64>::new();

    for relative in paths {
        let normalized = validate_kotlin_path(&relative)?;
        let bytes = read_pinned_blob(root, revision, &normalized)?;
        let source = std::str::from_utf8(&bytes)
            .map_err(|_| invalid_data("Kotlin diagnostic input is not UTF-8"))?;
        update_corpus_hash(&mut corpus_hasher, normalized.as_bytes(), &bytes)?;
        let shape = classify_source_shape(&relative);
        let extension = source_extension(&relative)?;
        let tree = parse_with_deadline(&mut parser, source)?;
        let file = inspect_tree(&tree, shape, bytes.len())?;
        add_counts(&mut summary, &file.counts)?;
        add_counts(strata.entry(shape).or_default(), &file.counts)?;
        add_counts(extension_strata.entry(extension).or_default(), &file.counts)?;
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
            tree_sitter_kotlin: locked_package_version("tree-sitter-kotlin")?,
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
        extension_strata: extension_strata
            .into_iter()
            .map(|(extension, counts)| ExtensionStratumReport {
                extension: extension.label(),
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
    match parser.parse_with_options(&mut input, None, Some(options)) {
        Some(tree) => Ok(tree),
        None if Instant::now() >= deadline => {
            Err(io::Error::new(io::ErrorKind::TimedOut, "Kotlin file parse timed out").into())
        }
        None => Err(io::Error::other("Kotlin parser returned no tree").into()),
    }
}

fn inspect_tree(tree: &Tree, shape: SourceShape, source_bytes: usize) -> AnyResult<FileDiagnostic> {
    let mut diagnostic = FileDiagnostic::default();
    diagnostic.counts.files = 1;
    diagnostic.counts.source_bytes = usize_to_u64(source_bytes)?;
    let incomplete = tree.root_node().has_error();
    diagnostic.counts.incomplete_files = u64::from(incomplete);

    let mut cursor = tree.walk();
    let mut ancestor_kinds = Vec::<(bool, bool)>::new();
    let mut definition_ancestors = 0_u64;
    let mut owner_ancestors = 0_u64;
    loop {
        let node = cursor.node();
        let node_is_definition = is_definition(node);
        let node_is_owner = is_owner_definition(node);
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
            add_recovery(&mut diagnostic.recovery, key, 1)?;
        }

        if node_is_definition {
            diagnostic.counts.syntax_node_counts.definitions =
                checked_add(diagnostic.counts.syntax_node_counts.definitions, 1)?;
            if definition_ancestors > 0 {
                diagnostic.counts.syntax_node_counts.nested_definitions =
                    checked_add(diagnostic.counts.syntax_node_counts.nested_definitions, 1)?;
            }
            if node_is_owner {
                diagnostic.counts.syntax_node_counts.owner_ranges =
                    checked_add(diagnostic.counts.syntax_node_counts.owner_ranges, 1)?;
            }
        } else if node.kind() == "import_header" {
            diagnostic.counts.syntax_node_counts.imports =
                checked_add(diagnostic.counts.syntax_node_counts.imports, 1)?;
        } else if node.kind() == "call_expression" {
            diagnostic.counts.syntax_node_counts.calls =
                checked_add(diagnostic.counts.syntax_node_counts.calls, 1)?;
            if owner_ancestors > 0 {
                diagnostic.counts.syntax_node_counts.calls_with_owner =
                    checked_add(diagnostic.counts.syntax_node_counts.calls_with_owner, 1)?;
            }
        }

        if cursor.goto_first_child() {
            ancestor_kinds.push((node_is_definition, node_is_owner));
            definition_ancestors =
                checked_add(definition_ancestors, u64::from(node_is_definition))?;
            owner_ancestors = checked_add(owner_ancestors, u64::from(node_is_owner))?;
            continue;
        }
        while !cursor.goto_next_sibling() {
            if !cursor.goto_parent() {
                return finish_file_diagnostic(diagnostic, incomplete);
            }
            let (parent_is_definition, parent_is_owner) = ancestor_kinds
                .pop()
                .ok_or_else(|| invalid_data("syntax traversal ancestor stack underflowed"))?;
            definition_ancestors = definition_ancestors
                .checked_sub(u64::from(parent_is_definition))
                .ok_or_else(|| invalid_data("definition ancestor count underflowed"))?;
            owner_ancestors = owner_ancestors
                .checked_sub(u64::from(parent_is_owner))
                .ok_or_else(|| invalid_data("owner ancestor count underflowed"))?;
        }
    }
}

fn finish_file_diagnostic(
    mut diagnostic: FileDiagnostic,
    incomplete: bool,
) -> AnyResult<FileDiagnostic> {
    if incomplete {
        diagnostic.counts.syntax_nodes_retained_in_incomplete_files =
            diagnostic.counts.syntax_node_counts.clone();
    }
    Ok(diagnostic)
}

fn add_recovery(
    recovery: &mut BTreeMap<RecoveryKey, u64>,
    key: RecoveryKey,
    count: u64,
) -> AnyResult<()> {
    if !recovery.contains_key(&key) && recovery.len() >= MAX_RECOVERY_CATEGORIES {
        return Err(invalid_data("diagnostic exceeded its recovery-category bound").into());
    }
    let total = recovery.entry(key).or_default();
    *total = checked_add(*total, count)?;
    Ok(())
}

fn is_definition(node: Node<'_>) -> bool {
    matches!(
        node.kind(),
        "class_declaration"
            | "object_declaration"
            | "function_declaration"
            | "property_declaration"
            | "enum_entry"
            | "type_alias"
            | "companion_object"
    )
}

fn is_owner_definition(node: Node<'_>) -> bool {
    matches!(
        node.kind(),
        "class_declaration" | "object_declaration" | "function_declaration" | "companion_object"
    )
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
    add_syntax_node_counts(&mut target.syntax_node_counts, &other.syntax_node_counts)?;
    add_syntax_node_counts(
        &mut target.syntax_nodes_retained_in_incomplete_files,
        &other.syntax_nodes_retained_in_incomplete_files,
    )
}

fn add_syntax_node_counts(
    target: &mut SyntaxNodeCounts,
    other: &SyntaxNodeCounts,
) -> AnyResult<()> {
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
        || filename.ends_with("test.kt")
        || filename.ends_with("test.kts")
    {
        SourceShape::Test
    } else {
        SourceShape::Ordinary
    }
}

fn source_extension(path: &Path) -> AnyResult<SourceExtension> {
    match path.extension().and_then(|extension| extension.to_str()) {
        Some("kt") => Ok(SourceExtension::KotlinSource),
        Some("kts") => Ok(SourceExtension::KotlinScript),
        _ => Err(invalid_data("diagnostic path has an unsupported extension").into()),
    }
}

fn pinned_kotlin_paths(repository: &Path, expected_revision: &str) -> AnyResult<Vec<PathBuf>> {
    validate_revision(expected_revision)?;
    let commit_spec = format!("{expected_revision}^{{commit}}");
    let actual = git_text(repository, &["rev-parse", "--verify", &commit_spec])?;
    if actual.trim() != expected_revision {
        return Err(invalid_data("requested revision did not resolve to the exact commit").into());
    }
    let paths = git_bytes(
        repository,
        &[
            "ls-tree",
            "--full-tree",
            "-r",
            "-z",
            "--name-only",
            expected_revision,
            "--",
        ],
    )?;
    decode_kotlin_paths(&paths, MAX_FILES)
}

fn decode_kotlin_paths(output: &[u8], max_files: usize) -> AnyResult<Vec<PathBuf>> {
    let mut paths = Vec::new();
    for path in output
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
    {
        let path = std::str::from_utf8(path)
            .map_err(|_| invalid_data("tracked Kotlin path is not UTF-8"))?;
        if !path.ends_with(".kt") && !path.ends_with(".kts") {
            continue;
        }
        if paths.len() >= max_files {
            return Err(invalid_data("diagnostic exceeded its file-count bound").into());
        }
        paths.push(PathBuf::from(path));
    }
    Ok(paths)
}

fn read_pinned_blob(repository: &Path, revision: &str, relative_path: &str) -> AnyResult<Vec<u8>> {
    read_pinned_blob_with_limit(repository, revision, relative_path, MAX_FILE_BYTES)
}

fn read_pinned_blob_with_limit(
    repository: &Path,
    revision: &str,
    relative_path: &str,
    max_file_bytes: u64,
) -> AnyResult<Vec<u8>> {
    let object = format!("{revision}:{relative_path}");
    let stdout_limit = usize::try_from(max_file_bytes)
        .map_err(|_| invalid_data("per-file byte bound exceeds the platform limit"))?;
    let bytes =
        git_bytes_with_stdout_limit(repository, &["cat-file", "blob", &object], stdout_limit)?;
    if usize_to_u64(bytes.len())? > max_file_bytes {
        return Err(invalid_data("diagnostic input exceeded its per-file byte bound").into());
    }
    Ok(bytes)
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

fn validate_kotlin_path(path: &Path) -> AnyResult<String> {
    let text = path
        .to_str()
        .ok_or_else(|| invalid_data("diagnostic path is not UTF-8"))?;
    if text.is_empty() || text.len() > MAX_PATH_BYTES || text.contains('\\') {
        return Err(invalid_data("diagnostic path is empty, oversized, or non-portable").into());
    }
    let supported_extension = matches!(
        path.extension().and_then(|extension| extension.to_str()),
        Some("kt" | "kts")
    );
    if !path
        .components()
        .all(|component| matches!(component, Component::Normal(_)))
        || !supported_extension
    {
        return Err(invalid_data("diagnostic path must be a relative Kotlin path").into());
    }
    Ok(text.to_owned())
}

fn git_text(repository: &Path, args: &[&str]) -> AnyResult<String> {
    String::from_utf8(git_bytes(repository, args)?)
        .map_err(|_| invalid_data("Git returned non-UTF-8 text output").into())
}

fn git_bytes(repository: &Path, args: &[&str]) -> AnyResult<Vec<u8>> {
    git_bytes_with_stdout_limit(repository, args, MAX_GIT_STDOUT_BYTES)
}

fn git_bytes_with_stdout_limit(
    repository: &Path,
    args: &[&str],
    stdout_limit: usize,
) -> AnyResult<Vec<u8>> {
    let mut command = Command::new("git");
    command
        .arg("--no-pager")
        .arg("--no-replace-objects")
        .args(["-c", "core.fsmonitor=false"])
        .arg("-C")
        .arg(repository)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let output = bounded_command_output(&mut command, stdout_limit)?;
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

fn bounded_command_output(command: &mut Command, stdout_limit: usize) -> AnyResult<BoundedOutput> {
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
        thread::spawn(move || read_stream_bounded(stdout, stdout_limit, stdout_overflow));
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
            .set_language(&kotlin_diagnostic_grammar::LANGUAGE.into())
            .expect("Kotlin language");
        let tree = parser.parse(source, None).expect("tree");
        inspect_tree(&tree, SourceShape::Ordinary, source.len()).expect("diagnostic")
    }

    #[test]
    fn realistic_source_counts_definitions_imports_calls_and_owners() {
        let diagnostic = inspect(
            "import java.time.Instant\n\nclass Store {\n    fun save() {\n        helper()\n    }\n}\n\nfun helper() {}\n",
        );
        assert_eq!(diagnostic.counts.incomplete_files, 0);
        assert_eq!(diagnostic.counts.syntax_node_counts.definitions, 3);
        assert_eq!(diagnostic.counts.syntax_node_counts.nested_definitions, 1);
        assert_eq!(diagnostic.counts.syntax_node_counts.imports, 1);
        assert_eq!(diagnostic.counts.syntax_node_counts.calls, 1);
        assert_eq!(diagnostic.counts.syntax_node_counts.calls_with_owner, 1);
    }

    #[test]
    fn malformed_source_retains_recoverable_facts() {
        let diagnostic = inspect("class Store { fun ready() {} fun broken( { helper() } }");
        assert_eq!(diagnostic.counts.incomplete_files, 1);
        assert!(
            diagnostic.counts.error_nodes + diagnostic.counts.missing_nodes > 0,
            "malformed syntax exposed no recovery nodes"
        );
        assert!(
            diagnostic
                .counts
                .syntax_nodes_retained_in_incomplete_files
                .definitions
                >= 2
        );
    }

    #[test]
    fn source_and_script_shapes_parse_with_the_locked_grammar() {
        for source in [
            "class Formatter {\n    fun format(): String = helper()\n}\n",
            "plugins {\n    kotlin(\"jvm\")\n}\n",
        ] {
            let diagnostic = inspect(source);
            assert_eq!(diagnostic.counts.incomplete_files, 0);
            assert_eq!(diagnostic.counts.error_nodes, 0);
            assert_eq!(diagnostic.counts.missing_nodes, 0);
        }
    }

    #[test]
    fn source_shape_classification_is_path_only() {
        assert_eq!(
            classify_source_shape(Path::new("src/main/kotlin/Store.kt")),
            SourceShape::Ordinary
        );
        assert_eq!(
            classify_source_shape(Path::new("src/test/kotlin/StoreTest.kt")),
            SourceShape::Test
        );
        assert_eq!(
            classify_source_shape(Path::new("src/generated/API.kt")),
            SourceShape::Generated
        );
    }

    #[test]
    fn pinned_corpus_uses_root_paths_and_ignores_worktree_and_replacements() {
        let repository = tempfile::tempdir().expect("temporary repository");
        git_bytes(repository.path(), &["init"]).expect("initialize repository");
        fs::create_dir(repository.path().join("nested")).expect("create nested directory");
        fs::write(repository.path().join("nested/App.kt"), b"class Frozen\n")
            .expect("write committed source");
        git_bytes(repository.path(), &["add", "nested/App.kt"]).expect("stage source");
        git_bytes(
            repository.path(),
            &[
                "-c",
                "user.name=LeanToken",
                "-c",
                "user.email=leantoken@example.invalid",
                "commit",
                "-m",
                "fixture",
            ],
        )
        .expect("commit source");
        let revision = git_text(repository.path(), &["rev-parse", "HEAD^{commit}"])
            .expect("fixture revision")
            .trim()
            .to_owned();

        fs::write(
            repository.path().join("nested/App.kt"),
            b"class Replacement\n",
        )
        .expect("write replacement source");
        git_bytes(repository.path(), &["add", "nested/App.kt"]).expect("stage replacement source");
        git_bytes(
            repository.path(),
            &[
                "-c",
                "user.name=LeanToken",
                "-c",
                "user.email=leantoken@example.invalid",
                "commit",
                "-m",
                "replacement fixture",
            ],
        )
        .expect("commit replacement source");
        let replacement = git_text(repository.path(), &["rev-parse", "HEAD^{commit}"])
            .expect("replacement revision")
            .trim()
            .to_owned();
        git_bytes(repository.path(), &["replace", &revision, &replacement])
            .expect("install replacement object");

        fs::write(repository.path().join("nested/App.kt"), b"class Mutated\n")
            .expect("mutate tracked source again");
        fs::write(
            repository.path().join("nested/Untracked.kts"),
            b"println(\"ignored\")\n",
        )
        .expect("write untracked script");

        let nested = repository.path().join("nested");
        let paths = pinned_kotlin_paths(&nested, &revision).expect("pinned Kotlin paths");
        assert_eq!(paths, vec![PathBuf::from("nested/App.kt")]);
        assert_eq!(
            read_pinned_blob(&nested, &revision, "nested/App.kt").expect("pinned blob"),
            b"class Frozen\n"
        );
        assert_eq!(
            read_pinned_blob_with_limit(&nested, &revision, "nested/App.kt", 4)
                .expect_err("reject blob while collecting bounded stdout")
                .to_string(),
            "Git output exceeded its byte bound"
        );
    }

    #[test]
    fn kotlin_paths_fail_at_the_collection_bound() {
        let error = decode_kotlin_paths(b"one.kt\0two.kts\0", 1)
            .expect_err("reject a second Kotlin path before retaining it");
        assert_eq!(
            error.to_string(),
            "diagnostic exceeded its file-count bound"
        );
    }

    #[test]
    fn lockfile_reports_an_evaluated_kotlin_grammar() {
        let version = locked_package_version("tree-sitter-kotlin").expect("locked version");
        assert_eq!(version, "0.4.0");
    }

    #[test]
    fn recovery_categories_are_bounded_before_insertion() {
        let mut recovery = BTreeMap::new();
        for index in 0..MAX_RECOVERY_CATEGORIES {
            add_recovery(
                &mut recovery,
                RecoveryKey {
                    shape: SourceShape::Ordinary,
                    recovery_kind: "error".to_owned(),
                    parent_kind: "parent".to_owned(),
                    syntax_kind: format!("kind-{index}"),
                },
                1,
            )
            .expect("category below the bound");
        }
        let error = add_recovery(
            &mut recovery,
            RecoveryKey {
                shape: SourceShape::Ordinary,
                recovery_kind: "error".to_owned(),
                parent_kind: "parent".to_owned(),
                syntax_kind: "one-too-many".to_owned(),
            },
            1,
        )
        .expect_err("new category above the bound must fail");
        assert_eq!(
            error.to_string(),
            "diagnostic exceeded its recovery-category bound"
        );
        assert_eq!(recovery.len(), MAX_RECOVERY_CATEGORIES);
    }

    #[test]
    fn checked_openclaw_reports_capture_the_no_ship_decision() {
        let diagnostic: serde_json::Value = serde_json::from_str(include_str!(
            "../../reports/kotlin-parse-diagnostic-openclaw-0.4.0-v1.json"
        ))
        .expect("parse diagnostic");
        let evaluation: serde_json::Value = serde_json::from_str(include_str!(
            "../../reports/kotlin-structural-evaluation-openclaw-v1.json"
        ))
        .expect("evaluation report");
        let attempts: serde_json::Value = serde_json::from_str(include_str!(
            "../../reports/kotlin-retrieval-attempts-openclaw-v1.json"
        ))
        .expect("attempt receipt");
        let raw_reports = [
            include_str!("../../reports/kotlin-retrieval-control-run1.json"),
            include_str!("../../reports/kotlin-retrieval-control-run2.json"),
            include_str!("../../reports/kotlin-retrieval-0.4.0-run1.json"),
            include_str!("../../reports/kotlin-retrieval-0.4.0-run2.json"),
        ];

        assert_eq!(
            diagnostic["corpus"]["revision"],
            "9feb6ad161877da86200693b039638dbf3411e66"
        );
        assert_eq!(diagnostic["corpus"]["files"], 419);
        assert_eq!(diagnostic["summary"]["incomplete_files"], 9);
        assert_eq!(diagnostic["summary"]["error_nodes"], 11);
        assert_eq!(diagnostic["summary"]["missing_nodes"], 0);
        assert_eq!(diagnostic["summary"]["syntax_node_counts"]["imports"], 6768);
        assert!(
            diagnostic["summary"].get("extracted").is_none(),
            "grammar node counts must not be labeled as production extraction"
        );
        let extension_strata = diagnostic["extension_strata"]
            .as_array()
            .expect("extension strata");
        assert_eq!(extension_strata.len(), 2);
        let scripts = extension_strata
            .iter()
            .find(|stratum| stratum["extension"] == "kts")
            .expect("Kotlin script stratum");
        assert_eq!(scripts["counts"]["files"], 6);
        assert_eq!(scripts["counts"]["incomplete_files"], 0);
        let reported_recovery = diagnostic["recovery_categories"]
            .as_array()
            .expect("recovery categories")
            .iter()
            .map(|category| category["count"].as_u64().expect("category count"))
            .sum::<u64>()
            + diagnostic["other_recovery_nodes"]
                .as_u64()
                .expect("other recovery count");
        assert_eq!(reported_recovery, 11);
        assert_eq!(evaluation["decision"], "do_not_ship_kotlin_parser");
        assert_eq!(
            evaluation["determinism_gate"]["result"],
            "inconclusive_legacy_accounting_normalization"
        );
        assert_eq!(
            evaluation["product_test_gate"]["result"],
            "inconclusive_no_candidate_revision_receipt"
        );
        assert!(
            evaluation["gate_failures"]
                .as_array()
                .expect("gate failures")
                .iter()
                .all(|failure| !failure
                    .as_str()
                    .expect("gate failure text")
                    .contains("structurally incomplete")),
            "parse incompleteness was diagnostic, not a frozen gate"
        );
        assert_eq!(
            evaluation["attempt_receipt"],
            "benchmarks/reports/kotlin-retrieval-attempts-openclaw-v1.json"
        );

        for raw in raw_reports {
            assert!(
                !raw.contains("\"content\":"),
                "raw retrieval report retained source content"
            );
            let report: serde_json::Value =
                serde_json::from_str(raw).expect("raw retrieval report");
            assert_eq!(report["schema_version"], 4);
            assert_eq!(
                report["manifest_blake3"],
                "39738183652e4d82af6e3dd73e3426ede8bab517e0f2ed8fd758ad10da207a59"
            );
            assert_eq!(report["aggregate"]["task_count"], 10);
            assert_eq!(report["aggregate"]["relevant_files"], 20);
            assert_eq!(report["aggregate"]["line_anchors"], 82);
        }

        let attempts = attempts["attempts"].as_array().expect("attempt list");
        assert_eq!(attempts.len(), 7);
        assert_eq!(
            attempts
                .iter()
                .filter(|attempt| attempt["outcome"] == "success")
                .count(),
            4
        );
        assert_eq!(
            attempts
                .iter()
                .filter(|attempt| attempt["outcome"] == "harness_false_positive")
                .count(),
            1
        );
    }
}
