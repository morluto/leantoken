use super::*;

pub(super) const MAX_SCHEDULED_PATHS: usize = 4_096;
pub(super) const MAX_WATCHED_DIRECTORIES: usize = 50_000;
pub(super) const MAX_WATCH_ADMISSION_ENTRIES: usize = 100_000;
pub(super) const RECONCILE_RETRY_INITIAL_DELAY: Duration = Duration::from_millis(500);
pub(super) const RECONCILE_RETRY_MAX_DELAY: Duration = Duration::from_secs(30);
pub(super) const FULL_RECONCILE_INITIAL_DELAY: Duration = Duration::from_secs(1);
pub(super) const FULL_RECONCILE_MAX_DELAY: Duration = Duration::from_secs(30);
pub(super) const FULL_RECONCILE_RESET_AFTER: Duration = Duration::from_secs(60);
pub(super) const WATCHER_POLL_INTERVAL: Duration = Duration::from_secs(30);

pub(super) type EventCallback = Box<dyn FnMut(notify::Result<Event>) + Send>;
pub(super) type NativeWatcher = Box<dyn Watcher + Send>;
pub(super) type WatcherFactory = fn(EventCallback, Config) -> notify::Result<NativeWatcher>;

pub(super) fn recommended_watcher(
    callback: EventCallback,
    config: Config,
) -> notify::Result<NativeWatcher> {
    RecommendedWatcher::new(callback, config).map(|watcher| Box::new(watcher) as NativeWatcher)
}

/// Filesystem change backend selected for one repository leader.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WatcherBackend {
    /// Native recursive filesystem notifications are active.
    Native,
    /// A bounded full reconciliation is scheduled at each polling interval.
    PeriodicPolling,
}

/// Why native recursive filesystem notifications were not enabled.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WatcherFallbackReason {
    /// The bounded admission walk reached its entry limit.
    AdmissionEntryLimit,
    /// The bounded admission walk exceeded its directory limit.
    AdmissionDirectoryLimit,
    /// The admission walk was cancelled before it completed.
    AdmissionCancelled,
    /// The admission walk could not inspect the complete tree.
    AdmissionError,
    /// The platform watcher could not be created.
    BackendCreationFailed,
    /// Recursive registration of the repository root failed.
    BackendRegistrationFailed,
}

/// Bounded, point-in-time watcher lifecycle counters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct WatcherDiagnostics {
    /// Selected filesystem change backend.
    pub backend: WatcherBackend,
    /// Why periodic polling was selected, when applicable.
    pub fallback_reason: Option<WatcherFallbackReason>,
    /// Filesystem entries examined by the bounded watcher-admission walk.
    pub admission_entries: usize,
    /// Directories observed by the bounded watcher-admission walk.
    pub admission_directories: usize,
    /// Whether the admission walk reached the end of the repository tree.
    pub admission_complete: bool,
    /// Polling timer ticks observed after backend initialization.
    pub poll_ticks: u64,
    /// Successfully delivered changed-path messages.
    pub changed_path_deliveries: u64,
    /// Successfully delivered full-reconciliation messages.
    pub full_reconciliation_deliveries: u64,
}

#[derive(Debug, Clone, Copy)]
pub(super) struct WatchAdmission {
    pub(super) entries: usize,
    pub(super) directories: usize,
    pub(super) complete: bool,
    pub(super) fallback_reason: Option<WatcherFallbackReason>,
}

#[derive(Debug)]
pub(super) struct WatcherReady {
    pub(super) backend: WatcherBackend,
    pub(super) fallback_reason: Option<WatcherFallbackReason>,
    pub(super) admission: WatchAdmission,
}

#[derive(Debug, Default)]
pub(super) struct WatcherCounters {
    pub(super) poll_ticks: AtomicU64,
    pub(super) changed_path_deliveries: AtomicU64,
    pub(super) full_reconciliation_deliveries: AtomicU64,
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
pub(super) enum PendingReconciliation {
    Paths(BTreeSet<String>),
    Full,
}

#[derive(Debug, Clone, Copy)]
pub(super) struct ReconciliationSchedulePolicy {
    pub(super) quiet_period: Duration,
    pub(super) max_pending_paths: usize,
    pub(super) retry_initial_delay: Duration,
    pub(super) retry_max_delay: Duration,
    pub(super) full_initial_delay: Duration,
    pub(super) full_max_delay: Duration,
    pub(super) full_reset_after: Duration,
}

impl ReconciliationSchedulePolicy {
    pub(super) fn runtime(quiet_period: Duration) -> Self {
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
