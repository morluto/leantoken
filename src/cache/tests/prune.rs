use super::*;

#[test]
fn incompatible_prune_is_dry_run_first_and_fail_closed() {
    let temp = tempfile::tempdir().expect("temporary directory");
    let manager = CacheManager::new(temp.path().join("managed"), 10_000);
    let repositories = (0..4)
        .map(|index| {
            let repository = temp.path().join(format!("repository-{index}"));
            fs::create_dir(&repository).expect("repository");
            repository
        })
        .collect::<Vec<_>>();
    let (_, current_database) = create_current_cache(&manager, &repositories[0], 9_000);
    let (older_id, older_database) = create_cache_with_content_identity(
        &manager,
        &repositories[1],
        8_000,
        Some(INDEX_CONTENT_VERSION - 1),
    );
    let (legacy_id, legacy_database) =
        create_cache_with_content_identity(&manager, &repositories[2], 7_000, None);
    let (_, future_database) = create_cache_with_content_identity(
        &manager,
        &repositories[3],
        6_000,
        Some(INDEX_CONTENT_VERSION + 1),
    );
    let corrupt = manager.root.join(FIRST_ID).join(DATABASE_NAME);
    fs::create_dir_all(corrupt.parent().expect("corrupt directory"))
        .expect("corrupt cache directory");
    fs::write(&corrupt, b"not sqlite").expect("corrupt database");
    let dry_run = CachePruneV2Request {
        request: request(),
        incompatible_with_current: true,
    };

    let plan = manager.prune_v2(&dry_run).expect("incompatible dry run");

    for id in [&older_id, &legacy_id] {
        let result = plan
            .results
            .iter()
            .find(|result| &result.id == id)
            .expect("incompatible result");
        assert_eq!(result.action, CachePruneAction::WouldDelete);
        assert_eq!(result.reasons.len(), 1);
        assert!(result.reasons[0].starts_with("incompatible_with_current:"));
    }
    assert!(
        plan.results
            .iter()
            .filter(|result| result.id != older_id && result.id != legacy_id)
            .all(|result| result.action == CachePruneAction::Kept)
    );
    for database in [
        &current_database,
        &older_database,
        &legacy_database,
        &future_database,
        &corrupt,
    ] {
        assert!(database.exists(), "dry run removed {}", database.display());
    }

    let applied = manager
        .prune_v2(&CachePruneV2Request {
            request: CachePruneRequest {
                dry_run: false,
                yes: true,
                ..request()
            },
            incompatible_with_current: true,
        })
        .expect("apply incompatible prune");
    for id in [&older_id, &legacy_id] {
        assert_eq!(
            applied
                .results
                .iter()
                .find(|result| &result.id == id)
                .expect("deleted incompatible result")
                .action,
            CachePruneAction::Deleted
        );
    }
    assert!(!older_database.exists());
    assert!(!legacy_database.exists());
    assert!(current_database.exists());
    assert!(future_database.exists());
    assert!(corrupt.exists());
}

#[test]
fn incompatible_prune_never_projects_an_active_cache_as_reclaimable() {
    let temp = tempfile::tempdir().expect("temporary directory");
    let repository = temp.path().join("repository");
    fs::create_dir(&repository).expect("repository");
    let repository = repository.canonicalize().expect("canonical repository");
    let manager = CacheManager::new(temp.path().join("managed"), 10_000);
    let (_, database) = create_cache_with_content_identity(
        &manager,
        &repository,
        9_000,
        Some(INDEX_CONTENT_VERSION - 1),
    );
    let config =
        Config::discover(&repository, Some(database.clone())).expect("active cache config");
    let services = Services::open(config).expect("active cache service");
    let request = CachePruneV2Request {
        request: request(),
        incompatible_with_current: true,
    };

    let listed = manager
        .list_v2_with(&CacheListV2Request::default())
        .expect("active compatibility summary");
    assert_eq!(listed.compatibility_counts["obsolete_older"].entries, 1);
    assert_eq!(listed.safely_reclaimable_incompatible_entries, 0);
    assert_eq!(listed.safely_reclaimable_incompatible_bytes, 0);

    let active = manager.prune_v2(&request).expect("active prune plan");
    assert_eq!(active.results[0].action, CachePruneAction::SkippedActive);
    assert!(database.exists());

    drop(services);
    let inactive = manager.prune_v2(&request).expect("inactive prune plan");
    assert_eq!(inactive.results[0].action, CachePruneAction::WouldDelete);
    assert!(database.exists());
}

#[test]
fn stale_cache_is_deleted_after_age_and_confirmation() {
    let temp = tempfile::tempdir().expect("temporary directory");
    let repository = temp.path().join("repository");
    fs::create_dir(&repository).expect("repository");
    let manager = CacheManager::new(temp.path().join("managed"), 40 * SECONDS_PER_DAY);
    let (id, database) = create_current_cache(&manager, &repository, SECONDS_PER_DAY);
    let mut prune = request();
    prune.older_than_days = Some(30);
    prune.dry_run = false;
    prune.yes = true;

    let report = manager.prune(&prune).expect("prune stale cache");

    let result = report
        .results
        .iter()
        .find(|result| result.id == id)
        .expect("stale result");
    assert_eq!(result.action, CachePruneAction::Deleted);
    assert_eq!(result.reasons, ["older_than"]);
    assert!(!database.exists());
    assert!(database.with_extension("sqlite.lease.lock").exists());
}

#[test]
fn explicit_database_outside_managed_root_is_never_considered() {
    let temp = tempfile::tempdir().expect("temporary directory");
    let repository = temp.path().join("repository");
    fs::create_dir(&repository).expect("repository");
    let explicit = temp.path().join("explicit.sqlite");
    let config = Config::discover(&repository, Some(explicit.clone())).expect("config");
    drop(Services::open(config).expect("services"));
    let manager = CacheManager::new(temp.path().join("managed"), 10_000);

    assert!(manager.list().expect("cache list").entries.is_empty());
    let mut prune = request();
    prune.max_total_bytes = Some(1);
    assert!(manager.prune(&prune).expect("prune").results.is_empty());
    assert!(explicit.exists());
}

#[test]
fn prune_requires_an_explicit_policy_and_mutation_consent() {
    let temp = tempfile::tempdir().expect("temporary directory");
    let manager = CacheManager::new(temp.path().join("managed"), 10_000);
    let empty = request();
    assert!(
        manager
            .prune(&empty)
            .unwrap_err()
            .to_string()
            .contains("requires --older-than")
    );

    let mut mutation = request();
    mutation.max_total_bytes = Some(1);
    mutation.dry_run = false;
    assert!(
        manager
            .prune(&mutation)
            .unwrap_err()
            .to_string()
            .contains("requires --yes")
    );
}
