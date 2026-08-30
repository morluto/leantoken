use super::*;
use crate::model::{SearchMode, SearchRequest};
use crate::query_receipt::{
    ExactQueryPredicate, MAX_QUERY_RECEIPTS, QUERY_RECEIPT_SEMANTICS_VERSION,
    QUERY_RECEIPT_TTL_MILLIS, QueryReceiptRecord, search_semantics_fingerprint,
};

fn request(query: &str) -> SearchRequest {
    SearchRequest {
        query: query.into(),
        mode: SearchMode::Text,
        include_paths: Vec::new(),
        exclude_paths: Vec::new(),
        focus_paths: Vec::new(),
        max_results: Some(100),
        max_tokens: Some(10_000),
        context_lines: Some(0),
        case_sensitive: true,
        all_occurrences: true,
        prefer_structural: false,
        receipt_id: None,
        query_receipt: None,
        cursor: None,
    }
}

fn record(storage: &Storage, query: &str) -> QueryReceiptRecord {
    let session = storage.begin_read().expect("read session");
    let meta = session.meta().expect("meta");
    let predicate = ExactQueryPredicate::from_request(&request(query)).expect("predicate");
    let predicate_blake3 = predicate.digest().expect("predicate digest");
    let partition = session
        .exact_query_partition(|_| true, || Ok(()))
        .expect("partition");
    QueryReceiptRecord {
        repository_generation: meta.repository_generation,
        config_hash: meta.config_hash,
        predicate,
        predicate_blake3,
        partition,
        match_count: 0,
        result_blake3: blake3::hash(format!("result:{query}").as_bytes())
            .to_hex()
            .to_string(),
    }
}

fn usage(storage: &Storage) -> (usize, usize) {
    let connection = storage
        .writer
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    connection
        .query_row(
            "SELECT receipt_count, logical_bytes
             FROM query_coverage_receipt_usage
             WHERE id = 1",
            [],
            |row| Ok((i64_to_usize(row.get(0)?)?, i64_to_usize(row.get(1)?)?)),
        )
        .expect("query receipt usage")
}

fn indexed_storage(database: &Path) -> Storage {
    let storage = Storage::open(database).expect("storage");
    storage
        .full_reconcile("config", vec![sample_file("lib.rs", "fn indexed() {}\n")])
        .expect("index");
    storage
}

#[test]
fn query_receipts_survive_restart_deduplicate_and_expire() {
    let directory = tempfile::tempdir().expect("directory");
    let database = directory.path().join("index.sqlite");
    let storage = indexed_storage(&database);
    let record_data = record(&storage, "absent");
    let receipt_id = storage
        .persist_query_receipt_at(&record_data, 1_000)
        .expect("persist receipt");
    let duplicate = storage
        .persist_query_receipt_at(&record_data, 1_001)
        .expect("deduplicate receipt");
    assert_eq!(duplicate, receipt_id);
    assert_eq!(usage(&storage).0, 1);
    let predicate_json: String = storage
        .writer
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .query_row(
            "SELECT predicate_json FROM query_coverage_receipts",
            [],
            |row| row.get(0),
        )
        .expect("stored predicate");
    assert!(!predicate_json.contains("absent"));
    assert!(predicate_json.contains("query_blake3"));
    drop(storage);

    let reopened = Storage::open(&database).expect("reopen");
    let session = reopened.begin_read().expect("read");
    assert_eq!(
        session
            .load_query_receipt_at(&receipt_id, 1_002)
            .expect("load persisted receipt")
            .predicate_blake3,
        record_data.predicate_blake3
    );
    assert!(matches!(
        session.load_query_receipt_at(&receipt_id, 1_000 + QUERY_RECEIPT_TTL_MILLIS),
        Err(Error::UnknownQueryReceipt(_))
    ));
    drop(session);

    let replacement = record(&reopened, "replacement");
    reopened
        .persist_query_receipt_at(&replacement, 1_001 + QUERY_RECEIPT_TTL_MILLIS)
        .expect("prune and replace");
    assert_eq!(usage(&reopened).0, 1);
}

