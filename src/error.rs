use std::path::PathBuf;

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
    #[error("stale cursor")]
    StaleCursor,
    #[error("request cancelled")]
    Cancelled,
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
}

pub type Result<T, E = Error> = std::result::Result<T, E>;
