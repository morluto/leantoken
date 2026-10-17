use super::*;
use rusqlite::OptionalExtension;
use rusqlite::types::Value;
use std::time::Instant;

pub(super) fn open(path: &Path, preview: bool) -> Result<Connection> {
    let flags = if preview {
        OpenFlags::SQLITE_OPEN_READ_ONLY
    } else {
        OpenFlags::SQLITE_OPEN_READ_WRITE
    };
    let connection = Connection::open_with_flags(
        path,
        flags | OpenFlags::SQLITE_OPEN_NO_MUTEX | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )?;
    connection.busy_timeout(Duration::from_millis(100))?;
    if !preview {
        connection.execute_batch("PRAGMA locking_mode=EXCLUSIVE")?;
    }
    connection
        .execute_batch("PRAGMA cache_size=-8192; PRAGMA mmap_size=0; PRAGMA temp_store=FILE;")?;
    Ok(connection)
}

pub(super) fn pin_database(directory: &cap_std::fs::Dir, path: &Path) -> Result<same_file::Handle> {
    #[cfg(unix)]
    let file = {
        let _ = path;
        directory.open(DATABASE_NAME)?.into_std()
    };
    #[cfg(windows)]
    let file = {
        use std::os::windows::fs::OpenOptionsExt;
        let _ = directory;
        // Deny delete/rename while SQLite opens this exact file. Reparse points
        // are opened themselves, then rejected by the identity/type checks.
        const FILE_SHARE_READ_WRITE: u32 = 0x1 | 0x2;
        const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x00200000;
        fs::OpenOptions::new()
            .read(true)
            .share_mode(FILE_SHARE_READ_WRITE)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
            .open(path)?
    };
    Ok(same_file::Handle::from_file(file)?)
}

pub(super) fn admit_database(
    directory: &cap_std::fs::Dir,
    path: &Path,
) -> Result<std::result::Result<same_file::Handle, CacheCompactOutcome>> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let metadata = fs::symlink_metadata(path)?;
        let identity = (metadata.dev(), metadata.ino());
        if open_file_identities()?
            .values()
            .any(|value| *value == identity)
        {
            // Even a read-only open/close can release an existing SQLite
            // connection's process-wide POSIX locks for this inode.
            return Ok(Err(CacheCompactOutcome::SkippedActive {
                detail: "cache database is already open in this process".into(),
            }));
        }
    }
    let database = pin_database(directory, path)?;
    if !has_wal_header(&database)? {
        return Ok(Err(CacheCompactOutcome::SkippedUnsafe {
            detail:
                "compaction requires an existing WAL database; rollback journals are not maintained"
                    .into(),
        }));
    }
    Ok(Ok(database))
}

pub(super) fn has_wal_header(database: &same_file::Handle) -> Result<bool> {
    use std::io::{Read, Seek, SeekFrom};
    let mut file = database.as_file();
    file.seek(SeekFrom::Start(0))?;
    let mut header = [0; 20];
    match file.read_exact(&mut header) {
        Ok(()) => Ok(&header[..16] == b"SQLite format 3\0" && header[18..20] == [2, 2]),
        Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => Ok(false),
        Err(error) => Err(error.into()),
    }
}

pub(super) fn open_pinned(
    expected: &same_file::Handle,
    path: &Path,
    preview: bool,
) -> Result<Option<Connection>> {
    open_pinned_with(expected, path, preview, open)
}

