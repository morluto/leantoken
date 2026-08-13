use crate::receipt::{
    MAX_EVIDENCE_BYTES_PER_RECEIPT, MAX_EVIDENCE_PER_RECEIPT, MAX_RECEIPT_EVIDENCE_LOGICAL_BYTES,
    MAX_RECEIPT_ID_BYTES, MAX_RECEIPTS, MAX_TOTAL_EVIDENCE, MAX_TOTAL_EVIDENCE_BYTES,
    MAX_TOTAL_RECEIPT_BYTES, RECEIPT_TTL_MILLIS, ReceiptDecision, ReceiptEvaluation,
    ReceiptEvidence, ReceiptRebaseSource, StoredReceipt, decide, format_receipt_id,
    parse_receipt_id,
};

pub(crate) const RECEIPT_HEADER_FIXED_LOGICAL_BYTES: usize = 9 * size_of::<u64>();

#[derive(Debug)]
pub(crate) struct PersistentReceiptRow {
    pub(crate) id: i64,
    pub(crate) repository_identity: String,
    pub(crate) repository_generation: u64,
    pub(crate) created_unix_millis: i64,
    pub(crate) last_access_unix_millis: i64,
    pub(crate) expires_unix_millis: i64,
    pub(crate) evidence_count: usize,
    pub(crate) evidence_bytes: usize,
}

