use super::*;

impl Indexer {
    /// Construct an indexer whose dedicated worker pool is created on demand.
    pub fn new(config: Arc<Config>, storage: Storage) -> Result<Self> {
        Self::validate_config(&config)?;
        let repository_root = Arc::new(Dir::open_ambient_dir(
            &config.root,
            cap_std::ambient_authority(),
        )?);
        let progress = IndexProgressRegistry::new(index_progress_cache_namespace(&config));
        Ok(Self {
            config,
            storage,
            pool: Arc::new(LazyWorkerPool::new()),
            repository_root,
            progress,
        })
    }

    pub(crate) fn repository_root(&self) -> Arc<Dir> {
        Arc::clone(&self.repository_root)
    }

    /// Return the latest bounded process-local initial-index progress snapshot.
    ///
    /// This read uses only the indexer's small progress registry and never
    /// acquires the SQLite writer or repository operation lock.
    #[must_use]
    pub fn progress_snapshot(&self) -> Option<IndexProgressSnapshot> {
        self.progress.snapshot()
    }

    /// Reconcile filesystem state into one committed repository generation.
    pub fn reconcile(&self, mode: IndexingMode) -> Result<IndexResponse> {
        self.reconcile_report(mode).map(IndexReport::into_response)
    }

    /// Reconcile filesystem state and include bounded preparation skip reasons.
    pub fn reconcile_report(&self, mode: IndexingMode) -> Result<IndexReport> {
        self.reconcile_cancellable_report(mode, &CancellationToken::new())
    }

    /// Reconcile a full repository and return phase diagnostics for benchmarks.
    pub fn reconcile_profiled(&self, mode: IndexingMode) -> Result<ProfiledIndexResponse> {
        self.reconcile_profiled_report(mode)
            .map(|profiled| ProfiledIndexResponse {
                response: profiled.report.into_response(),
                diagnostics: profiled.diagnostics,
            })
    }

    /// Reconcile a full repository with additive details and phase diagnostics.
    pub fn reconcile_profiled_report(&self, mode: IndexingMode) -> Result<ProfiledIndexReport> {
        self.reconcile_cancellable_profiled_report(mode, &CancellationToken::new())
    }

    /// Reconcile the repository with cooperative cancellation and stale-plan retry.
    pub fn reconcile_cancellable(
        &self,
        mode: IndexingMode,
        cancellation: &CancellationToken,
    ) -> Result<IndexResponse> {
        self.reconcile_cancellable_report(mode, cancellation)
            .map(IndexReport::into_response)
    }

    /// Reconcile with cancellation and include bounded preparation skip reasons.
    pub fn reconcile_cancellable_report(
        &self,
        mode: IndexingMode,
        cancellation: &CancellationToken,
    ) -> Result<IndexReport> {
        self.reconcile_cancellable_report_inner(mode, cancellation, StorageProfiling::Omit)
            .map(|profiled| profiled.report)
    }

    /// Reconcile a full repository with cancellation and phase diagnostics.
    pub fn reconcile_cancellable_profiled(
        &self,
        mode: IndexingMode,
        cancellation: &CancellationToken,
    ) -> Result<ProfiledIndexResponse> {
        self.reconcile_cancellable_profiled_report(mode, cancellation)
            .map(|profiled| ProfiledIndexResponse {
                response: profiled.report.into_response(),
                diagnostics: profiled.diagnostics,
            })
    }

    pub(super) fn reconcile_cancellable_profiled_report(
        &self,
        mode: IndexingMode,
        cancellation: &CancellationToken,
    ) -> Result<ProfiledIndexReport> {
        self.reconcile_cancellable_report_inner(mode, cancellation, StorageProfiling::Collect)
    }

    pub(super) fn reconcile_cancellable_report_inner(
        &self,
        mode: IndexingMode,
        cancellation: &CancellationToken,
        profiling: StorageProfiling,
    ) -> Result<ProfiledIndexReport> {
        for _ in 0..3 {
            match self.reconcile_once(mode, cancellation, profiling) {
                Err(Error::StaleReconciliation { .. }) => continue,
                result => return result,
            }
        }
        Err(Error::RetryableConflict(RetryableOperation::Reconciliation))
    }

