use super::startup::{INITIAL_INDEX_IDLE_GRACE, INITIAL_INDEX_PROBE_INTERVAL};
use super::*;

impl Services {
    pub async fn index(&self, mode: IndexingMode) -> Result<IndexResponse> {
        self.index_report(mode)
            .await
            .map(IndexReport::into_response)
    }

    /// Reconcile repository files and include bounded preparation skip reasons.
    pub async fn index_report(&self, mode: IndexingMode) -> Result<IndexReport> {
        self.index_cancellable_report(mode, CancellationToken::new())
            .await
    }

    /// Reconcile repository files while honoring caller-owned cancellation.
    pub async fn index_cancellable(
        &self,
        mode: IndexingMode,
        cancellation: CancellationToken,
    ) -> Result<IndexResponse> {
        self.index_cancellable_report(mode, cancellation)
            .await
            .map(IndexReport::into_response)
    }

    /// Reconcile with cancellation and include bounded preparation skip reasons.
    pub async fn index_cancellable_report(
        &self,
        mode: IndexingMode,
        cancellation: CancellationToken,
    ) -> Result<IndexReport> {
        let this = self.clone();
        let active_reconciliations = Arc::clone(&self.active_reconciliations);
        let reconciliation_changed = Arc::clone(&self.reconciliation_changed);
        active_reconciliations.fetch_add(1, Ordering::AcqRel);
        tokio::task::spawn_blocking(move || {
            let _active = ActiveReconciliation {
                count: active_reconciliations,
                changed: reconciliation_changed,
            };
            let operation = this.coordination.acquire_operation(&cancellation)?;
            let result = this
                .indexer
                .reconcile_cancellable_report(mode, &cancellation);
            operation.release()?;
            result
        })
        .await?
    }

    /// Reconcile watcher-reported paths, falling back internally when a
    /// repository-wide scan is required for correctness.
    pub async fn index_paths(&self, paths: Vec<String>) -> Result<IndexResponse> {
        self.index_paths_report(paths)
            .await
            .map(IndexReport::into_response)
    }

    /// Reconcile watcher paths and include bounded preparation skip reasons.
    pub async fn index_paths_report(&self, paths: Vec<String>) -> Result<IndexReport> {
        self.index_paths_cancellable_report(paths, CancellationToken::new())
            .await
    }

    /// Reconcile watcher-reported paths while honoring caller-owned cancellation.
    pub async fn index_paths_cancellable(
        &self,
        paths: Vec<String>,
        cancellation: CancellationToken,
    ) -> Result<IndexResponse> {
        self.index_paths_cancellable_report(paths, cancellation)
            .await
            .map(IndexReport::into_response)
    }

    /// Reconcile watcher paths with cancellation and preparation skip reasons.
    pub async fn index_paths_cancellable_report(
        &self,
        paths: Vec<String>,
        cancellation: CancellationToken,
    ) -> Result<IndexReport> {
        let this = self.clone();
        let active_reconciliations = Arc::clone(&self.active_reconciliations);
        let reconciliation_changed = Arc::clone(&self.reconciliation_changed);
        active_reconciliations.fetch_add(1, Ordering::AcqRel);
        tokio::task::spawn_blocking(move || {
            let _active = ActiveReconciliation {
                count: active_reconciliations,
                changed: reconciliation_changed,
            };
            let operation = this.coordination.acquire_operation(&cancellation)?;
            let result = this
                .indexer
                .reconcile_paths_cancellable_report(&paths, &cancellation);
            operation.release()?;
            result
        })
        .await?
    }

    /// Wait until the first committed generation is no longer being published.
    pub(crate) async fn wait_for_initial_index_cancellable(
        &self,
        cancellation: CancellationToken,
    ) -> Result<()> {
        let mut idle_deadline = None;
        loop {
            validation::check_cancelled(&cancellation)?;

            let changed = self.reconciliation_changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            if self.active_reconciliations.load(Ordering::Acquire) > 0 {
                idle_deadline = None;
                tokio::select! {
                    _ = cancellation.cancelled() => return Err(Error::Cancelled),
                    _ = &mut changed => {}
                }
                continue;
            }

            let this = self.clone();
            let probe = tokio::task::spawn_blocking(move || {
                let Some(operation) = this.coordination.try_acquire_operation()? else {
                    return Ok(None);
                };
                let generation = this.storage.repository_generation();
                operation.release()?;
                generation.map(Some)
            });
            let generation = tokio::select! {
                _ = cancellation.cancelled() => return Err(Error::Cancelled),
                _ = &mut changed => {
                    idle_deadline = None;
                    continue;
                },
                result = probe => result??,
            };
            if generation.is_some_and(|generation| generation > 0) {
                return Ok(());
            }
            let delay = if generation.is_none() {
                idle_deadline = None;
                INITIAL_INDEX_PROBE_INTERVAL
            } else {
                let deadline = idle_deadline
                    .get_or_insert_with(|| tokio::time::Instant::now() + INITIAL_INDEX_IDLE_GRACE);
                let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
                if remaining.is_zero() {
                    return Err(Error::IndexNotReady);
                }
                remaining.min(INITIAL_INDEX_PROBE_INTERVAL)
            };

            tokio::select! {
                _ = cancellation.cancelled() => return Err(Error::Cancelled),
                _ = &mut changed => {
                    idle_deadline = None;
                },
                _ = tokio::time::sleep(delay) => {}
            }
        }
    }

    /// Attempt to own automatic indexing and watching for this cache.
    pub fn try_acquire_index_leadership(&self) -> Result<Option<IndexLeadership>> {
        self.coordination.try_acquire_leadership()
    }
}

pub(super) struct ActiveReconciliation {
    count: Arc<AtomicUsize>,
    changed: Arc<tokio::sync::Notify>,
}

impl ActiveReconciliation {
    pub(super) fn new(counter: Arc<AtomicUsize>, changed: Arc<tokio::sync::Notify>) -> Self {
        counter.fetch_add(1, Ordering::AcqRel);
        Self {
            count: counter,
            changed,
        }
    }
}

impl Drop for ActiveReconciliation {
    fn drop(&mut self) {
        self.count.fetch_sub(1, Ordering::AcqRel);
        self.changed.notify_waiters();
    }
}
