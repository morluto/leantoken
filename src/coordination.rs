use std::{
    fs::{File, OpenOptions, TryLockError},
    path::{Path, PathBuf},
    thread,
    time::Duration,
};

use tokio_util::sync::CancellationToken;

use crate::{Error, Result};

const LOCK_RETRY_DELAY: Duration = Duration::from_millis(25);

/// Repository-scoped operating-system locks for index ownership and publication.
///
/// Leadership is held for an MCP leader's lifetime. The operation lock is held
/// only while discovering, preparing, and publishing one reconciliation, so
/// explicit CLI indexing and automatic indexing cannot build stale plans in
/// parallel across processes.
#[derive(Debug, Clone)]
pub struct IndexCoordination {
    initialization_path: PathBuf,
    leadership_path: PathBuf,
    operation_path: PathBuf,
}

impl IndexCoordination {
    /// Derive stable lock paths from the canonical SQLite cache identity.
    #[must_use]
    pub fn for_database(database_path: &Path) -> Self {
        Self {
            initialization_path: with_suffix(database_path, ".init.lock"),
            leadership_path: with_suffix(database_path, ".leader.lock"),
            operation_path: with_suffix(database_path, ".index.lock"),
        }
    }

    /// Wait for exclusive cache initialization ownership while honoring cancellation.
    pub fn acquire_initialization(
        &self,
        cancellation: &CancellationToken,
    ) -> Result<CacheInitialization> {
        acquire(&self.initialization_path, cancellation)
            .map(|file| CacheInitialization { _file: file })
    }

    /// Attempt to become the single automatic indexer and watcher.
    pub fn try_acquire_leadership(&self) -> Result<Option<IndexLeadership>> {
        let file = open_lock_file(&self.leadership_path)?;
        match file.try_lock() {
            Ok(()) => Ok(Some(IndexLeadership { _file: file })),
            Err(TryLockError::WouldBlock) => Ok(None),
            Err(TryLockError::Error(error)) => Err(error.into()),
        }
    }

    /// Wait for exclusive reconciliation ownership while honoring cancellation.
    pub fn acquire_operation(&self, cancellation: &CancellationToken) -> Result<IndexOperation> {
        acquire(&self.operation_path, cancellation).map(|file| IndexOperation { _file: file })
    }

    /// Return whether another handle or process currently owns a reconciliation.
    pub fn is_reconciling(&self) -> Result<bool> {
        let file = open_lock_file(&self.operation_path)?;
        match file.try_lock() {
            Ok(()) => Ok(false),
            Err(TryLockError::WouldBlock) => Ok(true),
            Err(TryLockError::Error(error)) => Err(error.into()),
        }
    }
}

/// Lifetime proof that this process owns cache initialization and recovery.
#[derive(Debug)]
pub struct CacheInitialization {
    _file: File,
}

/// Lifetime proof that this process owns automatic indexing for one cache.
#[derive(Debug)]
pub struct IndexLeadership {
    _file: File,
}

/// Lifetime proof that one reconciliation is serialized across processes.
#[derive(Debug)]
pub struct IndexOperation {
    _file: File,
}

fn acquire(path: &Path, cancellation: &CancellationToken) -> Result<File> {
    let file = open_lock_file(path)?;
    loop {
        if cancellation.is_cancelled() {
            return Err(Error::Cancelled);
        }
        match file.try_lock() {
            Ok(()) => return Ok(file),
            Err(TryLockError::WouldBlock) => thread::sleep(LOCK_RETRY_DELAY),
            Err(TryLockError::Error(error)) => return Err(error.into()),
        }
    }
}

fn open_lock_file(path: &Path) -> Result<File> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
        .map_err(Into::into)
}

fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut value = path.as_os_str().to_os_string();
    value.push(suffix);
    PathBuf::from(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn leadership_is_exclusive_and_released_with_the_guard() {
        let directory = tempfile::tempdir().expect("directory");
        let coordination = IndexCoordination::for_database(&directory.path().join("index.sqlite"));

        let leader = coordination
            .try_acquire_leadership()
            .expect("first attempt")
            .expect("leader");
        assert!(
            coordination
                .try_acquire_leadership()
                .expect("second attempt")
                .is_none()
        );

        drop(leader);
        assert!(
            coordination
                .try_acquire_leadership()
                .expect("released attempt")
                .is_some()
        );
    }

    #[test]
    fn operation_state_is_visible_across_handles() {
        let directory = tempfile::tempdir().expect("directory");
        let coordination = IndexCoordination::for_database(&directory.path().join("index.sqlite"));
        let cancellation = CancellationToken::new();

        let operation = coordination
            .acquire_operation(&cancellation)
            .expect("operation");
        assert!(coordination.is_reconciling().expect("state"));

        drop(operation);
        assert!(!coordination.is_reconciling().expect("released state"));
    }

    #[test]
    fn initialization_is_exclusive_and_released_with_the_guard() {
        let directory = tempfile::tempdir().expect("directory");
        let coordination = IndexCoordination::for_database(&directory.path().join("index.sqlite"));
        let cancellation = CancellationToken::new();

        let initialization = coordination
            .acquire_initialization(&cancellation)
            .expect("initialization");
        let waiting_coordination = coordination.clone();
        let waiting_cancellation = CancellationToken::new();
        let waiting_token = waiting_cancellation.clone();
        let waiter =
            std::thread::spawn(move || waiting_coordination.acquire_initialization(&waiting_token));

        std::thread::sleep(Duration::from_millis(50));
        waiting_cancellation.cancel();
        assert!(matches!(
            waiter.join().expect("join waiter"),
            Err(Error::Cancelled)
        ));

        drop(initialization);
        assert!(
            coordination
                .acquire_initialization(&CancellationToken::new())
                .is_ok()
        );
    }
}
