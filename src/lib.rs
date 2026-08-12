//! Token-budgeted repository retrieval for coding agents.
//!
//! [`services::Services`] is the main application API. The CLI and MCP
//! adapters use the same service methods and response models.

/// Command-line parsing and application requests.
pub mod cli;
mod concurrency;
/// Repository configuration and cache-path discovery.
pub mod config;
/// Cross-process ownership locks for one repository cache.
pub mod coordination;
/// Error and result types shared across the crate.
pub mod error;
/// Repository discovery, parsing, and transactional publication.
pub mod indexer;
/// MCP server adapter built on the official Rust SDK.
pub mod mcp;
/// Request and response models shared by CLI, MCP, and services.
pub mod model;
/// Tree-sitter language detection and syntax extraction.
pub mod parser;
/// Deterministic evidence ranking, deduplication, and selection.
pub mod ranking;
/// Ignore-aware file discovery and repository path containment.
pub mod repository;
/// Token-bounded repository retrieval services.
pub mod services;
/// SQLite schema, transactions, FTS5 queries, and indexed records.
pub mod storage;
mod symbol_identity;
/// UTF-8 preparation, chunking, hashing, and line-range helpers.
pub mod text;
/// Source-token counting and truncation with configurable exact or estimated tokenizers.
pub mod tokens;

pub use config::{Config, DiscoveryLimits};
pub use error::{
    Error, IndexLimitKind, InputViolation, InputViolations, RegexWorkDimension,
    ResponseBudgetBreakdown, Result, RetrievalLimitKind,
};
pub use model::*;
pub use repository::GitDiffResult;
pub use repository::{DiscoveryPolicy, IndexScope};
