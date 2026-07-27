use std::sync::{
    Arc, RwLock,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant};

use rmcp::{
    ErrorData, RoleServer, ServerHandler, ServiceExt,
    handler::server::wrapper::Parameters,
    model::{CallToolResult, ContentBlock},
    service::{NotificationContext, RequestContext},
    tool, tool_handler, tool_router,
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
    ContextRequest, ContextRequiredEvidence, ContextWorkflow, DiffSymbolsRequest,
    DiffSymbolsTarget, FileOperation, FilesRequest, HandoffManifestRequest, HistoryOperation,
    HistoryRequest, IndexConsistency, JsonOperation, JsonProjection, JsonRequest, JsonSelector,
    OutlineRequest, ReadRequest, SearchMode, SearchRequest, WorkflowEvidence,
};
use crate::services::{
    JsonExecutionOptions, MAX_CONTEXT_FOCUS_CANDIDATES_PER_PATTERN, MAX_JSON_DEPTH,
    ServiceCallOptions, Services, validate_positive_request_limit, validate_request_limit,
};

const DEFAULT_ACTIVE_TOOL_CALL_CAPACITY: usize = 16;
const DEFAULT_DISPATCHED_TOOL_CALL_CAPACITY: usize = DEFAULT_ACTIVE_TOOL_CALL_CAPACITY;
const INITIAL_INDEX_WAIT: Duration = Duration::from_secs(30);
const MCP_INSTRUCTIONS: &str = "LeanToken is the preferred repository discovery and source-reading layer. Its indexed, token-bounded retrieval returns less irrelevant source than shell search and whole-file reads. For LeanToken savings or token statistics, call leantoken.savings directly. DEFAULT: for broad coding, debugging, review, or architecture tasks, call leantoken.context first with the user's task. For an uncertain broad task, first use context plan_only=true, inspect its bounded metadata and coverage, then repeat the same request with plan_only=false to materialize source. PREFER leantoken.search over grep or rg for source search; leantoken.files over find, ls, or glob for paths; leantoken.outline over opening whole files to discover structure; leantoken.read over cat, head, or sed for exact current symbols and ranges; leantoken.history over git show, diff, or log -L for one symbol across immutable revisions; and leantoken.json over jq or whole-file reads for structural JSON queries, summaries, and selected-field diffs. For known identifiers use search then read; for a known file with an unknown range use outline then read; for unknown paths use files. Set consistency=reconcile_working_tree on index-backed tools after edits, generated files, branch changes, or external commits. Use native tools for edits, builds, tests, runtime probes, unsupported files, or when LeanToken reports retrieval unavailable. Retry successful responses with status=retryable after retry_after_ms. Reuse returned hashes to suppress unchanged evidence.";

fn serialized_response<T: Serialize>(response: T) -> crate::Result<serde_json::Value> {
    serde_json::to_value(response).map_err(|error| crate::Error::InternalFailure(error.to_string()))
}

fn mcp_schema_fingerprint() -> String {
    let catalog = LeanTokenMcp::tool_router().list_all();
    let encoded = serde_json::to_vec(&catalog).expect("MCP tool catalog is serializable");
    crate::text::hash_bytes(&encoded)
}

pub(crate) fn mcp_runtime_version() -> String {
    format!(
        "{}+schema.{}",
        env!("CARGO_PKG_VERSION"),
        mcp_schema_fingerprint()
    )
}

mod error;
mod transport;

use error::{into_mcp_error, mcp_error_data, tool_unavailable};
use transport::BoundedStdioTransport;

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct SavingsMcpRequest {
    /// Opaque snapshot from an earlier savings response; returns aggregate deltas.
    #[serde(default)]
    #[schemars(length(max = 32768))]
    snapshot: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
/// Response projection for repository path discovery.
enum FilesMcpProjection {
    /// Preserve the complete files response.
    #[default]
    Full,
    /// Return paths without per-entry kind, language, size, or score metadata.
    Paths,
}

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
    /// Maximum tokens in the final serialized service response.
    #[serde(default)]
    #[schemars(schema_with = "response_token_limit_schema")]
    max_response_tokens: Option<usize>,
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
    /// Response shape: `full` entries (default) or ordered `paths` only.
    #[serde(default)]
    projection: FilesMcpProjection,
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

#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
/// Response projection for indexed search.
enum SearchMcpProjection {
    /// Select `occurrences` for exhaustive lexical search and `full` otherwise.
    #[default]
    Auto,
    /// Preserve the complete ranked-hit response.
    Full,
    /// Group the selected page into symbol or file summaries.
    Grouped,
    /// Share each exhaustive lexical excerpt across its exact occurrence coordinates.
    Occurrences,
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
    /// Maximum tokens in the final serialized service response.
    #[serde(default)]
    #[schemars(schema_with = "response_token_limit_schema")]
    max_response_tokens: Option<usize>,
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
    /// Omit excerpts and hashes from an exhaustive occurrence response.
    #[serde(default)]
    coordinates_only: bool,
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
    /// Response shape. Exhaustive searches default to `occurrences`; others default to `full`.
    #[serde(default)]
    projection: SearchMcpProjection,
}

impl SearchMcpRequest {
    fn validate_limits(&self, limits: McpLimitPolicy) -> crate::Result<()> {
        validate_optional_positive_limit("max_results", self.max_results, limits.max_results)?;
        validate_optional_positive_limit("max_tokens", self.max_tokens, limits.max_output_tokens)?;
        validate_optional_positive_limit(
            "max_response_tokens",
            self.max_response_tokens,
            limits.max_response_tokens,
        )?;
        validate_optional_limit(
            "context_lines",
            self.context_lines,
            limits.max_context_lines,
        )?;
        if self.coordinates_only && !self.all_occurrences {
            return Err(crate::Error::InvalidInput {
                field: "coordinates_only",
                reason: "requires all_occurrences=true",
            });
        }
        if self.coordinates_only
            && !matches!(
                self.projection,
                SearchMcpProjection::Auto | SearchMcpProjection::Occurrences
            )
        {
            return Err(crate::Error::InvalidInput {
                field: "coordinates_only",
                reason: "requires the occurrences projection",
            });
        }
        if self.projection == SearchMcpProjection::Occurrences && !self.all_occurrences {
            return Err(crate::Error::InvalidInput {
                field: "projection",
                reason: "occurrences requires all_occurrences=true",
            });
        }
        Ok(())
    }

    fn into_parts(
        self,
    ) -> (
        SearchRequest,
        SearchMcpProjection,
        bool,
        IndexConsistency,
        ServiceCallOptions,
        Option<String>,
    ) {
        let projection = match self.projection {
            SearchMcpProjection::Auto if self.all_occurrences => SearchMcpProjection::Occurrences,
            SearchMcpProjection::Auto => SearchMcpProjection::Full,
            projection => projection,
        };
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
            projection,
            self.coordinates_only,
            self.consistency,
            service_call_options(self.max_response_tokens),
            self.expected_repository_id,
        )
    }
}

