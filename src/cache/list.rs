use super::*;

impl CacheManager {
    pub(super) fn for_current_user() -> Result<Self> {
        let root = managed_cache_root().ok_or_else(|| {
            Error::InvalidConfiguration(
                "this platform does not provide a central managed cache directory".into(),
            )
        })?;
        Ok(Self::new(root, unix_seconds(SystemTime::now())))
    }

    pub(super) fn new(root: PathBuf, now: u64) -> Self {
        Self { root, now }
    }

    #[cfg(test)]
    pub(super) fn list(&self) -> Result<CacheListReport> {
        self.list_with(&CacheListRequest::default())
    }

    pub(super) fn list_with(&self, request: &CacheListRequest) -> Result<CacheListReport> {
        validate_list_request(request)?;
        let repository_root = request
            .repository_root
            .as_deref()
            .map(normalize_repository_root_filter);
        let filter_hash = cache_list_filter_hash(request, repository_root.as_deref());
        let after_id = request
            .cursor
            .as_deref()
            .map(|cursor| decode_cache_list_cursor(cursor, &filter_hash))
            .transpose()?;

        let (entries, ignored_entries) = self.inspect_all()?;
        let total_bytes = entries.iter().fold(0u64, |total, cache| {
            total.saturating_add(cache.entry.size_bytes)
        });
        let matching = entries
            .iter()
            .filter(|cache| {
                (request.states.is_empty() || request.states.contains(&cache.entry.state))
                    && repository_root
                        .as_ref()
                        .is_none_or(|root| cache.entry.repository_root.as_ref() == Some(root))
            })
            .collect::<Vec<_>>();
        let matched_bytes = matching.iter().fold(0u64, |total, cache| {
            total.saturating_add(cache.entry.size_bytes)
        });
        let active_entries = matching.iter().filter(|cache| cache.entry.active).count();
        let missing_root_entries = matching
            .iter()
            .filter(|cache| cache.entry.repository_available == Some(false))
            .count();
        let mut state_counts = CacheState::ALL
            .into_iter()
            .map(|state| (state.label().to_owned(), 0usize))
            .collect::<BTreeMap<_, _>>();
        for cache in &matching {
            *state_counts
                .get_mut(cache.entry.state.label())
                .expect("every cache state has a summary bucket") += 1;
        }

        let start = after_id.as_deref().map_or(0, |after_id| {
            matching.partition_point(|cache| cache.entry.id.as_str() <= after_id)
        });
        let end = if request.summary {
            start
        } else {
            start.saturating_add(request.limit).min(matching.len())
        };
        let page = matching[start..end]
            .iter()
            .map(|cache| cache.entry.clone())
            .collect::<Vec<_>>();
        let next_cursor = if !request.summary && end < matching.len() {
            page.last()
                .map(|entry| encode_cache_list_cursor(&filter_hash, &entry.id))
        } else {
            None
        };
        Ok(CacheListReport {
            cache_root: self.root.clone(),
            total_entries: entries.len(),
            matched_entries: matching.len(),
            returned_entries: page.len(),
            total_bytes,
            matched_bytes,
            active_entries,
            missing_root_entries,
            state_counts,
            ignored_entries,
            summary_only: request.summary,
            next_cursor,
            entries: page,
        })
    }

