use super::*;

impl ReadSession {
    /// Return a row-id keyset page from this session's pinned snapshot.
    pub fn list_files(&self, max_results: usize, cursor: Option<i64>) -> Result<Vec<FileRecord>> {
        let limit = bounded_limit(max_results);
        let mut stmt = self.conn.prepare_cached(
            "SELECT id, path, language, size_bytes, modified_ns, content_hash, generation, structurally_complete FROM files WHERE (?1 IS NULL OR id > ?1) ORDER BY id LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![cursor, limit], Storage::map_file)?;
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
}
