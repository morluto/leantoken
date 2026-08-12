use std::io::Read;

use tokio_util::sync::CancellationToken;

use super::execution_options::RetrievalExecution;
use super::index_read::RepositoryGeneration;
use super::read::open_live_file;
use super::receipts::ReceiptEvidence;
use super::validation::check_cancelled;
use super::{ServiceCallOptions, Services};
use crate::model::{
    DEFAULT_RECEIPT_REBASE_SAMPLES_PER_OUTCOME, IndexConsistency,
    MAX_RECEIPT_REBASE_SAMPLES_PER_OUTCOME, ReceiptRebaseCounts, ReceiptRebaseOutcomeKind,
    ReceiptRebaseRequest, ReceiptRebaseResponse, ReceiptRebaseSample, ReceiptRebaseSamples,
    TokenAccountingOperation, TokenSavingsRequestClass,
};
use crate::receipt::{MAX_REBASE_LIVE_BYTES, RECEIPT_ID_RESPONSE_RESERVE, ReceiptRebaseSource};
use crate::{Error, Result};

#[derive(Debug)]
struct ReceiptRebaseClassification {
    generation: u64,
    outcomes: Vec<ReceiptRebaseOutcomeKind>,
    carried: Vec<ReceiptEvidence>,
    counts: ReceiptRebaseCounts,
    outcomes_blake3: String,
}

impl Services {
    /// Carry exactly unchanged receipt evidence into the current generation.
    pub async fn rebase_receipt(
        &self,
        request: ReceiptRebaseRequest,
    ) -> Result<ReceiptRebaseResponse> {
        self.rebase_receipt_with_options(request, ServiceCallOptions::new())
            .await
    }

    /// Rebase a receipt under explicit serialized-response controls.
    pub async fn rebase_receipt_with_options(
        &self,
        request: ReceiptRebaseRequest,
        options: ServiceCallOptions,
    ) -> Result<ReceiptRebaseResponse> {
        self.rebase_receipt_execute(
            request,
            RetrievalExecution::direct(options, CancellationToken::new()),
        )
        .await
    }

    /// Rebase a receipt after applying an explicit index consistency boundary.
    pub async fn rebase_receipt_with_options_consistency_cancellable(
        &self,
        request: ReceiptRebaseRequest,
        consistency: IndexConsistency,
        options: ServiceCallOptions,
        cancellation: CancellationToken,
    ) -> Result<ReceiptRebaseResponse> {
        self.rebase_receipt_execute(
            request,
            RetrievalExecution::consistent(consistency, options, cancellation),
        )
        .await
    }

    async fn rebase_receipt_execute(
        &self,
        request: ReceiptRebaseRequest,
        execution: RetrievalExecution,
    ) -> Result<ReceiptRebaseResponse> {
        let operation = TokenAccountingOperation::ReceiptRebase;
        let RetrievalExecution {
            consistency,
            options,
            cancellation,
        } = execution;
        self.observe_service_result(operation, self.validate_call_options(options))?;
        let request = self.observe_service_result(operation, parse_rebase_request(request))?;
        if let Some(consistency) = consistency {
            let consistency_result = self
                .apply_consistency_with_initial_deadline(
                    consistency,
                    cancellation.clone(),
                    options.initial_reconciliation_deadline(),
                )
                .await;
            self.observe_service_result(operation, consistency_result)?;
        }
        let this = self.clone();
        let result = self
            .blocking_executor
            .run(cancellation, move |cancellation| {
                this.rebase_receipt_sync(request, options, cancellation)
            })
            .await;
        self.observe_service_result(operation, result)
    }

    fn rebase_receipt_sync(
        &self,
        request: ParsedReceiptRebaseRequest,
        options: ServiceCallOptions,
        cancellation: &CancellationToken,
    ) -> Result<ReceiptRebaseResponse> {
        check_cancelled(cancellation)?;
        let source = self
            .storage
            .load_receipt_rebase_source(&request.receipt_id)?;
        let classification = self.consistent(|session| {
            let generation = session.generation();
            if source.repository_generation >= generation {
                return Err(Error::InvalidInput {
                    field: "receipt_id",
                    reason: "must belong to an earlier repository generation",
                });
            }
            classify_receipt(self, session, generation, &source, cancellation)
        })?;
        let requested_samples = request.samples_per_outcome;
        let mut selected = None;
        for sample_limit in (0..=requested_samples).rev() {
            let response = build_response(self, &source, &classification, sample_limit);
            if self.response_fits_with_receipt_reserve(&response, 0, options)? {
                selected = Some(response);
                break;
            }
        }
        let mut response = if let Some(response) = selected {
            response
        } else {
            let response = build_response(self, &source, &classification, 0);
            let limit = options
                .max_response_tokens()
                .expect("response fitting fails only with a configured limit");
            return Err(
                self.response_budget_error_with_receipt_reserve(&response, 0, limit, options)?
            );
        };
        check_cancelled(cancellation)?;
        let receipt_id = self.storage.persist_rebased_receipt(
            &source,
            classification.generation,
            &classification.carried,
        )?;
        response.meta.receipt_id = Some(receipt_id);
        self.finalize_bounded_response(&mut response, options)?;
        self.record_token_savings_classified(
            TokenAccountingOperation::ReceiptRebase,
            None,
            &response.meta,
            TokenSavingsRequestClass::Useful,
        );
        Ok(response)
    }
}

