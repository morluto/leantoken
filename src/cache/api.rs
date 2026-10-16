use super::*;

/// List centrally managed caches using explicit filters and response bounds.
pub fn list_with(request: &CacheListRequest) -> Result<CacheListReport> {
    CacheManager::for_current_user()?.list_with(request)
}

/// List managed caches with cooperative cancellation during bounded inventory.
pub fn list_with_cancellation(
    request: &CacheListRequest,
    cancellation: &tokio_util::sync::CancellationToken,
) -> Result<CacheListReport> {
    let mut manager = CacheManager::for_current_user()?;
    manager.cancellation = cancellation.clone();
    manager.list_with(request)
}

/// Prune centrally managed repository caches using explicit criteria.
pub fn prune(request: &CachePruneRequest) -> Result<CachePruneReport> {
    CacheManager::for_current_user()?.prune(request)
}

/// Prune managed caches with cooperative cancellation before each cache mutation.
///
/// Cancellation during the initial inventory fails before any deletion. Once
/// deletion starts, already completed removals remain in the returned report;
/// cancelled revalidations are reported as failures and prevent further removals.
pub fn prune_with_cancellation(
    request: &CachePruneRequest,
    cancellation: &tokio_util::sync::CancellationToken,
) -> Result<CachePruneReport> {
    let mut manager = CacheManager::for_current_user()?;
    manager.cancellation = cancellation.clone();
    manager.prune(request)
}

/// Compact explicitly selected inactive caches without migrating their schemas.
pub fn compact(request: &CacheCompactRequest) -> Result<CacheCompactReport> {
    CacheManager::for_current_user()?.compact(request)
}
