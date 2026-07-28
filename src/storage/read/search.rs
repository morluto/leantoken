impl ReadSession {
    pub fn search_word(&self, query: &str, max_results: usize) -> Result<Vec<ChunkHit>> {
        self.search_word_page(query, max_results, 0)
    }

    pub(crate) fn search_word_page(
        &self,
        query: &str,
        max_results: usize,
        offset: usize,
    ) -> Result<Vec<ChunkHit>> {
        self.search_fts(FtsTable::Word, query, max_results, offset)
    }

    pub fn search_trigram(&self, query: &str, max_results: usize) -> Result<Vec<ChunkHit>> {
        self.search_trigram_page(query, max_results, 0)
    }

    pub(crate) fn search_trigram_page(
        &self,
        query: &str,
        max_results: usize,
        offset: usize,
    ) -> Result<Vec<ChunkHit>> {
        if query.chars().count() < 3 {
            return Ok(Vec::new());
        }
        let quoted = quoted_fts_phrase(query);
        self.search_fts(FtsTable::Trigram, &quoted, max_results, offset)
    }

    pub(crate) fn search_regex_candidates_page(
        &self,
        query: &str,
        max_results: usize,
        offset: usize,
    ) -> Result<Vec<ChunkHit>> {
        let limit = bounded_limit(max_results);
        let offset = i64::try_from(offset).unwrap_or(i64::MAX);
        let mut stmt = self.conn.prepare_cached(
            "SELECT c.id, c.file_id, f.path, c.content, c.start_line, c.end_line,
                    c.start_byte, c.end_byte, c.token_count, f.generation, 0.0
             FROM chunks_fts_trigram
             JOIN chunks c ON chunks_fts_trigram.rowid = c.rowid
             JOIN files f ON c.file_id = f.id
             WHERE chunks_fts_trigram MATCH ?1
             ORDER BY f.id, c.start_byte, c.id
             LIMIT ?2 OFFSET ?3",
        )?;
        let rows = stmt.query_map(params![query, limit, offset], Storage::map_chunk_hit)?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    pub(crate) fn select_scoped_regex_candidate_ids(
        &self,
        query: &str,
        max_rows_scanned: usize,
        max_candidates: usize,
        include_paths: &[String],
        exclude_paths: &[String],
        mut allows_path: impl FnMut(&str) -> bool,
    ) -> Result<Vec<i64>> {
        let limit = i64::try_from(max_rows_scanned.saturating_add(1)).unwrap_or(i64::MAX);
        let path_sql = scoped_regex_path_sql(include_paths, exclude_paths);
        let sql = format!(
            "SELECT c.id, f.path
             FROM chunks_fts_trigram
             JOIN chunks c ON chunks_fts_trigram.rowid = c.rowid
             JOIN files f ON c.file_id = f.id
             WHERE chunks_fts_trigram MATCH ?1
             {path_clause}
             ORDER BY f.id, c.start_byte, c.id
             LIMIT ?2",
            path_clause = path_sql.clause,
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let mut bind = Vec::<rusqlite::types::Value>::with_capacity(2 + path_sql.params.len());
        bind.push(query.to_owned().into());
        bind.push(limit.into());
        bind.extend(path_sql.params.into_iter().map(Into::into));
        let mut rows = stmt.query(rusqlite::params_from_iter(bind))?;
        let mut rows_scanned = 0usize;
        let mut candidate_ids = Vec::new();
        while let Some(row) = rows.next()? {
            rows_scanned = rows_scanned.saturating_add(1);
            if rows_scanned > max_rows_scanned {
                return Err(Error::LimitExceeded);
            }
            let path: String = row.get(1)?;
            // Rust PathFilter remains the correctness gate for patterns SQL
            // cannot express and for over-broad include approximations.
            if !allows_path(&path) {
                continue;
            }
            if candidate_ids.len() == max_candidates {
                return Err(Error::LimitExceeded);
            }
            candidate_ids.push(row.get(0)?);
        }
        Ok(candidate_ids)
    }

    pub(crate) fn regex_candidates_by_ids(&self, chunk_ids: &[i64]) -> Result<Vec<ChunkHit>> {
        if chunk_ids.is_empty() {
            return Ok(Vec::new());
        }
        let input = serde_json::to_string(chunk_ids)?;
        let mut stmt = self.conn.prepare_cached(
            "WITH requested AS (
                 SELECT CAST(key AS INTEGER) AS request_index,
                        CAST(value AS INTEGER) AS chunk_id
                 FROM json_each(?1)
             )
             SELECT c.id, c.file_id, f.path, c.content, c.start_line, c.end_line,
                    c.start_byte, c.end_byte, c.token_count, f.generation, 0.0
             FROM requested
             JOIN chunks c ON c.id = requested.chunk_id
             JOIN files f ON c.file_id = f.id
             ORDER BY requested.request_index",
        )?;
        let rows = stmt.query_map(params![input], Storage::map_chunk_hit)?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    pub(crate) fn regex_candidate_count_up_to(
        &self,
        query: &str,
        max_results: usize,
    ) -> Result<usize> {
        let limit = i64::try_from(max_results).unwrap_or(i64::MAX);
        let count = self.conn.query_row(
            "SELECT COUNT(*)
             FROM (
                 SELECT rowid
                 FROM chunks_fts_trigram
                 WHERE chunks_fts_trigram MATCH ?1
                 LIMIT ?2
             )",
            params![query, limit],
            |row| row.get::<_, i64>(0),
        )?;
        Ok(i64_to_usize(count)?)
    }

    pub fn search_symbols(
        &self,
        query: &str,
        case_sensitive: bool,
        max_results: usize,
    ) -> Result<Vec<SymbolHit>> {
        self.search_symbols_page(query, case_sensitive, max_results, 0)
    }

    pub(crate) fn search_symbols_page(
        &self,
        query: &str,
        case_sensitive: bool,
        max_results: usize,
        offset: usize,
    ) -> Result<Vec<SymbolHit>> {
        let limit = bounded_limit(max_results);
        let offset = i64::try_from(offset).unwrap_or(i64::MAX);
        let indexed = query.chars().count() >= 3;
        let sql = if indexed {
            "SELECT f.path, f.content_hash, f.generation, s.id, s.file_id, s.name, s.kind, s.parent, s.signature, s.start_line, s.end_line, s.start_byte, s.end_byte
                 FROM symbols_fts_trigram
                 JOIN symbols s ON s.rowid = symbols_fts_trigram.rowid
                 JOIN files f ON f.id = s.file_id
                 WHERE symbols_fts_trigram MATCH ?5
                   AND CASE WHEN ?2 THEN instr(s.name, ?1) > 0 ELSE instr(lower(s.name), lower(?1)) > 0 END
                 ORDER BY CASE WHEN CASE WHEN ?2 THEN s.name = ?1 ELSE lower(s.name) = lower(?1) END THEN 0 ELSE 1 END,
                          length(s.name), f.path, s.start_byte
                 LIMIT ?3 OFFSET ?4"
        } else {
            "SELECT f.path, f.content_hash, f.generation, s.id, s.file_id, s.name, s.kind, s.parent, s.signature, s.start_line, s.end_line, s.start_byte, s.end_byte
                 FROM symbols s JOIN files f ON f.id = s.file_id
                 WHERE ?5 IS NULL
                   AND CASE WHEN ?2 THEN instr(s.name, ?1) > 0 ELSE instr(lower(s.name), lower(?1)) > 0 END
                 ORDER BY CASE WHEN CASE WHEN ?2 THEN s.name = ?1 ELSE lower(s.name) = lower(?1) END THEN 0 ELSE 1 END,
                          length(s.name), f.path, s.start_byte
                 LIMIT ?3 OFFSET ?4"
        };
        let quoted = indexed.then(|| quoted_fts_phrase(query));
        let mut stmt = self.conn.prepare_cached(sql)?;
        let rows = stmt.query_map(
            params![query, case_sensitive, limit, offset, quoted],
            Storage::map_symbol_hit,
        )?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    pub(crate) fn find_symbols_exact_batch(
        &self,
        names: &[String],
        max_results_per_name: usize,
    ) -> Result<Vec<Vec<SymbolHit>>> {
        if names.is_empty() {
            return Ok(Vec::new());
        }
        let input = serde_json::to_string(names)?;
        let limit = bounded_limit(max_results_per_name);
        let mut stmt = self.conn.prepare_cached(
            "WITH requested AS (
                 SELECT CAST(key AS INTEGER) AS request_index,
                        CAST(value AS TEXT) AS name
                 FROM json_each(?1)
             )
             SELECT requested.request_index,
                    f.path, f.content_hash, f.generation,
                    s.id, s.file_id, s.name, s.kind, s.parent, s.signature,
                    s.start_line, s.end_line, s.start_byte, s.end_byte
             FROM requested
             JOIN symbols AS s ON s.id IN (
                 SELECT exact.id
                 FROM symbols AS exact INDEXED BY symbols_name_idx
                 JOIN files AS exact_file ON exact_file.id = exact.file_id
                 WHERE exact.name = requested.name COLLATE NOCASE
                   AND exact.name = requested.name COLLATE BINARY
                 ORDER BY exact_file.path, exact.start_byte
                 LIMIT ?2
             )
             JOIN files f ON f.id = s.file_id
             ORDER BY requested.request_index, f.path, s.start_byte, s.id",
        )?;
        let rows = stmt.query_map(params![input, limit], |row| {
            let request_index = i64_to_usize(row.get(0)?)?;
            let hit = SymbolHit {
                path: row.get(1)?,
                content_hash: row.get(2)?,
                generation: i64_to_u64(row.get(3)?)?,
                symbol: SymbolRecord {
                    id: row.get(4)?,
                    file_id: row.get(5)?,
                    name: row.get(6)?,
                    kind: row.get(7)?,
                    parent: row.get(8)?,
                    signature: row.get(9)?,
                    start_line: i64_to_usize(row.get(10)?)?,
                    end_line: i64_to_usize(row.get(11)?)?,
                    start_byte: i64_to_usize(row.get(12)?)?,
                    end_byte: i64_to_usize(row.get(13)?)?,
                },
            };
            Ok((request_index, hit))
        })?;
        let mut matches = vec![Vec::new(); names.len()];
        for row in rows {
            let (request_index, hit) = row?;
            if let Some(slot) = matches.get_mut(request_index) {
                slot.push(hit);
            }
        }
        Ok(matches)
    }

    pub fn search_references(
        &self,
        query: &str,
        case_sensitive: bool,
        max_results: usize,
    ) -> Result<Vec<ReferenceHit>> {
        self.search_references_page(query, case_sensitive, max_results, 0)
    }

    pub(crate) fn search_references_page(
        &self,
        query: &str,
        case_sensitive: bool,
        max_results: usize,
        offset: usize,
    ) -> Result<Vec<ReferenceHit>> {
        let limit = bounded_limit(max_results);
        let offset = i64::try_from(offset).unwrap_or(i64::MAX);
        let indexed = query.chars().count() >= 3;
        let sql = if indexed {
            "SELECT f.path, f.content_hash, f.generation, r.id, r.file_id, r.name, r.kind, r.role, r.enclosing_symbol, r.start_line, r.end_line, r.start_byte, r.end_byte
                 FROM symbol_refs_fts_trigram
                 JOIN symbol_refs r ON r.rowid = symbol_refs_fts_trigram.rowid
                 JOIN files f ON f.id = r.file_id
                 WHERE symbol_refs_fts_trigram MATCH ?5
                   AND CASE WHEN ?2 THEN instr(r.name, ?1) > 0 ELSE instr(lower(r.name), lower(?1)) > 0 END
                 ORDER BY CASE WHEN CASE WHEN ?2 THEN r.name = ?1 ELSE lower(r.name) = lower(?1) END THEN 0 ELSE 1 END,
                          length(r.name), f.path, r.start_byte
                 LIMIT ?3 OFFSET ?4"
        } else {
            "SELECT f.path, f.content_hash, f.generation, r.id, r.file_id, r.name, r.kind, r.role, r.enclosing_symbol, r.start_line, r.end_line, r.start_byte, r.end_byte
                 FROM symbol_refs r JOIN files f ON f.id = r.file_id
                 WHERE ?5 IS NULL
                   AND CASE WHEN ?2 THEN instr(r.name, ?1) > 0 ELSE instr(lower(r.name), lower(?1)) > 0 END
                 ORDER BY CASE WHEN CASE WHEN ?2 THEN r.name = ?1 ELSE lower(r.name) = lower(?1) END THEN 0 ELSE 1 END,
                          length(r.name), f.path, r.start_byte
                 LIMIT ?3 OFFSET ?4"
        };
        let quoted = indexed.then(|| quoted_fts_phrase(query));
        let mut stmt = self.conn.prepare_cached(sql)?;
        let rows = stmt.query_map(
            params![query, case_sensitive, limit, offset, quoted],
            Storage::map_reference_hit,
        )?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }
}
