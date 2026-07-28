impl Clone for Storage {
    fn clone(&self) -> Self {
        Self {
            writer: Arc::clone(&self.writer),
            readers: self.readers.clone(),
            path: self.path.clone(),
            #[cfg(test)]
            diagnostics: Arc::clone(&self.diagnostics),
        }
    }
}

impl fmt::Debug for Storage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Storage")
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}

/// One read-only connection held under a DEFERRED transaction so all queries
/// on this session observe a single SQLite WAL snapshot.
pub struct ReadSession {
    conn: r2d2::PooledConnection<SqliteConnectionManager>,
    #[cfg(test)]
    diagnostics: Arc<StorageDiagnostics>,
}

impl Drop for ReadSession {
    fn drop(&mut self) {
        let _ = self.conn.execute_batch("ROLLBACK");
        #[cfg(test)]
        self.diagnostics
            .active_snapshots
            .fetch_sub(1, Ordering::AcqRel);
    }
}

#[derive(Clone, Copy, Debug)]
enum FtsTable {
    Word,
    Trigram,
}

impl FtsTable {
    fn as_str(self) -> &'static str {
        match self {
            FtsTable::Word => "chunks_fts_word",
            FtsTable::Trigram => "chunks_fts_trigram",
        }
    }
}

fn unix_seconds(time: SystemTime) -> i64 {
    time.duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_secs()).ok())
        .unwrap_or(0)
}
