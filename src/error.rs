use std::path::PathBuf;
use std::sync::Arc;

/// Repository indexing resource whose configured hard limit was exceeded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IndexLimitKind {
    /// Filesystem entries yielded by repository traversal.
    WalkEntries,
    /// Files admitted to the source index.
    Files,
    /// Aggregate bytes of files admitted to the source index.
    TotalSourceBytes,
    /// Repository-relative traversal depth below the root.
    Depth,
}

impl std::fmt::Display for IndexLimitKind {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::WalkEntries => formatter.write_str("walk entries"),
            Self::Files => formatter.write_str("source files"),
            Self::TotalSourceBytes => formatter.write_str("total source bytes"),
            Self::Depth => formatter.write_str("repository depth"),
        }
    }
}

/// Retrieval work whose configured hard limit was exceeded.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetrievalLimitKind {
    /// Indexed files considered by the regex full-scan fallback.
    RegexFullScanFiles,
    /// Chunks in one included file considered by the regex full-scan fallback.
    RegexChunksPerFile,
    /// Trigram candidate chunks admitted before regex verification.
    RegexCandidateChunks,
    /// Trigram rows inspected while applying path scope.
    RegexScopedRows,
    /// Regex-matching chunks retained for occurrence hydration.
    RegexRetainedChunks,
    /// Structural rows inspected by the Unicode case-fold full-scan fallback.
    UnicodeCaseFoldRows,
    /// Individual occurrences materialized for an exhaustive search.
    ExhaustiveOccurrences,
}

/// Aggregate regex resource that exhausted the server-owned work budget.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RegexWorkDimension {
    CandidateFiles,
    CandidateChunks,
    CandidateBytes,
}

impl RegexWorkDimension {
    /// Stable privacy-safe reason code used by CLI and MCP adapters.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::CandidateFiles => "candidate_files",
            Self::CandidateChunks => "candidate_chunks",
            Self::CandidateBytes => "candidate_bytes",
        }
    }

    /// Actionable bounded-search guidance for CLI and MCP callers.
    #[must_use]
    pub const fn guidance(self) -> &'static str {
        match self {
            Self::CandidateFiles => "narrow include_paths or index a smaller repository scope",
            Self::CandidateChunks | Self::CandidateBytes => {
                "add a mandatory case-sensitive literal or narrow include_paths"
            }
        }
    }
}

impl std::fmt::Display for RegexWorkDimension {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl RetrievalLimitKind {
    /// Stable privacy-safe reason code used by CLI and MCP adapters.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::RegexFullScanFiles => "regex_full_scan_files",
            Self::RegexChunksPerFile => "regex_chunks_per_file",
            Self::RegexCandidateChunks => "regex_candidate_chunks",
            Self::RegexScopedRows => "regex_scoped_rows",
            Self::RegexRetainedChunks => "regex_retained_chunks",
            Self::UnicodeCaseFoldRows => "unicode_case_fold_rows",
            Self::ExhaustiveOccurrences => "exhaustive_occurrences",
        }
    }

    /// Static remediation guidance that does not disclose repository content.
    #[must_use]
    pub const fn guidance(self) -> &'static str {
        match self {
            Self::RegexFullScanFiles => {
                "add a mandatory case-sensitive literal or use a smaller index scope"
            }
            Self::RegexChunksPerFile => {
                "exclude or narrow paths that include unusually large files"
            }
            Self::RegexCandidateChunks => "make the regex more selective",
            Self::RegexScopedRows => "narrow the path scope or make the regex more selective",
            Self::UnicodeCaseFoldRows => {
                "use case-sensitive search or index a smaller repository scope"
            }
            Self::RegexRetainedChunks | Self::ExhaustiveOccurrences => {
                "narrow the expression or requested scope"
            }
        }
    }
}

