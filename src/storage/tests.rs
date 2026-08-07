use super::*;

mod query_receipts;
mod read_delta;
mod receipts;

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
pub(crate) fn parser_coverage_rows_remain_pinned_across_publication() {
    let root = tempfile::tempdir().expect("root");
    let storage = Storage::open(root.path().join("index.sqlite")).expect("storage");
    storage
        .full_reconcile("config", vec![sample_file("alpha.rs", "fn alpha() {}\n")])
        .expect("initial publication");
    let pinned = storage.begin_read().expect("pinned read");
    assert_eq!(
        pinned.repository_generation().expect("pinned generation"),
        1
    );
    let initial = pinned
        .parser_coverage_rows(|_| "fixture".to_owned())
        .expect("initial parser coverage");
    assert_eq!(
        initial.languages.iter().map(|row| row.files).sum::<usize>(),
        1
    );

    storage
        .full_reconcile(
            "config",
            vec![
                sample_file("alpha.rs", "fn alpha() {}\n"),
                sample_file("bravo.rs", "fn bravo() {}\n"),
            ],
        )
        .expect("second publication");

    let still_pinned = pinned
        .parser_coverage_rows(|_| "fixture".to_owned())
        .expect("pinned parser coverage after publication");
    assert_eq!(
        still_pinned
            .languages
            .iter()
            .map(|row| row.files)
            .sum::<usize>(),
        1
    );
    let current = storage
        .begin_read()
        .expect("current read")
        .parser_coverage_rows(|_| "fixture".to_owned())
        .expect("current parser coverage");
    assert_eq!(
        current.languages.iter().map(|row| row.files).sum::<usize>(),
        2
    );
}

#[test]
pub(crate) fn cold_publication_reports_ordered_bounded_phases() {
    let root = tempfile::tempdir().expect("root");
    let database = root.path().join("index.sqlite");
    let storage = Storage::open(&database).expect("storage");
    let baseline = storage.meta().expect("baseline");
    let mut phases = Vec::new();

    let (generation, ()) = storage
        .publish_reconciliation_at_with_progress(
            &baseline,
            "config",
            false,
            |phase| {
                phases.push(phase);
                Ok(())
            },
            |writer| {
                writer.replace(sample_file("lib.rs", "fn answer() -> u8 { 42 }\n"))?;
                let unpublished =
                    Storage::read_only_status(&database, root.path()).expect("concurrent status");
                assert_eq!(unpublished.generation, 0);
                assert_eq!(unpublished.counts.files, 0);
                Ok(())
            },
        )
        .expect("cold publication");

    assert_eq!(generation, 1);
    let published = Storage::read_only_status(&database, root.path()).expect("published status");
    assert_eq!(published.generation, 1);
    assert_eq!(published.counts.files, 1);
    assert_eq!(
        phases,
        [
            ReconciliationPublicationPhase::ChunkWordFts,
            ReconciliationPublicationPhase::ChunkTrigramFts,
            ReconciliationPublicationPhase::SymbolFts,
            ReconciliationPublicationPhase::ReferenceFts,
            ReconciliationPublicationPhase::CommitAndCheckpoint,
        ]
    );
}

