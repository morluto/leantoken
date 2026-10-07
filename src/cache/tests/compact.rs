use super::*;
use crate::cache::{CacheCompactOutcome, CacheCompactRequest};
use crate::coordination::IndexCoordination;
use tokio_util::sync::CancellationToken;

fn fixture(manager: &CacheManager, repository: &Path) -> (String, PathBuf) {
    let (id, database) = create_current_cache(manager, repository, 42);
    let connection = Connection::open(&database).unwrap();
    connection.execute_batch("CREATE TABLE compact_churn(id INTEGER PRIMARY KEY, payload BLOB);
        INSERT INTO compact_churn SELECT value, zeroblob(65536) FROM json_each('[1,2,3,4,5,6,7,8,9,10,11,12,13,14,15,16]');
        DELETE FROM compact_churn WHERE id > 1;
        INSERT INTO files(id,path,content_hash,generation) VALUES(1,'src/compact.rs','unchanged',42);
        INSERT INTO chunks(id,file_id,content,start_line,end_line,start_byte,end_byte)
        VALUES(17,1,'pub fn compact() {}',1,1,0,19);
        UPDATE meta SET repository_generation=42;").unwrap();
    drop(connection);
    (id, database)
}

fn selected(id: &str, apply: bool) -> CacheCompactRequest {
    CacheCompactRequest {
        ids: vec![id.to_owned()],
        min_reclaim_bytes: 1,
        min_reclaim_percent: 1,
        dry_run: !apply,
        yes: apply,
        ..Default::default()
    }
}

fn content(database: &Path) -> (i64, Vec<(i64, String)>, Vec<i64>) {
    let connection = Connection::open(database).unwrap();
    (
        connection
            .query_row("SELECT repository_generation FROM meta", [], |row| {
                row.get(0)
            })
            .unwrap(),
        connection
            .prepare("SELECT id,content FROM chunks ORDER BY id")
            .unwrap()
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<std::result::Result<_, _>>()
            .unwrap(),
        connection
            .prepare("SELECT rowid FROM chunks_fts_word WHERE chunks_fts_word MATCH 'compact'")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<std::result::Result<_, _>>()
            .unwrap(),
    )
}

#[test]
fn compact_preserves_generation_content_fts_and_lease_identity() {
    let temp = tempfile::tempdir().unwrap();
    let manager = CacheManager::new(temp.path().join("managed"), 100);
    let (id, database) = fixture(&manager, temp.path());
    let before = content(&database);
    // Obtain the stable coordination identity before measuring its inode.
    drop(
        IndexCoordination::for_database(&database)
            .try_acquire_prune_lease()
            .unwrap(),
    );
    let lease = coordination_sidecar_path(&database, LEASE_LOCK_SUFFIX);
    let lease_before = fs::metadata(&lease).unwrap();
    let report = manager.compact(&selected(&id, true)).unwrap();
    assert_eq!(report.results[0].outcome, CacheCompactOutcome::Compacted);
    assert!(report.reclaimed_bytes > 512 * 1024);
    assert!(report.results[0].vacuum_committed);
    assert_eq!(content(&database), before);
    assert_eq!(before.2, vec![17]);
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        assert_eq!(lease_before.ino(), fs::metadata(&lease).unwrap().ino());
    }
    #[cfg(not(unix))]
    assert_eq!(
        lease_before.created().ok(),
        fs::metadata(&lease).unwrap().created().ok()
    );
    assert!(
        IndexCoordination::for_database(&database)
            .try_acquire_prune_lease()
            .unwrap()
            .is_some()
    );
}

#[test]
fn compact_preview_does_not_vacuum_checkpoint_or_claim_savings() {
    let temp = tempfile::tempdir().unwrap();
    let manager = CacheManager::new(temp.path().join("managed"), 100);
    let (id, database) = fixture(&manager, temp.path());
    let writer = Connection::open(&database).unwrap();
    writer
        .execute_batch("PRAGMA wal_autocheckpoint=0; UPDATE meta SET repository_generation=43;")
        .unwrap();
    let wal = database.with_file_name(WAL_NAME);
    let main_before = fs::read(&database).unwrap();
    let wal_before = fs::read(&wal).unwrap();
    let report = manager.compact(&selected(&id, false)).unwrap();
    assert_eq!(report.results[0].outcome, CacheCompactOutcome::WouldCompact);
    assert_eq!(report.reclaimed_bytes, 0);
    assert!(!report.results[0].vacuum_committed);
    assert_eq!(fs::read(&database).unwrap(), main_before);
    assert_eq!(fs::read(&wal).unwrap(), wal_before);
}

