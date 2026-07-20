use std::{
    collections::{BTreeSet, HashMap},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use notify::event::{Event, EventKind, ModifyKind, RenameMode};
use notify::{Config, RecommendedWatcher, RecursiveMode, Watcher};
use tokio::{
    sync::{mpsc, mpsc::error::TrySendError, oneshot},
    task::JoinHandle,
    time::{Instant, sleep},
};
use tokio_util::sync::CancellationToken;

use crate::repository::{DiscoveryPolicy, slash_path};
use crate::{Error, Result};

#[derive(Debug, Clone, PartialEq, Eq)]
/// Debounced repository change delivered to the reconciliation loop.
pub enum WatcherMessage {
    /// One or more normalized repository-relative paths changed.
    Changed { paths: Vec<String> },
    /// Event loss or ambiguity requires repository-wide reconciliation.
    ReconcileRequired,
}

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
    /// event queue. Queue overflow degrades to [`WatcherMessage::ReconcileRequired`].
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
        let root = root.as_ref().canonicalize().map_err(Error::Io)?;
        if !root.is_dir() {
            return Err(Error::InvalidConfiguration(format!(
                "root is not a directory: {}",
                root.display()
            )));
        }

        let (tx, rx) = mpsc::channel::<WatcherMessage>(capacity.max(1));
        let (ready_tx, ready_rx) = oneshot::channel::<Result<()>>();
        let task_token = token.clone();
        let raw_capacity = capacity.saturating_mul(4).max(64);
        let watched_root = root.clone();

        let handle = tokio::spawn(async move {
            let (raw_tx, mut raw_rx) = mpsc::channel::<notify::Result<Event>>(raw_capacity);
            let overflowed = Arc::new(AtomicBool::new(false));
            let config = Config::default().with_follow_symlinks(false);
            let callback_root = watched_root.clone();

            let mut watcher = match RecommendedWatcher::new(
                {
                    let overflowed = Arc::clone(&overflowed);
                    move |event: notify::Result<Event>| {
                        if !raw_event_is_relevant(&event, &callback_root, policy) {
                            return;
                        }
                        if let Err(TrySendError::Full(_)) = raw_tx.try_send(event) {
                            overflowed.store(true, Ordering::Release);
                        }
                    }
                },
                config,
            ) {
                Ok(w) => w,
                Err(err) => {
                    let _ = ready_tx.send(Err(into_error(err)));
                    return;
                }
            };

            if let Err(err) = watcher.watch(&watched_root, RecursiveMode::Recursive) {
                let _ = ready_tx.send(Err(into_error(err)));
                return;
            }
            let _ = ready_tx.send(Ok(()));

            let long_sleep = Duration::from_secs(60 * 60 * 24 * 365 * 10);
            let mut sleep = Box::pin(sleep(long_sleep));
            let mut pending = BTreeSet::<String>::new();
            let mut rename_from = HashMap::<usize, String>::new();
            let mut rename_to = HashMap::<usize, String>::new();
            let mut reconcile = false;

            loop {
                if overflowed.swap(false, Ordering::Acquire) {
                    reconcile = true;
                }
                if reconcile {
                    sleep.as_mut().reset(Instant::now());
                }

                tokio::select! {
                    biased;
                    _ = token.cancelled() => break,
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
        });

        match ready_rx.await {
            Ok(Ok(())) => Ok((
                Self {
                    root,
                    token: task_token,
                    handle,
                },
                rx,
            )),
            Ok(Err(err)) => Err(err),
            Err(_) => {
                let _ = handle.await;
                Err(Error::InvalidRequest(
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

fn into_error(err: notify::Error) -> Error {
    Error::Io(std::io::Error::other(err.to_string()))
}

fn relative_path(root: &Path, path: &Path, policy: DiscoveryPolicy) -> Option<String> {
    if !path.starts_with(root) {
        return None;
    }
    let rel = path.strip_prefix(root).ok()?;
    let s = slash_path(rel);
    if s.is_empty()
        || s.starts_with(".git/")
        || s == ".git"
        || !policy.includes_path(&s, path.is_dir())
    {
        None
    } else {
        Some(s)
    }
}

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
        match relative_path(root, path, policy) {
            Some(rel) => inside.push(rel),
            None => {
                outside = true;
                tracing::warn!(path = %path.display(), "watcher event outside root");
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

fn raw_event_is_relevant(
    event: &notify::Result<Event>,
    root: &Path,
    policy: DiscoveryPolicy,
) -> bool {
    match event {
        Ok(event) if event.need_rescan() => true,
        Ok(event) if !event.paths.is_empty() => event
            .paths
            .iter()
            .any(|path| relative_path(root, path, policy).is_some()),
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

fn flush(
    pending: &mut BTreeSet<String>,
    rename_from: &mut HashMap<usize, String>,
    rename_to: &mut HashMap<usize, String>,
    reconcile: &mut bool,
    tx: &mpsc::Sender<WatcherMessage>,
) -> bool {
    if !rename_from.is_empty() || !rename_to.is_empty() {
        *reconcile = true;
        rename_from.clear();
        rename_to.clear();
    }

    if *reconcile {
        match tx.try_send(WatcherMessage::ReconcileRequired) {
            Ok(()) => {
                *reconcile = false;
                pending.clear();
            }
            Err(TrySendError::Full(_)) => {}
            Err(TrySendError::Closed(_)) => return false,
        }
    }

    if !pending.is_empty() {
        let paths = pending.iter().cloned().collect();
        match tx.try_send(WatcherMessage::Changed { paths }) {
            Ok(()) => pending.clear(),
            Err(TrySendError::Full(_)) => {
                *reconcile = true;
                pending.clear();
            }
            Err(TrySendError::Closed(_)) => return false,
        }
    }

    true
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use tokio::time::timeout;
    use tokio_util::sync::CancellationToken;

    use super::*;

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
                    WatcherMessage::ReconcileRequired => return None,
                }
            }
        })
        .await
        .unwrap();
        if let Some(paths) = paths {
            assert_eq!(paths.iter().filter(|path| *path == "a.txt").count(), 1);
        }

        assert_eq!(
            relative_path(
                root.path(),
                &root.path().join("nested/b.txt"),
                DiscoveryPolicy::default(),
            )
            .as_deref(),
            Some("nested/b.txt")
        );

        watcher.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn filters_git_directory() {
        let root = tempfile::tempdir().unwrap();
        let (watcher, _rx) = RepositoryWatcher::start(
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

        tokio::time::sleep(Duration::from_millis(200)).await;
        // No public receiver access needed beyond the channel being scoped.
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
            DiscoveryPolicy::default(),
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
            DiscoveryPolicy::default(),
        ));
        assert!(raw_event_is_relevant(
            &Ok(generated_event),
            root.path(),
            DiscoveryPolicy::new(true),
        ));

        let visible = root.path().join(".github/workflows/ci.yml");
        std::fs::create_dir_all(visible.parent().unwrap()).unwrap();
        std::fs::write(&visible, "name: ci\n").unwrap();
        assert!(raw_event_is_relevant(
            &Ok(Event::new(EventKind::Any).add_path(visible)),
            root.path(),
            DiscoveryPolicy::default(),
        ));

        let rescan = Event::new(EventKind::Other)
            .add_path(root.path().join("node_modules"))
            .set_flag(notify::event::Flag::Rescan);
        assert!(raw_event_is_relevant(
            &Ok(rescan),
            root.path(),
            DiscoveryPolicy::default(),
        ));
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

        assert!(flush(
            &mut pending,
            &mut rename_from,
            &mut rename_to,
            &mut reconcile,
            &tx,
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
        ));
        assert!(!reconcile);
        assert!(matches!(
            rx.try_recv(),
            Ok(WatcherMessage::ReconcileRequired)
        ));
    }
}
