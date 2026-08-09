use super::*;

pub(super) fn flush(
    pending: &mut BTreeSet<String>,
    reconcile: &mut bool,
    tx: &mpsc::Sender<WatcherMessage>,
    counters: &WatcherCounters,
) -> bool {
    if *reconcile {
        match tx.try_send(WatcherMessage::ReconcileRequired) {
            Ok(()) => {
                *reconcile = false;
                pending.clear();
                counters
                    .full_reconciliation_deliveries
                    .fetch_add(1, Ordering::Relaxed);
            }
            Err(TrySendError::Full(_)) => {}
            Err(TrySendError::Closed(_)) => return false,
        }
    }

    if !pending.is_empty() {
        let paths = pending.iter().cloned().collect();
        match tx.try_send(WatcherMessage::Changed { paths }) {
            Ok(()) => {
                pending.clear();
                counters
                    .changed_path_deliveries
                    .fetch_add(1, Ordering::Relaxed);
            }
            Err(TrySendError::Full(_)) => {
                *reconcile = true;
                pending.clear();
            }
            Err(TrySendError::Closed(_)) => return false,
        }
    }

    true
}
