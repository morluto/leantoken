impl GenerationReadTransaction {
    /// Read metadata from this generation transaction's pinned repository snapshot.
    pub fn meta(&self) -> Result<MetaRecord> {
        self.conn
            .query_row(
                "SELECT schema_version, index_version, config_hash, repository_generation,
                        repository_identity, database_incarnation_id
                 FROM meta WHERE id = 1",
                [],
                |row| {
                    Ok(MetaRecord {
                        schema_version: row.get(0)?,
                        index_version: row.get(1)?,
                        config_hash: row.get(2)?,
                        repository_generation: i64_to_u64(row.get(3)?)?,
                        repository_identity: row.get(4)?,
                        database_incarnation_id: row.get(5)?,
                    })
                },
            )
            .map_err(Into::into)
    }

    /// Return the repository generation pinned by this generation transaction.
    pub fn repository_generation(&self) -> Result<u64> {
        let generation: i64 = self.conn.query_row(
            "SELECT repository_generation FROM meta WHERE id = 1",
            [],
            |row| row.get(0),
        )?;
        Ok(i64_to_u64(generation)?)
    }

    pub(crate) fn file_count(&self) -> Result<usize> {
        Ok(i64_to_usize(self.conn.query_row(
            "SELECT COUNT(*) FROM files",
            [],
            |row| row.get(0),
        )?)?)
    }

    pub(crate) fn whole_file_source_tokens(
        &self,
        paths: &[String],
        tokenizer: &str,
    ) -> Result<Option<usize>> {
        if paths.is_empty() {
            return Ok(Some(0));
        }
        let input = serde_json::to_string(paths)?;
        let tokens: Option<i64> = self.conn.query_row(
            "WITH requested(path) AS (
                 SELECT DISTINCT CAST(value AS TEXT) FROM json_each(?1)
             )
             SELECT CASE
                 WHEN COUNT(*) = SUM(files.source_tokenizer = ?2)
                     THEN COALESCE(SUM(files.source_token_count), 0)
                 ELSE NULL
             END
             FROM requested
             JOIN files ON files.path = requested.path",
            params![input, tokenizer],
            |row| row.get(0),
        )?;
        Ok(tokens.map(i64_to_usize).transpose()?)
    }
}
use super::*;
