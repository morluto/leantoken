use super::*;

#[test]
fn initial_index_progress_counters_and_phases_are_monotonic() {
    let registry = IndexProgressRegistry::new("cache-namespace".into());
    let first = registry.start(0, &CancellationToken::new());
    let initial = registry.snapshot().expect("initial progress");
    assert_eq!(initial.phase, Some(IndexProgressPhase::Discovery));
    assert_eq!(initial.update_sequence, Some(1));
    assert_eq!(initial.files_prepared, Some(0));

    first.discovered(12, 7, 4_096);
    first.phase(IndexProgressPhase::HashAndPlan);
    first.phase(IndexProgressPhase::Preparation);
    first.prepared_batch(4);
    first.phase(IndexProgressPhase::RelationalWrite);
    first.staged(3);
    for phase in [
        IndexProgressPhase::ChunkWordFts,
        IndexProgressPhase::ChunkTrigramFts,
        IndexProgressPhase::SymbolFts,
        IndexProgressPhase::ReferenceFts,
        IndexProgressPhase::CommitAndCheckpoint,
    ] {
        first.phase(phase);
        assert_eq!(
            registry.snapshot().expect("phase progress").phase,
            Some(phase)
        );
    }
    let advanced = registry.snapshot().expect("advanced progress");
    assert_eq!(advanced.walk_entries, Some(12));
    assert_eq!(advanced.files_discovered, Some(7));
    assert_eq!(advanced.discovered_source_bytes, Some(4_096));
    assert_eq!(advanced.files_prepared, Some(4));
    assert_eq!(advanced.files_staged, Some(3));
    assert_eq!(advanced.preparation_batches, Some(1));
    assert!(
        advanced.update_sequence.expect("advanced sequence")
            > initial.update_sequence.expect("initial sequence")
    );
}

#[test]
fn new_index_progress_attempt_resets_counters_and_rejects_stale_guards() {
    let registry = IndexProgressRegistry::new("cache-namespace".into());
    let first = registry.start(0, &CancellationToken::new());
    first.prepared_batch(4);
    first.staged(3);
    let first_id = registry.snapshot().expect("first attempt").attempt_id;

    let second = registry.start(0, &CancellationToken::new());
    let reset = registry.snapshot().expect("replacement attempt");
    assert_ne!(reset.attempt_id, first_id);
    assert_eq!(reset.phase, Some(IndexProgressPhase::Discovery));
    assert_eq!(reset.files_prepared, Some(0));
    assert_eq!(reset.files_staged, Some(0));

    drop(first);
    assert_eq!(
        registry.snapshot().expect("current attempt").attempt_id,
        reset.attempt_id,
        "an old attempt guard must not overwrite its replacement"
    );
    drop(second);
}

#[test]
fn index_progress_terminal_states_and_takeover_are_attempt_scoped() {
    let registry = IndexProgressRegistry::new("cache-namespace".into());
    let second_cancellation = CancellationToken::new();
    let second = registry.start(0, &second_cancellation);
    second_cancellation.cancel();
    drop(second);
    let cancelled = registry.snapshot().expect("cancelled attempt");
    assert!(!cancelled.active);
    assert_eq!(cancelled.phase, Some(IndexProgressPhase::Cancelled));

    let failed = registry.start(0, &CancellationToken::new());
    drop(failed);
    let failed = registry.snapshot().expect("failed attempt");
    assert!(!failed.active);
    assert_eq!(failed.phase, Some(IndexProgressPhase::Failed));

    let mut completed = registry.start(0, &CancellationToken::new());
    completed.phase(IndexProgressPhase::CommitAndCheckpoint);
    completed.complete(1);
    let published = registry.snapshot().expect("completed attempt");
    assert!(!published.active);
    assert_eq!(published.current_generation, 1);
    assert_eq!(published.phase, Some(IndexProgressPhase::Completed));

    let takeover = IndexProgressRegistry::new("cache-namespace".into());
    assert_eq!(
        takeover.snapshot(),
        None,
        "a replacement process or leader must not inherit stale process-local counters"
    );
    let takeover_attempt = takeover.start(0, &CancellationToken::new());
    assert_ne!(
        takeover.snapshot().expect("takeover progress").attempt_id,
        published.attempt_id
    );
    drop(takeover_attempt);
}

#[test]
fn initial_reconcile_reports_completed_aggregate_progress() {
    let root = tempfile::tempdir().expect("root");
    fs::write(root.path().join("one.rs"), "fn one() {}\n").expect("first source");
    fs::write(root.path().join("two.rs"), "fn two() {}\n").expect("second source");
    let mut config =
        Config::discover(root.path(), Some(root.path().join("index.sqlite"))).expect("config");
    config.max_prepare_batch_files = 1;
    let storage = Storage::open(&config.database_path).expect("storage");
    let indexer = Indexer::new(Arc::new(config), storage).expect("indexer");

    let response = indexer.reconcile(false).expect("initial reconcile");
    let progress = indexer.progress_snapshot().expect("completed progress");

    assert_eq!(progress.phase, Some(IndexProgressPhase::Completed));
    assert!(!progress.active);
    assert_eq!(progress.current_generation, response.repository_generation);
    assert_eq!(progress.files_discovered, Some(2));
    assert_eq!(progress.files_prepared, Some(2));
    assert_eq!(progress.files_staged, Some(2));
    assert_eq!(progress.preparation_batches, Some(2));
}

