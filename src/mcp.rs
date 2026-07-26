use std::sync::{
    Arc, RwLock,
    atomic::{AtomicBool, Ordering},
};

use rmcp::{
    ErrorData, RoleServer, ServerHandler, ServiceExt,
    handler::server::wrapper::Parameters,
    model::{CallToolResult, ContentBlock},
    service::{NotificationContext, RequestContext},
    tool, tool_handler, tool_router,
    transport::stdio,
};
use schemars::{JsonSchema, Schema, SchemaGenerator};
use serde::{Deserialize, Deserializer, Serialize};
use tokio_util::sync::CancellationToken;

use crate::Config;
use crate::config::{
    DEFAULT_CONTEXT_FRAGMENTS, DEFAULT_CONTEXT_LINES, DEFAULT_CONTEXT_TOKENS, DEFAULT_READ_TOKENS,
    DEFAULT_RESULTS, MAX_CONTEXT_LINES, MAX_OUTPUT_TOKENS, MAX_RESULTS,
};
use crate::model::{
    ContextRequest, ContextWorkflow, FileOperation, FilesRequest, HandoffManifestRequest,
    HistoryOperation, HistoryRequest, IndexConsistency, JsonOperation, JsonProjection, JsonRequest,
    JsonSelector, OutlineRequest, ReadRequest, SearchMode, SearchRequest,
};
use crate::services::{Services, validate_positive_request_limit, validate_request_limit};

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct SavingsMcpRequest {}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(transform = add_files_operation_constraints)]
struct FilesMcpRequest {
    /// Expected opaque repository identity from an earlier response.
    #[serde(default)]
    #[schemars(schema_with = "expected_repository_id_schema")]
    expected_repository_id: Option<String>,
    /// Path operation to perform.
    #[schemars(schema_with = "file_operation_schema")]
    operation: FilesMcpOperationInput,
    /// Optional repository-relative directory for `tree`.
    #[serde(default)]
    #[schemars(length(max = 4096))]
    path: Option<String>,
    /// Non-empty fuzzy filename or path query for `find`.
    #[serde(default)]
    #[schemars(length(min = 1, max = 65536))]
    query: Option<String>,
    /// Non-empty glob pattern for `glob`.
    #[serde(default)]
    #[schemars(length(min = 1, max = 4096))]
    pattern: Option<String>,
    /// Maximum entries to return (default 20, maximum 100).
    #[serde(default, deserialize_with = "deserialize_optional_limit")]
    #[schemars(schema_with = "result_limit_schema", default = "default_result_option")]
    max_results: Option<usize>,
    /// Cursor returned by the same operation and repository generation.
    #[serde(default)]
    #[schemars(length(max = 4096))]
    cursor: Option<String>,
    /// Use `reconcile_working_tree` after edits; otherwise `indexed_generation`.
    #[serde(default)]
    #[schemars(schema_with = "index_consistency_schema")]
    consistency: IndexConsistency,
    /// Maximum hierarchy depth below `path` for `tree`.
    #[serde(default)]
    depth: Option<usize>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum FilesMcpOperationInput {
    Flat(FileOperation),
    Nested(LegacyFilesMcpOperation),
}

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum LegacyFilesMcpOperation {
    Tree {
        #[serde(default)]
        path: Option<String>,
        #[serde(default)]
        depth: Option<usize>,
    },
    Find {
        query: String,
    },
    Glob {
        pattern: String,
    },
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct SearchMcpRequest {
    /// Expected opaque repository identity from an earlier response.
    #[serde(default)]
    #[schemars(schema_with = "expected_repository_id_schema")]
    expected_repository_id: Option<String>,
    /// Non-empty text, identifier, symbol, or Rust regular expression to find.
    #[schemars(length(min = 1, max = 65536))]
    query: String,
    /// Candidate source to search (default `auto`).
    #[serde(default)]
    mode: SearchMode,
    /// Include only matching repository paths.
    #[serde(default)]
    #[schemars(length(max = 256), inner(length(max = 4096)))]
    include_paths: Vec<String>,
    /// Exclude matching repository paths.
    #[serde(default)]
    #[schemars(length(max = 256), inner(length(max = 4096)))]
    exclude_paths: Vec<String>,
    /// Boost matching paths without filtering other results.
    #[serde(default)]
    #[schemars(length(max = 256), inner(length(max = 4096)))]
    focus_paths: Vec<String>,
    /// Maximum hits to return (default 20, maximum 100).
    #[serde(default, deserialize_with = "deserialize_optional_limit")]
    #[schemars(schema_with = "result_limit_schema", default = "default_result_option")]
    max_results: Option<usize>,
    /// Maximum source tokens across excerpts (default 8000, maximum 32000).
    #[serde(default, deserialize_with = "deserialize_optional_limit")]
    #[schemars(schema_with = "token_limit_schema", default = "default_token_option")]
    max_tokens: Option<usize>,
    /// Lines before and after each match (default 2, maximum 20).
    #[serde(default, deserialize_with = "deserialize_optional_limit")]
    #[schemars(
        schema_with = "context_line_limit_schema",
        default = "default_context_line_option"
    )]
    context_lines: Option<usize>,
    /// Preserve query case when matching.
    #[serde(default)]
    case_sensitive: bool,
    /// Return every text or regex occurrence with exact coordinates and counts.
    #[serde(default)]
    all_occurrences: bool,
    /// Prefer structural definitions when identifier channels find the same definition.
    #[serde(default)]
    prefer_structural: bool,
    /// Suppress evidence already returned under this server-managed receipt.
    #[serde(default)]
    #[schemars(length(max = 128))]
    receipt_id: Option<String>,
    /// Cursor returned by the same search and repository generation.
    #[serde(default)]
    #[schemars(length(max = 4096))]
    cursor: Option<String>,
    /// Use `reconcile_working_tree` after edits; otherwise `indexed_generation`.
    #[serde(default)]
    #[schemars(schema_with = "index_consistency_schema")]
    consistency: IndexConsistency,
}

impl SearchMcpRequest {
    fn validate_limits(&self, limits: McpLimitPolicy) -> crate::Result<()> {
        validate_optional_positive_limit("max_results", self.max_results, limits.max_results)?;
        validate_optional_positive_limit("max_tokens", self.max_tokens, limits.max_output_tokens)?;
        validate_optional_limit(
            "context_lines",
            self.context_lines,
            limits.max_context_lines,
        )
    }

