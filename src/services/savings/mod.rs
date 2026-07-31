use super::*;

const TOKEN_SAVINGS_ESTIMATE_BASIS: &str = "represented-source baseline minus source tokens emitted in successful useful responses for \
    newly classified records; incomplete, unsupported, hash-suppressed, and failed requests are \
    excluded";
const RESPONSE_ACCOUNTING_SCOPE: &str = "successful repository retrieval responses recorded after \
    full-response accounting was enabled; includes successful retries as separate requests but \
    excludes pre-response failures, tool discovery, task success, and native-tool costs; request \
    outcome classes remain separate from this complete response-cost total";
const RESPONSE_ACCOUNTING_ESTIMATE_BASIS: &str =
    "represented-source baseline minus complete serialized response tokens";
const OBSERVATION_SCOPE: &str = "repository-local best-effort service records; successful responses \
    are recorded after final token accounting, failures at instrumented service-operation \
    boundaries, outcome classes are mutually exclusive for newly recorded successes, and busy \
    telemetry writers are skipped without delaying retrieval";
const UNOBSERVED_OUTCOMES: [&str; 4] = [
    "retry chains without a host task/outcome identifier",
    "unused or irrelevant returned evidence",
    "superseded calls",
    "task completion or success",
];
const SAVINGS_SNAPSHOT_VERSION: u8 = 1;
const MAX_SAVINGS_SNAPSHOT_BYTES: usize = 32 * 1024;

#[derive(Debug, Serialize, Deserialize)]
struct TokenSavingsSnapshotState {
    version: u8,
    repository_id: String,
    tokenizer: String,
    records: Vec<(String, [u64; 19])>,
    failures: Vec<(String, String, u64)>,
}

fn snapshot_invalid() -> Error {
    Error::InvalidInput {
        field: "snapshot",
        reason: "must be a valid compatible savings snapshot whose counters do not exceed current totals",
    }
}

fn savings_snapshot_checksum(payload: &[u8]) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"leantoken-savings-snapshot-v1\0");
    hasher.update(payload);
    hasher.finalize().to_hex()[..32].to_string()
}

fn encode_savings_snapshot(state: &TokenSavingsSnapshotState) -> Result<String> {
    let payload = serde_json::to_vec(state)?;
    let snapshot = format!(
        "lts1.{}.{}",
        URL_SAFE_NO_PAD.encode(&payload),
        savings_snapshot_checksum(&payload)
    );
    if snapshot.len() > MAX_SAVINGS_SNAPSHOT_BYTES {
        return Err(Error::RequestLimitExceeded {
            field: "snapshot",
            requested: snapshot.len(),
            limit: MAX_SAVINGS_SNAPSHOT_BYTES,
        });
    }
    Ok(snapshot)
}

fn decode_savings_snapshot(snapshot: &str) -> Result<TokenSavingsSnapshotState> {
    if snapshot.len() > MAX_SAVINGS_SNAPSHOT_BYTES {
        return Err(Error::RequestLimitExceeded {
            field: "snapshot",
            requested: snapshot.len(),
            limit: MAX_SAVINGS_SNAPSHOT_BYTES,
        });
    }
    let mut parts = snapshot.split('.');
    if parts.next() != Some("lts1") {
        return Err(snapshot_invalid());
    }
    let payload = parts
        .next()
        .and_then(|value| URL_SAFE_NO_PAD.decode(value).ok())
        .ok_or_else(snapshot_invalid)?;
    let checksum = parts.next().ok_or_else(snapshot_invalid)?;
    if parts.next().is_some() || checksum != savings_snapshot_checksum(&payload) {
        return Err(snapshot_invalid());
    }
    let state: TokenSavingsSnapshotState =
        serde_json::from_slice(&payload).map_err(|_| snapshot_invalid())?;
    if state.version != SAVINGS_SNAPSHOT_VERSION {
        return Err(snapshot_invalid());
    }
    Ok(state)
}

