use std::sync::{Arc, Barrier};

use super::*;
use crate::read_delta::{
    MAX_READ_DELTA_BASE_BYTES, MAX_READ_DELTA_BASES, MAX_TOTAL_READ_DELTA_BASE_BYTES,
    READ_DELTA_BASE_TTL_MILLIS, ReadDeltaBase,
};

fn base(content: impl Into<String>, generation: u64) -> (String, ReadDeltaBase) {
    let content = content.into();
    let content_hash = crate::text::hash(&content);
    (
        content_hash,
        ReadDeltaBase {
            content,
            generation,
            target_start_line: 1,
            target_end_line: 4,
            returned_start_line: 1,
            returned_end_line: 4,
        },
    )
}

fn usage(storage: &Storage) -> (usize, usize) {
    let connection = storage
        .writer
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    connection
        .query_row(
            "SELECT base_count, base_bytes FROM read_delta_base_usage WHERE id = 1",
            [],
            |row| Ok((i64_to_usize(row.get(0)?)?, i64_to_usize(row.get(1)?)?)),
        )
        .expect("read delta usage")
}

#[test]
fn persistent_read_delta_base_survives_restart_and_prefers_newest_generation() {
    let directory = tempfile::tempdir().expect("directory");
    let database = directory.path().join("index.sqlite");
    let (old_hash, old) = base("old\n", 1);
    let (new_hash, new) = base("new\n", 2);
    let now = unix_millis(SystemTime::now());
    {
        let storage = Storage::open(&database).expect("storage");
        assert!(
            storage
                .persist_read_delta_base_at("target", &old_hash, &old, now - 100_000)
                .expect("persist old")
        );
        assert!(
            storage
                .persist_read_delta_base_at("target", &new_hash, &new, now - 99_999)
                .expect("persist new")
        );
        assert_eq!(
            storage
                .read_delta_base_at("target", Some(&old_hash), now - 30_000)
                .expect("touch old"),
            Some(old.clone())
        );
    }

    let reopened = Storage::open(&database).expect("reopen");
    assert_eq!(
        reopened
            .latest_read_delta_base("target")
            .expect("latest base"),
        Some((new_hash, new))
    );
}

#[test]
fn duplicate_base_refreshes_metadata_without_growing_usage() {
    let directory = tempfile::tempdir().expect("directory");
    let storage = Storage::open(directory.path().join("index.sqlite")).expect("storage");
    let (content_hash, mut original) = base("same\n", 1);
    storage
        .persist_read_delta_base_at("target", &content_hash, &original, 1_000)
        .expect("persist original");
    let before = usage(&storage);

    original.generation = 7;
    original.returned_end_line = 3;
    storage
        .persist_read_delta_base_at("target", &content_hash, &original, 2_000)
        .expect("refresh original");
    assert_eq!(usage(&storage), before);
    assert_eq!(
        storage
            .read_delta_base_at("target", Some(&content_hash), 2_000)
            .expect("read refreshed"),
        Some(original)
    );
}

#[test]
fn read_delta_expiry_and_clock_rollback_fail_closed() {
    let directory = tempfile::tempdir().expect("directory");
    let storage = Storage::open(directory.path().join("index.sqlite")).expect("storage");
    let (expired_hash, expired) = base("expired\n", 1);
    storage
        .persist_read_delta_base_at("expired", &expired_hash, &expired, 10_000)
        .expect("persist expiring base");
    assert_eq!(
        storage
            .read_delta_base_at(
                "expired",
                Some(&expired_hash),
                10_000 + READ_DELTA_BASE_TTL_MILLIS
            )
            .expect("expire base"),
        None
    );

    let (rollback_hash, rollback) = base("rollback\n", 1);
    storage
        .persist_read_delta_base_at("rollback", &rollback_hash, &rollback, 20_000)
        .expect("persist rollback base");
    assert_eq!(
        storage
            .read_delta_base_at("rollback", Some(&rollback_hash), 19_999)
            .expect("reject future base"),
        None
    );
    assert_eq!(usage(&storage), (0, 0));
}