    fn into_parts(self) -> (SearchRequest, IndexConsistency, Option<String>) {
        (
            SearchRequest {
                query: self.query,
                mode: self.mode,
                include_paths: self.include_paths,
                exclude_paths: self.exclude_paths,
                focus_paths: self.focus_paths,
                max_results: self.max_results,
                max_tokens: self.max_tokens,
                context_lines: self.context_lines,
                case_sensitive: self.case_sensitive,
                all_occurrences: self.all_occurrences,
                prefer_structural: self.prefer_structural,
                receipt_id: self.receipt_id,
                cursor: self.cursor,
            },
            self.consistency,
            self.expected_repository_id,
        )
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct OutlineMcpRequest {
    /// Expected opaque repository identity from an earlier response.
    #[serde(default)]
    #[schemars(schema_with = "expected_repository_id_schema")]
    expected_repository_id: Option<String>,
    /// One to 256 repository-relative source files to outline.
    #[schemars(length(min = 1, max = 256), inner(length(max = 4096)))]
    paths: Vec<String>,
    /// Keep definitions whose names contain this value.
    #[serde(default)]
    #[schemars(length(max = 4096))]
    symbol_name: Option<String>,
    /// Keep definitions of this exact syntax kind.
    #[serde(default)]
    #[schemars(length(max = 4096))]
    symbol_kind: Option<String>,
    /// Maximum definitions and imports to return (default 20, maximum 100).
    #[serde(default, deserialize_with = "deserialize_optional_limit")]
    #[schemars(schema_with = "result_limit_schema", default = "default_result_option")]
    max_results: Option<usize>,
    /// Maximum signature and import tokens (default 8000, maximum 32000).
    #[serde(default, deserialize_with = "deserialize_optional_limit")]
    #[schemars(schema_with = "token_limit_schema", default = "default_token_option")]
    max_tokens: Option<usize>,
    /// Suppress evidence already returned under this server-managed receipt.
    #[serde(default)]
    #[schemars(length(max = 128))]
    receipt_id: Option<String>,
    /// Opaque cursor from a result-limited outline response.
    #[serde(default)]
    #[schemars(length(max = 256))]
    cursor: Option<String>,
    /// Use `reconcile_working_tree` after edits; otherwise `indexed_generation`.
    #[serde(default)]
    #[schemars(schema_with = "index_consistency_schema")]
    consistency: IndexConsistency,
}

impl OutlineMcpRequest {
    fn validate_limits(&self, limits: McpLimitPolicy) -> crate::Result<()> {
        validate_optional_positive_limit("max_results", self.max_results, limits.max_results)?;
        validate_optional_positive_limit("max_tokens", self.max_tokens, limits.max_output_tokens)
    }

    fn into_parts(self) -> (OutlineRequest, IndexConsistency, Option<String>) {
        (
            OutlineRequest {
                paths: self.paths,
                symbol_name: self.symbol_name,
                symbol_kind: self.symbol_kind,
                max_results: self.max_results,
                max_tokens: self.max_tokens,
                receipt_id: self.receipt_id,
                cursor: self.cursor,
            },
            self.consistency,
            self.expected_repository_id,
        )
    }
}

impl FilesMcpRequest {
    fn validate_limits(&self, limits: McpLimitPolicy) -> crate::Result<()> {
        validate_optional_positive_limit("max_results", self.max_results, limits.max_results)?;
        if matches!(self.operation, FilesMcpOperationInput::Nested(_))
            && (self.path.is_some()
                || self.query.is_some()
                || self.pattern.is_some()
                || self.depth.is_some())
        {
            return Err(crate::Error::InvalidInput {
                field: "operation",
                reason: "nested compatibility arguments cannot be mixed with flat arguments",
            });
        }
        let (operation, path, query, pattern, depth) = match &self.operation {
            FilesMcpOperationInput::Flat(operation) => (
                operation.clone(),
                self.path.as_ref(),
                self.query.as_ref(),
                self.pattern.as_ref(),
                self.depth,
            ),
            FilesMcpOperationInput::Nested(LegacyFilesMcpOperation::Tree { path, depth }) => {
                (FileOperation::Tree, path.as_ref(), None, None, *depth)
            }
            FilesMcpOperationInput::Nested(LegacyFilesMcpOperation::Find { query }) => {
                (FileOperation::Find, None, Some(query), None, None)
            }
            FilesMcpOperationInput::Nested(LegacyFilesMcpOperation::Glob { pattern }) => {
                (FileOperation::Glob, None, None, Some(pattern), None)
            }
        };
        let invalid = match operation {
            FileOperation::Tree => query
                .map(|_| "query")
                .or_else(|| pattern.map(|_| "pattern")),
            FileOperation::Find => path
                .map(|_| "path")
                .or_else(|| pattern.map(|_| "pattern"))
                .or(depth.map(|_| "depth")),
            FileOperation::Glob => path
                .map(|_| "path")
                .or_else(|| query.map(|_| "query"))
                .or(depth.map(|_| "depth")),
        };
        if let Some(field) = invalid {
            return Err(crate::Error::InvalidInput {
                field,
                reason: "does not apply to the selected file operation",
            });
        }
        Ok(())
    }

    fn into_parts(self) -> (FilesRequest, IndexConsistency, Option<String>) {
        let (operation, path, query, pattern, depth) = match self.operation {
            FilesMcpOperationInput::Flat(operation) => {
                (operation, self.path, self.query, self.pattern, self.depth)
            }
            FilesMcpOperationInput::Nested(LegacyFilesMcpOperation::Tree { path, depth }) => {
                (FileOperation::Tree, path, None, None, depth)
            }
            FilesMcpOperationInput::Nested(LegacyFilesMcpOperation::Find { query }) => {
                (FileOperation::Find, None, Some(query), None, None)
            }
            FilesMcpOperationInput::Nested(LegacyFilesMcpOperation::Glob { pattern }) => {
                (FileOperation::Glob, None, None, Some(pattern), None)
            }
        };
        (
            FilesRequest {
                operation,
                path,
                query,
                pattern,
                max_results: self.max_results,
                cursor: self.cursor,
                depth,
            },
            self.consistency,
            self.expected_repository_id,
        )
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ReadMcpRequest {
    /// Expected opaque repository identity from an earlier response.
    #[serde(default)]
    #[schemars(schema_with = "expected_repository_id_schema")]
    expected_repository_id: Option<String>,
    /// Repository-relative UTF-8 source file.
    #[schemars(length(min = 1, max = 4096))]
    path: String,
    /// Exact symbol, Markdown heading, line range, or continuation to read.
    target: ReadMcpTarget,
    /// Maximum source tokens to return (default 8000, maximum 32000).
    #[serde(default, deserialize_with = "deserialize_optional_limit")]
    #[schemars(schema_with = "token_limit_schema", default = "default_token_option")]
    max_tokens: Option<usize>,
    /// Hash from the same prior target; matching content returns `not_modified`.
    #[serde(default)]
    #[schemars(schema_with = "expected_repository_id_schema")]
    expected_hash: Option<String>,
    /// Record this target and prefer a cheaper complete delta on changed follow-ups.
    #[serde(default)]
    delta: bool,
    /// Suppress evidence already returned under this server-managed receipt.
    #[serde(default)]
    #[schemars(length(max = 128))]
    receipt_id: Option<String>,
    /// Use `reconcile_working_tree` after edits; otherwise `indexed_generation`.
    #[serde(default)]
    #[schemars(schema_with = "index_consistency_schema")]
    consistency: IndexConsistency,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum ReadMcpTarget {
    /// Read one indexed symbol definition.
    Symbol {
        /// Exact indexed symbol name.
        #[schemars(length(min = 1, max = 4096))]
        name: String,
    },
    /// Read one indexed Markdown section by exact heading title or outline signature.
    Heading {
        /// Exact rendered heading title or outline signature such as `## Performance`.
        #[schemars(length(min = 1, max = 4096))]
        name: String,
        /// One-based occurrence when the heading text is duplicated.
        #[serde(default = "default_heading_occurrence")]
        #[schemars(default = "default_heading_occurrence", range(min = 1))]
        occurrence: usize,
    },
    /// Read one inclusive one-based line range.
    #[serde(alias = "range", alias = "line_range")]
    Lines {
        /// First one-based line.
        #[serde(alias = "start_line")]
        #[schemars(range(min = 1))]
        start: usize,
        /// Last one-based line; must be at least `start`.
        #[serde(alias = "end_line")]
        #[schemars(range(min = 1))]
        end: usize,
    },
    /// Continue a truncated read without losing a partial final line.
    Continuation {
        /// Opaque cursor from the preceding truncated response.
        #[schemars(length(min = 1, max = 256))]
        cursor: String,
    },
}

impl ReadMcpRequest {
    fn validate_limits(&self, limits: McpLimitPolicy) -> crate::Result<()> {
        validate_optional_positive_limit("max_tokens", self.max_tokens, limits.max_output_tokens)?;
        if matches!(self.target, ReadMcpTarget::Heading { occurrence: 0, .. }) {
            return Err(crate::Error::InvalidInput {
                field: "heading occurrence",
                reason: "must be one-based",
            });
        }
        Ok(())
    }

    fn into_parts(self) -> (ReadRequest, IndexConsistency, Option<String>) {
        let (start_line, end_line, symbol, heading, heading_occurrence, continuation_cursor) =
            match self.target {
                ReadMcpTarget::Symbol { name } => (None, None, Some(name), None, None, None),
                ReadMcpTarget::Heading { name, occurrence } => {
                    (None, None, None, Some(name), Some(occurrence), None)
                }
                ReadMcpTarget::Lines { start, end } => {
                    (Some(start), Some(end), None, None, None, None)
                }
                ReadMcpTarget::Continuation { cursor } => {
                    (None, None, None, None, None, Some(cursor))
                }
            };
        (
            ReadRequest {
                path: self.path,
                start_line,
                end_line,
                symbol,
                heading,
                heading_occurrence,
                continuation_cursor,
                max_tokens: self.max_tokens,
                expected_hash: self.expected_hash,
                delta: self.delta,
                receipt_id: self.receipt_id,
            },
            self.consistency,
            self.expected_repository_id,
        )
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct HistoryMcpRequest {
    /// Expected opaque repository identity from an earlier response.
    #[serde(default)]
    #[schemars(schema_with = "expected_repository_id_schema")]
    expected_repository_id: Option<String>,
    /// Git-backed symbol history operation.
    operation: HistoryMcpOperation,
    /// Maximum commits returned by `symbol_log` (default 20, maximum 100).
    #[serde(default, deserialize_with = "deserialize_optional_limit")]
    #[schemars(schema_with = "result_limit_schema", default = "default_result_option")]
    max_results: Option<usize>,
    /// Maximum source or diff tokens to return (default 8000, maximum 32000).
    #[serde(default, deserialize_with = "deserialize_optional_limit")]
    #[schemars(schema_with = "token_limit_schema", default = "default_token_option")]
    max_tokens: Option<usize>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum HistoryMcpOperation {
    /// Read one parsed symbol, optionally qualified as `parent.name`, from an immutable revision.
    ReadSymbol {
        #[schemars(length(min = 1, max = 4096))]
        path: String,
        #[schemars(length(min = 1, max = 4096))]
        symbol: String,
        #[schemars(length(min = 1, max = 4096))]
        revision: String,
    },
    /// Compare one parsed symbol across revisions, including added or removed endpoints.
    DiffSymbol {
        #[schemars(length(min = 1, max = 4096))]
        path: String,
        #[schemars(length(min = 1, max = 4096))]
        symbol: String,
        #[schemars(length(min = 1, max = 4096))]
        base_revision: String,
        #[schemars(length(min = 1, max = 4096))]
        head_revision: String,
    },
    /// List commits that touched the symbol's tracked historical lines.
    SymbolLog {
        #[schemars(length(min = 1, max = 4096))]
        path: String,
        #[schemars(length(min = 1, max = 4096))]
        symbol: String,
        #[serde(default)]
        #[schemars(length(min = 1, max = 4096))]
        revision: Option<String>,
    },
}

impl HistoryMcpRequest {
    fn validate_limits(&self, limits: McpLimitPolicy) -> crate::Result<()> {
        validate_optional_positive_limit("max_results", self.max_results, MAX_RESULTS)?;
        validate_optional_positive_limit("max_tokens", self.max_tokens, limits.max_output_tokens)
    }

    fn into_parts(self) -> (HistoryRequest, Option<String>) {
        let operation = match self.operation {
            HistoryMcpOperation::ReadSymbol {
                path,
                symbol,
                revision,
            } => HistoryOperation::ReadSymbol {
                path,
                symbol,
                revision,
            },
            HistoryMcpOperation::DiffSymbol {
                path,
                symbol,
                base_revision,
                head_revision,
            } => HistoryOperation::DiffSymbol {
                path,
                symbol,
                base_revision,
                head_revision,
            },
            HistoryMcpOperation::SymbolLog {
                path,
                symbol,
                revision,
            } => HistoryOperation::SymbolLog {
                path,
                symbol,
                revision,
            },
        };
        (
            HistoryRequest {
                operation,
                max_results: self.max_results,
                max_tokens: self.max_tokens,
            },
            self.expected_repository_id,
        )
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct JsonMcpRequest {
    /// Expected opaque repository identity from an earlier response.
    #[serde(default)]
    #[schemars(schema_with = "expected_repository_id_schema")]
    expected_repository_id: Option<String>,
    /// Structural JSON operation.
    operation: JsonMcpOperation,
    /// Maximum tokens across selected/projected JSON (default 8000, maximum 32000).
    #[serde(default, deserialize_with = "deserialize_optional_limit")]
    #[schemars(schema_with = "token_limit_schema", default = "default_token_option")]
    max_tokens: Option<usize>,
    /// Maximum structural items returned (default 1000, maximum 10000).
    #[serde(default, deserialize_with = "deserialize_optional_limit")]
    #[schemars(range(min = 1, max = 10000))]
    max_items: Option<usize>,
    /// Array elements sampled by collapsed projections (default 3, maximum 20).
    #[serde(default)]
    #[schemars(range(min = 0, max = 20))]
    array_sample_size: Option<usize>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum JsonMcpSelector {
    /// RFC 6901 JSON Pointer.
    Pointer {
        #[schemars(length(max = 4096))]
        pointer: String,
    },
    /// Standard JMESPath expression.
    Jmespath {
        #[schemars(length(min = 1, max = 4096))]
        expression: String,
    },
}

impl From<JsonMcpSelector> for JsonSelector {
    fn from(value: JsonMcpSelector) -> Self {
        match value {
            JsonMcpSelector::Pointer { pointer } => Self::Pointer { pointer },
            JsonMcpSelector::Jmespath { expression } => Self::Jmespath { expression },
        }
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum JsonMcpOperation {
    /// Select and project one JSON value.
    Query {
        #[schemars(length(min = 1, max = 4096))]
        path: String,
        #[serde(default)]
        selector: Option<JsonMcpSelector>,
        #[serde(default)]
        projection: JsonProjection,
    },
    /// Summarize numeric leaves below one JSON selection.
    NumericSummary {
        #[schemars(length(min = 1, max = 4096))]
        path: String,
        #[serde(default)]
        selector: Option<JsonMcpSelector>,
    },
    /// Compare selected fields between two JSON files.
    DiffFields {
        #[schemars(length(min = 1, max = 4096))]
        base_path: String,
        #[schemars(length(min = 1, max = 4096))]
        head_path: String,
        #[schemars(length(min = 1, max = 100))]
        selectors: Vec<JsonMcpSelector>,
        #[serde(default)]
        projection: JsonProjection,
    },
}

impl JsonMcpRequest {
    fn validate_limits(&self, limits: McpLimitPolicy) -> crate::Result<()> {
        validate_optional_positive_limit("max_tokens", self.max_tokens, limits.max_output_tokens)?;
        validate_optional_positive_limit("max_items", self.max_items, 10_000)?;
        if self.array_sample_size.is_some_and(|value| value > 20) {
            return Err(crate::Error::RequestLimitExceeded {
                field: "array_sample_size",
                requested: self.array_sample_size.unwrap_or_default(),
                limit: 20,
            });
        }
        Ok(())
    }

    fn into_parts(self) -> (JsonRequest, Option<String>) {
        let operation = match self.operation {
            JsonMcpOperation::Query {
                path,
                selector,
                projection,
            } => JsonOperation::Query {
                path,
                selector: selector.map(Into::into),
                projection,
            },
            JsonMcpOperation::NumericSummary { path, selector } => JsonOperation::NumericSummary {
                path,
                selector: selector.map(Into::into),
            },
            JsonMcpOperation::DiffFields {
                base_path,
                head_path,
                selectors,
                projection,
            } => JsonOperation::DiffFields {
                base_path,
                head_path,
                selectors: selectors.into_iter().map(Into::into).collect(),
                projection,
            },
        };
        (
            JsonRequest {
                operation,
                max_tokens: self.max_tokens,
                max_items: self.max_items,
                array_sample_size: self.array_sample_size,
            },
            self.expected_repository_id,
        )
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ContextMcpRequest {
    /// Expected opaque repository identity from an earlier response.
    #[serde(default)]
    #[schemars(length(max = 128))]
    expected_repository_id: Option<String>,
    /// Evidence workflow; `auto` selects only on high-confidence task language.
    #[serde(default)]
    workflow: ContextWorkflow,
    /// Natural-language coding task; include known identifiers and constraints.
    #[schemars(length(min = 3, max = 65536))]
    task: String,
    /// Maximum source tokens across selected fragments (default 3000, maximum 32000).
    #[serde(default, deserialize_with = "deserialize_optional_limit")]
    #[schemars(
        schema_with = "context_token_limit_schema",
        default = "default_context_token_option"
    )]
    token_budget: Option<usize>,
    /// Require every returned source fragment to match one of these path patterns.
    #[serde(default)]
    #[schemars(length(max = 256), inner(length(max = 4096)))]
    include_paths: Vec<String>,
    /// Require evidence matching every indexed path pattern.
    #[serde(default)]
    #[schemars(length(max = 256), inner(length(max = 4096)))]
    must_include_paths: Vec<String>,
    /// Require evidence for every exact indexed symbol.
    #[serde(default)]
    #[schemars(length(max = 256), inner(length(max = 4096)))]
    must_include_symbols: Vec<String>,
    /// Maximum returned fragments (default 8, maximum 100).
    #[serde(default, deserialize_with = "deserialize_optional_limit")]
    #[schemars(
        schema_with = "context_fragment_limit_schema",
        default = "default_context_fragment_option"
    )]
    max_fragments: Option<usize>,
    /// Preview ranked candidates without source or receipt mutation; omit `receipt_id`.
    #[serde(default)]
    plan_only: bool,
    /// Boost matching paths without filtering other candidates.
    #[serde(default)]
    #[schemars(length(max = 256), inner(length(max = 4096)))]
    focus_paths: Vec<String>,
    /// Require every returned fragment to match at least one focus path.
    #[serde(default)]
    strict_focus_paths: bool,
    /// Minimum returned fragments required for each focus path pattern.
    #[serde(default, deserialize_with = "deserialize_optional_limit")]
    #[schemars(schema_with = "context_fragment_limit_schema")]
    minimum_fragments_per_focus_path: Option<usize>,
    /// Boost candidates for these exact symbol names.
    #[serde(default)]
    #[schemars(length(max = 256), inner(length(max = 4096)))]
    focus_symbols: Vec<String>,
    /// Exclude matching repository paths.
    #[serde(default)]
    #[schemars(length(max = 256), inner(length(max = 4096)))]
    exclude_paths: Vec<String>,
    /// Fragment hashes already held by the caller and not to resend.
    #[serde(default)]
    #[schemars(length(max = 256), inner(length(max = 128)))]
    known_hashes: Vec<String>,
    /// Suppress evidence already returned under this server-managed receipt.
    #[serde(default)]
    #[schemars(length(max = 128))]
    receipt_id: Option<String>,
    /// Earlier generation used to boost files indexed since that response.
    #[serde(default)]
    prior_repository_generation: Option<u64>,
    /// Base revision or `BASE..HEAD` range for diff-scoped context.
    #[serde(default)]
    #[schemars(length(max = 256))]
    base_revision: Option<String>,
    /// Changed paths for diff-scoped context.
    #[serde(default)]
    #[schemars(length(max = 512), inner(length(max = 4096)))]
    changed_paths: Vec<String>,
    /// Require every returned fragment to belong to the resolved changed paths.
    #[serde(default)]
    strict_changed_paths: bool,
    /// Attach a compact provenance manifest for a host-triggered executor handoff.
    #[serde(default)]
    handoff: Option<HandoffManifestRequest>,
    /// Use `reconcile_working_tree` after edits; otherwise `indexed_generation`.
    #[serde(default)]
    #[schemars(schema_with = "index_consistency_schema")]
    consistency: IndexConsistency,
}

impl ContextMcpRequest {
    fn validate_limits(&self, limits: McpLimitPolicy) -> crate::Result<()> {
        validate_optional_positive_limit(
            "token_budget",
            self.token_budget,
            limits.max_output_tokens,
        )?;
        validate_optional_positive_limit("max_fragments", self.max_fragments, limits.max_results)?;
        validate_optional_positive_limit(
            "minimum_fragments_per_focus_path",
            self.minimum_fragments_per_focus_path,
            limits.max_results,
        )
    }

    fn into_parts(
        self,
        default_token_budget: usize,
    ) -> (
        ContextRequest,
        ContextWorkflow,
        IndexConsistency,
        Option<String>,
        Option<HandoffManifestRequest>,
    ) {
        (
            ContextRequest {
                task: self.task,
                token_budget: self.token_budget.unwrap_or(default_token_budget),
                include_paths: self.include_paths,
                must_include_paths: self.must_include_paths,
                must_include_symbols: self.must_include_symbols,
                max_fragments: self.max_fragments,
                plan_only: self.plan_only,
                focus_paths: self.focus_paths,
                strict_focus_paths: self.strict_focus_paths,
                minimum_fragments_per_focus_path: self.minimum_fragments_per_focus_path,
                focus_symbols: self.focus_symbols,
                exclude_paths: self.exclude_paths,
                known_hashes: self.known_hashes,
                receipt_id: self.receipt_id,
                prior_repository_generation: self.prior_repository_generation,
                base_revision: self.base_revision,
                changed_paths: self.changed_paths,
                strict_changed_paths: self.strict_changed_paths,
            },
            self.workflow,
            self.consistency,
            self.expected_repository_id,
            self.handoff,
        )
    }
}

const fn default_context_token_option() -> Option<usize> {
    Some(DEFAULT_CONTEXT_TOKENS)
}

const fn default_context_fragment_option() -> Option<usize> {
    Some(DEFAULT_CONTEXT_FRAGMENTS)
}

fn context_fragment_limit_schema(_: &mut SchemaGenerator) -> Schema {
    schemars::json_schema!({
        "type": "integer",
        "minimum": 1,
        "maximum": MAX_RESULTS,
        "default": DEFAULT_CONTEXT_FRAGMENTS
    })
}

const fn default_result_option() -> Option<usize> {
    Some(DEFAULT_RESULTS)
}

const fn default_token_option() -> Option<usize> {
    Some(DEFAULT_READ_TOKENS)
}

const fn default_context_line_option() -> Option<usize> {
    Some(DEFAULT_CONTEXT_LINES)
}

const fn default_heading_occurrence() -> usize {
    1
}

fn result_limit_schema(_: &mut SchemaGenerator) -> Schema {
    schemars::json_schema!({
        "type": "integer",
        "format": "uint",
        "minimum": 1,
        "maximum": MAX_RESULTS,
        "default": DEFAULT_RESULTS
    })
}

fn token_limit_schema(_: &mut SchemaGenerator) -> Schema {
    schemars::json_schema!({
        "type": "integer",
        "format": "uint",
        "minimum": 1,
        "maximum": MAX_OUTPUT_TOKENS,
        "default": DEFAULT_READ_TOKENS
    })
}

fn context_token_limit_schema(_: &mut SchemaGenerator) -> Schema {
    schemars::json_schema!({
        "type": "integer",
        "format": "uint",
        "minimum": 1,
        "maximum": MAX_OUTPUT_TOKENS,
        "default": DEFAULT_CONTEXT_TOKENS
    })
}

fn context_line_limit_schema(_: &mut SchemaGenerator) -> Schema {
    schemars::json_schema!({
        "type": "integer",
        "format": "uint",
        "minimum": 0,
        "maximum": MAX_CONTEXT_LINES,
        "default": DEFAULT_CONTEXT_LINES
    })
}

fn expected_repository_id_schema(_: &mut SchemaGenerator) -> Schema {
    schemars::json_schema!({
        "type": ["string", "null"],
        "maxLength": crate::services::MAX_EXPECTED_REPOSITORY_ID_BYTES
    })
}

fn file_operation_schema(generator: &mut SchemaGenerator) -> Schema {
    generator.subschema_for::<FileOperation>()
}

fn add_files_operation_constraints(schema: &mut Schema) {
    schema.insert(
        "oneOf".into(),
        serde_json::json!([
            {
                "properties": {"operation": {"const": "tree"}},
                "not": {"anyOf": [
                    {"required": ["query"]},
                    {"required": ["pattern"]}
                ]}
            },
            {
                "properties": {
                    "operation": {"const": "find"},
                    "query": {"type": "string"}
                },
                "required": ["query"],
                "not": {"anyOf": [
                    {"required": ["path"]},
                    {"required": ["pattern"]},
                    {"required": ["depth"]}
                ]}
            },
            {
                "properties": {
                    "operation": {"const": "glob"},
                    "pattern": {"type": "string"}
                },
                "required": ["pattern"],
                "not": {"anyOf": [
                    {"required": ["path"]},
                    {"required": ["query"]},
                    {"required": ["depth"]}
                ]}
            }
        ]),
    );
}

fn validate_optional_positive_limit(
    field: &'static str,
    requested: Option<usize>,
    limit: usize,
) -> crate::Result<()> {
    requested.map_or(Ok(()), |requested| {
        validate_positive_request_limit(field, requested, limit).map(drop)
    })
}

fn validate_optional_limit(
    field: &'static str,
    requested: Option<usize>,
    limit: usize,
) -> crate::Result<()> {
    requested.map_or(Ok(()), |requested| {
        validate_request_limit(field, requested, limit).map(drop)
    })
}

fn deserialize_optional_limit<'de, D>(
    deserializer: D,
) -> std::result::Result<Option<usize>, D::Error>
where
    D: Deserializer<'de>,
{
    usize::deserialize(deserializer).map(Some)
}

#[derive(Debug, Serialize)]
struct RetryableToolResponse {
    status: &'static str,
    reason: &'static str,
    message: &'static str,
    retry_after_ms: u64,
}

impl RetryableToolResponse {
    const fn new(reason: &'static str, message: &'static str, retry_after_ms: u64) -> Self {
        Self {
            status: "retryable",
            reason,
            message,
            retry_after_ms,
        }
    }
}

fn index_consistency_schema(_: &mut SchemaGenerator) -> Schema {
    schemars::json_schema!({
        "type": "string",
        "enum": ["indexed_generation", "reconcile_working_tree"]
    })
}

/// LeanToken MCP server.
#[derive(Clone)]
pub struct LeanTokenMcp {
    services: McpServices,
    result_mode: McpResultMode,
}

#[derive(Debug, Clone, Copy)]
struct McpLimitPolicy {
    max_results: usize,
    max_output_tokens: usize,
    max_context_lines: usize,
    default_context_tokens: usize,
}

impl McpLimitPolicy {
    const DEFAULT: Self = Self {
        max_results: MAX_RESULTS,
        max_output_tokens: MAX_OUTPUT_TOKENS,
        max_context_lines: MAX_CONTEXT_LINES,
        default_context_tokens: DEFAULT_CONTEXT_TOKENS,
    };

    fn from_config(config: &Config) -> crate::Result<Self> {
        config.validate()?;
        Ok(Self {
            max_results: config.max_results,
            max_output_tokens: config.max_output_tokens,
            max_context_lines: MAX_CONTEXT_LINES,
            default_context_tokens: config.default_context_tokens,
        })
    }
}

#[derive(Debug, Clone)]
enum McpServiceState {
    Starting(McpLimitPolicy),
    Ready {
        services: Arc<Services>,
        limits: McpLimitPolicy,
    },
    Failed(McpLimitPolicy),
}

impl McpServiceState {
    const fn limits(&self) -> McpLimitPolicy {
        match self {
            Self::Starting(limits) | Self::Ready { limits, .. } | Self::Failed(limits) => *limits,
        }
    }
}

/// Shared readiness handle used by handshake-first MCP startup.
#[derive(Debug, Clone)]
pub struct McpServices {
    state: Arc<RwLock<McpServiceState>>,
    protocol_initialized: Arc<AtomicBool>,
    initialized: Arc<tokio::sync::Notify>,
}

/// Wire representation used for successful MCP tool results.
#[derive(Debug, Clone, Copy, Default, clap::ValueEnum, PartialEq, Eq)]
pub enum McpResultMode {
    /// Send JSON as both text and structured content for broad host compatibility.
    #[default]
    Dual,
    /// Send JSON only as text content for hosts that ignore structured content.
    Text,
    /// Send only structured content for hosts verified to support it.
    Structured,
}

impl LeanTokenMcp {
    #[must_use]
    pub fn new(services: Arc<Services>) -> Self {
        Self {
            services: McpServices::ready(services),
            result_mode: McpResultMode::Dual,
        }
    }

    /// Construct a protocol-ready server before storage and indexing start.
    #[must_use]
    pub fn pending() -> (Self, McpServices) {
        let services = McpServices::starting(McpLimitPolicy::DEFAULT);
        (
            Self {
                services: services.clone(),
                result_mode: McpResultMode::Dual,
            },
            services,
        )
    }

    /// Select the successful-result representation for this server instance.
    #[must_use]
    pub fn with_result_mode(mut self, result_mode: McpResultMode) -> Self {
        self.result_mode = result_mode;
        self
    }

    fn result<T: Serialize>(&self, value: T) -> Result<CallToolResult, ErrorData> {
        tool_result(value, self.result_mode)
    }

    fn services(
        &self,
        state: &McpServiceState,
    ) -> std::result::Result<Arc<Services>, CallToolResult> {
        match state {
            McpServiceState::Ready { services, .. } => Ok(Arc::clone(services)),
            McpServiceState::Starting(_) => Err(self.retryable_result(RetryableToolResponse::new(
                "index_starting",
                "repository index is starting; retry the same call shortly",
                500,
            ))),
            McpServiceState::Failed(_) => Err(tool_unavailable(
                "repository index is unavailable; check server logs and retry",
            )),
        }
    }

    fn retryable_result(&self, response: RetryableToolResponse) -> CallToolResult {
        self.result(response).unwrap_or_else(|error| {
            tracing::error!(%error, "MCP retry response serialization failed");
            tool_unavailable("repository retrieval is temporarily unavailable; retry shortly")
        })
    }

    fn service_result<T: Serialize>(
        &self,
        result: crate::Result<T>,
    ) -> Result<CallToolResult, ErrorData> {
        match result {
            Ok(value) => self.result(value),
            Err(crate::Error::IndexNotReady) => {
                Ok(self.retryable_result(RetryableToolResponse::new(
                    "index_building",
                    "repository index is being built; retry the same call shortly",
                    500,
                )))
            }
            Err(crate::Error::RetryableConflict(_)) => {
                Ok(self.retryable_result(RetryableToolResponse::new(
                    "repository_changed",
                    "repository index changed during retrieval; retry the same call",
                    100,
                )))
            }
            Err(error) => Err(into_mcp_error(error)),
        }
    }
}

impl McpServices {
    fn starting(limits: McpLimitPolicy) -> Self {
        Self {
            state: Arc::new(RwLock::new(McpServiceState::Starting(limits))),
            protocol_initialized: Arc::new(AtomicBool::new(false)),
            initialized: Arc::new(tokio::sync::Notify::new()),
        }
    }

    fn ready(services: Arc<Services>) -> Self {
        let limits = McpLimitPolicy::from_config(services.config())
            .expect("Services always contains a validated configuration");
        Self {
            state: Arc::new(RwLock::new(McpServiceState::Ready { services, limits })),
            protocol_initialized: Arc::new(AtomicBool::new(false)),
            initialized: Arc::new(tokio::sync::Notify::new()),
        }
    }

    fn get(&self) -> McpServiceState {
        self.state
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    /// Make initialized retrieval services visible to MCP tool handlers.
    pub fn set_ready(&self, services: Arc<Services>) {
        let limits = McpLimitPolicy::from_config(services.config())
            .expect("Services always contains a validated configuration");
        *self
            .state
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) =
            McpServiceState::Ready { services, limits };
    }

    /// Apply validated configured request limits before retrieval services are ready.
    ///
    /// # Errors
    ///
    /// Returns an error when `config` contains invalid runtime limits.
    pub fn configure_limits(&self, config: &Config) -> crate::Result<()> {
        let limits = McpLimitPolicy::from_config(config)?;
        let mut state = self
            .state
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        match &mut *state {
            McpServiceState::Starting(current) | McpServiceState::Failed(current) => {
                *current = limits;
            }
            McpServiceState::Ready { .. } => {}
        }
        Ok(())
    }

    /// Mark startup as failed without exposing internal diagnostics to clients.
    pub fn set_failed(&self) {
        let mut state = self
            .state
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *state = McpServiceState::Failed(state.limits());
    }

    fn mark_protocol_initialized(&self) {
        self.protocol_initialized.store(true, Ordering::Release);
        self.initialized.notify_waiters();
    }

    /// Wait until the client completes the MCP initialization phase.
    pub async fn wait_initialized(&self) {
        loop {
            let notified = self.initialized.notified();
            if self.protocol_initialized.load(Ordering::Acquire) {
                return;
            }
            notified.await;
        }
    }
}

#[tool_router]
impl LeanTokenMcp {
    #[tool(
        name = "files",
        description = "Preferred repository path discovery instead of find, ls, or glob. Use tree for hierarchy, find for fuzzy filenames, and glob for path patterns; returns paths, not source. Example: {\"operation\":\"find\",\"query\":\"mcp\"}."
    )]
    async fn leantoken_files(
        &self,
        Parameters(req): Parameters<FilesMcpRequest>,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, ErrorData> {
        let state = self.services.get();
        req.validate_limits(state.limits())
            .map_err(into_mcp_error)?;
        let services = match self.services(&state) {
            Ok(services) => services,
            Err(result) => return Ok(result),
        };
        let (request, consistency, expected_repository_id) = req.into_parts();
        services
            .validate_repository_id(expected_repository_id.as_deref())
            .map_err(into_mcp_error)?;
        let resp = services
            .files_with_consistency_cancellable(request, consistency, context.ct.clone())
            .await;
        self.service_result(resp)
    }

    #[tool(
        name = "search",
        description = "Preferred indexed source search instead of grep or rg. Finds ranked symbols, references, identifiers, text, or regex matches. Set all_occurrences in text or regex mode for exact occurrence coordinates and returned/total counts; exhaustive scans fail instead of silently truncating at internal scan limits. Text and regex hits include the narrowest enclosing_symbol when structural data is available; use that exact name or the returned line range with leantoken.read. Example: {\"query\":\"RetryableConflict\",\"mode\":\"symbol\"}."
    )]
    async fn leantoken_search(
        &self,
        Parameters(req): Parameters<SearchMcpRequest>,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, ErrorData> {
        let state = self.services.get();
        req.validate_limits(state.limits())
            .map_err(into_mcp_error)?;
        let services = match self.services(&state) {
            Ok(services) => services,
            Err(result) => return Ok(result),
        };
        let (request, consistency, expected_repository_id) = req.into_parts();
        services
            .validate_repository_id(expected_repository_id.as_deref())
            .map_err(into_mcp_error)?;
        let resp = services
            .search_with_consistency_cancellable(request, consistency, context.ct.clone())
            .await;
        self.service_result(resp)
    }

    #[tool(
        name = "outline",
        description = "Inspect file structure without reading whole source files. Prefer this when the file is known but the relevant symbol or range is not; then use leantoken.read. Example: {\"paths\":[\"src/mcp.rs\"]}."
    )]
    async fn leantoken_outline(
        &self,
        Parameters(req): Parameters<OutlineMcpRequest>,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, ErrorData> {
        let state = self.services.get();
        req.validate_limits(state.limits())
            .map_err(into_mcp_error)?;
        let services = match self.services(&state) {
            Ok(services) => services,
            Err(result) => return Ok(result),
        };
        let (request, consistency, expected_repository_id) = req.into_parts();
        services
            .validate_repository_id(expected_repository_id.as_deref())
            .map_err(into_mcp_error)?;
        let resp = services
            .outline_with_consistency_cancellable(request, consistency, context.ct.clone())
            .await;
        self.service_result(resp)
    }

    #[tool(
        name = "read",
        description = "Preferred exact source and Markdown section reader instead of cat, head, or sed. Keep path as a file path; put the owner separately in target. Exact target shapes include {\"kind\":\"symbol\",\"name\":\"LeanTokenMcp\"}, {\"kind\":\"heading\",\"name\":\"## Performance\",\"occurrence\":2}, and {\"kind\":\"lines\",\"start\":120,\"end\":160}. Heading targets accept an exact rendered title or outline signature. Reuse content_hash as expected_hash to suppress unchanged source. Example: {\"path\":\"README.md\",\"target\":{\"kind\":\"heading\",\"name\":\"Installation\"}}."
    )]
    async fn leantoken_read(
        &self,
        Parameters(req): Parameters<ReadMcpRequest>,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, ErrorData> {
        let state = self.services.get();
        req.validate_limits(state.limits())
            .map_err(into_mcp_error)?;
        let services = match self.services(&state) {
            Ok(services) => services,
            Err(result) => return Ok(result),
        };
        let (request, consistency, expected_repository_id) = req.into_parts();
        services
            .validate_repository_id(expected_repository_id.as_deref())
            .map_err(into_mcp_error)?;
        let resp = services
            .read_with_consistency_cancellable(request, consistency, context.ct.clone())
            .await;
        self.service_result(resp)
    }

    #[tool(
        name = "history",
        description = "Read, diff, or trace one parsed symbol across immutable Git revisions. Symbols may use parent.name qualification. diff_symbol returns bounded add/delete diffs when the symbol or file exists at only one endpoint; symbol_log traces tracked lines. For immutable range-scoped context, pass BASE..HEAD as context.base_revision with strict_changed_paths. Example: {\"operation\":{\"kind\":\"diff_symbol\",\"path\":\"src/services.rs\",\"symbol\":\"Services.meta\",\"base_revision\":\"main~1\",\"head_revision\":\"main\"}}."
    )]
    async fn leantoken_history(
        &self,
        Parameters(req): Parameters<HistoryMcpRequest>,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, ErrorData> {
        let state = self.services.get();
        req.validate_limits(state.limits())
            .map_err(into_mcp_error)?;
        let services = match self.services(&state) {
            Ok(services) => services,
            Err(result) => return Ok(result),
        };
        let (request, expected_repository_id) = req.into_parts();
        services
            .validate_repository_id(expected_repository_id.as_deref())
            .map_err(into_mcp_error)?;
        let resp = services
            .history_cancellable(request, context.ct.clone())
            .await;
        self.service_result(resp)
    }

    #[tool(
        name = "json",
        description = "Query, summarize, or compare bounded live JSON without indexing raw artifacts. Select with RFC 6901 JSON Pointer or standard JMESPath; use collapsed, keys, or schema projections for large arrays and objects, numeric_summary for count/min/median/p95/max, and diff_fields for selected values across two files. Example: {\"operation\":{\"kind\":\"numeric_summary\",\"path\":\"artifacts/results.json\",\"selector\":{\"kind\":\"jmespath\",\"expression\":\"runs[].score\"}}}."
    )]
    async fn leantoken_json(
        &self,
        Parameters(req): Parameters<JsonMcpRequest>,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, ErrorData> {
        let state = self.services.get();
        req.validate_limits(state.limits())
            .map_err(into_mcp_error)?;
        let services = match self.services(&state) {
            Ok(services) => services,
            Err(result) => return Ok(result),
        };
        let (request, expected_repository_id) = req.into_parts();
        services
            .validate_repository_id(expected_repository_id.as_deref())
            .map_err(into_mcp_error)?;
        let resp = services.json_cancellable(request, context.ct.clone()).await;
        self.service_result(resp)
    }

    #[tool(
        name = "context",
        description = "DEFAULT FIRST CALL for broad coding, debugging, review, and architecture tasks. Returns the most relevant repository evidence within a strict token budget instead of manually combining search and whole-file reads. For uncertain broad tasks, set plan_only=true to preview bounded ranked paths, ranges, reasons, token estimates, focus coverage, and generated-artifact warnings without source or receipt mutation; then repeat the same request with plan_only=false to materialize. Use include_paths, strict_focus_paths, or strict_changed_paths for hard boundaries; pass BASE..HEAD as base_revision for an immutable Git range. Use minimum_fragments_per_focus_path and must-include constraints for required evidence. Coverage diagnostics fail loud when strict scopes are empty or underfilled. Oversized diff scopes may return bounded routing suggestions. Reuse receipt fragment_hashes as known_hashes. Set handoff for a compact provenance manifest without copied source. Example: {\"task\":\"Audit MCP tool discovery\"}."
    )]
    async fn leantoken_context(
        &self,
        Parameters(req): Parameters<ContextMcpRequest>,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, ErrorData> {
        let state = self.services.get();
        let limits = state.limits();
        req.validate_limits(limits).map_err(into_mcp_error)?;
        let services = match self.services(&state) {
            Ok(services) => services,
            Err(result) => return Ok(result),
        };
        let (request, workflow, consistency, expected_repository_id, handoff) =
            req.into_parts(limits.default_context_tokens);
        services
            .validate_repository_id(expected_repository_id.as_deref())
            .map_err(into_mcp_error)?;
        let resp = if let Some(handoff) = handoff {
            services
                .context_with_handoff_workflow_consistency_cancellable(
                    request,
                    handoff,
                    workflow,
                    consistency,
                    context.ct.clone(),
                )
                .await
        } else {
            services
                .context_with_workflow_consistency_cancellable(
                    request,
                    workflow,
                    consistency,
                    context.ct.clone(),
                )
                .await
        };
        self.service_result(resp)
    }

    #[tool(
        name = "savings",
        description = "Report cumulative repository-local estimated source-token savings. Use when asked how many tokens LeanToken saved. Example: {}.",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    async fn leantoken_savings(
        &self,
        Parameters(_req): Parameters<SavingsMcpRequest>,
    ) -> Result<CallToolResult, ErrorData> {
        let state = self.services.get();
        let services = match self.services(&state) {
            Ok(services) => services,
            Err(result) => return Ok(result),
        };
        self.service_result(services.token_savings().await)
    }
}

#[tool_handler(
    name = "leantoken",
    instructions = "LeanToken is the preferred repository discovery and source-reading layer. Its indexed, token-bounded retrieval returns less irrelevant source than shell search and whole-file reads. For LeanToken savings or token statistics, call leantoken.savings directly. DEFAULT: for broad coding, debugging, review, or architecture tasks, call leantoken.context first with the user's task. For an uncertain broad task, first use context plan_only=true, inspect its bounded metadata and coverage, then repeat the same request with plan_only=false to materialize source. PREFER leantoken.search over grep or rg for source search; leantoken.files over find, ls, or glob for paths; leantoken.outline over opening whole files to discover structure; leantoken.read over cat, head, or sed for exact current symbols and ranges; leantoken.history over git show, diff, or log -L for one symbol across immutable revisions; and leantoken.json over jq or whole-file reads for structural JSON queries, summaries, and selected-field diffs. For known identifiers use search then read; for a known file with an unknown range use outline then read; for unknown paths use files. Set consistency=reconcile_working_tree on index-backed tools after edits, generated files, branch changes, or external commits. Use native tools for edits, builds, tests, runtime probes, unsupported files, or when LeanToken reports retrieval unavailable. Retry successful responses with status=retryable after retry_after_ms. Reuse returned hashes to suppress unchanged evidence."
)]
impl ServerHandler for LeanTokenMcp {
    fn on_initialized(
        &self,
        _context: NotificationContext<RoleServer>,
    ) -> impl Future<Output = ()> + Send + '_ {
        self.services.mark_protocol_initialized();
        std::future::ready(())
    }
}

