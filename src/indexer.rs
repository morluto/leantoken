use std::collections::{HashMap, HashSet};
use std::fs;
use std::sync::Arc;

use rayon::prelude::*;

use crate::model::IndexResponse;
use crate::parser::{self, ParseOutput};
use crate::repository::{DiscoveredFile, discover_files};
use crate::storage::{ChunkInput, ImportInput, IndexedFile, ReferenceInput, Storage, SymbolInput};
use crate::text::{PreparedText, TextKind, hash_bytes};
use crate::{Config, Error, Result, tokens};

#[derive(Debug, Clone)]
pub struct Indexer {
    config: Arc<Config>,
    storage: Storage,
}

impl Indexer {
    #[must_use]
    pub fn new(config: Arc<Config>, storage: Storage) -> Self {
        Self { config, storage }
    }

    /// Reconcile filesystem state into one committed repository generation.
    pub fn reconcile(&self, rebuild: bool) -> Result<IndexResponse> {
        if self.config.chunk_lines == 0 || self.config.chunk_bytes == 0 {
            return Err(Error::InvalidRequest(
                "chunk_lines and chunk_bytes must be positive".into(),
            ));
        }

        let mut discovered = discover_files(&self.config.root, self.config.max_file_bytes)?;
        discovered
            .retain(|file| !is_database_artifact(&file.absolute_path, &self.config.database_path));
        let existing = self.existing_files()?;
        let config_hash = self.config_hash();
        let meta = self.storage.meta()?;
        let force = rebuild || meta.config_hash != config_hash;

        let repository_paths = discovered
            .iter()
            .map(|file| file.relative_path.clone())
            .collect::<HashSet<_>>();
        let mut deletions = existing
            .keys()
            .filter(|path| !repository_paths.contains(*path))
            .cloned()
            .collect::<Vec<_>>();

        let mut unchanged = 0usize;
        let candidates = discovered
            .into_iter()
            .filter(|file| {
                let same = existing.get(&file.relative_path).is_some_and(|record| {
                    record.size_bytes == file.size_bytes && record.modified_ns == file.modified_ns
                });
                if !force && same {
                    unchanged += 1;
                    false
                } else {
                    true
                }
            })
            .collect::<Vec<_>>();

        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(self.config.max_index_workers.max(1))
            .build()
            .map_err(|error| Error::InvalidRequest(format!("index worker pool: {error}")))?;
        let chunk_lines = self.config.chunk_lines;
        let chunk_bytes = self.config.chunk_bytes;
        let prepared = pool.install(|| {
            candidates
                .par_iter()
                .map(|file| prepare_file(file, chunk_lines, chunk_bytes))
                .collect::<Vec<_>>()
        });

        let mut replacements = Vec::new();
        let mut warnings = Vec::new();
        let mut skipped = 0usize;
        for result in prepared {
            match result {
                PreparedFile::Indexed(file, warning) => {
                    replacements.push(file);
                    if let Some(warning) = warning {
                        push_warning(&mut warnings, warning);
                    }
                }
                PreparedFile::Binary(path) => {
                    skipped += 1;
                    if existing.contains_key(&path) {
                        deletions.push(path);
                    }
                }
                PreparedFile::Failed(path, error) => {
                    skipped += 1;
                    push_warning(&mut warnings, format!("{path}: {error}"));
                }
            }
        }
        resolve_imports(&mut replacements, &repository_paths);

        deletions.sort_unstable();
        deletions.dedup();
        let files_seen = unchanged + candidates.len();
        let files_indexed = replacements.len();
        let files_removed = deletions.len();
        let generation = if rebuild {
            self.storage.full_reconcile(&config_hash, replacements)?
        } else {
            self.storage
                .reconcile_files(&config_hash, replacements, &deletions)?
        };

        Ok(IndexResponse {
            repository_generation: generation,
            files_seen,
            files_indexed,
            files_unchanged: unchanged,
            files_removed,
            files_skipped: skipped,
            warnings,
        })
    }

