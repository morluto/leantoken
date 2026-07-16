use std::{
    fmt, fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};

use rusqlite::{
    Connection, OpenFlags, OptionalExtension, Row, Transaction, TransactionBehavior, params,
};
use rusqlite_migration::{M, Migrations};

use crate::model::ReferenceRole;
use crate::{Error, Result};

pub const DEFAULT_MAX_RESULTS: usize = 100;
pub const HARD_MAX_RESULTS: usize = 10_000;

const DEFAULT_BUSY_TIMEOUT: Duration = Duration::from_millis(5_000);

const SCHEMA_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS meta (
    id INTEGER PRIMARY KEY,
    schema_version INTEGER NOT NULL,
    index_version INTEGER NOT NULL DEFAULT 0,
    config_hash TEXT NOT NULL DEFAULT '',
    repository_generation INTEGER NOT NULL DEFAULT 0
);

INSERT OR IGNORE INTO meta(id, schema_version, index_version, config_hash, repository_generation)
VALUES (1, 1, 0, '', 0);

CREATE TABLE IF NOT EXISTS files (
    id INTEGER PRIMARY KEY,
    path TEXT NOT NULL UNIQUE,
    language TEXT,
    structurally_complete INTEGER NOT NULL DEFAULT 0,
    size_bytes INTEGER NOT NULL DEFAULT 0,
    modified_ns INTEGER,
    content_hash TEXT NOT NULL,
    generation INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE IF NOT EXISTS chunks (
    id INTEGER PRIMARY KEY,
    file_id INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
    content TEXT NOT NULL,
    start_line INTEGER NOT NULL,
    end_line INTEGER NOT NULL,
    start_byte INTEGER NOT NULL,
    end_byte INTEGER NOT NULL,
    token_count INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE IF NOT EXISTS symbols (
    id INTEGER PRIMARY KEY,
    file_id INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    kind TEXT NOT NULL,
    parent TEXT,
    signature TEXT,
    start_line INTEGER NOT NULL,
    end_line INTEGER NOT NULL,
    start_byte INTEGER NOT NULL,
    end_byte INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS symbol_refs (
    id INTEGER PRIMARY KEY,
    file_id INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    kind TEXT NOT NULL,
    role TEXT NOT NULL,
    enclosing_symbol TEXT,
    start_line INTEGER NOT NULL,
    end_line INTEGER NOT NULL,
    start_byte INTEGER NOT NULL,
    end_byte INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS imports (
    id INTEGER PRIMARY KEY,
    file_id INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
    raw_target TEXT NOT NULL,
    resolved_path TEXT,
    line INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS files_generation_idx ON files(generation);
CREATE INDEX IF NOT EXISTS symbols_name_idx ON symbols(name COLLATE NOCASE);
CREATE INDEX IF NOT EXISTS symbol_refs_name_idx ON symbol_refs(name COLLATE NOCASE);
CREATE INDEX IF NOT EXISTS imports_resolved_path_idx ON imports(resolved_path);

CREATE VIRTUAL TABLE IF NOT EXISTS chunks_fts_word USING fts5(
    content,
    content='chunks',
    content_rowid='rowid',
    tokenize='unicode61'
);

CREATE VIRTUAL TABLE IF NOT EXISTS chunks_fts_trigram USING fts5(
    content,
    content='chunks',
    content_rowid='rowid',
    tokenize='trigram'
);

CREATE TRIGGER IF NOT EXISTS chunks_ai_word
AFTER INSERT ON chunks
BEGIN
    INSERT INTO chunks_fts_word(rowid, content) VALUES (new.rowid, new.content);
END;

CREATE TRIGGER IF NOT EXISTS chunks_ad_word
AFTER DELETE ON chunks
BEGIN
    INSERT INTO chunks_fts_word(chunks_fts_word, rowid, content)
    VALUES ('delete', old.rowid, old.content);
END;

CREATE TRIGGER IF NOT EXISTS chunks_au_word
AFTER UPDATE ON chunks
BEGIN
    INSERT INTO chunks_fts_word(chunks_fts_word, rowid, content)
    VALUES ('delete', old.rowid, old.content);
    INSERT INTO chunks_fts_word(rowid, content) VALUES (new.rowid, new.content);
END;

CREATE TRIGGER IF NOT EXISTS chunks_ai_trigram
AFTER INSERT ON chunks
BEGIN
    INSERT INTO chunks_fts_trigram(rowid, content) VALUES (new.rowid, new.content);
END;

CREATE TRIGGER IF NOT EXISTS chunks_ad_trigram
AFTER DELETE ON chunks
BEGIN
    INSERT INTO chunks_fts_trigram(chunks_fts_trigram, rowid, content)
    VALUES ('delete', old.rowid, old.content);
END;

CREATE TRIGGER IF NOT EXISTS chunks_au_trigram
AFTER UPDATE ON chunks
BEGIN
    INSERT INTO chunks_fts_trigram(chunks_fts_trigram, rowid, content)
    VALUES ('delete', old.rowid, old.content);
    INSERT INTO chunks_fts_trigram(rowid, content) VALUES (new.rowid, new.content);
END;
"#;

const LOOKUP_INDEXES_SQL: &str = r#"
CREATE INDEX IF NOT EXISTS chunks_file_line_idx
ON chunks(file_id, start_line, end_line);

CREATE INDEX IF NOT EXISTS symbols_file_start_idx
ON symbols(file_id, start_byte);

CREATE INDEX IF NOT EXISTS symbol_refs_file_start_idx
ON symbol_refs(file_id, start_byte);

CREATE INDEX IF NOT EXISTS imports_file_line_idx
ON imports(file_id, line);
"#;

const MIGRATIONS_SLICE: &[M<'_>] = &[
    M::up(SCHEMA_SQL).foreign_key_check(),
    M::up(LOOKUP_INDEXES_SQL),
];
const MIGRATIONS: Migrations<'_> = Migrations::from_slice(MIGRATIONS_SLICE);

#[derive(Debug, Clone)]
pub struct MetaRecord {
    pub schema_version: i64,
    pub index_version: i64,
    pub config_hash: String,
    pub repository_generation: u64,
}

#[derive(Debug, Clone)]
pub struct FileRecord {
    pub id: i64,
    pub path: String,
    pub language: Option<String>,
    pub structurally_complete: bool,
    pub size_bytes: u64,
    pub modified_ns: Option<u128>,
    pub content_hash: String,
    pub generation: u64,
}

#[derive(Debug, Clone)]
pub struct ChunkInput {
    pub content: String,
    pub start_line: usize,
    pub end_line: usize,
    pub start_byte: usize,
    pub end_byte: usize,
    pub token_count: usize,
}

#[derive(Debug, Clone)]
pub struct ChunkRecord {
    pub id: i64,
    pub file_id: i64,
    pub content: String,
    pub start_line: usize,
    pub end_line: usize,
    pub start_byte: usize,
    pub end_byte: usize,
    pub token_count: usize,
}

#[derive(Debug, Clone)]
pub struct SymbolInput {
    pub name: String,
    pub kind: String,
    pub parent: Option<String>,
    pub signature: Option<String>,
    pub start_line: usize,
    pub end_line: usize,
    pub start_byte: usize,
    pub end_byte: usize,
}

#[derive(Debug, Clone)]
pub struct SymbolRecord {
    pub id: i64,
    pub file_id: i64,
    pub name: String,
    pub kind: String,
    pub parent: Option<String>,
    pub signature: Option<String>,
    pub start_line: usize,
    pub end_line: usize,
    pub start_byte: usize,
    pub end_byte: usize,
}

#[derive(Debug, Clone)]
pub struct ReferenceInput {
    pub name: String,
    pub kind: String,
    pub role: ReferenceRole,
    pub enclosing_symbol: Option<String>,
    pub start_line: usize,
    pub end_line: usize,
    pub start_byte: usize,
    pub end_byte: usize,
}

#[derive(Debug, Clone)]
pub struct ReferenceRecord {
    pub id: i64,
    pub file_id: i64,
    pub name: String,
    pub kind: String,
    pub role: ReferenceRole,
    pub enclosing_symbol: Option<String>,
    pub start_line: usize,
    pub end_line: usize,
    pub start_byte: usize,
    pub end_byte: usize,
}

#[derive(Debug, Clone)]
pub struct ImportInput {
    pub raw_target: String,
    pub resolved_path: Option<String>,
    pub line: usize,
}

#[derive(Debug, Clone)]
pub struct ImportRecord {
    pub id: i64,
    pub file_id: i64,
    pub raw_target: String,
    pub resolved_path: Option<String>,
    pub line: usize,
}

#[derive(Debug, Clone)]
pub struct IndexedFile {
    pub path: String,
    pub language: Option<String>,
    pub structurally_complete: bool,
    pub size_bytes: u64,
    pub modified_ns: Option<u128>,
    pub content_hash: String,
    pub chunks: Vec<ChunkInput>,
    pub symbols: Vec<SymbolInput>,
    pub references: Vec<ReferenceInput>,
    pub imports: Vec<ImportInput>,
}

#[derive(Debug, Clone)]
pub struct ChunkHit {
    pub chunk_id: i64,
    pub file_id: i64,
    pub path: String,
    pub content: String,
    pub start_line: usize,
    pub end_line: usize,
    pub start_byte: usize,
    pub end_byte: usize,
    pub token_count: usize,
    pub score: f64,
}

#[derive(Debug, Clone)]
pub struct SymbolHit {
    pub path: String,
    pub content_hash: String,
    pub symbol: SymbolRecord,
}

#[derive(Debug, Clone)]
pub struct ReferenceHit {
    pub path: String,
    pub content_hash: String,
    pub reference: ReferenceRecord,
}

#[derive(Debug, Clone)]
pub struct StorageCounts {
    pub files: usize,
    pub chunks: usize,
    pub symbols: usize,
    pub languages: Vec<(String, usize)>,
}

pub struct Storage {
    writer: Arc<Mutex<Connection>>,
    path: PathBuf,
}

impl Clone for Storage {
    fn clone(&self) -> Self {
        Self {
            writer: Arc::clone(&self.writer),
            path: self.path.clone(),
        }
    }
}

impl fmt::Debug for Storage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Storage")
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}

/// One read-only connection held under a DEFERRED transaction so all queries
/// on this session observe a single SQLite WAL snapshot.
pub struct ReadSession {
    conn: Connection,
}

impl Drop for ReadSession {
    fn drop(&mut self) {
        let _ = self.conn.execute_batch("ROLLBACK");
    }
}

#[derive(Clone, Copy, Debug)]
enum FtsTable {
    Word,
    Trigram,
}

impl FtsTable {
    fn as_str(self) -> &'static str {
        match self {
            FtsTable::Word => "chunks_fts_word",
            FtsTable::Trigram => "chunks_fts_trigram",
        }
    }
}

impl Storage {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        Self::open_with_startup_timeout(path, DEFAULT_BUSY_TIMEOUT)
    }

    pub(crate) fn open_with_startup_timeout(
        path: impl AsRef<Path>,
        startup_timeout: Duration,
    ) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut conn = Connection::open(&path)?;
        Self::configure(&mut conn, startup_timeout)?;
        MIGRATIONS.to_latest(&mut conn)?;
        Self::validate_fts5(&mut conn)?;
        conn.busy_timeout(DEFAULT_BUSY_TIMEOUT)?;

        Ok(Self {
            writer: Arc::new(Mutex::new(conn)),
            path,
        })
    }

    fn configure(conn: &mut Connection, startup_timeout: Duration) -> Result<()> {
        conn.busy_timeout(startup_timeout)?;
        conn.pragma_update_and_check(None, "journal_mode", "WAL", |_| Ok(()))?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        Ok(())
    }

    fn validate_fts5(conn: &mut Connection) -> Result<()> {
        let probe = "leantoken_fts5_probe";
        conn.execute(
            &format!("CREATE VIRTUAL TABLE temp.{probe} USING fts5(text, tokenize='trigram')"),
            [],
        )
        .map_err(|source| Error::RuntimeCapabilityUnavailable {
            capability: "SQLite FTS5 with the trigram tokenizer",
            source: Some(source),
        })?;
        conn.execute(
            &format!("INSERT INTO temp.{probe}(text) VALUES (?1)"),
            params!["abc"],
        )?;
        let mut stmt = conn.prepare(&format!(
            "SELECT 1 FROM temp.{probe} WHERE {probe} MATCH ?1"
        ))?;
        let matched = stmt.exists(params!["\"abc\""])?;
        drop(stmt);
        conn.execute(&format!("DROP TABLE temp.{probe}"), [])?;
        if matched {
            Ok(())
        } else {
            Err(Error::RuntimeCapabilityUnavailable {
                capability: "SQLite FTS5 with a working trigram tokenizer",
                source: None,
            })
        }
    }

    pub fn meta(&self) -> Result<MetaRecord> {
        self.begin_read()?.meta()
    }

    pub fn repository_generation(&self) -> Result<u64> {
        self.begin_read()?.repository_generation()
    }

    pub fn full_reconcile(&self, config_hash: &str, files: Vec<IndexedFile>) -> Result<u64> {
        let baseline = self.meta()?;
        self.full_reconcile_at(&baseline, config_hash, files)
    }

    /// Replace the complete index only if the generation used to build the
    /// reconciliation plan is still current.
    pub fn full_reconcile_at(
        &self,
        baseline: &MetaRecord,
        config_hash: &str,
        files: Vec<IndexedFile>,
    ) -> Result<u64> {
        let mut conn = self
            .writer
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (current_generation, current_config): (i64, String) = tx.query_row(
            "SELECT repository_generation, config_hash FROM meta WHERE id = 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        verify_baseline(baseline, current_generation, &current_config)?;

        tx.execute("DELETE FROM files", [])?;
        let next_generation = current_generation.saturating_add(1);

        for file in files {
            Self::insert_file(&tx, &file, next_generation)?;
        }

        tx.execute(
            "UPDATE meta SET config_hash = ?1, repository_generation = ?2, index_version = index_version + 1 WHERE id = 1",
            params![config_hash, next_generation],
        )?;

        tx.commit()?;
        Ok(i64_to_u64(next_generation))
    }

    /// Atomically apply one repository reconciliation as a single generation.
    /// Unmentioned files remain unchanged; replacements and deletions become
    /// visible together when the transaction commits.
    pub fn reconcile_files(
        &self,
        config_hash: &str,
        replacements: Vec<IndexedFile>,
        deletions: &[String],
    ) -> Result<u64> {
        let baseline = self.meta()?;
        self.reconcile_files_at(&baseline, config_hash, replacements, deletions)
    }

    /// Publish an incremental plan only if its source generation and config
    /// still match the committed cache state.
    pub fn reconcile_files_at(
        &self,
        baseline: &MetaRecord,
        config_hash: &str,
        replacements: Vec<IndexedFile>,
        deletions: &[String],
    ) -> Result<u64> {
        let mut conn = self
            .writer
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (current_generation, current_config): (i64, String) = tx.query_row(
            "SELECT repository_generation, config_hash FROM meta WHERE id = 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        verify_baseline(baseline, current_generation, &current_config)?;

        if replacements.is_empty() && deletions.is_empty() && current_config == config_hash {
            tx.commit()?;
            return Ok(i64_to_u64(current_generation));
        }

        let next_generation = current_generation.saturating_add(1);
        for path in deletions {
            tx.execute("DELETE FROM files WHERE path = ?1", params![path])?;
            tx.execute(
                "UPDATE imports SET resolved_path = NULL WHERE resolved_path = ?1",
                params![path],
            )?;
        }
        for file in replacements {
            tx.execute("DELETE FROM files WHERE path = ?1", params![&file.path])?;
            Self::insert_file(&tx, &file, next_generation)?;
        }
        tx.execute(
            "UPDATE meta SET config_hash = ?1, repository_generation = ?2, index_version = index_version + 1 WHERE id = 1",
            params![config_hash, next_generation],
        )?;
        tx.commit()?;
        Ok(i64_to_u64(next_generation))
    }

    fn insert_file(tx: &Transaction, file: &IndexedFile, generation: i64) -> Result<()> {
        tx.execute(
            "INSERT INTO files(path, language, structurally_complete, size_bytes, modified_ns, content_hash, generation) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                &file.path,
                file.language.as_deref(),
                file.structurally_complete,
                u64_to_i64(file.size_bytes)?,
                file.modified_ns.map(u128_to_i64).transpose()?,
                &file.content_hash,
                generation,
            ],
        )?;
        let file_id = tx.last_insert_rowid();

        for chunk in &file.chunks {
            tx.execute(
                "INSERT INTO chunks(file_id, content, start_line, end_line, start_byte, end_byte, token_count) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    file_id,
                    &chunk.content,
                    usize_to_i64(chunk.start_line)?,
                    usize_to_i64(chunk.end_line)?,
                    usize_to_i64(chunk.start_byte)?,
                    usize_to_i64(chunk.end_byte)?,
                    usize_to_i64(chunk.token_count)?,
                ],
            )?;
        }

        for symbol in &file.symbols {
            tx.execute(
                "INSERT INTO symbols(file_id, name, kind, parent, signature, start_line, end_line, start_byte, end_byte) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![
                    file_id,
                    &symbol.name,
                    &symbol.kind,
                    symbol.parent.as_deref(),
                    symbol.signature.as_deref(),
                    usize_to_i64(symbol.start_line)?,
                    usize_to_i64(symbol.end_line)?,
                    usize_to_i64(symbol.start_byte)?,
                    usize_to_i64(symbol.end_byte)?,
                ],
            )?;
        }

        for reference in &file.references {
            tx.execute(
                "INSERT INTO symbol_refs(file_id, name, kind, role, enclosing_symbol, start_line, end_line, start_byte, end_byte) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![
                    file_id,
                    &reference.name,
                    &reference.kind,
                    role_to_str(reference.role),
                    reference.enclosing_symbol.as_deref(),
                    usize_to_i64(reference.start_line)?,
                    usize_to_i64(reference.end_line)?,
                    usize_to_i64(reference.start_byte)?,
                    usize_to_i64(reference.end_byte)?,
                ],
            )?;
        }

        for import in &file.imports {
            tx.execute(
                "INSERT INTO imports(file_id, raw_target, resolved_path, line) VALUES (?1, ?2, ?3, ?4)",
                params![
                    file_id,
                    &import.raw_target,
                    import.resolved_path.as_deref(),
                    usize_to_i64(import.line)?,
                ],
            )?;
        }

        Ok(())
    }

    pub fn list_files(&self, max_results: usize, cursor: Option<i64>) -> Result<Vec<FileRecord>> {
        self.begin_read()?.list_files(max_results, cursor)
    }

    pub fn find_file(&self, path: &str) -> Result<Option<FileRecord>> {
        self.begin_read()?.find_file(path)
    }

    pub fn get_chunks_for_file(
        &self,
        file_id: i64,
        max_results: usize,
    ) -> Result<Vec<ChunkRecord>> {
        self.begin_read()?.get_chunks_for_file(file_id, max_results)
    }

    pub fn get_symbols_for_file(
        &self,
        file_id: i64,
        max_results: usize,
    ) -> Result<Vec<SymbolRecord>> {
        self.begin_read()?
            .get_symbols_for_file(file_id, max_results)
    }

    pub fn get_references_for_file(
        &self,
        file_id: i64,
        max_results: usize,
    ) -> Result<Vec<ReferenceRecord>> {
        self.begin_read()?
            .get_references_for_file(file_id, max_results)
    }

    pub fn get_imports_for_file(
        &self,
        file_id: i64,
        max_results: usize,
    ) -> Result<Vec<ImportRecord>> {
        self.begin_read()?
            .get_imports_for_file(file_id, max_results)
    }

    pub fn search_word(&self, query: &str, max_results: usize) -> Result<Vec<ChunkHit>> {
        self.begin_read()?.search_word(query, max_results)
    }

    pub fn search_trigram(&self, query: &str, max_results: usize) -> Result<Vec<ChunkHit>> {
        self.begin_read()?.search_trigram(query, max_results)
    }

    pub fn search_symbols(
        &self,
        query: &str,
        case_sensitive: bool,
        max_results: usize,
    ) -> Result<Vec<SymbolHit>> {
        self.begin_read()?
            .search_symbols(query, case_sensitive, max_results)
    }

    pub fn search_references(
        &self,
        query: &str,
        case_sensitive: bool,
        max_results: usize,
    ) -> Result<Vec<ReferenceHit>> {
        self.begin_read()?
            .search_references(query, case_sensitive, max_results)
    }

    pub fn counts(&self) -> Result<StorageCounts> {
        self.begin_read()?.counts()
    }

    /// Open a read-only connection and begin a DEFERRED transaction so callers
    /// observe one WAL snapshot until the session is dropped.
    pub fn begin_read(&self) -> Result<ReadSession> {
        let conn = Connection::open_with_flags(&self.path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        conn.busy_timeout(DEFAULT_BUSY_TIMEOUT)?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        // Under WAL, the first read in a DEFERRED transaction pins the snapshot
        // for the rest of the connection's transaction lifetime.
        conn.execute_batch("BEGIN DEFERRED")?;
        Ok(ReadSession { conn })
    }

    fn map_file(row: &Row) -> std::result::Result<FileRecord, rusqlite::Error> {
        Ok(FileRecord {
            id: row.get(0)?,
            path: row.get(1)?,
            language: row.get(2)?,
            size_bytes: i64_to_u64(row.get(3)?),
            modified_ns: row.get::<_, Option<i64>>(4)?.map(i64_to_u128),
            content_hash: row.get(5)?,
            generation: i64_to_u64(row.get(6)?),
            structurally_complete: row.get(7)?,
        })
    }

    fn map_chunk(row: &Row) -> std::result::Result<ChunkRecord, rusqlite::Error> {
        Ok(ChunkRecord {
            id: row.get(0)?,
            file_id: row.get(1)?,
            content: row.get(2)?,
            start_line: i64_to_usize(row.get(3)?),
            end_line: i64_to_usize(row.get(4)?),
            start_byte: i64_to_usize(row.get(5)?),
            end_byte: i64_to_usize(row.get(6)?),
            token_count: i64_to_usize(row.get(7)?),
        })
    }

    fn map_symbol(row: &Row) -> std::result::Result<SymbolRecord, rusqlite::Error> {
        Ok(SymbolRecord {
            id: row.get(0)?,
            file_id: row.get(1)?,
            name: row.get(2)?,
            kind: row.get(3)?,
            parent: row.get(4)?,
            signature: row.get(5)?,
            start_line: i64_to_usize(row.get(6)?),
            end_line: i64_to_usize(row.get(7)?),
            start_byte: i64_to_usize(row.get(8)?),
            end_byte: i64_to_usize(row.get(9)?),
        })
    }

    fn map_reference(row: &Row) -> std::result::Result<ReferenceRecord, rusqlite::Error> {
        Ok(ReferenceRecord {
            id: row.get(0)?,
            file_id: row.get(1)?,
            name: row.get(2)?,
            kind: row.get(3)?,
            role: role_from_str(&row.get::<_, String>(4)?),
            enclosing_symbol: row.get(5)?,
            start_line: i64_to_usize(row.get(6)?),
            end_line: i64_to_usize(row.get(7)?),
            start_byte: i64_to_usize(row.get(8)?),
            end_byte: i64_to_usize(row.get(9)?),
        })
    }

    fn map_import(row: &Row) -> std::result::Result<ImportRecord, rusqlite::Error> {
        Ok(ImportRecord {
            id: row.get(0)?,
            file_id: row.get(1)?,
            raw_target: row.get(2)?,
            resolved_path: row.get(3)?,
            line: i64_to_usize(row.get(4)?),
        })
    }

    fn map_chunk_hit(row: &Row) -> std::result::Result<ChunkHit, rusqlite::Error> {
        Ok(ChunkHit {
            chunk_id: row.get(0)?,
            file_id: row.get(1)?,
            path: row.get(2)?,
            content: row.get(3)?,
            start_line: i64_to_usize(row.get(4)?),
            end_line: i64_to_usize(row.get(5)?),
            start_byte: i64_to_usize(row.get(6)?),
            end_byte: i64_to_usize(row.get(7)?),
            token_count: i64_to_usize(row.get(8)?),
            score: row.get::<_, f64>(9)?,
        })
    }
}

