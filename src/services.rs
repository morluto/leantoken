use std::collections::{BTreeMap, HashMap};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use std::thread;
use std::time::{Duration, Instant};

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use cap_std::fs::Dir;
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

use crate::config::{INDEX_CONTENT_VERSION, MAX_OUTPUT_TOKENS};
use crate::coordination::{CacheLease, IndexCoordination, IndexLeadership};
use crate::error::RetryableOperation;
use crate::indexer::{Indexer, index_progress_cache_namespace};
use crate::model::*;
use crate::storage::{
    ParserCoverageRows, ServiceFailureRecord, Storage, StorageCounts, TokenSavingsObservation,
    TokenSavingsRecord,
};
use crate::{Config, Error, Result};

mod accounting;
mod change_receipt;
#[cfg(test)]
mod concurrency_profile;
mod context;
mod coverage;
mod execution_options;
mod executor;
mod files;
mod handoff;
mod history;
mod index_read;
mod indexing;
mod json;
mod observer;
mod outline;
mod read;
mod read_delta;
mod receipt_rebase;
mod receipts;
mod reconciliation;
mod savings;
mod search;
mod startup;
mod status;
pub(crate) mod validation;

pub use context::ContextWorkflowOptions;
pub(crate) use context::MAX_CONTEXT_FOCUS_CANDIDATES_PER_PATTERN;
pub(crate) use history::MAX_DIFF_SYMBOL_TARGETS;
pub(crate) use json::{JsonExecutionOptions, MAX_JSON_DEPTH};

pub(crate) const MAX_EXPECTED_REPOSITORY_ID_BYTES: usize = 128;

pub(crate) fn retrieval_primitive_key(
    generation: u64,
    kind: &str,
    normalized_inputs: &str,
) -> RetrievalPrimitiveKey {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"leantoken-retrieval-primitive-v1\0");
    hasher.update(&generation.to_le_bytes());
    hasher.update(&(kind.len() as u64).to_le_bytes());
    hasher.update(kind.as_bytes());
    hasher.update(&(normalized_inputs.len() as u64).to_le_bytes());
    hasher.update(normalized_inputs.as_bytes());
    RetrievalPrimitiveKey {
        kind: kind.to_owned(),
        key_blake3: hasher.finalize().to_hex().to_string(),
    }
}

pub(crate) fn validate_positive_request_limit(
    field: &'static str,
    requested: usize,
    limit: usize,
) -> Result<usize> {
    if requested == 0 {
        return Err(Error::InvalidInput {
            field,
            reason: "must be greater than zero",
        });
    }
    validate_request_limit(field, requested, limit)
}

pub(crate) fn validate_request_limit(
    field: &'static str,
    requested: usize,
    limit: usize,
) -> Result<usize> {
    if requested > limit {
        return Err(Error::RequestLimitExceeded {
            field,
            requested,
            limit,
        });
    }
    Ok(requested)
}

#[derive(Debug, Clone)]
/// Shared application services used by both CLI and MCP adapters.
///
/// Blocking filesystem and SQLite work runs on Tokio's blocking pool. Index
/// reconciliations are serialized across processes, while reads use committed
/// SQLite WAL snapshots so every query in one response sees the same generation.
pub struct Services {
    config: Arc<Config>,
    storage: Storage,
    indexer: Indexer,
    repository_root: Arc<Dir>,
    coordination: IndexCoordination,
    _cache_lease: CacheLease,
    active_reconciliations: Arc<AtomicUsize>,
    reconciliation_changed: Arc<tokio::sync::Notify>,
    read_deltas: Arc<read_delta::ReadDeltaRegistry>,
    blocking_executor: executor::BlockingExecutor,
    response_accountant: accounting::ResponseAccountant,
    observer: observer::ServiceObserver,
    reconciliation: reconciliation::ReconciliationCoordinator,
    context_exclude_paths: validation::PathMatcher,
}

/// Per-call response controls shared by service entry points.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct ServiceCallOptions {
    max_response_tokens: Option<usize>,
    receipt_resource_reserve: bool,
    context_response_profile: Option<ContextResponseProfile>,
    initial_reconciliation_deadline: Option<tokio::time::Instant>,
}