impl std::fmt::Display for RetrievalLimitKind {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Repository operation that may be retried after concurrent state changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetryableOperation {
    /// Preparing and publishing an index generation.
    Reconciliation,
    /// Reading one consistent committed generation.
    Retrieval,
}

impl std::fmt::Display for RetryableOperation {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Reconciliation => formatter.write_str("reconciliation"),
            Self::Retrieval => formatter.write_str("retrieval"),
        }
    }
}

/// One statically known request-field dependency or incompatibility.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct InputViolation {
    /// Safe public field label.
    pub field: &'static str,
    /// Safe public validation rule.
    pub reason: &'static str,
}

impl InputViolation {
    pub(crate) const fn new(field: &'static str, reason: &'static str) -> Self {
        Self { field, reason }
    }
}

/// Deterministically ordered static request conflicts returned together.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(transparent)]
pub struct InputViolations(Vec<InputViolation>);

impl InputViolations {
    /// Preserve the supplied validation order for display and structured errors.
    #[must_use]
    pub fn new(violations: Vec<InputViolation>) -> Self {
        Self(violations)
    }

    /// Return the individual field conflicts in deterministic validation order.
    #[must_use]
    pub fn as_slice(&self) -> &[InputViolation] {
        &self.0
    }
}

impl std::fmt::Display for InputViolations {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for (index, violation) in self.0.iter().enumerate() {
            if index > 0 {
                formatter.write_str("; ")?;
            }
            write!(formatter, "{}: {}", violation.field, violation.reason)?;
        }
        Ok(())
    }
}

/// Exact aggregate accounting for the smallest retryable service response.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct ResponseBudgetBreakdown {
    /// Tokens required by the mandatory response DTO before receipt reservation.
    pub mandatory_response_tokens: usize,
    /// Source-content tokens within the mandatory response.
    pub source_tokens: usize,
    /// Protocol-envelope tokens within the mandatory response.
    pub protocol_tokens: usize,
    /// Path, metadata, and repeated-structure tokens within the mandatory response.
    pub path_and_metadata_tokens: usize,
    /// Additional tokens reserved for worst-case receipt metadata.
    pub receipt_reserve_tokens: usize,
}

