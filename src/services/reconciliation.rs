use std::fmt;
use std::sync::{Arc, Mutex, MutexGuard, atomic::AtomicUsize};

use tokio::sync::{Semaphore, oneshot};
use tokio_util::sync::CancellationToken;

use crate::coordination::IndexCoordination;
use crate::indexer::Indexer;
use crate::{Error, Result};

use super::indexing::ActiveReconciliation;

pub(super) const DEFAULT_RECONCILIATION_ACTIVE_CAPACITY: usize =
    super::executor::DEFAULT_BLOCKING_ACTIVE_CAPACITY;

#[derive(Debug, Clone, Default)]
pub(super) struct ReconciliationDiagnostics {
    pub requests: u64,
    pub rejected_requests: u64,
    pub waves_created: u64,
    pub waves_started: u64,
    pub waves_completed: u64,
    pub waves_failed: u64,
    pub waves_cancelled_before_start: u64,
    pub coalesced_requests: u64,
    pub cancelled_waiters: u64,
    pub timed_out_waiters: u64,
    pub active_waves: usize,
    pub peak_active_waves: usize,
    pub pending_waiters: usize,
    pub peak_pending_waiters: usize,
}

#[derive(Clone)]
pub(super) struct ReconciliationCoordinator {
    inner: Arc<CoordinatorInner>,
}

impl fmt::Debug for ReconciliationCoordinator {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ReconciliationCoordinator")
            .finish_non_exhaustive()
    }
}

struct CoordinatorInner {
    indexer: Indexer,
    coordination: IndexCoordination,
    active_reconciliations: Arc<AtomicUsize>,
    reconciliation_changed: Arc<tokio::sync::Notify>,
    active_requests: Arc<Semaphore>,
    state: Mutex<CoordinatorState>,
    #[cfg(test)]
    before_scan: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
}

#[derive(Default)]
struct CoordinatorState {
    next_wave_id: u64,
    next_waiter_id: u64,
    current: Option<Wave>,
    pending: Option<Wave>,
    diagnostics: ReconciliationDiagnostics,
}

struct Wave {
    id: u64,
    phase: WavePhase,
    cancellation: CancellationToken,
    waiters: Vec<Waiter>,
    _active: ActiveReconciliation,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WavePhase {
    WaitingForOperation,
    Running,
}

struct Waiter {
    id: u64,
    sender: oneshot::Sender<WaveOutcome>,
}

enum WaveOutcome {
    Complete,
    Failed(Arc<Error>),
}

#[derive(Clone, Copy)]
enum WaiterExit {
    Cancelled,
    TimedOut,
}

struct WaiterGuard {
    coordinator: ReconciliationCoordinator,
    waiter_id: u64,
    armed: bool,
}

impl WaiterGuard {
    fn new(coordinator: ReconciliationCoordinator, waiter_id: u64) -> Self {
        Self {
            coordinator,
            waiter_id,
            armed: true,
        }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }

    fn cancel(&mut self) -> Result<()> {
        self.exit(WaiterExit::Cancelled)
    }

    fn timeout(&mut self) -> Result<()> {
        self.exit(WaiterExit::TimedOut)
    }

    fn exit(&mut self, reason: WaiterExit) -> Result<()> {
        if self.armed {
            self.armed = false;
            self.coordinator.exit_waiter(self.waiter_id, reason)?;
        }
        Ok(())
    }
}

impl Drop for WaiterGuard {
    fn drop(&mut self) {
        let _ = self.cancel();
    }
}

impl ReconciliationCoordinator {
    fn state(&self) -> Result<MutexGuard<'_, CoordinatorState>> {
        self.inner
            .state
            .lock()
            .map_err(|_| reconciliation_state_poisoned())
    }

    pub(super) fn new(
        indexer: Indexer,
        coordination: IndexCoordination,
        active_reconciliations: Arc<AtomicUsize>,
        reconciliation_changed: Arc<tokio::sync::Notify>,
    ) -> Self {
        Self {
            inner: Arc::new(CoordinatorInner {
                indexer,
                coordination,
                active_reconciliations,
                reconciliation_changed,
                active_requests: Arc::new(Semaphore::new(DEFAULT_RECONCILIATION_ACTIVE_CAPACITY)),
                state: Mutex::new(CoordinatorState {
                    next_wave_id: 1,
                    next_waiter_id: 1,
                    ..CoordinatorState::default()
                }),
                #[cfg(test)]
                before_scan: Mutex::new(None),
            }),
        }
    }