impl ServiceCallOptions {
    /// Construct call options without a serialized-response ceiling.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            max_response_tokens: None,
            receipt_resource_reserve: false,
            context_response_profile: None,
            initial_reconciliation_deadline: None,
        }
    }

    /// Set the inclusive serialized service-response token ceiling.
    ///
    /// Services reject zero or values above the public 32,000-token ceiling
    /// before retrieval or consistency work begins.
    #[must_use]
    pub const fn with_max_response_tokens(mut self, max_response_tokens: usize) -> Self {
        self.max_response_tokens = Some(max_response_tokens);
        self
    }

    /// Return the configured serialized service-response ceiling.
    #[must_use]
    pub const fn max_response_tokens(self) -> Option<usize> {
        self.max_response_tokens
    }

    /// Reserve space for the adapter's optional receipt-resource decoration.
    #[must_use]
    pub const fn with_receipt_resource_reserve(mut self, enabled: bool) -> Self {
        self.receipt_resource_reserve = enabled;
        self
    }

    pub(crate) const fn receipt_resource_reserve(self) -> bool {
        self.receipt_resource_reserve
    }

    /// Select the presentation depth for a context response.
    ///
    /// This option has no effect on non-context retrieval services.
    #[must_use]
    pub const fn with_context_response_profile(mut self, profile: ContextResponseProfile) -> Self {
        self.context_response_profile = Some(profile);
        self
    }

    /// Return the explicitly configured context response profile.
    #[must_use]
    pub const fn context_response_profile(self) -> Option<ContextResponseProfile> {
        self.context_response_profile
    }

    pub(crate) fn with_initial_reconciliation_deadline(
        mut self,
        deadline: tokio::time::Instant,
    ) -> Self {
        self.initial_reconciliation_deadline = Some(deadline);
        self
    }

    const fn initial_reconciliation_deadline(self) -> Option<tokio::time::Instant> {
        self.initial_reconciliation_deadline
    }
}

pub(super) trait RetrievalResponse: Serialize {
    fn meta_mut(&mut self) -> &mut ResponseMeta;
}

macro_rules! impl_retrieval_response {
    ($($response:ty),+ $(,)?) => {
        $(
            impl RetrievalResponse for $response {
                fn meta_mut(&mut self) -> &mut ResponseMeta {
                    &mut self.meta
                }
            }
        )+
    };
}

impl_retrieval_response!(
    FilesResponse,
    FilesPathsResponse,
    HistoryResponse,
    DiffSymbolsResponse,
    JsonResponse,
    SearchResponse,
    SearchCompactResponse,
    SearchGroupedResponse,
    SearchOccurrencesResponse,
    OutlineResponse,
    OutlineSignaturesResponse,
    ReadResponse,
    ReceiptRebaseResponse,
    ContextResponse,
);

impl Services {
    pub fn config(&self) -> &Config {
        &self.config
    }

    fn finalize_response<T: RetrievalResponse>(&self, response: &mut T) -> Result<()> {
        self.response_accountant.finalize(response)
    }

    fn finalized_response_tokens<T>(&self, response: &T) -> Result<usize>
    where
        T: RetrievalResponse + Clone,
    {
        self.response_accountant.finalized_tokens(response)
    }

    fn response_fits<T>(&self, response: &T, options: ServiceCallOptions) -> Result<bool>
    where
        T: RetrievalResponse + Clone,
    {
        self.response_accountant.fits(response, options)
    }

    fn response_fits_with_receipt_reserve<T>(
        &self,
        response: &T,
        returned_items: usize,
        options: ServiceCallOptions,
    ) -> Result<bool>
    where
        T: RetrievalResponse + Clone,
    {
        if options.receipt_resource_reserve() {
            self.response_accountant
                .fits_with_receipt_reserve(response, returned_items, options)
        } else {
            self.response_fits(response, options)
        }
    }

    fn finalized_response_tokens_with_receipt_reserve<T>(
        &self,
        response: &T,
        returned_items: usize,
        options: ServiceCallOptions,
    ) -> Result<usize>
    where
        T: RetrievalResponse + Clone,
    {
        if options.receipt_resource_reserve() {
            self.response_accountant
                .finalized_tokens_with_receipt_reserve(response, returned_items)
        } else {
            self.finalized_response_tokens(response)
        }
    }

    fn response_budget_exceeded(
        meta: &ResponseMeta,
        provided_max_response_tokens: usize,
        minimum_required_response_tokens: usize,
    ) -> Error {
        accounting::ResponseAccountant::budget_exceeded(
            meta,
            provided_max_response_tokens,
            minimum_required_response_tokens,
        )
    }

    fn response_budget_error<T>(
        &self,
        response: &T,
        provided_max_response_tokens: usize,
    ) -> Result<Error>
    where
        T: RetrievalResponse + Clone,
    {
        self.response_accountant
            .budget_error(response, provided_max_response_tokens)
    }

