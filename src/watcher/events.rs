fn process_raw_event(
    raw: notify::Result<Event>,
    root: &Path,
    policy: DiscoveryPolicy,
    pending: &mut BTreeSet<String>,
    rename_from: &mut HashMap<usize, String>,
    rename_to: &mut HashMap<usize, String>,
    reconcile: &mut bool,
) {
    let event = match raw {
        Ok(e) if !e.need_rescan() => e,
        Ok(_) => {
            *reconcile = true;
            return;
        }
        Err(err) => {
            tracing::warn!(%err, "notify error");
            *reconcile = true;
            return;
        }
    };

    if event.kind.is_access() || event.kind.is_other() {
        return;
    }

    let mut inside = Vec::with_capacity(event.paths.len());
    let mut outside = false;
    for path in &event.paths {
        match relative_path(root, path, policy, event_path_is_directory(&event)) {
            Ok(Some(rel)) => inside.push(rel),
            Ok(None) => {
                outside = true;
                tracing::warn!(path = %path.display(), "watcher event outside root");
            }
            Err(()) => {
                *reconcile = true;
                tracing::warn!("non-UTF-8 watcher path requires full reconciliation");
            }
        }
    }

    if matches!(event.kind, EventKind::Modify(ModifyKind::Name(_))) {
        handle_rename(
            &event,
            inside,
            outside,
            pending,
            rename_from,
            rename_to,
            reconcile,
        );
    } else {
        if outside && inside.is_empty() {
            return;
        }
        for rel in inside {
            pending.insert(rel);
        }
    }
}

fn bound_pending_state(
    pending: &mut BTreeSet<String>,
    rename_from: &mut HashMap<usize, String>,
    rename_to: &mut HashMap<usize, String>,
    reconcile: &mut bool,
    limit: usize,
) {
    let retained = pending
        .len()
        .saturating_add(rename_from.len())
        .saturating_add(rename_to.len());
    if *reconcile || retained > limit {
        *reconcile = true;
        pending.clear();
        rename_from.clear();
        rename_to.clear();
    }
}

fn raw_event_is_relevant(
    event: &notify::Result<Event>,
    root: &Path,
    policy: DiscoveryPolicy,
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

fn handle_rename(
    event: &Event,
    inside: Vec<String>,
    outside: bool,
    pending: &mut BTreeSet<String>,
    rename_from: &mut HashMap<usize, String>,
    rename_to: &mut HashMap<usize, String>,
    reconcile: &mut bool,
) {
    if outside {
        *reconcile = true;
        return;
    }
    if inside.is_empty() {
        return;
    }
    if inside.len() == 2 {
        pending.insert(inside[0].clone());
        pending.insert(inside[1].clone());
        if let Some(cookie) = event.tracker() {
            rename_from.remove(&cookie);
            rename_to.remove(&cookie);
        }
        return;
    }
    if inside.len() > 2 {
        *reconcile = true;
        return;
    }

    let rel = inside.into_iter().next().unwrap();
    let Some(cookie) = event.tracker() else {
        *reconcile = true;
        return;
    };

    let mode = match event.kind {
        EventKind::Modify(ModifyKind::Name(mode)) => mode,
        _ => {
            *reconcile = true;
            return;
        }
    };

    match mode {
        RenameMode::From => {
            if let Some(to) = rename_to.remove(&cookie) {
                pending.insert(rel);
                pending.insert(to);
                rename_from.remove(&cookie);
            } else {
                rename_from.insert(cookie, rel);
            }
        }
        RenameMode::To => {
            if let Some(from) = rename_from.remove(&cookie) {
                pending.insert(from);
                pending.insert(rel);
                rename_to.remove(&cookie);
            } else {
                rename_to.insert(cookie, rel);
            }
        }
        _ => {
            *reconcile = true;
            rename_from.remove(&cookie);
            rename_to.remove(&cookie);
        }
    }
}