impl ReadSession {
    pub fn meta(&self) -> Result<MetaRecord> {
        self.conn
            .query_row(
                "SELECT schema_version, index_version, config_hash, repository_generation FROM meta WHERE id = 1",
                [],
                |row| {
                    Ok(MetaRecord {
                        schema_version: row.get(0)?,
                        index_version: row.get(1)?,
                        config_hash: row.get(2)?,
                        repository_generation: i64_to_u64(row.get(3)?),
                    })
                },
            )
            .map_err(Into::into)
    }

    pub fn repository_generation(&self) -> Result<u64> {
        let generation: i64 = self.conn.query_row(
            "SELECT repository_generation FROM meta WHERE id = 1",
            [],
            |row| row.get(0),
        )?;
        Ok(i64_to_u64(generation))
    }

    pub fn list_files(&self, max_results: usize, cursor: Option<i64>) -> Result<Vec<FileRecord>> {
        let limit = bounded_limit(max_results);
        let mut stmt = self.conn.prepare(
            "SELECT id, path, language, size_bytes, modified_ns, content_hash, generation, structurally_complete FROM files WHERE (?1 IS NULL OR id > ?1) ORDER BY id LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![cursor, limit], Storage::map_file)?;
        let mut files = Vec::new();
        for row in rows {
            files.push(row?);
        }
        Ok(files)
    }

    pub fn find_file(&self, path: &str) -> Result<Option<FileRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, path, language, size_bytes, modified_ns, content_hash, generation, structurally_complete FROM files WHERE path = ?1",
        )?;
        let mut rows = stmt.query_map(params![path], Storage::map_file)?;
        Ok(rows.next().transpose()?)
    }

    pub fn get_chunks_for_file(
        &self,
        file_id: i64,
        max_results: usize,
    ) -> Result<Vec<ChunkRecord>> {
        let limit = bounded_limit(max_results);
        let mut stmt = self.conn.prepare(
            "SELECT id, file_id, content, start_line, end_line, start_byte, end_byte, token_count FROM chunks WHERE file_id = ?1 ORDER BY start_byte LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![file_id, limit], Storage::map_chunk)?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    pub(crate) fn get_chunks_overlapping(
        &self,
        file_id: i64,
        start_line: usize,
        end_line: usize,
    ) -> Result<Vec<ChunkRecord>> {
        let start_line = usize_to_i64(start_line)?;
        let end_line = usize_to_i64(end_line)?;
        let mut stmt = self.conn.prepare(
            "SELECT id, file_id, content, start_line, end_line, start_byte, end_byte, token_count
                 FROM chunks
                 WHERE file_id = ?1 AND end_line >= ?2 AND start_line <= ?3
                 ORDER BY start_line",
        )?;
        let rows = stmt.query_map(params![file_id, start_line, end_line], Storage::map_chunk)?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    pub fn get_symbols_for_file(
        &self,
        file_id: i64,
        max_results: usize,
    ) -> Result<Vec<SymbolRecord>> {
        let limit = bounded_limit(max_results);
        let mut stmt = self.conn.prepare(
            "SELECT id, file_id, name, kind, parent, signature, start_line, end_line, start_byte, end_byte FROM symbols WHERE file_id = ?1 ORDER BY start_byte LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![file_id, limit], Storage::map_symbol)?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    pub(crate) fn get_symbols_for_file_filtered(
        &self,
        file_id: i64,
        name: Option<&str>,
        kind: Option<&str>,
        max_results: usize,
    ) -> Result<Vec<SymbolRecord>> {
        let limit = bounded_limit(max_results);
        let mut stmt = self.conn.prepare(
            "SELECT id, file_id, name, kind, parent, signature, start_line, end_line, start_byte, end_byte
                 FROM symbols
                 WHERE file_id = ?1
                   AND (?2 IS NULL OR instr(name, ?2) > 0)
                   AND (?3 IS NULL OR kind = ?3)
                 ORDER BY start_byte
                 LIMIT ?4",
        )?;
        let rows = stmt.query_map(params![file_id, name, kind, limit], Storage::map_symbol)?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    pub(crate) fn find_symbol(&self, file_id: i64, name: &str) -> Result<Option<SymbolRecord>> {
        Ok(self
            .conn
            .query_row(
                "SELECT id, file_id, name, kind, parent, signature, start_line, end_line, start_byte, end_byte
                     FROM symbols
                     WHERE file_id = ?1 AND name = ?2
                     ORDER BY start_byte
                     LIMIT 1",
                params![file_id, name],
                Storage::map_symbol,
            )
            .optional()?)
    }

    pub(crate) fn find_enclosing_symbol(
        &self,
        file_id: i64,
        line: usize,
    ) -> Result<Option<SymbolRecord>> {
        let line = usize_to_i64(line)?;
        Ok(self
            .conn
            .query_row(
                "SELECT id, file_id, name, kind, parent, signature, start_line, end_line, start_byte, end_byte
                     FROM symbols
                     WHERE file_id = ?1 AND start_line <= ?2 AND end_line >= ?2
                     ORDER BY (end_line - start_line), start_byte
                     LIMIT 1",
                params![file_id, line],
                Storage::map_symbol,
            )
            .optional()?)
    }

    pub fn get_references_for_file(
        &self,
        file_id: i64,
        max_results: usize,
    ) -> Result<Vec<ReferenceRecord>> {
        let limit = bounded_limit(max_results);
        let mut stmt = self.conn.prepare(
            "SELECT id, file_id, name, kind, role, enclosing_symbol, start_line, end_line, start_byte, end_byte FROM symbol_refs WHERE file_id = ?1 ORDER BY start_byte LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![file_id, limit], Storage::map_reference)?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    pub fn get_imports_for_file(
        &self,
        file_id: i64,
        max_results: usize,
    ) -> Result<Vec<ImportRecord>> {
        let limit = bounded_limit(max_results);
        let mut stmt = self.conn.prepare(
            "SELECT id, file_id, raw_target, resolved_path, line FROM imports WHERE file_id = ?1 ORDER BY line LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![file_id, limit], Storage::map_import)?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    pub fn search_word(&self, query: &str, max_results: usize) -> Result<Vec<ChunkHit>> {
        self.search_fts(FtsTable::Word, query, max_results)
    }

    pub fn search_trigram(&self, query: &str, max_results: usize) -> Result<Vec<ChunkHit>> {
        if query.chars().count() < 3 {
            return Ok(Vec::new());
        }
        let escaped = query.replace('"', "\"\"");
        let quoted = format!("\"{escaped}\"");
        self.search_fts(FtsTable::Trigram, &quoted, max_results)
    }

    pub fn search_symbols(
        &self,
        query: &str,
        case_sensitive: bool,
        max_results: usize,
    ) -> Result<Vec<SymbolHit>> {
        let limit = bounded_limit(max_results);
        let mut stmt = self.conn.prepare(
            "SELECT f.path, f.content_hash, s.id, s.file_id, s.name, s.kind, s.parent, s.signature, s.start_line, s.end_line, s.start_byte, s.end_byte
                 FROM symbols s JOIN files f ON f.id = s.file_id
                 WHERE CASE WHEN ?2 THEN instr(s.name, ?1) > 0 ELSE instr(lower(s.name), lower(?1)) > 0 END
                 ORDER BY CASE WHEN CASE WHEN ?2 THEN s.name = ?1 ELSE lower(s.name) = lower(?1) END THEN 0 ELSE 1 END,
                          length(s.name), f.path, s.start_byte
                 LIMIT ?3",
        )?;
        let rows = stmt.query_map(params![query, case_sensitive, limit], |row| {
            Ok(SymbolHit {
                path: row.get(0)?,
                content_hash: row.get(1)?,
                symbol: SymbolRecord {
                    id: row.get(2)?,
                    file_id: row.get(3)?,
                    name: row.get(4)?,
                    kind: row.get(5)?,
                    parent: row.get(6)?,
                    signature: row.get(7)?,
                    start_line: i64_to_usize(row.get(8)?),
                    end_line: i64_to_usize(row.get(9)?),
                    start_byte: i64_to_usize(row.get(10)?),
                    end_byte: i64_to_usize(row.get(11)?),
                },
            })
        })?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    pub fn search_references(
        &self,
        query: &str,
        case_sensitive: bool,
        max_results: usize,
    ) -> Result<Vec<ReferenceHit>> {
        let limit = bounded_limit(max_results);
        let mut stmt = self.conn.prepare(
            "SELECT f.path, f.content_hash, r.id, r.file_id, r.name, r.kind, r.role, r.enclosing_symbol, r.start_line, r.end_line, r.start_byte, r.end_byte
                 FROM symbol_refs r JOIN files f ON f.id = r.file_id
                 WHERE CASE WHEN ?2 THEN instr(r.name, ?1) > 0 ELSE instr(lower(r.name), lower(?1)) > 0 END
                 ORDER BY CASE WHEN CASE WHEN ?2 THEN r.name = ?1 ELSE lower(r.name) = lower(?1) END THEN 0 ELSE 1 END,
                          length(r.name), f.path, r.start_byte
                 LIMIT ?3",
        )?;
        let rows = stmt.query_map(params![query, case_sensitive, limit], |row| {
            Ok(ReferenceHit {
                path: row.get(0)?,
                content_hash: row.get(1)?,
                reference: ReferenceRecord {
                    id: row.get(2)?,
                    file_id: row.get(3)?,
                    name: row.get(4)?,
                    kind: row.get(5)?,
                    role: role_from_str(&row.get::<_, String>(6)?),
                    enclosing_symbol: row.get(7)?,
                    start_line: i64_to_usize(row.get(8)?),
                    end_line: i64_to_usize(row.get(9)?),
                    start_byte: i64_to_usize(row.get(10)?),
                    end_byte: i64_to_usize(row.get(11)?),
                },
            })
        })?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    pub fn counts(&self) -> Result<StorageCounts> {
        let files = i64_to_usize(
            self.conn
                .query_row("SELECT count(*) FROM files", [], |row| row.get(0))?,
        );
        let chunks = i64_to_usize(self.conn.query_row(
            "SELECT count(*) FROM chunks",
            [],
            |row| row.get(0),
        )?);
        let symbols = i64_to_usize(self.conn.query_row(
            "SELECT count(*) FROM symbols",
            [],
            |row| row.get(0),
        )?);
        let mut stmt = self.conn.prepare(
            "SELECT language, count(*) FROM files WHERE language IS NOT NULL GROUP BY language ORDER BY language",
        )?;
        let languages = stmt
            .query_map([], |row| Ok((row.get(0)?, i64_to_usize(row.get(1)?))))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(StorageCounts {
            files,
            chunks,
            symbols,
            languages,
        })
    }

    fn search_fts(
        &self,
        table: FtsTable,
        query: &str,
        max_results: usize,
    ) -> Result<Vec<ChunkHit>> {
        let limit = bounded_limit(max_results);
        let table_name = table.as_str();
        let sql = format!(
            "SELECT c.id, c.file_id, f.path, c.content, c.start_line, c.end_line, c.start_byte, c.end_byte, c.token_count, bm25({table_name}) as score \
             FROM {table_name} \
             JOIN chunks c ON {table_name}.rowid = c.rowid \
             JOIN files f ON c.file_id = f.id \
             WHERE {table_name} MATCH ?1 \
             ORDER BY bm25({table_name}), f.path, c.start_byte \
             LIMIT ?2"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(params![query, limit], Storage::map_chunk_hit)?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }
}

fn verify_baseline(
    baseline: &MetaRecord,
    current_generation: i64,
    current_config: &str,
) -> Result<()> {
    let actual = i64_to_u64(current_generation);
    if actual != baseline.repository_generation || current_config != baseline.config_hash {
        return Err(Error::StaleReconciliation {
            expected: baseline.repository_generation,
            actual,
        });
    }
    Ok(())
}

fn bounded_limit(limit: usize) -> i64 {
    let capped = limit.clamp(1, HARD_MAX_RESULTS);
    i64::try_from(capped).unwrap_or(i64::MAX)
}

fn u64_to_i64(value: u64) -> Result<i64> {
    i64::try_from(value)
        .map_err(|_| Error::InvalidRequest("value exceeds storage integer range".into()))
}

fn usize_to_i64(value: usize) -> Result<i64> {
    i64::try_from(value)
        .map_err(|_| Error::InvalidRequest("value exceeds storage integer range".into()))
}

fn u128_to_i64(value: u128) -> Result<i64> {
    i64::try_from(value)
        .map_err(|_| Error::InvalidRequest("value exceeds storage integer range".into()))
}

fn i64_to_u64(value: i64) -> u64 {
    u64::try_from(value).unwrap_or(0)
}

fn i64_to_usize(value: i64) -> usize {
    usize::try_from(value).unwrap_or(0)
}

fn i64_to_u128(value: i64) -> u128 {
    u128::try_from(value).unwrap_or(0)
}

fn role_to_str(role: ReferenceRole) -> &'static str {
    match role {
        ReferenceRole::Definition => "definition",
        ReferenceRole::Reference => "reference",
    }
}

fn role_from_str(role: &str) -> ReferenceRole {
    match role {
        "definition" => ReferenceRole::Definition,
        _ => ReferenceRole::Reference,
    }
}
