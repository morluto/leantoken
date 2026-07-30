use super::*;

/// Joined filesystem watcher for one repository root.
pub struct RepositoryWatcher {
    root: PathBuf,
    token: CancellationToken,
    handle: JoinHandle<()>,
    ready: WatcherReady,
    counters: Arc<WatcherCounters>,
}

pub(super) const PRODUCTION_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);

pub(crate) async fn join_watcher(handle: JoinHandle<()>) -> Result<()> {
    timeout(PRODUCTION_SHUTDOWN_TIMEOUT, handle)
        .await
        .map_err(|_| Error::ShutdownTimeout {
            component: "repository watcher",
        })??;
    Ok(())
}

impl RepositoryWatcher {
    /// Start watching a canonical repository root.
    ///
    /// `capacity` bounds both the public message queue and the internal raw
    /// event queue. It also derives the bound for retained paths and incomplete
    /// rename cookies. Queue or retained-state overflow degrades to
    /// [`WatcherMessage::ReconcileRequired`].
    pub async fn start(
        root: impl AsRef<Path>,
        capacity: usize,
        debounce: Duration,
        token: CancellationToken,
    ) -> Result<(Self, mpsc::Receiver<WatcherMessage>)> {
        Self::start_with_policy(root, capacity, debounce, DiscoveryPolicy::default(), token).await
    }

    /// Start watching with the same visibility policy used by discovery.
    pub async fn start_with_policy(
        root: impl AsRef<Path>,
        capacity: usize,
        debounce: Duration,
        policy: DiscoveryPolicy,
        token: CancellationToken,
    ) -> Result<(Self, mpsc::Receiver<WatcherMessage>)> {
        Self::start_with_factory(
            root,
            capacity,
            debounce,
            policy,
            token,
            recommended_watcher,
            WATCHER_POLL_INTERVAL,
        )
        .await
    }

