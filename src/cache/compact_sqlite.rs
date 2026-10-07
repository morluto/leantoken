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

pub(super) fn directory_matches(expected: &same_file::Handle, path: &Path) -> Result<bool> {
    Ok(expected == &same_file::Handle::from_path(path)?)
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

/// Match SQLite's bundled Unix VFS candidate order without changing globals.
fn temporary_directory(connection: &Connection) -> Result<PathBuf> {
    let configured: Option<String> = connection
        .query_row("PRAGMA temp_store_directory", [], |row| row.get(0))
        .optional()?;
    let mut candidates = Vec::new();
    if let Some(directory) = configured.filter(|v| !v.is_empty()) {
        candidates.push(PathBuf::from(directory));
    }
    #[cfg(unix)]
    {
        candidates.extend(
            ["SQLITE_TMPDIR", "TMPDIR"]
                .into_iter()
                .filter_map(std::env::var_os)
                .map(PathBuf::from),
        );
        candidates.extend(["/var/tmp", "/usr/tmp", "/tmp", "."].map(PathBuf::from));
    }
    #[cfg(windows)]
    {
        // GetTempPathW uses TMP before TEMP. Avoid guessing service-account paths
        // when neither environment variable names the actual SQLite directory.
        candidates.extend(
            ["TMP", "TEMP"]
                .into_iter()
                .filter_map(std::env::var_os)
                .map(PathBuf::from),
        );
    }
    for directory in candidates {
        if directory.is_dir() && tempfile::NamedTempFile::new_in(&directory).is_ok() {
            return Ok(fs::canonicalize(directory)?);
        }
    }
    Err(Error::InvalidConfiguration(
        "cannot determine a writable SQLite temporary directory".into(),
    ))
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
