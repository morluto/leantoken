use super::*;
use crate::query_receipt::{MAX_QUERY_RECEIPT_ID_BYTES, QueryReceiptRecord, StoredQueryReceipt};
use crate::read_delta::{MAX_READ_DELTA_BASE_BYTES, ReadDeltaBase};
use crate::receipt::{
    MAX_EVIDENCE_BYTES_PER_RECEIPT, MAX_EVIDENCE_PER_RECEIPT, MAX_RECEIPT_EVIDENCE_LOGICAL_BYTES,
    MAX_RECEIPT_ID_BYTES, ReceiptDecision, ReceiptEvaluation, ReceiptEvidence, ReceiptRebaseSource,
    StoredReceipt, decide,
};

const ARTIFACT_SCHEMA_VERSION: u8 = 1;
const MAX_ARTIFACTS: usize = 256;
const MAX_ARTIFACT_BYTES: usize = 1024 * 1024;
const MAX_TOTAL_ARTIFACT_BYTES: usize = 16 * 1024 * 1024;

const ARTIFACT_SCHEMA: &str = r#"
PRAGMA journal_mode = WAL;
PRAGMA synchronous = NORMAL;
CREATE TABLE IF NOT EXISTS artifacts (
    id TEXT PRIMARY KEY CHECK(length(id) = 65),
    kind TEXT NOT NULL,
    repository_identity TEXT NOT NULL,
    database_incarnation_id TEXT NOT NULL DEFAULT '',
    repository_generation INTEGER NOT NULL CHECK(repository_generation >= 0),
    payload BLOB NOT NULL,
    logical_bytes INTEGER NOT NULL CHECK(logical_bytes >= 0)
);
"#;

const ARTIFACT_REPOSITORY_GENERATION_INDEX_SQL: &str = "
DROP INDEX IF EXISTS artifacts_repository_generation_idx;
CREATE INDEX artifacts_repository_generation_idx
ON artifacts(repository_identity, database_incarnation_id, repository_generation, kind, id);
";

#[derive(Clone)]
pub(crate) struct ArtifactStorage {
    pub(super) writer: Arc<Mutex<Connection>>,
}

impl fmt::Debug for ArtifactStorage {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ArtifactStorage")
            .finish_non_exhaustive()
    }
}

#[derive(serde::Serialize, serde::Deserialize)]
struct EvidenceArtifact {
    version: u8,
    evidence: Vec<ReceiptEvidence>,
}

struct StoredArtifact {
    repository_identity: String,
    database_incarnation_id: String,
    repository_generation: u64,
    payload: Vec<u8>,
}

impl ArtifactStorage {
    pub(crate) fn open(path: &Path) -> Self {
        match Self::open_connection(path) {
            Ok(connection) => Self {
                writer: Arc::new(Mutex::new(connection)),
            },
            Err(error) => {
                tracing::warn!(%error, path = %path.display(), "artifact storage unavailable; using process-local memory");
                let connection =
                    Self::open_memory().expect("bundled SQLite must support the artifact schema");
                Self {
                    writer: Arc::new(Mutex::new(connection)),
                }
            }
        }
    }

