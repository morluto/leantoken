use super::*;

use std::fs;
use std::path::PathBuf;
use std::time::Instant;

use tempfile::TempDir;

const STAGE_FORMAT_VERSION: i64 = 1;

const STAGE_SCHEMA_SQL: &str = r#"
CREATE TABLE stage_meta (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

CREATE TABLE stage_files (
    id INTEGER PRIMARY KEY,
    ordinal INTEGER NOT NULL UNIQUE,
    path TEXT NOT NULL UNIQUE,
    language TEXT,
    structurally_complete INTEGER NOT NULL,
    size_bytes INTEGER NOT NULL,
    modified_ns INTEGER,
    content_hash TEXT NOT NULL,
    source_token_count INTEGER NOT NULL,
    source_tokenizer TEXT NOT NULL
);

CREATE TABLE stage_chunks (
    file_id INTEGER NOT NULL REFERENCES stage_files(id) ON DELETE CASCADE,
    ordinal INTEGER NOT NULL,
    content TEXT NOT NULL,
    start_line INTEGER NOT NULL,
    end_line INTEGER NOT NULL,
    start_byte INTEGER NOT NULL,
    end_byte INTEGER NOT NULL,
    token_count INTEGER NOT NULL,
    PRIMARY KEY(file_id, ordinal)
);

CREATE TABLE stage_symbols (
    file_id INTEGER NOT NULL REFERENCES stage_files(id) ON DELETE CASCADE,
    ordinal INTEGER NOT NULL,
    name TEXT NOT NULL,
    kind TEXT NOT NULL,
    parent TEXT,
    signature TEXT,
    start_line INTEGER NOT NULL,
    end_line INTEGER NOT NULL,
    start_byte INTEGER NOT NULL,
    end_byte INTEGER NOT NULL,
    PRIMARY KEY(file_id, ordinal)
);

CREATE TABLE stage_references (
    file_id INTEGER NOT NULL REFERENCES stage_files(id) ON DELETE CASCADE,
    ordinal INTEGER NOT NULL,
    name TEXT NOT NULL,
    kind TEXT NOT NULL,
    role TEXT NOT NULL,
    enclosing_symbol TEXT,
    start_line INTEGER NOT NULL,
    end_line INTEGER NOT NULL,
    start_byte INTEGER NOT NULL,
    end_byte INTEGER NOT NULL,
    PRIMARY KEY(file_id, ordinal)
);

CREATE TABLE stage_imports (
    id INTEGER PRIMARY KEY,
    file_id INTEGER NOT NULL REFERENCES stage_files(id) ON DELETE CASCADE,
    ordinal INTEGER NOT NULL,
    raw_target TEXT NOT NULL,
    resolved_path TEXT,
    line INTEGER NOT NULL,
    UNIQUE(file_id, ordinal)
);

CREATE TABLE stage_import_candidates (
    import_id INTEGER NOT NULL REFERENCES stage_imports(id) ON DELETE CASCADE,
    candidate_path TEXT NOT NULL,
    priority INTEGER NOT NULL,
    PRIMARY KEY(import_id, candidate_path)
);

CREATE TABLE stage_removals (
    path TEXT PRIMARY KEY,
    ordinal INTEGER NOT NULL UNIQUE
);

CREATE INDEX stage_files_ordinal_idx ON stage_files(ordinal);
CREATE INDEX stage_removals_ordinal_idx ON stage_removals(ordinal);
CREATE INDEX stage_import_candidates_import_idx
    ON stage_import_candidates(import_id, priority);
"#;

/// Diagnostics for the disposable, file-backed preparation database.
#[derive(Debug, Clone, Default)]
pub(crate) struct StagingDiagnostics {
    pub(crate) write_ms: f64,
    pub(crate) write_bytes: Option<u64>,
    pub(crate) database_bytes: u64,
}

/// Storage-owned normalized staging for prepared index records.
///
/// Only the current preparation batch is retained in Rust. The stage database
/// is immutable before publication and is read back one file at a time while
/// the production transaction is open. The production database remains the
/// sole database mutated by the generation publication transaction.
pub(crate) struct PreparedReconciliation {
    _directory: Option<TempDir>,
    path: Option<PathBuf>,
    connection: Option<Connection>,
    replacements: Vec<(IndexedFile, usize)>,
    removals: Vec<String>,
    tokenizer: String,
    baseline_generation: u64,
    config_hash: String,
    rebuild: bool,
    next_ordinal: i64,
    diagnostics: StagingDiagnostics,
    profile: bool,
}

pub(crate) struct FinalizedReconciliation {
    _directory: Option<TempDir>,
    path: Option<PathBuf>,
    tokenizer: String,
    baseline_generation: u64,
    config_hash: String,
    rebuild: bool,
    diagnostics: StagingDiagnostics,
}

impl PreparedReconciliation {
    pub(crate) fn new(
        _storage: &Storage,
        tokenizer: &str,
        baseline: &MetaRecord,
        config_hash: &str,
        rebuild: bool,
        profile: bool,
    ) -> Result<Self> {
        Ok(Self {
            _directory: None,
            path: None,
            connection: None,
            replacements: Vec::new(),
            removals: Vec::new(),
            tokenizer: tokenizer.to_string(),
            baseline_generation: baseline.repository_generation,
            config_hash: config_hash.to_string(),
            rebuild,
            next_ordinal: 0,
            diagnostics: StagingDiagnostics::default(),
            profile,
        })
    }

    fn initialize(&mut self) -> Result<()> {
        if self.connection.is_some() {
            return Ok(());
        }
        let write_before = self.profile.then(process_write_bytes).flatten();
        let started = Instant::now();
        let directory = tempfile::Builder::new()
            .prefix(".leantoken-stage-")
            .tempdir()?;
        let path = directory.path().join("stage.sqlite");
        let connection = Connection::open(&path)?;
        connection.busy_timeout(DEFAULT_BUSY_TIMEOUT)?;
        connection.pragma_update(None, "foreign_keys", "ON")?;
        connection.execute_batch(STAGE_SCHEMA_SQL)?;
        connection.execute_batch("PRAGMA journal_mode=DELETE; PRAGMA synchronous=NORMAL;")?;
        connection.execute_batch(
            "INSERT INTO stage_meta(key, value) VALUES
             ('format_version', '1'),
             ('baseline_generation', '0'),
             ('config_hash', ''),
             ('rebuild', '0'),
             ('tokenizer', '');",
        )?;
        connection.execute(
            "UPDATE stage_meta SET value = ?1 WHERE key = 'baseline_generation'",
            params![self.baseline_generation.to_string()],
        )?;
        connection.execute(
            "UPDATE stage_meta SET value = ?1 WHERE key = 'config_hash'",
            params![&self.config_hash],
        )?;
        connection.execute(
            "UPDATE stage_meta SET value = ?1 WHERE key = 'rebuild'",
            params![if self.rebuild { "1" } else { "0" }],
        )?;
        connection.execute(
            "UPDATE stage_meta SET value = ?1 WHERE key = 'tokenizer'",
            params![&self.tokenizer],
        )?;
        self.diagnostics.write_ms += started.elapsed().as_secs_f64() * 1_000.0;
        let write_after = self.profile.then(process_write_bytes).flatten();
        self.diagnostics.write_bytes = write_before
            .zip(write_after)
            .map(|(before, after)| after.saturating_sub(before));
        self._directory = Some(directory);
        self.path = Some(path);
        self.connection = Some(connection);
        Ok(())
    }

    pub(crate) fn stage_indexed(&mut self, file: IndexedFile, source_token_count: usize) {
        self.replacements.push((file, source_token_count));
    }

    pub(crate) fn stage_removal(&mut self, path: String) {
        self.removals.push(path);
    }

    pub(crate) fn pending_removals(&self) -> usize {
        self.removals.len()
    }

    /// Flush one bounded preparation batch to the stage database.
    pub(crate) fn flush(&mut self) -> Result<()> {
        if self.replacements.is_empty() && self.removals.is_empty() {
            return Ok(());
        }
        self.initialize()?;
        let write_before = self.profile.then(process_write_bytes).flatten();
        let started = Instant::now();
        let connection = self.connection.as_mut().ok_or_else(|| {
            Error::OperationFailure("reconciliation stage connection is closed".into())
        })?;
        let tx = connection.transaction()?;
        let mut removals = std::mem::take(&mut self.removals);
        removals.sort();
        let mut replacements = std::mem::take(&mut self.replacements);
        let next_ordinal = self.next_ordinal;
        let result = (|| {
            let mut ordinal = next_ordinal;
            for path in &removals {
                tx.execute(
                    "INSERT OR IGNORE INTO stage_removals(path, ordinal) VALUES (?1, ?2)",
                    params![path, ordinal],
                )?;
                ordinal = ordinal.saturating_add(1);
            }
            for (file, source_token_count) in &replacements {
                FinalizedReconciliation::insert_stage_file(
                    &tx,
                    file,
                    *source_token_count,
                    &self.tokenizer,
                    ordinal,
                )?;
                ordinal = ordinal.saturating_add(1);
            }
            tx.commit()?;
            Ok(ordinal)
        })();
        match result {
            Ok(ordinal) => self.next_ordinal = ordinal,
            Err(error) => {
                self.removals.append(&mut removals);
                self.replacements.append(&mut replacements);
                return Err(error);
            }
        }

        self.diagnostics.write_ms += started.elapsed().as_secs_f64() * 1_000.0;
        let write_after = self.profile.then(process_write_bytes).flatten();
        if let Some(bytes) = write_before
            .zip(write_after)
            .map(|(before, after)| after.saturating_sub(before))
        {
            self.diagnostics.write_bytes = Some(
                self.diagnostics
                    .write_bytes
                    .unwrap_or_default()
                    .saturating_add(bytes),
            );
        }
        Ok(())
    }

    /// Finish writes and close the mutable stage connection before publication.
    pub(crate) fn finish(mut self) -> Result<FinalizedReconciliation> {
        self.flush()?;
        self.connection.take();
        self.diagnostics.database_bytes = self
            .path
            .as_ref()
            .and_then(|path| fs::metadata(path).ok())
            .map_or(0, |metadata| metadata.len());
        Ok(FinalizedReconciliation {
            _directory: self._directory,
            path: self.path,
            tokenizer: self.tokenizer,
            baseline_generation: self.baseline_generation,
            config_hash: self.config_hash,
            rebuild: self.rebuild,
            diagnostics: self.diagnostics,
        })
    }
}

impl FinalizedReconciliation {
    pub(crate) fn diagnostics(&self) -> StagingDiagnostics {
        self.diagnostics.clone()
    }

    /// Apply normalized staged rows through the production reconciliation writer.
    ///
    /// The read connection is opened only after stage writes finish. It streams
    /// one file and its child rows at a time, so publication does not reconstruct
    /// the complete prepared generation in memory.
    pub(crate) fn apply(&self, writer: &mut ReconciliationWriter<'_, '_>) -> Result<()> {
        let Some(path) = self.path.as_ref() else {
            return Ok(());
        };
        let connection = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        connection.busy_timeout(DEFAULT_BUSY_TIMEOUT)?;
        let format_version = Self::stage_meta(&connection, "format_version")?
            .parse::<i64>()
            .map_err(|_| Error::OperationFailure("invalid reconciliation stage format".into()))?;
        if format_version != STAGE_FORMAT_VERSION {
            return Err(Error::OperationFailure(format!(
                "unsupported reconciliation stage format: {format_version}"
            )));
        }
        let baseline_generation = Self::stage_meta(&connection, "baseline_generation")?
            .parse::<u64>()
            .map_err(|_| Error::OperationFailure("invalid reconciliation stage baseline".into()))?;
        let config_hash = Self::stage_meta(&connection, "config_hash")?;
        let rebuild = Self::stage_meta(&connection, "rebuild")?;
        let tokenizer = Self::stage_meta(&connection, "tokenizer")?;
        if baseline_generation != self.baseline_generation
            || config_hash != self.config_hash
            || rebuild != if self.rebuild { "1" } else { "0" }
            || tokenizer != self.tokenizer
        {
            return Err(Error::OperationFailure(
                "reconciliation stage metadata does not match its owner".into(),
            ));
        }

        {
            let mut statement =
                connection.prepare("SELECT path FROM stage_removals ORDER BY ordinal")?;
            let mut rows = statement.query([])?;
            while let Some(row) = rows.next()? {
                let path = row.get::<_, String>(0)?;
                writer.delete(&path)?;
            }
        }

        let mut statement = connection.prepare(
            "SELECT id, path, language, structurally_complete, size_bytes,
                    modified_ns, content_hash, source_token_count, source_tokenizer
             FROM stage_files ORDER BY ordinal",
        )?;
        let mut rows = statement.query([])?;
        while let Some(row) = rows.next()? {
            let row = StageFileRow {
                id: row.get(0)?,
                path: row.get(1)?,
                language: row.get(2)?,
                structurally_complete: row.get(3)?,
                size_bytes: row.get(4)?,
                modified_ns: row.get(5)?,
                content_hash: row.get(6)?,
                source_token_count: row.get(7)?,
                source_tokenizer: row.get(8)?,
            };
            let file = Self::read_stage_file(&connection, &row)?;
            writer.replace_with_source_tokens(
                file,
                &row.source_tokenizer,
                i64_to_usize(row.source_token_count)?,
            )?;
        }
        Ok(())
    }

    fn stage_meta(connection: &Connection, key: &str) -> Result<String> {
        Ok(connection.query_row(
            "SELECT value FROM stage_meta WHERE key = ?1",
            params![key],
            |row| row.get(0),
        )?)
    }

    fn insert_stage_file(
        tx: &Transaction,
        file: &IndexedFile,
        source_token_count: usize,
        tokenizer: &str,
        ordinal: i64,
    ) -> Result<()> {
        tx.execute(
            "INSERT INTO stage_files(
                ordinal, path, language, structurally_complete, size_bytes,
                modified_ns, content_hash, source_token_count, source_tokenizer
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                ordinal,
                &file.path,
                file.language.as_deref(),
                file.structurally_complete,
                u64_to_i64(file.size_bytes)?,
                file.modified_ns.map(u128_to_i64).transpose()?,
                &file.content_hash,
                usize_to_i64(source_token_count)?,
                tokenizer,
            ],
        )?;
        let file_id = tx.last_insert_rowid();

        let mut chunks = tx.prepare_cached(
            "INSERT INTO stage_chunks(
                file_id, ordinal, content, start_line, end_line, start_byte, end_byte, token_count
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        )?;
        for (ordinal, chunk) in file.chunks.iter().enumerate() {
            chunks.execute(params![
                file_id,
                usize_to_i64(ordinal)?,
                &chunk.content,
                usize_to_i64(chunk.start_line)?,
                usize_to_i64(chunk.end_line)?,
                usize_to_i64(chunk.start_byte)?,
                usize_to_i64(chunk.end_byte)?,
                usize_to_i64(chunk.token_count)?,
            ])?;
        }
        drop(chunks);

        let mut symbols = tx.prepare_cached(
            "INSERT INTO stage_symbols(
                file_id, ordinal, name, kind, parent, signature,
                start_line, end_line, start_byte, end_byte
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        )?;
        for (ordinal, symbol) in file.symbols.iter().enumerate() {
            symbols.execute(params![
                file_id,
                usize_to_i64(ordinal)?,
                &symbol.name,
                &symbol.kind,
                symbol.parent.as_deref(),
                symbol.signature.as_deref(),
                usize_to_i64(symbol.start_line)?,
                usize_to_i64(symbol.end_line)?,
                usize_to_i64(symbol.start_byte)?,
                usize_to_i64(symbol.end_byte)?,
            ])?;
        }
        drop(symbols);

        let mut references = tx.prepare_cached(
            "INSERT INTO stage_references(
                file_id, ordinal, name, kind, role, enclosing_symbol,
                start_line, end_line, start_byte, end_byte
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        )?;
        for (ordinal, reference) in file.references.iter().enumerate() {
            references.execute(params![
                file_id,
                usize_to_i64(ordinal)?,
                &reference.name,
                &reference.kind,
                role_to_str(reference.role),
                reference.enclosing_symbol.as_deref(),
                usize_to_i64(reference.start_line)?,
                usize_to_i64(reference.end_line)?,
                usize_to_i64(reference.start_byte)?,
                usize_to_i64(reference.end_byte)?,
            ])?;
        }
        drop(references);

        let mut imports = tx.prepare_cached(
            "INSERT INTO stage_imports(
                file_id, ordinal, raw_target, resolved_path, line
             ) VALUES (?1, ?2, ?3, ?4, ?5)",
        )?;
        let mut candidates = tx.prepare_cached(
            "INSERT INTO stage_import_candidates(import_id, candidate_path, priority)
             VALUES (?1, ?2, ?3)",
        )?;
        for (ordinal, import) in file.imports.iter().enumerate() {
            imports.execute(params![
                file_id,
                usize_to_i64(ordinal)?,
                &import.raw_target,
                import.resolved_path.as_deref(),
                usize_to_i64(import.line)?,
            ])?;
            let import_id = tx.last_insert_rowid();
            for (priority, candidate_path) in import.candidate_paths.iter().enumerate() {
                candidates.execute(params![import_id, candidate_path, usize_to_i64(priority)?,])?;
            }
        }
        Ok(())
    }

    fn read_stage_file(connection: &Connection, row: &StageFileRow) -> Result<IndexedFile> {
        let chunks = {
            let mut statement = connection.prepare(
                "SELECT content, start_line, end_line, start_byte, end_byte, token_count
                 FROM stage_chunks WHERE file_id = ?1 ORDER BY ordinal",
            )?;
            statement
                .query_map(params![row.id], |row| {
                    Ok(ChunkInput {
                        content: row.get(0)?,
                        start_line: i64_to_usize(row.get(1)?)?,
                        end_line: i64_to_usize(row.get(2)?)?,
                        start_byte: i64_to_usize(row.get(3)?)?,
                        end_byte: i64_to_usize(row.get(4)?)?,
                        token_count: i64_to_usize(row.get(5)?)?,
                    })
                })?
                .collect::<std::result::Result<Vec<_>, _>>()?
        };
        let symbols = {
            let mut statement = connection.prepare(
                "SELECT name, kind, parent, signature, start_line, end_line, start_byte, end_byte
                 FROM stage_symbols WHERE file_id = ?1 ORDER BY ordinal",
            )?;
            statement
                .query_map(params![row.id], |row| {
                    Ok(SymbolInput {
                        name: row.get(0)?,
                        kind: row.get(1)?,
                        parent: row.get(2)?,
                        signature: row.get(3)?,
                        start_line: i64_to_usize(row.get(4)?)?,
                        end_line: i64_to_usize(row.get(5)?)?,
                        start_byte: i64_to_usize(row.get(6)?)?,
                        end_byte: i64_to_usize(row.get(7)?)?,
                    })
                })?
                .collect::<std::result::Result<Vec<_>, _>>()?
        };
        let references = {
            let mut statement = connection.prepare(
                "SELECT name, kind, role, enclosing_symbol, start_line, end_line, start_byte, end_byte
                 FROM stage_references WHERE file_id = ?1 ORDER BY ordinal",
            )?;
            statement
                .query_map(params![row.id], |row| {
                    Ok(ReferenceInput {
                        name: row.get(0)?,
                        kind: row.get(1)?,
                        role: role_from_str(&row.get::<_, String>(2)?),
                        enclosing_symbol: row.get(3)?,
                        start_line: i64_to_usize(row.get(4)?)?,
                        end_line: i64_to_usize(row.get(5)?)?,
                        start_byte: i64_to_usize(row.get(6)?)?,
                        end_byte: i64_to_usize(row.get(7)?)?,
                    })
                })?
                .collect::<std::result::Result<Vec<_>, _>>()?
        };
        let imports = {
            let import_rows = {
                let mut statement = connection.prepare(
                    "SELECT id, raw_target, resolved_path, line
                     FROM stage_imports WHERE file_id = ?1 ORDER BY ordinal",
                )?;
                statement
                    .query_map(params![row.id], |row| {
                        Ok((
                            row.get::<_, i64>(0)?,
                            ImportInput {
                                raw_target: row.get(1)?,
                                resolved_path: row.get(2)?,
                                candidate_paths: Vec::new(),
                                line: i64_to_usize(row.get(3)?)?,
                            },
                        ))
                    })?
                    .collect::<std::result::Result<Vec<_>, _>>()?
            };
            import_rows
                .into_iter()
                .map(|(id, mut import)| {
                    let mut candidates = connection.prepare(
                        "SELECT candidate_path FROM stage_import_candidates
                         WHERE import_id = ?1 ORDER BY priority",
                    )?;
                    import.candidate_paths = candidates
                        .query_map(params![id], |row| row.get::<_, String>(0))?
                        .collect::<std::result::Result<Vec<_>, _>>()?;
                    Ok(import)
                })
                .collect::<Result<Vec<_>>>()?
        };

        Ok(IndexedFile {
            path: row.path.clone(),
            language: row.language.clone(),
            structurally_complete: row.structurally_complete,
            size_bytes: i64_to_u64(row.size_bytes)?,
            modified_ns: row.modified_ns.map(i64_to_u128).transpose()?,
            content_hash: row.content_hash.clone(),
            chunks,
            symbols,
            references,
            imports,
        })
    }
}

