use std::time::{Duration, Instant};

impl Storage {
    /// Open a read-only connection and begin a DEFERRED transaction so callers
    /// observe one WAL snapshot until the session is dropped.
    ///
    /// Keep one session for every multi-query response. Dropping it rolls back
    /// the read transaction and returns the connection to the bounded pool.
    pub(super) fn begin_read(&self) -> Result<ReadSession> {
        let checkout_started = Instant::now();
        let conn = self.readers.get()?;
        let checkout_wait = checkout_started.elapsed();
        if checkout_wait >= Duration::from_millis(10) {
            tracing::debug!(
                wait_ms = checkout_wait.as_secs_f64() * 1_000.0,
                "storage reader pool checkout waited"
            );
        }
        // Under WAL, the first read in a DEFERRED transaction pins the snapshot
        // for the rest of the connection's transaction lifetime.
        conn.execute_batch("BEGIN DEFERRED")?;
        Ok(ReadSession { conn })
    }
}
use super::*;
