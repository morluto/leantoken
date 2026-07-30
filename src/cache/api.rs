use super::*;

/// List the first bounded page of centrally managed caches for the current user.
pub fn list() -> Result<CacheListReport> {
    list_with(&CacheListRequest::default())
}

/// List centrally managed caches using explicit filters and response bounds.
pub fn list_with(request: &CacheListRequest) -> Result<CacheListReport> {
    CacheManager::for_current_user()?.list_with(request)
}

/// List managed caches with explicit content-compatibility diagnostics.
pub fn list_v2_with(request: &CacheListV2Request) -> Result<CacheListV2Report> {
    CacheManager::for_current_user()?.list_v2_with(request)
}

/// Prune centrally managed repository caches using explicit criteria.
pub fn prune(request: &CachePruneRequest) -> Result<CachePruneReport> {
    CacheManager::for_current_user()?.prune(request)
}

/// Prune caches with versioned compatibility criteria.
pub fn prune_v2(request: &CachePruneV2Request) -> Result<CachePruneReport> {
    CacheManager::for_current_user()?.prune_v2(request)
}
