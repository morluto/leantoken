use crate::error::RetryableOperation;
use crate::query_receipt::{
    ExactQueryPredicate, MAX_QUERY_RECEIPT_ID_BYTES, MAX_QUERY_RECEIPTS,
    MAX_TOTAL_QUERY_RECEIPT_BYTES, QUERY_RECEIPT_TOUCH_INTERVAL_MILLIS, QUERY_RECEIPT_TTL_MILLIS,
    QueryPartition, QueryReceiptRecord, StoredQueryReceipt, format_query_receipt_id,
    parse_query_receipt_id,
};

#[derive(Debug)]
pub(crate) struct QueryReceiptUsage {
    pub(crate) namespace: String,
    pub(crate) next_access_sequence: i64,
    pub(crate) receipt_count: usize,
    pub(crate) logical_bytes: usize,
}

struct QueryReceiptRow {
    repository_identity: String,
    repository_generation: u64,
    config_hash: String,
    semantics_version: u64,
    predicate_json: String,
    predicate_blake3: String,
    partition_blake3: String,
    partition_file_count: usize,
    match_count: usize,
    result_blake3: String,
    created_unix_millis: i64,
    last_access_unix_millis: i64,
    expires_unix_millis: i64,
}

impl Storage {
    pub(crate) fn persist_query_receipt(&self, record: &QueryReceiptRecord) -> Result<String> {
        self.persist_query_receipt_at(record, unix_millis(SystemTime::now()))
    }

    pub(crate) fn touch_query_receipt(&self, receipt_id: &str) -> Result<()> {
        self.touch_query_receipt_with_clock(receipt_id, || unix_millis(SystemTime::now()))
    }

    #[cfg(test)]
    pub(crate) fn touch_query_receipt_at(
        &self,
        receipt_id: &str,
        now_unix_millis: i64,
    ) -> Result<()> {
        self.touch_query_receipt_with_clock(receipt_id, || now_unix_millis)
    }

    fn touch_query_receipt_with_clock<F>(&self, receipt_id: &str, now_unix_millis: F) -> Result<()>
    where
        F: FnOnce() -> i64,
    {
        let mut conn = self
            .writer
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        // Sample live time only after SQLite has serialized writers,
        // otherwise an earlier sample can commit after a later one and
        // look like a clock rollback.
        let now_unix_millis = now_unix_millis();
        if now_unix_millis < 0 {
            return Err(Error::OperationFailure(
                "system clock precedes the Unix epoch".into(),
            ));
        }
        let expires_unix_millis = now_unix_millis
            .checked_add(QUERY_RECEIPT_TTL_MILLIS)
            .ok_or_else(|| Error::OperationFailure("query receipt expiry overflow".into()))?;
        prune_expired_query_receipts(&tx, now_unix_millis)?;
        let usage = query_receipt_usage(&tx)?;
        let row_id = parse_query_receipt_id(receipt_id, &usage.namespace);
        if let Some(row_id) = row_id {
            let access_sequence =
                next_query_receipt_access_sequence(&tx, usage.next_access_sequence)?;
            let updated = tx.execute(
                "UPDATE query_coverage_receipts
                 SET last_access_unix_millis = ?1,
                     expires_unix_millis = ?2,
                     access_sequence = ?3
                 WHERE id = ?4",
                params![
                    now_unix_millis,
                    expires_unix_millis,
                    access_sequence,
                    row_id
                ],
            )?;
            if updated == 0 {
                return Err(Error::UnknownQueryReceipt(receipt_id.to_owned()));
            }
        }
        tx.commit()?;
        Ok(())
    }

