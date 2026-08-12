use super::*;

impl Indexer {
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
        let mut projections = Vec::with_capacity(seeds.len());
        for seed in seeds {
            check_cancelled(cancellation)?;
            let source_path = source_path_overrides
                .get(&seed.source_path)
                .map_or(seed.source_path.as_str(), String::as_str);
            let candidate_paths = import_candidates(source_path, &seed.raw_target);
            let resolved_path = resolve_import_candidates(&candidate_paths, repository_paths);
            projections.push(ImportProjection {
                id: seed.id,
                file_id: seed.file_id,
                resolved_path,
                candidate_paths,
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
