use super::*;
use crate::model::{SearchMode, SearchRequest};
use crate::query_receipt::{
    ExactQueryPredicate, MAX_QUERY_RECEIPTS, QUERY_RECEIPT_TTL_MILLIS, QueryReceiptRecord,
    search_semantics_fingerprint,
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
    assert_eq!(namespace_before, namespace_after);
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
    let directory =
        leantoken_test_support::Sandbox::new(module_path!(), "legacy_identity_migration")
            .expect("sandbox");
    let database = directory.root().join("index.sqlite");
    let mut connection = Connection::open(&database).expect("historical connection");
    migration_test_support::apply_prefix(&mut connection, 11)
        .expect("authoritative query-receipt prefix without runtime identity metadata");
    let namespace_before: String = connection
        .query_row(
            "SELECT namespace FROM query_coverage_receipt_usage WHERE id = 1",
            [],
            |row| row.get(0),
        )
        .expect("query namespace");
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
fn search_semantics_fingerprint_fits_sqlite_positive_integer_range() {
    let fingerprint = search_semantics_fingerprint();

    assert!((1..=i64::MAX as u64).contains(&fingerprint));
    let stored = u64_to_i64(fingerprint).expect("fingerprint fits SQLite INTEGER");
    assert_eq!(
        u64::try_from(stored).expect("positive fingerprint"),
        fingerprint
    );
}