    fn existing_files(&self) -> Result<HashMap<String, crate::storage::FileRecord>> {
        let mut result = HashMap::new();
        let mut cursor = None;
        loop {
            let page = self.storage.list_files(1_000, cursor)?;
            if page.is_empty() {
                break;
            }
            cursor = page.last().map(|file| file.id);
            for file in page {
                result.insert(file.path.clone(), file);
            }
        }
        Ok(result)
    }

    fn config_hash(&self) -> String {
        let input = format!(
            "leantoken-index-v3\0{}\0{}\0{}\0{}",
            env!("CARGO_PKG_VERSION"),
            self.config.max_file_bytes,
            self.config.chunk_lines,
            self.config.chunk_bytes
        );
        blake3::hash(input.as_bytes()).to_hex().to_string()
    }
}

enum PreparedFile {
    Indexed(IndexedFile, Option<String>),
    Binary(String),
    Failed(String, String),
}

fn prepare_file(file: &DiscoveredFile, chunk_lines: usize, chunk_bytes: usize) -> PreparedFile {
    let bytes = match fs::read(&file.absolute_path) {
        Ok(bytes) => bytes,
        Err(error) => {
            return PreparedFile::Failed(file.relative_path.clone(), error.to_string());
        }
    };
    let prepared = PreparedText::from_bytes(&bytes, chunk_lines, chunk_bytes);
    if prepared.kind == TextKind::Binary {
        return PreparedFile::Binary(file.relative_path.clone());
    }

    let (parsed, warning) = match parser::parse(&file.relative_path, &prepared.content) {
        Ok(parsed) => (parsed, None),
        Err(error) => (
            ParseOutput {
                language: parser::language_by_path(&file.relative_path),
                structurally_complete: false,
                symbols: Vec::new(),
                references: Vec::new(),
                imports: Vec::new(),
            },
            Some(format!(
                "{}: structural parse failed; text remains searchable: {error}",
                file.relative_path
            )),
        ),
    };

    let chunks = prepared
        .chunks
        .into_iter()
        .map(|chunk| ChunkInput {
            token_count: tokens::count(&chunk.content),
            content: chunk.content,
            start_line: chunk.start_line,
            end_line: chunk.end_line,
            start_byte: chunk.start_byte,
            end_byte: chunk.end_byte,
        })
        .collect();
    let symbols = parsed
        .symbols
        .into_iter()
        .map(|symbol| SymbolInput {
            name: symbol.name,
            kind: symbol.kind,
            parent: symbol.parent,
            signature: symbol.signature,
            start_line: symbol.start_line,
            end_line: symbol.end_line,
            start_byte: symbol.start_byte,
            end_byte: symbol.end_byte,
        })
        .collect();
    let references = parsed
        .references
        .into_iter()
        .map(|reference| ReferenceInput {
            name: reference.name,
            kind: reference.kind,
            role: reference.role,
            enclosing_symbol: reference.enclosing_symbol,
            start_line: reference.start_line,
            end_line: reference.end_line,
            start_byte: reference.start_byte,
            end_byte: reference.end_byte,
        })
        .collect();
    let imports = parsed
        .imports
        .into_iter()
        .map(|import| ImportInput {
            raw_target: import.raw_target,
            resolved_path: import.resolved_path,
            line: import.line,
        })
        .collect();

    PreparedFile::Indexed(
        IndexedFile {
            path: file.relative_path.clone(),
            language: parsed.language,
            structurally_complete: parsed.structurally_complete,
            size_bytes: file.size_bytes,
            modified_ns: file.modified_ns,
            content_hash: hash_bytes(&bytes),
            chunks,
            symbols,
            references,
            imports,
        },
        warning,
    )
}

fn push_warning(warnings: &mut Vec<String>, warning: String) {
    const MAX_WARNINGS: usize = 100;
    if warnings.len() < MAX_WARNINGS {
        warnings.push(warning);
    }
}