    pub(crate) fn persist_query_receipt_at(
        &self,
        record: &QueryReceiptRecord,
        now_unix_millis: i64,
    ) -> Result<String> {
        if now_unix_millis < 0 {
            return Err(Error::OperationFailure(
                "system clock precedes the Unix epoch".into(),
            ));
        }
        let expires_unix_millis = now_unix_millis
            .checked_add(QUERY_RECEIPT_TTL_MILLIS)
            .ok_or_else(|| Error::OperationFailure("query receipt expiry overflow".into()))?;
        let predicate_json = record.predicate.serialized()?;
        let mut conn = self
            .writer
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        prune_expired_query_receipts(&tx, now_unix_millis)?;
        let (repository_identity, repository_generation, config_hash): (String, u64, String) = tx
            .query_row(
            "SELECT repository_identity, repository_generation, config_hash
                 FROM meta
                 WHERE id = 1",
            [],
            |row| Ok((row.get(0)?, i64_to_u64(row.get(1)?)?, row.get(2)?)),
        )?;
        if repository_generation != record.repository_generation
            || config_hash != record.config_hash
        {
            return Err(Error::RetryableConflict(RetryableOperation::Retrieval));
        }
        let logical_bytes = record.logical_bytes(&repository_identity)?;
        if logical_bytes > MAX_TOTAL_QUERY_RECEIPT_BYTES {
            return Err(Error::LimitExceeded);
        }
        let mut usage = query_receipt_usage(&tx)?;
        let existing = tx
            .query_row(
                "SELECT id, last_access_unix_millis
                 FROM query_coverage_receipts
                 WHERE repository_identity = ?1
                   AND repository_generation = ?2
                   AND config_hash = ?3
                   AND semantics_version = ?4
                   AND predicate_json = ?5
                   AND predicate_blake3 = ?6
                   AND partition_blake3 = ?7
                   AND partition_file_count = ?8
                   AND match_count = ?9
                   AND result_blake3 = ?10
                 ORDER BY id
                 LIMIT 1",
                params![
                    repository_identity,
                    u64_to_i64(record.repository_generation)?,
                    record.config_hash,
                    u64_to_i64(crate::query_receipt::search_semantics_fingerprint())?,
                    predicate_json,
                    record.predicate_blake3,
                    record.partition.digest,
                    usize_to_i64(record.partition.file_count)?,
                    usize_to_i64(record.match_count)?,
                    record.result_blake3,
                ],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
            )
            .optional()?;
        if let Some((row_id, last_access_unix_millis)) = existing {
            if now_unix_millis.saturating_sub(last_access_unix_millis)
                >= QUERY_RECEIPT_TOUCH_INTERVAL_MILLIS
            {
                let access_sequence =
                    next_query_receipt_access_sequence(&tx, usage.next_access_sequence)?;
                tx.execute(
                    "UPDATE query_coverage_receipts
                     SET last_access_unix_millis = ?1,
                         expires_unix_millis = ?2,
                         access_sequence = ?3
                     WHERE id = ?4",
                    params![
                        now_unix_millis,
                        expires_unix_millis,
                        access_sequence,
                        row_id
                    ],
                )?;
            }
            let receipt_id = format_query_receipt_id(&usage.namespace, row_id);
            tx.commit()?;
            return Ok(receipt_id);
        }

        while usage.receipt_count >= MAX_QUERY_RECEIPTS
            || usage.logical_bytes.saturating_add(logical_bytes) > MAX_TOTAL_QUERY_RECEIPT_BYTES
        {
            if !evict_oldest_query_receipt(&tx)? {
                return Err(Error::OperationFailure(
                    "query receipt quota could not evict a receipt".into(),
                ));
            }
            usage = query_receipt_usage(&tx)?;
        }
        let access_sequence = next_query_receipt_access_sequence(&tx, usage.next_access_sequence)?;
        tx.execute(
            "INSERT INTO query_coverage_receipts(
                repository_identity,
                repository_generation,
                config_hash,
                semantics_version,
                predicate_json,
                predicate_blake3,
                partition_blake3,
                partition_file_count,
                match_count,
                result_blake3,
                created_unix_millis,
                last_access_unix_millis,
                expires_unix_millis,
                access_sequence,
                logical_bytes
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?11, ?12, ?13, ?14)",
            params![
                repository_identity,
                u64_to_i64(record.repository_generation)?,
                record.config_hash,
                u64_to_i64(crate::query_receipt::search_semantics_fingerprint())?,
                predicate_json,
                record.predicate_blake3,
                record.partition.digest,
                usize_to_i64(record.partition.file_count)?,
                usize_to_i64(record.match_count)?,
                record.result_blake3,
                now_unix_millis,
                expires_unix_millis,
                access_sequence,
                usize_to_i64(logical_bytes)?,
            ],
        )?;
        let row_id = tx.last_insert_rowid();
        let receipt_id = format_query_receipt_id(&usage.namespace, row_id);
        tx.commit()?;
        Ok(receipt_id)
    }
}

