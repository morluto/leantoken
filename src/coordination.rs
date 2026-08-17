use std::{
    fs::{self, File, OpenOptions, TryLockError},
    path::{Path, PathBuf},
    sync::Arc,
    thread,
    time::Duration,
};

use tokio_util::sync::CancellationToken;

use crate::{Error, Result};

const LOCK_RETRY_DELAY: Duration = Duration::from_millis(25);
pub(crate) const DEFAULT_INDEX_DATABASE_NAME: &str = "index.sqlite";
pub(crate) const LEASE_LOCK_SUFFIX: &str = ".lease.lock";
pub(crate) const INITIALIZATION_LOCK_SUFFIX: &str = ".init.lock";
pub(crate) const LEADERSHIP_LOCK_SUFFIX: &str = ".leader.lock";
pub(crate) const OPERATION_LOCK_SUFFIX: &str = ".index.lock";
pub(crate) const COORDINATION_LOCK_SUFFIXES: [&str; 4] = [
    LEASE_LOCK_SUFFIX,
    INITIALIZATION_LOCK_SUFFIX,
    LEADERSHIP_LOCK_SUFFIX,
    OPERATION_LOCK_SUFFIX,
];

pub(crate) fn coordination_sidecar_path(database_path: &Path, suffix: &str) -> PathBuf {
    let mut value = database_path.as_os_str().to_os_string();
    value.push(suffix);
    PathBuf::from(value)
}

pub(crate) fn is_coordination_sidecar_for_database(candidate: &Path, database_path: &Path) -> bool {
    COORDINATION_LOCK_SUFFIXES
        .into_iter()
        .any(|suffix| candidate == coordination_sidecar_path(database_path, suffix))
}

/// Recognize a stale default-name lock without ignoring arbitrary user locks.
pub(crate) fn is_recognized_stale_coordination_sidecar(candidate: &Path) -> bool {
    let Some(name) = candidate.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    let Some(suffix) = name.strip_prefix(DEFAULT_INDEX_DATABASE_NAME) else {
        return false;
    };
    if !COORDINATION_LOCK_SUFFIXES.contains(&suffix) {
        return false;
    }
    fs::symlink_metadata(candidate)
        .is_ok_and(|metadata| metadata.file_type().is_file() && metadata.len() == 0)
}

/// Repository-scoped operating-system locks for index ownership and publication.
///
/// Leadership is held for an MCP leader's lifetime. The operation lock is held
/// only while discovering, preparing, and publishing one reconciliation, so
/// explicit CLI indexing and automatic indexing cannot build stale plans in
/// parallel across processes.
#[derive(Debug, Clone)]
pub struct IndexCoordination {
    lease_path: PathBuf,
    initialization_path: PathBuf,
    leadership_path: PathBuf,
    operation_path: PathBuf,
}

impl IndexCoordination {
    /// Derive stable lock paths from the canonical SQLite cache identity.
    #[must_use]
    pub fn for_database(database_path: &Path) -> Self {
        Self {
            lease_path: coordination_sidecar_path(database_path, LEASE_LOCK_SUFFIX),
            initialization_path: coordination_sidecar_path(
                database_path,
                INITIALIZATION_LOCK_SUFFIX,
            ),
            leadership_path: coordination_sidecar_path(database_path, LEADERSHIP_LOCK_SUFFIX),
            operation_path: coordination_sidecar_path(database_path, OPERATION_LOCK_SUFFIX),
        }
    }

    /// Wait for shared lifetime ownership that prevents active-cache pruning.
    pub fn acquire_cache_lease(&self, cancellation: &CancellationToken) -> Result<CacheLease> {
        let file = open_lock_file(&self.lease_path)?;
        loop {
            if cancellation.is_cancelled() {
                return Err(Error::Cancelled);
            }
            if try_lock_shared_file(&file)? {
                return Ok(CacheLease {
                    _file: Arc::new(file),
                });
            }
            thread::sleep(LOCK_RETRY_DELAY);
        }
    }

