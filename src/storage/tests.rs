use super::*;

#[test]
pub(crate) fn structural_search_uses_complete_unicode_case_fold_candidates() {
    let root = tempfile::tempdir().expect("root");
    let storage = Storage::open(root.path().join("index.sqlite")).expect("storage");
    let mut file = sample_file("unicode.rs", "fn Აbc() { ſſſſſſ(); }\n");
    file.symbols = vec![
        SymbolInput {
            name: "Აbc".into(),
            kind: "function".into(),
            parent: None,
            signature: None,
            start_line: 1,
            end_line: 1,
            start_byte: 3,
            end_byte: 8,
        },
        SymbolInput {
            name: "ſſſſſſ".into(),
            kind: "function".into(),
            parent: None,
            signature: None,
            start_line: 1,
            end_line: 1,
            start_byte: 13,
            end_byte: 25,
        },
    ];
    file.references = vec![
        ReferenceInput {
            name: "Აbc".into(),
            kind: "call".into(),
            role: ReferenceRole::Reference,
            enclosing_symbol: None,
            start_line: 1,
            end_line: 1,
            start_byte: 3,
            end_byte: 8,
        },
        ReferenceInput {
            name: "ſſſſſſ".into(),
            kind: "call".into(),
            role: ReferenceRole::Reference,
            enclosing_symbol: None,
            start_line: 1,
            end_line: 1,
            start_byte: 13,
            end_byte: 25,
        },
    ];
    storage
        .full_reconcile("config", vec![file])
        .expect("index fixture");

    assert_eq!(
        storage
            .search_symbols("აbc", false, 10)
            .expect("expanded Unicode symbol search")[0]
            .symbol
            .name,
        "Აbc"
    );
    assert_eq!(
        storage
            .search_references("აbc", false, 10)
            .expect("expanded Unicode reference search")[0]
            .reference
            .name,
        "Აbc"
    );
    assert_eq!(
        storage
            .search_symbols("ssssss", false, 10)
            .expect("bounded variant-overflow fallback")[0]
            .symbol
            .name,
        "ſſſſſſ"
    );
    assert_eq!(
        storage
            .search_references("ssssss", false, 10)
            .expect("bounded reference variant-overflow fallback")[0]
            .reference
            .name,
        "ſſſſſſ"
    );
}

#[test]
pub(crate) fn unicode_case_fold_fallback_fails_before_truncating_structural_rows() {
    let root = tempfile::tempdir().expect("root");
    let storage = Storage::open(root.path().join("index.sqlite")).expect("storage");
    let mut file = sample_file("many.rs", "fn placeholder() {}\n");
    file.symbols = (0..=HARD_MAX_RESULTS)
        .map(|index| SymbolInput {
            name: format!("ordinary_{index}"),
            kind: "function".into(),
            parent: None,
            signature: None,
            start_line: 1,
            end_line: 1,
            start_byte: 0,
            end_byte: 1,
        })
        .collect();
    storage
        .full_reconcile("config", vec![file])
        .expect("index bounded fixture");

    let error = storage
        .search_symbols("ssssss", false, 10)
        .expect_err("variant-overflow fallback must not truncate its structural scan");
    assert!(matches!(
        error,
        Error::RetrievalLimitExceeded {
            kind: RetrievalLimitKind::UnicodeCaseFoldRows,
            observed: 10_001,
            limit: HARD_MAX_RESULTS,
        }
    ));
}

#[test]
pub(crate) fn scoped_regex_row_limit_reports_the_governing_bound() {
    let root = tempfile::tempdir().expect("root");
    let storage = Storage::open(root.path().join("index.sqlite")).expect("storage");
    storage
        .full_reconcile(
            "config",
            vec![
                sample_file("alpha.rs", "const needle_alpha: bool = true;\n"),
                sample_file("bravo.rs", "const needle_bravo: bool = true;\n"),
            ],
        )
        .expect("index fixture");
    let session = storage.begin_read().expect("read session");

    let error = session
        .select_scoped_regex_candidate_ids("\"needle\"", 1, 10, &[], &[], |_| true)
        .expect_err("second FTS row crosses the scan bound");

    assert!(matches!(
        error,
        Error::RetrievalLimitExceeded {
            kind: RetrievalLimitKind::RegexScopedRows,
            observed: 2,
            limit: 1,
        }
    ));
}