#[test]
fn cancelled_initial_reconcile_reports_cancelled_terminal_progress() {
    let root = tempfile::tempdir().expect("root");
    fs::write(root.path().join("lib.rs"), "fn pending() {}\n").expect("source");
    let config =
        Config::discover(root.path(), Some(root.path().join("index.sqlite"))).expect("config");
    let storage = Storage::open(&config.database_path).expect("storage");
    let indexer = Indexer::new(Arc::new(config), storage).expect("indexer");
    let cancellation = CancellationToken::new();

    let error = indexer
        .reconcile_once_with_preparation_hook(false, &cancellation, || cancellation.cancel())
        .expect_err("cancelled reconcile");
    assert!(matches!(error, Error::Cancelled));
    let progress = indexer.progress_snapshot().expect("cancelled progress");
    assert!(!progress.active);
    assert_eq!(progress.phase, Some(IndexProgressPhase::Cancelled));
    assert_eq!(progress.current_generation, 0);
}

#[test]
fn cancellation_is_checked_at_each_publication_progress_boundary() {
    for (publication_phase, progress_phase) in [
        (
            ReconciliationPublicationPhase::ChunkWordFts,
            IndexProgressPhase::ChunkWordFts,
        ),
        (
            ReconciliationPublicationPhase::ChunkTrigramFts,
            IndexProgressPhase::ChunkTrigramFts,
        ),
        (
            ReconciliationPublicationPhase::SymbolFts,
            IndexProgressPhase::SymbolFts,
        ),
        (
            ReconciliationPublicationPhase::ReferenceFts,
            IndexProgressPhase::ReferenceFts,
        ),
        (
            ReconciliationPublicationPhase::CommitAndCheckpoint,
            IndexProgressPhase::CommitAndCheckpoint,
        ),
    ] {
        let registry = IndexProgressRegistry::new("cache-namespace".into());
        let cancellation = CancellationToken::new();
        let progress = registry.start(0, &cancellation);
        cancellation.cancel();

        let error = observe_publication_phase(Some(&progress), &cancellation, publication_phase)
            .expect_err("publication boundary must observe cancellation");
        assert!(
            matches!(error, Error::Cancelled),
            "publication phase: {publication_phase:?}"
        );
        assert_eq!(
            registry.snapshot().expect("boundary progress").phase,
            Some(progress_phase),
            "publication phase: {publication_phase:?}"
        );
        drop(progress);
        let terminal = registry.snapshot().expect("cancelled progress");
        assert!(!terminal.active, "publication phase: {publication_phase:?}");
        assert_eq!(
            terminal.phase,
            Some(IndexProgressPhase::Cancelled),
            "publication phase: {publication_phase:?}"
        );
        assert_eq!(
            terminal.current_generation, 0,
            "publication phase: {publication_phase:?}"
        );
    }
}

#[test]
fn cancellation_after_publication_returns_committed_success_and_completed_progress() {
    let root = tempfile::tempdir().expect("root");
    fs::write(root.path().join("lib.rs"), "fn committed() {}\n").expect("source");
    let config =
        Config::discover(root.path(), Some(root.path().join("index.sqlite"))).expect("config");
    let storage = Storage::open(&config.database_path).expect("storage");
    let indexer = Indexer::new(Arc::new(config), storage.clone()).expect("indexer");
    let cancellation = CancellationToken::new();

    let response = indexer
        .reconcile_once_with_post_publication_hook(false, &cancellation, || cancellation.cancel())
        .expect("a committed generation must not be reported as cancelled")
        .report;

    assert_eq!(response.repository_generation, 1);
    assert_eq!(
        storage
            .meta()
            .expect("published metadata")
            .repository_generation,
        1
    );
    assert_eq!(
        storage
            .search_word("committed", 10)
            .expect("published search")
            .len(),
        1
    );
    let progress = indexer.progress_snapshot().expect("completed progress");
    assert!(!progress.active);
    assert_eq!(progress.phase, Some(IndexProgressPhase::Completed));
    assert_eq!(progress.current_generation, 1);
}

#[test]
fn targeted_cancellation_after_publication_returns_committed_success() {
    let root = tempfile::tempdir().expect("root");
    let source = root.path().join("lib.rs");
    fs::write(&source, "fn before() {}\n").expect("source");
    let config =
        Config::discover(root.path(), Some(root.path().join("index.sqlite"))).expect("config");
    let storage = Storage::open(&config.database_path).expect("storage");
    let indexer = Indexer::new(Arc::new(config), storage.clone()).expect("indexer");
    indexer.reconcile(false).expect("initial reconcile");
    fs::write(&source, "fn after() {}\n").expect("updated source");
    let cancellation = CancellationToken::new();

    let response = indexer
        .reconcile_paths_once_with_post_publication_hook(&["lib.rs".into()], &cancellation, || {
            cancellation.cancel()
        })
        .expect("a targeted committed generation must not be reported as cancelled");

    assert_eq!(response.repository_generation, 2);
    assert_eq!(
        storage
            .search_word("after", 10)
            .expect("published targeted search")
            .len(),
        1
    );
    assert_eq!(
        storage
            .search_word("before", 10)
            .expect("removed targeted search")
            .len(),
        0
    );
}

