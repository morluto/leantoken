//! Token-budgeted repository retrieval for coding agents.
//!
//! [`services::Services`] is the main application API. The CLI and MCP
//! adapters use the same service methods and response models.

/// Command-line parsing and application requests.
pub mod cli;
/// Repository configuration and cache-path discovery.
pub mod config;
/// Cross-process ownership and reconciliation locks for one repository cache.
pub mod coordination;
/// Executable MCP readiness diagnostics.
pub mod doctor;
/// Error and result types shared across the crate.
pub mod error;
/// Repository discovery, parsing, and transactional reconciliation.
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
/// Global MCP client registration and removal.
pub mod setup;
/// SQLite schema, transactions, FTS5 queries, and indexed records.
pub mod storage;
/// UTF-8 preparation, chunking, hashing, and line-range helpers.
pub mod text;
/// Source-token counting and truncation with configurable exact or estimated tokenizers.
pub mod tokens;
/// Package-manager-aware CLI updates.
pub mod upgrade;
/// Debounced repository watching and reconciliation signals.
pub mod watcher;

pub use config::Config;
pub use error::{Error, Result};
pub use model::*;