    pub(super) async fn reconcile(
        &self,
        cancellation: CancellationToken,
        deadline: Option<tokio::time::Instant>,
    ) -> Result<()> {
        {
            let mut state = self.state()?;
            state.diagnostics.requests = state.diagnostics.requests.saturating_add(1);
        }
        let _active_request = match Arc::clone(&self.inner.active_requests).try_acquire_owned() {
            Ok(permit) => permit,
            Err(_) => {
                let mut state = self.state()?;
                state.diagnostics.rejected_requests =
                    state.diagnostics.rejected_requests.saturating_add(1);
                return Err(Error::RetrievalOverloaded);
            }
        };
        if cancellation.is_cancelled() {
            let mut state = self.state()?;
            state.diagnostics.cancelled_waiters =
                state.diagnostics.cancelled_waiters.saturating_add(1);
            return Err(Error::Cancelled);
        }

        let (waiter_id, receiver) = self.enqueue()?;
        let mut guard = WaiterGuard::new(self.clone(), waiter_id);
        let deadline_elapsed = async move {
            match deadline {
                Some(deadline) => tokio::time::sleep_until(deadline).await,
                None => std::future::pending().await,
            }
        };
        tokio::pin!(deadline_elapsed);
        let outcome = tokio::select! {
            biased;
            _ = cancellation.cancelled() => {
                guard.cancel()?;
                return Err(Error::Cancelled);
            }
            outcome = receiver => outcome,
            _ = &mut deadline_elapsed => {
                guard.timeout()?;
                return Err(Error::IndexNotReady);
            }
        };
        guard.disarm();
        match outcome {
            Ok(WaveOutcome::Complete) => Ok(()),
            Ok(WaveOutcome::Failed(error)) => Err(Error::ReconciliationFailed(error)),
            Err(_) => Err(Error::OperationFailure(
                "reconciliation coordinator stopped unexpectedly".into(),
            )),
        }
    }

    fn enqueue(&self) -> Result<(u64, oneshot::Receiver<WaveOutcome>)> {
        let (sender, receiver) = oneshot::channel();
        let mut start = None;
        let waiter_id;
        {
            let mut state = self.state()?;
            waiter_id = state.next_waiter_id;
            state.next_waiter_id = state.next_waiter_id.saturating_add(1);
            let waiter = Waiter {
                id: waiter_id,
                sender,
            };

            match state.current.as_mut() {
                None => {
                    let wave = self.new_wave(&mut state, waiter);
                    start = Some((wave.id, wave.cancellation.clone()));
                    state.current = Some(wave);
                }
                Some(current)
                    if current.phase == WavePhase::WaitingForOperation
                        && !current.cancellation.is_cancelled() =>
                {
                    current.waiters.push(waiter);
                    state.diagnostics.coalesced_requests =
                        state.diagnostics.coalesced_requests.saturating_add(1);
                }
                Some(_) => match state.pending.as_mut() {
                    Some(pending) => {
                        pending.waiters.push(waiter);
                        state.diagnostics.coalesced_requests =
                            state.diagnostics.coalesced_requests.saturating_add(1);
                    }
                    None => {
                        state.pending = Some(self.new_wave(&mut state, waiter));
                    }
                },
            }
            update_waiter_high_water(&mut state);
        }

        if let Some((wave_id, wave_cancellation)) = start {
            self.spawn_wave(wave_id, wave_cancellation);
        }
        Ok((waiter_id, receiver))
    }

    fn new_wave(&self, state: &mut CoordinatorState, waiter: Waiter) -> Wave {
        let id = state.next_wave_id;
        state.next_wave_id = state.next_wave_id.saturating_add(1);
        state.diagnostics.waves_created = state.diagnostics.waves_created.saturating_add(1);
        state.diagnostics.active_waves = state.diagnostics.active_waves.saturating_add(1);
        state.diagnostics.peak_active_waves = state
            .diagnostics
            .peak_active_waves
            .max(state.diagnostics.active_waves);
        Wave {
            id,
            phase: WavePhase::WaitingForOperation,
            cancellation: CancellationToken::new(),
            waiters: vec![waiter],
            _active: ActiveReconciliation::new(
                Arc::clone(&self.inner.active_reconciliations),
                Arc::clone(&self.inner.reconciliation_changed),
            ),
        }
    }

