use super::*;

pub(super) const INSTRUMENTATION_SCHEMA: &str = r#"
PRAGMA journal_mode = WAL;
PRAGMA synchronous = NORMAL;
CREATE TABLE IF NOT EXISTS token_savings (
    tokenizer TEXT NOT NULL,
    operation TEXT NOT NULL,
    tracked_requests INTEGER NOT NULL DEFAULT 0,
    response_tracked_requests INTEGER NOT NULL DEFAULT 0,
    response_baseline_requests INTEGER NOT NULL DEFAULT 0,
    baseline_source_tokens INTEGER NOT NULL DEFAULT 0,
    response_baseline_source_tokens INTEGER NOT NULL DEFAULT 0,
    emitted_source_tokens INTEGER NOT NULL DEFAULT 0,
    estimated_source_tokens_saved INTEGER NOT NULL DEFAULT 0,
    response_source_tokens INTEGER NOT NULL DEFAULT 0,
    path_and_metadata_tokens INTEGER NOT NULL DEFAULT 0,
    protocol_tokens INTEGER NOT NULL DEFAULT 0,
    total_response_tokens INTEGER NOT NULL DEFAULT 0,
    receipt_suppressed_exact INTEGER NOT NULL DEFAULT 0,
    receipt_suppressed_overlap INTEGER NOT NULL DEFAULT 0,
    expected_hash_not_modified_responses INTEGER NOT NULL DEFAULT 0,
    expected_hash_suppressed_source_tokens INTEGER NOT NULL DEFAULT 0,
    useful_requests INTEGER NOT NULL DEFAULT 0,
    incomplete_requests INTEGER NOT NULL DEFAULT 0,
    unsupported_requests INTEGER NOT NULL DEFAULT 0,
    hash_suppressed_requests INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY(tokenizer, operation)
);
CREATE TABLE IF NOT EXISTS service_failures (
    tokenizer TEXT NOT NULL,
    operation TEXT NOT NULL,
    error_category TEXT NOT NULL,
    failed_requests INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY(tokenizer, operation, error_category)
);
CREATE TABLE IF NOT EXISTS legacy_primary_imports (
    source_database TEXT PRIMARY KEY
);
"#;

/// Best-effort process instrumentation isolated from repository generations.
#[derive(Clone)]
pub(crate) struct InstrumentationStorage {
    pub(super) writer: Arc<Mutex<Connection>>,
}

impl fmt::Debug for InstrumentationStorage {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("InstrumentationStorage")
            .finish_non_exhaustive()
    }
}

impl InstrumentationStorage {
    pub(crate) fn open(path: &Path) -> Self {
        match Self::open_connection(path) {
            Ok(connection) => Self {
                writer: Arc::new(Mutex::new(connection)),
            },
            Err(error) => {
                tracing::warn!(%error, path = %path.display(), "instrumentation storage unavailable; using process-local memory");
                let connection = Self::open_memory()
                    .expect("bundled SQLite must support the instrumentation schema");
                Self {
                    writer: Arc::new(Mutex::new(connection)),
                }
            }
        }
    }

