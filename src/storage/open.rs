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
            |conn| MIGRATIONS.to_latest(conn).map_err(Into::into),
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

    /// Verify persisted FTS5 indexes against their relational tables.
    ///
    /// A database can pass migrations, integrity_check, and the FTS5
    /// capability probe while FTS silently omits results. This function
    /// checks that each FTS table matches its external content. Corruption is
    /// returned to the cache owner, which discards a managed projection as a
    /// unit; storage never heals one derived table in place.
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

        for fts_table in FTS_TABLES {
            conn.execute(
                &format!("INSERT INTO {fts_table}({fts_table}, rank) VALUES('integrity-check', 1)"),
                [],
            )?;
        }
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