#[test]
fn compact_skips_every_shared_lifetime_lease() {
    let temp = tempfile::tempdir().unwrap();
    let manager = CacheManager::new(temp.path().join("managed"), 100);
    let (id, database) = fixture(&manager, temp.path());
    let coordination = IndexCoordination::for_database(&database);
    let cancellation = CancellationToken::new();
    let first = coordination.acquire_cache_lease(&cancellation).unwrap();
    let second = coordination.acquire_cache_lease(&cancellation).unwrap();
    for held in [true, false] {
        let report = manager.compact(&selected(&id, true)).unwrap();
        assert!(matches!(
            report.results[0].outcome,
            CacheCompactOutcome::SkippedActive { .. }
        ));
        assert!(!report.results[0].vacuum_committed);
        if held {
            assert_eq!(content(&database).0, 42);
        }
    }
    drop(first);
    assert!(matches!(
        manager.compact(&selected(&id, true)).unwrap().results[0].outcome,
        CacheCompactOutcome::SkippedActive { .. }
    ));
    drop(second);
    assert_eq!(
        manager.compact(&selected(&id, true)).unwrap().results[0].outcome,
        CacheCompactOutcome::Compacted
    );
}

#[test]
fn compact_requires_both_benefit_thresholds_and_size_bound() {
    let temp = tempfile::tempdir().unwrap();
    let manager = CacheManager::new(temp.path().join("managed"), 100);
    let (id, database) = fixture(&manager, temp.path());
    let before = fs::read(&database).unwrap();
    let mut request = selected(&id, true);
    request.min_reclaim_bytes = u64::MAX;
    assert_eq!(
        manager.compact(&request).unwrap().results[0].outcome,
        CacheCompactOutcome::SkippedLowBenefit
    );
    request.min_reclaim_bytes = 1;
    request.min_reclaim_percent = 100;
    assert_eq!(
        manager.compact(&request).unwrap().results[0].outcome,
        CacheCompactOutcome::SkippedLowBenefit
    );
    request.min_reclaim_percent = 1;
    request.max_database_bytes = 1;
    assert_eq!(
        manager.compact(&request).unwrap().results[0].outcome,
        CacheCompactOutcome::SkippedTooLarge
    );
    assert_eq!(fs::read(database).unwrap(), before);
}