    fn open_connection(path: &Path) -> Result<Connection> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let connection = Connection::open(path)?;
        connection.busy_timeout(DEFAULT_BUSY_TIMEOUT)?;
        connection.execute_batch(ARTIFACT_SCHEMA)?;
        ensure_artifact_incarnation_column(&connection)?;
        connection.execute_batch(ARTIFACT_REPOSITORY_GENERATION_INDEX_SQL)?;
        Ok(connection)
    }

    fn open_memory() -> Result<Connection> {
        let connection = Connection::open_in_memory()?;
        connection.execute_batch(ARTIFACT_SCHEMA)?;
        connection.execute_batch(ARTIFACT_REPOSITORY_GENERATION_INDEX_SQL)?;
        Ok(connection)
    }

    pub(crate) fn evaluate_receipt(
        &self,
        repository_identity: &str,
        database_incarnation_id: &str,
        requested_id: Option<&str>,
        generation: u64,
        candidates: &[ReceiptEvidence],
        suppress_overlap: bool,
    ) -> Result<ReceiptEvaluation> {
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

        let mut evidence = match requested_id {
            Some(id) => {
                let stored = self.read_evidence_artifact(id)?;
                if stored.repository_identity != repository_identity
                    || stored.database_incarnation_id != database_incarnation_id
                {
                    return Err(Error::UnknownReceipt(id.to_owned()));
                }
                if stored.repository_generation != generation {
                    return Err(Error::StaleReceipt {
                        receipt_generation: stored.repository_generation,
                        repository_generation: generation,
                    });
                }
                stored.evidence
            }
            None => Vec::new(),
        };
        let mut decisions = Vec::with_capacity(candidates.len());
        let mut returned = Vec::new();
        for candidate in candidates {
            let decision = decide(&evidence, candidate, suppress_overlap);
            if matches!(
                decision,
                ReceiptDecision::Return | ReceiptDecision::ReturnNearDuplicate
            ) {
                returned.push(candidate.clone());
            }
            decisions.push(decision);
        }
        let evidence_bytes = evidence
            .iter()
            .chain(&returned)
            .map(ReceiptEvidence::logical_bytes)
            .sum::<usize>();
        if evidence.len().saturating_add(returned.len()) > MAX_EVIDENCE_PER_RECEIPT
            || evidence_bytes > MAX_EVIDENCE_BYTES_PER_RECEIPT
        {
            return Err(Error::OperationFailure(
                "evidence artifact exceeds its bounded payload".into(),
            ));
        }
        evidence.extend(returned);
        let receipt_id = self.put_evidence_artifact(
            repository_identity,
            database_incarnation_id,
            generation,
            &evidence,
        )?;
        Ok(ReceiptEvaluation {
            receipt_id,
            decisions,
        })
    }

    pub(crate) fn read_receipt(
        &self,
        repository_identity: &str,
        database_incarnation_id: &str,
        requested_id: &str,
    ) -> Result<StoredReceipt> {
        let stored = self.read_evidence_artifact(requested_id)?;
        if stored.repository_identity != repository_identity
            || stored.database_incarnation_id != database_incarnation_id
        {
            return Err(Error::UnknownReceipt(requested_id.to_owned()));
        }
        let complete = stored.evidence.len() < MAX_EVIDENCE_PER_RECEIPT
            && stored
                .evidence
                .iter()
                .map(ReceiptEvidence::logical_bytes)
                .sum::<usize>()
                .saturating_add(MAX_RECEIPT_EVIDENCE_LOGICAL_BYTES)
                <= MAX_EVIDENCE_BYTES_PER_RECEIPT;
        Ok(StoredReceipt {
            receipt_id: requested_id.to_owned(),
            repository_identity: stored.repository_identity,
            repository_generation: stored.repository_generation,
            complete,
            evidence: stored.evidence,
        })
    }

    pub(crate) fn load_receipt_rebase_source(
        &self,
        repository_identity: &str,
        database_incarnation_id: &str,
        requested_id: &str,
    ) -> Result<ReceiptRebaseSource> {
        let receipt =
            self.read_receipt(repository_identity, database_incarnation_id, requested_id)?;
        Ok(ReceiptRebaseSource {
            receipt_id: receipt.receipt_id,
            repository_identity: receipt.repository_identity,
            repository_generation: receipt.repository_generation,
            evidence: receipt.evidence,
        })
    }

    pub(crate) fn persist_rebased_receipt(
        &self,
        source: &ReceiptRebaseSource,
        repository_identity: &str,
        database_incarnation_id: &str,
        generation: u64,
        evidence: &[ReceiptEvidence],
    ) -> Result<String> {
        if source.repository_identity != repository_identity {
            return Err(Error::UnknownReceipt(source.receipt_id.clone()));
        }
        self.put_evidence_artifact(
            repository_identity,
            database_incarnation_id,
            generation,
            evidence,
        )
    }

    pub(crate) fn persist_query_receipt(
        &self,
        repository_identity: &str,
        database_incarnation_id: &str,
        record: &QueryReceiptRecord,
    ) -> Result<String> {
        let payload = serde_json::to_vec(record)?;
        self.put(
            "query_proof",
            'q',
            repository_identity,
            database_incarnation_id,
            record.repository_generation,
            &payload,
        )
    }

    pub(crate) fn load_query_receipt(
        &self,
        repository_identity: &str,
        database_incarnation_id: &str,
        requested_id: &str,
    ) -> Result<StoredQueryReceipt> {
        if requested_id.len() > MAX_QUERY_RECEIPT_ID_BYTES {
            return Err(Error::InputTooLong {
                field: "query receipt_id",
                max_bytes: MAX_QUERY_RECEIPT_ID_BYTES,
            });
        }
        let stored = self
            .get("query_proof", 'q', requested_id)?
            .ok_or_else(|| Error::UnknownQueryReceipt(requested_id.to_owned()))?;
        if stored.repository_identity != repository_identity
            || stored.database_incarnation_id != database_incarnation_id
        {
            return Err(Error::UnknownQueryReceipt(requested_id.to_owned()));
        }
        let record: QueryReceiptRecord = serde_json::from_slice(&stored.payload)
            .map_err(|_| Error::UnknownQueryReceipt(requested_id.to_owned()))?;
        if record.repository_generation != stored.repository_generation
            || record.predicate.digest()? != record.predicate_blake3
        {
            return Err(Error::UnknownQueryReceipt(requested_id.to_owned()));
        }
        Ok(StoredQueryReceipt {
            receipt_id: requested_id.to_owned(),
            repository_generation: record.repository_generation,
            config_hash: record.config_hash,
            predicate: record.predicate,
            predicate_blake3: record.predicate_blake3,
            partition: record.partition,
            match_count: record.match_count,
            result_blake3: record.result_blake3,
        })
    }

    pub(crate) fn persist_read_base(
        &self,
        repository_identity: &str,
        database_incarnation_id: &str,
        base: &ReadDeltaBase,
    ) -> Result<String> {
        if base.content.len() > MAX_READ_DELTA_BASE_BYTES {
            return Err(Error::OperationFailure(
                "read artifact content exceeds its bound".into(),
            ));
        }
        let payload = serde_json::to_vec(base)?;
        self.put(
            "read_base",
            'd',
            repository_identity,
            database_incarnation_id,
            base.generation,
            &payload,
        )
    }

    pub(crate) fn load_read_base(
        &self,
        repository_identity: &str,
        database_incarnation_id: &str,
        requested_id: &str,
    ) -> Result<ReadDeltaBase> {
        let invalid = || Error::InvalidInput {
            field: "delta_base_artifact_id",
            reason: "does not identify a valid read artifact",
        };
        let stored = self
            .get("read_base", 'd', requested_id)?
            .ok_or_else(invalid)?;
        if stored.repository_identity != repository_identity
            || stored.database_incarnation_id != database_incarnation_id
        {
            return Err(Error::InvalidInput {
                field: "delta_base_artifact_id",
                reason: "belongs to another repository",
            });
        }
        let base: ReadDeltaBase = serde_json::from_slice(&stored.payload).map_err(|_| invalid())?;
        if base.generation != stored.repository_generation
            || base.content.len() > MAX_READ_DELTA_BASE_BYTES
            || crate::text::hash(&base.content) != base.content_hash
        {
            return Err(invalid());
        }
        Ok(base)
    }

    fn put_evidence_artifact(
        &self,
        repository_identity: &str,
        database_incarnation_id: &str,
        generation: u64,
        evidence: &[ReceiptEvidence],
    ) -> Result<String> {
        let payload = serde_json::to_vec(&EvidenceArtifact {
            version: ARTIFACT_SCHEMA_VERSION,
            evidence: evidence.to_vec(),
        })?;
        self.put(
            "evidence",
            'r',
            repository_identity,
            database_incarnation_id,
            generation,
            &payload,
        )
    }

    fn read_evidence_artifact(&self, requested_id: &str) -> Result<StoredEvidenceArtifact> {
        let stored = self
            .get("evidence", 'r', requested_id)?
            .ok_or_else(|| Error::UnknownReceipt(requested_id.to_owned()))?;
        let artifact: EvidenceArtifact = serde_json::from_slice(&stored.payload)
            .map_err(|_| Error::UnknownReceipt(requested_id.to_owned()))?;
        if artifact.version != ARTIFACT_SCHEMA_VERSION
            || artifact.evidence.len() > MAX_EVIDENCE_PER_RECEIPT
            || artifact
                .evidence
                .iter()
                .map(ReceiptEvidence::logical_bytes)
                .sum::<usize>()
                > MAX_EVIDENCE_BYTES_PER_RECEIPT
        {
            return Err(Error::UnknownReceipt(requested_id.to_owned()));
        }
        Ok(StoredEvidenceArtifact {
            repository_identity: stored.repository_identity,
            database_incarnation_id: stored.database_incarnation_id,
            repository_generation: stored.repository_generation,
            evidence: artifact.evidence,
        })
    }

    fn put(
        &self,
        kind: &str,
        prefix: char,
        repository_identity: &str,
        database_incarnation_id: &str,
        generation: u64,
        payload: &[u8],
    ) -> Result<String> {
        if payload.len() > MAX_ARTIFACT_BYTES {
            return Err(Error::OperationFailure(
                "artifact payload exceeds its bound".into(),
            ));
        }
        let id = artifact_id(
            prefix,
            kind,
            repository_identity,
            database_incarnation_id,
            generation,
            payload,
        );
        let logical_bytes = id
            .len()
            .saturating_add(kind.len())
            .saturating_add(repository_identity.len())
            .saturating_add(database_incarnation_id.len())
            .saturating_add(payload.len())
            .saturating_add(2 * size_of::<u64>());
        let mut connection = self
            .writer
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM artifacts WHERE id = ?1)",
            [&id],
            |row| row.get::<_, bool>(0),
        )? {
            transaction.commit()?;
            return Ok(id);
        }
        let (count, bytes): (usize, usize) = transaction.query_row(
            "SELECT count(*), coalesce(sum(logical_bytes), 0) FROM artifacts",
            [],
            |row| Ok((i64_to_usize(row.get(0)?)?, i64_to_usize(row.get(1)?)?)),
        )?;
        if count >= MAX_ARTIFACTS || bytes.saturating_add(logical_bytes) > MAX_TOTAL_ARTIFACT_BYTES
        {
            return Err(Error::OperationFailure(
                "artifact storage capacity is exhausted".into(),
            ));
        }
        transaction.execute(
            "INSERT INTO artifacts(id, kind, repository_identity, database_incarnation_id,
                                   repository_generation, payload, logical_bytes)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                id,
                kind,
                repository_identity,
                database_incarnation_id,
                u64_to_i64(generation)?,
                payload,
                usize_to_i64(logical_bytes)?
            ],
        )?;
        transaction.commit()?;
        Ok(id)
    }

    fn get(&self, kind: &str, prefix: char, requested_id: &str) -> Result<Option<StoredArtifact>> {
        if !valid_artifact_id(requested_id, prefix) {
            return Ok(None);
        }
        let connection = self
            .writer
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let stored = connection
            .query_row(
                "SELECT repository_identity, database_incarnation_id, repository_generation, payload
                 FROM artifacts WHERE id = ?1 AND kind = ?2",
                params![requested_id, kind],
                |row| {
                    Ok(StoredArtifact {
                        repository_identity: row.get(0)?,
                        database_incarnation_id: row.get(1)?,
                        repository_generation: i64_to_u64(row.get(2)?)?,
                        payload: row.get(3)?,
                    })
                },
            )
            .optional()?;
        Ok(stored.filter(|stored| {
            artifact_id(
                prefix,
                kind,
                &stored.repository_identity,
                &stored.database_incarnation_id,
                stored.repository_generation,
                &stored.payload,
            ) == requested_id
        }))
    }
}