    /// Try to obtain exclusive ownership for one managed-cache deletion.
    pub(crate) fn try_acquire_prune_lease(&self) -> Result<Option<CachePruneLease>> {
        let file = open_lock_file(&self.lease_path)?;
        if try_lock_file(&file)? {
            Ok(Some(CachePruneLease { _file: file }))
        } else {
            Ok(None)
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
        if try_lock_file(&file)? {
            Ok(Some(IndexLeadership { _file: file }))
        } else {
            Ok(None)
        }
    }

    /// Wait for exclusive reconciliation ownership while honoring cancellation.
    pub fn acquire_operation(&self, cancellation: &CancellationToken) -> Result<IndexOperation> {
        acquire(&self.operation_path, cancellation).map(|file| IndexOperation { file })
    }

    /// Try to reserve reconciliation ownership without waiting.
    pub(crate) fn try_acquire_operation(&self) -> Result<Option<IndexOperation>> {
        let file = open_lock_file(&self.operation_path)?;
        if try_lock_file(&file)? {
            Ok(Some(IndexOperation { file }))
        } else {
            Ok(None)
        }
    }

    /// Return whether another handle or process currently owns a reconciliation.
    pub fn is_reconciling(&self) -> Result<bool> {
        let file = open_lock_file(&self.operation_path)?;
        try_lock_file(&file).map(|acquired| !acquired)
    }
}

/// Lifetime proof that this process owns cache initialization and recovery.
#[derive(Debug)]
pub struct CacheInitialization {
    _file: File,
}

/// Shared lifetime proof that a cache is in use by application services.
#[derive(Debug, Clone)]
pub struct CacheLease {
    _file: Arc<File>,
}

/// Exclusive proof that no lease-aware process is using a cache.
#[derive(Debug)]
pub(crate) struct CachePruneLease {
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
    file: File,
}

impl IndexOperation {
    /// Release reconciliation ownership before publishing operation completion.
    pub(crate) fn release(self) -> Result<()> {
        unlock_file(&self.file)
    }
}

fn acquire(path: &Path, cancellation: &CancellationToken) -> Result<File> {
    let file = open_lock_file(path)?;
    loop {
        if cancellation.is_cancelled() {
            return Err(Error::Cancelled);
        }
        if try_lock_file(&file)? {
            return Ok(file);
        }
        thread::sleep(LOCK_RETRY_DELAY);
    }
}

fn try_lock_file(file: &File) -> Result<bool> {
    try_lock_with(|| file.try_lock())
}

fn try_lock_shared_file(file: &File) -> Result<bool> {
    try_lock_with(|| file.try_lock_shared())
}

fn unlock_file(file: &File) -> Result<()> {
    unlock_with(|| file.unlock())
}

fn try_lock_with(
    mut attempt: impl FnMut() -> std::result::Result<(), TryLockError>,
) -> Result<bool> {
    loop {
        match attempt() {
            Ok(()) => return Ok(true),
            Err(TryLockError::WouldBlock) => return Ok(false),
            Err(TryLockError::Error(error)) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(TryLockError::Error(error)) => return Err(error.into()),
        }
    }
}

fn unlock_with(mut attempt: impl FnMut() -> std::io::Result<()>) -> Result<()> {
    loop {
        match attempt() {
            Ok(()) => return Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error.into()),
        }
    }
}

fn open_lock_file(path: &Path) -> Result<File> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    open_lock_file_impl(path)
}

#[cfg(unix)]
fn open_lock_file_impl(path: &Path) -> Result<File> {
    use std::os::unix::fs::OpenOptionsExt;
    // O_NOFOLLOW prevents following symlinks at the final path component,
    // eliminating the TOCTOU window between the pre-open symlink check and
    // the open call. 0x020000 is O_NOFOLLOW on Linux; on macOS it is
    // 0x00000100, while 0x0100000 is O_DIRECTORY. std::os::unix::fs::
    // OpenOptionsExt does not expose O_NOFOLLOW directly, but custom_flags
    // passes raw flags to the open(2) syscall.
    const O_NOFOLLOW_LINUX: i32 = 0x020000;
    const O_NOFOLLOW_MACOS: i32 = 0x00000100;
    let flags = if cfg!(target_os = "macos") {
        O_NOFOLLOW_MACOS
    } else {
        O_NOFOLLOW_LINUX
    };
    OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .custom_flags(flags)
        .open(path)
        .map_err(Into::into)
}

