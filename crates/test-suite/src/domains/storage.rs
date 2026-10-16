use leantoken::model::ReferenceRole;
use leantoken::storage::{
    ChunkInput, ImportInput, IndexedFile, ReferenceInput, Storage, SymbolInput,
};
use leantoken_test_support::Sandbox;

fn sample_chunk(content: &str) -> ChunkInput {
    let lines = content.lines().count().max(1);
    ChunkInput {
        content: content.to_string(),
        start_line: 1,
        end_line: lines,
        start_byte: 0,
        end_byte: content.len(),
        token_count: 0,
    }
}

fn sample_file(path: &str, content: &str) -> IndexedFile {
    IndexedFile {
        path: path.to_string(),
        language: Some("rust".to_string()),
        structurally_complete: true,
        size_bytes: content.len() as u64,
        modified_ns: Some(1_700_000_000_000_000_000),
        content_hash: leantoken::text::hash(content),
        chunks: vec![sample_chunk(content)],
        symbols: vec![SymbolInput {
            name: "main".to_string(),
            kind: "function".to_string(),
            parent: None,
            signature: Some("fn main()".to_string()),
            start_line: 1,
            end_line: content.lines().count().max(1),
            start_byte: 0,
            end_byte: content.len(),
        }],
        references: vec![ReferenceInput {
            name: "println".to_string(),
            kind: "function".to_string(),
            role: ReferenceRole::Reference,
            enclosing_symbol: Some("main".to_string()),
            start_line: 1,
            end_line: 1,
            start_byte: 0,
            end_byte: content.len(),
        }],
        imports: vec![ImportInput {
            raw_target: "std::io".to_string(),
            resolved_path: None,
            candidate_paths: Vec::new(),
            line: 1,
        }],
    }
}

#[test]
fn storage_public_lifecycle_preserves_projections_search_and_atomic_generations() {
    let dir = Sandbox::new(module_path!(), "storage_case").expect("sandbox");
    let db = dir.root().join("index.sqlite");
    let storage = Storage::open(&db).expect("open");
    let meta = storage.meta().expect("meta");
    assert_eq!(meta.schema_version, 12);
    assert_eq!(meta.repository_generation, 0);
    assert!(db.exists());

    let generation = storage
        .full_reconcile(
            "hash1",
            vec![
                sample_file("src/lib.rs", "fn hello() {}\nfn worldliness() {}\n"),
                sample_file("src/main.rs", "fn removable() {}\n"),
            ],
        )
        .expect("initial reconcile");
    assert_eq!(generation, 1);
    assert_eq!(storage.list_files(10, None).expect("list").len(), 2);

    let file = storage
        .find_file("src/lib.rs")
        .expect("find")
        .expect("indexed file");
    assert_eq!(file.generation, 1);
    assert_eq!(
        storage
            .get_chunks_for_file(file.id, 10)
            .expect("chunks")
            .len(),
        1
    );
    assert_eq!(
        storage
            .get_symbols_for_file(file.id, 10)
            .expect("symbols")
            .len(),
        1
    );
    assert_eq!(
        storage
            .get_references_for_file(file.id, 10)
            .expect("references")
            .len(),
        1
    );
    assert_eq!(
        storage
            .get_imports_for_file(file.id, 10)
            .expect("imports")
            .len(),
        1
    );
    assert_eq!(
        storage.search_word("hello", 10).expect("word search").len(),
        1
    );
    assert_eq!(
        storage
            .search_trigram("wor", 10)
            .expect("trigram search")
            .len(),
        1
    );

    let generation = storage
        .reconcile_files(
            "hash1",
            vec![sample_file("src/lib.rs", "fn changed() {}\n")],
            &["src/main.rs".to_owned()],
        )
        .expect("atomic replacement and deletion");
    assert_eq!(generation, 2);
    assert!(
        storage
            .find_file("src/main.rs")
            .expect("find removed")
            .is_none()
    );
    assert!(
        storage
            .search_word("hello", 10)
            .expect("old search")
            .is_empty()
    );
    assert_eq!(
        storage
            .search_word("changed", 10)
            .expect("replacement search")
            .len(),
        1
    );
    assert_eq!(
        storage.meta().expect("updated meta").repository_generation,
        2
    );

    drop(storage);
    let reopened = Storage::open(&db).expect("reopen");
    assert_eq!(
        reopened
            .meta()
            .expect("reopened meta")
            .repository_generation,
        2
    );
    assert_eq!(
        reopened
            .search_word("changed", 10)
            .expect("reopened search")
            .len(),
        1
    );
}

