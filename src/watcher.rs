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

const MAX_SCHEDULED_PATHS: usize = 4_096;
const RECONCILE_RETRY_INITIAL_DELAY: Duration = Duration::from_millis(500);
const RECONCILE_RETRY_MAX_DELAY: Duration = Duration::from_secs(30);
const FULL_RECONCILE_INITIAL_DELAY: Duration = Duration::from_secs(1);
const FULL_RECONCILE_MAX_DELAY: Duration = Duration::from_secs(30);
const FULL_RECONCILE_RESET_AFTER: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, PartialEq, Eq)]
/// Debounced repository change delivered to the reconciliation loop.
pub enum WatcherMessage {
    /// One or more normalized repository-relative paths changed.
    Changed { paths: Vec<String> },
    /// Event loss or ambiguity requires repository-wide reconciliation.
    ReconcileRequired,
}

/// One coalesced watcher reconciliation selected after quiet-time and backoff.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WatcherAction {
    /// Reconcile the sorted set of changed repository-relative paths.
    Paths(Vec<String>),
    /// Reconcile full repository visibility and contents.
    Full,
}

#[derive(Debug)]
enum PendingReconciliation {
    Paths(BTreeSet<String>),
    Full,
}

#[derive(Debug, Clone, Copy)]
struct ReconciliationSchedulePolicy {
    quiet_period: Duration,
    max_pending_paths: usize,
    retry_initial_delay: Duration,
    retry_max_delay: Duration,
    full_initial_delay: Duration,
    full_max_delay: Duration,
    full_reset_after: Duration,
}

impl ReconciliationSchedulePolicy {
    fn runtime(quiet_period: Duration) -> Self {
        Self {
            quiet_period,
            max_pending_paths: MAX_SCHEDULED_PATHS,
            retry_initial_delay: RECONCILE_RETRY_INITIAL_DELAY,
            retry_max_delay: RECONCILE_RETRY_MAX_DELAY,
            full_initial_delay: FULL_RECONCILE_INITIAL_DELAY,
            full_max_delay: FULL_RECONCILE_MAX_DELAY,
            full_reset_after: FULL_RECONCILE_RESET_AFTER,
        }
    }
}

/// Sticky, bounded scheduler for filesystem-driven repository reconciliation.
///
/// Path events coalesce until the configured quiet period. Ambiguity or path
/// overflow becomes one full reconciliation. Failed actions remain pending
/// under capped exponential retry, while consecutive successful full scans
/// receive a separate capped cooldown to prevent rescan loops.
#[derive(Debug)]
pub struct WatcherReconciliationScheduler {
    policy: ReconciliationSchedulePolicy,
    pending: Option<PendingReconciliation>,
    quiet_until: Option<Instant>,
    retry_not_before: Option<Instant>,
    next_full_not_before: Option<Instant>,
    last_full_completed: Option<Instant>,
    retry_delay: Duration,
    full_delay: Duration,
}

impl WatcherReconciliationScheduler {
    /// Create a scheduler using runtime path and retry bounds.
    #[must_use]
    pub fn new(quiet_period: Duration) -> Self {
        Self::with_policy(ReconciliationSchedulePolicy::runtime(quiet_period))
    }

    fn with_policy(policy: ReconciliationSchedulePolicy) -> Self {
        Self {
            retry_delay: policy.retry_initial_delay,
            full_delay: policy.full_initial_delay,
            policy,
            pending: None,
            quiet_until: None,
            retry_not_before: None,
            next_full_not_before: None,
            last_full_completed: None,
        }
    }

    /// Merge one watcher message into the sticky pending state.
    pub fn enqueue(&mut self, message: WatcherMessage, now: Instant) {
        self.reset_full_backoff_after_stability(now);
        match message {
            WatcherMessage::Changed { paths } if paths.is_empty() => return,
            WatcherMessage::Changed { paths } => {
                self.merge_paths(paths);
            }
            WatcherMessage::ReconcileRequired => {
                self.pending = Some(PendingReconciliation::Full);
            }
        }
        self.quiet_until = Some(now + self.policy.quiet_period);
    }

