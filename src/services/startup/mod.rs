use super::*;

const STARTUP_BUSY_TIMEOUT: Duration = Duration::from_millis(250);
const STARTUP_RETRY_INITIAL_DELAY: Duration = Duration::from_millis(25);
const STARTUP_RETRY_MAX_DELAY: Duration = Duration::from_millis(500);
pub(super) const CANCELLATION_POLL_INTERVAL: Duration = Duration::from_millis(25);
pub(super) const INITIAL_INDEX_IDLE_GRACE: Duration = Duration::from_secs(1);
pub(super) const INITIAL_INDEX_PROBE_INTERVAL: Duration = Duration::from_millis(100);

impl Services {
    pub fn open(config: Config) -> Result<Self> {
        match Self::open_managed(config.clone()) {
            Ok(services) => Ok(services),
            Err(error) if should_use_repository_cache_fallback(&config, &error) => {
                let fallback = prepare_repository_cache_fallback(&config)?;
                tracing::warn!(
                    preferred_database = %config.database_path.display(),
                    fallback_database = %fallback.database_path.display(),
                    "managed cache is not writable; using repository-local fallback"
                );
                Self::open_managed(fallback)
            }
            Err(error) => Err(error),
        }
    }

    fn open_managed(config: Config) -> Result<Self> {
        config.validate()?;
        let coordination = IndexCoordination::for_database(&config.database_path);
        let cancellation = CancellationToken::new();
        let cache_lease = coordination.acquire_cache_lease(&cancellation)?;
        let _initialization = coordination.acquire_initialization(&cancellation)?;
        Self::open_once(&config, None, cache_lease)
    }

    /// Open services under exclusive cache initialization ownership, retrying
    /// transient SQLite contention until the caller cancels.
    pub fn open_cancellable(config: Config, cancellation: &CancellationToken) -> Result<Self> {
        match Self::open_cancellable_managed(config.clone(), cancellation) {
            Ok(services) => Ok(services),
            Err(error) if should_use_repository_cache_fallback(&config, &error) => {
                let fallback = prepare_repository_cache_fallback(&config)?;
                tracing::warn!(
                    preferred_database = %config.database_path.display(),
                    fallback_database = %fallback.database_path.display(),
                    "managed cache is not writable; using repository-local fallback"
                );
                Self::open_cancellable_managed(fallback, cancellation)
            }
            Err(error) => Err(error),
        }
    }

    fn open_cancellable_managed(config: Config, cancellation: &CancellationToken) -> Result<Self> {
        config.validate()?;
        let coordination = IndexCoordination::for_database(&config.database_path);
        let cache_lease = coordination.acquire_cache_lease(cancellation)?;
        let _initialization = coordination.acquire_initialization(cancellation)?;
        let mut delay = STARTUP_RETRY_INITIAL_DELAY;
        let mut attempt = 0u32;

        loop {
            validation::check_cancelled(cancellation)?;
            match Self::open_once(&config, Some(STARTUP_BUSY_TIMEOUT), cache_lease.clone()) {
                Ok(services) => return Ok(services),
                Err(error) if is_database_contention(&error) => {
                    attempt = attempt.saturating_add(1);
                    if attempt == 1 || attempt.is_multiple_of(20) {
                        tracing::warn!(
                            attempt,
                            retry_delay_ms = delay.as_millis(),
                            database = %config.database_path.display(),
                            %error,
                            "cache initialization is waiting for SQLite contention"
                        );
                    }
                    wait_cancellable(cancellation, delay)?;
                    delay = delay.saturating_mul(2).min(STARTUP_RETRY_MAX_DELAY);
                }
                Err(error) => return Err(error),
            }
        }
    }

    fn open_once(
        config: &Config,
        startup_timeout: Option<Duration>,
        cache_lease: CacheLease,
    ) -> Result<Self> {
        let open_storage = || match startup_timeout {
            Some(timeout) => Storage::open_for_repository_scoped_with_startup_timeout(
                &config.database_path,
                &config.root,
                config.index_scope().full_digest(),
                timeout,
            ),
            None => Storage::open_for_repository_scoped(
                &config.database_path,
                &config.root,
                config.index_scope().full_digest(),
            ),
        };
        let storage = match open_storage() {
            Ok(storage) => storage,
            Err(error) if config.database_is_managed_cache && is_database_corruption(&error) => {
                tracing::warn!(database = %config.database_path.display(), "rebuilding corrupt managed index");
                remove_database_artifacts(&config.database_path)?;
                open_storage()?
            }
            Err(error) => return Err(error),
        };
        Self::from_parts(Arc::new(config.clone()), storage, cache_lease)
    }