fn assert_skip_reasons(
    response: &IndexReport,
    binary: usize,
    oversized_during_read: usize,
    failed: usize,
) {
    assert_eq!(
        response.skip_reasons.as_ref(),
        Some(&IndexSkipReasonCounts {
            binary,
            oversized_during_read,
            failed,
        })
    );
    assert_eq!(
        response.files_skipped,
        response
            .skip_reasons
            .as_ref()
            .expect("current skip reasons")
            .total()
    );
}

#[test]
fn visibility_reconcile_does_not_reclassify_exclusion_after_discovery() {
    let root = tempfile::tempdir().expect("root");
    fs::create_dir(root.path().join(".git")).expect("git marker");
    let excluded_path = root.path().join("excluded.rs");
    fs::write(&excluded_path, "fn excluded() {}\n").expect("source fixture");
    fs::write(root.path().join(".gitignore"), "").expect("initial ignore");
    let config = Arc::new(
        Config::discover(root.path(), Some(root.path().join("index.sqlite"))).expect("config"),
    );
    let storage = Storage::open(&config.database_path).expect("storage");
    let indexer = Indexer::new(config, storage.clone()).expect("indexer");
    indexer.reconcile(false).expect("initial reconcile");
    fs::write(root.path().join(".gitignore"), "excluded.rs\n").expect("exclude source");

    let response = indexer
        .reconcile_paths_once_with_hooks(
            &[".gitignore".into()],
            &CancellationToken::new(),
            || fs::remove_file(&excluded_path).expect("remove after discovery"),
            || {},
        )
        .expect("visibility reconcile");

    assert_eq!(response.files_seen, 1);
    assert_eq!(response.files_indexed, 1);
    assert_eq!(response.files_removed, 1);
    assert!(storage.find_file("excluded.rs").expect("lookup").is_none());
}

#[test]
fn full_reconcile_counts_every_preparation_skip_reason() {
    let root = tempfile::tempdir().expect("root");
    let indexed_path = root.path().join("indexed.rs");
    let binary_path = root.path().join("binary.rs");
    let growing_path = root.path().join("growing.rs");
    let failed_path = root.path().join("failed.rs");
    fs::write(&indexed_path, "fn indexed() {}\n").expect("indexed fixture");
    fs::write(&binary_path, b"\0binary").expect("binary fixture");
    fs::write(&growing_path, "fn growing() {}\n").expect("growing fixture");
    fs::write(&failed_path, "fn failed() {}\n").expect("failed fixture");

    let mut config =
        Config::discover(root.path(), Some(root.path().join("index.sqlite"))).expect("config");
    config.max_file_bytes = 64;
    let storage = Storage::open(&config.database_path).expect("storage");
    let indexer = Indexer::new(Arc::new(config), storage.clone()).expect("indexer");

    let response = indexer
        .reconcile_once_with_preparation_hook(false, &CancellationToken::new(), move || {
            fs::write(growing_path, vec![b'x'; 65]).expect("grow after discovery");
            fs::remove_file(failed_path).expect("remove after discovery");
        })
        .expect("full reconcile")
        .report;

    assert_eq!(response.files_seen, 4);
    assert_eq!(response.files_indexed, 1);
    assert_eq!(response.files_unchanged, 0);
    assert_eq!(response.files_removed, 0);
    assert_skip_reasons(&response, 1, 1, 1);
    assert_eq!(response.warnings.len(), 1);
    assert!(response.warnings[0].starts_with("failed.rs: "));
    assert!(storage.find_file("indexed.rs").expect("indexed").is_some());
    assert!(storage.find_file("binary.rs").expect("binary").is_none());
    assert!(storage.find_file("growing.rs").expect("growing").is_none());
    assert!(storage.find_file("failed.rs").expect("failed").is_none());
}

