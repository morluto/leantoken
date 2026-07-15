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
fn targeted_reconcile_updates_only_reported_existing_file() {
    let root = tempfile::tempdir().expect("root");
    std::fs::write(root.path().join("a.rs"), "fn old() {}\n").expect("write a");
    std::fs::write(root.path().join("b.rs"), "fn stable() {}\n").expect("write b");
    let config = Arc::new(
        Config::discover(root.path(), Some(root.path().join("index.sqlite"))).expect("config"),
    );
    let storage = Storage::open(&config.database_path).expect("storage");
    let indexer = Indexer::new(config, storage.clone());
    indexer.reconcile(false).expect("initial reconcile");
    let stable_generation = storage
        .find_file("b.rs")
        .expect("find stable")
        .expect("stable file")
        .generation;

    std::fs::write(root.path().join("a.rs"), "fn replacement() {}\n").expect("modify a");
    let response = indexer
        .reconcile_paths(&["a.rs".into()])
        .expect("targeted reconcile");

    assert_eq!(response.files_seen, 1);
    assert_eq!(response.files_indexed, 1);
    assert_eq!(response.files_removed, 0);
    assert!(
        storage
            .search_word("old", 10)
            .expect("old search")
            .is_empty()
    );
    assert_eq!(
        storage
            .search_word("replacement", 10)
            .expect("replacement search")
            .len(),
        1
    );
    assert_eq!(
        storage
            .find_file("b.rs")
            .expect("find stable")
            .expect("stable file")
            .generation,
        stable_generation
    );
}

#[test]
fn targeted_reconcile_hashes_reported_files_even_when_metadata_is_unchanged() {
    let root = tempfile::tempdir().expect("root");
    let path = root.path().join("a.rs");
    std::fs::write(&path, "fn old() {}\n").expect("write old");
    let original_modified = std::fs::metadata(&path)
        .expect("metadata")
        .modified()
        .expect("modified");
    let config = Arc::new(
        Config::discover(root.path(), Some(root.path().join("index.sqlite"))).expect("config"),
    );
    let storage = Storage::open(&config.database_path).expect("storage");
    let indexer = Indexer::new(config, storage.clone());
    indexer.reconcile(false).expect("initial reconcile");

    std::fs::write(&path, "fn neo() {}\n").expect("same-size replacement");
    std::fs::File::options()
        .write(true)
        .open(&path)
        .expect("open")
        .set_times(std::fs::FileTimes::new().set_modified(original_modified))
        .expect("restore mtime");
    let response = indexer
        .reconcile_paths(&["a.rs".into()])
        .expect("targeted reconcile");

    assert_eq!(response.files_indexed, 1);
    assert!(storage.search_word("old", 10).expect("old").is_empty());
    assert_eq!(storage.search_word("neo", 10).expect("new").len(), 1);
}

#[test]
fn targeted_reconcile_deletes_existing_file() {
    let root = tempfile::tempdir().expect("root");
    std::fs::write(root.path().join("gone.rs"), "fn gone() {}\n").expect("write");
    let config = Arc::new(
        Config::discover(root.path(), Some(root.path().join("index.sqlite"))).expect("config"),
    );
    let storage = Storage::open(&config.database_path).expect("storage");
    let indexer = Indexer::new(config, storage.clone());
    indexer.reconcile(false).expect("initial reconcile");

    std::fs::remove_file(root.path().join("gone.rs")).expect("remove");
    let response = indexer
        .reconcile_paths(&["gone.rs".into()])
        .expect("targeted reconcile");

    assert_eq!(response.files_seen, 1);
    assert_eq!(response.files_removed, 1);
    assert!(storage.find_file("gone.rs").expect("find").is_none());
}

#[test]
fn targeted_reconcile_clears_imports_resolved_to_deleted_file() {
    let root = tempfile::tempdir().expect("root");
    std::fs::write(root.path().join("target.rs"), "pub fn item() {}\n").expect("target");
    std::fs::write(
        root.path().join("consumer.rs"),
        "use target::item;\nfn consumer() { item(); }\n",
    )
    .expect("consumer");
    let config = Arc::new(
        Config::discover(root.path(), Some(root.path().join("index.sqlite"))).expect("config"),
    );
    let storage = Storage::open(&config.database_path).expect("storage");
    let indexer = Indexer::new(config, storage.clone());
    indexer.reconcile(false).expect("initial reconcile");
    let consumer = storage
        .find_file("consumer.rs")
        .expect("find consumer")
        .expect("consumer");
    assert_eq!(
        storage
            .get_imports_for_file(consumer.id, 10)
            .expect("imports")[0]
            .resolved_path
            .as_deref(),
        Some("target.rs")
    );

    std::fs::remove_file(root.path().join("target.rs")).expect("remove target");
    indexer
        .reconcile_paths(&["target.rs".into()])
        .expect("targeted delete");

    assert_eq!(
        storage
            .get_imports_for_file(consumer.id, 10)
            .expect("imports")[0]
            .resolved_path,
        None
    );
}