struct StoredEvidenceArtifact {
    repository_identity: String,
    database_incarnation_id: String,
    repository_generation: u64,
    evidence: Vec<ReceiptEvidence>,
}

fn artifact_id(
    prefix: char,
    kind: &str,
    repository_identity: &str,
    database_incarnation_id: &str,
    generation: u64,
    payload: &[u8],
) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"leantoken-artifact-v2\0");
    hasher.update(kind.as_bytes());
    hasher.update(&[0]);
    hasher.update(repository_identity.as_bytes());
    hasher.update(&[0]);
    hasher.update(database_incarnation_id.as_bytes());
    hasher.update(&[0]);
    hasher.update(&generation.to_le_bytes());
    hasher.update(payload);
    format!("{prefix}{}", hasher.finalize().to_hex())
}

fn ensure_artifact_incarnation_column(connection: &Connection) -> Result<()> {
    let has_column = connection
        .prepare("PRAGMA table_info(artifacts)")?
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<std::result::Result<Vec<_>, _>>()?
        .iter()
        .any(|column| column == "database_incarnation_id");
    if !has_column {
        connection.execute(
            "ALTER TABLE artifacts ADD COLUMN database_incarnation_id TEXT NOT NULL DEFAULT ''",
            [],
        )?;
        connection.execute("DELETE FROM artifacts", [])?;
    }
    Ok(())
}

fn valid_artifact_id(id: &str, prefix: char) -> bool {
    id.len() == 65
        && id.starts_with(prefix)
        && id[1..]
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}
