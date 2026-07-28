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
    /// Internal operation failure retaining its historical CLI rendering.
    #[error("invalid request: {0}")]
    InternalFailure(String),
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
            Self::InvalidInput { .. } | Self::InvalidInputConstraints(_) => "invalid_input",
            Self::InvalidJson { .. } => "invalid_json",
            Self::InvalidJsonSelector { .. } => "invalid_json_selector",
            Self::InputTooLong { .. } => "input_too_long",
            Self::RequestLimitExceeded { .. } | Self::LimitExceeded => "request_limit_exceeded",
            Self::NotIndexed(_) => "not_indexed",
            Self::SymbolNotFound { .. } => "symbol_not_found",
            Self::HeadingNotFound { .. } => "heading_not_found",
            Self::IndexNotReady => "index_not_ready",
            Self::StaleCursor => "stale_cursor",
            Self::UnknownReceipt(_) => "unknown_receipt",
            Self::StaleReceipt { .. } => "stale_receipt",
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
            | Self::InvalidConfiguration(_) => "repository_configuration",
            Self::IndexLimitExceeded { .. } => "repository_index_limit",
            Self::RepositoryTraversal(_) => "repository_traversal",
            Self::RuntimeCapabilityUnavailable { .. } => "runtime_unavailable",
            Self::StaleReconciliation { .. } | Self::RetryableConflict(_) => "retryable_conflict",
            Self::RepositoryIdentityMismatch { .. }
            | Self::InternalFailure(_)
            | Self::RetrievalOverloaded
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
            Self::HeadingNotFound { .. } => "heading_not_found",
            Self::LimitExceeded => "limit_exceeded",
            Self::RequestLimitExceeded { .. } => "request_limit_exceeded",
            Self::UnsupportedLanguage(_) => "unsupported_language",
            Self::InvalidInput { .. } | Self::InvalidInputConstraints(_) => "invalid_input",
            Self::InvalidJson { .. } => "invalid_json",
            Self::InvalidJsonSelector { .. } => "invalid_json_selector",
            Self::InputTooLong { .. } => "input_too_long",
            Self::InvalidRequest(_) => "invalid_request",
            Self::InternalFailure(_) => "internal_failure",
            Self::DoctorFailure { .. } => "doctor_failure",
            Self::InvalidConfiguration(_) => "invalid_configuration",
            Self::RepositoryMismatch { .. } => "repository_mismatch",
            Self::RepositoryIdentityMismatch { .. } => "repository_identity_mismatch",
            Self::StaleCursor => "stale_cursor",
            Self::UnknownReceipt(_) => "unknown_receipt",
            Self::StaleReceipt { .. } => "stale_receipt",
            Self::Cancelled => "cancelled",
            Self::RetrievalOverloaded => "retrieval_overloaded",
            Self::RetrievalQueueTimeout => "retrieval_queue_timeout",
            Self::IndexNotReady => "index_not_ready",
            Self::StaleReconciliation { .. } => "stale_reconciliation",
            Self::ReconciliationFailed(_) => "reconciliation_failed",
            Self::RetryableConflict(_) => "retryable_conflict",
            Self::McpRuntimeStopped => "mcp_runtime_stopped",
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
