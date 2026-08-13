use super::*;

const IMPORT_REPAIR_PAGE_SIZE: usize = 32;

pub(super) const IMPORT_CANDIDATE_RESOLUTION_SQL: &str = "WITH requested(priority, path) AS (
         SELECT CAST(key AS INTEGER), value FROM json_each(?1)
     )
     SELECT requested.path
     FROM requested
     JOIN files ON files.path = requested.path
     ORDER BY requested.priority
     LIMIT 1";

impl Storage {
    pub(crate) fn ensure_path_projection(conn: &mut Connection) -> Result<()> {
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if path_projection_matches(&tx)? {
            tx.commit()?;
            return Ok(());
        }

        tracing::warn!("path projection integrity check failed; rebuilding");
        tx.execute("DELETE FROM path_entries", [])?;
        tx.execute_batch(
            "WITH RECURSIVE prefixes(remaining, prefix, depth) AS (
                 SELECT path, '', 0 FROM files
                 UNION ALL
                 SELECT substr(remaining, instr(remaining, '/') + 1),
                        CASE WHEN prefix = ''
                             THEN substr(remaining, 1, instr(remaining, '/') - 1)
                             ELSE prefix || '/' || substr(remaining, 1, instr(remaining, '/') - 1)
                        END,
                        depth + 1
                 FROM prefixes
                 WHERE instr(remaining, '/') > 0
             )
             INSERT INTO path_entries(path, depth, kind, file_id)
             SELECT DISTINCT prefix, depth, 0, NULL
             FROM prefixes
             WHERE depth > 0;

             INSERT INTO path_entries(path, depth, kind, file_id)
             SELECT path,
                    length(path) - length(replace(path, '/', '')) + 1,
                    1,
                    id
             FROM files;",
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Repair bounded-store usage rows from their authoritative relations.
    ///
    /// These singleton rows are quota accelerators, not sources of truth. A
    /// healthy open performs read comparisons only; mismatches are repaired in
    /// one transaction without loading individual evidence or content rows.
    pub(crate) fn ensure_quota_usage_projections(conn: &mut Connection) -> Result<()> {
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut repairs = 0usize;

        let retrieval_usage_exists = tx
            .query_row(
                "SELECT 1 FROM retrieval_receipt_usage WHERE id = 1",
                [],
                |_| Ok(()),
            )
            .optional()?
            .is_some();
        if retrieval_usage_exists {
            repairs = repairs.saturating_add(tx.execute(
                "UPDATE retrieval_receipts
                 SET evidence_count = (
                         SELECT count(*)
                         FROM retrieval_receipt_evidence
                         WHERE receipt_id = retrieval_receipts.id
                     ),
                     evidence_bytes = (
                         SELECT coalesce(sum(logical_bytes), 0)
                         FROM retrieval_receipt_evidence
                         WHERE receipt_id = retrieval_receipts.id
                     )
                 WHERE evidence_count != (
                           SELECT count(*)
                           FROM retrieval_receipt_evidence
                           WHERE receipt_id = retrieval_receipts.id
                       )
                    OR evidence_bytes != (
                           SELECT coalesce(sum(logical_bytes), 0)
                           FROM retrieval_receipt_evidence
                           WHERE receipt_id = retrieval_receipts.id
                       )",
                [],
            )?);
            repairs = repairs.saturating_add(tx.execute(
                "WITH receipt_totals AS (
                     SELECT count(*) AS row_count,
                            coalesce(sum(logical_bytes), 0) AS logical_bytes,
                            coalesce(max(access_sequence), 0) AS max_sequence
                     FROM retrieval_receipts
                 ),
                 evidence_totals AS (
                     SELECT count(*) AS row_count,
                            coalesce(sum(logical_bytes), 0) AS logical_bytes
                     FROM retrieval_receipt_evidence
                 )
                 UPDATE retrieval_receipt_usage
                 SET next_access_sequence = max(
                         next_access_sequence,
                         (SELECT max_sequence FROM receipt_totals)
                     ),
                     receipt_count = (SELECT row_count FROM receipt_totals),
                     receipt_bytes = (SELECT logical_bytes FROM receipt_totals),
                     evidence_count = (SELECT row_count FROM evidence_totals),
                     evidence_bytes = (SELECT logical_bytes FROM evidence_totals)
                 WHERE next_access_sequence < (SELECT max_sequence FROM receipt_totals)
                    OR receipt_count != (SELECT row_count FROM receipt_totals)
                    OR receipt_bytes != (SELECT logical_bytes FROM receipt_totals)
                    OR evidence_count != (SELECT row_count FROM evidence_totals)
                    OR evidence_bytes != (SELECT logical_bytes FROM evidence_totals)",
                [],
            )?);
        } else {
            tx.execute("DELETE FROM retrieval_receipts", [])?;
            tx.execute(
                "INSERT INTO retrieval_receipt_usage(id, namespace)
                 VALUES (1, lower(hex(randomblob(16))))",
                [],
            )?;
            repairs = repairs.saturating_add(1);
        }

        let query_usage_exists = tx
            .query_row(
                "SELECT 1 FROM query_coverage_receipt_usage WHERE id = 1",
                [],
                |_| Ok(()),
            )
            .optional()?
            .is_some();
        if query_usage_exists {
            repairs = repairs.saturating_add(tx.execute(
                "WITH authoritative AS (
                     SELECT count(*) AS row_count,
                            coalesce(sum(logical_bytes), 0) AS logical_bytes,
                            coalesce(max(access_sequence), 0) AS max_sequence
                     FROM query_coverage_receipts
                 )
                 UPDATE query_coverage_receipt_usage
                 SET next_access_sequence = max(
                         next_access_sequence,
                         (SELECT max_sequence FROM authoritative)
                     ),
                     receipt_count = (SELECT row_count FROM authoritative),
                     logical_bytes = (SELECT logical_bytes FROM authoritative)
                 WHERE next_access_sequence < (SELECT max_sequence FROM authoritative)
                    OR receipt_count != (SELECT row_count FROM authoritative)
                    OR logical_bytes != (SELECT logical_bytes FROM authoritative)",
                [],
            )?);
        } else {
            tx.execute("DELETE FROM query_coverage_receipts", [])?;
            tx.execute(
                "INSERT INTO query_coverage_receipt_usage(id, namespace)
                 VALUES (1, lower(hex(randomblob(16))))",
                [],
            )?;
            repairs = repairs.saturating_add(1);
        }

