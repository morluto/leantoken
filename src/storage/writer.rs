impl ReconciliationWriter<'_, '_> {
    /// Insert one complete file replacement without retaining it in memory.
    pub(crate) fn replace(&mut self, file: IndexedFile) -> Result<()> {
        self.replace_inner(file)
    }

    pub(crate) fn replace_inner(&mut self, file: IndexedFile) -> Result<()> {
        if !self.mode.is_rebuild() {
            self.transaction
                .execute("DELETE FROM files WHERE path = ?1", params![&file.path])?;
        }
        Storage::insert_file(self.transaction, &file, self.generation)?;
        self.replacements = self.replacements.saturating_add(1);
        Ok(())
    }

    /// Remove one path, deduplicating repeated deletion signals.
    pub(crate) fn delete(&mut self, path: &str) -> Result<()> {
        if !self.deletions.insert(path.to_string()) || self.mode.is_rebuild() {
            return Ok(());
        }
        self.transaction
            .execute("DELETE FROM files WHERE path = ?1", params![path])?;
        self.transaction.execute(
            "UPDATE imports SET resolved_path = NULL WHERE resolved_path = ?1",
            params![path],
        )?;
        Ok(())
    }

    pub(crate) fn relocate(
        &mut self,
        old_path: &str,
        new_path: &str,
        size_bytes: u64,
        modified_ns: Option<u128>,
        expected_hash: &str,
    ) -> Result<()> {
        let updated = self.transaction.execute(
            "UPDATE files
             SET path = ?1, size_bytes = ?2, modified_ns = ?3, generation = ?4
             WHERE path = ?5 AND content_hash = ?6",
            params![
                new_path,
                u64_to_i64(size_bytes)?,
                modified_ns.map(u128_to_i64).transpose()?,
                self.generation,
                old_path,
                expected_hash,
            ],
        )?;
        if updated != 1 {
            return Err(Error::OperationFailure(
                "relocation source changed before publication".into(),
            ));
        }
        self.deletions.insert(old_path.to_string());
        self.replacements = self.replacements.saturating_add(1);
        Ok(())
    }

    pub(crate) fn refresh_import_projections(
        &mut self,
        projections: &[ImportProjection],
    ) -> Result<()> {
        if projections.is_empty() {
            return Ok(());
        }
        let mut update_import = self
            .transaction
            .prepare_cached("UPDATE imports SET resolved_path = ?1 WHERE id = ?2")?;
        let mut delete_candidates = self
            .transaction
            .prepare_cached("DELETE FROM import_candidates WHERE import_id = ?1")?;
        let mut insert_candidate = self.transaction.prepare_cached(
            "INSERT INTO import_candidates(import_id, candidate_path, priority) VALUES (?1, ?2, ?3)",
        )?;
        let mut file_ids = HashSet::new();
        for projection in projections {
            if update_import.execute(params![projection.resolved_path.as_deref(), projection.id])?
                != 1
            {
                return Err(Error::OperationFailure(
                    "import changed before projection refresh".into(),
                ));
            }
            delete_candidates.execute(params![projection.id])?;
            for (priority, candidate_path) in projection.candidate_paths.iter().enumerate() {
                insert_candidate.execute(params![
                    projection.id,
                    candidate_path,
                    usize_to_i64(priority)?
                ])?;
            }
            file_ids.insert(projection.file_id);
        }
        drop(update_import);
        drop(delete_candidates);
        drop(insert_candidate);

        let mut update_generation = self
            .transaction
            .prepare_cached("UPDATE files SET generation = ?1 WHERE id = ?2")?;
        for file_id in file_ids {
            update_generation.execute(params![self.generation, file_id])?;
        }
        self.projection_refreshes = self.projection_refreshes.saturating_add(projections.len());
        Ok(())
    }
}
use super::*;
