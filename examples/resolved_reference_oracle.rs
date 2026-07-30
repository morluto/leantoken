//! Evaluate a bounded Python resolved-reference oracle for API migrations.

use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::mem;
use std::path::{Component, Path, PathBuf};

use clap::{Parser, Subcommand};
use leantoken::tokens::Tokenizer;
use regex::Regex;
use serde::{Deserialize, Serialize};
use tempfile::NamedTempFile;
use tree_sitter::{Node, Parser as TreeParser, Tree};

const SCHEMA_VERSION: u32 = 1;
const DEFAULT_MANIFEST: &str = "benchmarks/resolved_reference_oracle_v1.json";
const CHECKED_REPORT: &str = "benchmarks/reports/resolved-reference-oracle-python-v1.json";
const LOCKFILE: &str = include_str!("../Cargo.lock");
const MAX_HARD_MANIFEST_BYTES: u64 = 256 << 10;
const MAX_HARD_SOURCE_BYTES: u64 = 64 << 10;
const MAX_HARD_AST_NODES: u64 = 10_000;
const MAX_HARD_CANDIDATES: usize = 256;
const MAX_HARD_TYPE_BINDINGS: usize = 256;
const MAX_HARD_IDENTIFIER_BYTES: usize = 128;
const MAX_HARD_PAYLOAD_TOKENS: usize = 2_048;

type AnyError = Box<dyn Error + Send + Sync>;
type AnyResult<T> = Result<T, AnyError>;

