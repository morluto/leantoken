use std::time::Duration;

use leantoken::{Config, DiscoveryLimits, Error, services::Services};
use leantoken::tokens::Tokenizer;

#[test]
fn config_discovers_existing_root() {
    let root = tempfile::tempdir().expect("tempdir");
    let config = Config::discover(root.path(), None).expect("discover");
    assert!(config.root.exists());
    assert_eq!(
        config.root,
        root.path().canonicalize().expect("canonicalize")
    );
}

#[test]
fn default_cache_identity_is_independent_per_repository() {
    let first_root = tempfile::tempdir().expect("first root");
    let second_root = tempfile::tempdir().expect("second root");

    let first = Config::discover(first_root.path(), None).expect("first config");
    let second = Config::discover(second_root.path(), None).expect("second config");

    assert_ne!(first.database_path, second.database_path);
}

#[test]
fn config_canonicalizes_explicit_database_parent() {
    let root = tempfile::tempdir().expect("tempdir");
    let db = root.path().join("custom.sqlite");
    let config = Config::discover(root.path(), Some(db)).expect("discover");
    assert_eq!(
        config.database_path,
        root.path()
            .canonicalize()
            .expect("canonical root")
            .join("custom.sqlite")
    );
}

#[cfg(unix)]
#[test]
fn config_canonicalizes_database_parent_reached_through_symlink() {
    let root = tempfile::tempdir().expect("root");
    let aliases = tempfile::tempdir().expect("aliases");
    let alias = aliases.path().join("repository");
    std::os::unix::fs::symlink(root.path(), &alias).expect("symlink root");

    let config = Config::discover(&alias, Some(alias.join("index.sqlite"))).expect("discover");

    assert_eq!(
        config.database_path,
        root.path()
            .canonicalize()
            .expect("canonical root")
            .join("index.sqlite")
    );
}

#[cfg(unix)]
#[test]
fn config_canonicalizes_missing_database_descendants_below_symlink() {
    let root = tempfile::tempdir().expect("root");
    let aliases = tempfile::tempdir().expect("aliases");
    let alias = aliases.path().join("repository");
    std::os::unix::fs::symlink(root.path(), &alias).expect("symlink root");

    let config = Config::discover(
        root.path(),
        Some(alias.join("missing/cache/index.sqlite")),
    )
    .expect("discover");

    assert_eq!(
        config.database_path,
        root.path()
            .canonicalize()
            .expect("canonical root")
            .join("missing/cache/index.sqlite")
    );
    assert!(config.is_database_artifact("missing/cache/index.sqlite"));
    assert!(config.is_database_artifact("missing/cache/index.sqlite-wal"));
    assert!(config.is_database_artifact("missing/cache/index.sqlite-shm"));
    assert!(config.is_database_artifact("missing/cache/index.sqlite.lease.lock"));
    assert!(config.is_database_artifact("missing/cache/index.sqlite.leader.lock"));
    assert!(config.is_database_artifact("missing/cache/index.sqlite.index.lock"));
    assert!(config.is_database_artifact("missing/cache/index.sqlite.init.lock"));
}

#[cfg(unix)]
#[test]
fn config_canonicalizes_existing_database_symlink_for_shared_lock_identity() {
    let root = tempfile::tempdir().expect("root");
    let cache = tempfile::tempdir().expect("cache");
    let database = cache.path().join("index.sqlite");
    std::fs::write(&database, "placeholder").expect("database placeholder");
    let alias = root.path().join("alias.sqlite");
    std::os::unix::fs::symlink(&database, &alias).expect("database symlink");

    let config = Config::discover(root.path(), Some(alias)).expect("discover");

    assert_eq!(
        config.database_path,
        database.canonicalize().expect("canonical database")
    );
}

#[test]
fn config_rejects_missing_root() {
    let root = tempfile::tempdir().expect("tempdir");
    let missing = root.path().join("nowhere");
    let err = Config::discover(&missing, None).expect_err("missing root");
    assert!(matches!(err, leantoken::Error::RootNotFound(_)));
}

