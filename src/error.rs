use std::path::PathBuf;

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
    #[error("path is not indexed: {0}")]
    NotIndexed(String),
    #[error("requested content exceeds the configured limit")]
    LimitExceeded,
    #[error("unsupported structured language for {0}")]
    UnsupportedLanguage(String),
    #[error("invalid request: {0}")]
    InvalidRequest(String),
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
    #[error("stale cursor")]
    StaleCursor,
    #[error("request cancelled")]
    Cancelled,
    #[error("repository index is not ready")]
    IndexNotReady,
    #[error(
        "reconciliation plan is stale: expected generation {expected}, found generation {actual}"
    )]
    StaleReconciliation { expected: u64, actual: u64 },
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

pub type Result<T, E = Error> = std::result::Result<T, E>;
