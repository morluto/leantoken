use super::*;

async fn assert_full_queue_preserves_retry_deadline(debounce: Duration, poll_interval: Duration) {
    let root = tempfile::tempdir().unwrap();
    let (watcher, mut messages) = RepositoryWatcher::start_with_factory(
        root.path(),
        1,
        debounce,
        DiscoveryPolicy::default(),
        CancellationToken::new(),
        creation_failure,
        poll_interval,
    )
    .await
    .unwrap();

    // Leave the first periodic reconciliation in the single-slot delivery queue.
    advance(poll_interval + Duration::from_millis(1)).await;
    for _ in 0..8 {
        tokio::task::yield_now().await;
    }
    advance(Duration::from_millis(1)).await;
    assert_eq!(messages.len(), 1);
    advance(poll_interval).await;
    for _ in 0..8 {
        tokio::task::yield_now().await;
    }
    advance(Duration::from_millis(1)).await;
    assert!(watcher.diagnostics().poll_ticks >= 2);

    let retry_delay = debounce.max(Duration::from_millis(10));
    advance(retry_delay / 2).await;
    assert_eq!(messages.try_recv(), Ok(WatcherMessage::ReconcileRequired));
    advance(Duration::from_millis(1)).await;
    for _ in 0..8 {
        tokio::task::yield_now().await;
    }
    assert_eq!(
        messages.try_recv(),
        Err(tokio::sync::mpsc::error::TryRecvError::Empty),
        "a full queue must retain its retry deadline after capacity becomes available"
    );

    advance(retry_delay).await;
    for _ in 0..8 {
        tokio::task::yield_now().await;
    }
    assert_eq!(messages.try_recv(), Ok(WatcherMessage::ReconcileRequired));
    assert_eq!(watcher.diagnostics().full_reconciliation_deliveries, 2);
    timeout(Duration::from_secs(1), watcher.shutdown())
        .await
        .expect("shutdown timeout")
        .unwrap();
}

#[tokio::test(start_paused = true)]
async fn full_delivery_queue_retains_retry_deadline_and_pending_reconciliation() {
    assert_full_queue_preserves_retry_deadline(Duration::from_millis(100), Duration::from_secs(1))
        .await;
}

#[tokio::test(start_paused = true)]
async fn repeated_poll_ticks_do_not_restart_full_queue_retry() {
    assert_full_queue_preserves_retry_deadline(
        Duration::from_millis(100),
        Duration::from_millis(10),
    )
    .await;
}

#[tokio::test(start_paused = true)]
async fn zero_debounce_still_delays_full_queue_retry() {
    assert_full_queue_preserves_retry_deadline(Duration::ZERO, Duration::from_secs(1)).await;
}

#[tokio::test(start_paused = true)]
async fn shutdown_joins_while_full_reconciliation_is_backpressured() {
    let root = tempfile::tempdir().unwrap();
    let (watcher, mut messages) = RepositoryWatcher::start_with_factory(
        root.path(),
        1,
        Duration::from_secs(10),
        DiscoveryPolicy::default(),
        CancellationToken::new(),
        creation_failure,
        Duration::from_secs(1),
    )
    .await
    .unwrap();
    advance(Duration::from_secs(1)).await;
    for _ in 0..8 {
        tokio::task::yield_now().await;
    }
    advance(Duration::from_millis(1)).await;
    assert_eq!(messages.len(), 1);
    advance(Duration::from_secs(1)).await;
    for _ in 0..8 {
        tokio::task::yield_now().await;
    }
    advance(Duration::from_millis(1)).await;
    assert_eq!(watcher.diagnostics().poll_ticks, 2);
    timeout(Duration::from_millis(50), watcher.shutdown())
        .await
        .expect("shutdown must not wait for queue capacity or delivery retry")
        .unwrap();
    assert_eq!(messages.try_recv(), Ok(WatcherMessage::ReconcileRequired));
    assert_eq!(
        messages.try_recv(),
        Err(tokio::sync::mpsc::error::TryRecvError::Disconnected)
    );
}