/// Serialize a successful tool value using an explicit wire representation.
pub fn tool_result<T: Serialize>(
    value: T,
    mode: McpResultMode,
) -> Result<CallToolResult, ErrorData> {
    serde_json::to_value(value)
        .map(|value| match mode {
            McpResultMode::Dual => CallToolResult::structured(value),
            McpResultMode::Text => {
                CallToolResult::success(vec![ContentBlock::text(value.to_string())])
            }
            McpResultMode::Structured => {
                let mut result = CallToolResult::default();
                result.structured_content = Some(value);
                result.is_error = Some(false);
                result
            }
        })
        .map_err(|error| {
            tracing::error!(%error, "MCP response serialization failed");
            ErrorData::internal_error(
                "repository retrieval failed",
                mcp_error_data("response_serialization"),
            )
        })
}

fn into_mcp_error(error: crate::Error) -> ErrorData {
    match &error {
        crate::Error::Cancelled => {
            ErrorData::invalid_request("request cancelled", mcp_error_data("request_cancelled"))
        }
        crate::Error::PathOutsideRoot(_) => {
            tracing::debug!(%error, "MCP path rejected outside repository root");
            ErrorData::invalid_params(
                "path must stay within the repository root",
                mcp_error_data("path_outside_root"),
            )
        }
        crate::Error::UnsupportedPathEncoding(_) => ErrorData::invalid_params(
            "repository path is not valid UTF-8",
            mcp_error_data("unsupported_path_encoding"),
        ),
        crate::Error::NotIndexed(_) => ErrorData::invalid_params(
            "requested path is not indexed",
            mcp_error_data("not_indexed"),
        ),
        crate::Error::SymbolNotFound { .. } => ErrorData::invalid_params(
            "requested symbol is not indexed",
            mcp_error_data("symbol_not_found"),
        ),
        crate::Error::HeadingNotFound { .. } => ErrorData::invalid_params(
            "requested Markdown heading occurrence is not indexed",
            mcp_error_data("heading_not_found"),
        ),
        crate::Error::RepositoryIdentityMismatch { expected, actual } => ErrorData::invalid_params(
            "repository identity does not match this server",
            Some(serde_json::json!({
                "category": "repository_identity_mismatch",
                "expected_repository_id": expected,
                "actual_repository_id": actual,
            })),
        ),
        crate::Error::LimitExceeded => ErrorData::invalid_params(
            "request exceeds a configured limit",
            mcp_error_data("request_limit_exceeded"),
        ),
        crate::Error::RequestLimitExceeded {
            field,
            requested,
            limit,
        } => ErrorData::invalid_params(
            format!("{field} exceeds its configured limit"),
            Some(serde_json::json!({
                "category": "request_limit_exceeded",
                "field": field,
                "requested": requested,
                "limit": limit,
            })),
        ),
        crate::Error::UnsupportedLanguage(_) => ErrorData::invalid_params(
            "requested structured language is unsupported",
            mcp_error_data("unsupported_language"),
        ),
        crate::Error::InvalidInput { field, reason } => ErrorData::invalid_params(
            format!("invalid {field}: {reason}"),
            Some(serde_json::json!({
                "category": "invalid_input",
                "field": field,
            })),
        ),
        crate::Error::InputTooLong { field, max_bytes } => ErrorData::invalid_params(
            "request input exceeds its byte limit",
            Some(serde_json::json!({
                "category": "input_too_long",
                "field": field,
                "limit": max_bytes,
            })),
        ),
        crate::Error::InvalidRequest(_) => ErrorData::invalid_params(
            "request parameters are invalid",
            mcp_error_data("invalid_request"),
        ),
        crate::Error::StaleCursor => {
            ErrorData::invalid_params("cursor is stale or invalid", mcp_error_data("stale_cursor"))
        }
        crate::Error::UnknownReceipt(_) => ErrorData::invalid_params(
            "retrieval receipt is unknown or expired",
            mcp_error_data("unknown_receipt"),
        ),
        crate::Error::StaleReceipt { .. } => ErrorData::invalid_params(
            "retrieval receipt belongs to a stale repository generation",
            mcp_error_data("stale_receipt"),
        ),
        crate::Error::Regex(_) => ErrorData::invalid_params(
            "regular expression is invalid",
            mcp_error_data("invalid_regex"),
        ),
        crate::Error::Glob(_) => {
            ErrorData::invalid_params("glob pattern is invalid", mcp_error_data("invalid_glob"))
        }
        crate::Error::RootNotFound(_)
        | crate::Error::UnsafeRepositoryRoot(_)
        | crate::Error::RepositoryMismatch { .. }
        | crate::Error::InvalidConfiguration(_) => {
            tracing::error!(%error, "repository configuration is invalid");
            ErrorData::internal_error(
                "repository configuration is invalid",
                mcp_error_data("repository_configuration"),
            )
        }
        crate::Error::IndexLimitExceeded { .. } => {
            tracing::error!(%error, "repository indexing limit exceeded");
            ErrorData::internal_error(
                "repository indexing limit exceeded",
                mcp_error_data("repository_index_limit"),
            )
        }
        crate::Error::RepositoryTraversal(_) => {
            tracing::error!(%error, "repository traversal failed");
            ErrorData::internal_error(
                "repository traversal failed",
                mcp_error_data("repository_traversal"),
            )
        }
        crate::Error::RuntimeCapabilityUnavailable { .. } => {
            tracing::error!(%error, "repository runtime is unavailable");
            ErrorData::internal_error(
                "repository runtime is unavailable",
                mcp_error_data("runtime_unavailable"),
            )
        }
        crate::Error::IndexNotReady => ErrorData::internal_error(
            "repository index is not ready",
            mcp_error_data("index_not_ready"),
        ),
        crate::Error::RetryableConflict(_) => ErrorData::internal_error(
            "repository operation should be retried",
            mcp_error_data("retryable_conflict"),
        ),
        _ => {
            tracing::error!(%error, "MCP tool failed");
            ErrorData::internal_error(
                "repository retrieval failed",
                mcp_error_data("repository_retrieval"),
            )
        }
    }
}

