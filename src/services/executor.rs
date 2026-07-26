use std::sync::Arc;
use std::time::Duration;
#[cfg(test)]
use std::{collections::HashSet, sync::Mutex, time::Instant};

use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio_util::sync::CancellationToken;

use crate::{Error, Result};

pub(super) const DEFAULT_BLOCKING_EXECUTION_CAPACITY: usize = 8;
const DEFAULT_BLOCKING_QUEUE_CAPACITY: usize = 8;
pub(super) const DEFAULT_BLOCKING_ACTIVE_CAPACITY: usize =
    DEFAULT_BLOCKING_EXECUTION_CAPACITY + DEFAULT_BLOCKING_QUEUE_CAPACITY;
pub(super) const DEFAULT_BLOCKING_QUEUE_TIMEOUT: Duration = Duration::from_millis(500);

#[derive(Debug, Clone)]
pub(super) struct BlockingExecutor {
    inner: Arc<BlockingExecutorInner>,
}

#[derive(Debug)]
struct BlockingExecutorInner {
    active: Arc<Semaphore>,
    execution: Arc<Semaphore>,
    queue_timeout: Duration,
    #[cfg(test)]
    diagnostics: Arc<Mutex<BlockingExecutorDiagnostics>>,
}

#[cfg(test)]
#[derive(Debug, Clone, Default)]
pub(super) struct BlockingExecutorDiagnostics {
    pub submitted: u64,
    pub accepted: u64,
    pub rejected: u64,
    pub queue_timed_out: u64,
    pub cancelled_before_start: u64,
    pub started: u64,
    pub finished: u64,
    pub active: usize,
    pub peak_active: usize,
    pub running: usize,
    pub peak_running: usize,
    pub queue_wait_micros: Vec<u64>,
    pub blocking_threads: HashSet<String>,
}

struct ActivePermit {
    _permit: OwnedSemaphorePermit,
    #[cfg(test)]
    diagnostics: Arc<Mutex<BlockingExecutorDiagnostics>>,
}

impl ActivePermit {
    fn new(
        permit: OwnedSemaphorePermit,
        #[cfg(test)] diagnostics: Arc<Mutex<BlockingExecutorDiagnostics>>,
    ) -> Self {
        Self {
            _permit: permit,
            #[cfg(test)]
            diagnostics,
        }
    }
}

#[cfg(test)]
impl Drop for ActivePermit {
    fn drop(&mut self) {
        let mut diagnostics = self.diagnostics.lock().expect("executor diagnostics");
        diagnostics.active = diagnostics.active.saturating_sub(1);
    }
}

#[cfg(test)]
struct StartedWork {
    diagnostics: Arc<Mutex<BlockingExecutorDiagnostics>>,
}

#[cfg(test)]
impl StartedWork {
    fn new(diagnostics: Arc<Mutex<BlockingExecutorDiagnostics>>) -> Self {
        {
            let mut diagnostics_guard = diagnostics.lock().expect("executor diagnostics");
            diagnostics_guard.started = diagnostics_guard.started.saturating_add(1);
            diagnostics_guard.running = diagnostics_guard.running.saturating_add(1);
            diagnostics_guard.peak_running = diagnostics_guard
                .peak_running
                .max(diagnostics_guard.running);
            diagnostics_guard
                .blocking_threads
                .insert(format!("{:?}", std::thread::current().id()));
        }
        Self { diagnostics }
    }
}

#[cfg(test)]
impl Drop for StartedWork {
    fn drop(&mut self) {
        let mut diagnostics = self.diagnostics.lock().expect("executor diagnostics");
        diagnostics.finished = diagnostics.finished.saturating_add(1);
        diagnostics.running = diagnostics.running.saturating_sub(1);
    }
}

impl Default for BlockingExecutor {
    fn default() -> Self {
        Self::new(
            DEFAULT_BLOCKING_ACTIVE_CAPACITY,
            DEFAULT_BLOCKING_EXECUTION_CAPACITY,
            DEFAULT_BLOCKING_QUEUE_TIMEOUT,
        )
    }
}

impl BlockingExecutor {
    pub(super) fn new(
        active_capacity: usize,
        execution_capacity: usize,
        queue_timeout: Duration,
    ) -> Self {
        debug_assert!(active_capacity >= execution_capacity);
        debug_assert!(execution_capacity > 0);
        Self {
            inner: Arc::new(BlockingExecutorInner {
                active: Arc::new(Semaphore::new(active_capacity)),
                execution: Arc::new(Semaphore::new(execution_capacity)),
                queue_timeout,
                #[cfg(test)]
                diagnostics: Arc::new(Mutex::new(BlockingExecutorDiagnostics::default())),
            }),
        }
    }

