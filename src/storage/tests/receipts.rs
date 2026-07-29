use std::sync::{Arc, Barrier};

use super::*;
use crate::error::RetryableOperation;
use crate::receipt::{
    MAX_EVIDENCE_BYTES_PER_RECEIPT, MAX_EVIDENCE_PER_RECEIPT, MAX_RECEIPTS, MAX_TOTAL_EVIDENCE,
    RECEIPT_TTL_MILLIS, ReceiptDecision, ReceiptEvidence,
};

fn evidence(index: usize) -> ReceiptEvidence {
    ReceiptEvidence::new(
        format!("src/file-{index}.rs"),
        index.saturating_mul(2).saturating_add(1),
        index.saturating_mul(2).saturating_add(1),
        format!("{index:032x}"),
        Some(&format!("token alpha beta item_{index}")),
    )
}

fn usage(storage: &Storage) -> (usize, usize, usize, usize) {
    let connection = storage
        .writer
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    connection
        .query_row(
            "SELECT receipt_count, receipt_bytes, evidence_count, evidence_bytes
             FROM retrieval_receipt_usage
             WHERE id = 1",
            [],
            |row| {
                Ok((
                    i64_to_usize(row.get(0)?)?,
                    i64_to_usize(row.get(1)?)?,
                    i64_to_usize(row.get(2)?)?,
                    i64_to_usize(row.get(3)?)?,
                ))
            },
        )
        .expect("receipt usage")
}

#[test]
fn persistent_receipt_survives_storage_restart() {
    let directory = tempfile::tempdir().expect("directory");
    let database = directory.path().join("index.sqlite");
    let first = evidence(1);
    let receipt_id = {
        let storage = Storage::open(&database).expect("storage");
        storage
            .evaluate_receipt(None, 7, std::slice::from_ref(&first), true)
            .expect("create receipt")
            .receipt_id
    };
    let reopened = Storage::open(&database).expect("reopen storage");
    let repeated = reopened
        .evaluate_receipt(Some(&receipt_id), 7, &[first], true)
        .expect("reuse persistent receipt");
    assert_eq!(repeated.decisions, vec![ReceiptDecision::SuppressExact]);
}

