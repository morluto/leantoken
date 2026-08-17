use super::*;

pub(super) const MAX_SETUP_FILE_BYTES: u64 = 8 * 1024 * 1024;
pub(super) const MAX_SETUP_JOURNAL_BYTES: u64 = 256 * 1024 * 1024;

#[derive(Debug, Clone, Copy)]
pub(super) enum EditStatus {
    Configured,
    Updated,
    AlreadyConfigured,
    Removed,
    NotConfigured,
}

impl fmt::Display for EditStatus {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let value = match self {
            Self::Configured => "configured",
            Self::Updated => "updated",
            Self::AlreadyConfigured => "already configured",
            Self::Removed => "removed",
            Self::NotConfigured => "not configured",
        };
        formatter.write_str(value)
    }
}

impl From<EditStatus> for ClientSetupOutcome {
    fn from(status: EditStatus) -> Self {
        match status {
            EditStatus::Configured => Self::Configured,
            EditStatus::Updated => Self::Updated,
            EditStatus::AlreadyConfigured => Self::AlreadyConfigured,
            EditStatus::Removed => Self::Removed,
            EditStatus::NotConfigured => Self::NotConfigured,
        }
    }
}

#[derive(Debug)]
pub(super) enum ResolvedEdit {
    Configured {
        original: Option<String>,
        updated: String,
    },
    Updated {
        original: Option<String>,
        updated: String,
    },
    AlreadyConfigured {
        original: Option<String>,
    },
    Removed {
        original: String,
        updated: String,
    },
    NotConfigured {
        original: Option<String>,
    },
}

impl ResolvedEdit {
    pub(super) const fn status(&self) -> EditStatus {
        match self {
            Self::Configured { .. } => EditStatus::Configured,
            Self::Updated { .. } => EditStatus::Updated,
            Self::AlreadyConfigured { .. } => EditStatus::AlreadyConfigured,
            Self::Removed { .. } => EditStatus::Removed,
            Self::NotConfigured { .. } => EditStatus::NotConfigured,
        }
    }

    pub(super) fn original(&self) -> Option<&str> {
        match self {
            Self::Configured { original, .. }
            | Self::Updated { original, .. }
            | Self::AlreadyConfigured { original }
            | Self::NotConfigured { original } => original.as_deref(),
            Self::Removed { original, .. } => Some(original),
        }
    }

    pub(super) fn updated(&self) -> Option<&str> {
        match self {
            Self::Configured { updated, .. }
            | Self::Updated { updated, .. }
            | Self::Removed { updated, .. } => Some(updated),
            Self::AlreadyConfigured { .. } | Self::NotConfigured { .. } => None,
        }
    }

    pub(super) const fn action(&self) -> ClientPlanAction {
        match self {
            Self::Configured { original: None, .. } => ClientPlanAction::Create,
            Self::Configured { .. } | Self::Updated { .. } => ClientPlanAction::Update,
            Self::AlreadyConfigured { .. } => ClientPlanAction::AlreadyCurrent,
            Self::Removed { .. } => ClientPlanAction::Remove,
            Self::NotConfigured { .. } => ClientPlanAction::NotConfigured,
        }
    }
}

pub(super) fn read_optional(path: &Path) -> Result<Option<String>> {
    read_optional_with_limit(path, MAX_SETUP_FILE_BYTES)
}

pub(super) fn read_optional_with_limit(path: &Path, max_bytes: u64) -> Result<Option<String>> {
    let file = match fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    if file.metadata()?.len() > max_bytes {
        return Err(setup_file_limit_error(path, max_bytes));
    }
    let mut bytes = Vec::new();
    file.take(max_bytes.saturating_add(1))
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > max_bytes {
        return Err(setup_file_limit_error(path, max_bytes));
    }
    String::from_utf8(bytes)
        .map(Some)
        .map_err(|error| invalid_config(path, error))
}

fn setup_file_limit_error(path: &Path, max_bytes: u64) -> Error {
    Error::SetupFailure(format!(
        "refusing to read setup file above the {max_bytes}-byte limit: {}",
        path.display()
    ))
}

pub(super) fn validate_setup_content_size(path: &Path, content: &str) -> Result<()> {
    if content.len() as u64 > MAX_SETUP_FILE_BYTES {
        return Err(Error::SetupFailure(format!(
            "refusing to write setup file above the {MAX_SETUP_FILE_BYTES}-byte limit: {}",
            path.display()
        )));
    }
    Ok(())
}

