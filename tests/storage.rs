use leantoken::model::ReferenceRole;
use leantoken::storage::{
    ChunkInput, ImportInput, IndexedFile, ReferenceInput, Storage, SymbolInput,
};

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

fn query_plan(connection: &rusqlite::Connection, sql: &str, parameters: &[&dyn rusqlite::ToSql]) -> String {
    let mut statement = connection.prepare(sql).expect("prepare query plan");
    statement
        .query_map(parameters, |row| row.get::<_, String>(3))
        .expect("query plan")
        .collect::<std::result::Result<Vec<_>, _>>()
        .expect("query plan rows")
        .join("\n")
}

#[test]
fn storage_opens_and_validates_fts5_support() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("index.sqlite");
    let storage = Storage::open(&db).expect("open");
    let meta = storage.meta().expect("meta");
    assert_eq!(meta.schema_version, 4);
    assert_eq!(meta.repository_generation, 0);
    assert!(db.exists());
}

#[test]
fn storage_reopen_uses_existing_index() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("index.sqlite");

    let storage = Storage::open(&db).expect("open first");
    storage
        .full_reconcile("hash1", vec![sample_file("src/lib.rs", "fn a() {}")])
        .expect("reconcile");

    let storage2 = Storage::open(&db).expect("open second");
    let found = storage2.find_file("src/lib.rs").expect("find");
    assert!(found.is_some());
    let meta = storage2.meta().expect("meta");
    assert_eq!(meta.repository_generation, 1);
}

#[test]
fn pooled_read_sessions_serve_concurrent_snapshot_queries() {
    let dir = tempfile::tempdir().expect("tempdir");
    let storage = Storage::open(dir.path().join("index.sqlite")).expect("open");
    storage
        .full_reconcile("hash", vec![sample_file("lib.rs", "fn pooled() {}\n")])
        .expect("reconcile");

    let handles = (0..32)
        .map(|_| {
            let storage = storage.clone();
            std::thread::spawn(move || {
                let session = storage.begin_read().expect("read session");
                session.repository_generation().expect("generation")
            })
        })
        .collect::<Vec<_>>();
    for handle in handles {
        assert_eq!(handle.join().expect("reader thread"), 1);
    }
}

#[test]
fn storage_applies_lookup_index_migration_to_existing_databases() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("index.sqlite");
    drop(Storage::open(&db).expect("open first"));
    let connection = rusqlite::Connection::open(&db).expect("raw connection");
    connection
        .execute_batch(
            "DROP INDEX chunks_file_line_idx;
             DROP TABLE path_entries;
             DROP TABLE import_candidates;
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
fn hot_relational_projections_use_their_indexes() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("index.sqlite");
    let storage = Storage::open(&db).expect("open");
    let mut importer = sample_file("src/app.rs", "fn app() {}\n");
    importer.imports[0].candidate_paths = vec!["src/lib.rs".into()];
    importer.imports[0].resolved_path = Some("src/lib.rs".into());
    storage
        .full_reconcile(
            "hash",
            vec![importer, sample_file("src/lib.rs", "fn library() {}\n")],
        )
        .expect("reconcile");

    let connection = rusqlite::Connection::open(&db).expect("inspect");
    let changed = serde_json::to_string(&["src/lib.rs"]).expect("json");
    let import_plan = query_plan(
        &connection,
        "EXPLAIN QUERY PLAN
         SELECT DISTINCT files.path
         FROM json_each(?1) AS changed
         JOIN import_candidates ON import_candidates.candidate_path = changed.value
         JOIN imports ON imports.id = import_candidates.import_id
         JOIN files ON files.id = imports.file_id",
        &[&changed],
    );
    assert!(
        import_plan.contains("import_candidates_path_idx"),
        "unexpected reverse-import plan: {import_plan}"
    );

    let ranges = serde_json::json!([{"file_id": 1, "start_line": 1, "end_line": 2}]);
    let range_plan = query_plan(
        &connection,
        "EXPLAIN QUERY PLAN
         WITH requested AS (
             SELECT CAST(value ->> 'file_id' AS INTEGER) AS file_id,
                    CAST(value ->> 'start_line' AS INTEGER) AS start_line,
                    CAST(value ->> 'end_line' AS INTEGER) AS end_line
             FROM json_each(?1)
         )
         SELECT chunks.id FROM requested
         JOIN chunks ON chunks.file_id = requested.file_id
                    AND chunks.end_line >= requested.start_line
                    AND chunks.start_line <= requested.end_line",
        &[&ranges.to_string()],
    );
    assert!(
        range_plan.contains("chunks_file_line_idx"),
        "unexpected batched-range plan: {range_plan}"
    );

    let tree_plan = query_plan(
        &connection,
        "EXPLAIN QUERY PLAN
         SELECT path FROM path_entries
         WHERE path > ?1 ORDER BY path LIMIT ?2",
        &[&"src", &10_i64],
    );
    assert!(
        tree_plan.contains("sqlite_autoindex_path_entries_1"),
        "unexpected tree keyset plan: {tree_plan}"
    );
}