#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
/// Response projection for structural outlines.
enum OutlineMcpProjection {
    /// Preserve symbols, imports, and byte offsets.
    #[default]
    Full,
    /// Return symbol signatures and line ranges without imports or byte offsets.
    Signatures,
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
    /// Maximum tokens in the final serialized service response.
    #[serde(default)]
    #[schemars(schema_with = "response_token_limit_schema")]
    max_response_tokens: Option<usize>,
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
    /// Response shape: `full` definitions (default) or compact `signatures`.
    #[serde(default)]
    projection: OutlineMcpProjection,
}

impl OutlineMcpRequest {
    fn validate_limits(&self, limits: McpLimitPolicy) -> crate::Result<()> {
        validate_optional_positive_limit("max_results", self.max_results, limits.max_results)?;
        validate_optional_positive_limit("max_tokens", self.max_tokens, limits.max_output_tokens)?;
        validate_optional_positive_limit(
            "max_response_tokens",
            self.max_response_tokens,
            limits.max_response_tokens,
        )
    }

    fn into_parts(
        self,
    ) -> (
        OutlineRequest,
        OutlineMcpProjection,
        IndexConsistency,
        ServiceCallOptions,
        Option<String>,
    ) {
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
            self.projection,
            self.consistency,
            service_call_options(self.max_response_tokens),
            self.expected_repository_id,
        )
    }
}

