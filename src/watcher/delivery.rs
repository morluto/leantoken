use super::*;

pub(super) fn flush(
    pending: &mut BTreeSet<String>,
    rename_from: &mut HashMap<usize, String>,
    rename_to: &mut HashMap<usize, String>,
    reconcile: &mut bool,
    tx: &mpsc::Sender<WatcherMessage>,
    counters: &WatcherCounters,
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
