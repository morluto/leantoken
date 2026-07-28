const MAX_SCHEDULED_PATHS: usize = 4_096;
const MAX_WATCHED_DIRECTORIES: usize = 50_000;
const MAX_WATCH_ADMISSION_ENTRIES: usize = 100_000;
const RECONCILE_RETRY_INITIAL_DELAY: Duration = Duration::from_millis(500);
const RECONCILE_RETRY_MAX_DELAY: Duration = Duration::from_secs(30);
const FULL_RECONCILE_INITIAL_DELAY: Duration = Duration::from_secs(1);
const FULL_RECONCILE_MAX_DELAY: Duration = Duration::from_secs(30);
const FULL_RECONCILE_RESET_AFTER: Duration = Duration::from_secs(60);
const WATCHER_POLL_INTERVAL: Duration = Duration::from_secs(30);

type EventCallback = Box<dyn FnMut(notify::Result<Event>) + Send>;
type NativeWatcher = Box<dyn Watcher + Send>;
type WatcherFactory = fn(EventCallback, Config) -> notify::Result<NativeWatcher>;

fn recommended_watcher(callback: EventCallback, config: Config) -> notify::Result<NativeWatcher> {
    RecommendedWatcher::new(callback, config).map(|watcher| Box::new(watcher) as NativeWatcher)
}

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
