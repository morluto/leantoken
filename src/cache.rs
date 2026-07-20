//! Explicit inspection and pruning of centrally managed repository caches.

use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::OsStr,
    fs,
    io::Write,
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use rusqlite::{Connection, OpenFlags};
use serde::Serialize;

use crate::config::{managed_cache_id, managed_cache_root};
use crate::coordination::IndexCoordination;
use crate::storage::{CURRENT_MIGRATION_VERSION, CURRENT_SCHEMA_VERSION};
use crate::{Error, Result};

const DATABASE_NAME: &str = "index.sqlite";
const LEASE_NAME: &str = "index.sqlite.lease.lock";
const PRUNABLE_ARTIFACTS: &[&str] = &[
    DATABASE_NAME,
    "index.sqlite-wal",
    "index.sqlite-shm",
    "index.sqlite-journal",
    "index.sqlite.init.lock",
    "index.sqlite.leader.lock",
    "index.sqlite.index.lock",
];
const SECONDS_PER_DAY: u64 = 24 * 60 * 60;

/// Criteria and consent for one managed-cache prune operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CachePruneRequest {
    /// Remove caches not accessed for at least this many days.
    pub older_than_days: Option<u64>,
    /// Remove least-recently-used caches until managed bytes are at most this value.
    pub max_total_bytes: Option<u64>,
    /// Explicitly remove caches whose recorded repository root is missing.
    pub remove_missing_roots: bool,
    /// Report the resolved deletion plan without changing files.
    pub dry_run: bool,
    /// Confirm a non-dry-run deletion plan.
    pub yes: bool,
}

/// Metadata quality available for one cache directory.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CacheState {
    /// Current schema and access metadata were read successfully.
    Current,
    /// A readable older schema lacks current access metadata.
    Legacy,
    /// Known cache artifacts exist without a readable database.
    Incomplete,
    /// The SQLite database could not be inspected.
    Corrupt,
    /// A newer schema or mismatched identity is not safe for this binary to prune.
    Unsupported,
    /// Unexpected content makes automatic deletion unsafe.
    Unrecognized,
}

/// Source used for the last-access value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AccessTimeSource {
    /// Schema-v5 metadata updated during repository binding.
    Database,
    /// Latest artifact modification time used for an older or incomplete cache.
    FileMtime,
}

/// Auditable description of one managed cache.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CacheEntry {
    /// Stable directory identifier derived from the canonical repository root.
    pub id: String,
    /// Managed cache directory.
    pub path: PathBuf,
    /// Recorded canonical repository root, when readable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repository_root: Option<PathBuf>,
    /// Whether the recorded repository root is currently reachable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repository_available: Option<bool>,
    /// Most recent known access time as Unix seconds.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_access_unix_seconds: Option<u64>,
    /// Provenance for `last_access_unix_seconds`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub access_time_source: Option<AccessTimeSource>,
    /// Age at inspection time.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub age_seconds: Option<u64>,
    /// SQLite schema recorded by the cache, when readable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub schema_version: Option<i64>,
    /// Bytes in direct managed cache artifacts.
    pub size_bytes: u64,
    /// Whether a lease-aware LeanToken process currently uses this cache.
    pub active: bool,
    /// Metadata and directory safety classification.
    pub state: CacheState,
    /// Local diagnostic when metadata could not be read completely.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// Complete report for `cache list`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CacheListReport {
    /// Platform-managed cache root inspected by the command.
    pub cache_root: PathBuf,
    /// Sum of reported managed artifact bytes.
    pub total_bytes: u64,
    /// Entries ignored because their names are not managed cache identities.
    pub ignored_entries: usize,
    /// Stable cache entries sorted by identifier.
    pub entries: Vec<CacheEntry>,
}

/// Result action for one cache considered by prune.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CachePruneAction {
    /// No configured criterion selected this entry.
    Kept,
    /// Dry-run would delete this entry.
    WouldDelete,
    /// Managed artifacts were deleted.
    Deleted,
    /// A live process held the cache lease.
    SkippedActive,
    /// Unsupported metadata or unexpected content prevented automatic deletion.
    SkippedUnsafe,
    /// Filesystem deletion failed.
    Failed,
}

/// Auditable prune decision for one cache.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CachePruneResult {
    /// Managed cache identifier.
    pub id: String,
    /// Managed cache directory.
    pub path: PathBuf,
    /// Decision outcome.
    pub action: CachePruneAction,
    /// Selection reasons such as age, missing root, or total-byte budget.
    pub reasons: Vec<String>,
    /// Bytes associated with the entry at decision time.
    pub size_bytes: u64,
    /// Explanation for a skipped entry.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// Failure detail for a failed deletion.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Complete report for `cache prune`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CachePruneReport {
    /// Platform-managed cache root inspected by the command.
    pub cache_root: PathBuf,
    /// Whether no files were changed.
    pub dry_run: bool,
    /// Managed bytes before pruning.
    pub total_bytes_before: u64,
    /// Actual or projected managed bytes after pruning.
    pub total_bytes_after: u64,
    /// Actual or projected reclaimed bytes.
    pub reclaimed_bytes: u64,
    /// Stable per-entry decisions.
    pub results: Vec<CachePruneResult>,
}