    pub(crate) async fn start_with_factory(
        root: impl AsRef<Path>,
        capacity: usize,
        debounce: Duration,
        policy: DiscoveryPolicy,
        cancellation: CancellationToken,
        watcher_factory: WatcherFactory,
        poll_interval: Duration,
    ) -> Result<(Self, mpsc::Receiver<WatcherMessage>)> {
        let root = root.as_ref().canonicalize().map_err(Error::Io)?;
        if !root.is_dir() {
            return Err(Error::InvalidConfiguration(format!(
                "root is not a directory: {}",
                root.display()
            )));
        }

        let (tx, rx) = mpsc::channel::<WatcherMessage>(capacity.max(1));
        let (ready_tx, ready_rx) = oneshot::channel();
        let task_token = cancellation.clone();
        let counters = Arc::new(WatcherCounters::default());
        let task_counters = Arc::clone(&counters);
        let raw_capacity = capacity.saturating_mul(4).max(64);
        let watched_root = root.clone();

        let handle = tokio::spawn(async move {
            let (raw_tx, mut raw_rx) = mpsc::channel::<notify::Result<Event>>(raw_capacity);
            let overflowed = Arc::new(AtomicBool::new(false));
            let config = Config::default().with_follow_symlinks(false);
            let callback_root = watched_root.clone();
            let callback_policy = policy.clone();

            let admission_root = watched_root.clone();
            let admission_cancellation = cancellation.clone();
            let admission = tokio::task::spawn_blocking(move || {
                inspect_watch_admission(
                    &admission_root,
                    MAX_WATCHED_DIRECTORIES,
                    MAX_WATCH_ADMISSION_ENTRIES,
                    &admission_cancellation,
                )
            })
            .await
            .unwrap_or(WatchAdmission {
                entries: 0,
                directories: 0,
                complete: false,
                fallback_reason: Some(WatcherFallbackReason::AdmissionError),
            });
            let mut watcher = None;
            let mut watch_enabled = false;
            let mut fallback_reason = admission.fallback_reason;
            if fallback_reason.is_none() {
                let callback: EventCallback = Box::new({
                    let overflowed = Arc::clone(&overflowed);
                    move |event: notify::Result<Event>| {
                        if !raw_event_is_relevant(&event, &callback_root, &callback_policy) {
                            return;
                        }
                        if let Err(TrySendError::Full(_)) = raw_tx.try_send(event) {
                            overflowed.store(true, Ordering::Release);
                        }
                    }
                });
                match watcher_factory(callback, config) {
                    Ok(mut candidate) => {
                        match candidate.watch(&watched_root, RecursiveMode::Recursive) {
                            Ok(()) => {
                                watch_enabled = true;
                                watcher = Some(candidate);
                            }
                            Err(error) => {
                                fallback_reason =
                                    Some(WatcherFallbackReason::BackendRegistrationFailed);
                                tracing::warn!(
                                    %error,
                                    "filesystem watcher registration failed; \
                                     falling back to periodic full reconciliation"
                                );
                            }
                        }
                    }
                    Err(error) => {
                        fallback_reason = Some(WatcherFallbackReason::BackendCreationFailed);
                        tracing::warn!(
                            %error,
                            "filesystem watcher creation failed; \
                             falling back to periodic full reconciliation"
                        );
                    }
                }
            } else {
                tracing::warn!(
                    entries = admission.entries,
                    directories = admission.directories,
                    cap = MAX_WATCHED_DIRECTORIES,
                    reason = ?fallback_reason,
                    "native recursive watcher admission did not complete; \
                     falling back to periodic full reconciliation"
                );
            }
            let backend = if watch_enabled {
                WatcherBackend::Native
            } else {
                WatcherBackend::PeriodicPolling
            };
            tracing::info!(
                ?backend,
                ?fallback_reason,
                admission_entries = admission.entries,
                admission_directories = admission.directories,
                admission_complete = admission.complete,
                "repository watcher initialized"
            );
            let _ = ready_tx.send(WatcherReady {
                backend,
                fallback_reason,
                admission,
            });

            let long_sleep = Duration::from_secs(60 * 60 * 24 * 365 * 10);
            let mut sleep = Box::pin(sleep(long_sleep));
            let mut pending = BTreeSet::<String>::new();
            let mut rename_from = HashMap::<usize, String>::new();
            let mut rename_to = HashMap::<usize, String>::new();
            let mut reconcile = false;
            let poll_started_at = Instant::now()
                + if watch_enabled {
                    long_sleep
                } else {
                    poll_interval
                };
            let mut poll_timer = if !watch_enabled {
                interval_at(poll_started_at, poll_interval)
            } else {
                interval_at(poll_started_at, long_sleep)
            };
            poll_timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

            loop {
                if overflowed.swap(false, Ordering::Acquire) {
                    reconcile = true;
                    pending.clear();
                    rename_from.clear();
                    rename_to.clear();
                }
                if reconcile {
                    sleep.as_mut().reset(Instant::now());
                }

                tokio::select! {
                    biased;
                    _ = cancellation.cancelled() => break,
                    Some(raw) = raw_rx.recv() => {
                        if !reconcile {
                            process_raw_event(
                                raw,
                                &watched_root,
                                &policy,
                                &mut pending,
                                &mut rename_from,
                                &mut rename_to,
                                &mut reconcile,
                            );
                            bound_pending_state(
                                &mut pending,
                                &mut rename_from,
                                &mut rename_to,
                                &mut reconcile,
                                raw_capacity,
                            );
                        } else {
                            if let Err(err) = raw {
                                tracing::warn!(%err, "notify error");
                            }
                        }
                        if reconcile {
                            sleep.as_mut().reset(Instant::now());
                        } else if !pending.is_empty() {
                            sleep.as_mut().reset(Instant::now() + debounce);
                        } else {
                            sleep.as_mut().reset(Instant::now() + long_sleep);
                        }
                    }
                    _ = poll_timer.tick() => {
                        if !watch_enabled {
                            task_counters.poll_ticks.fetch_add(1, Ordering::Relaxed);
                            reconcile = true;
                        }
                    }
                    _ = sleep.as_mut() => {
                        if !flush(
                            &mut pending,
                            &mut rename_from,
                            &mut rename_to,
                            &mut reconcile,
                            &tx,
                            &task_counters,
                        ) {
                            return;
                        }
                        if reconcile {
                            sleep.as_mut().reset(Instant::now() + debounce);
                        } else if pending.is_empty() {
                            sleep.as_mut().reset(Instant::now() + long_sleep);
                        } else {
                            sleep.as_mut().reset(Instant::now() + debounce);
                        }
                    }
                    else => break,
                }
            }

            let _ = flush(
                &mut pending,
                &mut rename_from,
                &mut rename_to,
                &mut reconcile,
                &tx,
                &task_counters,
            );
            drop(watcher);
        });

        match ready_rx.await {
            Ok(ready) => Ok((
                Self {
                    root,
                    token: task_token,
                    handle,
                    ready,
                    counters,
                },
                rx,
            )),
            Err(_) => {
                let _ = handle.await;
                Err(Error::OperationFailure(
                    "watcher task terminated unexpectedly".into(),
                ))
            }
        }
    }

    /// Return the canonical watched root.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Return bounded watcher backend, admission, polling, and delivery counters.
    pub fn diagnostics(&self) -> WatcherDiagnostics {
        diagnostics_snapshot(&self.ready, &self.counters)
    }

    /// Cancel and join the watcher task.
    pub async fn shutdown(self) -> Result<()> {
        self.shutdown_with_diagnostics().await?;
        Ok(())
    }

    /// Cancel and join the watcher task, returning its final bounded counters.
    pub async fn shutdown_with_diagnostics(self) -> Result<WatcherDiagnostics> {
        let Self {
            token,
            handle,
            ready,
            counters,
            ..
        } = self;
        token.cancel();
        join_watcher(handle).await?;
        Ok(diagnostics_snapshot(&ready, &counters))
    }
}

pub(super) fn diagnostics_snapshot(
    ready: &WatcherReady,
    counters: &WatcherCounters,
) -> WatcherDiagnostics {
    WatcherDiagnostics {
        backend: ready.backend,
        fallback_reason: ready.fallback_reason,
        admission_entries: ready.admission.entries,
        admission_directories: ready.admission.directories,
        admission_complete: ready.admission.complete,
        poll_ticks: counters.poll_ticks.load(Ordering::Relaxed),
        changed_path_deliveries: counters.changed_path_deliveries.load(Ordering::Relaxed),
        full_reconciliation_deliveries: counters
            .full_reconciliation_deliveries
            .load(Ordering::Relaxed),
    }
}