fn mcp_error_data(category: &'static str) -> Option<serde_json::Value> {
    Some(serde_json::json!({ "category": category }))
}

fn tool_unavailable(message: &'static str) -> CallToolResult {
    CallToolResult::error(vec![ContentBlock::text(message)])
}

/// Return the complete JSON-serialized tool catalog for telemetry and snapshots.
///
/// Catalog size is measured rather than capped: descriptions are part of the
/// model-facing capability contract and require model-use evidence before removal.
pub fn tool_catalog_json() -> String {
    serde_json::to_string(&LeanTokenMcp::tool_router().list_all())
        .expect("tool catalog is serializable")
}

/// Run the MCP server over stdio until the transport closes or SIGINT is received.
pub async fn serve_stdio(services: Arc<Services>, result_mode: McpResultMode) -> crate::Result<()> {
    let server = LeanTokenMcp::new(services).with_result_mode(result_mode);
    serve_stdio_server(server).await
}

/// Run a prepared MCP server over stdio.
pub async fn serve_stdio_server(server: LeanTokenMcp) -> crate::Result<()> {
    let token = CancellationToken::new();

    let signal_task = tokio::spawn({
        let token = token.clone();
        async move {
            let _ = tokio::signal::ctrl_c().await;
            token.cancel();
        }
    });

    let result = async {
        let service = match server.serve_with_ct(stdio(), token.child_token()).await {
            Ok(service) => service,
            Err(
                rmcp::service::ServerInitializeError::ConnectionClosed(_)
                | rmcp::service::ServerInitializeError::ExpectedInitializeRequest(None),
            ) => return Ok(()),
            Err(error) => return Err(crate::Error::Io(std::io::Error::other(error))),
        };
        service.waiting().await?;
        Ok(())
    }
    .await;

    signal_task.abort();
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mcp_exposes_eight_tools() {
        let router = LeanTokenMcp::tool_router();
        let tools = router.list_all();
        assert_eq!(tools.len(), 8);

        let names: std::collections::HashSet<_> = tools.iter().map(|t| t.name.as_ref()).collect();
        for name in [
            "files", "search", "outline", "read", "history", "json", "context", "savings",
        ] {
            assert!(names.contains(name), "missing tool {name}");
        }
    }

    #[test]
    fn user_docs_list_the_exact_runtime_tool_catalog() {
        let expected = LeanTokenMcp::tool_router()
            .list_all()
            .into_iter()
            .map(|tool| format!("leantoken.{}", tool.name))
            .collect::<std::collections::BTreeSet<_>>();

        let readme = include_str!("../README.md");
        let readme_tools = readme
            .split_once("## Available tools")
            .expect("README tool section")
            .1
            .split_once("## CLI usage")
            .expect("README tool section end")
            .0
            .lines()
            .filter_map(|line| line.strip_prefix("| `"))
            .filter_map(|line| line.split_once('`').map(|(name, _)| name.to_owned()))
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(readme_tools, expected, "README tool table drifted");

        let usage_tools = include_str!("../docs/usage.md")
            .lines()
            .filter_map(|line| line.strip_prefix("## `"))
            .filter_map(|line| line.strip_suffix('`'))
            .filter(|name| name.starts_with("leantoken."))
            .map(str::to_owned)
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(usage_tools, expected, "usage guide tool sections drifted");
    }

    #[test]
    fn tools_have_input_schemas_without_redundant_output_schemas() {
        let router = LeanTokenMcp::tool_router();
        let tools = router.list_all();
        for tool in tools {
            assert!(
                !tool.input_schema.is_empty(),
                "{} input_schema is empty",
                tool.name
            );
            assert!(
                tool.output_schema.is_none(),
                "{} output_schema adds catalog tokens despite structured results",
                tool.name
            );
        }
    }

    #[test]
    fn result_modes_emit_only_the_selected_representations() {
        let value = serde_json::json!({"answer": 42});
        let dual = tool_result(value.clone(), McpResultMode::Dual).expect("dual");
        let text = tool_result(value.clone(), McpResultMode::Text).expect("text");
        let structured = tool_result(value, McpResultMode::Structured).expect("structured");

        assert!(!dual.content.is_empty());
        assert!(dual.structured_content.is_some());
        assert!(!text.content.is_empty());
        assert!(text.structured_content.is_none());
        assert!(structured.content.is_empty());
        assert!(structured.structured_content.is_some());
    }

    #[test]
    fn retryable_conflicts_are_successful_structured_results() {
        let (server, _state) = LeanTokenMcp::pending();
        let result = server
            .service_result::<()>(Err(crate::Error::RetryableConflict(
                crate::error::RetryableOperation::Retrieval,
            )))
            .expect("tool result");

        assert_eq!(result.is_error, Some(false));
        let structured = result.structured_content.expect("structured retry result");
        assert_eq!(structured["status"], "retryable");
        assert_eq!(structured["reason"], "repository_changed");
        assert_eq!(structured["retry_after_ms"], 100);
    }

    #[test]
    fn mcp_error_mapping_separates_invalid_input_from_internal_failures() {
        let invalid = into_mcp_error(crate::Error::InputTooLong {
            field: "search query",
            max_bytes: 64,
        });
        assert_eq!(invalid.code, rmcp::model::ErrorCode::INVALID_PARAMS);
        assert_eq!(
            invalid
                .data
                .as_ref()
                .and_then(|data| data["category"].as_str()),
            Some("input_too_long")
        );
        assert_eq!(
            invalid.data.as_ref().map(|data| &data["limit"]),
            Some(&serde_json::json!(64))
        );

        let request_limit = into_mcp_error(crate::Error::RequestLimitExceeded {
            field: "max_tokens",
            requested: 32_001,
            limit: 32_000,
        });
        assert_eq!(request_limit.code, rmcp::model::ErrorCode::INVALID_PARAMS);
        assert_eq!(
            request_limit.data,
            Some(serde_json::json!({
                "category": "request_limit_exceeded",
                "field": "max_tokens",
                "requested": 32_001,
                "limit": 32_000,
            }))
        );

        let stale_receipt = into_mcp_error(crate::Error::StaleReceipt {
            receipt_generation: 4,
            repository_generation: 5,
        });
        assert_eq!(stale_receipt.code, rmcp::model::ErrorCode::INVALID_PARAMS);
        assert_eq!(
            stale_receipt
                .data
                .as_ref()
                .and_then(|data| data["category"].as_str()),
            Some("stale_receipt")
        );

        let internal = [
            crate::Error::InvalidConfiguration("chunk size must be positive".into()),
            crate::Error::InternalFailure("parser returned None".into()),
            crate::Error::RuntimeCapabilityUnavailable {
                capability: "SQLite FTS5",
                source: None,
            },
        ];
        for error in internal {
            assert_eq!(
                into_mcp_error(error).code,
                rmcp::model::ErrorCode::INTERNAL_ERROR
            );
        }
    }

    #[test]
    fn mcp_error_mapping_never_serializes_internal_or_input_paths() {
        let unix_marker = "/home/example/sensitive-marker/external.sqlite";
        let windows_marker = r"C:\Users\example\sensitive-marker\external.sqlite";
        let invalid_regex = ["(?P<", "sensitive-marker", ">"].concat();
        let errors = [
            crate::Error::RootNotFound(unix_marker.into()),
            crate::Error::UnsafeRepositoryRoot(unix_marker.into()),
            crate::Error::PathOutsideRoot(unix_marker.into()),
            crate::Error::PathOutsideRoot(windows_marker.into()),
            crate::Error::NotIndexed(unix_marker.into()),
            crate::Error::SymbolNotFound {
                path: unix_marker.into(),
                symbol: "sensitive-marker".into(),
            },
            crate::Error::HeadingNotFound {
                path: unix_marker.into(),
                heading: "sensitive-marker".into(),
                occurrence: 2,
            },
            crate::Error::UnsupportedLanguage(unix_marker.into()),
            crate::Error::InvalidRequest(format!("invalid path: {unix_marker}")),
            crate::Error::InternalFailure(format!("failed at {unix_marker}")),
            crate::Error::RepositoryMismatch {
                database: windows_marker.into(),
                expected_repository: unix_marker.into(),
                actual_repository: unix_marker.into(),
            },
            crate::Error::Io(std::io::Error::other(format!(
                "permission denied at {unix_marker}"
            ))),
            crate::Error::Sqlite(rusqlite::Error::InvalidPath(windows_marker.into())),
            crate::Error::Regex(regex::Regex::new(&invalid_regex).expect_err("regex")),
            crate::Error::Glob(globset::Glob::new("[sensitive-marker").expect_err("glob")),
        ];

        for error in errors {
            let response = into_mcp_error(error);
            let wire = serde_json::to_string(&response).expect("serialize public error");
            for marker in [
                unix_marker,
                windows_marker,
                "sensitive-marker",
                "external.sqlite",
                "example",
            ] {
                assert!(
                    !wire.contains(marker),
                    "public error leaked {marker}: {wire}"
                );
            }
            assert!(
                response
                    .data
                    .as_ref()
                    .and_then(|data| data["category"].as_str())
                    .is_some(),
                "public error has no stable category: {wire}"
            );
        }
    }

    #[test]
    fn explicit_null_limits_are_not_treated_as_omitted() {
        assert!(
            serde_json::from_value::<FilesMcpRequest>(serde_json::json!({
                "operation": "tree",
                "max_results": null
            }))
            .is_err()
        );
        assert!(
            serde_json::from_value::<SearchMcpRequest>(serde_json::json!({
                "query": "answer",
                "max_results": null
            }))
            .is_err()
        );
        assert!(
            serde_json::from_value::<SearchMcpRequest>(serde_json::json!({
                "query": "answer",
                "max_tokens": null
            }))
            .is_err()
        );
        assert!(
            serde_json::from_value::<SearchMcpRequest>(serde_json::json!({
                "query": "answer",
                "context_lines": null
            }))
            .is_err()
        );
        assert!(
            serde_json::from_value::<OutlineMcpRequest>(serde_json::json!({
                "paths": ["lib.rs"],
                "max_results": null
            }))
            .is_err()
        );
        assert!(
            serde_json::from_value::<OutlineMcpRequest>(serde_json::json!({
                "paths": ["lib.rs"],
                "max_tokens": null
            }))
            .is_err()
        );
        assert!(
            serde_json::from_value::<ReadMcpRequest>(serde_json::json!({
                "path": "lib.rs",
                "target": {"kind": "lines", "start": 1, "end": 1},
                "max_tokens": null
            }))
            .is_err()
        );
        assert!(
            serde_json::from_value::<ContextMcpRequest>(serde_json::json!({
                "task": "find answer",
                "token_budget": null
            }))
            .is_err()
        );
        assert!(
            serde_json::from_value::<ContextMcpRequest>(serde_json::json!({
                "task": "find answer",
                "minimum_fragments_per_focus_path": null
            }))
            .is_err()
        );
    }

    #[test]
    fn omitted_context_budget_uses_the_runtime_default() {
        let request = serde_json::from_value::<ContextMcpRequest>(serde_json::json!({
            "task": "find answer"
        }))
        .expect("context request without a budget");
        let (request, _, _, _, _) = request.into_parts(37);
        assert_eq!(request.token_budget, 37);

        let request = serde_json::from_value::<ContextMcpRequest>(serde_json::json!({
            "task": "find answer",
            "token_budget": 23,
            "focus_paths": ["src/**"],
            "strict_focus_paths": true,
            "minimum_fragments_per_focus_path": 2,
            "changed_paths": ["src/lib.rs"],
            "strict_changed_paths": true
        }))
        .expect("context request with a budget");
        let (request, _, _, _, _) = request.into_parts(37);
        assert_eq!(request.token_budget, 23);
        assert_eq!(request.focus_paths, ["src/**"]);
        assert!(request.strict_focus_paths);
        assert_eq!(request.minimum_fragments_per_focus_path, Some(2));
        assert_eq!(request.changed_paths, ["src/lib.rs"]);
        assert!(request.strict_changed_paths);
    }

    #[test]
    fn context_mcp_maps_bounded_handoff_state() {
        let request = serde_json::from_value::<ContextMcpRequest>(serde_json::json!({
            "task": "continue implementation",
            "handoff": {
                "summary": "executor state",
                "validations": [{
                    "command": "cargo test",
                    "status": "passed",
                    "summary": "all tests passed"
                }],
                "assumptions": ["public API remains stable"],
                "open_questions": ["is another fixture required?"],
                "negative_evidence": ["no alternate owner found"],
                "avoid_rules": ["do not copy source bodies"]
            }
        }))
        .expect("context handoff request");
        let (_, _, _, _, handoff) = request.into_parts(37);
        let handoff = handoff.expect("handoff");
        assert_eq!(handoff.summary.as_deref(), Some("executor state"));
        assert_eq!(handoff.validations.len(), 1);
        assert_eq!(handoff.assumptions, ["public API remains stable"]);

        assert!(
            serde_json::from_value::<ContextMcpRequest>(serde_json::json!({
                "task": "continue implementation",
                "handoff": {"unexpected": true}
            }))
            .is_err()
        );
    }

    #[test]
    fn tool_input_fields_are_documented() {
        for tool in LeanTokenMcp::tool_router().list_all() {
            let properties = tool
                .input_schema
                .get("properties")
                .and_then(serde_json::Value::as_object);
            if tool.name == "savings" {
                assert!(
                    properties.is_none_or(serde_json::Map::is_empty),
                    "savings must remain a zero-input tool"
                );
                continue;
            }
            let properties =
                properties.unwrap_or_else(|| panic!("{} input properties missing", tool.name));
            for (field, schema) in properties {
                assert!(
                    schema
                        .get("description")
                        .and_then(serde_json::Value::as_str)
                        .is_some_and(|description| !description.trim().is_empty()),
                    "{}.{} is missing a schema description",
                    tool.name,
                    field
                );
            }
        }
    }

    #[test]
    fn files_schema_matches_operation_specific_runtime_requirements() {
        let tool = LeanTokenMcp::tool_router()
            .list_all()
            .into_iter()
            .find(|tool| tool.name == "files")
            .expect("files tool");
        let schema = serde_json::Value::Object((*tool.input_schema).clone());
        let variants = schema["oneOf"].as_array().expect("operation variants");
        assert_eq!(variants.len(), 3);
        assert_eq!(variants[0]["properties"]["operation"]["const"], "tree");
        assert_eq!(variants[1]["properties"]["operation"]["const"], "find");
        assert_eq!(variants[1]["properties"]["query"]["type"], "string");
        assert_eq!(variants[1]["required"], serde_json::json!(["query"]));
        assert_eq!(variants[2]["properties"]["operation"]["const"], "glob");
        assert_eq!(variants[2]["properties"]["pattern"]["type"], "string");
        assert_eq!(variants[2]["required"], serde_json::json!(["pattern"]));
        assert_eq!(schema["properties"]["query"]["minLength"], 1);
        assert_eq!(schema["properties"]["pattern"]["minLength"], 1);
    }

    #[test]
    fn retrieval_tools_expose_consistency_boundary() {
        for tool in LeanTokenMcp::tool_router()
            .list_all()
            .into_iter()
            .filter(|tool| tool.name != "savings" && tool.name != "history" && tool.name != "json")
        {
            let consistency = tool
                .input_schema
                .get("properties")
                .and_then(serde_json::Value::as_object)
                .and_then(|properties| properties.get("consistency"))
                .unwrap_or_else(|| panic!("{} consistency schema missing", tool.name));
            assert_eq!(
                consistency.get("default"),
                Some(&serde_json::json!("indexed_generation"))
            );
            assert_eq!(
                consistency.get("enum"),
                Some(&serde_json::json!([
                    "indexed_generation",
                    "reconcile_working_tree"
                ]))
            );
            assert!(
                consistency
                    .get("description")
                    .and_then(serde_json::Value::as_str)
                    .is_some_and(|description| {
                        description.contains("reconcile_working_tree")
                            && description.contains("edits")
                    }),
                "{}.consistency must tell agents when to synchronize",
                tool.name
            );
        }
        let history = LeanTokenMcp::tool_router()
            .list_all()
            .into_iter()
            .find(|tool| tool.name == "history")
            .expect("history tool");
        assert!(
            history
                .input_schema
                .get("properties")
                .and_then(serde_json::Value::as_object)
                .is_none_or(|properties| !properties.contains_key("consistency"))
        );
    }

    #[test]
    fn tool_descriptions_route_native_discovery_workflows() {
        let descriptions = LeanTokenMcp::tool_router()
            .list_all()
            .into_iter()
            .map(|tool| {
                (
                    tool.name.into_owned(),
                    tool.description.expect("tool description").into_owned(),
                )
            })
            .collect::<std::collections::HashMap<_, _>>();
        assert!(descriptions["files"].contains("instead of find"));
        assert!(descriptions["search"].contains("instead of grep or rg"));
        assert!(descriptions["outline"].contains("without reading whole source files"));
        assert!(descriptions["read"].contains("expected_hash"));
        assert!(descriptions["read"].contains("instead of cat"));
        assert!(descriptions["context"].contains("DEFAULT FIRST CALL"));
        assert!(descriptions["savings"].contains("how many tokens LeanToken saved"));
        assert!(
            descriptions
                .values()
                .all(|description| description.contains("Example:"))
        );
    }

    #[test]
    fn savings_tool_is_local_and_read_only() {
        let tool = LeanTokenMcp::tool_router()
            .list_all()
            .into_iter()
            .find(|tool| tool.name == "savings")
            .expect("savings tool");
        let annotations = tool.annotations.expect("savings annotations");
        assert_eq!(annotations.read_only_hint, Some(true));
        assert_eq!(annotations.open_world_hint, Some(false));
    }

    #[test]
    fn tool_schemas_are_closed_bounded_and_remove_ambiguous_inputs() {
        let tools = LeanTokenMcp::tool_router()
            .list_all()
            .into_iter()
            .map(|tool| {
                (
                    tool.name.into_owned(),
                    serde_json::Value::Object((*tool.input_schema).clone()),
                )
            })
            .collect::<std::collections::HashMap<_, _>>();

        for (name, schema) in &tools {
            assert_eq!(
                schema.get("additionalProperties"),
                Some(&serde_json::json!(false)),
                "{name} must reject unknown arguments"
            );
        }
        assert_eq!(
            tools["context"].pointer("/properties/token_budget/default"),
            Some(&serde_json::json!(3_000))
        );
        assert!(tools["files"].pointer("/properties/query").is_some());
        assert!(tools["files"].pointer("/properties/pattern").is_some());
        assert!(tools["read"].pointer("/properties/symbol").is_none());
        assert!(tools["read"].pointer("/properties/start_line").is_none());
        assert!(tools["read"].pointer("/properties/target").is_some());

        let request = serde_json::from_value::<FilesMcpRequest>(serde_json::json!({
            "operation": "find",
            "query": "mcp",
            "pattern": "*.rs"
        }))
        .expect("flat files request shape");
        assert!(request.validate_limits(McpLimitPolicy::DEFAULT).is_err());
        assert!(
            serde_json::from_value::<ReadMcpRequest>(serde_json::json!({
                "path": "src/mcp.rs",
                "target": {"kind": "symbol", "name": "LeanTokenMcp", "start": 1}
            }))
            .is_err()
        );
        for target in [
            serde_json::json!({"kind": "range", "start": 10, "end": 20}),
            serde_json::json!({"kind": "line_range", "start_line": 10, "end_line": 20}),
        ] {
            let request = serde_json::from_value::<ReadMcpRequest>(serde_json::json!({
                "path": "src/mcp.rs",
                "target": target
            }))
            .expect("common line-range aliases should remain readable");
            let (request, _, _) = request.into_parts();
            assert_eq!(request.start_line, Some(10));
            assert_eq!(request.end_line, Some(20));
        }
        let heading = serde_json::from_value::<ReadMcpRequest>(serde_json::json!({
            "path": "README.md",
            "target": {"kind": "heading", "name": "Installation", "occurrence": 2}
        }))
        .expect("Markdown heading target");
        assert!(heading.validate_limits(McpLimitPolicy::DEFAULT).is_ok());
        let (heading, _, _) = heading.into_parts();
        assert_eq!(heading.heading.as_deref(), Some("Installation"));
        assert_eq!(heading.heading_occurrence, Some(2));
        assert!(heading.symbol.is_none());
        let invalid_heading = serde_json::from_value::<ReadMcpRequest>(serde_json::json!({
            "path": "README.md",
            "target": {"kind": "heading", "name": "Installation", "occurrence": 0}
        }))
        .expect("schema validation remains a runtime boundary");
        assert!(
            invalid_heading
                .validate_limits(McpLimitPolicy::DEFAULT)
                .is_err()
        );
        let continuation = serde_json::from_value::<ReadMcpRequest>(serde_json::json!({
            "path": "src/mcp.rs",
            "target": {"kind": "continuation", "cursor": "opaque"}
        }))
        .expect("continuation target");
        let (continuation, _, _) = continuation.into_parts();
        assert_eq!(continuation.continuation_cursor.as_deref(), Some("opaque"));
        assert!(continuation.symbol.is_none());
        assert!(continuation.heading.is_none());
        assert!(continuation.heading_occurrence.is_none());
        assert!(continuation.start_line.is_none());
        assert!(continuation.end_line.is_none());
    }

    #[test]
    fn receipt_id_maps_to_the_service_request() {
        let request = serde_json::from_value::<ReadMcpRequest>(serde_json::json!({
            "path": "README.md",
            "receipt_id": "r0000000000000001",
            "target": {"kind": "lines", "start": 1, "end": 2}
        }))
        .expect("read request with receipt");
        let (request, _, _) = request.into_parts();
        assert_eq!(request.receipt_id.as_deref(), Some("r0000000000000001"));
    }

    #[test]
    fn history_operation_maps_to_the_service_request() {
        let request = serde_json::from_value::<HistoryMcpRequest>(serde_json::json!({
            "operation": {
                "kind": "diff_symbol",
                "path": "src/lib.rs",
                "symbol": "Services",
                "base_revision": "main~1",
                "head_revision": "main"
            },
            "max_tokens": 500
        }))
        .expect("history request");
        request
            .validate_limits(McpLimitPolicy::DEFAULT)
            .expect("history limits");
        let (request, _) = request.into_parts();
        assert_eq!(request.max_tokens, Some(500));
        assert!(matches!(
            request.operation,
            HistoryOperation::DiffSymbol {
                path,
                symbol,
                base_revision,
                head_revision,
            } if path == "src/lib.rs"
                && symbol == "Services"
                && base_revision == "main~1"
                && head_revision == "main"
        ));
    }

    #[test]
    fn json_operation_maps_to_the_service_request() {
        let request = serde_json::from_value::<JsonMcpRequest>(serde_json::json!({
            "operation": {
                "kind": "numeric_summary",
                "path": "artifacts/results.json",
                "selector": {
                    "kind": "jmespath",
                    "expression": "runs[].score"
                }
            },
            "max_items": 500
        }))
        .expect("JSON request");
        request
            .validate_limits(McpLimitPolicy::DEFAULT)
            .expect("JSON limits");
        let (request, _) = request.into_parts();
        assert_eq!(request.max_items, Some(500));
        assert!(matches!(
            request.operation,
            JsonOperation::NumericSummary {
                path,
                selector: Some(JsonSelector::Jmespath { expression }),
            } if path == "artifacts/results.json" && expression == "runs[].score"
        ));
    }

    #[test]
    fn tool_catalog_schema_snapshot() {
        let tools = LeanTokenMcp::tool_router().list_all();
        insta::assert_json_snapshot!("mcp_tool_catalog", tools);
    }

    #[test]
    fn outline_cursor_maps_to_the_service_request() {
        let request = serde_json::from_value::<OutlineMcpRequest>(serde_json::json!({
            "paths": ["src/lib.rs"],
            "cursor": "12:outline:34:0000000000000000"
        }))
        .expect("outline request");
        let (request, _, _) = request.into_parts();

        assert_eq!(
            request.cursor.as_deref(),
            Some("12:outline:34:0000000000000000")
        );
    }
}
