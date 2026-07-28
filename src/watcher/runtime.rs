/// Joined filesystem watcher for one repository root.
pub struct RepositoryWatcher {
    root: PathBuf,
    token: CancellationToken,
    handle: JoinHandle<()>,
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

    async fn start_with_factory(
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
        let raw_capacity = capacity.saturating_mul(4).max(64);
        let watched_root = root.clone();

        let handle = tokio::spawn(async move {
            let (raw_tx, mut raw_rx) = mpsc::channel::<notify::Result<Event>>(raw_capacity);
            let overflowed = Arc::new(AtomicBool::new(false));
            let config = Config::default().with_follow_symlinks(false);
            let callback_root = watched_root.clone();

            let admission_root = watched_root.clone();
            let admission_cancellation = cancellation.clone();
            let directory_count = tokio::task::spawn_blocking(move || {
                count_watch_directories(
                    &admission_root,
                    MAX_WATCHED_DIRECTORIES,
                    MAX_WATCH_ADMISSION_ENTRIES,
                    &admission_cancellation,
                )
            })
            .await
            .unwrap_or(MAX_WATCHED_DIRECTORIES.saturating_add(1));
            let mut watcher = None;
            let mut watch_enabled = false;
            if directory_count <= MAX_WATCHED_DIRECTORIES {
                let callback: EventCallback = Box::new({
                    let overflowed = Arc::clone(&overflowed);
                    move |event: notify::Result<Event>| {
                        if !raw_event_is_relevant(&event, &callback_root, policy) {
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
                                tracing::warn!(
                                    %error,
                                    "filesystem watcher registration failed; \
                                     falling back to periodic full reconciliation"
                                );
                            }
                        }
                    }
                    Err(error) => {
                        tracing::warn!(
                            %error,
                            "filesystem watcher creation failed; \
                             falling back to periodic full reconciliation"
                        );
                    }
                }
            } else {
                tracing::warn!(
                    directories = directory_count,
                    cap = MAX_WATCHED_DIRECTORIES,
                    "root has too many directories for recursive watching; \
                     falling back to periodic full reconciliation"
                );
            }
            let _ = ready_tx.send(());

            let long_sleep = Duration::from_secs(60 * 60 * 24 * 365 * 10);
            let mut sleep = Box::pin(sleep(long_sleep));
            let mut pending = BTreeSet::<String>::new();
            let mut rename_from = HashMap::<usize, String>::new();
            let mut rename_to = HashMap::<usize, String>::new();
            let mut reconcile = false;
            let mut poll_timer = if !watch_enabled {
                tokio::time::interval(poll_interval)
            } else {
                tokio::time::interval(Duration::from_secs(60 * 60 * 24 * 365 * 10))
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
                                policy,
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
            );
            drop(watcher);
        });

        match ready_rx.await {
            Ok(()) => Ok((
                Self {
                    root,
                    token: task_token,
                    handle,
                },
                rx,
            )),
            Err(_) => {
                let _ = handle.await;
                Err(Error::InternalFailure(
                    "watcher task terminated unexpectedly".into(),
                ))
            }
        }
    }

    /// Return the canonical watched root.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Cancel and join the watcher task.
    pub async fn shutdown(self) -> Result<()> {
        self.token.cancel();
        self.handle.await?;
        Ok(())
    }
}
