use std::collections::{HashMap, HashSet};
use std::fmt;
use std::fs;
use std::path::Path;
use std::sync::Arc;
use std::time::UNIX_EPOCH;

use rayon::ThreadPool;
use rayon::prelude::*;
use tokio_util::sync::CancellationToken;

use crate::error::RetryableOperation;
use crate::model::IndexResponse;
use crate::parser::{self, ParseOutput};
use crate::repository::{
    DiscoveredFile, discover_files_cancellable, slash_path, validate_relative,
};
use crate::storage::{ChunkInput, ImportInput, IndexedFile, ReferenceInput, Storage, SymbolInput};
use crate::text::{PreparedText, TextKind, hash_bytes};
use crate::{Config, Error, Result};

/// Owns discovery/parse publication for one repository cache.
///
/// The Rayon worker pool is built once per indexer (per `Services` / cache) so
/// reconciles reuse it without a process-global worker count and without
/// paying `ThreadPoolBuilder` on every prepare.
#[derive(Clone)]
pub struct Indexer {
    config: Arc<Config>,
    storage: Storage,
    pool: Arc<ThreadPool>,
}

impl fmt::Debug for Indexer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Indexer")
            .field("config", &self.config)
            .field("storage", &self.storage)
            .field("pool_threads", &self.pool.current_num_threads())
            .finish()
    }
}

impl Indexer {
    /// Construct an indexer and its dedicated worker pool from config.
    pub fn new(config: Arc<Config>, storage: Storage) -> Result<Self> {
        let workers = config.max_index_workers.max(1);
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(workers)
            .thread_name(|index| format!("leantoken-index-{index}"))
            .build()
            .map_err(|error| Error::InvalidRequest(format!("index worker pool: {error}")))?;
        Ok(Self {
            config,
            storage,
            pool: Arc::new(pool),
        })
    }

    /// Reconcile filesystem state into one committed repository generation.
    pub fn reconcile(&self, rebuild: bool) -> Result<IndexResponse> {
        self.reconcile_cancellable(rebuild, &CancellationToken::new())
    }

    /// Reconcile the repository with cooperative cancellation and stale-plan retry.
    pub fn reconcile_cancellable(
        &self,
        rebuild: bool,
        cancellation: &CancellationToken,
    ) -> Result<IndexResponse> {
        for _ in 0..3 {
            match self.reconcile_once(rebuild, cancellation) {
                Err(Error::StaleReconciliation { .. }) => continue,
                result => return result,
            }
        }
        Err(Error::RetryableConflict(RetryableOperation::Reconciliation))
    }

