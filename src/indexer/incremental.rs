use super::*;

enum VisibilityObservation {
    Stable,
    Changed { observed_deletions: HashSet<String> },
}

impl Indexer {
    /// Reconcile watcher-reported paths without walking the full repository.
    ///
    /// Existing regular files and deletions are safe to apply directly. New
    /// paths, directories, symlinks, and ignore-rule changes fall back to a
    /// full reconciliation because they can affect files beyond the reported
    /// path.
    pub fn reconcile_paths(&self, paths: &[String]) -> Result<IndexResponse> {
        self.reconcile_paths_report(paths)
            .map(IndexReport::into_response)
    }

    /// Reconcile watcher paths and include bounded preparation skip reasons.
    pub fn reconcile_paths_report(&self, paths: &[String]) -> Result<IndexReport> {
        self.reconcile_paths_cancellable_report(paths, &CancellationToken::new())
    }

    /// Reconcile watcher paths with cooperative cancellation and stale-plan retry.
    pub fn reconcile_paths_cancellable(
        &self,
        paths: &[String],
        cancellation: &CancellationToken,
    ) -> Result<IndexResponse> {
        self.reconcile_paths_cancellable_report(paths, cancellation)
            .map(IndexReport::into_response)
    }

    /// Reconcile watcher paths with cancellation and preparation skip reasons.
    pub fn reconcile_paths_cancellable_report(
        &self,
        paths: &[String],
        cancellation: &CancellationToken,
    ) -> Result<IndexReport> {
        for _ in 0..3 {
            match self.reconcile_paths_once(paths, cancellation) {
                Err(Error::StaleReconciliation { .. }) => continue,
                result => return result,
            }
        }
        Err(Error::RetryableConflict(RetryableOperation::Reconciliation))
    }

    pub(super) fn reconcile_paths_once(
        &self,
        paths: &[String],
        cancellation: &CancellationToken,
    ) -> Result<IndexReport> {
        self.reconcile_paths_once_with_preparation_hook(paths, cancellation, || {})
    }

    pub(super) fn reconcile_paths_once_with_preparation_hook(
        &self,
        paths: &[String],
        cancellation: &CancellationToken,
        before_preparation: impl FnOnce(),
    ) -> Result<IndexReport> {
        self.reconcile_paths_once_with_all_hooks(
            paths,
            cancellation,
            || {},
            before_preparation,
            || {},
        )
    }

    #[cfg(test)]
    pub(super) fn reconcile_paths_once_with_post_publication_hook(
        &self,
        paths: &[String],
        cancellation: &CancellationToken,
        after_publication: impl FnOnce(),
    ) -> Result<IndexReport> {
        self.reconcile_paths_once_with_all_hooks(
            paths,
            cancellation,
            || {},
            || {},
            after_publication,
        )
    }

