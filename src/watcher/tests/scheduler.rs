    #[tokio::test(start_paused = true)]
    async fn initial_burst_collapses_to_one_quiet_full_reconciliation() {
        let mut scheduler = WatcherReconciliationScheduler::with_policy(test_schedule_policy());
        scheduler.enqueue(
            WatcherMessage::Changed {
                paths: vec!["a.rs".into(), "b.rs".into()],
            },
            Instant::now(),
        );
        scheduler.enqueue(
            WatcherMessage::Changed {
                paths: vec!["c.rs".into()],
            },
            Instant::now(),
        );
        for _ in 0..10 {
            scheduler.enqueue(WatcherMessage::ReconcileRequired, Instant::now());
        }

        advance(Duration::from_millis(99)).await;
        assert!(scheduler.take_ready(Instant::now()).is_none());
        advance(Duration::from_millis(1)).await;
        assert_eq!(
            scheduler.take_ready(Instant::now()),
            Some(WatcherAction::Full)
        );
        assert!(scheduler.take_ready(Instant::now()).is_none());
    }

    #[tokio::test(start_paused = true)]
    async fn new_activity_extends_quiet_period_and_coalesces_paths() {
        let mut scheduler = WatcherReconciliationScheduler::with_policy(test_schedule_policy());
        scheduler.enqueue(
            WatcherMessage::Changed {
                paths: vec!["b.rs".into()],
            },
            Instant::now(),
        );
        advance(Duration::from_millis(75)).await;
        scheduler.enqueue(
            WatcherMessage::Changed {
                paths: vec!["a.rs".into(), "b.rs".into()],
            },
            Instant::now(),
        );

        advance(Duration::from_millis(99)).await;
        assert!(scheduler.take_ready(Instant::now()).is_none());
        advance(Duration::from_millis(1)).await;
        assert_eq!(
            scheduler.take_ready(Instant::now()),
            Some(WatcherAction::Paths(vec!["a.rs".into(), "b.rs".into()]))
        );
    }

    #[tokio::test(start_paused = true)]
    async fn consecutive_full_reconciliations_observe_capped_cooldown() {
        let mut scheduler = WatcherReconciliationScheduler::with_policy(test_schedule_policy());
        scheduler.enqueue(WatcherMessage::ReconcileRequired, Instant::now());
        advance(Duration::from_millis(100)).await;
        let first = scheduler.take_ready(Instant::now()).expect("first full");
        scheduler.finish_success(&first, Instant::now());

        for expected_delay in [1_000, 2_000, 4_000, 4_000] {
            scheduler.enqueue(WatcherMessage::ReconcileRequired, Instant::now());
            advance(Duration::from_millis(expected_delay - 1)).await;
            assert!(scheduler.take_ready(Instant::now()).is_none());
            advance(Duration::from_millis(1)).await;
            let action = scheduler.take_ready(Instant::now()).expect("next full");
            assert_eq!(action, WatcherAction::Full);
            scheduler.finish_success(&action, Instant::now());
        }
    }

    #[tokio::test(start_paused = true)]
    async fn stable_period_resets_full_reconciliation_cooldown() {
        let mut scheduler = WatcherReconciliationScheduler::with_policy(test_schedule_policy());
        scheduler.enqueue(WatcherMessage::ReconcileRequired, Instant::now());
        advance(Duration::from_millis(100)).await;
        let first = scheduler.take_ready(Instant::now()).expect("first full");
        scheduler.finish_success(&first, Instant::now());

        advance(Duration::from_secs(10)).await;
        scheduler.enqueue(WatcherMessage::ReconcileRequired, Instant::now());
        advance(Duration::from_millis(99)).await;
        assert!(scheduler.take_ready(Instant::now()).is_none());
        advance(Duration::from_millis(1)).await;
        assert_eq!(
            scheduler.take_ready(Instant::now()),
            Some(WatcherAction::Full)
        );
    }

    #[tokio::test(start_paused = true)]
    async fn transient_failure_retains_work_and_backs_off_before_retry() {
        let mut scheduler = WatcherReconciliationScheduler::with_policy(test_schedule_policy());
        scheduler.enqueue(
            WatcherMessage::Changed {
                paths: vec!["a.rs".into()],
            },
            Instant::now(),
        );
        advance(Duration::from_millis(100)).await;
        let action = scheduler
            .take_ready(Instant::now())
            .expect("initial action");
        scheduler.finish_failure(action, Instant::now());
        scheduler.enqueue(
            WatcherMessage::Changed {
                paths: vec!["b.rs".into()],
            },
            Instant::now(),
        );

        advance(Duration::from_millis(99)).await;
        assert!(scheduler.take_ready(Instant::now()).is_none());
        advance(Duration::from_millis(1)).await;
        let retry = scheduler
            .take_ready(Instant::now())
            .expect("retained retry");
        assert_eq!(
            retry,
            WatcherAction::Paths(vec!["a.rs".into(), "b.rs".into()])
        );
        scheduler.finish_failure(retry, Instant::now());

        advance(Duration::from_millis(99)).await;
        assert!(scheduler.take_ready(Instant::now()).is_none());
        advance(Duration::from_millis(1)).await;
        assert!(scheduler.take_ready(Instant::now()).is_some());
    }

    #[tokio::test(start_paused = true)]
    async fn failed_action_does_not_replace_a_later_full_request() {
        let mut scheduler = WatcherReconciliationScheduler::with_policy(test_schedule_policy());
        scheduler.enqueue(
            WatcherMessage::Changed {
                paths: vec!["a.rs".into()],
            },
            Instant::now(),
        );
        advance(Duration::from_millis(100)).await;
        let action = scheduler.take_ready(Instant::now()).expect("path action");

        scheduler.enqueue(WatcherMessage::ReconcileRequired, Instant::now());
        scheduler.finish_failure(action, Instant::now());
        advance(Duration::from_secs(1)).await;

        assert_eq!(
            scheduler.take_ready(Instant::now()),
            Some(WatcherAction::Full)
        );
    }