struct StageFileRow {
    id: i64,
    path: String,
    language: Option<String>,
    structurally_complete: bool,
    size_bytes: i64,
    modified_ns: Option<i64>,
    content_hash: String,
    source_token_count: i64,
    source_tokenizer: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::ReferenceRole;

    #[test]
    fn failed_flush_retains_the_pending_batch_for_retry() {
        let root = tempfile::tempdir().expect("repository root");
        let storage = Storage::open(root.path().join("index.sqlite")).expect("storage");
        let baseline = storage.meta().expect("baseline");
        let mut stage = PreparedReconciliation::new(
            &storage,
            "fixture-tokenizer",
            &baseline,
            "config",
            false,
            false,
        )
        .expect("stage");
        stage.stage_removal("old.rs".into());
        stage.initialize().expect("initialize stage");
        stage
            .connection
            .as_ref()
            .expect("initialized stage connection")
            .execute_batch("DROP TABLE stage_removals")
            .expect("break stage fixture");

        assert!(stage.flush().is_err());

        assert_eq!(stage.removals, vec!["old.rs"]);
        assert!(stage.replacements.is_empty());
        assert_eq!(stage.next_ordinal, 0);
    }

    #[test]
    fn normalized_stage_roundtrip_preserves_derived_rows_and_cleans_up() {
        let root = tempfile::tempdir().expect("repository root");
        let database = root.path().join("index.sqlite");
        let storage = Storage::open(&database).expect("storage");
        storage
            .full_reconcile(
                "config",
                vec![crate::storage::tests::sample_file("old.rs", "old\n")],
            )
            .expect("initial generation");

        let baseline = storage.meta().expect("baseline");
        let mut file = crate::storage::tests::sample_file("src/lib.rs", "fn answer() {}\n");
        file.symbols.push(SymbolInput {
            name: "answer".into(),
            kind: "function".into(),
            parent: None,
            signature: Some("fn answer()".into()),
            start_line: 1,
            end_line: 1,
            start_byte: 0,
            end_byte: 14,
        });
        file.references.push(ReferenceInput {
            name: "answer".into(),
            kind: "call".into(),
            role: ReferenceRole::Reference,
            enclosing_symbol: Some("main".into()),
            start_line: 2,
            end_line: 2,
            start_byte: 0,
            end_byte: 6,
        });
        file.imports.push(ImportInput {
            raw_target: "crate::util".into(),
            resolved_path: Some("src/util.rs".into()),
            candidate_paths: vec!["src/util.rs".into(), "src/util/mod.rs".into()],
            line: 1,
        });

        let mut stage = PreparedReconciliation::new(
            &storage,
            "fixture-tokenizer",
            &baseline,
            "config",
            false,
            true,
        )
        .expect("stage");
        stage.stage_removal("old.rs".into());
        stage.stage_indexed(file, 7);
        stage.flush().expect("stage batch");
        let stage_path = stage.path.clone().expect("initialized stage path");
        assert!(
            stage_path.exists(),
            "stage database should exist before publish"
        );
        let stage = stage.finish().expect("finish stage");
        assert!(stage.diagnostics().database_bytes > 0);

        let (generation, ()) = storage
            .publish_reconciliation_at(&baseline, "config", false, |writer| stage.apply(writer))
            .expect("publish staged rows");
        assert_eq!(generation, 2);

        let connection = storage
            .writer
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        for (table, expected) in [
            ("files", 1),
            ("chunks", 1),
            ("symbols", 1),
            ("symbol_refs", 1),
            ("imports", 1),
            ("import_candidates", 2),
        ] {
            let count: i64 = connection
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                    row.get(0)
                })
                .expect("count published rows");
            assert_eq!(count, expected, "published {table} rows");
        }
        assert!(
            connection
                .query_row(
                    "SELECT path FROM files WHERE path = 'src/lib.rs'",
                    [],
                    |row| row.get::<_, String>(0),
                )
                .is_ok()
        );
        drop(connection);
        drop(stage);
        assert!(
            !stage_path.exists(),
            "stage database and its directory should be removed after publication"
        );
    }
}