impl CachePruneReport {
    /// Return true when one or more selected entries could not be deleted.
    #[must_use]
    pub fn has_failures(&self) -> bool {
        self.results
            .iter()
            .any(|result| result.action == CachePruneAction::Failed)
    }
}

impl CacheState {
    fn label(self) -> &'static str {
        match self {
            Self::Current => "current",
            Self::Legacy => "legacy",
            Self::Incomplete => "incomplete",
            Self::Corrupt => "corrupt",
            Self::Unsupported => "unsupported",
            Self::Unrecognized => "unrecognized",
        }
    }
}

impl CachePruneAction {
    fn label(self) -> &'static str {
        match self {
            Self::Kept => "kept",
            Self::WouldDelete => "would_delete",
            Self::Deleted => "deleted",
            Self::SkippedActive => "skipped_active",
            Self::SkippedUnsafe => "skipped_unsafe",
            Self::Failed => "failed",
        }
    }
}

#[derive(Debug)]
struct CacheManager {
    root: PathBuf,
    now: u64,
}

#[derive(Debug)]
struct InspectedCache {
    entry: CacheEntry,
    safe_to_prune: bool,
}

#[derive(Debug)]
struct ArtifactScan {
    size_bytes: u64,
    latest_mtime: Option<u64>,
    has_artifacts: bool,
    unexpected: bool,
}

/// List every centrally managed repository cache for the current user.
pub fn list() -> Result<CacheListReport> {
    CacheManager::for_current_user()?.list()
}

/// Prune centrally managed repository caches using explicit criteria.
pub fn prune(request: &CachePruneRequest) -> Result<CachePruneReport> {
    CacheManager::for_current_user()?.prune(request)
}

impl CacheManager {
    fn for_current_user() -> Result<Self> {
        let root = managed_cache_root().ok_or_else(|| {
            Error::InvalidConfiguration(
                "this platform does not provide a central managed cache directory".into(),
            )
        })?;
        Ok(Self::new(root, unix_seconds(SystemTime::now())))
    }

    fn new(root: PathBuf, now: u64) -> Self {
        Self { root, now }
    }

    fn list(&self) -> Result<CacheListReport> {
        let (entries, ignored_entries) = self.inspect_all()?;
        let total_bytes = entries.iter().fold(0u64, |total, cache| {
            total.saturating_add(cache.entry.size_bytes)
        });
        Ok(CacheListReport {
            cache_root: self.root.clone(),
            total_bytes,
            ignored_entries,
            entries: entries.into_iter().map(|cache| cache.entry).collect(),
        })
    }

    fn prune(&self, request: &CachePruneRequest) -> Result<CachePruneReport> {
        validate_prune_request(request)?;
        let (entries, _) = self.inspect_all()?;
        let total_bytes_before = entries.iter().fold(0u64, |total, cache| {
            total.saturating_add(cache.entry.size_bytes)
        });
        let selected = select_prune_candidates(&entries, request, total_bytes_before);
        let mut reclaimed_bytes = 0u64;
        let mut results = Vec::with_capacity(entries.len());

        for cache in entries {
            let Some(reasons) = selected.get(&cache.entry.id).cloned() else {
                results.push(prune_result(
                    &cache,
                    CachePruneAction::Kept,
                    Vec::new(),
                    None,
                ));
                continue;
            };
            if cache.entry.active {
                results.push(prune_result(
                    &cache,
                    CachePruneAction::SkippedActive,
                    reasons,
                    None,
                ));
                continue;
            }
            if !cache.safe_to_prune {
                results.push(prune_result(
                    &cache,
                    CachePruneAction::SkippedUnsafe,
                    reasons,
                    None,
                ));
                continue;
            }
            if request.dry_run {
                reclaimed_bytes = reclaimed_bytes.saturating_add(cache.entry.size_bytes);
                results.push(prune_result(
                    &cache,
                    CachePruneAction::WouldDelete,
                    reasons,
                    None,
                ));
                continue;
            }

            let database = cache.entry.path.join(DATABASE_NAME);
            let coordination = IndexCoordination::for_database(&database);
            let _lease = match coordination.try_acquire_prune_lease() {
                Ok(Some(lease)) => lease,
                Ok(None) => {
                    results.push(prune_result(
                        &cache,
                        CachePruneAction::SkippedActive,
                        reasons,
                        None,
                    ));
                    continue;
                }
                Err(error) => {
                    results.push(prune_result(
                        &cache,
                        CachePruneAction::Failed,
                        reasons,
                        Some(error.to_string()),
                    ));
                    continue;
                }
            };
            let current = match self.inspect_cache(&cache.entry.id, false) {
                Ok(current) => current,
                Err(error) => {
                    results.push(prune_result(
                        &cache,
                        CachePruneAction::Failed,
                        reasons,
                        Some(error.to_string()),
                    ));
                    continue;
                }
            };
            if !current.safe_to_prune {
                results.push(prune_result(
                    &current,
                    CachePruneAction::SkippedUnsafe,
                    reasons,
                    None,
                ));
                continue;
            }
            if reasons.len() == 1
                && reasons[0] == "missing_repository"
                && current.entry.repository_available != Some(false)
            {
                results.push(prune_result(
                    &current,
                    CachePruneAction::Kept,
                    reasons,
                    None,
                ));
                continue;
            }

            let removal = remove_managed_artifacts(&current.entry.path);
            reclaimed_bytes = reclaimed_bytes.saturating_add(removal.reclaimed_bytes);
            match removal.error {
                None => {
                    results.push(prune_result(
                        &current,
                        CachePruneAction::Deleted,
                        reasons,
                        None,
                    ));
                }
                Some(error) => results.push(prune_result(
                    &current,
                    CachePruneAction::Failed,
                    reasons,
                    Some(error),
                )),
            }
        }

        results.sort_by(|left, right| left.id.cmp(&right.id));
        Ok(CachePruneReport {
            cache_root: self.root.clone(),
            dry_run: request.dry_run,
            total_bytes_before,
            total_bytes_after: total_bytes_before.saturating_sub(reclaimed_bytes),
            reclaimed_bytes,
            results,
        })
    }

