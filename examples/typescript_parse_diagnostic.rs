//! Build a bounded, source-free diagnostic for incomplete TypeScript parsing.

use std::collections::{BTreeMap, BTreeSet};
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

use clap::{Parser, Subcommand};
use leantoken::model::ReferenceRole;
use serde::{Deserialize, Serialize};
use tempfile::NamedTempFile;
use tree_sitter::{Language, Node, ParseOptions, Parser as TreeParser, Tree};

const SCHEMA_VERSION: u32 = 1;
const MAX_MANIFEST_BYTES: u64 = 1 << 20;
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
#[command(about = "Diagnose bounded TypeScript and TSX parser recovery")]
struct Args {
    #[command(subcommand)]
    command: DiagnosticCommand,
}

#[derive(Debug, Subcommand)]
enum DiagnosticCommand {
    /// Analyze tracked TypeScript files in a clean checkout at an exact revision.
    Analyze {
        #[arg(long)]
        repository: PathBuf,
        #[arg(long)]
        revision: String,
        #[arg(long)]
        output: PathBuf,
    },
    /// Materialize the repository-owned synthetic fixture report.
    Fixture {
        #[arg(long)]
        output: PathBuf,
    },
    /// Verify that the checked-in synthetic report matches current behavior.
    VerifyFixture,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct DiagnosticReport {
    schema_version: u32,
    parser_dependencies: ParserDependencies,
    corpus: CorpusReport,
    limits: LimitReport,
    summary: DiagnosticCounts,
    strata: Vec<StratumReport>,
    recovery_category_count: u64,
    recovery_categories: Vec<RecoveryCategory>,
    other_recovery_nodes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct ParserDependencies {
    tree_sitter: String,
    tree_sitter_typescript: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct CorpusReport {
    kind: String,
    identity: String,
    content_blake3: String,
    files: u64,
    source_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct LimitReport {
    max_manifest_bytes: u64,
    max_files: u64,
    max_path_bytes: u64,
    max_file_bytes: u64,
    max_total_source_bytes: u64,
    max_nodes_per_file: u64,
    max_total_nodes: u64,
    max_recovery_nodes: u64,
    max_recovery_categories: u64,
    max_reported_recovery_categories: u64,
    diagnostic_tree_parse_timeout_seconds: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct DiagnosticCounts {
    files: u64,
    incomplete_files: u64,
    source_bytes: u64,
    nodes_visited: u64,
    error_nodes: u64,
    missing_nodes: u64,
    extracted: ExtractionCounts,
    retained_in_incomplete_files: ExtractionCounts,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct ExtractionCounts {
    definitions: u64,
    nested_definitions: u64,
    imports: u64,
    references: u64,
    references_with_owner: u64,
    owner_ranges: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct StratumReport {
    language: String,
    source_shape: String,
    counts: DiagnosticCounts,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct RecoveryCategory {
    language: String,
    source_shape: String,
    recovery_kind: String,
    parent_kind: String,
    syntax_kind: String,
    count: u64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct FixtureManifest {
    schema_version: u32,
    corpus_id: String,
    files: Vec<String>,
}

#[derive(Debug, Clone, Copy)]
struct AnalysisLimits {
    max_files: usize,
    max_path_bytes: usize,
    max_file_bytes: u64,
    max_total_source_bytes: u64,
    max_nodes_per_file: u64,
    max_total_nodes: u64,
    max_recovery_nodes: u64,
    max_recovery_categories: usize,
}

impl Default for AnalysisLimits {
    fn default() -> Self {
        Self {
            max_files: MAX_FILES,
            max_path_bytes: MAX_PATH_BYTES,
            max_file_bytes: MAX_FILE_BYTES,
            max_total_source_bytes: MAX_TOTAL_SOURCE_BYTES,
            max_nodes_per_file: MAX_NODES_PER_FILE,
            max_total_nodes: MAX_TOTAL_NODES,
            max_recovery_nodes: MAX_RECOVERY_NODES,
            max_recovery_categories: MAX_RECOVERY_CATEGORIES,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum SourceLanguage {
    TypeScript,
    Tsx,
}

impl SourceLanguage {
    fn label(self) -> &'static str {
        match self {
            Self::TypeScript => "typescript",
            Self::Tsx => "tsx",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum SourceShape {
    OrdinaryOrDeclaration,
    Test,
    MockFixtureOrHarness,
    Generated,
    IntentionalInvalidSyntax,
}

impl SourceShape {
    fn label(self) -> &'static str {
        match self {
            Self::OrdinaryOrDeclaration => "ordinary_or_declaration",
            Self::Test => "test",
            Self::MockFixtureOrHarness => "mock_fixture_or_harness",
            Self::Generated => "generated",
            Self::IntentionalInvalidSyntax => "intentional_invalid_syntax",
        }
    }
}

const LANGUAGES: [SourceLanguage; 2] = [SourceLanguage::TypeScript, SourceLanguage::Tsx];
const SOURCE_SHAPES: [SourceShape; 5] = [
    SourceShape::OrdinaryOrDeclaration,
    SourceShape::Test,
    SourceShape::MockFixtureOrHarness,
    SourceShape::Generated,
    SourceShape::IntentionalInvalidSyntax,
];

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct RecoveryKey {
    language: SourceLanguage,
    source_shape: SourceShape,
    syntax: SyntaxRecoveryKey,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct SyntaxRecoveryKey {
    recovery_kind: String,
    parent_kind: String,
    syntax_kind: String,
}

#[derive(Debug, Default)]
struct TreeDiagnostic {
    nodes_visited: u64,
    error_nodes: u64,
    missing_nodes: u64,
    categories: BTreeMap<SyntaxRecoveryKey, u64>,
}

struct FileObservation<'a> {
    language: SourceLanguage,
    shape: SourceShape,
    source_bytes: u64,
    structurally_complete: bool,
    tree: &'a TreeDiagnostic,
    extracted: &'a ExtractionCounts,
}

struct FinalizedDiagnostic {
    summary: DiagnosticCounts,
    strata: Vec<StratumReport>,
    recovery_category_count: u64,
    recovery_categories: Vec<RecoveryCategory>,
    other_recovery_nodes: u64,
}

struct DiagnosticParsers {
    typescript: TreeParser,
    tsx: TreeParser,
}

impl DiagnosticParsers {
    fn new() -> AnyResult<Self> {
        Ok(Self {
            typescript: configured_parser(tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into())?,
            tsx: configured_parser(tree_sitter_typescript::LANGUAGE_TSX.into())?,
        })
    }

    fn parse(&mut self, language: SourceLanguage, source: &str) -> AnyResult<Tree> {
        let parser = match language {
            SourceLanguage::TypeScript => &mut self.typescript,
            SourceLanguage::Tsx => &mut self.tsx,
        };
        let started = Instant::now();
        let bytes = source.as_bytes();
        let mut input = |offset: usize, _| bytes.get(offset..).unwrap_or_default();
        let mut progress = |_: &tree_sitter::ParseState| {
            if started.elapsed() >= FILE_PARSE_TIMEOUT {
                ControlFlow::Break(())
            } else {
                ControlFlow::Continue(())
            }
        };
        let options = ParseOptions::new().progress_callback(&mut progress);
        parser
            .parse_with_options(&mut input, None, Some(options))
            .ok_or_else(|| invalid_data("TypeScript parser timed out or returned no tree").into())
    }
}

#[derive(Default)]
struct Accumulator {
    summary: DiagnosticCounts,
    strata: BTreeMap<(SourceLanguage, SourceShape), DiagnosticCounts>,
    recovery_categories: BTreeMap<RecoveryKey, u64>,
}

impl Accumulator {
    fn new() -> Self {
        let mut strata = BTreeMap::new();
        for language in LANGUAGES {
            for shape in SOURCE_SHAPES {
                strata.insert((language, shape), DiagnosticCounts::default());
            }
        }
        Self {
            summary: DiagnosticCounts::default(),
            strata,
            recovery_categories: BTreeMap::new(),
        }
    }

    fn record(
        &mut self,
        observation: FileObservation<'_>,
        limits: AnalysisLimits,
    ) -> AnyResult<()> {
        let FileObservation {
            language,
            shape,
            source_bytes,
            structurally_complete,
            tree,
            extracted,
        } = observation;
        record_counts(
            &mut self.summary,
            source_bytes,
            structurally_complete,
            tree,
            extracted,
        )?;
        let stratum = self
            .strata
            .get_mut(&(language, shape))
            .expect("all fixed strata are initialized");
        record_counts(
            stratum,
            source_bytes,
            structurally_complete,
            tree,
            extracted,
        )?;
        if self.summary.nodes_visited > limits.max_total_nodes {
            return Err(invalid_data("diagnostic exceeded its total syntax-node bound").into());
        }
        let recovery_nodes = checked_add(self.summary.error_nodes, self.summary.missing_nodes)?;
        if recovery_nodes > limits.max_recovery_nodes {
            return Err(invalid_data("diagnostic exceeded its recovery-node bound").into());
        }
        for (category, count) in &tree.categories {
            let category = RecoveryKey {
                language,
                source_shape: shape,
                syntax: category.clone(),
            };
            if !self.recovery_categories.contains_key(&category)
                && self.recovery_categories.len() >= limits.max_recovery_categories
            {
                return Err(invalid_data("diagnostic exceeded its recovery-category bound").into());
            }
            let total = self.recovery_categories.entry(category).or_default();
            *total = checked_add(*total, *count)?;
        }
        Ok(())
    }

    fn finish(self) -> AnyResult<FinalizedDiagnostic> {
        let strata = self
            .strata
            .into_iter()
            .map(|((language, shape), counts)| StratumReport {
                language: language.label().to_owned(),
                source_shape: shape.label().to_owned(),
                counts,
            })
            .collect();
        let category_count = u64::try_from(self.recovery_categories.len())
            .map_err(|_| invalid_data("recovery category count overflowed"))?;
        let mut categories = self.recovery_categories.into_iter().collect::<Vec<_>>();
        categories.sort_by(|(left_key, left_count), (right_key, right_count)| {
            right_count
                .cmp(left_count)
                .then_with(|| left_key.cmp(right_key))
        });
        let mut other_recovery_nodes = 0_u64;
        let recovery_categories = categories
            .into_iter()
            .enumerate()
            .filter_map(|(index, (key, count))| {
                if index < MAX_REPORTED_RECOVERY_CATEGORIES {
                    Some(Ok(RecoveryCategory {
                        language: key.language.label().to_owned(),
                        source_shape: key.source_shape.label().to_owned(),
                        recovery_kind: key.syntax.recovery_kind,
                        parent_kind: key.syntax.parent_kind,
                        syntax_kind: key.syntax.syntax_kind,
                        count,
                    }))
                } else {
                    other_recovery_nodes = match checked_add(other_recovery_nodes, count) {
                        Ok(total) => total,
                        Err(error) => return Some(Err(error)),
                    };
                    None
                }
            })
            .collect::<AnyResult<Vec<_>>>()?;
        Ok(FinalizedDiagnostic {
            summary: self.summary,
            strata,
            recovery_category_count: category_count,
            recovery_categories,
            other_recovery_nodes,
        })
    }
}

fn main() -> AnyResult<()> {
    match Args::parse().command {
        DiagnosticCommand::Analyze {
            repository,
            revision,
            output,
        } => {
            let paths = pinned_typescript_paths(&repository, &revision)?;
            let report = analyze_corpus(
                &repository,
                "git",
                &revision,
                paths,
                AnalysisLimits::default(),
            )?;
            write_report(&output, &report)?;
            print_summary(&report, &output);
        }
        DiagnosticCommand::Fixture { output } => {
            let report = fixture_report()?;
            write_report(&output, &report)?;
            print_summary(&report, &output);
        }
        DiagnosticCommand::VerifyFixture => {
            verify_checked_in_fixture()?;
            println!("TypeScript parse diagnostic fixture: ok");
        }
    }
    Ok(())
}

fn configured_parser(language: Language) -> AnyResult<TreeParser> {
    let mut parser = TreeParser::new();
    parser.set_language(&language)?;
    Ok(parser)
}

fn analyze_corpus(
    root: &Path,
    corpus_kind: &str,
    corpus_identity: &str,
    mut paths: Vec<PathBuf>,
    limits: AnalysisLimits,
) -> AnyResult<DiagnosticReport> {
    if paths.len() > limits.max_files {
        return Err(invalid_data("diagnostic exceeded its file-count bound").into());
    }
    if paths.is_empty() {
        return Err(invalid_data("diagnostic input contains no TypeScript files").into());
    }
    validate_corpus_identity(corpus_identity)?;
    paths.sort_by(|left, right| {
        left.to_str()
            .unwrap_or_default()
            .cmp(right.to_str().unwrap_or_default())
    });
    if paths.windows(2).any(|pair| pair[0] == pair[1]) {
        return Err(invalid_data("diagnostic input contains a duplicate path").into());
    }

    let mut corpus_hasher = blake3::Hasher::new();
    let mut accumulator = Accumulator::new();
    let mut diagnostic_parsers = DiagnosticParsers::new()?;
    let mut total_source_bytes = 0_u64;
    for relative_path in paths {
        let normalized_path = validate_relative_typescript_path(&relative_path, limits)?;
        let path = root.join(&relative_path);
        let metadata = fs::symlink_metadata(&path)?;
        if !metadata.file_type().is_file() {
            return Err(invalid_data("diagnostic inputs must be regular files").into());
        }
        let bytes = read_bounded_file(&path, limits.max_file_bytes)?;
        let source_bytes =
            u64::try_from(bytes.len()).map_err(|_| invalid_data("source byte count overflowed"))?;
        total_source_bytes = checked_add(total_source_bytes, source_bytes)?;
        if total_source_bytes > limits.max_total_source_bytes {
            return Err(invalid_data("diagnostic exceeded its total source-byte bound").into());
        }
        let source = std::str::from_utf8(&bytes)
            .map_err(|_| invalid_data("TypeScript diagnostic input is not UTF-8"))?;
        update_corpus_hash(&mut corpus_hasher, normalized_path.as_bytes(), &bytes)?;

        let language = language_for_path(&relative_path)?;
        let tree = diagnostic_parsers.parse(language, source)?;
        let tree_diagnostic = inspect_tree(&tree, limits.max_nodes_per_file)?;
        let parsed = leantoken::parser::parse(&relative_path, source)?;
        let structurally_complete = !tree.root_node().has_error();
        if parsed.structurally_complete != structurally_complete {
            return Err(invalid_data(
                "diagnostic tree completeness disagrees with production extraction",
            )
            .into());
        }
        let recovery_nodes =
            checked_add(tree_diagnostic.error_nodes, tree_diagnostic.missing_nodes)?;
        if !structurally_complete && recovery_nodes == 0 {
            return Err(invalid_data(
                "incomplete syntax tree did not expose an ERROR or MISSING node",
            )
            .into());
        }
        let extracted = extraction_counts(&parsed)?;
        accumulator.record(
            FileObservation {
                language,
                shape: classify_source_shape(&relative_path),
                source_bytes,
                structurally_complete,
                tree: &tree_diagnostic,
                extracted: &extracted,
            },
            limits,
        )?;
    }

    let FinalizedDiagnostic {
        summary,
        strata,
        recovery_category_count,
        recovery_categories,
        other_recovery_nodes,
    } = accumulator.finish()?;
    Ok(DiagnosticReport {
        schema_version: SCHEMA_VERSION,
        parser_dependencies: ParserDependencies {
            tree_sitter: locked_package_version("tree-sitter")?,
            tree_sitter_typescript: locked_package_version("tree-sitter-typescript")?,
        },
        corpus: CorpusReport {
            kind: corpus_kind.to_owned(),
            identity: corpus_identity.to_owned(),
            content_blake3: corpus_hasher.finalize().to_hex().to_string(),
            files: summary.files,
            source_bytes: summary.source_bytes,
        },
        limits: limit_report(),
        summary,
        strata,
        recovery_category_count,
        recovery_categories,
        other_recovery_nodes,
    })
}

fn inspect_tree(tree: &Tree, max_nodes: u64) -> AnyResult<TreeDiagnostic> {
    let mut diagnostic = TreeDiagnostic::default();
    let mut cursor = tree.walk();
    loop {
        let node = cursor.node();
        diagnostic.nodes_visited = checked_add(diagnostic.nodes_visited, 1)?;
        if diagnostic.nodes_visited > max_nodes {
            return Err(invalid_data("diagnostic exceeded its per-file syntax-node bound").into());
        }
        if node.is_error() || node.is_missing() {
            let recovery_kind = if node.is_missing() {
                diagnostic.missing_nodes = checked_add(diagnostic.missing_nodes, 1)?;
                "missing"
            } else {
                diagnostic.error_nodes = checked_add(diagnostic.error_nodes, 1)?;
                "error"
            };
            let category = SyntaxRecoveryKey {
                recovery_kind: recovery_kind.to_owned(),
                parent_kind: safe_kind(node.parent().map_or("<root>", |parent| parent.kind())),
                syntax_kind: recovery_syntax_kind(node),
            };
            let count = diagnostic.categories.entry(category).or_default();
            *count = checked_add(*count, 1)?;
        }

        if cursor.goto_first_child() {
            continue;
        }
        while !cursor.goto_next_sibling() {
            if !cursor.goto_parent() {
                return Ok(diagnostic);
            }
        }
    }
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

fn extraction_counts(parsed: &leantoken::parser::ParseOutput) -> AnyResult<ExtractionCounts> {
    let references = parsed
        .references
        .iter()
        .filter(|reference| reference.role == ReferenceRole::Reference)
        .collect::<Vec<_>>();
    let mut owner_ranges = BTreeSet::new();
    for reference in &references {
        let Some(owner) = reference.enclosing_symbol.as_deref() else {
            continue;
        };
        if let Some(symbol) = parsed
            .symbols
            .iter()
            .filter(|symbol| {
                symbol.name == owner
                    && symbol.start_byte <= reference.start_byte
                    && symbol.end_byte >= reference.end_byte
            })
            .min_by_key(|symbol| {
                (
                    symbol.end_byte.saturating_sub(symbol.start_byte),
                    symbol.start_byte,
                    symbol.end_byte,
                )
            })
        {
            owner_ranges.insert((symbol.start_byte, symbol.end_byte));
        }
    }
    Ok(ExtractionCounts {
        definitions: usize_to_u64(parsed.symbols.len())?,
        nested_definitions: usize_to_u64(
            parsed
                .symbols
                .iter()
                .filter(|symbol| symbol.parent.is_some())
                .count(),
        )?,
        imports: usize_to_u64(parsed.imports.len())?,
        references: usize_to_u64(references.len())?,
        references_with_owner: usize_to_u64(
            references
                .iter()
                .filter(|reference| reference.enclosing_symbol.is_some())
                .count(),
        )?,
        owner_ranges: usize_to_u64(owner_ranges.len())?,
    })
}

fn record_counts(
    counts: &mut DiagnosticCounts,
    source_bytes: u64,
    structurally_complete: bool,
    tree: &TreeDiagnostic,
    extracted: &ExtractionCounts,
) -> AnyResult<()> {
    counts.files = checked_add(counts.files, 1)?;
    counts.source_bytes = checked_add(counts.source_bytes, source_bytes)?;
    counts.nodes_visited = checked_add(counts.nodes_visited, tree.nodes_visited)?;
    counts.error_nodes = checked_add(counts.error_nodes, tree.error_nodes)?;
    counts.missing_nodes = checked_add(counts.missing_nodes, tree.missing_nodes)?;
    counts.extracted.checked_add_assign(extracted)?;
    if !structurally_complete {
        counts.incomplete_files = checked_add(counts.incomplete_files, 1)?;
        counts
            .retained_in_incomplete_files
            .checked_add_assign(extracted)?;
    }
    Ok(())
}

impl ExtractionCounts {
    fn checked_add_assign(&mut self, other: &Self) -> AnyResult<()> {
        self.definitions = checked_add(self.definitions, other.definitions)?;
        self.nested_definitions = checked_add(self.nested_definitions, other.nested_definitions)?;
        self.imports = checked_add(self.imports, other.imports)?;
        self.references = checked_add(self.references, other.references)?;
        self.references_with_owner =
            checked_add(self.references_with_owner, other.references_with_owner)?;
        self.owner_ranges = checked_add(self.owner_ranges, other.owner_ranges)?;
        Ok(())
    }
}

fn classify_source_shape(path: &Path) -> SourceShape {
    let path = path.to_str().unwrap_or_default().to_ascii_lowercase();
    let filename = path.rsplit('/').next().unwrap_or(&path);
    let segments = path.split('/').collect::<Vec<_>>();
    if segments.iter().any(|segment| {
        matches!(
            *segment,
            "invalid" | "malformed" | "syntax-error" | "syntax-errors"
        )
    }) || filename.contains(".invalid.")
        || filename.contains(".malformed.")
    {
        SourceShape::IntentionalInvalidSyntax
    } else if segments
        .iter()
        .any(|segment| matches!(*segment, "generated" | "gen" | "codegen"))
        || filename.contains(".generated.")
    {
        SourceShape::Generated
    } else if segments.iter().any(|segment| {
        matches!(
            *segment,
            "mock" | "mocks" | "__mocks__" | "fixture" | "fixtures" | "harness"
        )
    }) || filename.contains(".mock.")
    {
        SourceShape::MockFixtureOrHarness
    } else if segments
        .iter()
        .any(|segment| matches!(*segment, "test" | "tests" | "__tests__"))
        || filename.contains(".test.")
        || filename.contains(".spec.")
    {
        SourceShape::Test
    } else {
        SourceShape::OrdinaryOrDeclaration
    }
}

fn language_for_path(path: &Path) -> AnyResult<SourceLanguage> {
    let extension = path
        .extension()
        .and_then(|extension| extension.to_str())
        .ok_or_else(|| invalid_data("diagnostic path has no UTF-8 extension"))?
        .to_ascii_lowercase();
    match extension.as_str() {
        "ts" | "mts" | "cts" => Ok(SourceLanguage::TypeScript),
        "tsx" => Ok(SourceLanguage::Tsx),
        _ => Err(invalid_data("diagnostic path is not TypeScript or TSX").into()),
    }
}

fn validate_relative_typescript_path(path: &Path, limits: AnalysisLimits) -> AnyResult<String> {
    let text = path
        .to_str()
        .ok_or_else(|| invalid_data("diagnostic path is not UTF-8"))?;
    if text.is_empty() || text.len() > limits.max_path_bytes || text.contains('\\') {
        return Err(invalid_data("diagnostic path is empty, oversized, or non-portable").into());
    }
    if !path
        .components()
        .all(|component| matches!(component, Component::Normal(_)))
    {
        return Err(invalid_data("diagnostic path must be repository-relative").into());
    }
    language_for_path(path)?;
    Ok(text.to_owned())
}

fn validate_corpus_identity(identity: &str) -> AnyResult<()> {
    if identity.is_empty()
        || identity.len() > 128
        || !identity
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        return Err(invalid_data("corpus identity is not a bounded safe label").into());
    }
    Ok(())
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
    if u64::try_from(bytes.len()).map_err(|_| invalid_data("file size overflowed"))? > limit {
        return Err(invalid_data("diagnostic input exceeded its per-file byte bound").into());
    }
    Ok(bytes)
}

fn update_corpus_hash(
    hasher: &mut blake3::Hasher,
    relative_path: &[u8],
    content: &[u8],
) -> AnyResult<()> {
    hasher.update(
        &u64::try_from(relative_path.len())
            .map_err(|_| invalid_data("path length overflowed"))?
            .to_le_bytes(),
    );
    hasher.update(relative_path);
    hasher.update(
        &u64::try_from(content.len())
            .map_err(|_| invalid_data("content length overflowed"))?
            .to_le_bytes(),
    );
    hasher.update(content);
    Ok(())
}

fn pinned_typescript_paths(repository: &Path, expected_revision: &str) -> AnyResult<Vec<PathBuf>> {
    if expected_revision.len() != 40
        || !expected_revision
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        return Err(invalid_data("revision must be a full lowercase Git object ID").into());
    }
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
    let paths = git_bytes(
        repository,
        &["ls-files", "-z", "--", "*.ts", "*.tsx", "*.mts", "*.cts"],
    )?;
    let mut result = Vec::new();
    for path in paths
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
    {
        let path = std::str::from_utf8(path)
            .map_err(|_| invalid_data("tracked TypeScript path is not UTF-8"))?;
        result.push(PathBuf::from(path));
        if result.len() > MAX_FILES {
            return Err(invalid_data("diagnostic exceeded its file-count bound").into());
        }
    }
    Ok(result)
}

fn git_text(repository: &Path, args: &[&str]) -> AnyResult<String> {
    let bytes = git_bytes(repository, args)?;
    String::from_utf8(bytes).map_err(|_| invalid_data("Git returned non-UTF-8 text output").into())
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
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(io::Error::other(format!(
            "Git command failed with {}: {}",
            output.status,
            stderr.trim()
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

fn fixture_report() -> AnyResult<DiagnosticReport> {
    let fixture_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../benchmarks/fixtures/typescript_parse_diagnostic");
    let manifest_path = fixture_root.join("manifest.json");
    let manifest_bytes = read_bounded_file(&manifest_path, MAX_MANIFEST_BYTES)?;
    let manifest: FixtureManifest = serde_json::from_slice(&manifest_bytes)?;
    if manifest.schema_version != SCHEMA_VERSION {
        return Err(invalid_data("unsupported TypeScript diagnostic fixture schema").into());
    }
    if manifest.files.len() > 256 {
        return Err(invalid_data("TypeScript diagnostic fixture has too many files").into());
    }
    let paths = manifest.files.into_iter().map(PathBuf::from).collect();
    analyze_corpus(
        &fixture_root,
        "synthetic_fixture",
        &manifest.corpus_id,
        paths,
        AnalysisLimits::default(),
    )
}

fn verify_checked_in_fixture() -> AnyResult<()> {
    let expected_path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../benchmarks/fixtures/typescript_parse_diagnostic/report.json");
    let expected_bytes = read_bounded_file(&expected_path, MAX_MANIFEST_BYTES)?;
    let expected: DiagnosticReport = serde_json::from_slice(&expected_bytes)?;
    let actual = fixture_report()?;
    if actual != expected {
        return Err(invalid_data(
            "checked-in TypeScript diagnostic report differs from current behavior",
        )
        .into());
    }
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

fn print_summary(report: &DiagnosticReport, output: &Path) {
    println!(
        "TypeScript parse diagnostic: {} files, {} incomplete, {} ERROR, {} MISSING",
        report.summary.files,
        report.summary.incomplete_files,
        report.summary.error_nodes,
        report.summary.missing_nodes
    );
    println!("Wrote immutable report to {}", output.display());
}

fn limit_report() -> LimitReport {
    LimitReport {
        max_manifest_bytes: MAX_MANIFEST_BYTES,
        max_files: MAX_FILES as u64,
        max_path_bytes: MAX_PATH_BYTES as u64,
        max_file_bytes: MAX_FILE_BYTES,
        max_total_source_bytes: MAX_TOTAL_SOURCE_BYTES,
        max_nodes_per_file: MAX_NODES_PER_FILE,
        max_total_nodes: MAX_TOTAL_NODES,
        max_recovery_nodes: MAX_RECOVERY_NODES,
        max_recovery_categories: MAX_RECOVERY_CATEGORIES as u64,
        max_reported_recovery_categories: MAX_REPORTED_RECOVERY_CATEGORIES as u64,
        diagnostic_tree_parse_timeout_seconds: FILE_PARSE_TIMEOUT.as_secs(),
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
    use std::io::Cursor;

    #[test]
    fn checked_in_fixture_report_matches_current_behavior() {
        verify_checked_in_fixture().expect("checked fixture report");
    }

    #[test]
    fn checked_in_openclaw_report_matches_issue_evidence() {
        let json = include_str!(
            "../benchmarks/reports/typescript-parse-diagnostic-openclaw-v1-2026-07-30.json"
        );
        let report: DiagnosticReport = serde_json::from_str(json).expect("checked OpenClaw report");
        assert_eq!(
            report.corpus.identity,
            "9feb6ad161877da86200693b039638dbf3411e66"
        );
        assert_eq!(
            report.corpus.content_blake3,
            "ba170cefc4bf348ea1b752d7c2fff2ea179f512854b46c5abf46b8035c80d006"
        );
        assert_eq!(report.summary.files, 23_738);
        assert_eq!(report.summary.incomplete_files, 810);
        assert_eq!(report.summary.error_nodes, 1_380);
        assert_eq!(report.summary.missing_nodes, 40);
        assert_eq!(
            report
                .recovery_categories
                .iter()
                .map(|category| category.count)
                .sum::<u64>()
                + report.other_recovery_nodes,
            report.summary.error_nodes + report.summary.missing_nodes
        );
    }

    #[test]
    fn source_shape_classification_is_fixed_and_path_only() {
        assert_eq!(
            classify_source_shape(Path::new("src/service.ts")),
            SourceShape::OrdinaryOrDeclaration
        );
        assert_eq!(
            classify_source_shape(Path::new("src/service.test.ts")),
            SourceShape::Test
        );
        assert_eq!(
            classify_source_shape(Path::new("src/__mocks__/service.test.ts")),
            SourceShape::MockFixtureOrHarness
        );
        assert_eq!(
            classify_source_shape(Path::new("src/generated/service.test.ts")),
            SourceShape::Generated
        );
        assert_eq!(
            classify_source_shape(Path::new("test/invalid/service.generated.ts")),
            SourceShape::IntentionalInvalidSyntax
        );
    }

    #[test]
    fn bounded_reader_rejects_oversized_input() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("large.ts");
        fs::write(&path, b"12345").expect("write oversized fixture");
        let error = read_bounded_file(&path, 4).expect_err("oversized input was accepted");
        assert!(error.to_string().contains("per-file byte bound"));
    }

    #[test]
    fn bounded_stream_marks_reader_failure_for_process_termination() {
        struct FailingReader;

        impl Read for FailingReader {
            fn read(&mut self, _buffer: &mut [u8]) -> io::Result<usize> {
                Err(io::Error::other("reader failed"))
            }
        }

        let terminate = Arc::new(AtomicBool::new(false));
        let error = read_stream_bounded(FailingReader, 4, Arc::clone(&terminate))
            .expect_err("reader failure was accepted");
        assert_eq!(error.kind(), io::ErrorKind::Other);
        assert!(terminate.load(Ordering::Relaxed));
    }

    #[test]
    fn syntax_node_bound_fails_closed() {
        let mut parser = configured_parser(tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into())
            .expect("configured parser");
        let tree = parser
            .parse("const value = 1;", None)
            .expect("TypeScript tree");
        let error = inspect_tree(&tree, 1).expect_err("syntax-node overflow was accepted");
        assert!(error.to_string().contains("per-file syntax-node bound"));
    }

    #[test]
    fn recovery_category_bound_fails_closed() {
        let mut accumulator = Accumulator::new();
        let mut categories = BTreeMap::new();
        categories.insert(
            SyntaxRecoveryKey {
                recovery_kind: "error".to_owned(),
                parent_kind: "program".to_owned(),
                syntax_kind: "identifier".to_owned(),
            },
            1,
        );
        categories.insert(
            SyntaxRecoveryKey {
                recovery_kind: "missing".to_owned(),
                parent_kind: "program".to_owned(),
                syntax_kind: ";".to_owned(),
            },
            1,
        );
        let tree = TreeDiagnostic {
            nodes_visited: 2,
            error_nodes: 1,
            missing_nodes: 1,
            categories,
        };
        let limits = AnalysisLimits {
            max_recovery_categories: 1,
            ..AnalysisLimits::default()
        };
        let error = accumulator
            .record(
                FileObservation {
                    language: SourceLanguage::TypeScript,
                    shape: SourceShape::Test,
                    source_bytes: 1,
                    structurally_complete: false,
                    tree: &tree,
                    extracted: &ExtractionCounts::default(),
                },
                limits,
            )
            .expect_err("recovery-category overflow was accepted");
        assert!(error.to_string().contains("recovery-category bound"));
    }

    #[test]
    fn empty_and_file_count_inputs_fail_closed() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let identity = "0000000000000000000000000000000000000000";
        let empty_error = analyze_corpus(
            directory.path(),
            "test",
            identity,
            Vec::new(),
            AnalysisLimits::default(),
        )
        .expect_err("empty corpus was accepted");
        assert!(empty_error.to_string().contains("no TypeScript files"));

        let limits = AnalysisLimits {
            max_files: 0,
            ..AnalysisLimits::default()
        };
        let count_error = analyze_corpus(
            directory.path(),
            "test",
            identity,
            vec![PathBuf::from("one.ts")],
            limits,
        )
        .expect_err("file-count overflow was accepted");
        assert!(count_error.to_string().contains("file-count bound"));
    }

    #[test]
    fn report_output_is_immutable() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("report.json");
        let report = fixture_report().expect("fixture report");
        write_report(&path, &report).expect("first immutable write");
        let before = fs::read(&path).expect("read first report");
        let error = write_report(&path, &report).expect_err("existing report was overwritten");
        assert_eq!(
            error.downcast_ref::<io::Error>().map(io::Error::kind),
            Some(io::ErrorKind::AlreadyExists)
        );
        assert_eq!(fs::read(path).expect("read retained report"), before);
    }

    #[test]
    fn bounded_stream_rejects_oversized_input() {
        let overflow = Arc::new(AtomicBool::new(false));
        let retained = read_stream_bounded(Cursor::new(b"12345"), 4, Arc::clone(&overflow))
            .expect("bounded reader");
        assert_eq!(retained, b"1234");
        assert!(overflow.load(Ordering::Relaxed));
    }

    #[cfg(unix)]
    #[test]
    fn pinned_git_inventory_does_not_execute_repository_fsmonitor() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().expect("temporary directory");
        let repository = directory.path();
        let run_git = |args: &[&str]| {
            let status = Command::new("git")
                .arg("-C")
                .arg(repository)
                .args(args)
                .status()
                .expect("run fixture Git command");
            assert!(status.success(), "fixture Git command failed: {args:?}");
        };
        run_git(&["init", "--quiet"]);
        run_git(&["config", "user.name", "LeanToken test"]);
        run_git(&["config", "user.email", "leantoken@example.invalid"]);
        fs::write(repository.join("one.ts"), "export const one = 1;\n")
            .expect("write TypeScript fixture");
        run_git(&["add", "one.ts"]);
        run_git(&["commit", "--quiet", "-m", "fixture"]);

        let hook = repository.join("fsmonitor-hook");
        fs::write(&hook, "#!/bin/sh\ntouch \"$0.ran\"\n").expect("write fsmonitor hook");
        let mut permissions = fs::metadata(&hook).expect("hook metadata").permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&hook, permissions).expect("make fsmonitor hook executable");
        run_git(&[
            "config",
            "core.fsmonitor",
            hook.to_str().expect("UTF-8 hook path"),
        ]);

        let revision =
            git_text(repository, &["rev-parse", "HEAD^{commit}"]).expect("fixture revision");
        let paths = pinned_typescript_paths(repository, revision.trim()).expect("pinned inventory");
        assert_eq!(paths, vec![PathBuf::from("one.ts")]);
        assert!(!repository.join("fsmonitor-hook.ran").exists());
    }

    #[test]
    fn report_does_not_retain_fixture_paths_or_source() {
        let report = fixture_report().expect("fixture report");
        let json = serde_json::to_string(&report).expect("serialize report");
        for private_value in [
            "PRIVATE_TYPESCRIPT_SENTINEL",
            "import-original.test.ts",
            "broken-component.invalid.tsx",
        ] {
            assert!(!json.contains(private_value));
        }
    }

    #[test]
    fn dependency_versions_match_the_locked_graph() {
        assert_eq!(
            locked_package_version("tree-sitter").expect("tree-sitter version"),
            "0.26.11"
        );
        assert_eq!(
            locked_package_version("tree-sitter-typescript")
                .expect("tree-sitter-typescript version"),
            "0.23.2"
        );
    }

    #[test]
    fn relative_path_validation_fails_closed() {
        let limits = AnalysisLimits::default();
        for path in [
            Path::new("../escape.ts"),
            Path::new("/absolute.ts"),
            Path::new("windows\\path.ts"),
            Path::new("not-typescript.rs"),
        ] {
            assert!(validate_relative_typescript_path(path, limits).is_err());
        }
    }
}