#[tokio::test(start_paused = true)]
async fn expired_full_queue_retry_progresses_during_continuous_raw_events() {
    let root = tempfile::tempdir().unwrap();
    let (callback_tx, callback_rx) = oneshot::channel();
    let (watcher, mut messages) = RepositoryWatcher::start_with_factory(
        root.path(),
        64,
        Duration::from_millis(100),
        DiscoveryPolicy::default(),
        CancellationToken::new(),
        move |callback, _config| {
            assert!(callback_tx.send(callback).is_ok());
            Ok(registration_success())
        },
        Duration::from_secs(30),
    )
    .await
    .unwrap();
    let mut callback = callback_rx.await.unwrap();
    assert_eq!(watcher.diagnostics().backend, WatcherBackend::Native);

    // Fill delivery, then retain one failed full reconciliation. The larger
    // raw queue lets a producer keep it ready across cooperative task yields.
    for _ in 0..65 {
        advance(Duration::from_secs(300)).await;
        for _ in 0..8 {
            tokio::task::yield_now().await;
        }
        advance(Duration::from_millis(1)).await;
    }
    assert_eq!(messages.len(), 64);
    assert_eq!(watcher.diagnostics().full_reconciliation_deliveries, 64);
    let producing = Arc::new(AtomicBool::new(true));
    let producer = tokio::spawn({
        let producing = Arc::clone(&producing);
        let event = Event::new(EventKind::Any).add_path(root.path().join("changed.rs"));
        async move {
            while producing.load(Ordering::Relaxed) {
                for _ in 0..256 {
                    callback(Ok(event.clone()));
                }
                tokio::task::yield_now().await;
            }
        }
    });
    for _ in 0..8 {
        tokio::task::yield_now().await;
    }
    while messages.try_recv().is_ok() {}
    advance(Duration::from_millis(100)).await;
    for _ in 0..8 {
        tokio::task::yield_now().await;
    }
    let delivered = messages.try_recv();
    producing.store(false, Ordering::Relaxed);
    producer.await.unwrap();
    watcher.shutdown().await.unwrap();
    assert_eq!(
        delivered,
        Ok(WatcherMessage::ReconcileRequired),
        "expired full delivery retry must progress even while raw events remain ready"
    );
}

#[tokio::test]
async fn lifecycle_shutdown_joins() {
    let root = tempfile::tempdir().unwrap();
    let (watcher, mut rx) = RepositoryWatcher::start(
        root.path(),
        64,
        Duration::from_millis(50),
        CancellationToken::new(),
    )
    .await
    .unwrap();
    watcher.shutdown().await.unwrap();
    assert!(rx.recv().await.is_none());
}

#[tokio::test(start_paused = true)]
async fn lifecycle_shutdown_has_a_bounded_deadline() {
    let handle = tokio::spawn(std::future::pending::<()>());
    let join = tokio::spawn(join_watcher(handle));
    tokio::time::advance(Duration::from_secs(5)).await;
    assert!(matches!(
        join.await.unwrap(),
        Err(Error::ShutdownTimeout {
            component: "repository watcher"
        })
    ));
}

#[tokio::test]
async fn coalesces_and_normalizes_paths() {
    let root = tempfile::tempdir().unwrap();
    let (watcher, mut rx) = RepositoryWatcher::start(
        root.path(),
        64,
        Duration::from_millis(100),
        CancellationToken::new(),
    )
    .await
    .unwrap();

    tokio::fs::write(root.path().join("a.txt"), "a")
        .await
        .unwrap();
    tokio::fs::write(root.path().join("a.txt"), "updated")
        .await
        .unwrap();

    let paths = timeout(Duration::from_secs(5), async {
        loop {
            match rx.recv().await.unwrap() {
                WatcherMessage::Changed { paths } if paths.iter().any(|path| path == "a.txt") => {
                    return Some(paths);
                }
                WatcherMessage::Changed { .. } => {}
                WatcherMessage::ReconcileRequired => {
                    #[cfg(target_os = "macos")]
                    return None;
                    #[cfg(not(target_os = "macos"))]
                    panic!("ordinary file updates should not require reconciliation");
                }
            }
        }
    })
    .await
    .unwrap();
    match paths {
        Some(paths) => {
            assert_eq!(paths.iter().filter(|path| *path == "a.txt").count(), 1);
        }
        None => {
            #[cfg(not(target_os = "macos"))]
            unreachable!("only the FSEvents backend may require reconciliation here");
        }
    }

    assert_eq!(
        relative_path(
            root.path(),
            &root.path().join("nested/b.txt"),
            &DiscoveryPolicy::default(),
            DirectoryHint::No,
        )
        .expect("UTF-8 relative path")
        .as_deref(),
        Some("nested/b.txt")
    );

    watcher.shutdown().await.unwrap();
}

