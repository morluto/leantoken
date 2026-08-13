use super::super::*;
use crate::receipt::{ReceiptDecision, ReceiptEvidence};

fn evidence(hash: &str) -> ReceiptEvidence {
    ReceiptEvidence::new("src/lib.rs", 1, 2, hash, Some("fn example() {}"))
}

#[test]
fn evidence_artifacts_are_content_addressed_and_immutable() {
    let directory = tempfile::tempdir().expect("directory");
    let storage = ArtifactStorage::open(&directory.path().join("artifacts.sqlite"));
    let first = storage
        .evaluate_receipt(
            "repository",
            "incarnation",
            None,
            7,
            &[evidence("one")],
            true,
        )
        .expect("first artifact");
    let same = storage
        .evaluate_receipt(
            "repository",
            "incarnation",
            None,
            7,
            &[evidence("one")],
            true,
        )
        .expect("same artifact");
    assert_eq!(same.receipt_id, first.receipt_id);

    let extended = storage
        .evaluate_receipt(
            "repository",
            "incarnation",
            Some(&first.receipt_id),
            7,
            &[evidence("two")],
            false,
        )
        .expect("extended artifact");
    assert_ne!(extended.receipt_id, first.receipt_id);
    assert_eq!(
        storage
            .read_receipt("repository", "incarnation", &first.receipt_id)
            .expect("first")
            .evidence
            .len(),
        1
    );
    assert_eq!(
        storage
            .read_receipt("repository", "incarnation", &extended.receipt_id)
            .expect("extended")
            .evidence
            .len(),
        2
    );
}

#[test]
fn one_response_batch_does_not_suppress_its_own_candidates() {
    let directory = tempfile::tempdir().expect("directory");
    let storage = ArtifactStorage::open(&directory.path().join("artifacts.sqlite"));
    let duplicate = evidence("same");
    let evaluation = storage
        .evaluate_receipt(
            "repository",
            "incarnation",
            None,
            7,
            &[duplicate.clone(), duplicate],
            true,
        )
        .expect("batch artifact");
    assert_eq!(
        evaluation.decisions,
        vec![ReceiptDecision::Return, ReceiptDecision::Return]
    );
    assert_eq!(
        storage
            .read_receipt("repository", "incarnation", &evaluation.receipt_id)
            .expect("stored batch")
            .evidence
            .len(),
        2
    );
}

#[test]
fn artifact_ids_bind_repository_generation_and_payload_integrity() {
    let directory = tempfile::tempdir().expect("directory");
    let storage = ArtifactStorage::open(&directory.path().join("artifacts.sqlite"));
    let artifact = storage
        .evaluate_receipt(
            "repository",
            "incarnation",
            None,
            7,
            &[evidence("one")],
            true,
        )
        .expect("artifact");
    assert!(matches!(
        storage.evaluate_receipt(
            "other",
            "incarnation",
            Some(&artifact.receipt_id),
            7,
            &[],
            true
        ),
        Err(Error::UnknownReceipt(_))
    ));
    assert!(matches!(
        storage.evaluate_receipt(
            "repository",
            "incarnation",
            Some(&artifact.receipt_id),
            8,
            &[],
            true
        ),
        Err(Error::StaleReceipt { .. })
    ));
    assert!(matches!(
        storage.evaluate_receipt(
            "repository",
            "recreated-database",
            Some(&artifact.receipt_id),
            7,
            &[],
            true,
        ),
        Err(Error::UnknownReceipt(_))
    ));

    let connection = storage
        .writer
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    connection
        .execute(
            "UPDATE artifacts SET payload = json_set(payload, '$.version', 2) WHERE id = ?1",
            [&artifact.receipt_id],
        )
        .expect("tamper artifact");
    drop(connection);
    assert!(
        storage
            .read_receipt("repository", "incarnation", &artifact.receipt_id)
            .is_err()
    );
}

