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
/// Direct executables outside the configured runtime root need no lease. Older
/// executables that do not call this function cannot advertise their lifetime.
pub fn acquire_current_runtime_lease() -> Result<Option<RuntimeLease>> {
    let executable = std::env::current_exe()?.canonicalize()?;
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
    // Match setup when Windows known folders are unavailable but HOME or
    // USERPROFILE still identifies the configured fallback runtime root.
    let home = home_directory()
        .ok_or_else(|| Error::SetupFailure("could not locate the home directory".into()))?;
    let runtime_root = setup_runtime_root(&home);
    let canonical_root = match runtime_root.canonicalize() {
        Ok(root) => root,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    if directory.parent() != Some(canonical_root.as_path()) {
        return Ok(None);
    }
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
    let digest = file_digest(&executable)?;
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
