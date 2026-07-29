/// Count every directory that a recursive backend may register before
/// enabling the watcher. Callback filtering does not reduce kernel watches.
fn inspect_watch_admission(
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
                complete: false,
                fallback_reason: Some(WatcherFallbackReason::AdmissionCancelled),
            };
        }
        if entries >= entry_cap {
            return WatchAdmission {
                entries,
                directories,
                complete: false,
                fallback_reason: Some(WatcherFallbackReason::AdmissionEntryLimit),
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
                        complete: false,
                        fallback_reason: Some(WatcherFallbackReason::AdmissionDirectoryLimit),
                    };
                }
            }
            // Failure to inspect a subtree means the recursive backend's
            // watch count cannot be proven bounded. Use polling instead.
            Err(_) => {
                return WatchAdmission {
                    entries,
                    directories,
                    complete: false,
                    fallback_reason: Some(WatcherFallbackReason::AdmissionError),
                };
            }
        }
    }
    WatchAdmission {
        entries,
        directories,
        complete: true,
        fallback_reason: None,
    }
}

fn relative_path(
    root: &Path,
    path: &Path,
    policy: &DiscoveryPolicy,
    directory_hint: bool,
) -> std::result::Result<Option<String>, ()> {
    if !path.starts_with(root) {
        return Ok(None);
    }
    let rel = path.strip_prefix(root).map_err(|_| ())?;
    let s = checked_slash_path(rel).map_err(|_| ())?;
    if s.is_empty() || !policy.includes_watch_path(&s, directory_hint || path.is_dir()) {
        Ok(None)
    } else {
        Ok(Some(s))
    }
}

fn event_path_is_directory(event: &Event) -> bool {
    matches!(
        event.kind,
        EventKind::Create(CreateKind::Folder) | EventKind::Remove(RemoveKind::Folder)
    )
}
