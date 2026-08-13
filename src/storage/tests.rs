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
pub(crate) fn import_projection_repair_resolves_membership_through_the_path_index() {
    let root = tempfile::tempdir().expect("root");
    let storage = Storage::open(root.path().join("index.sqlite")).expect("storage");
    let connection = storage
        .writer
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let details = connection
        .prepare(&format!(
            "EXPLAIN QUERY PLAN {IMPORT_CANDIDATE_RESOLUTION_SQL}"
        ))
        .expect("resolution query plan")
        .query_map(params!["[\"src/lib.rs\"]"], |row| row.get::<_, String>(3))
        .expect("resolution plan rows")
        .collect::<rusqlite::Result<Vec<_>>>()
        .expect("resolution plan details");

    assert!(
        details
            .iter()
            .any(|detail| detail.contains("SEARCH files USING COVERING INDEX")),
        "candidate membership must use the unique files.path index: {details:?}"
    );
    assert!(
        details.iter().all(|detail| !detail.contains("SCAN files")),
        "candidate membership must not scan the repository table: {details:?}"
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
            IndexingMode::Reconcile,
            |phase| {
                phases.push(phase);
                Ok(())
            },
            |writer| {
                writer.replace(sample_file("lib.rs", "fn answer() -> u8 { 42 }\n"))?;
                let unpublished = Storage::read_only_status_scoped(&database, root.path(), None)
                    .expect("concurrent status");
                assert_eq!(unpublished.generation, 0);
                assert_eq!(unpublished.counts.files, 0);
                Ok(())
            },
        )
        .expect("cold publication");

    assert_eq!(generation, 1);
    let published =
        Storage::read_only_status_scoped(&database, root.path(), None).expect("published status");
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
                IndexingMode::Reconcile,
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
        let cancelled = Storage::read_only_status_scoped(&database, root.path(), None)
            .expect("cancelled status");
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
pub(crate) fn checkpoint_policy_is_restored_when_suspended_operation_fails() {
    let root = tempfile::tempdir().expect("root");
    let database = root.path().join("index.sqlite");
    let storage = Storage::open(&database).expect("storage");
    let mut writer = storage
        .writer
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    writer
        .pragma_update(None, "wal_autocheckpoint", 37)
        .expect("set distinctive checkpoint policy");

    let error =
        with_auto_checkpoint_suspended(&mut writer, AutoCheckpointCompletion::RestoreOnly, |_| {
            Err::<(), _>(Error::OperationFailure("expected failure".into()))
        })
        .expect_err("operation must fail");

    assert!(matches!(error, Error::OperationFailure(_)));
    assert_eq!(
        writer
            .query_row("PRAGMA wal_autocheckpoint", [], |row| row.get::<_, i64>(0))
            .expect("restored checkpoint policy"),
        37
    );
}

#[test]
pub(crate) fn startup_repairs_equal_count_path_projection_mismatch() {
    let root = tempfile::tempdir().expect("root");
    let database = root.path().join("index.sqlite");
    let storage = Storage::open(&database).expect("storage");
    storage
        .full_reconcile("config", vec![sample_file("src/real.rs", "fn real() {}\n")])
        .expect("index fixture");
    {
        let writer = storage
            .writer
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        writer
            .execute(
                "UPDATE path_entries SET path = 'src/phantom.rs' WHERE kind = 1",
                [],
            )
            .expect("replace real path with equal-count phantom");
    }
    drop(storage);

    let reopened = Storage::open(&database).expect("repair projection on reopen");
    let session = reopened.begin_read().expect("read repaired projection");
    let tree = session
        .list_tree_paths("src", 4, None, 10)
        .expect("tree projection");
    let glob = session
        .list_glob_paths("src/*.rs", None, None, 10)
        .expect("glob projection");

    assert!(tree.iter().any(|entry| entry.path == "src/real.rs"));
    assert!(!tree.iter().any(|entry| entry.path == "src/phantom.rs"));
    assert_eq!(glob.len(), 1);
    assert_eq!(glob[0].path, "src/real.rs");
    assert!(
        session
            .find_file("src/real.rs")
            .expect("direct lookup")
            .is_some()
    );
    assert!(
        session
            .find_file("src/phantom.rs")
            .expect("phantom lookup")
            .is_none()
    );
}

#[test]
pub(crate) fn path_projection_integrity_covers_all_relational_fields() {
    let root = tempfile::tempdir().expect("root");
    let database = root.path().join("index.sqlite");
    let storage = Storage::open(&database).expect("storage");
    storage
        .full_reconcile(
            "config",
            vec![
                sample_file("src/one.rs", "fn one() {}\n"),
                sample_file("src/nested/two.rs", "fn two() {}\n"),
            ],
        )
        .expect("index fixture");
    let mut writer = storage
        .writer
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());

    for corruption in [
        "UPDATE path_entries SET depth = depth + 1 WHERE path = 'src/one.rs'",
        "UPDATE path_entries SET kind = 0 WHERE path = 'src/one.rs'",
        "DELETE FROM path_entries WHERE path = 'src/nested'; INSERT INTO path_entries(path, depth, kind, file_id) VALUES ('phantom', 1, 0, NULL)",
        "DELETE FROM path_entries WHERE path IN ('src/one.rs', 'src/nested/two.rs'); INSERT INTO path_entries(path, depth, kind, file_id) SELECT 'src/one.rs', 2, 1, id FROM files WHERE path = 'src/nested/two.rs'; INSERT INTO path_entries(path, depth, kind, file_id) SELECT 'src/nested/two.rs', 3, 1, id FROM files WHERE path = 'src/one.rs'",
    ] {
        writer
            .execute_batch(corruption)
            .expect("damage one projection field");
        assert!(!path_projection_is_current(&writer).expect("detect corruption"));
        Storage::ensure_path_projection(&mut writer).expect("repair projection");
        assert!(path_projection_is_current(&writer).expect("validate repair"));
    }
}