fn is_database_artifact(path: &std::path::Path, database: &std::path::Path) -> bool {
    if path == database {
        return true;
    }
    let database = database.as_os_str().to_string_lossy();
    let path = path.as_os_str().to_string_lossy();
    path == format!("{database}-wal") || path == format!("{database}-shm")
}

fn resolve_imports(files: &mut [IndexedFile], repository_paths: &HashSet<String>) {
    for file in files {
        for import in &mut file.imports {
            import.resolved_path = resolve_import(&file.path, &import.raw_target, repository_paths);
        }
    }
}

fn resolve_import(
    source_path: &str,
    raw_target: &str,
    repository_paths: &HashSet<String>,
) -> Option<String> {
    let source = std::path::Path::new(source_path);
    let parent = source.parent().unwrap_or_else(|| std::path::Path::new(""));
    let mut bases = Vec::new();

    if raw_target.starts_with('.') {
        bases.push(parent.join(raw_target));
    } else if matches!(
        source.extension().and_then(|ext| ext.to_str()),
        Some("py" | "pyi")
    ) {
        bases.push(parent.join(raw_target.replace('.', "/")));
    } else if source.extension().and_then(|ext| ext.to_str()) == Some("rs") {
        let module = raw_target
            .split(['{', ':', ',', ' '])
            .find(|part| !part.is_empty() && !matches!(*part, "crate" | "self" | "super"))?;
        bases.push(parent.join(module));
        bases.push(std::path::Path::new("src").join(module));
    } else {
        return None;
    }

    let extensions: &[&str] = match source.extension().and_then(|ext| ext.to_str()) {
        Some("js" | "mjs" | "cjs") => &["", "js", "mjs", "cjs"],
        Some("ts" | "mts" | "cts" | "tsx") => &["", "ts", "tsx", "mts", "cts", "js"],
        Some("py" | "pyi") => &["", "py", "pyi"],
        Some("rs") => &["", "rs"],
        _ => &[""],
    };
    let mut matches = Vec::new();
    for base in bases {
        let Some(base) = normalize_relative(&base) else {
            continue;
        };
        for extension in extensions {
            let exact = if extension.is_empty() || base.extension().is_some() {
                base.clone()
            } else {
                base.with_extension(extension)
            };
            let index = if extension.is_empty() {
                base.join("index")
            } else {
                base.join("index").with_extension(extension)
            };
            for candidate in [exact, index] {
                let candidate = if candidate.extension().is_some() || extension.is_empty() {
                    candidate
                } else {
                    candidate.with_extension(extension)
                };
                let candidate = candidate.to_string_lossy().replace('\\', "/");
                if repository_paths.contains(&candidate) && !matches.contains(&candidate) {
                    matches.push(candidate);
                }
            }
        }
    }
    (matches.len() == 1).then(|| matches.remove(0))
}

fn normalize_relative(path: &std::path::Path) -> Option<std::path::PathBuf> {
    let mut normalized = std::path::PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::Normal(part) => normalized.push(part),
            std::path::Component::ParentDir => {
                if !normalized.pop() {
                    return None;
                }
            }
            std::path::Component::RootDir | std::path::Component::Prefix(_) => return None,
        }
    }
    Some(normalized)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conservative_import_resolution_requires_one_existing_file() {
        let paths = [
            "src/app.ts".to_string(),
            "src/lib.ts".to_string(),
            "src/pkg/index.ts".to_string(),
            "pkg/helpers.py".to_string(),
            "pkg/main.py".to_string(),
        ]
        .into_iter()
        .collect();
        assert_eq!(
            resolve_import("src/app.ts", "./lib", &paths).as_deref(),
            Some("src/lib.ts")
        );
        assert_eq!(
            resolve_import("pkg/main.py", "helpers", &paths).as_deref(),
            Some("pkg/helpers.py")
        );
        assert_eq!(
            resolve_import("src/app.ts", "./pkg", &paths).as_deref(),
            Some("src/pkg/index.ts")
        );
        assert!(resolve_import("src/app.ts", "external-package", &paths).is_none());
    }
}