pub(super) fn open_pinned_with(
    expected: &same_file::Handle,
    path: &Path,
    preview: bool,
    opener: impl FnOnce(&Path, bool) -> Result<Connection>,
) -> Result<Option<Connection>> {
    if !database_matches(expected, path)? {
        return Ok(None);
    }
    #[cfg(unix)]
    let before = if preview {
        None
    } else {
        Some(open_file_identities()?)
    };
    #[cfg(unix)]
    if let Some(before) = &before {
        let mut allowed = sqlite_file_identities(expected, path)?;
        let lease = coordination_sidecar_path(path, LEASE_LOCK_SUFFIX);
        match fs::symlink_metadata(lease) {
            Ok(metadata) if metadata.is_file() && metadata.len() == 0 => {
                use std::os::unix::fs::MetadataExt;
                allowed.insert((metadata.dev(), metadata.ino()));
            }
            Ok(_) => return Ok(None),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        if before
            .iter()
            .any(|(number, identity)| *number > 2 && !allowed.contains(identity))
        {
            // SQLite can reuse a deferred descriptor. Unknown pre-existing
            // regular files make a before/after proof ambiguous too.
            return Ok(None);
        }
    }
    let connection = opener(path, preview)?;
    #[cfg(unix)]
    if let Some(before) = before
        && !sqlite_opened_expected_file(expected, path, &before)?
    {
        return Ok(None);
    }
    if !database_matches(expected, path)? {
        return Ok(None);
    }
    Ok(Some(connection))
}

#[cfg(unix)]
type FileIdentity = (u64, u64);

#[cfg(unix)]
pub(super) fn open_file_identities() -> Result<BTreeMap<u32, FileIdentity>> {
    use nix::sys::stat::{SFlag, fstat};
    let root = if Path::new("/proc/self/fd").is_dir() {
        Path::new("/proc/self/fd")
    } else {
        Path::new("/dev/fd")
    };
    let mut files = BTreeMap::new();
    for (count, entry) in fs::read_dir(root)?.enumerate() {
        if count >= 1024 {
            return Err(Error::InvalidConfiguration(
                "too many open descriptors to verify SQLite's database identity".into(),
            ));
        }
        let entry = entry?;
        let Some(number) = entry
            .file_name()
            .to_str()
            .and_then(|v| v.parse::<u32>().ok())
        else {
            continue;
        };
        let metadata = match fstat(number as std::os::fd::RawFd) {
            Ok(metadata) => metadata,
            Err(nix::errno::Errno::EBADF) => continue,
            Err(error) => return Err(std::io::Error::from_raw_os_error(error as i32).into()),
        };
        if SFlag::from_bits_truncate(metadata.st_mode) & SFlag::S_IFMT == SFlag::S_IFREG {
            #[cfg(target_os = "macos")]
            let device = metadata.st_dev as u64;
            #[cfg(not(target_os = "macos"))]
            let device = metadata.st_dev;
            files.insert(number, (device, metadata.st_ino));
        }
    }
    Ok(files)
}

#[cfg(unix)]
fn sqlite_opened_expected_file(
    expected: &same_file::Handle,
    path: &Path,
    before: &BTreeMap<u32, FileIdentity>,
) -> Result<bool> {
    use std::os::unix::fs::MetadataExt;
    let metadata = expected.as_file().metadata()?;
    let main = (metadata.dev(), metadata.ino());
    let allowed = sqlite_file_identities(expected, path)?;
    let mut found_main = false;
    for (descriptor, identity) in open_file_identities()? {
        if before.get(&descriptor) == Some(&identity) {
            continue;
        }
        if !allowed.contains(&identity) {
            // Concurrent unrelated regular-file opens also fail closed; no
            // inference from mutable path names can authorize an unknown FD.
            return Ok(false);
        }
        found_main |= identity == main;
    }
    Ok(found_main)
}

#[cfg(unix)]
fn sqlite_file_identities(
    expected: &same_file::Handle,
    path: &Path,
) -> Result<BTreeSet<FileIdentity>> {
    use std::os::unix::fs::MetadataExt;
    let metadata = expected.as_file().metadata()?;
    let main = (metadata.dev(), metadata.ino());
    let mut allowed = BTreeSet::from([main]);
    for suffix in ["-wal", "-shm"] {
        let sidecar = PathBuf::from(format!("{}{suffix}", path.display()));
        match fs::symlink_metadata(sidecar) {
            Ok(metadata) if metadata.is_file() && metadata.nlink() == 1 => {
                allowed.insert((metadata.dev(), metadata.ino()));
            }
            Ok(_) => {
                return Err(Error::InvalidConfiguration(
                    "unsafe SQLite sidecar identity".into(),
                ));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(allowed)
}

pub(super) fn unsafe_artifact_detail(
    directory: &same_file::Handle,
    database: &Path,
    cache_path: &Path,
) -> Result<Option<String>> {
    if !directory_matches(directory, database.parent().expect("database parent"))? {
        return Ok(Some("cache directory identity changed".into()));
    }
    linked_artifact(cache_path)
}

pub(super) fn opened_path_matches(
    connection: &Connection,
    directory: &same_file::Handle,
    path: &Path,
) -> Result<bool> {
    if connection
        .path()
        .and_then(|p| fs::canonicalize(p).ok())
        .as_deref()
        != Some(path)
    {
        return Ok(false);
    }
    directory_matches(directory, path.parent().expect("database parent"))
}

pub(super) fn directory_matches(expected: &same_file::Handle, path: &Path) -> Result<bool> {
    Ok(expected == &same_file::Handle::from_path(path)?)
}

pub(super) fn database_matches(expected: &same_file::Handle, path: &Path) -> Result<bool> {
    // Never open and close another Unix handle for this inode: closing it
    // would release this process's POSIX locks, including SQLite's locks.
    let current = fs::symlink_metadata(path)?;
    if file_link_count(expected.as_file())? != 1 || !current.file_type().is_file() {
        return Ok(false);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let held = expected.as_file().metadata()?;
        Ok(current.nlink() == 1 && (held.dev(), held.ino()) == (current.dev(), current.ino()))
    }
    #[cfg(windows)]
    {
        let current = same_file::Handle::from_path(path)?;
        Ok(expected == &current && file_link_count(current.as_file())? == 1)
    }
}

fn file_link_count(file: &fs::File) -> std::io::Result<u64> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        Ok(file.metadata()?.nlink())
    }
    #[cfg(windows)]
    {
        Ok(u64::from(
            winapi_util::file::information(file)?.number_of_links(),
        ))
    }
}

pub(super) fn linked_artifact(directory: &Path) -> Result<Option<String>> {
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        #[cfg(unix)]
        let links = {
            use std::os::unix::fs::MetadataExt;
            fs::symlink_metadata(entry.path())?.nlink()
        };
        #[cfg(windows)]
        let links =
            winapi_util::file::information(&fs::File::open(entry.path())?)?.number_of_links();
        if links != 1 {
            return Ok(Some(format!(
                "cache artifact has multiple filesystem links: {}",
                entry.file_name().to_string_lossy()
            )));
        }
    }
    Ok(None)
}

pub(super) fn page_space(connection: &Connection) -> Result<(u64, u64)> {
    connection.execute_batch("BEGIN")?;
    let result = (|| {
        let page = unsigned_pragma(connection, "page_size")?;
        let count = unsigned_pragma(connection, "page_count")?;
        let free = unsigned_pragma(connection, "freelist_count")?;
        let bytes = page
            .checked_mul(count)
            .ok_or_else(|| Error::InvalidConfiguration("database page size overflow".into()))?;
        let reusable = page
            .checked_mul(free)
            .filter(|v| *v <= bytes)
            .ok_or_else(|| Error::InvalidConfiguration("invalid database freelist size".into()))?;
        Ok((bytes, reusable))
    })();
    connection.execute_batch("ROLLBACK")?;
    result
}

/// Match the bundled VFS selection before testing writability; never substitute
/// another Windows volume when its first configured candidate is unusable.
fn temporary_directory(connection: &Connection) -> Result<PathBuf> {
    let configured: Option<String> = connection
        .query_row("PRAGMA temp_store_directory", [], |row| row.get(0))
        .optional()?;
    #[cfg(unix)]
    {
        let mut candidates = Vec::new();
        if let Some(directory) = configured.filter(|v| !v.is_empty()) {
            candidates.push(PathBuf::from(directory));
        }
        candidates.extend(
            ["SQLITE_TMPDIR", "TMPDIR"]
                .into_iter()
                .filter_map(std::env::var_os)
                .map(PathBuf::from),
        );
        candidates.extend(["/var/tmp", "/usr/tmp", "/tmp", "."].map(PathBuf::from));
        for directory in candidates {
            if directory.is_dir() && tempfile::NamedTempFile::new_in(&directory).is_ok() {
                return Ok(fs::canonicalize(directory)?);
            }
        }
        Err(Error::InvalidConfiguration(
            "cannot determine a writable SQLite temporary directory".into(),
        ))
    }
    #[cfg(windows)]
    {
        let directory = windows_temporary_candidate(configured, |name| std::env::var_os(name))?;
        writable_temporary_directory(&directory)
    }
}

#[cfg(any(windows, test))]
pub(super) fn windows_temporary_candidate(
    configured: Option<String>,
    lookup: impl Fn(&str) -> Option<std::ffi::OsString>,
) -> Result<PathBuf> {
    if let Some(directory) = configured.filter(|v| !v.is_empty()) {
        return Ok(PathBuf::from(directory));
    }
    // GetTempPathW selects the first nonempty value without existence/access
    // checks. SQLite uses that API, not GetTempPath2W (Rust's temp_dir fallback).
    for name in ["TMP", "TEMP", "USERPROFILE"] {
        if let Some(value) = lookup(name).filter(|v| !v.is_empty()) {
            return Ok(PathBuf::from(value));
        }
    }
    // Without a known environment candidate, fail closed instead of guessing
    // Windows' system directory or changing SQLite/process global settings.
    Err(Error::InvalidConfiguration(
        "cannot identify SQLite's Windows temporary directory".into(),
    ))
}

#[cfg(any(windows, test))]
pub(super) fn writable_temporary_directory(directory: &Path) -> Result<PathBuf> {
    let _probe = tempfile::NamedTempFile::new_in(directory)?;
    Ok(fs::canonicalize(directory)?)
}

pub(super) fn space_shortage(
    connection: &Connection,
    directory: &Path,
    bytes: u64,
) -> Result<Option<String>> {
    space_shortage_with(connection, directory, bytes, |path| {
        fs2::available_space(path)
    })
}

pub(super) fn space_shortage_with(
    connection: &Connection,
    directory: &Path,
    bytes: u64,
    available: impl Fn(&Path) -> std::io::Result<u64>,
) -> Result<Option<String>> {
    let temporary = temporary_directory(connection)?;
    let shared = same_volume(directory, &temporary).unwrap_or(true);
    Ok(space_shortage_from(
        &temporary,
        bytes,
        available(directory)?,
        available(&temporary)?,
        shared,
    ))
}

pub(super) fn same_volume(left: &Path, right: &Path) -> std::io::Result<bool> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        Ok(fs::metadata(left)?.dev() == fs::metadata(right)?.dev())
    }
    #[cfg(windows)]
    {
        let left = winapi_util::Handle::from_path_any(left)?;
        let right = winapi_util::Handle::from_path_any(right)?;
        Ok(
            winapi_util::file::information(left.as_file())?.volume_serial_number()
                == winapi_util::file::information(right.as_file())?.volume_serial_number(),
        )
    }
}