    fn spawn_wave(&self, wave_id: u64, cancellation: CancellationToken) {
        let coordinator = self.clone();
        let runner_coordinator = self.clone();
        let indexer = self.inner.indexer.clone();
        let coordination = self.inner.coordination.clone();
        tokio::spawn(async move {
            let result = tokio::task::spawn_blocking(move || {
                let operation = coordination.acquire_operation(&cancellation)?;
                if !runner_coordinator.mark_running(wave_id)? {
                    operation.release()?;
                    return Err(Error::Cancelled);
                }
                runner_coordinator.run_before_scan_hook();
                let result = indexer
                    .reconcile_cancellable_report(false, &cancellation)
                    .map(|_| ());
                operation.release()?;
                result
            })
            .await
            .map_err(Error::from)
            .and_then(|result| result);
            coordinator.finish_wave(wave_id, result);
        });
    }

    fn mark_running(&self, wave_id: u64) -> Result<bool> {
        let mut state = self.state()?;
        let Some(current) = state.current.as_mut() else {
            return Ok(false);
        };
        if current.id != wave_id
            || current.phase != WavePhase::WaitingForOperation
            || current.waiters.is_empty()
            || current.cancellation.is_cancelled()
        {
            return Ok(false);
        }
        current.phase = WavePhase::Running;
        let waiters = current.waiters.len();
        state.diagnostics.waves_started = state.diagnostics.waves_started.saturating_add(1);
        tracing::debug!(wave_id, waiters, "reconciliation wave started");
        Ok(true)
    }

    fn finish_wave(&self, wave_id: u64, result: Result<()>) {
        let mut start = None;
        {
            let mut state = match self.inner.state.lock() {
                Ok(state) => state,
                Err(poisoned) => {
                    let mut state = poisoned.into_inner();
                    let mut waiters = state
                        .current
                        .take()
                        .map_or_else(Vec::new, |wave| wave.waiters);
                    if let Some(pending) = state.pending.take() {
                        waiters.extend(pending.waiters);
                    }
                    state.diagnostics.active_waves = 0;
                    state.diagnostics.pending_waiters = 0;
                    state.diagnostics.waves_failed =
                        state.diagnostics.waves_failed.saturating_add(1);
                    drop(state);
                    distribute_failure(waiters, reconciliation_state_poisoned());
                    return;
                }
            };
            let Some(current) = state.current.take() else {
                return;
            };
            if current.id != wave_id {
                state.current = Some(current);
                return;
            }

            match result {
                Ok(()) => {
                    state.diagnostics.waves_completed =
                        state.diagnostics.waves_completed.saturating_add(1);
                    for waiter in current.waiters {
                        let _ = waiter.sender.send(WaveOutcome::Complete);
                    }
                    tracing::debug!(wave_id, "reconciliation wave completed");
                }
                Err(Error::Cancelled) if current.waiters.is_empty() => {
                    state.diagnostics.waves_cancelled_before_start = state
                        .diagnostics
                        .waves_cancelled_before_start
                        .saturating_add(1);
                    tracing::debug!(wave_id, "unused reconciliation wave cancelled");
                }
                Err(error) => {
                    state.diagnostics.waves_failed =
                        state.diagnostics.waves_failed.saturating_add(1);
                    distribute_failure(current.waiters, error);
                    tracing::debug!(wave_id, "reconciliation wave failed");
                }
            }

            if let Some(pending) = state.pending.take() {
                if pending.waiters.is_empty() {
                    state.diagnostics.waves_cancelled_before_start = state
                        .diagnostics
                        .waves_cancelled_before_start
                        .saturating_add(1);
                } else {
                    start = Some((pending.id, pending.cancellation.clone()));
                    state.current = Some(pending);
                }
            }
            update_waiter_high_water(&mut state);
        }

        if let Some((next_wave_id, cancellation)) = start {
            self.spawn_wave(next_wave_id, cancellation);
        }
    }