    fn from_parts(config: Arc<Config>, storage: Storage, cache_lease: CacheLease) -> Result<Self> {
        let tokenizer = config.tokenizer;
        let indexer = Indexer::new(Arc::clone(&config), storage.clone())?;
        let repository_root = indexer.repository_root();
        let coordination = IndexCoordination::for_database(&config.database_path);
        let active_reconciliations = Arc::new(AtomicUsize::new(0));
        let reconciliation_changed = Arc::new(tokio::sync::Notify::new());
        let reconciliation = reconciliation::ReconciliationCoordinator::new(
            indexer.clone(),
            coordination.clone(),
            Arc::clone(&active_reconciliations),
            Arc::clone(&reconciliation_changed),
        );
        let observer = observer::ServiceObserver::new(storage.clone(), tokenizer);
        Ok(Self {
            config,
            storage,
            indexer,
            repository_root,
            coordination,
            _cache_lease: cache_lease,
            active_reconciliations,
            reconciliation_changed,
            read_deltas: Arc::new(read_delta::ReadDeltaRegistry::default()),
            blocking_executor: executor::BlockingExecutor::default(),
            response_accountant: accounting::ResponseAccountant::new(tokenizer),
            observer,
            reconciliation,
        })
    }
}

fn should_use_repository_cache_fallback(config: &Config, error: &Error) -> bool {
    config.repository_cache_fallback().is_some()
        && matches!(
            error,
            Error::Io(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::PermissionDenied
                        | std::io::ErrorKind::ReadOnlyFilesystem
                )
        )
}

fn prepare_repository_cache_fallback(config: &Config) -> Result<Config> {
    let fallback = config
        .repository_cache_fallback()
        .expect("fallback eligibility checked by caller");
    let cache_root = config.root.join(".leantoken");
    match fs::symlink_metadata(&cache_root) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
            return Err(Error::InvalidConfiguration(
                "repository cache fallback must be a real directory".into(),
            ));
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            match fs::create_dir(&cache_root) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error.into()),
            }
        }
        Err(error) => return Err(error.into()),
    }
    let canonical_cache = cache_root.canonicalize()?;
    if !canonical_cache.starts_with(&config.root) {
        return Err(Error::PathOutsideRoot(canonical_cache));
    }
    let ignore_path = cache_root.join(".gitignore");
    match OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&ignore_path)
    {
        Ok(mut ignore) => {
            ignore.write_all(b"*\n")?;
            ignore.sync_all()?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error.into()),
    }
    Ok(fallback)
}

fn is_database_corruption(error: &Error) -> bool {
    matches!(
        sqlite_error_code(error),
        Some(rusqlite::ErrorCode::DatabaseCorrupt | rusqlite::ErrorCode::NotADatabase)
    )
}

fn remove_database_artifacts(database: &std::path::Path) -> Result<()> {
    for suffix in ["", "-wal", "-shm"] {
        let mut path = database.as_os_str().to_os_string();
        path.push(suffix);
        match fs::remove_file(std::path::PathBuf::from(path)) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

fn wait_cancellable(cancellation: &CancellationToken, duration: Duration) -> Result<()> {
    let deadline = Instant::now() + duration;
    loop {
        validation::check_cancelled(cancellation)?;
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Ok(());
        }
        thread::sleep(remaining.min(CANCELLATION_POLL_INTERVAL));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn managed_permission_failure_selects_self_ignored_repository_cache() {
        let root = tempfile::tempdir().expect("repository");
        let config = Config::discover(root.path(), None).expect("managed config");
        let denied = Error::Io(std::io::Error::from(std::io::ErrorKind::PermissionDenied));

        assert!(should_use_repository_cache_fallback(&config, &denied));
        let fallback = prepare_repository_cache_fallback(&config).expect("fallback config");
        assert!(fallback.uses_repository_cache_fallback());
        assert!(
            fallback
                .database_path
                .starts_with(root.path().join(".leantoken"))
        );
        assert_eq!(
            fs::read_to_string(root.path().join(".leantoken/.gitignore")).expect("fallback ignore"),
            "*\n"
        );

        // Preparation is idempotent and never replaces an existing ignore file.
        prepare_repository_cache_fallback(&config).expect("repeat fallback preparation");
    }

    #[test]
    fn explicit_database_never_uses_repository_cache_fallback() {
        let root = tempfile::tempdir().expect("repository");
        let config = Config::discover(root.path(), Some(root.path().join("index.sqlite")))
            .expect("explicit config");
        let denied = Error::Io(std::io::Error::from(std::io::ErrorKind::PermissionDenied));

        assert!(!should_use_repository_cache_fallback(&config, &denied));
        assert!(config.repository_cache_fallback().is_none());
    }

    #[cfg(unix)]
    #[test]
    fn repository_cache_fallback_rejects_symlinked_directory() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().expect("repository");
        let outside = tempfile::tempdir().expect("outside");
        symlink(outside.path(), root.path().join(".leantoken")).expect("cache symlink");
        let config = Config::discover(root.path(), None).expect("managed config");

        let error = prepare_repository_cache_fallback(&config).expect_err("reject symlink");
        assert!(matches!(error, Error::InvalidConfiguration(_)));
    }
}
