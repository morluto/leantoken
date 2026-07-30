use super::*;

impl Indexer {
    pub(super) fn validate_config(config: &Config) -> Result<()> {
        config.validate()
    }

    pub(super) fn prepare_candidate_batches(
        &self,
        candidates: &[DiscoveredFile],
        cancellation: &CancellationToken,
        profiling: StorageProfiling,
        consume: impl FnMut(Vec<PreparedFile>) -> Result<()>,
    ) -> Result<PreparationMetrics> {
        self.prepare_candidate_batches_with_progress(
            candidates,
            cancellation,
            profiling,
            || {},
            consume,
        )
    }

    pub(super) fn prepare_candidate_batches_with_progress(
        &self,
        candidates: &[DiscoveredFile],
        cancellation: &CancellationToken,
        profiling: StorageProfiling,
        mut before_batch: impl FnMut(),
        mut consume: impl FnMut(Vec<PreparedFile>) -> Result<()>,
    ) -> Result<PreparationMetrics> {
        check_cancelled(cancellation)?;
        if candidates.is_empty() {
            return Ok(PreparationMetrics::default());
        }

        // One lazy pool per Services/cache preserves that instance's
        // configured worker bound without allocating threads in followers.
        let pool = self
            .pool
            .get_or_build(self.config.max_index_workers.max(1))?;
        let chunk_lines = self.config.chunk_lines;
        let chunk_bytes = self.config.chunk_bytes;
        let tokenizer = self.config.tokenizer;
        let limits = self.config.discovery_limits();
        let mut metrics = PreparationMetrics::default();
        let mut start = 0usize;
        while start < candidates.len() {
            check_cancelled(cancellation)?;
            before_batch();
            let end = prepare_batch_end(candidates, start, limits);
            if end <= start {
                return Err(Error::OperationFailure(
                    "candidate batch preparation made no progress".into(),
                ));
            }
            let batch_source_bytes = candidates[start..end]
                .iter()
                .fold(0u64, |total, file| total.saturating_add(file.size_bytes));
            metrics.batches = metrics.batches.saturating_add(1);
            metrics.max_batch_files = metrics.max_batch_files.max(end - start);
            metrics.max_batch_source_bytes = metrics.max_batch_source_bytes.max(batch_source_bytes);
            let preparation_started = Instant::now();
            let batch = if profiling == StorageProfiling::Collect {
                let profiled = pool.install(|| {
                    candidates[start..end]
                        .par_iter()
                        .map(|file| {
                            check_cancelled(cancellation)?;
                            let mut detail = FilePreparationDiagnostics::default();
                            let prepared = prepare_file_profiled(
                                &self.repository_root,
                                file,
                                chunk_lines,
                                chunk_bytes,
                                tokenizer,
                                limits.max_file_bytes,
                                cancellation,
                                &mut detail,
                            )?;
                            check_cancelled(cancellation)?;
                            Ok((prepared, detail))
                        })
                        .collect::<Result<Vec<_>>>()
                })?;
                let mut batch = Vec::with_capacity(profiled.len());
                for (prepared, detail) in profiled {
                    if let PreparedFile::Indexed(file, _, _) = &prepared {
                        let language = file.language.as_deref().unwrap_or("<unknown>");
                        metrics
                            .detail_by_language
                            .entry(language.to_owned())
                            .or_default()
                            .add(&detail);
                    }
                    metrics.detail.add(&detail);
                    batch.push(prepared);
                }
                batch
            } else {
                pool.install(|| {
                    candidates[start..end]
                        .par_iter()
                        .map(|file| {
                            check_cancelled(cancellation)?;
                            let prepared = prepare_file(
                                &self.repository_root,
                                file,
                                chunk_lines,
                                chunk_bytes,
                                tokenizer,
                                limits.max_file_bytes,
                                cancellation,
                            )?;
                            check_cancelled(cancellation)?;
                            Ok(prepared)
                        })
                        .collect::<Result<Vec<_>>>()
                })?
            };
            metrics.preparation += preparation_started.elapsed();
            let write_before = (profiling == StorageProfiling::Collect)
                .then(process_write_bytes)
                .flatten();
            let insertion_started = Instant::now();
            consume(batch)?;
            metrics.insertion += insertion_started.elapsed();
            let write_after = (profiling == StorageProfiling::Collect)
                .then(process_write_bytes)
                .flatten();
            let batch_write_bytes = write_before
                .zip(write_after)
                .map(|(before, after)| after.saturating_sub(before));
            metrics.insertion_write_bytes = match (metrics.insertion_write_bytes, batch_write_bytes)
            {
                (Some(total), Some(current)) => Some(total.saturating_add(current)),
                (None, Some(current)) if metrics.batches == 1 => Some(current),
                _ => None,
            };
            start = end;
        }
        Ok(metrics)
    }

    pub(super) fn plan_relocations(
        &self,
        existing: &HashMap<String, crate::storage::FileRecord>,
        candidates: &HashMap<String, DiscoveredFile>,
        change_set: &ChangeSet,
        cancellation: &CancellationToken,
    ) -> Result<Vec<RelocationPlan>> {
        if change_set.created.is_empty() || change_set.deleted.is_empty() {
            return Ok(Vec::new());
        }

        let mut old_by_key = HashMap::<RelocationKey, Vec<String>>::new();
        for path in &change_set.deleted {
            check_cancelled(cancellation)?;
            let Some(record) = existing.get(path) else {
                continue;
            };
            old_by_key
                .entry(RelocationKey {
                    content_hash: record.content_hash.clone(),
                    size_bytes: record.size_bytes,
                    language: record.language.clone(),
                })
                .or_default()
                .push(path.clone());
        }

        let mut new_by_key = HashMap::<RelocationKey, Vec<DiscoveredFile>>::new();
        for path in &change_set.created {
            check_cancelled(cancellation)?;
            let Some(file) = candidates.get(path) else {
                continue;
            };
            let bytes = match read_bounded(
                &self.repository_root,
                &file.relative_path,
                self.config.max_file_bytes,
            ) {
                Ok(Some(bytes)) => bytes,
                Ok(None) | Err(_) => continue,
            };
            new_by_key
                .entry(RelocationKey {
                    content_hash: hash_bytes(&bytes),
                    size_bytes: u64::try_from(bytes.len()).unwrap_or(u64::MAX),
                    language: parser::language_by_path(&file.relative_path),
                })
                .or_default()
                .push(file.clone());
        }

        let mut relocations = Vec::new();
        for (key, old_paths) in old_by_key {
            let Some(new_files) = new_by_key.get(&key) else {
                continue;
            };
            if old_paths.len() != 1 || new_files.len() != 1 {
                continue;
            }
            relocations.push(RelocationPlan {
                old_path: old_paths[0].clone(),
                new_file: new_files[0].clone(),
                expected_hash: key.content_hash,
            });
        }
        relocations.sort_unstable_by(|left, right| {
            left.new_file
                .relative_path
                .cmp(&right.new_file.relative_path)
        });
        Ok(relocations)
    }
}
