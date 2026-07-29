use crate::receipt::{
    MAX_EVIDENCE_BYTES_PER_RECEIPT, MAX_EVIDENCE_PER_RECEIPT,
    MAX_RECEIPT_EVIDENCE_LOGICAL_BYTES, MAX_RECEIPT_ID_BYTES, MAX_RECEIPTS,
    MAX_TOTAL_EVIDENCE, MAX_TOTAL_EVIDENCE_BYTES, MAX_TOTAL_RECEIPT_BYTES,
    RECEIPT_TOUCH_INTERVAL_MILLIS, RECEIPT_TTL_MILLIS, ReceiptDecision, ReceiptEvaluation,
    ReceiptEvidence, ReceiptRebaseSource, StoredReceipt, decide, format_receipt_id,
    parse_receipt_id,
};

const RECEIPT_HEADER_FIXED_LOGICAL_BYTES: usize = 9 * size_of::<u64>();

#[derive(Debug)]
struct PersistentReceiptRow {
    id: i64,
    repository_identity: String,
    repository_generation: u64,
    created_unix_millis: i64,
    last_access_unix_millis: i64,
    expires_unix_millis: i64,
    evidence_count: usize,
    evidence_bytes: usize,
}

#[derive(Debug)]
struct PersistentReceiptUsage {
    namespace: String,
    next_access_sequence: i64,
    receipt_count: usize,
    receipt_bytes: usize,
    evidence_count: usize,
    evidence_bytes: usize,
}

