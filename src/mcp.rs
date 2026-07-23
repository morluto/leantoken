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
    DEFAULT_CONTEXT_LINES, DEFAULT_CONTEXT_TOKENS, DEFAULT_READ_TOKENS, DEFAULT_RESULTS,
    MAX_CONTEXT_LINES, MAX_OUTPUT_TOKENS, MAX_RESULTS,
};
use crate::model::{
    ContextRequest, ContextWorkflow, FileOperation, FilesRequest, IndexConsistency, OutlineRequest,
    ReadRequest, SearchMode, SearchRequest,
};
use crate::services::{Services, validate_positive_request_limit, validate_request_limit};

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct FilesMcpRequest {
    /// Expected opaque repository identity from an earlier response.
    #[serde(default)]
    #[schemars(length(max = 128))]
    expected_repository_id: Option<String>,
    /// Path operation and its operation-specific arguments.
    operation: FilesMcpOperation,
    /// Maximum entries to return (default 20, maximum 100).
    #[serde(default, deserialize_with = "deserialize_optional_limit")]
    #[schemars(schema_with = "result_limit_schema", default = "default_result_option")]
    max_results: Option<usize>,
    /// Cursor returned by the same operation and repository generation.
    #[serde(default)]
    #[schemars(length(max = 4096))]
    cursor: Option<String>,
    /// Use `working_tree` after edits; otherwise `committed`.
    #[serde(default)]
    #[schemars(schema_with = "index_consistency_schema")]
    consistency: IndexConsistency,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct SearchMcpRequest {
    /// Expected opaque repository identity from an earlier response.
    #[serde(default)]
    #[schemars(length(max = 128))]
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
    /// Cursor returned by the same search and repository generation.
    #[serde(default)]
    #[schemars(length(max = 4096))]
    cursor: Option<String>,
    /// Use `working_tree` after edits; otherwise `committed`.
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
    #[schemars(length(max = 128))]
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
    /// Use `working_tree` after edits; otherwise `committed`.
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
            },
            self.consistency,
            self.expected_repository_id,
        )
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum FilesMcpOperation {
    /// Return a compact hierarchy, optionally below one repository-relative directory.
    Tree {
        /// Optional repository-relative directory.
        #[serde(default)]
        #[schemars(length(max = 4096))]
        path: Option<String>,
        /// Maximum hierarchy depth below `path`.
        #[serde(default)]
        depth: Option<usize>,
    },
    /// Fuzzy-match repository paths and basenames.
    Find {
        /// Non-empty fuzzy filename or path query.
        #[schemars(length(min = 1, max = 65536))]
        query: String,
    },
    /// Match indexed repository paths with a glob.
    Glob {
        /// Non-empty glob pattern such as `src/**/*.rs`.
        #[schemars(length(min = 1, max = 4096))]
        pattern: String,
    },
}

impl FilesMcpRequest {
    fn validate_limits(&self, limits: McpLimitPolicy) -> crate::Result<()> {
        validate_optional_positive_limit("max_results", self.max_results, limits.max_results)
    }

