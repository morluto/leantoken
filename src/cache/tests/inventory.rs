use super::super::inventory::{InventoryBudget, InventoryLimits};
use super::*;

fn assert_incomplete(error: Error, expected: &str) {
    match error {
        Error::CacheInventoryIncomplete {
            dimension,
            observed,
            limit,
        } => {
            assert_eq!(dimension, expected);
            assert!(observed > limit);
        }
        other => panic!("expected incomplete {expected} inventory, got {other:?}"),
    }
}

// Small injected quotas exercise the production admission paths without building
// a default-size filesystem or database population in ordinary CI.
#[test]
fn inventory_admission_limits_prevent_partial_list_and_prune() {
    let temp = tempfile::tempdir().unwrap();
    let mut manager = CacheManager::new(temp.path().join("managed"), 10_000);
    let mut databases = Vec::new();
    for index in 0..2 {
        let repository = temp.path().join(format!("repository-{index}"));
        fs::create_dir(&repository).unwrap();
        let (_, database) = create_current_cache(&manager, &repository, 9_000);
        let before = fs::read(&database).unwrap();
        databases.push((database, before));
    }
    let defaults = InventoryLimits::default();
    for (dimension, limits) in [
        (
            "root_entries",
            InventoryLimits {
                root_entries: 1,
                ..defaults
            },
        ),
        (
            "candidate_caches",
            InventoryLimits {
                candidates: 1,
                ..defaults
            },
        ),
        (
            "artifact_entries",
            InventoryLimits {
                artifacts: 1,
                ..defaults
            },
        ),
        (
            "sqlite_opens",
            InventoryLimits {
                sqlite_opens: 1,
                ..defaults
            },
        ),
        (
            "metadata_bytes",
            InventoryLimits {
                metadata_bytes: 1,
                ..defaults
            },
        ),
    ] {
        manager.inventory_limits = limits;
        assert_incomplete(
            manager
                .list_with(&CacheListRequest {
                    limit: 1,
                    ..Default::default()
                })
                .unwrap_err(),
            dimension,
        );
        let mut prune = request();
        prune.max_total_bytes = Some(0);
        prune.dry_run = false;
        prune.yes = true;
        assert_incomplete(manager.prune(&prune).unwrap_err(), dimension);
        for (database, before) in &databases {
            assert_eq!(
                fs::read(database).unwrap(),
                *before,
                "overflow must precede every removal"
            );
        }
    }
}

#[test]
fn inventory_artifact_limit_counts_both_safety_passes() {
    let temp = tempfile::tempdir().unwrap();
    let mut manager = CacheManager::new(temp.path().join("managed"), 0);
    let directory = manager.root.join(FIRST_ID);
    fs::create_dir_all(&directory).unwrap();
    fs::write(directory.join(DATABASE_NAME), "corrupt fixture").unwrap();
    manager.inventory_limits.artifacts = 1;
    let mut budget = InventoryBudget::new(&manager);
    assert_incomplete(
        manager
            .inspect_managed_cache_with_budget(
                FIRST_ID,
                parse_managed_cache_id(FIRST_ID).unwrap(),
                true,
                &mut budget,
            )
            .unwrap_err(),
        "artifact_entries",
    );
    assert_eq!(budget.sqlite_opens, 1);
    assert_eq!(budget.artifacts, 2);
}

#[test]
fn inventory_sqlite_progress_interrupts_expensive_metadata() {
    let temp = tempfile::tempdir().unwrap();
    let mut manager = CacheManager::new(temp.path().join("managed"), 0);
    let directory = manager.root.join(FIRST_ID);
    fs::create_dir_all(&directory).unwrap();
    let database = directory.join(DATABASE_NAME);
    Connection::open(&database).unwrap().execute_batch(&format!(
        "CREATE VIEW meta AS WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<1000000) SELECT 1 AS id, {CURRENT_SCHEMA_VERSION} AS schema_version FROM n WHERE x=1000000;"
    )).unwrap();
    manager.inventory_limits.sqlite_steps = 500;
    assert_incomplete(
        manager.list_with(&CacheListRequest::default()).unwrap_err(),
        "sqlite_steps",
    );
    assert!(database.exists());
}

#[test]
fn inventory_revalidation_shares_sqlite_admission_between_caches() {
    let temp = tempfile::tempdir().unwrap();
    let mut manager = CacheManager::new(temp.path().join("managed"), 10_000);
    let mut ids = Vec::new();
    for index in 0..2 {
        let repository = temp.path().join(format!("repository-{index}"));
        fs::create_dir(&repository).unwrap();
        ids.push(create_current_cache(&manager, &repository, 9_000).0);
    }
    manager.inventory_limits.sqlite_opens = 1;
    let mut budget = InventoryBudget::new(&manager);
    manager
        .inspect_managed_cache_with_budget(
            &ids[0],
            parse_managed_cache_id(&ids[0]).unwrap(),
            false,
            &mut budget,
        )
        .unwrap();
    assert_incomplete(
        manager
            .inspect_managed_cache_with_budget(
                &ids[1],
                parse_managed_cache_id(&ids[1]).unwrap(),
                false,
                &mut budget,
            )
            .unwrap_err(),
        "sqlite_opens",
    );
    assert_eq!(budget.sqlite_opens, 2);
}

#[test]
fn inventory_rejects_oversized_metadata_instead_of_reporting_complete_corruption() {
    let temp = tempfile::tempdir().unwrap();
    let repository = temp.path().join("repository");
    fs::create_dir(&repository).unwrap();
    let manager = CacheManager::new(temp.path().join("managed"), 10_000);
    let (_, database) = create_current_cache(&manager, &repository, 9_000);
    for (bytes, dimension) in [
        (8193, "repository_root_bytes"),
        (100_000, "sqlite_value_bytes"),
    ] {
        Connection::open(&database)
            .unwrap()
            .execute(
                "UPDATE meta SET repository_root=?1 WHERE id=1",
                ["x".repeat(bytes)],
            )
            .unwrap();
        assert_incomplete(
            manager.list_with(&CacheListRequest::default()).unwrap_err(),
            dimension,
        );
    }
}

#[test]
fn cancelled_inventory_does_not_authorize_pruning() {
    let temp = tempfile::tempdir().unwrap();
    let repository = temp.path().join("repository");
    fs::create_dir(&repository).unwrap();
    let manager = CacheManager::new(temp.path().join("managed"), 10_000);
    let (_, database) = create_current_cache(&manager, &repository, 9_000);
    let before = fs::read(&database).unwrap();
    manager.cancellation.cancel();
    assert!(matches!(
        manager.list_with(&CacheListRequest::default()),
        Err(Error::Cancelled)
    ));
    let mut prune = request();
    prune.max_total_bytes = Some(0);
    prune.dry_run = false;
    prune.yes = true;
    assert!(matches!(manager.prune(&prune), Err(Error::Cancelled)));
    assert_eq!(fs::read(database).unwrap(), before);
}
