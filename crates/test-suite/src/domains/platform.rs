use leantoken::repository::{normalize_relative, slash_path};
use leantoken::watcher::RepositoryWatcher;
use leantoken_test_support::Sandbox;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

#[test]
fn platform_paths_have_stable_repository_keys() {
    assert_eq!(normalize_relative(r".\src\lib.rs").unwrap(), "src/lib.rs");
    assert_eq!(slash_path(std::path::Path::new("src/lib.rs")), "src/lib.rs");
}

#[tokio::test]
async fn watcher_reports_file_change() {
    let sandbox = Sandbox::new(module_path!(), "watcher_reports_file_change").expect("sandbox");
    let token = CancellationToken::new();
    let (watcher, mut rx) = RepositoryWatcher::start(
        sandbox.repo(),
        64,
        Duration::from_millis(50),
        token.child_token(),
    )
    .await
    .expect("start watcher");

    std::fs::write(sandbox.repo().join("changed.rs"), "fn changed() {}\n").expect("write");

    let msg = tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("watcher should emit within 5s")
        .expect("channel open");

    match msg {
        leantoken::watcher::WatcherMessage::Changed { paths } => {
            assert!(
                paths.iter().any(|path| path.contains("changed.rs")),
                "expected changed.rs in {paths:?}"
            );
        }
        leantoken::watcher::WatcherMessage::ReconcileRequired => {}
    }

    watcher.shutdown().await.expect("shutdown");
}

#[tokio::test]
async fn watcher_shutdown_cancels_task() {
    let sandbox = Sandbox::new(module_path!(), "watcher_shutdown_cancels_task").expect("sandbox");
    let token = CancellationToken::new();
    let (watcher, mut rx) = RepositoryWatcher::start(
        sandbox.repo(),
        64,
        Duration::from_millis(50),
        token.child_token(),
    )
    .await
    .expect("start watcher");

    watcher.shutdown().await.expect("shutdown");

    let result = tokio::time::timeout(Duration::from_millis(200), rx.recv()).await;
    assert!(result.is_ok());
    assert!(result.unwrap().is_none());
}

use leantoken::tokens::Tokenizer;
use leantoken::{Config, DiscoveryLimits, Error, IndexScope, services::Services};

#[test]
fn config_discovers_existing_root() {
    let root = Sandbox::new(module_path!(), "config_case").expect("sandbox");
    let config = Config::discover(root.repo(), None).expect("discover");
    assert!(config.root.exists());
    assert_eq!(
        config.root,
        root.repo().canonicalize().expect("canonicalize")
    );
}

#[test]
fn default_cache_identity_is_independent_per_repository() {
    let first_root = Sandbox::new(module_path!(), "config_case").expect("sandbox");
    let second_root = Sandbox::new(module_path!(), "config_case").expect("sandbox");

    let first = Config::discover(first_root.repo(), None).expect("first config");
    let second = Config::discover(second_root.repo(), None).expect("second config");

    assert_ne!(first.database_path, second.database_path);
}

#[test]
fn managed_cache_identity_isolated_by_normalized_index_scope() {
    let root = Sandbox::new(module_path!(), "config_case").expect("sandbox");
    let full = Config::discover(root.repo(), None).expect("full config");
    let scoped = Config::discover_scoped(
        root.repo(),
        None,
        IndexScope::new(vec!["src/**".into()], vec!["third_party/**".into()]).expect("scope"),
    )
    .expect("scoped config");
    let equivalent = Config::discover_scoped(
        root.repo(),
        None,
        IndexScope::new(
            vec!["./src\\**".into(), "src/**".into()],
            vec!["third_party//**".into()],
        )
        .expect("equivalent scope"),
    )
    .expect("equivalent config");

    assert_ne!(full.database_path, scoped.database_path);
    assert_eq!(scoped.database_path, equivalent.database_path);
    assert!(full.index_scope().is_full());
    assert!(!scoped.index_scope().is_full());
    assert_eq!(
        scoped.index_scope().digest(),
        equivalent.index_scope().digest()
    );
}

#[test]
fn config_canonicalizes_explicit_database_parent() {
    let root = Sandbox::new(module_path!(), "config_case").expect("sandbox");
    let db = root.repo().join("custom.sqlite");
    let config = Config::discover(root.repo(), Some(db)).expect("discover");
    assert_eq!(
        config.database_path,
        root.repo()
            .canonicalize()
            .expect("canonical root")
            .join("custom.sqlite")
    );
}

