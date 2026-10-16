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