        let delta_usage_exists = tx
            .query_row(
                "SELECT 1 FROM read_delta_base_usage WHERE id = 1",
                [],
                |_| Ok(()),
            )
            .optional()?
            .is_some();
        if !delta_usage_exists {
            tx.execute("INSERT INTO read_delta_base_usage(id) VALUES (1)", [])?;
            repairs = repairs.saturating_add(1);
        }
        repairs = repairs.saturating_add(tx.execute(
            "WITH authoritative AS (
                 SELECT count(*) AS row_count,
                        coalesce(sum(logical_bytes), 0) AS logical_bytes,
                        coalesce(max(access_sequence), 0) AS max_sequence
                 FROM read_delta_bases
             )
             UPDATE read_delta_base_usage
             SET next_access_sequence = max(
                     next_access_sequence,
                     (SELECT max_sequence FROM authoritative)
                 ),
                 base_count = (SELECT row_count FROM authoritative),
                 base_bytes = (SELECT logical_bytes FROM authoritative)
             WHERE next_access_sequence < (SELECT max_sequence FROM authoritative)
                OR base_count != (SELECT row_count FROM authoritative)
                OR base_bytes != (SELECT logical_bytes FROM authoritative)",
            [],
        )?);

        tx.commit()?;
        if repairs > 0 {
            tracing::warn!(repairs, "persisted quota usage projections repaired");
        }
        Ok(())
    }

    pub(crate) fn insert_path_projection(
        tx: &Transaction<'_>,
        path: &str,
        file_id: i64,
    ) -> Result<()> {
        let parts = path.split('/').collect::<Vec<_>>();
        let mut insert_directory = tx.prepare_cached(
            "INSERT OR IGNORE INTO path_entries(path, depth, kind, file_id) VALUES (?1, ?2, 0, NULL)",
        )?;
        for index in 1..parts.len() {
            let directory = parts[..index].join("/");
            insert_directory.execute(params![directory, usize_to_i64(index)?])?;
        }
        drop(insert_directory);
        tx.prepare_cached(
            "INSERT OR REPLACE INTO path_entries(path, depth, kind, file_id) VALUES (?1, ?2, 1, ?3)",
        )?
        .execute(params![path, usize_to_i64(parts.len())?, file_id])?;
        Ok(())
    }

    pub(crate) fn remove_orphan_path_entries(tx: &Transaction<'_>) -> Result<()> {
        tx.execute(
            "DELETE FROM path_entries
             WHERE kind = 0
               AND NOT EXISTS (
                   SELECT 1 FROM files
                   WHERE substr(files.path, 1, length(path_entries.path) + 1)
                         = path_entries.path || '/'
               )",
            [],
        )?;
        Ok(())
    }
}

