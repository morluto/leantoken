use super::super::*;
use crate::model::{Freshness, IndexScopeMode, ResponseMeta};

fn response_meta() -> ResponseMeta {
    ResponseMeta {
        repository_id: "repository".into(),
        repository_generation: 1,
        freshness: Freshness::Current,
        index_scope: IndexScopeMode::Full,
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
    }
}

fn table_exists(connection: &Connection, table: &str) -> bool {
    connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type = 'table' AND name = ?1)",
            [table],
            |row| row.get(0),
        )
        .expect("table lookup")
}

#[test]
fn instrumentation_tables_are_not_part_of_the_repository_index() {
    let directory = tempfile::tempdir().expect("directory");
    let index = Storage::open(directory.path().join("index.sqlite")).expect("index storage");
    let instrumentation =
        InstrumentationStorage::open(&directory.path().join("instrumentation.sqlite"));

    let index_writer = index
        .writer
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    assert!(!table_exists(&index_writer, "token_savings"));
    assert!(!table_exists(&index_writer, "service_failures"));
    drop(index_writer);

    let instrumentation_writer = instrumentation
        .writer
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    assert!(table_exists(&instrumentation_writer, "token_savings"));
    assert!(table_exists(&instrumentation_writer, "service_failures"));
}

#[test]
fn corrupt_instrumentation_falls_back_without_touching_the_index() {
    let directory = tempfile::tempdir().expect("directory");
    let index_path = directory.path().join("index.sqlite");
    let instrumentation_path = directory.path().join("instrumentation.sqlite");
    fs::write(&instrumentation_path, b"not a sqlite database").expect("corrupt fixture");

    let instrumentation = InstrumentationStorage::open(&instrumentation_path);
    let index = Storage::open(&index_path).expect("repository index remains available");
    assert_eq!(
        index.meta().expect("index metadata").repository_generation,
        0
    );
    assert!(
        instrumentation
            .record_service_failure("cl100k_base", TokenAccountingOperation::Search, "fixture",)
            .expect("process-local instrumentation")
    );
}

#[test]
fn legacy_primary_accounting_is_copied_before_the_tables_are_removed() {
    let directory = tempfile::tempdir().expect("directory");
    let index_path = directory.path().join("index.sqlite");
    let instrumentation_path = directory.path().join("instrumentation.sqlite");
    let index = Storage::open(&index_path).expect("index storage");
    {
        let writer = index
            .writer
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        writer
            .execute_batch(super::super::instrumentation::INSTRUMENTATION_SCHEMA)
            .expect("legacy accounting tables");
        writer
            .execute(
                "INSERT INTO token_savings(tokenizer, operation, tracked_requests)
                 VALUES ('cl100k_base', 'search', 7)",
                [],
            )
            .expect("legacy token savings");
        writer
            .execute(
                "INSERT INTO service_failures(tokenizer, operation, error_category, failed_requests)
                 VALUES ('cl100k_base', 'search', 'fixture', 3)",
                [],
            )
            .expect("legacy service failures");
    }
    drop(index);

    let instrumentation = InstrumentationStorage::open(&instrumentation_path);
    instrumentation
        .migrate_legacy_primary(&index_path)
        .expect("copy legacy accounting");
    instrumentation
        .migrate_legacy_primary(&index_path)
        .expect("idempotent legacy accounting copy");

    let (savings, failures) = instrumentation
        .snapshot("cl100k_base")
        .expect("migrated accounting");
    assert_eq!(
        savings
            .get("search")
            .expect("search savings")
            .tracked_requests,
        7
    );
    assert_eq!(
        failures,
        vec![ServiceFailureRecord {
            operation: "search".into(),
            error_category: "fixture".into(),
            failed_requests: 3,
        }]
    );
}

#[test]
fn older_legacy_primary_accounting_defaults_new_columns_before_copying() {
    let directory = tempfile::tempdir().expect("directory");
    let index_path = directory.path().join("index.sqlite");
    let instrumentation_path = directory.path().join("instrumentation.sqlite");
    let index = Connection::open(&index_path).expect("legacy index");
    index
        .execute_batch(
            "CREATE TABLE token_savings (
                 tokenizer TEXT NOT NULL,
                 operation TEXT NOT NULL,
                 tracked_requests INTEGER NOT NULL DEFAULT 0,
                 baseline_source_tokens INTEGER NOT NULL DEFAULT 0,
                 emitted_source_tokens INTEGER NOT NULL DEFAULT 0,
                 estimated_source_tokens_saved INTEGER NOT NULL DEFAULT 0,
                 PRIMARY KEY(tokenizer, operation)
             );
             CREATE TABLE service_failures (
                 tokenizer TEXT NOT NULL,
                 operation TEXT NOT NULL,
                 error_category TEXT NOT NULL,
                 failed_requests INTEGER NOT NULL DEFAULT 0,
                 PRIMARY KEY(tokenizer, operation, error_category)
             );
             INSERT INTO token_savings(
                 tokenizer, operation, tracked_requests,
                 baseline_source_tokens, emitted_source_tokens,
                 estimated_source_tokens_saved
             ) VALUES ('cl100k_base', 'search', 7, 11, 3, 8);
             INSERT INTO service_failures(
                 tokenizer, operation, error_category, failed_requests
             ) VALUES ('cl100k_base', 'search', 'fixture', 3);",
        )
        .expect("older additive schema");
    drop(index);

    let instrumentation = InstrumentationStorage::open(&instrumentation_path);
    instrumentation
        .migrate_legacy_primary(&index_path)
        .expect("copy older legacy accounting");

    let (savings, failures) = instrumentation
        .snapshot("cl100k_base")
        .expect("migrated accounting");
    Storage::open(&index_path).expect("older primary opens after migration");
    let record = savings.get("search").expect("search savings");
    assert_eq!(record.tracked_requests, 7);
    assert_eq!(record.baseline_source_tokens, 11);
    assert_eq!(record.emitted_source_tokens, 3);
    assert_eq!(record.estimated_source_tokens_saved, 8);
    assert_eq!(record.response_tracked_requests, 0);
    assert_eq!(record.total_response_tokens, 0);
    assert_eq!(
        failures,
        vec![ServiceFailureRecord {
            operation: "search".into(),
            error_category: "fixture".into(),
            failed_requests: 3,
        }]
    );
}

#[test]
fn accounting_skips_a_busy_local_writer() {
    let directory = tempfile::tempdir().expect("directory");
    let storage = InstrumentationStorage::open(&directory.path().join("instrumentation.sqlite"));
    let meta = response_meta();
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
    let (_, failures) = storage
        .snapshot("cl100k_base")
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
