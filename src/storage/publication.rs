impl Storage {
    /// Read the currently committed schema, configuration, and generation metadata.
    pub fn meta(&self) -> Result<MetaRecord> {
        self.begin_read()?.meta()
    }

    /// Return the identifier of the latest atomically committed repository view.
    pub fn repository_generation(&self) -> Result<u64> {
        self.begin_read()?.repository_generation()
    }

    /// Replace the complete index using an internally captured optimistic baseline.
    ///
    /// Indexing code that performs filesystem work before publication should use
    /// [`Self::full_reconcile_at`] with the baseline captured before that work.
    pub fn full_reconcile(&self, config_hash: &str, files: Vec<IndexedFile>) -> Result<u64> {
        let baseline = self.meta()?;
        self.full_reconcile_at(&baseline, config_hash, files)
    }

    /// Replace the complete index only if the generation and configuration used
    /// to build the reconciliation plan are still current.
    ///
    /// On success, all derived rows and the next generation become visible in one
    /// commit. A stale baseline returns [`Error::StaleReconciliation`] before any
    /// mutation is published.
    pub fn full_reconcile_at(
        &self,
        baseline: &MetaRecord,
        config_hash: &str,
        files: Vec<IndexedFile>,
    ) -> Result<u64> {
        self.publish_reconciliation_at(
            baseline,
            config_hash,
            IndexingMode::Rebuild,
            move |writer| {
                for file in files {
                    writer.replace(file)?;
                }
                Ok(())
            },
        )
        .map(|(generation, ())| generation)
    }

    /// Atomically apply one repository reconciliation using an internally captured baseline.
    ///
    /// Unmentioned files remain unchanged; replacements, deletions, derived path
    /// rows, import edges, and generation advancement become visible together.
    /// Indexing code should prefer [`Self::reconcile_files_at`] when planning and
    /// publication are separated by filesystem or parsing work.
    pub fn reconcile_files(
        &self,
        config_hash: &str,
        replacements: Vec<IndexedFile>,
        deletions: &[String],
    ) -> Result<u64> {
        let baseline = self.meta()?;
        self.reconcile_files_at(&baseline, config_hash, replacements, deletions)
    }

    /// Publish an incremental plan only if its source generation and config
    /// still match the committed cache state.
    ///
    /// Replacements are whole-file units and deletions are repository-relative
    /// paths. A no-op preserves the current generation when the configuration is
    /// unchanged. Stale plans fail before publishing any mutation.
    pub fn reconcile_files_at(
        &self,
        baseline: &MetaRecord,
        config_hash: &str,
        replacements: Vec<IndexedFile>,
        deletions: &[String],
    ) -> Result<u64> {
        self.publish_reconciliation_at(
            baseline,
            config_hash,
            IndexingMode::Reconcile,
            move |writer| {
                for path in deletions {
                    writer.delete(path)?;
                }
                for file in replacements {
                    writer.replace(file)?;
                }
                Ok(())
            },
        )
        .map(|(generation, ())| generation)
    }

    /// Build and publish one generation through a bounded caller-owned stream.
    pub(crate) fn publish_reconciliation_at<T>(
        &self,
        baseline: &MetaRecord,
        config_hash: &str,
        mode: IndexingMode,
        write: impl FnOnce(&mut ReconciliationWriter<'_, '_>) -> Result<T>,
    ) -> Result<(u64, T)> {
        self.publish_reconciliation_inner(
            baseline,
            config_hash,
            mode,
            StorageProfiling::Omit,
            |_| Ok(()),
            write,
        )
        .map(|(generation, output, _)| (generation, output))
    }

    /// Build and publish one generation while reporting bounded storage phases.
    pub(crate) fn publish_reconciliation_at_with_progress<T>(
        &self,
        baseline: &MetaRecord,
        config_hash: &str,
        mode: IndexingMode,
        observe: impl FnMut(ReconciliationPublicationPhase) -> Result<()>,
        write: impl FnOnce(&mut ReconciliationWriter<'_, '_>) -> Result<T>,
    ) -> Result<(u64, T)> {
        self.publish_reconciliation_inner(
            baseline,
            config_hash,
            mode,
            StorageProfiling::Omit,
            observe,
            write,
        )
        .map(|(generation, output, _)| (generation, output))
    }

    /// Build and publish one generation with storage-level profiling enabled.
    #[cfg(test)]
    pub(crate) fn publish_reconciliation_profiled_at<T>(
        &self,
        baseline: &MetaRecord,
        config_hash: &str,
        mode: IndexingMode,
        write: impl FnOnce(&mut ReconciliationWriter<'_, '_>) -> Result<T>,
    ) -> Result<(u64, T, PublicationDiagnostics)> {
        self.publish_reconciliation_inner(
            baseline,
            config_hash,
            mode,
            StorageProfiling::Collect,
            |_| Ok(()),
            write,
        )
    }

    /// Build and publish with profiling plus bounded storage-phase reporting.
    pub(crate) fn publish_reconciliation_profiled_at_with_progress<T>(
        &self,
        baseline: &MetaRecord,
        config_hash: &str,
        mode: IndexingMode,
        observe: impl FnMut(ReconciliationPublicationPhase) -> Result<()>,
        write: impl FnOnce(&mut ReconciliationWriter<'_, '_>) -> Result<T>,
    ) -> Result<(u64, T, PublicationDiagnostics)> {
        self.publish_reconciliation_inner(
            baseline,
            config_hash,
            mode,
            StorageProfiling::Collect,
            observe,
            write,
        )
    }

    pub(crate) fn publish_reconciliation_inner<T>(
        &self,
        baseline: &MetaRecord,
        config_hash: &str,
        mode: IndexingMode,
        profiling: StorageProfiling,
        mut observe: impl FnMut(ReconciliationPublicationPhase) -> Result<()>,
        write: impl FnOnce(&mut ReconciliationWriter<'_, '_>) -> Result<T>,
    ) -> Result<(u64, T, PublicationDiagnostics)> {
        let mut writer = self
            .writer
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        // Profiling uses a disposable writer connection so its temporary
        // auto-checkpoint policy can never leak into ordinary publications.
        // Holding the normal writer lock still serializes in-process mutation.
        let mut profiled_connection = if profiling.is_collecting() {
            let mut connection = Connection::open(&self.path)?;
            Self::configure(&mut connection, DEFAULT_BUSY_TIMEOUT)?;
            connection.busy_timeout(DEFAULT_BUSY_TIMEOUT)?;
            connection.pragma_update(None, "wal_autocheckpoint", 0)?;
            Some(connection)
        } else {
            None
        };
        let conn = match profiled_connection.as_mut() {
            Some(connection) => connection,
            None => &mut *writer,
        };
        (|| {
            let mut diagnostics = PublicationDiagnostics::default();
            let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let (current_generation, current_config): (i64, String) = tx.query_row(
                "SELECT repository_generation, config_hash FROM meta WHERE id = 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?;
            verify_baseline(baseline, current_generation, &current_config)?;

            // Initial and replacement publications can build the external-content
            // FTS indexes once instead of maintaining them for every chunk mutation.
            let bulk_fts = mode.is_rebuild() || current_generation == 0;
            let mut trigger_guard = bulk_fts
                .then(|| DatabaseTriggerGuard::disable(&tx))
                .transpose()?;

            if mode.is_rebuild() {
                tx.execute("DELETE FROM files", [])?;
            }

            let next_generation = current_generation
                .checked_add(1)
                .ok_or_else(|| Error::OperationFailure("repository generation exhausted".into()))?;
            let mut writer = ReconciliationWriter {
                transaction: &tx,
                generation: next_generation,
                mode,
                replacements: 0,
                deletions: HashSet::new(),
                projection_refreshes: 0,
            };
            let (output, relational_write_ms, relational_write_bytes) =
                measured_storage_phase(profiling, || write(&mut writer))?;
            diagnostics.relational_write_ms = relational_write_ms;
            diagnostics.relational_write_bytes = relational_write_bytes;
            let changed = mode.is_rebuild()
                || writer.replacements > 0
                || !writer.deletions.is_empty()
                || writer.projection_refreshes > 0
                || current_config != config_hash;
            drop(writer);

            if changed && bulk_fts {
                observe(ReconciliationPublicationPhase::ChunkWordFts)?;
                let (_, elapsed_ms, write_bytes) = measured_storage_phase(profiling, || {
                    tx.execute(
                        "INSERT INTO chunks_fts_word(chunks_fts_word) VALUES('rebuild')",
                        [],
                    )?;
                    Ok(())
                })?;
                diagnostics.chunk_word_fts_rebuild_ms = elapsed_ms;
                diagnostics.chunk_word_fts_rebuild_write_bytes = write_bytes;

                observe(ReconciliationPublicationPhase::ChunkTrigramFts)?;
                let (_, elapsed_ms, write_bytes) = measured_storage_phase(profiling, || {
                    tx.execute(
                        "INSERT INTO chunks_fts_trigram(chunks_fts_trigram) VALUES('rebuild')",
                        [],
                    )?;
                    Ok(())
                })?;
                diagnostics.chunk_trigram_fts_rebuild_ms = elapsed_ms;
                diagnostics.chunk_trigram_fts_rebuild_write_bytes = write_bytes;

                observe(ReconciliationPublicationPhase::SymbolFts)?;
                let (_, elapsed_ms, write_bytes) = measured_storage_phase(profiling, || {
                    tx.execute(
                        "INSERT INTO symbols_fts_trigram(symbols_fts_trigram) VALUES('rebuild')",
                        [],
                    )?;
                    Ok(())
                })?;
                diagnostics.symbol_fts_rebuild_ms = elapsed_ms;
                diagnostics.symbol_fts_rebuild_write_bytes = write_bytes;

                observe(ReconciliationPublicationPhase::ReferenceFts)?;
                let (_, elapsed_ms, write_bytes) = measured_storage_phase(profiling, || {
                    tx.execute(
                        "INSERT INTO symbol_refs_fts_trigram(symbol_refs_fts_trigram) VALUES('rebuild')",
                        [],
                    )?;
                    Ok(())
                })?;
                diagnostics.reference_fts_rebuild_ms = elapsed_ms;
                diagnostics.reference_fts_rebuild_write_bytes = write_bytes;
            }
            let published_generation = if changed {
                tx.execute(
                    "UPDATE meta SET config_hash = ?1, repository_generation = ?2, index_version = index_version + 1 WHERE id = 1",
                    params![config_hash, next_generation],
                )?;
                next_generation
            } else {
                current_generation
            };
            if let Some(guard) = trigger_guard.take() {
                guard.restore()?;
            }
            drop(trigger_guard);
            observe(ReconciliationPublicationPhase::CommitAndCheckpoint)?;
            // A read-only COMMIT can invoke SQLite auto-checkpointing on a
            // pre-existing WAL backlog. Rollback still releases BEGIN
            // IMMEDIATE after the baseline check, without rewriting pages for
            // a generation that did not change.
            let (_, elapsed_ms, write_bytes) = measured_storage_phase(profiling, || {
                if changed {
                    Ok(tx.commit()?)
                } else {
                    Ok(tx.rollback()?)
                }
            })?;
            diagnostics.commit_ms = elapsed_ms;
            diagnostics.commit_write_bytes = write_bytes;

            if profiling.is_collecting() {
                diagnostics.post_commit_diagnostics_complete =
                    populate_post_commit_diagnostics(conn, &self.path, changed, &mut diagnostics)
                        .is_ok();
            }
            Ok((i64_to_u64(published_generation)?, output, diagnostics))
        })()
    }

    pub(crate) fn insert_file(tx: &Transaction, file: &IndexedFile, generation: i64) -> Result<()> {
        tx.prepare_cached(
            "INSERT INTO files(path, language, structurally_complete, size_bytes, modified_ns, content_hash, generation) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        )?
        .execute(params![
                &file.path,
                file.language.as_deref(),
                file.structurally_complete,
                u64_to_i64(file.size_bytes)?,
                file.modified_ns.map(u128_to_i64).transpose()?,
                &file.content_hash,
                generation,
            ])?;
        let file_id = tx.last_insert_rowid();

        let mut insert_chunk = tx.prepare_cached(
            "INSERT INTO chunks(file_id, content, start_line, end_line, start_byte, end_byte, token_count) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        )?;
        for chunk in &file.chunks {
            insert_chunk.execute(params![
                file_id,
                &chunk.content,
                usize_to_i64(chunk.start_line)?,
                usize_to_i64(chunk.end_line)?,
                usize_to_i64(chunk.start_byte)?,
                usize_to_i64(chunk.end_byte)?,
                usize_to_i64(chunk.token_count)?,
            ])?;
        }
        drop(insert_chunk);

        let mut insert_symbol = tx.prepare_cached(
            "INSERT INTO symbols(file_id, name, kind, parent, signature, start_line, end_line, start_byte, end_byte) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        )?;
        for symbol in &file.symbols {
            insert_symbol.execute(params![
                file_id,
                &symbol.name,
                &symbol.kind,
                symbol.parent.as_deref(),
                symbol.signature.as_deref(),
                usize_to_i64(symbol.start_line)?,
                usize_to_i64(symbol.end_line)?,
                usize_to_i64(symbol.start_byte)?,
                usize_to_i64(symbol.end_byte)?,
            ])?;
        }
        drop(insert_symbol);

        let mut insert_reference = tx.prepare_cached(
            "INSERT INTO symbol_refs(file_id, name, kind, role, enclosing_symbol, start_line, end_line, start_byte, end_byte) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        )?;
        for reference in &file.references {
            insert_reference.execute(params![
                file_id,
                &reference.name,
                &reference.kind,
                role_to_str(reference.role),
                reference.enclosing_symbol.as_deref(),
                usize_to_i64(reference.start_line)?,
                usize_to_i64(reference.end_line)?,
                usize_to_i64(reference.start_byte)?,
                usize_to_i64(reference.end_byte)?,
            ])?;
        }
        drop(insert_reference);

        let mut insert_import = tx.prepare_cached(
            "INSERT INTO imports(file_id, raw_target, resolved_path, line) VALUES (?1, ?2, ?3, ?4)",
        )?;
        let mut insert_import_candidate = tx.prepare_cached(
            "INSERT INTO import_candidates(import_id, candidate_path, priority) VALUES (?1, ?2, ?3)",
        )?;
        for import in &file.imports {
            insert_import.execute(params![
                file_id,
                &import.raw_target,
                import.resolved_path.as_deref(),
                usize_to_i64(import.line)?,
            ])?;
            let import_id = tx.last_insert_rowid();
            for (priority, candidate_path) in import.candidate_paths.iter().enumerate() {
                insert_import_candidate.execute(params![
                    import_id,
                    candidate_path,
                    usize_to_i64(priority)?
                ])?;
            }
        }

        Ok(())
    }
}

use super::*;