#[test]
fn incremental_reconcile_counts_every_preparation_skip_reason() {
    let root = tempfile::tempdir().expect("root");
    let indexed_path = root.path().join("indexed.rs");
    let binary_path = root.path().join("binary.rs");
    let growing_path = root.path().join("growing.rs");
    let failed_path = root.path().join("failed.rs");
    for path in [&indexed_path, &binary_path, &growing_path, &failed_path] {
        fs::write(path, "fn original() {}\n").expect("initial fixture");
    }

    let mut config =
        Config::discover(root.path(), Some(root.path().join("index.sqlite"))).expect("config");
    config.max_file_bytes = 64;
    let storage = Storage::open(&config.database_path).expect("storage");
    let indexer = Indexer::new(Arc::new(config), storage.clone()).expect("indexer");
    indexer.reconcile(false).expect("initial reconcile");

    fs::write(&indexed_path, "fn replacement() {}\n").expect("indexed replacement");
    fs::write(&binary_path, b"\0binary").expect("binary replacement");
    fs::write(&growing_path, "fn changed_growing() {}\n").expect("growing replacement");
    fs::write(&failed_path, "fn changed_failed() {}\n").expect("failed replacement");
    let paths = vec![
        "indexed.rs".into(),
        "binary.rs".into(),
        "growing.rs".into(),
        "failed.rs".into(),
    ];
    let response = indexer
        .reconcile_paths_once_with_preparation_hook(&paths, &CancellationToken::new(), move || {
            fs::write(growing_path, vec![b'x'; 65]).expect("grow after admission");
            fs::remove_file(failed_path).expect("remove after admission");
        })
        .expect("incremental reconcile");

    assert_eq!(response.files_seen, 4);
    assert_eq!(response.files_indexed, 1);
    assert_eq!(response.files_unchanged, 0);
    assert_eq!(response.files_removed, 2);
    assert_skip_reasons(&response, 1, 1, 1);
    assert_eq!(response.warnings.len(), 1);
    assert!(response.warnings[0].starts_with("failed.rs: "));
    assert!(storage.find_file("indexed.rs").expect("indexed").is_some());
    assert!(storage.find_file("binary.rs").expect("binary").is_none());
    assert!(storage.find_file("growing.rs").expect("growing").is_none());
    assert!(storage.find_file("failed.rs").expect("failed").is_some());
}

#[test]
fn conservative_import_resolution_requires_one_existing_file() {
    let paths = [
        "src/app.ts".to_string(),
        "src/lib.ts".to_string(),
        "src/pkg/index.ts".to_string(),
        "helpers.py".to_string(),
        "pkg/main.py".to_string(),
    ]
    .into_iter()
    .collect();
    assert_eq!(
        resolve_import("src/app.ts", "./lib", &paths).as_deref(),
        Some("src/lib.ts")
    );
    assert_eq!(
        resolve_import("pkg/main.py", "helpers", &paths).as_deref(),
        Some("helpers.py")
    );
    assert_eq!(
        resolve_import("src/app.ts", "./pkg", &paths).as_deref(),
        Some("src/pkg/index.ts")
    );
    assert!(resolve_import("src/app.ts", "external-package", &paths).is_none());
}

#[test]
fn latex_inputs_resolve_relative_tex_files() {
    let paths = [
        "paper/main.tex".to_string(),
        "paper/sections/results.tex".to_string(),
        "paper/appendix.ltx".to_string(),
    ]
    .into_iter()
    .collect();
    assert_eq!(
        resolve_import("paper/main.tex", "sections/results", &paths).as_deref(),
        Some("paper/sections/results.tex")
    );
    assert_eq!(
        resolve_import("paper/main.tex", "appendix.ltx", &paths).as_deref(),
        Some("paper/appendix.ltx")
    );
    assert!(resolve_import("paper/main.tex", "missing", &paths).is_none());
}

#[test]
fn rust_module_symbol_resolves_to_module_file() {
    let paths = ["target.rs".to_string(), "consumer.rs".to_string()]
        .into_iter()
        .collect();
    assert_eq!(
        resolve_import("consumer.rs", "target::item", &paths).as_deref(),
        Some("target.rs")
    );
}

#[test]
fn rust_grouped_import_resolves_to_module_file() {
    let paths = ["target.rs".to_string(), "consumer.rs".to_string()]
        .into_iter()
        .collect();
    assert_eq!(
        resolve_import("consumer.rs", "target::{foo, bar}", &paths).as_deref(),
        Some("target.rs")
    );
}

#[test]
fn rust_aliased_import_resolves_to_module_file() {
    let paths = ["foo.rs".to_string(), "crate_import.rs".to_string()]
        .into_iter()
        .collect();
    assert_eq!(
        resolve_import("crate_import.rs", "crate::foo::{bar as b}", &paths).as_deref(),
        Some("foo.rs")
    );
}

#[test]
fn rust_nested_module_resolves_before_symbol_fallback() {
    let paths = ["src/foo/bar.rs".to_string(), "src/foo.rs".to_string()]
        .into_iter()
        .collect();
    // Full path src/foo/bar.rs exists, so it wins over the shorter prefix.
    assert_eq!(
        resolve_import("src/app.rs", "foo::bar", &paths).as_deref(),
        Some("src/foo/bar.rs")
    );
}

#[test]
fn rust_mod_rs_resolves_for_directory_module() {
    let paths = ["src/pkg/mod.rs".to_string(), "src/app.rs".to_string()]
        .into_iter()
        .collect();
    assert_eq!(
        resolve_import("src/app.rs", "pkg", &paths).as_deref(),
        Some("src/pkg/mod.rs")
    );
}

#[test]
fn python_init_py_resolves_for_directory_package() {
    let paths = ["pkg/__init__.py".to_string(), "main.py".to_string()]
        .into_iter()
        .collect();
    assert_eq!(
        resolve_import("main.py", "pkg", &paths).as_deref(),
        Some("pkg/__init__.py")
    );
}

