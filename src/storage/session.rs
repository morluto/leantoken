impl Storage {
    /// Open a read-only connection and begin a DEFERRED transaction so callers
    /// observe one WAL snapshot until the session is dropped.
    ///
    /// Keep one session for every multi-query response. Dropping it rolls back
    /// the read transaction and returns the connection to the bounded pool.
    pub fn begin_read(&self) -> Result<ReadSession> {
        #[cfg(test)]
        let checkout_started = Instant::now();
        let conn = self.readers.get()?;
        #[cfg(test)]
        self.diagnostics
            .reader_checkout_wait_micros
            .lock()
            .expect("storage diagnostics")
            .push(
                checkout_started
                    .elapsed()
                    .as_micros()
                    .min(u128::from(u64::MAX)) as u64,
            );
        // Under WAL, the first read in a DEFERRED transaction pins the snapshot
        // for the rest of the connection's transaction lifetime.
        conn.execute_batch("BEGIN DEFERRED")?;
        #[cfg(test)]
        {
            let active = self
                .diagnostics
                .active_snapshots
                .fetch_add(1, Ordering::AcqRel)
                .saturating_add(1);
            self.diagnostics
                .peak_active_snapshots
                .fetch_max(active, Ordering::AcqRel);
        }
        Ok(ReadSession {
            conn,
            #[cfg(test)]
            diagnostics: Arc::clone(&self.diagnostics),
        })
    }

    #[cfg(test)]
    pub(crate) fn reset_diagnostics(&self) {
        self.diagnostics
            .active_snapshots
            .store(0, Ordering::Release);
        self.diagnostics
            .peak_active_snapshots
            .store(0, Ordering::Release);
        self.diagnostics
            .reader_checkout_wait_micros
            .lock()
            .expect("storage diagnostics")
            .clear();
    }

    #[cfg(test)]
    pub(crate) fn diagnostics(&self) -> StorageDiagnosticsSnapshot {
        StorageDiagnosticsSnapshot {
            active_snapshots: self.diagnostics.active_snapshots.load(Ordering::Acquire),
            peak_active_snapshots: self
                .diagnostics
                .peak_active_snapshots
                .load(Ordering::Acquire),
            reader_checkout_wait_micros: self
                .diagnostics
                .reader_checkout_wait_micros
                .lock()
                .expect("storage diagnostics")
                .clone(),
        }
    }
}