fn subtract_savings_record(
    current: &TokenSavingsRecord,
    base: &TokenSavingsRecord,
) -> Option<TokenSavingsRecord> {
    macro_rules! difference {
        ($field:ident) => {
            current.$field.checked_sub(base.$field)?
        };
    }
    Some(TokenSavingsRecord {
        tracked_requests: difference!(tracked_requests),
        response_tracked_requests: difference!(response_tracked_requests),
        response_baseline_requests: difference!(response_baseline_requests),
        baseline_source_tokens: difference!(baseline_source_tokens),
        response_baseline_source_tokens: difference!(response_baseline_source_tokens),
        emitted_source_tokens: difference!(emitted_source_tokens),
        estimated_source_tokens_saved: difference!(estimated_source_tokens_saved),
        response_source_tokens: difference!(response_source_tokens),
        path_and_metadata_tokens: difference!(path_and_metadata_tokens),
        protocol_tokens: difference!(protocol_tokens),
        total_response_tokens: difference!(total_response_tokens),
        receipt_suppressed_exact: difference!(receipt_suppressed_exact),
        receipt_suppressed_overlap: difference!(receipt_suppressed_overlap),
        expected_hash_not_modified_responses: difference!(expected_hash_not_modified_responses),
        expected_hash_suppressed_source_tokens: difference!(expected_hash_suppressed_source_tokens),
        useful_requests: difference!(useful_requests),
        incomplete_requests: difference!(incomplete_requests),
        unsupported_requests: difference!(unsupported_requests),
        hash_suppressed_requests: difference!(hash_suppressed_requests),
    })
}

fn savings_record_values(record: &TokenSavingsRecord) -> [u64; 19] {
    [
        record.tracked_requests,
        record.response_tracked_requests,
        record.response_baseline_requests,
        record.baseline_source_tokens,
        record.response_baseline_source_tokens,
        record.emitted_source_tokens,
        record.estimated_source_tokens_saved,
        record.response_source_tokens,
        record.path_and_metadata_tokens,
        record.protocol_tokens,
        record.total_response_tokens,
        record.receipt_suppressed_exact,
        record.receipt_suppressed_overlap,
        record.expected_hash_not_modified_responses,
        record.expected_hash_suppressed_source_tokens,
        record.useful_requests,
        record.incomplete_requests,
        record.unsupported_requests,
        record.hash_suppressed_requests,
    ]
}

fn savings_record_from_values(values: [u64; 19]) -> TokenSavingsRecord {
    TokenSavingsRecord {
        tracked_requests: values[0],
        response_tracked_requests: values[1],
        response_baseline_requests: values[2],
        baseline_source_tokens: values[3],
        response_baseline_source_tokens: values[4],
        emitted_source_tokens: values[5],
        estimated_source_tokens_saved: values[6],
        response_source_tokens: values[7],
        path_and_metadata_tokens: values[8],
        protocol_tokens: values[9],
        total_response_tokens: values[10],
        receipt_suppressed_exact: values[11],
        receipt_suppressed_overlap: values[12],
        expected_hash_not_modified_responses: values[13],
        expected_hash_suppressed_source_tokens: values[14],
        useful_requests: values[15],
        incomplete_requests: values[16],
        unsupported_requests: values[17],
        hash_suppressed_requests: values[18],
    }
}

fn savings_snapshot_state(
    repository_id: String,
    tokenizer: String,
    records: &HashMap<String, TokenSavingsRecord>,
    failures: &[ServiceFailureRecord],
) -> TokenSavingsSnapshotState {
    let mut records = records
        .iter()
        .map(|(operation, record)| (operation.clone(), savings_record_values(record)))
        .collect::<Vec<_>>();
    records.sort_by(|left, right| left.0.cmp(&right.0));
    let mut failures = failures
        .iter()
        .map(|record| {
            (
                record.operation.clone(),
                record.error_category.clone(),
                record.failed_requests,
            )
        })
        .collect::<Vec<_>>();
    failures.sort_by(|left, right| (&left.0, &left.1).cmp(&(&right.0, &right.1)));
    TokenSavingsSnapshotState {
        version: SAVINGS_SNAPSHOT_VERSION,
        repository_id,
        tokenizer,
        records,
        failures,
    }
}

