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

    pub(crate) fn progress_snapshot(&self) -> Option<IndexProgressSnapshot> {
        self.progress.snapshot()
    }

    /// Reconcile filesystem state into one committed repository generation.
    pub fn reconcile(&self, rebuild: bool) -> Result<IndexResponse> {
        self.reconcile_report(rebuild)
            .map(IndexReport::into_response)
    }

    /// Reconcile filesystem state and include bounded preparation skip reasons.
    pub fn reconcile_report(&self, rebuild: bool) -> Result<IndexReport> {
        self.reconcile_cancellable_report(rebuild, &CancellationToken::new())
    }

    /// Reconcile a full repository and return phase diagnostics for benchmarks.
    pub fn reconcile_profiled(&self, rebuild: bool) -> Result<ProfiledIndexResponse> {
        self.reconcile_profiled_report(rebuild)
            .map(|profiled| ProfiledIndexResponse {
                response: profiled.report.into_response(),
                diagnostics: profiled.diagnostics,
            })
    }

    /// Reconcile a full repository with additive details and phase diagnostics.
    pub fn reconcile_profiled_report(&self, rebuild: bool) -> Result<ProfiledIndexReport> {
        self.reconcile_cancellable_profiled_report(rebuild, &CancellationToken::new())
    }

    /// Reconcile the repository with cooperative cancellation and stale-plan retry.
    pub fn reconcile_cancellable(
        &self,
        rebuild: bool,
        cancellation: &CancellationToken,
    ) -> Result<IndexResponse> {
        self.reconcile_cancellable_report(rebuild, cancellation)
            .map(IndexReport::into_response)
    }

    /// Reconcile with cancellation and include bounded preparation skip reasons.
    pub fn reconcile_cancellable_report(
        &self,
        rebuild: bool,
        cancellation: &CancellationToken,
    ) -> Result<IndexReport> {
        self.reconcile_cancellable_report_inner(rebuild, cancellation, StorageProfiling::Omit)
            .map(|profiled| profiled.report)
    }

    /// Reconcile a full repository with cancellation and phase diagnostics.
    pub fn reconcile_cancellable_profiled(
        &self,
        rebuild: bool,
        cancellation: &CancellationToken,
    ) -> Result<ProfiledIndexResponse> {
        self.reconcile_cancellable_profiled_report(rebuild, cancellation)
            .map(|profiled| ProfiledIndexResponse {
                response: profiled.report.into_response(),
                diagnostics: profiled.diagnostics,
            })
    }

    fn reconcile_cancellable_profiled_report(
        &self,
        rebuild: bool,
        cancellation: &CancellationToken,
    ) -> Result<ProfiledIndexReport> {
        self.reconcile_cancellable_report_inner(rebuild, cancellation, StorageProfiling::Collect)
    }

    fn reconcile_cancellable_report_inner(
        &self,
        rebuild: bool,
        cancellation: &CancellationToken,
        profiling: StorageProfiling,
    ) -> Result<ProfiledIndexReport> {
        for _ in 0..3 {
            match self.reconcile_once(rebuild, cancellation, profiling) {
                Err(Error::StaleReconciliation { .. }) => continue,
                result => return result,
            }
        }
        Err(Error::RetryableConflict(RetryableOperation::Reconciliation))
    }

    fn reconcile_once(
        &self,
        rebuild: bool,
        cancellation: &CancellationToken,
        profiling: StorageProfiling,
    ) -> Result<ProfiledIndexReport> {
        self.reconcile_once_with_profiling_hook(rebuild, cancellation, profiling, || {})
    }

    #[cfg(test)]
    fn reconcile_once_with_preparation_hook(
        &self,
        rebuild: bool,
        cancellation: &CancellationToken,
        before_preparation: impl FnOnce(),
    ) -> Result<ProfiledIndexReport> {
        self.reconcile_once_with_profiling_hook(
            rebuild,
            cancellation,
            StorageProfiling::Omit,
            before_preparation,
        )
    }

    fn reconcile_once_with_profiling_hook(
        &self,
        rebuild: bool,
        cancellation: &CancellationToken,
        profiling: StorageProfiling,
        before_preparation: impl FnOnce(),
    ) -> Result<ProfiledIndexReport> {
        let total_started = Instant::now();
        check_cancelled(cancellation)?;
        let baseline = self.storage.meta()?;
        let mut progress = (baseline.repository_generation == 0)
            .then(|| self.progress.start(0, cancellation));

        let discovery_started = Instant::now();
        let discovery = discover_files_with_limits_policy_filter_and_progress(
            &self.config.root,
            self.config.discovery_limits(),
            self.config.discovery_policy(),
            cancellation,
            |path| !self.config.is_database_artifact_path(path),
            |stats| {
                if let Some(progress) = &progress {
                    progress.discovered(
                        stats.walk_entries,
                        stats.files,
                        stats.total_source_bytes,
                    );
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
        let force = rebuild || baseline.config_hash != config_hash;

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
            // mtime+size alone cannot prove content identity (bind mounts, copy
            // tools that preserve mtime, some network filesystems). Content-hash
            // before skipping so silent overwrites still reindex.
            if !force
                && let Some(record) = existing.get(&file.relative_path)
                && record.size_bytes == file.size_bytes
                && record.modified_ns == file.modified_ns
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
        let publication_started = Instant::now();
        let publish = |writer: &mut ReconciliationWriter<'_, '_>| {
            for path in &removed_paths {
                writer.delete(path)?;
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
                                if existing.contains_key(&path)
                                    && removed_paths.insert(path.clone())
                                {
                                    writer.delete(&path)?;
                                }
                            }
                            PreparedFile::Oversized(path) => {
                                source_bytes.remove(&path);
                                skip_reasons.oversized_during_read =
                                    skip_reasons.oversized_during_read.saturating_add(1);
                                if existing.contains_key(&path)
                                    && removed_paths.insert(path.clone())
                                {
                                    writer.delete(&path)?;
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
                        writer.replace_with_source_tokens(
                            file,
                            self.config.tokenizer.name(),
                            source_token_count,
                        )?;
                    }
                    if let Some(progress) = &progress {
                        progress.staged(staged_files);
                    }
                    Ok(())
                },
            )?;
            source_bytes.enforce()?;
            Ok(preparation)
        };
        let observe_publication = |phase| {
            let Some(progress) = &progress else {
                return;
            };
            progress.phase(match phase {
                ReconciliationPublicationPhase::ChunkWordFts => {
                    IndexProgressPhase::ChunkWordFts
                }
                ReconciliationPublicationPhase::ChunkTrigramFts => {
                    IndexProgressPhase::ChunkTrigramFts
                }
                ReconciliationPublicationPhase::SymbolFts => IndexProgressPhase::SymbolFts,
                ReconciliationPublicationPhase::ReferenceFts => IndexProgressPhase::ReferenceFts,
                ReconciliationPublicationPhase::CommitAndCheckpoint => {
                    IndexProgressPhase::CommitAndCheckpoint
                }
            });
        };
        let (generation, preparation, mut publication_detail) =
            if profiling == StorageProfiling::Collect {
                self.storage
                    .publish_reconciliation_profiled_at_with_progress(
                    &baseline,
                    &config_hash,
                    rebuild,
                    observe_publication,
                    publish,
                )?
            } else {
                let (generation, preparation) =
                    self.storage.publish_reconciliation_at_with_progress(
                        &baseline,
                        &config_hash,
                        rebuild,
                        observe_publication,
                        publish,
                    )?;
                (generation, preparation, PublicationDiagnostics::default())
            };
        if let Some(progress) = &mut progress {
            progress.complete(generation);
        }
        publication_detail.relational_write_ms = duration_ms(preparation.insertion);
        publication_detail.relational_write_bytes = preparation.insertion_write_bytes;
        let publication_elapsed = publication_started.elapsed();

        check_cancelled(cancellation)?;
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
        let diagnostics = IndexingDiagnostics {
            total_ms: duration_ms(total_started.elapsed()),
            discovery_ms: duration_ms(discovery_elapsed),
            hash_and_plan_ms: duration_ms(planning_elapsed),
            preparation_ms: duration_ms(preparation.preparation),
            preparation_detail: preparation.detail.report(),
            insertion_ms: duration_ms(preparation.insertion),
            publication_ms: duration_ms(publication_elapsed),
            publication_detail,
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
