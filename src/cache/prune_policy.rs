use super::*;

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

pub(super) fn remove_managed_artifacts(directory: &Path) -> RemovalOutcome {
    let mut reclaimed_bytes = 0u64;
    let database = directory.join(DATABASE_NAME);
    let paths = PRUNABLE_ARTIFACTS
        .iter()
        .map(|artifact| directory.join(artifact))
        .chain(
            COORDINATION_LOCK_SUFFIXES
                .into_iter()
                .filter(|suffix| *suffix != LEASE_LOCK_SUFFIX)
                .map(|suffix| coordination_sidecar_path(&database, suffix)),
        );
    for path in paths {
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