    fn inspect_all(&self) -> Result<(Vec<InspectedCache>, usize)> {
        let read_dir = match fs::read_dir(&self.root) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok((Vec::new(), 0));
            }
            Err(error) => return Err(error.into()),
        };
        let mut entries = Vec::new();
        let mut ignored = 0usize;
        for entry in read_dir {
            let entry = entry?;
            let file_type = entry.file_type()?;
            let Some(id) = entry.file_name().to_str().map(str::to_owned) else {
                ignored += 1;
                continue;
            };
            if !file_type.is_dir() || !is_cache_id(&id) {
                ignored += 1;
                continue;
            }
            let cache = self.inspect_cache(&id, true)?;
            if cache.entry.size_bytes == 0 && cache.entry.state == CacheState::Incomplete {
                continue;
            }
            entries.push(cache);
        }
        entries.sort_by(|left, right| left.entry.id.cmp(&right.entry.id));
        Ok((entries, ignored))
    }

    fn inspect_cache(&self, id: &str, probe_active: bool) -> Result<InspectedCache> {
        let path = self.root.join(id);
        let database = path.join(DATABASE_NAME);
        let initial_scan = scan_artifacts(&path)?;
        let latest_mtime = initial_scan.latest_mtime;
        let mut unexpected = initial_scan.unexpected;
        let mut metadata_safe = true;

        let active = if probe_active && path.join(LEASE_NAME).exists() {
            IndexCoordination::for_database(&database)
                .try_acquire_prune_lease()?
                .is_none()
        } else {
            false
        };
        let mut entry = CacheEntry {
            id: id.into(),
            path,
            repository_root: None,
            repository_available: None,
            last_access_unix_seconds: latest_mtime,
            access_time_source: latest_mtime.map(|_| AccessTimeSource::FileMtime),
            age_seconds: latest_mtime.map(|accessed| self.now.saturating_sub(accessed)),
            schema_version: None,
            size_bytes: initial_scan.size_bytes,
            active,
            state: CacheState::Incomplete,
            detail: None,
        };

        let database_is_regular =
            fs::symlink_metadata(&database).is_ok_and(|metadata| metadata.file_type().is_file());
        if initial_scan.has_artifacts && database_is_regular {
            match inspect_database(&database) {
                Ok(metadata) => {
                    entry.schema_version = metadata.schema_version;
                    entry.repository_root = metadata.repository_root;
                    entry.repository_available =
                        entry.repository_root.as_deref().and_then(root_available);
                    if let Some(accessed) = metadata.last_access_unix_seconds {
                        entry.last_access_unix_seconds = Some(accessed);
                        entry.access_time_source = Some(AccessTimeSource::Database);
                        entry.age_seconds = Some(self.now.saturating_sub(accessed));
                    }
                    entry.state = if metadata.future_schema {
                        metadata_safe = false;
                        entry.detail = Some("cache uses a newer unsupported schema".into());
                        CacheState::Unsupported
                    } else if metadata.current {
                        CacheState::Current
                    } else {
                        CacheState::Legacy
                    };
                    if let Some(repository_root) = &entry.repository_root
                        && managed_cache_id(repository_root) != id
                    {
                        metadata_safe = false;
                        entry.state = CacheState::Unsupported;
                        entry.detail =
                            Some("cache identity does not match its recorded root".into());
                    }
                }
                Err(error) => {
                    entry.state = CacheState::Corrupt;
                    entry.detail = Some(error.to_string());
                }
            }
        }
        let final_scan = scan_artifacts(&entry.path)?;
        entry.size_bytes = final_scan.size_bytes;
        unexpected |= final_scan.unexpected;
        if unexpected {
            entry.state = CacheState::Unrecognized;
            entry.detail = Some("cache directory contains unexpected entries".into());
        }

        Ok(InspectedCache {
            safe_to_prune: final_scan.has_artifacts && !unexpected && metadata_safe,
            entry,
        })
    }
}