#[test]
pub(crate) fn writer_bounds_recycled_wal_size() {
    let root = tempfile::tempdir().expect("root");
    let storage = Storage::open(root.path().join("index.sqlite")).expect("storage");
    let connection = storage
        .writer
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let limit: i64 = connection
        .query_row("PRAGMA journal_size_limit", [], |row| row.get(0))
        .expect("journal size limit");
    assert_eq!(limit, WAL_JOURNAL_SIZE_LIMIT_BYTES);
}

#[test]
pub(crate) fn ordinary_noop_does_not_autocheckpoint_existing_wal_backlog() {
    let root = tempfile::tempdir().expect("root");
    let database = root.path().join("index.sqlite");
    let storage = Storage::open(&database).expect("storage");
    {
        let writer = storage
            .writer
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        writer
            .pragma_update(None, "wal_autocheckpoint", 0)
            .expect("disable auto-checkpoint for backlog fixture");
    }
    storage
        .full_reconcile(
            "config",
            vec![sample_file(
                "backlog.rs",
                &"checkpoint backlog\n".repeat(32_768),
            )],
        )
        .expect("backlogged generation");
    {
        let writer = storage
            .writer
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        writer
            .pragma_update(None, "wal_autocheckpoint", 1)
            .expect("arm auto-checkpoint for no-op");
    }
    let wal_bytes_before = fs::metadata(wal_path(&database))
        .expect("backlogged WAL")
        .len();
    assert!(wal_bytes_before > 0);
    let database_hash_before =
        crate::text::hash_bytes(&fs::read(&database).expect("database bytes before no-op"));

    let generation = storage
        .reconcile_files("config", Vec::new(), &[])
        .expect("ordinary no-op");

    assert_eq!(generation, 1);
    assert_eq!(
        fs::metadata(wal_path(&database))
            .expect("WAL after no-op")
            .len(),
        wal_bytes_before
    );
    assert_eq!(
        crate::text::hash_bytes(&fs::read(&database).expect("database bytes after no-op")),
        database_hash_before,
        "a no-change baseline verification must not copy WAL pages into the main database"
    );
}

#[test]
pub(crate) fn repository_open_does_not_checkpoint_existing_wal_backlog() {
    let root = tempfile::tempdir().expect("root");
    let repository = root.path().join("repository");
    fs::create_dir(&repository).expect("repository");
    let database = root.path().join("index.sqlite");
    let storage = Storage::open(&database).expect("storage");
    storage
        .bind_repository_at(&repository, None, 1_234)
        .expect("initial repository binding");
    let (auto_checkpoint_pages, page_size) = {
        let writer = storage
            .writer
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let pages = writer
            .query_row("PRAGMA wal_autocheckpoint", [], |row| row.get::<_, i64>(0))
            .expect("default auto-checkpoint pages");
        let page_size = writer
            .query_row("PRAGMA page_size", [], |row| row.get::<_, i64>(0))
            .expect("database page size");
        writer
            .pragma_update(None, "wal_autocheckpoint", 0)
            .expect("disable auto-checkpoint for backlog fixture");
        (pages, page_size)
    };
    assert!(auto_checkpoint_pages > 0);
    assert!(page_size > 0);
    let payload = "x".repeat(512 * 1024);
    storage
        .full_reconcile(
            "config",
            (0..8)
                .map(|index| {
                    sample_file(
                        &format!("backlog-{index}.rs"),
                        &format!("file {index}\n{payload}"),
                    )
                })
                .collect(),
        )
        .expect("backlogged generation");
    let reader = storage.begin_read().expect("latest pinned reader");
    assert_eq!(
        reader.repository_generation().expect("pinned generation"),
        1
    );
    let wal_bytes_before = fs::metadata(wal_path(&database))
        .expect("backlogged WAL")
        .len();
    assert!(
        wal_bytes_before
            > u64::try_from(auto_checkpoint_pages)
                .expect("positive auto-checkpoint pages")
                .saturating_mul(u64::try_from(page_size).expect("positive database page size"))
    );
    let database_hash_before =
        crate::text::hash_bytes(&fs::read(&database).expect("database before reopen"));

    let reopened =
        Storage::open_for_repository_scoped(&database, &repository, None).expect("reopen storage");

    assert_eq!(
        crate::text::hash_bytes(&fs::read(&database).expect("database after reopen")),
        database_hash_before,
        "startup schema checks and binding telemetry must not checkpoint existing WAL pages"
    );
    assert!(
        fs::metadata(wal_path(&database))
            .expect("WAL after reopen")
            .len()
            >= wal_bytes_before
    );
    let restored_auto_checkpoint = reopened
        .writer
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .query_row("PRAGMA wal_autocheckpoint", [], |row| row.get::<_, i64>(0))
        .expect("restored auto-checkpoint pages");
    assert_eq!(restored_auto_checkpoint, auto_checkpoint_pages);
    assert_eq!(reader.repository_generation().expect("stable snapshot"), 1);
}