    fn observe_visibility_delta(
        &self,
        paths: &[String],
        existing: &HashMap<String, crate::storage::FileRecord>,
    ) -> VisibilityObservation {
        let mut visibility_delta = false;
        let mut observed_deletions = HashSet::new();
        for requested in paths {
            let relative = Path::new(requested);
            let relative_path = slash_path(relative);
            visibility_delta |= self
                .config
                .discovery_policy()
                .is_ignore_control_path(&relative_path);
            let absolute = self.config.root.join(relative);
            match fs::symlink_metadata(&absolute) {
                Ok(metadata) => {
                    let is_file = metadata.file_type().is_file();
                    visibility_delta |= !existing.contains_key(&relative_path) || !is_file;
                    if !is_file {
                        if existing.contains_key(&relative_path) {
                            observed_deletions.insert(relative_path.clone());
                        }
                        if !metadata.file_type().is_dir() {
                            let prefix = format!("{relative_path}/");
                            observed_deletions.extend(
                                existing
                                    .keys()
                                    .filter(|path| path.starts_with(&prefix))
                                    .cloned(),
                            );
                        }
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    let prefix = format!("{relative_path}/");
                    let observed = existing
                        .keys()
                        .filter(|path| *path == &relative_path || path.starts_with(&prefix))
                        .cloned()
                        .collect::<Vec<_>>();
                    visibility_delta |= observed.iter().any(|path| path.starts_with(&prefix));
                    observed_deletions.extend(observed);
                }
                Err(_) => {}
            }
        }
        if visibility_delta {
            VisibilityObservation::Changed { observed_deletions }
        } else {
            VisibilityObservation::Stable
        }
    }

    #[cfg(test)]
    pub(super) fn reconcile_paths_once_with_hooks(
        &self,
        paths: &[String],
        cancellation: &CancellationToken,
        after_discovery: impl FnOnce(),
        before_preparation: impl FnOnce(),
    ) -> Result<IndexReport> {
        self.reconcile_paths_once_with_all_hooks(
            paths,
            cancellation,
            after_discovery,
            before_preparation,
            || {},
        )
    }

    fn reconcile_paths_once_with_all_hooks(
        &self,
        paths: &[String],
        cancellation: &CancellationToken,
        after_discovery: impl FnOnce(),
        before_preparation: impl FnOnce(),
        after_publication: impl FnOnce(),
    ) -> Result<IndexReport> {
        check_cancelled(cancellation)?;
        let baseline = self.storage.meta()?;
        let config_hash = self.config_hash();
        if baseline.config_hash != config_hash
            || baseline.derivation_fingerprint
                != crate::index_derivation::index_derivation_fingerprint()
        {
            return self.reconcile_cancellable_report(IndexingMode::Rebuild, cancellation);
        }

        let existing = self.existing_files(cancellation)?;
        let mut repository_paths = HashSet::with_capacity(existing.len());
        for path in existing.keys() {
            check_cancelled(cancellation)?;
            repository_paths.insert(path.clone());
        }
        let mut unique = HashSet::with_capacity(paths.len());
        for path in paths {
            check_cancelled(cancellation)?;
            unique.insert(slash_path(&validate_relative(path)?));
        }
        let mut paths = unique.drain().collect::<Vec<_>>();
        check_cancelled(cancellation)?;
        paths.sort_unstable();
        check_cancelled(cancellation)?;

        // Preserve targeted deletion evidence from the observation that triggers discovery.
        let (discovered, visibility_observed_deletions) =
            match self.observe_visibility_delta(&paths, &existing) {
                VisibilityObservation::Stable => (None, HashSet::new()),
                VisibilityObservation::Changed { observed_deletions } => (
                    Some(
                        discover_files_with_limits_policy_and_filter(
                            &self.config.root,
                            self.config.discovery_limits(),
                            self.config.discovery_policy(),
                            cancellation,
                            |path| !self.config.is_database_artifact_path(path),
                        )
                        .map(|discovery| discovery.files)?,
                    ),
                    observed_deletions,
                ),
            };
        let discovered_by_path = discovered.as_ref().map(|files| {
            files
                .iter()
                .cloned()
                .map(|file| (file.relative_path.clone(), file))
                .collect::<HashMap<_, _>>()
        });
        after_discovery();

        let mut candidates = HashMap::new();
        let mut deletions = HashSet::new();
        let mut directly_observed_deletions = visibility_observed_deletions;
        let mut unchanged = 0usize;
        if let Some(discovered) = &discovered_by_path {
            for (path, file) in discovered {
                check_cancelled(cancellation)?;
                if !existing.contains_key(path) || paths.binary_search(path).is_ok() {
                    candidates.insert(path.clone(), file.clone());
                }
            }
            for path in existing.keys() {
                check_cancelled(cancellation)?;
                if !discovered.contains_key(path) {
                    deletions.insert(path.clone());
                }
            }
        } else {
            let discovery_policy = self.config.discovery_policy();
            for requested in &paths {
                check_cancelled(cancellation)?;
                let relative = validate_relative(requested)?;
                let relative_path = slash_path(&relative);
                enforce_limit(
                    crate::IndexLimitKind::Depth,
                    u64::try_from(relative.components().count()).unwrap_or(u64::MAX),
                    u64::try_from(self.config.max_depth).unwrap_or(u64::MAX),
                )?;
                let absolute_path = self.config.root.join(&relative);
                if self.config.is_database_artifact_path(&absolute_path) {
                    if existing.contains_key(&relative_path) {
                        directly_observed_deletions.insert(relative_path.clone());
                        deletions.insert(relative_path);
                    }
                    continue;
                }
                let metadata = match fs::symlink_metadata(&absolute_path) {
                    Ok(metadata) => metadata,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                        if existing.contains_key(&relative_path) {
                            directly_observed_deletions.insert(relative_path.clone());
                            deletions.insert(relative_path);
                        }
                        continue;
                    }
                    Err(error) => return Err(error.into()),
                };
                if !discovery_policy.includes_path(&relative_path, metadata.file_type().is_dir()) {
                    if existing.contains_key(&relative_path) {
                        directly_observed_deletions.insert(relative_path.clone());
                        deletions.insert(relative_path);
                    }
                    continue;
                }
                if metadata.len() > self.config.max_file_bytes {
                    deletions.insert(relative_path);
                    continue;
                }
                let modified_ns = metadata
                    .modified()
                    .ok()
                    .and_then(|value| value.duration_since(UNIX_EPOCH).ok())
                    .map(|duration| duration.as_nanos());
                candidates.insert(
                    relative_path.clone(),
                    DiscoveredFile {
                        absolute_path,
                        relative_path,
                        size_bytes: metadata.len(),
                        modified_ns,
                    },
                );
            }
        }

        unchanged = unchanged.saturating_add(self.remove_content_stable_candidates(
            &existing,
            &mut candidates,
            cancellation,
        )?);

        let change_set = ChangeSet::classify(&existing, &candidates, &deletions);
        debug_assert_eq!(
            change_set.modified.len() + change_set.created.len(),
            candidates.len()
        );
        for deletion in &change_set.deleted {
            repository_paths.remove(deletion);
        }
        repository_paths.extend(candidates.keys().cloned());
        let relocations =
            self.plan_relocations(&existing, &candidates, &change_set, cancellation)?;
        let affected_importers = self.affected_importers(&deletions, &change_set, cancellation)?;
        self.validate_membership_limits(&existing, &candidates, &deletions, cancellation)?;
        directly_observed_deletions.retain(|path| deletions.contains(path));
        debug_assert!(directly_observed_deletions.is_subset(&deletions));

        let files_seen = unchanged
            .saturating_add(candidates.len())
            .saturating_add(directly_observed_deletions.len());
        let relocation_old_paths = relocations
            .iter()
            .map(|relocation| relocation.old_path.clone())
            .collect::<HashSet<_>>();
        let relocation_new_paths = relocations
            .iter()
            .map(|relocation| relocation.new_file.relative_path.clone())
            .collect::<HashSet<_>>();
        let mut import_refresh_paths = affected_importers;
        import_refresh_paths.extend(relocation_old_paths.iter().cloned());
        let source_path_overrides = relocations
            .iter()
            .map(|relocation| {
                (
                    relocation.old_path.clone(),
                    relocation.new_file.relative_path.clone(),
                )
            })
            .collect::<HashMap<_, _>>();
        let import_projections = self.import_projections(
            &import_refresh_paths,
            &source_path_overrides,
            &repository_paths,
            cancellation,
        )?;
        let mut updated_paths = import_refresh_paths
            .iter()
            .filter(|path| !deletions.contains(*path))
            .cloned()
            .collect::<HashSet<_>>();
        for relocation in &relocations {
            updated_paths.remove(&relocation.old_path);
            updated_paths.insert(relocation.new_file.relative_path.clone());
        }
        let candidates = candidates
            .into_iter()
            .filter_map(|(path, file)| (!relocation_new_paths.contains(&path)).then_some(file))
            .collect::<Vec<_>>();
        let mut source_bytes =
            PublishedSourceBytes::new(&existing, &deletions, self.config.max_total_source_bytes);
        for relocation in &relocations {
            source_bytes.replace(
                &relocation.new_file.relative_path,
                relocation.new_file.size_bytes,
            );
        }
        let mut warnings = Vec::new();
        let mut skip_reasons = IndexSkipReasonCounts::default();
        before_preparation();

        // Phase 1: Preparation runs outside BEGIN IMMEDIATE so the SQLite
        // writer lock is not held during filesystem reads, hashing, parsing,
        // tokenization, or import resolution. Prepared records are flushed
        // from each bounded batch into a storage-owned SQLite stage.
        let mut staged = PreparedReconciliation::new(
            &self.storage,
            self.config.tokenizer.name(),
            &baseline,
            &config_hash,
            IndexingMode::Reconcile,
            StorageProfiling::Omit,
        )?;
        let preparation = self.prepare_candidate_batches(
            &candidates,
            cancellation,
            StorageProfiling::Omit,
            |prepared| {
                let mut indexed = Vec::with_capacity(prepared.len());
                let mut source_token_counts = HashMap::with_capacity(prepared.len());
                for result in prepared {
                    check_cancelled(cancellation)?;
                    match result {
                        PreparedFile::Indexed(file, source_token_count, warning) => {
                            source_bytes.replace(&file.path, file.size_bytes);
                            let same = existing.get(&file.path).is_some_and(|record| {
                                record.content_hash == file.content_hash
                                    && record.size_bytes == file.size_bytes
                            });
                            if same {
                                unchanged += 1;
                                continue;
                            }
                            source_token_counts.insert(file.path.clone(), source_token_count);
                            indexed.push(*file);
                            if let Some(warning) = warning {
                                push_warning(&mut warnings, warning);
                            }
                        }
                        PreparedFile::Binary(path) => {
                            source_bytes.remove(&path);
                            skip_reasons.binary = skip_reasons.binary.saturating_add(1);
                            if existing.contains_key(&path) && deletions.insert(path.clone()) {
                                staged.stage_removal(path);
                            }
                        }
                        PreparedFile::Oversized(path) => {
                            source_bytes.remove(&path);
                            skip_reasons.oversized_during_read =
                                skip_reasons.oversized_during_read.saturating_add(1);
                            if existing.contains_key(&path) && deletions.insert(path.clone()) {
                                staged.stage_removal(path);
                            }
                        }
                        PreparedFile::Failed(path, error) => {
                            skip_reasons.failed = skip_reasons.failed.saturating_add(1);
                            push_warning(&mut warnings, format!("{path}: {error}"));
                        }
                    }
                }
                resolve_imports(&mut indexed, &repository_paths, cancellation)?;
                for file in indexed {
                    check_cancelled(cancellation)?;
                    updated_paths.insert(file.path.clone());
                    let source_token_count = source_token_counts
                        .remove(&file.path)
                        .expect("prepared file has a source token count");
                    staged.stage_indexed(file, source_token_count);
                }
                staged.flush()?;
                Ok(())
            },
        )?;
        source_bytes.enforce()?;
        let staged = staged.finish()?;
        check_cancelled(cancellation)?;
        let publication_changed_import_semantics =
            !change_set.created.is_empty() || !deletions.is_empty();

        // Phase 2: Publication inside BEGIN IMMEDIATE.  Relocations and import
        // projection refresh remain inside the transaction because they
        // require live table state.  staged.apply performs only fast
        // DELETE + INSERT operations for the prepared files.
        let (generation, (_preparation, repaired_imports)) =
            self.storage.publish_reconciliation_at(
                &baseline,
                &config_hash,
                IndexingMode::Reconcile,
                |writer| {
                    for path in &deletions {
                        if relocation_old_paths.contains(path) {
                            continue;
                        }
                        writer.delete(path)?;
                    }
                    for relocation in &relocations {
                        writer.relocate(
                            &relocation.old_path,
                            &relocation.new_file.relative_path,
                            relocation.new_file.size_bytes,
                            relocation.new_file.modified_ns,
                            &relocation.expected_hash,
                        )?;
                    }
                    writer.refresh_import_projections(&import_projections)?;
                    staged.apply(writer)?;
                    let repaired_imports = self.verify_or_repair_import_projections(
                        writer,
                        cancellation,
                        publication_changed_import_semantics,
                    )?;
                    Ok((preparation, repaired_imports))
                },
            )?;
        self.mark_import_projections_verified();
        if repaired_imports > 0 {
            push_warning(
                &mut warnings,
                format!("repaired {repaired_imports} persisted import projections"),
            );
        }
        after_publication();
        let files_removed = deletions.len();
        let files_indexed = updated_paths.len();
        let files_skipped = skip_reasons.total();

        let response = IndexResponse {
            repository_generation: generation,
            files_seen,
            files_indexed,
            files_unchanged: unchanged,
            files_removed,
            files_skipped,
            warnings,
        };
        Ok(IndexReport::with_skip_reasons(response, skip_reasons))
    }
}