fn scan_artifacts(path: &Path) -> Result<ArtifactScan> {
    let mut scan = ArtifactScan {
        size_bytes: 0,
        latest_mtime: None,
        has_artifacts: false,
        unexpected: false,
    };
    for child in fs::read_dir(path)? {
        let child = child?;
        let metadata = fs::symlink_metadata(child.path())?;
        let known = child
            .file_name()
            .to_str()
            .is_some_and(|name| PRUNABLE_ARTIFACTS.contains(&name) || name == LEASE_NAME);
        if !known || !metadata.file_type().is_file() {
            scan.unexpected = true;
            continue;
        }
        if child.file_name() == OsStr::new(LEASE_NAME) {
            continue;
        }
        scan.has_artifacts = true;
        scan.size_bytes = scan.size_bytes.saturating_add(metadata.len());
        if let Ok(modified) = metadata.modified() {
            let modified = unix_seconds(modified);
            scan.latest_mtime = Some(
                scan.latest_mtime
                    .map_or(modified, |current| current.max(modified)),
            );
        }
    }
    Ok(scan)
}

#[derive(Debug)]
struct DatabaseMetadata {
    schema_version: Option<i64>,
    repository_root: Option<PathBuf>,
    last_access_unix_seconds: Option<u64>,
    current: bool,
    future_schema: bool,
}

fn inspect_database(path: &Path) -> Result<DatabaseMetadata> {
    let connection = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    connection.busy_timeout(Duration::from_millis(100))?;
    let migration_version =
        connection.pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))?;
    if migration_version > CURRENT_MIGRATION_VERSION {
        return Ok(DatabaseMetadata {
            schema_version: None,
            repository_root: None,
            last_access_unix_seconds: None,
            current: false,
            future_schema: true,
        });
    }
    let mut statement = connection.prepare("PRAGMA table_info(meta)")?;
    let columns = statement
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<std::result::Result<BTreeSet<_>, _>>()?;
    if !columns.contains("schema_version") {
        return Err(Error::InvalidConfiguration(
            "cache metadata table has no schema version".into(),
        ));
    }
    let schema_version =
        connection.query_row("SELECT schema_version FROM meta WHERE id = 1", [], |row| {
            row.get::<_, i64>(0)
        })?;
    if schema_version > CURRENT_SCHEMA_VERSION {
        return Ok(DatabaseMetadata {
            schema_version: Some(schema_version),
            repository_root: None,
            last_access_unix_seconds: None,
            current: false,
            future_schema: true,
        });
    }
    let repository_root = if columns.contains("repository_root") {
        let root =
            connection.query_row("SELECT repository_root FROM meta WHERE id = 1", [], |row| {
                row.get::<_, String>(0)
            })?;
        (!root.is_empty()).then(|| PathBuf::from(root))
    } else {
        None
    };
    let last_access_unix_seconds = if columns.contains("last_access_unix_seconds") {
        let accessed = connection.query_row(
            "SELECT last_access_unix_seconds FROM meta WHERE id = 1",
            [],
            |row| row.get::<_, i64>(0),
        )?;
        u64::try_from(accessed).ok().filter(|value| *value > 0)
    } else {
        None
    };
    Ok(DatabaseMetadata {
        schema_version: Some(schema_version),
        repository_root,
        last_access_unix_seconds,
        current: schema_version == CURRENT_SCHEMA_VERSION
            && columns.contains("last_access_unix_seconds"),
        future_schema: false,
    })
}

fn validate_prune_request(request: &CachePruneRequest) -> Result<()> {
    if request.older_than_days.is_none()
        && request.max_total_bytes.is_none()
        && !request.remove_missing_roots
    {
        return Err(Error::InvalidRequest(
            "cache prune requires --older-than, --max-total-bytes, or --remove-missing-roots"
                .into(),
        ));
    }
    if request.older_than_days == Some(0) {
        return Err(Error::InvalidRequest(
            "--older-than must be at least one day".into(),
        ));
    }
    if !request.dry_run && !request.yes {
        return Err(Error::InvalidRequest(
            "cache prune requires --yes unless --dry-run is used".into(),
        ));
    }
    Ok(())
}