#[test]
fn receipt_resource_read_is_snapshot_only_and_does_not_extend_lifetime() {
    let directory = tempfile::tempdir().expect("directory");
    let storage = Storage::open(directory.path().join("index.sqlite")).expect("storage");
    let receipt_id = storage
        .evaluate_receipt_at(None, 7, &[evidence(1)], true, 1_000)
        .expect("create receipt")
        .receipt_id;
    let before: (i64, i64, i64) = {
        let connection = storage
            .writer
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        connection
            .query_row(
                "SELECT last_access_unix_millis, expires_unix_millis, access_sequence
                 FROM retrieval_receipts WHERE id = 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .expect("receipt state")
    };
    let receipt = storage
        .read_receipt(&receipt_id, 1_001)
        .expect("read stored receipt");
    assert_eq!(receipt.evidence, vec![evidence(1)]);
    let after: (i64, i64, i64) = {
        let connection = storage
            .writer
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        connection
            .query_row(
                "SELECT last_access_unix_millis, expires_unix_millis, access_sequence
                 FROM retrieval_receipts WHERE id = 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .expect("receipt state")
    };
    assert_eq!(after, before);
    assert!(matches!(
        storage.read_receipt(&receipt_id, 1_000 + RECEIPT_TTL_MILLIS),
        Err(Error::UnknownReceipt(_))
    ));
}

#[test]
fn sqlite_decisions_match_the_previous_in_memory_oracle() {
    let directory = tempfile::tempdir().expect("directory");
    let storage = Storage::open(directory.path().join("index.sqlite")).expect("storage");
    let first = ReceiptEvidence::new(
        "src/lib.rs",
        10,
        20,
        "first",
        Some("alpha beta gamma delta epsilon"),
    );
    let receipt_id = storage
        .evaluate_receipt(None, 7, std::slice::from_ref(&first), true)
        .expect("create receipt")
        .receipt_id;
    let mut oracle = vec![first.clone()];
    let candidates = vec![
        first,
        ReceiptEvidence::new("src/lib.rs", 20, 30, "second", Some("unrelated words here")),
        ReceiptEvidence::new(
            "src/other.rs",
            1,
            2,
            "third",
            Some("alpha beta gamma delta epsilon zeta"),
        ),
        ReceiptEvidence::new(
            "src/new.rs",
            50,
            55,
            "fourth",
            Some("completely separate implementation detail"),
        ),
    ];
    for suppress_overlap in [true, false] {
        let expected = candidates
            .iter()
            .map(|candidate| oracle_decide(&oracle, candidate, suppress_overlap))
            .collect::<Vec<_>>();
        let actual = storage
            .evaluate_receipt(Some(&receipt_id), 7, &candidates, suppress_overlap)
            .expect("persistent evaluation");
        assert_eq!(actual.decisions, expected);
        oracle.extend(
            candidates
                .iter()
                .zip(expected)
                .filter(|(_, decision)| {
                    matches!(
                        decision,
                        ReceiptDecision::Return | ReceiptDecision::ReturnNearDuplicate
                    )
                })
                .map(|(candidate, _)| candidate.clone()),
        );
    }
}

#[test]
fn concurrent_duplicate_append_returns_source_once_without_lost_update() {
    let directory = tempfile::tempdir().expect("directory");
    let database = directory.path().join("index.sqlite");
    let storage = Storage::open(&database).expect("storage");
    let receipt_id = storage
        .evaluate_receipt(None, 1, &[], true)
        .expect("create receipt")
        .receipt_id;
    let barrier = Arc::new(Barrier::new(3));
    let candidate = evidence(1);
    let mut threads = Vec::new();
    for _ in 0..2 {
        let database = database.clone();
        let receipt_id = receipt_id.clone();
        let barrier = Arc::clone(&barrier);
        let candidate = candidate.clone();
        threads.push(std::thread::spawn(move || {
            let storage = Storage::open(database).expect("independent storage");
            barrier.wait();
            storage
                .evaluate_receipt(Some(&receipt_id), 1, &[candidate], true)
                .expect("concurrent evaluation")
                .decisions[0]
        }));
    }
    barrier.wait();
    let mut decisions = threads
        .into_iter()
        .map(|thread| thread.join().expect("join"))
        .collect::<Vec<_>>();
    decisions.sort_by_key(|decision| match decision {
        ReceiptDecision::Return => 0,
        ReceiptDecision::SuppressExact => 1,
        ReceiptDecision::SuppressOverlap => 2,
        ReceiptDecision::ReturnNearDuplicate => 3,
    });
    assert_eq!(
        decisions,
        vec![ReceiptDecision::Return, ReceiptDecision::SuppressExact]
    );
    assert_eq!(usage(&storage).2, 1);
}

#[test]
fn independent_writers_append_distinct_evidence_without_lost_update() {
    let directory = tempfile::tempdir().expect("directory");
    let database = directory.path().join("index.sqlite");
    let storage = Storage::open(&database).expect("storage");
    let receipt_id = storage
        .evaluate_receipt(None, 1, &[], true)
        .expect("create receipt")
        .receipt_id;
    let barrier = Arc::new(Barrier::new(3));
    let mut threads = Vec::new();
    for index in 1..=2 {
        let database = database.clone();
        let receipt_id = receipt_id.clone();
        let barrier = Arc::clone(&barrier);
        threads.push(std::thread::spawn(move || {
            let storage = Storage::open(database).expect("independent storage");
            barrier.wait();
            storage
                .evaluate_receipt(Some(&receipt_id), 1, &[evidence(index)], true)
                .expect("append distinct evidence");
        }));
    }
    barrier.wait();
    for thread in threads {
        thread.join().expect("join");
    }
    let repeated = storage
        .evaluate_receipt(Some(&receipt_id), 1, &[evidence(1), evidence(2)], true)
        .expect("read both appends");
    assert_eq!(
        repeated.decisions,
        vec![
            ReceiptDecision::SuppressExact,
            ReceiptDecision::SuppressExact
        ]
    );
    assert_eq!(usage(&storage).2, 2);
}

#[test]
fn receipt_expiry_and_clock_rollback_fail_closed() {
    let directory = tempfile::tempdir().expect("directory");
    let storage = Storage::open(directory.path().join("index.sqlite")).expect("storage");
    let expires = storage
        .evaluate_receipt_at(None, 1, &[], true, 10_000)
        .expect("create expiring receipt")
        .receipt_id;
    assert!(matches!(
        storage.evaluate_receipt_at(
            Some(&expires),
            1,
            &[],
            true,
            10_000 + RECEIPT_TTL_MILLIS
        ),
        Err(Error::UnknownReceipt(id)) if id == expires
    ));
    assert_eq!(usage(&storage).0, 0);

    let rollback = storage
        .evaluate_receipt_at(None, 1, &[], true, 20_000)
        .expect("create rollback receipt")
        .receipt_id;
    assert!(matches!(
        storage.evaluate_receipt_at(Some(&rollback), 1, &[], true, 19_999),
        Err(Error::UnknownReceipt(id)) if id == rollback
    ));
}

#[test]
fn receipt_namespace_prevents_cross_database_id_reuse() {
    let directory = tempfile::tempdir().expect("directory");
    let left = Storage::open(directory.path().join("left.sqlite")).expect("left");
    let right = Storage::open(directory.path().join("right.sqlite")).expect("right");
    let left_id = left
        .evaluate_receipt(None, 1, &[], true)
        .expect("left receipt")
        .receipt_id;
    let right_id = right
        .evaluate_receipt(None, 1, &[], true)
        .expect("right receipt")
        .receipt_id;
    assert_ne!(left_id, right_id);
    assert!(matches!(
        right.evaluate_receipt(Some(&left_id), 1, &[], true),
        Err(Error::UnknownReceipt(id)) if id == left_id
    ));
    assert!(matches!(
        right.load_receipt_rebase_source(&left_id),
        Err(Error::UnknownReceipt(id)) if id == left_id
    ));
}

#[test]
fn receipt_schema_contains_metadata_only() {
    let directory = tempfile::tempdir().expect("directory");
    let storage = Storage::open(directory.path().join("index.sqlite")).expect("storage");
    let connection = storage
        .writer
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let mut columns = Vec::new();
    for table in ["retrieval_receipts", "retrieval_receipt_evidence"] {
        let mut statement = connection
            .prepare(&format!("PRAGMA table_info({table})"))
            .expect("table info");
        columns.extend(
            statement
                .query_map([], |row| row.get::<_, String>(1))
                .expect("column rows")
                .collect::<rusqlite::Result<Vec<_>>>()
                .expect("columns"),
        );
    }
    assert!(!columns.is_empty());
    for forbidden in ["task", "query", "source", "content", "prompt", "message"] {
        assert!(
            columns.iter().all(|column| column != forbidden),
            "receipt schema persisted forbidden field {forbidden:?}: {columns:?}"
        );
    }
    assert!(columns.iter().all(|column| !column.starts_with("raw_")));
}

#[test]
fn stale_and_overlong_receipts_remain_fail_loud() {
    let directory = tempfile::tempdir().expect("directory");
    let storage = Storage::open(directory.path().join("index.sqlite")).expect("storage");
    let receipt_id = storage
        .evaluate_receipt(None, 1, &[evidence(1)], true)
        .expect("create receipt")
        .receipt_id;
    assert!(matches!(
        storage.evaluate_receipt(Some(&receipt_id), 2, &[], true),
        Err(Error::StaleReceipt {
            receipt_generation: 1,
            repository_generation: 2
        })
    ));
    assert_eq!(usage(&storage).2, 1, "stale evidence remains until TTL");
    assert!(matches!(
        storage.evaluate_receipt(Some(&"x".repeat(129)), 1, &[], true),
        Err(Error::InputTooLong {
            field: "receipt_id",
            max_bytes: 128
        })
    ));
}

#[test]
fn old_snapshot_receipt_remains_generation_bound_after_publication() {
    let directory = tempfile::tempdir().expect("directory");
    let storage = Storage::open(directory.path().join("index.sqlite")).expect("storage");
    let first_generation = storage
        .full_reconcile("first", vec![sample_file("lib.rs", "fn first() {}\n")])
        .expect("first generation");
    let snapshot = storage.begin_read().expect("pin old snapshot");
    assert_eq!(
        snapshot
            .meta()
            .expect("snapshot meta")
            .repository_generation,
        first_generation
    );
    let second_generation = storage
        .full_reconcile("second", vec![sample_file("lib.rs", "fn second() {}\n")])
        .expect("publish second generation");
    assert!(second_generation > first_generation);

    let candidate = evidence(1);
    let receipt_id = storage
        .evaluate_receipt(
            None,
            first_generation,
            std::slice::from_ref(&candidate),
            true,
        )
        .expect("receipt for pinned snapshot")
        .receipt_id;
    assert_eq!(
        storage
            .evaluate_receipt(Some(&receipt_id), first_generation, &[candidate], true,)
            .expect("same snapshot reuse")
            .decisions,
        vec![ReceiptDecision::SuppressExact]
    );
    assert!(matches!(
        storage.evaluate_receipt(Some(&receipt_id), second_generation, &[], true),
        Err(Error::StaleReceipt {
            receipt_generation,
            repository_generation
        }) if receipt_generation == first_generation
            && repository_generation == second_generation
    ));
    drop(snapshot);
}

#[test]
fn rebased_receipt_is_atomic_source_preserving_and_restart_safe() {
    let directory = tempfile::tempdir().expect("directory");
    let database = directory.path().join("index.sqlite");
    let storage = Storage::open(&database).expect("storage");
    let first_generation = storage
        .full_reconcile("first", vec![sample_file("lib.rs", "fn stable() {}\n")])
        .expect("first generation");
    let stable = evidence(1);
    let changed = evidence(2);
    let source_id = storage
        .evaluate_receipt(
            None,
            first_generation,
            &[stable.clone(), changed.clone()],
            true,
        )
        .expect("source receipt")
        .receipt_id;
    let source = storage
        .load_receipt_rebase_source(&source_id)
        .expect("load source");
    let second_generation = storage
        .full_reconcile("second", vec![sample_file("lib.rs", "fn stable() {}\n")])
        .expect("second generation");
    let rebased_id = storage
        .persist_rebased_receipt(&source, second_generation, std::slice::from_ref(&stable))
        .expect("persist exact subset");

    let connection = storage
        .writer
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let namespace: String = connection
        .query_row(
            "SELECT namespace FROM retrieval_receipt_usage WHERE id = 1",
            [],
            |row| row.get(0),
        )
        .expect("namespace");
    let source_row =
        crate::receipt::parse_receipt_id(&source_id, &namespace).expect("source row id");
    let rebased_row =
        crate::receipt::parse_receipt_id(&rebased_id, &namespace).expect("rebased row id");
    let source_state: (u64, usize) = connection
        .query_row(
            "SELECT repository_generation, evidence_count
             FROM retrieval_receipts WHERE id = ?1",
            [source_row],
            |row| Ok((i64_to_u64(row.get(0)?)?, i64_to_usize(row.get(1)?)?)),
        )
        .expect("source state");
    assert_eq!(source_state, (first_generation, 2));
    let rebased_state: (u64, usize) = connection
        .query_row(
            "SELECT repository_generation, evidence_count
             FROM retrieval_receipts WHERE id = ?1",
            [rebased_row],
            |row| Ok((i64_to_u64(row.get(0)?)?, i64_to_usize(row.get(1)?)?)),
        )
        .expect("rebased state");
    assert_eq!(rebased_state, (second_generation, 1));
    assert_eq!(
        connection
            .query_row(
                "SELECT exact_only
                 FROM retrieval_receipt_evidence
                 WHERE receipt_id = ?1 AND ordinal = 0",
                [rebased_row],
                |row| row.get::<_, i64>(0),
            )
            .expect("rebased evidence mode"),
        1
    );
    drop(connection);
    drop(storage);

    let reopened = Storage::open(&database).expect("reopen");
    let overlapping_changed = ReceiptEvidence::new(
        stable.path.clone(),
        stable.start_line,
        stable.end_line,
        "changed-after-rebase",
        Some("completely different current representation"),
    );
    let repeated = reopened
        .evaluate_receipt(
            Some(&rebased_id),
            second_generation,
            &[stable, overlapping_changed],
            true,
        )
        .expect("reuse rebased receipt");
    assert_eq!(
        repeated.decisions,
        vec![ReceiptDecision::SuppressExact, ReceiptDecision::Return]
    );
}

#[test]
fn rebase_conflicts_roll_back_without_mutating_the_source() {
    let directory = tempfile::tempdir().expect("directory");
    let storage = Storage::open(directory.path().join("index.sqlite")).expect("storage");
    let first_generation = storage
        .full_reconcile("first", vec![sample_file("lib.rs", "fn first() {}\n")])
        .expect("first generation");
    let first = evidence(1);
    let appended = evidence(2);
    let source_id = storage
        .evaluate_receipt(None, first_generation, std::slice::from_ref(&first), true)
        .expect("source receipt")
        .receipt_id;
    let stale_source = storage
        .load_receipt_rebase_source(&source_id)
        .expect("source snapshot");
    storage
        .evaluate_receipt(
            Some(&source_id),
            first_generation,
            std::slice::from_ref(&appended),
            true,
        )
        .expect("concurrent append");
    let second_generation = storage
        .full_reconcile("second", vec![sample_file("lib.rs", "fn second() {}\n")])
        .expect("second generation");
    let before = usage(&storage);
    assert!(matches!(
        storage.persist_rebased_receipt(
            &stale_source,
            second_generation,
            std::slice::from_ref(&first)
        ),
        Err(Error::RetryableConflict(RetryableOperation::Retrieval))
    ));
    assert_eq!(usage(&storage), before);

    let current_source = storage
        .load_receipt_rebase_source(&source_id)
        .expect("current source");
    let third_generation = storage
        .full_reconcile("third", vec![sample_file("lib.rs", "fn third() {}\n")])
        .expect("third generation");
    assert!(third_generation > second_generation);
    assert!(matches!(
        storage.persist_rebased_receipt(
            &current_source,
            second_generation,
            std::slice::from_ref(&first)
        ),
        Err(Error::RetryableConflict(RetryableOperation::Retrieval))
    ));
    assert_eq!(usage(&storage), before);
    assert_eq!(
        storage
            .load_receipt_rebase_source(&source_id)
            .expect("source remains")
            .evidence,
        vec![first, appended]
    );
}

#[test]
fn receipt_lru_refresh_is_deterministic_at_the_header_bound() {
    let directory = tempfile::tempdir().expect("directory");
    let storage = Storage::open(directory.path().join("index.sqlite")).expect("storage");
    let mut ids = Vec::new();
    for index in 0..MAX_RECEIPTS {
        ids.push(
            storage
                .evaluate_receipt_at(None, 1, &[], true, 1_000 + index as i64)
                .expect("create receipt")
                .receipt_id,
        );
    }
    storage
        .evaluate_receipt_at(Some(&ids[0]), 1, &[], true, 70_000)
        .expect("refresh oldest receipt");
    storage
        .evaluate_receipt_at(None, 1, &[], true, 70_001)
        .expect("evict one receipt");
    assert!(
        storage
            .evaluate_receipt_at(Some(&ids[0]), 1, &[], true, 70_002)
            .is_ok()
    );
    assert!(matches!(
        storage.evaluate_receipt_at(Some(&ids[1]), 1, &[], true, 70_002),
        Err(Error::UnknownReceipt(id)) if id == ids[1]
    ));
    assert_eq!(usage(&storage).0, MAX_RECEIPTS);
}

#[test]
fn receipt_count_and_logical_byte_quotas_are_enforced() {
    let directory = tempfile::tempdir().expect("directory");
    let storage = Storage::open(directory.path().join("index.sqlite")).expect("storage");
    let candidates = (0..=MAX_EVIDENCE_PER_RECEIPT)
        .map(evidence)
        .collect::<Vec<_>>();
    let receipt_id = storage
        .evaluate_receipt(None, 1, &candidates, true)
        .expect("bounded evidence append")
        .receipt_id;
    let connection = storage
        .writer
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let namespace: String = connection
        .query_row(
            "SELECT namespace FROM retrieval_receipt_usage WHERE id = 1",
            [],
            |row| row.get(0),
        )
        .expect("namespace");
    let row_id = crate::receipt::parse_receipt_id(&receipt_id, &namespace).expect("receipt row");
    let (count, bytes): (usize, usize) = connection
        .query_row(
            "SELECT evidence_count, evidence_bytes
             FROM retrieval_receipts
             WHERE id = ?1",
            [row_id],
            |row| Ok((i64_to_usize(row.get(0)?)?, i64_to_usize(row.get(1)?)?)),
        )
        .expect("receipt counters");
    assert_eq!(count, MAX_EVIDENCE_PER_RECEIPT);
    assert!(bytes <= MAX_EVIDENCE_BYTES_PER_RECEIPT);
    drop(connection);

    for batch in 1..=(MAX_TOTAL_EVIDENCE / MAX_EVIDENCE_PER_RECEIPT) {
        let offset = batch.saturating_mul(MAX_EVIDENCE_PER_RECEIPT + 1);
        let batch = (0..MAX_EVIDENCE_PER_RECEIPT)
            .map(|index| evidence(offset + index))
            .collect::<Vec<_>>();
        storage
            .evaluate_receipt(None, 1, &batch, true)
            .expect("global bounded append");
    }
    let totals = usage(&storage);
    assert!(totals.2 <= MAX_TOTAL_EVIDENCE);
    assert!(totals.3 <= crate::receipt::MAX_TOTAL_EVIDENCE_BYTES);
}

#[test]
fn oversized_logical_evidence_is_returned_but_never_persisted() {
    let directory = tempfile::tempdir().expect("directory");
    let storage = Storage::open(directory.path().join("index.sqlite")).expect("storage");
    let oversized = ReceiptEvidence::new(
        "x".repeat(MAX_EVIDENCE_BYTES_PER_RECEIPT + 1),
        1,
        1,
        "hash",
        None,
    );
    let created = storage
        .evaluate_receipt(None, 1, std::slice::from_ref(&oversized), true)
        .expect("return oversized evidence");
    assert_eq!(created.decisions, vec![ReceiptDecision::Return]);
    assert_eq!(usage(&storage).2, 0);
    let repeated = storage
        .evaluate_receipt(Some(&created.receipt_id), 1, &[oversized], true)
        .expect("oversized evidence was not recorded");
    assert_eq!(repeated.decisions, vec![ReceiptDecision::Return]);
}

#[test]
fn receipt_insert_failure_rolls_back_header_usage_and_evidence() {
    let directory = tempfile::tempdir().expect("directory");
    let storage = Storage::open(directory.path().join("index.sqlite")).expect("storage");
    {
        let connection = storage
            .writer
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        connection
            .execute_batch(
                "CREATE TRIGGER fail_receipt_evidence
                 BEFORE INSERT ON retrieval_receipt_evidence
                 BEGIN
                     SELECT RAISE(ABORT, 'injected receipt failure');
                 END;",
            )
            .expect("failure trigger");
    }
    assert!(
        storage
            .evaluate_receipt(None, 1, &[evidence(1)], true)
            .is_err()
    );
    assert_eq!(usage(&storage), (0, 0, 0, 0));
}

#[test]
fn rebase_insert_failure_rolls_back_and_preserves_the_source() {
    let directory = tempfile::tempdir().expect("directory");
    let storage = Storage::open(directory.path().join("index.sqlite")).expect("storage");
    let first_generation = storage
        .full_reconcile("first", vec![sample_file("lib.rs", "fn first() {}\n")])
        .expect("first generation");
    let first = evidence(1);
    let source_id = storage
        .evaluate_receipt(None, first_generation, std::slice::from_ref(&first), true)
        .expect("source receipt")
        .receipt_id;
    let source = storage
        .load_receipt_rebase_source(&source_id)
        .expect("source snapshot");
    let second_generation = storage
        .full_reconcile("second", vec![sample_file("lib.rs", "fn second() {}\n")])
        .expect("second generation");
    {
        let connection = storage
            .writer
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        connection
            .execute_batch(
                "CREATE TRIGGER fail_rebased_evidence
                 BEFORE INSERT ON retrieval_receipt_evidence
                 BEGIN
                     SELECT RAISE(ABORT, 'injected rebase failure');
                 END;",
            )
            .expect("failure trigger");
    }
    let before = usage(&storage);
    assert!(
        storage
            .persist_rebased_receipt(&source, second_generation, std::slice::from_ref(&first))
            .is_err()
    );
    assert_eq!(usage(&storage), before);
    assert_eq!(
        storage
            .load_receipt_rebase_source(&source_id)
            .expect("source remains")
            .evidence,
        vec![first]
    );
}

#[test]
fn rebase_quota_eviction_never_selects_the_source_receipt() {
    let directory = tempfile::tempdir().expect("directory");
    let storage = Storage::open(directory.path().join("index.sqlite")).expect("storage");
    let first_generation = storage
        .full_reconcile("first", vec![sample_file("lib.rs", "fn first() {}\n")])
        .expect("first generation");
    let first = evidence(1);
    let source_id = storage
        .evaluate_receipt(None, first_generation, std::slice::from_ref(&first), true)
        .expect("source receipt")
        .receipt_id;
    for _ in 1..MAX_RECEIPTS {
        storage
            .evaluate_receipt(None, first_generation, &[], true)
            .expect("fill receipt headers");
    }
    assert_eq!(usage(&storage).0, MAX_RECEIPTS);
    let source = storage
        .load_receipt_rebase_source(&source_id)
        .expect("source snapshot");
    let second_generation = storage
        .full_reconcile("second", vec![sample_file("lib.rs", "fn second() {}\n")])
        .expect("second generation");
    let rebased = storage
        .persist_rebased_receipt(&source, second_generation, std::slice::from_ref(&first))
        .expect("rebase at header quota");
    assert_eq!(usage(&storage).0, MAX_RECEIPTS);
    assert_eq!(
        storage
            .load_receipt_rebase_source(&source_id)
            .expect("source retained")
            .evidence,
        vec![first.clone()]
    );
    assert_eq!(
        storage
            .evaluate_receipt(Some(&rebased), second_generation, &[first], true)
            .expect("rebased receipt")
            .decisions,
        vec![ReceiptDecision::SuppressExact]
    );
}

#[test]
fn poisoned_process_local_writer_mutex_does_not_corrupt_receipts() {
    let directory = tempfile::tempdir().expect("directory");
    let storage = Storage::open(directory.path().join("index.sqlite")).expect("storage");
    let poisoned = storage.clone();
    assert!(
        std::thread::spawn(move || {
            let _connection = poisoned.writer.lock().expect("writer lock");
            panic!("inject writer mutex poison");
        })
        .join()
        .is_err()
    );
    let evaluation = storage
        .evaluate_receipt(None, 1, &[evidence(1)], true)
        .expect("poison recovery");
    assert_eq!(evaluation.decisions, vec![ReceiptDecision::Return]);
    assert_eq!(usage(&storage).2, 1);
}

#[test]
fn receipt_lookup_append_and_prune_query_plans_use_bounded_indexes() {
    let directory = tempfile::tempdir().expect("directory");
    let storage = Storage::open(directory.path().join("index.sqlite")).expect("storage");
    let connection = storage
        .writer
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    for (sql, expected) in [
        (
            "SELECT id FROM retrieval_receipts WHERE id = 1",
            "USING INTEGER PRIMARY KEY",
        ),
        (
            "SELECT path FROM retrieval_receipt_evidence
             WHERE receipt_id = 1 ORDER BY ordinal",
            "USING INDEX sqlite_autoindex_retrieval_receipt_evidence_1",
        ),
        (
            "DELETE FROM retrieval_receipts WHERE expires_unix_millis <= 1",
            "retrieval_receipts_expiry_idx",
        ),
        (
            "DELETE FROM retrieval_receipts
             WHERE expires_unix_millis <= 1 AND id != 2",
            "retrieval_receipts_expiry_idx",
        ),
        (
            "SELECT id FROM retrieval_receipts
             WHERE access_sequence > 0
             ORDER BY access_sequence, id LIMIT 1",
            "USING COVERING INDEX retrieval_receipts_lru_idx",
        ),
    ] {
        let mut statement = connection
            .prepare(&format!("EXPLAIN QUERY PLAN {sql}"))
            .expect("query plan");
        let details = statement
            .query_map([], |row| row.get::<_, String>(3))
            .expect("plan rows")
            .collect::<rusqlite::Result<Vec<_>>>()
            .expect("plan details")
            .join(" | ");
        assert!(
            details.contains(expected),
            "expected {expected:?} in query plan {details:?} for {sql}"
        );
        assert!(
            !details.contains("SCAN retrieval_receipt"),
            "unbounded receipt scan: {details}"
        );
    }
}

#[test]
fn receipt_rebase_structural_lookup_uses_file_bounded_indexes() {
    let directory = tempfile::tempdir().expect("directory");
    let storage = Storage::open(directory.path().join("index.sqlite")).expect("storage");
    let connection = storage
        .writer
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let sql = "SELECT content
               FROM (
                   SELECT COALESCE(signature, name) AS content, rowid AS stable_id, 0 AS kind
                   FROM symbols
                   WHERE file_id = 1 AND start_line = 2 AND end_line = 3
                   UNION ALL
                   SELECT raw_target AS content, id AS stable_id, 1 AS kind
                   FROM imports
                   WHERE file_id = 1 AND line = 2 AND line = 3
               )
               ORDER BY kind, stable_id
               LIMIT 65";
    let mut statement = connection
        .prepare(&format!("EXPLAIN QUERY PLAN {sql}"))
        .expect("query plan");
    let details = statement
        .query_map([], |row| row.get::<_, String>(3))
        .expect("plan rows")
        .collect::<rusqlite::Result<Vec<_>>>()
        .expect("plan details")
        .join(" | ");
    assert!(
        details.contains("symbols_file_start_idx"),
        "symbol lookup must stay file-bounded: {details}"
    );
    assert!(
        details.contains("imports_file_line_idx"),
        "import lookup must stay file/line-bounded: {details}"
    );
    assert!(!details.contains("SCAN symbols"), "{details}");
    assert!(!details.contains("SCAN imports"), "{details}");
}

#[test]
fn receipt_migration_preserves_existing_index_and_failed_migration_rolls_back() {
    let directory = tempfile::tempdir().expect("directory");
    let database = directory.path().join("index.sqlite");
    {
        let storage = Storage::open(&database).expect("storage");
        storage
            .full_reconcile("config", vec![sample_file("lib.rs", "fn indexed() {}\n")])
            .expect("index");
    }
    downgrade_receipt_schema(&database, false);
    let migrated = Storage::open(&database).expect("migrate receipts");
    assert!(migrated.find_file("lib.rs").expect("find").is_some());
    drop(migrated);

    downgrade_receipt_schema(&database, true);
    assert!(Storage::open(&database).is_err());
    let connection = Connection::open(&database).expect("inspect failed migration");
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM files", [], |row| row.get::<_, i64>(0))
            .expect("file count"),
        1
    );
    assert_eq!(
        connection
            .pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
            .expect("migration version"),
        7
    );
}

#[test]
fn exact_only_migration_keeps_existing_evidence_as_ordinary() {
    let directory = tempfile::tempdir().expect("directory");
    let database = directory.path().join("index.sqlite");
    let storage = Storage::open(&database).expect("storage");
    let generation = storage
        .full_reconcile("config", vec![sample_file("lib.rs", "fn indexed() {}\n")])
        .expect("index");
    let prior = evidence(1);
    let receipt_id = storage
        .evaluate_receipt(None, generation, std::slice::from_ref(&prior), true)
        .expect("receipt")
        .receipt_id;
    drop(storage);

    let connection = Connection::open(&database).expect("downgrade connection");
    connection
        .execute_batch(
            "DROP TABLE query_coverage_receipts;
             DROP TABLE query_coverage_receipt_usage;
             ALTER TABLE retrieval_receipt_evidence DROP COLUMN exact_only;
             UPDATE retrieval_receipt_evidence
             SET logical_bytes = logical_bytes - 8;
             UPDATE retrieval_receipts
             SET evidence_bytes = evidence_bytes - evidence_count * 8;
             UPDATE retrieval_receipt_usage
             SET evidence_bytes = evidence_bytes - evidence_count * 8
             WHERE id = 1;
             UPDATE meta SET schema_version = 8 WHERE id = 1;
             PRAGMA user_version = 9;",
        )
        .expect("downgrade exact-only migration");
    drop(connection);

    let migrated = Storage::open(&database).expect("migrate exact-only column");
    let overlapping = ReceiptEvidence::new(
        prior.path,
        prior.start_line,
        prior.end_line,
        "changed",
        Some("unrelated replacement"),
    );
    let evaluation = migrated
        .evaluate_receipt(Some(&receipt_id), generation, &[overlapping], true)
        .expect("evaluate migrated ordinary evidence");
    assert_eq!(evaluation.decisions, vec![ReceiptDecision::SuppressOverlap]);
}

fn downgrade_receipt_schema(database: &Path, conflicting_table: bool) {
    let connection = Connection::open(database).expect("downgrade connection");
    connection
        .execute_batch(
            "DROP TABLE IF EXISTS query_coverage_receipts;
             DROP TABLE IF EXISTS query_coverage_receipt_usage;
             DROP TABLE IF EXISTS read_delta_bases;
             DROP TABLE IF EXISTS read_delta_base_usage;
             DROP TABLE IF EXISTS retrieval_receipt_evidence;
             DROP TABLE IF EXISTS retrieval_receipts;
             DROP TABLE IF EXISTS retrieval_receipt_usage;
             UPDATE meta SET schema_version = 6 WHERE id = 1;
             PRAGMA user_version = 7;",
        )
        .expect("downgrade receipt schema");
    if conflicting_table {
        connection
            .execute(
                "CREATE TABLE retrieval_receipts(conflicting_value TEXT)",
                [],
            )
            .expect("conflicting migration table");
    }
}

fn oracle_decide(
    previous: &[ReceiptEvidence],
    candidate: &ReceiptEvidence,
    suppress_overlap: bool,
) -> ReceiptDecision {
    if !candidate.content_hash.is_empty()
        && previous
            .iter()
            .any(|seen| seen.content_hash == candidate.content_hash)
    {
        return ReceiptDecision::SuppressExact;
    }
    if suppress_overlap
        && previous.iter().any(|seen| {
            !seen.exact_only
                && seen.path == candidate.path
                && seen.start_line <= candidate.end_line
                && candidate.start_line <= seen.end_line
        })
    {
        return ReceiptDecision::SuppressOverlap;
    }
    if candidate.semantic_signature.is_some_and(|signature| {
        previous.iter().any(|seen| {
            !seen.exact_only
                && seen
                    .semantic_signature
                    .is_some_and(|prior| (signature ^ prior).count_ones() <= 8)
        })
    }) {
        return ReceiptDecision::ReturnNearDuplicate;
    }
    ReceiptDecision::Return
}