/// Errors returned by LeanToken operations.
///
/// Callers should match the variants they can recover from and retain a
/// fallback arm for errors added by later releases.
#[non_exhaustive]
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("repository root does not exist: {0}")]
    RootNotFound(PathBuf),
    /// Automatic indexing refused a filesystem root, home directory, or parent
    /// of the current user's home directory.
    #[error(
        "repository root is too broad for automatic indexing: {0}; pass --allow-broad-root to override"
    )]
    UnsafeRepositoryRoot(PathBuf),
    /// Repository discovery stopped instead of returning a truncated index.
    #[error("index {kind} limit exceeded: observed {observed}, limit {limit}")]
    IndexLimitExceeded {
        /// Resource whose configured bound was crossed.
        kind: IndexLimitKind,
        /// First observed value outside the configured bound.
        observed: u64,
        /// Configured inclusive maximum.
        limit: u64,
    },
    #[error("path escapes repository root: {0}")]
    PathOutsideRoot(PathBuf),
    /// Filesystem path cannot be represented by the UTF-8 retrieval API.
    #[error("repository path is not valid UTF-8: {0}")]
    UnsupportedPathEncoding(PathBuf),
    /// Ignore-aware traversal failed before complete membership was known.
    #[error("repository traversal failed: {0}")]
    RepositoryTraversal(#[from] ignore::Error),
    #[error("path is not indexed: {0}")]
    NotIndexed(String),
    /// Requested symbol was absent from an indexed file.
    #[error("symbol is not indexed in {path}: {symbol}")]
    SymbolNotFound {
        /// Repository-relative indexed file.
        path: String,
        /// Exact symbol identity requested by the caller.
        symbol: String,
    },
    /// A bare or qualified symbol identity matched multiple definitions.
    #[error("symbol is ambiguous in {path}: {symbol}")]
    AmbiguousSymbol {
        /// Repository-relative indexed or historical file.
        path: String,
        /// Exact symbol identity requested by the caller.
        symbol: String,
    },
    /// Requested document heading occurrence was absent from an indexed file.
    #[error("document heading occurrence {occurrence} is not indexed in {path}: {heading}")]
    HeadingNotFound {
        /// Repository-relative indexed file.
        path: String,
        /// Exact rendered heading text requested by the caller.
        heading: String,
        /// One-based duplicate occurrence requested by the caller.
        occurrence: usize,
    },
    #[error("requested content exceeds the configured limit")]
    LimitExceeded,
    /// Retrieval crossed a named hard work bound.
    #[error("retrieval {kind} limit exceeded: observed {observed}, limit {limit}")]
    RetrievalLimitExceeded {
        /// Stable privacy-safe retrieval owner.
        kind: RetrievalLimitKind,
        /// First observed value outside the configured bound.
        observed: usize,
        /// Configured inclusive maximum.
        limit: usize,
    },
    /// Regex candidate verification stopped before complete coverage.
    #[error(
        "regex work budget exhausted on {dimension}: files={candidate_files}, chunks={candidate_chunks}, bytes={candidate_bytes}, limit={limit}"
    )]
    RegexWorkBudgetExceeded {
        /// Resource that crossed its calibrated request budget.
        dimension: RegexWorkDimension,
        /// Candidate files admitted before exhaustion.
        candidate_files: usize,
        /// Candidate chunks admitted before exhaustion.
        candidate_chunks: usize,
        /// Candidate content bytes admitted before exhaustion.
        candidate_bytes: usize,
        /// Inclusive budget for the limiting dimension.
        limit: usize,
    },
    /// Caller-controlled response limit crossed its configured maximum.
    #[error("{field} exceeds its configured limit: requested {requested}, limit {limit}")]
    RequestLimitExceeded {
        /// Safe public request field name.
        field: &'static str,
        /// Caller-provided value.
        requested: usize,
        /// Configured inclusive maximum.
        limit: usize,
    },
    /// A caller-provided response ceiling cannot hold the smallest valid DTO.
    #[error(
        "max_response_tokens is too small: provided {provided_max_response_tokens}, minimum required {minimum_required_response_tokens}; retry with at least {retry_with_at_least}"
    )]
    ResponseBudgetExceeded {
        /// Caller-provided response-token ceiling.
        provided_max_response_tokens: usize,
        /// Exact token count of the smallest valid response, including reserves.
        minimum_required_response_tokens: usize,
        /// Exact inclusive ceiling that the caller can retry with.
        retry_with_at_least: usize,
        /// Bounded aggregate accounting for the retryable minimum.
        breakdown: ResponseBudgetBreakdown,
    },
    #[error("unsupported structured language for {0}")]
    UnsupportedLanguage(String),
    /// Request input failed a validation rule described entirely by static text.
    #[error("invalid {field}: {reason}")]
    InvalidInput {
        /// Safe public field label.
        field: &'static str,
        /// Safe public validation rule.
        reason: &'static str,
    },
    /// Search options requested mutually incompatible ranked and exhaustive semantics.
    #[error(
        "invalid {field}: allowed modes are {allowed_modes:?}; conflicting options: {conflicting_options:?}"
    )]
    InvalidSearchOptions {
        /// Rejected request field.
        field: &'static str,
        /// Wire mode names that support the requested behavior.
        allowed_modes: &'static [&'static str],
        /// Bounded, caller-supplied options that made the request contradictory.
        conflicting_options: Vec<String>,
        /// Valid ranked structural request using the same conceptual query.
        ranked_symbol_example: &'static str,
        /// Valid exhaustive lexical request using the same conceptual query.
        exhaustive_text_example: &'static str,
    },
    /// Multiple static request-field dependencies or incompatibilities failed.
    #[error("invalid input constraints: {0}")]
    InvalidInputConstraints(InputViolations),
    /// A JSON document failed strict parsing at a bounded source coordinate.
    #[error(
        "file is not valid JSON ({syntax_category} at byte {byte_offset}, line {line}, column {column}): {reason}"
    )]
    InvalidJson {
        /// Stable serde_json error category.
        syntax_category: &'static str,
        /// Zero-based byte offset in the UTF-8 document.
        byte_offset: usize,
        /// One-based source line.
        line: usize,
        /// One-based source column.
        column: usize,
        /// Bounded parser diagnostic without file contents.
        reason: String,
    },
    /// A JMESPath expression failed compilation or typed evaluation.
    #[error("JMESPath {stage} failed at offset {offset}, line {line}, column {column}: {reason}")]
    InvalidJsonSelector {
        /// Stable failure boundary: `compile` or `evaluate`.
        stage: &'static str,
        /// Zero-based character offset in the caller-provided expression.
        offset: usize,
        /// One-based expression line.
        line: usize,
        /// One-based expression column.
        column: usize,
        /// Parser or runtime type diagnostic.
        reason: String,
    },
    /// Request input crossed its configured byte bound.
    #[error("{field} exceeds {max_bytes} bytes")]
    InputTooLong {
        /// Safe public field label.
        field: &'static str,
        /// Inclusive byte bound.
        max_bytes: usize,
    },
    #[error("invalid request: {0}")]
    InvalidRequest(String),
    /// A serialization boundary failed after request validation.
    #[error("serialization failed: {0}")]
    SerializationFailure(String),
    /// Final response accounting could not reach a valid fixed point.
    #[error("response accounting invariant failed: {0}")]
    ResponseAccountingInvariant(String),
    /// A best-effort cache maintenance operation failed.
    #[error("cache pruning failed: {0}")]
    CachePruneFailure(String),
    /// Setup or installation state failed an ownership or recovery invariant.
    #[error("setup failed: {0}")]
    SetupFailure(String),
    /// A product operation reached an impossible internal state.
    #[error("operation failed: {0}")]
    OperationFailure(String),
    /// A doctor probe failed at a named integration boundary.
    #[error("doctor {stage} check failed: {message}")]
    DoctorFailure {
        /// Stable integration stage suitable for structured diagnostics.
        stage: &'static str,
        /// Human-readable failure detail.
        message: String,
    },
    #[error("invalid repository configuration: {0}")]
    InvalidConfiguration(String),
    #[error(
        "SQLite index {database} belongs to repository {expected_repository}, not {actual_repository}"
    )]
    RepositoryMismatch {
        database: PathBuf,
        expected_repository: String,
        actual_repository: PathBuf,
    },
    /// An explicit SQLite path is already bound to another normalized index scope.
    #[error("SQLite index {database} belongs to a different indexing scope")]
    IndexScopeMismatch {
        /// SQLite path whose immutable repository membership boundary differs.
        database: PathBuf,
    },
    /// Retrieval request was intended for a different bound repository.
    #[error("repository identity mismatch: expected {expected}, actual {actual}")]
    RepositoryIdentityMismatch {
        /// Opaque identity supplied by the caller.
        expected: String,
        /// Opaque identity of this server's canonical root.
        actual: String,
    },
    #[error("stale cursor")]
    StaleCursor,
    /// Retrieval receipt is unknown or has been evicted from the bounded session registry.
    #[error("unknown retrieval receipt: {0}")]
    UnknownReceipt(String),
    /// Retrieval receipt belongs to an earlier committed repository generation.
    #[error(
        "stale retrieval receipt: receipt generation {receipt_generation}, repository generation {repository_generation}"
    )]
    StaleReceipt {
        /// Repository generation recorded when the receipt was created.
        receipt_generation: u64,
        /// Repository generation serving the current retrieval.
        repository_generation: u64,
    },
    /// Exhaustive-query coverage receipt is unknown, expired, or has been evicted.
    #[error("unknown query coverage receipt: {0}")]
    UnknownQueryReceipt(String),
    /// Query receipt cannot prove the normalized predicate requested by the caller.
    #[error("query coverage receipt does not cover the requested predicate")]
    QueryReceiptMismatch,
    /// Relevant indexed partitions changed after the query receipt was recorded.
    #[error(
        "stale query coverage receipt: receipt generation {receipt_generation}, repository generation {repository_generation}"
    )]
    StaleQueryReceipt {
        /// Repository generation recorded when the receipt was created.
        receipt_generation: u64,
        /// Repository generation serving the current retrieval.
        repository_generation: u64,
    },
    #[error("request cancelled")]
    Cancelled,
    /// Process-local retrieval admission is full; no blocking work was queued.
    #[error("retrieval capacity is exhausted; retry")]
    RetrievalOverloaded,
    /// Retrieval did not obtain blocking execution capacity before its queue deadline.
    #[error("retrieval waited too long for execution capacity; retry")]
    RetrievalQueueTimeout,
    #[error("repository index is not ready")]
    IndexNotReady,
    #[error(
        "reconciliation plan is stale: expected generation {expected}, found generation {actual}"
    )]
    StaleReconciliation { expected: u64, actual: u64 },
    /// A shared request-triggered reconciliation wave failed.
    ///
    /// The shared source keeps the original typed error available to every
    /// coalesced caller without rerunning a failed scan for each waiter.
    #[error("reconciliation failed: {0}")]
    ReconciliationFailed(#[source] Arc<Error>),
    #[error("repository {0} could not stabilize because the index changed repeatedly; retry")]
    RetryableConflict(RetryableOperation),
    #[error("MCP indexing runtime stopped unexpectedly")]
    McpRuntimeStopped,
    /// A production-owned background component did not stop before its deadline.
    #[error("shutdown timed out while stopping {component}")]
    ShutdownTimeout { component: &'static str },
    #[error("required runtime capability is unavailable: {capability}")]
    RuntimeCapabilityUnavailable {
        capability: &'static str,
        #[source]
        source: Option<rusqlite::Error>,
    },
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("SQLite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("database migration error: {0}")]
    Migration(#[from] rusqlite_migration::Error),
    #[error("SQLite connection pool error: {0}")]
    ConnectionPool(#[from] r2d2::Error),
    #[error("tree-sitter language error: {0}")]
    TreeSitterLanguage(#[from] tree_sitter::LanguageError),
    #[error("tree-sitter query error: {0}")]
    TreeSitterQuery(#[from] tree_sitter::QueryError),
    #[error("regex error: {0}")]
    Regex(#[from] regex::Error),
    #[error("glob error: {0}")]
    Glob(#[from] globset::Error),
    #[error("serialization error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("background task failed: {0}")]
    Join(#[from] tokio::task::JoinError),
    #[error("index worker pool failed: {0}")]
    ThreadPoolBuild(#[from] rayon::ThreadPoolBuildError),
}

impl Error {
    /// Stable, non-sensitive category exposed by public adapters.
    ///
    /// CLI and MCP envelopes may attach different transport metadata, but this
    /// allowlisted classification remains adapter-neutral.
    #[must_use]
    pub fn public_category(&self) -> &'static str {
        match self.reconciliation_cause() {
            Self::DoctorFailure { .. } => "doctor_failure",
            Self::InvalidInput { .. }
            | Self::InvalidSearchOptions { .. }
            | Self::InvalidInputConstraints(_) => "invalid_input",
            Self::InvalidJson { .. } => "invalid_json",
            Self::InvalidJsonSelector { .. } => "invalid_json_selector",
            Self::InputTooLong { .. } => "input_too_long",
            Self::RegexWorkBudgetExceeded { .. } => "incomplete_work",
            Self::RequestLimitExceeded { .. }
            | Self::ResponseBudgetExceeded { .. }
            | Self::RetrievalLimitExceeded { .. }
            | Self::LimitExceeded => "request_limit_exceeded",
            Self::NotIndexed(_) => "not_indexed",
            Self::SymbolNotFound { .. } => "symbol_not_found",
            Self::AmbiguousSymbol { .. } => "symbol_ambiguous",
            Self::HeadingNotFound { .. } => "heading_not_found",
            Self::IndexNotReady => "index_not_ready",
            Self::StaleCursor => "stale_cursor",
            Self::UnknownReceipt(_) => "unknown_receipt",
            Self::StaleReceipt { .. } => "stale_receipt",
            Self::UnknownQueryReceipt(_) => "unknown_query_receipt",
            Self::QueryReceiptMismatch => "query_receipt_mismatch",
            Self::StaleQueryReceipt { .. } => "stale_query_receipt",
            Self::RepositoryIdentityMismatch { .. } => "repository_identity_mismatch",
            Self::Cancelled => "request_cancelled",
            Self::PathOutsideRoot(_) => "path_outside_root",
            Self::UnsupportedPathEncoding(_) => "unsupported_path_encoding",
            Self::UnsupportedLanguage(_) => "unsupported_language",
            Self::InvalidRequest(_) => "invalid_request",
            Self::Regex(_) => "invalid_regex",
            Self::Glob(_) => "invalid_glob",
            Self::RootNotFound(_)
            | Self::UnsafeRepositoryRoot(_)
            | Self::RepositoryMismatch { .. }
            | Self::IndexScopeMismatch { .. }
            | Self::InvalidConfiguration(_) => "repository_configuration",
            Self::IndexLimitExceeded { .. } => "repository_index_limit",
            Self::RepositoryTraversal(_) => "repository_traversal",
            Self::RuntimeCapabilityUnavailable { .. } => "runtime_unavailable",
            Self::StaleReconciliation { .. } | Self::RetryableConflict(_) => "retryable_conflict",
            Self::SerializationFailure(_) => "serialization_failure",
            Self::ResponseAccountingInvariant(_) => "response_accounting_invariant",
            Self::CachePruneFailure(_) => "cache_prune_failure",
            Self::SetupFailure(_) => "setup_failure",
            Self::OperationFailure(_) => "operation_failure",
            Self::ShutdownTimeout { .. } => "shutdown_timeout",
            Self::RetrievalOverloaded
            | Self::RetrievalQueueTimeout
            | Self::ReconciliationFailed(_)
            | Self::McpRuntimeStopped
            | Self::Io(_)
            | Self::Sqlite(_)
            | Self::Migration(_)
            | Self::ConnectionPool(_)
            | Self::TreeSitterLanguage(_)
            | Self::TreeSitterQuery(_)
            | Self::Json(_)
            | Self::Join(_)
            | Self::ThreadPoolBuild(_) => "internal_error",
        }
    }

    /// Stable, non-sensitive category for repository-local service observations.
    pub(crate) fn observation_category(&self) -> &'static str {
        match self.reconciliation_cause() {
            Self::RootNotFound(_) => "root_not_found",
            Self::UnsafeRepositoryRoot(_) => "unsafe_repository_root",
            Self::IndexLimitExceeded { .. } => "index_limit_exceeded",
            Self::PathOutsideRoot(_) => "path_outside_root",
            Self::UnsupportedPathEncoding(_) => "unsupported_path_encoding",
            Self::RepositoryTraversal(_) => "repository_traversal",
            Self::NotIndexed(_) => "not_indexed",
            Self::SymbolNotFound { .. } => "symbol_not_found",
            Self::AmbiguousSymbol { .. } => "symbol_ambiguous",
            Self::HeadingNotFound { .. } => "heading_not_found",
            Self::LimitExceeded => "limit_exceeded",
            Self::RetrievalLimitExceeded { .. } => "request_limit_exceeded",
            Self::RegexWorkBudgetExceeded { .. } => "incomplete_work",
            Self::RequestLimitExceeded { .. } => "request_limit_exceeded",
            Self::ResponseBudgetExceeded { .. } => "request_limit_exceeded",
            Self::UnsupportedLanguage(_) => "unsupported_language",
            Self::InvalidInput { .. }
            | Self::InvalidSearchOptions { .. }
            | Self::InvalidInputConstraints(_) => "invalid_input",
            Self::InvalidJson { .. } => "invalid_json",
            Self::InvalidJsonSelector { .. } => "invalid_json_selector",
            Self::InputTooLong { .. } => "input_too_long",
            Self::InvalidRequest(_) => "invalid_request",
            Self::SerializationFailure(_) => "serialization_failure",
            Self::ResponseAccountingInvariant(_) => "response_accounting_invariant",
            Self::CachePruneFailure(_) => "cache_prune_failure",
            Self::SetupFailure(_) => "setup_failure",
            Self::OperationFailure(_) => "operation_failure",
            Self::DoctorFailure { .. } => "doctor_failure",
            Self::InvalidConfiguration(_) => "invalid_configuration",
            Self::RepositoryMismatch { .. } => "repository_mismatch",
            Self::IndexScopeMismatch { .. } => "index_scope_mismatch",
            Self::RepositoryIdentityMismatch { .. } => "repository_identity_mismatch",
            Self::StaleCursor => "stale_cursor",
            Self::UnknownReceipt(_) => "unknown_receipt",
            Self::StaleReceipt { .. } => "stale_receipt",
            Self::UnknownQueryReceipt(_) => "unknown_query_receipt",
            Self::QueryReceiptMismatch => "query_receipt_mismatch",
            Self::StaleQueryReceipt { .. } => "stale_query_receipt",
            Self::Cancelled => "cancelled",
            Self::RetrievalOverloaded => "retrieval_overloaded",
            Self::RetrievalQueueTimeout => "retrieval_queue_timeout",
            Self::IndexNotReady => "index_not_ready",
            Self::StaleReconciliation { .. } => "stale_reconciliation",
            Self::ReconciliationFailed(_) => "reconciliation_failed",
            Self::RetryableConflict(_) => "retryable_conflict",
            Self::McpRuntimeStopped => "mcp_runtime_stopped",
            Self::ShutdownTimeout { .. } => "shutdown_timeout",
            Self::RuntimeCapabilityUnavailable { .. } => "runtime_capability_unavailable",
            Self::Io(_) => "io",
            Self::Sqlite(_) => "sqlite",
            Self::Migration(_) => "migration",
            Self::ConnectionPool(_) => "connection_pool",
            Self::TreeSitterLanguage(_) => "tree_sitter_language",
            Self::TreeSitterQuery(_) => "tree_sitter_query",
            Self::Regex(_) => "regex",
            Self::Glob(_) => "glob",
            Self::Json(_) => "serialization",
            Self::Join(_) => "join",
            Self::ThreadPoolBuild(_) => "thread_pool_build",
        }
    }

    /// Return the original typed failure for a shared reconciliation wave.
    ///
    /// Adapters can use this to preserve their existing retry and error
    /// classification while [`Error::ReconciliationFailed`] retains shared
    /// ownership for coalesced callers.
    #[must_use]
    pub fn reconciliation_cause(&self) -> &Self {
        let mut error = self;
        while let Self::ReconciliationFailed(source) = error {
            error = source;
        }
        error
    }
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

#[cfg(test)]
mod tests {
    use super::Error;

    #[test]
    fn shutdown_timeout_has_a_public_shutdown_category() {
        assert_eq!(
            Error::ShutdownTimeout {
                component: "repository watcher"
            }
            .public_category(),
            "shutdown_timeout"
        );
    }

    #[test]
    fn repository_identity_mismatch_has_a_public_client_category() {
        assert_eq!(
            Error::RepositoryIdentityMismatch {
                expected: "expected".into(),
                actual: "actual".into(),
            }
            .public_category(),
            "repository_identity_mismatch"
        );
    }
}