#[cfg(not(unix))]
fn open_lock_file_impl(path: &Path) -> Result<File> {
    OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
        .map_err(Into::into)
}

#[cfg(test)]
mod tests {
    use std::io;

    use super::*;

    #[test]
    fn stale_sidecar_recognition_is_exact_and_requires_a_zero_byte_regular_file() {
        let directory = tempfile::tempdir().expect("directory");
        for suffix in COORDINATION_LOCK_SUFFIXES {
            let path = directory
                .path()
                .join(format!("{DEFAULT_INDEX_DATABASE_NAME}{suffix}"));
            fs::write(&path, []).expect("zero-byte sidecar");
            assert!(is_recognized_stale_coordination_sidecar(&path));
        }

        let nonzero = directory.path().join("index.sqlite.lease.lock");
        fs::write(&nonzero, "user data").expect("non-zero same-name file");
        assert!(!is_recognized_stale_coordination_sidecar(&nonzero));

        let arbitrary = directory.path().join("project.lock");
        fs::write(&arbitrary, []).expect("arbitrary lock");
        assert!(!is_recognized_stale_coordination_sidecar(&arbitrary));

        let directory_match = directory.path().join("index.sqlite.init.lock");
        fs::remove_file(&directory_match).expect("replace sidecar");
        fs::create_dir(&directory_match).expect("same-name directory");
        assert!(!is_recognized_stale_coordination_sidecar(&directory_match));
    }

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
    fn cache_prune_lease_waits_for_every_shared_lifetime_lease() {
        let directory = tempfile::tempdir().expect("directory");
        let coordination = IndexCoordination::for_database(&directory.path().join("index.sqlite"));
        let cancellation = CancellationToken::new();
        let first = coordination
            .acquire_cache_lease(&cancellation)
            .expect("first lease");
        let second = coordination
            .acquire_cache_lease(&cancellation)
            .expect("second lease");

        assert!(
            coordination
                .try_acquire_prune_lease()
                .expect("active probe")
                .is_none()
        );
        drop(first);
        assert!(
            coordination
                .try_acquire_prune_lease()
                .expect("one lease remains")
                .is_none()
        );
        drop(second);
        assert!(
            coordination
                .try_acquire_prune_lease()
                .expect("leases released")
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
    fn lock_probe_retries_interruption_before_acquiring() {
        let mut attempts = 0;

        let acquired = try_lock_with(|| {
            attempts += 1;
            if attempts < 3 {
                Err(TryLockError::Error(io::Error::from(
                    io::ErrorKind::Interrupted,
                )))
            } else {
                Ok(())
            }
        })
        .expect("lock probe");

        assert!(acquired);
        assert_eq!(attempts, 3);
    }

    #[test]
    fn lock_probe_preserves_would_block_after_interruption() {
        let mut attempts = 0;

        let acquired = try_lock_with(|| {
            attempts += 1;
            if attempts == 1 {
                Err(TryLockError::Error(io::Error::from(
                    io::ErrorKind::Interrupted,
                )))
            } else {
                Err(TryLockError::WouldBlock)
            }
        })
        .expect("lock probe");

        assert!(!acquired);
        assert_eq!(attempts, 2);
    }

    #[test]
    fn lock_probe_propagates_non_interruption_errors() {
        let error = try_lock_with(|| {
            Err(TryLockError::Error(io::Error::from(
                io::ErrorKind::PermissionDenied,
            )))
        })
        .expect_err("permission error");

        assert!(
            matches!(error, Error::Io(source) if source.kind() == io::ErrorKind::PermissionDenied)
        );
    }

    #[test]
    fn unlock_retries_interruption_before_succeeding() {
        let mut attempts = 0;

        unlock_with(|| {
            attempts += 1;
            if attempts < 3 {
                Err(io::Error::from(io::ErrorKind::Interrupted))
            } else {
                Ok(())
            }
        })
        .expect("unlock");

        assert_eq!(attempts, 3);
    }

    #[test]
    fn operation_release_makes_completion_observable() {
        let directory = tempfile::tempdir().expect("directory");
        let coordination = IndexCoordination::for_database(&directory.path().join("index.sqlite"));
        let operation = coordination
            .acquire_operation(&CancellationToken::new())
            .expect("operation");

        operation.release().expect("release");

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