#[test]
fn initial_reconcile_advances_generation_and_indexes() {
    let dir = tempfile::tempdir().expect("tempdir");
    let storage = Storage::open(dir.path().join("index.sqlite")).expect("open");

    let files = vec![
        sample_file("src/lib.rs", "fn hello() {}\nfn world() {}\n"),
        sample_file("src/main.rs", "fn greet() {}\n"),
    ];

    let generation = storage.full_reconcile("hash1", files).expect("reconcile");
    assert_eq!(generation, 1);

    let listed = storage.list_files(10, None).expect("list");
    assert_eq!(listed.len(), 2);

    let found = storage.find_file("src/lib.rs").expect("find");
    assert!(found.is_some());
    let file = found.unwrap();
    assert_eq!(file.path, "src/lib.rs");
    assert_eq!(file.generation, 1);

    let chunks = storage.get_chunks_for_file(file.id, 10).expect("chunks");
    assert_eq!(chunks.len(), 1);

    let symbols = storage.get_symbols_for_file(file.id, 10).expect("symbols");
    assert_eq!(symbols.len(), 1);
    assert_eq!(symbols[0].name, "main");

    let refs = storage.get_references_for_file(file.id, 10).expect("refs");
    assert_eq!(refs.len(), 1);

    let imports = storage.get_imports_for_file(file.id, 10).expect("imports");
    assert_eq!(imports.len(), 1);
}

#[test]
fn fts5_word_search_finds_indexed_content() {
    let dir = tempfile::tempdir().expect("tempdir");
    let storage = Storage::open(dir.path().join("index.sqlite")).expect("open");

    storage
        .full_reconcile("hash1", vec![sample_file("src/lib.rs", "fn hello() {}\n")])
        .expect("reconcile");

    let hits = storage.search_word("hello", 10).expect("search word");
    assert_eq!(hits.len(), 1);
    assert!(hits[0].content.contains("hello"));
    assert_eq!(hits[0].path, "src/lib.rs");
}

#[test]
fn fts5_trigram_search_finds_substrings() {
    let dir = tempfile::tempdir().expect("tempdir");
    let storage = Storage::open(dir.path().join("index.sqlite")).expect("open");

    storage
        .full_reconcile(
            "hash1",
            vec![sample_file("src/lib.rs", "fn worldliness() {}\n")],
        )
        .expect("reconcile");

    let hits = storage.search_trigram("wor", 10).expect("search trigram");
    assert_eq!(hits.len(), 1);
    assert!(hits[0].content.contains("worldliness"));
}