#[test]
fn negative_persisted_unsigned_values_fail_decoding() {
    let dir = Sandbox::new(module_path!(), "storage_case").expect("sandbox");
    let db = dir.root().join("index.sqlite");
    let storage = Storage::open(&db).expect("open");
    storage
        .full_reconcile(
            "hash",
            vec![sample_file("src/lib.rs", "fn corrupted() {}\n")],
        )
        .expect("reconcile");

    let connection = rusqlite::Connection::open(&db).expect("raw connection");
    connection
        .execute(
            "UPDATE files SET size_bytes = -1 WHERE path = 'src/lib.rs'",
            [],
        )
        .expect("inject invalid persisted value");
    drop(connection);

    let error = storage
        .find_file("src/lib.rs")
        .expect_err("negative unsigned field must not be coerced to zero");
    assert!(
        error.to_string().contains("Integer -1 out of range"),
        "unexpected error: {error}"
    );
}

#[test]
fn pooled_read_sessions_serve_concurrent_snapshot_queries() {
    let dir = Sandbox::new(module_path!(), "storage_case").expect("sandbox");
    let storage = Storage::open(dir.root().join("index.sqlite")).expect("open");
    storage
        .full_reconcile("hash", vec![sample_file("lib.rs", "fn pooled() {}\n")])
        .expect("reconcile");

    let handles = (0..32)
        .map(|_| {
            let storage = storage.clone();
            std::thread::spawn(move || storage.snapshot().expect("read snapshot").generation())
        })
        .collect::<Vec<_>>();
    for handle in handles {
        assert_eq!(handle.join().expect("reader thread"), 1);
    }
}

#[test]
fn storage_applies_lookup_index_migration_to_existing_databases() {
    let dir = Sandbox::new(module_path!(), "storage_case").expect("sandbox");
    let db = dir.root().join("index.sqlite");
    drop(Storage::open(&db).expect("open first"));
    let connection = rusqlite::Connection::open(&db).expect("raw connection");
    connection
        .execute_batch(
            "DROP INDEX chunks_file_line_idx;
             DROP TABLE query_coverage_receipts;
             DROP TABLE query_coverage_receipt_usage;
             DROP TABLE read_delta_bases;
             DROP TABLE read_delta_base_usage;
             DROP TABLE retrieval_receipt_evidence;
             DROP TABLE retrieval_receipts;
             DROP TABLE retrieval_receipt_usage;
             DROP TABLE path_entries;
             DROP TABLE import_candidates;
             DROP TABLE token_savings;
             ALTER TABLE files DROP COLUMN source_tokenizer;
             ALTER TABLE files DROP COLUMN source_token_count;
             ALTER TABLE meta DROP COLUMN derivation_fingerprint;
             ALTER TABLE meta DROP COLUMN index_scope_includes;
             ALTER TABLE meta DROP COLUMN index_scope_excludes;
             ALTER TABLE meta DROP COLUMN last_access_unix_seconds;
             ALTER TABLE meta DROP COLUMN repository_identity;
             ALTER TABLE meta DROP COLUMN repository_root;
             UPDATE meta SET schema_version = 1 WHERE id = 1;
             PRAGMA user_version = 1;",
        )
        .expect("simulate version one database");
    drop(connection);

    drop(Storage::open(&db).expect("migrate"));
    let connection = rusqlite::Connection::open(&db).expect("inspect");
    let index_count: i64 = connection
        .query_row(
            "SELECT count(*) FROM pragma_index_list('chunks') WHERE name = 'chunks_file_line_idx'",
            [],
            |row| row.get(0),
        )
        .expect("index count");

    assert_eq!(index_count, 1);
}

