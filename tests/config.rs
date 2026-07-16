use std::time::Duration;

use leantoken::Config;
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
    assert!(matches!(error, leantoken::Error::InvalidRequest(_)));
}

#[test]
fn config_defaults_bound_output_and_timing() {
    let root = tempfile::tempdir().expect("tempdir");
    let config = Config::discover(root.path(), None).expect("discover");
    assert!(config.max_file_bytes > 0);
    assert!(config.max_results > 0);
    assert!(config.max_output_tokens > 0);
    assert!(config.context_lines > 0);
    assert!(config.chunk_lines > 0);
    assert!(config.chunk_bytes > 0);
    assert!(config.watcher_debounce >= Duration::ZERO);
    assert_eq!(config.tokenizer, Tokenizer::default());
    assert!(config.tokenizer.is_exact());
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
    assert!(!config.is_database_artifact("src/index.sqlite"));
}
