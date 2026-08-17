use super::*;

impl CacheManager {
    pub(super) fn prune(&self, request: &CachePruneRequest) -> Result<CachePruneReport> {
        let plan = CachePrunePlan::try_from(request)?;
        let (entries, _) = self.inspect_all()?;
        let total_bytes_before = entries.iter().fold(0u64, |total, cache| {
            total.saturating_add(cache.entry.size_bytes)
        });
        let selected = select_prune_candidates(&entries, &plan, total_bytes_before);
        let mut reclaimed_bytes = 0u64;
        let mut results = Vec::with_capacity(entries.len());

        for cache in entries {
            let Some(mut reasons) = selected.get(&cache.entry.id).cloned() else {
                results.push(prune_result(&cache, CachePruneOutcome::Kept, Vec::new()));
                continue;
            };
            if cache.entry.active {
                results.push(prune_result(
                    &cache,
                    CachePruneOutcome::skipped_active(),
                    reasons,
                ));
                continue;
            }
            if !cache.safe_to_prune {
                results.push(prune_result(
                    &cache,
                    CachePruneOutcome::skipped_unsafe(cache.entry.detail.clone()),
                    reasons,
                ));
                continue;
            }
            if plan.execution.is_dry_run() {
                reclaimed_bytes = reclaimed_bytes.saturating_add(cache.entry.size_bytes);
                results.push(prune_result(
                    &cache,
                    CachePruneOutcome::WouldDelete,
                    reasons,
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
                        CachePruneOutcome::skipped_active(),
                        reasons,
                    ));
                    continue;
                }
                Err(error) => {
                    results.push(prune_result(
                        &cache,
                        CachePruneOutcome::Failed {
                            error: error.to_string(),
                        },
                        reasons,
                    ));
                    continue;
                }
            };
            let current =
                match self.inspect_managed_cache(&cache.entry.id, cache.identity.clone(), false) {
                    Ok(current) => current,
                    Err(error) => {
                        results.push(prune_result(
                            &cache,
                            CachePruneOutcome::Failed {
                                error: error.to_string(),
                            },
                            reasons,
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
                    results.push(prune_result(&current, CachePruneOutcome::Kept, reasons));
                    continue;
                }
            }
            if !current.safe_to_prune {
                results.push(prune_result(
                    &current,
                    CachePruneOutcome::skipped_unsafe(current.entry.detail.clone()),
                    reasons,
                ));
                continue;
            }
            // Re-check older_than eligibility using the fresh post-lease inspection.
            if let Some(older_than_days) = plan.older_than_days
                && current
                    .entry
                    .age_seconds
                    .is_some_and(|age| age < older_than_days.get().saturating_mul(SECONDS_PER_DAY))
            {
                reasons.retain(|reason| reason != "older_than");
                if reasons.is_empty() {
                    reasons.push("older_than_revalidated_young".to_string());
                    results.push(prune_result(&current, CachePruneOutcome::Kept, reasons));
                    continue;
                }
            }
            if reasons.len() == 1
                && reasons[0] == "missing_repository"
                && current.entry.repository_available != Some(false)
            {
                results.push(prune_result(&current, CachePruneOutcome::Kept, reasons));
                continue;
            }

            let removal = remove_managed_artifacts(&current.entry.path);
            reclaimed_bytes = reclaimed_bytes.saturating_add(removal.reclaimed_bytes);
            match removal.error {
                None => {
                    results.push(prune_result(&current, CachePruneOutcome::Deleted, reasons));
                }
                Some(error) => results.push(prune_result(
                    &current,
                    CachePruneOutcome::Failed { error },
                    reasons,
                )),
            }
        }

        results.sort_by(|left, right| left.id.cmp(&right.id));
        Ok(CachePruneReport {
            cache_root: self.root.clone(),
            dry_run: plan.execution.is_dry_run(),
            total_bytes_before,
            total_bytes_after: total_bytes_before.saturating_sub(reclaimed_bytes),
            reclaimed_bytes,
            results,
        })
    }
}