pub(super) fn space_shortage_from(
    temporary: &Path,
    bytes: u64,
    database_available: u64,
    temporary_available: u64,
    shared: bool,
) -> Option<String> {
    let margin = 32 * 1024 * 1024;
    let mut database_required = bytes.saturating_mul(2).saturating_add(margin);
    let mut temporary_required = bytes.saturating_add(margin);
    if shared {
        let combined = database_required.saturating_add(temporary_required);
        database_required = combined;
        temporary_required = combined;
    }
    if database_available < database_required || temporary_available < temporary_required {
        return Some(format!(
            "database volume needs {database_required} free bytes (available {database_available}); SQLite temporary directory {} needs {temporary_required} (available {temporary_available}); shared or unidentified volume: {shared}",
            temporary.display()
        ));
    }
    None
}

fn unsigned_pragma(connection: &Connection, name: &str) -> Result<u64> {
    let value: i64 = connection.pragma_query_value(None, name, |row| row.get(0))?;
    u64::try_from(value).map_err(|_| Error::InvalidConfiguration(format!("negative SQLite {name}")))
}

type Fingerprint = (Vec<Value>, i64, Vec<(String, String, Option<String>)>);
pub(super) const COMPACT_META_SQL: &str = "SELECT * FROM meta WHERE id=1";