struct ParsedReceiptRebaseRequest {
    receipt_id: String,
    samples_per_outcome: usize,
}

fn parse_rebase_request(request: ReceiptRebaseRequest) -> Result<ParsedReceiptRebaseRequest> {
    super::validation::validate_input(&request.receipt_id, "receipt_id", 128)?;
    if request
        .max_samples_per_outcome
        .is_some_and(|value| value > MAX_RECEIPT_REBASE_SAMPLES_PER_OUTCOME)
    {
        return Err(Error::RequestLimitExceeded {
            field: "max_samples_per_outcome",
            requested: request.max_samples_per_outcome.unwrap_or_default(),
            limit: MAX_RECEIPT_REBASE_SAMPLES_PER_OUTCOME,
        });
    }
    Ok(ParsedReceiptRebaseRequest {
        receipt_id: request.receipt_id,
        samples_per_outcome: request
            .max_samples_per_outcome
            .unwrap_or(DEFAULT_RECEIPT_REBASE_SAMPLES_PER_OUTCOME),
    })
}

fn classify_receipt(
    services: &Services,
    session: &RepositoryGeneration,
    generation: u64,
    source: &ReceiptRebaseSource,
    cancellation: &CancellationToken,
) -> Result<ReceiptRebaseClassification> {
    let mut order = (0..source.evidence.len()).collect::<Vec<_>>();
    order.sort_by(|left, right| {
        source.evidence[*left]
            .path
            .cmp(&source.evidence[*right].path)
            .then_with(|| left.cmp(right))
    });
    let mut outcomes = vec![ReceiptRebaseOutcomeKind::Unmapped; source.evidence.len()];
    let mut cursor = 0usize;
    let mut live_bytes = 0u64;
    while cursor < order.len() {
        check_cancelled(cancellation)?;
        let path = &source.evidence[order[cursor]].path;
        let end = order[cursor..]
            .iter()
            .position(|index| source.evidence[*index].path != *path)
            .map_or(order.len(), |offset| cursor.saturating_add(offset));
        let indices = &order[cursor..end];
        let Some(indexed) = session.find_file(path)? else {
            for index in indices {
                outcomes[*index] = ReceiptRebaseOutcomeKind::Missing;
            }
            cursor = end;
            continue;
        };
        let (text, observed_bytes) = load_exact_current_text(
            services,
            path,
            &indexed.content_hash,
            MAX_REBASE_LIVE_BYTES.saturating_sub(live_bytes),
        );
        live_bytes = live_bytes.saturating_add(observed_bytes);
        let Some(text) = text else {
            cursor = end;
            continue;
        };
        let line_starts = crate::text::line_starts(&text);
        for index in indices {
            check_cancelled(cancellation)?;
            let evidence = &source.evidence[*index];
            if evidence.start_line == 0
                || evidence.end_line < evidence.start_line
                || evidence.end_line > line_starts.len()
            {
                continue;
            }
            let (start, end) = crate::text::line_range_to_byte_range(
                &line_starts,
                text.len(),
                evidence.start_line,
                evidence.end_line,
            );
            let source_matches = crate::text::hash(&text[start..end]) == evidence.content_hash;
            let structural_matches = if source_matches {
                Some(true)
            } else {
                session.receipt_structural_hash_matches(
                    indexed.id,
                    evidence.start_line,
                    evidence.end_line,
                    &evidence.content_hash,
                )?
            };
            outcomes[*index] = match structural_matches {
                Some(true) => ReceiptRebaseOutcomeKind::Carried,
                Some(false) => ReceiptRebaseOutcomeKind::Changed,
                None => ReceiptRebaseOutcomeKind::Unmapped,
            };
        }
        cursor = end;
    }
    let mut counts = ReceiptRebaseCounts::default();
    let mut carried = Vec::new();
    for (evidence, outcome) in source.evidence.iter().zip(&outcomes) {
        match outcome {
            ReceiptRebaseOutcomeKind::Carried => {
                counts.carried = counts.carried.saturating_add(1);
                carried.push(evidence.clone());
            }
            ReceiptRebaseOutcomeKind::Changed => {
                counts.changed = counts.changed.saturating_add(1);
            }
            ReceiptRebaseOutcomeKind::Missing => {
                counts.missing = counts.missing.saturating_add(1);
            }
            ReceiptRebaseOutcomeKind::Unmapped => {
                counts.unmapped = counts.unmapped.saturating_add(1);
            }
        }
    }
    Ok(ReceiptRebaseClassification {
        generation,
        outcomes_blake3: classification_digest(source, generation, &outcomes),
        outcomes,
        carried,
        counts,
    })
}

