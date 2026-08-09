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
    Services::open(Config::discover(first_root.path(), Some(database)).expect("same-root config"))
        .expect("same root may share database");
}

#[test]
fn explicit_database_rejects_a_different_index_scope() {
    let root = tempfile::tempdir().expect("root");
    let database = root.path().join("shared.sqlite");
    let source_scope = IndexScope::new(vec!["src/**".into()], Vec::new()).expect("source scope");
    let first = Services::open(
        Config::discover_scoped(root.path(), Some(database.clone()), source_scope.clone())
            .expect("scoped config"),
    )
    .expect("claim scoped database");

    let full_error =
        Services::open(Config::discover(root.path(), Some(database.clone())).expect("full config"))
            .expect_err("full scope must not share a scoped explicit database");
    assert!(matches!(full_error, Error::IndexScopeMismatch { .. }));

    drop(first);
    Services::open(
        Config::discover_scoped(
            root.path(),
            Some(database),
            IndexScope::new(vec!["./src\\**".into()], Vec::new()).expect("equivalent scope"),
        )
        .expect("equivalent config"),
    )
    .expect("normalized equivalent scope may share the database");
}

#[tokio::test]
async fn scoped_index_preserves_selected_retrievals_and_discloses_negative_evidence_boundary() {
    let root = tempfile::tempdir().expect("root");
    let cache = tempfile::tempdir().expect("cache");
    std::fs::create_dir_all(root.path().join("src")).expect("src");
    std::fs::create_dir_all(root.path().join("tests")).expect("tests");
    std::fs::create_dir_all(root.path().join("third_party/dependency")).expect("dependency");
    std::fs::write(
        root.path().join("src/lib.rs"),
        "pub fn first_party_scope_target() -> bool { true }\n",
    )
    .expect("first-party source");
    std::fs::write(
        root.path().join("tests/smoke.rs"),
        "fn scoped_smoke_test() {}\n",
    )
    .expect("test source");
    std::fs::write(
        root.path().join("third_party/dependency/lib.rs"),
        "pub fn dependency_only_target() {}\n",
    )
    .expect("dependency source");
    let scope = IndexScope::new(
        vec!["src/**".into(), "tests/**".into()],
        vec!["tests/generated/**".into()],
    )
    .expect("scope");
    let scoped = Services::open(
        Config::discover_scoped(
            root.path(),
            Some(cache.path().join("scoped.sqlite")),
            scope.clone(),
        )
        .expect("scoped config"),
    )
    .expect("scoped services");
    scoped.index(false).await.expect("scoped index");

    let status = scoped.status().await.expect("scoped status");
    assert_eq!(status.index_scope, IndexScopeMode::Scoped);
    assert_eq!(status.index_scope_digest.as_deref(), scope.digest());
    assert_eq!(status.index_include_paths, ["src/**", "tests/**"]);
    assert_eq!(status.index_exclude_paths, ["tests/generated/**"]);
    assert_eq!(status.file_count, 2);

    let request = SearchRequest {
        query: "first_party_scope_target".into(),
        mode: SearchMode::Identifier,
        include_paths: Vec::new(),
        exclude_paths: Vec::new(),
        focus_paths: Vec::new(),
        max_results: Some(10),
        max_tokens: Some(1_000),
        context_lines: Some(1),
        case_sensitive: true,
        all_occurrences: false,
        prefer_structural: true,
        receipt_id: None,
        query_receipt: None,
        cursor: None,
    };
    let scoped_result = scoped.search(request.clone()).await.expect("scoped search");
    assert_eq!(scoped_result.meta.index_scope, IndexScopeMode::Scoped);
    assert_eq!(
        scoped_result.meta.index_scope_digest.as_deref(),
        scope.digest()
    );
    assert_eq!(scoped_result.hits.len(), 1);
    assert_eq!(scoped_result.hits[0].path, "src/lib.rs");

    let mut absent_request = request.clone();
    absent_request.query = "dependency_only_target".into();
    let absent = scoped.search(absent_request).await.expect("scoped absence");
    assert!(absent.hits.is_empty());
    assert_eq!(absent.meta.index_scope, IndexScopeMode::Scoped);

    std::fs::write(
        root.path().join("third_party/dependency/new.rs"),
        "pub fn outside_scope_change() {}\n",
    )
    .expect("outside-scope change");
    scoped
        .index_paths(vec!["third_party/dependency/new.rs".into()])
        .await
        .expect("outside-scope targeted reconciliation");
    assert_eq!(
        scoped
            .status()
            .await
            .expect("status after outside change")
            .file_count,
        2
    );

    std::fs::write(
        root.path().join("src/new.rs"),
        "pub fn scoped_rename_target() {}\n",
    )
    .expect("included change");
    scoped
        .index_paths(vec!["src/new.rs".into()])
        .await
        .expect("included targeted reconciliation");
    assert_eq!(
        scoped
            .status()
            .await
            .expect("status after included change")
            .file_count,
        3
    );
    std::fs::rename(
        root.path().join("src/new.rs"),
        root.path().join("third_party/dependency/moved.rs"),
    )
    .expect("rename across scope");
    scoped
        .index_paths(vec![
            "src/new.rs".into(),
            "third_party/dependency/moved.rs".into(),
        ])
        .await
        .expect("cross-scope rename reconciliation");
    assert_eq!(
        scoped
            .status()
            .await
            .expect("status after rename")
            .file_count,
        2
    );

    let full = Services::open(
        Config::discover(root.path(), Some(cache.path().join("full.sqlite"))).expect("full config"),
    )
    .expect("full services");
    full.index(false).await.expect("full index");
    let full_result = full.search(request).await.expect("full search");
    assert_eq!(full_result.meta.index_scope, IndexScopeMode::Full);
    assert_eq!(full_result.meta.index_scope_digest, None);
    assert_eq!(full_result.hits.len(), scoped_result.hits.len());
    assert_eq!(full_result.hits[0].path, scoped_result.hits[0].path);
    assert_eq!(
        full_result.hits[0].content_hash,
        scoped_result.hits[0].content_hash
    );
    assert_eq!(
        full_result.hits[0].start_line,
        scoped_result.hits[0].start_line
    );
    assert_eq!(full_result.hits[0].end_line, scoped_result.hits[0].end_line);
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
    let second =
        Services::open(Config::discover(root.path(), Some(database)).expect("second config"))
            .expect("second services");

    let indexed = first.index(false).await.expect("index");
    let observed = second.status().await.expect("follower status");

    assert_eq!(
        observed.repository_generation,
        indexed.repository_generation
    );
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

    let config = Config::discover(root.path(), Some(alias.join("missing/cache/index.sqlite")))
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