#[test]
fn query_receipt_namespaces_survive_ordinary_reopens() {
    let directory = tempfile::tempdir().expect("directory");
    let database = directory.path().join("index.sqlite");
    let storage = indexed_storage(&database);
    let receipt_id = storage
        .persist_query_receipt_at(&record(&storage, "absent"), 1_000)
        .expect("persist receipt");
    let namespace_before: String = storage
        .writer
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .query_row(
            "SELECT namespace FROM query_coverage_receipt_usage WHERE id = 1",
            [],
            |row| row.get(0),
        )
        .expect("query namespace");
    drop(storage);

    let reopened = Storage::open(&database).expect("reopen");
    let namespace_after: String = reopened
        .writer
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .query_row(
            "SELECT namespace FROM query_coverage_receipt_usage WHERE id = 1",
            [],
            |row| row.get(0),
        )
        .expect("query namespace after reopen");
    assert_eq!(
        namespace_before, namespace_after,
        "a plain restart must not regenerate the receipt namespace"
    );
    let session = reopened.begin_read().expect("read");
    assert_eq!(
        session
            .load_query_receipt_at(&receipt_id, 1_001)
            .expect("load persisted receipt")
            .predicate_blake3,
        record(&reopened, "absent").predicate_blake3
    );
}

#[test]
fn cloned_databases_regenerate_receipt_namespaces() {
    let directory = tempfile::tempdir().expect("directory");
    let database = directory.path().join("index.sqlite");
    let storage = indexed_storage(&database);
    storage
        .persist_query_receipt_at(&record(&storage, "absent"), 1_000)
        .expect("persist receipt");
    let namespace_before: String = storage
        .writer
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .query_row(
            "SELECT namespace FROM query_coverage_receipt_usage WHERE id = 1",
            [],
            |row| row.get(0),
        )
        .expect("query namespace");
    drop(storage);

    let cloned = directory.path().join("clone.sqlite");
    std::fs::copy(&database, &cloned).expect("clone main database");
    for extension in ["-wal", "-shm"] {
        let sidecar = database.with_extension("sqlite".to_owned() + extension);
        if sidecar.exists() {
            std::fs::copy(
                &sidecar,
                cloned.with_extension("sqlite".to_owned() + extension),
            )
            .expect("clone sidecar");
        }
    }
    let reopened = Storage::open(&cloned).expect("reopen clone");
    let namespace_after: String = reopened
        .writer
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .query_row(
            "SELECT namespace FROM query_coverage_receipt_usage WHERE id = 1",
            [],
            |row| row.get(0),
        )
        .expect("query namespace after clone");
    assert_ne!(
        namespace_before, namespace_after,
        "a copied database must regenerate the receipt namespace"
    );
}

#[test]
fn first_identity_recording_rotates_legacy_receipt_namespaces() {
    let directory = tempfile::tempdir().expect("directory");
    let database = directory.path().join("index.sqlite");
    let storage = indexed_storage(&database);
    storage
        .persist_query_receipt_at(&record(&storage, "absent"), 1_000)
        .expect("persist receipt");
    let namespace_before: String = storage
        .writer
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .query_row(
            "SELECT namespace FROM query_coverage_receipt_usage WHERE id = 1",
            [],
            |row| row.get(0),
        )
        .expect("query namespace");
    drop(storage);

    // Simulate a pre-change release database: receipts exist but the
    // clone-signal column has never been recorded.
    let connection = Connection::open(&database).expect("open legacy database");
    connection
        .execute_batch("ALTER TABLE meta DROP COLUMN database_identity;")
        .expect("drop clone-signal column");
    drop(connection);

    let reopened = Storage::open(&database).expect("reopen");
    let namespace_after: String = reopened
        .writer
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .query_row(
            "SELECT namespace FROM query_coverage_receipt_usage WHERE id = 1",
            [],
            |row| row.get(0),
        )
        .expect("query namespace after first identity recording");
    assert_ne!(
        namespace_before, namespace_after,
        "recording the identity for the first time must rotate namespaces so \
         pre-change release clones diverge from the moment recording begins"
    );
}