impl Storage {
    /// Load an immutable receipt snapshot without pruning, touching, or extending it.
    pub(crate) fn read_receipt(
        &self,
        requested_id: &str,
        now_unix_millis: i64,
    ) -> Result<StoredReceipt> {
        if requested_id.len() > MAX_RECEIPT_ID_BYTES {
            return Err(Error::UnknownReceipt(requested_id.to_owned()));
        }
        let mut conn = self.readers.get()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Deferred)?;
        let namespace: String = tx.query_row(
            "SELECT namespace FROM retrieval_receipt_usage WHERE id = 1",
            [],
            |row| row.get(0),
        )?;
        let Some(row_id) = parse_receipt_id(requested_id, &namespace) else {
            return Err(Error::UnknownReceipt(requested_id.to_owned()));
        };
        let Some(receipt) = load_receipt_connection(&tx, row_id)? else {
            return Err(Error::UnknownReceipt(requested_id.to_owned()));
        };
        let repository_identity: String = tx.query_row(
            "SELECT repository_identity FROM meta WHERE id = 1",
            [],
            |row| row.get(0),
        )?;
        if receipt.repository_identity != repository_identity
            || receipt.created_unix_millis > now_unix_millis
            || receipt.last_access_unix_millis > now_unix_millis
            || receipt.expires_unix_millis <= now_unix_millis
        {
            return Err(Error::UnknownReceipt(requested_id.to_owned()));
        }
        let evidence = load_receipt_evidence_connection(&tx, row_id)?;
        if evidence.len() != receipt.evidence_count
            || evidence.len() > MAX_EVIDENCE_PER_RECEIPT
            || receipt.evidence_bytes > MAX_EVIDENCE_BYTES_PER_RECEIPT
        {
            return Err(Error::InternalFailure(
                "retrieval receipt storage bounds are inconsistent".into(),
            ));
        }
        tx.commit()?;
        Ok(StoredReceipt {
            receipt_id: requested_id.to_owned(),
            repository_identity,
            repository_generation: receipt.repository_generation,
            created_unix_millis: receipt.created_unix_millis,
            expires_unix_millis: receipt.expires_unix_millis,
            complete: receipt.evidence_count < MAX_EVIDENCE_PER_RECEIPT
                && receipt
                    .evidence_bytes
                    .saturating_add(MAX_RECEIPT_EVIDENCE_LOGICAL_BYTES)
                    <= MAX_EVIDENCE_BYTES_PER_RECEIPT,
            evidence,
        })
    }

    pub(crate) fn evaluate_receipt(
        &self,
        requested_id: Option<&str>,
        generation: u64,
        candidates: &[ReceiptEvidence],
        suppress_overlap: bool,
    ) -> Result<ReceiptEvaluation> {
        self.evaluate_receipt_at(
            requested_id,
            generation,
            candidates,
            suppress_overlap,
            unix_millis(SystemTime::now()),
        )
    }

    fn evaluate_receipt_at(
        &self,
        requested_id: Option<&str>,
        generation: u64,
        candidates: &[ReceiptEvidence],
        suppress_overlap: bool,
        now_unix_millis: i64,
    ) -> Result<ReceiptEvaluation> {
        if requested_id.is_some_and(|id| id.len() > MAX_RECEIPT_ID_BYTES) {
            return Err(Error::InputTooLong {
                field: "receipt_id",
                max_bytes: MAX_RECEIPT_ID_BYTES,
            });
        }
        if now_unix_millis < 0 {
            return Err(Error::InternalFailure(
                "system clock precedes the Unix epoch".into(),
            ));
        }
        if candidates
            .iter()
            .any(|evidence| evidence.logical_bytes() > MAX_EVIDENCE_BYTES_PER_RECEIPT)
        {
            return Err(Error::InputTooLong {
                field: "receipt_evidence",
                max_bytes: MAX_EVIDENCE_BYTES_PER_RECEIPT,
            });
        }
        let expires_unix_millis = now_unix_millis
            .checked_add(RECEIPT_TTL_MILLIS)
            .ok_or_else(|| Error::InternalFailure("retrieval receipt expiry overflow".into()))?;
        let mut conn = self
            .writer
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        prune_expired_receipts(&tx, now_unix_millis)?;
        let mut usage = receipt_usage(&tx)?;
        let requested_existing = requested_id.is_some();

        let (receipt_id, receipt) = if let Some(requested_id) = requested_id {
            let Some(row_id) = parse_receipt_id(requested_id, &usage.namespace) else {
                tx.commit()?;
                return Err(Error::UnknownReceipt(requested_id.to_owned()));
            };
            let Some(receipt) = load_receipt(&tx, row_id)? else {
                tx.commit()?;
                return Err(Error::UnknownReceipt(requested_id.to_owned()));
            };
            if receipt.created_unix_millis > now_unix_millis
                || receipt.last_access_unix_millis > now_unix_millis
            {
                tx.execute("DELETE FROM retrieval_receipts WHERE id = ?1", [row_id])?;
                tx.commit()?;
                return Err(Error::UnknownReceipt(requested_id.to_owned()));
            }
            if receipt.repository_generation != generation {
                tx.commit()?;
                return Err(Error::StaleReceipt {
                    receipt_generation: receipt.repository_generation,
                    repository_generation: generation,
                });
            }
            let repository_identity: String = tx.query_row(
                "SELECT repository_identity FROM meta WHERE id = 1",
                [],
                |row| row.get(0),
            )?;
            if receipt.repository_identity != repository_identity {
                tx.commit()?;
                return Err(Error::UnknownReceipt(requested_id.to_owned()));
            }
            (requested_id.to_owned(), receipt)
        } else {
            let repository_identity: String = tx.query_row(
                "SELECT repository_identity FROM meta WHERE id = 1",
                [],
                |row| row.get(0),
            )?;
            let logical_bytes = RECEIPT_HEADER_FIXED_LOGICAL_BYTES
                .checked_add(repository_identity.len())
                .ok_or_else(|| {
                    Error::InternalFailure("retrieval receipt byte accounting overflow".into())
                })?;
            if logical_bytes > MAX_TOTAL_RECEIPT_BYTES {
                return Err(Error::InternalFailure(
                    "repository identity exceeds the retrieval receipt byte quota".into(),
                ));
            }
            while usage.receipt_count >= MAX_RECEIPTS
                || usage.receipt_bytes.saturating_add(logical_bytes) > MAX_TOTAL_RECEIPT_BYTES
            {
                if !evict_oldest_receipt_except(&tx, None)? {
                    return Err(Error::InternalFailure(
                        "retrieval receipt quota could not evict a receipt".into(),
                    ));
                }
                usage = receipt_usage(&tx)?;
            }
            let access_sequence = next_receipt_access_sequence(&tx, usage.next_access_sequence)?;
            usage.next_access_sequence = access_sequence;
            tx.execute(
                "INSERT INTO retrieval_receipts(
                    repository_identity,
                    repository_generation,
                    created_unix_millis,
                    last_access_unix_millis,
                    expires_unix_millis,
                    access_sequence,
                    logical_bytes
                 ) VALUES (?1, ?2, ?3, ?3, ?4, ?5, ?6)",
                params![
                    repository_identity,
                    u64_to_i64(generation)?,
                    now_unix_millis,
                    expires_unix_millis,
                    access_sequence,
                    usize_to_i64(logical_bytes)?
                ],
            )?;
            let row_id = tx.last_insert_rowid();
            let receipt_id = format_receipt_id(&usage.namespace, row_id);
            (
                receipt_id,
                PersistentReceiptRow {
                    id: row_id,
                    repository_identity,
                    repository_generation: generation,
                    created_unix_millis: now_unix_millis,
                    last_access_unix_millis: now_unix_millis,
                    expires_unix_millis,
                    evidence_count: 0,
                    evidence_bytes: 0,
                },
            )
        };

        let previous = load_receipt_evidence(&tx, receipt.id)?;
        let decisions = candidates
            .iter()
            .map(|candidate| decide(&previous, candidate, suppress_overlap))
            .collect::<Vec<_>>();
        let returned = candidates
            .iter()
            .zip(&decisions)
            .filter(|(_, decision)| {
                matches!(
                    decision,
                    ReceiptDecision::Return | ReceiptDecision::ReturnNearDuplicate
                )
            })
            .map(|(candidate, _)| candidate.clone())
            .collect::<Vec<_>>();
        let mut append = Vec::new();
        let mut append_bytes = 0usize;
        for evidence in returned {
            let logical_bytes = evidence.logical_bytes();
            if receipt.evidence_count.saturating_add(append.len())
                >= MAX_EVIDENCE_PER_RECEIPT
                || receipt
                    .evidence_bytes
                    .saturating_add(append_bytes)
                    .saturating_add(logical_bytes)
                    > MAX_EVIDENCE_BYTES_PER_RECEIPT
            {
                break;
            }
            append_bytes = append_bytes.saturating_add(logical_bytes);
            append.push((evidence, logical_bytes));
        }

        usage = receipt_usage(&tx)?;
        while usage.evidence_count.saturating_add(append.len()) > MAX_TOTAL_EVIDENCE
            || usage.evidence_bytes.saturating_add(append_bytes) > MAX_TOTAL_EVIDENCE_BYTES
        {
            if !evict_oldest_receipt_except(&tx, Some(receipt.id))? {
                break;
            }
            usage = receipt_usage(&tx)?;
        }
        while usage.evidence_count.saturating_add(append.len()) > MAX_TOTAL_EVIDENCE
            || usage.evidence_bytes.saturating_add(append_bytes) > MAX_TOTAL_EVIDENCE_BYTES
        {
            let Some((_, removed_bytes)) = append.pop() else {
                break;
            };
            append_bytes = append_bytes.saturating_sub(removed_bytes);
        }

        let append_count = append.len();
        for (index, (evidence, logical_bytes)) in append.into_iter().enumerate() {
            let ordinal = receipt
                .evidence_count
                .checked_add(index)
                .ok_or_else(|| {
                    Error::InternalFailure("retrieval receipt ordinal overflow".into())
                })?;
            tx.execute(
                "INSERT INTO retrieval_receipt_evidence(
                    receipt_id,
                    ordinal,
                    path,
                    start_line,
                    end_line,
                    content_hash,
                    semantic_signature,
                    exact_only,
                    logical_bytes
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![
                    receipt.id,
                    usize_to_i64(ordinal)?,
                    evidence.path,
                    usize_to_i64(evidence.start_line)?,
                    usize_to_i64(evidence.end_line)?,
                    evidence.content_hash,
                    evidence.semantic_signature.map(|signature| signature as i64),
                    i64::from(evidence.exact_only),
                    usize_to_i64(logical_bytes)?
                ],
            )?;
        }
        if append_count > 0 {
            let append_count = usize_to_i64(append_count)?;
            let append_bytes = usize_to_i64(append_bytes)?;
            let updated = tx.execute(
                "UPDATE retrieval_receipts
                 SET evidence_count = evidence_count + ?1,
                     evidence_bytes = evidence_bytes + ?2
                 WHERE id = ?3",
                params![append_count, append_bytes, receipt.id],
            )?;
            if updated != 1 {
                return Err(Error::InternalFailure(
                    "retrieval receipt disappeared before counter update".into(),
                ));
            }
            tx.execute(
                "UPDATE retrieval_receipt_usage
                 SET evidence_count = evidence_count + ?1,
                     evidence_bytes = evidence_bytes + ?2
                 WHERE id = 1",
                params![append_count, append_bytes],
            )?;
        }
        let touch_due = now_unix_millis
            .saturating_sub(receipt.last_access_unix_millis)
            >= RECEIPT_TOUCH_INTERVAL_MILLIS;
        if requested_existing && (append_count > 0 || touch_due) {
            let access_sequence = next_receipt_access_sequence(&tx, usage.next_access_sequence)?;
            tx.execute(
                "UPDATE retrieval_receipts
                 SET last_access_unix_millis = ?1,
                     expires_unix_millis = ?2,
                     access_sequence = ?3
                 WHERE id = ?4",
                params![
                    now_unix_millis,
                    expires_unix_millis,
                    access_sequence,
                    receipt.id
                ],
            )?;
        }
        tx.commit()?;
        Ok(ReceiptEvaluation {
            receipt_id,
            decisions,
        })
    }

    pub(crate) fn load_receipt_rebase_source(
        &self,
        requested_id: &str,
    ) -> Result<ReceiptRebaseSource> {
        self.load_receipt_rebase_source_at(requested_id, unix_millis(SystemTime::now()))
    }

    fn load_receipt_rebase_source_at(
        &self,
        requested_id: &str,
        now_unix_millis: i64,
    ) -> Result<ReceiptRebaseSource> {
        if requested_id.len() > MAX_RECEIPT_ID_BYTES {
            return Err(Error::InputTooLong {
                field: "receipt_id",
                max_bytes: MAX_RECEIPT_ID_BYTES,
            });
        }
        if now_unix_millis < 0 {
            return Err(Error::InternalFailure(
                "system clock precedes the Unix epoch".into(),
            ));
        }
        let mut conn = self
            .writer
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let tx = conn.transaction_with_behavior(TransactionBehavior::Deferred)?;
        let usage = receipt_usage(&tx)?;
        let Some(row_id) = parse_receipt_id(requested_id, &usage.namespace) else {
            tx.commit()?;
            return Err(Error::UnknownReceipt(requested_id.to_owned()));
        };
        let Some(receipt) = load_receipt(&tx, row_id)? else {
            tx.commit()?;
            return Err(Error::UnknownReceipt(requested_id.to_owned()));
        };
        if receipt.created_unix_millis > now_unix_millis
            || receipt.last_access_unix_millis > now_unix_millis
            || receipt.expires_unix_millis <= now_unix_millis
        {
            tx.commit()?;
            return Err(Error::UnknownReceipt(requested_id.to_owned()));
        }
        let repository_identity: String = tx.query_row(
            "SELECT repository_identity FROM meta WHERE id = 1",
            [],
            |row| row.get(0),
        )?;
        if receipt.repository_identity != repository_identity {
            tx.commit()?;
            return Err(Error::UnknownReceipt(requested_id.to_owned()));
        }
        let evidence = load_receipt_evidence(&tx, receipt.id)?;
        if evidence.len() != receipt.evidence_count {
            return Err(Error::InternalFailure(
                "retrieval receipt evidence count is inconsistent".into(),
            ));
        }
        tx.commit()?;
        Ok(ReceiptRebaseSource {
            receipt_id: requested_id.to_owned(),
            repository_identity,
            repository_generation: receipt.repository_generation,
            evidence,
        })
    }

    pub(crate) fn persist_rebased_receipt(
        &self,
        source: &ReceiptRebaseSource,
        expected_repository_generation: u64,
        carried: &[ReceiptEvidence],
    ) -> Result<String> {
        self.persist_rebased_receipt_at(
            source,
            expected_repository_generation,
            carried,
            unix_millis(SystemTime::now()),
        )
    }

    fn persist_rebased_receipt_at(
        &self,
        source: &ReceiptRebaseSource,
        expected_repository_generation: u64,
        carried: &[ReceiptEvidence],
        now_unix_millis: i64,
    ) -> Result<String> {
        if now_unix_millis < 0 {
            return Err(Error::InternalFailure(
                "system clock precedes the Unix epoch".into(),
            ));
        }
        let expires_unix_millis = now_unix_millis
            .checked_add(RECEIPT_TTL_MILLIS)
            .ok_or_else(|| Error::InternalFailure("retrieval receipt expiry overflow".into()))?;
        let mut conn = self
            .writer
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut usage = receipt_usage(&tx)?;
        let Some(source_row_id) = parse_receipt_id(&source.receipt_id, &usage.namespace) else {
            tx.commit()?;
            return Err(Error::UnknownReceipt(source.receipt_id.clone()));
        };
        let Some(current_source) = load_receipt(&tx, source_row_id)? else {
            tx.commit()?;
            return Err(Error::UnknownReceipt(source.receipt_id.clone()));
        };
        if current_source.created_unix_millis > now_unix_millis
            || current_source.last_access_unix_millis > now_unix_millis
            || current_source.expires_unix_millis <= now_unix_millis
        {
            tx.commit()?;
            return Err(Error::UnknownReceipt(source.receipt_id.clone()));
        }
        let (repository_identity, repository_generation): (String, u64) = tx.query_row(
            "SELECT repository_identity, repository_generation FROM meta WHERE id = 1",
            [],
            |row| Ok((row.get(0)?, i64_to_u64(row.get(1)?)?)),
        )?;
        if repository_identity != source.repository_identity
            || current_source.repository_identity != source.repository_identity
        {
            tx.commit()?;
            return Err(Error::UnknownReceipt(source.receipt_id.clone()));
        }
        if repository_generation != expected_repository_generation {
            return Err(Error::RetryableConflict(
                crate::error::RetryableOperation::Retrieval,
            ));
        }
        let current_evidence = load_receipt_evidence(&tx, source_row_id)?;
        if current_source.repository_generation != source.repository_generation
            || current_evidence != source.evidence
        {
            return Err(Error::RetryableConflict(
                crate::error::RetryableOperation::Retrieval,
            ));
        }
        let mut next_source = 0usize;
        for evidence in carried {
            let Some(offset) = source.evidence[next_source..]
                .iter()
                .position(|candidate| candidate == evidence)
            else {
                return Err(Error::InternalFailure(
                    "rebased receipt evidence is not an ordered source subset".into(),
                ));
            };
            next_source = next_source.saturating_add(offset).saturating_add(1);
        }

        tx.execute(
            "DELETE FROM retrieval_receipts
             WHERE expires_unix_millis <= ?1 AND id != ?2",
            params![now_unix_millis, source_row_id],
        )?;
        usage = receipt_usage(&tx)?;
        let logical_bytes = RECEIPT_HEADER_FIXED_LOGICAL_BYTES
            .checked_add(repository_identity.len())
            .ok_or_else(|| {
                Error::InternalFailure("retrieval receipt byte accounting overflow".into())
            })?;
        let evidence_bytes = carried.iter().try_fold(0usize, |total, evidence| {
            total.checked_add(evidence.logical_bytes()).ok_or_else(|| {
                Error::InternalFailure("retrieval receipt byte accounting overflow".into())
            })
        })?;
        if carried.len() > MAX_EVIDENCE_PER_RECEIPT
            || evidence_bytes > MAX_EVIDENCE_BYTES_PER_RECEIPT
            || logical_bytes > MAX_TOTAL_RECEIPT_BYTES
        {
            return Err(Error::InternalFailure(
                "rebased retrieval receipt exceeds its source-derived quota".into(),
            ));
        }
        while usage.receipt_count >= MAX_RECEIPTS
            || usage.receipt_bytes.saturating_add(logical_bytes) > MAX_TOTAL_RECEIPT_BYTES
            || usage.evidence_count.saturating_add(carried.len()) > MAX_TOTAL_EVIDENCE
            || usage.evidence_bytes.saturating_add(evidence_bytes) > MAX_TOTAL_EVIDENCE_BYTES
        {
            if !evict_oldest_receipt_except(&tx, Some(source_row_id))? {
                return Err(Error::InternalFailure(
                    "retrieval receipt quota cannot preserve the rebase source".into(),
                ));
            }
            usage = receipt_usage(&tx)?;
        }
        let access_sequence = next_receipt_access_sequence(&tx, usage.next_access_sequence)?;
        tx.execute(
            "INSERT INTO retrieval_receipts(
                repository_identity,
                repository_generation,
                created_unix_millis,
                last_access_unix_millis,
                expires_unix_millis,
                access_sequence,
                logical_bytes,
                evidence_count,
                evidence_bytes
             ) VALUES (?1, ?2, ?3, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                repository_identity,
                u64_to_i64(repository_generation)?,
                now_unix_millis,
                expires_unix_millis,
                access_sequence,
                usize_to_i64(logical_bytes)?,
                usize_to_i64(carried.len())?,
                usize_to_i64(evidence_bytes)?,
            ],
        )?;
        let row_id = tx.last_insert_rowid();
        for (ordinal, evidence) in carried.iter().enumerate() {
            tx.execute(
                "INSERT INTO retrieval_receipt_evidence(
                    receipt_id,
                    ordinal,
                    path,
                    start_line,
                    end_line,
                    content_hash,
                    semantic_signature,
                    exact_only,
                    logical_bytes
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![
                    row_id,
                    usize_to_i64(ordinal)?,
                    evidence.path,
                    usize_to_i64(evidence.start_line)?,
                    usize_to_i64(evidence.end_line)?,
                    evidence.content_hash,
                    evidence.semantic_signature.map(|signature| signature as i64),
                    1_i64,
                    usize_to_i64(evidence.logical_bytes())?,
                ],
            )?;
        }
        if !carried.is_empty() {
            tx.execute(
                "UPDATE retrieval_receipt_usage
                 SET evidence_count = evidence_count + ?1,
                     evidence_bytes = evidence_bytes + ?2
                 WHERE id = 1",
                params![
                    usize_to_i64(carried.len())?,
                    usize_to_i64(evidence_bytes)?
                ],
            )?;
        }
        let receipt_id = format_receipt_id(&usage.namespace, row_id);
        tx.commit()?;
        Ok(receipt_id)
    }
}