pub(super) fn write_if_changed(path: &Path, original: Option<&str>, updated: &str) -> Result<()> {
    validate_setup_content_size(path, updated)?;
    if original == Some(updated) {
        return Ok(());
    }
    let parent = path.parent().ok_or_else(|| {
        Error::SetupFailure(format!("config path has no parent: {}", path.display()))
    })?;
    fs::create_dir_all(parent)?;
    // Prepare the complete replacement before the conditional swap so the
    // compare-to-replace window contains no file I/O.
    let mut temporary = NamedTempFile::new_in(parent)?;
    temporary.write_all(updated.as_bytes())?;
    temporary.as_file_mut().sync_all()?;
    if let Ok(metadata) = fs::metadata(path) {
        temporary
            .as_file()
            .set_permissions(metadata.permissions())?;
    }
    // Compare-and-swap: re-read the entry and capture its no-follow identity
    // immediately before the conditional replacement.
    let on_disk = read_optional(path)?;
    if on_disk.as_deref() != original {
        return Err(changed_before_persist(path));
    }
    let identity = capture_entry_identity(path)?;
    replace_entry_conditionally(path, temporary, identity, original)?;
    sync_parent_directory(path)?;
    Ok(())
}

/// Stable identity of a directory entry captured without following symlinks.
/// The conditional replacement verifies that the entry being replaced is
/// exactly the entry that was validated moments earlier.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct EntryIdentity {
    device: u64,
    inode: u64,
}

#[cfg(unix)]
fn entry_identity(metadata: &fs::Metadata) -> EntryIdentity {
    use std::os::unix::fs::MetadataExt;
    EntryIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
    }
}

#[cfg(windows)]
fn entry_identity(metadata: &fs::Metadata) -> EntryIdentity {
    use std::os::windows::fs::MetadataExt;
    EntryIdentity {
        device: metadata.volume_serial_number(),
        inode: metadata.file_index(),
    }
}

/// No-follow snapshot of the entry at `path`, or `None` when it is absent.
fn capture_entry_identity(path: &Path) -> Result<Option<EntryIdentity>> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => Ok(Some(entry_identity(&metadata))),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn changed_before_persist(path: &Path) -> Error {
    Error::SetupFailure(format!(
        "configuration changed before persist: {}",
        path.display()
    ))
}

/// Atomically replace `path` with the pre-prepared `temporary` entry only when
/// the entry at `path` is the same regular file whose content was validated as
/// `original`.
///
/// On Linux and macOS the kernel exchange primitive makes the replacement
/// atomic with the compare: the fully-prepared temporary entry is swapped with
/// the current entry in one syscall, the displaced entry is re-read and
/// verified against the validated original, and the swap is reversed on any
/// mismatch, restoring rather than overwriting a concurrent modification. A
/// symlink or directory raced into `path` fails the verification and is
/// swapped back. Creation uses the no-replace primitive so a concurrently
/// created file is never clobbered. Filesystems without the primitive fall
/// back to re-verifying the entry identity immediately before a plain rename,
/// shrinking the remaining window to the rename syscall itself; Windows has no
/// conditional-replace primitive and always uses that path.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn replace_entry_conditionally(
    path: &Path,
    temporary: NamedTempFile,
    identity: Option<EntryIdentity>,
    original: Option<&str>,
) -> Result<()> {
    let (from, to) = (temporary.path().to_owned(), path);
    match (identity, original) {
        (Some(_), Some(_)) => match exchange_entries(&from, to) {
            Ok(()) => {}
            Err(error) if unsupported_exchange(&error) => {
                return replace_entry_portable(path, temporary, identity, original);
            }
            Err(rustix::io::Errno::NOENT) => {
                return Err(changed_before_persist(path));
            }
            Err(error) => return Err(Error::Io(std::io::Error::from(error))),
        },
        (None, None) => match noreplace_entries(&from, to) {
            Ok(()) => return Ok(()),
            Err(rustix::io::Errno::EXIST) => {
                return Err(changed_before_persist(path));
            }
            Err(error) if unsupported_exchange(&error) => {
                return replace_entry_portable(path, temporary, identity, original);
            }
            Err(error) => return Err(Error::Io(std::io::Error::from(error))),
        },
        _ => return Err(changed_before_persist(path)),
    }
    // `from` now names the entry displaced from `path`. Verify it is exactly
    // the validated entry; on any mismatch the guard reverses the swap so the
    // concurrent modification is restored, not overwritten.
    let mut guard = SwapGuard {
        from: &from,
        to,
        committed: false,
    };
    if !verify_displaced_entry(&from, identity, original)? {
        return Err(changed_before_persist(path));
    }
    guard.committed = true;
    // Remove the displaced entry before `write_if_changed` syncs the parent
    // directory, so the removal is covered by the same durable sync.
    drop(temporary);
    Ok(())
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn replace_entry_conditionally(
    path: &Path,
    temporary: NamedTempFile,
    identity: Option<EntryIdentity>,
    original: Option<&str>,
) -> Result<()> {
    replace_entry_portable(path, temporary, identity, original)
}

/// Fallback for platforms or filesystems without an exchange or no-replace
/// primitive: re-verify the entry immediately before a plain rename, shrinking
/// the remaining race window to the rename syscall itself.
fn replace_entry_portable(
    path: &Path,
    temporary: NamedTempFile,
    identity: Option<EntryIdentity>,
    original: Option<&str>,
) -> Result<()> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            if original.is_none() {
                return persist_rename(temporary, path);
            }
            return Err(changed_before_persist(path));
        }
        Err(error) => return Err(error.into()),
    };
    if !metadata.file_type().is_file() || Some(entry_identity(&metadata)) != identity {
        return Err(changed_before_persist(path));
    }
    if read_optional(path)?.as_deref() != original {
        return Err(changed_before_persist(path));
    }
    persist_rename(temporary, path)
}

