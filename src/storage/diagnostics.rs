pub(crate) const MAX_READ_CONNECTIONS: u32 = 8;

// SQLite normally recycles a completed WAL without shrinking it. Retain four
// default auto-checkpoint windows for reuse while bounding the steady-state
// disk footprint after a large initial publication.
const WAL_JOURNAL_SIZE_LIMIT_BYTES: i64 = 16 * 1024 * 1024;

pub(crate) const CURRENT_SCHEMA_VERSION: i64 = 8;

/// Default row limit used by callers that do not provide a tighter bound.
pub const DEFAULT_MAX_RESULTS: usize = 100;
/// Absolute row limit applied by storage queries, including internal batch reads.
pub const HARD_MAX_RESULTS: usize = 10_000;

const DEFAULT_BUSY_TIMEOUT: Duration = Duration::from_millis(5_000);
const READ_ONLY_STATUS_BUSY_TIMEOUT: Duration = Duration::from_millis(100);

pub(crate) fn process_write_bytes() -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        fs::read_to_string("/proc/self/io")
            .ok()?
            .lines()
            .find_map(|line| line.strip_prefix("write_bytes: "))
            .and_then(|value| value.trim().parse().ok())
    }
    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}

fn measured_storage_phase<T>(
    enabled: bool,
    operation: impl FnOnce() -> Result<T>,
) -> Result<(T, f64, Option<u64>)> {
    let write_before = enabled.then(process_write_bytes).flatten();
    let started = enabled.then(std::time::Instant::now);
    let output = operation()?;
    let elapsed_ms = started
        .map(|started| started.elapsed().as_secs_f64() * 1_000.0)
        .unwrap_or(0.0);
    let write_after = enabled.then(process_write_bytes).flatten();
    let write_bytes = write_before
        .zip(write_after)
        .map(|(before, after)| after.saturating_sub(before));
    Ok((output, elapsed_ms, write_bytes))
}

fn wal_path(database: &Path) -> PathBuf {
    let mut path = database.as_os_str().to_os_string();
    path.push("-wal");
    PathBuf::from(path)
}

fn fts_storage_footprint(conn: &Connection) -> Result<FtsStorageFootprint> {
    let (chunk_word, chunk_trigram, symbol, reference) = conn.query_row(
        "SELECT
             COALESCE(SUM(CASE WHEN name GLOB 'chunks_fts_word_*' THEN pgsize ELSE 0 END), 0),
             COALESCE(SUM(CASE WHEN name GLOB 'chunks_fts_trigram_*' THEN pgsize ELSE 0 END), 0),
             COALESCE(SUM(CASE WHEN name GLOB 'symbols_fts_trigram_*' THEN pgsize ELSE 0 END), 0),
             COALESCE(SUM(CASE WHEN name GLOB 'symbol_refs_fts_trigram_*' THEN pgsize ELSE 0 END), 0)
         FROM dbstat",
        [],
        |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, i64>(3)?,
            ))
        },
    )?;
    Ok(FtsStorageFootprint {
        chunk_word_bytes: i64_to_u64(chunk_word)?,
        chunk_trigram_bytes: i64_to_u64(chunk_trigram)?,
        symbol_bytes: i64_to_u64(symbol)?,
        reference_bytes: i64_to_u64(reference)?,
    })
}

fn populate_post_commit_diagnostics(
    conn: &Connection,
    database: &Path,
    diagnostics: &mut PublicationDiagnostics,
) -> Result<()> {
    let ((busy, log_frames, checkpointed_frames), elapsed_ms, write_bytes) =
        measured_storage_phase(true, || {
            Ok(
                conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, i64>(2)?,
                    ))
                })?,
            )
        })?;
    diagnostics.checkpoint_ms = elapsed_ms;
    diagnostics.checkpoint_write_bytes = write_bytes;
    diagnostics.checkpoint_busy = busy;
    diagnostics.checkpoint_log_frames = log_frames;
    diagnostics.checkpointed_frames = checkpointed_frames;
    diagnostics.database_bytes = fs::metadata(database)
        .map(|metadata| metadata.len())
        .unwrap_or(0);
    diagnostics.wal_bytes = fs::metadata(wal_path(database))
        .map(|metadata| metadata.len())
        .unwrap_or(0);
    diagnostics.fts_storage = fts_storage_footprint(conn)?;
    Ok(())
}