fn fingerprint(connection: &Connection) -> Result<Fingerprint> {
    let mut meta = connection.prepare(COMPACT_META_SQL)?;
    if meta.column_count() > 32 {
        return Err(Error::InvalidConfiguration(
            "unsupported cache metadata width".into(),
        ));
    }
    let columns = meta.column_count();
    let values = meta.query_row([], |row| {
        (0..columns).map(|i| {
            let value = row.get_ref(i)?;
            if matches!(value, rusqlite::types::ValueRef::Text(v) | rusqlite::types::ValueRef::Blob(v) if v.len() > 16384) {
                return Err(rusqlite::Error::FromSqlConversionFailure(
                    i,
                    value.data_type(),
                    std::io::Error::new(std::io::ErrorKind::InvalidData, "cache fingerprint value exceeds 16 KiB").into(),
                ));
            }
            row.get(i)
        }).collect::<std::result::Result<Vec<Value>, _>>()
    })?;
    let version = connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
    let schema = connection
        .prepare("SELECT type,name,sql FROM sqlite_schema ORDER BY type,name LIMIT 257")?
        .query_map([], |row| {
            for column in 0..3 {
                let value = row.get_ref(column)?;
                if matches!(value, rusqlite::types::ValueRef::Text(text) if text.len() > 16384) {
                    return Err(rusqlite::Error::FromSqlConversionFailure(
                        column,
                        value.data_type(),
                        std::io::Error::new(
                            std::io::ErrorKind::InvalidData,
                            "cache fingerprint value exceeds 16 KiB",
                        )
                        .into(),
                    ));
                }
            }
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    if schema.len() > 256 {
        return Err(Error::InvalidConfiguration(
            "unsupported cache schema size".into(),
        ));
    }
    Ok((values, version, schema))
}

fn quick_check(connection: &Connection) -> Result<()> {
    let check: String = connection.query_row("PRAGMA quick_check(1)", [], |row| row.get(0))?;
    if check != "ok" {
        return Err(Error::InvalidConfiguration(format!(
            "SQLite quick_check failed: {check}"
        )));
    }
    Ok(())
}

fn checkpoint(connection: &Connection) -> Result<()> {
    let (busy, _, _): (i64, i64, i64) =
        connection.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })?;
    if busy != 0 {
        return Err(Error::InvalidConfiguration(
            "checkpoint blocked by a connection outside the cache lease protocol".into(),
        ));
    }
    Ok(())
}

pub(super) fn vacuum(connection: &Connection, seconds: u64, committed: &mut bool) -> Result<()> {
    vacuum_with_budget(connection, Duration::from_secs(seconds), 10000, committed)
}

pub(super) fn vacuum_with_budget(
    connection: &Connection,
    budget: Duration,
    instructions: i32,
    committed: &mut bool,
) -> Result<()> {
    let before = fingerprint(connection)?;
    let started = Instant::now();
    connection.progress_handler(
        instructions,
        Some(move || {
            // Cooperative pacing; callers needing hard CPU limits use OS quotas.
            std::thread::sleep(Duration::from_millis(1));
            started.elapsed() >= budget
        }),
    )?;
    let operation = (|| {
        quick_check(connection)?;
        checkpoint(connection)?;
        connection.execute_batch("VACUUM")?;
        *committed = true;
        if fingerprint(connection)? != before {
            return Err(Error::OperationFailure(
                "compaction changed persisted metadata or schema".into(),
            ));
        }
        quick_check(connection)?;
        checkpoint(connection)?;
        Ok(())
    })();
    connection.progress_handler(0, None::<fn() -> bool>)?;
    operation
}