#[test]
fn targeted_reconcile_falls_back_for_deleted_directory() {
    let root = tempfile::tempdir().expect("root");
    std::fs::create_dir(root.path().join("removed")).expect("directory");
    std::fs::write(root.path().join("removed/a.rs"), "fn gone_a() {}\n").expect("a");
    std::fs::write(root.path().join("removed/b.rs"), "fn gone_b() {}\n").expect("b");
    std::fs::write(root.path().join("keep.rs"), "fn keep() {}\n").expect("keep");
    let config = Arc::new(
        Config::discover(root.path(), Some(root.path().join("index.sqlite"))).expect("config"),
    );
    let storage = Storage::open(&config.database_path).expect("storage");
    let indexer = Indexer::new(config, storage.clone());
    indexer.reconcile(false).expect("initial reconcile");

    std::fs::remove_dir_all(root.path().join("removed")).expect("remove directory");
    let response = indexer
        .reconcile_paths(&["removed".into()])
        .expect("directory fallback");

    assert_eq!(response.files_removed, 2);
    assert!(storage.find_file("removed/a.rs").expect("find a").is_none());
    assert!(storage.find_file("removed/b.rs").expect("find b").is_none());
    assert!(storage.find_file("keep.rs").expect("find keep").is_some());
}

#[test]
fn targeted_reconcile_falls_back_for_new_files_and_ignore_changes() {
    let root = tempfile::tempdir().expect("root");
    std::fs::create_dir(root.path().join(".git")).expect("git marker");
    std::fs::write(root.path().join("keep.rs"), "fn keep() {}\n").expect("write keep");
    std::fs::write(root.path().join("hide.rs"), "fn hide() {}\n").expect("write hide");
    std::fs::write(root.path().join(".gitignore"), "").expect("write ignore");
    let config = Arc::new(
        Config::discover(root.path(), Some(root.path().join("index.sqlite"))).expect("config"),
    );
    let storage = Storage::open(&config.database_path).expect("storage");
    let indexer = Indexer::new(config, storage.clone());
    indexer.reconcile(false).expect("initial reconcile");

    std::fs::write(root.path().join("new.rs"), "fn new_file() {}\n").expect("new file");
    let added = indexer
        .reconcile_paths(&["new.rs".into()])
        .expect("new path fallback");
    assert!(added.files_seen >= 4, "new files require a full scan");
    assert!(storage.find_file("new.rs").expect("find new").is_some());

    std::fs::write(root.path().join(".gitignore"), "hide.rs\n").expect("change ignore");
    let ignored = indexer
        .reconcile_paths(&[".gitignore".into()])
        .expect("ignore fallback");
    assert!(
        ignored.files_seen >= 3,
        "ignore changes require a full scan"
    );
    assert!(storage.find_file("hide.rs").expect("find hidden").is_none());
}

#[test]
fn new_file_fallback_resolves_existing_importers() {
    let root = tempfile::tempdir().expect("root");
    std::fs::write(
        root.path().join("consumer.rs"),
        "use target::item;\nfn consumer() { item(); }\n",
    )
    .expect("consumer");
    let config = Arc::new(
        Config::discover(root.path(), Some(root.path().join("index.sqlite"))).expect("config"),
    );
    let storage = Storage::open(&config.database_path).expect("storage");
    let indexer = Indexer::new(config, storage.clone());
    indexer.reconcile(false).expect("initial reconcile");
    let consumer = storage
        .find_file("consumer.rs")
        .expect("find consumer")
        .expect("consumer");
    assert_eq!(
        storage
            .get_imports_for_file(consumer.id, 10)
            .expect("imports")[0]
            .resolved_path,
        None
    );

    std::fs::write(root.path().join("target.rs"), "pub fn item() {}\n").expect("target");
    indexer
        .reconcile_paths(&["target.rs".into()])
        .expect("new target fallback");

    let consumer = storage
        .find_file("consumer.rs")
        .expect("find consumer")
        .expect("consumer after rebuild");
    assert_eq!(
        storage
            .get_imports_for_file(consumer.id, 10)
            .expect("imports")[0]
            .resolved_path
            .as_deref(),
        Some("target.rs")
    );
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
