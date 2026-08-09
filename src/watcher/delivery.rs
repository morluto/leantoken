use super::*;

pub(super) fn flush(
    pending: &mut PendingReconciliation,
    tx: &mpsc::Sender<WatcherMessage>,
    counters: &WatcherCounters,
) -> bool {
    if pending.is_full() {
        match tx.try_send(WatcherMessage::ReconcileRequired) {
            Ok(()) => {
                *pending = PendingReconciliation::empty();
                counters
                    .full_reconciliation_deliveries
                    .fetch_add(1, Ordering::Relaxed);
            }
            Err(TrySendError::Full(_)) => {}
            Err(TrySendError::Closed(_)) => return false,
        }
    }

    if let PendingReconciliation::Paths(paths) = pending
        && !paths.is_empty()
    {
        let message_paths = paths.iter().cloned().collect();
        match tx.try_send(WatcherMessage::Changed {
            paths: message_paths,
        }) {
            Ok(()) => {
                paths.clear();
                counters
                    .changed_path_deliveries
                    .fetch_add(1, Ordering::Relaxed);
            }
            Err(TrySendError::Full(_)) => {
                pending.require_full();
            }
            Err(TrySendError::Closed(_)) => return false,
        }
    }

    true
}