fn subtract_savings_records(
    current: &HashMap<String, TokenSavingsRecord>,
    base: Vec<(String, [u64; 19])>,
) -> Result<HashMap<String, TokenSavingsRecord>> {
    let mut delta = current.clone();
    for (operation, base_record) in base {
        let base_record = savings_record_from_values(base_record);
        let current_record = current.get(&operation).cloned().unwrap_or_default();
        let difference =
            subtract_savings_record(&current_record, &base_record).ok_or_else(snapshot_invalid)?;
        delta.insert(operation, difference);
    }
    Ok(delta)
}

fn subtract_service_failures(
    current: &[ServiceFailureRecord],
    base: Vec<(String, String, u64)>,
) -> Result<Vec<ServiceFailureRecord>> {
    let current = current
        .iter()
        .map(|record| {
            (
                (record.operation.clone(), record.error_category.clone()),
                record.failed_requests,
            )
        })
        .collect::<HashMap<_, _>>();
    let mut delta = current.clone();
    for (operation, error_category, failed_requests) in base {
        let key = (operation, error_category);
        let current_count = current.get(&key).copied().unwrap_or(0);
        delta.insert(
            key,
            current_count
                .checked_sub(failed_requests)
                .ok_or_else(snapshot_invalid)?,
        );
    }
    let mut delta = delta
        .into_iter()
        .filter(|(_, failed_requests)| *failed_requests != 0)
        .map(
            |((operation, error_category), failed_requests)| ServiceFailureRecord {
                operation,
                error_category,
                failed_requests,
            },
        )
        .collect::<Vec<_>>();
    delta.sort_by(|left, right| {
        (&left.operation, &left.error_category).cmp(&(&right.operation, &right.error_category))
    });
    Ok(delta)
}

pub(super) fn signed_token_difference(baseline: u64, response: u64) -> i64 {
    let difference = i128::from(baseline) - i128::from(response);
    difference.clamp(i128::from(i64::MIN), i128::from(i64::MAX)) as i64
}

fn service_failure_observation(record: ServiceFailureRecord) -> Result<ServiceFailureObservation> {
    let operation = TokenAccountingOperation::from_str(&record.operation).ok_or_else(|| {
        Error::OperationFailure(format!(
            "unknown observed service operation: {}",
            record.operation
        ))
    })?;
    Ok(ServiceFailureObservation {
        operation,
        error_category: record.error_category,
        failed_requests: record.failed_requests,
    })
}

impl Services {
    /// Return cumulative source-token savings estimates for this repository and tokenizer.
    pub async fn token_savings(&self) -> Result<TokenSavingsResponse> {
        let this = self.clone();
        self.blocking_executor
            .run(CancellationToken::new(), move |_| this.token_savings_sync())
            .await
    }

    fn token_savings_sync(&self) -> Result<TokenSavingsResponse> {
        let tokenizer = self.config.tokenizer.name();
        let stored = self.storage.token_savings(tokenizer)?;
        Ok(self.source_savings_from_records(&stored))
    }

    /// Return complete successful-response accounting.
    pub async fn token_savings_report(&self) -> Result<TokenSavingsReport> {
        let this = self.clone();
        self.blocking_executor
            .run(CancellationToken::new(), move |_| {
                this.token_savings_report_sync()
            })
            .await
    }

    /// Return response accounting plus directly observed service outcomes.
    pub async fn observed_token_savings_report(&self) -> Result<ObservedTokenSavingsReport> {
        let this = self.clone();
        self.blocking_executor
            .run(CancellationToken::new(), move |_| {
                this.observed_token_savings_report_sync()
            })
            .await
    }