    fn open_connection(path: &Path) -> Result<Connection> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let connection = Connection::open(path)?;
        connection.busy_timeout(DEFAULT_BUSY_TIMEOUT)?;
        connection.execute_batch(INSTRUMENTATION_SCHEMA)?;
        Ok(connection)
    }

    fn open_memory() -> Result<Connection> {
        let connection = Connection::open_in_memory()?;
        connection.busy_timeout(DEFAULT_BUSY_TIMEOUT)?;
        connection.execute_batch(INSTRUMENTATION_SCHEMA)?;
        Ok(connection)
    }

    pub(crate) fn snapshot(
        &self,
        tokenizer: &str,
    ) -> Result<(
        HashMap<String, TokenSavingsRecord>,
        Vec<ServiceFailureRecord>,
    )> {
        let connection = self
            .writer
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        connection.execute_batch("BEGIN DEFERRED")?;
        let result = (|| {
            Ok((
                query_token_savings(&connection, tokenizer)?,
                query_service_failures(&connection, tokenizer)?,
            ))
        })();
        let rollback = connection.execute_batch("ROLLBACK");
        result.and_then(|value| rollback.map(|()| value).map_err(Into::into))
    }

    /// Preserve pre-sidecar accounting before the index migration drops its
    /// former tables. The ledger makes a restart between copy and migration
    /// idempotent.
    pub(crate) fn migrate_legacy_primary(&self, path: &Path) -> Result<()> {
        if !path.exists() {
            return Ok(());
        }
        let source_database = path.to_string_lossy().into_owned();
        let connection = self
            .writer
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        connection.execute("ATTACH DATABASE ?1 AS legacy", params![&source_database])?;
        let result = (|| {
            let has_legacy_token_savings = connection.query_row(
                "SELECT EXISTS(
                    SELECT 1 FROM legacy.sqlite_schema
                    WHERE type = 'table' AND name = 'token_savings'
                )",
                [],
                |row| row.get::<_, bool>(0),
            )?;
            let has_legacy_failures = connection.query_row(
                "SELECT EXISTS(
                    SELECT 1 FROM legacy.sqlite_schema
                    WHERE type = 'table' AND name = 'service_failures'
                )",
                [],
                |row| row.get::<_, bool>(0),
            )?;
            if !has_legacy_token_savings || !has_legacy_failures {
                return Ok(());
            }
            let already_imported = connection.query_row(
                "SELECT EXISTS(
                    SELECT 1 FROM legacy_primary_imports WHERE source_database = ?1
                )",
                params![source_database],
                |row| row.get::<_, bool>(0),
            )?;
            if already_imported {
                return Ok(());
            }

            connection.execute_batch("BEGIN IMMEDIATE")?;
            let imported = (|| {
                connection.execute_batch(
                    "INSERT OR IGNORE INTO token_savings (
                        tokenizer, operation, tracked_requests,
                        response_tracked_requests, response_baseline_requests,
                        baseline_source_tokens, response_baseline_source_tokens,
                        emitted_source_tokens, estimated_source_tokens_saved,
                        response_source_tokens, path_and_metadata_tokens,
                        protocol_tokens, total_response_tokens,
                        receipt_suppressed_exact, receipt_suppressed_overlap,
                        expected_hash_not_modified_responses,
                        expected_hash_suppressed_source_tokens, useful_requests,
                        incomplete_requests, unsupported_requests,
                        hash_suppressed_requests
                    )
                    SELECT tokenizer, operation, tracked_requests,
                           response_tracked_requests, response_baseline_requests,
                           baseline_source_tokens, response_baseline_source_tokens,
                           emitted_source_tokens, estimated_source_tokens_saved,
                           response_source_tokens, path_and_metadata_tokens,
                           protocol_tokens, total_response_tokens,
                           receipt_suppressed_exact, receipt_suppressed_overlap,
                           expected_hash_not_modified_responses,
                           expected_hash_suppressed_source_tokens, useful_requests,
                           incomplete_requests, unsupported_requests,
                           hash_suppressed_requests
                    FROM legacy.token_savings;

                    INSERT OR IGNORE INTO service_failures (
                        tokenizer, operation, error_category, failed_requests
                    )
                    SELECT tokenizer, operation, error_category, failed_requests
                    FROM legacy.service_failures;",
                )?;
                connection.execute(
                    "INSERT INTO legacy_primary_imports(source_database) VALUES (?1)",
                    params![source_database],
                )?;
                Ok(())
            })();
            match imported {
                Ok(()) => connection.execute_batch("COMMIT")?,
                Err(error) => {
                    let _ = connection.execute_batch("ROLLBACK");
                    return Err(error);
                }
            }
            Ok(())
        })();
        let detach = connection.execute_batch("DETACH DATABASE legacy");
        result.and_then(|()| detach.map_err(Into::into))
    }
}

fn query_token_savings(
    connection: &Connection,
    tokenizer: &str,
) -> Result<HashMap<String, TokenSavingsRecord>> {
    let mut statement = connection.prepare_cached(
        "SELECT operation, tracked_requests, response_tracked_requests,
                response_baseline_requests, baseline_source_tokens,
                response_baseline_source_tokens, emitted_source_tokens,
                estimated_source_tokens_saved, response_source_tokens,
                path_and_metadata_tokens, protocol_tokens, total_response_tokens,
                receipt_suppressed_exact, receipt_suppressed_overlap,
                expected_hash_not_modified_responses,
                expected_hash_suppressed_source_tokens, useful_requests,
                incomplete_requests, unsupported_requests, hash_suppressed_requests
         FROM token_savings WHERE tokenizer = ?1 ORDER BY operation",
    )?;
    let rows = statement.query_map(params![tokenizer], |row| {
        Ok((
            row.get::<_, String>(0)?,
            TokenSavingsRecord {
                tracked_requests: i64_to_u64(row.get(1)?)?,
                response_tracked_requests: i64_to_u64(row.get(2)?)?,
                response_baseline_requests: i64_to_u64(row.get(3)?)?,
                baseline_source_tokens: i64_to_u64(row.get(4)?)?,
                response_baseline_source_tokens: i64_to_u64(row.get(5)?)?,
                emitted_source_tokens: i64_to_u64(row.get(6)?)?,
                estimated_source_tokens_saved: i64_to_u64(row.get(7)?)?,
                response_source_tokens: i64_to_u64(row.get(8)?)?,
                path_and_metadata_tokens: i64_to_u64(row.get(9)?)?,
                protocol_tokens: i64_to_u64(row.get(10)?)?,
                total_response_tokens: i64_to_u64(row.get(11)?)?,
                receipt_suppressed_exact: i64_to_u64(row.get(12)?)?,
                receipt_suppressed_overlap: i64_to_u64(row.get(13)?)?,
                expected_hash_not_modified_responses: i64_to_u64(row.get(14)?)?,
                expected_hash_suppressed_source_tokens: i64_to_u64(row.get(15)?)?,
                useful_requests: i64_to_u64(row.get(16)?)?,
                incomplete_requests: i64_to_u64(row.get(17)?)?,
                unsupported_requests: i64_to_u64(row.get(18)?)?,
                hash_suppressed_requests: i64_to_u64(row.get(19)?)?,
            },
        ))
    })?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}

fn query_service_failures(
    connection: &Connection,
    tokenizer: &str,
) -> Result<Vec<ServiceFailureRecord>> {
    let mut statement = connection.prepare_cached(
        "SELECT operation, error_category, failed_requests
         FROM service_failures WHERE tokenizer = ?1 ORDER BY operation, error_category",
    )?;
    let rows = statement.query_map(params![tokenizer], |row| {
        Ok(ServiceFailureRecord {
            operation: row.get(0)?,
            error_category: row.get(1)?,
            failed_requests: i64_to_u64(row.get(2)?)?,
        })
    })?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}
