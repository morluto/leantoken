use super::*;

#[test]
fn corrupt_and_legacy_caches_are_listed_without_mutation() {
    let temp = tempfile::tempdir().expect("temporary directory");
    let root = temp.path().join("managed");
    let corrupt = root.join(FIRST_ID);
    fs::create_dir_all(&corrupt).expect("corrupt directory");
    fs::write(corrupt.join(DATABASE_NAME), b"not sqlite").expect("corrupt database");
    let legacy = root.join(SECOND_ID);
    fs::create_dir_all(&legacy).expect("legacy directory");
    let connection = Connection::open(legacy.join(DATABASE_NAME)).expect("legacy database");
    connection
        .execute_batch(
            "CREATE TABLE meta (
                    id INTEGER PRIMARY KEY,
                    schema_version INTEGER NOT NULL,
                    repository_root TEXT NOT NULL
                );
                INSERT INTO meta VALUES (1, 4, '');",
        )
        .expect("legacy schema");
    drop(connection);
    let manager = CacheManager::new(root, 10_000);

    let report = manager
        .list_with(&CacheListRequest::default())
        .expect("cache list");

    assert_eq!(report.entries[0].entry.state, CacheState::Corrupt);
    assert_eq!(report.entries[1].entry.state, CacheState::OlderSchema);
    assert!(corrupt.join(DATABASE_NAME).exists());
    assert!(legacy.join(DATABASE_NAME).exists());

    let mut prune = request();
    prune.max_total_bytes = Some(0);
    let plan = manager.prune(&prune).expect("prune plan");
    assert_eq!(plan.results[0].action, CachePruneAction::Kept);
    assert_eq!(plan.results[1].action, CachePruneAction::WouldDelete);
    assert!(corrupt.join(DATABASE_NAME).exists());
    assert!(legacy.join(DATABASE_NAME).exists());
}

#[cfg(unix)]
#[test]
fn prune_rejects_cache_directory_replaced_with_symlink() {
    use std::os::unix::fs::symlink;

    let temp = tempfile::tempdir().expect("temporary directory");
    let repository = temp.path().join("repository");
    fs::create_dir(&repository).expect("repository");
    let manager = CacheManager::new(temp.path().join("managed"), 10_000);
    let (id, database) = create_current_cache(&manager, &repository, 100);
    let inspected = manager
        .inspect_cache(&id, false)
        .expect("inspect cache before replacement");
    assert!(inspected.safe_to_prune);

    let external = temp.path().join("external");
    fs::create_dir(&external).expect("external directory");
    let external_database = external.join(DATABASE_NAME);
    fs::write(&external_database, b"must remain").expect("external database");
    let cache_directory = database.parent().expect("cache directory").to_owned();
    let displaced_directory = temp.path().join("displaced-cache");
    fs::rename(&cache_directory, &displaced_directory).expect("displace managed cache directory");
    symlink(&external, cache_directory).expect("replace cache with symlink");

    let removal = remove_managed_artifacts(&inspected.entry.path);

    assert!(removal.error.is_some());
    assert_eq!(
        fs::read(&external_database).expect("external database"),
        b"must remain"
    );
}

#[test]
fn legacy_wal_list_keeps_file_mtime_access_age_stable() {
    let temp = tempfile::tempdir().expect("temporary directory");
    let manager = CacheManager::new(temp.path().join("managed"), 20 * SECONDS_PER_DAY);
    create_legacy_wal_cache(&manager, FIRST_ID, SECONDS_PER_DAY);

    let first = manager
        .list_with(&CacheListRequest::default())
        .expect("first cache list");
    let second = manager
        .list_with(&CacheListRequest::default())
        .expect("second cache list");

    assert_eq!(first.entries[0].entry.state, CacheState::OlderSchema);
    assert_eq!(
        first.entries[0].entry.access_time_source,
        Some(AccessTimeSource::FileMtime)
    );
    assert_eq!(
        first.entries[0].entry.last_access_unix_seconds,
        Some(SECONDS_PER_DAY)
    );
    assert_eq!(
        second.entries[0].entry.last_access_unix_seconds,
        first.entries[0].entry.last_access_unix_seconds
    );
    assert_eq!(
        second.entries[0].entry.age_seconds,
        first.entries[0].entry.age_seconds
    );
}

#[test]
fn legacy_wal_dry_run_keeps_age_selection_stable() {
    let temp = tempfile::tempdir().expect("temporary directory");
    let manager = CacheManager::new(temp.path().join("managed"), 20 * SECONDS_PER_DAY);
    create_legacy_wal_cache(&manager, FIRST_ID, SECONDS_PER_DAY);
    let mut request = request();
    request.older_than_days = Some(7);

    let first = manager.prune(&request).expect("first prune plan");
    let second = manager.prune(&request).expect("second prune plan");

    assert_eq!(first.results[0].action, CachePruneAction::WouldDelete);
    assert_eq!(first.results[0].reasons, ["older_than"]);
    assert_eq!(second.results[0], first.results[0]);
}