#[cfg(unix)]
#[test]
fn config_canonicalizes_database_parent_reached_through_symlink() {
    let root = Sandbox::new(module_path!(), "config_case").expect("sandbox");
    let aliases = Sandbox::new(module_path!(), "config_case").expect("sandbox");
    let alias = aliases.repo().join("repository");
    std::os::unix::fs::symlink(root.repo(), &alias).expect("symlink root");

    let config = Config::discover(&alias, Some(alias.join("index.sqlite"))).expect("discover");

    assert_eq!(
        config.database_path,
        root.repo()
            .canonicalize()
            .expect("canonical root")
            .join("index.sqlite")
    );
}

#[cfg(unix)]
#[test]
fn config_canonicalizes_missing_database_descendants_below_symlink() {
    let root = Sandbox::new(module_path!(), "config_case").expect("sandbox");
    let aliases = Sandbox::new(module_path!(), "config_case").expect("sandbox");
    let alias = aliases.repo().join("repository");
    std::os::unix::fs::symlink(root.repo(), &alias).expect("symlink root");

    let config = Config::discover(root.repo(), Some(alias.join("missing/cache/index.sqlite")))
        .expect("discover");

    assert_eq!(
        config.database_path,
        root.repo()
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
    let root = Sandbox::new(module_path!(), "config_case").expect("sandbox");
    let cache = Sandbox::new(module_path!(), "config_case").expect("sandbox");
    let database = cache.repo().join("index.sqlite");
    std::fs::write(&database, "placeholder").expect("database placeholder");
    let alias = root.repo().join("alias.sqlite");
    std::os::unix::fs::symlink(&database, &alias).expect("database symlink");

    let config = Config::discover(root.repo(), Some(alias)).expect("discover");

    assert_eq!(
        config.database_path,
        database.canonicalize().expect("canonical database")
    );
}

#[test]
fn config_rejects_missing_root() {
    let root = Sandbox::new(module_path!(), "config_case").expect("sandbox");
    let missing = root.repo().join("nowhere");
    let err = Config::discover(&missing, None).expect_err("missing root");
    assert!(matches!(err, leantoken::Error::RootNotFound(_)));
}

#[test]
fn config_rejects_file_as_root() {
    let directory = Sandbox::new(module_path!(), "config_case").expect("sandbox");
    let file = directory.repo().join("not-a-repository");
    std::fs::write(&file, "content").expect("write file");
    let error = Config::discover(&file, None).expect_err("file root must fail");
    assert!(matches!(error, leantoken::Error::InvalidConfiguration(_)));
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
    let root = Sandbox::new(module_path!(), "config_case").expect("sandbox");
    let config = Config::discover(root.repo(), None).expect("discover");
    assert_eq!(config.discovery_limits(), DiscoveryLimits::default());
    assert!(config.max_results > 0);
    assert!(config.max_output_tokens > 0);
    assert!(config.default_context_tokens > 0);
    assert!(config.context_lines > 0);
    assert!(config.chunk_lines > 0);
    assert!(config.chunk_bytes > 0);
    assert!(config.max_index_workers > 0);
    assert!(config.max_index_workers <= 4);
    assert!(config.watcher_debounce >= Duration::ZERO);
    assert_eq!(config.tokenizer, Tokenizer::default());
    assert!(config.tokenizer.is_exact());
}

#[test]
fn services_reject_invalid_retrieval_limit_configuration() {
    let root = Sandbox::new(module_path!(), "config_case").expect("sandbox");
    let base =
        Config::discover(root.repo(), Some(root.repo().join("index.sqlite"))).expect("discover");
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
        assert!(
            matches!(error, Error::InvalidConfiguration(_)),
            "got {error:?}"
        );
    }
}

#[test]
fn config_identifies_database_and_wal_artifacts_inside_the_root() {
    let root = Sandbox::new(module_path!(), "config_case").expect("sandbox");
    let database = root.repo().join(".cache/index.sqlite");
    std::fs::create_dir_all(database.parent().expect("database parent")).expect("parent");
    let config = Config::discover(root.repo(), Some(database)).expect("config");

    assert!(config.is_database_artifact(".cache/index.sqlite"));
    assert!(config.is_database_artifact(".cache/index.sqlite-wal"));
    assert!(config.is_database_artifact(".cache/index.sqlite-shm"));
    assert!(config.is_database_artifact(".cache/index.sqlite.lease.lock"));
    assert!(config.is_database_artifact(".cache/index.sqlite.leader.lock"));
    assert!(config.is_database_artifact(".cache/index.sqlite.index.lock"));
    assert!(config.is_database_artifact(".cache/index.sqlite.init.lock"));
    assert!(!config.is_database_artifact("src/index.sqlite"));
}
