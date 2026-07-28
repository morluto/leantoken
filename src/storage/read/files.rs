impl ReadSession {
    /// Return a row-id keyset page from this session's pinned snapshot.
    ///
    /// Use the final record's `id` as the next cursor. Cursors are storage-layer
    /// values and should not be exposed as service cursors without binding them
    /// to the repository generation and operation parameters.
    pub fn list_files(&self, max_results: usize, cursor: Option<i64>) -> Result<Vec<FileRecord>> {
        let limit = bounded_limit(max_results);
        let mut stmt = self.conn.prepare_cached(
            "SELECT id, path, language, size_bytes, modified_ns, content_hash, generation, structurally_complete FROM files WHERE (?1 IS NULL OR id > ?1) ORDER BY id LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![cursor, limit], Storage::map_file)?;
        let mut files = Vec::new();
        for row in rows {
            files.push(row?);
        }
        Ok(files)
    }

    /// Return a lean path projection page for fuzzy find scans.
    ///
    /// Use the final record's `id` as the next cursor. Omits hash, generation,
    /// and modified metadata that find does not need.
    pub(crate) fn list_file_paths(
        &self,
        max_results: usize,
        cursor: Option<i64>,
    ) -> Result<Vec<FilePathRecord>> {
        let limit = bounded_limit(max_results);
        let mut stmt = self.conn.prepare_cached(
            "SELECT id, path, language, size_bytes FROM files
             WHERE (?1 IS NULL OR id > ?1)
             ORDER BY id
             LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![cursor, limit], |row| {
            Ok(FilePathRecord {
                id: row.get(0)?,
                path: row.get(1)?,
                language: row.get(2)?,
                size_bytes: i64_to_u64(row.get(3)?)?,
            })
        })?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// SQL-paged file paths matching one or two SQLite `GLOB` patterns.
    ///
    /// Uses the `path_entries` file projection with a path keyset cursor. Callers
    /// map globset patterns to SQL GLOB forms; brace expansion and other
    /// unexpressible patterns keep the in-process globset fallback.
    pub(crate) fn list_glob_paths(
        &self,
        primary: &str,
        alternate: Option<&str>,
        after: Option<&str>,
        max_results: usize,
    ) -> Result<Vec<PathRecord>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT path_entries.path, files.language, files.size_bytes
             FROM path_entries
             JOIN files ON files.id = path_entries.file_id
             WHERE path_entries.kind = 1
               AND (path_entries.path GLOB ?1
                    OR (?2 IS NOT NULL AND path_entries.path GLOB ?2))
               AND (?3 IS NULL OR path_entries.path > ?3)
             ORDER BY path_entries.path
             LIMIT ?4",
        )?;
        let rows = stmt.query_map(
            params![primary, alternate, after, bounded_limit(max_results)],
            |row| {
                Ok(PathRecord {
                    path: row.get(0)?,
                    is_directory: false,
                    language: row.get(1)?,
                    size_bytes: row.get::<_, Option<i64>>(2)?.map(i64_to_u64).transpose()?,
                })
            },
        )?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    pub(crate) fn regex_scan_files(&self, max_results: usize) -> Result<Vec<(FileRecord, usize)>> {
        let limit = bounded_limit(max_results);
        let mut stmt = self.conn.prepare_cached(
            "SELECT f.id, f.path, f.language, f.size_bytes, f.modified_ns,
                    f.content_hash, f.generation, f.structurally_complete,
                    COUNT(c.id)
             FROM files f
             LEFT JOIN chunks c ON c.file_id = f.id
             GROUP BY f.id
             ORDER BY f.id
             LIMIT ?1",
        )?;
        let rows = stmt.query_map(params![limit], |row| {
            Ok((
                Storage::map_file(row)?,
                i64_to_usize(row.get::<_, i64>(8)?)?,
            ))
        })?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// Read a lexicographically ordered keyset page from the relational path projection.
    pub(crate) fn list_tree_paths(
        &self,
        root: &str,
        max_depth: usize,
        after: Option<&str>,
        max_results: usize,
    ) -> Result<Vec<PathRecord>> {
        let root_depth = root.split('/').filter(|part| !part.is_empty()).count();
        let depth_limit = i64::try_from(root_depth.saturating_add(max_depth)).unwrap_or(i64::MAX);
        let mut stmt = self.conn.prepare_cached(
            "SELECT path_entries.path, path_entries.kind, files.language, files.size_bytes
             FROM path_entries
             LEFT JOIN files ON files.id = path_entries.file_id
             WHERE (?1 = '' OR path_entries.path = ?1
                    OR substr(path_entries.path, 1, length(?1) + 1) = ?1 || '/')
               AND path_entries.depth <= ?2
               AND (?3 IS NULL OR path_entries.path > ?3)
             ORDER BY path_entries.path
             LIMIT ?4",
        )?;
        let rows = stmt.query_map(
            params![root, depth_limit, after, bounded_limit(max_results)],
            |row| {
                let kind: i64 = row.get(1)?;
                Ok(PathRecord {
                    path: row.get(0)?,
                    is_directory: kind == 0,
                    language: row.get(2)?,
                    size_bytes: row.get::<_, Option<i64>>(3)?.map(i64_to_u64).transpose()?,
                })
            },
        )?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }
}
