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
fn config_explicit_database_path_wins() {
    let root = tempfile::tempdir().expect("tempdir");
    let db = root.path().join("custom.sqlite");
    let config = Config::discover(root.path(), Some(db.clone())).expect("discover");
    assert_eq!(config.database_path, db);
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