    pub(super) fn list_v2_with(&self, request: &CacheListV2Request) -> Result<CacheListV2Report> {
        validate_list_request(&request.request)?;
        if request.compatibilities.len() > MAX_CACHE_COMPATIBILITY_FILTERS {
            return Err(Error::RequestLimitExceeded {
                field: "cache compatibility filters",
                requested: request.compatibilities.len(),
                limit: MAX_CACHE_COMPATIBILITY_FILTERS,
            });
        }
        if request.index_content_versions.len() > MAX_CACHE_CONTENT_VERSION_FILTERS {
            return Err(Error::RequestLimitExceeded {
                field: "cache content-version filters",
                requested: request.index_content_versions.len(),
                limit: MAX_CACHE_CONTENT_VERSION_FILTERS,
            });
        }
        if request.index_content_versions.contains(&0) {
            return Err(Error::InvalidInput {
                field: "cache content-version filter",
                reason: "must be positive",
            });
        }
        let repository_root = request
            .request
            .repository_root
            .as_deref()
            .map(normalize_repository_root_filter);
        let filter_hash = cache_list_v2_filter_hash(request, repository_root.as_deref());
        let after_id = request
            .request
            .cursor
            .as_deref()
            .map(|cursor| {
                decode_cache_list_cursor_with_prefix(
                    cursor,
                    CACHE_LIST_V2_CURSOR_PREFIX,
                    &filter_hash,
                )
            })
            .transpose()?;

        let (entries, ignored_entries) = self.inspect_all()?;
        let total_bytes = entries.iter().fold(0u64, |total, cache| {
            total.saturating_add(cache.entry.size_bytes)
        });
        let matching = entries
            .iter()
            .filter(|cache| {
                (request.request.states.is_empty()
                    || request.request.states.contains(&cache.entry.state))
                    && repository_root
                        .as_ref()
                        .is_none_or(|root| cache.entry.repository_root.as_ref() == Some(root))
                    && (request.compatibilities.is_empty()
                        || request.compatibilities.contains(&cache.compatibility))
                    && (request.index_content_versions.is_empty()
                        || cache.entry.index_content_version.is_some_and(|version| {
                            request.index_content_versions.contains(&version)
                        }))
                    && (!request.incompatible_with_current
                        || cache.compatibility.safely_incompatible())
            })
            .collect::<Vec<_>>();
        let matched_bytes = matching.iter().fold(0u64, |total, cache| {
            total.saturating_add(cache.entry.size_bytes)
        });
        let active_entries = matching.iter().filter(|cache| cache.entry.active).count();
        let missing_root_entries = matching
            .iter()
            .filter(|cache| cache.entry.repository_available == Some(false))
            .count();
        let mut state_counts = CacheState::ALL
            .into_iter()
            .map(|state| (state.label().to_owned(), 0usize))
            .collect::<BTreeMap<_, _>>();
        let mut compatibility_counts = CacheCompatibility::ALL
            .into_iter()
            .map(|compatibility| {
                (
                    compatibility.label().to_owned(),
                    CacheCompatibilitySummary::default(),
                )
            })
            .collect::<BTreeMap<_, _>>();
        let mut safely_reclaimable_incompatible_entries = 0usize;
        let mut safely_reclaimable_incompatible_bytes = 0u64;
        for cache in &matching {
            *state_counts
                .get_mut(cache.entry.state.label())
                .expect("every cache state has a summary bucket") += 1;
            let summary = compatibility_counts
                .get_mut(cache.compatibility.label())
                .expect("every compatibility has a summary bucket");
            summary.entries = summary.entries.saturating_add(1);
            summary.bytes = summary.bytes.saturating_add(cache.entry.size_bytes);
            if cache.compatibility.safely_incompatible()
                && cache.safe_to_prune
                && !cache.entry.active
            {
                safely_reclaimable_incompatible_entries =
                    safely_reclaimable_incompatible_entries.saturating_add(1);
                safely_reclaimable_incompatible_bytes =
                    safely_reclaimable_incompatible_bytes.saturating_add(cache.entry.size_bytes);
            }
        }

        let start = after_id.as_deref().map_or(0, |after_id| {
            matching.partition_point(|cache| cache.entry.id.as_str() <= after_id)
        });
        let end = if request.request.summary {
            start
        } else {
            start
                .saturating_add(request.request.limit)
                .min(matching.len())
        };
        let page = matching[start..end]
            .iter()
            .map(|cache| CacheEntryV2 {
                entry: cache.entry.clone(),
                compatibility: cache.compatibility,
            })
            .collect::<Vec<_>>();
        let next_cursor = if !request.request.summary && end < matching.len() {
            page.last().map(|entry| {
                encode_cache_list_cursor_with_prefix(
                    CACHE_LIST_V2_CURSOR_PREFIX,
                    &filter_hash,
                    &entry.entry.id,
                )
            })
        } else {
            None
        };
        Ok(CacheListV2Report {
            report_version: 2,
            cache_root: self.root.clone(),
            total_entries: entries.len(),
            matched_entries: matching.len(),
            returned_entries: page.len(),
            total_bytes,
            matched_bytes,
            active_entries,
            missing_root_entries,
            state_counts,
            compatibility_counts,
            safely_reclaimable_incompatible_entries,
            safely_reclaimable_incompatible_bytes,
            ignored_entries,
            summary_only: request.request.summary,
            next_cursor,
            entries: page,
        })
    }
}