#[test]
fn query_receipt_quota_is_bounded_and_generation_recheck_rolls_back() {
    let directory = tempfile::tempdir().expect("directory");
    let database = directory.path().join("index.sqlite");
    let storage = indexed_storage(&database);
    let first_record = record(&storage, "query-0");
    let first_id = storage
        .persist_query_receipt_at(&first_record, 1_000)
        .expect("first receipt");
    for index in 1..=MAX_QUERY_RECEIPTS {
        storage
            .persist_query_receipt_at(
                &record(&storage, &format!("query-{index}")),
                1_000 + index as i64,
            )
            .expect("bounded receipt");
    }
    assert_eq!(usage(&storage).0, MAX_QUERY_RECEIPTS);
    assert!(matches!(
        storage
            .begin_read()
            .expect("read")
            .load_query_receipt_at(&first_id, 2_000),
        Err(Error::UnknownQueryReceipt(_))
    ));

    let stale = record(&storage, "stale");
    storage
        .full_reconcile(
            "config",
            vec![sample_file("lib.rs", "fn changed_index() {}\n")],
        )
        .expect("advance generation");
    let before = usage(&storage);
    let error = storage
        .persist_query_receipt_at(&stale, 3_000)
        .expect_err("stale snapshot must not persist");
    assert!(matches!(
        error,
        Error::RetryableConflict(crate::error::RetryableOperation::Retrieval)
    ));
    assert_eq!(usage(&storage), before);
}

#[test]
fn touch_query_receipt_bumps_access_sequence_and_extends_ttl() {
    let directory = tempfile::tempdir().expect("directory");
    let database = directory.path().join("index.sqlite");
    let storage = indexed_storage(&database);
    let first_id = storage
        .persist_query_receipt_at(&record(&storage, "query-0"), 1_000)
        .expect("first receipt");
    for index in 1..MAX_QUERY_RECEIPTS {
        storage
            .persist_query_receipt_at(
                &record(&storage, &format!("query-{index}")),
                1_500 + index as i64,
            )
            .expect("fill receipts");
    }
    assert_eq!(usage(&storage).0, MAX_QUERY_RECEIPTS);

    // Touch the first receipt at a later time.
    storage
        .touch_query_receipt_at(&first_id, 10_000)
        .expect("touch");
    let loaded = storage
        .begin_read()
        .expect("read")
        .load_query_receipt_at(&first_id, 10_000)
        .expect("receipt still readable");
    assert_eq!(loaded.receipt_id, first_id);

    // After touch, the first receipt should NOT be evicted by the next receipt.
    // Without the touch, it would be the oldest by access_sequence.
    let next_id = storage
        .persist_query_receipt_at(&record(&storage, "query-next"), 11_000)
        .expect("next receipt");
    assert_ne!(next_id, first_id);
    storage
        .begin_read()
        .expect("read")
        .load_query_receipt_at(&first_id, 11_000)
        .expect("touched receipt survived eviction");
}

