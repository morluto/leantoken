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
                    WatcherMessage::Changed { paths }
                        if paths.iter().any(|path| path == "a.txt") =>
                    {
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
                DiscoveryPolicy::default(),
                false,
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
                            paths.iter().all(|path| path != ".git"
                                && !path.starts_with(".git/")),
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
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(rx.try_recv().is_err());

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
