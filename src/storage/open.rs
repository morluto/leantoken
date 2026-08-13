use std::time::Instant;

fn validate_repository_binding_values(
    database: &Path,
    expected_repository: &str,
    expected_identity: &str,
    repository_root: &Path,
    index_scope_digest: Option<&str>,
) -> Result<()> {
    let actual_repository = repository_root.to_path_buf();
    let actual_display = repository_root.to_string_lossy();
    let actual_identity = repository_identity(repository_root, index_scope_digest);
    if expected_identity.is_empty() {
        if !expected_repository.is_empty() && expected_repository != actual_display {
            return Err(Error::RepositoryMismatch {
                database: database.to_path_buf(),
                expected_repository: expected_repository.to_owned(),
                actual_repository,
            });
        }
        return Ok(());
    }
    if expected_identity != actual_identity {
        return if expected_repository == actual_display {
            Err(Error::IndexScopeMismatch {
                database: database.to_path_buf(),
            })
        } else {
            Err(Error::RepositoryMismatch {
                database: database.to_path_buf(),
                expected_repository: expected_repository.to_owned(),
                actual_repository,
            })
        };
    }
    Ok(())
}

/// Reject an already-bound foreign LeanToken cache before startup pragmas,
/// migrations, projection repair, or checkpoint behavior can mutate it.
fn verify_repository_binding_before_mutation(
    conn: &Connection,
    database: &Path,
    repository_root: &Path,
    index_scope_digest: Option<&str>,
) -> Result<()> {
    if !table_exists(conn, StorageTable::Meta)?
        || !column_exists(conn, StorageColumn::MetaRepositoryRoot)?
    {
        return Ok(());
    }
    let expected_repository =
        conn.query_row("SELECT repository_root FROM meta WHERE id = 1", [], |row| {
            row.get::<_, String>(0)
        })?;
    let expected_identity = if column_exists(conn, StorageColumn::MetaRepositoryIdentity)? {
        conn.query_row(
            "SELECT repository_identity FROM meta WHERE id = 1",
            [],
            |row| row.get::<_, String>(0),
        )?
    } else {
        String::new()
    };
    validate_repository_binding_values(
        database,
        &expected_repository,
        &expected_identity,
        repository_root,
        index_scope_digest,
    )
}

