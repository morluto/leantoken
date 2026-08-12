impl ReadSession {
    pub fn counts(&self) -> Result<StorageCounts> {
        let files = i64_to_usize(self.conn.query_row(
            "SELECT count(*) FROM files",
            [],
            |row| row.get(0),
        )?)?;
        let chunks = i64_to_usize(self.conn.query_row(
            "SELECT count(*) FROM chunks",
            [],
            |row| row.get(0),
        )?)?;
        let symbols = i64_to_usize(self.conn.query_row(
            "SELECT count(*) FROM symbols",
            [],
            |row| row.get(0),
        )?)?;
        let source_bytes = i64_to_u64(self.conn.query_row(
            "SELECT coalesce(sum(size_bytes), 0) FROM files",
            [],
            |row| row.get::<_, i64>(0),
        )?)?;
        let mut stmt = self.conn.prepare_cached(
            "SELECT language, count(*) FROM files WHERE language IS NOT NULL GROUP BY language ORDER BY language",
        )?;
        let languages = stmt
            .query_map([], |row| Ok((row.get(0)?, i64_to_usize(row.get(1)?)?)))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(StorageCounts {
            files,
            chunks,
            symbols,
            source_bytes,
            languages,
        })
    }

    pub(crate) fn search_fts(
        &self,
        table: FtsTable,
        query: &str,
        max_results: usize,
        offset: usize,
    ) -> Result<Vec<ChunkHit>> {
        let limit = bounded_limit(max_results);
        let offset = i64::try_from(offset).unwrap_or(i64::MAX);
        let table_name = table.as_str();
        let sql = format!(
            "SELECT c.id, c.file_id, f.path, c.content, c.start_line, c.end_line, c.start_byte, c.end_byte, c.token_count, f.generation, bm25({table_name}) as score \
             FROM {table_name} \
             JOIN chunks c ON {table_name}.rowid = c.rowid \
             JOIN files f ON c.file_id = f.id \
             WHERE {table_name} MATCH ?1 \
             ORDER BY bm25({table_name}), f.path, c.start_byte \
             LIMIT ?2 OFFSET ?3"
        );
        let mut stmt = self.conn.prepare_cached(&sql)?;
        let rows = stmt.query_map(params![query, limit, offset], Storage::map_chunk_hit)?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }
}
use super::*;
