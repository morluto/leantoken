pub(crate) const SCHEMA_SQL: &str = r#"
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

pub(crate) const LOOKUP_INDEXES_SQL: &str = r#"
CREATE INDEX IF NOT EXISTS chunks_file_line_idx
ON chunks(file_id, start_line, end_line);

CREATE INDEX IF NOT EXISTS symbols_file_start_idx
ON symbols(file_id, start_byte);

CREATE INDEX IF NOT EXISTS symbol_refs_file_start_idx
ON symbol_refs(file_id, start_byte);

CREATE INDEX IF NOT EXISTS imports_file_line_idx
ON imports(file_id, line);
"#;

pub(crate) const REPOSITORY_OWNERSHIP_SQL: &str = r#"
ALTER TABLE meta ADD COLUMN repository_root TEXT NOT NULL DEFAULT '';
ALTER TABLE meta ADD COLUMN repository_identity TEXT NOT NULL DEFAULT '';
UPDATE meta SET schema_version = 2 WHERE id = 1;
"#;

pub(crate) const IMPORT_CANDIDATES_SQL: &str = r#"
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

pub(crate) const PATH_PROJECTION_SQL: &str = r#"
CREATE TABLE path_entries (
    path TEXT PRIMARY KEY,
    depth INTEGER NOT NULL,
    kind INTEGER NOT NULL,
    file_id INTEGER UNIQUE REFERENCES files(id) ON DELETE CASCADE
);
CREATE INDEX path_entries_depth_path_idx ON path_entries(depth, path);
UPDATE meta SET schema_version = 4 WHERE id = 1;
"#;

pub(crate) const CACHE_ACCESS_SQL: &str = r#"
ALTER TABLE meta ADD COLUMN last_access_unix_seconds INTEGER NOT NULL DEFAULT 0;
UPDATE meta
SET last_access_unix_seconds = CAST(strftime('%s', 'now') AS INTEGER),
    schema_version = 5
WHERE id = 1;
"#;

pub(crate) const STRUCTURAL_SEARCH_SQL: &str = r#"
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

pub(crate) const RETRIEVAL_RECEIPTS_SQL: &str = r#"
CREATE TABLE retrieval_receipt_usage (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    namespace TEXT NOT NULL CHECK (length(namespace) = 32),
    next_access_sequence INTEGER NOT NULL DEFAULT 0 CHECK (next_access_sequence >= 0),
    receipt_count INTEGER NOT NULL DEFAULT 0 CHECK (receipt_count >= 0),
    receipt_bytes INTEGER NOT NULL DEFAULT 0 CHECK (receipt_bytes >= 0),
    evidence_count INTEGER NOT NULL DEFAULT 0 CHECK (evidence_count >= 0),
    evidence_bytes INTEGER NOT NULL DEFAULT 0 CHECK (evidence_bytes >= 0)
);

INSERT INTO retrieval_receipt_usage(id, namespace)
VALUES (1, lower(hex(randomblob(16))));

CREATE TABLE retrieval_receipts (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    repository_identity TEXT NOT NULL,
    repository_generation INTEGER NOT NULL CHECK (repository_generation >= 0),
    created_unix_millis INTEGER NOT NULL CHECK (created_unix_millis >= 0),
    last_access_unix_millis INTEGER NOT NULL CHECK (last_access_unix_millis >= 0),
    expires_unix_millis INTEGER NOT NULL CHECK (expires_unix_millis >= 0),
    access_sequence INTEGER NOT NULL CHECK (access_sequence > 0),
    logical_bytes INTEGER NOT NULL CHECK (logical_bytes >= 0),
    evidence_count INTEGER NOT NULL DEFAULT 0 CHECK (evidence_count >= 0),
    evidence_bytes INTEGER NOT NULL DEFAULT 0 CHECK (evidence_bytes >= 0)
);

CREATE INDEX retrieval_receipts_expiry_idx
ON retrieval_receipts(expires_unix_millis, id);
CREATE INDEX retrieval_receipts_lru_idx
ON retrieval_receipts(access_sequence, id);

CREATE TABLE retrieval_receipt_evidence (
    receipt_id INTEGER NOT NULL
        REFERENCES retrieval_receipts(id) ON DELETE CASCADE,
    ordinal INTEGER NOT NULL CHECK (ordinal >= 0),
    path TEXT NOT NULL,
    start_line INTEGER NOT NULL CHECK (start_line >= 0),
    end_line INTEGER NOT NULL CHECK (end_line >= start_line),
    content_hash TEXT NOT NULL,
    semantic_signature INTEGER,
    logical_bytes INTEGER NOT NULL CHECK (logical_bytes >= 0),
    PRIMARY KEY(receipt_id, ordinal)
);