    fn exit_waiter(&self, waiter_id: u64, reason: WaiterExit) -> Result<()> {
        let mut state = self.state()?;
        match reason {
            WaiterExit::Cancelled => {
                state.diagnostics.cancelled_waiters =
                    state.diagnostics.cancelled_waiters.saturating_add(1);
            }
            WaiterExit::TimedOut => {
                state.diagnostics.timed_out_waiters =
                    state.diagnostics.timed_out_waiters.saturating_add(1);
            }
        }

        if let Some(current) = state.current.as_mut()
            && remove_waiter(&mut current.waiters, waiter_id)
        {
            if current.waiters.is_empty() && current.phase == WavePhase::WaitingForOperation {
                current.cancellation.cancel();
            }
            update_waiter_high_water(&mut state);
            return Ok(());
        }

        let remove_pending = state
            .pending
            .as_mut()
            .is_some_and(|pending| remove_waiter(&mut pending.waiters, waiter_id));
        if remove_pending
            && state
                .pending
                .as_ref()
                .is_some_and(|wave| wave.waiters.is_empty())
        {
            state.pending = None;
            state.diagnostics.waves_cancelled_before_start = state
                .diagnostics
                .waves_cancelled_before_start
                .saturating_add(1);
        }
        update_waiter_high_water(&mut state);
        Ok(())
    }

    #[cfg(test)]
    pub(super) fn reset_diagnostics(&self) {
        let mut state = self
            .inner
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let active_waves =
            usize::from(state.current.is_some()) + usize::from(state.pending.is_some());
        let pending_waiters = state.pending.as_ref().map_or(0, |wave| wave.waiters.len());
        state.diagnostics = ReconciliationDiagnostics {
            active_waves,
            peak_active_waves: active_waves,
            pending_waiters,
            peak_pending_waiters: pending_waiters,
            ..ReconciliationDiagnostics::default()
        };
    }

    #[cfg(test)]
    pub(super) fn diagnostics(&self) -> ReconciliationDiagnostics {
        self.inner
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .diagnostics
            .clone()
    }

    #[cfg(test)]
    pub(super) fn poison_state_for_test(&self) {
        let inner = Arc::clone(&self.inner);
        let _ = std::thread::spawn(move || {
            let _state = inner.state.lock().expect("lock reconciliation state");
            panic!("poison reconciliation state");
        })
        .join();
    }

    #[cfg(test)]
    pub(super) fn set_before_scan_hook(&self, hook: Option<Arc<dyn Fn() + Send + Sync>>) {
        *self.inner.before_scan.lock().expect("reconciliation hook") = hook;
    }

    #[cfg(test)]
    fn run_before_scan_hook(&self) {
        let hook = self
            .inner
            .before_scan
            .lock()
            .expect("reconciliation hook")
            .clone();
        if let Some(hook) = hook {
            hook();
        }
    }

    #[cfg(not(test))]
    fn run_before_scan_hook(&self) {}
}

fn update_waiter_high_water(state: &mut CoordinatorState) {
    state.diagnostics.active_waves =
        usize::from(state.current.is_some()) + usize::from(state.pending.is_some());
    state.diagnostics.pending_waiters = state.pending.as_ref().map_or(0, |wave| wave.waiters.len());
    state.diagnostics.peak_pending_waiters = state
        .diagnostics
        .peak_pending_waiters
        .max(state.diagnostics.pending_waiters);
}

fn reconciliation_state_poisoned() -> Error {
    Error::OperationFailure("reconciliation coordinator state poisoned".into())
}

fn remove_waiter(waiters: &mut Vec<Waiter>, waiter_id: u64) -> bool {
    let Some(index) = waiters.iter().position(|waiter| waiter.id == waiter_id) else {
        return false;
    };
    waiters.swap_remove(index);
    true
}

fn distribute_failure(waiters: Vec<Waiter>, error: Error) {
    let error = Arc::new(error);
    for waiter in waiters {
        let _ = waiter.sender.send(WaveOutcome::Failed(Arc::clone(&error)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failed_wave_shares_one_typed_error_without_retrying_waiters() {
        let (closed_sender, closed_receiver) = oneshot::channel();
        drop(closed_receiver);
        let (first_sender, mut first_receiver) = oneshot::channel();
        let (second_sender, mut second_receiver) = oneshot::channel();

        distribute_failure(
            vec![
                Waiter {
                    id: 1,
                    sender: closed_sender,
                },
                Waiter {
                    id: 2,
                    sender: first_sender,
                },
                Waiter {
                    id: 3,
                    sender: second_sender,
                },
            ],
            Error::LimitExceeded,
        );

        let Ok(WaveOutcome::Failed(first)) = first_receiver.try_recv() else {
            panic!("first live waiter should receive the failure");
        };
        let Ok(WaveOutcome::Failed(second)) = second_receiver.try_recv() else {
            panic!("second live waiter should receive the failure");
        };
        assert!(Arc::ptr_eq(&first, &second));
        assert!(matches!(first.as_ref(), Error::LimitExceeded));
    }
}