#[test]
fn read_delta_count_and_logical_byte_quotas_evict_deterministically() {
    let directory = tempfile::tempdir().expect("directory");
    let storage = Storage::open(directory.path().join("index.sqlite")).expect("storage");
    let mut hashes = Vec::new();
    for index in 0..MAX_READ_DELTA_BASES {
        let (content_hash, candidate) = base(format!("base-{index}\n"), index as u64);
        storage
            .persist_read_delta_base_at(
                &format!("target-{index}"),
                &content_hash,
                &candidate,
                1_000 + index as i64,
            )
            .expect("fill count quota");
        hashes.push(content_hash);
    }
    storage
        .read_delta_base_at("target-0", Some(&hashes[0]), 70_000)
        .expect("refresh oldest");
    let (extra_hash, extra) = base("extra\n", MAX_READ_DELTA_BASES as u64);
    storage
        .persist_read_delta_base_at("target-extra", &extra_hash, &extra, 70_001)
        .expect("evict by count");
    assert!(
        storage
            .read_delta_base_at("target-0", Some(&hashes[0]), 70_002)
            .expect("retained touched base")
            .is_some()
    );
    assert_eq!(
        storage
            .read_delta_base_at("target-1", Some(&hashes[1]), 70_002)
            .expect("evicted oldest base"),
        None
    );
    assert_eq!(usage(&storage).0, MAX_READ_DELTA_BASES);

    let large = "x".repeat(128 * 1024);
    for index in 0..70 {
        let (content_hash, candidate) = base(format!("{index}:{large}"), 1_000 + index);
        storage
            .persist_read_delta_base_at(
                &format!("large-{index}"),
                &content_hash,
                &candidate,
                80_000 + index as i64,
            )
            .expect("enforce byte quota");
    }
    let (count, bytes) = usage(&storage);
    assert!(count <= MAX_READ_DELTA_BASES);
    assert!(bytes <= MAX_TOTAL_READ_DELTA_BASE_BYTES);

    let (oversized_hash, oversized) = base("x".repeat(MAX_READ_DELTA_BASE_BYTES + 1), 2_000);
    assert!(
        !storage
            .persist_read_delta_base_at("oversized", &oversized_hash, &oversized, 90_000)
            .expect("reject oversized base")
    );
    assert_eq!(usage(&storage), (count, bytes));
}

#[test]
fn independent_read_delta_writers_do_not_lose_updates() {
    let directory = tempfile::tempdir().expect("directory");
    let database = directory.path().join("index.sqlite");
    let storage = Storage::open(&database).expect("storage");
    let barrier = Arc::new(Barrier::new(3));
    let mut threads = Vec::new();
    for index in 0..2 {
        let database = database.clone();
        let barrier = Arc::clone(&barrier);
        threads.push(std::thread::spawn(move || {
            let storage = Storage::open(database).expect("independent storage");
            let (content_hash, candidate) = base(format!("concurrent-{index}\n"), index + 1);
            barrier.wait();
            storage
                .persist_read_delta_base(&format!("target-{index}"), &content_hash, &candidate)
                .expect("concurrent persist");
            (content_hash, candidate)
        }));
    }
    barrier.wait();
    let written = threads
        .into_iter()
        .map(|thread| thread.join().expect("join"))
        .collect::<Vec<_>>();
    assert_eq!(usage(&storage).0, 2);
    for (index, (content_hash, candidate)) in written.into_iter().enumerate() {
        assert_eq!(
            storage
                .read_delta_base(&format!("target-{index}"), &content_hash)
                .expect("read concurrent base"),
            Some(candidate)
        );
    }
}

#[test]
fn read_delta_corruption_and_insert_failures_are_fail_loud_and_atomic() {
    let directory = tempfile::tempdir().expect("directory");
    let storage = Storage::open(directory.path().join("index.sqlite")).expect("storage");
    let (content_hash, candidate) = base("valid\n", 1);
    assert!(
        storage
            .persist_read_delta_base_at("target", "wrong-hash", &candidate, 1_000)
            .is_err()
    );
    assert_eq!(usage(&storage), (0, 0));

    {
        let connection = storage
            .writer
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        connection
            .execute_batch(
                "CREATE TRIGGER fail_read_delta_base
                 BEFORE INSERT ON read_delta_bases
                 BEGIN
                     SELECT RAISE(ABORT, 'injected read delta failure');
                 END;",
            )
            .expect("failure trigger");
    }
    assert!(
        storage
            .persist_read_delta_base_at("target", &content_hash, &candidate, 1_000)
            .is_err()
    );
    assert_eq!(usage(&storage), (0, 0));
}