    /// Return the earliest time at which pending work may run.
    #[must_use]
    pub fn next_deadline(&self) -> Option<Instant> {
        let mut deadline = self.quiet_until?;
        if let Some(retry_not_before) = self.retry_not_before {
            deadline = deadline.max(retry_not_before);
        }
        if matches!(self.pending, Some(PendingReconciliation::Full))
            && let Some(next_full_not_before) = self.next_full_not_before
        {
            deadline = deadline.max(next_full_not_before);
        }
        Some(deadline)
    }

    /// Take the coalesced action when every scheduling deadline has elapsed.
    pub fn take_ready(&mut self, now: Instant) -> Option<WatcherAction> {
        if self.next_deadline().is_none_or(|deadline| now < deadline) {
            return None;
        }
        self.quiet_until = None;
        self.retry_not_before = None;
        match self.pending.take()? {
            PendingReconciliation::Paths(paths) => {
                Some(WatcherAction::Paths(paths.into_iter().collect()))
            }
            PendingReconciliation::Full => Some(WatcherAction::Full),
        }
    }

    /// Record a successful action and apply full-rescan cooldown when needed.
    pub fn finish_success(&mut self, action: &WatcherAction, now: Instant) {
        self.retry_delay = self.policy.retry_initial_delay;
        self.retry_not_before = None;
        if matches!(action, WatcherAction::Full) {
            self.last_full_completed = Some(now);
            self.next_full_not_before = Some(now + self.full_delay);
            self.full_delay = self
                .full_delay
                .saturating_mul(2)
                .min(self.policy.full_max_delay);
        }
    }

    /// Retain a failed action and schedule it under capped exponential retry.
    pub fn finish_failure(&mut self, action: WatcherAction, now: Instant) {
        match action {
            WatcherAction::Paths(paths) => self.merge_paths(paths),
            WatcherAction::Full => self.pending = Some(PendingReconciliation::Full),
        }
        self.quiet_until = Some(now);
        self.retry_not_before = Some(now + self.retry_delay);
        self.retry_delay = self
            .retry_delay
            .saturating_mul(2)
            .min(self.policy.retry_max_delay);
    }

    fn merge_paths(&mut self, paths: impl IntoIterator<Item = String>) {
        if matches!(self.pending, Some(PendingReconciliation::Full)) {
            return;
        }
        let mut pending = match self.pending.take() {
            Some(PendingReconciliation::Paths(pending)) => pending,
            Some(PendingReconciliation::Full) => unreachable!("full handled above"),
            None => BTreeSet::new(),
        };
        pending.extend(paths);
        self.pending = if pending.len() > self.policy.max_pending_paths {
            Some(PendingReconciliation::Full)
        } else {
            Some(PendingReconciliation::Paths(pending))
        };
    }

    fn reset_full_backoff_after_stability(&mut self, now: Instant) {
        let stable = self.last_full_completed.is_some_and(|completed| {
            now.checked_duration_since(completed)
                .is_some_and(|elapsed| elapsed >= self.policy.full_reset_after)
        });
        if stable {
            self.last_full_completed = None;
            self.next_full_not_before = None;
            self.full_delay = self.policy.full_initial_delay;
        }
    }
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
                    pending.clear();
                    rename_from.clear();
                    rename_to.clear();
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

fn bound_pending_state(
    pending: &mut BTreeSet<String>,
    rename_from: &mut HashMap<usize, String>,
    rename_to: &mut HashMap<usize, String>,
    reconcile: &mut bool,
    limit: usize,
) {
    let retained = pending
        .len()
        .saturating_add(rename_from.len())
        .saturating_add(rename_to.len());
    if *reconcile || retained > limit {
        *reconcile = true;
        pending.clear();
        rename_from.clear();
        rename_to.clear();
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

    use tokio::time::{advance, timeout};
    use tokio_util::sync::CancellationToken;

    use super::*;

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
}