#[test]
fn recreated_primary_database_cannot_use_old_sidecar_artifacts() {
    let directory = tempfile::tempdir().expect("directory");
    let index_path = directory.path().join("index.sqlite");
    let artifact_path = directory.path().join("index.artifacts.sqlite");
    let index = Storage::open(&index_path).expect("first index");
    let first_incarnation = index
        .meta()
        .expect("first metadata")
        .database_incarnation_id;
    assert_eq!(first_incarnation.len(), 32);
    let artifacts = ArtifactStorage::open(&artifact_path);
    let receipt_id = artifacts
        .evaluate_receipt(
            "repository",
            &first_incarnation,
            None,
            1,
            &[evidence("first")],
            true,
        )
        .expect("artifact")
        .receipt_id;
    drop(index);

    std::fs::remove_file(&index_path).expect("remove only primary database");
    let recreated = Storage::open(&index_path).expect("recreated index");
    let recreated_incarnation = recreated
        .meta()
        .expect("recreated metadata")
        .database_incarnation_id;
    assert_eq!(recreated_incarnation.len(), 32);
    assert_ne!(recreated_incarnation, first_incarnation);
    assert!(matches!(
        artifacts.read_receipt("repository", &recreated_incarnation, &receipt_id),
        Err(Error::UnknownReceipt(_))
    ));
}

#[test]
fn recreated_primary_database_reclaims_old_sidecar_quota() {
    let directory = tempfile::tempdir().expect("directory");
    let index_path = directory.path().join("index.sqlite");
    let artifact_path = directory.path().join("index.artifacts.sqlite");
    let index = Storage::open(&index_path).expect("first index");
    let first_incarnation = index
        .meta()
        .expect("first metadata")
        .database_incarnation_id;
    let artifacts = ArtifactStorage::open(&artifact_path);

    for index in 0..256 {
        artifacts
            .evaluate_receipt(
                "repository",
                &first_incarnation,
                None,
                index as u64,
                &[evidence(&format!("old-{index}"))],
                true,
            )
            .expect("fill old incarnation quota");
    }
    drop(index);

    std::fs::remove_file(&index_path).expect("remove only primary database");
    let recreated = Storage::open(&index_path).expect("recreated index");
    let recreated_incarnation = recreated
        .meta()
        .expect("recreated metadata")
        .database_incarnation_id;
    assert_ne!(recreated_incarnation, first_incarnation);

    artifacts
        .evaluate_receipt(
            "repository",
            &recreated_incarnation,
            None,
            1,
            &[evidence("new")],
            true,
        )
        .expect("new incarnation must not inherit old quota usage");
}

#[test]
fn reused_primary_database_reclaims_old_repository_quota() {
    let directory = tempfile::tempdir().expect("directory");
    let artifact_path = directory.path().join("index.artifacts.sqlite");
    let artifacts = ArtifactStorage::open(&artifact_path);

    for index in 0..256 {
        artifacts
            .evaluate_receipt(
                "old-repository",
                "old-incarnation",
                None,
                index as u64,
                &[evidence(&format!("old-{index}"))],
                true,
            )
            .expect("fill old repository quota");
    }

    artifacts
        .evaluate_receipt(
            "new-repository",
            "new-incarnation",
            None,
            1,
            &[evidence("new")],
            true,
        )
        .expect("new repository must not inherit old quota usage");
}

#[test]
fn legacy_mutable_state_is_absent_from_the_repository_index() {
    let directory = tempfile::tempdir().expect("directory");
    let index = Storage::open(directory.path().join("index.sqlite")).expect("index");
    let connection = index
        .writer
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    for table in [
        "retrieval_receipts",
        "retrieval_receipt_evidence",
        "query_coverage_receipts",
        "read_delta_bases",
    ] {
        let exists: bool = connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type = 'table' AND name = ?1)",
                [table],
                |row| row.get(0),
            )
            .expect("schema query");
        assert!(!exists, "legacy mutable table remained: {table}");
    }
}

#[test]
fn artifact_lookup_uses_the_primary_key_index() {
    let directory = tempfile::tempdir().expect("directory");
    let storage = ArtifactStorage::open(&directory.path().join("artifacts.sqlite"));
    let connection = storage
        .writer
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let detail: String = connection
        .query_row(
            "EXPLAIN QUERY PLAN
             SELECT repository_identity, repository_generation, payload
             FROM artifacts WHERE id = 'r0000000000000000000000000000000000000000000000000000000000000000'
               AND kind = 'evidence'",
            [],
            |row| row.get(3),
        )
        .expect("query plan");
    assert!(detail.contains("sqlite_autoindex_artifacts_1"), "{detail}");
}
