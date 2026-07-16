use std::path::PathBuf;

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