#[derive(Debug)]
pub(crate) struct PersistentReceiptUsage {
    pub(crate) namespace: String,
    pub(crate) next_access_sequence: i64,
    pub(crate) receipt_count: usize,
    pub(crate) receipt_bytes: usize,
    pub(crate) evidence_count: usize,
    pub(crate) evidence_bytes: usize,
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
            return Err(Error::OperationFailure(
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
        self.evaluate_receipt_with_clock(
            requested_id,
            generation,
            candidates,
            suppress_overlap,
            || unix_millis(SystemTime::now()),
        )
    }

    #[cfg(test)]
    pub(crate) fn evaluate_receipt_at(
        &self,
        requested_id: Option<&str>,
        generation: u64,
        candidates: &[ReceiptEvidence],
        suppress_overlap: bool,
        now_unix_millis: i64,
    ) -> Result<ReceiptEvaluation> {
        self.evaluate_receipt_with_clock(
            requested_id,
            generation,
            candidates,
            suppress_overlap,
            || now_unix_millis,
        )
    }

    pub(crate) fn evaluate_receipt_with_clock<F>(
        &self,
        requested_id: Option<&str>,
        generation: u64,
        candidates: &[ReceiptEvidence],
        suppress_overlap: bool,
        now_unix_millis: F,
    ) -> Result<ReceiptEvaluation>
    where
        F: FnOnce() -> i64,
    {
        if requested_id.is_some_and(|id| id.len() > MAX_RECEIPT_ID_BYTES) {
            return Err(Error::InputTooLong {
                field: "receipt_id",
                max_bytes: MAX_RECEIPT_ID_BYTES,
            });
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
        let mut conn = self
            .writer
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        // Independent `Storage` instances have independent process-local
        // mutexes. Sample live time only after SQLite has serialized writers,
        // otherwise an earlier sample can commit after a later one and look
        // like a clock rollback.
        let now_unix_millis = now_unix_millis();
        if now_unix_millis < 0 {
            return Err(Error::OperationFailure(
                "system clock precedes the Unix epoch".into(),
            ));
        }
        let expires_unix_millis = now_unix_millis
            .checked_add(RECEIPT_TTL_MILLIS)
            .ok_or_else(|| Error::OperationFailure("retrieval receipt expiry overflow".into()))?;
        prune_expired_receipts(&tx, now_unix_millis)?;
        let mut usage = receipt_usage(&tx)?;
        let repository_identity: String = tx.query_row(
            "SELECT repository_identity FROM meta WHERE id = 1",
            [],
            |row| row.get(0),
        )?;
        let source = if let Some(requested_id) = requested_id {
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
            if receipt.repository_identity != repository_identity {
                tx.commit()?;
                return Err(Error::UnknownReceipt(requested_id.to_owned()));
            }
            Some((requested_id.to_owned(), receipt))
        } else {
            None
        };
        let previous = if let Some((_, source)) = &source {
            load_receipt_evidence(&tx, source.id)?
        } else {
            Vec::new()
        };
        let previous_bytes = previous.iter().try_fold(0usize, |total, evidence| {
            total.checked_add(evidence.logical_bytes()).ok_or_else(|| {
                Error::OperationFailure("retrieval receipt byte accounting overflow".into())
            })
        })?;
        if source.as_ref().is_some_and(|(_, source)| {
            previous.len() != source.evidence_count || previous_bytes != source.evidence_bytes
        }) {
            return Err(Error::OperationFailure(
                "retrieval receipt storage bounds are inconsistent".into(),
            ));
        }
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
            if previous.len().saturating_add(append.len()) >= MAX_EVIDENCE_PER_RECEIPT
                || previous_bytes
                    .saturating_add(append_bytes)
                    .saturating_add(logical_bytes)
                    > MAX_EVIDENCE_BYTES_PER_RECEIPT
            {
                break;
            }
            append_bytes = append_bytes.saturating_add(logical_bytes);
            append.push((evidence, logical_bytes));
        }

        // A caller-provided receipt is an immutable acknowledgement token. If
        // this response does not extend caller knowledge, reuse that snapshot;
        // otherwise persist a copy-on-write successor and leave the source
        // untouched. A lost response can therefore be retried with the source
        // without suppressing evidence the caller never observed.
        if source.is_some() && append.is_empty() {
            let receipt_id = source
                .as_ref()
                .map(|(receipt_id, _)| receipt_id.clone())
                .expect("source checked above");
            tx.commit()?;
            return Ok(ReceiptEvaluation {
                receipt_id,
                decisions,
            });
        }

        let header_bytes = RECEIPT_HEADER_FIXED_LOGICAL_BYTES
            .checked_add(repository_identity.len())
            .ok_or_else(|| {
                Error::OperationFailure("retrieval receipt byte accounting overflow".into())
            })?;
        if header_bytes > MAX_TOTAL_RECEIPT_BYTES {
            return Err(Error::OperationFailure(
                "repository identity exceeds the retrieval receipt byte quota".into(),
            ));
        }
        let retained_source = source.as_ref().map(|(_, source)| source.id);
        loop {
            let successor_count = previous.len().saturating_add(append.len());
            let successor_bytes = previous_bytes.saturating_add(append_bytes);
            let header_over_quota = usage.receipt_count >= MAX_RECEIPTS
                || usage.receipt_bytes.saturating_add(header_bytes) > MAX_TOTAL_RECEIPT_BYTES;
            let evidence_over_quota = usage.evidence_count.saturating_add(successor_count)
                > MAX_TOTAL_EVIDENCE
                || usage.evidence_bytes.saturating_add(successor_bytes) > MAX_TOTAL_EVIDENCE_BYTES;
            if !header_over_quota && !evidence_over_quota {
                break;
            }
            if evict_oldest_receipt_except(&tx, retained_source)? {
                usage = receipt_usage(&tx)?;
                continue;
            }
            if header_over_quota {
                return Err(Error::OperationFailure(
                    "retrieval receipt quota cannot preserve the source snapshot".into(),
                ));
            }
            let Some((_, removed_bytes)) = append.pop() else {
                return Err(Error::OperationFailure(
                    "retrieval receipt evidence quota cannot preserve the source snapshot".into(),
                ));
            };
            append_bytes = append_bytes.saturating_sub(removed_bytes);
        }

        let evidence_count = previous.len().saturating_add(append.len());
        let evidence_bytes = previous_bytes.saturating_add(append_bytes);
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
                u64_to_i64(generation)?,
                now_unix_millis,
                expires_unix_millis,
                access_sequence,
                usize_to_i64(header_bytes)?,
                usize_to_i64(evidence_count)?,
                usize_to_i64(evidence_bytes)?,
            ],
        )?;
        let row_id = tx.last_insert_rowid();
        for (ordinal, evidence) in previous
            .iter()
            .chain(append.iter().map(|(evidence, _)| evidence))
            .enumerate()
        {
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
                    evidence.path.as_str(),
                    usize_to_i64(evidence.start_line)?,
                    usize_to_i64(evidence.end_line)?,
                    evidence.content_hash.as_str(),
                    evidence
                        .semantic_signature()
                        .map(|signature| signature as i64),
                    i64::from(evidence.exact_only()),
                    usize_to_i64(evidence.logical_bytes())?
                ],
            )?;
        }
        if evidence_count > 0 {
            let updated = tx.execute(
                "UPDATE retrieval_receipt_usage
                 SET evidence_count = evidence_count + ?1,
                     evidence_bytes = evidence_bytes + ?2
                 WHERE id = 1",
                params![usize_to_i64(evidence_count)?, usize_to_i64(evidence_bytes)?],
            )?;
            if updated != 1 {
                return Err(Error::OperationFailure(
                    "retrieval receipt usage row disappeared".into(),
                ));
            }
        }
        let receipt_id = format_receipt_id(&usage.namespace, row_id);
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

    pub(crate) fn load_receipt_rebase_source_at(
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
            return Err(Error::OperationFailure(
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
            return Err(Error::OperationFailure(
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

    pub(crate) fn persist_rebased_receipt_at(
        &self,
        source: &ReceiptRebaseSource,
        expected_repository_generation: u64,
        carried: &[ReceiptEvidence],
        now_unix_millis: i64,
    ) -> Result<String> {
        if now_unix_millis < 0 {
            return Err(Error::OperationFailure(
                "system clock precedes the Unix epoch".into(),
            ));
        }
        let expires_unix_millis = now_unix_millis
            .checked_add(RECEIPT_TTL_MILLIS)
            .ok_or_else(|| Error::OperationFailure("retrieval receipt expiry overflow".into()))?;
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
                return Err(Error::OperationFailure(
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
                Error::OperationFailure("retrieval receipt byte accounting overflow".into())
            })?;
        let evidence_bytes = carried.iter().try_fold(0usize, |total, evidence| {
            total.checked_add(evidence.logical_bytes()).ok_or_else(|| {
                Error::OperationFailure("retrieval receipt byte accounting overflow".into())
            })
        })?;
        if carried.len() > MAX_EVIDENCE_PER_RECEIPT
            || evidence_bytes > MAX_EVIDENCE_BYTES_PER_RECEIPT
            || logical_bytes > MAX_TOTAL_RECEIPT_BYTES
        {
            return Err(Error::OperationFailure(
                "rebased retrieval receipt exceeds its source-derived quota".into(),
            ));
        }
        while usage.receipt_count >= MAX_RECEIPTS
            || usage.receipt_bytes.saturating_add(logical_bytes) > MAX_TOTAL_RECEIPT_BYTES
            || usage.evidence_count.saturating_add(carried.len()) > MAX_TOTAL_EVIDENCE
            || usage.evidence_bytes.saturating_add(evidence_bytes) > MAX_TOTAL_EVIDENCE_BYTES
        {
            if !evict_oldest_receipt_except(&tx, Some(source_row_id))? {
                return Err(Error::OperationFailure(
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
                    evidence
                        .semantic_signature()
                        .map(|signature| signature as i64),
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
                params![usize_to_i64(carried.len())?, usize_to_i64(evidence_bytes)?],
            )?;
        }
        let receipt_id = format_receipt_id(&usage.namespace, row_id);
        tx.commit()?;
        Ok(receipt_id)
    }
}

pub(crate) fn receipt_usage(tx: &Transaction<'_>) -> Result<PersistentReceiptUsage> {
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

pub(crate) fn next_receipt_access_sequence(tx: &Transaction<'_>, current: i64) -> Result<i64> {
    let next = current.checked_add(1).ok_or_else(|| {
        Error::OperationFailure("retrieval receipt access sequence overflow".into())
    })?;
    let updated = tx.execute(
        "UPDATE retrieval_receipt_usage
         SET next_access_sequence = ?1
         WHERE id = 1 AND next_access_sequence = ?2",
        params![next, current],
    )?;
    if updated != 1 {
        return Err(Error::OperationFailure(
            "retrieval receipt access sequence changed unexpectedly".into(),
        ));
    }
    Ok(next)
}

pub(crate) fn load_receipt(
    tx: &Transaction<'_>,
    row_id: i64,
) -> Result<Option<PersistentReceiptRow>> {
    load_receipt_connection(tx, row_id)
}

pub(crate) fn load_receipt_connection(
    connection: &Connection,
    row_id: i64,
) -> Result<Option<PersistentReceiptRow>> {
    connection
        .query_row(
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

pub(crate) fn load_receipt_evidence(
    tx: &Transaction<'_>,
    receipt_id: i64,
) -> Result<Vec<ReceiptEvidence>> {
    load_receipt_evidence_connection(tx, receipt_id)
}

pub(crate) fn load_receipt_evidence_connection(
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
            Ok(ReceiptEvidence::from_stored(
                row.get(0)?,
                i64_to_usize(row.get(1)?)?,
                i64_to_usize(row.get(2)?)?,
                row.get(3)?,
                row.get::<_, Option<i64>>(4)?.map(|value| value as u64),
                row.get::<_, i64>(5)? != 0,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(Into::into)
}

pub(crate) fn prune_expired_receipts(tx: &Transaction<'_>, now_unix_millis: i64) -> Result<()> {
    tx.execute(
        "DELETE FROM retrieval_receipts
         WHERE expires_unix_millis <= ?1",
        [now_unix_millis],
    )?;
    Ok(())
}

pub(crate) fn evict_oldest_receipt_except(
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
    Ok(tx.execute("DELETE FROM retrieval_receipts WHERE id = ?1", [candidate])? == 1)
}

pub(crate) fn unix_millis(time: SystemTime) -> i64 {
    time.duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_millis()).ok())
        .unwrap_or(0)
}
use super::*;
