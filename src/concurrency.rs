use std::num::NonZeroUsize;

pub(crate) const MIN_CPU_CAPACITY: usize = 4;
pub(crate) const MAX_CPU_CAPACITY: usize = 16;
pub(crate) const BLOCKING_QUEUE_CAPACITY: usize = 8;

/// Return a bounded CPU-aware capacity that respects process and container limits.
pub(crate) fn default_cpu_capacity() -> usize {
    std::thread::available_parallelism()
        .map(NonZeroUsize::get)
        .unwrap_or(1)
        .clamp(MIN_CPU_CAPACITY, MAX_CPU_CAPACITY)
}

pub(crate) fn default_blocking_active_capacity() -> usize {
    default_cpu_capacity() + BLOCKING_QUEUE_CAPACITY
}

pub(crate) fn default_read_connection_capacity() -> u32 {
    // Retrieval can use every blocking execution slot while a reconciliation
    // independently reads the committed baseline before publishing. Reserve
    // one bounded reader for that publication path so a full retrieval cohort
    // cannot turn snapshot consistency into a pool timeout.
    (default_cpu_capacity() + 1) as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_capacities_are_bounded_and_consistent() {
        let execution = default_cpu_capacity();
        assert!((MIN_CPU_CAPACITY..=MAX_CPU_CAPACITY).contains(&execution));
        assert_eq!(
            default_blocking_active_capacity(),
            execution + BLOCKING_QUEUE_CAPACITY
        );
        assert_eq!(default_read_connection_capacity() as usize, execution + 1);
    }
}
