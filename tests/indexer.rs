use std::sync::Arc;

use leantoken::Config;
use leantoken::Error;
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
    let indexer = Indexer::new(config, storage.clone()).expect("indexer");

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
fn full_reconcile_limit_error_preserves_the_committed_generation() {
    let root = tempfile::tempdir().expect("root");
    let database = root.path().join("index.sqlite");
    std::fs::write(root.path().join("a.rs"), "fn old() {}\n").expect("a");
    let first_config = Arc::new(Config::discover(root.path(), Some(database.clone())).expect("config"));
    let storage = Storage::open(&database).expect("storage");
    Indexer::new(first_config, storage.clone())
        .expect("indexer")
        .reconcile(false)
        .expect("initial reconcile");
    std::fs::write(root.path().join("b.rs"), "fn new() {}\n").expect("b");

    let mut limited = Config::discover(root.path(), Some(database)).expect("limited config");
    limited.max_files = 1;
    let error = Indexer::new(Arc::new(limited), storage.clone())
        .expect("limited indexer")
        .reconcile(false)
        .expect_err("file limit");

    assert!(matches!(
        error,
        Error::IndexLimitExceeded {
            kind: leantoken::IndexLimitKind::Files,
            observed: 2,
            limit: 1
        }
    ));
    assert_eq!(storage.meta().expect("meta").repository_generation, 1);
    assert!(storage.find_file("a.rs").expect("a").is_some());
    assert!(storage.find_file("b.rs").expect("b").is_none());
}

#[test]
fn targeted_reconcile_enforces_aggregate_bytes_before_publication() {
    let root = tempfile::tempdir().expect("root");
    let database = root.path().join("index.sqlite");
    std::fs::write(root.path().join("a.rs"), "a").expect("a");
    std::fs::write(root.path().join("b.rs"), "b").expect("b");
    let mut config = Config::discover(root.path(), Some(database)).expect("config");
    config.max_total_source_bytes = 2;
    let config = Arc::new(config);
    let storage = Storage::open(&config.database_path).expect("storage");
    let indexer = Indexer::new(config, storage.clone()).expect("indexer");
    indexer.reconcile(false).expect("initial reconcile");

    std::fs::write(root.path().join("a.rs"), "aa").expect("grow a");
    let error = indexer
        .reconcile_paths(&["a.rs".into()])
        .expect_err("aggregate limit");

    assert!(matches!(
        error,
        Error::IndexLimitExceeded {
            kind: leantoken::IndexLimitKind::TotalSourceBytes,
            observed: 3,
            limit: 2
        }
    ));
    assert_eq!(storage.meta().expect("meta").repository_generation, 1);
    assert_eq!(
        storage
            .find_file("a.rs")
            .expect("a")
            .expect("indexed a")
            .size_bytes,
        1
    );
}

#[test]
fn changing_discovery_limits_invalidates_the_index_configuration_hash() {
    let root = tempfile::tempdir().expect("root");
    let database = root.path().join("index.sqlite");
    std::fs::write(root.path().join("a.rs"), "fn stable() {}\n").expect("a");
    let first_config = Arc::new(Config::discover(root.path(), Some(database.clone())).expect("config"));
    let storage = Storage::open(&database).expect("storage");
    Indexer::new(first_config, storage.clone())
        .expect("indexer")
        .reconcile(false)
        .expect("initial reconcile");

    let mut changed = Config::discover(root.path(), Some(database)).expect("changed config");
    changed.max_files -= 1;
    let response = Indexer::new(Arc::new(changed), storage.clone())
        .expect("changed indexer")
        .reconcile(false)
        .expect("configuration rebuild");

    assert_eq!(response.repository_generation, 2);
    assert_eq!(response.files_indexed, 1);
    assert_eq!(storage.meta().expect("meta").repository_generation, 2);
}

#[test]
fn indexer_rejects_invalid_chunk_configuration_at_construction() {
    let root = tempfile::tempdir().expect("root");
    let mut config =
        Config::discover(root.path(), Some(root.path().join("index.sqlite"))).expect("config");
    config.chunk_lines = 0;
    let storage = Storage::open(&config.database_path).expect("storage");

    let error = Indexer::new(Arc::new(config), storage).expect_err("invalid chunk configuration");

    assert!(matches!(error, Error::InvalidConfiguration(_)));
}