    /// Return lifetime or caller-carried aggregate-delta accounting plus a new snapshot.
    pub async fn observed_token_savings_snapshot(
        &self,
        snapshot: Option<String>,
    ) -> Result<TokenSavingsSnapshotReport> {
        let this = self.clone();
        self.blocking_executor
            .run(CancellationToken::new(), move |_| {
                this.observed_token_savings_snapshot_sync(snapshot)
            })
            .await
    }

    fn observed_token_savings_report_sync(&self) -> Result<ObservedTokenSavingsReport> {
        let tokenizer = self.config.tokenizer.name();
        let session = self.storage.begin_read()?;
        let stored = session.token_savings(tokenizer)?;
        let failures = session.service_failures(tokenizer)?;
        self.observed_token_savings_report_from_records(&stored, failures)
    }

    fn observed_token_savings_snapshot_sync(
        &self,
        snapshot: Option<String>,
    ) -> Result<TokenSavingsSnapshotReport> {
        let tokenizer = self.config.tokenizer.name();
        let repository_id = self.repository_id();
        let session = self.storage.begin_read()?;
        let current_records = session.token_savings(tokenizer)?;
        let current_failures = session.service_failures(tokenizer)?;
        let current_state = savings_snapshot_state(
            repository_id.clone(),
            tokenizer.to_owned(),
            &current_records,
            &current_failures,
        );
        let next_snapshot = encode_savings_snapshot(&current_state)?;
        let (records, failures, window) = if let Some(snapshot) = snapshot {
            let base = decode_savings_snapshot(&snapshot)?;
            if base.repository_id != repository_id || base.tokenizer != tokenizer {
                return Err(snapshot_invalid());
            }
            (
                subtract_savings_records(&current_records, base.records)?,
                subtract_service_failures(&current_failures, base.failures)?,
                TokenSavingsWindow::Delta,
            )
        } else {
            (
                current_records,
                current_failures,
                TokenSavingsWindow::Lifetime,
            )
        };
        Ok(TokenSavingsSnapshotReport {
            observed: self.observed_token_savings_report_from_records(&records, failures)?,
            snapshot: next_snapshot,
            window,
        })
    }

    fn observed_token_savings_report_from_records(
        &self,
        stored: &HashMap<String, TokenSavingsRecord>,
        failures: Vec<ServiceFailureRecord>,
    ) -> Result<ObservedTokenSavingsReport> {
        let report = self.token_savings_report_from_records(stored);
        let expected_hash_not_modified_responses = stored
            .values()
            .map(|record| record.expected_hash_not_modified_responses)
            .fold(0u64, u64::saturating_add);
        let expected_hash_suppressed_source_tokens = stored
            .values()
            .map(|record| record.expected_hash_suppressed_source_tokens)
            .fold(0u64, u64::saturating_add);
        let failed_service_requests = failures
            .iter()
            .map(|record| record.failed_requests)
            .fold(0u64, u64::saturating_add);
        let unsupported_failures = failures
            .iter()
            .filter(|record| record.error_category == "unsupported_language")
            .map(|record| record.failed_requests)
            .fold(0u64, u64::saturating_add);
        let failed_by_operation_and_category = failures
            .into_iter()
            .map(service_failure_observation)
            .collect::<Result<Vec<_>>>()?;
        let useful = stored
            .values()
            .map(|record| record.useful_requests)
            .fold(0u64, u64::saturating_add);
        let incomplete = stored
            .values()
            .map(|record| record.incomplete_requests)
            .fold(0u64, u64::saturating_add);
        let successful_unsupported = stored
            .values()
            .map(|record| record.unsupported_requests)
            .fold(0u64, u64::saturating_add);
        let unsupported = successful_unsupported.saturating_add(unsupported_failures);
        let hash_suppressed = stored
            .values()
            .map(|record| record.hash_suppressed_requests)
            .fold(0u64, u64::saturating_add);
        Ok(ObservedTokenSavingsReport {
            observations: TokenSavingsObservations {
                observation_scope: OBSERVATION_SCOPE.to_owned(),
                successful_response_records: report.response_accounting.tracked_requests,
                responses_with_baseline: report.response_accounting.baseline_requests,
                failed_service_requests,
                expected_hash_not_modified_responses,
                expected_hash_suppressed_source_tokens,
                request_classification: TokenSavingsRequestClassification {
                    useful,
                    incomplete,
                    unsupported,
                    hash_suppressed,
                    failed: failed_service_requests.saturating_sub(unsupported_failures),
                },
                failed_by_operation_and_category,
                unobserved: UNOBSERVED_OUTCOMES.map(str::to_owned).to_vec(),
            },
            report,
        })
    }