#[test]
pub(crate) fn incremental_reconciliation_recycles_wal_after_long_lived_reader_drops() {
    let root = tempfile::tempdir().expect("root");
    let database = root.path().join("index.sqlite");
    let storage = Storage::open(&database).expect("storage");
    storage
        .full_reconcile("config", vec![sample_file("pinned.rs", "old snapshot\n")])
        .expect("initial generation");

    let reader = storage.begin_read().expect("long-lived reader");
    assert_eq!(reader.repository_generation().expect("pin snapshot"), 1);
    assert!(
        reader
            .find_file("pinned.rs")
            .expect("pinned lookup")
            .is_some()
    );

    let payload = "x".repeat(512 * 1024);
    for round in 0..4 {
        let replacements = (0..8)
            .map(|file| {
                sample_file(
                    &format!("large-{file}.rs"),
                    &format!("round_{round}_file_{file}\n{payload}"),
                )
            })
            .collect();
        storage
            .reconcile_files("config", replacements, &[])
            .expect("large incremental reconciliation");
    }

    assert_eq!(
        reader.repository_generation().expect("stable snapshot"),
        1,
        "the reader must retain its original generation while the WAL grows"
    );
    let retained_wal_bytes = fs::metadata(wal_path(&database))
        .expect("retained WAL")
        .len();
    assert!(
        retained_wal_bytes > WAL_JOURNAL_SIZE_LIMIT_BYTES as u64,
        "a pinned reader should prevent recycling the large WAL: {retained_wal_bytes} bytes"
    );

    let blocked_baseline = storage.meta().expect("blocked checkpoint baseline");
    let (_, (), blocked_checkpoint) = storage
        .publish_reconciliation_profiled_at(
            &blocked_baseline,
            "config",
            IndexingMode::Reconcile,
            |writer| {
                writer.replace(sample_file("while-pinned.rs", "still pinned\n"))?;
                Ok(())
            },
        )
        .expect("profiled reconciliation with pinned reader");
    assert!(blocked_checkpoint.post_commit_diagnostics_complete);
    assert!(blocked_checkpoint.checkpoint_attempted);
    assert_eq!(blocked_checkpoint.checkpoint_busy, 1);
    assert!(blocked_checkpoint.wal_bytes >= retained_wal_bytes);

    drop(reader);
    let checkpoint_baseline = storage.meta().expect("checkpoint baseline");
    let (_, (), completed_checkpoint) = storage
        .publish_reconciliation_profiled_at(
            &checkpoint_baseline,
            "config",
            IndexingMode::Reconcile,
            |writer| {
                writer.replace(sample_file("checkpoint-trigger.rs", "latest\n"))?;
                Ok(())
            },
        )
        .expect("post-reader reconciliation");

    assert!(completed_checkpoint.post_commit_diagnostics_complete);
    assert!(completed_checkpoint.checkpoint_attempted);
    assert_eq!(completed_checkpoint.checkpoint_busy, 0);
    assert_eq!(completed_checkpoint.checkpoint_log_frames, 0);
    assert_eq!(completed_checkpoint.checkpointed_frames, 0);
    assert_eq!(
        completed_checkpoint.wal_bytes, 0,
        "the explicit post-commit checkpoint should truncate the WAL after the reader drops"
    );
}