CREATE TRIGGER retrieval_receipts_ai
AFTER INSERT ON retrieval_receipts
BEGIN
    UPDATE retrieval_receipt_usage
    SET receipt_count = receipt_count + 1,
        receipt_bytes = receipt_bytes + new.logical_bytes
    WHERE id = 1;
END;

CREATE TRIGGER retrieval_receipts_ad
AFTER DELETE ON retrieval_receipts
BEGIN
    UPDATE retrieval_receipt_usage
    SET receipt_count = receipt_count - 1,
        receipt_bytes = receipt_bytes - old.logical_bytes,
        evidence_count = evidence_count - old.evidence_count,
        evidence_bytes = evidence_bytes - old.evidence_bytes
    WHERE id = 1;
END;

UPDATE meta SET schema_version = 7 WHERE id = 1;
"#;

pub(crate) const READ_DELTA_BASES_SQL: &str = r#"
CREATE TABLE read_delta_base_usage (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    next_access_sequence INTEGER NOT NULL DEFAULT 0 CHECK (next_access_sequence >= 0),
    base_count INTEGER NOT NULL DEFAULT 0 CHECK (base_count >= 0),
    base_bytes INTEGER NOT NULL DEFAULT 0 CHECK (base_bytes >= 0)
);
INSERT INTO read_delta_base_usage(id) VALUES (1);

CREATE TABLE read_delta_bases (
    target_key TEXT NOT NULL,
    content_hash TEXT NOT NULL,
    repository_generation INTEGER NOT NULL CHECK (repository_generation >= 0),
    target_start_line INTEGER NOT NULL CHECK (target_start_line > 0),
    target_end_line INTEGER NOT NULL CHECK (target_end_line >= target_start_line),
    returned_start_line INTEGER NOT NULL CHECK (returned_start_line > 0),
    returned_end_line INTEGER NOT NULL CHECK (returned_end_line >= returned_start_line),
    content TEXT NOT NULL,
    created_unix_millis INTEGER NOT NULL CHECK (created_unix_millis >= 0),
    last_access_unix_millis INTEGER NOT NULL CHECK (last_access_unix_millis >= 0),
    expires_unix_millis INTEGER NOT NULL CHECK (expires_unix_millis >= 0),
    access_sequence INTEGER NOT NULL CHECK (access_sequence > 0),
    logical_bytes INTEGER NOT NULL CHECK (logical_bytes >= 0),
    PRIMARY KEY(target_key, content_hash)
);
CREATE INDEX read_delta_bases_expiry_idx
ON read_delta_bases(expires_unix_millis, target_key, content_hash);
CREATE INDEX read_delta_bases_lru_idx
ON read_delta_bases(access_sequence, target_key, content_hash);
CREATE INDEX read_delta_bases_target_latest_idx
ON read_delta_bases(
    target_key, repository_generation DESC, access_sequence DESC, content_hash
);

CREATE TRIGGER read_delta_bases_ai
AFTER INSERT ON read_delta_bases
BEGIN
    UPDATE read_delta_base_usage
    SET base_count = base_count + 1,
        base_bytes = base_bytes + new.logical_bytes
    WHERE id = 1;
END;
CREATE TRIGGER read_delta_bases_ad
AFTER DELETE ON read_delta_bases
BEGIN
    UPDATE read_delta_base_usage
    SET base_count = base_count - 1,
        base_bytes = base_bytes - old.logical_bytes
    WHERE id = 1;
END;

UPDATE meta SET schema_version = 8 WHERE id = 1;
"#;

pub(crate) const RECEIPT_EXACT_ONLY_SQL: &str = r#"
ALTER TABLE retrieval_receipt_evidence
ADD COLUMN exact_only INTEGER NOT NULL DEFAULT 0 CHECK (exact_only IN (0, 1));

UPDATE retrieval_receipt_evidence
SET logical_bytes = logical_bytes + 8;
UPDATE retrieval_receipts
SET evidence_bytes = evidence_bytes + evidence_count * 8;
UPDATE retrieval_receipt_usage
SET evidence_bytes = evidence_bytes + evidence_count * 8
WHERE id = 1;

UPDATE meta SET schema_version = 9 WHERE id = 1;
"#;