#[test]
pub(crate) fn publication_phase_cancellation_rolls_back_and_rebuilds_from_the_same_cache() {
    for target in [
        ReconciliationPublicationPhase::ChunkWordFts,
        ReconciliationPublicationPhase::ChunkTrigramFts,
        ReconciliationPublicationPhase::SymbolFts,
        ReconciliationPublicationPhase::ReferenceFts,
        ReconciliationPublicationPhase::CommitAndCheckpoint,
    ] {
        let root = tempfile::tempdir().expect("root");
        let database = root.path().join("index.sqlite");
        let storage = Storage::open(&database).expect("storage");
        let baseline = storage.meta().expect("baseline");
        let error = storage
            .publish_reconciliation_at_with_progress(
                &baseline,
                "config",
                false,
                |phase| {
                    if phase == target {
                        Err(Error::Cancelled)
                    } else {
                        Ok(())
                    }
                },
                |writer| {
                    writer.replace(sample_file("lib.rs", "fn answer() -> u8 { 42 }\n"))?;
                    Ok(())
                },
            )
            .expect_err("cancellation before commit must roll back");
        assert!(
            matches!(error, Error::Cancelled),
            "target phase: {target:?}"
        );
        drop(storage);

        let reopened = Storage::open(&database).expect("reopen cancelled cache");
        let cancelled =
            Storage::read_only_status(&database, root.path()).expect("cancelled status");
        assert_eq!(cancelled.generation, 0, "target phase: {target:?}");
        assert_eq!(cancelled.counts.files, 0, "target phase: {target:?}");

        let generation = reopened
            .full_reconcile(
                "config",
                vec![sample_file("lib.rs", "fn answer() -> u8 { 42 }\n")],
            )
            .expect("rebuild cancelled cache");
        assert_eq!(generation, 1, "target phase: {target:?}");
        assert_eq!(
            reopened
                .search_word("answer", 10)
                .expect("search rebuilt cache")
                .len(),
            1,
            "target phase: {target:?}"
        );
    }
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
        .publish_reconciliation_profiled_at(&blocked_baseline, "config", false, |writer| {
            writer.replace(sample_file("while-pinned.rs", "still pinned\n"))?;
            Ok(())
        })
        .expect("profiled reconciliation with pinned reader");
    assert!(blocked_checkpoint.post_commit_diagnostics_complete);
    assert_eq!(blocked_checkpoint.checkpoint_busy, 1);
    assert!(blocked_checkpoint.wal_bytes >= retained_wal_bytes);

    drop(reader);
    let checkpoint_baseline = storage.meta().expect("checkpoint baseline");
    let (_, (), completed_checkpoint) = storage
        .publish_reconciliation_profiled_at(&checkpoint_baseline, "config", false, |writer| {
            writer.replace(sample_file("checkpoint-trigger.rs", "latest\n"))?;
            Ok(())
        })
        .expect("post-reader reconciliation");

    assert!(completed_checkpoint.post_commit_diagnostics_complete);
    assert_eq!(completed_checkpoint.checkpoint_busy, 0);
    assert_eq!(completed_checkpoint.checkpoint_log_frames, 0);
    assert_eq!(completed_checkpoint.checkpointed_frames, 0);
    assert_eq!(
        completed_checkpoint.wal_bytes, 0,
        "the explicit post-commit checkpoint should truncate the WAL after the reader drops"
    );
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
        .publish_reconciliation_at(&baseline, "config", true, |writer| -> Result<()> {
            writer.replace(sample_file("first.rs", "fn first() {}\n"))?;
            Err(Error::Cancelled)
        })
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
        .publish_reconciliation_at(&baseline, "config", false, |writer| -> Result<()> {
            writer.relocate(
                "old.rs",
                "new.rs",
                old.size_bytes,
                old.modified_ns,
                &old.content_hash,
            )?;
            Err(Error::Cancelled)
        })
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
        .publish_reconciliation_at(&baseline, "config", true, |writer| {
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
        let _ =
            storage.publish_reconciliation_at(&baseline, "config", true, |writer| -> Result<()> {
                writer.replace(sample_file("new.rs", "fn new() {}\n"))?;
                panic!("injected batch panic");
            });
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
        .publish_reconciliation_at(&baseline, "config", true, |writer| {
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
        .publish_reconciliation_at(&baseline, "config", true, |writer| {
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
        .publish_reconciliation_at(&stale, "config", false, |_| {
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

#[test]
pub(crate) fn token_savings_accounting_skips_a_busy_local_writer() {
    let directory = tempfile::tempdir().expect("directory");
    let storage = Storage::open(directory.path().join("index.sqlite")).expect("storage");
    let meta = ResponseMeta {
        repository_id: "repository".into(),
        repository_generation: 1,
        freshness: crate::model::Freshness::Current,
        index_scope: crate::model::IndexScopeMode::Full,
        index_scope_digest: None,
        source_tokens: 2,
        protocol_tokens: 3,
        path_and_metadata_tokens: 5,
        total_response_tokens: 10,
        tokenizer: "cl100k_base".into(),
        token_count_exact: true,
        receipt_id: None,
        receipt_suppressed_exact: 0,
        receipt_suppressed_overlap: 0,
        receipt_near_duplicates: 0,
        next_cursor: None,
    };
    let writer = storage
        .writer
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());

    assert!(
        !storage
            .record_token_savings(
                "cl100k_base",
                TokenSavingsObservation {
                    operation: TokenAccountingOperation::Search,
                    baseline_source_tokens: Some(10),
                    meta: &meta,
                    classification: TokenSavingsRequestClass::Useful,
                    expected_hash_not_modified: false,
                    expected_hash_suppressed_source_tokens: 0,
                },
            )
            .expect("best-effort accounting")
    );
    assert!(
        !storage
            .record_service_failure(
                "cl100k_base",
                TokenAccountingOperation::Search,
                "invalid_input",
            )
            .expect("best-effort failure accounting")
    );
    drop(writer);
    assert!(
        storage
            .record_token_savings(
                "cl100k_base",
                TokenSavingsObservation {
                    operation: TokenAccountingOperation::Search,
                    baseline_source_tokens: Some(10),
                    meta: &meta,
                    classification: TokenSavingsRequestClass::HashSuppressed,
                    expected_hash_not_modified: true,
                    expected_hash_suppressed_source_tokens: 8,
                },
            )
            .expect("available accounting")
    );
    assert!(
        storage
            .record_service_failure(
                "cl100k_base",
                TokenAccountingOperation::Search,
                "invalid_input",
            )
            .expect("available failure accounting")
    );
    let records = storage
        .token_savings("cl100k_base")
        .expect("stored accounting");
    let record = records.get("search").expect("search accounting");
    assert_eq!(record.tracked_requests, 0);
    assert_eq!(record.response_tracked_requests, 1);
    assert_eq!(record.response_baseline_requests, 1);
    assert_eq!(record.baseline_source_tokens, 0);
    assert_eq!(record.response_baseline_source_tokens, 10);
    assert_eq!(record.emitted_source_tokens, 0);
    assert_eq!(record.response_source_tokens, 2);
    assert_eq!(record.path_and_metadata_tokens, 5);
    assert_eq!(record.protocol_tokens, 3);
    assert_eq!(record.total_response_tokens, 10);
    assert_eq!(record.expected_hash_not_modified_responses, 1);
    assert_eq!(record.expected_hash_suppressed_source_tokens, 8);
    assert_eq!(record.hash_suppressed_requests, 1);
    let failures = storage
        .begin_read()
        .expect("failure read session")
        .service_failures("cl100k_base")
        .expect("stored failure accounting");
    assert_eq!(
        failures,
        vec![ServiceFailureRecord {
            operation: "search".into(),
            error_category: "invalid_input".into(),
            failed_requests: 1,
        }]
    );
}

#[test]
pub(crate) fn whole_file_source_tokens_uses_the_exact_indexed_file_count() {
    let directory = tempfile::tempdir().expect("directory");
    let storage = Storage::open(directory.path().join("index.sqlite")).expect("storage");
    let mut file = sample_file("source.rs", "hello\n\n");
    file.chunks = vec![
        ChunkInput {
            content: "hello\n".into(),
            start_line: 1,
            end_line: 1,
            start_byte: 0,
            end_byte: 6,
            token_count: 2,
        },
        ChunkInput {
            content: "\n".into(),
            start_line: 2,
            end_line: 2,
            start_byte: 6,
            end_byte: 7,
            token_count: 1,
        },
    ];
    let baseline = storage.meta().expect("baseline");
    storage
        .publish_reconciliation_at(&baseline, "config", false, |writer| {
            writer.replace_with_source_tokens(file, "cl100k_base", 2)
        })
        .expect("indexed file");

    assert_eq!(
        storage
            .begin_read()
            .expect("read session")
            .whole_file_source_tokens(&["source.rs".into()], "cl100k_base")
            .expect("whole-file tokens"),
        Some(2)
    );
    assert_eq!(
        storage
            .begin_read()
            .expect("read session")
            .whole_file_source_tokens(&["source.rs".into()], "o200k_base")
            .expect("mismatched tokenizer"),
        None
    );
}

#[test]
pub(crate) fn list_glob_paths_pages_selective_matches_with_keyset_cursor() {
    let directory = tempfile::tempdir().expect("directory");
    let storage = Storage::open(directory.path().join("index.sqlite")).expect("storage");
    let mut files = (0..80)
        .map(|i| {
            sample_file(
                &format!("src/other{i}.rs"),
                &format!("fn other{i}() {{}}\n"),
            )
        })
        .collect::<Vec<_>>();
    files.push(sample_file("src/target_alpha.rs", "fn target_alpha() {}\n"));
    files.push(sample_file("src/target_bravo.rs", "fn target_bravo() {}\n"));
    files.push(sample_file(
        "src/target_charlie.rs",
        "fn target_charlie() {}\n",
    ));
    storage.full_reconcile("config", files).expect("reconcile");

    let session = storage.begin_read().expect("session");
    let first = session
        .list_glob_paths("src/target_*.rs", None, None, 2)
        .expect("first glob page");
    assert_eq!(first.len(), 2);
    assert!(
        first.iter().all(|entry| entry.path.contains("target_")),
        "selective glob must not return non-matching paths: {first:?}"
    );
    let second = session
        .list_glob_paths(
            "src/target_*.rs",
            None,
            first.last().map(|entry| entry.path.as_str()),
            2,
        )
        .expect("second glob page");
    assert_eq!(second.len(), 1);
    assert_eq!(second[0].path, "src/target_charlie.rs");

    let lean = session.list_file_paths(10, None).expect("lean paths");
    assert_eq!(lean.len(), 10);
    assert_eq!(
        lean[0].path,
        session.list_files(1, None).expect("full")[0].path
    );
}