fn receipt_usage(tx: &Transaction<'_>) -> Result<PersistentReceiptUsage> {
    tx.query_row(
        "SELECT namespace,
                next_access_sequence,
                receipt_count,
                receipt_bytes,
                evidence_count,
                evidence_bytes
         FROM retrieval_receipt_usage
         WHERE id = 1",
        [],
        |row| {
            Ok(PersistentReceiptUsage {
                namespace: row.get(0)?,
                next_access_sequence: row.get(1)?,
                receipt_count: i64_to_usize(row.get(2)?)?,
                receipt_bytes: i64_to_usize(row.get(3)?)?,
                evidence_count: i64_to_usize(row.get(4)?)?,
                evidence_bytes: i64_to_usize(row.get(5)?)?,
            })
        },
    )
    .map_err(Into::into)
}

fn next_receipt_access_sequence(tx: &Transaction<'_>, current: i64) -> Result<i64> {
    let next = current.checked_add(1).ok_or_else(|| {
        Error::InternalFailure("retrieval receipt access sequence overflow".into())
    })?;
    let updated = tx.execute(
        "UPDATE retrieval_receipt_usage
         SET next_access_sequence = ?1
         WHERE id = 1 AND next_access_sequence = ?2",
        params![next, current],
    )?;
    if updated != 1 {
        return Err(Error::InternalFailure(
            "retrieval receipt access sequence changed unexpectedly".into(),
        ));
    }
    Ok(next)
}