#[test]
fn compact_retains_readable_older_metadata_without_migration() {
    let temp = tempfile::tempdir().unwrap();
    let manager = CacheManager::new(temp.path().join("managed"), 100);
    let id = managed_cache_id(temp.path());
    let directory = manager.root.join(&id);
    fs::create_dir_all(&directory).unwrap();
    let database = directory.join(DATABASE_NAME);
    let connection = Connection::open(&database).unwrap();
    // A minimal older readable metadata layout, not a simulated full release schema.
    connection
        .execute_batch(
            "CREATE TABLE meta(id INTEGER PRIMARY KEY,schema_version INTEGER,repository_root TEXT,last_access_unix_seconds INTEGER);
        CREATE TABLE payload(id INTEGER PRIMARY KEY,data BLOB);
        INSERT INTO payload VALUES(1,zeroblob(1048576)); DELETE FROM payload;",
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO meta VALUES(1,4,?1,42)",
            [temp.path().to_str().unwrap()],
        )
        .unwrap();
    drop(connection);
    assert_eq!(
        manager.compact(&selected(&id, true)).unwrap().results[0].outcome,
        CacheCompactOutcome::Compacted
    );
    let connection = Connection::open(database).unwrap();
    assert_eq!(
        connection
            .query_row("SELECT schema_version FROM meta", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        4
    );
    assert_eq!(
        connection
            .pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
            .unwrap(),
        0
    );
    assert!(
        connection
            .prepare("SELECT repository_generation FROM meta")
            .is_err()
    );
}

#[test]
fn compact_fails_closed_for_external_reader_without_forcing_snapshot_release() {
    let temp = tempfile::tempdir().unwrap();
    let manager = CacheManager::new(temp.path().join("managed"), 100);
    let (id, database) = fixture(&manager, temp.path());
    let reader = Connection::open(&database).unwrap();
    reader.execute_batch("BEGIN").unwrap();
    assert_eq!(
        reader
            .query_row("SELECT repository_generation FROM meta", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        42
    );
    let writer = Connection::open(&database).unwrap();
    writer
        .execute_batch("UPDATE meta SET repository_generation=43;")
        .unwrap();
    let report = manager.compact(&selected(&id, true)).unwrap();
    assert!(report.has_failures());
    assert!(!report.results[0].vacuum_committed);
    assert_eq!(
        reader
            .query_row("SELECT repository_generation FROM meta", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        42
    );
    assert_eq!(content(&database).0, 43);
}

#[test]
fn compact_rejects_future_metadata_unexpected_artifacts_and_invalid_selectors() {
    let temp = tempfile::tempdir().unwrap();
    let manager = CacheManager::new(temp.path().join("managed"), 100);
    let (id, database) = fixture(&manager, temp.path());
    fs::write(database.parent().unwrap().join("keep.txt"), "unmanaged").unwrap();
    assert!(matches!(
        manager.compact(&selected(&id, true)).unwrap().results[0].outcome,
        CacheCompactOutcome::SkippedUnsafe { .. }
    ));
    fs::remove_file(database.parent().unwrap().join("keep.txt")).unwrap();
    let connection = Connection::open(&database).unwrap();
    connection
        .execute_batch("UPDATE meta SET repository_root=''")
        .unwrap();
    drop(connection);
    assert!(matches!(
        manager.compact(&selected(&id, true)).unwrap().results[0].outcome,
        CacheCompactOutcome::SkippedUnsafe { .. }
    ));
    let connection = Connection::open(&database).unwrap();
    connection
        .execute(
            "UPDATE meta SET repository_root=?1",
            [temp.path().to_str().unwrap()],
        )
        .unwrap();
    connection
        .execute(
            "UPDATE meta SET schema_version=?1",
            [CURRENT_SCHEMA_VERSION + 1],
        )
        .unwrap();
    drop(connection);
    assert!(matches!(
        manager.compact(&selected(&id, true)).unwrap().results[0].outcome,
        CacheCompactOutcome::SkippedUnsafe { .. }
    ));
    for ids in [
        vec![],
        vec!["../outside".into()],
        vec![id.clone(); 2],
        vec![id; 9],
    ] {
        assert!(
            manager
                .compact(&CacheCompactRequest {
                    ids,
                    ..Default::default()
                })
                .is_err()
        );
    }
}

#[cfg(unix)]
#[test]
fn compact_rejects_symlink_database_and_directory() {
    use std::os::unix::fs::symlink;
    let temp = tempfile::tempdir().unwrap();
    let manager = CacheManager::new(temp.path().join("managed"), 100);
    let (id, database) = fixture(&manager, temp.path());
    let moved = temp.path().join("external.sqlite");
    fs::rename(&database, &moved).unwrap();
    symlink(&moved, &database).unwrap();
    let before = fs::read(&moved).unwrap();
    assert!(matches!(
        manager.compact(&selected(&id, true)).unwrap().results[0].outcome,
        CacheCompactOutcome::SkippedUnsafe { .. }
    ));
    assert_eq!(fs::read(&moved).unwrap(), before);
    fs::remove_file(&database).unwrap();
    fs::rename(&moved, &database).unwrap();
    let directory = database.parent().unwrap();
    let external = temp.path().join("external");
    fs::rename(directory, &external).unwrap();
    symlink(&external, directory).unwrap();
    assert!(matches!(
        manager.compact(&selected(&id, true)).unwrap().results[0].outcome,
        CacheCompactOutcome::SkippedUnsafe { .. }
    ));
}

#[test]
fn compact_rejects_hardlinked_database_without_modifying_alias() {
    let temp = tempfile::tempdir().unwrap();
    let manager = CacheManager::new(temp.path().join("managed"), 100);
    let (id, database) = fixture(&manager, temp.path());
    let alias = temp.path().join("alias.sqlite");
    fs::hard_link(&database, &alias).unwrap();
    let before = fs::read(&alias).unwrap();
    assert!(matches!(
        manager.compact(&selected(&id, true)).unwrap().results[0].outcome,
        CacheCompactOutcome::SkippedUnsafe { .. }
    ));
    assert_eq!(fs::read(alias).unwrap(), before);
}

#[test]
fn compact_budget_and_space_failures_preserve_data_and_query_plan() {
    use crate::cache::compact_sqlite::{COMPACT_META_SQL, space_shortage_with, vacuum_with_budget};
    let temp = tempfile::tempdir().unwrap();
    let manager = CacheManager::new(temp.path().join("managed"), 100);
    let (_, database) = fixture(&manager, temp.path());
    let before = content(&database);
    let connection = Connection::open(&database).unwrap();
    let details = connection
        .prepare(&format!("EXPLAIN QUERY PLAN {COMPACT_META_SQL}"))
        .unwrap()
        .query_map([], |row| row.get::<_, String>(3))
        .unwrap()
        .collect::<std::result::Result<Vec<_>, _>>()
        .unwrap();
    assert!(
        details
            .iter()
            .any(|detail| detail.contains("SEARCH meta USING INTEGER PRIMARY KEY")),
        "{details:?}"
    );
    assert!(
        space_shortage_with(&connection, temp.path(), 1024 * 1024, |_| Ok(0))
            .unwrap()
            .is_some()
    );
    let mut committed = false;
    assert!(vacuum_with_budget(&connection, Duration::ZERO, 1, &mut committed).is_err());
    assert!(!committed);
    assert!(connection.is_autocommit());
    drop(connection);
    assert_eq!(content(&database), before);
}

#[test]
fn compact_only_inspects_explicit_ids() {
    let temp = tempfile::tempdir().unwrap();
    let manager = CacheManager::new(temp.path().join("managed"), 100);
    let one = temp.path().join("one");
    let two = temp.path().join("two");
    fs::create_dir_all(&one).unwrap();
    fs::create_dir_all(&two).unwrap();
    let (id, _) = fixture(&manager, &one);
    let (_, unselected) = fixture(&manager, &two);
    fs::write(unselected.parent().unwrap().join("unexpected"), "untouched").unwrap();
    let before = fs::read(&unselected).unwrap();
    let report = manager.compact(&selected(&id, true)).unwrap();
    assert_eq!(report.results.len(), 1);
    assert_eq!(report.results[0].id, id);
    assert_eq!(report.results[0].outcome, CacheCompactOutcome::Compacted);
    assert_eq!(fs::read(unselected).unwrap(), before);
}

#[test]
fn compact_interrupted_vacuum_rolls_back_logical_contents() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    let temp = tempfile::tempdir().unwrap();
    let manager = CacheManager::new(temp.path().join("managed"), 100);
    let (_, database) = fixture(&manager, temp.path());
    let before = content(&database);
    let connection = Connection::open(&database).unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let observed = calls.clone();
    connection
        .progress_handler(
            1,
            Some(move || observed.fetch_add(1, Ordering::Relaxed) >= 50),
        )
        .unwrap();
    let error = connection.execute_batch("VACUUM").unwrap_err();
    assert!(
        matches!(error, rusqlite::Error::SqliteFailure(ref failure, _) if failure.code == rusqlite::ErrorCode::OperationInterrupted)
    );
    connection
        .progress_handler(0, None::<fn() -> bool>)
        .unwrap();
    assert!(calls.load(Ordering::Relaxed) > 50);
    assert!(connection.is_autocommit());
    assert_eq!(
        connection
            .query_row("PRAGMA integrity_check", [], |row| row.get::<_, String>(0))
            .unwrap(),
        "ok"
    );
    drop(connection);
    assert_eq!(content(&database), before);
}

#[test]
fn compact_rejects_oversized_schema_fingerprints_before_vacuum() {
    let temp = tempfile::tempdir().unwrap();
    let manager = CacheManager::new(temp.path().join("managed"), 100);
    let (id, database) = fixture(&manager, temp.path());
    let before = content(&database);
    let connection = Connection::open(&database).unwrap();
    let columns = (0..512)
        .map(|id| format!("column_with_a_long_name_for_a_size_bound_{id} TEXT"))
        .collect::<Vec<_>>()
        .join(",");
    connection
        .execute_batch(&format!("CREATE TABLE large_schema({columns})"))
        .unwrap();
    drop(connection);
    let report = manager.compact(&selected(&id, true)).unwrap();
    assert!(report.has_failures());
    assert!(
        matches!(&report.results[0].outcome, CacheCompactOutcome::Failed { error } if error.contains("cache fingerprint value exceeds 16 KiB"))
    );
    assert!(!report.results[0].vacuum_committed);
    assert_eq!(content(&database), before);
}

#[test]
fn compact_post_validation_failure_keeps_commit_flag_and_data() {
    use crate::cache::compact_sqlite::vacuum_with_budget;
    use rusqlite::hooks::{AuthAction, AuthContext, Authorization};
    let temp = tempfile::tempdir().unwrap();
    let manager = CacheManager::new(temp.path().join("managed"), 100);
    let (_, database) = fixture(&manager, temp.path());
    let before = content(&database);
    let connection = Connection::open(&database).unwrap();
    let mut checks = 0;
    connection
        .authorizer(Some(move |context: AuthContext<'_>| {
            if matches!(
                context.action,
                AuthAction::Pragma {
                    pragma_name: "quick_check",
                    ..
                }
            ) {
                checks += 1;
                if checks > 1 {
                    return Authorization::Deny;
                }
            }
            Authorization::Allow
        }))
        .unwrap();
    let mut committed = false;
    assert!(
        vacuum_with_budget(&connection, Duration::from_secs(10), 10000, &mut committed).is_err()
    );
    assert!(
        committed,
        "a later validation failure must not imply VACUUM rollback"
    );
    drop(connection);
    assert_eq!(content(&database), before);
    let connection = Connection::open(database).unwrap();
    assert_eq!(
        connection
            .query_row("PRAGMA integrity_check", [], |row| row.get::<_, String>(0))
            .unwrap(),
        "ok"
    );
}

#[test]
fn compact_preserves_mtime_based_retention_by_skipping_legacy_cache() {
    let temp = tempfile::tempdir().unwrap();
    let manager = CacheManager::new(temp.path().join("managed"), 10000);
    let (id, database) = fixture(&manager, temp.path());
    let connection = Connection::open(&database).unwrap();
    connection
        .execute_batch("ALTER TABLE meta DROP COLUMN last_access_unix_seconds")
        .unwrap();
    drop(connection);
    fs::OpenOptions::new()
        .write(true)
        .open(&database)
        .unwrap()
        .set_times(fs::FileTimes::new().set_modified(UNIX_EPOCH + Duration::from_secs(100)))
        .unwrap();
    let before = manager
        .inspect_managed_cache(&id, parse_managed_cache_id(&id).unwrap(), true)
        .unwrap()
        .entry;
    assert_eq!(before.access_time_source, Some(AccessTimeSource::FileMtime));
    for apply in [false, true] {
        let report = manager.compact(&selected(&id, apply)).unwrap();
        assert!(matches!(
            report.results[0].outcome,
            CacheCompactOutcome::SkippedUnsafe { .. }
        ));
        assert!(!report.results[0].vacuum_committed);
    }
    let after = manager
        .inspect_managed_cache(&id, parse_managed_cache_id(&id).unwrap(), true)
        .unwrap()
        .entry;
    assert_eq!(
        before.last_access_unix_seconds,
        after.last_access_unix_seconds
    );
    assert_eq!(before.age_seconds, after.age_seconds);
    assert_eq!(content(&database).0, 42);
}

#[test]
fn compact_aggregates_shared_volume_space_requirements() {
    use crate::cache::compact_sqlite::{same_volume, space_shortage_from};
    let temp = tempfile::tempdir().unwrap();
    assert!(same_volume(temp.path(), temp.path()).unwrap());
    let mib = 1024 * 1024;
    // Individually sufficient volumes are insufficient when sharing one pool.
    assert!(space_shortage_from(temp.path(), mib, 34 * mib, 33 * mib, false).is_none());
    assert!(space_shortage_from(temp.path(), mib, 34 * mib, 34 * mib, true).is_some());
    assert!(space_shortage_from(temp.path(), mib, 67 * mib, 67 * mib, true).is_none());
    assert!(space_shortage_from(temp.path(), mib, 67 * mib, 67 * mib - 1, true).is_some());
}

#[test]
fn compact_human_diagnostics_match_machine_actions() {
    let outcomes = [
        CacheCompactOutcome::WouldCompact,
        CacheCompactOutcome::Compacted,
        CacheCompactOutcome::SkippedActive {
            detail: "held".into(),
        },
        CacheCompactOutcome::SkippedUnsafe {
            detail: "ownership".into(),
        },
        CacheCompactOutcome::SkippedLowBenefit,
        CacheCompactOutcome::SkippedTooLarge,
        CacheCompactOutcome::SkippedInsufficientSpace {
            detail: "headroom".into(),
        },
        CacheCompactOutcome::Failed {
            error: "interrupted".into(),
        },
    ];
    for outcome in outcomes {
        let wire = serde_json::to_value(&outcome).unwrap();
        assert_eq!(wire["action"].as_str(), Some(outcome.label()));
        let diagnostic = wire
            .get("detail")
            .or_else(|| wire.get("error"))
            .and_then(serde_json::Value::as_str);
        assert_eq!(diagnostic, outcome.diagnostic());
    }
}
