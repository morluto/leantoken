impl ReadSession {
    pub fn find_file(&self, path: &str) -> Result<Option<FileRecord>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT id, path, language, size_bytes, modified_ns, content_hash, generation, structurally_complete FROM files WHERE path = ?1",
        )?;
        let mut rows = stmt.query_map(params![path], Storage::map_file)?;
        Ok(rows.next().transpose()?)
    }

    /// Find importers whose persisted candidate set intersects changed repository paths.
    pub(crate) fn affected_importers(&self, candidate_paths: &[String]) -> Result<Vec<String>> {
        if candidate_paths.is_empty() {
            return Ok(Vec::new());
        }
        let input = serde_json::to_string(candidate_paths)?;
        let mut stmt = self.conn.prepare_cached(
            "SELECT DISTINCT files.path
             FROM json_each(?1) AS changed
             JOIN import_candidates ON import_candidates.candidate_path = changed.value
             JOIN imports ON imports.id = import_candidates.import_id
             JOIN files ON files.id = imports.file_id
             ORDER BY files.path",
        )?;
        let rows = stmt.query_map(params![input], |row| row.get(0))?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    pub(crate) fn import_seeds_for_paths(&self, paths: &[String]) -> Result<Vec<ImportSeed>> {
        if paths.is_empty() {
            return Ok(Vec::new());
        }
        let input = serde_json::to_string(paths)?;
        let mut stmt = self.conn.prepare_cached(
            "SELECT imports.id, imports.file_id, files.path, imports.raw_target
             FROM json_each(?1) AS requested
             JOIN files ON files.path = requested.value
             JOIN imports ON imports.file_id = files.id
             ORDER BY files.path, imports.line, imports.id",
        )?;
        let rows = stmt.query_map(params![input], |row| {
            Ok(ImportSeed {
                id: row.get(0)?,
                file_id: row.get(1)?,
                source_path: row.get(2)?,
                raw_target: row.get(3)?,
            })
        })?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// Batch import expansion for context ranking, bounded independently per seed.
    ///
    /// `seed_index` in each result maps back to the original input order. SQL
    /// performs the joins and per-seed limits; ranking decides which evidence to use.
    pub(crate) fn import_symbol_targets(
        &self,
        seed_paths: &[String],
        max_imports_per_seed: usize,
        max_symbols_per_target: usize,
    ) -> Result<Vec<ImportSymbolTarget>> {
        if seed_paths.is_empty() {
            return Ok(Vec::new());
        }
        let input = serde_json::to_string(seed_paths)?;
        let mut stmt = self.conn.prepare_cached(
            "WITH requested AS (
                 SELECT CAST(key AS INTEGER) AS seed_index, value AS seed_path
                 FROM json_each(?1)
             ), ranked_imports AS (
                 SELECT requested.seed_index,
                        imports.id AS import_id,
                        imports.resolved_path,
                        ROW_NUMBER() OVER (
                            PARTITION BY requested.seed_index
                            ORDER BY imports.line, imports.id
                        ) AS import_rank
                 FROM requested
                 JOIN files AS seed ON seed.path = requested.seed_path
                 JOIN imports ON imports.file_id = seed.id
                 WHERE imports.resolved_path IS NOT NULL
             )
             SELECT ranked_imports.seed_index, ranked_imports.import_rank,
                    target.id, target.path, target.language, target.size_bytes,
                    target.modified_ns, target.content_hash, target.generation,
                    target.structurally_complete,
                    symbols.id, symbols.file_id, symbols.name, symbols.kind,
                    symbols.parent, symbols.signature,
                    symbols.start_line, symbols.end_line,
                    symbols.start_byte, symbols.end_byte
             FROM ranked_imports
             JOIN files AS target ON target.path = ranked_imports.resolved_path
             JOIN symbols ON symbols.file_id = target.id
                         AND symbols.id IN (
                             SELECT limited.id
                             FROM symbols AS limited
                             WHERE limited.file_id = target.id
                             ORDER BY limited.start_byte
                             LIMIT ?3
                         )
             WHERE ranked_imports.import_rank <= ?2
             ORDER BY ranked_imports.seed_index, ranked_imports.import_rank, symbols.start_byte",
        )?;
        let rows = stmt.query_map(
            params![
                input,
                bounded_limit(max_imports_per_seed),
                bounded_limit(max_symbols_per_target)
            ],
            |row| {
                let seed_index = i64_to_usize(row.get(0)?)?;
                let import_rank: i64 = row.get(1)?;
                let target_file = FileRecord {
                    id: row.get(2)?,
                    path: row.get(3)?,
                    language: row.get(4)?,
                    size_bytes: i64_to_u64(row.get(5)?)?,
                    modified_ns: row.get::<_, Option<i64>>(6)?.map(i64_to_u128).transpose()?,
                    content_hash: row.get(7)?,
                    generation: i64_to_u64(row.get(8)?)?,
                    structurally_complete: row.get(9)?,
                };
                let symbol = SymbolRecord {
                    id: row.get(10)?,
                    file_id: row.get(11)?,
                    name: row.get(12)?,
                    kind: row.get(13)?,
                    parent: row.get(14)?,
                    signature: row.get(15)?,
                    start_line: i64_to_usize(row.get(16)?)?,
                    end_line: i64_to_usize(row.get(17)?)?,
                    start_byte: i64_to_usize(row.get(18)?)?,
                    end_byte: i64_to_usize(row.get(19)?)?,
                };
                Ok((seed_index, import_rank, target_file, symbol))
            },
        )?;
        let mut grouped = Vec::<ImportSymbolTarget>::new();
        let mut current_key = None;
        for row in rows {
            let (seed_index, import_rank, target_file, symbol) = row?;
            let key = (seed_index, import_rank);
            if current_key != Some(key) {
                grouped.push(ImportSymbolTarget {
                    seed_index,
                    target_file,
                    symbols: Vec::new(),
                });
                current_key = Some(key);
            }
            if let Some(target) = grouped.last_mut() {
                target.symbols.push(symbol);
            }
        }
        Ok(grouped)
    }
}