#[test]
fn config_rejects_file_as_root() {
    let directory = tempfile::tempdir().expect("tempdir");
    let file = directory.path().join("not-a-repository");
    std::fs::write(&file, "content").expect("write file");
    let error = Config::discover(&file, None).expect_err("file root must fail");
    assert!(matches!(
        error,
        leantoken::Error::InvalidConfiguration(_)
    ));
}

#[test]
fn config_rejects_the_current_home_directory_by_default() {
    let home = directories::BaseDirs::new()
        .expect("home directories")
        .home_dir()
        .canonicalize()
        .expect("canonical home");

    let error = Config::discover(&home, None).expect_err("home root must fail closed");

    assert!(matches!(
        error,
        leantoken::Error::UnsafeRepositoryRoot(path) if path == home
    ));
}

#[test]
fn config_defaults_bound_output_and_timing() {
    let root = tempfile::tempdir().expect("tempdir");
    let config = Config::discover(root.path(), None).expect("discover");
    assert_eq!(config.discovery_limits(), DiscoveryLimits::default());
    assert!(config.max_results > 0);
    assert!(config.max_output_tokens > 0);
    assert!(config.default_context_tokens > 0);
    assert!(config.context_lines > 0);
    assert!(config.chunk_lines > 0);
    assert!(config.chunk_bytes > 0);
    assert!(config.watcher_debounce >= Duration::ZERO);
    assert_eq!(config.tokenizer, Tokenizer::default());
    assert!(config.tokenizer.is_exact());
}

#[test]
fn services_reject_invalid_retrieval_limit_configuration() {
    let root = tempfile::tempdir().expect("tempdir");
    let base = Config::discover(root.path(), Some(root.path().join("index.sqlite")))
        .expect("discover");
    let mut invalid = Vec::new();

    let mut config = base.clone();
    config.default_results = 0;
    invalid.push(config);
    let mut config = base.clone();
    config.max_results = 0;
    invalid.push(config);
    let mut config = base.clone();
    config.default_results = config.max_results + 1;
    invalid.push(config);
    let mut config = base.clone();
    config.max_results = leantoken::storage::HARD_MAX_RESULTS;
    invalid.push(config);
    let mut config = base.clone();
    config.max_results = usize::MAX;
    invalid.push(config);
    let mut config = base.clone();
    config.default_read_tokens = 0;
    invalid.push(config);
    let mut config = base.clone();
    config.default_context_tokens = 0;
    invalid.push(config);
    let mut config = base.clone();
    config.max_output_tokens = 0;
    invalid.push(config);
    let mut config = base.clone();
    config.default_read_tokens = config.max_output_tokens + 1;
    invalid.push(config);
    let mut config = base.clone();
    config.default_context_tokens = config.max_output_tokens + 1;
    invalid.push(config);
    let mut config = base.clone();
    config.max_output_tokens = 32_001;
    invalid.push(config);
    let mut config = base;
    config.context_lines = 21;
    invalid.push(config);

    for config in invalid {
        let error = Services::open(config).expect_err("invalid retrieval limits");
        assert!(matches!(error, Error::InvalidConfiguration(_)), "got {error:?}");
    }
}

#[test]
fn config_identifies_database_and_wal_artifacts_inside_the_root() {
    let root = tempfile::tempdir().expect("root");
    let database = root.path().join(".cache/index.sqlite");
    std::fs::create_dir_all(database.parent().expect("database parent")).expect("parent");
    let config = Config::discover(root.path(), Some(database)).expect("config");

    assert!(config.is_database_artifact(".cache/index.sqlite"));
    assert!(config.is_database_artifact(".cache/index.sqlite-wal"));
    assert!(config.is_database_artifact(".cache/index.sqlite-shm"));
    assert!(config.is_database_artifact(".cache/index.sqlite.lease.lock"));
    assert!(config.is_database_artifact(".cache/index.sqlite.leader.lock"));
    assert!(config.is_database_artifact(".cache/index.sqlite.index.lock"));
    assert!(config.is_database_artifact(".cache/index.sqlite.init.lock"));
    assert!(!config.is_database_artifact("src/index.sqlite"));
}
