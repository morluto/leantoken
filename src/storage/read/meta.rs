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

    pub(crate) fn token_savings(
        &self,
        tokenizer: &str,
    ) -> Result<HashMap<String, TokenSavingsRecord>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT operation, tracked_requests, response_tracked_requests,
                    response_baseline_requests, baseline_source_tokens,
                    response_baseline_source_tokens, emitted_source_tokens,
                    estimated_source_tokens_saved, response_source_tokens,
                    path_and_metadata_tokens, protocol_tokens,
                    total_response_tokens, receipt_suppressed_exact,
                    receipt_suppressed_overlap,
                    expected_hash_not_modified_responses,
                    expected_hash_suppressed_source_tokens,
                    useful_requests, incomplete_requests,
                    unsupported_requests, hash_suppressed_requests
             FROM token_savings
             WHERE tokenizer = ?1
             ORDER BY operation",
        )?;
        let rows = stmt.query_map(params![tokenizer], |row| {
            Ok((
                row.get::<_, String>(0)?,
                TokenSavingsRecord {
                    tracked_requests: i64_to_u64(row.get(1)?)?,
                    response_tracked_requests: i64_to_u64(row.get(2)?)?,
                    response_baseline_requests: i64_to_u64(row.get(3)?)?,
                    baseline_source_tokens: i64_to_u64(row.get(4)?)?,
                    response_baseline_source_tokens: i64_to_u64(row.get(5)?)?,
                    emitted_source_tokens: i64_to_u64(row.get(6)?)?,
                    estimated_source_tokens_saved: i64_to_u64(row.get(7)?)?,
                    response_source_tokens: i64_to_u64(row.get(8)?)?,
                    path_and_metadata_tokens: i64_to_u64(row.get(9)?)?,
                    protocol_tokens: i64_to_u64(row.get(10)?)?,
                    total_response_tokens: i64_to_u64(row.get(11)?)?,
                    receipt_suppressed_exact: i64_to_u64(row.get(12)?)?,
                    receipt_suppressed_overlap: i64_to_u64(row.get(13)?)?,
                    expected_hash_not_modified_responses: i64_to_u64(row.get(14)?)?,
                    expected_hash_suppressed_source_tokens: i64_to_u64(row.get(15)?)?,
                    useful_requests: i64_to_u64(row.get(16)?)?,
                    incomplete_requests: i64_to_u64(row.get(17)?)?,
                    unsupported_requests: i64_to_u64(row.get(18)?)?,
                    hash_suppressed_requests: i64_to_u64(row.get(19)?)?,
                },
            ))
        })?;
        Ok(rows.collect::<std::result::Result<HashMap<_, _>, _>>()?)
    }

    pub(crate) fn service_failures(&self, tokenizer: &str) -> Result<Vec<ServiceFailureRecord>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT operation, error_category, failed_requests
             FROM service_failures
             WHERE tokenizer = ?1
             ORDER BY operation, error_category",
        )?;
        let rows = stmt.query_map(params![tokenizer], |row| {
            Ok(ServiceFailureRecord {
                operation: row.get(0)?,
                error_category: row.get(1)?,
                failed_requests: i64_to_u64(row.get(2)?)?,
            })
        })?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }
}
use super::*;
