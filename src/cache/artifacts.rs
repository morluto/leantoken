use super::*;

pub(super) fn scan_artifacts(path: &Path, budget: &mut InventoryBudget) -> Result<ArtifactScan> {
    budget.check()?;
    let mut scan = ArtifactScan {
        size_bytes: 0,
        latest_access_mtime: None,
        has_artifacts: false,
        unexpected: false,
    };
    let database = path.join(DATABASE_NAME);
    let lease_path = coordination_sidecar_path(&database, LEASE_LOCK_SUFFIX);
    for child in fs::read_dir(path)? {
        budget.check()?;
        budget.artifacts += 1;
        InventoryBudget::enforce(
            "artifact_entries",
            budget.artifacts,
            budget.limits.artifacts,
        )?;
        let child = child?;
        let metadata = fs::symlink_metadata(child.path())?;
        let child_path = child.path();
        let known = child
            .file_name()
            .to_str()
            .is_some_and(|name| PRUNABLE_ARTIFACTS.contains(&name))
            || is_coordination_sidecar_for_database(&child_path, &database);
        if !known || !metadata.file_type().is_file() {
            scan.unexpected = true;
            continue;
        }
        if child_path == lease_path {
            continue;
        }
        scan.has_artifacts = true;
        scan.size_bytes = scan.size_bytes.saturating_add(metadata.len());
        let name = child.file_name();
        // Read-only inspection can create an empty WAL or refresh SHM/lock mtimes.
        // Only a WAL containing bytes can carry a later transaction timestamp.
        if (name == OsStr::new(DATABASE_NAME)
            || (name == OsStr::new(WAL_NAME) && metadata.len() > 0))
            && let Ok(modified) = metadata.modified()
        {
            let modified = unix_seconds(modified);
            scan.latest_access_mtime = Some(
                scan.latest_access_mtime
                    .map_or(modified, |current| current.max(modified)),
            );
        }
    }
    budget.check()?;
    Ok(scan)
}

#[derive(Debug)]
pub(super) struct DatabaseMetadata {
    pub(super) schema_version: Option<i64>,
    pub(super) repository_root: Option<PathBuf>,
    pub(super) last_access_unix_seconds: Option<u64>,
    pub(super) schema: DatabaseSchema,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum DatabaseSchema {
    Current,
    Older,
    Future,
}

pub(super) fn inspect_database(
    path: &Path,
    budget: &mut InventoryBudget,
) -> Result<DatabaseMetadata> {
    budget.check()?;
    budget.sqlite_opens += 1;
    InventoryBudget::enforce(
        "sqlite_opens",
        budget.sqlite_opens,
        budget.limits.sqlite_opens,
    )?;
    let connection = budget.sqlite_result(Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    ))?;
    budget.observe_sqlite(&connection)?;
    budget.sqlite_result(connection.set_limit(
        rusqlite::limits::Limit::SQLITE_LIMIT_LENGTH,
        MAX_INVENTORY_VALUE_BYTES,
    ))?;
    budget.sqlite_result(connection.set_limit(
        rusqlite::limits::Limit::SQLITE_LIMIT_SQL_LENGTH,
        MAX_INVENTORY_VALUE_BYTES,
    ))?;
    budget.sqlite_result(connection.pragma_update(None, "cache_size", -256))?;
    budget.sqlite_result(connection.busy_timeout(Duration::from_millis(100)))?;
    let migration_version =
        budget.sqlite_result(
            connection.pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0)),
        )?;
    if migration_version > CURRENT_MIGRATION_VERSION {
        return Ok(DatabaseMetadata {
            schema_version: None,
            repository_root: None,
            last_access_unix_seconds: None,
            schema: DatabaseSchema::Future,
        });
    }
    let mut statement = budget.sqlite_result(connection.prepare("PRAGMA table_info(meta)"))?;
    let mut rows = budget.sqlite_result(statement.query([]))?;
    let mut columns = BTreeSet::new();
    while let Some(row) = budget.sqlite_result(rows.next())? {
        let column: String = budget.sqlite_result(row.get(1))?;
        budget.admit_metadata(column.len())?;
        columns.insert(column);
    }
    if !columns.contains("schema_version") {
        return Err(Error::InvalidConfiguration(
            "cache metadata table has no schema version".into(),
        ));
    }
    let schema_version = budget.sqlite_result(connection.query_row(
        "SELECT schema_version FROM meta WHERE id = 1",
        [],
        |row| row.get::<_, i64>(0),
    ))?;
    if schema_version > CURRENT_SCHEMA_VERSION {
        return Ok(DatabaseMetadata {
            schema_version: Some(schema_version),
            repository_root: None,
            last_access_unix_seconds: None,
            schema: DatabaseSchema::Future,
        });
    }
    let repository_root = if columns.contains("repository_root") {
        let root = budget.sqlite_result(connection.query_row(
            "SELECT repository_root FROM meta WHERE id = 1",
            [],
            |row| row.get::<_, String>(0),
        ))?;
        InventoryBudget::enforce("repository_root_bytes", root.len(), 8192)?;
        budget.admit_metadata(root.len())?;
        (!root.is_empty()).then(|| PathBuf::from(root))
    } else {
        None
    };
    let last_access_unix_seconds = if columns.contains("last_access_unix_seconds") {
        let accessed = budget.sqlite_result(connection.query_row(
            "SELECT last_access_unix_seconds FROM meta WHERE id = 1",
            [],
            |row| row.get::<_, i64>(0),
        ))?;
        u64::try_from(accessed).ok().filter(|value| *value > 0)
    } else {
        None
    };
    Ok(DatabaseMetadata {
        schema_version: Some(schema_version),
        repository_root,
        last_access_unix_seconds,
        schema: if schema_version == CURRENT_SCHEMA_VERSION
            && columns.contains("last_access_unix_seconds")
        {
            DatabaseSchema::Current
        } else {
            DatabaseSchema::Older
        },
    })
}