    pub(super) async fn run<T, F>(&self, cancellation: CancellationToken, operation: F) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce(&CancellationToken) -> Result<T> + Send + 'static,
    {
        #[cfg(test)]
        {
            let mut diagnostics = self.inner.diagnostics.lock().expect("executor diagnostics");
            diagnostics.submitted = diagnostics.submitted.saturating_add(1);
        }
        if cancellation.is_cancelled() {
            #[cfg(test)]
            {
                let mut diagnostics = self.inner.diagnostics.lock().expect("executor diagnostics");
                diagnostics.cancelled_before_start =
                    diagnostics.cancelled_before_start.saturating_add(1);
            }
            return Err(Error::Cancelled);
        }

        let active = match Arc::clone(&self.inner.active).try_acquire_owned() {
            Ok(permit) => {
                #[cfg(test)]
                {
                    let mut diagnostics =
                        self.inner.diagnostics.lock().expect("executor diagnostics");
                    diagnostics.accepted = diagnostics.accepted.saturating_add(1);
                    diagnostics.active = diagnostics.active.saturating_add(1);
                    diagnostics.peak_active = diagnostics.peak_active.max(diagnostics.active);
                }
                ActivePermit::new(
                    permit,
                    #[cfg(test)]
                    Arc::clone(&self.inner.diagnostics),
                )
            }
            Err(_) => {
                #[cfg(test)]
                {
                    let mut diagnostics =
                        self.inner.diagnostics.lock().expect("executor diagnostics");
                    diagnostics.rejected = diagnostics.rejected.saturating_add(1);
                }
                return Err(Error::RetrievalOverloaded);
            }
        };
        #[cfg(test)]
        let queue_started = Instant::now();
        let execution = match self.wait_for_execution(cancellation.clone()).await {
            Ok(permit) => {
                #[cfg(test)]
                {
                    let wait = queue_started
                        .elapsed()
                        .as_micros()
                        .min(u128::from(u64::MAX)) as u64;
                    self.inner
                        .diagnostics
                        .lock()
                        .expect("executor diagnostics")
                        .queue_wait_micros
                        .push(wait);
                }
                permit
            }
            Err(error) => {
                #[cfg(test)]
                {
                    let mut diagnostics =
                        self.inner.diagnostics.lock().expect("executor diagnostics");
                    match &error {
                        Error::RetrievalQueueTimeout => {
                            diagnostics.queue_timed_out =
                                diagnostics.queue_timed_out.saturating_add(1);
                        }
                        Error::Cancelled => {
                            diagnostics.cancelled_before_start =
                                diagnostics.cancelled_before_start.saturating_add(1);
                        }
                        _ => {}
                    }
                }
                return Err(error);
            }
        };

        if cancellation.is_cancelled() {
            #[cfg(test)]
            {
                let mut diagnostics = self.inner.diagnostics.lock().expect("executor diagnostics");
                diagnostics.cancelled_before_start =
                    diagnostics.cancelled_before_start.saturating_add(1);
            }
            return Err(Error::Cancelled);
        }

        #[cfg(test)]
        let diagnostics = Arc::clone(&self.inner.diagnostics);
        tokio::task::spawn_blocking(move || {
            // These permits deliberately belong to the blocking closure. Dropping
            // or aborting the async caller must not make still-running work
            // invisible to either bound.
            let _active = active;
            let _execution = execution;
            #[cfg(test)]
            let _started = StartedWork::new(diagnostics);
            if cancellation.is_cancelled() {
                return Err(Error::Cancelled);
            }
            operation(&cancellation)
        })
        .await?
    }

    async fn wait_for_execution(
        &self,
        cancellation: CancellationToken,
    ) -> Result<OwnedSemaphorePermit> {
        let acquire = Arc::clone(&self.inner.execution).acquire_owned();
        tokio::select! {
            biased;
            _ = cancellation.cancelled() => Err(Error::Cancelled),
            result = tokio::time::timeout(self.inner.queue_timeout, acquire) => match result {
                Ok(Ok(permit)) => Ok(permit),
                Ok(Err(_)) => unreachable!("blocking executor semaphore is never closed"),
                Err(_) => Err(Error::RetrievalQueueTimeout),
            },
        }
    }

