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

const STRUCTURAL_SEARCH_SQL: &str = r#"
CREATE VIRTUAL TABLE IF NOT EXISTS symbols_fts_trigram USING fts5(
    name,
    content='symbols',
    content_rowid='rowid',
    tokenize='trigram'
);

CREATE VIRTUAL TABLE IF NOT EXISTS symbol_refs_fts_trigram USING fts5(
    name,
    content='symbol_refs',
    content_rowid='rowid',
    tokenize='trigram'
);

CREATE TRIGGER IF NOT EXISTS symbols_ai_trigram
AFTER INSERT ON symbols
BEGIN
    INSERT INTO symbols_fts_trigram(rowid, name) VALUES (new.rowid, new.name);
END;

CREATE TRIGGER IF NOT EXISTS symbols_ad_trigram
AFTER DELETE ON symbols
BEGIN
    INSERT INTO symbols_fts_trigram(symbols_fts_trigram, rowid, name)
    VALUES ('delete', old.rowid, old.name);
END;

CREATE TRIGGER IF NOT EXISTS symbols_au_trigram
AFTER UPDATE ON symbols
BEGIN
    INSERT INTO symbols_fts_trigram(symbols_fts_trigram, rowid, name)
    VALUES ('delete', old.rowid, old.name);
    INSERT INTO symbols_fts_trigram(rowid, name) VALUES (new.rowid, new.name);
END;

CREATE TRIGGER IF NOT EXISTS symbol_refs_ai_trigram
AFTER INSERT ON symbol_refs
BEGIN
    INSERT INTO symbol_refs_fts_trigram(rowid, name) VALUES (new.rowid, new.name);
END;

CREATE TRIGGER IF NOT EXISTS symbol_refs_ad_trigram
AFTER DELETE ON symbol_refs
BEGIN
    INSERT INTO symbol_refs_fts_trigram(symbol_refs_fts_trigram, rowid, name)
    VALUES ('delete', old.rowid, old.name);
END;

CREATE TRIGGER IF NOT EXISTS symbol_refs_au_trigram
AFTER UPDATE ON symbol_refs
BEGIN
    INSERT INTO symbol_refs_fts_trigram(symbol_refs_fts_trigram, rowid, name)
    VALUES ('delete', old.rowid, old.name);
    INSERT INTO symbol_refs_fts_trigram(rowid, name) VALUES (new.rowid, new.name);
END;

INSERT INTO symbols_fts_trigram(symbols_fts_trigram) VALUES('rebuild');
INSERT INTO symbol_refs_fts_trigram(symbol_refs_fts_trigram) VALUES('rebuild');
UPDATE meta SET schema_version = 6 WHERE id = 1;
"#;

const TOKEN_SAVINGS_TABLE_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS token_savings (
    tokenizer TEXT NOT NULL,
    operation TEXT NOT NULL,
    tracked_requests INTEGER NOT NULL DEFAULT 0,
    response_tracked_requests INTEGER NOT NULL DEFAULT 0,
    response_baseline_requests INTEGER NOT NULL DEFAULT 0,
    baseline_source_tokens INTEGER NOT NULL DEFAULT 0,
    response_baseline_source_tokens INTEGER NOT NULL DEFAULT 0,
    emitted_source_tokens INTEGER NOT NULL DEFAULT 0,
    estimated_source_tokens_saved INTEGER NOT NULL DEFAULT 0,
    response_source_tokens INTEGER NOT NULL DEFAULT 0,
    path_and_metadata_tokens INTEGER NOT NULL DEFAULT 0,
    protocol_tokens INTEGER NOT NULL DEFAULT 0,
    total_response_tokens INTEGER NOT NULL DEFAULT 0,
    receipt_suppressed_exact INTEGER NOT NULL DEFAULT 0,
    receipt_suppressed_overlap INTEGER NOT NULL DEFAULT 0,
    expected_hash_not_modified_responses INTEGER NOT NULL DEFAULT 0,
    expected_hash_suppressed_source_tokens INTEGER NOT NULL DEFAULT 0,
    useful_requests INTEGER NOT NULL DEFAULT 0,
    incomplete_requests INTEGER NOT NULL DEFAULT 0,
    unsupported_requests INTEGER NOT NULL DEFAULT 0,
    hash_suppressed_requests INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY(tokenizer, operation)
);
"#;

const SERVICE_FAILURES_TABLE_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS service_failures (
    tokenizer TEXT NOT NULL,
    operation TEXT NOT NULL,
    error_category TEXT NOT NULL,
    failed_requests INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY(tokenizer, operation, error_category)
);
"#;

const MIGRATIONS_SLICE: &[M<'_>] = &[
    M::up(SCHEMA_SQL).foreign_key_check(),
    M::up(LOOKUP_INDEXES_SQL),
    M::up(REPOSITORY_OWNERSHIP_SQL),
    M::up(IMPORT_CANDIDATES_SQL),
    M::up(PATH_PROJECTION_SQL),
    M::up(CACHE_ACCESS_SQL),
    M::up(STRUCTURAL_SEARCH_SQL),
];
pub(crate) const CURRENT_MIGRATION_VERSION: i64 = 7;
const _: () = assert!(MIGRATIONS_SLICE.len() == CURRENT_MIGRATION_VERSION as usize);
const MIGRATIONS: Migrations<'_> = Migrations::from_slice(MIGRATIONS_SLICE);
