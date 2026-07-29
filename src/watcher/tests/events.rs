    #[test]
    fn paired_rename_event_coalesces_both_paths() {
        let root = tempfile::tempdir().unwrap();
        let mut pending = BTreeSet::new();
        let mut rename_from = HashMap::new();
        let mut rename_to = HashMap::new();
        let mut reconcile = false;
        let event = Event::new(EventKind::Modify(ModifyKind::Name(RenameMode::Both)))
            .add_path(root.path().join("a.txt"))
            .add_path(root.path().join("b.txt"));

        process_raw_event(
            Ok(event),
            root.path(),
            &DiscoveryPolicy::default(),
            &mut pending,
            &mut rename_from,
            &mut rename_to,
            &mut reconcile,
        );

        assert!(!reconcile);
        assert_eq!(
            pending,
            BTreeSet::from(["a.txt".to_string(), "b.txt".to_string()])
        );
    }

    #[test]
    fn generated_events_are_filtered_before_the_raw_queue() {
        let root = tempfile::tempdir().unwrap();
        let generated = root.path().join("node_modules/pkg/index.js");
        std::fs::create_dir_all(generated.parent().unwrap()).unwrap();
        std::fs::write(&generated, "generated").unwrap();
        let generated_event = Event::new(EventKind::Any).add_path(generated);

        assert!(!raw_event_is_relevant(
            &Ok(generated_event.clone()),
            root.path(),
            &DiscoveryPolicy::default(),
        ));
        assert!(raw_event_is_relevant(
            &Ok(generated_event),
            root.path(),
            &DiscoveryPolicy::new(true),
        ));

        let visible = root.path().join(".github/workflows/ci.yml");
        std::fs::create_dir_all(visible.parent().unwrap()).unwrap();
        std::fs::write(&visible, "name: ci\n").unwrap();
        assert!(raw_event_is_relevant(
            &Ok(Event::new(EventKind::Any).add_path(visible)),
            root.path(),
            &DiscoveryPolicy::default(),
        ));

        let git_config = root.path().join(".git/config");
        std::fs::create_dir_all(git_config.parent().unwrap()).unwrap();
        std::fs::write(&git_config, "[core]\n").unwrap();
        assert!(!raw_event_is_relevant(
            &Ok(Event::new(EventKind::Any).add_path(git_config)),
            root.path(),
            &DiscoveryPolicy::default(),
        ));

        let rescan = Event::new(EventKind::Other)
            .add_path(root.path().join("node_modules"))
            .set_flag(notify::event::Flag::Rescan);
        assert!(raw_event_is_relevant(
            &Ok(rescan),
            root.path(),
            &DiscoveryPolicy::default(),
        ));
    }

    #[test]
    fn removed_generated_directory_is_filtered_after_it_disappears() {
        let root = tempfile::tempdir().unwrap();
        let generated = root.path().join("node_modules");
        let event = Event::new(EventKind::Remove(RemoveKind::Folder)).add_path(generated);

        assert!(!raw_event_is_relevant(
            &Ok(event),
            root.path(),
            &DiscoveryPolicy::default(),
        ));
    }

    #[test]
    fn scoped_watcher_keeps_relevant_paths_and_ancestor_ignore_controls() {
        let root = tempfile::tempdir().unwrap();
        let policy = DiscoveryPolicy::default().with_index_scope(
            crate::IndexScope::new(
                vec!["src/**".into()],
                vec!["src/generated/**".into()],
            )
            .expect("scope"),
        );
        let event = |path: &str| {
            Ok(Event::new(EventKind::Any).add_path(root.path().join(path)))
        };

        assert!(raw_event_is_relevant(
            &event("src/lib.rs"),
            root.path(),
            &policy,
        ));
        assert!(!raw_event_is_relevant(
            &event("third_party/lib.rs"),
            root.path(),
            &policy,
        ));
        assert!(!raw_event_is_relevant(
            &event("src/generated/schema.rs"),
            root.path(),
            &policy,
        ));
        assert!(raw_event_is_relevant(
            &event(".gitignore"),
            root.path(),
            &policy,
        ));
        assert!(raw_event_is_relevant(
            &event("src/.leantokenignore"),
            root.path(),
            &policy,
        ));
        assert!(!raw_event_is_relevant(
            &event("third_party/.gitignore"),
            root.path(),
            &policy,
        ));

        let mut pending = BTreeSet::new();
        let mut rename_from = HashMap::new();
        let mut rename_to = HashMap::new();
        let mut reconcile = false;
        process_raw_event(
            Ok(
                Event::new(EventKind::Modify(ModifyKind::Name(RenameMode::Both)))
                    .add_path(root.path().join("src/lib.rs"))
                    .add_path(root.path().join("third_party/lib.rs")),
            ),
            root.path(),
            &policy,
            &mut pending,
            &mut rename_from,
            &mut rename_to,
            &mut reconcile,
        );
        assert!(reconcile, "cross-scope rename requires a full scoped reconciliation");
        assert!(pending.is_empty());
    }

    #[test]
    fn watch_count_includes_ignored_and_generated_directories() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join(".gitignore"), "ignored/\n").unwrap();
        std::fs::create_dir_all(root.path().join("ignored/nested")).unwrap();
        std::fs::create_dir_all(root.path().join("node_modules/pkg")).unwrap();

        let cancellation = CancellationToken::new();
        let complete = inspect_watch_admission(root.path(), 100, 100, &cancellation);
        assert_eq!(complete.entries, 6);
        assert_eq!(complete.directories, 5);
        assert!(complete.complete);
        assert_eq!(complete.fallback_reason, None);

        let directory_limited = inspect_watch_admission(root.path(), 2, 100, &cancellation);
        assert_eq!(directory_limited.directories, 3);
        assert!(!directory_limited.complete);
        assert_eq!(
            directory_limited.fallback_reason,
            Some(WatcherFallbackReason::AdmissionDirectoryLimit)
        );
    }

    #[test]
    fn watch_admission_is_bounded_by_entries_and_cancellation() {
        let root = tempfile::tempdir().unwrap();
        for index in 0..10 {
            std::fs::write(root.path().join(format!("{index}.txt")), "").unwrap();
        }
        let cancellation = CancellationToken::new();
        let entry_limited = inspect_watch_admission(root.path(), 100, 4, &cancellation);
        assert_eq!(entry_limited.entries, 4);
        assert!(!entry_limited.complete);
        assert_eq!(
            entry_limited.fallback_reason,
            Some(WatcherFallbackReason::AdmissionEntryLimit)
        );

        cancellation.cancel();
        let cancelled = inspect_watch_admission(root.path(), 100, 100, &cancellation);
        assert_eq!(cancelled.entries, 0);
        assert!(!cancelled.complete);
        assert_eq!(
            cancelled.fallback_reason,
            Some(WatcherFallbackReason::AdmissionCancelled)
        );
    }

    #[test]
    fn full_output_queue_degrades_changes_to_reconciliation() {
        let (tx, mut rx) = mpsc::channel(1);
        tx.try_send(WatcherMessage::Changed {
            paths: vec!["occupied.txt".into()],
        })
        .unwrap();
        let mut pending = BTreeSet::from(["changed.txt".to_string()]);
        let mut rename_from = HashMap::new();
        let mut rename_to = HashMap::new();
        let mut reconcile = false;
        let counters = WatcherCounters::default();

        assert!(flush(
            &mut pending,
            &mut rename_from,
            &mut rename_to,
            &mut reconcile,
            &tx,
            &counters,
        ));
        assert!(pending.is_empty());
        assert!(reconcile);

        assert!(matches!(
            rx.try_recv(),
            Ok(WatcherMessage::Changed { paths }) if paths == ["occupied.txt"]
        ));
        assert!(flush(
            &mut pending,
            &mut rename_from,
            &mut rename_to,
            &mut reconcile,
            &tx,
            &counters,
        ));
        assert!(!reconcile);
        assert_eq!(
            counters
                .full_reconciliation_deliveries
                .load(Ordering::Relaxed),
            1
        );
        assert!(matches!(
            rx.try_recv(),
            Ok(WatcherMessage::ReconcileRequired)
        ));
    }

    #[test]
    fn retained_path_state_overflow_becomes_one_sticky_reconciliation() {
        let mut pending =
            BTreeSet::from(["a.rs".to_string(), "b.rs".to_string(), "c.rs".to_string()]);
        let mut rename_from = HashMap::from([(1, "old.rs".to_string())]);
        let mut rename_to = HashMap::new();
        let mut reconcile = false;

        bound_pending_state(
            &mut pending,
            &mut rename_from,
            &mut rename_to,
            &mut reconcile,
            3,
        );

        assert!(reconcile);
        assert!(pending.is_empty());
        assert!(rename_from.is_empty());
        assert!(rename_to.is_empty());

        pending.insert("later.rs".into());
        bound_pending_state(
            &mut pending,
            &mut rename_from,
            &mut rename_to,
            &mut reconcile,
            3,
        );
        assert!(pending.is_empty());
    }