#[test]
pub(crate) fn startup_rejects_a_generation_with_corrupt_fts_projection() {
    let root = tempfile::tempdir().expect("root");
    let database = root.path().join("index.sqlite");
    let storage = Storage::open(&database).expect("storage");
    storage
        .full_reconcile(
            "config",
            vec![sample_file("needle.rs", "fn repaired_fts_needle() {}\n")],
        )
        .expect("index fixture");
    drop(storage);

    let storage = Storage::open(&database).expect("initial integrity verification");
    {
        let writer = storage
            .writer
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        writer
            .execute(
                "INSERT INTO chunks_fts_word(chunks_fts_word) VALUES('delete-all')",
                [],
            )
            .expect("remove FTS postings while retaining relational content");
    }
    assert!(
        storage
            .search_word("repaired_fts_needle", 10)
            .expect("search damaged index")
            .is_empty()
    );
    drop(storage);

    assert!(Storage::open(&database).is_err());
}

pub(crate) fn sample_file(path: &str, content: &str) -> IndexedFile {
    IndexedFile {
        path: path.to_string(),
        language: Some("rust".into()),
        structurally_complete: true,
        size_bytes: u64::try_from(content.len()).expect("content length"),
        modified_ns: None,
        content_hash: crate::text::hash_bytes(content.as_bytes()),
        chunks: vec![ChunkInput {
            content: content.to_string(),
            start_line: 1,
            end_line: 1,
            start_byte: 0,
            end_byte: content.len(),
            token_count: 1,
        }],
        symbols: Vec::new(),
        references: Vec::new(),
        imports: Vec::new(),
    }
}

