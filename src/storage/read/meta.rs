use super::*;

impl ReadSession {
    /// Read metadata from this session's pinned repository snapshot.
    pub fn meta(&self) -> Result<MetaRecord> {
        self.conn
            .query_row(
                "SELECT schema_version, index_version, config_hash, repository_generation FROM meta WHERE id = 1",
                [],
                |row| {
                    Ok(MetaRecord {
                        schema_version: row.get(0)?,
                        index_version: row.get(1)?,
                        config_hash: row.get(2)?,
                        repository_generation: i64_to_u64(row.get(3)?)?,
                    })
                },
            )
            .map_err(Into::into)
    }

    /// Return the repository generation pinned by this session.
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
}
