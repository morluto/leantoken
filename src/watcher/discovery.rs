use super::*;

/// Count every directory that a recursive backend may register before
/// enabling the watcher. Callback filtering does not reduce kernel watches.
pub(super) fn inspect_watch_admission(
    root: &Path,
    directory_cap: usize,
    entry_cap: usize,
    cancellation: &CancellationToken,
) -> WatchAdmission {
    use ignore::WalkBuilder;
    let mut builder = WalkBuilder::new(root);
    builder.hidden(false);
    builder.ignore(false);
    builder.parents(false);
    builder.git_ignore(false);
    builder.git_exclude(false);
    builder.git_global(false);
    builder.follow_links(false);
    let walker = builder.build();
    let mut entries = 0usize;
    let mut directories = 0usize;
    for entry in walker {
        if cancellation.is_cancelled() {
            return WatchAdmission {
                entries,
                directories,
                outcome: WatchAdmissionOutcome::Fallback(WatcherFallbackReason::AdmissionCancelled),
            };
        }
        if entries >= entry_cap {
            return WatchAdmission {
                entries,
                directories,
                outcome: WatchAdmissionOutcome::Fallback(
                    WatcherFallbackReason::AdmissionEntryLimit,
                ),
            };
        }
        entries += 1;
        match entry {
            Ok(entry) => {
                if !entry.file_type().map(|ft| ft.is_dir()).unwrap_or(false) {
                    continue;
                }
                directories += 1;
                if directories > directory_cap {
                    return WatchAdmission {
                        entries,
                        directories,
                        outcome: WatchAdmissionOutcome::Fallback(
                            WatcherFallbackReason::AdmissionDirectoryLimit,
                        ),
                    };
                }
            }
            // Failure to inspect a subtree means the recursive backend's
            // watch count cannot be proven bounded. Use polling instead.
            Err(_) => {
                return WatchAdmission {
                    entries,
                    directories,
                    outcome: WatchAdmissionOutcome::Fallback(WatcherFallbackReason::AdmissionError),
                };
            }
        }
    }
    WatchAdmission {
        entries,
        directories,
        outcome: WatchAdmissionOutcome::Complete,
    }
}

pub(super) fn relative_path(
    root: &Path,
    path: &Path,
    policy: &DiscoveryPolicy,
    directory_hint: DirectoryHint,
) -> std::result::Result<Option<String>, ()> {
    if !path.starts_with(root) {
        return Ok(None);
    }
    let rel = path.strip_prefix(root).map_err(|_| ())?;
    let s = checked_slash_path(rel).map_err(|_| ())?;
    if s.is_empty() {
        Ok(None)
    } else if directory_hint == DirectoryHint::Unknown {
        // Rename notifications can refer to a source that has already
        // disappeared. Admit the path before generated-tree filtering so the
        // ambiguity reaches bounded reconciliation.
        Ok(Some(s))
    } else if !policy.includes_watch_path(&s, directory_hint == DirectoryHint::Yes || path.is_dir())
    {
        Ok(None)
    } else {
        Ok(Some(s))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum DirectoryHint {
    Yes,
    No,
    Unknown,
}

pub(super) fn event_path_is_directory(event: &Event) -> DirectoryHint {
    matches!(
        event.kind,
        EventKind::Create(CreateKind::Folder) | EventKind::Remove(RemoveKind::Folder)
    )
    .then_some(DirectoryHint::Yes)
    .unwrap_or({
        if matches!(event.kind, EventKind::Modify(ModifyKind::Name(_))) {
            DirectoryHint::Unknown
        } else {
            DirectoryHint::No
        }
    })
}
