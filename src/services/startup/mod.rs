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
        reject_symlinked_managed_database_artifacts(&config)?;
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
        reject_symlinked_managed_database_artifacts(&config)?;
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
        reject_symlinked_managed_database_artifacts(config)?;
        // Migration 12 moves these best-effort counters out of the generation
        // database. Preserve them before Storage runs that destructive step.
        let migration = {
            let instrumentation =
                InstrumentationStorage::open(&config.instrumentation_database_path());
            instrumentation.migrate_legacy_primary(&config.database_path)
        };
        if let Err(error) = migration
            && !(config.database_is_managed_cache() && is_database_corruption(&error))
        {
            return Err(error);
        }
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
            Err(error) if config.database_is_managed_cache() && is_database_corruption(&error) => {
                tracing::warn!(database = %config.database_path.display(), "rebuilding corrupt managed index");
                remove_managed_database_artifacts(config)?;
                open_storage()?
            }
            Err(error) => return Err(error),
        };
        Self::from_parts(Arc::new(config.clone()), storage, cache_lease)
    }

    fn from_parts(config: Arc<Config>, storage: Storage, cache_lease: CacheLease) -> Result<Self> {
        let tokenizer = config.tokenizer;
        let artifacts = ArtifactStorage::open(&config.artifact_database_path());
        let instrumentation = InstrumentationStorage::open(&config.instrumentation_database_path());
        let context_exclude_paths = validation::PathMatcher::new(&config.context_exclude_paths)?;
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
        let observer = observer::ServiceObserver::new(instrumentation.clone(), tokenizer);
        Ok(Self {
            config,
            storage,
            artifacts,
            instrumentation,
            indexer,
            repository_root,
            coordination,
            _cache_lease: cache_lease,
            active_reconciliations,
            reconciliation_changed,
            blocking_executor: executor::BlockingExecutor::default(),
            response_accountant: accounting::ResponseAccountant::new(tokenizer),
            observer,
            reconciliation,
            context_exclude_paths,
        })
    }
}

fn reject_symlinked_managed_database_artifacts(config: &Config) -> Result<()> {
    if !config.database_is_managed_cache() {
        return Ok(());
    }
    let instrumentation = config.instrumentation_database_path();
    let artifacts = config.artifact_database_path();
    let databases = [
        config.database_path.as_path(),
        instrumentation.as_path(),
        artifacts.as_path(),
    ];
    let database_artifacts = databases.into_iter().flat_map(|database| {
        ["", "-wal", "-shm", "-journal"].map(move |suffix| {
            let mut path = database.as_os_str().to_os_string();
            path.push(suffix);
            std::path::PathBuf::from(path)
        })
    });
    let coordination_artifacts = crate::coordination::COORDINATION_LOCK_SUFFIXES.map(|suffix| {
        let mut path = config.database_path.as_os_str().to_os_string();
        path.push(suffix);
        std::path::PathBuf::from(path)
    });
    for path in database_artifacts.chain(coordination_artifacts) {
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.into()),
        };
        if metadata.file_type().is_symlink() {
            return Err(Error::InvalidConfiguration(
                "managed database and coordination artifacts must not be symlinks".into(),
            ));
        }
    }
    Ok(())
}

fn should_use_repository_cache_fallback(config: &Config, error: &Error) -> bool {
    config.repository_cache_fallback().is_some()
        && (matches!(
            error,
            Error::Io(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::PermissionDenied
                        | std::io::ErrorKind::ReadOnlyFilesystem
                )
        ) || sqlite_error_code(error) == Some(rusqlite::ErrorCode::ReadOnly))
}

fn prepare_repository_cache_fallback(config: &Config) -> Result<Config> {
    let mut fallback = config.repository_cache_fallback().ok_or_else(|| {
        Error::InvalidConfiguration("repository cache fallback requested but not configured".into())
    })?;
    let database_parent = fallback
        .database_path
        .parent()
        .map(std::path::Path::to_path_buf)
        .ok_or_else(|| {
            Error::InvalidConfiguration("repository cache fallback has no parent directory".into())
        })?;
    let relative_parent = database_parent
        .strip_prefix(&config.root)
        .map_err(|_| Error::PathOutsideRoot(database_parent.clone()))?;
    let repository = Dir::open_ambient_dir(&config.root, cap_std::ambient_authority())?;
    ensure_real_directories(&repository, relative_parent)?;
    let database_name = fallback
        .database_path
        .file_name()
        .ok_or_else(|| {
            Error::InvalidConfiguration(
                "repository cache fallback database path has no file name".into(),
            )
        })?
        .to_owned();
    fallback.database_path = database_parent.canonicalize()?.join(database_name);

    let cache_root = config.root.join(".leantoken");
    let canonical_cache = cache_root.canonicalize()?;
    if !canonical_cache.starts_with(&config.root) {
        return Err(Error::PathOutsideRoot(canonical_cache));
    }
    let ignore_path = cache_root.join(".gitignore");
    ensure_effective_cache_ignore(&ignore_path)?;
    Ok(fallback)
}

