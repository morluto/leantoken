use std::collections::BTreeMap;
use std::env;
use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use blake3::Hasher;
use leantoken::parser;
use serde::Serialize;
use tree_sitter::{Node, Parser as SyntaxParser};

const EXPECTED_CORPUS_REVISION: &str = "9feb6ad161877da86200693b039638dbf3411e66";
const GRAMMAR_REVISION: &str = "c10ad83a66c76855e006496db3bdb002afc49203";
const MAX_INCOMPLETE_PATH_SAMPLES: usize = 64;
const MAX_DIAGNOSTIC_SHAPES: usize = 64;
const MAX_VISITED_NODES_PER_FILE: usize = 2_000_000;

#[derive(Serialize)]
struct ExtensionReport {
    extension: &'static str,
    files: usize,
    structurally_complete: usize,
    structurally_incomplete: usize,
}

#[derive(Serialize)]
struct DiagnosticShape {
    shape: String,
    count: usize,
}

#[derive(Default, Serialize)]
struct ParseDiagnostics {
    error_nodes: usize,
    missing_nodes: usize,
    visited_nodes: usize,
    files_with_truncated_traversal: usize,
    unclassified_shapes: usize,
    shapes: Vec<DiagnosticShape>,
}

#[derive(Serialize)]
struct DiagnosticReport {
    schema_version: u32,
    corpus_revision: String,
    corpus_digest_blake3: String,
    candidate_revision: String,
    grammar_package: &'static str,
    grammar_version: &'static str,
    grammar_revision: &'static str,
    source_files: usize,
    source_bytes: u64,
    structurally_complete: usize,
    structurally_incomplete: usize,
    symbols: usize,
    references: usize,
    imports: usize,
    extensions: Vec<ExtensionReport>,
    parse_diagnostics: ParseDiagnostics,
    incomplete_path_samples: Vec<String>,
    incomplete_path_samples_truncated: bool,
}

fn git(root: &Path, args: &[&str]) -> Result<Vec<u8>, Box<dyn Error>> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()?;
    if !output.status.success() {
        return Err(format!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        )
        .into());
    }
    Ok(output.stdout)
}

fn revision(root: &Path) -> Result<String, Box<dyn Error>> {
    Ok(String::from_utf8(git(root, &["rev-parse", "HEAD"])?)?
        .trim()
        .to_owned())
}

fn tracked_kotlin_paths(root: &Path) -> Result<Vec<PathBuf>, Box<dyn Error>> {
    let output = git(root, &["ls-files", "-z", "--", "*.kt", "*.kts"])?;
    let mut paths = output
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
        .map(|path| String::from_utf8(path.to_vec()).map(PathBuf::from))
        .collect::<Result<Vec<_>, _>>()?;
    paths.sort();
    Ok(paths)
}

fn record_diagnostic_shape(
    shapes: &mut BTreeMap<String, usize>,
    unclassified_shapes: &mut usize,
    shape: String,
) {
    if let Some(count) = shapes.get_mut(&shape) {
        *count += 1;
    } else if shapes.len() < MAX_DIAGNOSTIC_SHAPES {
        shapes.insert(shape, 1);
    } else {
        *unclassified_shapes += 1;
    }
}

fn diagnostic_shape(node: Node<'_>, category: &str) -> String {
    let parent = node.parent().map_or("<root>", |value| value.kind());
    format!("{category}:{} under {parent}", node.kind())
}

fn collect_parse_diagnostics(
    root: Node<'_>,
    diagnostics: &mut ParseDiagnostics,
    shapes: &mut BTreeMap<String, usize>,
) {
    let mut cursor = root.walk();
    let mut visited = 0usize;
    loop {
        let node = cursor.node();
        visited += 1;
        diagnostics.visited_nodes += 1;
        if node.is_error() {
            diagnostics.error_nodes += 1;
            record_diagnostic_shape(
                shapes,
                &mut diagnostics.unclassified_shapes,
                diagnostic_shape(node, "ERROR"),
            );
        }
        if node.is_missing() {
            diagnostics.missing_nodes += 1;
            record_diagnostic_shape(
                shapes,
                &mut diagnostics.unclassified_shapes,
                diagnostic_shape(node, "MISSING"),
            );
        }
        if visited == MAX_VISITED_NODES_PER_FILE {
            diagnostics.files_with_truncated_traversal += 1;
            return;
        }
        if cursor.goto_first_child() {
            continue;
        }
        while !cursor.goto_next_sibling() {
            if !cursor.goto_parent() {
                return;
            }
        }
    }
}