#[test]
pub(crate) fn startup_repairs_all_persisted_quota_usage_projections() {
    let directory = tempfile::tempdir().expect("directory");
    let database = directory.path().join("index.sqlite");
    let storage = Storage::open(&database).expect("storage");
    storage
        .evaluate_receipt_at(
            None,
            1,
            &[crate::receipt::ReceiptEvidence::new(
                "lib.rs",
                1,
                1,
                "receipt-hash",
                Some("fn receipt() {}"),
            )],
            true,
            1_000,
        )
        .expect("retrieval receipt");
    {
        let connection = storage
            .writer
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        connection
            .execute_batch(
                "INSERT INTO query_coverage_receipts(
                     repository_identity, repository_generation, config_hash,
                     semantics_version, predicate_json, predicate_blake3,
                     partition_blake3, partition_file_count, match_count,
                     result_blake3, created_unix_millis, last_access_unix_millis,
                     expires_unix_millis, access_sequence, logical_bytes
                 ) VALUES (
                     (SELECT repository_identity FROM meta WHERE id = 1),
                     0, 'config', 1, '{}', lower(hex(zeroblob(32))),
                     lower(hex(zeroblob(32))), 0, 0,
                     lower(hex(zeroblob(32))), 1000, 1000, 2000, 7, 123
                 );
                 INSERT INTO read_delta_bases(
                     target_key, content_hash, repository_generation,
                     target_start_line, target_end_line,
                     returned_start_line, returned_end_line, content,
                     created_unix_millis, last_access_unix_millis,
                     expires_unix_millis, access_sequence, logical_bytes
                 ) VALUES (
                     'target', 'hash', 1, 1, 1, 1, 1, 'content',
                     1000, 1000, 2000, 9, 77
                 );
                 UPDATE retrieval_receipts
                 SET evidence_count = 0, evidence_bytes = 0;
                 UPDATE retrieval_receipt_usage
                 SET next_access_sequence = 0,
                     receipt_count = 128,
                     receipt_bytes = 0,
                     evidence_count = 0,
                     evidence_bytes = 999;
                 UPDATE query_coverage_receipt_usage
                 SET next_access_sequence = 0,
                     receipt_count = 0,
                     logical_bytes = 0;
                 UPDATE read_delta_base_usage
                 SET next_access_sequence = 0,
                     base_count = 999,
                     base_bytes = 0;",
            )
            .expect("corrupt quota projections");
    }
    drop(storage);

    let repaired = Storage::open(&database).expect("repair projections on reopen");
    let connection = repaired
        .writer
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let retrieval: (i64, i64, i64, i64, i64) = connection
        .query_row(
            "SELECT next_access_sequence, receipt_count, receipt_bytes,
                    evidence_count, evidence_bytes
             FROM retrieval_receipt_usage WHERE id = 1",
            [],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            },
        )
        .expect("retrieval usage");
    let authoritative_retrieval: (i64, i64, i64, i64) = connection
        .query_row(
            "SELECT (SELECT count(*) FROM retrieval_receipts),
                    (SELECT coalesce(sum(logical_bytes), 0) FROM retrieval_receipts),
                    (SELECT count(*) FROM retrieval_receipt_evidence),
                    (SELECT coalesce(sum(logical_bytes), 0)
                     FROM retrieval_receipt_evidence)",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .expect("authoritative retrieval totals");
    assert_eq!(
        (retrieval.1, retrieval.2, retrieval.3, retrieval.4),
        authoritative_retrieval
    );
    assert!(retrieval.0 >= 1);
    assert_eq!(
        connection
            .query_row(
                "SELECT evidence_count, evidence_bytes FROM retrieval_receipts",
                [],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
            )
            .expect("receipt header counters"),
        (authoritative_retrieval.2, authoritative_retrieval.3)
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT next_access_sequence, receipt_count, logical_bytes
                 FROM query_coverage_receipt_usage WHERE id = 1",
                [],
                |row| Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?
                )),
            )
            .expect("query receipt usage"),
        (7, 1, 123)
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT next_access_sequence, base_count, base_bytes
                 FROM read_delta_base_usage WHERE id = 1",
                [],
                |row| Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?
                )),
            )
            .expect("read delta usage"),
        (9, 1, 77)
    );
    let old_retrieval_namespace: String = connection
        .query_row(
            "SELECT namespace FROM retrieval_receipt_usage WHERE id = 1",
            [],
            |row| row.get(0),
        )
        .expect("retrieval namespace");
    let old_query_namespace: String = connection
        .query_row(
            "SELECT namespace FROM query_coverage_receipt_usage WHERE id = 1",
            [],
            |row| row.get(0),
        )
        .expect("query namespace");
    connection
        .execute_batch(
            "DELETE FROM retrieval_receipt_usage;
             DELETE FROM query_coverage_receipt_usage;
             DELETE FROM read_delta_base_usage;",
        )
        .expect("remove singleton projections");
    drop(connection);
    drop(repaired);

    let restored = Storage::open(&database).expect("restore singleton projections");
    let connection = restored
        .writer
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    assert_eq!(
        connection
            .query_row("SELECT count(*) FROM retrieval_receipts", [], |row| {
                row.get::<_, i64>(0)
            })
            .expect("retrieval rows after namespace loss"),
        0
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT receipt_count FROM retrieval_receipt_usage WHERE id = 1",
                [],
                |row| row.get::<_, i64>(0),
            )
            .expect("restored retrieval usage"),
        0
    );
    assert_ne!(
        connection
            .query_row(
                "SELECT namespace FROM retrieval_receipt_usage WHERE id = 1",
                [],
                |row| row.get::<_, String>(0),
            )
            .expect("new retrieval namespace"),
        old_retrieval_namespace
    );
    assert_eq!(
        connection
            .query_row("SELECT count(*) FROM query_coverage_receipts", [], |row| {
                row.get::<_, i64>(0)
            })
            .expect("query rows after namespace loss"),
        0
    );
    assert_ne!(
        connection
            .query_row(
                "SELECT namespace FROM query_coverage_receipt_usage WHERE id = 1",
                [],
                |row| row.get::<_, String>(0),
            )
            .expect("new query namespace"),
        old_query_namespace
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT next_access_sequence, base_count, base_bytes
                 FROM read_delta_base_usage WHERE id = 1",
                [],
                |row| Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?
                )),
            )
            .expect("restored read delta usage"),
        (9, 1, 77)
    );
}

