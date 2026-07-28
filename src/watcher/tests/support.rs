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

    async fn assert_backend_failure_uses_polling(
        factory: WatcherFactory,
        expected_reason: WatcherFallbackReason,
    ) {
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
            watcher.diagnostics().backend,
            WatcherBackend::PeriodicPolling
        );
        assert_eq!(
            watcher.diagnostics().fallback_reason,
            Some(expected_reason)
        );
        assert_eq!(watcher.diagnostics().poll_ticks, 0);
        assert!(messages.try_recv().is_err());
        advance(Duration::from_secs(29)).await;
        assert!(messages.try_recv().is_err());
        advance(Duration::from_secs(1)).await;
        assert_eq!(
            messages.recv().await,
            Some(WatcherMessage::ReconcileRequired)
        );
        assert_eq!(watcher.diagnostics().poll_ticks, 1);
        assert_eq!(
            watcher.diagnostics().full_reconciliation_deliveries,
            1
        );
        let final_diagnostics = timeout(
            Duration::from_secs(1),
            watcher.shutdown_with_diagnostics(),
        )
            .await
            .expect("shutdown timeout")
            .expect("shutdown");
        assert_eq!(final_diagnostics.poll_ticks, 1);
        assert_eq!(final_diagnostics.full_reconciliation_deliveries, 1);
    }

    #[tokio::test(start_paused = true)]
    async fn watcher_creation_failure_falls_back_to_polling() {
        assert_backend_failure_uses_polling(
            creation_failure,
            WatcherFallbackReason::BackendCreationFailed,
        )
        .await;
    }

    #[tokio::test(start_paused = true)]
    async fn watcher_registration_failure_falls_back_to_polling() {
        assert_backend_failure_uses_polling(
            registration_failure,
            WatcherFallbackReason::BackendRegistrationFailed,
        )
        .await;
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