fn main() -> Result<(), Box<dyn Error>> {
    let mut args = env::args_os().skip(1);
    let root = args
        .next()
        .map(PathBuf::from)
        .ok_or("usage: kotlin_parse_diagnostic <corpus-root> [output.json]")?
        .canonicalize()?;
    let output_path = args.next().map(PathBuf::from);
    if args.next().is_some() {
        return Err("usage: kotlin_parse_diagnostic <corpus-root> [output.json]".into());
    }

    let corpus_revision = revision(&root)?;
    if corpus_revision != EXPECTED_CORPUS_REVISION {
        return Err(format!(
            "expected corpus revision {EXPECTED_CORPUS_REVISION}, got {corpus_revision}"
        )
        .into());
    }
    if !git(
        &root,
        &["status", "--porcelain=v1", "--untracked-files=all"],
    )?
    .is_empty()
    {
        return Err("corpus worktree is dirty".into());
    }

    let candidate_root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let candidate_revision = revision(candidate_root)?;
    if !git(
        candidate_root,
        &["status", "--porcelain=v1", "--untracked-files=all"],
    )?
    .is_empty()
    {
        return Err("candidate worktree is dirty".into());
    }

    let paths = tracked_kotlin_paths(&root)?;
    let mut digest = Hasher::new();
    let mut source_bytes = 0u64;
    let mut structurally_complete = 0usize;
    let mut symbols = 0usize;
    let mut references = 0usize;
    let mut imports = 0usize;
    let mut diagnostics = ParseDiagnostics::default();
    let mut diagnostic_shapes = BTreeMap::new();
    let mut incomplete_path_samples = Vec::new();
    let mut syntax_parser = SyntaxParser::new();
    syntax_parser.set_language(&tree_sitter_kotlin::LANGUAGE.into())?;
    let mut kt = ExtensionReport {
        extension: "kt",
        files: 0,
        structurally_complete: 0,
        structurally_incomplete: 0,
    };
    let mut kts = ExtensionReport {
        extension: "kts",
        files: 0,
        structurally_complete: 0,
        structurally_incomplete: 0,
    };

    for path in &paths {
        let source = fs::read_to_string(root.join(path))?;
        let extension = path.extension().and_then(|value| value.to_str());
        let extension_report = match extension {
            Some("kt") => &mut kt,
            Some("kts") => &mut kts,
            _ => return Err(format!("unexpected Kotlin path {}", path.display()).into()),
        };
        extension_report.files += 1;
        source_bytes = source_bytes.saturating_add(source.len() as u64);
        digest.update(path.to_string_lossy().as_bytes());
        digest.update(&[0]);
        digest.update(&(source.len() as u64).to_le_bytes());
        digest.update(source.as_bytes());

        let parsed = parser::parse(path, &source)?;
        if parsed.language.as_deref() != Some("kotlin") {
            return Err(format!("{} was not detected as Kotlin", path.display()).into());
        }
        symbols = symbols.saturating_add(parsed.symbols.len());
        references = references.saturating_add(parsed.references.len());
        imports = imports.saturating_add(parsed.imports.len());
        let tree = syntax_parser
            .parse(&source, None)
            .ok_or_else(|| format!("tree-sitter returned no tree for {}", path.display()))?;
        collect_parse_diagnostics(tree.root_node(), &mut diagnostics, &mut diagnostic_shapes);
        if tree.root_node().has_error() != !parsed.structurally_complete {
            return Err(format!("parser completeness disagreement for {}", path.display()).into());
        }
        if parsed.structurally_complete {
            structurally_complete += 1;
            extension_report.structurally_complete += 1;
        } else {
            extension_report.structurally_incomplete += 1;
            if incomplete_path_samples.len() < MAX_INCOMPLETE_PATH_SAMPLES {
                incomplete_path_samples.push(path.to_string_lossy().into_owned());
            }
        }
    }

    let structurally_incomplete = paths.len().saturating_sub(structurally_complete);
    diagnostics.shapes = diagnostic_shapes
        .into_iter()
        .map(|(shape, count)| DiagnosticShape { shape, count })
        .collect();
    let report = DiagnosticReport {
        schema_version: 1,
        corpus_revision,
        corpus_digest_blake3: digest.finalize().to_hex().to_string(),
        candidate_revision,
        grammar_package: "tree-sitter-kotlin",
        grammar_version: "0.4.0",
        grammar_revision: GRAMMAR_REVISION,
        source_files: paths.len(),
        source_bytes,
        structurally_complete,
        structurally_incomplete,
        symbols,
        references,
        imports,
        extensions: vec![kt, kts],
        parse_diagnostics: diagnostics,
        incomplete_path_samples,
        incomplete_path_samples_truncated: structurally_incomplete > MAX_INCOMPLETE_PATH_SAMPLES,
    };
    let json = serde_json::to_string_pretty(&report)? + "\n";
    if let Some(path) = output_path {
        fs::write(path, json)?;
    } else {
        print!("{json}");
    }
    Ok(())
}