#[test]
fn query_receipt_queries_use_bounded_indexes() {
    let directory = tempfile::tempdir().expect("directory");
    let storage = indexed_storage(&directory.path().join("index.sqlite"));
    let connection = storage
        .writer
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let partition_plan = connection
        .prepare("EXPLAIN QUERY PLAN SELECT path, content_hash FROM files ORDER BY path")
        .expect("partition plan")
        .query_map([], |row| row.get::<_, String>(3))
        .expect("partition rows")
        .collect::<rusqlite::Result<Vec<_>>>()
        .expect("partition details")
        .join("\n");
    assert!(
        partition_plan.contains("sqlite_autoindex_files_1"),
        "{partition_plan}"
    );

    let lookup_plan = connection
        .prepare(
            "EXPLAIN QUERY PLAN
             SELECT id
             FROM query_coverage_receipts
             WHERE repository_generation = 1
               AND predicate_blake3 = 'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa'
               AND partition_blake3 = 'bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb'
               AND result_blake3 = 'cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc'
             ORDER BY id
             LIMIT 1",
        )
        .expect("lookup plan")
        .query_map([], |row| row.get::<_, String>(3))
        .expect("lookup rows")
        .collect::<rusqlite::Result<Vec<_>>>()
        .expect("lookup details")
        .join("\n");
    assert!(
        lookup_plan.contains("query_coverage_receipts_predicate_idx"),
        "{lookup_plan}"
    );
}

#[test]
fn query_receipt_migration_preserves_existing_index_and_rolls_back_conflicts() {
    let directory = tempfile::tempdir().expect("directory");
    let database = directory.path().join("index.sqlite");
    let storage = indexed_storage(&database);
    let before = storage.counts().expect("counts");
    drop(storage);

    downgrade_query_receipt_schema(&database, false);
    let migrated = Storage::open(&database).expect("migrate query receipts");
    let after = migrated.counts().expect("migrated counts");
    assert_eq!(
        (
            after.files,
            after.chunks,
            after.symbols,
            after.source_bytes,
            after.languages
        ),
        (
            before.files,
            before.chunks,
            before.symbols,
            before.source_bytes,
            before.languages.clone()
        )
    );
    assert_eq!(
        migrated.meta().expect("migrated meta").schema_version,
        CURRENT_SCHEMA_VERSION
    );
    let connection = Connection::open(&database).expect("inspect migration");
    assert_eq!(
        connection
            .pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
            .expect("migration version"),
        CURRENT_MIGRATION_VERSION
    );
    drop(connection);
    drop(migrated);

    downgrade_query_receipt_schema(&database, true);
    assert!(Storage::open(&database).is_err());
    let connection = Connection::open(&database).expect("inspect rollback");
    assert_eq!(
        connection
            .pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
            .expect("rolled back version"),
        10
    );
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM files", [], |row| row.get::<_, i64>(0))
            .expect("preserved files"),
        i64::try_from(before.files).expect("bounded files")
    );
}

fn downgrade_query_receipt_schema(database: &Path, conflicting_table: bool) {
    let connection = Connection::open(database).expect("downgrade connection");
    connection
        .execute_batch(
            "DROP TABLE query_coverage_receipts;
             DROP TABLE query_coverage_receipt_usage;
             ALTER TABLE meta DROP COLUMN derivation_fingerprint;
             ALTER TABLE meta DROP COLUMN index_scope_includes;
             ALTER TABLE meta DROP COLUMN index_scope_excludes;
             UPDATE meta SET schema_version = 9;
             PRAGMA user_version = 10;",
        )
        .expect("downgrade query receipt schema");
    if conflicting_table {
        connection
            .execute(
                "CREATE TABLE query_coverage_receipt_usage(conflicting_value TEXT)",
                [],
            )
            .expect("conflicting migration table");
    }
}

#[test]
fn stored_semantics_version_matches_the_code_contract() {
    assert_eq!(QUERY_RECEIPT_SEMANTICS_VERSION, 2);
}

#[test]
fn search_semantics_fingerprint_fits_sqlite_positive_integer_range() {
    let fingerprint = search_semantics_fingerprint();

    assert!((1..=i64::MAX as u64).contains(&fingerprint));
    let stored = u64_to_i64(fingerprint).expect("fingerprint fits SQLite INTEGER");
    assert_eq!(
        u64::try_from(stored).expect("positive fingerprint"),
        fingerprint
    );
}
