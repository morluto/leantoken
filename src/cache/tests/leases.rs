use super::*;

#[test]
fn active_service_clones_block_prune_until_every_lease_is_dropped() {
    let temp = tempfile::tempdir().expect("temporary directory");
    let root = temp.path().join("managed");
    let repository = temp.path().join("repository");
    fs::create_dir(&repository).expect("repository");
    let repository = fs::canonicalize(repository).expect("canonical repository");
    let directory = root.join(managed_cache_id(&repository));
    fs::create_dir_all(&directory).expect("cache directory");
    let database = directory.join(DATABASE_NAME);
    let config = Config::discover(&repository, Some(database.clone())).expect("config");
    let services = Services::open(config).expect("services");
    let follower = services.clone();
    let manager = CacheManager::new(root, unix_seconds(SystemTime::now()));
    let mut prune = request();
    prune.max_total_bytes = Some(1);
    prune.dry_run = false;
    prune.yes = true;

    let first = manager.prune(&prune).expect("active prune");
    assert_eq!(first.results[0].action, CachePruneAction::Kept);
    assert!(database.exists());
    drop(services);
    let second = manager.prune(&prune).expect("follower prune");
    assert_eq!(second.results[0].action, CachePruneAction::Kept);
    drop(follower);

    let deleted = manager.prune(&prune).expect("inactive prune");
    assert_eq!(
        deleted.results[0].action,
        CachePruneAction::Deleted,
        "unexpected prune report: {deleted:#?}"
    );
    assert!(!database.exists());
    assert!(coordination_sidecar_path(&database, LEASE_LOCK_SUFFIX).exists());
    assert!(
        manager
            .list_with(&CacheListRequest::default())
            .expect("empty list")
            .entries
            .is_empty()
    );
}

#[test]
fn missing_repository_requires_age_or_explicit_override() {
    let temp = tempfile::tempdir().expect("temporary directory");
    let repository = temp.path().join("offline-repository");
    fs::create_dir(&repository).expect("repository");
    let manager = CacheManager::new(temp.path().join("managed"), 10 * SECONDS_PER_DAY);
    create_current_cache(&manager, &repository, 9 * SECONDS_PER_DAY);
    fs::remove_dir(&repository).expect("take repository offline");

    let mut age_only = request();
    age_only.older_than_days = Some(30);
    let kept = manager.prune(&age_only).expect("age plan");
    assert_eq!(kept.results[0].action, CachePruneAction::Kept);

    age_only.remove_missing_roots = true;
    let selected = manager.prune(&age_only).expect("missing-root plan");
    assert_eq!(selected.results[0].action, CachePruneAction::WouldDelete);
    assert_eq!(selected.results[0].reasons, ["missing_repository"]);
}

#[test]
fn lru_budget_selects_oldest_cache_and_dry_run_preserves_files() {
    let temp = tempfile::tempdir().expect("temporary directory");
    let first_root = temp.path().join("first-repository");
    let second_root = temp.path().join("second-repository");
    fs::create_dir(&first_root).expect("first repository");
    fs::create_dir(&second_root).expect("second repository");
    let manager = CacheManager::new(temp.path().join("managed"), 1_000);
    let (first_id, first) = create_current_cache(&manager, &first_root, 100);
    let (second_id, second) = create_current_cache(&manager, &second_root, 900);
    let listed = manager
        .list_with(&CacheListRequest::default())
        .expect("cache list");
    let oldest_size = listed
        .entries
        .iter()
        .find(|entry| entry.entry.id == first_id)
        .expect("oldest cache")
        .entry
        .size_bytes;
    let mut prune = request();
    prune.max_total_bytes = Some(listed.total_bytes - oldest_size);

    let report = manager.prune(&prune).expect("LRU plan");

    assert_eq!(report.total_bytes_before, listed.total_bytes);
    let first_result = report
        .results
        .iter()
        .find(|result| result.id == first_id)
        .expect("oldest result");
    let second_result = report
        .results
        .iter()
        .find(|result| result.id == second_id)
        .expect("newest result");
    assert_eq!(first_result.action, CachePruneAction::WouldDelete);
    assert_eq!(first_result.reasons, ["max_total_bytes"]);
    assert_eq!(second_result.action, CachePruneAction::Kept);
    assert_eq!(report.reclaimed_bytes, oldest_size);
    assert!(first.exists());
    assert!(second.exists());
}
