use super::*;

pub(super) const RUNTIME_LEASE_DIRECTORY: &str = ".leases";

/// Shared lifetime ownership of one verified application-owned runtime artifact.
///
/// The operating system releases ownership when this guard or its process exits.
#[derive(Debug)]
pub struct RuntimeLease {
    _file: fs::File,
}

/// Protect the current executable when it is a verified managed private runtime.
///
/// Unmarked executables outside the configured runtime root need no lease. Older
/// executables that do not call this function cannot advertise their lifetime.
/// The parsed setup marker opts a valid installation path into managed lifetime
/// protection independently of home/data environment changes. Unmarked paths
/// require a resolvable setup home and a matching configured runtime root.
pub fn acquire_current_runtime_lease(managed_by_setup: bool) -> Result<Option<RuntimeLease>> {
    let executable = std::env::current_exe()?.canonicalize()?;
    acquire_executable_runtime_lease(&executable, managed_by_setup, || {
        // Use setup's HOME/Windows USERPROFILE fallbacks for unmarked runs too.
        home_directory().map(|home| setup_runtime_root(&home))
    })
}

fn acquire_executable_runtime_lease(
    executable: &Path,
    managed_by_setup: bool,
    resolve_configured_root: impl FnOnce() -> Option<PathBuf>,
) -> Result<Option<RuntimeLease>> {
    let Some(directory) = executable.parent() else {
        return Ok(None);
    };
    if executable.file_name() != Some(std::ffi::OsStr::new(runtime_executable_name(cfg!(windows))))
    {
        return Ok(None);
    }
    let Some(directory_name) = directory.file_name().and_then(|name| name.to_str()) else {
        return Ok(None);
    };
    let Some((version, named_digest)) = parse_runtime_directory_name(directory_name) else {
        return Ok(None);
    };
    let Some(runtime_root) =
        executable_runtime_root(directory, managed_by_setup, resolve_configured_root)?
    else {
        return Ok(None);
    };
    if version.to_string() != env!("CARGO_PKG_VERSION") {
        return Err(Error::SetupFailure(
            "private runtime release identity mismatch".into(),
        ));
    }
    let root = open_runtime_root(&runtime_root)?;
    let directory_metadata = root.symlink_metadata(directory_name)?;
    if !directory_metadata.file_type().is_dir() || directory_metadata.file_type().is_symlink() {
        return Err(Error::SetupFailure(
            "private runtime artifact is not a non-symlink directory".into(),
        ));
    }
    let directory_identity = capability_directory_identity(&directory_metadata);
    let digest = file_digest(executable)?;
    if named_digest.is_some_and(|expected| expected != digest) {
        return Err(Error::SetupFailure(
            "private runtime artifact digest mismatch".into(),
        ));
    }
    let lease = open_runtime_lease(&runtime_root, &root, Path::new(directory_name), true)?
        .expect("creating a runtime lease returns its file");
    lease.file.lock_shared()?;
    revalidate_runtime_lease(&runtime_root, &root, &lease)?;
    // Prune may have held the exclusive lease while this process was starting.
    // Revalidate after acquiring ownership before serving any request.
    validate_runtime_root_handle(&runtime_root, &root)?;
    let current = root.symlink_metadata(directory_name)?;
    if !current.file_type().is_dir()
        || current.file_type().is_symlink()
        || capability_directory_identity(&current) != directory_identity
    {
        return Err(Error::SetupFailure(
            "private runtime artifact changed during startup".into(),
        ));
    }
    let artifact = root.open_dir(directory_name)?;
    if capability_directory_identity(&artifact.dir_metadata()?) != directory_identity
        || !capability_runtime_file_matches(
            &artifact,
            Path::new(runtime_executable_name(cfg!(windows))),
            &digest,
        )?
    {
        return Err(Error::SetupFailure(
            "private runtime artifact changed during startup".into(),
        ));
    }
    Ok(Some(RuntimeLease { _file: lease.file }))
}

