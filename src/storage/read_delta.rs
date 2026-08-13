use crate::read_delta::{
    MAX_READ_DELTA_BASE_BYTES, MAX_READ_DELTA_BASES, MAX_TOTAL_READ_DELTA_BASE_BYTES,
    READ_DELTA_BASE_TTL_MILLIS, ReadDeltaBase,
};

pub(crate) const READ_DELTA_TOUCH_INTERVAL_MILLIS: i64 = 60 * 1_000;

impl Storage {
    pub(crate) fn read_delta_base(
        &self,
        target_key: &str,
        content_hash: &str,
    ) -> Result<Option<ReadDeltaBase>> {
        self.read_delta_base_at(
            target_key,
            Some(content_hash),
            unix_millis(SystemTime::now()),
        )
    }

    pub(crate) fn latest_read_delta_base(
        &self,
        target_key: &str,
    ) -> Result<Option<(String, ReadDeltaBase)>> {
        let Some(base) =
            self.read_delta_base_row_at(target_key, None, unix_millis(SystemTime::now()))?
        else {
            return Ok(None);
        };
        Ok(Some((base.0, base.1)))
    }

    pub(crate) fn persist_read_delta_base(
        &self,
        target_key: &str,
        content_hash: &str,
        base: &ReadDeltaBase,
    ) -> Result<bool> {
        self.persist_read_delta_base_at(
            target_key,
            content_hash,
            base,
            unix_millis(SystemTime::now()),
        )
    }

    pub(crate) fn read_delta_base_at(
        &self,
        target_key: &str,
        content_hash: Option<&str>,
        now_unix_millis: i64,
    ) -> Result<Option<ReadDeltaBase>> {
        Ok(self
            .read_delta_base_row_at(target_key, content_hash, now_unix_millis)?
            .map(|(_, base)| base))
    }

    pub(crate) fn read_delta_base_row_at(
        &self,
        target_key: &str,
        content_hash: Option<&str>,
        now_unix_millis: i64,
    ) -> Result<Option<(String, ReadDeltaBase)>> {
        // Fast path: use a reader-pool connection without holding the writer
        // lock. Only fall back to the writer lock when a mutation is needed.
        let mut read_conn = self.readers.get()?;
        let read_tx = read_conn.transaction()?;
        let row = load_read_delta_base(&read_tx, target_key, content_hash)?;
        let Some((hash, base, created, last_access)) = row else {
            return Ok(None);
        };
        let needs_stale_delete = created > now_unix_millis || last_access > now_unix_millis;
        let needs_touch =
            now_unix_millis.saturating_sub(last_access) < READ_DELTA_TOUCH_INTERVAL_MILLIS;
        if needs_touch && !needs_stale_delete {
            if crate::text::hash(&base.content) != hash {
                return Err(Error::OperationFailure(
                    "persistent read delta base hash mismatch".into(),
                ));
            }
            return Ok(Some((hash, base)));
        }
        drop(read_tx);
        drop(read_conn);

        // Slow path: acquire the writer lock for mutations.
        let mut connection = self
            .writer
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        prune_read_delta_bases(&transaction, now_unix_millis)?;
        let row = load_read_delta_base(&transaction, target_key, content_hash)?;
        let Some((hash, base, created, last_access)) = row else {
            transaction.commit()?;
            return Ok(None);
        };
        if created > now_unix_millis || last_access > now_unix_millis {
            transaction.execute(
                "DELETE FROM read_delta_bases
                 WHERE target_key = ?1 AND content_hash = ?2",
                params![target_key, hash],
            )?;
            transaction.commit()?;
            return Ok(None);
        }
        if crate::text::hash(&base.content) != hash {
            return Err(Error::OperationFailure(
                "persistent read delta base hash mismatch".into(),
            ));
        }
        if now_unix_millis.saturating_sub(last_access) >= READ_DELTA_TOUCH_INTERVAL_MILLIS {
            let sequence = next_read_delta_access_sequence(&transaction)?;
            transaction.execute(
                "UPDATE read_delta_bases
                 SET last_access_unix_millis = ?1,
                     expires_unix_millis = ?2,
                     access_sequence = ?3
                 WHERE target_key = ?4 AND content_hash = ?5",
                params![
                    now_unix_millis,
                    now_unix_millis.saturating_add(READ_DELTA_BASE_TTL_MILLIS),
                    sequence,
                    target_key,
                    hash
                ],
            )?;
        }
        transaction.commit()?;
        Ok(Some((hash, base)))
    }

