use super::*;

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

    pub(super) fn with_policy(policy: ReconciliationSchedulePolicy) -> Self {
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

    pub(super) fn merge_paths(&mut self, paths: impl IntoIterator<Item = String>) {
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

    pub(super) fn reset_full_backoff_after_stability(&mut self, now: Instant) {
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