#[test]
fn unexpected_content_is_never_removed_automatically() {
    let temp = tempfile::tempdir().expect("temporary directory");
    let directory = temp.path().join("managed").join(FIRST_ID);
    fs::create_dir_all(&directory).expect("cache directory");
    fs::write(directory.join(DATABASE_NAME), b"not sqlite").expect("database");
    fs::write(directory.join("keep.txt"), b"owner data").expect("unexpected file");
    let manager = CacheManager::new(temp.path().join("managed"), 10_000);
    let mut prune = request();
    prune.max_total_bytes = Some(1);
    prune.dry_run = false;
    prune.yes = true;

    let report = manager.prune(&prune).expect("prune");

    assert_eq!(report.results[0].action, CachePruneAction::Kept);
    assert!(report.results[0].reasons.is_empty());
    assert!(directory.join(DATABASE_NAME).exists());
    assert!(directory.join("keep.txt").exists());
}

#[test]
fn future_schema_and_mismatched_identity_are_never_removed_automatically() {
    let temp = tempfile::tempdir().expect("temporary directory");
    let root = temp.path().join("managed");
    let future_root = temp.path().join("future-repository");
    let mismatch_root = temp.path().join("mismatch-repository");
    fs::create_dir(&future_root).expect("future repository");
    fs::create_dir(&mismatch_root).expect("mismatch repository");
    let manager = CacheManager::new(root, 10_000);
    let (future_id, future_database) = create_current_cache(&manager, &future_root, 100);
    Connection::open(&future_database)
        .expect("future database")
        .execute(
            "UPDATE meta SET schema_version = ?1, repository_root = x'80' WHERE id = 1",
            [CURRENT_SCHEMA_VERSION + 1],
        )
        .expect("future schema");
    let mismatch_id = FIRST_ID;
    assert_ne!(mismatch_id, managed_cache_id(&mismatch_root));
    let mismatch_directory = manager.root.join(mismatch_id);
    fs::create_dir_all(&mismatch_directory).expect("mismatch directory");
    let mismatch_database = mismatch_directory.join(DATABASE_NAME);
    drop(
        Storage::open_for_repository(&mismatch_database, &mismatch_root)
            .expect("mismatch database"),
    );
    let future_migration_id = SECOND_ID;
    let future_migration_directory = manager.root.join(future_migration_id);
    fs::create_dir_all(&future_migration_directory).expect("future migration directory");
    let future_migration_database = future_migration_directory.join(DATABASE_NAME);
    Connection::open(&future_migration_database)
        .expect("future migration database")
        .execute_batch(&format!(
            "PRAGMA user_version = {}; CREATE TABLE replacement(value INTEGER);",
            CURRENT_MIGRATION_VERSION + 1
        ))
        .expect("future migration");
    let mut prune = request();
    prune.max_total_bytes = Some(0);
    prune.dry_run = false;
    prune.yes = true;

    let report = manager.prune(&prune).expect("prune plan");

    let future = report
        .results
        .iter()
        .find(|result| result.id == future_id)
        .expect("future result");
    let mismatch = report
        .results
        .iter()
        .find(|result| result.id == mismatch_id)
        .expect("mismatch result");
    let future_migration = report
        .results
        .iter()
        .find(|result| result.id == future_migration_id)
        .expect("future migration result");
    assert_eq!(future.action, CachePruneAction::Kept);
    assert_eq!(mismatch.action, CachePruneAction::Kept);
    assert_eq!(future_migration.action, CachePruneAction::Kept);
    assert!(future_database.exists());
    assert!(mismatch_database.exists());
    assert!(future_migration_database.exists());
}

#[test]
fn future_index_content_cache_is_visible_but_never_removed() {
    let temp = tempfile::tempdir().expect("temporary directory");
    let root = temp.path().join("managed");
    let repository = temp.path().join("repository");
    fs::create_dir(&repository).expect("repository");
    let current_id = managed_cache_id(&repository);
    let root_hash = current_id.split_once('-').expect("versioned identity").1;
    let future_id = format!("v{}-{root_hash}", INDEX_CONTENT_VERSION + 1);
    let directory = root.join(&future_id);
    fs::create_dir_all(&directory).expect("future cache directory");
    let database = directory.join(DATABASE_NAME);
    drop(
        Storage::open_for_repository(&database, &repository)
            .expect("future cache database fixture"),
    );
    let manager = CacheManager::new(root, 10_000);

    let listed = manager
        .list_with(&CacheListRequest::default())
        .expect("cache list");

    assert_eq!(listed.entries.len(), 1);
    assert_eq!(listed.entries[0].entry.id, future_id);
    assert_eq!(
        listed.entries[0].entry.index_content_version,
        Some(INDEX_CONTENT_VERSION + 1)
    );
    assert_eq!(listed.entries[0].entry.state, CacheState::Unsupported);
    assert_eq!(
        listed.entries[0].entry.detail.as_deref(),
        Some("cache uses a newer index-content version")
    );

    let mut request = request();
    request.max_total_bytes = Some(0);
    let pruned = manager.prune(&request).expect("future cache prune plan");
    assert_eq!(pruned.results[0].action, CachePruneAction::Kept);
    assert!(database.exists());
}
