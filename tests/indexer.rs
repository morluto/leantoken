use std::sync::Arc;

use leantoken::Config;
use leantoken::indexer::Indexer;
use leantoken::storage::Storage;

#[test]
fn indexer_initial_reconcile_indexes_files_and_advances_generation() {
    let root = tempfile::tempdir().expect("root");
    std::fs::write(root.path().join("a.rs"), "fn first() {}\n").expect("write a");
    std::fs::write(root.path().join("b.txt"), "searchable text\n").expect("write b");

    let config = Arc::new(
        Config::discover(root.path(), Some(root.path().join("index.sqlite"))).expect("config"),
    );
    let storage = Storage::open(&config.database_path).expect("storage");
    let indexer = Indexer::new(config, storage.clone());

    let response = indexer.reconcile(false).expect("first reconcile");
    assert_eq!(response.files_indexed, 2);
    assert_eq!(response.repository_generation, 1);
    assert_eq!(response.files_unchanged, 0);
    assert_eq!(response.files_removed, 0);

    let hits = storage.search_word("first", 10).expect("search");
    assert_eq!(hits.len(), 1);
    assert!(hits[0].content.contains("first"));
}

#[test]
fn indexer_reopen_leaves_unchanged_files_and_generation() {
    let root = tempfile::tempdir().expect("root");
    std::fs::write(root.path().join("a.rs"), "fn stable() {}\n").expect("write a");

    let config = Arc::new(
        Config::discover(root.path(), Some(root.path().join("index.sqlite"))).expect("config"),
    );
    let storage = Storage::open(&config.database_path).expect("storage");
    let indexer = Indexer::new(config.clone(), storage.clone());

    let first = indexer.reconcile(false).expect("first reconcile");
    assert_eq!(first.repository_generation, 1);

    let second = indexer.reconcile(false).expect("second reconcile");
    assert_eq!(second.files_unchanged, 1);
    assert_eq!(second.files_indexed, 0);
    assert_eq!(second.repository_generation, 1);

    let meta = storage.meta().expect("meta");
    assert_eq!(meta.repository_generation, 1);
}

#[test]
fn indexer_change_updates_generation_and_search_index() {
    let root = tempfile::tempdir().expect("root");
    std::fs::write(root.path().join("a.rs"), "fn old() {}\n").expect("write a");

    let config = Arc::new(
        Config::discover(root.path(), Some(root.path().join("index.sqlite"))).expect("config"),
    );
    let storage = Storage::open(&config.database_path).expect("storage");
    let indexer = Indexer::new(config, storage.clone());

    let first = indexer.reconcile(false).expect("first reconcile");
    assert_eq!(first.repository_generation, 1);

    std::fs::write(root.path().join("a.rs"), "fn new_name() {}\n").expect("change a");

    let second = indexer.reconcile(false).expect("second reconcile");
    assert_eq!(second.files_indexed, 1);
    assert_eq!(second.files_unchanged, 0);
    assert_eq!(second.repository_generation, 2);

    let old_hits = storage.search_word("old", 10).expect("search old");
    assert_eq!(old_hits.len(), 0);

    let new_hits = storage.search_word("new_name", 10).expect("search new");
    assert_eq!(new_hits.len(), 1);
}

#[test]
fn indexer_delete_removes_file_and_advances_generation() {
    let root = tempfile::tempdir().expect("root");
    std::fs::write(root.path().join("a.rs"), "fn gone() {}\n").expect("write a");
    std::fs::write(root.path().join("b.rs"), "fn kept() {}\n").expect("write b");

    let config = Arc::new(
        Config::discover(root.path(), Some(root.path().join("index.sqlite"))).expect("config"),
    );
    let storage = Storage::open(&config.database_path).expect("storage");
    let indexer = Indexer::new(config, storage.clone());

    let first = indexer.reconcile(false).expect("first reconcile");
    assert_eq!(first.repository_generation, 1);
    assert_eq!(first.files_indexed, 2);

    std::fs::remove_file(root.path().join("a.rs")).expect("remove a");

    let second = indexer.reconcile(false).expect("second reconcile");
    assert_eq!(second.files_removed, 1);
    assert_eq!(second.files_unchanged, 1);
    assert_eq!(second.repository_generation, 2);

    assert!(storage.find_file("a.rs").expect("find").is_none());
    let gone_hits = storage.search_word("gone", 10).expect("search gone");
    assert_eq!(gone_hits.len(), 0);
    let kept_hits = storage.search_word("kept", 10).expect("search kept");
    assert_eq!(kept_hits.len(), 1);
}

#[test]
fn indexer_rebuild_resets_index_and_advances_generation() {
    let root = tempfile::tempdir().expect("root");
    std::fs::write(root.path().join("a.rs"), "fn only() {}\n").expect("write a");

    let config = Arc::new(
        Config::discover(root.path(), Some(root.path().join("index.sqlite"))).expect("config"),
    );
    let storage = Storage::open(&config.database_path).expect("storage");
    let indexer = Indexer::new(config, storage.clone());

    let first = indexer.reconcile(false).expect("first reconcile");
    assert_eq!(first.repository_generation, 1);

    let rebuild = indexer.reconcile(true).expect("rebuild");
    assert_eq!(rebuild.files_indexed, 1);
    assert_eq!(rebuild.repository_generation, 2);

    let hits = storage.search_word("only", 10).expect("search");
    assert_eq!(hits.len(), 1);
}

#[test]
fn indexer_respects_chunk_lines_and_bytes() {
    let root = tempfile::tempdir().expect("root");
    let content: String = (0..100).map(|i| format!("line {}\n", i)).collect();
    std::fs::write(root.path().join("big.rs"), &content).expect("write big");

    let config = Arc::new(
        Config::discover(root.path(), Some(root.path().join("index.sqlite"))).expect("config"),
    );
    let storage = Storage::open(&config.database_path).expect("storage");
    let indexer = Indexer::new(config, storage.clone());

    let response = indexer.reconcile(false).expect("reconcile");
    assert_eq!(response.files_indexed, 1);

    let file = storage.find_file("big.rs").expect("find").expect("exists");
    let chunks = storage.get_chunks_for_file(file.id, 100).expect("chunks");
    assert!(
        chunks.len() > 1,
        "large file should be split into multiple chunks"
    );
    for chunk in &chunks {
        assert!(chunk.end_line - chunk.start_line < 80);
        assert!(chunk.content.len() <= 32 * 1024);
    }
}
