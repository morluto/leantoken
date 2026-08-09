use super::*;

pub(super) fn process_raw_event(
    raw: notify::Result<Event>,
    root: &Path,
    policy: &DiscoveryPolicy,
    pending: &mut PendingReconciliation,
) {
    let event = match raw {
        Ok(e) if !e.need_rescan() => e,
        Ok(_) => {
            pending.require_full();
            return;
        }
        Err(err) => {
            tracing::warn!(%err, "notify error");
            pending.require_full();
            return;
        }
    };

    if event.kind.is_access() || event.kind.is_other() {
        return;
    }

    let directory_hint = event_path_is_directory(&event);
    if directory_hint == DirectoryHint::Unknown {
        // Do this before generated-directory policy is applied by
        // `relative_path`; a missing rename endpoint cannot be classified
        // from filesystem metadata.
        pending.require_full();
        // The full reconciliation supersedes path-level rename bookkeeping.
        // In particular, do not let a generated-directory rename become a
        // stale per-path update merely because one endpoint disappeared.
        return;
    }
    let mut inside = Vec::with_capacity(event.paths.len());
    let mut outside = false;
    for path in &event.paths {
        match relative_path(root, path, policy, event_path_is_directory(&event)) {
            Ok(Some(rel)) => inside.push(rel),
            Ok(None) => {
                outside = true;
                tracing::warn!(
                    path = %path.display(),
                    "watcher event outside the active index boundary"
                );
            }
            Err(()) => {
                pending.require_full();
                tracing::warn!("non-UTF-8 watcher path requires full reconciliation");
            }
        }
    }

    if outside && inside.is_empty() {
        return;
    }
    for rel in inside {
        pending.insert(rel);
    }
}

pub(super) fn bound_pending_state(pending: &mut PendingReconciliation, limit: usize) {
    pending.bound(limit);
}

pub(super) fn raw_event_is_relevant(
    event: &notify::Result<Event>,
    root: &Path,
    policy: &DiscoveryPolicy,
) -> bool {
    match event {
        Ok(event) if event.need_rescan() => true,
        Ok(event) if !event.paths.is_empty() => event.paths.iter().any(|path| {
            !matches!(
                relative_path(root, path, policy, event_path_is_directory(event)),
                Ok(None)
            )
        }),
        Ok(_) | Err(_) => true,
    }
}
