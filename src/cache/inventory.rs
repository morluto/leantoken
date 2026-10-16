use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use std::time::Instant;

use super::*;

/// Inventory admission is independent of response pagination and filtering.
#[derive(Debug, Clone, Copy)]
pub(super) struct InventoryLimits {
    pub(super) root_entries: usize,
    pub(super) candidates: usize,
    pub(super) artifacts: usize,
    pub(super) sqlite_opens: usize,
    pub(super) metadata_bytes: usize,
    pub(super) sqlite_steps: usize,
    pub(super) busy_timeout: Duration,
    pub(super) elapsed: Duration,
}

impl Default for InventoryLimits {
    fn default() -> Self {
        Self {
            root_entries: 4096,
            candidates: 1024,
            artifacts: 32768,
            sqlite_opens: 1024,
            metadata_bytes: 8 * 1024 * 1024,
            sqlite_steps: 2_000_000,
            busy_timeout: Duration::from_millis(100),
            elapsed: Duration::from_secs(10),
        }
    }
}

pub(super) const MAX_INVENTORY_VALUE_BYTES: i32 = 64 * 1024;

pub(super) struct InventoryBudget {
    pub(super) limits: InventoryLimits,
    pub(super) root_entries: usize,
    pub(super) candidates: usize,
    pub(super) artifacts: usize,
    pub(super) sqlite_opens: usize,
    pub(super) metadata_bytes: usize,
    started: Instant,
    cancellation: tokio_util::sync::CancellationToken,
    sqlite_steps: Arc<AtomicUsize>,
}

impl InventoryBudget {
    pub(super) fn new(manager: &CacheManager) -> Self {
        Self {
            limits: manager.inventory_limits,
            root_entries: 0,
            candidates: 0,
            artifacts: 0,
            sqlite_opens: 0,
            metadata_bytes: 0,
            started: Instant::now(),
            cancellation: manager.cancellation.clone(),
            sqlite_steps: Arc::default(),
        }
    }

    pub(super) fn check(&self) -> Result<()> {
        if self.cancellation.is_cancelled() {
            return Err(Error::Cancelled);
        }
        Self::enforce(
            "elapsed_millis",
            usize::try_from(self.started.elapsed().as_millis()).unwrap_or(usize::MAX),
            usize::try_from(self.limits.elapsed.as_millis()).unwrap_or(usize::MAX),
        )?;
        Self::enforce(
            "sqlite_steps",
            self.sqlite_steps.load(Ordering::Relaxed),
            self.limits.sqlite_steps,
        )
    }

    pub(super) fn enforce(dimension: &'static str, observed: usize, limit: usize) -> Result<()> {
        if observed > limit {
            return Err(Error::CacheInventoryIncomplete {
                dimension,
                observed,
                limit,
            });
        }
        Ok(())
    }

    pub(super) fn admit_metadata(&mut self, bytes: usize) -> Result<()> {
        self.check()?;
        self.metadata_bytes = self.metadata_bytes.saturating_add(bytes);
        Self::enforce(
            "metadata_bytes",
            self.metadata_bytes,
            self.limits.metadata_bytes,
        )
    }

    pub(super) fn observe_sqlite(&self, connection: &Connection) -> Result<()> {
        let started = self.started;
        let elapsed = self.limits.elapsed;
        let steps = Arc::clone(&self.sqlite_steps);
        let limit = self.limits.sqlite_steps;
        let cancellation = self.cancellation.clone();
        connection.progress_handler(
            1,
            Some(move || {
                let observed = steps.fetch_add(1, Ordering::Relaxed).saturating_add(1);
                observed > limit
                    || (observed.is_multiple_of(1000)
                        && (cancellation.is_cancelled()
                            || started.elapsed().as_millis() > elapsed.as_millis()))
            }),
        )?;
        Ok(())
    }

    pub(super) fn sqlite_result<T>(&self, result: rusqlite::Result<T>) -> Result<T> {
        self.check()?;
        match result {
            Err(rusqlite::Error::SqliteFailure(error, _))
                if error.code == rusqlite::ErrorCode::TooBig =>
            {
                let limit = MAX_INVENTORY_VALUE_BYTES as usize;
                Err(Error::CacheInventoryIncomplete {
                    dimension: "sqlite_value_bytes",
                    observed: limit + 1,
                    limit,
                })
            }
            other => Ok(other?),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn elapsed_budget_is_checked_without_waiting_for_another_sqlite_callback() {
        let manager = CacheManager::new(PathBuf::new(), 0);
        let mut budget = InventoryBudget::new(&manager);
        budget.started = Instant::now() - Duration::from_secs(11);
        assert!(matches!(
            budget.check(),
            Err(Error::CacheInventoryIncomplete {
                dimension: "elapsed_millis",
                ..
            })
        ));
    }
}