#[test]
fn storage_migrates_schema_four_with_cache_access_metadata() {
    let dir = Sandbox::new(module_path!(), "storage_case").expect("sandbox");
    let db = dir.root().join("index.sqlite");
    drop(Storage::open(&db).expect("open first"));
    let connection = rusqlite::Connection::open(&db).expect("raw connection");
    connection
        .execute_batch(
            "DROP TABLE read_delta_bases;
             DROP TABLE query_coverage_receipts;
             DROP TABLE query_coverage_receipt_usage;
             DROP TABLE read_delta_base_usage;
             DROP TABLE retrieval_receipt_evidence;
             DROP TABLE retrieval_receipts;
             DROP TABLE retrieval_receipt_usage;
             DROP TABLE token_savings;
             ALTER TABLE files DROP COLUMN source_tokenizer;
             ALTER TABLE files DROP COLUMN source_token_count;
             ALTER TABLE meta DROP COLUMN derivation_fingerprint;
             ALTER TABLE meta DROP COLUMN index_scope_includes;
             ALTER TABLE meta DROP COLUMN index_scope_excludes;
             ALTER TABLE meta DROP COLUMN last_access_unix_seconds;
             UPDATE meta SET schema_version = 4 WHERE id = 1;
             PRAGMA user_version = 5;",
        )
        .expect("simulate schema four database");
    drop(connection);

    let storage = Storage::open(&db).expect("migrate");
    assert_eq!(storage.meta().expect("metadata").schema_version, 12);
    let connection = rusqlite::Connection::open(&db).expect("inspect");
    let last_access: i64 = connection
        .query_row(
            "SELECT last_access_unix_seconds FROM meta WHERE id = 1",
            [],
            |row| row.get(0),
        )
        .expect("last access");
    assert!(last_access > 0);
    let savings_table: i64 = connection
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = 'token_savings'",
            [],
            |row| row.get(0),
        )
        .expect("token savings table");
    assert_eq!(savings_table, 1);
    let source_token_count_column: i64 = connection
        .query_row(
            "SELECT count(*) FROM pragma_table_info('files') WHERE name = 'source_token_count'",
            [],
            |row| row.get(0),
        )
        .expect("source token count column");
    assert_eq!(source_token_count_column, 1);
    let source_tokenizer_column: i64 = connection
        .query_row(
            "SELECT count(*) FROM pragma_table_info('files') WHERE name = 'source_tokenizer'",
            [],
            |row| row.get(0),
        )
        .expect("source tokenizer column");
    assert_eq!(source_tokenizer_column, 1);
    let migration_version: i64 = connection
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .expect("migration version");
    assert_eq!(migration_version, 13);
}