    pub(crate) fn persist_read_delta_base_at(
        &self,
        target_key: &str,
        content_hash: &str,
        base: &ReadDeltaBase,
        now_unix_millis: i64,
    ) -> Result<bool> {
        if now_unix_millis < 0 {
            return Err(Error::OperationFailure(
                "read delta base timestamp must be non-negative".into(),
            ));
        }
        if base.content.len() > MAX_READ_DELTA_BASE_BYTES {
            return Ok(false);
        }
        if crate::text::hash(&base.content) != content_hash {
            return Err(Error::OperationFailure(
                "read delta base content hash mismatch".into(),
            ));
        }
        let logical_bytes = base.logical_bytes(target_key, content_hash);
        if logical_bytes > MAX_TOTAL_READ_DELTA_BASE_BYTES {
            return Ok(false);
        }
        let expires = now_unix_millis
            .checked_add(READ_DELTA_BASE_TTL_MILLIS)
            .ok_or_else(|| Error::OperationFailure("read delta base expiry overflow".into()))?;
        let mut connection = self
            .writer
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        prune_read_delta_bases(&transaction, now_unix_millis)?;
        let existing = transaction
            .query_row(
                "SELECT repository_generation,
                        target_start_line, target_end_line,
                        returned_start_line, returned_end_line,
                        content, created_unix_millis, last_access_unix_millis
                 FROM read_delta_bases
                 WHERE target_key = ?1 AND content_hash = ?2",
                params![target_key, content_hash],
                |row| {
                    Ok((
                        i64_to_u64(row.get(0)?)?,
                        i64_to_usize(row.get(1)?)?,
                        i64_to_usize(row.get(2)?)?,
                        i64_to_usize(row.get(3)?)?,
                        i64_to_usize(row.get(4)?)?,
                        row.get::<_, String>(5)?,
                        row.get::<_, i64>(6)?,
                        row.get::<_, i64>(7)?,
                    ))
                },
            )
            .optional()?;
        if let Some((
            generation,
            target_start_line,
            target_end_line,
            returned_start_line,
            returned_end_line,
            stored_content,
            created,
            last_access,
        )) = existing
        {
            if crate::text::hash(&stored_content) != content_hash {
                return Err(Error::OperationFailure(
                    "persistent read delta base hash mismatch".into(),
                ));
            }
            if created > now_unix_millis || last_access > now_unix_millis {
                transaction.execute(
                    "DELETE FROM read_delta_bases
                     WHERE target_key = ?1 AND content_hash = ?2",
                    params![target_key, content_hash],
                )?;
            } else if generation == base.generation
                && target_start_line == base.target_start_line
                && target_end_line == base.target_end_line
                && returned_start_line == base.returned_start_line
                && returned_end_line == base.returned_end_line
                && now_unix_millis.saturating_sub(last_access) < READ_DELTA_TOUCH_INTERVAL_MILLIS
            {
                transaction.commit()?;
                return Ok(true);
            } else {
                let sequence = next_read_delta_access_sequence(&transaction)?;
                let updated = transaction.execute(
                    "UPDATE read_delta_bases
                     SET repository_generation = ?1,
                         target_start_line = ?2,
                         target_end_line = ?3,
                         returned_start_line = ?4,
                         returned_end_line = ?5,
                         last_access_unix_millis = ?6,
                         expires_unix_millis = ?7,
                         access_sequence = ?8
                     WHERE target_key = ?9 AND content_hash = ?10",
                    params![
                        u64_to_i64(base.generation)?,
                        usize_to_i64(base.target_start_line)?,
                        usize_to_i64(base.target_end_line)?,
                        usize_to_i64(base.returned_start_line)?,
                        usize_to_i64(base.returned_end_line)?,
                        now_unix_millis,
                        expires,
                        sequence,
                        target_key,
                        content_hash
                    ],
                )?;
                if updated != 1 {
                    return Err(Error::OperationFailure(
                        "persistent read delta base update lost its row".into(),
                    ));
                }
                transaction.commit()?;
                return Ok(true);
            }
        }
        loop {
            let (count, bytes) = read_delta_usage(&transaction)?;
            if count < MAX_READ_DELTA_BASES
                && bytes.saturating_add(logical_bytes) <= MAX_TOTAL_READ_DELTA_BASE_BYTES
            {
                break;
            }
            if !evict_oldest_read_delta_base(&transaction)? {
                transaction.commit()?;
                return Ok(false);
            }
        }
        let sequence = next_read_delta_access_sequence(&transaction)?;
        transaction.execute(
            "INSERT INTO read_delta_bases(
                target_key, content_hash, repository_generation,
                target_start_line, target_end_line,
                returned_start_line, returned_end_line, content,
                created_unix_millis, last_access_unix_millis,
                expires_unix_millis, access_sequence, logical_bytes
             ) VALUES (
                ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?9, ?10, ?11, ?12
             )",
            params![
                target_key,
                content_hash,
                u64_to_i64(base.generation)?,
                usize_to_i64(base.target_start_line)?,
                usize_to_i64(base.target_end_line)?,
                usize_to_i64(base.returned_start_line)?,
                usize_to_i64(base.returned_end_line)?,
                base.content,
                now_unix_millis,
                expires,
                sequence,
                usize_to_i64(logical_bytes)?
            ],
        )?;
        transaction.commit()?;
        Ok(true)
    }
}