    pub(super) fn reconcile_once(
        &self,
        mode: IndexingMode,
        cancellation: &CancellationToken,
        profiling: StorageProfiling,
    ) -> Result<ProfiledIndexReport> {
        self.reconcile_once_with_profiling_hooks(mode, cancellation, profiling, || {}, || {})
    }

    #[cfg(test)]
    pub(super) fn reconcile_once_with_preparation_hook(
        &self,
        mode: IndexingMode,
        cancellation: &CancellationToken,
        before_preparation: impl FnOnce(),
    ) -> Result<ProfiledIndexReport> {
        self.reconcile_once_with_profiling_hooks(
            mode,
            cancellation,
            StorageProfiling::Omit,
            before_preparation,
            || {},
        )
    }

    #[cfg(test)]
    pub(super) fn reconcile_once_with_post_publication_hook(
        &self,
        mode: IndexingMode,
        cancellation: &CancellationToken,
        after_publication: impl FnOnce(),
    ) -> Result<ProfiledIndexReport> {
        self.reconcile_once_with_profiling_hooks(
            mode,
            cancellation,
            StorageProfiling::Omit,
            || {},
            after_publication,
        )
    }

    pub(super) fn reconcile_once_with_profiling_hooks(
        &self,
        mode: IndexingMode,
        cancellation: &CancellationToken,
        profiling: StorageProfiling,
        before_preparation: impl FnOnce(),
        after_publication: impl FnOnce(),
    ) -> Result<ProfiledIndexReport> {
        let total_started = Instant::now();
        let process_write_before = profiling
            .is_collecting()
            .then(process_write_bytes)
            .flatten();
        check_cancelled(cancellation)?;
        let baseline = self.storage.meta()?;
        let mut progress =
            (baseline.repository_generation == 0).then(|| self.progress.start(0, cancellation));

        let discovery_started = Instant::now();
        let discovery = discover_files_with_limits_policy_filter_and_progress(
            &self.config.root,
            self.config.discovery_limits(),
            self.config.discovery_policy(),
            cancellation,
            |path| !self.config.is_database_artifact_path(path),
            |stats| {
                if let Some(progress) = &progress {
                    progress.discovered(stats.walk_entries, stats.files, stats.total_source_bytes);
                }
            },
        )?;
        let discovery_elapsed = discovery_started.elapsed();
        let discovery_stats = discovery.stats;
        tracing::debug!(
            walk_entries = discovery.stats.walk_entries,
            files = discovery.stats.files,
            total_source_bytes = discovery.stats.total_source_bytes,
            max_depth = discovery.stats.max_depth,
            "repository discovery completed"
        );
        let discovered = discovery.files;
        if let Some(progress) = &progress {
            progress.discovered(
                discovery_stats.walk_entries,
                discovery_stats.files,
                discovery_stats.total_source_bytes,
            );
            progress.phase(IndexProgressPhase::HashAndPlan);
        }
        let planning_started = Instant::now();
        check_cancelled(cancellation)?;
        let existing = self.existing_files(cancellation)?;
        let config_hash = self.config_hash();
        let force = mode.is_rebuild() || baseline.config_hash != config_hash;

        let mut repository_paths = HashSet::with_capacity(discovered.len());
        for file in &discovered {
            check_cancelled(cancellation)?;
            repository_paths.insert(file.relative_path.clone());
        }
        let mut deletions = Vec::new();
        for path in existing.keys() {
            check_cancelled(cancellation)?;
            if !repository_paths.contains(path) {
                deletions.push(path.clone());
            }
        }

        let mut unchanged = 0usize;
        let mut candidates = Vec::new();
        for file in discovered {
            check_cancelled(cancellation)?;
            // Size bounds the identity read; the indexed content hash is the
            // authority. mtime can churn across checkouts, copy tools, and
            // filesystems without changing the indexed repository view.
            if !force
                && let Some(record) = existing.get(&file.relative_path)
                && record.size_bytes == file.size_bytes
                && content_unchanged(
                    &self.repository_root,
                    &file.relative_path,
                    &record.content_hash,
                    self.config.max_file_bytes,
                )
            {
                unchanged += 1;
                continue;
            }
            candidates.push(file);
        }
        let planning_elapsed = planning_started.elapsed();

        let mut removed_paths = deletions.into_iter().collect::<HashSet<_>>();
        let mut source_bytes = PublishedSourceBytes::new(
            &existing,
            &removed_paths,
            self.config.max_total_source_bytes,
        );
        let mut warnings = Vec::new();
        let mut skip_reasons = IndexSkipReasonCounts::default();
        let mut files_indexed = 0usize;
        before_preparation();
        if let Some(progress) = &progress {
            progress.phase(IndexProgressPhase::Preparation);
        }

        // Phase 1: Preparation runs outside BEGIN IMMEDIATE so the SQLite
        // writer lock is not held during filesystem reads, hashing, parsing,
        // tokenization, or import resolution. Prepared records are flushed
        // from each bounded batch into a storage-owned SQLite stage.
        let mut staged = PreparedReconciliation::new(
            &self.storage,
            self.config.tokenizer.name(),
            &baseline,
            &config_hash,
            mode,
            profiling,
        )?;
        const MAX_INITIAL_REMOVALS_PER_STAGE: usize = 256;
        for path in &removed_paths {
            staged.stage_removal(path.clone());
            if staged.pending_removals() >= MAX_INITIAL_REMOVALS_PER_STAGE {
                staged.flush()?;
            }
        }
        let preparation = self.prepare_candidate_batches_with_progress(
            &candidates,
            cancellation,
            profiling,
            || {
                if let Some(progress) = &progress {
                    progress.phase(IndexProgressPhase::Preparation);
                }
            },
            |prepared| {
                if let Some(progress) = &progress {
                    progress.prepared_batch(prepared.len());
                    progress.phase(IndexProgressPhase::RelationalWrite);
                }
                let mut indexed = Vec::with_capacity(prepared.len());
                let mut source_token_counts = HashMap::with_capacity(prepared.len());
                for result in prepared {
                    check_cancelled(cancellation)?;
                    match result {
                        PreparedFile::Indexed(file, source_token_count, warning) => {
                            source_bytes.replace(&file.path, file.size_bytes);
                            source_token_counts.insert(file.path.clone(), source_token_count);
                            indexed.push(*file);
                            if let Some(warning) = warning {
                                push_warning(&mut warnings, warning);
                            }
                        }
                        PreparedFile::Binary(path) => {
                            source_bytes.remove(&path);
                            skip_reasons.binary = skip_reasons.binary.saturating_add(1);
                            if existing.contains_key(&path) && removed_paths.insert(path.clone()) {
                                staged.stage_removal(path);
                            }
                        }
                        PreparedFile::Oversized(path) => {
                            source_bytes.remove(&path);
                            skip_reasons.oversized_during_read =
                                skip_reasons.oversized_during_read.saturating_add(1);
                            if existing.contains_key(&path) && removed_paths.insert(path.clone()) {
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
                let staged_files = indexed.len();
                files_indexed = files_indexed.saturating_add(indexed.len());
                for file in indexed {
                    check_cancelled(cancellation)?;
                    let source_token_count = source_token_counts
                        .remove(&file.path)
                        .expect("prepared file has a source token count");
                    staged.stage_indexed(file, source_token_count);
                }
                if let Some(progress) = &progress {
                    progress.staged(staged_files);
                }
                staged.flush()?;
                Ok(())
            },
        )?;
        source_bytes.enforce()?;
        let staged = staged.finish()?;
        let staging = staged.diagnostics();
        check_cancelled(cancellation)?;

        // Phase 2: Publication inside BEGIN IMMEDIATE performs only fast
        // DELETE + INSERT operations via staged.apply.  The transaction
        // rechecks the baseline generation and config_hash before applying.
        let publication_started = Instant::now();
        let publish = |writer: &mut ReconciliationWriter<'_, '_>| {
            staged.apply(writer)?;
            Ok(preparation)
        };
        let observe_publication =
            |phase| observe_publication_phase(progress.as_ref(), cancellation, phase);
        let (generation, preparation, mut publication_detail) = if profiling.is_collecting() {
            self.storage
                .publish_reconciliation_profiled_at_with_progress(
                    &baseline,
                    &config_hash,
                    mode,
                    observe_publication,
                    publish,
                )?
        } else {
            let (generation, preparation) = self.storage.publish_reconciliation_at_with_progress(
                &baseline,
                &config_hash,
                mode,
                observe_publication,
                publish,
            )?;
            (generation, preparation, PublicationDiagnostics::default())
        };
        after_publication();
        if let Some(progress) = &mut progress {
            progress.complete(generation);
        }
        publication_detail.stage_write_ms = staging.write_ms;
        publication_detail.stage_write_bytes = staging.write_bytes;
        publication_detail.stage_database_bytes = staging.database_bytes;
        let publication_elapsed = publication_started.elapsed();

        // Storage returns only after the generation transaction commits. A
        // cancellation observed after this commit point cannot roll publication
        // back and must not turn committed success into a `Cancelled` outcome.
        let files_seen = unchanged + candidates.len();
        let files_removed = removed_paths.len();
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
        let report = IndexReport::with_skip_reasons(response, skip_reasons);
        let process_write_after = profiling
            .is_collecting()
            .then(process_write_bytes)
            .flatten();
        let process_write_bytes = process_write_before
            .zip(process_write_after)
            .map(|(before, after)| after.saturating_sub(before));
        let storage_phase_write_bytes = publication_detail.measured_write_bytes();
        let unattributed_process_write_bytes = process_write_bytes
            .zip(storage_phase_write_bytes)
            .map(|(total, attributed)| total.saturating_sub(attributed));
        let diagnostics = IndexingDiagnostics {
            total_ms: duration_ms(total_started.elapsed()),
            discovery_ms: duration_ms(discovery_elapsed),
            hash_and_plan_ms: duration_ms(planning_elapsed),
            preparation_ms: duration_ms(preparation.preparation),
            preparation_detail: preparation.detail.report(),
            preparation_by_language: preparation
                .detail_by_language
                .iter()
                .map(|(language, detail)| (language.clone(), detail.report()))
                .collect(),
            insertion_ms: duration_ms(preparation.insertion),
            publication_ms: duration_ms(publication_elapsed),
            publication_detail,
            process_write_bytes,
            storage_phase_write_bytes,
            unattributed_process_write_bytes,
            generation_before: baseline.repository_generation,
            generation_after: generation,
            generation_published: generation != baseline.repository_generation,
            preparation_batches: preparation.batches,
            max_batch_files: preparation.max_batch_files,
            max_batch_source_bytes: preparation.max_batch_source_bytes,
            walk_entries: discovery_stats.walk_entries,
            discovered_files: discovery_stats.files,
            discovered_source_bytes: discovery_stats.total_source_bytes,
        };
        tracing::debug!(
            total_ms = diagnostics.total_ms,
            discovery_ms = diagnostics.discovery_ms,
            hash_and_plan_ms = diagnostics.hash_and_plan_ms,
            preparation_ms = diagnostics.preparation_ms,
            insertion_ms = diagnostics.insertion_ms,
            publication_ms = diagnostics.publication_ms,
            preparation_batches = diagnostics.preparation_batches,
            max_batch_files = diagnostics.max_batch_files,
            max_batch_source_bytes = diagnostics.max_batch_source_bytes,
            "repository reconciliation profile"
        );
        Ok(ProfiledIndexReport {
            report,
            diagnostics,
        })
    }
}

pub(super) fn observe_publication_phase(
    progress: Option<&IndexProgressAttempt>,
    cancellation: &CancellationToken,
    phase: ReconciliationPublicationPhase,
) -> Result<()> {
    if let Some(progress) = progress {
        progress.phase(match phase {
            ReconciliationPublicationPhase::ChunkWordFts => IndexProgressPhase::ChunkWordFts,
            ReconciliationPublicationPhase::ChunkTrigramFts => IndexProgressPhase::ChunkTrigramFts,
            ReconciliationPublicationPhase::SymbolFts => IndexProgressPhase::SymbolFts,
            ReconciliationPublicationPhase::ReferenceFts => IndexProgressPhase::ReferenceFts,
            ReconciliationPublicationPhase::CommitAndCheckpoint => {
                IndexProgressPhase::CommitAndCheckpoint
            }
        });
    }
    check_cancelled(cancellation)
}