#[test]
fn storage_adds_full_response_accounting_to_existing_savings_table() {
    let dir = Sandbox::new(module_path!(), "storage_case").expect("sandbox");
    let db = dir.root().join("index.sqlite");
    drop(Storage::open(&db).expect("initial storage"));
    let connection = rusqlite::Connection::open(&db).expect("legacy connection");
    connection
        .execute_batch(
            "ALTER TABLE token_savings DROP COLUMN response_tracked_requests;
             ALTER TABLE token_savings DROP COLUMN response_baseline_requests;
             ALTER TABLE token_savings DROP COLUMN response_baseline_source_tokens;
             ALTER TABLE token_savings DROP COLUMN response_source_tokens;
             ALTER TABLE token_savings DROP COLUMN path_and_metadata_tokens;
             ALTER TABLE token_savings DROP COLUMN protocol_tokens;
             ALTER TABLE token_savings DROP COLUMN total_response_tokens;
             ALTER TABLE token_savings DROP COLUMN receipt_suppressed_exact;
             ALTER TABLE token_savings DROP COLUMN receipt_suppressed_overlap;
             ALTER TABLE token_savings DROP COLUMN expected_hash_not_modified_responses;
             ALTER TABLE token_savings DROP COLUMN expected_hash_suppressed_source_tokens;
             ALTER TABLE token_savings DROP COLUMN useful_requests;
             ALTER TABLE token_savings DROP COLUMN incomplete_requests;
             ALTER TABLE token_savings DROP COLUMN unsupported_requests;
             ALTER TABLE token_savings DROP COLUMN hash_suppressed_requests;
             DROP TABLE service_failures;",
        )
        .expect("simulate source-only savings table");
    connection
        .execute(
            "INSERT INTO token_savings(
                 tokenizer, operation, tracked_requests, baseline_source_tokens,
                 emitted_source_tokens, estimated_source_tokens_saved
             ) VALUES ('cl100k_base', 'search', 2, 100, 20, 80)",
            [],
        )
        .expect("legacy savings row");
    drop(connection);

    drop(Storage::open(&db).expect("upgrade savings accounting"));
    let connection = rusqlite::Connection::open(&db).expect("inspect upgraded table");
    for column in [
        "response_tracked_requests",
        "response_baseline_requests",
        "response_baseline_source_tokens",
        "response_source_tokens",
        "path_and_metadata_tokens",
        "protocol_tokens",
        "total_response_tokens",
        "receipt_suppressed_exact",
        "receipt_suppressed_overlap",
        "expected_hash_not_modified_responses",
        "expected_hash_suppressed_source_tokens",
        "useful_requests",
        "incomplete_requests",
        "unsupported_requests",
        "hash_suppressed_requests",
    ] {
        let present: i64 = connection
            .query_row(
                "SELECT count(*) FROM pragma_table_info('token_savings') WHERE name = ?1",
                [column],
                |row| row.get(0),
            )
            .expect("accounting column");
        assert_eq!(present, 1, "missing {column}");
    }
    let legacy: (i64, i64, i64, i64, i64, i64) = connection
        .query_row(
            "SELECT tracked_requests, baseline_source_tokens,
                    response_tracked_requests, response_baseline_source_tokens,
                    expected_hash_not_modified_responses,
                    expected_hash_suppressed_source_tokens
             FROM token_savings
             WHERE tokenizer = 'cl100k_base' AND operation = 'search'",
            [],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                ))
            },
        )
        .expect("legacy row");
    assert_eq!(legacy, (2, 100, 0, 0, 0, 0));
    let failures_table: i64 = connection
        .query_row(
            "SELECT count(*) FROM sqlite_master
             WHERE type = 'table' AND name = 'service_failures'",
            [],
            |row| row.get(0),
        )
        .expect("service failures table");
    assert_eq!(failures_table, 1);
}

#[test]
fn structural_search_preserves_substring_case_and_short_query_behavior() {
    let dir = Sandbox::new(module_path!(), "storage_case").expect("sandbox");
    let storage = Storage::open(dir.root().join("index.sqlite")).expect("open");
    let mut file = sample_file("src/lib.rs", "fn MainHandler() { PrintLine(); }\n");
    file.symbols[0].name = "MainHandler".into();
    file.references[0].name = "PrintLine".into();
    storage
        .full_reconcile("hash", vec![file])
        .expect("reconcile");

    assert_eq!(
        storage
            .search_symbols("handler", false, 10)
            .expect("case-insensitive symbol search")[0]
            .symbol
            .name,
        "MainHandler"
    );
    assert!(
        storage
            .search_symbols("handler", true, 10)
            .expect("case-sensitive symbol search")
            .is_empty()
    );
    assert_eq!(
        storage
            .search_references("intL", true, 10)
            .expect("case-sensitive reference search")[0]
            .reference
            .name,
        "PrintLine"
    );
    assert_eq!(
        storage
            .search_symbols("Ma", true, 10)
            .expect("short symbol search")[0]
            .symbol
            .name,
        "MainHandler"
    );

    let mut replacement = sample_file("src/lib.rs", "fn NextHandler() { EmitLine(); }\n");
    replacement.symbols[0].name = "NextHandler".into();
    replacement.references[0].name = "EmitLine".into();
    storage
        .reconcile_files("hash", vec![replacement], &[])
        .expect("incremental replacement");
    assert!(
        storage
            .search_symbols("MainHandler", true, 10)
            .expect("removed symbol search")
            .is_empty()
    );
    assert_eq!(
        storage
            .search_references("EmitLine", true, 10)
            .expect("replacement reference search")[0]
            .reference
            .name,
        "EmitLine"
    );
}