#[test]
pub(crate) fn enclosing_symbol_lookup_benchmark_rejects_unproven_nesting_depth() {
    const BASELINE_SQL: &str = r#"
        WITH requested AS (
            SELECT CAST(key AS INTEGER) AS request_index,
                   CAST(value ->> 'file_id' AS INTEGER) AS file_id,
                   CAST(value ->> 'line' AS INTEGER) AS line
            FROM json_each(?1)
        )
        SELECT requested.request_index, symbols.id
        FROM requested
        JOIN symbols ON symbols.id = (
            SELECT enclosing.id
            FROM symbols AS enclosing
            WHERE enclosing.file_id = requested.file_id
              AND enclosing.start_line <= requested.line
              AND enclosing.end_line >= requested.line
            ORDER BY (enclosing.end_line - enclosing.start_line), enclosing.start_byte
            LIMIT 1
        )
        ORDER BY requested.request_index
    "#;
    const PREFILTER_SQL: &str = r#"
        WITH requested AS (
            SELECT CAST(key AS INTEGER) AS request_index,
                   CAST(value ->> 'file_id' AS INTEGER) AS file_id,
                   CAST(value ->> 'line' AS INTEGER) AS line
            FROM json_each(?1)
        ), ranked AS (
            SELECT requested.request_index, symbols.id,
                   ROW_NUMBER() OVER (
                       PARTITION BY requested.request_index
                       ORDER BY (symbols.end_line - symbols.start_line), symbols.start_byte
                   ) AS rank
            FROM requested
            JOIN symbols
              ON symbols.file_id = requested.file_id
             AND symbols.start_line <= requested.line
             AND symbols.end_line >= requested.line
        )
        SELECT request_index, id
        FROM ranked
        WHERE rank = 1
        ORDER BY request_index
    "#;

    let root = tempfile::tempdir().expect("root");
    let storage = Storage::open(root.path().join("index.sqlite")).expect("storage");
    let mut files = Vec::new();
    for file_index in 0..32 {
        let path = format!("src/file_{file_index:02}.rs");
        let content = "x\n".repeat(200);
        let symbols = [
            ("module", 1, 200, 0),
            ("outer", 2, 180, 10),
            ("middle", 10, 150, 20),
            ("inner", 40, 90, 30),
        ]
        .into_iter()
        .map(|(name, start_line, end_line, start_byte)| SymbolInput {
            name: format!("{name}_{file_index}"),
            kind: "function".into(),
            parent: None,
            signature: None,
            start_line,
            end_line,
            start_byte,
            end_byte: start_byte + 10,
        })
        .collect();
        files.push(IndexedFile {
            path,
            language: Some("rust".into()),
            structurally_complete: true,
            size_bytes: content.len() as u64,
            modified_ns: None,
            content_hash: crate::text::hash_bytes(content.as_bytes()),
            chunks: vec![ChunkInput {
                content,
                start_line: 1,
                end_line: 200,
                start_byte: 0,
                end_byte: 400,
                token_count: 200,
            }],
            symbols,
            references: Vec::new(),
            imports: Vec::new(),
        });
    }
    storage
        .full_reconcile("benchmark", files)
        .expect("index fixture");
    let session = storage.begin_read().expect("read session");
    let file_ids = (0..32)
        .map(|file_index| {
            session
                .find_file(&format!("src/file_{file_index:02}.rs"))
                .expect("find file")
                .expect("file id")
                .id
        })
        .collect::<Vec<_>>();
    let locations = file_ids
        .iter()
        .flat_map(|file_id| [1, 2, 10, 40, 90, 151, 201].map(|line| (*file_id, line)))
        .chain(std::iter::once((file_ids[0], 40)))
        .collect::<Vec<_>>();
    let input = serde_json::to_string(
        &locations
            .iter()
            .map(|(file_id, line)| serde_json::json!({ "file_id": file_id, "line": line }))
            .collect::<Vec<_>>(),
    )
    .expect("serialize locations");
    let baseline_expected = session
        .find_enclosing_symbols_batch(&locations)
        .expect("baseline lookup")
        .into_iter()
        .map(|symbol| symbol.map(|symbol| symbol.id))
        .collect::<Vec<_>>();

    let mut candidate_statement = session
        .conn
        .prepare(PREFILTER_SQL)
        .expect("candidate query");
    let mut candidate_lookup = || {
        let mut result = vec![None; locations.len()];
        let rows = candidate_statement
            .query_map(params![&input], |row| {
                Ok((i64_to_usize(row.get(0)?)?, row.get::<_, i64>(1)?))
            })
            .expect("candidate rows");
        for row in rows {
            let (index, id) = row.expect("candidate row");
            result[index] = Some(id);
        }
        result
    };
    assert_eq!(baseline_expected, candidate_lookup());

    let baseline_plan = session
        .conn
        .prepare(&format!("EXPLAIN QUERY PLAN {BASELINE_SQL}"))
        .expect("baseline plan")
        .query_map(params![&input], |row| row.get::<_, String>(3))
        .expect("baseline plan rows")
        .collect::<rusqlite::Result<Vec<_>>>()
        .expect("baseline plan values");
    let candidate_plan = session
        .conn
        .prepare(&format!("EXPLAIN QUERY PLAN {PREFILTER_SQL}"))
        .expect("candidate plan")
        .query_map(params![&input], |row| row.get::<_, String>(3))
        .expect("candidate plan rows")
        .collect::<rusqlite::Result<Vec<_>>>()
        .expect("candidate plan values");
    assert!(!baseline_plan.is_empty());
    assert!(!candidate_plan.is_empty());

    const ITERATIONS: usize = 100;
    let baseline_start = Instant::now();
    for _ in 0..ITERATIONS {
        let actual = session
            .find_enclosing_symbols_batch(&locations)
            .expect("baseline benchmark lookup");
        assert_eq!(
            actual
                .into_iter()
                .map(|symbol| symbol.map(|symbol| symbol.id))
                .collect::<Vec<_>>(),
            baseline_expected
        );
    }
    let baseline_micros = baseline_start.elapsed().as_micros();
    let candidate_start = Instant::now();
    for _ in 0..ITERATIONS {
        assert_eq!(candidate_lookup(), baseline_expected);
    }
    let candidate_micros = candidate_start.elapsed().as_micros();
    eprintln!(
        "enclosing lookup benchmark: locations={} baseline_us={} prefilter_us={} baseline_plan={baseline_plan:?} prefilter_plan={candidate_plan:?}",
        locations.len(),
        baseline_micros,
        candidate_micros
    );
    // The prefilter shape is a correctness/planning comparison only. A schema
    // column such as nesting_depth is not justified until it wins end-to-end
    // on representative repositories, not just this synthetic micro-phase.
}