#[derive(Debug, Parser)]
#[command(about = "Evaluate a bounded Python resolved-reference oracle")]
struct Args {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Evaluate a frozen manifest and write a new immutable report.
    Evaluate {
        #[arg(long, default_value = DEFAULT_MANIFEST)]
        manifest: PathBuf,
        #[arg(long, default_value = ".")]
        repository_root: PathBuf,
        #[arg(long)]
        output: PathBuf,
    },
    /// Verify the repository-owned fixture and checked report.
    VerifyFixture,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Manifest {
    schema_version: u32,
    experiment_id: String,
    language: String,
    source: SourceSpec,
    target: Target,
    limits: Limits,
    expected: Vec<Occurrence>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct SourceSpec {
    path: String,
    blake3: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Target {
    owner: String,
    symbol: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Limits {
    max_manifest_bytes: u64,
    max_source_bytes: u64,
    max_ast_nodes: u64,
    max_candidates: usize,
    max_type_bindings: usize,
    max_identifier_bytes: usize,
    max_candidate_payload_tokens: usize,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
enum Classification {
    Resolved,
    Ambiguous,
    Unrelated,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
enum Confidence {
    High,
    Low,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
enum Role {
    TargetDefinition,
    ProtocolDefinition,
    SubclassDefinition,
    FixtureMockDefinition,
    WrapperDefinition,
    UnrelatedDefinition,
    WrapperForwarder,
    ExportAliasDefinition,
    ExportSourceReference,
    ReexportAliasReference,
    ResolvedCall,
    AmbiguousCall,
    UnrelatedCall,
    ResolvedValueReference,
    AmbiguousValueReference,
    UnrelatedValueReference,
    NonCodeString,
    NonCodeComment,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(deny_unknown_fields)]
struct Occurrence {
    start: Position,
    end: Position,
    classification: Classification,
    role: Role,
    confidence: Confidence,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(deny_unknown_fields)]
struct Position {
    line: u64,
    column: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Report {
    schema_version: u32,
    experiment_id: String,
    manifest_blake3: String,
    source: SourceReport,
    engine: EngineReport,
    target: Target,
    limits: Limits,
    coverage: CoverageReport,
    measurements: Measurements,
    comparison: ComparisonReport,
    observations: Vec<Occurrence>,
    decision: DecisionReport,
    limitations: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct SourceReport {
    path: String,
    blake3: String,
    bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct EngineReport {
    language: String,
    tree_sitter: String,
    tree_sitter_python: String,
    tokenizer: String,
    token_count_exact: bool,
    evaluation_only: bool,
    production_index_rows_loaded: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct CoverageReport {
    syntax_complete: bool,
    lexical_target_occurrences: u64,
    classified_occurrences: u64,
    unclassified_occurrences: u64,
    exact_membership: bool,
    exact_coordinates: bool,
    exact_classification: bool,
    exact_roles: bool,
    exact_confidence: bool,
    supported_resolution_shapes: Vec<String>,
    explicit_coverage_gaps: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Measurements {
    source_bytes: u64,
    unique_ast_nodes_collected: u64,
    modeled_post_parse_ast_node_inspection_upper_bound: u64,
    modeled_post_parse_lookup_iteration_upper_bound: u64,
    candidate_volume: u64,
    ast_candidates: u64,
    string_or_comment_candidates: u64,
    type_bindings_loaded: u64,
    retained_candidate_bytes: u64,
    modeled_partial_allocation_estimate_bytes: u64,
    candidate_payload_tokens_cl100k: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct ComparisonReport {
    expected_occurrences: u64,
    observed_occurrences: u64,
    false_positives: u64,
    false_negatives: u64,
    coordinate_mismatches: u64,
    classification_mismatches: u64,
    role_mismatches: u64,
    confidence_mismatches: u64,
    resolved: ClassCount,
    ambiguous: ClassCount,
    unrelated: ClassCount,
    passed: bool,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct ClassCount {
    expected: u64,
    observed: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct DecisionReport {
    oracle_result: String,
    issue_outcome: String,
    add_public_impact_analysis_tool: bool,
    rationale: String,
}

#[derive(Debug, Clone)]
struct ClassInfo {
    name: String,
    bases: Vec<String>,
    start_byte: usize,
    end_byte: usize,
    module_scope: bool,
    module_binding_end_byte: usize,
    fields: BTreeMap<String, String>,
}

#[derive(Debug, Clone)]
struct FunctionInfo {
    start_byte: usize,
    end_byte: usize,
    owner: Option<String>,
    bindings: BTreeMap<String, String>,
}

#[derive(Debug)]
struct Analysis {
    observations: Vec<Occurrence>,
    unique_ast_nodes_collected: u64,
    ast_candidates: u64,
    non_code_candidates: u64,
    type_bindings_loaded: u64,
    retained_candidate_bytes: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Resolution {
    Resolved,
    Unrelated,
    Ambiguous,
}

fn main() -> AnyResult<()> {
    match Args::parse().command {
        Command::Evaluate {
            manifest,
            repository_root,
            output,
        } => {
            let report = evaluate_manifest(&manifest, &repository_root)?;
            write_report(&output, &report)?;
            println!(
                "Resolved-reference oracle: {} candidates, {}, wrote {}",
                report.measurements.candidate_volume,
                report.decision.oracle_result,
                output.display()
            );
            if !report.comparison.passed {
                return Err(invalid_data("resolved-reference oracle comparison failed").into());
            }
        }
        Command::VerifyFixture => {
            verify_fixture()?;
            println!("Resolved-reference oracle fixture: ok");
        }
    }
    Ok(())
}

fn evaluate_manifest(manifest_path: &Path, repository_root: &Path) -> AnyResult<Report> {
    let manifest_bytes = read_bounded_file(manifest_path, MAX_HARD_MANIFEST_BYTES)?;
    let raw_manifest_len = manifest_bytes.len();
    let normalized_manifest = normalize_lf(&manifest_bytes)?;
    let manifest: Manifest = serde_json::from_str(&normalized_manifest)?;
    validate_manifest(&manifest, raw_manifest_len)?;

    let source_path = validate_relative_path(&manifest.source.path)?;
    let source_bytes = read_bounded_file(
        &repository_root.join(source_path),
        manifest.limits.max_source_bytes,
    )?;
    let source = normalize_lf(&source_bytes)?;
    let source_hash = blake3::hash(source.as_bytes()).to_hex().to_string();
    if source_hash != manifest.source.blake3 {
        return Err(invalid_data("source BLAKE3 does not match the frozen manifest").into());
    }

    let mut parser = TreeParser::new();
    parser.set_language(&tree_sitter_python::LANGUAGE.into())?;
    let tree = parser
        .parse(source.as_bytes(), None)
        .ok_or_else(|| invalid_data("Python parser returned no tree"))?;
    if tree.root_node().has_error() {
        return Err(invalid_data("Python oracle source contains syntax recovery nodes").into());
    }

    let analysis = analyze(&tree, &source, &manifest.target, manifest.limits)?;
    let comparison = compare(&manifest.expected, &analysis.observations);
    let oracle_passed = comparison.passed;
    let payload = serde_json::to_string(&analysis.observations)?;
    let payload_tokens = Tokenizer::Cl100kBase.count(&payload);
    if payload_tokens > manifest.limits.max_candidate_payload_tokens {
        return Err(invalid_data("candidate payload exceeded its exact token bound").into());
    }
    let source_len =
        u64::try_from(source.len()).map_err(|_| invalid_data("source byte count overflowed"))?;
    let modeled_post_parse_ast_node_inspection_upper_bound =
        modeled_post_parse_ast_node_inspection_upper_bound(manifest.limits)?;
    let modeled_post_parse_lookup_iteration_upper_bound =
        modeled_post_parse_lookup_iteration_upper_bound(manifest.limits)?;
    let modeled_partial_allocation_estimate_bytes =
        modeled_partial_allocation_estimate(manifest.limits)?;
    let (exact_membership, exact_coordinates, exact_classification, exact_roles, exact_confidence) =
        comparison_exactness(&comparison);

    Ok(Report {
        schema_version: SCHEMA_VERSION,
        experiment_id: manifest.experiment_id,
        manifest_blake3: blake3::hash(normalized_manifest.as_bytes())
            .to_hex()
            .to_string(),
        source: SourceReport {
            path: manifest.source.path,
            blake3: source_hash,
            bytes: source_len,
        },
        engine: EngineReport {
            language: "python".into(),
            tree_sitter: locked_package_version("tree-sitter")?,
            tree_sitter_python: locked_package_version("tree-sitter-python")?,
            tokenizer: Tokenizer::Cl100kBase.name().into(),
            token_count_exact: Tokenizer::Cl100kBase.is_exact(),
            evaluation_only: true,
            production_index_rows_loaded: 0,
        },
        target: manifest.target,
        limits: manifest.limits,
        coverage: CoverageReport {
            syntax_complete: true,
            lexical_target_occurrences: analysis.observations.len() as u64,
            classified_occurrences: analysis.observations.len() as u64,
            unclassified_occurrences: 0,
            exact_membership,
            exact_coordinates,
            exact_classification,
            exact_roles,
            exact_confidence,
            supported_resolution_shapes: vec![
                "target, ancestor-protocol, subclass, and mock method definitions".into(),
                "typed function parameters, straight-line local rebindings, and direct constructors".into(),
                "wrapper fields assigned from typed constructor parameters".into(),
                "module export aliases and one-hop re-export aliases".into(),
                "same-name methods on known unrelated receiver types".into(),
                "unknown receivers retained as ambiguous".into(),
                "string and comment matches retained as non-executable".into(),
            ],
            explicit_coverage_gaps: vec![
                "dynamic dispatch, monkey-patching, descriptors, and getattr/setattr".into(),
                "import aliases, star imports, and cross-module re-export chains".into(),
                "generic, union, stringized, and inferred return types".into(),
                "control-flow-sensitive local bindings, closures, comprehensions, and lambda scopes".into(),
                "decorator-generated methods and runtime protocol registration".into(),
                "languages other than Python and non-synthetic repositories".into(),
            ],
        },
        measurements: Measurements {
            source_bytes: source_len,
            unique_ast_nodes_collected: analysis.unique_ast_nodes_collected,
            modeled_post_parse_ast_node_inspection_upper_bound,
            modeled_post_parse_lookup_iteration_upper_bound,
            candidate_volume: analysis.observations.len() as u64,
            ast_candidates: analysis.ast_candidates,
            string_or_comment_candidates: analysis.non_code_candidates,
            type_bindings_loaded: analysis.type_bindings_loaded,
            retained_candidate_bytes: analysis.retained_candidate_bytes,
            modeled_partial_allocation_estimate_bytes,
            candidate_payload_tokens_cl100k: payload_tokens as u64,
        },
        comparison,
        observations: analysis.observations,
        decision: decision_for_result(oracle_passed),
        limitations: vec![
            "This is a deterministic mechanism check, not a representative repository benchmark.".into(),
            "The partial allocation estimate models source copies, candidate slots, type-binding strings, and 64 bytes per configured AST node; it excludes manifest/deserialization/report buffers and parser/tree allocator overhead, so it is not a memory bound. Peak RSS is recorded separately because it is host- and build-dependent.".into(),
            "Gold labels are used only after candidate discovery and classification, for exact comparison.".into(),
            "No production service, index schema, CLI command, MCP schema, or ranking behavior changes.".into(),
        ],
    })
}

fn validate_manifest(manifest: &Manifest, manifest_bytes: usize) -> AnyResult<()> {
    if manifest.schema_version != SCHEMA_VERSION {
        return Err(invalid_data("unsupported resolved-reference manifest schema").into());
    }
    if manifest.language != "python" {
        return Err(invalid_data("resolved-reference oracle supports only Python").into());
    }
    if manifest.experiment_id.trim().is_empty()
        || manifest.source.blake3.len() != 64
        || !manifest
            .source
            .blake3
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(invalid_data("manifest identity is incomplete").into());
    }
    validate_identifier(&manifest.target.owner, manifest.limits.max_identifier_bytes)?;
    validate_identifier(
        &manifest.target.symbol,
        manifest.limits.max_identifier_bytes,
    )?;
    validate_relative_path(&manifest.source.path)?;
    validate_limits(manifest.limits, manifest_bytes)?;
    if manifest.expected.is_empty()
        || manifest.expected.len() > manifest.limits.max_candidates
        || !manifest
            .expected
            .windows(2)
            .all(|pair| pair[0].start < pair[1].start)
    {
        return Err(invalid_data(
            "expected occurrences must be non-empty, bounded, uniquely positioned, and sorted",
        )
        .into());
    }
    Ok(())
}

fn validate_limits(limits: Limits, manifest_bytes: usize) -> AnyResult<()> {
    let valid = limits.max_manifest_bytes > 0
        && limits.max_manifest_bytes <= MAX_HARD_MANIFEST_BYTES
        && manifest_bytes as u64 <= limits.max_manifest_bytes
        && limits.max_source_bytes > 0
        && limits.max_source_bytes <= MAX_HARD_SOURCE_BYTES
        && limits.max_ast_nodes > 0
        && limits.max_ast_nodes <= MAX_HARD_AST_NODES
        && limits.max_candidates > 0
        && limits.max_candidates <= MAX_HARD_CANDIDATES
        && limits.max_type_bindings > 0
        && limits.max_type_bindings <= MAX_HARD_TYPE_BINDINGS
        && limits.max_identifier_bytes > 0
        && limits.max_identifier_bytes <= MAX_HARD_IDENTIFIER_BYTES
        && limits.max_candidate_payload_tokens > 0
        && limits.max_candidate_payload_tokens <= MAX_HARD_PAYLOAD_TOKENS;
    if !valid {
        return Err(
            invalid_data("manifest limits are zero, inconsistent, or above hard caps").into(),
        );
    }
    Ok(())
}

fn analyze(tree: &Tree, source: &str, target: &Target, limits: Limits) -> AnyResult<Analysis> {
    let mut unique_ast_nodes_collected = 0_u64;
    let nodes = collect_nodes(
        tree.root_node(),
        limits.max_ast_nodes,
        &mut unique_ast_nodes_collected,
    )?;
    let classes = collect_classes(&nodes, source, limits)?;
    let functions = collect_functions(&nodes, source, &classes, limits)?;
    let classes = attach_field_bindings(classes, &nodes, source, &functions, limits)?;
    let alias_references =
        collect_alias_references(&nodes, source, &classes, &functions, target, limits)?;

    let mut observations = Vec::new();
    let mut ast_candidates = 0_u64;
    let mut non_code_candidates = 0_u64;
    let mut seen_offsets = BTreeSet::new();

    for node in &nodes {
        match node.kind() {
            "identifier" if node_text(*node, source)? == target.symbol => {
                if observations.len() >= limits.max_candidates {
                    return Err(
                        invalid_data("candidate discovery exceeded its configured bound").into(),
                    );
                }
                let occurrence = classify_identifier(
                    *node,
                    source,
                    target,
                    &classes,
                    &functions,
                    &alias_references,
                )?;
                push_candidate(
                    &mut observations,
                    &mut seen_offsets,
                    occurrence,
                    node.start_byte(),
                    limits.max_candidates,
                )?;
                ast_candidates = checked_add(ast_candidates, 1)?;
            }
            // Python f-strings contain executable interpolation descendants.
            // Scanning only literal content keeps those identifiers in the AST
            // path while still retaining ordinary string occurrences.
            "string_content" | "comment" => {
                for offset in word_offsets(node_text(*node, source)?, &target.symbol) {
                    let byte = node
                        .start_byte()
                        .checked_add(offset)
                        .ok_or_else(|| invalid_data("candidate byte offset overflowed"))?;
                    let role = if node.kind() == "string_content" {
                        Role::NonCodeString
                    } else {
                        Role::NonCodeComment
                    };
                    let occurrence = occurrence_at(
                        source,
                        byte,
                        target.symbol.len(),
                        Classification::Unrelated,
                        role,
                        Confidence::High,
                    )?;
                    push_candidate(
                        &mut observations,
                        &mut seen_offsets,
                        occurrence,
                        byte,
                        limits.max_candidates,
                    )?;
                    non_code_candidates = checked_add(non_code_candidates, 1)?;
                }
            }
            _ => {}
        }
    }
    observations.sort();
    let lexical_count = word_offsets(source, &target.symbol).len();
    if lexical_count != observations.len() {
        return Err(invalid_data(
            "candidate discovery did not classify every lexical target occurrence",
        )
        .into());
    }

    let type_bindings_loaded = classes
        .iter()
        .map(|class| class.fields.len())
        .sum::<usize>()
        .checked_add(
            functions
                .iter()
                .map(|function| function.bindings.len())
                .sum::<usize>(),
        )
        .ok_or_else(|| invalid_data("type binding count overflowed"))?;
    if type_bindings_loaded > limits.max_type_bindings {
        return Err(invalid_data("type bindings exceeded their configured bound").into());
    }
    let retained_candidate_bytes = observations
        .len()
        .checked_mul(mem::size_of::<Occurrence>())
        .ok_or_else(|| invalid_data("retained candidate bytes overflowed"))?;

    Ok(Analysis {
        observations,
        unique_ast_nodes_collected,
        ast_candidates,
        non_code_candidates,
        type_bindings_loaded: type_bindings_loaded as u64,
        retained_candidate_bytes: retained_candidate_bytes as u64,
    })
}

fn collect_nodes<'tree>(
    root: Node<'tree>,
    max_nodes: u64,
    visited: &mut u64,
) -> AnyResult<Vec<Node<'tree>>> {
    let mut result = Vec::new();
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        *visited = checked_add(*visited, 1)?;
        if *visited > max_nodes {
            return Err(invalid_data("AST traversal exceeded its node bound").into());
        }
        result.push(node);
        let mut cursor = node.walk();
        let retained = result
            .len()
            .checked_add(stack.len())
            .ok_or_else(|| invalid_data("retained AST node count overflowed"))?;
        let retained = u64::try_from(retained)
            .map_err(|_| invalid_data("retained AST node count overflowed"))?;
        let remaining = max_nodes
            .checked_sub(retained)
            .ok_or_else(|| invalid_data("retained AST nodes exceeded their configured bound"))?;
        let remaining = usize::try_from(remaining)
            .map_err(|_| invalid_data("remaining AST node allowance overflowed"))?;
        let mut children = Vec::new();
        for child in node.children(&mut cursor) {
            if children.len() >= remaining {
                return Err(
                    invalid_data("AST traversal exceeded its node bound while queuing").into(),
                );
            }
            children.push(child);
        }
        stack.extend(children.into_iter().rev());
    }
    Ok(result)
}

fn collect_classes(nodes: &[Node<'_>], source: &str, limits: Limits) -> AnyResult<Vec<ClassInfo>> {
    let mut classes = Vec::new();
    for node in nodes
        .iter()
        .copied()
        .filter(|node| node.kind() == "class_definition")
    {
        let name = node
            .child_by_field_name("name")
            .ok_or_else(|| invalid_data("class definition omitted its name"))?;
        let name = node_text(name, source)?.to_owned();
        validate_identifier(&name, limits.max_identifier_bytes)?;
        let mut bases = Vec::new();
        if let Some(superclasses) = node.child_by_field_name("superclasses") {
            collect_identifier_text(superclasses, source, &mut bases)?;
            bases.retain(|base| base != &name);
            bases.sort();
            bases.dedup();
            for base in &bases {
                validate_identifier(base, limits.max_identifier_bytes)?;
            }
        }
        classes.push(ClassInfo {
            name,
            bases,
            start_byte: node.start_byte(),
            end_byte: node.end_byte(),
            module_scope: is_lexical_module_scope(node),
            module_binding_end_byte: usize::MAX,
            fields: BTreeMap::new(),
        });
    }
    classes.sort_by_key(|class| class.start_byte);
    for node in nodes {
        if !is_lexical_module_scope(*node) {
            continue;
        }
        let Some(name) = module_binding_name(*node, source)? else {
            continue;
        };
        let effective_byte = node.end_byte();
        if let Some(class) = classes
            .iter_mut()
            .filter(|class| {
                class.module_scope
                    && class.name == name
                    && class.end_byte < effective_byte
                    && class.module_binding_end_byte == usize::MAX
            })
            .max_by_key(|class| class.end_byte)
        {
            class.module_binding_end_byte = effective_byte;
        }
    }
    Ok(classes)
}

fn collect_functions(
    nodes: &[Node<'_>],
    source: &str,
    classes: &[ClassInfo],
    limits: Limits,
) -> AnyResult<Vec<FunctionInfo>> {
    let annotation = Regex::new(r"(?m)([A-Za-z_][A-Za-z0-9_]*)\s*:\s*([A-Za-z_][A-Za-z0-9_]*)")?;
    let mut functions = Vec::new();
    let mut binding_count = 0_usize;
    for node in nodes
        .iter()
        .copied()
        .filter(|node| node.kind() == "function_definition")
    {
        let parameters = node
            .child_by_field_name("parameters")
            .ok_or_else(|| invalid_data("function definition omitted parameters"))?;
        let parameter_text = node_text(parameters, source)?;
        let mut bindings = BTreeMap::new();
        for captures in annotation.captures_iter(parameter_text) {
            let name = captures.get(1).expect("capture one exists").as_str();
            let annotation = captures.get(2).expect("capture two exists").as_str();
            validate_identifier(name, limits.max_identifier_bytes)?;
            validate_identifier(annotation, limits.max_identifier_bytes)?;
            bindings.insert(name.to_owned(), annotation.to_owned());
            binding_count = binding_count
                .checked_add(1)
                .ok_or_else(|| invalid_data("type binding count overflowed"))?;
            if binding_count > limits.max_type_bindings {
                return Err(invalid_data("type bindings exceeded their configured bound").into());
            }
        }
        functions.push(FunctionInfo {
            start_byte: node.start_byte(),
            end_byte: node.end_byte(),
            owner: direct_enclosing_class(node, classes).map(|class| class.name.clone()),
            bindings,
        });
    }
    functions.sort_by_key(|function| function.start_byte);
    Ok(functions)
}

fn attach_field_bindings(
    mut classes: Vec<ClassInfo>,
    nodes: &[Node<'_>],
    source: &str,
    functions: &[FunctionInfo],
    limits: Limits,
) -> AnyResult<Vec<ClassInfo>> {
    let assignment =
        Regex::new(r"^self\.([A-Za-z_][A-Za-z0-9_]*)\s*=\s*([A-Za-z_][A-Za-z0-9_]*)$")?;
    let mut count = functions
        .iter()
        .map(|function| function.bindings.len())
        .sum::<usize>();
    for node in nodes
        .iter()
        .copied()
        .filter(|node| node.kind() == "assignment")
    {
        let text = node_text(node, source)?.trim();
        let Some(captures) = assignment.captures(text) else {
            continue;
        };
        let Some(function) = enclosing_function(node.start_byte(), functions) else {
            continue;
        };
        let Some(owner) = function.owner.as_deref() else {
            continue;
        };
        let field = captures.get(1).expect("capture one exists").as_str();
        let parameter = captures.get(2).expect("capture two exists").as_str();
        let Some(annotation) = function.bindings.get(parameter) else {
            continue;
        };
        validate_identifier(field, limits.max_identifier_bytes)?;
        let class = classes
            .iter_mut()
            .find(|class| class.name == owner)
            .ok_or_else(|| invalid_data("enclosing class disappeared"))?;
        class.fields.insert(field.to_owned(), annotation.clone());
        count = count
            .checked_add(1)
            .ok_or_else(|| invalid_data("type binding count overflowed"))?;
        if count > limits.max_type_bindings {
            return Err(invalid_data("type bindings exceeded their configured bound").into());
        }
    }
    Ok(classes)
}

fn collect_alias_references(
    nodes: &[Node<'_>],
    source: &str,
    classes: &[ClassInfo],
    functions: &[FunctionInfo],
    target: &Target,
    limits: Limits,
) -> AnyResult<BTreeMap<usize, Resolution>> {
    let mut references = BTreeMap::new();
    let mut current_binding = None;
    for node in nodes.iter().copied() {
        if !is_lexical_module_scope(node) {
            continue;
        }
        if node.kind() == "assignment" {
            let (Some(left), Some(right)) = (
                node.child_by_field_name("left"),
                node.child_by_field_name("right"),
            ) else {
                continue;
            };
            if right.kind() == "identifier" && node_text(right, source)? == target.symbol {
                if references.len() >= limits.max_candidates {
                    return Err(
                        invalid_data("alias references exceeded the candidate bound").into(),
                    );
                }
                references.insert(
                    right.start_byte(),
                    current_binding.unwrap_or(Resolution::Ambiguous),
                );
            }
            if left.kind() != "identifier" || node_text(left, source)? != target.symbol {
                continue;
            }
            current_binding = Some(
                if right.kind() == "attribute"
                    && right
                        .child_by_field_name("attribute")
                        .is_some_and(|attribute| {
                            node_text(attribute, source).ok() == Some(&target.symbol)
                        })
                {
                    resolve_attribute(right, source, classes, functions, target)?
                } else if right.kind() == "identifier" && node_text(right, source)? == target.symbol
                {
                    current_binding.unwrap_or(Resolution::Ambiguous)
                } else {
                    Resolution::Ambiguous
                },
            );
        } else if matches!(node.kind(), "function_definition" | "class_definition")
            && node
                .child_by_field_name("name")
                .is_some_and(|name| node_text(name, source).ok() == Some(&target.symbol))
        {
            current_binding = Some(Resolution::Ambiguous);
        }
    }
    Ok(references)
}

fn classify_identifier(
    node: Node<'_>,
    source: &str,
    target: &Target,
    classes: &[ClassInfo],
    functions: &[FunctionInfo],
    alias_references: &BTreeMap<usize, Resolution>,
) -> AnyResult<Occurrence> {
    let parent = node
        .parent()
        .ok_or_else(|| invalid_data("target identifier omitted its parent"))?;
    let module_assignment =
        parent.kind() == "assignment" && is_module_scope(parent.start_byte(), classes, functions);
    let export_definition_resolution = if module_assignment
        && parent
            .child_by_field_name("left")
            .is_some_and(|left| left.id() == node.id())
    {
        match parent.child_by_field_name("right") {
            Some(right) if right.kind() == "attribute" => Some(resolve_attribute(
                right, source, classes, functions, target,
            )?),
            _ => None,
        }
    } else {
        None
    };
    let (classification, role, confidence) = if parent.kind() == "function_definition"
        && parent
            .child_by_field_name("name")
            .is_some_and(|name| name.id() == node.id())
    {
        classify_method_definition(parent, source, target, classes, functions)?
    } else if parent.kind() == "attribute"
        && parent
            .child_by_field_name("attribute")
            .is_some_and(|attribute| attribute.id() == node.id())
    {
        classify_attribute(parent, source, target, classes, functions)?
    } else if let Some(resolution) = export_definition_resolution {
        match resolution {
            Resolution::Resolved => (
                Classification::Resolved,
                Role::ExportAliasDefinition,
                Confidence::High,
            ),
            Resolution::Unrelated => (
                Classification::Unrelated,
                Role::UnrelatedDefinition,
                Confidence::High,
            ),
            Resolution::Ambiguous => (
                Classification::Ambiguous,
                Role::AmbiguousValueReference,
                Confidence::Low,
            ),
        }
    } else if module_assignment
        && parent
            .child_by_field_name("right")
            .is_some_and(|right| right.id() == node.id())
    {
        match alias_references
            .get(&node.start_byte())
            .copied()
            .unwrap_or(Resolution::Ambiguous)
        {
            Resolution::Resolved => (
                Classification::Resolved,
                Role::ReexportAliasReference,
                Confidence::High,
            ),
            Resolution::Unrelated => (
                Classification::Unrelated,
                Role::UnrelatedValueReference,
                Confidence::High,
            ),
            Resolution::Ambiguous => (
                Classification::Ambiguous,
                Role::AmbiguousValueReference,
                Confidence::Low,
            ),
        }
    } else {
        (
            Classification::Ambiguous,
            Role::AmbiguousValueReference,
            Confidence::Low,
        )
    };
    occurrence_at(
        source,
        node.start_byte(),
        node.end_byte() - node.start_byte(),
        classification,
        role,
        confidence,
    )
}

fn classify_method_definition(
    function: Node<'_>,
    source: &str,
    target: &Target,
    classes: &[ClassInfo],
    functions: &[FunctionInfo],
) -> AnyResult<(Classification, Role, Confidence)> {
    if !functions
        .iter()
        .any(|candidate| candidate.start_byte == function.start_byte() && candidate.owner.is_some())
    {
        return Ok((
            Classification::Ambiguous,
            Role::AmbiguousValueReference,
            Confidence::Low,
        ));
    }
    let Some(class) = enclosing_class(function.start_byte(), classes) else {
        return Ok((
            Classification::Ambiguous,
            Role::AmbiguousValueReference,
            Confidence::Low,
        ));
    };
    if class.name == target.owner {
        return Ok((
            Classification::Resolved,
            Role::TargetDefinition,
            Confidence::High,
        ));
    }
    if is_related(&class.name, &target.owner, classes) {
        let lower = class.name.to_ascii_lowercase();
        let role = if is_ancestor(&class.name, &target.owner, classes)
            && (lower.contains("protocol") || class.bases.iter().any(|base| base == "Protocol"))
        {
            Role::ProtocolDefinition
        } else if lower.contains("mock") || lower.contains("fake") || lower.contains("stub") {
            Role::FixtureMockDefinition
        } else if is_ancestor(&target.owner, &class.name, classes) {
            Role::SubclassDefinition
        } else {
            Role::ResolvedValueReference
        };
        return Ok((Classification::Resolved, role, Confidence::High));
    }
    if class
        .fields
        .values()
        .any(|field_type| is_related(field_type, &target.owner, classes))
        && contains_wrapper_forwarder(function, source, target, classes, functions)?
    {
        return Ok((
            Classification::Resolved,
            Role::WrapperDefinition,
            Confidence::High,
        ));
    }
    Ok((
        Classification::Unrelated,
        Role::UnrelatedDefinition,
        Confidence::High,
    ))
}

fn contains_wrapper_forwarder(
    root: Node<'_>,
    source: &str,
    target: &Target,
    classes: &[ClassInfo],
    functions: &[FunctionInfo],
) -> AnyResult<bool> {
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if node.id() != root.id()
            && matches!(node.kind(), "function_definition" | "class_definition")
        {
            continue;
        }
        if node.kind() == "attribute"
            && node
                .child_by_field_name("attribute")
                .is_some_and(|attribute| node_text(attribute, source).ok() == Some(&target.symbol))
        {
            let resolution = resolve_attribute(node, source, classes, functions, target)?;
            if is_wrapper_forwarder(node, source, target, classes, resolution)? {
                return Ok(true);
            }
        }
        let mut cursor = node.walk();
        stack.extend(node.children(&mut cursor));
    }
    Ok(false)
}

fn classify_attribute(
    attribute: Node<'_>,
    source: &str,
    target: &Target,
    classes: &[ClassInfo],
    functions: &[FunctionInfo],
) -> AnyResult<(Classification, Role, Confidence)> {
    let resolution = resolve_attribute(attribute, source, classes, functions, target)?;
    let parent = attribute.parent();
    let is_call = parent.is_some_and(|parent| {
        parent.kind() == "call"
            && parent
                .child_by_field_name("function")
                .is_some_and(|function| function.id() == attribute.id())
    });
    let is_export_source = parent.is_some_and(|parent| {
        parent.kind() == "assignment"
            && is_module_scope(parent.start_byte(), classes, functions)
            && parent
                .child_by_field_name("right")
                .is_some_and(|right| right.id() == attribute.id())
    });
    let wrapper_forwarder = is_wrapper_forwarder(attribute, source, target, classes, resolution)?;
    let role = if wrapper_forwarder {
        Role::WrapperForwarder
    } else if is_export_source && resolution == Resolution::Resolved {
        Role::ExportSourceReference
    } else {
        match (resolution, is_call) {
            (Resolution::Resolved, true) => Role::ResolvedCall,
            (Resolution::Unrelated, true) => Role::UnrelatedCall,
            (Resolution::Ambiguous, true) => Role::AmbiguousCall,
            (Resolution::Resolved, false) => Role::ResolvedValueReference,
            (Resolution::Unrelated, false) => Role::UnrelatedValueReference,
            (Resolution::Ambiguous, false) => Role::AmbiguousValueReference,
        }
    };
    let (classification, confidence) = match resolution {
        Resolution::Resolved => (Classification::Resolved, Confidence::High),
        Resolution::Unrelated => (Classification::Unrelated, Confidence::High),
        Resolution::Ambiguous => (Classification::Ambiguous, Confidence::Low),
    };
    Ok((classification, role, confidence))
}

fn is_wrapper_forwarder(
    attribute: Node<'_>,
    source: &str,
    target: &Target,
    classes: &[ClassInfo],
    resolution: Resolution,
) -> AnyResult<bool> {
    if resolution != Resolution::Resolved {
        return Ok(false);
    }
    let Some(class) = enclosing_class(attribute.start_byte(), classes) else {
        return Ok(false);
    };
    let Some(object) = attribute
        .child_by_field_name("object")
        .filter(|object| object.kind() == "attribute")
    else {
        return Ok(false);
    };
    let Some(base) = object.child_by_field_name("object") else {
        return Ok(false);
    };
    if base.kind() != "identifier" || node_text(base, source)? != "self" {
        return Ok(false);
    }
    let Some(field) = object.child_by_field_name("attribute") else {
        return Ok(false);
    };
    let field = node_text(field, source)?;
    Ok(class
        .fields
        .get(field)
        .is_some_and(|field_type| is_related(field_type, &target.owner, classes)))
}

fn resolve_attribute(
    attribute: Node<'_>,
    source: &str,
    classes: &[ClassInfo],
    functions: &[FunctionInfo],
    target: &Target,
) -> AnyResult<Resolution> {
    let object = attribute
        .child_by_field_name("object")
        .ok_or_else(|| invalid_data("attribute omitted its object"))?;
    let inferred_type = match object.kind() {
        "identifier" => {
            let name = node_text(object, source)?;
            match local_value_binding_type(attribute, name, source, classes, functions)? {
                Some(local_binding) => local_binding,
                None => active_constructor_class(attribute, name, classes)
                    .map(|class| class.name.as_str()),
            }
        }
        "call" => {
            let function = object
                .child_by_field_name("function")
                .filter(|function| function.kind() == "identifier");
            let name = function
                .map(|function| node_text(function, source))
                .transpose()?;
            match name {
                Some(name)
                    if local_value_binding_type(attribute, name, source, classes, functions)?
                        .is_none() =>
                {
                    active_constructor_class(attribute, name, classes)
                        .map(|class| class.name.as_str())
                }
                _ => None,
            }
        }
        "attribute" => {
            let base = object.child_by_field_name("object");
            let field = object.child_by_field_name("attribute");
            if base.is_some_and(|base| {
                base.kind() == "identifier" && node_text(base, source).ok() == Some("self")
            }) {
                let field = field
                    .map(|field| node_text(field, source))
                    .transpose()?
                    .unwrap_or_default();
                enclosing_class(attribute.start_byte(), classes)
                    .and_then(|class| class.fields.get(field))
                    .map(String::as_str)
            } else {
                None
            }
        }
        _ => None,
    };
    Ok(match inferred_type {
        Some(inferred_type) if is_related(inferred_type, &target.owner, classes) => {
            Resolution::Resolved
        }
        Some(inferred_type) if classes.iter().any(|class| class.name == inferred_type) => {
            Resolution::Unrelated
        }
        _ => Resolution::Ambiguous,
    })
}

fn is_related(left: &str, right: &str, classes: &[ClassInfo]) -> bool {
    left == right || is_ancestor(left, right, classes) || is_ancestor(right, left, classes)
}

fn is_ancestor(candidate: &str, descendant: &str, classes: &[ClassInfo]) -> bool {
    let mut visited = BTreeSet::new();
    let mut stack = vec![descendant];
    while let Some(name) = stack.pop() {
        if !visited.insert(name) {
            continue;
        }
        let Some(class) = classes.iter().find(|class| class.name == name) else {
            continue;
        };
        for base in &class.bases {
            if base == candidate {
                return true;
            }
            stack.push(base);
        }
    }
    false
}

fn compare(expected: &[Occurrence], observed: &[Occurrence]) -> ComparisonReport {
    let expected_by_start = expected
        .iter()
        .map(|occurrence| (occurrence.start, occurrence))
        .collect::<BTreeMap<_, _>>();
    let observed_by_start = observed
        .iter()
        .map(|occurrence| (occurrence.start, occurrence))
        .collect::<BTreeMap<_, _>>();
    let false_positives = observed_by_start
        .keys()
        .filter(|start| !expected_by_start.contains_key(start))
        .count() as u64;
    let false_negatives = expected_by_start
        .keys()
        .filter(|start| !observed_by_start.contains_key(start))
        .count() as u64;
    let mut coordinate_mismatches = 0_u64;
    let mut classification_mismatches = 0_u64;
    let mut role_mismatches = 0_u64;
    let mut confidence_mismatches = 0_u64;
    for (start, expected) in &expected_by_start {
        let Some(observed) = observed_by_start.get(start) else {
            continue;
        };
        coordinate_mismatches += u64::from(expected.end != observed.end);
        classification_mismatches += u64::from(expected.classification != observed.classification);
        role_mismatches += u64::from(expected.role != observed.role);
        confidence_mismatches += u64::from(expected.confidence != observed.confidence);
    }
    let class_count = |classification| ClassCount {
        expected: expected
            .iter()
            .filter(|item| item.classification == classification)
            .count() as u64,
        observed: observed
            .iter()
            .filter(|item| item.classification == classification)
            .count() as u64,
    };
    let passed = false_positives == 0
        && false_negatives == 0
        && expected.len() == observed.len()
        && coordinate_mismatches == 0
        && classification_mismatches == 0
        && role_mismatches == 0
        && confidence_mismatches == 0;
    ComparisonReport {
        expected_occurrences: expected.len() as u64,
        observed_occurrences: observed.len() as u64,
        false_positives,
        false_negatives,
        coordinate_mismatches,
        classification_mismatches,
        role_mismatches,
        confidence_mismatches,
        resolved: class_count(Classification::Resolved),
        ambiguous: class_count(Classification::Ambiguous),
        unrelated: class_count(Classification::Unrelated),
        passed,
    }
}

fn comparison_exactness(comparison: &ComparisonReport) -> (bool, bool, bool, bool, bool) {
    let exact_membership = comparison.false_positives == 0
        && comparison.false_negatives == 0
        && comparison.expected_occurrences == comparison.observed_occurrences;
    (
        exact_membership,
        exact_membership && comparison.coordinate_mismatches == 0,
        exact_membership && comparison.classification_mismatches == 0,
        exact_membership && comparison.role_mismatches == 0,
        exact_membership && comparison.confidence_mismatches == 0,
    )
}

fn decision_for_result(oracle_passed: bool) -> DecisionReport {
    if oracle_passed {
        DecisionReport {
            oracle_result: "pass".into(),
            issue_outcome: "evaluation_complete_no_public_tool".into(),
            add_public_impact_analysis_tool: false,
            rationale: "The frozen Python oracle separates resolved, ambiguous, and unrelated occurrences exactly, but one synthetic single-language fixture cannot establish production binding semantics or justify another public tool.".into(),
        }
    } else {
        DecisionReport {
            oracle_result: "fail".into(),
            issue_outcome: "evaluation_failed".into(),
            add_public_impact_analysis_tool: false,
            rationale: "The gold comparison failed, so this run supports neither evaluation completion nor a public impact-analysis tool.".into(),
        }
    }
}

fn occurrence_at(
    source: &str,
    byte: usize,
    length: usize,
    classification: Classification,
    role: Role,
    confidence: Confidence,
) -> AnyResult<Occurrence> {
    let end = byte
        .checked_add(length)
        .ok_or_else(|| invalid_data("candidate range overflowed"))?;
    if end > source.len() || !source.is_char_boundary(byte) || !source.is_char_boundary(end) {
        return Err(invalid_data("candidate range is outside UTF-8 source boundaries").into());
    }
    Ok(Occurrence {
        start: position_at(source, byte)?,
        end: position_at(source, end)?,
        classification,
        role,
        confidence,
    })
}

fn position_at(source: &str, byte: usize) -> AnyResult<Position> {
    let prefix = source
        .get(..byte)
        .ok_or_else(|| invalid_data("position is outside source"))?;
    let line = prefix.bytes().filter(|byte| *byte == b'\n').count() + 1;
    let column = prefix
        .rsplit_once('\n')
        .map_or(prefix.len(), |(_, tail)| tail.len())
        + 1;
    Ok(Position {
        line: line as u64,
        column: column as u64,
    })
}

fn word_offsets(text: &str, needle: &str) -> Vec<usize> {
    text.match_indices(needle)
        .filter_map(|(offset, _)| {
            let before = text[..offset].chars().next_back();
            let after = text[offset + needle.len()..].chars().next();
            (!before.is_some_and(is_identifier_continue)
                && !after.is_some_and(is_identifier_continue))
            .then_some(offset)
        })
        .collect()
}

fn is_identifier_continue(character: char) -> bool {
    character == '_' || character.is_alphanumeric()
}

fn push_candidate(
    observations: &mut Vec<Occurrence>,
    seen_offsets: &mut BTreeSet<usize>,
    occurrence: Occurrence,
    byte: usize,
    max_candidates: usize,
) -> AnyResult<()> {
    if !seen_offsets.insert(byte) {
        return Err(invalid_data("candidate discovery emitted a duplicate coordinate").into());
    }
    if observations.len() >= max_candidates {
        return Err(invalid_data("candidate discovery exceeded its configured bound").into());
    }
    observations.push(occurrence);
    Ok(())
}

fn enclosing_class(byte: usize, classes: &[ClassInfo]) -> Option<&ClassInfo> {
    classes
        .iter()
        .filter(|class| class.start_byte <= byte && byte < class.end_byte)
        .min_by_key(|class| class.end_byte - class.start_byte)
}

fn direct_enclosing_class<'a>(
    function: Node<'_>,
    classes: &'a [ClassInfo],
) -> Option<&'a ClassInfo> {
    let mut ancestor = function.parent();
    while let Some(node) = ancestor {
        match node.kind() {
            "function_definition" => return None,
            "class_definition" => {
                return classes
                    .iter()
                    .find(|class| class.start_byte == node.start_byte());
            }
            _ => ancestor = node.parent(),
        }
    }
    None
}

fn active_module_class<'a>(
    name: &str,
    byte: usize,
    classes: &'a [ClassInfo],
) -> Option<&'a ClassInfo> {
    classes
        .iter()
        .filter(|class| {
            class.module_scope
                && class.name == name
                && class.end_byte <= byte
                && byte < class.module_binding_end_byte
        })
        .max_by_key(|class| class.end_byte)
}

fn active_constructor_class<'a>(
    reference: Node<'_>,
    name: &str,
    classes: &'a [ClassInfo],
) -> Option<&'a ClassInfo> {
    active_module_class(name, reference.start_byte(), classes).or_else(|| {
        let function = enclosing_function_node(reference)?;
        let class = direct_enclosing_class(function, classes)?;
        (class.module_scope && class.name == name && class.module_binding_end_byte == usize::MAX)
            .then_some(class)
    })
}

fn local_value_binding_type<'a>(
    attribute: Node<'_>,
    name: &str,
    source: &'a str,
    classes: &'a [ClassInfo],
    functions: &'a [FunctionInfo],
) -> AnyResult<Option<Option<&'a str>>> {
    let Some(function_node) = enclosing_function_node(attribute) else {
        return Ok(None);
    };
    let parameter_binding = parameter_binds_name(function_node, name, source)?;
    let body_binding = function_body_binds_name(function_node, name, source)?;
    if !parameter_binding && !body_binding {
        return Ok(None);
    }

    let mut inferred_type = functions
        .iter()
        .find(|function| function.start_byte == function_node.start_byte())
        .and_then(|function| function.bindings.get(name))
        .map(String::as_str);
    if let Some(block) = enclosing_block(attribute) {
        let mut cursor = block.walk();
        for statement in block.named_children(&mut cursor) {
            if statement.end_byte() > attribute.start_byte() {
                break;
            }
            if let Some(assignment_type) = simple_assignment_type(statement, name, source, classes)?
            {
                inferred_type = assignment_type;
            }
        }
    }
    Ok(Some(inferred_type))
}

fn enclosing_function_node(node: Node<'_>) -> Option<Node<'_>> {
    let mut ancestor = node.parent();
    while let Some(node) = ancestor {
        if node.kind() == "function_definition" {
            return Some(node);
        }
        ancestor = node.parent();
    }
    None
}

fn enclosing_block(node: Node<'_>) -> Option<Node<'_>> {
    let mut ancestor = node.parent();
    while let Some(node) = ancestor {
        if node.kind() == "block" {
            return Some(node);
        }
        ancestor = node.parent();
    }
    None
}

fn parameter_binds_name(function: Node<'_>, name: &str, source: &str) -> AnyResult<bool> {
    let parameters = function
        .child_by_field_name("parameters")
        .ok_or_else(|| invalid_data("function definition omitted parameters"))?;
    let mut stack = vec![parameters];
    while let Some(node) = stack.pop() {
        if node.kind() == "identifier"
            && node_text(node, source)? == name
            && parameter_identifier_is_binding(node, parameters)
        {
            return Ok(true);
        }
        let mut cursor = node.walk();
        stack.extend(node.named_children(&mut cursor));
    }
    Ok(false)
}

fn parameter_identifier_is_binding(identifier: Node<'_>, parameters: Node<'_>) -> bool {
    let mut ancestor = identifier.parent();
    while let Some(node) = ancestor {
        for field in ["type", "value"] {
            if node.child_by_field_name(field).is_some_and(|field_node| {
                field_node.start_byte() <= identifier.start_byte()
                    && identifier.end_byte() <= field_node.end_byte()
            }) {
                return false;
            }
        }
        if node.id() == parameters.id() {
            break;
        }
        ancestor = node.parent();
    }
    true
}

fn function_body_binds_name(function: Node<'_>, name: &str, source: &str) -> AnyResult<bool> {
    let Some(body) = function.child_by_field_name("body") else {
        return Ok(false);
    };
    let mut stack = vec![body];
    while let Some(node) = stack.pop() {
        if matches!(node.kind(), "function_definition" | "class_definition") {
            if node
                .child_by_field_name("name")
                .is_some_and(|identifier| node_text(identifier, source).ok() == Some(name))
            {
                return Ok(true);
            }
            continue;
        }
        if rebinding_node_binds_name(node, name, source) {
            return Ok(true);
        }
        let mut cursor = node.walk();
        stack.extend(node.named_children(&mut cursor));
    }
    Ok(false)
}

fn assignment_target_binds_name(target: Node<'_>, name: &str, source: &str) -> bool {
    match target.kind() {
        "identifier" => node_text(target, source).ok() == Some(name),
        "attribute" | "subscript" => false,
        _ => {
            let mut cursor = target.walk();
            target
                .named_children(&mut cursor)
                .any(|child| assignment_target_binds_name(child, name, source))
        }
    }
}

fn rebinding_node_binds_name(node: Node<'_>, name: &str, source: &str) -> bool {
    match node.kind() {
        "assignment" | "augmented_assignment" => node
            .child_by_field_name("left")
            .is_some_and(|left| assignment_target_binds_name(left, name, source)),
        "named_expression" => node
            .child_by_field_name("name")
            .is_some_and(|target| assignment_target_binds_name(target, name, source)),
        "delete_statement" => assignment_target_binds_name(node, name, source),
        _ => false,
    }
}

fn simple_assignment_type<'a>(
    statement: Node<'_>,
    name: &str,
    source: &'a str,
    classes: &'a [ClassInfo],
) -> AnyResult<Option<Option<&'a str>>> {
    let mut stack = vec![statement];
    while let Some(node) = stack.pop() {
        if node.kind() == "block"
            || matches!(node.kind(), "function_definition" | "class_definition")
        {
            continue;
        }
        if rebinding_node_binds_name(node, name, source) {
            if node.kind() != "assignment" {
                return Ok(Some(None));
            }
            let left = node
                .child_by_field_name("left")
                .ok_or_else(|| invalid_data("assignment omitted its target"))?;
            let inferred_type = if left.kind() != "identifier" {
                None
            } else {
                match node.child_by_field_name("right") {
                    Some(right) if right.kind() == "call" => right
                        .child_by_field_name("function")
                        .filter(|function| function.kind() == "identifier")
                        .map(|function| node_text(function, source))
                        .transpose()?
                        .and_then(|class_name| active_constructor_class(node, class_name, classes))
                        .map(|class| class.name.as_str()),
                    Some(right) if right.kind() == "identifier" => {
                        active_constructor_class(node, node_text(right, source)?, classes)
                            .map(|class| class.name.as_str())
                    }
                    _ => None,
                }
            };
            return Ok(Some(inferred_type));
        }
        let mut cursor = node.walk();
        stack.extend(node.named_children(&mut cursor));
    }
    Ok(None)
}

fn enclosing_function(byte: usize, functions: &[FunctionInfo]) -> Option<&FunctionInfo> {
    functions
        .iter()
        .filter(|function| function.start_byte <= byte && byte < function.end_byte)
        .min_by_key(|function| function.end_byte - function.start_byte)
}

fn is_lexical_module_scope(node: Node<'_>) -> bool {
    let mut ancestor = node.parent();
    while let Some(node) = ancestor {
        if matches!(node.kind(), "function_definition" | "class_definition") {
            return false;
        }
        ancestor = node.parent();
    }
    true
}

fn module_binding_name<'a>(node: Node<'_>, source: &'a str) -> AnyResult<Option<&'a str>> {
    let name = match node.kind() {
        "class_definition" | "function_definition" => node.child_by_field_name("name"),
        "assignment" => node
            .child_by_field_name("left")
            .filter(|left| left.kind() == "identifier"),
        _ => None,
    };
    name.map(|name| node_text(name, source)).transpose()
}

fn is_module_scope(byte: usize, classes: &[ClassInfo], functions: &[FunctionInfo]) -> bool {
    enclosing_class(byte, classes).is_none() && enclosing_function(byte, functions).is_none()
}

fn collect_identifier_text(
    root: Node<'_>,
    source: &str,
    output: &mut Vec<String>,
) -> AnyResult<()> {
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if node.kind() == "identifier" {
            output.push(node_text(node, source)?.to_owned());
        }
        let mut cursor = node.walk();
        stack.extend(node.children(&mut cursor));
    }
    Ok(())
}

fn node_text<'a>(node: Node<'_>, source: &'a str) -> AnyResult<&'a str> {
    source
        .get(node.byte_range())
        .ok_or_else(|| invalid_data("tree-sitter node is outside UTF-8 source boundaries").into())
}

fn validate_identifier(identifier: &str, max_bytes: usize) -> AnyResult<()> {
    let mut characters = identifier.chars();
    let valid = !identifier.is_empty()
        && identifier.len() <= max_bytes
        && characters
            .next()
            .is_some_and(|character| character == '_' || character.is_ascii_alphabetic())
        && characters.all(|character| character == '_' || character.is_ascii_alphanumeric());
    if !valid {
        return Err(invalid_data("manifest contains an invalid or oversized identifier").into());
    }
    Ok(())
}

fn validate_relative_path(path: &str) -> AnyResult<&Path> {
    let path = Path::new(path);
    if path.as_os_str().is_empty()
        || path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(invalid_data("source path must be normalized and repository-relative").into());
    }
    Ok(path)
}

fn locked_package_version(package_name: &str) -> AnyResult<String> {
    let mut version = None;
    for package in LOCKFILE.split("[[package]]").skip(1) {
        if lockfile_value(package, "name").as_deref() != Some(package_name) {
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

fn modeled_partial_allocation_estimate(limits: Limits) -> AnyResult<u64> {
    let candidates = u64::try_from(limits.max_candidates)
        .map_err(|_| invalid_data("candidate limit overflowed"))?;
    let bindings = u64::try_from(limits.max_type_bindings)
        .map_err(|_| invalid_data("binding limit overflowed"))?;
    let identifier_bytes = u64::try_from(limits.max_identifier_bytes)
        .map_err(|_| invalid_data("identifier limit overflowed"))?;
    let candidate_bytes = candidates
        .checked_mul(mem::size_of::<Occurrence>() as u64)
        .ok_or_else(|| invalid_data("candidate memory bound overflowed"))?;
    let binding_bytes = bindings
        .checked_mul(
            identifier_bytes
                .checked_mul(2)
                .and_then(|bytes| bytes.checked_add(128))
                .ok_or_else(|| invalid_data("binding memory bound overflowed"))?,
        )
        .ok_or_else(|| invalid_data("binding memory bound overflowed"))?;
    limits
        .max_source_bytes
        .checked_mul(2)
        .and_then(|bytes| bytes.checked_add(candidate_bytes))
        .and_then(|bytes| bytes.checked_add(binding_bytes))
        .and_then(|bytes| bytes.checked_add(limits.max_ast_nodes.checked_mul(64)?))
        .ok_or_else(|| invalid_data("working-set bound overflowed").into())
}

fn modeled_post_parse_ast_node_inspection_upper_bound(limits: Limits) -> AnyResult<u64> {
    let candidates = u64::try_from(limits.max_candidates)
        .map_err(|_| invalid_data("candidate limit overflowed"))?;
    let node_squared = limits
        .max_ast_nodes
        .checked_mul(limits.max_ast_nodes)
        .ok_or_else(|| invalid_data("AST node-inspection bound overflowed"))?;
    candidates
        .checked_mul(4)
        .and_then(|passes| passes.checked_add(8))
        .and_then(|passes| passes.checked_mul(node_squared))
        .ok_or_else(|| invalid_data("AST node-inspection bound overflowed").into())
}

fn modeled_post_parse_lookup_iteration_upper_bound(limits: Limits) -> AnyResult<u64> {
    // Classes, functions, base edges, field bindings, and subtree nodes are each
    // bounded by the AST-node cap. The deepest path is a candidate-local
    // subtree walk around ancestry lookups; eight node-cubed terms per
    // candidate plus eight collection/alias terms conservatively cover every
    // explicit linear loop in the evaluator.
    let candidates = u64::try_from(limits.max_candidates)
        .map_err(|_| invalid_data("candidate limit overflowed"))?;
    let node_cube = limits
        .max_ast_nodes
        .checked_mul(limits.max_ast_nodes)
        .and_then(|squared| squared.checked_mul(limits.max_ast_nodes))
        .ok_or_else(|| invalid_data("AST lookup-iteration bound overflowed"))?;
    candidates
        .checked_mul(8)
        .and_then(|factor| factor.checked_add(8))
        .and_then(|factor| factor.checked_mul(node_cube))
        .ok_or_else(|| invalid_data("AST lookup-iteration bound overflowed").into())
}

fn verify_fixture() -> AnyResult<()> {
    let root = repository_root();
    let manifest_path = root.join(DEFAULT_MANIFEST);
    let actual = evaluate_manifest(&manifest_path, &root)?;
    let expected_bytes = read_bounded_file(&root.join(CHECKED_REPORT), MAX_HARD_MANIFEST_BYTES)?;
    let expected: Report = serde_json::from_slice(&expected_bytes)?;
    if actual != expected {
        return Err(invalid_data(
            "checked resolved-reference report differs from current evaluator behavior",
        )
        .into());
    }
    if !actual.comparison.passed {
        return Err(invalid_data("checked resolved-reference oracle does not pass").into());
    }
    Ok(())
}

fn repository_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn write_report(path: &Path, report: &Report) -> AnyResult<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;
    let mut temporary = NamedTempFile::new_in(parent)?;
    serde_json::to_writer_pretty(&mut temporary, report)?;
    temporary.write_all(b"\n")?;
    temporary.flush()?;
    temporary
        .persist_noclobber(path)
        .map_err(|error| error.error)?;
    Ok(())
}

fn read_bounded_file(path: &Path, max_bytes: u64) -> AnyResult<Vec<u8>> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.file_type().is_file() || metadata.len() > max_bytes {
        return Err(invalid_data("input is not a bounded regular file").into());
    }
    let mut file = File::open(path)?;
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    Read::by_ref(&mut file)
        .take(max_bytes + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > max_bytes {
        return Err(invalid_data("input exceeded its byte bound while reading").into());
    }
    Ok(bytes)
}

fn normalize_lf(bytes: &[u8]) -> AnyResult<String> {
    let text =
        std::str::from_utf8(bytes).map_err(|_| invalid_data("oracle inputs must be UTF-8"))?;
    Ok(text.replace("\r\n", "\n"))
}

fn checked_add(left: u64, right: u64) -> AnyResult<u64> {
    left.checked_add(right)
        .ok_or_else(|| invalid_data("counter overflowed").into())
}

fn invalid_data(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_manifest() -> Manifest {
        serde_json::from_str(include_str!(
            "../benchmarks/resolved_reference_oracle_v1.json"
        ))
        .expect("fixture manifest")
    }

    fn parse_python(source: &str) -> Tree {
        let mut parser = TreeParser::new();
        parser
            .set_language(&tree_sitter_python::LANGUAGE.into())
            .expect("Python language");
        let tree = parser.parse(source.as_bytes(), None).expect("Python tree");
        assert!(!tree.root_node().has_error());
        tree
    }

    fn fixture() -> Report {
        let root = repository_root();
        evaluate_manifest(&root.join(DEFAULT_MANIFEST), &root).expect("fixture report")
    }

    #[test]
    fn fixture_separates_same_name_receivers_and_non_code() {
        let report = fixture();
        assert!(report.comparison.passed);
        assert_eq!(report.comparison.resolved.observed, 11);
        assert_eq!(report.comparison.ambiguous.observed, 1);
        assert_eq!(report.comparison.unrelated.observed, 5);
        assert_eq!(report.coverage.unclassified_occurrences, 0);
        assert!(
            report
                .observations
                .iter()
                .any(|item| item.role == Role::WrapperForwarder)
        );
        assert!(
            report
                .observations
                .iter()
                .any(|item| item.role == Role::ReexportAliasReference)
        );
    }

    #[test]
    fn comparison_fails_closed_on_classification_drift() {
        let report = fixture();
        let mut observed = report.observations.clone();
        observed[0].classification = Classification::Ambiguous;
        let comparison = compare(&report.observations, &observed);
        assert!(!comparison.passed);
        assert_eq!(comparison.classification_mismatches, 1);
    }

    #[test]
    fn unmatched_coordinates_invalidate_every_exact_comparison_flag() {
        let report = fixture();
        let mut expected = report.observations.clone();
        expected[0].start.column -= 1;
        let comparison = compare(&expected, &report.observations);
        let exactness = comparison_exactness(&comparison);

        assert_eq!(exactness, (false, false, false, false, false));
    }

    #[test]
    fn failed_oracle_does_not_claim_evaluation_completion() {
        let decision = decision_for_result(false);

        assert_eq!(decision.oracle_result, "fail");
        assert_eq!(decision.issue_outcome, "evaluation_failed");
        assert!(!decision.add_public_impact_analysis_tool);
        assert!(!decision.rationale.contains("separates"));
    }

    #[test]
    fn duplicate_gold_coordinates_and_relaxed_hard_caps_fail_closed() {
        let mut manifest = fixture_manifest();
        let duplicate = manifest.expected[0].clone();
        manifest.expected.insert(1, duplicate);
        assert!(validate_manifest(&manifest, 1_024).is_err());

        let expected = manifest.expected;
        let observed = expected[1..].to_vec();
        assert!(!compare(&expected, &observed).passed);

        let mut manifest = fixture_manifest();
        manifest.limits.max_source_bytes = MAX_HARD_SOURCE_BYTES + 1;
        assert!(validate_manifest(&manifest, 1_024).is_err());
    }

    #[test]
    fn retained_superclass_identifiers_obey_the_identifier_bound() {
        let manifest = fixture_manifest();
        let oversized = "A".repeat(manifest.limits.max_identifier_bytes + 1);
        let source = format!("class Target:\n    pass\n\nclass Child({oversized}):\n    pass\n");
        let tree = parse_python(&source);
        let mut visited = 0;
        let nodes = collect_nodes(
            tree.root_node(),
            manifest.limits.max_ast_nodes,
            &mut visited,
        )
        .expect("bounded nodes");

        assert!(collect_classes(&nodes, &source, manifest.limits).is_err());
    }

    #[test]
    fn wide_ast_fails_before_children_exceed_the_retained_node_cap() {
        let source = "first\nsecond\nthird\n";
        let tree = parse_python(source);
        let mut visited = 0;

        assert!(collect_nodes(tree.root_node(), 2, &mut visited).is_err());
        assert_eq!(visited, 1);
    }

    #[test]
    fn aliases_are_scoped_to_module_bindings() {
        let manifest = fixture_manifest();
        let source = r#"
class Runtime:
    def close(self):
        pass

def local():
    close = Runtime.close
    forwarded = close

close = Runtime.close
final_close = close
"#;
        let tree = parse_python(source);
        let analysis = analyze(&tree, source, &manifest.target, manifest.limits)
            .expect("bounded alias analysis");
        let role_count = |role| {
            analysis
                .observations
                .iter()
                .filter(|occurrence| occurrence.role == role)
                .count()
        };

        assert_eq!(role_count(Role::ExportAliasDefinition), 1);
        assert_eq!(role_count(Role::ExportSourceReference), 1);
        assert_eq!(role_count(Role::ReexportAliasReference), 1);
        assert_eq!(role_count(Role::AmbiguousValueReference), 2);
    }

    #[test]
    fn f_string_interpolations_remain_executable_candidates() {
        let manifest = fixture_manifest();
        let source = r#"
class Runtime:
    def close(self):
        pass

def migrate(runtime: Runtime):
    message = f"literal close: {runtime.close()}"
"#;
        let tree = parse_python(source);
        let analysis = analyze(&tree, source, &manifest.target, manifest.limits)
            .expect("bounded f-string analysis");

        assert!(analysis.observations.iter().any(|occurrence| {
            occurrence.classification == Classification::Resolved
                && occurrence.role == Role::ResolvedCall
        }));
        assert!(analysis.observations.iter().any(|occurrence| {
            occurrence.classification == Classification::Unrelated
                && occurrence.role == Role::NonCodeString
        }));
    }

    #[test]
    fn function_bindings_shadow_same_named_classes() {
        let manifest = fixture_manifest();
        let source = r#"
class Runtime:
    def close(self):
        pass

class OtherRuntime:
    def close(self):
        pass

def migrate(Runtime: OtherRuntime):
    Runtime.close()
    Runtime().close()
"#;
        let tree = parse_python(source);
        let analysis = analyze(&tree, source, &manifest.target, manifest.limits)
            .expect("bounded shadowing analysis");

        assert!(analysis.observations.iter().any(|occurrence| {
            occurrence.start.line == 11
                && occurrence.classification == Classification::Unrelated
                && occurrence.role == Role::UnrelatedCall
        }));
        assert!(analysis.observations.iter().any(|occurrence| {
            occurrence.start.line == 12
                && occurrence.classification == Classification::Ambiguous
                && occurrence.role == Role::AmbiguousCall
        }));
    }

    #[test]
    fn direct_constructor_inside_class_method_uses_resulting_module_binding() {
        let manifest = fixture_manifest();
        let source = r#"
class Runtime:
    def close(self):
        pass

    @staticmethod
    def make():
        Runtime().close()
"#;
        let tree = parse_python(source);
        let analysis = analyze(&tree, source, &manifest.target, manifest.limits)
            .expect("bounded in-class constructor analysis");

        assert!(analysis.observations.iter().any(|occurrence| {
            occurrence.start.line == 8
                && occurrence.classification == Classification::Resolved
                && occurrence.role == Role::ResolvedCall
        }));
    }

    #[test]
    fn local_rebindings_replace_parameter_receiver_types() {
        let manifest = fixture_manifest();
        let source = r#"
class Runtime:
    pass

class OtherRuntime:
    def close(self):
        pass

def migrate(runtime: Runtime):
    runtime = OtherRuntime()
    runtime.close()
"#;
        let tree = parse_python(source);
        let analysis = analyze(&tree, source, &manifest.target, manifest.limits)
            .expect("bounded local-rebinding analysis");

        assert!(analysis.observations.iter().any(|occurrence| {
            occurrence.start.line == 11
                && occurrence.classification == Classification::Unrelated
                && occurrence.role == Role::UnrelatedCall
        }));
        assert!(!analysis.observations.iter().any(|occurrence| {
            occurrence.start.line == 11 && occurrence.classification == Classification::Resolved
        }));
    }

    #[test]
    fn destructuring_rebindings_invalidate_parameter_receiver_types() {
        let manifest = fixture_manifest();
        let source = r#"
class Runtime:
    def close(self):
        pass

def migrate(runtime: Runtime):
    runtime, other = make_pair()
    runtime.close()
"#;
        let tree = parse_python(source);
        let analysis = analyze(&tree, source, &manifest.target, manifest.limits)
            .expect("bounded destructuring-rebinding analysis");

        assert!(analysis.observations.iter().any(|occurrence| {
            occurrence.start.line == 8
                && occurrence.classification == Classification::Ambiguous
                && occurrence.role == Role::AmbiguousCall
        }));
        assert!(!analysis.observations.iter().any(|occurrence| {
            occurrence.start.line == 8 && occurrence.classification == Classification::Resolved
        }));
    }

    #[test]
    fn non_plain_rebindings_invalidate_parameter_receiver_types() {
        let manifest = fixture_manifest();
        let source = r#"
class Runtime:
    def close(self):
        pass

def augmented(runtime: Runtime):
    runtime += other
    runtime.close()

def named(runtime: Runtime):
    (runtime := other)
    runtime.close()

def deleted(runtime: Runtime):
    del runtime
    runtime.close()
"#;
        let tree = parse_python(source);
        let analysis = analyze(&tree, source, &manifest.target, manifest.limits)
            .expect("bounded non-plain-rebinding analysis");

        for line in [8, 12, 16] {
            assert!(analysis.observations.iter().any(|occurrence| {
                occurrence.start.line == line
                    && occurrence.classification == Classification::Ambiguous
                    && occurrence.role == Role::AmbiguousCall
            }));
            assert!(!analysis.observations.iter().any(|occurrence| {
                occurrence.start.line == line
                    && occurrence.classification == Classification::Resolved
            }));
        }
    }

    #[test]
    fn known_unrelated_alias_definitions_remain_unrelated() {
        let manifest = fixture_manifest();
        let source = r#"
class Runtime:
    def close(self):
        pass

class OtherRuntime:
    def close(self):
        pass

close = OtherRuntime.close
"#;
        let tree = parse_python(source);
        let analysis = analyze(&tree, source, &manifest.target, manifest.limits)
            .expect("bounded unrelated alias analysis");

        assert!(analysis.observations.iter().any(|occurrence| {
            occurrence.start.line == 10
                && occurrence.start.column == 1
                && occurrence.classification == Classification::Unrelated
                && occurrence.role == Role::UnrelatedDefinition
        }));
        assert!(analysis.observations.iter().any(|occurrence| {
            occurrence.start.line == 10
                && occurrence.start.column == 22
                && occurrence.classification == Classification::Unrelated
                && occurrence.role == Role::UnrelatedValueReference
        }));
    }

    #[test]
    fn nested_functions_are_not_class_method_definitions() {
        let manifest = fixture_manifest();
        let source = r#"
class Runtime:
    def outer(self):
        def close():
            pass
        close()
"#;
        let tree = parse_python(source);
        let analysis = analyze(&tree, source, &manifest.target, manifest.limits)
            .expect("bounded nested-function analysis");

        assert!(!analysis.observations.iter().any(|occurrence| {
            occurrence.classification == Classification::Resolved
                && occurrence.role == Role::TargetDefinition
        }));
        assert!(analysis.observations.iter().any(|occurrence| {
            occurrence.start.line == 4
                && occurrence.classification == Classification::Ambiguous
                && occurrence.role == Role::AmbiguousValueReference
        }));
    }

    #[test]
    fn module_rebindings_shadow_class_constructors() {
        let manifest = fixture_manifest();
        let source = r#"
class Runtime:
    def close(self):
        pass

class OtherRuntime:
    def close(self):
        pass

def Runtime():
    return OtherRuntime()

Runtime().close()
"#;
        let tree = parse_python(source);
        let analysis = analyze(&tree, source, &manifest.target, manifest.limits)
            .expect("bounded module-shadowing analysis");

        assert!(analysis.observations.iter().any(|occurrence| {
            occurrence.start.line == 13
                && occurrence.classification == Classification::Ambiguous
                && occurrence.role == Role::AmbiguousCall
        }));
        assert!(!analysis.observations.iter().any(|occurrence| {
            occurrence.start.line == 13 && occurrence.classification == Classification::Resolved
        }));
    }

    #[test]
    fn alias_references_follow_source_order_and_rebindings() {
        let manifest = fixture_manifest();
        let source = r#"
class Runtime:
    def close(self):
        pass

class OtherRuntime:
    def close(self):
        pass

before = close
close = Runtime.close
resolved = close
close = OtherRuntime.close
after = close
"#;
        let tree = parse_python(source);
        let analysis = analyze(&tree, source, &manifest.target, manifest.limits)
            .expect("bounded ordered-alias analysis");

        let at_line = |line| {
            analysis
                .observations
                .iter()
                .find(|occurrence| occurrence.start.line == line)
                .expect("line occurrence")
        };
        assert_eq!(at_line(10).classification, Classification::Ambiguous);
        assert_eq!(at_line(12).classification, Classification::Resolved);
        assert_eq!(at_line(14).classification, Classification::Unrelated);
    }

    #[test]
    fn subclass_override_has_a_definition_role() {
        let manifest = fixture_manifest();
        let source = r#"
class Runtime:
    def close(self):
        pass

class RuntimeChild(Runtime):
    def close(self):
        pass
"#;
        let tree = parse_python(source);
        let analysis = analyze(&tree, source, &manifest.target, manifest.limits)
            .expect("bounded subclass analysis");

        assert!(analysis.observations.iter().any(|occurrence| {
            occurrence.classification == Classification::Resolved
                && occurrence.role == Role::SubclassDefinition
        }));
    }

    #[test]
    fn manifest_limit_counts_raw_bytes_before_crlf_normalization() {
        let raw = b"{}\r\n";
        let normalized = normalize_lf(raw).expect("normalized manifest");
        let mut manifest = fixture_manifest();
        manifest.limits.max_manifest_bytes = normalized.len() as u64;

        assert!(validate_manifest(&manifest, normalized.len()).is_ok());
        assert!(validate_manifest(&manifest, raw.len()).is_err());
    }

    #[test]
    fn wrapper_role_is_bound_to_the_called_field_type() {
        let manifest = fixture_manifest();
        let source = r#"
class Runtime:
    def close(self):
        pass

class OtherRuntime:
    def close(self):
        pass

class Wrapper:
    def __init__(self, runtime: Runtime, other: OtherRuntime):
        self.runtime = runtime
        self.other = other

    def close(self):
        self.runtime.close()
        self.other.close()
"#;
        let tree = parse_python(source);
        let analysis = analyze(&tree, source, &manifest.target, manifest.limits)
            .expect("bounded wrapper analysis");

        assert!(analysis.observations.iter().any(|occurrence| {
            occurrence.classification == Classification::Resolved
                && occurrence.role == Role::WrapperForwarder
        }));
        assert!(analysis.observations.iter().any(|occurrence| {
            occurrence.classification == Classification::Unrelated
                && occurrence.role == Role::UnrelatedCall
        }));
        assert!(!analysis.observations.iter().any(|occurrence| {
            occurrence.classification == Classification::Unrelated
                && occurrence.role == Role::WrapperForwarder
        }));
    }

    #[test]
    fn direct_constructor_call_does_not_make_an_unrelated_method_a_wrapper() {
        let manifest = fixture_manifest();
        let source = r#"
class Runtime:
    def close(self):
        pass

class OtherRuntime:
    def close(self):
        pass

class Wrapper:
    def __init__(self, runtime: Runtime, other: OtherRuntime):
        self.runtime = runtime
        self.other = other

    def close(self):
        self.other.close()
        Runtime().close()
"#;
        let tree = parse_python(source);
        let analysis = analyze(&tree, source, &manifest.target, manifest.limits)
            .expect("bounded non-forwarding wrapper analysis");

        assert!(analysis.observations.iter().any(|occurrence| {
            occurrence.start.line == 15
                && occurrence.classification == Classification::Unrelated
                && occurrence.role == Role::UnrelatedDefinition
        }));
        assert!(analysis.observations.iter().any(|occurrence| {
            occurrence.start.line == 17
                && occurrence.classification == Classification::Resolved
                && occurrence.role == Role::ResolvedCall
        }));
    }

    #[test]
    fn nested_forwarder_does_not_make_the_outer_method_a_wrapper() {
        let manifest = fixture_manifest();
        let source = r#"
class Runtime:
    def close(self):
        pass

class Wrapper:
    def __init__(self, runtime: Runtime):
        self.runtime = runtime

    def close(self):
        def nested():
            self.runtime.close()
"#;
        let tree = parse_python(source);
        let analysis = analyze(&tree, source, &manifest.target, manifest.limits)
            .expect("bounded nested-forwarder analysis");

        assert!(analysis.observations.iter().any(|occurrence| {
            occurrence.start.line == 10
                && occurrence.classification == Classification::Unrelated
                && occurrence.role == Role::UnrelatedDefinition
        }));
        assert!(analysis.observations.iter().any(|occurrence| {
            occurrence.start.line == 12
                && occurrence.classification == Classification::Resolved
                && occurrence.role == Role::WrapperForwarder
        }));
    }

    #[test]
    fn configured_bounds_cover_the_fixture_and_partial_estimate_is_nonzero() {
        let report = fixture();
        assert!(report.measurements.unique_ast_nodes_collected <= report.limits.max_ast_nodes);
        assert_eq!(
            report
                .measurements
                .modeled_post_parse_ast_node_inspection_upper_bound,
            (report.limits.max_candidates as u64 * 4 + 8) * report.limits.max_ast_nodes.pow(2)
        );
        assert_eq!(
            report
                .measurements
                .modeled_post_parse_lookup_iteration_upper_bound,
            (report.limits.max_candidates as u64 * 8 + 8) * report.limits.max_ast_nodes.pow(3)
        );
        assert!(report.measurements.candidate_volume <= report.limits.max_candidates as u64);
        assert!(report.measurements.type_bindings_loaded <= report.limits.max_type_bindings as u64);
        assert!(
            report.measurements.candidate_payload_tokens_cl100k
                <= report.limits.max_candidate_payload_tokens as u64
        );
        assert!(
            report.measurements.retained_candidate_bytes
                <= report
                    .measurements
                    .modeled_partial_allocation_estimate_bytes
        );
    }

    #[test]
    fn report_uses_locked_parser_versions() {
        let report = fixture();

        assert_eq!(
            report.engine.tree_sitter,
            locked_package_version("tree-sitter").expect("locked tree-sitter")
        );
        assert_eq!(
            report.engine.tree_sitter_python,
            locked_package_version("tree-sitter-python").expect("locked Python parser")
        );
    }

    #[test]
    fn checked_report_matches_current_behavior() {
        verify_fixture().expect("checked report");
    }
}