#[test]
pub(crate) fn startup_path_repairs_checkpoint_backlog_without_writing_fts_verification_marker() {
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
            .expect("disable auto-checkpoint for repair fixture");
    }
    storage
        .full_reconcile(
            "config",
            (0..8)
                .map(|index| sample_file(&format!("repair-{index}.rs"), "fn repair() {}\n"))
                .collect(),
        )
        .expect("backlogged generation");
    {
        let writer = storage
            .writer
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        writer
            .execute(
                "DELETE FROM path_entries WHERE file_id = (SELECT id FROM files LIMIT 1)",
                [],
            )
            .expect("damage path projection");
    }
    let wal_bytes_before = fs::metadata(wal_path(&database))
        .expect("repair fixture WAL")
        .len();
    assert!(wal_bytes_before > 0);

    let repaired = Storage::open(&database).expect("repair storage on reopen");

    let writer = repaired
        .writer
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let (files, paths) = writer
        .query_row(
            "SELECT (SELECT count(*) FROM files),
                    (SELECT count(*) FROM path_entries WHERE kind = 1)",
            [],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
        )
        .expect("repaired projection counts");
    assert_eq!(paths, files);
    assert!(
        writer
            .query_row("PRAGMA wal_autocheckpoint", [], |row| row.get::<_, i64>(0))
            .expect("restored auto-checkpoint policy")
            > 0
    );
    let wal_bytes_after = fs::metadata(wal_path(&database))
        .expect("WAL after repair")
        .len();
    assert_eq!(
        wal_bytes_after, 0,
        "the path-projection repair should checkpoint the pre-existing backlog; a clean FTS verification does not write a marker"
    );
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
pub(crate) fn startup_rebuilds_external_content_fts_indexes_when_integrity_check_fails() {
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

    let reopened = Storage::open(&database).expect("rebuild FTS index on reopen");

    assert_eq!(
        reopened
            .search_word("repaired_fts_needle", 10)
            .expect("search rebuilt index")[0]
            .path,
        "needle.rs"
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

#[test]
pub(crate) fn unversioned_repository_binding_preserves_a_nonempty_root() {
    let directory = tempfile::tempdir().expect("directory");
    let expected_repository = directory.path().join("expected");
    let other_repository = directory.path().join("other");
    fs::create_dir(&expected_repository).expect("expected repository");
    fs::create_dir(&other_repository).expect("other repository");
    let database = directory.path().join("index.sqlite");
    let storage = Storage::open(&database).expect("storage");
    {
        let connection = storage
            .writer
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        connection
            .execute(
                "UPDATE meta SET repository_root = ?1, repository_identity = '' WHERE id = 1",
                [expected_repository.to_string_lossy().as_ref()],
            )
            .expect("install unversioned repository binding");
    }

    assert!(matches!(
        storage.bind_repository_at(&other_repository, None, 1_234),
        Err(Error::RepositoryMismatch {
            expected_repository: expected,
            actual_repository: actual,
            ..
        }) if expected == expected_repository.to_string_lossy()
            && actual == other_repository
    ));
    assert!(matches!(
        Storage::read_only_status_scoped(&database, &other_repository, None),
        Err(Error::RepositoryMismatch {
            expected_repository: expected,
            actual_repository: actual,
            ..
        }) if expected == expected_repository.to_string_lossy()
            && actual == other_repository
    ));

    storage
        .bind_repository_at(&expected_repository, None, 5_678)
        .expect("upgrade matching unversioned binding");
    let connection = Connection::open(&database).expect("inspect upgraded binding");
    let (root, identity): (String, String) = connection
        .query_row(
            "SELECT repository_root, repository_identity FROM meta WHERE id = 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("upgraded binding");
    assert_eq!(root, expected_repository.to_string_lossy());
    assert_eq!(identity, repository_identity(&expected_repository, None));
}

#[test]
pub(crate) fn mismatched_repository_open_repairs_nothing_before_binding_rejects() {
    let directory = tempfile::tempdir().expect("directory");
    let expected_repository = directory.path().join("expected");
    let other_repository = directory.path().join("other");
    fs::create_dir(&expected_repository).expect("expected repository");
    fs::create_dir(&other_repository).expect("other repository");
    let database = directory.path().join("index.sqlite");
    let storage = Storage::open(&database).expect("storage");
    storage
        .bind_repository_at(&expected_repository, None, 1_234)
        .expect("initial binding");
    storage
        .evaluate_receipt(None, 0, &[], true)
        .expect("receipt fixture");
    drop(storage);

    let connection = Connection::open(&database).expect("damage usage projection");
    connection
        .execute("DELETE FROM retrieval_receipt_usage WHERE id = 1", [])
        .expect("remove usage projection");
    drop(connection);

    assert!(matches!(
        Storage::open_for_repository_scoped(&database, &other_repository, None),
        Err(Error::RepositoryMismatch { .. })
    ));
    let connection = Connection::open(&database).expect("inspect rejected database");
    assert_eq!(
        connection
            .query_row("SELECT count(*) FROM retrieval_receipts", [], |row| {
                row.get::<_, i64>(0)
            })
            .expect("receipt count"),
        1,
        "ownership rejection must not delete authoritative receipts"
    );
    assert_eq!(
        connection
            .query_row("SELECT count(*) FROM retrieval_receipt_usage", [], |row| {
                row.get::<_, i64>(0)
            })
            .expect("usage count"),
        0,
        "ownership rejection must not repair a foreign cache"
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
        .publish_reconciliation_at(&baseline, "config", IndexingMode::Reconcile, |writer| {
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