#[tokio::test]
async fn filters_git_directory() {
    let root = tempfile::tempdir().unwrap();
    let (watcher, mut rx) = RepositoryWatcher::start(
        root.path(),
        64,
        Duration::from_millis(50),
        CancellationToken::new(),
    )
    .await
    .unwrap();

    tokio::fs::create_dir(root.path().join(".git"))
        .await
        .unwrap();
    tokio::fs::write(root.path().join(".git/config"), "x")
        .await
        .unwrap();
    tokio::fs::write(root.path().join("visible.txt"), "visible")
        .await
        .unwrap();

    timeout(Duration::from_secs(5), async {
        loop {
            match rx.recv().await.unwrap() {
                WatcherMessage::Changed { paths } => {
                    assert!(
                        paths
                            .iter()
                            .all(|path| path != ".git" && !path.starts_with(".git/")),
                        "watcher exposed ignored .git paths: {paths:?}"
                    );
                    if paths.iter().any(|path| path == "visible.txt") {
                        break;
                    }
                }
                WatcherMessage::ReconcileRequired => {
                    #[cfg(target_os = "macos")]
                    break;
                    #[cfg(not(target_os = "macos"))]
                    panic!("ordinary file creation should not require reconciliation");
                }
            }
        }
    })
    .await
    .unwrap();

    watcher.shutdown().await.unwrap();
}

#[tokio::test]
async fn ignores_access_only_events() {
    let root = tempfile::tempdir().unwrap();
    let (watcher, mut rx) = RepositoryWatcher::start(
        root.path(),
        64,
        Duration::from_millis(50),
        CancellationToken::new(),
    )
    .await
    .unwrap();

    let file = root.path().join("a.txt");
    tokio::fs::write(&file, "a").await.unwrap();
    let _ = timeout(Duration::from_secs(5), rx.recv())
        .await
        .unwrap()
        .unwrap();

    let _ = tokio::fs::read_to_string(&file).await.unwrap();
    assert!(
        timeout(Duration::from_millis(200), rx.recv())
            .await
            .is_err(),
        "an access-only event reached the watcher channel"
    );

    watcher.shutdown().await.unwrap();
}

#[tokio::test]
async fn rename_inside_root_is_reported_or_reconciled() {
    let root = tempfile::tempdir().unwrap();
    let (watcher, mut rx) = RepositoryWatcher::start(
        root.path(),
        64,
        Duration::from_millis(100),
        CancellationToken::new(),
    )
    .await
    .unwrap();

    let a = root.path().join("a.txt");
    let b = root.path().join("b.txt");
    tokio::fs::write(&a, "a").await.unwrap();
    let _ = timeout(Duration::from_secs(5), rx.recv())
        .await
        .unwrap()
        .unwrap();

    tokio::fs::rename(&a, &b).await.unwrap();
    let msg = timeout(Duration::from_secs(5), rx.recv())
        .await
        .unwrap()
        .unwrap();
    match msg {
        WatcherMessage::Changed { paths } => {
            assert!(paths.contains(&"a.txt".to_string()));
            assert!(paths.contains(&"b.txt".to_string()));
        }
        // FSEvents cannot associate the old and new sides of a rename.
        // The watcher must conservatively request a full reconciliation
        // when the backend cannot provide both paths.
        WatcherMessage::ReconcileRequired => {}
    }

    watcher.shutdown().await.unwrap();
}