    fn reconcile_once(
        &self,
        rebuild: bool,
        cancellation: &CancellationToken,
    ) -> Result<IndexResponse> {
        self.validate_config()?;
        check_cancelled(cancellation)?;
        let baseline = self.storage.meta()?;

        let mut discovered = discover_files_cancellable(
            &self.config.root,
            self.config.max_file_bytes,
            cancellation,
        )?;
        discovered.retain(|file| !self.config.is_database_artifact_path(&file.absolute_path));
        check_cancelled(cancellation)?;
        let existing = self.existing_files(cancellation)?;
        let config_hash = self.config_hash();
        let force = rebuild || baseline.config_hash != config_hash;

        let mut repository_paths = HashSet::with_capacity(discovered.len());
        for file in &discovered {
            check_cancelled(cancellation)?;
            repository_paths.insert(file.relative_path.clone());
        }
        let mut deletions = Vec::new();
        for path in existing.keys() {
            check_cancelled(cancellation)?;
            if !repository_paths.contains(path) {
                deletions.push(path.clone());
            }
        }

        let mut unchanged = 0usize;
        let mut candidates = Vec::new();
        for file in discovered {
            check_cancelled(cancellation)?;
            // mtime+size alone cannot prove content identity (bind mounts, copy
            // tools that preserve mtime, some network filesystems). Content-hash
            // before skipping so silent overwrites still reindex (#22).
            if !force
                && let Some(record) = existing.get(&file.relative_path)
                && record.size_bytes == file.size_bytes
                && record.modified_ns == file.modified_ns
                && content_unchanged(&file.absolute_path, &record.content_hash)
            {
                unchanged += 1;
                continue;
            }
            candidates.push(file);
        }

        let prepared = self.prepare_candidates(&candidates, cancellation)?;

        let mut replacements = Vec::new();
        let mut warnings = Vec::new();
        let mut skipped = 0usize;
        for result in prepared {
            check_cancelled(cancellation)?;
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
        resolve_imports(&mut replacements, &repository_paths, cancellation)?;

        check_cancelled(cancellation)?;
        deletions.sort_unstable();
        deletions.dedup();
        check_cancelled(cancellation)?;
        let files_seen = unchanged + candidates.len();
        let files_indexed = replacements.len();
        let files_removed = deletions.len();
        let generation = if rebuild {
            self.storage
                .full_reconcile_at(&baseline, &config_hash, replacements)?
        } else {
            self.storage
                .reconcile_files_at(&baseline, &config_hash, replacements, &deletions)?
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

    /// Reconcile watcher-reported paths without walking the full repository.
    ///
    /// Existing regular files and deletions are safe to apply directly. New
    /// paths, directories, symlinks, and ignore-rule changes fall back to a
    /// full reconciliation because they can affect files beyond the reported
    /// path.
    pub fn reconcile_paths(&self, paths: &[String]) -> Result<IndexResponse> {
        self.reconcile_paths_cancellable(paths, &CancellationToken::new())
    }

    /// Reconcile watcher paths with cooperative cancellation and stale-plan retry.
    pub fn reconcile_paths_cancellable(
        &self,
        paths: &[String],
        cancellation: &CancellationToken,
    ) -> Result<IndexResponse> {
        for _ in 0..3 {
            match self.reconcile_paths_once(paths, cancellation) {
                Err(Error::StaleReconciliation { .. }) => continue,
                result => return result,
            }
        }
        Err(Error::RetryableConflict(RetryableOperation::Reconciliation))
    }

    fn reconcile_paths_once(
        &self,
        paths: &[String],
        cancellation: &CancellationToken,
    ) -> Result<IndexResponse> {
        self.validate_config()?;
        check_cancelled(cancellation)?;
        let baseline = self.storage.meta()?;
        let config_hash = self.config_hash();
        if baseline.config_hash != config_hash {
            return self.reconcile_cancellable(true, cancellation);
        }

        let existing = self.existing_files(cancellation)?;
        let mut repository_paths = HashSet::with_capacity(existing.len());
        for path in existing.keys() {
            check_cancelled(cancellation)?;
            repository_paths.insert(path.clone());
        }
        let mut unique = HashSet::with_capacity(paths.len());
        for path in paths {
            check_cancelled(cancellation)?;
            unique.insert(path.clone());
        }
        let mut paths = unique.drain().collect::<Vec<_>>();
        check_cancelled(cancellation)?;
        paths.sort_unstable();
        check_cancelled(cancellation)?;

        let mut candidates = Vec::new();
        let mut deletions = Vec::new();
        let mut unchanged = 0usize;
        for requested in &paths {
            check_cancelled(cancellation)?;
            let relative = validate_relative(requested)?;
            let relative_path = slash_path(&relative);
            if is_ignore_control_path(&relative_path) {
                return self.reconcile_cancellable(true, cancellation);
            }
            let absolute_path = self.config.root.join(&relative);
            if self.config.is_database_artifact_path(&absolute_path) {
                continue;
            }

            let indexed = existing.get(&relative_path);
            let metadata = match fs::symlink_metadata(&absolute_path) {
                Ok(metadata) => metadata,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    if indexed.is_some() {
                        deletions.push(relative_path);
                    } else if existing
                        .keys()
                        .any(|path| path.starts_with(&format!("{relative_path}/")))
                    {
                        return self.reconcile_cancellable(false, cancellation);
                    }
                    continue;
                }
                Err(error) => return Err(error.into()),
            };

            if indexed.is_none() || !metadata.file_type().is_file() {
                return self.reconcile_cancellable(true, cancellation);
            }
            if metadata.len() > self.config.max_file_bytes {
                deletions.push(relative_path);
                continue;
            }

            let modified_ns = metadata
                .modified()
                .ok()
                .and_then(|value| value.duration_since(UNIX_EPOCH).ok())
                .map(|duration| duration.as_nanos());
            candidates.push(DiscoveredFile {
                absolute_path,
                relative_path,
                size_bytes: metadata.len(),
                modified_ns,
            });
        }

        let prepared = self.prepare_candidates(&candidates, cancellation)?;
        let mut replacements = Vec::new();
        let mut warnings = Vec::new();
        let mut skipped = 0usize;
        for result in prepared {
            check_cancelled(cancellation)?;
            match result {
                PreparedFile::Indexed(file, warning) => {
                    let same = existing.get(&file.path).is_some_and(|record| {
                        record.content_hash == file.content_hash
                            && record.size_bytes == file.size_bytes
                            && record.modified_ns == file.modified_ns
                    });
                    if same {
                        unchanged += 1;
                        continue;
                    }
                    replacements.push(file);
                    if let Some(warning) = warning {
                        push_warning(&mut warnings, warning);
                    }
                }
                PreparedFile::Binary(path) => {
                    skipped += 1;
                    deletions.push(path);
                }
                PreparedFile::Failed(path, error) => {
                    skipped += 1;
                    push_warning(&mut warnings, format!("{path}: {error}"));
                }
            }
        }
        check_cancelled(cancellation)?;
        deletions.sort_unstable();
        deletions.dedup();
        check_cancelled(cancellation)?;
        for deletion in &deletions {
            check_cancelled(cancellation)?;
            repository_paths.remove(deletion);
        }
        resolve_imports(&mut replacements, &repository_paths, cancellation)?;
        let files_indexed = replacements.len();
        let files_removed = deletions.len();
        let generation =
            self.storage
                .reconcile_files_at(&baseline, &config_hash, replacements, &deletions)?;

        Ok(IndexResponse {
            repository_generation: generation,
            files_seen: paths.len(),
            files_indexed,
            files_unchanged: unchanged,
            files_removed,
            files_skipped: skipped,
            warnings,
        })
    }

    fn validate_config(&self) -> Result<()> {
        if self.config.chunk_lines == 0 || self.config.chunk_bytes == 0 {
            return Err(Error::InvalidRequest(
                "chunk_lines and chunk_bytes must be positive".into(),
            ));
        }
        Ok(())
    }

    fn prepare_candidates(
        &self,
        candidates: &[DiscoveredFile],
        cancellation: &CancellationToken,
    ) -> Result<Vec<PreparedFile>> {
        // Reuse the pool owned by this indexer (issue #24): one pool per
        // Services/cache, sized from that instance's Config.max_index_workers.
        let chunk_lines = self.config.chunk_lines;
        let chunk_bytes = self.config.chunk_bytes;
        let tokenizer = self.config.tokenizer;
        self.pool.install(|| {
            candidates
                .par_iter()
                .map(|file| {
                    check_cancelled(cancellation)?;
                    let prepared = prepare_file(file, chunk_lines, chunk_bytes, tokenizer);
                    check_cancelled(cancellation)?;
                    Ok(prepared)
                })
                .collect()
        })
    }

    fn existing_files(
        &self,
        cancellation: &CancellationToken,
    ) -> Result<HashMap<String, crate::storage::FileRecord>> {
        let mut result = HashMap::new();
        let mut cursor = None;
        loop {
            check_cancelled(cancellation)?;
            let page = self.storage.list_files(1_000, cursor)?;
            if page.is_empty() {
                break;
            }
            cursor = page.last().map(|file| file.id);
            for file in page {
                check_cancelled(cancellation)?;
                result.insert(file.path.clone(), file);
            }
        }
        Ok(result)
    }

    fn config_hash(&self) -> String {
        let input = format!(
            "leantoken-index-v3\0{}\0{}\0{}\0{}\0{}",
            env!("CARGO_PKG_VERSION"),
            self.config.max_file_bytes,
            self.config.chunk_lines,
            self.config.chunk_bytes,
            self.config.tokenizer.name()
        );
        blake3::hash(input.as_bytes()).to_hex().to_string()
    }
}

fn check_cancelled(cancellation: &CancellationToken) -> Result<()> {
    if cancellation.is_cancelled() {
        Err(Error::Cancelled)
    } else {
        Ok(())
    }
}

enum PreparedFile {
    Indexed(IndexedFile, Option<String>),
    Binary(String),
    Failed(String, String),
}

fn prepare_file(
    file: &DiscoveredFile,
    chunk_lines: usize,
    chunk_bytes: usize,
    tokenizer: crate::tokens::Tokenizer,
) -> PreparedFile {
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
            token_count: tokenizer.count(&chunk.content),
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

/// Return whether the on-disk file still matches the indexed content hash.
///
/// Used when size and mtime look unchanged so full reconcile cannot skip a
/// content rewrite that preserved those metadata fields.
fn content_unchanged(path: &Path, expected_hash: &str) -> bool {
    match fs::read(path) {
        Ok(bytes) => hash_bytes(&bytes) == expected_hash,
        Err(_) => false,
    }
}

fn is_ignore_control_path(path: &str) -> bool {
    path == ".gitignore"
        || path == ".ignore"
        || path.ends_with("/.gitignore")
        || path.ends_with("/.ignore")
}

fn resolve_imports(
    files: &mut [IndexedFile],
    repository_paths: &HashSet<String>,
    cancellation: &CancellationToken,
) -> Result<()> {
    for file in files {
        check_cancelled(cancellation)?;
        for import in &mut file.imports {
            check_cancelled(cancellation)?;
            import.resolved_path = resolve_import(&file.path, &import.raw_target, repository_paths);
        }
    }
    Ok(())
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

    #[test]
    fn import_resolution_honors_cancellation() {
        let mut files = vec![IndexedFile {
            path: "src/app.ts".into(),
            language: Some("typescript".into()),
            structurally_complete: true,
            size_bytes: 1,
            modified_ns: None,
            content_hash: "hash".into(),
            chunks: Vec::new(),
            symbols: Vec::new(),
            references: Vec::new(),
            imports: vec![ImportInput {
                raw_target: "./lib".into(),
                resolved_path: None,
                line: 1,
            }],
        }];
        let paths = ["src/app.ts".to_string(), "src/lib.ts".to_string()]
            .into_iter()
            .collect();
        let cancellation = CancellationToken::new();
        cancellation.cancel();

        assert!(matches!(
            resolve_imports(&mut files, &paths, &cancellation),
            Err(Error::Cancelled)
        ));
    }

    #[test]
    fn pool_threads_follow_config_per_indexer() {
        let root = tempfile::tempdir().expect("root");
        let mut config_a =
            Config::discover(root.path(), Some(root.path().join("a.sqlite"))).expect("config a");
        config_a.max_index_workers = 1;
        let storage_a = Storage::open(&config_a.database_path).expect("storage a");
        let indexer_a = Indexer::new(Arc::new(config_a), storage_a).expect("indexer a");

        let mut config_b =
            Config::discover(root.path(), Some(root.path().join("b.sqlite"))).expect("config b");
        config_b.max_index_workers = 3;
        let storage_b = Storage::open(&config_b.database_path).expect("storage b");
        let indexer_b = Indexer::new(Arc::new(config_b), storage_b).expect("indexer b");

        assert_eq!(indexer_a.pool.current_num_threads(), 1);
        assert_eq!(indexer_b.pool.current_num_threads(), 3);
    }
}