fn load_exact_current_text(
    services: &Services,
    path: &str,
    indexed_content_hash: &str,
    remaining_bytes: u64,
) -> (Option<String>, u64) {
    let mut file = match open_live_file(services, path) {
        Ok(file) => file,
        Err(_) => return (None, 0),
    };
    let metadata_bytes = match file.metadata() {
        Ok(metadata) => metadata.len(),
        Err(_) => return (None, 0),
    };
    if metadata_bytes > services.config.max_file_bytes || metadata_bytes > remaining_bytes {
        return (None, 0);
    }
    let mut bytes = Vec::new();
    if (&mut file)
        .take(metadata_bytes)
        .read_to_end(&mut bytes)
        .is_err()
    {
        return (None, 0);
    }
    let payload_bytes = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
    if payload_bytes != metadata_bytes {
        return (None, payload_bytes);
    }
    let final_metadata_bytes = match file.metadata() {
        Ok(metadata) => metadata.len(),
        Err(_) => return (None, payload_bytes),
    };
    if final_metadata_bytes != metadata_bytes
        || crate::text::hash_bytes(&bytes) != indexed_content_hash
    {
        return (None, payload_bytes);
    }
    (String::from_utf8(bytes).ok(), payload_bytes)
}

fn classification_digest(
    source: &ReceiptRebaseSource,
    generation: u64,
    outcomes: &[ReceiptRebaseOutcomeKind],
) -> String {
    fn update(hasher: &mut blake3::Hasher, bytes: &[u8]) {
        hasher.update(&(bytes.len() as u64).to_le_bytes());
        hasher.update(bytes);
    }
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"leantoken-receipt-rebase-v1\0");
    update(&mut hasher, source.receipt_id.as_bytes());
    hasher.update(&source.repository_generation.to_le_bytes());
    hasher.update(&generation.to_le_bytes());
    for (ordinal, (evidence, outcome)) in source.evidence.iter().zip(outcomes).enumerate() {
        hasher.update(&(ordinal as u64).to_le_bytes());
        update(&mut hasher, evidence.path.as_bytes());
        hasher.update(&(evidence.start_line as u64).to_le_bytes());
        hasher.update(&(evidence.end_line as u64).to_le_bytes());
        update(&mut hasher, evidence.content_hash.as_bytes());
        hasher.update(&[match outcome {
            ReceiptRebaseOutcomeKind::Carried => 0,
            ReceiptRebaseOutcomeKind::Changed => 1,
            ReceiptRebaseOutcomeKind::Missing => 2,
            ReceiptRebaseOutcomeKind::Unmapped => 3,
        }]);
    }
    hasher.finalize().to_hex().to_string()
}

fn build_response(
    services: &Services,
    source: &ReceiptRebaseSource,
    classification: &ReceiptRebaseClassification,
    sample_limit: usize,
) -> ReceiptRebaseResponse {
    let mut samples = ReceiptRebaseSamples::default();
    for (ordinal, (evidence, outcome)) in source
        .evidence
        .iter()
        .zip(&classification.outcomes)
        .enumerate()
    {
        let bucket = match outcome {
            ReceiptRebaseOutcomeKind::Carried => &mut samples.carried,
            ReceiptRebaseOutcomeKind::Changed => &mut samples.changed,
            ReceiptRebaseOutcomeKind::Missing => &mut samples.missing,
            ReceiptRebaseOutcomeKind::Unmapped => &mut samples.unmapped,
        };
        if bucket.len() < sample_limit {
            bucket.push(ReceiptRebaseSample {
                ordinal,
                path: evidence.path.clone(),
                start_line: evidence.start_line,
                end_line: evidence.end_line,
            });
        }
    }
    let sampled = samples
        .carried
        .len()
        .saturating_add(samples.changed.len())
        .saturating_add(samples.missing.len())
        .saturating_add(samples.unmapped.len());
    let mut meta = services.meta(classification.generation, 0, None);
    meta.receipt_id = Some(RECEIPT_ID_RESPONSE_RESERVE.into());
    ReceiptRebaseResponse {
        source_receipt_id: source.receipt_id.clone(),
        source_repository_generation: source.repository_generation,
        counts: classification.counts.clone(),
        samples,
        samples_complete: sampled == classification.counts.total(),
        outcomes_blake3: classification.outcomes_blake3.clone(),
        meta,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn live_validation_refuses_a_file_before_crossing_the_cumulative_bound() {
        let root = tempfile::tempdir().expect("temporary repository");
        let content = "0123456789";
        std::fs::write(root.path().join("bounded.rs"), content).expect("write fixture");
        let config = crate::Config::discover(root.path(), Some(root.path().join("index.sqlite")))
            .expect("config");
        let services = Services::open(config).expect("services");
        let digest = crate::text::hash(content);

        assert_eq!(
            load_exact_current_text(&services, "bounded.rs", &digest, 9),
            (None, 0)
        );
        assert_eq!(
            load_exact_current_text(&services, "bounded.rs", &digest, 10),
            (Some(content.into()), 10)
        );
    }
}