#[test]
fn structural_search_migration_rebuilds_existing_rows() {
    let dir = Sandbox::new(module_path!(), "storage_case").expect("sandbox");
    let db = dir.root().join("index.sqlite");
    let storage = Storage::open(&db).expect("open");
    storage
        .full_reconcile("hash", vec![sample_file("src/lib.rs", "fn main() {}\n")])
        .expect("reconcile");
    drop(storage);

    let connection = rusqlite::Connection::open(&db).expect("raw connection");
    connection
        .execute_batch(
            "DROP TRIGGER symbols_ai_trigram;
             DROP TRIGGER symbols_ad_trigram;
             DROP TRIGGER symbols_au_trigram;
             DROP TRIGGER symbol_refs_ai_trigram;
             DROP TRIGGER symbol_refs_ad_trigram;
             DROP TRIGGER symbol_refs_au_trigram;
             DROP TABLE query_coverage_receipts;
             DROP TABLE query_coverage_receipt_usage;
             DROP TABLE read_delta_bases;
             DROP TABLE read_delta_base_usage;
             DROP TABLE retrieval_receipt_evidence;
             DROP TABLE retrieval_receipts;
             DROP TABLE retrieval_receipt_usage;
             DROP TABLE symbols_fts_trigram;
             DROP TABLE symbol_refs_fts_trigram;
             ALTER TABLE meta DROP COLUMN derivation_fingerprint;
             ALTER TABLE meta DROP COLUMN index_scope_includes;
             ALTER TABLE meta DROP COLUMN index_scope_excludes;
             UPDATE meta SET schema_version = 5 WHERE id = 1;
             PRAGMA user_version = 6;",
        )
        .expect("simulate pre-structural-search database");
    drop(connection);

    let migrated = Storage::open(&db).expect("migrate");
    assert_eq!(
        migrated
            .search_symbols("main", true, 10)
            .expect("migrated symbol search")[0]
            .symbol
            .name,
        "main"
    );
    assert_eq!(
        migrated
            .search_references("println", true, 10)
            .expect("migrated reference search")[0]
            .reference
            .name,
        "println"
    );
}

#[test]
fn failed_reconcile_rolls_back_file_and_generation() {
    let dir = Sandbox::new(module_path!(), "storage_case").expect("sandbox");
    let storage = Storage::open(dir.root().join("index.sqlite")).expect("open");
    storage
        .full_reconcile("hash1", vec![sample_file("src/lib.rs", "fn old() {}\n")])
        .expect("initial reconcile");

    let mut invalid = sample_file("src/lib.rs", "fn replacement() {}\n");
    invalid.chunks[0].end_line = usize::MAX;
    storage
        .reconcile_files("hash1", vec![invalid], &[])
        .expect_err("out-of-range row must fail");

    assert_eq!(storage.repository_generation().expect("generation"), 1);
    assert_eq!(
        storage.search_word("old", 10).expect("old content").len(),
        1
    );
    assert!(
        storage
            .search_word("replacement", 10)
            .expect("new content")
            .is_empty()
    );
}

