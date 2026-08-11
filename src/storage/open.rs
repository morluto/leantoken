use std::time::Instant;

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
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut conn = Connection::open(&path)?;
        Self::configure(&mut conn, startup_timeout)?;
        with_auto_checkpoint_suspended(
            &mut conn,
            AutoCheckpointCompletion::CheckpointIfMutated,
            |conn| {
                MIGRATIONS.to_latest(conn)?;
                Self::ensure_token_savings_schema(conn)?;
                Self::ensure_path_projection(conn)
            },
        )?;
        Self::validate_fts5(&mut conn)?;
        Self::verify_fts_integrity(&mut conn)?;
        conn.busy_timeout(DEFAULT_BUSY_TIMEOUT)?;

        let manager = SqliteConnectionManager::file(&path)
            .with_flags(OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX)
            .with_init(|connection| {
                connection.busy_timeout(DEFAULT_BUSY_TIMEOUT)?;
                connection.pragma_update(None, "foreign_keys", "ON")
            });
        let readers = r2d2::Pool::builder()
            .max_size(default_read_connection_capacity())
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

    pub(crate) fn open_for_repository_scoped_with_startup_timeout(
        path: impl AsRef<Path>,
        repository_root: &Path,
        index_scope_digest: Option<&str>,
        startup_timeout: Duration,
    ) -> Result<Self> {
        let storage = Self::open_with_startup_timeout(path, startup_timeout)?;
        storage.bind_repository(repository_root, index_scope_digest)?;
        Ok(storage)
    }

    pub(crate) fn bind_repository(
        &self,
        repository_root: &Path,
        index_scope_digest: Option<&str>,
    ) -> Result<()> {
        self.bind_repository_at(
            repository_root,
            index_scope_digest,
            unix_seconds(SystemTime::now()),
        )
    }

    pub(crate) fn bind_repository_at(
        &self,
        repository_root: &Path,
        index_scope_digest: Option<&str>,
        accessed_at: i64,
    ) -> Result<()> {
        let actual_repository = repository_root.to_path_buf();
        let actual_display = repository_root.to_string_lossy();
        let actual_identity = repository_identity(repository_root, index_scope_digest);
        let mut conn = self
            .writer
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        with_auto_checkpoint_suspended(&mut conn, AutoCheckpointCompletion::RestoreOnly, |conn| {
            let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let (expected_repository, expected_identity): (String, String) = tx.query_row(
                "SELECT repository_root, repository_identity FROM meta WHERE id = 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?;

            if expected_identity.is_empty() {
                tx.execute(
                    "UPDATE meta SET repository_root = ?1, repository_identity = ?2, last_access_unix_seconds = ?3 WHERE id = 1",
                    params![actual_display.as_ref(), actual_identity, accessed_at],
                )?;
                tx.commit()?;
                return Ok(());
            }
            if expected_identity != actual_identity {
                if expected_repository == actual_display {
                    return Err(Error::IndexScopeMismatch {
                        database: self.path.clone(),
                    });
                }
                return Err(Error::RepositoryMismatch {
                    database: self.path.clone(),
                    expected_repository,
                    actual_repository,
                });
            }

            tx.execute(
                "UPDATE meta SET last_access_unix_seconds = ?1 WHERE id = 1",
                params![accessed_at],
            )?;
            tx.commit()?;
            Ok(())
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

    /// Verify that persisted FTS5 indexes agree with their relational tables.
    ///
    /// A database can pass migrations, integrity_check, and the FTS5
    /// capability probe while FTS silently omits results. This function
    /// checks that each FTS table has a row count matching its relational
    /// counterpart and issues a `rebuild` command if they diverge.
    ///
    /// See issue #563: Validate and repair external-content FTS indexes
    /// instead of probing only FTS5 availability.
    pub(crate) fn verify_fts_integrity(conn: &mut Connection) -> Result<()> {
        // Use FTS5's built-in integrity-check command. For external-content
        // tables, `SELECT count(*)` reads through the content table and always
        // matches, even when the FTS index is corrupted. The integrity-check
        // command verifies that the FTS index postings agree with the content
        // table and returns one row per inconsistency (0 rows = healthy).
        // See issue #563.
        const FTS_TABLES: &[&str] = &[
            "chunks_fts_word",
            "chunks_fts_trigram",
            "symbols_fts_trigram",
            "symbol_refs_fts_trigram",
        ];
        for fts_table in FTS_TABLES {
            let mut stmt = conn.prepare(&format!(
                "SELECT 1 FROM {fts_table} WHERE {fts_table} = 'integrity-check' LIMIT 1"
            ))?;
            let has_errors = stmt.exists([])?;
            drop(stmt);
            if has_errors {
                tracing::warn!(fts_table, "FTS index integrity check failed; rebuilding");
                conn.execute(
                    &format!("INSERT INTO {fts_table}({fts_table}) VALUES('rebuild')"),
                    [],
                )?;
            }
        }
        Ok(())
    }

    pub(crate) fn ensure_path_projection(conn: &mut Connection) -> Result<()> {
        let file_count: i64 = conn.query_row("SELECT count(*) FROM files", [], |row| row.get(0))?;
        let projected_files: i64 = conn.query_row(
            "SELECT count(*) FROM path_entries WHERE kind = 1",
            [],
            |row| row.get(0),
        )?;
        if file_count == projected_files {
            return Ok(());
        }
        let paths = {
            let mut stmt = conn.prepare("SELECT id, path FROM files ORDER BY id")?;
            stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
                .collect::<std::result::Result<Vec<(i64, String)>, _>>()?
        };
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute("DELETE FROM path_entries", [])?;
        for (file_id, path) in paths {
            Self::insert_path_projection(&tx, &path, file_id)?;
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
enum AutoCheckpointCompletion {
    RestoreOnly,
    CheckpointIfMutated,
}

impl AutoCheckpointCompletion {
    const fn checkpoints_mutations(self) -> bool {
        matches!(self, Self::CheckpointIfMutated)
    }
}

fn with_auto_checkpoint_suspended<T>(
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
    let result = operation(conn);
    let restore = if previous != 0 {
        conn.pragma_update(None, "wal_autocheckpoint", previous)
            .map_err(Error::from)
    } else {
        Ok(())
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
    match result {
        Err(error) => Err(error),
        Ok(output) => {
            restore?;
            checkpoint?;
            Ok(output)
        }
    }
}
use super::*;
