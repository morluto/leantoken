/// Count every directory that a recursive backend may register before
/// enabling the watcher. Callback filtering does not reduce kernel watches.
fn count_watch_directories(
    root: &Path,
    directory_cap: usize,
    entry_cap: usize,
    cancellation: &CancellationToken,
) -> usize {
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
    let mut count = 0usize;
    for (entries, entry) in walker.enumerate() {
        if entries >= entry_cap || cancellation.is_cancelled() {
            return directory_cap.saturating_add(1);
        }
        match entry {
            Ok(entry) => {
                if !entry.file_type().map(|ft| ft.is_dir()).unwrap_or(false) {
                    continue;
                }
                count += 1;
                if count > directory_cap {
                    return count;
                }
            }
            // Failure to inspect a subtree means the recursive backend's
            // watch count cannot be proven bounded. Use polling instead.
            Err(_) => return directory_cap.saturating_add(1),
        }
    }
    count
}

fn relative_path(
    root: &Path,
    path: &Path,
    policy: DiscoveryPolicy,
    directory_hint: bool,
) -> std::result::Result<Option<String>, ()> {
    if !path.starts_with(root) {
        return Ok(None);
    }
    let rel = path.strip_prefix(root).map_err(|_| ())?;
    let s = checked_slash_path(rel).map_err(|_| ())?;
    if s.is_empty()
        || s.starts_with(".git/")
        || s == ".git"
        || !policy.includes_path(&s, directory_hint || path.is_dir())
    {
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