/// Logical on-disk bytes owned by each FTS5 search index.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct FtsStorageFootprint {
    /// Word-tokenized chunk index bytes.
    pub chunk_word_bytes: u64,
    /// Trigram-tokenized chunk index bytes.
    pub chunk_trigram_bytes: u64,
    /// Trigram-tokenized symbol index bytes.
    pub symbol_bytes: u64,
    /// Trigram-tokenized symbol-reference index bytes.
    pub reference_bytes: u64,
}

/// Storage phases and footprint captured only by profiled reconciliation.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct PublicationDiagnostics {
    /// Import resolution and relational insertion time supplied by the indexer.
    pub relational_write_ms: f64,
    /// Linux process write bytes observed during relational insertion.
    pub relational_write_bytes: Option<u64>,
    /// Chunk word-index rebuild time.
    pub chunk_word_fts_rebuild_ms: f64,
    /// Linux process write bytes observed during the chunk word-index rebuild.
    pub chunk_word_fts_rebuild_write_bytes: Option<u64>,
    /// Chunk trigram-index rebuild time.
    pub chunk_trigram_fts_rebuild_ms: f64,
    /// Linux process write bytes observed during the chunk trigram-index rebuild.
    pub chunk_trigram_fts_rebuild_write_bytes: Option<u64>,
    /// Symbol trigram-index rebuild time.
    pub symbol_fts_rebuild_ms: f64,
    /// Linux process write bytes observed during the symbol-index rebuild.
    pub symbol_fts_rebuild_write_bytes: Option<u64>,
    /// Reference trigram-index rebuild time.
    pub reference_fts_rebuild_ms: f64,
    /// Linux process write bytes observed during the reference-index rebuild.
    pub reference_fts_rebuild_write_bytes: Option<u64>,
    /// Transaction commit time with auto-checkpointing disabled for this profile.
    pub commit_ms: f64,
    /// Linux process write bytes observed during commit.
    pub commit_write_bytes: Option<u64>,
    /// Explicit post-commit checkpoint time.
    pub checkpoint_ms: f64,
    /// Linux process write bytes observed during the checkpoint.
    pub checkpoint_write_bytes: Option<u64>,
    /// Busy readers reported by the explicit checkpoint.
    pub checkpoint_busy: i64,
    /// WAL frames reported after the explicit checkpoint.
    ///
    /// A successful `TRUNCATE` checkpoint reports zero after truncating the log.
    pub checkpoint_log_frames: i64,
    /// Checkpointed frames reported after the explicit checkpoint.
    ///
    /// A successful `TRUNCATE` checkpoint reports zero after truncating the log.
    pub checkpointed_frames: i64,
    /// Main database bytes after the explicit checkpoint.
    pub database_bytes: u64,
    /// WAL bytes remaining after the explicit checkpoint.
    pub wal_bytes: u64,
    /// Per-index logical bytes from SQLite's `dbstat` virtual table.
    pub fts_storage: FtsStorageFootprint,
    /// Whether every post-commit checkpoint and footprint diagnostic completed.
    ///
    /// Publication success does not depend on diagnostic collection after the
    /// transaction has committed.
    pub post_commit_diagnostics_complete: bool,
}

struct DatabaseTriggerGuard<'connection> {
    connection: &'connection Connection,
    previous: Option<bool>,
}

impl<'connection> DatabaseTriggerGuard<'connection> {
    fn disable(connection: &'connection Connection) -> rusqlite::Result<Self> {
        let previous = connection.db_config(DbConfig::SQLITE_DBCONFIG_ENABLE_TRIGGER)?;
        connection.set_db_config(DbConfig::SQLITE_DBCONFIG_ENABLE_TRIGGER, false)?;
        Ok(Self {
            connection,
            previous: Some(previous),
        })
    }

    fn restore(mut self) -> rusqlite::Result<()> {
        self.restore_inner()
    }

    fn restore_inner(&mut self) -> rusqlite::Result<()> {
        let Some(previous) = self.previous else {
            return Ok(());
        };
        self.connection
            .set_db_config(DbConfig::SQLITE_DBCONFIG_ENABLE_TRIGGER, previous)?;
        self.previous = None;
        Ok(())
    }
}

impl Drop for DatabaseTriggerGuard<'_> {
    fn drop(&mut self) {
        let _ = self.restore_inner();
    }
}
