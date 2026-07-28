use super::*;


#[test]
fn services_reject_database_owned_by_another_repository() {
    let first_root = tempfile::tempdir().expect("first root");
    let second_root = tempfile::tempdir().expect("second root");
    let cache = tempfile::tempdir().expect("cache");
    let database = cache.path().join("shared.sqlite");

    let first_config =
        Config::discover(first_root.path(), Some(database.clone())).expect("first config");
    let first = Services::open(first_config).expect("claim database");
    let second_config =
        Config::discover(second_root.path(), Some(database.clone())).expect("second config");
    let error = Services::open(second_config).expect_err("different root must be rejected");

    assert!(matches!(error, Error::RepositoryMismatch { .. }));
    drop(first);
    Services::open(
        Config::discover(first_root.path(), Some(database)).expect("same-root config"),
    )
    .expect("same root may share database");
}

#[tokio::test]
async fn same_repository_services_share_committed_generations() {
    let root = tempfile::tempdir().expect("root");
    std::fs::write(root.path().join("lib.rs"), "fn shared() {}\n").expect("source");
    let database = root.path().join("index.sqlite");
    let first = Services::open(
        Config::discover(root.path(), Some(database.clone())).expect("first config"),
    )
    .expect("first services");
    let second = Services::open(
        Config::discover(root.path(), Some(database)).expect("second config"),
    )
    .expect("second services");

    let indexed = first.index(false).await.expect("index");
    let observed = second.status().await.expect("follower status");

    assert_eq!(observed.repository_generation, indexed.repository_generation);
}

#[tokio::test]
async fn independent_repositories_index_concurrently_without_result_leakage() {
    let first_root = tempfile::tempdir().expect("first root");
    let second_root = tempfile::tempdir().expect("second root");
    let cache = tempfile::tempdir().expect("cache");
    std::fs::write(first_root.path().join("first.rs"), "fn alpha_only() {}\n")
        .expect("first source");
    std::fs::write(second_root.path().join("second.rs"), "fn beta_only() {}\n")
        .expect("second source");
    let first = Services::open(
        Config::discover(first_root.path(), Some(cache.path().join("first.sqlite")))
            .expect("first config"),
    )
    .expect("first services");
    let second = Services::open(
        Config::discover(second_root.path(), Some(cache.path().join("second.sqlite")))
            .expect("second config"),
    )
    .expect("second services");

    let (first_index, second_index) = tokio::join!(first.index(false), second.index(false));
    first_index.expect("first index");
    second_index.expect("second index");
    let first_status = first.status().await.expect("first status");
    let second_status = second.status().await.expect("second status");

    assert_eq!(first_status.file_count, 1);
    assert_eq!(second_status.file_count, 1);
    assert_ne!(first.config().database_path, second.config().database_path);
    assert_ne!(first.repository_id(), second.repository_id());
}

#[cfg(unix)]
#[tokio::test]
async fn repository_identity_is_stable_across_symlink_aliases() {
    let root = tempfile::tempdir().expect("root");
    let aliases = tempfile::tempdir().expect("aliases");
    let alias = aliases.path().join("repository");
    std::os::unix::fs::symlink(root.path(), &alias).expect("symlink root");
    let first = Services::open(
        Config::discover(root.path(), Some(root.path().join("first.sqlite"))).expect("root config"),
    )
    .expect("root services");
    let second = Services::open(
        Config::discover(&alias, Some(root.path().join("second.sqlite"))).expect("alias config"),
    )
    .expect("alias services");

    assert_eq!(first.repository_id(), second.repository_id());
}

#[cfg(unix)]
#[tokio::test]
async fn index_excludes_database_below_missing_symlinked_parent() {
    let root = tempfile::tempdir().expect("root");
    let aliases = tempfile::tempdir().expect("aliases");
    let alias = aliases.path().join("repository");
    std::os::unix::fs::symlink(root.path(), &alias).expect("symlink root");
    std::fs::write(root.path().join("lib.rs"), "fn source() {}\n").expect("source");

    let config = Config::discover(
        root.path(),
        Some(alias.join("missing/cache/index.sqlite")),
    )
    .expect("config");
    let services = Services::open(config).expect("services");
    services.index(false).await.expect("index");

    let files = services
        .files(FilesRequest {
            operation: FileOperation::Tree,
            path: None,
            query: None,
            pattern: None,
            max_results: Some(100),
            cursor: None,
            depth: Some(8),
        })
        .await
        .expect("files");
    assert!(files.entries.iter().any(|entry| entry.path == "lib.rs"));
    assert!(
        files
            .entries
            .iter()
            .all(|entry| !entry.path.starts_with("missing/cache/index.sqlite")),
        "database artifacts leaked into the index: {:?}",
        files.entries
    );
}

#[tokio::test]
async fn database_artifact_notifications_do_not_publish_a_generation() {
    let (_root, services) = fixture().await;
    let before = services
        .status()
        .await
        .expect("status before artifacts")
        .repository_generation;

    let response = services
        .index_paths_report(vec![
            "index.sqlite".into(),
            "index.sqlite-wal".into(),
            "index.sqlite-shm".into(),
        ])
        .await
        .expect("ignore database artifacts");

    assert_eq!(response.repository_generation, before);
    assert_eq!(response.files_indexed, 0);
    assert_eq!(response.files_removed, 0);
    assert_eq!(response.files_unchanged, 0);
    assert_eq!(response.files_skipped, 0);
    assert_eq!(
        response
            .skip_reasons
            .as_ref()
            .expect("current skip reasons")
            .total(),
        0
    );
    assert!(response.warnings.is_empty());
    assert_eq!(
        services
            .status()
            .await
            .expect("status after artifacts")
            .repository_generation,
        before
    );
}