    fn response_budget_error_with_receipt_reserve<T>(
        &self,
        response: &T,
        returned_items: usize,
        provided_max_response_tokens: usize,
        options: ServiceCallOptions,
    ) -> Result<Error>
    where
        T: RetrievalResponse + Clone,
    {
        if options.receipt_resource_reserve() {
            self.response_accountant.budget_error_with_receipt_reserve(
                response,
                returned_items,
                provided_max_response_tokens,
            )
        } else {
            self.response_budget_error(response, provided_max_response_tokens)
        }
    }

    fn finalize_bounded_response<T>(
        &self,
        response: &mut T,
        options: ServiceCallOptions,
    ) -> Result<()>
    where
        T: RetrievalResponse + Clone,
    {
        if options.receipt_resource_reserve() {
            let reserved_tokens = self
                .response_accountant
                .finalized_tokens_with_receipt_resource(response)?;
            self.response_accountant.finalize(response)?;
            if let Some(limit) = options.max_response_tokens()
                && reserved_tokens > limit
            {
                let minimum = reserved_tokens;
                return Err(accounting::ResponseAccountant::budget_exceeded(
                    response.meta_mut(),
                    limit,
                    minimum,
                ));
            }
            Ok(())
        } else {
            self.response_accountant.finalize_bounded(response, options)
        }
    }

    fn validate_call_options(&self, options: ServiceCallOptions) -> Result<()> {
        match options.max_response_tokens() {
            Some(0) => Err(Error::InvalidInput {
                field: "max_response_tokens",
                reason: "must be greater than zero",
            }),
            Some(requested) if requested > MAX_OUTPUT_TOKENS => Err(Error::RequestLimitExceeded {
                field: "max_response_tokens",
                requested,
                limit: MAX_OUTPUT_TOKENS,
            }),
            _ => Ok(()),
        }
    }

    pub(super) fn consistent<T>(
        &self,
        operation: impl Fn(&index_read::IndexReadSnapshot) -> Result<T>,
    ) -> Result<T> {
        self.consistent_inner(false, operation)
    }

    fn consistent_allow_empty<T>(
        &self,
        operation: impl Fn(&index_read::IndexReadSnapshot) -> Result<T>,
    ) -> Result<T> {
        self.consistent_inner(true, operation)
    }

    /// Assemble a response against one WAL snapshot (DEFERRED read transaction).
    /// Concurrent writers cannot mix generations inside a single assembly. If
    /// opening the snapshot fails transiently or the index is still empty,
    /// returns a typed retryable error rather than a partial response.
    fn consistent_inner<T>(
        &self,
        allow_empty: bool,
        operation: impl Fn(&index_read::IndexReadSnapshot) -> Result<T>,
    ) -> Result<T> {
        for attempt in 0..3 {
            let snapshot = index_read::IndexReadSnapshot::open(&self.storage);
            let snapshot = match snapshot {
                Ok(snapshot) => snapshot,
                Err(error) if is_database_contention(&error) => {
                    if attempt + 1 < 3 {
                        thread::sleep(startup::CANCELLATION_POLL_INTERVAL);
                    }
                    continue;
                }
                Err(error) => return Err(error),
            };
            let generation = snapshot.generation();
            if generation == 0 && !allow_empty {
                return Err(Error::IndexNotReady);
            }
            // Do not retry operation errors: after the first read, this session
            // is pinned and concurrent publication cannot have caused them.
            return operation(&snapshot);
        }
        Err(Error::RetryableConflict(RetryableOperation::Retrieval))
    }

    pub(super) fn result_limit(&self, requested: Option<usize>) -> Result<usize> {
        validate_positive_request_limit(
            "max_results",
            requested.unwrap_or(self.config.default_results),
            self.config.max_results,
        )
    }

    pub(super) fn token_limit(&self, requested: Option<usize>, default: usize) -> Result<usize> {
        validate_positive_request_limit(
            "max_tokens",
            requested.unwrap_or(default),
            self.config.max_output_tokens,
        )
    }

    pub(super) fn token_budget_limit(&self, requested: usize) -> Result<usize> {
        validate_positive_request_limit("token_budget", requested, self.config.max_output_tokens)
    }

    pub(super) fn context_line_limit(&self, requested: Option<usize>) -> Result<usize> {
        validate_request_limit(
            "context_lines",
            requested.unwrap_or(self.config.context_lines),
            crate::config::MAX_CONTEXT_LINES,
        )
    }