#[test]
fn read_delta_queries_use_bounded_indexes() {
    let directory = tempfile::tempdir().expect("directory");
    let storage = Storage::open(directory.path().join("index.sqlite")).expect("storage");
    let connection = storage
        .writer
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    for (sql, expected) in [
        (
            "SELECT content FROM read_delta_bases
             WHERE target_key = 'target' AND content_hash = 'hash'",
            "sqlite_autoindex_read_delta_bases_1",
        ),
        (
            "SELECT content_hash FROM read_delta_bases
             WHERE target_key = 'target'
             ORDER BY repository_generation DESC, access_sequence DESC, content_hash
             LIMIT 1",
            "read_delta_bases_target_latest_idx",
        ),
        (
            "DELETE FROM read_delta_bases WHERE expires_unix_millis <= 1",
            "read_delta_bases_expiry_idx",
        ),
        (
            "SELECT target_key, content_hash FROM read_delta_bases
             ORDER BY access_sequence, target_key, content_hash LIMIT 1",
            "read_delta_bases_lru_idx",
        ),
    ] {
        let mut statement = connection
            .prepare(&format!("EXPLAIN QUERY PLAN {sql}"))
            .expect("query plan");
        let details = statement
            .query_map([], |row| row.get::<_, String>(3))
            .expect("plan rows")
            .collect::<rusqlite::Result<Vec<_>>>()
            .expect("plan details")
            .join(" | ");
        assert!(
            details.contains(expected),
            "expected {expected:?} in query plan {details:?} for {sql}"
        );
        assert!(
            !details.contains("USE TEMP B-TREE"),
            "query plan did not preserve index ordering: {details}"
        );
    }
}

#[test]
fn read_delta_migration_preserves_index_and_failed_migration_rolls_back() {
    let directory = tempfile::tempdir().expect("directory");
    let database = directory.path().join("index.sqlite");
    {
        let storage = Storage::open(&database).expect("storage");
        storage
            .full_reconcile("config", vec![sample_file("lib.rs", "fn indexed() {}\n")])
            .expect("index");
    }
    downgrade_read_delta_schema(&database, false);
    let migrated = Storage::open(&database).expect("migrate read delta bases");
    assert!(migrated.find_file("lib.rs").expect("find").is_some());
    drop(migrated);

    downgrade_read_delta_schema(&database, true);
    assert!(Storage::open(&database).is_err());
    let connection = Connection::open(&database).expect("inspect failed migration");
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM files", [], |row| row.get::<_, i64>(0))
            .expect("file count"),
        1
    );
    assert_eq!(
        connection
            .pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
            .expect("migration version"),
        8
    );
}

#[cfg(unix)]
#[test]
fn persistent_read_delta_content_keeps_index_database_ownership_and_permissions() {
    use std::os::unix::fs::MetadataExt;

    let directory = tempfile::tempdir().expect("directory");
    let database = directory.path().join("index.sqlite");
    let storage = Storage::open(&database).expect("storage");
    let before = std::fs::metadata(&database).expect("metadata before");
    let (content_hash, candidate) = base("same database content\n", 1);
    storage
        .persist_read_delta_base("target", &content_hash, &candidate)
        .expect("persist base");
    let after = std::fs::metadata(&database).expect("metadata after");
    assert_eq!(after.uid(), before.uid());
    assert_eq!(after.gid(), before.gid());
    assert_eq!(after.mode(), before.mode());

    let connection = storage
        .writer
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM pragma_database_list
                 WHERE name NOT IN ('main', 'temp')",
                [],
                |row| row.get::<_, i64>(0),
            )
            .expect("attached source databases"),
        0,
        "persistent source content must not use an attached sidecar database"
    );
}

fn downgrade_read_delta_schema(database: &Path, conflicting_table: bool) {
    let connection = Connection::open(database).expect("downgrade connection");
    connection
        .execute_batch(
            "DROP TABLE IF EXISTS read_delta_bases;
             DROP TABLE IF EXISTS read_delta_base_usage;
             DROP TABLE IF EXISTS query_coverage_receipts;
             DROP TABLE IF EXISTS query_coverage_receipt_usage;
             ALTER TABLE retrieval_receipt_evidence DROP COLUMN exact_only;
             UPDATE retrieval_receipt_evidence
             SET logical_bytes = logical_bytes - 8;
             UPDATE retrieval_receipts
             SET evidence_bytes = evidence_bytes - evidence_count * 8;
             UPDATE retrieval_receipt_usage
             SET evidence_bytes = evidence_bytes - evidence_count * 8
             WHERE id = 1;
             UPDATE meta SET schema_version = 7 WHERE id = 1;
             PRAGMA user_version = 8;",
        )
        .expect("downgrade read delta schema");
    if conflicting_table {
        connection
            .execute("CREATE TABLE read_delta_bases(conflicting_value TEXT)", [])
            .expect("conflicting migration table");
    }
}
