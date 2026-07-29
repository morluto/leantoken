impl ReadSession {
    pub fn get_chunks_for_file(
        &self,
        file_id: i64,
        max_results: usize,
    ) -> Result<Vec<ChunkRecord>> {
        let limit = bounded_limit(max_results);
        let mut stmt = self.conn.prepare_cached(
            "SELECT id, file_id, content, start_line, end_line, start_byte, end_byte, token_count FROM chunks WHERE file_id = ?1 ORDER BY start_byte LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![file_id, limit], Storage::map_chunk)?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// Return the final indexed line for each file id, preserving request order.
    pub(crate) fn file_end_lines_batch(&self, file_ids: &[i64]) -> Result<Vec<Option<usize>>> {
        if file_ids.is_empty() {
            return Ok(Vec::new());
        }
        let mut seen = HashSet::new();
        let unique_file_ids = file_ids
            .iter()
            .copied()
            .filter(|file_id| seen.insert(*file_id))
            .collect::<Vec<_>>();
        let input = serde_json::to_string(&unique_file_ids)?;
        let mut stmt = self.conn.prepare_cached(
            "WITH requested AS (
                 SELECT CAST(value AS INTEGER) AS file_id
                 FROM json_each(?1)
             )
             SELECT requested.file_id, MAX(chunks.end_line)
             FROM requested
             LEFT JOIN chunks ON chunks.file_id = requested.file_id
             GROUP BY requested.file_id",
        )?;
        let rows = stmt.query_map(params![input], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, Option<i64>>(1)?
                    .map(i64_to_usize)
                    .transpose()?,
            ))
        })?;
        let end_lines = rows.collect::<std::result::Result<HashMap<_, _>, _>>()?;
        Ok(file_ids
            .iter()
            .map(|file_id| end_lines.get(file_id).copied().flatten())
            .collect())
    }

    /// Hydrate overlapping chunks for every requested range in one SQL query.
    ///
    /// The outer vector is aligned one-for-one with `ranges`, including duplicate
    /// requests and ranges with no matches. Each inner vector is ordered by line.
    pub(crate) fn get_chunks_overlapping_batch(
        &self,
        ranges: &[(i64, usize, usize)],
    ) -> Result<Vec<Vec<ChunkRecord>>> {
        if ranges.is_empty() {
            return Ok(Vec::new());
        }
        let input = ranges
            .iter()
            .map(|(file_id, start_line, end_line)| {
                serde_json::json!({
                    "file_id": file_id,
                    "start_line": start_line,
                    "end_line": end_line,
                })
            })
            .collect::<Vec<_>>();
        let input = serde_json::to_string(&input)?;
        let mut stmt = self.conn.prepare_cached(
            "WITH requested AS (
                 SELECT CAST(key AS INTEGER) AS request_index,
                        CAST(value ->> 'file_id' AS INTEGER) AS file_id,
                        CAST(value ->> 'start_line' AS INTEGER) AS start_line,
                        CAST(value ->> 'end_line' AS INTEGER) AS end_line
                 FROM json_each(?1)
             )
             SELECT requested.request_index,
                    chunks.id, chunks.file_id, chunks.content,
                    chunks.start_line, chunks.end_line,
                    chunks.start_byte, chunks.end_byte, chunks.token_count
             FROM requested
             JOIN chunks
               ON chunks.file_id = requested.file_id
              AND chunks.end_line >= requested.start_line
              AND chunks.start_line <= requested.end_line
             ORDER BY requested.request_index, chunks.start_line",
        )?;
        let rows = stmt.query_map(params![input], |row| {
            let request_index = i64_to_usize(row.get(0)?)?;
            let chunk = ChunkRecord {
                id: row.get(1)?,
                file_id: row.get(2)?,
                content: row.get(3)?,
                start_line: i64_to_usize(row.get(4)?)?,
                end_line: i64_to_usize(row.get(5)?)?,
                start_byte: i64_to_usize(row.get(6)?)?,
                end_byte: i64_to_usize(row.get(7)?)?,
                token_count: i64_to_usize(row.get(8)?)?,
            };
            Ok((request_index, chunk))
        })?;
        let mut grouped = vec![Vec::new(); ranges.len()];
        for row in rows {
            let (request_index, chunk) = row?;
            if let Some(chunks) = grouped.get_mut(request_index) {
                chunks.push(chunk);
            }
        }
        Ok(grouped)
    }

    pub fn get_symbols_for_file(
        &self,
        file_id: i64,
        max_results: usize,
    ) -> Result<Vec<SymbolRecord>> {
        let limit = bounded_limit(max_results);
        let mut stmt = self.conn.prepare_cached(
            "SELECT id, file_id, name, kind, parent, signature, start_line, end_line, start_byte, end_byte FROM symbols WHERE file_id = ?1 ORDER BY start_byte LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![file_id, limit], Storage::map_symbol)?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    pub(crate) fn get_symbols_for_file_filtered_page(
        &self,
        file_id: i64,
        name: Option<&str>,
        kind: Option<&str>,
        max_results: usize,
        offset: usize,
    ) -> Result<Vec<SymbolRecord>> {
        let limit = bounded_limit(max_results);
        let offset = i64::try_from(offset).unwrap_or(i64::MAX);
        let qualified =
            name.is_some_and(|name| crate::symbol_identity::split_qualified_symbol(name).is_some());
        let mut stmt = self.conn.prepare_cached(
            "SELECT id, file_id, name, kind, parent, signature, start_line, end_line, start_byte, end_byte
                 FROM symbols
                 WHERE file_id = ?1
                   AND (
                       ?2 IS NULL
                       OR instr(name, ?2) > 0
                       OR (
                           ?4
                           AND parent IS NOT NULL
                           AND instr(parent || '.' || name, ?2) > 0
                       )
                   )
                   AND (?3 IS NULL OR kind = ?3)
                 ORDER BY start_byte, id
                 LIMIT ?5 OFFSET ?6",
        )?;
        let rows = stmt.query_map(
            params![file_id, name, kind, qualified, limit, offset],
            Storage::map_symbol,
        )?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    pub(crate) fn symbol_counts_for_file_filtered(
        &self,
        file_id: i64,
        name: Option<&str>,
        kind: Option<&str>,
    ) -> Result<Vec<(String, usize)>> {
        let qualified =
            name.is_some_and(|name| crate::symbol_identity::split_qualified_symbol(name).is_some());
        let mut stmt = self.conn.prepare_cached(
            "SELECT kind, COUNT(*)
                 FROM symbols
                 WHERE file_id = ?1
                   AND (
                       ?2 IS NULL
                       OR instr(name, ?2) > 0
                       OR (
                           ?4
                           AND parent IS NOT NULL
                           AND instr(parent || '.' || name, ?2) > 0
                       )
                   )
                   AND (?3 IS NULL OR kind = ?3)
                 GROUP BY kind
                 ORDER BY kind",
        )?;
        let rows = stmt.query_map(params![file_id, name, kind, qualified], |row| {
            Ok((row.get(0)?, i64_to_usize(row.get(1)?)?))
        })?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    pub(crate) fn find_symbol(
        &self,
        file_id: i64,
        name: &str,
    ) -> Result<crate::symbol_identity::SymbolResolution<SymbolRecord>> {
        let qualified = crate::symbol_identity::split_qualified_symbol(name);
        let sql = if qualified.is_some() {
            "WITH matches AS (
                 SELECT id, file_id, name, kind, parent, signature,
                        start_line, end_line, start_byte, end_byte
                 FROM symbols INDEXED BY symbols_name_idx
                 WHERE file_id = ?1
                   AND name = ?2 COLLATE NOCASE
                   AND name = ?2 COLLATE BINARY
                 UNION ALL
                 SELECT id, file_id, name, kind, parent, signature,
                        start_line, end_line, start_byte, end_byte
                 FROM symbols INDEXED BY symbols_name_idx
                 WHERE file_id = ?1
                   AND name = ?4 COLLATE NOCASE
                   AND name = ?4 COLLATE BINARY
                   AND parent = ?3
             )
             SELECT id, file_id, name, kind, parent, signature,
                    start_line, end_line, start_byte, end_byte
             FROM matches
             ORDER BY start_byte, id
             LIMIT 2"
        } else {
            "SELECT id, file_id, name, kind, parent, signature,
                    start_line, end_line, start_byte, end_byte
             FROM symbols INDEXED BY symbols_name_idx
             WHERE file_id = ?1
               AND name = ?2 COLLATE NOCASE
               AND name = ?2 COLLATE BINARY
               AND ?3 IS NULL
               AND ?4 IS NULL
             ORDER BY start_byte, id
             LIMIT 2"
        };
        let (parent, leaf_name) = qualified.unzip();
        let mut stmt = self.conn.prepare_cached(sql)?;
        let rows = stmt.query_map(
            params![file_id, name, parent, leaf_name],
            Storage::map_symbol,
        )?;
        let matches = rows.collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(crate::symbol_identity::resolve_symbol_matches(
            matches.into_iter(),
        ))
    }

    pub(crate) fn find_document_heading(
        &self,
        file_id: i64,
        name: &str,
        occurrence: usize,
    ) -> Result<Option<SymbolRecord>> {
        let offset = usize_to_i64(occurrence.saturating_sub(1))?;
        Ok(self
            .conn
            .query_row(
                "SELECT id, file_id, name, kind, parent, signature, start_line, end_line, start_byte, end_byte
                     FROM symbols
                     WHERE file_id = ?1
                       AND kind IN (
                           'markdown_heading',
                           'latex_section',
                           'latex_subsection',
                           'latex_subsubsection',
                           'latex_paragraph'
                       )
                       AND (name = ?2 OR signature = ?2)
                     ORDER BY start_byte, id
                     LIMIT 1 OFFSET ?3",
                params![file_id, name, offset],
                Storage::map_symbol,
            )
            .optional()?)
    }

    #[cfg(test)]
    pub(crate) fn find_enclosing_symbol(
        &self,
        file_id: i64,
        line: usize,
    ) -> Result<Option<SymbolRecord>> {
        let line = usize_to_i64(line)?;
        Ok(self
            .conn
            .query_row(
                "SELECT id, file_id, name, kind, parent, signature, start_line, end_line, start_byte, end_byte
                     FROM symbols
                     WHERE file_id = ?1 AND start_line <= ?2 AND end_line >= ?2
                     ORDER BY (end_line - start_line), start_byte
                     LIMIT 1",
                params![file_id, line],
                Storage::map_symbol,
            )
            .optional()?)
    }

    /// Find the narrowest enclosing symbol for every requested file/line pair.
    ///
    /// Results preserve input order and cardinality; `None` marks a location with
    /// no enclosing declaration. Duplicate locations share one storage lookup.
    pub(crate) fn find_enclosing_symbols_batch(
        &self,
        locations: &[(i64, usize)],
    ) -> Result<Vec<Option<SymbolRecord>>> {
        if locations.is_empty() {
            return Ok(Vec::new());
        }
        let mut unique_indices = HashMap::new();
        let mut unique_locations = Vec::new();
        let location_mapping = locations
            .iter()
            .map(|location| {
                *unique_indices.entry(*location).or_insert_with(|| {
                    let unique_index = unique_locations.len();
                    unique_locations.push(*location);
                    unique_index
                })
            })
            .collect::<Vec<_>>();
        let input = unique_locations
            .iter()
            .map(|(file_id, line)| serde_json::json!({ "file_id": file_id, "line": line }))
            .collect::<Vec<_>>();
        let input = serde_json::to_string(&input)?;
        let mut stmt = self.conn.prepare_cached(
            "WITH requested AS (
                 SELECT CAST(key AS INTEGER) AS request_index,
                        CAST(value ->> 'file_id' AS INTEGER) AS file_id,
                        CAST(value ->> 'line' AS INTEGER) AS line
                 FROM json_each(?1)
             )
             SELECT requested.request_index,
                    symbols.id, symbols.file_id, symbols.name, symbols.kind,
                    symbols.parent, symbols.signature,
                    symbols.start_line, symbols.end_line,
                    symbols.start_byte, symbols.end_byte
             FROM requested
             JOIN symbols ON symbols.id = (
                 SELECT enclosing.id
                 FROM symbols AS enclosing
                 WHERE enclosing.file_id = requested.file_id
                   AND enclosing.start_line <= requested.line
                   AND enclosing.end_line >= requested.line
                 ORDER BY (enclosing.end_line - enclosing.start_line), enclosing.start_byte
                 LIMIT 1
             )
             ORDER BY requested.request_index",
        )?;
        let rows = stmt.query_map(params![input], |row| {
            let request_index = i64_to_usize(row.get(0)?)?;
            let symbol = SymbolRecord {
                id: row.get(1)?,
                file_id: row.get(2)?,
                name: row.get(3)?,
                kind: row.get(4)?,
                parent: row.get(5)?,
                signature: row.get(6)?,
                start_line: i64_to_usize(row.get(7)?)?,
                end_line: i64_to_usize(row.get(8)?)?,
                start_byte: i64_to_usize(row.get(9)?)?,
                end_byte: i64_to_usize(row.get(10)?)?,
            };
            Ok((request_index, symbol))
        })?;
        let mut symbols = vec![None; unique_locations.len()];
        for row in rows {
            let (request_index, symbol) = row?;
            if let Some(slot) = symbols.get_mut(request_index) {
                *slot = Some(symbol);
            }
        }
        Ok(location_mapping
            .into_iter()
            .map(|unique_index| symbols[unique_index].clone())
            .collect())
    }

    pub fn get_references_for_file(
        &self,
        file_id: i64,
        max_results: usize,
    ) -> Result<Vec<ReferenceRecord>> {
        let limit = bounded_limit(max_results);
        let mut stmt = self.conn.prepare_cached(
            "SELECT id, file_id, name, kind, role, enclosing_symbol, start_line, end_line, start_byte, end_byte FROM symbol_refs WHERE file_id = ?1 ORDER BY start_byte LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![file_id, limit], Storage::map_reference)?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    pub fn get_imports_for_file(
        &self,
        file_id: i64,
        max_results: usize,
    ) -> Result<Vec<ImportRecord>> {
        self.get_imports_for_file_page(file_id, max_results, 0)
    }

    pub(crate) fn get_imports_for_file_page(
        &self,
        file_id: i64,
        max_results: usize,
        offset: usize,
    ) -> Result<Vec<ImportRecord>> {
        let limit = bounded_limit(max_results);
        let offset = i64::try_from(offset).unwrap_or(i64::MAX);
        let mut stmt = self.conn.prepare_cached(
            "SELECT id, file_id, raw_target, resolved_path, line
                 FROM imports
                 WHERE file_id = ?1
                 ORDER BY line, id
                 LIMIT ?2 OFFSET ?3",
        )?;
        let rows = stmt.query_map(params![file_id, limit, offset], Storage::map_import)?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    pub(crate) fn count_imports_for_file(&self, file_id: i64) -> Result<usize> {
        self.conn
            .query_row(
                "SELECT COUNT(*) FROM imports WHERE file_id = ?1",
                params![file_id],
                |row| i64_to_usize(row.get(0)?),
            )
            .map_err(Into::into)
    }
}