    #[cfg(test)]
    fn active_available_permits(&self) -> usize {
        self.inner.active.available_permits()
    }

    #[cfg(test)]
    pub(super) fn reset_diagnostics(&self) {
        *self.inner.diagnostics.lock().expect("executor diagnostics") =
            BlockingExecutorDiagnostics::default();
    }

    #[cfg(test)]
    pub(super) fn diagnostics(&self) -> BlockingExecutorDiagnostics {
        self.inner
            .diagnostics
            .lock()
            .expect("executor diagnostics")
            .clone()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::{Arc, Condvar, Mutex};

    use super::*;

    #[derive(Default)]
    struct Gate {
        open: Mutex<bool>,
        changed: Condvar,
    }

    impl Gate {
        fn wait(&self) {
            let mut open = self.open.lock().expect("gate lock");
            while !*open {
                open = self.changed.wait(open).expect("gate wait");
            }
        }

        fn open(&self) {
            *self.open.lock().expect("gate lock") = true;
            self.changed.notify_all();
        }
    }

    async fn wait_until(predicate: impl Fn() -> bool) {
        for _ in 0..1_000 {
            if predicate() {
                return;
            }
            tokio::task::yield_now().await;
        }
        panic!("condition was not reached");
    }

    #[tokio::test]
    async fn queued_work_boundary_rejects_without_starting_more_work() {
        let executor = BlockingExecutor::new(2, 1, Duration::from_secs(30));
        let gate = Arc::new(Gate::default());
        let started = Arc::new(AtomicUsize::new(0));

        let running = {
            let executor = executor.clone();
            let gate = Arc::clone(&gate);
            let started = Arc::clone(&started);
            tokio::spawn(async move {
                executor
                    .run(CancellationToken::new(), move |_| {
                        started.fetch_add(1, Ordering::SeqCst);
                        gate.wait();
                        Ok(())
                    })
                    .await
            })
        };
        wait_until(|| started.load(Ordering::SeqCst) == 1).await;

        let queued = {
            let executor = executor.clone();
            tokio::spawn(async move { executor.run(CancellationToken::new(), |_| Ok(())).await })
        };
        wait_until(|| executor.active_available_permits() == 0).await;

        assert!(matches!(
            executor.run(CancellationToken::new(), |_| Ok(())).await,
            Err(Error::RetrievalOverloaded)
        ));

        gate.open();
        running
            .await
            .expect("running task")
            .expect("running result");
        queued.await.expect("queued task").expect("queued result");
    }

    #[tokio::test(start_paused = true)]
    async fn queue_timeout_never_starts_the_operation() {
        let executor = BlockingExecutor::new(2, 1, Duration::from_millis(500));
        let gate = Arc::new(Gate::default());
        let running_started = Arc::new(AtomicBool::new(false));

        let running = {
            let executor = executor.clone();
            let gate = Arc::clone(&gate);
            let running_started = Arc::clone(&running_started);
            tokio::spawn(async move {
                executor
                    .run(CancellationToken::new(), move |_| {
                        running_started.store(true, Ordering::SeqCst);
                        gate.wait();
                        Ok(())
                    })
                    .await
            })
        };
        wait_until(|| running_started.load(Ordering::SeqCst)).await;

        let queued_started = Arc::new(AtomicBool::new(false));
        let queued = {
            let executor = executor.clone();
            let queued_started = Arc::clone(&queued_started);
            tokio::spawn(async move {
                executor
                    .run(CancellationToken::new(), move |_| {
                        queued_started.store(true, Ordering::SeqCst);
                        Ok(())
                    })
                    .await
            })
        };
        wait_until(|| executor.active_available_permits() == 0).await;
        tokio::time::advance(Duration::from_millis(501)).await;

        assert!(matches!(
            queued.await.expect("queued task"),
            Err(Error::RetrievalQueueTimeout)
        ));
        assert!(!queued_started.load(Ordering::SeqCst));

        gate.open();
        running
            .await
            .expect("running task")
            .expect("running result");
    }

    #[tokio::test]
    async fn queued_cancellation_never_starts_the_operation() {
        let executor = BlockingExecutor::new(2, 1, Duration::from_secs(30));
        let gate = Arc::new(Gate::default());
        let running_started = Arc::new(AtomicBool::new(false));

        let running = {
            let executor = executor.clone();
            let gate = Arc::clone(&gate);
            let running_started = Arc::clone(&running_started);
            tokio::spawn(async move {
                executor
                    .run(CancellationToken::new(), move |_| {
                        running_started.store(true, Ordering::SeqCst);
                        gate.wait();
                        Ok(())
                    })
                    .await
            })
        };
        wait_until(|| running_started.load(Ordering::SeqCst)).await;

        let cancellation = CancellationToken::new();
        let queued_started = Arc::new(AtomicBool::new(false));
        let queued = {
            let executor = executor.clone();
            let queued_started = Arc::clone(&queued_started);
            let cancellation = cancellation.clone();
            tokio::spawn(async move {
                executor
                    .run(cancellation, move |_| {
                        queued_started.store(true, Ordering::SeqCst);
                        Ok(())
                    })
                    .await
            })
        };
        wait_until(|| executor.active_available_permits() == 0).await;
        cancellation.cancel();

        assert!(matches!(
            queued.await.expect("queued task"),
            Err(Error::Cancelled)
        ));
        assert!(!queued_started.load(Ordering::SeqCst));
        assert_eq!(executor.active_available_permits(), 1);

        gate.open();
        running
            .await
            .expect("running task")
            .expect("running result");
        assert_eq!(executor.active_available_permits(), 2);
    }

    #[tokio::test]
    async fn execution_never_exceeds_its_limit() {
        let executor = BlockingExecutor::new(6, 2, Duration::from_secs(30));
        let gate = Arc::new(Gate::default());
        let current = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let mut tasks = Vec::new();

        for _ in 0..6 {
            let executor = executor.clone();
            let gate = Arc::clone(&gate);
            let current = Arc::clone(&current);
            let peak = Arc::clone(&peak);
            tasks.push(tokio::spawn(async move {
                executor
                    .run(CancellationToken::new(), move |_| {
                        let now = current.fetch_add(1, Ordering::SeqCst) + 1;
                        peak.fetch_max(now, Ordering::SeqCst);
                        gate.wait();
                        current.fetch_sub(1, Ordering::SeqCst);
                        Ok(())
                    })
                    .await
            }));
        }
        wait_until(|| current.load(Ordering::SeqCst) == 2).await;
        assert_eq!(peak.load(Ordering::SeqCst), 2);

        gate.open();
        for task in tasks {
            task.await.expect("task").expect("result");
        }
        assert_eq!(peak.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn aborting_a_running_caller_does_not_release_capacity_early() {
        let executor = BlockingExecutor::new(1, 1, Duration::from_secs(30));
        let gate = Arc::new(Gate::default());
        let first_started = Arc::new(AtomicBool::new(false));

        let first = {
            let executor = executor.clone();
            let gate = Arc::clone(&gate);
            let first_started = Arc::clone(&first_started);
            tokio::spawn(async move {
                executor
                    .run(CancellationToken::new(), move |_| {
                        first_started.store(true, Ordering::SeqCst);
                        gate.wait();
                        Ok(())
                    })
                    .await
            })
        };
        wait_until(|| first_started.load(Ordering::SeqCst)).await;
        first.abort();
        assert!(first.await.expect_err("aborted caller").is_cancelled());

        assert!(matches!(
            executor.run(CancellationToken::new(), |_| Ok(())).await,
            Err(Error::RetrievalOverloaded)
        ));

        gate.open();
        wait_until(|| executor.active_available_permits() == 1).await;
        executor
            .run(CancellationToken::new(), |_| Ok(()))
            .await
            .expect("capacity returns after closure completion");
    }

    #[tokio::test]
    async fn permits_return_after_success_error_and_panic() {
        let executor = BlockingExecutor::new(1, 1, Duration::from_secs(30));

        executor
            .run(CancellationToken::new(), |_| Ok(()))
            .await
            .expect("success");
        assert_eq!(executor.active_available_permits(), 1);

        let cancelled = CancellationToken::new();
        cancelled.cancel();
        assert!(matches!(
            executor.run(cancelled, |_| Ok(())).await,
            Err(Error::Cancelled)
        ));
        assert_eq!(executor.active_available_permits(), 1);

        assert!(matches!(
            executor
                .run(CancellationToken::new(), |_| Err::<(), _>(Error::Cancelled))
                .await,
            Err(Error::Cancelled)
        ));
        assert_eq!(executor.active_available_permits(), 1);

        assert!(matches!(
            executor
                .run::<(), _>(CancellationToken::new(), |_| panic!("test panic"))
                .await,
            Err(Error::Join(error)) if error.is_panic()
        ));
        assert_eq!(executor.active_available_permits(), 1);
    }
}
