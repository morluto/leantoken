async fn run_index_leader(services: Arc<Services>, cancellation: CancellationToken) -> Result<()> {
    let (watcher, changes) = RepositoryWatcher::start_with_policy(
        &services.config().root,
        WATCHER_QUEUE_CAPACITY,
        services.config().watcher_debounce,
        services.config().discovery_policy(),
        cancellation.child_token(),
    )
    .await?;

    let result = run_index_leader_until_shutdown(services, changes, cancellation).await;
    let shutdown = watcher.shutdown().await;
    match (result, shutdown) {
        (Err(error), Err(shutdown_error)) => {
            tracing::warn!(%shutdown_error, "watcher shutdown failed after index error");
            Err(error)
        }
        (Err(error), Ok(())) => Err(error),
        (Ok(()), shutdown) => shutdown,
    }
}

async fn run_index_leader_until_shutdown(
    services: Arc<Services>,
    changes: tokio::sync::mpsc::Receiver<WatcherMessage>,
    cancellation: CancellationToken,
) -> Result<()> {
    // The watcher is registered before the scan. Events queued during the scan
    // are applied afterward, closing the startup gap without a second walk.
    let indexed = services
        .index_cancellable(false, cancellation.clone())
        .await;
    let indexed = match indexed {
        Ok(indexed) => indexed,
        Err(leantoken::Error::Cancelled) if cancellation.is_cancelled() => return Ok(()),
        Err(error) => return Err(error),
    };
    for warning in &indexed.warnings {
        tracing::warn!(%warning, "index warning");
    }

    run_watcher_reconciliations(services, changes, cancellation).await
}

async fn run_watcher_reconciliations(
    services: Arc<Services>,
    mut changes: tokio::sync::mpsc::Receiver<WatcherMessage>,
    cancellation: CancellationToken,
) -> Result<()> {
    let mut scheduler = WatcherReconciliationScheduler::new(services.config().watcher_debounce);

    loop {
        let changes_open = drain_watcher_messages(&mut scheduler, &services, &mut changes);

        let Some(deadline) = scheduler.next_deadline() else {
            if !changes_open {
                break;
            }
            tokio::select! {
                _ = cancellation.cancelled() => break,
                message = changes.recv() => match message {
                    Some(message) => schedule_watcher_message(&mut scheduler, &services, message),
                    None => break,
                }
            }
            continue;
        };

        tokio::select! {
            _ = cancellation.cancelled() => break,
            message = changes.recv(), if changes_open => match message {
                Some(message) => schedule_watcher_message(&mut scheduler, &services, message),
                None => continue,
            },
            _ = tokio::time::sleep_until(deadline) => {
                let Some(action) = scheduler.take_ready(Instant::now()) else {
                    continue;
                };
                if !execute_watcher_action(
                    &mut scheduler,
                    Arc::clone(&services),
                    action,
                    cancellation.clone(),
                ).await? {
                    break;
                }
            }
        }
    }

    Ok(())
}

fn drain_watcher_messages(
    scheduler: &mut WatcherReconciliationScheduler,
    services: &Services,
    changes: &mut tokio::sync::mpsc::Receiver<WatcherMessage>,
) -> bool {
    loop {
        match changes.try_recv() {
            Ok(message) => schedule_watcher_message(scheduler, services, message),
            Err(tokio::sync::mpsc::error::TryRecvError::Empty) => return true,
            Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => return false,
        }
    }
}

async fn execute_watcher_action(
    scheduler: &mut WatcherReconciliationScheduler,
    services: Arc<Services>,
    action: WatcherAction,
    cancellation: CancellationToken,
) -> Result<bool> {
    match reconcile_watcher_action(services, &action, cancellation.clone()).await {
        Ok(indexed) => {
            scheduler.finish_success(&action, Instant::now());
            for warning in &indexed.warnings {
                tracing::warn!(%warning, "index warning");
            }
            Ok(true)
        }
        Err(leantoken::Error::Cancelled) if cancellation.is_cancelled() => Ok(false),
        Err(error) if is_terminal_index_error(&error) => Err(error),
        Err(error) => {
            scheduler.finish_failure(action, Instant::now());
            let retry_at = scheduler.next_deadline();
            tracing::error!(
                %error,
                retry_delay_ms = retry_at.map_or(0, |at| at.saturating_duration_since(Instant::now()).as_millis()),
                "background reconciliation failed; retained for retry"
            );
            Ok(true)
        }
    }
}

fn is_terminal_index_error(error: &leantoken::Error) -> bool {
    matches!(
        error,
        leantoken::Error::RootNotFound(_)
            | leantoken::Error::UnsafeRepositoryRoot(_)
            | leantoken::Error::IndexLimitExceeded { .. }
            | leantoken::Error::InvalidConfiguration(_)
            | leantoken::Error::RepositoryMismatch { .. }
            | leantoken::Error::RuntimeCapabilityUnavailable { .. }
    )
}

fn schedule_watcher_message(
    scheduler: &mut WatcherReconciliationScheduler,
    services: &Services,
    message: WatcherMessage,
) {
    let message = match message {
        WatcherMessage::Changed { paths } => {
            let paths = paths
                .into_iter()
                .filter(|path| !services.config().is_database_artifact(path))
                .collect::<Vec<_>>();
            if paths.is_empty() {
                return;
            }
            WatcherMessage::Changed { paths }
        }
        WatcherMessage::ReconcileRequired => WatcherMessage::ReconcileRequired,
    };
    scheduler.enqueue(message, Instant::now());
}

async fn reconcile_watcher_action(
    services: Arc<Services>,
    action: &WatcherAction,
    cancellation: CancellationToken,
) -> Result<leantoken::model::IndexResponse> {
    match action {
        WatcherAction::Paths(paths) => {
            tracing::debug!(changed_paths = paths.len(), "repository change detected");
            services
                .index_paths_cancellable(paths.clone(), cancellation)
                .await
        }
        WatcherAction::Full => {
            tracing::warn!("watcher scheduled bounded full reconciliation");
            services.index_cancellable(false, cancellation).await
        }
    }
}
