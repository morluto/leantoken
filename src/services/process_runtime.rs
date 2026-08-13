use std::fmt;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use tokio::sync::Semaphore;

use super::executor::BlockingExecutor;
use crate::concurrency::{default_blocking_active_capacity, default_read_connection_capacity};
use crate::config::MAX_REPOSITORY_CONTEXTS;
use crate::indexer::LazyWorkerPool;
use crate::{Config, Error, Result};

const MAX_REPOSITORY_SERVICES: usize = MAX_REPOSITORY_CONTEXTS + 1;

/// Process-owned execution and resource budgets shared by repository services.
///
/// A standalone [`super::Services`] creates one runtime automatically. Servers
/// that host multiple repository contexts should construct one runtime and use
/// it for every context so adding repositories does not multiply thread,
/// snapshot, reconciliation, or indexing-memory limits.
#[derive(Clone)]
pub struct ServicesRuntime {
    pub(super) blocking_executor: BlockingExecutor,
    pub(super) snapshot_admission: Arc<Semaphore>,
    pub(super) reconciliation_admission: Arc<Semaphore>,
    pub(super) indexing_admission: Arc<Semaphore>,
    pub(crate) index_pool: Arc<LazyWorkerPool>,
    repository_services: Arc<AtomicUsize>,
    max_index_workers: usize,
    snapshot_capacity: usize,
    reconciliation_capacity: usize,
}

/// Current process-owned resource bounds and occupancy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ServicesRuntimeDiagnostics {
    /// Maximum simultaneously registered primary and context repositories.
    pub max_repository_services: usize,
    /// Repository services currently registered with this runtime.
    pub active_repository_services: usize,
    /// Maximum pooled SQLite readers across a full runtime.
    pub max_pooled_reader_connections: usize,
    /// Maximum concurrently pinned read snapshots across repositories.
    pub snapshot_capacity: usize,
    /// Read snapshots currently holding a process permit.
    pub active_snapshots: usize,
    /// Maximum admitted blocking retrieval operations, including waiters.
    pub blocking_active_capacity: usize,
    /// Maximum blocking retrieval operations executing concurrently.
    pub blocking_execution_capacity: usize,
    /// Maximum admitted reconciliation waves across repositories.
    pub reconciliation_capacity: usize,
    /// Reconciliation waves currently holding a process permit.
    pub active_reconciliations: usize,
    /// Maximum index preparation/publication operations executing concurrently.
    pub indexing_capacity: usize,
    /// Index operations currently holding the process permit.
    pub active_indexing: usize,
    /// Rayon workers in the shared lazy indexing pool.
    pub index_workers: usize,
}

#[derive(Debug, Clone)]
pub(super) struct RuntimeRepositoryRegistration {
    inner: Arc<RuntimeRepositoryRegistrationInner>,
}

#[derive(Debug)]
struct RuntimeRepositoryRegistrationInner {
    active: Arc<AtomicUsize>,
    reader_connection_capacity: u32,
}

impl Drop for RuntimeRepositoryRegistrationInner {
    fn drop(&mut self) {
        self.active.fetch_sub(1, Ordering::AcqRel);
    }
}

impl RuntimeRepositoryRegistration {
    pub(super) fn reader_connection_capacity(&self) -> u32 {
        self.inner.reader_connection_capacity
    }
}

impl fmt::Debug for ServicesRuntime {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ServicesRuntime")
            .field("max_index_workers", &self.max_index_workers)
            .field("diagnostics", &self.diagnostics())
            .finish_non_exhaustive()
    }
}

impl ServicesRuntime {
    /// Create a process runtime with one bounded indexing worker pool.
    pub fn new(max_index_workers: usize) -> Result<Self> {
        if max_index_workers == 0 {
            return Err(Error::InvalidConfiguration(
                "max_index_workers must be positive".into(),
            ));
        }
        let snapshot_capacity = default_read_connection_capacity() as usize;
        let reconciliation_capacity = default_blocking_active_capacity();
        Ok(Self {
            blocking_executor: BlockingExecutor::default(),
            snapshot_admission: Arc::new(Semaphore::new(snapshot_capacity)),
            reconciliation_admission: Arc::new(Semaphore::new(reconciliation_capacity)),
            // One reconciliation owns the bounded preparation/publication
            // footprint at a time; every repository shares the worker pool.
            indexing_admission: Arc::new(Semaphore::new(1)),
            index_pool: Arc::new(LazyWorkerPool::new()),
            repository_services: Arc::new(AtomicUsize::new(0)),
            max_index_workers,
            snapshot_capacity,
            reconciliation_capacity,
        })
    }

    pub(super) fn for_config(config: &Config) -> Result<Self> {
        Self::new(config.max_index_workers)
    }

    /// Number of Rayon workers shared by every repository in this runtime.
    #[must_use]
    pub const fn max_index_workers(&self) -> usize {
        self.max_index_workers
    }

    /// Inspect process-wide limits without mutating runtime state.
    #[must_use]
    pub fn diagnostics(&self) -> ServicesRuntimeDiagnostics {
        let (blocking_active_capacity, blocking_execution_capacity) =
            self.blocking_executor.capacities();
        ServicesRuntimeDiagnostics {
            max_repository_services: MAX_REPOSITORY_SERVICES,
            active_repository_services: self.repository_services.load(Ordering::Acquire),
            max_pooled_reader_connections: self
                .snapshot_capacity
                .saturating_add(MAX_REPOSITORY_SERVICES.saturating_sub(1)),
            snapshot_capacity: self.snapshot_capacity,
            active_snapshots: self
                .snapshot_capacity
                .saturating_sub(self.snapshot_admission.available_permits()),
            blocking_active_capacity,
            blocking_execution_capacity,
            reconciliation_capacity: self.reconciliation_capacity,
            active_reconciliations: self
                .reconciliation_capacity
                .saturating_sub(self.reconciliation_admission.available_permits()),
            indexing_capacity: 1,
            active_indexing: 1usize.saturating_sub(self.indexing_admission.available_permits()),
            index_workers: self.max_index_workers,
        }
    }

    pub(super) fn register_repository(&self) -> Result<RuntimeRepositoryRegistration> {
        let mut active = self.repository_services.load(Ordering::Acquire);
        loop {
            if active >= MAX_REPOSITORY_SERVICES {
                return Err(Error::RequestLimitExceeded {
                    field: "process repository services",
                    requested: active.saturating_add(1),
                    limit: MAX_REPOSITORY_SERVICES,
                });
            }
            match self.repository_services.compare_exchange_weak(
                active,
                active + 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => {
                    let reader_connection_capacity = if active == 0 {
                        u32::try_from(self.snapshot_capacity).unwrap_or(u32::MAX)
                    } else {
                        1
                    };
                    return Ok(RuntimeRepositoryRegistration {
                        inner: Arc::new(RuntimeRepositoryRegistrationInner {
                            active: Arc::clone(&self.repository_services),
                            reader_connection_capacity,
                        }),
                    });
                }
                Err(observed) => active = observed,
            }
        }
    }

    pub(super) fn validate_config(&self, config: &Config) -> Result<()> {
        if config.max_index_workers != self.max_index_workers {
            return Err(Error::InvalidConfiguration(format!(
                "repository max_index_workers ({}) must match the process runtime budget ({})",
                config.max_index_workers, self.max_index_workers
            )));
        }
        Ok(())
    }
}