    fn into_parts(self) -> (FilesRequest, IndexConsistency, Option<String>) {
        let (operation, path, query, pattern, depth) = match self.operation {
            FilesMcpOperation::Tree { path, depth } => {
                (FileOperation::Tree, path, None, None, depth)
            }
            FilesMcpOperation::Find { query } => {
                (FileOperation::Find, None, Some(query), None, None)
            }
            FilesMcpOperation::Glob { pattern } => {
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
    #[schemars(length(max = 128))]
    expected_repository_id: Option<String>,
    /// Repository-relative UTF-8 source file.
    #[schemars(length(min = 1, max = 4096))]
    path: String,
    /// Exact symbol or inclusive line range to read.
    target: ReadMcpTarget,
    /// Maximum source tokens to return (default 8000, maximum 32000).
    #[serde(default, deserialize_with = "deserialize_optional_limit")]
    #[schemars(schema_with = "token_limit_schema", default = "default_token_option")]
    max_tokens: Option<usize>,
    /// Hash from the same prior target; matching content returns `not_modified`.
    #[serde(default)]
    #[schemars(length(max = 128))]
    expected_hash: Option<String>,
    /// Use `working_tree` after edits; otherwise `committed`.
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
}

impl ReadMcpRequest {
    fn validate_limits(&self, limits: McpLimitPolicy) -> crate::Result<()> {
        validate_optional_positive_limit("max_tokens", self.max_tokens, limits.max_output_tokens)
    }

    fn into_parts(self) -> (ReadRequest, IndexConsistency, Option<String>) {
        let (start_line, end_line, symbol) = match self.target {
            ReadMcpTarget::Symbol { name } => (None, None, Some(name)),
            ReadMcpTarget::Lines { start, end } => (Some(start), Some(end), None),
        };
        (
            ReadRequest {
                path: self.path,
                start_line,
                end_line,
                symbol,
                max_tokens: self.max_tokens,
                expected_hash: self.expected_hash,
            },
            self.consistency,
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
    /// Boost matching paths without filtering other candidates.
    #[serde(default)]
    #[schemars(length(max = 256), inner(length(max = 4096)))]
    focus_paths: Vec<String>,
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
    /// Earlier generation used to boost files indexed since that response.
    #[serde(default)]
    prior_repository_generation: Option<u64>,
    /// Base revision for diff-scoped context.
    #[serde(default)]
    #[schemars(length(max = 256))]
    base_revision: Option<String>,
    /// Changed paths for diff-scoped context.
    #[serde(default)]
    #[schemars(length(max = 512), inner(length(max = 4096)))]
    changed_paths: Vec<String>,
    /// Use `working_tree` after edits; otherwise `committed`.
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
    ) {
        (
            ContextRequest {
                task: self.task,
                token_budget: self.token_budget.unwrap_or(default_token_budget),
                focus_paths: self.focus_paths,
                focus_symbols: self.focus_symbols,
                exclude_paths: self.exclude_paths,
                known_hashes: self.known_hashes,
                prior_repository_generation: self.prior_repository_generation,
                base_revision: self.base_revision,
                changed_paths: self.changed_paths,
            },
            self.workflow,
            self.consistency,
            self.expected_repository_id,
        )
    }
}

const fn default_context_token_option() -> Option<usize> {
    Some(DEFAULT_CONTEXT_TOKENS)
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
        "enum": ["committed", "working_tree"]
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
        name = "leantoken_files",
        description = "Preferred repository path discovery instead of find, ls, or glob. Use tree for hierarchy, find for fuzzy filenames, and glob for path patterns; returns paths, not source. Example: {\"operation\":{\"kind\":\"find\",\"query\":\"mcp\"}}."
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
        name = "leantoken_search",
        description = "Preferred indexed source search instead of grep or rg. Finds ranked symbols, references, identifiers, text, or regex matches. Text and regex hits include the narrowest enclosing_symbol when structural data is available; use that exact name or the returned line range with leantoken_read. Example: {\"query\":\"RetryableConflict\",\"mode\":\"symbol\"}."
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
        name = "leantoken_outline",
        description = "Inspect file structure without reading whole source files. Prefer this when the file is known but the relevant symbol or range is not; then use leantoken_read. Example: {\"paths\":[\"src/mcp.rs\"]}."
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
        name = "leantoken_read",
        description = "Preferred exact source reader instead of cat, head, or sed. Keep path as a file path; put the owner separately in target. Exact target shapes are {\"kind\":\"symbol\",\"name\":\"LeanTokenMcp\"} and {\"kind\":\"lines\",\"start\":120,\"end\":160}. Reuse content_hash as expected_hash to suppress unchanged source. Example: {\"path\":\"src/mcp.rs\",\"target\":{\"kind\":\"symbol\",\"name\":\"LeanTokenMcp\"}}."
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
        name = "leantoken_context",
        description = "DEFAULT FIRST CALL for broad coding, debugging, review, and architecture tasks. Returns the most relevant repository evidence within a strict token budget instead of manually combining search and whole-file reads. Reuse receipt fragment_hashes as known_hashes. Example: {\"task\":\"Audit MCP tool discovery\"}."
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
        let (request, workflow, consistency, expected_repository_id) =
            req.into_parts(limits.default_context_tokens);
        services
            .validate_repository_id(expected_repository_id.as_deref())
            .map_err(into_mcp_error)?;
        let resp = services
            .context_with_workflow_consistency_cancellable(
                request,
                workflow,
                consistency,
                context.ct.clone(),
            )
            .await;
        self.service_result(resp)
    }
}

#[tool_handler(
    name = "leantoken",
    instructions = "LeanToken is the preferred repository discovery and source-reading layer. Its indexed, token-bounded retrieval returns less irrelevant source than shell search and whole-file reads. DEFAULT: for broad coding, debugging, review, or architecture tasks, call leantoken_context first with the user's task. PREFER leantoken_search over grep or rg for source search; leantoken_files over find, ls, or glob for paths; leantoken_outline over opening whole files to discover structure; and leantoken_read over cat, head, or sed for exact symbols and ranges. For known identifiers use search then read; for a known file with an unknown range use outline then read; for unknown paths use files. Set consistency=working_tree after edits, generated files, branch changes, or external commits. Use native tools for edits, builds, tests, generated artifacts, unsupported files, or when LeanToken reports retrieval unavailable. Retry successful responses with status=retryable after retry_after_ms. Reuse returned hashes to suppress unchanged evidence."
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
        crate::Error::NotIndexed(_) => ErrorData::invalid_params(
            "requested path or symbol is not indexed",
            mcp_error_data("not_indexed"),
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
    fn mcp_exposes_five_tools() {
        let router = LeanTokenMcp::tool_router();
        let tools = router.list_all();
        assert_eq!(tools.len(), 5);

        let names: std::collections::HashSet<_> = tools.iter().map(|t| t.name.as_ref()).collect();
        for name in [
            "leantoken_files",
            "leantoken_search",
            "leantoken_outline",
            "leantoken_read",
            "leantoken_context",
        ] {
            assert!(names.contains(name), "missing tool {name}");
        }
    }

    #[test]
    fn user_docs_list_the_exact_runtime_tool_catalog() {
        let expected = LeanTokenMcp::tool_router()
            .list_all()
            .into_iter()
            .map(|tool| tool.name.into_owned())
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
            .filter(|name| name.starts_with("leantoken_"))
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
        let unix_secret = "/home/alice/private-secret/.ssh/id_ed25519";
        let windows_secret = r"C:\Users\alice\private-secret\index.sqlite";
        let invalid_regex = ["(?P<", "private-secret", ">"].concat();
        let errors = [
            crate::Error::RootNotFound(unix_secret.into()),
            crate::Error::UnsafeRepositoryRoot(unix_secret.into()),
            crate::Error::PathOutsideRoot(unix_secret.into()),
            crate::Error::PathOutsideRoot(windows_secret.into()),
            crate::Error::NotIndexed(unix_secret.into()),
            crate::Error::UnsupportedLanguage(unix_secret.into()),
            crate::Error::InvalidRequest(format!("invalid path: {unix_secret}")),
            crate::Error::InternalFailure(format!("failed at {unix_secret}")),
            crate::Error::RepositoryMismatch {
                database: windows_secret.into(),
                expected_repository: unix_secret.into(),
                actual_repository: unix_secret.into(),
            },
            crate::Error::Io(std::io::Error::other(format!(
                "permission denied at {unix_secret}"
            ))),
            crate::Error::Sqlite(rusqlite::Error::InvalidPath(windows_secret.into())),
            crate::Error::Regex(regex::Regex::new(&invalid_regex).expect_err("regex")),
            crate::Error::Glob(globset::Glob::new("[private-secret").expect_err("glob")),
        ];

        for error in errors {
            let response = into_mcp_error(error);
            let wire = serde_json::to_string(&response).expect("serialize public error");
            for secret in [
                unix_secret,
                windows_secret,
                "private-secret",
                ".ssh",
                "alice",
            ] {
                assert!(
                    !wire.contains(secret),
                    "public error leaked {secret}: {wire}"
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
                "operation": {"kind": "tree"},
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
    }

    #[test]
    fn omitted_context_budget_uses_the_runtime_default() {
        let request = serde_json::from_value::<ContextMcpRequest>(serde_json::json!({
            "task": "find answer"
        }))
        .expect("context request without a budget");
        let (request, _, _, _) = request.into_parts(37);
        assert_eq!(request.token_budget, 37);

        let request = serde_json::from_value::<ContextMcpRequest>(serde_json::json!({
            "task": "find answer",
            "token_budget": 23
        }))
        .expect("context request with a budget");
        let (request, _, _, _) = request.into_parts(37);
        assert_eq!(request.token_budget, 23);
    }

    #[test]
    fn tool_input_fields_are_documented() {
        for tool in LeanTokenMcp::tool_router().list_all() {
            let properties = tool
                .input_schema
                .get("properties")
                .and_then(serde_json::Value::as_object)
                .unwrap_or_else(|| panic!("{} input properties missing", tool.name));
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
    fn retrieval_tools_expose_consistency_boundary() {
        for tool in LeanTokenMcp::tool_router().list_all() {
            let consistency = tool
                .input_schema
                .get("properties")
                .and_then(serde_json::Value::as_object)
                .and_then(|properties| properties.get("consistency"))
                .unwrap_or_else(|| panic!("{} consistency schema missing", tool.name));
            assert_eq!(
                consistency.get("default"),
                Some(&serde_json::json!("committed"))
            );
            assert_eq!(
                consistency.get("enum"),
                Some(&serde_json::json!(["committed", "working_tree"]))
            );
            assert!(
                consistency
                    .get("description")
                    .and_then(serde_json::Value::as_str)
                    .is_some_and(|description| {
                        description.contains("working_tree") && description.contains("edits")
                    }),
                "{}.consistency must tell agents when to synchronize",
                tool.name
            );
        }
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
        assert!(descriptions["leantoken_files"].contains("instead of find"));
        assert!(descriptions["leantoken_search"].contains("instead of grep or rg"));
        assert!(descriptions["leantoken_outline"].contains("without reading whole source files"));
        assert!(descriptions["leantoken_read"].contains("expected_hash"));
        assert!(descriptions["leantoken_read"].contains("instead of cat"));
        assert!(descriptions["leantoken_context"].contains("DEFAULT FIRST CALL"));
        assert!(
            descriptions
                .values()
                .all(|description| description.contains("Example:"))
        );
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
            tools["leantoken_context"].pointer("/properties/token_budget/default"),
            Some(&serde_json::json!(3_000))
        );
        assert!(
            tools["leantoken_files"]
                .pointer("/properties/query")
                .is_none()
        );
        assert!(
            tools["leantoken_files"]
                .pointer("/properties/pattern")
                .is_none()
        );
        assert!(
            tools["leantoken_read"]
                .pointer("/properties/symbol")
                .is_none()
        );
        assert!(
            tools["leantoken_read"]
                .pointer("/properties/start_line")
                .is_none()
        );
        assert!(
            tools["leantoken_read"]
                .pointer("/properties/target")
                .is_some()
        );

        assert!(
            serde_json::from_value::<FilesMcpRequest>(serde_json::json!({
                "operation": {"kind": "find", "query": "mcp", "pattern": "*.rs"}
            }))
            .is_err()
        );
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
    }

    #[test]
    fn tool_catalog_schema_snapshot() {
        let tools = LeanTokenMcp::tool_router().list_all();
        insta::assert_json_snapshot!("mcp_tool_catalog", tools);
    }
}