impl FilesMcpRequest {
    fn validate_limits(&self, limits: McpLimitPolicy) -> crate::Result<()> {
        validate_optional_positive_limit("max_results", self.max_results, limits.max_results)?;
        validate_optional_positive_limit(
            "max_response_tokens",
            self.max_response_tokens,
            limits.max_response_tokens,
        )?;
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

    fn into_parts(
        self,
    ) -> (
        FilesRequest,
        FilesMcpProjection,
        IndexConsistency,
        ServiceCallOptions,
        Option<String>,
    ) {
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
            self.projection,
            self.consistency,
            service_call_options(self.max_response_tokens),
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
    /// Exact symbol, document heading, line range, or continuation to read.
    target: ReadMcpTarget,
    /// Maximum source tokens to return (default 8000, maximum 32000).
    #[serde(default, deserialize_with = "deserialize_optional_limit")]
    #[schemars(schema_with = "token_limit_schema", default = "default_token_option")]
    max_tokens: Option<usize>,
    /// Maximum tokens in the final serialized service response.
    #[serde(default)]
    #[schemars(schema_with = "response_token_limit_schema")]
    max_response_tokens: Option<usize>,
    /// Hash from the same prior target; matching content returns `not_modified`.
    #[serde(default)]
    #[schemars(schema_with = "expected_repository_id_schema")]
    expected_hash: Option<String>,
    /// Record this target and prefer a cheaper follow-up. Without `expected_hash`,
    /// select the latest compatible base for this exact target.
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
    /// Read one indexed Markdown or LaTeX section by exact title or outline signature.
    Heading {
        /// Exact title or outline signature such as `## Performance` or `\section{Method}`.
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
        validate_optional_positive_limit(
            "max_response_tokens",
            self.max_response_tokens,
            limits.max_response_tokens,
        )?;
        if matches!(self.target, ReadMcpTarget::Heading { occurrence: 0, .. }) {
            return Err(crate::Error::InvalidInput {
                field: "heading occurrence",
                reason: "must be one-based",
            });
        }
        Ok(())
    }

    fn into_parts(
        self,
    ) -> (
        ReadRequest,
        IndexConsistency,
        ServiceCallOptions,
        Option<String>,
    ) {
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
            service_call_options(self.max_response_tokens),
            self.expected_repository_id,
        )
    }
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct HistoryMcpRequest {
    /// Expected opaque repository identity from an earlier response.
    #[serde(default)]
    #[schemars(schema_with = "expected_repository_id_schema")]
    expected_repository_id: Option<String>,
    /// Git-backed symbol history operation.
    operation: HistoryMcpOperation,
    /// Maximum results (default 20): 32 for `diff_symbols`, 100 for `symbol_log`.
    #[serde(default, deserialize_with = "deserialize_optional_limit")]
    #[schemars(schema_with = "result_limit_schema", default = "default_result_option")]
    max_results: Option<usize>,
    /// Maximum source or diff tokens to return (default 8000, maximum 32000).
    #[serde(default, deserialize_with = "deserialize_optional_limit")]
    #[schemars(schema_with = "token_limit_schema", default = "default_token_option")]
    max_tokens: Option<usize>,
    /// Maximum tokens in the final serialized service response.
    #[serde(default)]
    #[schemars(schema_with = "response_token_limit_schema")]
    max_response_tokens: Option<usize>,
    /// Opaque cursor returned by `diff_symbols`; reuse the exact operation.
    #[serde(default)]
    #[schemars(length(min = 1, max = 128))]
    cursor: Option<String>,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
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
    /// Diff an ordered symbol set with shared revisions, metadata, and bounded Git work.
    DiffSymbols {
        #[schemars(length(min = 1, max = "crate::services::MAX_DIFF_SYMBOL_TARGETS"))]
        targets: Vec<HistoryMcpTarget>,
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

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct HistoryMcpTarget {
    #[schemars(length(min = 1, max = 4096))]
    path: String,
    #[schemars(length(min = 1, max = 4096))]
    symbol: String,
    #[serde(default)]
    #[schemars(length(min = 1, max = 4096))]
    head_path: Option<String>,
    #[serde(default)]
    #[schemars(length(min = 1, max = 4096))]
    head_symbol: Option<String>,
}

#[derive(Debug, Clone)]
enum HistoryMcpCall {
    Single(HistoryRequest),
    DiffSymbols(DiffSymbolsRequest),
}

impl HistoryMcpRequest {
    fn validate_limits(&self, limits: McpLimitPolicy) -> crate::Result<()> {
        validate_optional_positive_limit("max_results", self.max_results, MAX_RESULTS)?;
        validate_optional_positive_limit("max_tokens", self.max_tokens, limits.max_output_tokens)?;
        validate_optional_positive_limit(
            "max_response_tokens",
            self.max_response_tokens,
            limits.max_response_tokens,
        )
    }

    fn into_parts(self) -> crate::Result<(HistoryMcpCall, ServiceCallOptions, Option<String>)> {
        let has_cursor = self.cursor.is_some();
        let cursor = self.cursor.clone();
        let call = match self.operation {
            HistoryMcpOperation::ReadSymbol {
                path,
                symbol,
                revision,
            } => HistoryMcpCall::Single(HistoryRequest {
                operation: HistoryOperation::ReadSymbol {
                    path,
                    symbol,
                    revision,
                },
                max_results: self.max_results,
                max_tokens: self.max_tokens,
            }),
            HistoryMcpOperation::DiffSymbol {
                path,
                symbol,
                base_revision,
                head_revision,
            } => HistoryMcpCall::Single(HistoryRequest {
                operation: HistoryOperation::DiffSymbol {
                    path,
                    symbol,
                    base_revision,
                    head_revision,
                },
                max_results: self.max_results,
                max_tokens: self.max_tokens,
            }),
            HistoryMcpOperation::DiffSymbols {
                targets,
                base_revision,
                head_revision,
            } => HistoryMcpCall::DiffSymbols(DiffSymbolsRequest {
                targets: targets
                    .into_iter()
                    .map(|target| DiffSymbolsTarget {
                        path: target.path,
                        symbol: target.symbol,
                        head_path: target.head_path,
                        head_symbol: target.head_symbol,
                    })
                    .collect(),
                base_revision,
                head_revision,
                max_results: self.max_results,
                max_tokens: self.max_tokens,
                cursor,
            }),
            HistoryMcpOperation::SymbolLog {
                path,
                symbol,
                revision,
            } => HistoryMcpCall::Single(HistoryRequest {
                operation: HistoryOperation::SymbolLog {
                    path,
                    symbol,
                    revision,
                },
                max_results: self.max_results,
                max_tokens: self.max_tokens,
            }),
        };
        if has_cursor && matches!(call, HistoryMcpCall::Single(_)) {
            return Err(crate::Error::InvalidInput {
                field: "cursor",
                reason: "is only valid for diff_symbols",
            });
        }
        Ok((
            call,
            service_call_options(self.max_response_tokens),
            self.expected_repository_id,
        ))
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
    /// Maximum tokens in the final serialized service response.
    #[serde(default)]
    #[schemars(schema_with = "response_token_limit_schema")]
    max_response_tokens: Option<usize>,
    /// Maximum structural items returned (default 1000, maximum 10000).
    #[serde(default, deserialize_with = "deserialize_optional_limit")]
    #[schemars(range(min = 1, max = 10000))]
    max_items: Option<usize>,
    /// Array elements sampled by collapsed projections (default 3, maximum 20).
    #[serde(default)]
    #[schemars(range(min = 0, max = 20))]
    array_sample_size: Option<usize>,
    /// Maximum keys traversal depth relative to the selected root (root is zero).
    #[serde(default)]
    #[schemars(range(min = 0, max = 64))]
    depth: Option<usize>,
    /// Opaque cursor returned by an incomplete keys projection.
    #[serde(default)]
    #[schemars(length(max = 256))]
    cursor: Option<String>,
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
        validate_optional_positive_limit(
            "max_response_tokens",
            self.max_response_tokens,
            limits.max_response_tokens,
        )?;
        validate_optional_positive_limit("max_items", self.max_items, 10_000)?;
        if self.array_sample_size.is_some_and(|value| value > 20) {
            return Err(crate::Error::RequestLimitExceeded {
                field: "array_sample_size",
                requested: self.array_sample_size.unwrap_or_default(),
                limit: 20,
            });
        }
        if self.depth.is_some_and(|value| value > MAX_JSON_DEPTH) {
            return Err(crate::Error::RequestLimitExceeded {
                field: "depth",
                requested: self.depth.unwrap_or_default(),
                limit: MAX_JSON_DEPTH,
            });
        }
        Ok(())
    }

    fn into_parts(
        self,
    ) -> (
        JsonRequest,
        ServiceCallOptions,
        JsonExecutionOptions,
        Option<String>,
    ) {
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
        let execution = JsonExecutionOptions::mcp(self.depth);
        (
            JsonRequest {
                operation,
                max_tokens: self.max_tokens,
                max_items: self.max_items,
                array_sample_size: self.array_sample_size,
                cursor: self.cursor,
            },
            service_call_options(self.max_response_tokens),
            execution,
            self.expected_repository_id,
        )
    }
}

fn service_call_options(max_response_tokens: Option<usize>) -> ServiceCallOptions {
    max_response_tokens.map_or_else(ServiceCallOptions::new, |limit| {
        ServiceCallOptions::new().with_max_response_tokens(limit)
    })
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
    /// Typed caller-observed failure traces, symbols, paths, and test intent.
    #[serde(default)]
    workflow_evidence: WorkflowEvidence,
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
    /// Maximum tokens in the final serialized service response.
    #[serde(default)]
    #[schemars(schema_with = "response_token_limit_schema")]
    max_response_tokens: Option<usize>,
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
    /// Require matching query evidence within each path-scoped contract.
    #[serde(default)]
    #[schemars(length(max = 32))]
    required_evidence: Vec<ContextRequiredEvidence>,
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
    #[schemars(length(max = 32), inner(length(max = 4096)))]
    focus_paths: Vec<String>,
    /// Require every returned fragment to match at least one focus path.
    #[serde(default)]
    strict_focus_paths: bool,
    /// Minimum returned fragments required per focus path (maximum 8).
    #[serde(default, deserialize_with = "deserialize_optional_limit")]
    #[schemars(schema_with = "context_focus_fragment_limit_schema")]
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
    /// Include full path, file-type, reason, score-band, focus, and change omission facets.
    #[serde(default)]
    verbose_diagnostics: bool,
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
        validate_optional_positive_limit(
            "max_response_tokens",
            self.max_response_tokens,
            limits.max_response_tokens,
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
        WorkflowEvidence,
        IndexConsistency,
        ServiceCallOptions,
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
                required_evidence: self.required_evidence,
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
                verbose_diagnostics: self.verbose_diagnostics,
            },
            self.workflow,
            self.workflow_evidence,
            self.consistency,
            self.max_response_tokens
                .map_or_else(ServiceCallOptions::new, |limit| {
                    ServiceCallOptions::new().with_max_response_tokens(limit)
                }),
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

fn context_focus_fragment_limit_schema(_: &mut SchemaGenerator) -> Schema {
    schemars::json_schema!({
        "type": "integer",
        "minimum": 1,
        "maximum": MAX_CONTEXT_FOCUS_CANDIDATES_PER_PATTERN,
        "default": null
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

fn response_token_limit_schema(_: &mut SchemaGenerator) -> Schema {
    schemars::json_schema!({
        "type": "integer",
        "format": "uint",
        "minimum": 1,
        "maximum": MAX_OUTPUT_TOKENS
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
    request_admission: RequestAdmission,
    request_dispatch: RequestAdmission,
}

#[derive(Debug, Clone)]
struct RequestAdmission {
    active: Arc<tokio::sync::Semaphore>,
}

impl RequestAdmission {
    fn new(active_capacity: usize) -> Self {
        Self {
            active: Arc::new(tokio::sync::Semaphore::new(active_capacity)),
        }
    }

    fn try_admit(&self) -> crate::Result<tokio::sync::OwnedSemaphorePermit> {
        Arc::clone(&self.active)
            .try_acquire_owned()
            .map_err(|_| crate::Error::RetrievalOverloaded)
    }

    #[cfg(test)]
    fn available_permits(&self) -> usize {
        self.active.available_permits()
    }
}

#[derive(Debug, Clone, Copy)]
struct McpLimitPolicy {
    max_results: usize,
    max_output_tokens: usize,
    max_response_tokens: usize,
    max_context_lines: usize,
    default_context_tokens: usize,
}

impl McpLimitPolicy {
    const DEFAULT: Self = Self {
        max_results: MAX_RESULTS,
        max_output_tokens: MAX_OUTPUT_TOKENS,
        max_response_tokens: MAX_OUTPUT_TOKENS,
        max_context_lines: MAX_CONTEXT_LINES,
        default_context_tokens: DEFAULT_CONTEXT_TOKENS,
    };

    fn from_config(config: &Config) -> crate::Result<Self> {
        config.validate()?;
        Ok(Self {
            max_results: config.max_results,
            max_output_tokens: config.max_output_tokens,
            max_response_tokens: MAX_OUTPUT_TOKENS,
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
    Failed {
        limits: McpLimitPolicy,
        failure: StartupFailure,
    },
}

struct PreparedRetrievalCall {
    services: Arc<Services>,
    limits: McpLimitPolicy,
    cancellation: CancellationToken,
    deadline: tokio::time::Instant,
}

enum RetrievalPreparation {
    Ready(PreparedRetrievalCall),
    Unavailable(CallToolResult),
}

impl McpServiceState {
    const fn limits(&self) -> McpLimitPolicy {
        match self {
            Self::Starting(limits) | Self::Ready { limits, .. } | Self::Failed { limits, .. } => {
                *limits
            }
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct StartupFailure {
    reason: &'static str,
    message: &'static str,
}

impl StartupFailure {
    fn from_error(error: &crate::Error) -> Self {
        match error {
            crate::Error::UnsafeRepositoryRoot(_) => Self {
                reason: "unsafe_repository_root",
                message: "repository index is unavailable because the root is too broad; start LeanToken from a repository root or explicitly allow the broad root",
            },
            crate::Error::RootNotFound(_) => Self {
                reason: "repository_root_not_found",
                message: "repository index is unavailable because the root does not exist; start LeanToken from an existing repository root",
            },
            crate::Error::IndexLimitExceeded { .. } => Self {
                reason: "repository_index_limit",
                message: "repository index is unavailable because a discovery limit was exceeded; narrow the root or adjust the configured discovery limits",
            },
            crate::Error::InvalidConfiguration(_) => Self {
                reason: "invalid_configuration",
                message: "repository index is unavailable because its configuration is invalid; review the server configuration",
            },
            _ => Self {
                reason: "index_startup_failed",
                message: "repository index is unavailable because startup failed; check bounded server diagnostics and retry",
            },
        }
    }
}

/// Shared readiness handle used by handshake-first MCP startup.
#[derive(Debug, Clone)]
pub struct McpServices {
    state: Arc<RwLock<McpServiceState>>,
    state_changed: Arc<tokio::sync::Notify>,
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
            request_admission: RequestAdmission::new(DEFAULT_ACTIVE_TOOL_CALL_CAPACITY),
            request_dispatch: RequestAdmission::new(DEFAULT_DISPATCHED_TOOL_CALL_CAPACITY),
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
                request_admission: RequestAdmission::new(DEFAULT_ACTIVE_TOOL_CALL_CAPACITY),
                request_dispatch: RequestAdmission::new(DEFAULT_DISPATCHED_TOOL_CALL_CAPACITY),
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
            McpServiceState::Failed { failure, .. } => {
                Err(tool_unavailable(failure.reason, failure.message))
            }
        }
    }

    fn retryable_result(&self, response: RetryableToolResponse) -> CallToolResult {
        retryable_tool_result(response, self.result_mode)
    }

    async fn prepare_retrieval_call(
        &self,
        cancellation: CancellationToken,
        validate: impl Fn(McpLimitPolicy) -> crate::Result<()>,
    ) -> Result<RetrievalPreparation, ErrorData> {
        let deadline = tokio::time::Instant::now() + INITIAL_INDEX_WAIT;
        let state = self.services.get();
        validate(state.limits()).map_err(into_mcp_error)?;
        let state = self
            .services
            .wait_for_services(state, cancellation.clone(), deadline)
            .await
            .map_err(into_mcp_error)?;
        let limits = state.limits();
        validate(limits).map_err(into_mcp_error)?;
        let services = match self.services(&state) {
            Ok(services) => services,
            Err(result) => return Ok(RetrievalPreparation::Unavailable(result)),
        };
        Ok(RetrievalPreparation::Ready(PreparedRetrievalCall {
            services,
            limits,
            cancellation,
            deadline,
        }))
    }

    async fn run_prepared<T, F, Fut>(
        &self,
        tool: &'static str,
        prepared: PreparedRetrievalCall,
        expected_repository_id: Option<String>,
        mut operation: F,
    ) -> Result<CallToolResult, ErrorData>
    where
        T: Serialize,
        F: FnMut(Arc<Services>, CancellationToken) -> Fut,
        Fut: Future<Output = crate::Result<T>>,
    {
        let PreparedRetrievalCall {
            services,
            cancellation,
            deadline,
            ..
        } = prepared;
        let mcp_services = self.services.clone();
        self.run_admitted(
            services,
            expected_repository_id,
            move |services| async move {
                retry_after_initial_index(
                    tool,
                    &mcp_services,
                    &services,
                    cancellation.clone(),
                    deadline,
                    || operation(Arc::clone(&services), cancellation.clone()),
                )
                .await
            },
        )
        .await
    }

    fn service_result<T: Serialize>(
        &self,
        result: crate::Result<T>,
    ) -> Result<CallToolResult, ErrorData> {
        match result {
            Ok(value) => self.result(value),
            Err(error) if matches!(error.reconciliation_cause(), crate::Error::IndexNotReady) => {
                Ok(self.retryable_result(RetryableToolResponse::new(
                    "index_building",
                    "repository index is being built; retry the same call shortly",
                    500,
                )))
            }
            Err(error)
                if matches!(
                    error.reconciliation_cause(),
                    crate::Error::RetryableConflict(_)
                ) =>
            {
                Ok(self.retryable_result(RetryableToolResponse::new(
                    "repository_changed",
                    "repository index changed during retrieval; retry the same call",
                    100,
                )))
            }
            Err(error)
                if matches!(
                    error.reconciliation_cause(),
                    crate::Error::RetrievalOverloaded
                ) =>
            {
                Ok(self.retryable_result(RetryableToolResponse::new(
                    "retrieval_capacity_exhausted",
                    "repository tool-call capacity is exhausted; retry shortly",
                    500,
                )))
            }
            Err(error)
                if matches!(
                    error.reconciliation_cause(),
                    crate::Error::RetrievalQueueTimeout
                ) =>
            {
                Ok(self.retryable_result(RetryableToolResponse::new(
                    "retrieval_queue_timeout",
                    "repository retrieval did not obtain execution capacity in time; retry shortly",
                    500,
                )))
            }
            Err(crate::Error::McpRuntimeStopped) => Ok(tool_unavailable(
                "index_runtime_stopped",
                "repository index is unavailable; check server logs and retry",
            )),
            Err(error) => Err(into_mcp_error(error)),
        }
    }

    async fn run_admitted<T, F, Fut>(
        &self,
        services: Arc<Services>,
        expected_repository_id: Option<String>,
        operation: F,
    ) -> Result<CallToolResult, ErrorData>
    where
        T: Serialize,
        F: FnOnce(Arc<Services>) -> Fut,
        Fut: Future<Output = crate::Result<T>>,
    {
        services
            .validate_repository_id(expected_repository_id.as_deref())
            .map_err(into_mcp_error)?;
        let _admission = match self.request_admission.try_admit() {
            Ok(permit) => permit,
            Err(error) => return self.service_result::<T>(Err(error)),
        };
        self.service_result(operation(services).await)
    }
}

async fn retry_after_initial_index<T, F, Fut>(
    tool: &'static str,
    mcp_services: &McpServices,
    services: &Services,
    cancellation: CancellationToken,
    deadline: tokio::time::Instant,
    operation: F,
) -> crate::Result<T>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = crate::Result<T>>,
{
    retry_after_initial_index_with_policy(
        tool,
        mcp_services,
        cancellation,
        deadline.saturating_duration_since(tokio::time::Instant::now()),
        |wait_cancellation| services.wait_for_initial_index_cancellable(wait_cancellation),
        operation,
    )
    .await
}

async fn retry_after_initial_index_with_policy<T, F, Fut, W, WaitFut>(
    tool: &'static str,
    mcp_services: &McpServices,
    cancellation: CancellationToken,
    wait: Duration,
    wait_until_ready: W,
    mut operation: F,
) -> crate::Result<T>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = crate::Result<T>>,
    W: FnOnce(CancellationToken) -> WaitFut,
    WaitFut: Future<Output = crate::Result<()>>,
{
    let result = operation().await;
    if !matches!(result, Err(crate::Error::IndexNotReady)) {
        return result;
    }

    let started = Instant::now();
    let deadline = tokio::time::Instant::now() + wait;
    let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
    if remaining.is_zero() {
        tracing::debug!(
            tool,
            waited_ms = started.elapsed().as_millis(),
            ready = false,
            "MCP retrieval waited for the first index generation"
        );
        return result;
    }

    let wait_cancellation = cancellation.child_token();
    let readiness = wait_until_ready(wait_cancellation.clone());
    tokio::pin!(readiness);
    loop {
        let state_changed = mcp_services.state_changed.notified();
        tokio::pin!(state_changed);
        state_changed.as_mut().enable();
        if matches!(mcp_services.get(), McpServiceState::Failed { .. }) {
            wait_cancellation.cancel();
            return Err(crate::Error::McpRuntimeStopped);
        }
        tokio::select! {
            ready = &mut readiness => {
                if matches!(mcp_services.get(), McpServiceState::Failed { .. }) {
                    wait_cancellation.cancel();
                    return Err(crate::Error::McpRuntimeStopped);
                }
                ready?;
                if matches!(mcp_services.get(), McpServiceState::Failed { .. }) {
                    wait_cancellation.cancel();
                    return Err(crate::Error::McpRuntimeStopped);
                }
                let result = operation().await;
                if matches!(mcp_services.get(), McpServiceState::Failed { .. }) {
                    wait_cancellation.cancel();
                    return Err(crate::Error::McpRuntimeStopped);
                }
                tracing::debug!(
                    tool,
                    waited_ms = started.elapsed().as_millis(),
                    ready = !matches!(result, Err(crate::Error::IndexNotReady)),
                    "MCP retrieval waited for the first index generation"
                );
                return result;
            }
            _ = cancellation.cancelled() => {
                wait_cancellation.cancel();
                return Err(crate::Error::Cancelled);
            }
            _ = tokio::time::sleep_until(deadline) => {
                wait_cancellation.cancel();
                tracing::debug!(
                    tool,
                    waited_ms = started.elapsed().as_millis(),
                    ready = false,
                    "MCP retrieval waited for the first index generation"
                );
                return result;
            }
            _ = &mut state_changed => {}
        }
        if matches!(mcp_services.get(), McpServiceState::Failed { .. }) {
            wait_cancellation.cancel();
            return Err(crate::Error::McpRuntimeStopped);
        }
    }
}

impl McpServices {
    fn starting(limits: McpLimitPolicy) -> Self {
        Self {
            state: Arc::new(RwLock::new(McpServiceState::Starting(limits))),
            state_changed: Arc::new(tokio::sync::Notify::new()),
            protocol_initialized: Arc::new(AtomicBool::new(false)),
            initialized: Arc::new(tokio::sync::Notify::new()),
        }
    }

    fn ready(services: Arc<Services>) -> Self {
        let limits = McpLimitPolicy::from_config(services.config())
            .expect("Services always contains a validated configuration");
        Self {
            state: Arc::new(RwLock::new(McpServiceState::Ready { services, limits })),
            state_changed: Arc::new(tokio::sync::Notify::new()),
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

    async fn wait_for_services(
        &self,
        initial_state: McpServiceState,
        cancellation: CancellationToken,
        deadline: tokio::time::Instant,
    ) -> crate::Result<McpServiceState> {
        if !matches!(initial_state, McpServiceState::Starting(_)) {
            return Ok(initial_state);
        }
        let started = Instant::now();
        loop {
            let state_changed = self.state_changed.notified();
            tokio::pin!(state_changed);
            state_changed.as_mut().enable();
            let state = self.get();
            if !matches!(state, McpServiceState::Starting(_)) {
                tracing::debug!(
                    waited_ms = started.elapsed().as_millis(),
                    ready = matches!(state, McpServiceState::Ready { .. }),
                    "MCP retrieval waited for repository services"
                );
                return Ok(state);
            }
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                tracing::debug!(
                    waited_ms = started.elapsed().as_millis(),
                    ready = false,
                    "MCP retrieval waited for repository services"
                );
                return Ok(state);
            }
            tokio::select! {
                _ = cancellation.cancelled() => return Err(crate::Error::Cancelled),
                _ = tokio::time::sleep(remaining) => {},
                _ = &mut state_changed => {}
            }
        }
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
        self.state_changed.notify_waiters();
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
            McpServiceState::Starting(current)
            | McpServiceState::Failed {
                limits: current, ..
            } => {
                *current = limits;
            }
            McpServiceState::Ready { .. } => {}
        }
        Ok(())
    }

    /// Mark startup as failed while retaining only an allowlisted client-safe reason.
    pub fn set_failed(&self, error: &crate::Error) {
        let mut state = self
            .state
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *state = McpServiceState::Failed {
            limits: state.limits(),
            failure: StartupFailure::from_error(error),
        };
        drop(state);
        self.state_changed.notify_waiters();
    }

    fn mark_protocol_initialized(&self) {
        self.protocol_initialized.store(true, Ordering::Release);
        self.initialized.notify_waiters();
    }

    /// Wait until the client completes the MCP initialization phase.
    pub async fn wait_initialized(&self) {
        loop {
            let notified = self.initialized.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
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
        description = "Preferred repository path discovery instead of find, ls, or glob. Use tree for hierarchy, find for fuzzy filenames, and glob for path patterns; returns paths, not source. Set projection=paths for opt-in path-only results without kind, language, size, or score metadata. Example: {\"operation\":\"find\",\"query\":\"mcp\"}."
    )]
    async fn leantoken_files(
        &self,
        Parameters(req): Parameters<FilesMcpRequest>,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, ErrorData> {
        let prepared = match self
            .prepare_retrieval_call(context.ct.clone(), |limits| req.validate_limits(limits))
            .await?
        {
            RetrievalPreparation::Ready(prepared) => prepared,
            RetrievalPreparation::Unavailable(result) => return Ok(result),
        };
        let (request, projection, consistency, options, expected_repository_id) = req.into_parts();
        self.run_prepared(
            "files",
            prepared,
            expected_repository_id,
            move |services, cancellation| {
                let request = request.clone();
                async move {
                    match projection {
                        FilesMcpProjection::Full => services
                            .files_with_options_consistency_cancellable(
                                request,
                                consistency,
                                options,
                                cancellation,
                            )
                            .await
                            .and_then(serialized_response),
                        FilesMcpProjection::Paths => services
                            .files_paths_with_options_consistency_cancellable(
                                request,
                                consistency,
                                options,
                                cancellation,
                            )
                            .await
                            .and_then(serialized_response),
                    }
                }
            },
        )
        .await
    }

    #[tool(
        name = "search",
        description = "Preferred indexed source search instead of grep or rg. Finds ranked symbols, references, identifiers, text, or regex matches. Set projection=grouped for opt-in symbol/file summaries. Exhaustive text or regex searches default to projection=occurrences: one excerpt plus every exact line/column coordinate; set coordinates_only=true to omit excerpts and hashes. Use explicit projection=full for legacy per-occurrence hits. Exhaustive scans keep exact returned/total counts and fail instead of silently truncating at internal scan limits. Text and regex hits include the narrowest enclosing_symbol when structural data is available; use that exact name or the returned line range with leantoken.read. Example: {\"query\":\"RetryableConflict\",\"mode\":\"symbol\"}."
    )]
    async fn leantoken_search(
        &self,
        Parameters(req): Parameters<SearchMcpRequest>,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, ErrorData> {
        let prepared = match self
            .prepare_retrieval_call(context.ct.clone(), |limits| req.validate_limits(limits))
            .await?
        {
            RetrievalPreparation::Ready(prepared) => prepared,
            RetrievalPreparation::Unavailable(result) => return Ok(result),
        };
        let (request, projection, coordinates_only, consistency, options, expected_repository_id) =
            req.into_parts();
        self.run_prepared(
            "search",
            prepared,
            expected_repository_id,
            move |services, cancellation| {
                let request = request.clone();
                async move {
                    match projection {
                        SearchMcpProjection::Auto => {
                            unreachable!("search projection is resolved by into_parts")
                        }
                        SearchMcpProjection::Full => services
                            .search_with_options_consistency_cancellable(
                                request,
                                consistency,
                                options,
                                cancellation,
                            )
                            .await
                            .and_then(serialized_response),
                        SearchMcpProjection::Grouped => services
                            .search_grouped_with_options_consistency_cancellable(
                                request,
                                consistency,
                                options,
                                cancellation,
                            )
                            .await
                            .and_then(serialized_response),
                        SearchMcpProjection::Occurrences => services
                            .search_occurrences_with_options_consistency_cancellable(
                                request,
                                coordinates_only,
                                consistency,
                                options,
                                cancellation,
                            )
                            .await
                            .and_then(serialized_response),
                    }
                }
            },
        )
        .await
    }

    #[tool(
        name = "outline",
        description = "Inspect file structure without reading whole source files. Prefer this when the file is known but the relevant symbol or range is not; then use leantoken.read. Set projection=signatures to omit imports and byte offsets while retaining path, line range, signature-set hash, parse coverage, freshness, and continuation. Example: {\"paths\":[\"src/mcp.rs\"]}."
    )]
    async fn leantoken_outline(
        &self,
        Parameters(req): Parameters<OutlineMcpRequest>,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, ErrorData> {
        let prepared = match self
            .prepare_retrieval_call(context.ct.clone(), |limits| req.validate_limits(limits))
            .await?
        {
            RetrievalPreparation::Ready(prepared) => prepared,
            RetrievalPreparation::Unavailable(result) => return Ok(result),
        };
        let (request, projection, consistency, options, expected_repository_id) = req.into_parts();
        self.run_prepared(
            "outline",
            prepared,
            expected_repository_id,
            move |services, cancellation| {
                let request = request.clone();
                async move {
                    match projection {
                        OutlineMcpProjection::Full => services
                            .outline_with_options_consistency_cancellable(
                                request,
                                consistency,
                                options,
                                cancellation,
                            )
                            .await
                            .and_then(serialized_response),
                        OutlineMcpProjection::Signatures => services
                            .outline_signatures_with_options_consistency_cancellable(
                                request,
                                consistency,
                                options,
                                cancellation,
                            )
                            .await
                            .and_then(serialized_response),
                    }
                }
            },
        )
        .await
    }

    #[tool(
        name = "read",
        description = "Preferred exact source and Markdown section reader instead of cat, head, or sed. Keep path as a file path; put the owner separately in target. Exact target shapes include {\"kind\":\"symbol\",\"name\":\"LeanTokenMcp\"}, {\"kind\":\"heading\",\"name\":\"## Performance\",\"occurrence\":2}, and {\"kind\":\"lines\",\"start\":120,\"end\":160}. Heading targets accept an exact rendered title or outline signature. Set delta=true to reuse the latest compatible base for the exact target; unchanged content returns not_modified. Pass expected_hash to require one explicit base. Example: {\"path\":\"README.md\",\"target\":{\"kind\":\"heading\",\"name\":\"Installation\"}}."
    )]
    async fn leantoken_read(
        &self,
        Parameters(req): Parameters<ReadMcpRequest>,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, ErrorData> {
        let prepared = match self
            .prepare_retrieval_call(context.ct.clone(), |limits| req.validate_limits(limits))
            .await?
        {
            RetrievalPreparation::Ready(prepared) => prepared,
            RetrievalPreparation::Unavailable(result) => return Ok(result),
        };
        let (request, consistency, options, expected_repository_id) = req.into_parts();
        self.run_prepared(
            "read",
            prepared,
            expected_repository_id,
            move |services, cancellation| {
                let request = request.clone();
                async move {
                    services
                        .read_with_options_consistency_cancellable(
                            request,
                            consistency,
                            options,
                            cancellation,
                        )
                        .await
                }
            },
        )
        .await
    }

    #[tool(
        name = "history",
        description = "Read, diff, batch-diff, or trace parsed symbols across immutable Git revisions. Symbols may use parent.name qualification. diff_symbols resolves one shared range, loads each bounded path once per endpoint, and returns cursor-paged per-symbol outcomes without N Git subprocess chains. diff_symbol returns bounded add/delete diffs when one endpoint is absent; symbol_log traces tracked lines. For immutable range-scoped context, pass BASE..HEAD as context.base_revision with strict_changed_paths. Example: {\"operation\":{\"kind\":\"diff_symbols\",\"targets\":[{\"path\":\"src/services.rs\",\"symbol\":\"Services.meta\"}],\"base_revision\":\"main~1\",\"head_revision\":\"main\"}}."
    )]
    async fn leantoken_history(
        &self,
        Parameters(req): Parameters<HistoryMcpRequest>,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, ErrorData> {
        let prepared = match self
            .prepare_retrieval_call(context.ct.clone(), |limits| req.validate_limits(limits))
            .await?
        {
            RetrievalPreparation::Ready(prepared) => prepared,
            RetrievalPreparation::Unavailable(result) => return Ok(result),
        };
        let (call, options, expected_repository_id) = req.into_parts().map_err(into_mcp_error)?;
        self.run_prepared(
            "history",
            prepared,
            expected_repository_id,
            move |services, cancellation| {
                let call = call.clone();
                async move {
                    match call {
                        HistoryMcpCall::Single(request) => services
                            .history_cancellable_with_options(request, options, cancellation)
                            .await
                            .and_then(serialized_response),
                        HistoryMcpCall::DiffSymbols(request) => services
                            .history_diff_symbols_cancellable_with_options(
                                request,
                                options,
                                cancellation,
                            )
                            .await
                            .and_then(serialized_response),
                    }
                }
            },
        )
        .await
    }

    #[tool(
        name = "json",
        description = "Query, summarize, or compare bounded live JSON without indexing raw artifacts. Select with RFC 6901 JSON Pointer or standard JMESPath; use collapsed, keys, or schema projections for large arrays and objects, numeric_summary for count/min/median/p95/max, and diff_fields for selected values across two files. Keys can be bounded by depth (root is zero) and paginate in depth-then-pointer order; incomplete schemas return a breadth-first shape with explicit omission metadata. Repeat an incomplete keys query with its cursor. Example: {\"operation\":{\"kind\":\"numeric_summary\",\"path\":\"artifacts/results.json\",\"selector\":{\"kind\":\"jmespath\",\"expression\":\"runs[].score\"}}}."
    )]
    async fn leantoken_json(
        &self,
        Parameters(req): Parameters<JsonMcpRequest>,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, ErrorData> {
        let prepared = match self
            .prepare_retrieval_call(context.ct.clone(), |limits| req.validate_limits(limits))
            .await?
        {
            RetrievalPreparation::Ready(prepared) => prepared,
            RetrievalPreparation::Unavailable(result) => return Ok(result),
        };
        let (request, options, execution, expected_repository_id) = req.into_parts();
        self.run_prepared(
            "json",
            prepared,
            expected_repository_id,
            move |services, cancellation| {
                let request = request.clone();
                async move {
                    services
                        .json_cancellable_with_execution_options(
                            request,
                            options,
                            execution,
                            cancellation,
                        )
                        .await
                }
            },
        )
        .await
    }

    #[tool(
        name = "context",
        description = "DEFAULT FIRST CALL for broad coding, debugging, review, and architecture tasks. Returns the most relevant repository evidence within a strict token budget instead of manually combining search and whole-file reads. For uncertain broad tasks, set plan_only=true to preview bounded ranked paths, ranges, reasons, token estimates, focus coverage, and generated-artifact warnings without source or receipt mutation; then repeat the same request with plan_only=false to materialize. Use include_paths, strict_focus_paths, or strict_changed_paths for hard boundaries; pass BASE..HEAD as base_revision for an immutable Git range. Use minimum_fragments_per_focus_path and must-include constraints for required paths or symbols. When path presence is insufficient, pass required_evidence entries with a path and literal queries; path_scope_satisfied reports only path coverage, while evidence_scope_satisfied requires matching selected evidence. When the caller has directly observed a failure, pass workflow_evidence with bounded failure_traces, symbols, paths, or test_intents; do not infer or copy gold labels into it. Compact omission counts preserve fail-loud coverage by default; set verbose_diagnostics=true only for full omission facets. Oversized diff scopes may return bounded routing suggestions. Reuse receipt fragment_hashes as known_hashes. Set handoff for a compact provenance manifest without copied source. Example: {\"task\":\"Audit MCP tool discovery\"}."
    )]
    async fn leantoken_context(
        &self,
        Parameters(req): Parameters<ContextMcpRequest>,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, ErrorData> {
        let prepared = match self
            .prepare_retrieval_call(context.ct.clone(), |limits| req.validate_limits(limits))
            .await?
        {
            RetrievalPreparation::Ready(prepared) => prepared,
            RetrievalPreparation::Unavailable(result) => return Ok(result),
        };
        let (
            request,
            workflow,
            workflow_evidence,
            consistency,
            options,
            expected_repository_id,
            handoff,
        ) = req.into_parts(prepared.limits.default_context_tokens);
        self.run_prepared(
            "context",
            prepared,
            expected_repository_id,
            move |services, cancellation| {
                let request = request.clone();
                let handoff = handoff.clone();
                let workflow_evidence = workflow_evidence.clone();
                async move {
                    services
                        .context_with_workflow_evidence_options_consistency_cancellable(
                            request,
                            handoff,
                            workflow,
                            workflow_evidence,
                            consistency,
                            options,
                            cancellation,
                        )
                        .await
                }
            },
        )
        .await
    }

    #[tool(
        name = "savings",
        description = "Report repository-local observed response accounting, request classifications, expected-hash suppression, service failures, and explicitly unobserved task outcomes. Returns an opaque snapshot; supply it later for a bounded aggregate delta. Source compression and full-response net cost are separate comparisons against represented source, not claims about task success or complete session savings. Example: {}.",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    async fn leantoken_savings(
        &self,
        Parameters(req): Parameters<SavingsMcpRequest>,
    ) -> Result<CallToolResult, ErrorData> {
        let state = self.services.get();
        let services = match self.services(&state) {
            Ok(services) => services,
            Err(result) => return Ok(result),
        };
        self.run_admitted(services, None, |services| async move {
            services.observed_token_savings_snapshot(req.snapshot).await
        })
        .await
    }
}

#[tool_handler(name = "leantoken")]
impl ServerHandler for LeanTokenMcp {
    fn get_info(&self) -> rmcp::model::ServerInfo {
        rmcp::model::ServerInfo::new(
            rmcp::model::ServerCapabilities::builder()
                .enable_tools()
                .build(),
        )
        .with_server_info(rmcp::model::Implementation::new(
            "leantoken",
            mcp_runtime_version(),
        ))
        .with_instructions(MCP_INSTRUCTIONS.to_string())
    }

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

fn retryable_tool_result(response: RetryableToolResponse, mode: McpResultMode) -> CallToolResult {
    tool_result(response, mode).unwrap_or_else(|error| {
        tracing::error!(%error, "MCP retry response serialization failed");
        tool_unavailable(
            "response_serialization",
            "repository retrieval is temporarily unavailable; retry shortly",
        )
    })
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
    let transport = BoundedStdioTransport::new(server.request_dispatch.clone(), server.result_mode);

    let signal_task = tokio::spawn({
        let token = token.clone();
        async move {
            let _ = tokio::signal::ctrl_c().await;
            token.cancel();
        }
    });

    let result = async {
        let service = match server.serve_with_ct(transport, token.child_token()).await {
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
mod tests;