#[test]
pub(crate) fn file_end_line_batch_maps_duplicate_and_missing_file_ids() {
    let directory = tempfile::tempdir().expect("directory");
    let storage = Storage::open(directory.path().join("index.sqlite")).expect("storage");
    storage
        .full_reconcile("config", vec![sample_file("source.rs", "fn source() {}\n")])
        .expect("index source");
    let session = storage.begin_read().expect("read session");
    let file_id = session
        .find_file("source.rs")
        .expect("find source")
        .expect("indexed source")
        .id;

    assert_eq!(
        session
            .file_end_lines_batch(&[file_id, file_id, i64::MAX, file_id])
            .expect("end lines"),
        vec![Some(1), Some(1), None, Some(1)]
    );
}

#[test]
pub(crate) fn streamed_cancellation_rolls_back_every_insert_and_generation() {
    let directory = tempfile::tempdir().expect("directory");
    let database = directory.path().join("index.sqlite");
    let storage = Storage::open(&database).expect("storage");
    storage
        .full_reconcile("config", vec![sample_file("old.rs", "fn old() {}\n")])
        .expect("initial generation");
    let baseline = storage.meta().expect("baseline");

    let error = storage
        .publish_reconciliation_at(
            &baseline,
            "config",
            IndexingMode::Rebuild,
            |writer| -> Result<()> {
                writer.replace(sample_file("first.rs", "fn first() {}\n"))?;
                Err(Error::Cancelled)
            },
        )
        .expect_err("later batch failure");

    assert!(matches!(error, Error::Cancelled));
    drop(storage);
    let reopened = Storage::open(&database).expect("reopen after rollback");
    assert_eq!(reopened.repository_generation().expect("generation"), 1);
    assert!(reopened.find_file("old.rs").expect("old lookup").is_some());
    assert!(
        reopened
            .find_file("first.rs")
            .expect("first lookup")
            .is_none()
    );
}

#[test]
pub(crate) fn exhausted_repository_generation_fails_before_publication() {
    let directory = tempfile::tempdir().expect("directory");
    let storage = Storage::open(directory.path().join("index.sqlite")).expect("storage");
    storage
        .full_reconcile("config", vec![sample_file("old.rs", "fn old() {}\n")])
        .expect("initial generation");
    {
        let connection = storage
            .writer
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        connection
            .execute(
                "UPDATE meta SET repository_generation = ?1 WHERE id = 1",
                [i64::MAX],
            )
            .expect("set exhausted generation");
    }
    let baseline = storage.meta().expect("exhausted baseline");

    let error = storage
        .publish_reconciliation_at(&baseline, "config", IndexingMode::Reconcile, |writer| {
            writer.replace(sample_file("new.rs", "fn new() {}\n"))
        })
        .expect_err("generation exhaustion must fail");

    assert!(matches!(
        error,
        Error::OperationFailure(message) if message == "repository generation exhausted"
    ));
    assert!(storage.find_file("old.rs").expect("old lookup").is_some());
    assert!(storage.find_file("new.rs").expect("new lookup").is_none());
}

