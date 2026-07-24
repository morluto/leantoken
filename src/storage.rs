use std::{
    collections::{HashMap, HashSet},
    fmt, fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use r2d2_sqlite::SqliteConnectionManager;
use rusqlite::{
    Connection, OpenFlags, OptionalExtension, Row, Transaction, TransactionBehavior, params,
};
use rusqlite_migration::{M, Migrations};

use crate::model::{ReferenceRole, TokenSavingsOperation};
use crate::{Error, Result};

// SQLite normally recycles a completed WAL without shrinking it. Retain four
// default auto-checkpoint windows for reuse while bounding the steady-state
// disk footprint after a large initial publication.
const WAL_JOURNAL_SIZE_LIMIT_BYTES: i64 = 16 * 1024 * 1024;

pub(crate) const CURRENT_SCHEMA_VERSION: i64 = 5;

/// Default row limit used by callers that do not provide a tighter bound.
pub const DEFAULT_MAX_RESULTS: usize = 100;
/// Absolute row limit applied by storage queries, including internal batch reads.
pub const HARD_MAX_RESULTS: usize = 10_000;

const DEFAULT_BUSY_TIMEOUT: Duration = Duration::from_millis(5_000);
const READ_ONLY_STATUS_BUSY_TIMEOUT: Duration = Duration::from_millis(100);

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

const REPOSITORY_OWNERSHIP_SQL: &str = r#"
ALTER TABLE meta ADD COLUMN repository_root TEXT NOT NULL DEFAULT '';
ALTER TABLE meta ADD COLUMN repository_identity TEXT NOT NULL DEFAULT '';
UPDATE meta SET schema_version = 2 WHERE id = 1;
"#;

const IMPORT_CANDIDATES_SQL: &str = r#"
CREATE TABLE import_candidates (
    import_id INTEGER NOT NULL REFERENCES imports(id) ON DELETE CASCADE,
    candidate_path TEXT NOT NULL,
    priority INTEGER NOT NULL,
    PRIMARY KEY(import_id, candidate_path)
);
CREATE INDEX import_candidates_path_idx
ON import_candidates(candidate_path, import_id);
UPDATE meta SET schema_version = 3 WHERE id = 1;
"#;

const PATH_PROJECTION_SQL: &str = r#"
CREATE TABLE path_entries (
    path TEXT PRIMARY KEY,
    depth INTEGER NOT NULL,
    kind INTEGER NOT NULL,
    file_id INTEGER UNIQUE REFERENCES files(id) ON DELETE CASCADE
);
CREATE INDEX path_entries_depth_path_idx ON path_entries(depth, path);
UPDATE meta SET schema_version = 4 WHERE id = 1;
"#;

const CACHE_ACCESS_SQL: &str = r#"
ALTER TABLE meta ADD COLUMN last_access_unix_seconds INTEGER NOT NULL DEFAULT 0;
UPDATE meta
SET last_access_unix_seconds = CAST(strftime('%s', 'now') AS INTEGER),
    schema_version = 5
WHERE id = 1;
"#;

const TOKEN_SAVINGS_TABLE_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS token_savings (
    tokenizer TEXT NOT NULL,
    operation TEXT NOT NULL,
    tracked_requests INTEGER NOT NULL DEFAULT 0,
    baseline_source_tokens INTEGER NOT NULL DEFAULT 0,
    emitted_source_tokens INTEGER NOT NULL DEFAULT 0,
    estimated_source_tokens_saved INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY(tokenizer, operation)
);
"#;

const MIGRATIONS_SLICE: &[M<'_>] = &[
    M::up(SCHEMA_SQL).foreign_key_check(),
    M::up(LOOKUP_INDEXES_SQL),
    M::up(REPOSITORY_OWNERSHIP_SQL),
    M::up(IMPORT_CANDIDATES_SQL),
    M::up(PATH_PROJECTION_SQL),
    M::up(CACHE_ACCESS_SQL),
];
pub(crate) const CURRENT_MIGRATION_VERSION: i64 = 6;
const _: () = assert!(MIGRATIONS_SLICE.len() == CURRENT_MIGRATION_VERSION as usize);
const MIGRATIONS: Migrations<'_> = Migrations::from_slice(MIGRATIONS_SLICE);

#[derive(Debug, Clone)]
/// Version and publication state read from the singleton metadata row.
///
/// `repository_generation` identifies an atomically published repository view.
/// A reconciliation plan must retain this record and pass it back to a checked
/// publication method so stale filesystem work cannot overwrite newer state.
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
pub(crate) struct PathRecord {
    pub path: String,
    pub is_directory: bool,
    pub language: Option<String>,
    pub size_bytes: Option<u64>,
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
/// One parsed import and the bounded path candidates produced by the indexer's
/// language-specific import policy.
///
/// Storage persists every candidate in priority order for reverse invalidation;
/// `resolved_path` is populated only when exactly one candidate exists in the
/// repository view used to prepare the file.
pub struct ImportInput {
    pub raw_target: String,
    pub resolved_path: Option<String>,
    pub candidate_paths: Vec<String>,
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
/// Complete derived representation of one file, ready for transactional publication.
///
/// The indexer constructs these values one bounded batch at a time. Storage
/// treats each file's chunks, symbols, references, imports, and path projection
/// as one replacement unit inside the caller's uncommitted generation.
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
    pub generation: u64,
    pub score: f64,
}

#[derive(Debug, Clone)]
pub struct SymbolHit {
    pub path: String,
    pub content_hash: String,
    pub generation: u64,
    pub symbol: SymbolRecord,
}

#[derive(Debug, Clone)]
pub struct ReferenceHit {
    pub path: String,
    pub content_hash: String,
    pub generation: u64,
    pub reference: ReferenceRecord,
}

pub(crate) struct ImportSymbolTarget {
    pub seed_index: usize,
    pub target_file: FileRecord,
    pub symbols: Vec<SymbolRecord>,
}

#[derive(Debug, Clone)]
pub struct StorageCounts {
    pub files: usize,
    pub chunks: usize,
    pub symbols: usize,
    pub languages: Vec<(String, usize)>,
}

#[derive(Debug, Clone)]
pub(crate) struct ReadOnlyStatusSnapshot {
    pub generation: u64,
    pub counts: StorageCounts,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct TokenSavingsRecord {
    pub tracked_requests: u64,
    pub baseline_source_tokens: u64,
    pub emitted_source_tokens: u64,
    pub estimated_source_tokens_saved: u64,
}

/// SQLite-backed repository index with one serialized writer and pooled readers.
///
/// Clones share the same writer mutex and established read pool. Each
/// [`ReadSession`] checks out one read-only connection and pins a WAL snapshot,
/// while reconciliation publishes through one immediate transaction. Pooling is
/// process-local; repository ownership and cross-process write serialization are
/// enforced separately by the services and coordination layers.
pub struct Storage {
    writer: Arc<Mutex<Connection>>,
    readers: r2d2::Pool<SqliteConnectionManager>,
    path: PathBuf,
}

/// Restricted writer for one uncommitted repository generation.
pub(crate) struct ReconciliationWriter<'transaction, 'connection> {
    transaction: &'transaction Transaction<'connection>,
    generation: i64,
    rebuild: bool,
    replacements: usize,
    deletions: HashSet<String>,
}

impl ReconciliationWriter<'_, '_> {
    /// Insert one complete file replacement without retaining it in memory.
    pub(crate) fn replace(&mut self, file: IndexedFile) -> Result<()> {
        self.replace_inner(file, None)
    }

    pub(crate) fn replace_with_source_tokens(
        &mut self,
        file: IndexedFile,
        tokenizer: &str,
        source_token_count: usize,
    ) -> Result<()> {
        self.replace_inner(file, Some((tokenizer, source_token_count)))
    }

    fn replace_inner(
        &mut self,
        file: IndexedFile,
        source_tokens: Option<(&str, usize)>,
    ) -> Result<()> {
        if !self.rebuild {
            self.transaction
                .execute("DELETE FROM files WHERE path = ?1", params![&file.path])?;
        }
        Storage::insert_file(self.transaction, &file, self.generation, source_tokens)?;
        self.replacements = self.replacements.saturating_add(1);
        Ok(())
    }

    /// Remove one path, deduplicating repeated deletion signals.
    pub(crate) fn delete(&mut self, path: &str) -> Result<()> {
        if !self.deletions.insert(path.to_string()) || self.rebuild {
            return Ok(());
        }
        self.transaction
            .execute("DELETE FROM files WHERE path = ?1", params![path])?;
        self.transaction.execute(
            "UPDATE imports SET resolved_path = NULL WHERE resolved_path = ?1",
            params![path],
        )?;
        Ok(())
    }
}

impl Clone for Storage {
    fn clone(&self) -> Self {
        Self {
            writer: Arc::clone(&self.writer),
            readers: self.readers.clone(),
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
    conn: r2d2::PooledConnection<SqliteConnectionManager>,
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
    /// Open or migrate a SQLite index without binding it to a repository root.
    ///
    /// Application code should normally construct [`crate::services::Services`],
    /// which also verifies repository ownership. This lower-level constructor is
    /// useful for storage tests and tools that deliberately manage that invariant.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        Self::open_with_startup_timeout(path, DEFAULT_BUSY_TIMEOUT)
    }

    /// Read status from an existing cache without running migrations, changing
    /// SQLite pragmas, or binding the cache to a repository.
    pub(crate) fn read_only_status(
        path: &Path,
        repository_root: &Path,
    ) -> Result<ReadOnlyStatusSnapshot> {
        let conn = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        conn.busy_timeout(READ_ONLY_STATUS_BUSY_TIMEOUT)?;
        conn.execute_batch("BEGIN DEFERRED")?;

        if !table_exists(&conn, "meta")? {
            return Ok(ReadOnlyStatusSnapshot {
                generation: 0,
                counts: StorageCounts {
                    files: 0,
                    chunks: 0,
                    symbols: 0,
                    languages: Vec::new(),
                },
            });
        }

        let has_repository_root = column_exists(&conn, "meta", "repository_root")?;
        let has_repository_identity = column_exists(&conn, "meta", "repository_identity")?;
        let expected_repository = if has_repository_root {
            conn.query_row("SELECT repository_root FROM meta WHERE id = 1", [], |row| {
                row.get::<_, String>(0)
            })?
        } else {
            String::new()
        };
        let expected_identity = if has_repository_identity {
            conn.query_row(
                "SELECT repository_identity FROM meta WHERE id = 1",
                [],
                |row| row.get::<_, String>(0),
            )?
        } else {
            String::new()
        };
        let actual_identity = repository_identity(repository_root);
        let actual_repository = repository_root.to_string_lossy();
        let mismatched_identity =
            !expected_identity.is_empty() && expected_identity != actual_identity;
        let mismatched_legacy_root = expected_identity.is_empty()
            && !expected_repository.is_empty()
            && expected_repository != actual_repository;
        if mismatched_identity || mismatched_legacy_root {
            return Err(Error::RepositoryMismatch {
                database: path.to_path_buf(),
                expected_repository,
                actual_repository: repository_root.to_path_buf(),
            });
        }

        let generation = if column_exists(&conn, "meta", "repository_generation")? {
            i64_to_u64(conn.query_row(
                "SELECT repository_generation FROM meta WHERE id = 1",
                [],
                |row| row.get::<_, i64>(0),
            )?)
        } else {
            0
        };
        let files = count_table_rows(&conn, "files")?;
        let chunks = count_table_rows(&conn, "chunks")?;
        let symbols = count_table_rows(&conn, "symbols")?;
        let languages = if table_exists(&conn, "files")? {
            let mut statement = conn.prepare(
                "SELECT language, count(*) FROM files WHERE language IS NOT NULL GROUP BY language ORDER BY language",
            )?;
            statement
                .query_map([], |row| Ok((row.get(0)?, i64_to_usize(row.get(1)?))))?
                .collect::<std::result::Result<Vec<_>, _>>()?
        } else {
            Vec::new()
        };

        Ok(ReadOnlyStatusSnapshot {
            generation,
            counts: StorageCounts {
                files,
                chunks,
                symbols,
                languages,
            },
        })
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
        Self::ensure_token_savings_schema(&mut conn)?;
        Self::ensure_path_projection(&mut conn)?;
        Self::validate_fts5(&mut conn)?;
        conn.busy_timeout(DEFAULT_BUSY_TIMEOUT)?;

        let manager = SqliteConnectionManager::file(&path)
            .with_flags(OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX)
            .with_init(|connection| {
                connection.busy_timeout(DEFAULT_BUSY_TIMEOUT)?;
                connection.pragma_update(None, "foreign_keys", "ON")
            });
        let readers = r2d2::Pool::builder()
            .max_size(8)
            .connection_timeout(DEFAULT_BUSY_TIMEOUT)
            .test_on_check_out(false)
            .build(manager)?;

        Ok(Self {
            writer: Arc::new(Mutex::new(conn)),
            readers,
            path,
        })
    }

    pub(crate) fn open_for_repository(
        path: impl AsRef<Path>,
        repository_root: &Path,
    ) -> Result<Self> {
        Self::open_for_repository_with_startup_timeout(path, repository_root, DEFAULT_BUSY_TIMEOUT)
    }

    pub(crate) fn open_for_repository_with_startup_timeout(
        path: impl AsRef<Path>,
        repository_root: &Path,
        startup_timeout: Duration,
    ) -> Result<Self> {
        let storage = Self::open_with_startup_timeout(path, startup_timeout)?;
        storage.bind_repository(repository_root)?;
        Ok(storage)
    }

    fn bind_repository(&self, repository_root: &Path) -> Result<()> {
        self.bind_repository_at(repository_root, unix_seconds(SystemTime::now()))
    }

    fn bind_repository_at(&self, repository_root: &Path, accessed_at: i64) -> Result<()> {
        let actual_repository = repository_root.to_path_buf();
        let actual_display = repository_root.to_string_lossy();
        let actual_identity = repository_identity(repository_root);
        let mut conn = self
            .writer
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (expected_repository, expected_identity): (String, String) = tx.query_row(
            "SELECT repository_root, repository_identity FROM meta WHERE id = 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;

        if expected_identity.is_empty() {
            tx.execute(
                "UPDATE meta SET repository_root = ?1, repository_identity = ?2, last_access_unix_seconds = ?3 WHERE id = 1",
                params![actual_display.as_ref(), actual_identity, accessed_at],
            )?;
            tx.commit()?;
            return Ok(());
        }
        if expected_identity != actual_identity {
            return Err(Error::RepositoryMismatch {
                database: self.path.clone(),
                expected_repository,
                actual_repository,
            });
        }

        tx.execute(
            "UPDATE meta SET last_access_unix_seconds = ?1 WHERE id = 1",
            params![accessed_at],
        )?;
        tx.commit()?;
        Ok(())
    }

    fn configure(conn: &mut Connection, startup_timeout: Duration) -> Result<()> {
        conn.busy_timeout(startup_timeout)?;
        conn.pragma_update_and_check(None, "journal_mode", "WAL", |_| Ok(()))?;
        conn.pragma_update(None, "journal_size_limit", WAL_JOURNAL_SIZE_LIMIT_BYTES)?;
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

    fn ensure_path_projection(conn: &mut Connection) -> Result<()> {
        let file_count: i64 = conn.query_row("SELECT count(*) FROM files", [], |row| row.get(0))?;
        let projected_files: i64 = conn.query_row(
            "SELECT count(*) FROM path_entries WHERE kind = 1",
            [],
            |row| row.get(0),
        )?;
        if file_count == projected_files {
            return Ok(());
        }
        let paths = {
            let mut stmt = conn.prepare("SELECT id, path FROM files ORDER BY id")?;
            stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
                .collect::<std::result::Result<Vec<(i64, String)>, _>>()?
        };
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute("DELETE FROM path_entries", [])?;
        for (file_id, path) in paths {
            Self::insert_path_projection(&tx, &path, file_id)?;
        }
        tx.commit()?;
        Ok(())
    }

    fn ensure_token_savings_schema(conn: &mut Connection) -> Result<()> {
        // These additive fields are intentionally outside the numbered cache
        // schema so older LeanToken versions can still open and rebuild it.
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let columns = {
            let mut stmt = tx.prepare("PRAGMA table_info(files)")?;
            stmt.query_map([], |row| row.get::<_, String>(1))?
                .collect::<std::result::Result<HashSet<_>, _>>()?
        };
        if !columns.contains("source_token_count") {
            tx.execute_batch(
                "ALTER TABLE files ADD COLUMN source_token_count INTEGER NOT NULL DEFAULT 0;",
            )?;
        }
        if !columns.contains("source_tokenizer") {
            tx.execute_batch(
                "ALTER TABLE files ADD COLUMN source_tokenizer TEXT NOT NULL DEFAULT '';",
            )?;
        }
        tx.execute_batch(TOKEN_SAVINGS_TABLE_SQL)?;
        tx.commit()?;
        Ok(())
    }

    /// Read the currently committed schema, configuration, and generation metadata.
    pub fn meta(&self) -> Result<MetaRecord> {
        self.begin_read()?.meta()
    }

    /// Return the identifier of the latest atomically committed repository view.
    pub fn repository_generation(&self) -> Result<u64> {
        self.begin_read()?.repository_generation()
    }

    /// Replace the complete index using an internally captured optimistic baseline.
    ///
    /// Indexing code that performs filesystem work before publication should use
    /// [`Self::full_reconcile_at`] with the baseline captured before that work.
    pub fn full_reconcile(&self, config_hash: &str, files: Vec<IndexedFile>) -> Result<u64> {
        let baseline = self.meta()?;
        self.full_reconcile_at(&baseline, config_hash, files)
    }

    /// Replace the complete index only if the generation and configuration used
    /// to build the reconciliation plan are still current.
    ///
    /// On success, all derived rows and the next generation become visible in one
    /// commit. A stale baseline returns [`Error::StaleReconciliation`] before any
    /// mutation is published.
    pub fn full_reconcile_at(
        &self,
        baseline: &MetaRecord,
        config_hash: &str,
        files: Vec<IndexedFile>,
    ) -> Result<u64> {
        self.publish_reconciliation_at(baseline, config_hash, true, move |writer| {
            for file in files {
                writer.replace(file)?;
            }
            Ok(())
        })
        .map(|(generation, ())| generation)
    }

    /// Atomically apply one repository reconciliation using an internally captured baseline.
    ///
    /// Unmentioned files remain unchanged; replacements, deletions, derived path
    /// rows, import edges, and generation advancement become visible together.
    /// Indexing code should prefer [`Self::reconcile_files_at`] when planning and
    /// publication are separated by filesystem or parsing work.
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
    ///
    /// Replacements are whole-file units and deletions are repository-relative
    /// paths. A no-op preserves the current generation when the configuration is
    /// unchanged. Stale plans fail before publishing any mutation.
    pub fn reconcile_files_at(
        &self,
        baseline: &MetaRecord,
        config_hash: &str,
        replacements: Vec<IndexedFile>,
        deletions: &[String],
    ) -> Result<u64> {
        self.publish_reconciliation_at(baseline, config_hash, false, move |writer| {
            for path in deletions {
                writer.delete(path)?;
            }
            for file in replacements {
                writer.replace(file)?;
            }
            Ok(())
        })
        .map(|(generation, ())| generation)
    }

    /// Build and publish one generation through a bounded caller-owned stream.
    pub(crate) fn publish_reconciliation_at<T>(
        &self,
        baseline: &MetaRecord,
        config_hash: &str,
        rebuild: bool,
        write: impl FnOnce(&mut ReconciliationWriter<'_, '_>) -> Result<T>,
    ) -> Result<(u64, T)> {
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

        if rebuild {
            tx.execute("DELETE FROM files", [])?;
            tx.execute("DELETE FROM path_entries", [])?;
        }

        let next_generation = current_generation.saturating_add(1);
        let mut writer = ReconciliationWriter {
            transaction: &tx,
            generation: next_generation,
            rebuild,
            replacements: 0,
            deletions: HashSet::new(),
        };
        let output = write(&mut writer)?;
        let changed = rebuild
            || writer.replacements > 0
            || !writer.deletions.is_empty()
            || current_config != config_hash;
        if !rebuild && !writer.deletions.is_empty() {
            Self::remove_orphan_path_entries(&tx)?;
        }
        drop(writer);

        if !changed {
            tx.commit()?;
            return Ok((i64_to_u64(current_generation), output));
        }
        tx.execute(
            "UPDATE meta SET config_hash = ?1, repository_generation = ?2, index_version = index_version + 1 WHERE id = 1",
            params![config_hash, next_generation],
        )?;
        tx.commit()?;
        Ok((i64_to_u64(next_generation), output))
    }

    fn insert_file(
        tx: &Transaction,
        file: &IndexedFile,
        generation: i64,
        source_tokens: Option<(&str, usize)>,
    ) -> Result<()> {
        let (source_tokenizer, source_token_count) = source_tokens.unwrap_or(("", 0));
        tx.execute(
            "INSERT INTO files(path, language, structurally_complete, size_bytes, modified_ns, content_hash, generation, source_token_count, source_tokenizer) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                &file.path,
                file.language.as_deref(),
                file.structurally_complete,
                u64_to_i64(file.size_bytes)?,
                file.modified_ns.map(u128_to_i64).transpose()?,
                &file.content_hash,
                generation,
                usize_to_i64(source_token_count)?,
                source_tokenizer,
            ],
        )?;
        let file_id = tx.last_insert_rowid();
        Self::insert_path_projection(tx, &file.path, file_id)?;

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
            let import_id = tx.last_insert_rowid();
            for (priority, candidate_path) in import.candidate_paths.iter().enumerate() {
                tx.execute(
                    "INSERT INTO import_candidates(import_id, candidate_path, priority) VALUES (?1, ?2, ?3)",
                    params![import_id, candidate_path, usize_to_i64(priority)?],
                )?;
            }
        }

        Ok(())
    }

    fn insert_path_projection(tx: &Transaction, path: &str, file_id: i64) -> Result<()> {
        let parts = path.split('/').collect::<Vec<_>>();
        for index in 1..parts.len() {
            let directory = parts[..index].join("/");
            tx.execute(
                "INSERT OR IGNORE INTO path_entries(path, depth, kind, file_id) VALUES (?1, ?2, 0, NULL)",
                params![directory, usize_to_i64(index)?],
            )?;
        }
        tx.execute(
            "INSERT OR REPLACE INTO path_entries(path, depth, kind, file_id) VALUES (?1, ?2, 1, ?3)",
            params![path, usize_to_i64(parts.len())?, file_id],
        )?;
        Ok(())
    }

    fn remove_orphan_path_entries(tx: &Transaction) -> Result<()> {
        tx.execute(
            "DELETE FROM path_entries
             WHERE kind = 0
               AND NOT EXISTS (
                   SELECT 1 FROM files
                   WHERE substr(files.path, 1, length(path_entries.path) + 1)
                         = path_entries.path || '/'
               )",
            [],
        )?;
        Ok(())
    }

    /// Return files in increasing row-id order after an optional keyset cursor.
    ///
    /// The returned record's `id` is the cursor for the next page. Callers that
    /// require a consistent multi-page view must use [`ReadSession::list_files`]
    /// on one session because file replacement can assign a new row id.
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

    pub(crate) fn affected_importers(&self, candidate_paths: &[String]) -> Result<Vec<String>> {
        self.begin_read()?.affected_importers(candidate_paths)
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

    pub(crate) fn record_token_savings(
        &self,
        tokenizer: &str,
        operation: TokenSavingsOperation,
        baseline_source_tokens: usize,
        emitted_source_tokens: usize,
    ) -> Result<bool> {
        let baseline_source_tokens = usize_to_i64(baseline_source_tokens)?;
        let emitted_source_tokens = usize_to_i64(emitted_source_tokens)?;
        let estimated_source_tokens_saved = baseline_source_tokens
            .saturating_sub(emitted_source_tokens)
            .max(0);
        let conn = match self.writer.try_lock() {
            Ok(conn) => conn,
            Err(std::sync::TryLockError::WouldBlock) => return Ok(false),
            Err(std::sync::TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
        };
        conn.busy_timeout(Duration::ZERO)?;
        let result = conn.execute(
            "INSERT INTO token_savings(
                 tokenizer, operation, tracked_requests, baseline_source_tokens,
                 emitted_source_tokens, estimated_source_tokens_saved
             ) VALUES (?1, ?2, 1, ?3, ?4, ?5)
             ON CONFLICT(tokenizer, operation) DO UPDATE SET
                 tracked_requests = CASE
                     WHEN tracked_requests = 9223372036854775807 THEN tracked_requests
                     ELSE tracked_requests + 1
                 END,
                 baseline_source_tokens = CASE
                     WHEN baseline_source_tokens > 9223372036854775807 - excluded.baseline_source_tokens
                         THEN 9223372036854775807
                     ELSE baseline_source_tokens + excluded.baseline_source_tokens
                 END,
                 emitted_source_tokens = CASE
                     WHEN emitted_source_tokens > 9223372036854775807 - excluded.emitted_source_tokens
                         THEN 9223372036854775807
                     ELSE emitted_source_tokens + excluded.emitted_source_tokens
                 END,
                 estimated_source_tokens_saved = CASE
                     WHEN estimated_source_tokens_saved > 9223372036854775807 - excluded.estimated_source_tokens_saved
                         THEN 9223372036854775807
                     ELSE estimated_source_tokens_saved + excluded.estimated_source_tokens_saved
                 END",
            params![
                tokenizer,
                operation.as_str(),
                baseline_source_tokens,
                emitted_source_tokens,
                estimated_source_tokens_saved,
            ],
        );
        let restore_timeout = conn.busy_timeout(DEFAULT_BUSY_TIMEOUT);
        match result {
            Ok(_) => {
                restore_timeout?;
                Ok(true)
            }
            Err(rusqlite::Error::SqliteFailure(error, _))
                if matches!(
                    error.code,
                    rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked
                ) =>
            {
                restore_timeout?;
                Ok(false)
            }
            Err(error) => {
                restore_timeout?;
                Err(error.into())
            }
        }
    }

    pub(crate) fn token_savings(
        &self,
        tokenizer: &str,
    ) -> Result<HashMap<String, TokenSavingsRecord>> {
        self.begin_read()?.token_savings(tokenizer)
    }

    /// Open a read-only connection and begin a DEFERRED transaction so callers
    /// observe one WAL snapshot until the session is dropped.
    ///
    /// Keep one session for every multi-query response. Dropping it rolls back
    /// the read transaction and returns the connection to the bounded pool.
    pub fn begin_read(&self) -> Result<ReadSession> {
        let conn = self.readers.get()?;
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
            generation: i64_to_u64(row.get(9)?),
            score: row.get::<_, f64>(10)?,
        })
    }
}

fn unix_seconds(time: SystemTime) -> i64 {
    time.duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_secs()).ok())
        .unwrap_or(0)
}

impl ReadSession {
    /// Read metadata from this session's pinned repository snapshot.
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

    /// Return the repository generation pinned by this session.
    pub fn repository_generation(&self) -> Result<u64> {
        let generation: i64 = self.conn.query_row(
            "SELECT repository_generation FROM meta WHERE id = 1",
            [],
            |row| row.get(0),
        )?;
        Ok(i64_to_u64(generation))
    }

    pub(crate) fn whole_file_source_tokens(
        &self,
        paths: &[String],
        tokenizer: &str,
    ) -> Result<Option<usize>> {
        if paths.is_empty() {
            return Ok(Some(0));
        }
        let input = serde_json::to_string(paths)?;
        let tokens: Option<i64> = self.conn.query_row(
            "WITH requested(path) AS (
                 SELECT DISTINCT CAST(value AS TEXT) FROM json_each(?1)
             )
             SELECT CASE
                 WHEN COUNT(*) = SUM(files.source_tokenizer = ?2)
                     THEN COALESCE(SUM(files.source_token_count), 0)
                 ELSE NULL
             END
             FROM requested
             JOIN files ON files.path = requested.path",
            params![input, tokenizer],
            |row| row.get(0),
        )?;
        Ok(tokens.map(i64_to_usize))
    }

    pub(crate) fn token_savings(
        &self,
        tokenizer: &str,
    ) -> Result<HashMap<String, TokenSavingsRecord>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT operation, tracked_requests, baseline_source_tokens,
                    emitted_source_tokens, estimated_source_tokens_saved
             FROM token_savings
             WHERE tokenizer = ?1
             ORDER BY operation",
        )?;
        let rows = stmt.query_map(params![tokenizer], |row| {
            Ok((
                row.get::<_, String>(0)?,
                TokenSavingsRecord {
                    tracked_requests: i64_to_u64(row.get(1)?),
                    baseline_source_tokens: i64_to_u64(row.get(2)?),
                    emitted_source_tokens: i64_to_u64(row.get(3)?),
                    estimated_source_tokens_saved: i64_to_u64(row.get(4)?),
                },
            ))
        })?;
        Ok(rows.collect::<std::result::Result<HashMap<_, _>, _>>()?)
    }

    /// Return a row-id keyset page from this session's pinned snapshot.
    ///
    /// Use the final record's `id` as the next cursor. Cursors are storage-layer
    /// values and should not be exposed as service cursors without binding them
    /// to the repository generation and operation parameters.
    pub fn list_files(&self, max_results: usize, cursor: Option<i64>) -> Result<Vec<FileRecord>> {
        let limit = bounded_limit(max_results);
        let mut stmt = self.conn.prepare_cached(
            "SELECT id, path, language, size_bytes, modified_ns, content_hash, generation, structurally_complete FROM files WHERE (?1 IS NULL OR id > ?1) ORDER BY id LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![cursor, limit], Storage::map_file)?;
        let mut files = Vec::new();
        for row in rows {
            files.push(row?);
        }
        Ok(files)
    }

    /// Read a lexicographically ordered keyset page from the relational path projection.
    pub(crate) fn list_tree_paths(
        &self,
        root: &str,
        max_depth: usize,
        after: Option<&str>,
        max_results: usize,
    ) -> Result<Vec<PathRecord>> {
        let root_depth = root.split('/').filter(|part| !part.is_empty()).count();
        let depth_limit = i64::try_from(root_depth.saturating_add(max_depth)).unwrap_or(i64::MAX);
        let mut stmt = self.conn.prepare_cached(
            "SELECT path_entries.path, path_entries.kind, files.language, files.size_bytes
             FROM path_entries
             LEFT JOIN files ON files.id = path_entries.file_id
             WHERE (?1 = '' OR path_entries.path = ?1
                    OR substr(path_entries.path, 1, length(?1) + 1) = ?1 || '/')
               AND path_entries.depth <= ?2
               AND (?3 IS NULL OR path_entries.path > ?3)
             ORDER BY path_entries.path
             LIMIT ?4",
        )?;
        let rows = stmt.query_map(
            params![root, depth_limit, after, bounded_limit(max_results)],
            |row| {
                let kind: i64 = row.get(1)?;
                Ok(PathRecord {
                    path: row.get(0)?,
                    is_directory: kind == 0,
                    language: row.get(2)?,
                    size_bytes: row.get::<_, Option<i64>>(3)?.map(i64_to_u64),
                })
            },
        )?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    pub fn find_file(&self, path: &str) -> Result<Option<FileRecord>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT id, path, language, size_bytes, modified_ns, content_hash, generation, structurally_complete FROM files WHERE path = ?1",
        )?;
        let mut rows = stmt.query_map(params![path], Storage::map_file)?;
        Ok(rows.next().transpose()?)
    }

    /// Find importers whose persisted candidate set intersects changed repository paths.
    pub(crate) fn affected_importers(&self, candidate_paths: &[String]) -> Result<Vec<String>> {
        if candidate_paths.is_empty() {
            return Ok(Vec::new());
        }
        let input = serde_json::to_string(candidate_paths)?;
        let mut stmt = self.conn.prepare_cached(
            "SELECT DISTINCT files.path
             FROM json_each(?1) AS changed
             JOIN import_candidates ON import_candidates.candidate_path = changed.value
             JOIN imports ON imports.id = import_candidates.import_id
             JOIN files ON files.id = imports.file_id
             ORDER BY files.path",
        )?;
        let rows = stmt.query_map(params![input], |row| row.get(0))?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// Batch import expansion for context ranking, bounded independently per seed.
    ///
    /// `seed_index` in each result maps back to the original input order. SQL
    /// performs the joins and per-seed limits; ranking decides which evidence to use.
    pub(crate) fn import_symbol_targets(
        &self,
        seed_paths: &[String],
        max_imports_per_seed: usize,
        max_symbols_per_target: usize,
    ) -> Result<Vec<ImportSymbolTarget>> {
        if seed_paths.is_empty() {
            return Ok(Vec::new());
        }
        let input = serde_json::to_string(seed_paths)?;
        let mut stmt = self.conn.prepare_cached(
            "WITH requested AS (
                 SELECT CAST(key AS INTEGER) AS seed_index, value AS seed_path
                 FROM json_each(?1)
             ), ranked_imports AS (
                 SELECT requested.seed_index,
                        imports.id AS import_id,
                        imports.resolved_path,
                        ROW_NUMBER() OVER (
                            PARTITION BY requested.seed_index
                            ORDER BY imports.line, imports.id
                        ) AS import_rank
                 FROM requested
                 JOIN files AS seed ON seed.path = requested.seed_path
                 JOIN imports ON imports.file_id = seed.id
                 WHERE imports.resolved_path IS NOT NULL
             )
             SELECT ranked_imports.seed_index, ranked_imports.import_rank,
                    target.id, target.path, target.language, target.size_bytes,
                    target.modified_ns, target.content_hash, target.generation,
                    target.structurally_complete,
                    symbols.id, symbols.file_id, symbols.name, symbols.kind,
                    symbols.parent, symbols.signature,
                    symbols.start_line, symbols.end_line,
                    symbols.start_byte, symbols.end_byte
             FROM ranked_imports
             JOIN files AS target ON target.path = ranked_imports.resolved_path
             JOIN symbols ON symbols.file_id = target.id
                         AND symbols.id IN (
                             SELECT limited.id
                             FROM symbols AS limited
                             WHERE limited.file_id = target.id
                             ORDER BY limited.start_byte
                             LIMIT ?3
                         )
             WHERE ranked_imports.import_rank <= ?2
             ORDER BY ranked_imports.seed_index, ranked_imports.import_rank, symbols.start_byte",
        )?;
        let rows = stmt.query_map(
            params![
                input,
                bounded_limit(max_imports_per_seed),
                bounded_limit(max_symbols_per_target)
            ],
            |row| {
                let seed_index = i64_to_usize(row.get(0)?);
                let import_rank: i64 = row.get(1)?;
                let target_file = FileRecord {
                    id: row.get(2)?,
                    path: row.get(3)?,
                    language: row.get(4)?,
                    size_bytes: i64_to_u64(row.get(5)?),
                    modified_ns: row.get::<_, Option<i64>>(6)?.map(i64_to_u128),
                    content_hash: row.get(7)?,
                    generation: i64_to_u64(row.get(8)?),
                    structurally_complete: row.get(9)?,
                };
                let symbol = SymbolRecord {
                    id: row.get(10)?,
                    file_id: row.get(11)?,
                    name: row.get(12)?,
                    kind: row.get(13)?,
                    parent: row.get(14)?,
                    signature: row.get(15)?,
                    start_line: i64_to_usize(row.get(16)?),
                    end_line: i64_to_usize(row.get(17)?),
                    start_byte: i64_to_usize(row.get(18)?),
                    end_byte: i64_to_usize(row.get(19)?),
                };
                Ok((seed_index, import_rank, target_file, symbol))
            },
        )?;
        let mut grouped = Vec::<ImportSymbolTarget>::new();
        let mut current_key = None;
        for row in rows {
            let (seed_index, import_rank, target_file, symbol) = row?;
            let key = (seed_index, import_rank);
            if current_key != Some(key) {
                grouped.push(ImportSymbolTarget {
                    seed_index,
                    target_file,
                    symbols: Vec::new(),
                });
                current_key = Some(key);
            }
            if let Some(target) = grouped.last_mut() {
                target.symbols.push(symbol);
            }
        }
        Ok(grouped)
    }

    pub fn get_chunks_for_file(
        &self,
        file_id: i64,
        max_results: usize,
    ) -> Result<Vec<ChunkRecord>> {
        let limit = bounded_limit(max_results);
        let mut stmt = self.conn.prepare_cached(
            "SELECT id, file_id, content, start_line, end_line, start_byte, end_byte, token_count FROM chunks WHERE file_id = ?1 ORDER BY start_byte LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![file_id, limit], Storage::map_chunk)?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// Return the final indexed line for each file id, preserving request order.
    pub(crate) fn file_end_lines_batch(&self, file_ids: &[i64]) -> Result<Vec<Option<usize>>> {
        if file_ids.is_empty() {
            return Ok(Vec::new());
        }
        let mut seen = HashSet::new();
        let unique_file_ids = file_ids
            .iter()
            .copied()
            .filter(|file_id| seen.insert(*file_id))
            .collect::<Vec<_>>();
        let input = serde_json::to_string(&unique_file_ids)?;
        let mut stmt = self.conn.prepare_cached(
            "WITH requested AS (
                 SELECT CAST(value AS INTEGER) AS file_id
                 FROM json_each(?1)
             )
             SELECT requested.file_id, MAX(chunks.end_line)
             FROM requested
             LEFT JOIN chunks ON chunks.file_id = requested.file_id
             GROUP BY requested.file_id",
        )?;
        let rows = stmt.query_map(params![input], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, Option<i64>>(1)?.map(i64_to_usize),
            ))
        })?;
        let end_lines = rows.collect::<std::result::Result<HashMap<_, _>, _>>()?;
        Ok(file_ids
            .iter()
            .map(|file_id| end_lines.get(file_id).copied().flatten())
            .collect())
    }

    /// Hydrate overlapping chunks for every requested range in one SQL query.
    ///
    /// The outer vector is aligned one-for-one with `ranges`, including duplicate
    /// requests and ranges with no matches. Each inner vector is ordered by line.
    pub(crate) fn get_chunks_overlapping_batch(
        &self,
        ranges: &[(i64, usize, usize)],
    ) -> Result<Vec<Vec<ChunkRecord>>> {
        if ranges.is_empty() {
            return Ok(Vec::new());
        }
        let input = ranges
            .iter()
            .map(|(file_id, start_line, end_line)| {
                serde_json::json!({
                    "file_id": file_id,
                    "start_line": start_line,
                    "end_line": end_line,
                })
            })
            .collect::<Vec<_>>();
        let input = serde_json::to_string(&input)?;
        let mut stmt = self.conn.prepare_cached(
            "WITH requested AS (
                 SELECT CAST(key AS INTEGER) AS request_index,
                        CAST(value ->> 'file_id' AS INTEGER) AS file_id,
                        CAST(value ->> 'start_line' AS INTEGER) AS start_line,
                        CAST(value ->> 'end_line' AS INTEGER) AS end_line
                 FROM json_each(?1)
             )
             SELECT requested.request_index,
                    chunks.id, chunks.file_id, chunks.content,
                    chunks.start_line, chunks.end_line,
                    chunks.start_byte, chunks.end_byte, chunks.token_count
             FROM requested
             JOIN chunks
               ON chunks.file_id = requested.file_id
              AND chunks.end_line >= requested.start_line
              AND chunks.start_line <= requested.end_line
             ORDER BY requested.request_index, chunks.start_line",
        )?;
        let rows = stmt.query_map(params![input], |row| {
            let request_index = i64_to_usize(row.get(0)?);
            let chunk = ChunkRecord {
                id: row.get(1)?,
                file_id: row.get(2)?,
                content: row.get(3)?,
                start_line: i64_to_usize(row.get(4)?),
                end_line: i64_to_usize(row.get(5)?),
                start_byte: i64_to_usize(row.get(6)?),
                end_byte: i64_to_usize(row.get(7)?),
                token_count: i64_to_usize(row.get(8)?),
            };
            Ok((request_index, chunk))
        })?;
        let mut grouped = vec![Vec::new(); ranges.len()];
        for row in rows {
            let (request_index, chunk) = row?;
            if let Some(chunks) = grouped.get_mut(request_index) {
                chunks.push(chunk);
            }
        }
        Ok(grouped)
    }

    pub fn get_symbols_for_file(
        &self,
        file_id: i64,
        max_results: usize,
    ) -> Result<Vec<SymbolRecord>> {
        let limit = bounded_limit(max_results);
        let mut stmt = self.conn.prepare_cached(
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
        let mut stmt = self.conn.prepare_cached(
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
                     WHERE file_id = ?1
                       AND (name = ?2 OR (parent IS NOT NULL AND parent || '.' || name = ?2))
                     ORDER BY CASE WHEN name = ?2 THEN 0 ELSE 1 END, start_byte
                     LIMIT 1",
                params![file_id, name],
                Storage::map_symbol,
            )
            .optional()?)
    }

    #[cfg(test)]
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

    /// Find the narrowest enclosing symbol for every requested file/line pair.
    ///
    /// Results preserve input order and cardinality; `None` marks a location with
    /// no enclosing declaration. Duplicate locations remain duplicate outputs.
    pub(crate) fn find_enclosing_symbols_batch(
        &self,
        locations: &[(i64, usize)],
    ) -> Result<Vec<Option<SymbolRecord>>> {
        if locations.is_empty() {
            return Ok(Vec::new());
        }
        let input = locations
            .iter()
            .map(|(file_id, line)| serde_json::json!({ "file_id": file_id, "line": line }))
            .collect::<Vec<_>>();
        let input = serde_json::to_string(&input)?;
        let mut stmt = self.conn.prepare_cached(
            "WITH requested AS (
                 SELECT CAST(key AS INTEGER) AS request_index,
                        CAST(value ->> 'file_id' AS INTEGER) AS file_id,
                        CAST(value ->> 'line' AS INTEGER) AS line
                 FROM json_each(?1)
             )
             SELECT requested.request_index,
                    symbols.id, symbols.file_id, symbols.name, symbols.kind,
                    symbols.parent, symbols.signature,
                    symbols.start_line, symbols.end_line,
                    symbols.start_byte, symbols.end_byte
             FROM requested
             JOIN symbols ON symbols.id = (
                 SELECT enclosing.id
                 FROM symbols AS enclosing
                 WHERE enclosing.file_id = requested.file_id
                   AND enclosing.start_line <= requested.line
                   AND enclosing.end_line >= requested.line
                 ORDER BY (enclosing.end_line - enclosing.start_line), enclosing.start_byte
                 LIMIT 1
             )
             ORDER BY requested.request_index",
        )?;
        let rows = stmt.query_map(params![input], |row| {
            let request_index = i64_to_usize(row.get(0)?);
            let symbol = SymbolRecord {
                id: row.get(1)?,
                file_id: row.get(2)?,
                name: row.get(3)?,
                kind: row.get(4)?,
                parent: row.get(5)?,
                signature: row.get(6)?,
                start_line: i64_to_usize(row.get(7)?),
                end_line: i64_to_usize(row.get(8)?),
                start_byte: i64_to_usize(row.get(9)?),
                end_byte: i64_to_usize(row.get(10)?),
            };
            Ok((request_index, symbol))
        })?;
        let mut symbols = vec![None; locations.len()];
        for row in rows {
            let (request_index, symbol) = row?;
            if let Some(slot) = symbols.get_mut(request_index) {
                *slot = Some(symbol);
            }
        }
        Ok(symbols)
    }

    pub fn get_references_for_file(
        &self,
        file_id: i64,
        max_results: usize,
    ) -> Result<Vec<ReferenceRecord>> {
        let limit = bounded_limit(max_results);
        let mut stmt = self.conn.prepare_cached(
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
        let mut stmt = self.conn.prepare_cached(
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
        let mut stmt = self.conn.prepare_cached(
            "SELECT f.path, f.content_hash, f.generation, s.id, s.file_id, s.name, s.kind, s.parent, s.signature, s.start_line, s.end_line, s.start_byte, s.end_byte
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
                generation: i64_to_u64(row.get(2)?),
                symbol: SymbolRecord {
                    id: row.get(3)?,
                    file_id: row.get(4)?,
                    name: row.get(5)?,
                    kind: row.get(6)?,
                    parent: row.get(7)?,
                    signature: row.get(8)?,
                    start_line: i64_to_usize(row.get(9)?),
                    end_line: i64_to_usize(row.get(10)?),
                    start_byte: i64_to_usize(row.get(11)?),
                    end_byte: i64_to_usize(row.get(12)?),
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
        let mut stmt = self.conn.prepare_cached(
            "SELECT f.path, f.content_hash, f.generation, r.id, r.file_id, r.name, r.kind, r.role, r.enclosing_symbol, r.start_line, r.end_line, r.start_byte, r.end_byte
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
                generation: i64_to_u64(row.get(2)?),
                reference: ReferenceRecord {
                    id: row.get(3)?,
                    file_id: row.get(4)?,
                    name: row.get(5)?,
                    kind: row.get(6)?,
                    role: role_from_str(&row.get::<_, String>(7)?),
                    enclosing_symbol: row.get(8)?,
                    start_line: i64_to_usize(row.get(9)?),
                    end_line: i64_to_usize(row.get(10)?),
                    start_byte: i64_to_usize(row.get(11)?),
                    end_byte: i64_to_usize(row.get(12)?),
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
        let mut stmt = self.conn.prepare_cached(
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
            "SELECT c.id, c.file_id, f.path, c.content, c.start_line, c.end_line, c.start_byte, c.end_byte, c.token_count, f.generation, bm25({table_name}) as score \
             FROM {table_name} \
             JOIN chunks c ON {table_name}.rowid = c.rowid \
             JOIN files f ON c.file_id = f.id \
             WHERE {table_name} MATCH ?1 \
             ORDER BY bm25({table_name}), f.path, c.start_byte \
             LIMIT ?2"
        );
        let mut stmt = self.conn.prepare_cached(&sql)?;
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

fn table_exists(conn: &Connection, table: &str) -> Result<bool> {
    Ok(conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1)",
        [table],
        |row| row.get(0),
    )?)
}

fn column_exists(conn: &Connection, table: &str, column: &str) -> Result<bool> {
    let sql = format!("SELECT EXISTS(SELECT 1 FROM pragma_table_info('{table}') WHERE name = ?1)");
    Ok(conn.query_row(&sql, [column], |row| row.get(0))?)
}

fn count_table_rows(conn: &Connection, table: &str) -> Result<usize> {
    if !table_exists(conn, table)? {
        return Ok(0);
    }
    let sql = format!("SELECT count(*) FROM {table}");
    Ok(i64_to_usize(conn.query_row(&sql, [], |row| row.get(0))?))
}

fn repository_identity(path: &Path) -> String {
    let mut hasher = blake3::Hasher::new();
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        hasher.update(path.as_os_str().as_bytes());
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        for unit in path.as_os_str().encode_wide() {
            hasher.update(&unit.to_le_bytes());
        }
    }
    #[cfg(not(any(unix, windows)))]
    hasher.update(path.to_string_lossy().as_bytes());
    hasher.finalize().to_hex().to_string()
}

fn u64_to_i64(value: u64) -> Result<i64> {
    i64::try_from(value)
        .map_err(|_| Error::InternalFailure("value exceeds storage integer range".into()))
}

fn usize_to_i64(value: usize) -> Result<i64> {
    i64::try_from(value)
        .map_err(|_| Error::InternalFailure("value exceeds storage integer range".into()))
}

fn u128_to_i64(value: u128) -> Result<i64> {
    i64::try_from(value)
        .map_err(|_| Error::InternalFailure("value exceeds storage integer range".into()))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writer_bounds_recycled_wal_size() {
        let root = tempfile::tempdir().expect("root");
        let storage = Storage::open(root.path().join("index.sqlite")).expect("storage");
        let connection = storage
            .writer
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let limit: i64 = connection
            .query_row("PRAGMA journal_size_limit", [], |row| row.get(0))
            .expect("journal size limit");
        assert_eq!(limit, WAL_JOURNAL_SIZE_LIMIT_BYTES);
    }

    fn sample_file(path: &str, content: &str) -> IndexedFile {
        IndexedFile {
            path: path.to_string(),
            language: Some("rust".into()),
            structurally_complete: true,
            size_bytes: u64::try_from(content.len()).expect("content length"),
            modified_ns: None,
            content_hash: crate::text::hash_bytes(content.as_bytes()),
            chunks: vec![ChunkInput {
                content: content.to_string(),
                start_line: 1,
                end_line: 1,
                start_byte: 0,
                end_byte: content.len(),
                token_count: 1,
            }],
            symbols: Vec::new(),
            references: Vec::new(),
            imports: Vec::new(),
        }
    }

    #[test]
    fn file_end_line_batch_maps_duplicate_and_missing_file_ids() {
        let directory = tempfile::tempdir().expect("directory");
        let storage = Storage::open(directory.path().join("index.sqlite")).expect("storage");
        storage
            .full_reconcile("config", vec![sample_file("source.rs", "fn source() {}\n")])
            .expect("index source");
        let session = storage.begin_read().expect("read session");
        let file_id = session
            .find_file("source.rs")
            .expect("find source")
            .expect("indexed source")
            .id;

        assert_eq!(
            session
                .file_end_lines_batch(&[file_id, file_id, i64::MAX, file_id])
                .expect("end lines"),
            vec![Some(1), Some(1), None, Some(1)]
        );
    }

    #[test]
    fn streamed_cancellation_rolls_back_every_insert_and_generation() {
        let directory = tempfile::tempdir().expect("directory");
        let database = directory.path().join("index.sqlite");
        let storage = Storage::open(&database).expect("storage");
        storage
            .full_reconcile("config", vec![sample_file("old.rs", "fn old() {}\n")])
            .expect("initial generation");
        let baseline = storage.meta().expect("baseline");

        let error = storage
            .publish_reconciliation_at(&baseline, "config", true, |writer| -> Result<()> {
                writer.replace(sample_file("first.rs", "fn first() {}\n"))?;
                Err(Error::Cancelled)
            })
            .expect_err("later batch failure");

        assert!(matches!(error, Error::Cancelled));
        drop(storage);
        let reopened = Storage::open(&database).expect("reopen after rollback");
        assert_eq!(reopened.repository_generation().expect("generation"), 1);
        assert!(reopened.find_file("old.rs").expect("old lookup").is_some());
        assert!(
            reopened
                .find_file("first.rs")
                .expect("first lookup")
                .is_none()
        );
    }

    #[test]
    fn later_streamed_storage_failure_rolls_back_earlier_files() {
        let directory = tempfile::tempdir().expect("directory");
        let database = directory.path().join("index.sqlite");
        let storage = Storage::open(&database).expect("storage");
        storage
            .full_reconcile("config", vec![sample_file("old.rs", "fn old() {}\n")])
            .expect("initial generation");
        let baseline = storage.meta().expect("baseline");
        let mut invalid = sample_file("invalid.rs", "fn invalid() {}\n");
        invalid.chunks[0].end_line = usize::MAX;

        storage
            .publish_reconciliation_at(&baseline, "config", true, |writer| {
                writer.replace(sample_file("first.rs", "fn first() {}\n"))?;
                writer.replace(invalid)
            })
            .expect_err("second insert must fail");

        drop(storage);
        let reopened = Storage::open(&database).expect("reopen after rollback");
        assert_eq!(reopened.repository_generation().expect("generation"), 1);
        assert!(reopened.find_file("old.rs").expect("old lookup").is_some());
        assert!(
            reopened
                .find_file("first.rs")
                .expect("first lookup")
                .is_none()
        );
        assert!(
            reopened
                .find_file("invalid.rs")
                .expect("invalid lookup")
                .is_none()
        );
    }

    #[test]
    fn streamed_panic_rolls_back_and_leaves_storage_reusable() {
        let directory = tempfile::tempdir().expect("directory");
        let storage = Storage::open(directory.path().join("index.sqlite")).expect("storage");
        storage
            .full_reconcile("config", vec![sample_file("old.rs", "fn old() {}\n")])
            .expect("initial generation");
        let baseline = storage.meta().expect("baseline");

        let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = storage.publish_reconciliation_at(
                &baseline,
                "config",
                true,
                |writer| -> Result<()> {
                    writer.replace(sample_file("new.rs", "fn new() {}\n"))?;
                    panic!("injected batch panic");
                },
            );
        }));

        assert!(panic.is_err());
        assert_eq!(storage.repository_generation().expect("generation"), 1);
        assert!(storage.find_file("old.rs").expect("old lookup").is_some());
        assert!(storage.find_file("new.rs").expect("new lookup").is_none());
        assert_eq!(
            storage
                .reconcile_files(
                    "config",
                    vec![sample_file("after.rs", "fn after() {}\n")],
                    &[],
                )
                .expect("writer remains usable"),
            2
        );
    }

    #[test]
    fn readers_see_old_generation_until_streamed_publication_commits() {
        let directory = tempfile::tempdir().expect("directory");
        let storage = Storage::open(directory.path().join("index.sqlite")).expect("storage");
        storage
            .full_reconcile("config", vec![sample_file("old.rs", "fn old() {}\n")])
            .expect("initial generation");
        let baseline = storage.meta().expect("baseline");

        let (generation, ()) = storage
            .publish_reconciliation_at(&baseline, "config", true, |writer| {
                writer.replace(sample_file("new.rs", "fn new() {}\n"))?;
                let reader = storage.begin_read()?;
                assert_eq!(reader.repository_generation()?, 1);
                assert!(reader.find_file("old.rs")?.is_some());
                assert!(reader.find_file("new.rs")?.is_none());
                Ok(())
            })
            .expect("publish");

        assert_eq!(generation, 2);
        assert!(storage.find_file("old.rs").expect("old lookup").is_none());
        assert!(storage.find_file("new.rs").expect("new lookup").is_some());
    }

    #[test]
    fn stale_streaming_baseline_fails_before_invoking_the_writer() {
        let directory = tempfile::tempdir().expect("directory");
        let storage = Storage::open(directory.path().join("index.sqlite")).expect("storage");
        let stale = storage.meta().expect("stale baseline");
        storage
            .full_reconcile(
                "config",
                vec![sample_file("current.rs", "fn current() {}\n")],
            )
            .expect("current generation");
        let mut invoked = false;

        let error = storage
            .publish_reconciliation_at(&stale, "config", false, |_| {
                invoked = true;
                Ok(())
            })
            .expect_err("stale publication");

        assert!(matches!(error, Error::StaleReconciliation { .. }));
        assert!(!invoked);
    }

    #[test]
    fn repository_binding_updates_last_access_once_per_open() {
        let directory = tempfile::tempdir().expect("directory");
        let repository = directory.path().join("repository");
        fs::create_dir(&repository).expect("repository");
        let database = directory.path().join("index.sqlite");
        let storage = Storage::open(&database).expect("storage");

        storage
            .bind_repository_at(&repository, 1_234)
            .expect("initial binding");
        let connection = Connection::open(&database).expect("inspect binding");
        assert_eq!(
            connection
                .query_row(
                    "SELECT last_access_unix_seconds FROM meta WHERE id = 1",
                    [],
                    |row| row.get::<_, i64>(0)
                )
                .expect("first access"),
            1_234
        );

        storage
            .bind_repository_at(&repository, 5_678)
            .expect("repeat binding");
        assert_eq!(
            connection
                .query_row(
                    "SELECT last_access_unix_seconds FROM meta WHERE id = 1",
                    [],
                    |row| row.get::<_, i64>(0)
                )
                .expect("second access"),
            5_678
        );
    }

    #[test]
    fn token_savings_accounting_skips_a_busy_local_writer() {
        let directory = tempfile::tempdir().expect("directory");
        let storage = Storage::open(directory.path().join("index.sqlite")).expect("storage");
        let writer = storage
            .writer
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());

        assert!(
            !storage
                .record_token_savings("cl100k_base", TokenSavingsOperation::Search, 10, 2)
                .expect("best-effort accounting")
        );
        drop(writer);
        assert!(
            storage
                .record_token_savings("cl100k_base", TokenSavingsOperation::Search, 10, 2)
                .expect("available accounting")
        );
    }

    #[test]
    fn whole_file_source_tokens_uses_the_exact_indexed_file_count() {
        let directory = tempfile::tempdir().expect("directory");
        let storage = Storage::open(directory.path().join("index.sqlite")).expect("storage");
        let mut file = sample_file("source.rs", "hello\n\n");
        file.chunks = vec![
            ChunkInput {
                content: "hello\n".into(),
                start_line: 1,
                end_line: 1,
                start_byte: 0,
                end_byte: 6,
                token_count: 2,
            },
            ChunkInput {
                content: "\n".into(),
                start_line: 2,
                end_line: 2,
                start_byte: 6,
                end_byte: 7,
                token_count: 1,
            },
        ];
        let baseline = storage.meta().expect("baseline");
        storage
            .publish_reconciliation_at(&baseline, "config", false, |writer| {
                writer.replace_with_source_tokens(file, "cl100k_base", 2)
            })
            .expect("indexed file");

        assert_eq!(
            storage
                .begin_read()
                .expect("read session")
                .whole_file_source_tokens(&["source.rs".into()], "cl100k_base")
                .expect("whole-file tokens"),
            Some(2)
        );
        assert_eq!(
            storage
                .begin_read()
                .expect("read session")
                .whole_file_source_tokens(&["source.rs".into()], "o200k_base")
                .expect("mismatched tokenizer"),
            None
        );
    }
}