fn load_receipt(tx: &Transaction<'_>, row_id: i64) -> Result<Option<PersistentReceiptRow>> {
    load_receipt_connection(tx, row_id)
}

fn load_receipt_connection(
    connection: &Connection,
    row_id: i64,
) -> Result<Option<PersistentReceiptRow>> {
    connection.query_row(
        "SELECT id,
                repository_identity,
                repository_generation,
                created_unix_millis,
                last_access_unix_millis,
                expires_unix_millis,
                evidence_count,
                evidence_bytes
         FROM retrieval_receipts
         WHERE id = ?1",
        [row_id],
        |row| {
            Ok(PersistentReceiptRow {
                id: row.get(0)?,
                repository_identity: row.get(1)?,
                repository_generation: i64_to_u64(row.get(2)?)?,
                created_unix_millis: row.get(3)?,
                last_access_unix_millis: row.get(4)?,
                expires_unix_millis: row.get(5)?,
                evidence_count: i64_to_usize(row.get(6)?)?,
                evidence_bytes: i64_to_usize(row.get(7)?)?,
            })
        },
    )
    .optional()
    .map_err(Into::into)
}

fn load_receipt_evidence(
    tx: &Transaction<'_>,
    receipt_id: i64,
) -> Result<Vec<ReceiptEvidence>> {
    load_receipt_evidence_connection(tx, receipt_id)
}