    fn token_savings_report_sync(&self) -> Result<TokenSavingsReport> {
        let tokenizer = self.config.tokenizer.name();
        let stored = self.storage.token_savings(tokenizer)?;
        Ok(self.token_savings_report_from_records(&stored))
    }

    fn token_savings_report_from_records(
        &self,
        stored: &HashMap<String, TokenSavingsRecord>,
    ) -> TokenSavingsReport {
        let mut tracked_requests = 0u64;
        let mut baseline_requests = 0u64;
        let mut baseline_source_tokens = 0u64;
        let mut response_source_tokens = 0u64;
        let mut path_and_metadata_tokens = 0u64;
        let mut protocol_tokens = 0u64;
        let mut total_response_tokens = 0u64;
        let mut receipt_suppressed_exact = 0u64;
        let mut receipt_suppressed_overlap = 0u64;
        let by_operation = TokenAccountingOperation::ALL
            .into_iter()
            .map(|operation| {
                let record = stored.get(operation.as_str()).cloned().unwrap_or_default();
                tracked_requests =
                    tracked_requests.saturating_add(record.response_tracked_requests);
                baseline_requests =
                    baseline_requests.saturating_add(record.response_baseline_requests);
                baseline_source_tokens =
                    baseline_source_tokens.saturating_add(record.response_baseline_source_tokens);
                response_source_tokens =
                    response_source_tokens.saturating_add(record.response_source_tokens);
                path_and_metadata_tokens =
                    path_and_metadata_tokens.saturating_add(record.path_and_metadata_tokens);
                protocol_tokens = protocol_tokens.saturating_add(record.protocol_tokens);
                total_response_tokens =
                    total_response_tokens.saturating_add(record.total_response_tokens);
                receipt_suppressed_exact =
                    receipt_suppressed_exact.saturating_add(record.receipt_suppressed_exact);
                receipt_suppressed_overlap =
                    receipt_suppressed_overlap.saturating_add(record.receipt_suppressed_overlap);
                ResponseTokenAccountingByOperation {
                    operation,
                    tracked_requests: record.response_tracked_requests,
                    baseline_requests: record.response_baseline_requests,
                    baseline_source_tokens: record.response_baseline_source_tokens,
                    response_source_tokens: record.response_source_tokens,
                    path_and_metadata_tokens: record.path_and_metadata_tokens,
                    protocol_tokens: record.protocol_tokens,
                    total_response_tokens: record.total_response_tokens,
                    estimated_net_tokens_saved: signed_token_difference(
                        record.response_baseline_source_tokens,
                        record.total_response_tokens,
                    ),
                    receipt_suppressed_exact: record.receipt_suppressed_exact,
                    receipt_suppressed_overlap: record.receipt_suppressed_overlap,
                }
            })
            .collect();
        TokenSavingsReport {
            response_accounting: ResponseTokenAccounting {
                accounting_scope: RESPONSE_ACCOUNTING_SCOPE.to_owned(),
                estimate_basis: RESPONSE_ACCOUNTING_ESTIMATE_BASIS.to_owned(),
                tracked_requests,
                baseline_requests,
                baseline_source_tokens,
                response_source_tokens,
                path_and_metadata_tokens,
                protocol_tokens,
                total_response_tokens,
                estimated_net_tokens_saved: signed_token_difference(
                    baseline_source_tokens,
                    total_response_tokens,
                ),
                receipt_suppressed_exact,
                receipt_suppressed_overlap,
                by_operation,
            },
        }
    }

