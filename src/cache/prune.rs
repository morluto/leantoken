impl CacheManager {
    fn prune(&self, request: &CachePruneRequest) -> Result<CachePruneReport> {
        self.prune_with_compatibility(request, false)
    }

    fn prune_v2(&self, request: &CachePruneV2Request) -> Result<CachePruneReport> {
        self.prune_with_compatibility(&request.request, request.incompatible_with_current)
    }

    fn prune_with_compatibility(
        &self,
        request: &CachePruneRequest,
        incompatible_with_current: bool,
    ) -> Result<CachePruneReport> {
        validate_prune_request(request, incompatible_with_current)?;
        let (entries, _) = self.inspect_all()?;
        let total_bytes_before = entries.iter().fold(0u64, |total, cache| {
            total.saturating_add(cache.entry.size_bytes)
        });
        let selected = select_prune_candidates(
            &entries,
            request,
            total_bytes_before,
            incompatible_with_current,
        );
        let mut reclaimed_bytes = 0u64;
        let mut results = Vec::with_capacity(entries.len());

        for cache in entries {
            let Some(mut reasons) = selected.get(&cache.entry.id).cloned() else {
                results.push(prune_result(
                    &cache,
                    CachePruneAction::Kept,
                    Vec::new(),
                    None,
                ));
                continue;
            };
            if cache.entry.active {
                results.push(prune_result(
                    &cache,
                    CachePruneAction::SkippedActive,
                    reasons,
                    None,
                ));
                continue;
            }
            if !cache.safe_to_prune {
                results.push(prune_result(
                    &cache,
                    CachePruneAction::SkippedUnsafe,
                    reasons,
                    None,
                ));
                continue;
            }
            if request.dry_run {
                reclaimed_bytes = reclaimed_bytes.saturating_add(cache.entry.size_bytes);
                results.push(prune_result(
                    &cache,
                    CachePruneAction::WouldDelete,
                    reasons,
                    None,
                ));
                continue;
            }

            let database = cache.entry.path.join(DATABASE_NAME);
            let coordination = IndexCoordination::for_database(&database);
            let _lease = match coordination.try_acquire_prune_lease() {
                Ok(Some(lease)) => lease,
                Ok(None) => {
                    reasons.push("prune_lease_unavailable".into());
                    results.push(prune_result(
                        &cache,
                        CachePruneAction::SkippedActive,
                        reasons,
                        None,
                    ));
                    continue;
                }
                Err(error) => {
                    results.push(prune_result(
                        &cache,
                        CachePruneAction::Failed,
                        reasons,
                        Some(error.to_string()),
                    ));
                    continue;
                }
            };
            let current = match self.inspect_cache(&cache.entry.id, false) {
                Ok(current) => current,
                Err(error) => {
                    results.push(prune_result(
                        &cache,
                        CachePruneAction::Failed,
                        reasons,
                        Some(error.to_string()),
                    ));
                    continue;
                }
            };
            let selected_for_compatibility = reasons
                .iter()
                .any(|reason| reason.starts_with("incompatible_with_current:"));
            if selected_for_compatibility && !current.compatibility.safely_incompatible() {
                reasons.retain(|reason| !reason.starts_with("incompatible_with_current:"));
                if reasons.is_empty() {
                    reasons.push(format!(
                        "incompatible_with_current_revalidated:{}",
                        current.compatibility.label()
                    ));
                    results.push(prune_result(
                        &current,
                        CachePruneAction::Kept,
                        reasons,
                        None,
                    ));
                    continue;
                }
            }
            if !current.safe_to_prune {
                results.push(prune_result(
                    &current,
                    CachePruneAction::SkippedUnsafe,
                    reasons,
                    None,
                ));
                continue;
            }
            if reasons.len() == 1
                && reasons[0] == "missing_repository"
                && current.entry.repository_available != Some(false)
            {
                results.push(prune_result(
                    &current,
                    CachePruneAction::Kept,
                    reasons,
                    None,
                ));
                continue;
            }

            let removal = remove_managed_artifacts(&current.entry.path);
            reclaimed_bytes = reclaimed_bytes.saturating_add(removal.reclaimed_bytes);
            match removal.error {
                None => {
                    results.push(prune_result(
                        &current,
                        CachePruneAction::Deleted,
                        reasons,
                        None,
                    ));
                }
                Some(error) => results.push(prune_result(
                    &current,
                    CachePruneAction::Failed,
                    reasons,
                    Some(error),
                )),
            }
        }

        results.sort_by(|left, right| left.id.cmp(&right.id));
        Ok(CachePruneReport {
            cache_root: self.root.clone(),
            dry_run: request.dry_run,
            total_bytes_before,
            total_bytes_after: total_bytes_before.saturating_sub(reclaimed_bytes),
            reclaimed_bytes,
            results,
        })
    }
}