impl ReadSession {
    pub(crate) fn load_query_receipt(&self, requested_id: &str) -> Result<StoredQueryReceipt> {
        self.load_query_receipt_at(requested_id, unix_millis(SystemTime::now()))
    }

    pub(crate) fn load_query_receipt_at(
        &self,
        requested_id: &str,
        now_unix_millis: i64,
    ) -> Result<StoredQueryReceipt> {
        if requested_id.len() > MAX_QUERY_RECEIPT_ID_BYTES {
            return Err(Error::InputTooLong {
                field: "query receipt_id",
                max_bytes: MAX_QUERY_RECEIPT_ID_BYTES,
            });
        }
        if now_unix_millis < 0 {
            return Err(Error::UnknownQueryReceipt(requested_id.to_owned()));
        }
        let namespace: String = self.conn.query_row(
            "SELECT namespace FROM query_coverage_receipt_usage WHERE id = 1",
            [],
            |row| row.get(0),
        )?;
        let Some(row_id) = parse_query_receipt_id(requested_id, &namespace) else {
            return Err(Error::UnknownQueryReceipt(requested_id.to_owned()));
        };
        let repository_identity: String = self.conn.query_row(
            "SELECT repository_identity FROM meta WHERE id = 1",
            [],
            |row| row.get(0),
        )?;
        let row: Option<QueryReceiptRow> = self
            .conn
            .query_row(
                "SELECT repository_identity,
                        repository_generation,
                        config_hash,
                        semantics_version,
                        predicate_json,
                        predicate_blake3,
                        partition_blake3,
                        partition_file_count,
                        match_count,
                        result_blake3,
                        created_unix_millis,
                        last_access_unix_millis,
                        expires_unix_millis
                 FROM query_coverage_receipts
                 WHERE id = ?1",
                [row_id],
                |row| {
                    Ok(QueryReceiptRow {
                        repository_identity: row.get(0)?,
                        repository_generation: i64_to_u64(row.get(1)?)?,
                        config_hash: row.get(2)?,
                        semantics_version: i64_to_u64(row.get(3)?)?,
                        predicate_json: row.get(4)?,
                        predicate_blake3: row.get(5)?,
                        partition_blake3: row.get(6)?,
                        partition_file_count: i64_to_usize(row.get(7)?)?,
                        match_count: i64_to_usize(row.get(8)?)?,
                        result_blake3: row.get(9)?,
                        created_unix_millis: row.get(10)?,
                        last_access_unix_millis: row.get(11)?,
                        expires_unix_millis: row.get(12)?,
                    })
                },
            )
            .optional()?;
        let Some(QueryReceiptRow {
            repository_identity: receipt_repository_identity,
            repository_generation,
            config_hash,
            semantics_version,
            predicate_json,
            predicate_blake3,
            partition_blake3,
            partition_file_count,
            match_count,
            result_blake3,
            created_unix_millis,
            last_access_unix_millis,
            expires_unix_millis,
        }) = row
        else {
            return Err(Error::UnknownQueryReceipt(requested_id.to_owned()));
        };
        if receipt_repository_identity != repository_identity
            || semantics_version != crate::query_receipt::search_semantics_fingerprint()
            || created_unix_millis > now_unix_millis
            || last_access_unix_millis > now_unix_millis
            || expires_unix_millis <= now_unix_millis
        {
            return Err(Error::UnknownQueryReceipt(requested_id.to_owned()));
        }
        let predicate: ExactQueryPredicate = serde_json::from_str(&predicate_json)
            .map_err(|_| Error::UnknownQueryReceipt(requested_id.to_owned()))?;
        if predicate.digest()? != predicate_blake3 {
            return Err(Error::UnknownQueryReceipt(requested_id.to_owned()));
        }
        Ok(StoredQueryReceipt {
            receipt_id: requested_id.to_owned(),
            repository_generation,
            config_hash,
            predicate,
            predicate_blake3,
            partition: QueryPartition {
                digest: partition_blake3,
                file_count: partition_file_count,
            },
            match_count,
            result_blake3,
        })
    }

