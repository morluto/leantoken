use super::*;

impl Indexer {
    pub(super) fn verify_or_repair_import_projections(
        &self,
        writer: &mut ReconciliationWriter<'_, '_>,
        cancellation: &CancellationToken,
        publication_changed_import_semantics: bool,
    ) -> Result<usize> {
        if !publication_changed_import_semantics
            && self.import_projections_verified.load(Ordering::Acquire)
        {
            return Ok(0);
        }
        writer.repair_import_projections(|seed, membership| {
            check_cancelled(cancellation)?;
            let go_modules = GoModuleIndex::load(membership, &self.repository_root, cancellation)?;
            let sorted_paths = sorted_indexed_paths(membership);
            Ok(import_candidates(
                &seed.source_path,
                &seed.raw_target,
                &sorted_paths,
                &go_modules,
            ))
        })
    }

    pub(super) fn mark_import_projections_verified(&self) {
        self.import_projections_verified
            .store(true, Ordering::Release);
    }

    pub(super) fn import_projections(
        &self,
        paths: &HashSet<String>,
        source_path_overrides: &HashMap<String, String>,
        repository_paths: &HashSet<String>,
        cancellation: &CancellationToken,
    ) -> Result<Vec<ImportProjection>> {
        let mut paths = paths.iter().cloned().collect::<Vec<_>>();
        paths.sort_unstable();
        let seeds = self.storage.import_seeds_for_paths(&paths)?;
        let go_modules =
            GoModuleIndex::load(repository_paths, &self.repository_root, cancellation)?;
        let sorted_paths = sorted_indexed_paths(repository_paths);
        let mut projections = Vec::with_capacity(seeds.len());
        for seed in seeds {
            check_cancelled(cancellation)?;
            let source_path = source_path_overrides
                .get(&seed.source_path)
                .map_or(seed.source_path.as_str(), String::as_str);
            let value =
                derive_import_projection(source_path, &seed.raw_target, &sorted_paths, &go_modules);
            projections.push(ImportProjection {
                id: seed.id,
                file_id: seed.file_id,
                value,
            });
        }
        Ok(projections)
    }

    pub(super) fn affected_importers(
        &self,
        deletions: &HashSet<String>,
        change_set: &ChangeSet,
        cancellation: &CancellationToken,
    ) -> Result<HashSet<String>> {
        let membership_changes = change_set.membership_changes();
        let mut affected = HashSet::new();
        for importer_path in self.storage.affected_importers(&membership_changes)? {
            check_cancelled(cancellation)?;
            if deletions.contains(&importer_path) {
                continue;
            }
            affected.insert(importer_path);
        }
        Ok(affected)
    }
}