fn executable_runtime_root(
    directory: &Path,
    managed_by_setup: bool,
    resolve_configured_root: impl FnOnce() -> Option<PathBuf>,
) -> Result<Option<PathBuf>> {
    if managed_by_setup {
        // The parsed marker is an explicit lifecycle opt-in, not filesystem
        // authority. The caller already checked the canonical executable's
        // native basename and runtime namespace; all capability/digest checks
        // still apply before a lease is acquired in this installation root.
        return Ok(directory
            .parent()
            .filter(|root| root.file_name() == Some(std::ffi::OsStr::new("runtimes")))
            .map(Path::to_path_buf));
    }
    let Some(runtime_root) = resolve_configured_root() else {
        // A version-shaped parent alone does not establish managed ownership.
        return Ok(None);
    };
    let canonical_root = match runtime_root.canonicalize() {
        Ok(root) => root,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    Ok((directory.parent() == Some(canonical_root.as_path())).then_some(runtime_root))
}

pub(super) fn runtime_lease_is_active(runtime_root: &Path, directory_name: &Path) -> Result<bool> {
    let root = open_runtime_root(runtime_root)?;
    let Some(lease) = open_runtime_lease(runtime_root, &root, directory_name, false)? else {
        // A cooperating process creates its stable lease before serving requests.
        return Ok(false);
    };
    let active = !try_exclusive_lease(&lease.file)?;
    revalidate_runtime_lease(runtime_root, &root, &lease)?;
    Ok(active)
}

pub(super) fn try_acquire_runtime_prune_lease(
    runtime_root: &Path,
    root: &cap_std::fs::Dir,
    directory_name: &Path,
) -> Result<Option<RuntimeLease>> {
    let lease = open_runtime_lease(runtime_root, root, directory_name, true)?
        .expect("creating a runtime lease returns its file");
    let exclusive = try_exclusive_lease(&lease.file)?;
    revalidate_runtime_lease(runtime_root, root, &lease)?;
    if exclusive {
        Ok(Some(RuntimeLease { _file: lease.file }))
    } else {
        Ok(None)
    }
}

fn try_exclusive_lease(file: &fs::File) -> Result<bool> {
    loop {
        match file.try_lock() {
            Ok(()) => return Ok(true),
            Err(std::fs::TryLockError::WouldBlock) => return Ok(false),
            Err(std::fs::TryLockError::Error(error))
                if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(std::fs::TryLockError::Error(error)) => return Err(error.into()),
        }
    }
}

struct OpenedRuntimeLease {
    file: fs::File,
    identity: RuntimeDirectoryIdentity,
    directory: cap_std::fs::Dir,
    name: String,
}

fn revalidate_runtime_lease(
    runtime_root: &Path,
    root: &cap_std::fs::Dir,
    lease: &OpenedRuntimeLease,
) -> Result<()> {
    validate_runtime_root_handle(runtime_root, root)?;
    let directory = root.symlink_metadata(RUNTIME_LEASE_DIRECTORY)?;
    let file = lease.directory.symlink_metadata(&lease.name)?;
    if !directory.file_type().is_dir()
        || directory.file_type().is_symlink()
        || capability_directory_identity(&directory)
            != capability_directory_identity(&lease.directory.dir_metadata()?)
        || !safe_lease_metadata(&file)
        || capability_directory_identity(&file) != lease.identity
        || runtime_directory_identity(&lease.file.metadata()?) != lease.identity
    {
        return Err(Error::SetupFailure(
            "private runtime lease changed after acquisition".into(),
        ));
    }
    Ok(())
}

fn open_runtime_lease(
    runtime_root: &Path,
    root: &cap_std::fs::Dir,
    directory_name: &Path,
    create: bool,
) -> Result<Option<OpenedRuntimeLease>> {
    validate_runtime_root_handle(runtime_root, root)?;
    if create {
        match root.create_dir(RUNTIME_LEASE_DIRECTORY) {
            Ok(()) => sync_capability_directory(root)?,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error.into()),
        }
    }
    let before = match root.symlink_metadata(RUNTIME_LEASE_DIRECTORY) {
        Ok(metadata) => metadata,
        Err(error) if !create && error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    if !before.file_type().is_dir() || before.file_type().is_symlink() {
        return Err(Error::SetupFailure(
            "private runtime lease root must be a non-symlink directory".into(),
        ));
    }
    let identity = capability_directory_identity(&before);
    let leases = root.open_dir(RUNTIME_LEASE_DIRECTORY)?;
    if capability_directory_identity(&leases.dir_metadata()?) != identity {
        return Err(Error::SetupFailure(
            "private runtime lease root changed while opening".into(),
        ));
    }
    let name = directory_name
        .to_str()
        .filter(|name| parse_runtime_directory_name(name).is_some())
        .ok_or_else(|| Error::SetupFailure("invalid runtime lease artifact identity".into()))?;
    // Hash the complete directory identity, including the legacy/new namespace,
    // so lock names remain bounded and different artifacts never share a lease.
    let lease_name = format!("{}.lock", blake3::hash(name.as_bytes()).to_hex());
    if create {
        let mut options = cap_std::fs::OpenOptions::new();
        options.read(true).write(true).create_new(true);
        match leases.open_with(&lease_name, &options) {
            Ok(file) => {
                file.sync_all()?;
                sync_capability_directory(&leases)?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error.into()),
        }
    }
    let before_file = match leases.symlink_metadata(&lease_name) {
        Ok(metadata) => metadata,
        Err(error) if !create && error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    if !safe_lease_metadata(&before_file) {
        return Err(Error::SetupFailure(
            "private runtime lease must be an empty non-symlink file with one link".into(),
        ));
    }
    let file_identity = capability_directory_identity(&before_file);
    let mut options = cap_std::fs::OpenOptions::new();
    options.read(true).write(true);
    #[cfg(unix)]
    {
        use cap_std::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    let file = leases.open_with(&lease_name, &options)?;
    let opened = file.metadata()?;
    let current_file = leases.symlink_metadata(&lease_name)?;
    let current_directory = root.symlink_metadata(RUNTIME_LEASE_DIRECTORY)?;
    validate_runtime_root_handle(runtime_root, root)?;
    if !safe_lease_metadata(&opened)
        || !safe_lease_metadata(&current_file)
        || capability_directory_identity(&opened) != file_identity
        || capability_directory_identity(&current_file) != file_identity
        || !current_directory.file_type().is_dir()
        || current_directory.file_type().is_symlink()
        || capability_directory_identity(&current_directory)
            != capability_directory_identity(&leases.dir_metadata()?)
    {
        return Err(Error::SetupFailure(
            "private runtime lease identity changed while opening".into(),
        ));
    }
    Ok(Some(OpenedRuntimeLease {
        file: file.into_std(),
        identity: file_identity,
        directory: leases,
        name: lease_name,
    }))
}

fn safe_lease_metadata(metadata: &cap_std::fs::Metadata) -> bool {
    if !metadata.file_type().is_file() || metadata.file_type().is_symlink() || metadata.len() != 0 {
        return false;
    }
    #[cfg(unix)]
    {
        use cap_std::fs::MetadataExt;
        metadata.nlink() == 1
    }
    #[cfg(not(unix))]
    {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn executable_fixture(root: &Path, namespace: &str, name: &str) -> PathBuf {
        let directory = root.join(namespace);
        fs::create_dir_all(&directory).unwrap();
        let executable = directory.join(name);
        fs::write(&executable, b"standalone executable fixture").unwrap();
        executable.canonicalize().unwrap()
    }

    #[test]
    fn standalone_versioned_executable_needs_no_home_before_managed_ownership() {
        let temporary = tempfile::tempdir().unwrap();
        let directory = temporary.path().join("1.2.3");
        fs::create_dir(&directory).unwrap();
        let executable = directory.join(runtime_executable_name(cfg!(windows)));
        fs::write(&executable, b"standalone executable fixture").unwrap();

        let lease =
            acquire_executable_runtime_lease(&executable.canonicalize().unwrap(), false, || None)
                .expect("an unavailable setup home cannot establish managed ownership");

        assert!(lease.is_none());
        assert_eq!(
            fs::read(&executable).unwrap(),
            b"standalone executable fixture"
        );
        assert_eq!(fs::read_dir(temporary.path()).unwrap().count(), 1);
        assert_eq!(fs::read_dir(&directory).unwrap().count(), 1);
    }

    #[test]
    fn marked_installations_hold_their_original_lease_without_home_rediscovery() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("runtimes");
        let digest = blake3::hash(b"standalone executable fixture").to_hex();
        for namespace in [
            env!("CARGO_PKG_VERSION").to_owned(),
            format!("{}_{digest}", env!("CARGO_PKG_VERSION")),
        ] {
            let executable =
                executable_fixture(&root, &namespace, runtime_executable_name(cfg!(windows)));
            let lease = acquire_executable_runtime_lease(&executable, true, || {
                panic!("a marked installation must not rediscover its root from home")
            })
            .unwrap()
            .expect("valid marked installation acquires its lifetime lease");

            assert!(runtime_lease_is_active(&root, Path::new(&namespace)).unwrap());
            drop(lease);
            assert!(!runtime_lease_is_active(&root, Path::new(&namespace)).unwrap());
        }
    }

    #[test]
    fn unmarked_installations_require_the_same_configured_root() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("runtimes");
        let digest = blake3::hash(b"standalone executable fixture").to_hex();
        let namespace = format!("{}_{digest}", env!("CARGO_PKG_VERSION"));
        let executable =
            executable_fixture(&root, &namespace, runtime_executable_name(cfg!(windows)));
        let lease = acquire_executable_runtime_lease(&executable, false, || Some(root.clone()))
            .unwrap()
            .expect("an unmarked configured installation acquires its lifetime lease");
        let other_root = temporary.path().join("different-runtimes");
        fs::create_dir(&other_root).unwrap();

        assert!(
            acquire_executable_runtime_lease(&executable, false, || Some(other_root.clone()))
                .unwrap()
                .is_none()
        );
        assert!(!other_root.join(RUNTIME_LEASE_DIRECTORY).exists());
        assert!(runtime_lease_is_active(&root, Path::new(&namespace)).unwrap());
        drop(lease);
    }

    #[test]
    fn marker_alone_does_not_recognize_an_arbitrary_executable_path() {
        let temporary = tempfile::tempdir().unwrap();
        for (root_name, namespace, executable_name) in [
            (
                "runtimes",
                "not-a-version",
                runtime_executable_name(cfg!(windows)),
            ),
            (
                "runtimes",
                "1.2.3_short",
                runtime_executable_name(cfg!(windows)),
            ),
            (
                "standalone",
                "1.2.3",
                runtime_executable_name(cfg!(windows)),
            ),
            ("runtimes", env!("CARGO_PKG_VERSION"), "other-executable"),
        ] {
            let root = temporary.path().join(root_name);
            let executable = executable_fixture(&root, namespace, executable_name);
            let lease = acquire_executable_runtime_lease(&executable, true, || {
                panic!("a marked path must not fall back to home discovery")
            })
            .unwrap();

            assert!(lease.is_none(), "{}", executable.display());
            assert!(!root.join(RUNTIME_LEASE_DIRECTORY).exists());
            assert_eq!(
                fs::read(&executable).unwrap(),
                b"standalone executable fixture"
            );
        }
    }

    #[test]
    fn marked_installations_still_require_exact_release_and_named_digest() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("runtimes");
        let digest = blake3::hash(b"standalone executable fixture").to_hex();
        for (namespace, expected) in [
            (
                format!("{}_{}", env!("CARGO_PKG_VERSION"), "0".repeat(64)),
                "artifact digest mismatch",
            ),
            (
                format!("0.0.0-incorrect_{digest}"),
                "release identity mismatch",
            ),
        ] {
            let executable =
                executable_fixture(&root, &namespace, runtime_executable_name(cfg!(windows)));
            let error = acquire_executable_runtime_lease(&executable, true, || {
                panic!("marked identity validation must not depend on home")
            })
            .expect_err("invalid marked identity must fail closed");

            assert!(error.to_string().contains(expected), "{error}");
            assert!(!root.join(RUNTIME_LEASE_DIRECTORY).exists());
        }
    }
}