fn select_prune_candidates(
    entries: &[InspectedCache],
    request: &CachePruneRequest,
    total_bytes: u64,
) -> BTreeMap<String, Vec<String>> {
    let mut selected = BTreeMap::<String, Vec<String>>::new();
    let minimum_age = request
        .older_than_days
        .map(|days| days.saturating_mul(SECONDS_PER_DAY));
    for cache in entries {
        if minimum_age.is_some_and(|age| cache.entry.age_seconds.is_some_and(|value| value >= age))
        {
            selected
                .entry(cache.entry.id.clone())
                .or_default()
                .push("older_than".into());
        }
        if request.remove_missing_roots && cache.entry.repository_available == Some(false) {
            selected
                .entry(cache.entry.id.clone())
                .or_default()
                .push("missing_repository".into());
        }
    }

    let Some(max_total_bytes) = request.max_total_bytes else {
        return selected;
    };
    let mut projected = total_bytes;
    for cache in entries {
        if selected.contains_key(&cache.entry.id) && cache.safe_to_prune && !cache.entry.active {
            projected = projected.saturating_sub(cache.entry.size_bytes);
        }
    }
    let mut lru = entries
        .iter()
        .filter(|cache| !selected.contains_key(&cache.entry.id))
        .collect::<Vec<_>>();
    lru.sort_by(|left, right| {
        left.entry
            .last_access_unix_seconds
            .unwrap_or(0)
            .cmp(&right.entry.last_access_unix_seconds.unwrap_or(0))
            .then_with(|| left.entry.id.cmp(&right.entry.id))
    });
    for cache in lru {
        if projected <= max_total_bytes {
            break;
        }
        selected
            .entry(cache.entry.id.clone())
            .or_default()
            .push("max_total_bytes".into());
        if cache.safe_to_prune && !cache.entry.active {
            projected = projected.saturating_sub(cache.entry.size_bytes);
        }
    }
    selected
}

fn prune_result(
    cache: &InspectedCache,
    action: CachePruneAction,
    reasons: Vec<String>,
    error: Option<String>,
) -> CachePruneResult {
    let detail = match action {
        CachePruneAction::SkippedActive => Some("cache lease is held by a running process".into()),
        CachePruneAction::SkippedUnsafe => cache
            .entry
            .detail
            .clone()
            .or_else(|| Some("cache metadata is not safe to prune".into())),
        _ => None,
    };
    CachePruneResult {
        id: cache.entry.id.clone(),
        path: cache.entry.path.clone(),
        action,
        reasons,
        size_bytes: cache.entry.size_bytes,
        detail,
        error,
    }
}

struct RemovalOutcome {
    reclaimed_bytes: u64,
    error: Option<String>,
}

fn remove_managed_artifacts(directory: &Path) -> RemovalOutcome {
    let mut reclaimed_bytes = 0u64;
    for artifact in PRUNABLE_ARTIFACTS {
        let path = directory.join(artifact);
        let size = fs::symlink_metadata(&path)
            .map(|metadata| metadata.len())
            .unwrap_or(0);
        match fs::remove_file(path) {
            Ok(()) => reclaimed_bytes = reclaimed_bytes.saturating_add(size),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return RemovalOutcome {
                    reclaimed_bytes,
                    error: Some(error.to_string()),
                };
            }
        }
    }
    RemovalOutcome {
        reclaimed_bytes,
        error: None,
    }
}

