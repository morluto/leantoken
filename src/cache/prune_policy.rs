use super::*;
use crate::coordination::COORDINATION_LOCK_SUFFIXES;
use cap_std::fs::Dir;
use std::ffi::OsString;

pub(super) fn validate_prune_request(
    request: &CachePruneRequest,
    incompatible_with_current: bool,
) -> Result<()> {
    if request.older_than_days.is_none()
        && request.max_total_bytes.is_none()
        && !request.remove_missing_roots
        && !incompatible_with_current
    {
        return Err(Error::InvalidRequest(
            "cache prune requires --older-than, --max-total-bytes, \
             --remove-missing-roots, or --incompatible-with-current"
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

pub(super) fn select_prune_candidates(
    entries: &[InspectedCache],
    request: &CachePruneRequest,
    total_bytes: u64,
    incompatible_with_current: bool,
) -> BTreeMap<String, Vec<String>> {
    let mut selected = BTreeMap::<String, Vec<String>>::new();
    let minimum_age = request
        .older_than_days
        .map(|days| days.saturating_mul(SECONDS_PER_DAY));
    for cache in entries {
        if incompatible_with_current && cache.compatibility.safely_incompatible() {
            selected
                .entry(cache.entry.id.clone())
                .or_default()
                .push(format!(
                    "incompatible_with_current:{}",
                    cache.compatibility.label()
                ));
        }
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
        .filter(|cache| {
            !selected.contains_key(&cache.entry.id) && cache.safe_to_prune && !cache.entry.active
        })
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
        projected = projected.saturating_sub(cache.entry.size_bytes);
    }
    selected
}

pub(super) fn prune_result(
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

pub(super) struct RemovalOutcome {
    pub(super) reclaimed_bytes: u64,
    pub(super) error: Option<String>,
}

fn ensure_real_directory(directory: &Path) -> std::result::Result<(), String> {
    let metadata = fs::symlink_metadata(directory).map_err(|error| error.to_string())?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(format!(
            "cache directory is not a real directory: {}",
            directory.display()
        ));
    }
    Ok(())
}

fn prepare_removal(directory: &Path) -> std::result::Result<(Dir, Vec<(OsString, u64)>), String> {
    ensure_real_directory(directory)?;
    let handle = Dir::open_ambient_dir(directory, cap_std::ambient_authority())
        .map_err(|error| error.to_string())?;
    // Check the path again after opening the handle. If it was replaced by a
    // symlink, the handle remains anchored to the directory that was opened,
    // but pruning must still fail closed rather than report success.
    ensure_real_directory(directory)?;

    let database = directory.join(DATABASE_NAME);
    let lease_name = coordination_sidecar_path(&database, LEASE_LOCK_SUFFIX)
        .file_name()
        .expect("database sidecar has a file name")
        .to_owned();
    let mut artifacts = BTreeMap::<OsString, u64>::new();
    for entry in handle.entries().map_err(|error| error.to_string())? {
        let entry = entry.map_err(|error| error.to_string())?;
        let name = entry.file_name();
        let path = directory.join(&name);
        let known = name
            .to_str()
            .is_some_and(|name| PRUNABLE_ARTIFACTS.contains(&name))
            || COORDINATION_LOCK_SUFFIXES
                .into_iter()
                .any(|suffix| path == coordination_sidecar_path(&database, suffix));
        if !known {
            return Err(format!(
                "cache directory contains an unexpected entry: {}",
                path.display()
            ));
        }
        let metadata = handle
            .symlink_metadata(&name)
            .map_err(|error| error.to_string())?;
        if metadata.file_type().is_symlink() || !metadata.file_type().is_file() {
            return Err(format!(
                "cache directory contains a non-regular artifact: {}",
                path.display()
            ));
        }
        if name != lease_name {
            artifacts.insert(name, metadata.len());
        }
    }
    Ok((handle, artifacts.into_iter().collect()))
}

pub(super) fn remove_managed_artifacts(directory: &Path) -> RemovalOutcome {
    let (directory, artifacts) = match prepare_removal(directory) {
        Ok(value) => value,
        Err(error) => {
            return RemovalOutcome {
                reclaimed_bytes: 0,
                error: Some(error),
            };
        }
    };
    let mut reclaimed_bytes = 0u64;
    for (name, size) in artifacts {
        match directory.symlink_metadata(&name) {
            Ok(metadata) if metadata.file_type().is_file() => {}
            Ok(_) => {
                return RemovalOutcome {
                    reclaimed_bytes,
                    error: Some(format!(
                        "cache artifact is no longer a regular file: {}",
                        name.to_string_lossy()
                    )),
                };
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                return RemovalOutcome {
                    reclaimed_bytes,
                    error: Some(error.to_string()),
                };
            }
        }
        match directory.remove_file(&name) {
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

pub(super) fn is_cache_id(value: &str) -> bool {
    parse_managed_cache_id(value).is_some()
}

pub(super) fn root_available(path: &Path) -> Option<bool> {
    match fs::metadata(path) {
        Ok(_) => Some(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Some(false),
        Err(_) => None,
    }
}

pub(super) fn unix_seconds(time: SystemTime) -> u64 {
    time.duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs())
}