fn load_receipt_evidence_connection(
    connection: &Connection,
    receipt_id: i64,
) -> Result<Vec<ReceiptEvidence>> {
    let mut statement = connection.prepare(
        "SELECT path, start_line, end_line, content_hash, semantic_signature, exact_only
         FROM retrieval_receipt_evidence
         WHERE receipt_id = ?1
         ORDER BY ordinal",
    )?;
    statement
        .query_map([receipt_id], |row| {
            Ok(ReceiptEvidence {
                path: row.get(0)?,
                start_line: i64_to_usize(row.get(1)?)?,
                end_line: i64_to_usize(row.get(2)?)?,
                content_hash: row.get(3)?,
                semantic_signature: row.get::<_, Option<i64>>(4)?.map(|value| value as u64),
                exact_only: row.get::<_, i64>(5)? != 0,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(Into::into)
}

fn prune_expired_receipts(tx: &Transaction<'_>, now_unix_millis: i64) -> Result<()> {
    tx.execute(
        "DELETE FROM retrieval_receipts
         WHERE expires_unix_millis <= ?1",
        [now_unix_millis],
    )?;
    Ok(())
}

fn evict_oldest_receipt_except(
    tx: &Transaction<'_>,
    retained_id: Option<i64>,
) -> Result<bool> {
    let candidate = match retained_id {
        Some(retained_id) => tx
            .query_row(
                "SELECT id
                 FROM retrieval_receipts
                 WHERE access_sequence > 0 AND id != ?1
                 ORDER BY access_sequence, id
                 LIMIT 1",
                [retained_id],
                |row| row.get::<_, i64>(0),
            )
            .optional()?,
        None => tx
            .query_row(
                "SELECT id
                 FROM retrieval_receipts
                 WHERE access_sequence > 0
                 ORDER BY access_sequence, id
                 LIMIT 1",
                [],
                |row| row.get::<_, i64>(0),
            )
            .optional()?,
    };
    let Some(candidate) = candidate else {
        return Ok(false);
    };
    Ok(tx.execute(
        "DELETE FROM retrieval_receipts WHERE id = ?1",
        [candidate],
    )? == 1)
}

fn unix_millis(time: SystemTime) -> i64 {
    time.duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_millis()).ok())
        .unwrap_or(0)
}