#[test]
fn python_imports_resolve_by_package_semantics() {
    let paths = [
        "tests/service_test.py",
        "src/service.py",
        "pkg/mod.py",
        "thing.py",
        "other.py",
        "pkg/sub/module.py",
        "pkg/sub/helpers.py",
        "pkg/core.py",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();

    assert_eq!(
        resolve_import("tests/service_test.py", "src.service", &paths).as_deref(),
        Some("src/service.py")
    );
    assert_eq!(
        resolve_import("pkg/sub/module.py", "pkg.mod", &paths).as_deref(),
        Some("pkg/mod.py")
    );
    assert_eq!(
        resolve_import("pkg/sub/module.py", ".helpers", &paths).as_deref(),
        Some("pkg/sub/helpers.py")
    );
    assert_eq!(
        resolve_import("pkg/sub/module.py", "..core", &paths).as_deref(),
        Some("pkg/core.py")
    );
}

#[test]
fn rust_qualified_imports_resolve_from_the_source_module() {
    let paths = [
        "src/foo/bar.rs",
        "src/foo/baz.rs",
        "src/foo/qux.rs",
        "src/foo/mod.rs",
        "src/top.rs",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();

    assert_eq!(
        resolve_import("src/foo/bar.rs", "super::baz::Item", &paths).as_deref(),
        Some("src/foo/baz.rs")
    );
    assert_eq!(
        resolve_import("src/foo/mod.rs", "self::qux::Item", &paths).as_deref(),
        Some("src/foo/qux.rs")
    );
    assert_eq!(
        resolve_import(
            "src/foo/nested/module.rs",
            "super::super::super::top::Item",
            &paths
        )
        .as_deref(),
        Some("src/top.rs")
    );
    assert_eq!(
        resolve_import("src/foo/bar.rs", "crate::top::Item", &paths).as_deref(),
        Some("src/top.rs")
    );
}

#[test]
fn import_resolution_honors_cancellation() {
    let mut files = vec![IndexedFile {
        path: "src/app.ts".into(),
        language: Some("typescript".into()),
        structurally_complete: true,
        size_bytes: 1,
        modified_ns: None,
        content_hash: "hash".into(),
        chunks: Vec::new(),
        symbols: Vec::new(),
        references: Vec::new(),
        imports: vec![ImportInput {
            raw_target: "./lib".into(),
            resolved_path: None,
            candidate_paths: Vec::new(),
            line: 1,
        }],
    }];
    let paths = ["src/app.ts".to_string(), "src/lib.ts".to_string()]
        .into_iter()
        .collect();
    let cancellation = CancellationToken::new();
    cancellation.cancel();

    assert!(matches!(
        resolve_imports(&mut files, &paths, &cancellation),
        Err(Error::Cancelled)
    ));
}

#[test]
fn parser_cancellation_is_not_downgraded_to_a_file_warning() {
    let root = tempfile::tempdir().expect("root");
    let absolute_path = root.path().join("cancelled.rs");
    let source = "fn cancelled() {}\n";
    std::fs::write(&absolute_path, source).expect("source");
    let file = DiscoveredFile {
        absolute_path,
        relative_path: "cancelled.rs".into(),
        size_bytes: source.len() as u64,
        modified_ns: None,
    };
    let cancellation = CancellationToken::new();
    cancellation.cancel();
    let repository_root =
        Dir::open_ambient_dir(root.path(), cap_std::ambient_authority()).expect("repository root");

    assert!(matches!(
        prepare_file(
            &repository_root,
            &file,
            80,
            32 * 1024,
            crate::tokens::Tokenizer::default(),
            2 * 1024 * 1024,
            &cancellation,
        ),
        Err(Error::Cancelled)
    ));
}

#[test]
fn parser_content_version_reindexes_legacy_symbol_rows() {
    let root = tempfile::tempdir().expect("root");
    let database = root.path().join("index.sqlite");
    std::fs::write(
        root.path().join("point.rs"),
        "struct Point;\nimpl Point { fn distance(&self) {} }\n",
    )
    .expect("source");
    let config = Arc::new(Config::discover(root.path(), Some(database.clone())).expect("config"));
    let storage = Storage::open(&database).expect("storage");
    let indexer = Indexer::new(config, storage.clone()).expect("indexer");

    let first = indexer.reconcile(false).expect("initial reconcile");
    assert_eq!(first.repository_generation, 1);
    let legacy_hash = indexer.config_hash_for_content_marker(PREVIOUS_INDEX_CONTENT_MARKER);
    let connection = rusqlite::Connection::open(&database).expect("legacy connection");
    connection
            .execute(
                "INSERT INTO symbols(file_id, name, kind, parent, signature, start_line, end_line, start_byte, end_byte)
                 SELECT file_id, name, 'function', name, signature, start_line, end_line, start_byte, end_byte
                 FROM symbols WHERE name = 'distance'",
                [],
            )
            .expect("inject legacy duplicate");
    connection
        .execute(
            "UPDATE meta SET config_hash = ?1 WHERE id = 1",
            rusqlite::params![legacy_hash],
        )
        .expect("set legacy marker");
    drop(connection);
    assert_eq!(
        storage
            .search_symbols("distance", true, 10)
            .expect("legacy symbols")
            .len(),
        2
    );

    let reparsed = indexer.reconcile(false).expect("content-version reparse");
    assert_eq!(reparsed.repository_generation, 2);
    assert_eq!(reparsed.files_indexed, 1);
    assert_eq!(reparsed.files_unchanged, 0);
    let symbols = storage
        .search_symbols("distance", true, 10)
        .expect("reparsed symbols");
    assert_eq!(symbols.len(), 1);
    assert_eq!(symbols[0].symbol.kind, "method");
    assert_eq!(symbols[0].symbol.parent.as_deref(), Some("Point"));
    assert_eq!(
        storage.meta().expect("metadata").config_hash,
        indexer.config_hash()
    );
}

#[test]
fn bounded_read_stops_at_limit_plus_one() {
    let root = tempfile::tempdir().expect("root");
    let path = root.path().join("growing.rs");
    std::fs::write(&path, "12345").expect("source");

    assert_eq!(
        read_bounded_path(&path, 5).expect("boundary"),
        Some(b"12345".to_vec())
    );
    assert_eq!(read_bounded_path(&path, 4).expect("limit plus one"), None);
}

#[cfg(unix)]
#[test]
fn preparation_never_publishes_external_symlink_targets() {
    use std::os::unix::fs::symlink;

    let root = tempfile::tempdir().expect("root");
    let outside = tempfile::tempdir().expect("outside");
    let inside_path = root.path().join("inside.rs");
    let external_path = outside.path().join("external.rs");
    fs::write(&inside_path, "fn inside_original() {}\n").expect("inside");
    fs::write(&external_path, "fn external_marker_needle() {}\n").expect("external");
    let config = Arc::new(
        Config::discover(root.path(), Some(root.path().join("index.sqlite"))).expect("config"),
    );
    let storage = Storage::open(&config.database_path).expect("storage");
    let indexer = Indexer::new(config, storage.clone()).expect("indexer");

    let inside_for_full = inside_path.clone();
    let external_for_full = external_path.clone();
    indexer
        .reconcile_once_with_preparation_hook(false, &CancellationToken::new(), move || {
            fs::remove_file(&inside_for_full).expect("remove discovered file");
            symlink(external_for_full, inside_for_full).expect("replace with symlink");
        })
        .expect("bounded full reconcile");
    assert!(storage.find_file("inside.rs").expect("lookup").is_none());

    fs::remove_file(&inside_path).expect("remove symlink");
    fs::write(&inside_path, "fn inside_original() {}\n").expect("restore inside");
    indexer.reconcile(false).expect("initial safe index");
    let original = storage
        .find_file("inside.rs")
        .expect("lookup")
        .expect("indexed inside");
    let inside_for_targeted = inside_path.clone();
    let external_for_targeted = external_path.clone();
    indexer
        .reconcile_paths_once_with_preparation_hook(
            &["inside.rs".into()],
            &CancellationToken::new(),
            move || {
                fs::remove_file(&inside_for_targeted).expect("remove discovered file");
                symlink(external_for_targeted, inside_for_targeted).expect("replace with symlink");
            },
        )
        .expect("bounded targeted reconcile");
    let preserved = storage
        .find_file("inside.rs")
        .expect("lookup")
        .expect("preserved inside");
    assert_eq!(preserved.content_hash, original.content_hash);
}

#[test]
fn actual_prepared_bytes_drive_aggregate_limit_and_stored_size() {
    let root = tempfile::tempdir().expect("root");
    let path = root.path().join("growing.rs");
    fs::write(&path, "x").expect("source");
    let mut config =
        Config::discover(root.path(), Some(root.path().join("index.sqlite"))).expect("config");
    config.max_file_bytes = 64;
    config.max_prepare_batch_bytes = 64;
    config.max_total_source_bytes = 16;
    let storage = Storage::open(&config.database_path).expect("storage");
    let indexer = Indexer::new(Arc::new(config), storage.clone()).expect("indexer");
    let generation = storage.meta().expect("metadata").repository_generation;
    let growing = path.clone();

    let error = indexer
        .reconcile_once_with_preparation_hook(false, &CancellationToken::new(), move || {
            fs::write(growing, vec![b'x'; 32]).expect("grow after discovery")
        })
        .expect_err("actual bytes must cross aggregate limit");
    assert!(matches!(
        error,
        Error::IndexLimitExceeded {
            kind: crate::IndexLimitKind::TotalSourceBytes,
            observed: 32,
            limit: 16
        }
    ));
    assert_eq!(
        storage.meta().expect("metadata").repository_generation,
        generation
    );
    assert!(storage.find_file("growing.rs").expect("lookup").is_none());

    fs::write(&path, "x").expect("reset source");
    let mut config =
        Config::discover(root.path(), Some(root.path().join("index.sqlite"))).expect("config");
    config.max_file_bytes = 64;
    config.max_prepare_batch_bytes = 64;
    config.max_total_source_bytes = 48;
    let indexer = Indexer::new(Arc::new(config), storage.clone()).expect("indexer");
    indexer
        .reconcile_once_with_preparation_hook(false, &CancellationToken::new(), move || {
            fs::write(path, vec![b'x'; 32]).expect("grow within limit")
        })
        .expect("reconcile");
    assert_eq!(
        storage
            .find_file("growing.rs")
            .expect("lookup")
            .expect("indexed")
            .size_bytes,
        32
    );

    let generation = storage.meta().expect("metadata").repository_generation;
    fs::write(root.path().join("growing.rs"), "x").expect("reset for targeted");
    let targeted_path = root.path().join("growing.rs");
    let error = indexer
        .reconcile_paths_once_with_preparation_hook(
            &["growing.rs".into()],
            &CancellationToken::new(),
            move || fs::write(targeted_path, vec![b'x'; 64]).expect("grow after discovery"),
        )
        .expect_err("targeted actual bytes must cross aggregate limit");
    assert!(matches!(
        error,
        Error::IndexLimitExceeded {
            kind: crate::IndexLimitKind::TotalSourceBytes,
            observed: 64,
            limit: 48
        }
    ));
    assert_eq!(
        storage.meta().expect("metadata").repository_generation,
        generation
    );
    assert_eq!(
        storage
            .find_file("growing.rs")
            .expect("lookup")
            .expect("preserved")
            .size_bytes,
        32
    );
}

#[test]
fn aggregate_limit_is_enforced_on_final_state_not_candidate_order() {
    let root = tempfile::tempdir().expect("root");
    let growing = root.path().join("a_growing.rs");
    let shrinking = root.path().join("z_shrinking.rs");
    fs::write(&growing, vec![b' '; 40]).expect("growing source");
    fs::write(&shrinking, vec![b' '; 10]).expect("shrinking source");
    let mut config =
        Config::discover(root.path(), Some(root.path().join("index.sqlite"))).expect("config");
    config.max_file_bytes = 64;
    config.max_prepare_batch_bytes = 64;
    config.max_total_source_bytes = 50;
    let storage = Storage::open(&config.database_path).expect("storage");
    let indexer = Indexer::new(Arc::new(config), storage.clone()).expect("indexer");
    indexer.reconcile(false).expect("initial index");

    fs::write(&growing, vec![b' '; 45]).expect("grow source");
    fs::write(&shrinking, vec![b' '; 5]).expect("shrink source");
    indexer
        .reconcile(false)
        .expect("final aggregate remains within limit");

    assert_eq!(
        storage
            .find_file("a_growing.rs")
            .expect("lookup")
            .expect("growing")
            .size_bytes
            + storage
                .find_file("z_shrinking.rs")
                .expect("lookup")
                .expect("shrinking")
                .size_bytes,
        50
    );
}

#[test]
fn reconciliation_can_reduce_an_existing_generation_below_a_new_limit() {
    let root = tempfile::tempdir().expect("root");
    let path = root.path().join("source.rs");
    fs::write(&path, vec![b' '; 60]).expect("source");
    let database = root.path().join("index.sqlite");
    let mut initial = Config::discover(root.path(), Some(database.clone())).expect("config");
    initial.max_file_bytes = 64;
    initial.max_prepare_batch_bytes = 64;
    initial.max_total_source_bytes = 64;
    let storage = Storage::open(&database).expect("storage");
    Indexer::new(Arc::new(initial), storage.clone())
        .expect("indexer")
        .reconcile(false)
        .expect("initial index");

    fs::write(&path, vec![b' '; 40]).expect("shrink source");
    let mut tightened = Config::discover(root.path(), Some(database)).expect("config");
    tightened.max_file_bytes = 64;
    tightened.max_prepare_batch_bytes = 64;
    tightened.max_total_source_bytes = 50;
    Indexer::new(Arc::new(tightened), storage.clone())
        .expect("indexer")
        .reconcile(false)
        .expect("reduce stored aggregate");

    assert_eq!(
        storage
            .find_file("source.rs")
            .expect("lookup")
            .expect("source")
            .size_bytes,
        40
    );
}

#[test]
fn preparation_batches_honor_file_and_byte_boundaries() {
    let candidates = (0..3)
        .map(|index| DiscoveredFile {
            absolute_path: format!("{index}.rs").into(),
            relative_path: format!("{index}.rs"),
            size_bytes: 2,
            modified_ns: None,
        })
        .collect::<Vec<_>>();
    let file_limited = crate::DiscoveryLimits {
        max_file_bytes: 2,
        max_prepare_batch_files: 2,
        max_prepare_batch_bytes: 10,
        ..crate::DiscoveryLimits::default()
    };
    let byte_limited = crate::DiscoveryLimits {
        max_file_bytes: 2,
        max_prepare_batch_files: 3,
        max_prepare_batch_bytes: 3,
        ..crate::DiscoveryLimits::default()
    };
    let oversized_first_file = crate::DiscoveryLimits {
        max_file_bytes: 2,
        max_prepare_batch_files: 3,
        max_prepare_batch_bytes: 1,
        ..crate::DiscoveryLimits::default()
    };

    assert_eq!(prepare_batch_end(&candidates, 0, file_limited), 2);
    assert_eq!(prepare_batch_end(&candidates, 2, file_limited), 3);
    assert_eq!(prepare_batch_end(&candidates, 0, byte_limited), 1);
    assert_eq!(prepare_batch_end(&candidates, 1, byte_limited), 2);
    assert_eq!(prepare_batch_end(&candidates, 0, oversized_first_file), 1);
    assert_eq!(prepare_batch_end(&candidates, 1, oversized_first_file), 2);
}

#[test]
fn preparation_batches_terminate_with_an_oversized_first_candidate() {
    let root = tempfile::tempdir().expect("root");
    let paths = [
        root.path().join("oversized.rs"),
        root.path().join("next.rs"),
    ];
    for path in &paths {
        fs::write(path, "x").expect("fixture");
    }

    let mut config =
        Config::discover(root.path(), Some(root.path().join("index.sqlite"))).expect("config");
    config.max_file_bytes = 2;
    config.max_prepare_batch_files = 2;
    config.max_prepare_batch_bytes = 2;
    let storage = Storage::open(&config.database_path).expect("storage");
    let indexer = Indexer::new(Arc::new(config), storage).expect("indexer");
    let candidates = paths
        .iter()
        .enumerate()
        .map(|(index, path)| DiscoveredFile {
            absolute_path: path.clone(),
            relative_path: path
                .file_name()
                .expect("file name")
                .to_string_lossy()
                .into_owned(),
            size_bytes: if index == 0 { 3 } else { 1 },
            modified_ns: None,
        })
        .collect::<Vec<_>>();
    let mut batches = 0usize;
    let mut consumed = 0usize;

    let metrics = indexer
        .prepare_candidate_batches(
            &candidates,
            &CancellationToken::new(),
            StorageProfiling::Omit,
            |prepared| {
                batches += 1;
                consumed += prepared.len();
                Ok(())
            },
        )
        .expect("oversized first candidate must not stall preparation");

    assert_eq!(batches, 2);
    assert_eq!(consumed, candidates.len());
    assert_eq!(metrics.batches, 2);
}

#[test]
fn worker_pool_is_lazy_and_threads_follow_config_per_indexer() {
    let root = tempfile::tempdir().expect("root");
    let mut config_a =
        Config::discover(root.path(), Some(root.path().join("a.sqlite"))).expect("config a");
    config_a.max_index_workers = 1;
    let storage_a = Storage::open(&config_a.database_path).expect("storage a");
    let indexer_a = Indexer::new(Arc::new(config_a), storage_a).expect("indexer a");

    let mut config_b =
        Config::discover(root.path(), Some(root.path().join("b.sqlite"))).expect("config b");
    config_b.max_index_workers = 3;
    let storage_b = Storage::open(&config_b.database_path).expect("storage b");
    let indexer_b = Indexer::new(Arc::new(config_b), storage_b).expect("indexer b");

    assert!(indexer_a.pool.pool.get().is_none());
    assert!(indexer_b.pool.pool.get().is_none());
    let mut consumed = false;
    indexer_a
        .prepare_candidate_batches(
            &[],
            &CancellationToken::new(),
            StorageProfiling::Omit,
            |_| {
                consumed = true;
                Ok(())
            },
        )
        .expect("empty prepare");
    assert!(!consumed);
    assert!(indexer_a.pool.pool.get().is_none());

    assert_eq!(
        indexer_a
            .pool
            .get_or_build(indexer_a.config.max_index_workers)
            .expect("pool a")
            .current_num_threads(),
        1
    );
    assert_eq!(
        indexer_b
            .pool
            .get_or_build(indexer_b.config.max_index_workers)
            .expect("pool b")
            .current_num_threads(),
        3
    );
}

#[test]
fn cancellation_between_preparation_batches_stops_before_the_next_batch() {
    let root = tempfile::tempdir().expect("root");
    let paths = [root.path().join("a.rs"), root.path().join("b.rs")];
    for path in &paths {
        fs::write(path, "fn item() {}\n").expect("fixture");
    }
    let mut config =
        Config::discover(root.path(), Some(root.path().join("index.sqlite"))).expect("config");
    config.max_prepare_batch_files = 1;
    let storage = Storage::open(&config.database_path).expect("storage");
    let indexer = Indexer::new(Arc::new(config), storage).expect("indexer");
    let candidates = paths
        .iter()
        .map(|path| {
            let metadata = fs::metadata(path).expect("metadata");
            DiscoveredFile {
                absolute_path: path.clone(),
                relative_path: path
                    .file_name()
                    .expect("file name")
                    .to_string_lossy()
                    .into_owned(),
                size_bytes: metadata.len(),
                modified_ns: None,
            }
        })
        .collect::<Vec<_>>();
    let cancellation = CancellationToken::new();
    let mut batches = 0usize;

    let error = indexer
        .prepare_candidate_batches(&candidates, &cancellation, StorageProfiling::Omit, |_| {
            batches += 1;
            cancellation.cancel();
            Ok(())
        })
        .expect_err("second batch must observe cancellation");

    assert!(matches!(error, Error::Cancelled));
    assert_eq!(batches, 1);
}