pub(crate) const QUERY_COVERAGE_RECEIPTS_SQL: &str = r#"
CREATE TABLE query_coverage_receipt_usage (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    namespace TEXT NOT NULL CHECK (length(namespace) = 32),
    next_access_sequence INTEGER NOT NULL DEFAULT 0 CHECK (next_access_sequence >= 0),
    receipt_count INTEGER NOT NULL DEFAULT 0 CHECK (receipt_count >= 0),
    logical_bytes INTEGER NOT NULL DEFAULT 0 CHECK (logical_bytes >= 0)
);

INSERT INTO query_coverage_receipt_usage(id, namespace)
VALUES (1, lower(hex(randomblob(16))));

CREATE TABLE query_coverage_receipts (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    repository_identity TEXT NOT NULL,
    repository_generation INTEGER NOT NULL CHECK (repository_generation >= 0),
    config_hash TEXT NOT NULL,
    semantics_version INTEGER NOT NULL CHECK (semantics_version > 0),
    predicate_json TEXT NOT NULL,
    predicate_blake3 TEXT NOT NULL CHECK (length(predicate_blake3) = 64),
    partition_blake3 TEXT NOT NULL CHECK (length(partition_blake3) = 64),
    partition_file_count INTEGER NOT NULL CHECK (partition_file_count >= 0),
    match_count INTEGER NOT NULL CHECK (match_count >= 0),
    result_blake3 TEXT NOT NULL CHECK (length(result_blake3) = 64),
    created_unix_millis INTEGER NOT NULL CHECK (created_unix_millis >= 0),
    last_access_unix_millis INTEGER NOT NULL CHECK (last_access_unix_millis >= 0),
    expires_unix_millis INTEGER NOT NULL CHECK (expires_unix_millis >= 0),
    access_sequence INTEGER NOT NULL CHECK (access_sequence > 0),
    logical_bytes INTEGER NOT NULL CHECK (logical_bytes >= 0)
);

CREATE INDEX query_coverage_receipts_expiry_idx
ON query_coverage_receipts(expires_unix_millis, id);
CREATE INDEX query_coverage_receipts_lru_idx
ON query_coverage_receipts(access_sequence, id);
CREATE INDEX query_coverage_receipts_predicate_idx
ON query_coverage_receipts(
    repository_generation,
    predicate_blake3,
    partition_blake3,
    result_blake3
);

CREATE TRIGGER query_coverage_receipts_ai
AFTER INSERT ON query_coverage_receipts
BEGIN
    UPDATE query_coverage_receipt_usage
    SET receipt_count = receipt_count + 1,
        logical_bytes = logical_bytes + new.logical_bytes
    WHERE id = 1;
END;

CREATE TRIGGER query_coverage_receipts_ad
AFTER DELETE ON query_coverage_receipts
BEGIN
    UPDATE query_coverage_receipt_usage
    SET receipt_count = receipt_count - 1,
        logical_bytes = logical_bytes - old.logical_bytes
    WHERE id = 1;
END;

UPDATE meta SET schema_version = 10 WHERE id = 1;
"#;

pub(crate) const AUXILIARY_STORAGE_SPLIT_SQL: &str = r#"
DROP TABLE IF EXISTS token_savings;
DROP TABLE IF EXISTS service_failures;
UPDATE meta SET schema_version = 11 WHERE id = 1;
"#;

pub(crate) const MIGRATIONS_SLICE: &[M<'_>] = &[
    M::up(SCHEMA_SQL).foreign_key_check(),
    M::up(LOOKUP_INDEXES_SQL),
    M::up(REPOSITORY_OWNERSHIP_SQL),
    M::up(IMPORT_CANDIDATES_SQL),
    M::up(PATH_PROJECTION_SQL),
    M::up(CACHE_ACCESS_SQL),
    M::up(STRUCTURAL_SEARCH_SQL),
    M::up(RETRIEVAL_RECEIPTS_SQL).foreign_key_check(),
    M::up(READ_DELTA_BASES_SQL),
    M::up(RECEIPT_EXACT_ONLY_SQL),
    M::up(QUERY_COVERAGE_RECEIPTS_SQL),
    M::up(AUXILIARY_STORAGE_SPLIT_SQL),
];
pub(crate) const CURRENT_MIGRATION_VERSION: i64 = 12;
const _: () = assert!(MIGRATIONS_SLICE.len() == CURRENT_MIGRATION_VERSION as usize);
pub(crate) const MIGRATIONS: Migrations<'_> = Migrations::from_slice(MIGRATIONS_SLICE);
use super::*;