#[test]
fn indexer_rejects_zero_discovery_limits_at_construction() {
    let root = tempfile::tempdir().expect("root");
    let mut config =
        Config::discover(root.path(), Some(root.path().join("index.sqlite"))).expect("config");
    config.max_walk_entries = 0;
    let storage = Storage::open(&config.database_path).expect("storage");

    let error = Indexer::new(Arc::new(config), storage).expect_err("invalid discovery limits");

    assert!(matches!(error, Error::InvalidConfiguration(_)));
}

#[test]
fn indexer_reopen_leaves_unchanged_files_and_generation() {
    let root = tempfile::tempdir().expect("root");
    std::fs::write(root.path().join("a.rs"), "fn stable() {}\n").expect("write a");

    let config = Arc::new(
        Config::discover(root.path(), Some(root.path().join("index.sqlite"))).expect("config"),
    );
    let storage = Storage::open(&config.database_path).expect("storage");
    let indexer = Indexer::new(config.clone(), storage.clone()).expect("indexer");

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
    let indexer = Indexer::new(config, storage.clone()).expect("indexer");

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
    let indexer = Indexer::new(config, storage.clone()).expect("indexer");
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
    let indexer = Indexer::new(config, storage.clone()).expect("indexer");
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
    let indexer = Indexer::new(config, storage.clone()).expect("indexer");
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
    let indexer = Indexer::new(config, storage.clone()).expect("indexer");
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
fn targeted_reconcile_applies_deleted_directory_delta() {
    let root = tempfile::tempdir().expect("root");
    std::fs::create_dir(root.path().join("removed")).expect("directory");
    std::fs::write(root.path().join("removed/a.rs"), "fn gone_a() {}\n").expect("a");
    std::fs::write(root.path().join("removed/b.rs"), "fn gone_b() {}\n").expect("b");
    std::fs::write(root.path().join("keep.rs"), "fn keep() {}\n").expect("keep");
    let config = Arc::new(
        Config::discover(root.path(), Some(root.path().join("index.sqlite"))).expect("config"),
    );
    let storage = Storage::open(&config.database_path).expect("storage");
    let indexer = Indexer::new(config, storage.clone()).expect("indexer");
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
fn targeted_reconcile_applies_new_file_and_ignore_deltas() {
    let root = tempfile::tempdir().expect("root");
    std::fs::create_dir(root.path().join(".git")).expect("git marker");
    std::fs::write(root.path().join("keep.rs"), "fn keep() {}\n").expect("write keep");
    std::fs::write(root.path().join("hide.rs"), "fn hide() {}\n").expect("write hide");
    std::fs::write(root.path().join(".gitignore"), "").expect("write ignore");
    let config = Arc::new(
        Config::discover(root.path(), Some(root.path().join("index.sqlite"))).expect("config"),
    );
    let storage = Storage::open(&config.database_path).expect("storage");
    let indexer = Indexer::new(config, storage.clone()).expect("indexer");
    indexer.reconcile(false).expect("initial reconcile");

    std::fs::write(root.path().join("new.rs"), "fn new_file() {}\n").expect("new file");
    let added = indexer
        .reconcile_paths(&["new.rs".into()])
        .expect("new path delta");
    assert_eq!(added.files_seen, 1);
    assert!(storage.find_file("new.rs").expect("find new").is_some());

    std::fs::write(root.path().join(".gitignore"), "hide.rs\n").expect("change ignore");
    let ignored = indexer
        .reconcile_paths(&[".gitignore".into()])
        .expect("ignore delta");
    assert_eq!(ignored.files_seen, 2);
    assert_eq!(ignored.files_indexed, 1);
    assert_eq!(ignored.files_removed, 1);
    assert!(storage.find_file("hide.rs").expect("find hidden").is_none());
}

#[test]
fn targeted_reconcile_applies_leantokenignore_add_change_and_removal() {
    let root = tempfile::tempdir().expect("root");
    std::fs::write(root.path().join("first.rs"), "fn first() {}\n").expect("first");
    std::fs::write(root.path().join("second.rs"), "fn second() {}\n").expect("second");
    let config = Arc::new(
        Config::discover(root.path(), Some(root.path().join("index.sqlite"))).expect("config"),
    );
    let storage = Storage::open(&config.database_path).expect("storage");
    let indexer = Indexer::new(config, storage.clone()).expect("indexer");
    indexer.reconcile(false).expect("initial reconcile");

    std::fs::write(root.path().join(".leantokenignore"), "first.rs\n").expect("add ignore");
    indexer
        .reconcile_paths(&[".leantokenignore".into()])
        .expect("apply added ignore");
    assert!(storage.find_file("first.rs").expect("first lookup").is_none());
    assert!(storage.find_file("second.rs").expect("second lookup").is_some());

    std::fs::write(root.path().join(".leantokenignore"), "second.rs\n")
        .expect("change ignore");
    indexer
        .reconcile_paths(&[".leantokenignore".into()])
        .expect("apply changed ignore");
    assert!(storage.find_file("first.rs").expect("first lookup").is_some());
    assert!(storage.find_file("second.rs").expect("second lookup").is_none());

    std::fs::remove_file(root.path().join(".leantokenignore")).expect("remove ignore");
    indexer
        .reconcile_paths(&[".leantokenignore".into()])
        .expect("apply removed ignore");
    assert!(storage.find_file("first.rs").expect("first lookup").is_some());
    assert!(storage.find_file("second.rs").expect("second lookup").is_some());
}

#[test]
fn changing_generated_policy_invalidates_the_index_configuration_hash() {
    let root = tempfile::tempdir().expect("root");
    let database = root.path().join("index.sqlite");
    std::fs::create_dir(root.path().join("target")).expect("target");
    std::fs::write(root.path().join("target/generated.rs"), "fn generated() {}\n")
        .expect("generated");
    let first_config = Arc::new(Config::discover(root.path(), Some(database.clone())).expect("config"));
    let storage = Storage::open(&database).expect("storage");
    Indexer::new(first_config, storage.clone())
        .expect("indexer")
        .reconcile(false)
        .expect("initial reconcile");
    assert!(storage.find_file("target/generated.rs").expect("lookup").is_none());

    let mut changed = Config::discover(root.path(), Some(database)).expect("changed config");
    changed.include_generated = true;
    let response = Indexer::new(Arc::new(changed), storage.clone())
        .expect("changed indexer")
        .reconcile(false)
        .expect("configuration rebuild");

    assert_eq!(response.repository_generation, 2);
    assert!(storage.find_file("target/generated.rs").expect("lookup").is_some());
}

#[test]
fn preparation_batch_size_does_not_change_the_logical_index() {
    let root = tempfile::tempdir().expect("root");
    for index in 0..6 {
        std::fs::write(
            root.path().join(format!("file_{index}.rs")),
            format!("pub fn item_{index}() {{ item_{}(); }}\n", (index + 1) % 6),
        )
        .expect("fixture");
    }
    let databases = tempfile::tempdir().expect("databases");
    let mut small =
        Config::discover(root.path(), Some(databases.path().join("small.sqlite"))).expect("small");
    small.max_prepare_batch_files = 1;
    let small_storage = Storage::open(&small.database_path).expect("small storage");
    Indexer::new(Arc::new(small), small_storage.clone())
        .expect("small indexer")
        .reconcile(false)
        .expect("small index");

    let mut large =
        Config::discover(root.path(), Some(databases.path().join("large.sqlite"))).expect("large");
    large.max_prepare_batch_files = 64;
    let large_storage = Storage::open(&large.database_path).expect("large storage");
    Indexer::new(Arc::new(large), large_storage.clone())
        .expect("large indexer")
        .reconcile(false)
        .expect("large index");

    let project = |storage: &Storage| {
        storage
            .list_files(100, None)
            .expect("files")
            .into_iter()
            .map(|file| (file.path, file.content_hash, file.size_bytes))
            .collect::<Vec<_>>()
    };
    assert_eq!(project(&small_storage), project(&large_storage));
    assert_eq!(
        small_storage.counts().expect("small counts").files,
        large_storage.counts().expect("large counts").files
    );
    assert_eq!(
        small_storage.search_word("item_3", 100).expect("small search").len(),
        large_storage.search_word("item_3", 100).expect("large search").len()
    );
}

#[test]
fn profiled_reconcile_reports_bounded_batch_high_water_and_phases() {
    let root = tempfile::tempdir().expect("root");
    let mut total_bytes = 0u64;
    for index in 0..3 {
        let source = format!("fn item_{index}() {{}}\n");
        total_bytes += u64::try_from(source.len()).expect("source length");
        std::fs::write(root.path().join(format!("file_{index}.rs")), source).expect("fixture");
    }
    let database = tempfile::tempdir().expect("database");
    let mut config =
        Config::discover(root.path(), Some(database.path().join("index.sqlite"))).expect("config");
    config.max_prepare_batch_files = 2;
    let storage = Storage::open(&config.database_path).expect("storage");
    let profiled = Indexer::new(Arc::new(config), storage)
        .expect("indexer")
        .reconcile_profiled(false)
        .expect("profiled reconcile");

    assert_eq!(profiled.response.files_indexed, 3);
    assert_eq!(profiled.diagnostics.discovered_files, 3);
    assert_eq!(profiled.diagnostics.discovered_source_bytes, total_bytes);
    assert_eq!(profiled.diagnostics.preparation_batches, 2);
    assert_eq!(profiled.diagnostics.max_batch_files, 2);
    assert!(profiled.diagnostics.max_batch_source_bytes <= total_bytes);
    assert!(profiled.diagnostics.total_ms >= profiled.diagnostics.discovery_ms);
    assert!(profiled.diagnostics.publication_ms >= profiled.diagnostics.preparation_ms);
}

#[test]
fn new_file_delta_resolves_existing_importers() {
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
    let indexer = Indexer::new(config, storage.clone()).expect("indexer");
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
    let response = indexer
        .reconcile_paths(&["target.rs".into(), "consumer.rs".into()])
        .expect("new target delta");
    assert_eq!(response.files_indexed, 2);

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
    let indexer = Indexer::new(config, storage.clone()).expect("indexer");

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
    let indexer = Indexer::new(config, storage.clone()).expect("indexer");

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
    let indexer = Indexer::new(config, storage.clone()).expect("indexer");

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

#[test]
fn full_reconcile_reindexes_when_content_changes_but_size_and_mtime_match() {
    use std::fs::{File, FileTimes};

    let root = tempfile::tempdir().expect("root");
    let path = root.path().join("twin.rs");
    // Same-length payloads so size_bytes matches after overwrite.
    let original = "fn alpha_v1() {}\n";
    let updated = "fn beta__v2() {}\n";
    assert_eq!(original.len(), updated.len(), "fixture sizes must match");
    std::fs::write(&path, original).expect("write original");

    let config = Arc::new(
        Config::discover(root.path(), Some(root.path().join("index.sqlite"))).expect("config"),
    );
    let storage = Storage::open(&config.database_path).expect("storage");
    let indexer = Indexer::new(config, storage.clone()).expect("indexer");

    let first = indexer.reconcile(false).expect("first reconcile");
    assert_eq!(first.files_indexed, 1);
    assert_eq!(first.repository_generation, 1);
    assert!(!storage.search_word("alpha_v1", 10).expect("search").is_empty());

    let original_meta = std::fs::metadata(&path).expect("metadata");
    let original_mtime = original_meta.modified().expect("mtime before");
    std::fs::write(&path, updated).expect("overwrite same-size content");
    // Portable mtime restore for Windows/macOS/Linux CI (no external touch -r).
    let file = File::options()
        .write(true)
        .open(&path)
        .expect("open for set_times");
    file.set_times(FileTimes::new().set_modified(original_mtime))
        .expect("restore mtime");
    drop(file);

    let after_meta = std::fs::metadata(&path).expect("metadata after");
    assert_eq!(after_meta.len(), original_meta.len());
    assert_eq!(
        after_meta.modified().expect("mtime after"),
        original_mtime
    );

    let second = indexer.reconcile(false).expect("second reconcile");
    assert_eq!(
        second.files_indexed, 1,
        "content-hash must detect same-size mtime-preserved rewrite"
    );
    assert_eq!(second.repository_generation, 2);
    assert!(storage.search_word("alpha_v1", 10).expect("old").is_empty());
    assert!(!storage.search_word("beta__v2", 10).expect("new").is_empty());
}