#[test]
pub(crate) fn relocation_failure_rolls_back_path_and_preserves_content_rows() {
    let directory = tempfile::tempdir().expect("directory");
    let storage = Storage::open(directory.path().join("index.sqlite")).expect("storage");
    storage
        .full_reconcile("config", vec![sample_file("old.rs", "fn old() {}\n")])
        .expect("initial generation");
    let old = storage
        .find_file("old.rs")
        .expect("old lookup")
        .expect("old file");
    let chunk_id = storage.get_chunks_for_file(old.id, 10).expect("old chunks")[0].id;
    let baseline = storage.meta().expect("baseline");

    let error = storage
        .publish_reconciliation_at(
            &baseline,
            "config",
            IndexingMode::Reconcile,
            |writer| -> Result<()> {
                writer.relocate(
                    "old.rs",
                    "new.rs",
                    old.size_bytes,
                    old.modified_ns,
                    &old.content_hash,
                )?;
                Err(Error::Cancelled)
            },
        )
        .expect_err("injected failure");

    assert!(matches!(error, Error::Cancelled));
    assert!(storage.find_file("new.rs").expect("new lookup").is_none());
    let restored = storage
        .find_file("old.rs")
        .expect("restored lookup")
        .expect("restored file");
    assert_eq!(restored.id, old.id);
    assert_eq!(
        storage
            .get_chunks_for_file(restored.id, 10)
            .expect("restored chunks")[0]
            .id,
        chunk_id
    );
}

#[test]
pub(crate) fn later_streamed_storage_failure_rolls_back_earlier_files() {
    let directory = tempfile::tempdir().expect("directory");
    let database = directory.path().join("index.sqlite");
    let storage = Storage::open(&database).expect("storage");
    storage
        .full_reconcile("config", vec![sample_file("old.rs", "fn old() {}\n")])
        .expect("initial generation");
    let baseline = storage.meta().expect("baseline");
    let mut invalid = sample_file("invalid.rs", "fn invalid() {}\n");
    invalid.chunks[0].end_line = usize::MAX;

    storage
        .publish_reconciliation_at(&baseline, "config", IndexingMode::Rebuild, |writer| {
            writer.replace(sample_file("first.rs", "fn first() {}\n"))?;
            writer.replace(invalid)
        })
        .expect_err("second insert must fail");

    drop(storage);
    let reopened = Storage::open(&database).expect("reopen after rollback");
    assert_eq!(reopened.repository_generation().expect("generation"), 1);
    assert!(reopened.find_file("old.rs").expect("old lookup").is_some());
    assert!(
        reopened
            .find_file("first.rs")
            .expect("first lookup")
            .is_none()
    );
    assert!(
        reopened
            .find_file("invalid.rs")
            .expect("invalid lookup")
            .is_none()
    );
}

#[test]
pub(crate) fn streamed_panic_rolls_back_and_leaves_storage_reusable() {
    let directory = tempfile::tempdir().expect("directory");
    let storage = Storage::open(directory.path().join("index.sqlite")).expect("storage");
    storage
        .full_reconcile("config", vec![sample_file("old.rs", "fn old() {}\n")])
        .expect("initial generation");
    let baseline = storage.meta().expect("baseline");

    let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _ = storage.publish_reconciliation_at(
            &baseline,
            "config",
            IndexingMode::Rebuild,
            |writer| -> Result<()> {
                writer.replace(sample_file("new.rs", "fn new() {}\n"))?;
                panic!("injected batch panic");
            },
        );
    }));

    assert!(panic.is_err());
    assert_eq!(storage.repository_generation().expect("generation"), 1);
    assert!(storage.find_file("old.rs").expect("old lookup").is_some());
    assert!(storage.find_file("new.rs").expect("new lookup").is_none());
    assert_eq!(
        storage
            .reconcile_files(
                "config",
                vec![sample_file("after.rs", "fn after() {}\n")],
                &[],
            )
            .expect("writer remains usable"),
        2
    );
    assert_eq!(
        storage.search_word("after", 10).expect("word search")[0].path,
        "after.rs"
    );
    assert_eq!(
        storage.search_trigram("after", 10).expect("trigram search")[0].path,
        "after.rs"
    );
}