fn bind_repository_connection(
    conn: &mut Connection,
    database: &Path,
    repository_root: &Path,
    index_scope_digest: Option<&str>,
    accessed_at: i64,
) -> Result<()> {
    let actual_display = repository_root.to_string_lossy();
    let actual_identity = repository_identity(repository_root, index_scope_digest);
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let (expected_repository, expected_identity): (String, String) = tx.query_row(
        "SELECT repository_root, repository_identity FROM meta WHERE id = 1",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    validate_repository_binding_values(
        database,
        &expected_repository,
        &expected_identity,
        repository_root,
        index_scope_digest,
    )?;
    if expected_identity.is_empty() {
        tx.execute(
            "UPDATE meta
             SET repository_root = ?1,
                 repository_identity = ?2,
                 last_access_unix_seconds = ?3
             WHERE id = 1",
            params![actual_display.as_ref(), actual_identity, accessed_at],
        )?;
    } else {
        tx.execute(
            "UPDATE meta SET last_access_unix_seconds = ?1 WHERE id = 1",
            params![accessed_at],
        )?;
    }
    tx.commit()?;
    Ok(())
}

impl Storage {
    /// Open or migrate a SQLite index without binding it to a repository root.
    ///
    /// Application code should normally construct [`crate::services::Services`],
    /// which also verifies repository ownership. This lower-level constructor is
    /// useful for storage tests and tools that deliberately manage that invariant.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        Self::open_with_startup_timeout(path, DEFAULT_BUSY_TIMEOUT)
    }

    /// Read status from an existing cache without running migrations, changing
    /// SQLite pragmas, or binding the cache to a repository.
    pub(crate) fn read_only_status_scoped(
        path: &Path,
        repository_root: &Path,
        index_scope_digest: Option<&str>,
    ) -> Result<ReadOnlyStatusSnapshot> {
        let conn = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        conn.busy_timeout(READ_ONLY_STATUS_BUSY_TIMEOUT)?;
        conn.execute_batch("BEGIN DEFERRED")?;

        if !table_exists(&conn, StorageTable::Meta)? {
            return Ok(ReadOnlyStatusSnapshot {
                generation: 0,
                derivation_fingerprint: None,
                counts: StorageCounts {
                    files: 0,
                    chunks: 0,
                    symbols: 0,
                    source_bytes: 0,
                    languages: Vec::new(),
                },
            });
        }

        let has_repository_root = column_exists(&conn, StorageColumn::MetaRepositoryRoot)?;
        let has_repository_identity = column_exists(&conn, StorageColumn::MetaRepositoryIdentity)?;
        let expected_repository = if has_repository_root {
            conn.query_row("SELECT repository_root FROM meta WHERE id = 1", [], |row| {
                row.get::<_, String>(0)
            })?
        } else {
            String::new()
        };
        let expected_identity = if has_repository_identity {
            conn.query_row(
                "SELECT repository_identity FROM meta WHERE id = 1",
                [],
                |row| row.get::<_, String>(0),
            )?
        } else {
            String::new()
        };
        let actual_identity = repository_identity(repository_root, index_scope_digest);
        let actual_repository = repository_root.to_string_lossy();
        let mismatched_identity =
            !expected_identity.is_empty() && expected_identity != actual_identity;
        let mismatched_unversioned_root = expected_identity.is_empty()
            && !expected_repository.is_empty()
            && expected_repository != actual_repository;
        if mismatched_identity && expected_repository == actual_repository {
            return Err(Error::IndexScopeMismatch {
                database: path.to_path_buf(),
            });
        }
        if mismatched_identity || mismatched_unversioned_root {
            return Err(Error::RepositoryMismatch {
                database: path.to_path_buf(),
                expected_repository,
                actual_repository: repository_root.to_path_buf(),
            });
        }

        let generation = if column_exists(&conn, StorageColumn::MetaRepositoryGeneration)? {
            i64_to_u64(conn.query_row(
                "SELECT repository_generation FROM meta WHERE id = 1",
                [],
                |row| row.get::<_, i64>(0),
            )?)?
        } else {
            0
        };
        let derivation_fingerprint =
            if column_exists(&conn, StorageColumn::MetaDerivationFingerprint)? {
                let fingerprint = conn.query_row(
                    "SELECT derivation_fingerprint FROM meta WHERE id = 1",
                    [],
                    |row| row.get::<_, String>(0),
                )?;
                (!fingerprint.is_empty()).then_some(fingerprint)
            } else {
                None
            };
        let files = count_table_rows(&conn, StorageTable::Files)?;
        let chunks = count_table_rows(&conn, StorageTable::Chunks)?;
        let symbols = count_table_rows(&conn, StorageTable::Symbols)?;
        let source_bytes = if table_exists(&conn, StorageTable::Files)?
            && column_exists(&conn, StorageColumn::FilesSizeBytes)?
        {
            i64_to_u64(conn.query_row(
                "SELECT coalesce(sum(size_bytes), 0) FROM files",
                [],
                |row| row.get::<_, i64>(0),
            )?)?
        } else {
            0
        };
        let languages = if table_exists(&conn, StorageTable::Files)? {
            let mut statement = conn.prepare(
                "SELECT language, count(*) FROM files WHERE language IS NOT NULL GROUP BY language ORDER BY language",
            )?;
            statement
                .query_map([], |row| Ok((row.get(0)?, i64_to_usize(row.get(1)?)?)))?
                .collect::<std::result::Result<Vec<_>, _>>()?
        } else {
            Vec::new()
        };

        Ok(ReadOnlyStatusSnapshot {
            generation,
            derivation_fingerprint,
            counts: StorageCounts {
                files,
                chunks,
                symbols,
                source_bytes,
                languages,
            },
        })
    }

    pub(crate) fn open_with_startup_timeout(
        path: impl AsRef<Path>,
        startup_timeout: Duration,
    ) -> Result<Self> {
        Self::open_with_startup_timeout_and_read_capacity(
            path,
            startup_timeout,
            default_read_connection_capacity(),
        )
    }

    fn open_with_startup_timeout_and_read_capacity(
        path: impl AsRef<Path>,
        startup_timeout: Duration,
        read_connection_capacity: u32,
    ) -> Result<Self> {
        Self::open_with_startup_timeout_read_capacity_and_binding(
            path,
            startup_timeout,
            read_connection_capacity,
            None,
        )
    }

    fn open_with_startup_timeout_read_capacity_and_binding(
        path: impl AsRef<Path>,
        startup_timeout: Duration,
        read_connection_capacity: u32,
        repository_binding: Option<(&Path, Option<&str>)>,
    ) -> Result<Self> {
        if read_connection_capacity == 0 {
            return Err(Error::InvalidConfiguration(
                "read connection capacity must be positive".into(),
            ));
        }
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut conn = Connection::open(&path)?;
        if let Some((repository_root, index_scope_digest)) = repository_binding {
            verify_repository_binding_before_mutation(
                &conn,
                &path,
                repository_root,
                index_scope_digest,
            )?;
        }
        Self::configure(&mut conn, startup_timeout)?;
        with_auto_checkpoint_suspended(
            &mut conn,
            AutoCheckpointCompletion::CheckpointIfMutated,
            |conn| {
                MIGRATIONS.to_latest(conn)?;
                Self::ensure_token_savings_schema(conn)
            },
        )?;
        if let Some((repository_root, index_scope_digest)) = repository_binding {
            with_auto_checkpoint_suspended(
                &mut conn,
                AutoCheckpointCompletion::RestoreOnly,
                |conn| {
                    bind_repository_connection(
                        conn,
                        &path,
                        repository_root,
                        index_scope_digest,
                        unix_seconds(SystemTime::now()),
                    )
                },
            )?;
        }
        with_auto_checkpoint_suspended(
            &mut conn,
            AutoCheckpointCompletion::CheckpointIfMutated,
            |conn| {
                Self::ensure_path_projection(conn)?;
                Self::ensure_quota_usage_projections(conn)
            },
        )?;
        Self::validate_fts5(&mut conn)?;
        with_auto_checkpoint_suspended(
            &mut conn,
            AutoCheckpointCompletion::RestoreOnly,
            Self::verify_fts_integrity,
        )?;
        conn.busy_timeout(DEFAULT_BUSY_TIMEOUT)?;

        let manager = SqliteConnectionManager::file(&path)
            .with_flags(OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX)
            .with_init(|connection| {
                connection.busy_timeout(DEFAULT_BUSY_TIMEOUT)?;
                connection.pragma_update(None, "foreign_keys", "ON")
            });
        let readers = r2d2::Pool::builder()
            .max_size(read_connection_capacity)
            .connection_timeout(DEFAULT_BUSY_TIMEOUT)
            .test_on_check_out(false)
            .build(manager)?;

        Ok(Self {
            writer: Arc::new(Mutex::new(conn)),
            readers,
            path,
            #[cfg(test)]
            diagnostics: Arc::new(StorageDiagnostics::default()),
        })
    }

    #[cfg(test)]
    pub(crate) fn open_for_repository_scoped(
        path: impl AsRef<Path>,
        repository_root: &Path,
        index_scope_digest: Option<&str>,
    ) -> Result<Self> {
        Self::open_for_repository_scoped_with_startup_timeout(
            path,
            repository_root,
            index_scope_digest,
            DEFAULT_BUSY_TIMEOUT,
        )
    }

    #[cfg(test)]
    pub(crate) fn open_for_repository_scoped_with_startup_timeout(
        path: impl AsRef<Path>,
        repository_root: &Path,
        index_scope_digest: Option<&str>,
        startup_timeout: Duration,
    ) -> Result<Self> {
        Self::open_with_startup_timeout_read_capacity_and_binding(
            path,
            startup_timeout,
            default_read_connection_capacity(),
            Some((repository_root, index_scope_digest)),
        )
    }

    pub(crate) fn open_for_repository_scoped_with_runtime_limits(
        path: impl AsRef<Path>,
        repository_root: &Path,
        index_scope_digest: Option<&str>,
        startup_timeout: Duration,
        read_connection_capacity: u32,
    ) -> Result<Self> {
        Self::open_with_startup_timeout_read_capacity_and_binding(
            path,
            startup_timeout,
            read_connection_capacity,
            Some((repository_root, index_scope_digest)),
        )
    }

    #[cfg(test)]
    pub(crate) fn bind_repository_at(
        &self,
        repository_root: &Path,
        index_scope_digest: Option<&str>,
        accessed_at: i64,
    ) -> Result<()> {
        let actual_repository = repository_root.to_path_buf();
        let mut conn = self
            .writer
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        with_auto_checkpoint_suspended(&mut conn, AutoCheckpointCompletion::RestoreOnly, |conn| {
            bind_repository_connection(
                conn,
                &self.path,
                &actual_repository,
                index_scope_digest,
                accessed_at,
            )
        })
    }

    pub(crate) fn configure(conn: &mut Connection, startup_timeout: Duration) -> Result<()> {
        conn.busy_timeout(startup_timeout)?;
        let started = Instant::now();
        conn.pragma_update_and_check(None, "journal_mode", "WAL", |_| Ok(()))?;
        tracing::debug!(
            pragma = "journal_mode",
            elapsed_us = started.elapsed().as_micros(),
            "storage startup pragma completed"
        );
        let started = Instant::now();
        conn.pragma_update(None, "journal_size_limit", WAL_JOURNAL_SIZE_LIMIT_BYTES)?;
        tracing::debug!(
            pragma = "journal_size_limit",
            elapsed_us = started.elapsed().as_micros(),
            "storage startup pragma completed"
        );
        let started = Instant::now();
        conn.pragma_update(None, "foreign_keys", "ON")?;
        tracing::debug!(
            pragma = "foreign_keys",
            elapsed_us = started.elapsed().as_micros(),
            "storage startup pragma completed"
        );
        Ok(())
    }

    pub(crate) fn validate_fts5(conn: &mut Connection) -> Result<()> {
        let probe = "leantoken_fts5_probe";
        conn.execute(
            &format!("CREATE VIRTUAL TABLE temp.{probe} USING fts5(text, tokenize='trigram')"),
            [],
        )
        .map_err(|source| Error::RuntimeCapabilityUnavailable {
            capability: "SQLite FTS5 with the trigram tokenizer",
            source: Some(source),
        })?;
        conn.execute(
            &format!("INSERT INTO temp.{probe}(text) VALUES (?1)"),
            params!["abc"],
        )?;
        let mut stmt = conn.prepare(&format!(
            "SELECT 1 FROM temp.{probe} WHERE {probe} MATCH ?1"
        ))?;
        let matched = stmt.exists(params!["\"abc\""])?;
        drop(stmt);
        conn.execute(&format!("DROP TABLE temp.{probe}"), [])?;
        if matched {
            Ok(())
        } else {
            Err(Error::RuntimeCapabilityUnavailable {
                capability: "SQLite FTS5 with a working trigram tokenizer",
                source: None,
            })
        }
    }

    /// Verify persisted FTS5 indexes against their relational tables.
    ///
    /// A database can pass migrations, integrity_check, and the FTS5
    /// capability probe while FTS silently omits results. This function
    /// checks that each FTS table matches its external content and issues a
    /// `rebuild` command if it does not. This runs on every database open:
    /// external writers can damage an index without changing LeanToken's
    /// generation marker.
    ///
    /// See issue #563: Validate and repair external-content FTS indexes
    /// instead of probing only FTS5 availability.
    pub(crate) fn verify_fts_integrity(conn: &mut Connection) -> Result<()> {
        // Use FTS5's built-in integrity-check command. For external-content
        // tables, `SELECT count(*)` reads through the content table and always
        // matches, even when the FTS index is corrupted. The integrity-check
        // command verifies that the FTS index postings agree with the content
        // table and fails with SQLITE_CORRUPT_VTAB when they do not.
        // See issue #563.
        const FTS_TABLES: &[&str] = &[
            "chunks_fts_word",
            "chunks_fts_trigram",
            "symbols_fts_trigram",
            "symbol_refs_fts_trigram",
        ];

        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        for fts_table in FTS_TABLES {
            match tx.execute(
                &format!("INSERT INTO {fts_table}({fts_table}, rank) VALUES('integrity-check', 1)"),
                [],
            ) {
                Ok(_) => {}
                Err(rusqlite::Error::SqliteFailure(error, _))
                    if error.extended_code == rusqlite::ffi::SQLITE_CORRUPT_VTAB =>
                {
                    tracing::warn!(fts_table, "FTS index integrity check failed; rebuilding");
                    tx.execute(
                        &format!("INSERT INTO {fts_table}({fts_table}) VALUES('rebuild')"),
                        [],
                    )?;
                }
                Err(error) => return Err(error.into()),
            }
        }
        tx.commit()?;
        Ok(())
    }

    pub(crate) fn ensure_token_savings_schema(conn: &mut Connection) -> Result<()> {
        // These additive fields are intentionally outside the numbered cache
        // schema so older LeanToken versions can still open and rebuild it.
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let columns = {
            let mut stmt = tx.prepare("PRAGMA table_info(files)")?;
            stmt.query_map([], |row| row.get::<_, String>(1))?
                .collect::<std::result::Result<HashSet<_>, _>>()?
        };
        if !columns.contains("source_token_count") {
            tx.execute_batch(
                "ALTER TABLE files ADD COLUMN source_token_count INTEGER NOT NULL DEFAULT 0;",
            )?;
        }
        if !columns.contains("source_tokenizer") {
            tx.execute_batch(
                "ALTER TABLE files ADD COLUMN source_tokenizer TEXT NOT NULL DEFAULT '';",
            )?;
        }
        tx.execute_batch(TOKEN_SAVINGS_TABLE_SQL)?;
        let savings_columns = {
            let mut stmt = tx.prepare("PRAGMA table_info(token_savings)")?;
            stmt.query_map([], |row| row.get::<_, String>(1))?
                .collect::<std::result::Result<HashSet<_>, _>>()?
        };
        for (column, statement) in [
            (
                "response_tracked_requests",
                "ALTER TABLE token_savings ADD COLUMN response_tracked_requests INTEGER NOT NULL DEFAULT 0;",
            ),
            (
                "response_baseline_requests",
                "ALTER TABLE token_savings ADD COLUMN response_baseline_requests INTEGER NOT NULL DEFAULT 0;",
            ),
            (
                "response_baseline_source_tokens",
                "ALTER TABLE token_savings ADD COLUMN response_baseline_source_tokens INTEGER NOT NULL DEFAULT 0;",
            ),
            (
                "response_source_tokens",
                "ALTER TABLE token_savings ADD COLUMN response_source_tokens INTEGER NOT NULL DEFAULT 0;",
            ),
            (
                "path_and_metadata_tokens",
                "ALTER TABLE token_savings ADD COLUMN path_and_metadata_tokens INTEGER NOT NULL DEFAULT 0;",
            ),
            (
                "protocol_tokens",
                "ALTER TABLE token_savings ADD COLUMN protocol_tokens INTEGER NOT NULL DEFAULT 0;",
            ),
            (
                "total_response_tokens",
                "ALTER TABLE token_savings ADD COLUMN total_response_tokens INTEGER NOT NULL DEFAULT 0;",
            ),
            (
                "receipt_suppressed_exact",
                "ALTER TABLE token_savings ADD COLUMN receipt_suppressed_exact INTEGER NOT NULL DEFAULT 0;",
            ),
            (
                "receipt_suppressed_overlap",
                "ALTER TABLE token_savings ADD COLUMN receipt_suppressed_overlap INTEGER NOT NULL DEFAULT 0;",
            ),
            (
                "expected_hash_not_modified_responses",
                "ALTER TABLE token_savings ADD COLUMN expected_hash_not_modified_responses INTEGER NOT NULL DEFAULT 0;",
            ),
            (
                "expected_hash_suppressed_source_tokens",
                "ALTER TABLE token_savings ADD COLUMN expected_hash_suppressed_source_tokens INTEGER NOT NULL DEFAULT 0;",
            ),
            (
                "useful_requests",
                "ALTER TABLE token_savings ADD COLUMN useful_requests INTEGER NOT NULL DEFAULT 0;",
            ),
            (
                "incomplete_requests",
                "ALTER TABLE token_savings ADD COLUMN incomplete_requests INTEGER NOT NULL DEFAULT 0;",
            ),
            (
                "unsupported_requests",
                "ALTER TABLE token_savings ADD COLUMN unsupported_requests INTEGER NOT NULL DEFAULT 0;",
            ),
            (
                "hash_suppressed_requests",
                "ALTER TABLE token_savings ADD COLUMN hash_suppressed_requests INTEGER NOT NULL DEFAULT 0;",
            ),
        ] {
            if !savings_columns.contains(column) {
                tx.execute_batch(statement)?;
            }
        }
        tx.execute_batch(SERVICE_FAILURES_TABLE_SQL)?;
        tx.commit()?;
        Ok(())
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum AutoCheckpointCompletion {
    RestoreOnly,
    CheckpointIfMutated,
}

impl AutoCheckpointCompletion {
    const fn checkpoints_mutations(self) -> bool {
        matches!(self, Self::CheckpointIfMutated)
    }
}

pub(super) fn with_auto_checkpoint_suspended<T>(
    conn: &mut Connection,
    completion: AutoCheckpointCompletion,
    operation: impl FnOnce(&mut Connection) -> Result<T>,
) -> Result<T> {
    let total_changes_before = completion
        .checkpoints_mutations()
        .then(|| conn.total_changes());
    let schema_version_before = completion
        .checkpoints_mutations()
        .then(|| conn.query_row("PRAGMA schema_version", [], |row| row.get::<_, i64>(0)))
        .transpose()?;
    let previous = conn.query_row("PRAGMA wal_autocheckpoint", [], |row| row.get::<_, i64>(0))?;
    if previous != 0 {
        conn.pragma_update(None, "wal_autocheckpoint", 0)?;
    }
    let operation_result = operation(conn);
    let restoration_result = if previous != 0 {
        conn.pragma_update(None, "wal_autocheckpoint", previous)
            .map_err(Error::from)
    } else {
        Ok(())
    };

    let output = match operation_result {
        Ok(output) => {
            restoration_result?;
            output
        }
        Err(operation_error) => {
            if let Err(restoration_error) = restoration_result {
                tracing::warn!(
                    %restoration_error,
                    "failed to restore wal_autocheckpoint after an operation error"
                );
            }
            return Err(operation_error);
        }
    };

    let schema_version_after = completion
        .checkpoints_mutations()
        .then(|| conn.query_row("PRAGMA schema_version", [], |row| row.get::<_, i64>(0)))
        .transpose()?;
    let main_database_changed = total_changes_before
        .zip(schema_version_before)
        .zip(schema_version_after)
        .is_some_and(|((changes, schema), current_schema)| {
            conn.total_changes() != changes || current_schema != schema
        });
    let checkpoint = if main_database_changed {
        conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()))
            .map_err(Error::from)
    } else {
        Ok(())
    };
    checkpoint?;
    Ok(output)
}

use super::*;
