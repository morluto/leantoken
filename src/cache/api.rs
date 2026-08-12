use super::*;

/// List centrally managed caches using explicit filters and response bounds.
pub fn list_with(request: &CacheListRequest) -> Result<CacheListReport> {
    CacheManager::for_current_user()?.list_with(request)
}

/// Prune centrally managed repository caches using explicit criteria.
pub fn prune(request: &CachePruneRequest) -> Result<CachePruneReport> {
    CacheManager::for_current_user()?.prune(request)
}
