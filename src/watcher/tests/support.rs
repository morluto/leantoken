    use std::time::Duration;

    use tokio::time::{advance, timeout};
    use tokio_util::sync::CancellationToken;

    use super::*;

    fn creation_failure(
        _callback: EventCallback,
        _config: Config,
    ) -> notify::Result<NativeWatcher> {
        Err(notify::Error::generic("creation unavailable"))
    }

    struct RegistrationFailure;

    impl Watcher for RegistrationFailure {
        fn kind() -> notify::WatcherKind
        where
            Self: Sized,
        {
            notify::WatcherKind::PollWatcher
        }

        fn new<F: notify::EventHandler>(_event_handler: F, _config: Config) -> notify::Result<Self>
        where
            Self: Sized,
        {
            Ok(Self)
        }

        fn watch(&mut self, _path: &Path, _recursive_mode: RecursiveMode) -> notify::Result<()> {
            Err(notify::Error::generic("registration unavailable"))
        }

        fn unwatch(&mut self, _path: &Path) -> notify::Result<()> {
            Ok(())
        }
    }

    fn registration_failure(
        _callback: EventCallback,
        _config: Config,
    ) -> notify::Result<NativeWatcher> {
        Ok(Box::new(RegistrationFailure))
    }

    async fn assert_backend_failure_uses_polling(factory: WatcherFactory) {
        let directory = tempfile::tempdir().expect("directory");
        let cancellation = CancellationToken::new();
        let (watcher, mut messages) = RepositoryWatcher::start_with_factory(
            directory.path(),
            4,
            Duration::from_millis(10),
            DiscoveryPolicy::default(),
            cancellation,
            factory,
            Duration::from_secs(30),
        )
        .await
        .expect("polling watcher");

        assert_eq!(
            messages.recv().await,
            Some(WatcherMessage::ReconcileRequired)
        );
        advance(Duration::from_secs(30)).await;
        assert_eq!(
            messages.recv().await,
            Some(WatcherMessage::ReconcileRequired)
        );
        timeout(Duration::from_secs(1), watcher.shutdown())
            .await
            .expect("shutdown timeout")
            .expect("shutdown");
    }

    #[tokio::test(start_paused = true)]
    async fn watcher_creation_failure_falls_back_to_polling() {
        assert_backend_failure_uses_polling(creation_failure).await;
    }

    #[tokio::test(start_paused = true)]
    async fn watcher_registration_failure_falls_back_to_polling() {
        assert_backend_failure_uses_polling(registration_failure).await;
    }

    fn test_schedule_policy() -> ReconciliationSchedulePolicy {
        ReconciliationSchedulePolicy {
            quiet_period: Duration::from_millis(100),
            max_pending_paths: 2,
            retry_initial_delay: Duration::from_millis(50),
            retry_max_delay: Duration::from_millis(200),
            full_initial_delay: Duration::from_secs(1),
            full_max_delay: Duration::from_secs(4),
            full_reset_after: Duration::from_secs(10),
        }
    }