fn ensure_effective_cache_ignore(path: &std::path::Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
            return Err(Error::InvalidConfiguration(
                "repository cache .gitignore must be a regular file".into(),
            ));
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let mut ignore = OpenOptions::new().write(true).create_new(true).open(path)?;
            ignore.write_all(b"*\n")?;
            ignore.sync_all()?;
            return Ok(());
        }
        Err(error) => return Err(error.into()),
    }
    let contents = fs::read_to_string(path)?;
    if contents.lines().any(|line| line.trim() == "*") {
        return Ok(());
    }
    let mut ignore = OpenOptions::new().append(true).open(path)?;
    if !contents.is_empty() && !contents.ends_with('\n') {
        ignore.write_all(b"\n")?;
    }
    ignore.write_all(b"*\n")?;
    ignore.sync_all()?;
    Ok(())
}

fn ensure_real_directories(repository: &Dir, relative: &std::path::Path) -> Result<()> {
    let mut current = std::path::PathBuf::new();
    for component in relative.components() {
        let std::path::Component::Normal(component) = component else {
            return Err(Error::InvalidConfiguration(
                "repository cache fallback must use normal relative path components".into(),
            ));
        };
        current.push(component);
        match repository.symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
                return Err(Error::InvalidConfiguration(
                    "repository cache fallback must contain only real directories".into(),
                ));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                match repository.create_dir(&current) {
                    Ok(()) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                        let metadata = repository.symlink_metadata(&current)?;
                        if metadata.file_type().is_symlink() || !metadata.is_dir() {
                            return Err(Error::InvalidConfiguration(
                                "repository cache fallback must contain only real directories"
                                    .into(),
                            ));
                        }
                    }
                    Err(error) => return Err(error.into()),
                }
            }
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

fn is_database_corruption(error: &Error) -> bool {
    matches!(error, Error::InvalidIndexGeneration { .. })
        || matches!(
            sqlite_error_code(error),
            Some(rusqlite::ErrorCode::DatabaseCorrupt | rusqlite::ErrorCode::NotADatabase)
        )
}