fn persist_rename(temporary: NamedTempFile, path: &Path) -> Result<()> {
    temporary
        .persist(path)
        .map(|_| ())
        .map_err(|error| Error::Io(error.error))
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn exchange_entries(from: &Path, to: &Path) -> rustix::io::Result<()> {
    rustix::fs::renameat_with(
        rustix::fs::CWD,
        from,
        rustix::fs::CWD,
        to,
        rustix::fs::RenameFlags::EXCHANGE,
    )
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn noreplace_entries(from: &Path, to: &Path) -> rustix::io::Result<()> {
    rustix::fs::renameat_with(
        rustix::fs::CWD,
        from,
        rustix::fs::CWD,
        to,
        rustix::fs::RenameFlags::NOREPLACE,
    )
}

/// Errors indicating the filesystem rejects the exchange or no-replace
/// primitive, in which case the portable fallback applies.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn unsupported_exchange(error: &rustix::io::Errno) -> bool {
    matches!(
        *error,
        rustix::io::Errno::INVAL | rustix::io::Errno::NOSYS | rustix::io::Errno::NOTSUP
    )
}

/// Reverses a conditional swap unless it committed, restoring the entry that
/// was displaced from `path` when verification found a concurrent change.
#[cfg(any(target_os = "linux", target_os = "macos"))]
struct SwapGuard<'a> {
    from: &'a Path,
    to: &'a Path,
    committed: bool,
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
impl Drop for SwapGuard<'_> {
    fn drop(&mut self) {
        if !self.committed
            && let Err(error) = exchange_entries(self.to, self.from)
        {
            tracing::warn!(
                %error,
                "could not restore a concurrent setup-path modification after a failed conditional replace"
            );
        }
    }
}

/// Verify that the entry displaced into `from` is the exact regular file whose
/// content was validated as `original`.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn verify_displaced_entry(
    from: &Path,
    identity: Option<EntryIdentity>,
    original: Option<&str>,
) -> Result<bool> {
    let Some(expected) = identity else {
        return Ok(false);
    };
    let metadata = match fs::symlink_metadata(from) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.into()),
    };
    if !metadata.file_type().is_file() || entry_identity(&metadata) != expected {
        return Ok(false);
    }
    Ok(read_optional(from)?.as_deref() == original)
}

#[cfg(unix)]
pub(super) fn sync_parent_directory(path: &Path) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| Error::SetupFailure(format!("path has no parent: {}", path.display())))?;
    fs::File::open(parent)?.sync_all()?;
    Ok(())
}

#[cfg(not(unix))]
pub(super) fn sync_parent_directory(_path: &Path) -> Result<()> {
    // Windows does not expose the Unix directory-fsync contract through
    // std::fs. File contents are still synced before atomic replacement.
    Ok(())
}

pub(super) fn invalid_config(path: &Path, error: impl fmt::Display) -> Error {
    Error::SetupFailure(format!(
        "refusing to overwrite malformed config {}: {error}",
        path.display()
    ))
}

pub(super) fn toml_positive_integer(item: &Item) -> Option<u64> {
    if let Some(value) = item.as_integer() {
        return u64::try_from(value).ok().filter(|value| *value > 0);
    }
    item.as_float()
        .filter(|value| value.is_finite() && *value >= 1.0 && value.fract() == 0.0)
        .and_then(|value| value.to_string().parse().ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entry_identity_tracks_directory_entry_replacement() {
        let directory = tempfile::tempdir().expect("directory");
        let path = directory.path().join("config.json");
        fs::write(&path, "original").expect("write");
        let original_identity = capture_entry_identity(&path)
            .expect("capture")
            .expect("entry present");

        let replacement = NamedTempFile::new_in(directory.path()).expect("temporary");
        fs::write(replacement.path(), "concurrent edit").expect("write");
        replacement.persist(&path).expect("replace");

        let replaced_identity = capture_entry_identity(&path)
            .expect("capture")
            .expect("entry present");
        assert_ne!(original_identity, replaced_identity);
    }

    #[test]
    fn entry_identity_is_absent_and_no_follow_for_symlinks() {
        let directory = tempfile::tempdir().expect("directory");
        let path = directory.path().join("config.json");
        assert_eq!(
            capture_entry_identity(&path).expect("capture"),
            None,
            "absent entries have no identity"
        );
        fs::write(&path, "original").expect("write");
        fs::remove_file(&path).expect("remove before symlink");
        let target = directory.path().join("target.json");
        fs::write(&target, "target").expect("write");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&target, &path).expect("symlink");
        #[cfg(windows)]
        std::os::windows::fs::symlink_file(&target, &path).expect("symlink");
        let identity = capture_entry_identity(&path).expect("capture");
        assert_ne!(
            identity,
            capture_entry_identity(&target).expect("capture"),
            "identity must not follow the symlink target"
        );
    }
}