fn is_cache_id(value: &str) -> bool {
    value.len() == 16
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn root_available(path: &Path) -> Option<bool> {
    match fs::metadata(path) {
        Ok(_) => Some(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Some(false),
        Err(_) => None,
    }
}

fn unix_seconds(time: SystemTime) -> u64 {
    time.duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs())
}

/// Print a cache-list report as JSON or concise human-readable output.
pub fn print_list(report: &CacheListReport, json_output: bool) -> Result<()> {
    let stdout = std::io::stdout();
    let mut output = stdout.lock();
    if json_output {
        serde_json::to_writer(&mut output, report)?;
        output.write_all(b"\n")?;
        return Ok(());
    }
    writeln!(
        output,
        "Managed cache root: {}",
        report.cache_root.display()
    )?;
    writeln!(
        output,
        "{} cache(s), {} bytes",
        report.entries.len(),
        report.total_bytes
    )?;
    for entry in &report.entries {
        writeln!(
            output,
            "{}  {} bytes  {}  {}  last_access={}  root_available={}  {}",
            entry.id,
            entry.size_bytes,
            if entry.active { "active" } else { "inactive" },
            entry.state.label(),
            entry
                .last_access_unix_seconds
                .map_or_else(|| "unknown".into(), |timestamp| timestamp.to_string()),
            entry
                .repository_available
                .map_or("unknown", |available| if available { "yes" } else { "no" }),
            entry
                .repository_root
                .as_deref()
                .map_or_else(|| "unknown root".into(), |root| root.display().to_string())
        )?;
    }
    Ok(())
}

/// Print a cache-prune report as JSON or concise human-readable output.
pub fn print_prune(report: &CachePruneReport, json_output: bool) -> Result<()> {
    let stdout = std::io::stdout();
    let mut output = stdout.lock();
    if json_output {
        serde_json::to_writer(&mut output, report)?;
        output.write_all(b"\n")?;
        return Ok(());
    }
    writeln!(
        output,
        "Managed cache prune{}: {} -> {} bytes",
        if report.dry_run { " dry-run" } else { "" },
        report.total_bytes_before,
        report.total_bytes_after
    )?;
    for result in &report.results {
        let detail = result.error.as_deref().or(result.detail.as_deref());
        writeln!(
            output,
            "{}  {}  {} bytes{}{}",
            result.action.label(),
            result.id,
            result.size_bytes,
            if result.reasons.is_empty() {
                String::new()
            } else {
                format!("  {}", result.reasons.join(","))
            },
            detail.map_or_else(String::new, |detail| format!("  {detail}"))
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Config;
    use crate::services::Services;
    use crate::storage::Storage;

    const FIRST_ID: &str = "0000000000000001";
    const SECOND_ID: &str = "0000000000000002";

    fn request() -> CachePruneRequest {
        CachePruneRequest {
            older_than_days: None,
            max_total_bytes: None,
            remove_missing_roots: false,
            dry_run: true,
            yes: false,
        }
    }

    fn create_current_cache(
        manager: &CacheManager,
        repository: &Path,
        accessed_at: u64,
    ) -> (String, PathBuf) {
        let id = managed_cache_id(repository);
        let directory = manager.root.join(&id);
        fs::create_dir_all(&directory).expect("cache directory");
        let database = directory.join(DATABASE_NAME);
        drop(Storage::open_for_repository(&database, repository).expect("cache database"));
        Connection::open(&database)
            .expect("cache metadata")
            .execute(
                "UPDATE meta SET last_access_unix_seconds = ?1 WHERE id = 1",
                [i64::try_from(accessed_at).expect("test timestamp")],
            )
            .expect("access timestamp");
        (id, database)
    }

    #[test]
    fn list_reports_current_metadata_and_ignores_non_cache_directories() {
        let temp = tempfile::tempdir().expect("temporary directory");
        let root = temp.path().join("managed");
        let repository = temp.path().join("repository");
        fs::create_dir(&repository).expect("repository");
        let manager = CacheManager::new(root.clone(), 10_000);
        create_current_cache(&manager, &repository, 9_000);
        fs::create_dir_all(root.join("not-managed")).expect("unmanaged directory");

        let report = manager.list().expect("cache list");

        assert_eq!(report.entries.len(), 1);
        assert_eq!(report.ignored_entries, 1);
        assert_eq!(report.entries[0].state, CacheState::Current);
        assert_eq!(
            report.entries[0].repository_root.as_deref(),
            Some(repository.as_path())
        );
        assert_eq!(report.entries[0].repository_available, Some(true));
        assert_eq!(report.entries[0].last_access_unix_seconds, Some(9_000));
        assert_eq!(report.entries[0].age_seconds, Some(1_000));
        assert_eq!(
            report.entries[0].access_time_source,
            Some(AccessTimeSource::Database)
        );
        assert!(report.total_bytes > 0);
    }

    #[test]
    fn active_service_clones_block_prune_until_every_lease_is_dropped() {
        let temp = tempfile::tempdir().expect("temporary directory");
        let root = temp.path().join("managed");
        let repository = temp.path().join("repository");
        fs::create_dir(&repository).expect("repository");
        let directory = root.join(managed_cache_id(&repository));
        fs::create_dir_all(&directory).expect("cache directory");
        let database = directory.join(DATABASE_NAME);
        let config = Config::discover(&repository, Some(database.clone())).expect("config");
        let services = Services::open(config).expect("services");
        let follower = services.clone();
        let manager = CacheManager::new(root, unix_seconds(SystemTime::now()));
        let mut prune = request();
        prune.max_total_bytes = Some(1);
        prune.dry_run = false;
        prune.yes = true;

        let first = manager.prune(&prune).expect("active prune");
        assert_eq!(first.results[0].action, CachePruneAction::SkippedActive);
        assert!(database.exists());
        drop(services);
        let second = manager.prune(&prune).expect("follower prune");
        assert_eq!(second.results[0].action, CachePruneAction::SkippedActive);
        drop(follower);

        let deleted = manager.prune(&prune).expect("inactive prune");
        assert_eq!(deleted.results[0].action, CachePruneAction::Deleted);
        assert!(!database.exists());
        assert!(directory.join(LEASE_NAME).exists());
        assert!(manager.list().expect("empty list").entries.is_empty());
    }

    #[test]
    fn missing_repository_requires_age_or_explicit_override() {
        let temp = tempfile::tempdir().expect("temporary directory");
        let repository = temp.path().join("offline-repository");
        fs::create_dir(&repository).expect("repository");
        let manager = CacheManager::new(temp.path().join("managed"), 10 * SECONDS_PER_DAY);
        create_current_cache(&manager, &repository, 9 * SECONDS_PER_DAY);
        fs::remove_dir(&repository).expect("take repository offline");

        let mut age_only = request();
        age_only.older_than_days = Some(30);
        let kept = manager.prune(&age_only).expect("age plan");
        assert_eq!(kept.results[0].action, CachePruneAction::Kept);

        age_only.remove_missing_roots = true;
        let selected = manager.prune(&age_only).expect("missing-root plan");
        assert_eq!(selected.results[0].action, CachePruneAction::WouldDelete);
        assert_eq!(selected.results[0].reasons, ["missing_repository"]);
    }

    #[test]
    fn lru_budget_selects_oldest_cache_and_dry_run_preserves_files() {
        let temp = tempfile::tempdir().expect("temporary directory");
        let first_root = temp.path().join("first-repository");
        let second_root = temp.path().join("second-repository");
        fs::create_dir(&first_root).expect("first repository");
        fs::create_dir(&second_root).expect("second repository");
        let manager = CacheManager::new(temp.path().join("managed"), 1_000);
        let (first_id, first) = create_current_cache(&manager, &first_root, 100);
        let (second_id, second) = create_current_cache(&manager, &second_root, 900);
        let listed = manager.list().expect("cache list");
        let oldest_size = listed
            .entries
            .iter()
            .find(|entry| entry.id == first_id)
            .expect("oldest cache")
            .size_bytes;
        let mut prune = request();
        prune.max_total_bytes = Some(listed.total_bytes - oldest_size);

        let report = manager.prune(&prune).expect("LRU plan");

        assert_eq!(report.total_bytes_before, listed.total_bytes);
        let first_result = report
            .results
            .iter()
            .find(|result| result.id == first_id)
            .expect("oldest result");
        let second_result = report
            .results
            .iter()
            .find(|result| result.id == second_id)
            .expect("newest result");
        assert_eq!(first_result.action, CachePruneAction::WouldDelete);
        assert_eq!(first_result.reasons, ["max_total_bytes"]);
        assert_eq!(second_result.action, CachePruneAction::Kept);
        assert_eq!(report.reclaimed_bytes, oldest_size);
        assert!(first.exists());
        assert!(second.exists());
    }

    #[test]
    fn corrupt_and_legacy_caches_are_listed_without_mutation() {
        let temp = tempfile::tempdir().expect("temporary directory");
        let root = temp.path().join("managed");
        let corrupt = root.join(FIRST_ID);
        fs::create_dir_all(&corrupt).expect("corrupt directory");
        fs::write(corrupt.join(DATABASE_NAME), b"not sqlite").expect("corrupt database");
        let legacy = root.join(SECOND_ID);
        fs::create_dir_all(&legacy).expect("legacy directory");
        let connection = Connection::open(legacy.join(DATABASE_NAME)).expect("legacy database");
        connection
            .execute_batch(
                "CREATE TABLE meta (
                    id INTEGER PRIMARY KEY,
                    schema_version INTEGER NOT NULL,
                    repository_root TEXT NOT NULL
                );
                INSERT INTO meta VALUES (1, 4, '');",
            )
            .expect("legacy schema");
        drop(connection);
        let manager = CacheManager::new(root, 10_000);

        let report = manager.list().expect("cache list");

        assert_eq!(report.entries[0].state, CacheState::Corrupt);
        assert_eq!(report.entries[1].state, CacheState::Legacy);
        assert!(corrupt.join(DATABASE_NAME).exists());
        assert!(legacy.join(DATABASE_NAME).exists());

        let mut prune = request();
        prune.max_total_bytes = Some(0);
        let plan = manager.prune(&prune).expect("prune plan");
        assert!(
            plan.results
                .iter()
                .all(|result| result.action == CachePruneAction::WouldDelete)
        );
        assert!(corrupt.join(DATABASE_NAME).exists());
        assert!(legacy.join(DATABASE_NAME).exists());
    }

    #[test]
    fn unexpected_content_is_never_removed_automatically() {
        let temp = tempfile::tempdir().expect("temporary directory");
        let directory = temp.path().join("managed").join(FIRST_ID);
        fs::create_dir_all(&directory).expect("cache directory");
        fs::write(directory.join(DATABASE_NAME), b"not sqlite").expect("database");
        fs::write(directory.join("keep.txt"), b"owner data").expect("unexpected file");
        let manager = CacheManager::new(temp.path().join("managed"), 10_000);
        let mut prune = request();
        prune.max_total_bytes = Some(1);
        prune.dry_run = false;
        prune.yes = true;

        let report = manager.prune(&prune).expect("prune");

        assert_eq!(report.results[0].action, CachePruneAction::SkippedUnsafe);
        assert!(directory.join(DATABASE_NAME).exists());
        assert!(directory.join("keep.txt").exists());
    }

    #[test]
    fn future_schema_and_mismatched_identity_are_never_removed_automatically() {
        let temp = tempfile::tempdir().expect("temporary directory");
        let root = temp.path().join("managed");
        let future_root = temp.path().join("future-repository");
        let mismatch_root = temp.path().join("mismatch-repository");
        fs::create_dir(&future_root).expect("future repository");
        fs::create_dir(&mismatch_root).expect("mismatch repository");
        let manager = CacheManager::new(root, 10_000);
        let (future_id, future_database) = create_current_cache(&manager, &future_root, 100);
        Connection::open(&future_database)
            .expect("future database")
            .execute(
                "UPDATE meta SET schema_version = 6, repository_root = x'80' WHERE id = 1",
                [],
            )
            .expect("future schema");
        let mismatch_id = FIRST_ID;
        assert_ne!(mismatch_id, managed_cache_id(&mismatch_root));
        let mismatch_directory = manager.root.join(mismatch_id);
        fs::create_dir_all(&mismatch_directory).expect("mismatch directory");
        let mismatch_database = mismatch_directory.join(DATABASE_NAME);
        drop(
            Storage::open_for_repository(&mismatch_database, &mismatch_root)
                .expect("mismatch database"),
        );
        let future_migration_id = SECOND_ID;
        let future_migration_directory = manager.root.join(future_migration_id);
        fs::create_dir_all(&future_migration_directory).expect("future migration directory");
        let future_migration_database = future_migration_directory.join(DATABASE_NAME);
        Connection::open(&future_migration_database)
            .expect("future migration database")
            .execute_batch("PRAGMA user_version = 7; CREATE TABLE replacement(value INTEGER);")
            .expect("future migration");
        let mut prune = request();
        prune.max_total_bytes = Some(0);
        prune.dry_run = false;
        prune.yes = true;

        let report = manager.prune(&prune).expect("prune plan");

        let future = report
            .results
            .iter()
            .find(|result| result.id == future_id)
            .expect("future result");
        let mismatch = report
            .results
            .iter()
            .find(|result| result.id == mismatch_id)
            .expect("mismatch result");
        let future_migration = report
            .results
            .iter()
            .find(|result| result.id == future_migration_id)
            .expect("future migration result");
        assert_eq!(future.action, CachePruneAction::SkippedUnsafe);
        assert_eq!(mismatch.action, CachePruneAction::SkippedUnsafe);
        assert_eq!(future_migration.action, CachePruneAction::SkippedUnsafe);
        assert!(
            future
                .detail
                .as_deref()
                .is_some_and(|detail| detail.contains("newer unsupported schema"))
        );
        assert!(future_database.exists());
        assert!(mismatch_database.exists());
        assert!(future_migration_database.exists());
    }

    #[test]
    fn stale_cache_is_deleted_after_age_and_confirmation() {
        let temp = tempfile::tempdir().expect("temporary directory");
        let repository = temp.path().join("repository");
        fs::create_dir(&repository).expect("repository");
        let manager = CacheManager::new(temp.path().join("managed"), 40 * SECONDS_PER_DAY);
        let (id, database) = create_current_cache(&manager, &repository, SECONDS_PER_DAY);
        let mut prune = request();
        prune.older_than_days = Some(30);
        prune.dry_run = false;
        prune.yes = true;

        let report = manager.prune(&prune).expect("prune stale cache");

        let result = report
            .results
            .iter()
            .find(|result| result.id == id)
            .expect("stale result");
        assert_eq!(result.action, CachePruneAction::Deleted);
        assert_eq!(result.reasons, ["older_than"]);
        assert!(!database.exists());
        assert!(database.with_extension("sqlite.lease.lock").exists());
    }

    #[test]
    fn explicit_database_outside_managed_root_is_never_considered() {
        let temp = tempfile::tempdir().expect("temporary directory");
        let repository = temp.path().join("repository");
        fs::create_dir(&repository).expect("repository");
        let explicit = temp.path().join("explicit.sqlite");
        let config = Config::discover(&repository, Some(explicit.clone())).expect("config");
        drop(Services::open(config).expect("services"));
        let manager = CacheManager::new(temp.path().join("managed"), 10_000);

        assert!(manager.list().expect("cache list").entries.is_empty());
        let mut prune = request();
        prune.max_total_bytes = Some(1);
        assert!(manager.prune(&prune).expect("prune").results.is_empty());
        assert!(explicit.exists());
    }

    #[test]
    fn prune_requires_an_explicit_policy_and_mutation_consent() {
        let temp = tempfile::tempdir().expect("temporary directory");
        let manager = CacheManager::new(temp.path().join("managed"), 10_000);
        let empty = request();
        assert!(
            manager
                .prune(&empty)
                .unwrap_err()
                .to_string()
                .contains("requires --older-than")
        );

        let mut mutation = request();
        mutation.max_total_bytes = Some(1);
        mutation.dry_run = false;
        assert!(
            manager
                .prune(&mutation)
                .unwrap_err()
                .to_string()
                .contains("requires --yes")
        );
    }
}
