use std::env;
use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use blake3::Hasher;
use leantoken::parser;
use serde::Serialize;

const EXPECTED_CORPUS_REVISION: &str = "9feb6ad161877da86200693b039638dbf3411e66";
const GRAMMAR_REVISION: &str = "c10ad83a66c76855e006496db3bdb002afc49203";
const MAX_INCOMPLETE_PATH_SAMPLES: usize = 64;

#[derive(Serialize)]
struct ExtensionReport {
    extension: &'static str,
    files: usize,
    structurally_complete: usize,
    structurally_incomplete: usize,
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
    let mut incomplete_path_samples = Vec::new();
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