fn remove_database_artifacts(database: &std::path::Path) -> Result<()> {
    for suffix in ["", "-wal", "-shm", "-journal"] {
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

fn remove_managed_database_artifacts(config: &Config) -> Result<()> {
    for database in [
        config.database_path.clone(),
        config.artifact_database_path(),
        config.instrumentation_database_path(),
    ] {
        remove_database_artifacts(&database)?;
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
                .starts_with(config.root.join(".leantoken"))
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

    #[test]
    fn managed_sqlite_read_only_failure_selects_repository_cache() {
        let root = tempfile::tempdir().expect("repository");
        let config = Config::discover(root.path(), None).expect("managed config");
        let read_only = Error::Sqlite(rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_READONLY),
            None,
        ));

        assert!(should_use_repository_cache_fallback(&config, &read_only));
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
        assert!(
            matches!(
                error,
                Error::InvalidConfiguration(_) | Error::PathOutsideRoot(_)
            ),
            "{error}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn repository_cache_fallback_rejects_symlinked_version_directory() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().expect("repository");
        let outside = tempfile::tempdir().expect("outside");
        fs::create_dir(root.path().join(".leantoken")).expect("cache root");
        let config = Config::discover(root.path(), None).expect("managed config");
        let version_directory = config
            .repository_cache_fallback()
            .expect("fallback")
            .database_path
            .parent()
            .and_then(std::path::Path::file_name)
            .expect("version directory")
            .to_owned();
        symlink(
            outside.path(),
            root.path().join(".leantoken").join(version_directory),
        )
        .expect("version symlink");

        let error = prepare_repository_cache_fallback(&config).expect_err("reject symlink");
        assert!(
            matches!(
                error,
                Error::InvalidConfiguration(_) | Error::PathOutsideRoot(_)
            ),
            "{error}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn managed_database_symlink_is_rejected_without_mutating_target() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().expect("repository");
        let target = tempfile::NamedTempFile::new().expect("external target");
        fs::write(target.path(), b"external sentinel").expect("sentinel");
        let link = root.path().join("index.sqlite");
        symlink(target.path(), &link).expect("database symlink");

        let mut config = Config::discover(root.path(), Some(link.clone())).expect("config");
        config.database_path = link;
        config.mark_database_as_managed_platform();

        let error = Services::open(config).expect_err("managed symlink must be rejected");
        assert!(matches!(error, Error::InvalidConfiguration(_)), "{error}");
        assert_eq!(
            fs::read(target.path()).expect("sentinel contents"),
            b"external sentinel"
        );
    }

    #[cfg(unix)]
    #[test]
    fn managed_database_journal_symlink_is_rejected_without_mutating_target() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().expect("repository");
        let target = tempfile::NamedTempFile::new().expect("external target");
        fs::write(target.path(), b"external journal sentinel").expect("sentinel");
        symlink(target.path(), root.path().join("index.sqlite-journal")).expect("journal symlink");

        let mut config =
            Config::discover(root.path(), Some(root.path().join("index.sqlite"))).expect("config");
        config.mark_database_as_managed_platform();

        let error = Services::open(config).expect_err("managed journal symlink must be rejected");
        assert!(matches!(error, Error::InvalidConfiguration(_)), "{error}");
        assert_eq!(
            fs::read(target.path()).expect("sentinel contents"),
            b"external journal sentinel"
        );
    }

    #[cfg(unix)]
    #[test]
    fn managed_instrumentation_symlink_is_rejected_without_mutating_target() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().expect("repository");
        let target = tempfile::NamedTempFile::new().expect("external target");
        fs::write(target.path(), b"external instrumentation sentinel").expect("sentinel");
        let mut config =
            Config::discover(root.path(), Some(root.path().join("index.sqlite"))).expect("config");
        let instrumentation = config.instrumentation_database_path();
        symlink(target.path(), instrumentation).expect("instrumentation symlink");
        config.mark_database_as_managed_platform();

        let error = Services::open(config).expect_err("managed instrumentation symlink rejected");
        assert!(matches!(error, Error::InvalidConfiguration(_)), "{error}");
        assert_eq!(
            fs::read(target.path()).expect("sentinel contents"),
            b"external instrumentation sentinel"
        );
    }

    #[cfg(unix)]
    #[test]
    fn managed_artifact_symlink_is_rejected_without_mutating_target() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().expect("repository");
        let target = tempfile::NamedTempFile::new().expect("external target");
        fs::write(target.path(), b"external artifact sentinel").expect("sentinel");
        let mut config =
            Config::discover(root.path(), Some(root.path().join("index.sqlite"))).expect("config");
        symlink(target.path(), config.artifact_database_path()).expect("artifact symlink");
        config.mark_database_as_managed_platform();

        let error = Services::open(config).expect_err("managed artifact symlink rejected");
        assert!(matches!(error, Error::InvalidConfiguration(_)), "{error}");
        assert_eq!(
            fs::read(target.path()).expect("sentinel contents"),
            b"external artifact sentinel"
        );
    }

    #[cfg(unix)]
    #[test]
    fn managed_coordination_symlink_is_rejected_before_acquiring_locks() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().expect("repository");
        let target = tempfile::NamedTempFile::new().expect("external target");
        fs::write(target.path(), b"external coordination sentinel").expect("sentinel");
        symlink(target.path(), root.path().join("index.sqlite.lease.lock")).expect("lease symlink");

        let mut config =
            Config::discover(root.path(), Some(root.path().join("index.sqlite"))).expect("config");
        config.mark_database_as_managed_platform();

        let error = Services::open(config).expect_err("managed lock symlink must be rejected");
        assert!(matches!(error, Error::InvalidConfiguration(_)), "{error}");
        assert_eq!(
            fs::read(target.path()).expect("sentinel contents"),
            b"external coordination sentinel"
        );
    }

    #[test]
    fn managed_corruption_rebuild_discards_auxiliary_generation_databases() {
        use crate::receipt::ReceiptEvidence;

        let root = tempfile::tempdir().expect("repository");
        let mut config =
            Config::discover(root.path(), Some(root.path().join("index.sqlite"))).expect("config");
        config.mark_database_as_managed_platform();
        let artifacts_path = config.artifact_database_path();
        let instrumentation_path = config.instrumentation_database_path();
        let receipt_id = ArtifactStorage::open(&artifacts_path)
            .evaluate_receipt(
                "repository",
                "old-incarnation",
                None,
                1,
                &[ReceiptEvidence::new(
                    "src/lib.rs",
                    1,
                    2,
                    "old-generation",
                    Some("fn old_generation() {}"),
                )],
                true,
            )
            .expect("old receipt")
            .receipt_id;
        fs::write(&instrumentation_path, b"not sqlite").expect("old instrumentation");
        fs::write(&config.database_path, b"not sqlite").expect("corrupt index");

        drop(Services::open(config).expect("rebuild managed index"));

        assert!(matches!(
            ArtifactStorage::open(&artifacts_path).read_receipt(
                "repository",
                "old-incarnation",
                &receipt_id,
            ),
            Err(Error::UnknownReceipt(_))
        ));
        assert_eq!(
            fs::read(&instrumentation_path)
                .expect("rebuilt instrumentation")
                .get(..16),
            Some(&b"SQLite format 3\0"[..])
        );
    }
}