pub(crate) type StoredReadDeltaBase = (String, ReadDeltaBase, i64, i64);

pub(crate) fn load_read_delta_base(
    transaction: &Transaction<'_>,
    target_key: &str,
    content_hash: Option<&str>,
) -> Result<Option<StoredReadDeltaBase>> {
    let map = |row: &Row<'_>| {
        Ok((
            row.get(0)?,
            ReadDeltaBase {
                generation: i64_to_u64(row.get(1)?)?,
                target_start_line: i64_to_usize(row.get(2)?)?,
                target_end_line: i64_to_usize(row.get(3)?)?,
                returned_start_line: i64_to_usize(row.get(4)?)?,
                returned_end_line: i64_to_usize(row.get(5)?)?,
                content: row.get(6)?,
            },
            row.get(7)?,
            row.get(8)?,
        ))
    };
    match content_hash {
        Some(content_hash) => transaction
            .query_row(
                "SELECT content_hash, repository_generation,
                        target_start_line, target_end_line,
                        returned_start_line, returned_end_line, content,
                        created_unix_millis, last_access_unix_millis
                 FROM read_delta_bases
                 WHERE target_key = ?1 AND content_hash = ?2",
                params![target_key, content_hash],
                map,
            )
            .optional()
            .map_err(Into::into),
        None => transaction
            .query_row(
                "SELECT content_hash, repository_generation,
                        target_start_line, target_end_line,
                        returned_start_line, returned_end_line, content,
                        created_unix_millis, last_access_unix_millis
                 FROM read_delta_bases
                 WHERE target_key = ?1
                 ORDER BY repository_generation DESC, access_sequence DESC, content_hash
                 LIMIT 1",
                [target_key],
                map,
            )
            .optional()
            .map_err(Into::into),
    }
}

pub(crate) fn read_delta_usage(transaction: &Transaction<'_>) -> Result<(usize, usize)> {
    transaction
        .query_row(
            "SELECT base_count, base_bytes FROM read_delta_base_usage WHERE id = 1",
            [],
            |row| Ok((i64_to_usize(row.get(0)?)?, i64_to_usize(row.get(1)?)?)),
        )
        .map_err(Into::into)
}

pub(crate) fn next_read_delta_access_sequence(transaction: &Transaction<'_>) -> Result<i64> {
    let current: i64 = transaction.query_row(
        "SELECT next_access_sequence FROM read_delta_base_usage WHERE id = 1",
        [],
        |row| row.get(0),
    )?;
    let next = current
        .checked_add(1)
        .ok_or_else(|| Error::OperationFailure("read delta access sequence overflow".into()))?;
    let updated = transaction.execute(
        "UPDATE read_delta_base_usage SET next_access_sequence = ?1 WHERE id = 1",
        [next],
    )?;
    if updated != 1 {
        return Err(Error::OperationFailure(
            "persistent read delta usage row is missing".into(),
        ));
    }
    Ok(next)
}

pub(crate) fn prune_read_delta_bases(
    transaction: &Transaction<'_>,
    now_unix_millis: i64,
) -> Result<()> {
    transaction.execute(
        "DELETE FROM read_delta_bases WHERE expires_unix_millis <= ?1",
        [now_unix_millis],
    )?;
    Ok(())
}

pub(crate) fn evict_oldest_read_delta_base(transaction: &Transaction<'_>) -> Result<bool> {
    let candidate = transaction
        .query_row(
            "SELECT target_key, content_hash
             FROM read_delta_bases
             ORDER BY access_sequence, target_key, content_hash
             LIMIT 1",
            [],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()?;
    let Some((target_key, content_hash)) = candidate else {
        return Ok(false);
    };
    Ok(transaction.execute(
        "DELETE FROM read_delta_bases
         WHERE target_key = ?1 AND content_hash = ?2",
        params![target_key, content_hash],
    )? == 1)
}
use super::*;