    #[cfg(test)]
    pub(super) async fn apply_consistency(
        &self,
        consistency: IndexConsistency,
        cancellation: CancellationToken,
    ) -> Result<()> {
        self.apply_consistency_with_initial_deadline(consistency, cancellation, None)
            .await
    }

    async fn apply_consistency_with_initial_deadline(
        &self,
        consistency: IndexConsistency,
        cancellation: CancellationToken,
        deadline: Option<tokio::time::Instant>,
    ) -> Result<()> {
        if consistency == IndexConsistency::ReconcileWorkingTree {
            let deadline = if let Some(deadline) = deadline {
                let storage = self.storage.clone();
                let generation =
                    tokio::task::spawn_blocking(move || storage.repository_generation());
                tokio::select! {
                    biased;
                    _ = cancellation.cancelled() => return Err(Error::Cancelled),
                    result = generation => {
                        (result?? == 0).then_some(deadline)
                    }
                    _ = tokio::time::sleep_until(deadline) => return Err(Error::IndexNotReady),
                }
            } else {
                None
            };
            self.reconciliation
                .reconcile(cancellation, deadline)
                .await?;
        }
        Ok(())
    }

    pub(super) fn freshness(&self) -> Freshness {
        let local = self.active_reconciliations.load(Ordering::Acquire) > 0;
        let shared = self.coordination.is_reconciling().unwrap_or(true);
        if local || shared {
            Freshness::Reconciling
        } else {
            Freshness::Current
        }
    }

    pub(super) fn meta(
        &self,
        generation: u64,
        emitted_tokens: usize,
        next_cursor: Option<String>,
    ) -> ResponseMeta {
        ResponseMeta {
            repository_id: self.repository_id(),
            repository_generation: generation,
            freshness: self.freshness(),
            index_scope: if self.config.index_scope().is_full() {
                IndexScopeMode::Full
            } else {
                IndexScopeMode::Scoped
            },
            index_scope_digest: self.config.index_scope().digest().map(str::to_owned),
            source_tokens: emitted_tokens,
            protocol_tokens: 0,
            path_and_metadata_tokens: 0,
            total_response_tokens: 0,
            tokenizer: self.config.tokenizer.name().into(),
            token_count_exact: self.config.tokenizer.is_exact(),
            receipt_id: None,
            receipt_suppressed_exact: 0,
            receipt_suppressed_overlap: 0,
            receipt_near_duplicates: 0,
            next_cursor,
        }
    }

    /// Returns a stable opaque identity for the canonical repository root.
    pub fn repository_id(&self) -> String {
        let mut input = b"leantoken-repository-root-v1\0".to_vec();
        input.extend_from_slice(self.config.root.as_os_str().as_encoded_bytes());
        blake3::hash(&input).to_hex()[..32].to_string()
    }

    pub(crate) fn read_stored_receipt(
        &self,
        receipt_id: &str,
        now_unix_millis: i64,
    ) -> Result<crate::receipt::StoredReceipt> {
        self.storage.read_receipt(receipt_id, now_unix_millis)
    }

    /// Rejects retrieval bound to a different repository/worktree.
    pub fn validate_repository_id(&self, expected: Option<&str>) -> Result<()> {
        if let Some(expected) = expected {
            validation::validate_input(
                expected,
                "expected_repository_id",
                MAX_EXPECTED_REPOSITORY_ID_BYTES,
            )?;
        }
        let actual = self.repository_id();
        if expected.is_none_or(|expected| expected == actual) {
            return Ok(());
        }
        Err(Error::RepositoryIdentityMismatch {
            expected: expected.unwrap_or_default().to_owned(),
            actual,
        })
    }

    pub(super) fn observe_service_result<T>(
        &self,
        operation: TokenAccountingOperation,
        result: Result<T>,
    ) -> Result<T> {
        self.observer.observe(operation, result)
    }
}

fn is_database_contention(error: &Error) -> bool {
    matches!(
        sqlite_error_code(error),
        Some(rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked)
    )
}

fn sqlite_error_code(error: &Error) -> Option<rusqlite::ErrorCode> {
    let error = match error {
        Error::Sqlite(error) => error,
        Error::Migration(rusqlite_migration::Error::RusqliteError { err, .. }) => err,
        _ => return None,
    };
    match error {
        rusqlite::Error::SqliteFailure(inner, _) => Some(inner.code),
        _ => None,
    }
}

#[cfg(test)]
mod tests;