#[test]
fn generation_consistency_across_reopen_and_modify() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("index.sqlite");
    let storage = Storage::open(&db).expect("open");

    let generation1 = storage
        .full_reconcile("hash1", vec![sample_file("src/a.rs", "fn alpha() {}\n")])
        .expect("reconcile");
    assert_eq!(generation1, 1);

    let generation2 = storage
        .reconcile_files(
            "hash1",
            vec![sample_file("src/a.rs", "fn beta() {}\n")],
            &[],
        )
        .expect("reconcile replacement");
    assert_eq!(generation2, 2);

    let storage2 = Storage::open(&db).expect("reopen");
    let meta = storage2.meta().expect("meta");
    assert_eq!(meta.repository_generation, 2);

    let hits = storage2.search_word("beta", 10).expect("search");
    assert_eq!(hits.len(), 1);
}

#[test]
fn failed_reconcile_rolls_back_file_and_generation() {
    let dir = tempfile::tempdir().expect("tempdir");
    let storage = Storage::open(dir.path().join("index.sqlite")).expect("open");
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
fn reconcile_files_commits_replacements_and_deletions_as_one_generation() {
    let dir = tempfile::tempdir().expect("tempdir");
    let storage = Storage::open(dir.path().join("index.sqlite")).expect("open");
    storage
        .full_reconcile(
            "hash1",
            vec![
                sample_file("keep.rs", "fn keep() {}\n"),
                sample_file("remove.rs", "fn remove() {}\n"),
            ],
        )
        .expect("initial reconcile");

    let generation = storage
        .reconcile_files(
            "hash1",
            vec![sample_file("keep.rs", "fn changed() {}\n")],
            &["remove.rs".to_string()],
        )
        .expect("incremental reconcile");

    assert_eq!(generation, 2);
    assert!(storage.find_file("remove.rs").expect("find").is_none());
    assert_eq!(storage.search_word("changed", 10).expect("search").len(), 1);
    assert!(storage.search_word("keep", 10).expect("search").is_empty());
    assert!(storage.search_word("remove", 10).expect("search").is_empty());
    assert_eq!(storage.meta().expect("meta").repository_generation, 2);
}

#[test]
fn stale_reconciliation_plan_cannot_overwrite_a_newer_generation() {
    let dir = tempfile::tempdir().expect("tempdir");
    let storage = Storage::open(dir.path().join("index.sqlite")).expect("open");
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
    assert_eq!(storage.search_word("current", 10).expect("current").len(), 1);
    assert!(storage.search_word("stale", 10).expect("stale").is_empty());
}

#[test]
fn list_files_respects_hard_result_bound() {
    let dir = tempfile::tempdir().expect("tempdir");
    let storage = Storage::open(dir.path().join("index.sqlite")).expect("open");

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
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("index.sqlite");
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
    let dir = tempfile::tempdir().expect("dir");
    let path = dir.path().join("index.sqlite");
    let storage = Storage::open(&path).expect("open");
    let gen1 = storage
        .full_reconcile("cfg-a", vec![sample_file("a.rs", "fn a() {}\n")])
        .expect("gen1");
    assert_eq!(gen1, 1);

    let session = storage.begin_read().expect("session");
    assert_eq!(session.repository_generation().expect("gen"), 1);
    let files = session.list_files(100, None).expect("list");
    assert_eq!(files.len(), 1);

    // Concurrent publish must not change the open snapshot.
    let gen2 = storage
        .full_reconcile("cfg-b", vec![sample_file("b.rs", "fn b() {}\n")])
        .expect("gen2");
    assert_eq!(gen2, 2);
    assert_eq!(session.repository_generation().expect("pinned gen"), 1);
    let still = session.list_files(100, None).expect("list pinned");
    assert_eq!(still.len(), 1);
    assert_eq!(still[0].path, "a.rs");

    // Fresh session sees the new generation.
    let latest = storage.begin_read().expect("fresh");
    assert_eq!(latest.repository_generation().expect("latest"), 2);
    assert_eq!(latest.list_files(100, None).expect("list").len(), 1);
    assert_eq!(latest.list_files(100, None).expect("list")[0].path, "b.rs");
}