    pub(crate) fn exact_query_partition(
        &self,
        mut allows_path: impl FnMut(&str) -> bool,
        mut check: impl FnMut() -> Result<()>,
    ) -> Result<QueryPartition> {
        let mut statement = self
            .conn
            .prepare("SELECT path, content_hash FROM files ORDER BY path")?;
        let mut rows = statement.query([])?;
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"leantoken-exact-query-partition-v1\0");
        let mut file_count = 0usize;
        while let Some(row) = rows.next()? {
            check()?;
            let path: String = row.get(0)?;
            if !allows_path(&path) {
                continue;
            }
            let content_hash: String = row.get(1)?;
            hash_query_partition_bytes(&mut hasher, path.as_bytes());
            hash_query_partition_bytes(&mut hasher, content_hash.as_bytes());
            file_count = file_count
                .checked_add(1)
                .ok_or_else(|| Error::OperationFailure("query partition count overflow".into()))?;
        }
        hasher.update(&(file_count as u64).to_le_bytes());
        Ok(QueryPartition {
            digest: hasher.finalize().to_hex().to_string(),
            file_count,
        })
    }
}

pub(crate) fn query_receipt_usage(tx: &Transaction<'_>) -> Result<QueryReceiptUsage> {
    tx.query_row(
        "SELECT namespace, next_access_sequence, receipt_count, logical_bytes
         FROM query_coverage_receipt_usage
         WHERE id = 1",
        [],
        |row| {
            Ok(QueryReceiptUsage {
                namespace: row.get(0)?,
                next_access_sequence: row.get(1)?,
                receipt_count: i64_to_usize(row.get(2)?)?,
                logical_bytes: i64_to_usize(row.get(3)?)?,
            })
        },
    )
    .map_err(Into::into)
}

pub(crate) fn next_query_receipt_access_sequence(
    tx: &Transaction<'_>,
    current: i64,
) -> Result<i64> {
    let next = current
        .checked_add(1)
        .ok_or_else(|| Error::OperationFailure("query receipt access sequence overflow".into()))?;
    let updated = tx.execute(
        "UPDATE query_coverage_receipt_usage
         SET next_access_sequence = ?1
         WHERE id = 1 AND next_access_sequence = ?2",
        params![next, current],
    )?;
    if updated != 1 {
        return Err(Error::OperationFailure(
            "query receipt access sequence changed unexpectedly".into(),
        ));
    }
    Ok(next)
}

pub(crate) fn prune_expired_query_receipts(
    tx: &Transaction<'_>,
    now_unix_millis: i64,
) -> Result<()> {
    tx.execute(
        "DELETE FROM query_coverage_receipts
         WHERE expires_unix_millis <= ?1
            OR created_unix_millis > ?1
            OR last_access_unix_millis > ?1",
        [now_unix_millis],
    )?;
    Ok(())
}

pub(crate) fn evict_oldest_query_receipt(tx: &Transaction<'_>) -> Result<bool> {
    let oldest = tx
        .query_row(
            "SELECT id
             FROM query_coverage_receipts
             ORDER BY access_sequence, id
             LIMIT 1",
            [],
            |row| row.get::<_, i64>(0),
        )
        .optional()?;
    let Some(oldest) = oldest else {
        return Ok(false);
    };
    Ok(tx.execute(
        "DELETE FROM query_coverage_receipts WHERE id = ?1",
        [oldest],
    )? == 1)
}

pub(crate) fn hash_query_partition_bytes(hasher: &mut blake3::Hasher, bytes: &[u8]) {
    hasher.update(&(bytes.len() as u64).to_le_bytes());
    hasher.update(bytes);
}
use super::*;