#[test]
pub(crate) fn bulk_rebuild_refreshes_both_chunk_search_indexes() {
    let directory = tempfile::tempdir().expect("directory");
    let storage = Storage::open(directory.path().join("index.sqlite")).expect("storage");
    storage
        .full_reconcile(
            "config",
            vec![sample_file("old.rs", "fn obsolete_marker() {}\n")],
        )
        .expect("initial generation");
    let baseline = storage.meta().expect("baseline");

    storage
        .publish_reconciliation_at(&baseline, "config", IndexingMode::Rebuild, |writer| {
            writer.replace(sample_file("new.rs", "fn replacement_marker() {}\n"))
        })
        .expect("replacement generation");

    assert!(
        storage
            .search_word("obsolete_marker", 10)
            .expect("old word search")
            .is_empty()
    );
    assert!(
        storage
            .search_trigram("obsolete_marker", 10)
            .expect("old trigram search")
            .is_empty()
    );
    assert_eq!(
        storage
            .search_word("replacement_marker", 10)
            .expect("new word search")[0]
            .path,
        "new.rs"
    );
    assert_eq!(
        storage
            .search_trigram("replacement_marker", 10)
            .expect("new trigram search")[0]
            .path,
        "new.rs"
    );
}

#[test]
pub(crate) fn readers_see_old_generation_until_streamed_publication_commits() {
    let directory = tempfile::tempdir().expect("directory");
    let storage = Storage::open(directory.path().join("index.sqlite")).expect("storage");
    storage
        .full_reconcile("config", vec![sample_file("old.rs", "fn old() {}\n")])
        .expect("initial generation");
    let baseline = storage.meta().expect("baseline");

    let (generation, ()) = storage
        .publish_reconciliation_at(&baseline, "config", IndexingMode::Rebuild, |writer| {
            writer.replace(sample_file("new.rs", "fn new() {}\n"))?;
            let reader = storage.begin_read()?;
            assert_eq!(reader.repository_generation()?, 1);
            assert!(reader.find_file("old.rs")?.is_some());
            assert!(reader.find_file("new.rs")?.is_none());
            Ok(())
        })
        .expect("publish");

    assert_eq!(generation, 2);
    assert!(storage.find_file("old.rs").expect("old lookup").is_none());
    assert!(storage.find_file("new.rs").expect("new lookup").is_some());
}

#[test]
pub(crate) fn stale_streaming_baseline_fails_before_invoking_the_writer() {
    let directory = tempfile::tempdir().expect("directory");
    let storage = Storage::open(directory.path().join("index.sqlite")).expect("storage");
    let stale = storage.meta().expect("stale baseline");
    storage
        .full_reconcile(
            "config",
            vec![sample_file("current.rs", "fn current() {}\n")],
        )
        .expect("current generation");
    let mut invoked = false;

    let error = storage
        .publish_reconciliation_at(&stale, "config", IndexingMode::Reconcile, |_| {
            invoked = true;
            Ok(())
        })
        .expect_err("stale publication");

    assert!(matches!(error, Error::StaleReconciliation { .. }));
    assert!(!invoked);
}

#[test]
pub(crate) fn repository_binding_updates_last_access_once_per_open() {
    let directory = tempfile::tempdir().expect("directory");
    let repository = directory.path().join("repository");
    fs::create_dir(&repository).expect("repository");
    let database = directory.path().join("index.sqlite");
    let storage = Storage::open(&database).expect("storage");

    storage
        .bind_repository_at(&repository, None, 1_234)
        .expect("initial binding");
    let connection = Connection::open(&database).expect("inspect binding");
    assert_eq!(
        connection
            .query_row(
                "SELECT last_access_unix_seconds FROM meta WHERE id = 1",
                [],
                |row| row.get::<_, i64>(0)
            )
            .expect("first access"),
        1_234
    );

    storage
        .bind_repository_at(&repository, None, 5_678)
        .expect("repeat binding");
    assert_eq!(
        connection
            .query_row(
                "SELECT last_access_unix_seconds FROM meta WHERE id = 1",
                [],
                |row| row.get::<_, i64>(0)
            )
            .expect("second access"),
        5_678
    );
}