#[test]
fn stale_reconciliation_plan_cannot_overwrite_a_newer_generation() {
    let dir = Sandbox::new(module_path!(), "storage_case").expect("sandbox");
    let storage = Storage::open(dir.root().join("index.sqlite")).expect("open");
    let stale_baseline = storage.meta().expect("baseline");

    storage
        .full_reconcile("hash1", vec![sample_file("lib.rs", "fn current() {}\n")])
        .expect("current generation");

    let error = storage
        .reconcile_files_at(
            &stale_baseline,
            "hash1",
            vec![sample_file("lib.rs", "fn stale() {}\n")],
            &[],
        )
        .expect_err("stale plan must be rejected");
    assert!(matches!(
        error,
        leantoken::Error::StaleReconciliation {
            expected: 0,
            actual: 1
        }
    ));
    assert_eq!(storage.repository_generation().expect("generation"), 1);
    assert_eq!(
        storage.search_word("current", 10).expect("current").len(),
        1
    );
    assert!(storage.search_word("stale", 10).expect("stale").is_empty());
}

#[test]
fn list_files_respects_hard_result_bound() {
    let dir = Sandbox::new(module_path!(), "storage_case").expect("sandbox");
    let storage = Storage::open(dir.root().join("index.sqlite")).expect("open");

    let files: Vec<_> = (0..150)
        .map(|i| sample_file(&format!("src/file{i}.rs"), &format!("fn func{i}() {{}}\n")))
        .collect();
    storage.full_reconcile("hash1", files).expect("reconcile");

    let first = storage.list_files(10, None).expect("first page");
    assert_eq!(first.len(), 10);

    let huge = storage.list_files(100_000, None).expect("bounded request");
    assert!(
        huge.len() <= 150,
        "should return no more than actual files, got {}",
        huge.len()
    );

    // Pagination using cursor should progress deterministically.
    let second = storage
        .list_files(10, Some(first.last().unwrap().id))
        .expect("second page");
    assert_eq!(second.len(), 10);
    assert!(second[0].id > first[0].id);
}

#[test]
fn wal_and_foreign_keys_enabled() {
    let dir = Sandbox::new(module_path!(), "storage_case").expect("sandbox");
    let db = dir.root().join("index.sqlite");
    let _storage = Storage::open(&db).expect("open");

    use rusqlite::Connection;
    let conn = Connection::open(&db).expect("open check");
    let journal: String = conn
        .query_row("PRAGMA journal_mode", [], |row| row.get(0))
        .expect("journal_mode");
    assert_eq!(journal, "wal");

    let foreign_keys: i64 = conn
        .query_row("PRAGMA foreign_keys", [], |row| row.get(0))
        .expect("foreign_keys");
    assert_eq!(foreign_keys, 1);
}

#[test]
fn read_session_pins_generation_across_queries() {
    let dir = Sandbox::new(module_path!(), "storage_case").expect("sandbox");
    let path = dir.root().join("index.sqlite");
    let storage = Storage::open(&path).expect("open");
    let gen1 = storage
        .full_reconcile("cfg-a", vec![sample_file("a.rs", "fn a() {}\n")])
        .expect("gen1");
    assert_eq!(gen1, 1);

    let session = storage.snapshot().expect("snapshot");
    assert_eq!(session.generation(), 1);
    let files = session.list_files(100, None).expect("list");
    assert_eq!(files.len(), 1);

    // Concurrent publish must not change the open snapshot.
    let gen2 = storage
        .full_reconcile("cfg-b", vec![sample_file("b.rs", "fn b() {}\n")])
        .expect("gen2");
    assert_eq!(gen2, 2);
    assert_eq!(session.generation(), 1);
    let still = session.list_files(100, None).expect("list pinned");
    assert_eq!(still.len(), 1);
    assert_eq!(still[0].path, "a.rs");

    // Fresh session sees the new generation.
    let latest = storage.snapshot().expect("fresh snapshot");
    assert_eq!(latest.generation(), 2);
    assert_eq!(latest.list_files(100, None).expect("list").len(), 1);
    assert_eq!(latest.list_files(100, None).expect("list")[0].path, "b.rs");
}