impl ReconciliationWriter<'_, '_> {
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
            if update_import.execute(params![
                projection.value.resolved_path.as_deref(),
                projection.id
            ])? != 1
            {
                return Err(Error::OperationFailure(
                    "import changed before projection refresh".into(),
                ));
            }
            delete_candidates.execute(params![projection.id])?;
            for (priority, candidate_path) in projection.value.candidate_paths.iter().enumerate() {
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

    /// Validate and repair every persisted import projection in bounded pages.
    ///
    /// Candidate generation remains owned by the indexer callback. Storage owns
    /// the transaction and resolves each bounded candidate vector against the
    /// exact file membership that graph readers observe after this publication.
    pub(crate) fn repair_import_projections(
        &mut self,
        mut derive_candidates: impl FnMut(&ImportSeed) -> Result<Vec<String>>,
    ) -> Result<usize> {
        let mut after_id = None;
        let mut repaired = 0usize;
        loop {
            let stored = self.import_projection_page(after_id)?;
            if stored.is_empty() {
                break;
            }
            after_id = stored.last().map(|(seed, _)| seed.id);
            let persisted_candidates = self.persisted_import_candidates(&stored)?;
            let mut repairs = Vec::new();
            {
                let mut resolve = self
                    .transaction
                    .prepare_cached(IMPORT_CANDIDATE_RESOLUTION_SQL)?;
                for (seed, resolved_path) in stored {
                    let candidate_paths = derive_candidates(&seed)?;
                    if candidate_paths.len() > MAX_IMPORT_CANDIDATES_PER_IMPORT
                        || candidate_paths.iter().any(|candidate| {
                            candidate.is_empty()
                                || candidate.len() > MAX_IMPORT_CANDIDATE_PATH_BYTES
                        })
                    {
                        return Err(Error::OperationFailure(
                            "derived import candidates exceed persisted bounds".into(),
                        ));
                    }
                    let candidate_input = serde_json::to_string(&candidate_paths)?;
                    let expected_resolved = resolve
                        .query_row(params![candidate_input], |row| row.get::<_, String>(0))
                        .optional()?;
                    let value = ImportProjectionValue {
                        resolved_path: expected_resolved,
                        candidate_paths,
                    };
                    let candidates = persisted_candidates
                        .get(&seed.id)
                        .map(Vec::as_slice)
                        .unwrap_or_default();
                    let candidates_match = candidates.len() == value.candidate_paths.len()
                        && candidates.iter().enumerate().all(
                            |(priority, (stored_priority, stored_path))| {
                                *stored_priority == i64::try_from(priority).unwrap_or(i64::MAX)
                                    && stored_path == &value.candidate_paths[priority]
                            },
                        );
                    if resolved_path != value.resolved_path || !candidates_match {
                        repairs.push(ImportProjection {
                            id: seed.id,
                            file_id: seed.file_id,
                            value,
                        });
                    }
                }
            }
            repaired = repaired.saturating_add(repairs.len());
            self.refresh_import_projections(&repairs)?;
        }
        if repaired > 0 {
            tracing::warn!(repaired, "persisted import projections repaired");
        }
        Ok(repaired)
    }

    fn import_projection_page(
        &self,
        after_id: Option<i64>,
    ) -> Result<Vec<(ImportSeed, Option<String>)>> {
        let mut statement = self.transaction.prepare(
            "SELECT imports.id, imports.file_id, files.path,
                    imports.raw_target, imports.resolved_path
             FROM imports
             JOIN files ON files.id = imports.file_id
             WHERE (?1 IS NULL OR imports.id > ?1)
             ORDER BY imports.id
             LIMIT ?2",
        )?;
        Ok(statement
            .query_map(
                params![after_id, usize_to_i64(IMPORT_REPAIR_PAGE_SIZE)?],
                |row| {
                    Ok((
                        ImportSeed {
                            id: row.get(0)?,
                            file_id: row.get(1)?,
                            source_path: row.get(2)?,
                            raw_target: row.get(3)?,
                        },
                        row.get::<_, Option<String>>(4)?,
                    ))
                },
            )?
            .collect::<std::result::Result<Vec<_>, _>>()?)
    }

    fn persisted_import_candidates(
        &self,
        stored: &[(ImportSeed, Option<String>)],
    ) -> Result<HashMap<i64, Vec<(i64, String)>>> {
        let input =
            serde_json::to_string(&stored.iter().map(|(seed, _)| seed.id).collect::<Vec<_>>())?;
        let mut by_import = HashMap::<i64, Vec<(i64, String)>>::new();
        let mut statement = self.transaction.prepare(
            "WITH requested(import_id) AS (
                 SELECT CAST(value AS INTEGER) FROM json_each(?1)
             )
             SELECT import_candidates.import_id,
                    import_candidates.priority,
                    import_candidates.candidate_path
             FROM requested
             JOIN import_candidates USING(import_id)
             ORDER BY import_candidates.import_id,
                      import_candidates.priority,
                      import_candidates.candidate_path",
        )?;
        let rows = statement.query_map(params![input], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?;
        for row in rows {
            let (import_id, priority, candidate_path) = row?;
            by_import
                .entry(import_id)
                .or_default()
                .push((priority, candidate_path));
        }
        Ok(by_import)
    }
}

#[cfg(test)]
pub(super) fn path_projection_is_current(conn: &Connection) -> Result<bool> {
    path_projection_matches(conn)
}

fn path_projection_matches(conn: &Connection) -> Result<bool> {
    let mismatched: bool = conn.query_row(
        "WITH RECURSIVE prefixes(remaining, prefix, depth) AS (
             SELECT path, '', 0 FROM files
             UNION ALL
             SELECT substr(remaining, instr(remaining, '/') + 1),
                    CASE WHEN prefix = ''
                         THEN substr(remaining, 1, instr(remaining, '/') - 1)
                         ELSE prefix || '/' || substr(remaining, 1, instr(remaining, '/') - 1)
                    END,
                    depth + 1
             FROM prefixes
             WHERE instr(remaining, '/') > 0
         ), expected AS (
             SELECT path,
                    length(path) - length(replace(path, '/', '')) + 1 AS depth,
                    1 AS kind,
                    id AS file_id
             FROM files
             UNION
             SELECT prefix, depth, 0, NULL
             FROM prefixes
             WHERE depth > 0
         ), difference AS (
             SELECT path, depth, kind, file_id FROM expected
             EXCEPT
             SELECT path, depth, kind, file_id FROM path_entries
         ), reverse_difference AS (
             SELECT path, depth, kind, file_id FROM path_entries
             EXCEPT
             SELECT path, depth, kind, file_id FROM expected
         )
         SELECT EXISTS(SELECT 1 FROM difference)
             OR EXISTS(SELECT 1 FROM reverse_difference)",
        [],
        |row| row.get(0),
    )?;
    Ok(!mismatched)
}