    fn source_savings_from_records(
        &self,
        stored: &HashMap<String, TokenSavingsRecord>,
    ) -> TokenSavingsResponse {
        let tokenizer = self.config.tokenizer.name();
        let mut tracked_requests = 0u64;
        let mut baseline_source_tokens = 0u64;
        let mut emitted_source_tokens = 0u64;
        let mut estimated_source_tokens_saved = 0u64;
        let by_operation = TokenSavingsOperation::ALL
            .into_iter()
            .map(|operation| {
                let record = stored.get(operation.as_str()).cloned().unwrap_or_default();
                tracked_requests = tracked_requests.saturating_add(record.tracked_requests);
                baseline_source_tokens =
                    baseline_source_tokens.saturating_add(record.baseline_source_tokens);
                emitted_source_tokens =
                    emitted_source_tokens.saturating_add(record.emitted_source_tokens);
                estimated_source_tokens_saved = estimated_source_tokens_saved
                    .saturating_add(record.estimated_source_tokens_saved);
                TokenSavingsByOperation {
                    operation,
                    tracked_requests: record.tracked_requests,
                    baseline_source_tokens: record.baseline_source_tokens,
                    emitted_source_tokens: record.emitted_source_tokens,
                    estimated_source_tokens_saved: record.estimated_source_tokens_saved,
                }
            })
            .collect();
        TokenSavingsResponse {
            tokenizer: tokenizer.to_owned(),
            token_count_exact: self.config.tokenizer.is_exact(),
            estimate_basis: TOKEN_SAVINGS_ESTIMATE_BASIS.to_owned(),
            tracked_requests,
            baseline_source_tokens,
            emitted_source_tokens,
            estimated_source_tokens_saved,
            by_operation,
        }
    }

    pub(super) fn record_token_savings(
        &self,
        operation: TokenAccountingOperation,
        baseline_source_tokens: Option<usize>,
        meta: &ResponseMeta,
    ) {
        let classification = if meta.next_cursor.is_some() {
            TokenSavingsRequestClass::Incomplete
        } else {
            TokenSavingsRequestClass::Useful
        };
        self.record_token_savings_classified(
            operation,
            baseline_source_tokens,
            meta,
            classification,
        );
    }

    pub(super) fn record_token_savings_classified(
        &self,
        operation: TokenAccountingOperation,
        baseline_source_tokens: Option<usize>,
        meta: &ResponseMeta,
        classification: TokenSavingsRequestClass,
    ) {
        self.record_token_savings_with_expected_hash(
            operation,
            baseline_source_tokens,
            meta,
            classification,
            false,
            0,
        );
    }

    pub(super) fn record_token_savings_with_expected_hash(
        &self,
        operation: TokenAccountingOperation,
        baseline_source_tokens: Option<usize>,
        meta: &ResponseMeta,
        classification: TokenSavingsRequestClass,
        expected_hash_not_modified: bool,
        expected_hash_suppressed_source_tokens: usize,
    ) {
        match self.storage.record_token_savings(
            self.config.tokenizer.name(),
            TokenSavingsObservation {
                operation,
                baseline_source_tokens,
                meta,
                classification,
                expected_hash_not_modified,
                expected_hash_suppressed_source_tokens,
            },
        ) {
            Ok(true) => {}
            Ok(false) => tracing::debug!(
                operation = operation.as_str(),
                "token-savings accounting skipped a busy writer"
            ),
            Err(error) => tracing::warn!(
                %error,
                operation = operation.as_str(),
                "token-savings accounting was skipped"
            ),
        }
    }
}
