use std::time::{Duration, Instant};
use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex, RwLock,
        atomic::{AtomicBool, Ordering},
    },
};

use rmcp::{
    ErrorData, RoleServer, ServerHandler, ServiceExt,
    handler::server::wrapper::Parameters,
    model::{
        CallToolResult, ClientNotification, ClientRequest, ContentBlock, GetExtensions,
        JsonRpcMessage, ServerResult,
    },
    service::{NotificationContext, RequestContext, RxJsonRpcMessage, TxJsonRpcMessage},
    tool, tool_handler, tool_router,
    transport::Transport,
};
use schemars::{JsonSchema, Schema, SchemaGenerator};
use serde::{Deserialize, Deserializer, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
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
const MAX_MCP_STDIO_FRAME_BYTES: usize = 4 * 1024 * 1024;
const RETAINED_MCP_FRAME_CAPACITY: usize = 64 * 1024;
const INITIAL_INDEX_WAIT: Duration = Duration::from_secs(30);

fn serialized_response<T: Serialize>(response: T) -> crate::Result<serde_json::Value> {
    serde_json::to_value(response).map_err(|error| crate::Error::InternalFailure(error.to_string()))
}

#[derive(Clone)]
struct DispatchedToolCall {
    id: rmcp::model::RequestId,
    dispatched_calls:
        Arc<Mutex<HashMap<rmcp::model::RequestId, tokio::sync::OwnedSemaphorePermit>>>,
}

impl Drop for DispatchedToolCall {
    fn drop(&mut self) {
        if std::thread::panicking() {
            self.dispatched_calls
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .remove(&self.id);
        }
    }
}

struct BoundedStdioTransport {
    reader: tokio::io::BufReader<tokio::io::Stdin>,
    writer: Arc<tokio::sync::Mutex<tokio::io::Stdout>>,
    frame: Vec<u8>,
    discarding_oversized_frame: bool,
    request_dispatch: RequestAdmission,
    dispatched_calls:
        Arc<Mutex<HashMap<rmcp::model::RequestId, tokio::sync::OwnedSemaphorePermit>>>,
    result_mode: McpResultMode,
}

impl BoundedStdioTransport {
    fn new(request_dispatch: RequestAdmission, result_mode: McpResultMode) -> Self {
        Self {
            reader: tokio::io::BufReader::with_capacity(8 * 1024, tokio::io::stdin()),
            writer: Arc::new(tokio::sync::Mutex::new(tokio::io::stdout())),
            frame: Vec::new(),
            discarding_oversized_frame: false,
            request_dispatch,
            dispatched_calls: Arc::new(Mutex::new(HashMap::new())),
            result_mode,
        }
    }

    fn release_frame(&mut self) {
        self.frame.clear();
        if self.frame.capacity() > RETAINED_MCP_FRAME_CAPACITY {
            self.frame = Vec::new();
        }
    }

    async fn write_message(
        writer: Arc<tokio::sync::Mutex<tokio::io::Stdout>>,
        item: TxJsonRpcMessage<RoleServer>,
    ) -> std::io::Result<()> {
        let mut bytes = serde_json::to_vec(&item)
            .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
        bytes.push(b'\n');
        let mut writer = writer.lock().await;
        writer.write_all(&bytes).await?;
        writer.flush().await
    }

    fn admit_message(
        &self,
        message: &mut RxJsonRpcMessage<RoleServer>,
    ) -> Result<(), rmcp::model::RequestId> {
        let request = match message {
            JsonRpcMessage::Notification(notification) => {
                if let ClientNotification::CancelledNotification(cancelled) =
                    &notification.notification
                    && let Some(id) = &cancelled.params.request_id
                {
                    Self::finish_dispatch(&self.dispatched_calls, id);
                }
                return Ok(());
            }
            JsonRpcMessage::Request(request) => request,
            JsonRpcMessage::Response(_) | JsonRpcMessage::Error(_) => return Ok(()),
        };
        if !matches!(&request.request, ClientRequest::CallToolRequest(_)) {
            return Ok(());
        }
        let id = request.id.clone();
        let mut dispatched = self
            .dispatched_calls
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if dispatched.contains_key(&id) {
            return Err(id);
        }
        let permit = match self.request_dispatch.try_admit() {
            Ok(permit) => permit,
            Err(_) => return Err(id),
        };
        dispatched.insert(id.clone(), permit);
        drop(dispatched);
        request.request.extensions_mut().insert(DispatchedToolCall {
            id,
            dispatched_calls: Arc::clone(&self.dispatched_calls),
        });
        Ok(())
    }

    fn response_id(item: &TxJsonRpcMessage<RoleServer>) -> Option<rmcp::model::RequestId> {
        match item {
            JsonRpcMessage::Response(response) => Some(response.id.clone()),
            JsonRpcMessage::Error(error) => error.id.clone(),
            JsonRpcMessage::Request(_) | JsonRpcMessage::Notification(_) => None,
        }
    }

    fn finish_dispatch(
        dispatched_calls: &Mutex<
            HashMap<rmcp::model::RequestId, tokio::sync::OwnedSemaphorePermit>,
        >,
        id: &rmcp::model::RequestId,
    ) {
        dispatched_calls
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(id);
    }

    fn overloaded_response(&self, id: rmcp::model::RequestId) -> TxJsonRpcMessage<RoleServer> {
        let result = retryable_tool_result(
            RetryableToolResponse::new(
                "retrieval_capacity_exhausted",
                "repository tool-call capacity is exhausted; retry shortly",
                500,
            ),
            self.result_mode,
        );
        TxJsonRpcMessage::<RoleServer>::response(ServerResult::CallToolResult(result), id)
    }
}

impl Transport<RoleServer> for BoundedStdioTransport {
    type Error = std::io::Error;

    fn send(
        &mut self,
        item: TxJsonRpcMessage<RoleServer>,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send + 'static {
        let writer = Arc::clone(&self.writer);
        let dispatched_calls = Arc::clone(&self.dispatched_calls);
        let response_id = Self::response_id(&item);
        async move {
            let result = Self::write_message(writer, item).await;
            if let Some(id) = response_id {
                Self::finish_dispatch(&dispatched_calls, &id);
            }
            result
        }
    }

    async fn receive(&mut self) -> Option<RxJsonRpcMessage<RoleServer>> {
        loop {
            let available = match self.reader.fill_buf().await {
                Ok([]) => return None,
                Ok(bytes) => bytes,
                Err(error) => {
                    tracing::warn!(%error, "MCP stdio read failed");
                    return None;
                }
            };
            let newline = available.iter().position(|byte| *byte == b'\n');

            if self.discarding_oversized_frame {
                let consumed = newline.map_or(available.len(), |position| position + 1);
                self.reader.consume(consumed);
                if newline.is_some() {
                    self.discarding_oversized_frame = false;
                }
                continue;
            }

            let payload_bytes = newline.unwrap_or(available.len());
            if self.frame.len().saturating_add(payload_bytes) > MAX_MCP_STDIO_FRAME_BYTES {
                let consumed = newline.map_or(available.len(), |position| position + 1);
                self.reader.consume(consumed);
                self.release_frame();
                self.discarding_oversized_frame = newline.is_none();
                tracing::warn!(
                    limit = MAX_MCP_STDIO_FRAME_BYTES,
                    "discarded oversized MCP stdio frame"
                );
                continue;
            }

            self.frame.extend_from_slice(&available[..payload_bytes]);
            self.reader
                .consume(newline.map_or(payload_bytes, |position| position + 1));
            if newline.is_none() {
                continue;
            }
            if self.frame.last() == Some(&b'\r') {
                self.frame.pop();
            }
            if self.frame.is_empty() {
                continue;
            }

            let parsed = serde_json::from_slice(&self.frame);
            self.release_frame();
            match parsed {
                Ok(mut message) => match self.admit_message(&mut message) {
                    Ok(()) => return Some(message),
                    Err(id) => {
                        let response = self.overloaded_response(id);
                        if Self::write_message(Arc::clone(&self.writer), response)
                            .await
                            .is_err()
                        {
                            return None;
                        }
                    }
                },
                Err(error) => match error.classify() {
                    serde_json::error::Category::Syntax | serde_json::error::Category::Eof => {
                        tracing::debug!("ignored unparsable MCP stdio frame");
                    }
                    serde_json::error::Category::Data | serde_json::error::Category::Io => {
                        tracing::debug!("rejected invalid MCP stdio message shape");
                        let response = TxJsonRpcMessage::<RoleServer>::error(
                            ErrorData::invalid_request("Invalid request", None),
                            None,
                        );
                        if Self::write_message(Arc::clone(&self.writer), response)
                            .await
                            .is_err()
                        {
                            return None;
                        }
                    }
                },
            }
        }
    }

    async fn close(&mut self) -> Result<(), Self::Error> {
        self.writer.lock().await.shutdown().await
    }
}
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct SavingsMcpRequest {}

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
    /// Preserve the complete ranked-hit response.
    #[default]
    Full,
    /// Group the selected page into symbol or file summaries.
    Grouped,
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
    /// Response shape: `full` hits (default) or compact `grouped` matches.
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
        )
    }

    fn into_parts(
        self,
    ) -> (
        SearchRequest,
        SearchMcpProjection,
        IndexConsistency,
        ServiceCallOptions,
        Option<String>,
    ) {
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
            self.projection,
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
    /// Exact symbol, Markdown heading, line range, or continuation to read.
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
        let deadline = tokio::time::Instant::now() + INITIAL_INDEX_WAIT;
        let state = self.services.get();
        req.validate_limits(state.limits())
            .map_err(into_mcp_error)?;
        let state = self
            .services
            .wait_for_services(state, context.ct.clone(), deadline)
            .await
            .map_err(into_mcp_error)?;
        req.validate_limits(state.limits())
            .map_err(into_mcp_error)?;
        let services = match self.services(&state) {
            Ok(services) => services,
            Err(result) => return Ok(result),
        };
        let (request, projection, consistency, options, expected_repository_id) = req.into_parts();
        let cancellation = context.ct.clone();
        let mcp_services = self.services.clone();
        self.run_admitted(
            services,
            expected_repository_id,
            move |services| async move {
                retry_after_initial_index(
                    "files",
                    &mcp_services,
                    &services,
                    cancellation.clone(),
                    deadline,
                    || {
                        let request = request.clone();
                        let cancellation = cancellation.clone();
                        async {
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
            },
        )
        .await
    }

    #[tool(
        name = "search",
        description = "Preferred indexed source search instead of grep or rg. Finds ranked symbols, references, identifiers, text, or regex matches. Set projection=grouped for opt-in symbol/file summaries that retain one verifiable excerpt, reference counts, coverage, freshness, and continuation without repeated scores or excerpts. Set all_occurrences in text or regex mode for exact occurrence coordinates and returned/total counts; exhaustive scans fail instead of silently truncating at internal scan limits. Text and regex hits include the narrowest enclosing_symbol when structural data is available; use that exact name or the returned line range with leantoken.read. Example: {\"query\":\"RetryableConflict\",\"mode\":\"symbol\"}."
    )]
    async fn leantoken_search(
        &self,
        Parameters(req): Parameters<SearchMcpRequest>,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, ErrorData> {
        let deadline = tokio::time::Instant::now() + INITIAL_INDEX_WAIT;
        let state = self.services.get();
        req.validate_limits(state.limits())
            .map_err(into_mcp_error)?;
        let state = self
            .services
            .wait_for_services(state, context.ct.clone(), deadline)
            .await
            .map_err(into_mcp_error)?;
        req.validate_limits(state.limits())
            .map_err(into_mcp_error)?;
        let services = match self.services(&state) {
            Ok(services) => services,
            Err(result) => return Ok(result),
        };
        let (request, projection, consistency, options, expected_repository_id) = req.into_parts();
        let cancellation = context.ct.clone();
        let mcp_services = self.services.clone();
        self.run_admitted(
            services,
            expected_repository_id,
            move |services| async move {
                retry_after_initial_index(
                    "search",
                    &mcp_services,
                    &services,
                    cancellation.clone(),
                    deadline,
                    || {
                        let request = request.clone();
                        let cancellation = cancellation.clone();
                        async {
                            match projection {
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
                            }
                        }
                    },
                )
                .await
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
        let deadline = tokio::time::Instant::now() + INITIAL_INDEX_WAIT;
        let state = self.services.get();
        req.validate_limits(state.limits())
            .map_err(into_mcp_error)?;
        let state = self
            .services
            .wait_for_services(state, context.ct.clone(), deadline)
            .await
            .map_err(into_mcp_error)?;
        req.validate_limits(state.limits())
            .map_err(into_mcp_error)?;
        let services = match self.services(&state) {
            Ok(services) => services,
            Err(result) => return Ok(result),
        };
        let (request, projection, consistency, options, expected_repository_id) = req.into_parts();
        let cancellation = context.ct.clone();
        let mcp_services = self.services.clone();
        self.run_admitted(
            services,
            expected_repository_id,
            move |services| async move {
                retry_after_initial_index(
                    "outline",
                    &mcp_services,
                    &services,
                    cancellation.clone(),
                    deadline,
                    || {
                        let request = request.clone();
                        let cancellation = cancellation.clone();
                        async {
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
            },
        )
        .await
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
        let deadline = tokio::time::Instant::now() + INITIAL_INDEX_WAIT;
        let state = self.services.get();
        req.validate_limits(state.limits())
            .map_err(into_mcp_error)?;
        let state = self
            .services
            .wait_for_services(state, context.ct.clone(), deadline)
            .await
            .map_err(into_mcp_error)?;
        req.validate_limits(state.limits())
            .map_err(into_mcp_error)?;
        let services = match self.services(&state) {
            Ok(services) => services,
            Err(result) => return Ok(result),
        };
        let (request, consistency, options, expected_repository_id) = req.into_parts();
        let cancellation = context.ct.clone();
        let mcp_services = self.services.clone();
        self.run_admitted(
            services,
            expected_repository_id,
            move |services| async move {
                retry_after_initial_index(
                    "read",
                    &mcp_services,
                    &services,
                    cancellation.clone(),
                    deadline,
                    || {
                        services.read_with_options_consistency_cancellable(
                            request.clone(),
                            consistency,
                            options,
                            cancellation.clone(),
                        )
                    },
                )
                .await
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
        let deadline = tokio::time::Instant::now() + INITIAL_INDEX_WAIT;
        let state = self.services.get();
        req.validate_limits(state.limits())
            .map_err(into_mcp_error)?;
        let state = self
            .services
            .wait_for_services(state, context.ct.clone(), deadline)
            .await
            .map_err(into_mcp_error)?;
        req.validate_limits(state.limits())
            .map_err(into_mcp_error)?;
        let services = match self.services(&state) {
            Ok(services) => services,
            Err(result) => return Ok(result),
        };
        let (call, options, expected_repository_id) = req.into_parts().map_err(into_mcp_error)?;
        let cancellation = context.ct.clone();
        let mcp_services = self.services.clone();
        self.run_admitted(
            services,
            expected_repository_id,
            move |services| async move {
                retry_after_initial_index(
                    "history",
                    &mcp_services,
                    &services,
                    cancellation.clone(),
                    deadline,
                    || {
                        let call = call.clone();
                        let services = services.clone();
                        let cancellation = cancellation.clone();
                        async move {
                            match call {
                                HistoryMcpCall::Single(request) => services
                                    .history_cancellable_with_options(
                                        request,
                                        options,
                                        cancellation,
                                    )
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
        let deadline = tokio::time::Instant::now() + INITIAL_INDEX_WAIT;
        let state = self.services.get();
        req.validate_limits(state.limits())
            .map_err(into_mcp_error)?;
        let state = self
            .services
            .wait_for_services(state, context.ct.clone(), deadline)
            .await
            .map_err(into_mcp_error)?;
        req.validate_limits(state.limits())
            .map_err(into_mcp_error)?;
        let services = match self.services(&state) {
            Ok(services) => services,
            Err(result) => return Ok(result),
        };
        let (request, options, execution, expected_repository_id) = req.into_parts();
        let cancellation = context.ct.clone();
        let mcp_services = self.services.clone();
        self.run_admitted(
            services,
            expected_repository_id,
            move |services| async move {
                retry_after_initial_index(
                    "json",
                    &mcp_services,
                    &services,
                    cancellation.clone(),
                    deadline,
                    || {
                        services.json_cancellable_with_execution_options(
                            request.clone(),
                            options,
                            execution,
                            cancellation.clone(),
                        )
                    },
                )
                .await
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
        let deadline = tokio::time::Instant::now() + INITIAL_INDEX_WAIT;
        let state = self.services.get();
        let limits = state.limits();
        req.validate_limits(limits).map_err(into_mcp_error)?;
        let state = self
            .services
            .wait_for_services(state, context.ct.clone(), deadline)
            .await
            .map_err(into_mcp_error)?;
        let limits = state.limits();
        req.validate_limits(limits).map_err(into_mcp_error)?;
        let services = match self.services(&state) {
            Ok(services) => services,
            Err(result) => return Ok(result),
        };
        let (
            request,
            workflow,
            workflow_evidence,
            consistency,
            options,
            expected_repository_id,
            handoff,
        ) = req.into_parts(limits.default_context_tokens);
        let cancellation = context.ct.clone();
        let mcp_services = self.services.clone();
        self.run_admitted(
            services,
            expected_repository_id,
            move |services| async move {
                retry_after_initial_index(
                    "context",
                    &mcp_services,
                    &services,
                    cancellation.clone(),
                    deadline,
                    || {
                        services.context_with_workflow_evidence_options_consistency_cancellable(
                            request.clone(),
                            handoff.clone(),
                            workflow,
                            workflow_evidence.clone(),
                            consistency,
                            options,
                            cancellation.clone(),
                        )
                    },
                )
                .await
            },
        )
        .await
    }

    #[tool(
        name = "savings",
        description = "Report repository-local observed response accounting, expected-hash suppression, service failures, and explicitly unobserved task outcomes. Source and response deltas are comparisons against represented source, not claims about task success. Example: {}.",
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
        self.run_admitted(services, None, |services| async move {
            services.observed_token_savings_report().await
        })
        .await
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

fn retryable_tool_result(response: RetryableToolResponse, mode: McpResultMode) -> CallToolResult {
    tool_result(response, mode).unwrap_or_else(|error| {
        tracing::error!(%error, "MCP retry response serialization failed");
        tool_unavailable(
            "response_serialization",
            "repository retrieval is temporarily unavailable; retry shortly",
        )
    })
}

fn into_mcp_error(error: crate::Error) -> ErrorData {
    let cause = error.reconciliation_cause();
    match cause {
        crate::Error::Cancelled => {
            ErrorData::invalid_request("request cancelled", mcp_error_data("request_cancelled"))
        }
        crate::Error::PathOutsideRoot(_) => {
            tracing::debug!(%cause, "MCP path rejected outside repository root");
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
        crate::Error::InvalidJson {
            syntax_category,
            byte_offset,
            line,
            column,
            reason,
        } => ErrorData::invalid_params(
            format!("file is not valid JSON at line {line}, column {column}"),
            Some(serde_json::json!({
                "category": "invalid_json",
                "field": "path",
                "syntax_category": syntax_category,
                "byte_offset": byte_offset,
                "line": line,
                "column": column,
                "reason": reason,
            })),
        ),
        crate::Error::InvalidJsonSelector {
            stage,
            offset,
            line,
            column,
            reason,
        } => ErrorData::invalid_params(
            format!("JMESPath {stage} failed at line {line}, column {column}"),
            Some(serde_json::json!({
                "category": "invalid_json_selector",
                "field": "JMESPath expression",
                "stage": stage,
                "offset": offset,
                "line": line,
                "column": column,
                "reason": reason,
            })),
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
            tracing::error!(%cause, "repository configuration is invalid");
            ErrorData::internal_error(
                "repository configuration is invalid",
                mcp_error_data("repository_configuration"),
            )
        }
        crate::Error::IndexLimitExceeded { .. } => {
            tracing::error!(%cause, "repository indexing limit exceeded");
            ErrorData::internal_error(
                "repository indexing limit exceeded",
                mcp_error_data("repository_index_limit"),
            )
        }
        crate::Error::RepositoryTraversal(_) => {
            tracing::error!(%cause, "repository traversal failed");
            ErrorData::internal_error(
                "repository traversal failed",
                mcp_error_data("repository_traversal"),
            )
        }
        crate::Error::RuntimeCapabilityUnavailable { .. } => {
            tracing::error!(%cause, "repository runtime is unavailable");
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
            tracing::error!(%cause, "MCP tool failed");
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

fn tool_unavailable(reason: &'static str, message: &'static str) -> CallToolResult {
    let mut result = CallToolResult::error(vec![ContentBlock::text(message)]);
    result.structured_content = Some(serde_json::json!({
        "status": "unavailable",
        "reason": reason,
        "message": message,
    }));
    result
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
mod tests {
    use rmcp::{serve_client, serve_server};

    use super::*;

    fn incoming_request(value: serde_json::Value) -> RxJsonRpcMessage<RoleServer> {
        serde_json::from_value(value).expect("valid MCP request")
    }

    #[test]
    fn transport_dispatch_has_an_exact_tool_boundary_and_bypasses_control_requests() {
        let dispatch = RequestAdmission::new(DEFAULT_DISPATCHED_TOOL_CALL_CAPACITY);
        let transport = BoundedStdioTransport::new(dispatch.clone(), McpResultMode::Dual);
        let mut admitted = (0..DEFAULT_DISPATCHED_TOOL_CALL_CAPACITY)
            .map(|id| {
                let mut request = incoming_request(serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "method": "tools/call",
                    "params": {"name": "files", "arguments": {}}
                }));
                transport
                    .admit_message(&mut request)
                    .expect("admit tool call");
                request
            })
            .collect::<Vec<_>>();
        assert_eq!(dispatch.available_permits(), 0);

        let mut excess = incoming_request(serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1000,
            "method": "tools/call",
            "params": {"name": "files", "arguments": {}}
        }));
        assert!(transport.admit_message(&mut excess).is_err());

        for mut control in [
            incoming_request(serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1001,
                "method": "initialize",
                "params": {
                    "protocolVersion": "2025-06-18",
                    "capabilities": {},
                    "clientInfo": {"name": "test", "version": "1"}
                }
            })),
            incoming_request(serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1002,
                "method": "tools/list",
                "params": {}
            })),
        ] {
            transport
                .admit_message(&mut control)
                .expect("control request bypasses tool dispatch");
        }

        let request_ids = admitted
            .iter()
            .map(|message| match message {
                JsonRpcMessage::Request(request) => request.id.clone(),
                _ => unreachable!("test creates requests"),
            })
            .collect::<Vec<_>>();
        admitted.clear();
        assert_eq!(
            dispatch.available_permits(),
            0,
            "handler completion alone must not release response capacity"
        );
        for id in request_ids {
            BoundedStdioTransport::finish_dispatch(&transport.dispatched_calls, &id);
        }
        assert_eq!(
            dispatch.available_permits(),
            DEFAULT_DISPATCHED_TOOL_CALL_CAPACITY
        );
    }

    #[test]
    fn transport_dispatch_permit_returns_when_a_handler_unwinds() {
        let dispatch = RequestAdmission::new(1);
        let transport = BoundedStdioTransport::new(dispatch.clone(), McpResultMode::Dual);

        let unwind = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let mut request = incoming_request(serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "tools/call",
                "params": {"name": "files", "arguments": {}}
            }));
            transport
                .admit_message(&mut request)
                .expect("admit tool call");
            assert_eq!(dispatch.available_permits(), 0);
            panic!("injected handler panic");
        }));

        assert!(unwind.is_err());
        assert_eq!(dispatch.available_permits(), 1);
    }

    #[test]
    fn transport_dispatch_permit_returns_when_a_request_is_cancelled() {
        let dispatch = RequestAdmission::new(1);
        let transport = BoundedStdioTransport::new(dispatch.clone(), McpResultMode::Dual);
        let mut request = incoming_request(serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {"name": "files", "arguments": {}}
        }));
        transport
            .admit_message(&mut request)
            .expect("admit tool call");
        assert_eq!(dispatch.available_permits(), 0);

        let mut cancellation = incoming_request(serde_json::json!({
            "jsonrpc": "2.0",
            "method": "notifications/cancelled",
            "params": {"requestId": 1, "reason": "no longer needed"}
        }));
        transport
            .admit_message(&mut cancellation)
            .expect("admit cancellation");
        assert_eq!(dispatch.available_permits(), 1);

        drop(request);
        assert_eq!(dispatch.available_permits(), 1);
    }

    #[test]
    fn request_admission_has_an_exact_fail_fast_boundary() {
        let (server, _) = LeanTokenMcp::pending();
        let permits = (0..DEFAULT_ACTIVE_TOOL_CALL_CAPACITY)
            .map(|_| {
                server
                    .request_admission
                    .try_admit()
                    .expect("admitted tool call")
            })
            .collect::<Vec<_>>();
        assert_eq!(server.request_admission.available_permits(), 0);
        assert!(matches!(
            server.request_admission.try_admit(),
            Err(crate::Error::RetrievalOverloaded)
        ));
        drop(permits);
        assert_eq!(
            server.request_admission.available_permits(),
            DEFAULT_ACTIVE_TOOL_CALL_CAPACITY
        );
    }

    #[test]
    fn cloned_servers_share_admission_but_separate_instances_do_not() {
        let (server, _) = LeanTokenMcp::pending();
        let clone = server.clone();
        let permits = (0..DEFAULT_ACTIVE_TOOL_CALL_CAPACITY)
            .map(|_| {
                clone
                    .request_admission
                    .try_admit()
                    .expect("admitted clone tool call")
            })
            .collect::<Vec<_>>();

        assert!(matches!(
            server.request_admission.try_admit(),
            Err(crate::Error::RetrievalOverloaded)
        ));

        let (independent, _) = LeanTokenMcp::pending();
        let independent_permit = independent
            .request_admission
            .try_admit()
            .expect("separate server has independent capacity");
        drop(independent_permit);
        drop(permits);
    }

    #[test]
    fn capacity_errors_are_structured_retryable_tool_results() {
        let (server, _) = LeanTokenMcp::pending();
        for (error, reason) in [
            (
                crate::Error::RetrievalOverloaded,
                "retrieval_capacity_exhausted",
            ),
            (
                crate::Error::RetrievalQueueTimeout,
                "retrieval_queue_timeout",
            ),
        ] {
            let result = server
                .service_result::<()>(Err(error))
                .expect("capacity result");
            let structured = result.structured_content.expect("structured result");
            assert_eq!(structured["status"], "retryable");
            assert_eq!(structured["reason"], reason);
            assert_eq!(structured["retry_after_ms"], 500);
            assert_eq!(result.is_error, Some(false));
        }
    }

    #[tokio::test]
    async fn initialization_and_tool_listing_bypass_saturated_tool_admission() {
        let (server, _) = LeanTokenMcp::pending();
        let permits = (0..DEFAULT_ACTIVE_TOOL_CALL_CAPACITY)
            .map(|_| {
                server
                    .request_admission
                    .try_admit()
                    .expect("saturate tool admission")
            })
            .collect::<Vec<_>>();
        let (client_stream, server_stream) = tokio::io::duplex(64 * 1024);
        let server_start = tokio::spawn(async move {
            serve_server(server, server_stream)
                .await
                .expect("start server")
        });
        let mut client = serve_client((), client_stream)
            .await
            .expect("initialize client while saturated");
        let mut server = server_start.await.expect("join server startup");

        assert_eq!(
            client
                .peer()
                .peer_info()
                .expect("initialize response")
                .server_info
                .name,
            "leantoken"
        );
        assert_eq!(
            client
                .peer()
                .list_all_tools()
                .await
                .expect("list tools while saturated")
                .len(),
            8
        );

        drop(permits);
        client.close().await.expect("close client");
        server.close().await.expect("close server");
    }

    #[tokio::test]
    async fn repository_identity_is_checked_before_tool_admission() {
        let root = tempfile::tempdir().expect("root");
        let config =
            Config::discover(root.path(), Some(root.path().join("index.sqlite"))).expect("config");
        let services = Arc::new(Services::open(config).expect("services"));
        let server = LeanTokenMcp::new(Arc::clone(&services));
        let permits = (0..DEFAULT_ACTIVE_TOOL_CALL_CAPACITY)
            .map(|_| {
                server
                    .request_admission
                    .try_admit()
                    .expect("saturate tool admission")
            })
            .collect::<Vec<_>>();
        let called = Arc::new(AtomicBool::new(false));
        let operation_called = Arc::clone(&called);

        let error = server
            .run_admitted::<(), _, _>(
                services,
                Some("not-this-repository".into()),
                move |_| async move {
                    operation_called.store(true, Ordering::SeqCst);
                    Ok(())
                },
            )
            .await
            .expect_err("identity mismatch must win over overload");

        assert_eq!(
            error.data,
            Some(serde_json::json!({
                "category": "repository_identity_mismatch",
                "expected_repository_id": "not-this-repository",
                "actual_repository_id": server.services(&server.services.get())
                    .expect("ready services")
                    .repository_id(),
            }))
        );
        assert!(!called.load(Ordering::SeqCst));
        drop(permits);
    }

    #[tokio::test]
    async fn savings_is_covered_by_protocol_admission() {
        let root = tempfile::tempdir().expect("root");
        let config =
            Config::discover(root.path(), Some(root.path().join("index.sqlite"))).expect("config");
        let services = Arc::new(Services::open(config).expect("services"));
        let server = LeanTokenMcp::new(services);
        let permits = (0..DEFAULT_ACTIVE_TOOL_CALL_CAPACITY)
            .map(|_| {
                server
                    .request_admission
                    .try_admit()
                    .expect("saturate tool admission")
            })
            .collect::<Vec<_>>();

        let result = server
            .leantoken_savings(Parameters(SavingsMcpRequest {}))
            .await
            .expect("retryable savings response");
        assert_eq!(
            result
                .structured_content
                .as_ref()
                .and_then(|value| value["reason"].as_str()),
            Some("retrieval_capacity_exhausted")
        );
        drop(permits);
    }

    #[test]
    fn startup_failures_expose_only_allowlisted_guidance() {
        let marker = "/secret/repository";
        let failure =
            StartupFailure::from_error(&crate::Error::UnsafeRepositoryRoot(marker.into()));
        let result = tool_unavailable(failure.reason, failure.message);
        assert_eq!(
            result
                .structured_content
                .as_ref()
                .and_then(|value| value["reason"].as_str()),
            Some("unsafe_repository_root")
        );
        assert!(!serde_json::to_string(&result).unwrap().contains(marker));
    }

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
        for error in [
            crate::Error::RetryableConflict(crate::error::RetryableOperation::Retrieval),
            crate::Error::ReconciliationFailed(Arc::new(crate::Error::RetryableConflict(
                crate::error::RetryableOperation::Retrieval,
            ))),
        ] {
            let result = server
                .service_result::<()>(Err(error))
                .expect("tool result");

            assert_eq!(result.is_error, Some(false));
            let structured = result.structured_content.expect("structured retry result");
            assert_eq!(structured["status"], "retryable");
            assert_eq!(structured["reason"], "repository_changed");
            assert_eq!(structured["retry_after_ms"], 100);
        }
    }

    #[tokio::test]
    async fn ready_operation_is_not_retried() {
        let (_server, mcp_services) = LeanTokenMcp::pending();
        let calls = std::sync::atomic::AtomicUsize::new(0);
        let waits = std::sync::atomic::AtomicUsize::new(0);

        let result = retry_after_initial_index_with_policy(
            "files",
            &mcp_services,
            CancellationToken::new(),
            Duration::from_secs(30),
            |_| {
                waits.fetch_add(1, Ordering::AcqRel);
                std::future::ready(Ok::<(), crate::Error>(()))
            },
            || {
                calls.fetch_add(1, Ordering::AcqRel);
                std::future::ready(Ok::<_, crate::Error>(42))
            },
        )
        .await
        .expect("ready operation");

        assert_eq!(result, 42);
        assert_eq!(calls.load(Ordering::Acquire), 1);
        assert_eq!(waits.load(Ordering::Acquire), 0);
    }

    #[tokio::test(start_paused = true)]
    async fn initial_index_retry_is_bounded() {
        let (_server, mcp_services) = LeanTokenMcp::pending();
        let calls = std::sync::atomic::AtomicUsize::new(0);
        let waits = std::sync::atomic::AtomicUsize::new(0);

        let error = retry_after_initial_index_with_policy(
            "files",
            &mcp_services,
            CancellationToken::new(),
            Duration::from_millis(250),
            |cancellation| {
                waits.fetch_add(1, Ordering::AcqRel);
                async move {
                    cancellation.cancelled().await;
                    Err(crate::Error::Cancelled)
                }
            },
            || {
                calls.fetch_add(1, Ordering::AcqRel);
                std::future::ready(Err::<(), _>(crate::Error::IndexNotReady))
            },
        )
        .await
        .expect_err("generation-zero operation must time out");

        assert!(matches!(error, crate::Error::IndexNotReady));
        assert_eq!(calls.load(Ordering::Acquire), 1);
        assert_eq!(waits.load(Ordering::Acquire), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn initial_index_retry_returns_first_published_result() {
        let (_server, mcp_services) = LeanTokenMcp::pending();
        let ready = Arc::new(AtomicBool::new(false));
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let operation_ready = Arc::clone(&ready);
        let operation_calls = Arc::clone(&calls);
        let waiting = tokio::spawn(async move {
            retry_after_initial_index_with_policy(
                "files",
                &mcp_services,
                CancellationToken::new(),
                Duration::from_secs(1),
                |_| async {
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    Ok(())
                },
                move || {
                    operation_calls.fetch_add(1, Ordering::AcqRel);
                    let result = if operation_ready.load(Ordering::Acquire) {
                        Ok(42)
                    } else {
                        Err(crate::Error::IndexNotReady)
                    };
                    std::future::ready(result)
                },
            )
            .await
        });
        tokio::task::yield_now().await;
        assert_eq!(calls.load(Ordering::Acquire), 1);

        ready.store(true, Ordering::Release);
        tokio::time::advance(Duration::from_millis(100)).await;

        assert_eq!(
            waiting
                .await
                .expect("join readiness retry")
                .expect("published result"),
            42
        );
        assert_eq!(calls.load(Ordering::Acquire), 2);
    }

    #[tokio::test]
    async fn initial_index_retry_honors_cancellation() {
        let (_server, mcp_services) = LeanTokenMcp::pending();
        let cancellation = CancellationToken::new();
        cancellation.cancel();

        let error = retry_after_initial_index_with_policy(
            "files",
            &mcp_services,
            cancellation,
            Duration::from_secs(30),
            |_| std::future::pending::<crate::Result<()>>(),
            || std::future::ready(Err::<(), _>(crate::Error::IndexNotReady)),
        )
        .await
        .expect_err("cancelled retry must stop");

        assert!(matches!(error, crate::Error::Cancelled));
    }

    #[tokio::test]
    async fn initial_index_retry_stops_when_runtime_fails() {
        let (_server, mcp_services) = LeanTokenMcp::pending();
        let waiting_services = mcp_services.clone();
        let waiting = tokio::spawn(async move {
            retry_after_initial_index_with_policy(
                "files",
                &waiting_services,
                CancellationToken::new(),
                Duration::from_secs(30),
                |_| std::future::pending::<crate::Result<()>>(),
                || std::future::ready(Err::<(), _>(crate::Error::IndexNotReady)),
            )
            .await
        });
        tokio::task::yield_now().await;
        assert!(!waiting.is_finished());

        mcp_services.set_failed(&crate::Error::McpRuntimeStopped);

        let error = waiting
            .await
            .expect("join initial-index retry")
            .expect_err("runtime failure must interrupt readiness retry");
        assert!(matches!(error, crate::Error::McpRuntimeStopped));
    }

    #[tokio::test]
    async fn initial_index_retry_prefers_runtime_failure_over_readiness_error() {
        let (_server, mcp_services) = LeanTokenMcp::pending();
        let failed_services = mcp_services.clone();

        let error = retry_after_initial_index_with_policy(
            "files",
            &mcp_services,
            CancellationToken::new(),
            Duration::from_secs(30),
            move |_| async move {
                failed_services.set_failed(&crate::Error::McpRuntimeStopped);
                Err(crate::Error::IndexNotReady)
            },
            || std::future::ready(Err::<(), _>(crate::Error::IndexNotReady)),
        )
        .await
        .expect_err("terminal runtime failure must supersede readiness error");

        assert!(matches!(error, crate::Error::McpRuntimeStopped));
    }

    #[tokio::test]
    async fn protocol_initialization_wait_observes_transition() {
        let (_server, services) = LeanTokenMcp::pending();
        let waiting_services = services.clone();
        let waiting = tokio::spawn(async move {
            waiting_services.wait_initialized().await;
        });
        tokio::task::yield_now().await;
        assert!(!waiting.is_finished());

        services.mark_protocol_initialized();

        tokio::time::timeout(Duration::from_secs(1), waiting)
            .await
            .expect("initialization wait must wake")
            .expect("join initialization wait");
    }

    #[tokio::test(start_paused = true)]
    async fn starting_service_wait_is_bounded() {
        let (_server, services) = LeanTokenMcp::pending();

        let state = services
            .wait_for_services(
                services.get(),
                CancellationToken::new(),
                tokio::time::Instant::now() + Duration::from_millis(250),
            )
            .await
            .expect("bounded wait");

        assert!(matches!(state, McpServiceState::Starting(_)));
    }

    #[tokio::test]
    async fn starting_service_wait_observes_terminal_transition() {
        let (_server, services) = LeanTokenMcp::pending();
        let waiting_services = services.clone();
        let waiting = tokio::spawn(async move {
            waiting_services
                .wait_for_services(
                    waiting_services.get(),
                    CancellationToken::new(),
                    tokio::time::Instant::now() + Duration::from_secs(1),
                )
                .await
        });
        tokio::task::yield_now().await;
        assert!(!waiting.is_finished());

        services.set_failed(&crate::Error::McpRuntimeStopped);

        let state = waiting
            .await
            .expect("join service wait")
            .expect("terminal service state");
        assert!(matches!(state, McpServiceState::Failed { .. }));
    }

    #[tokio::test]
    async fn starting_service_wait_honors_cancellation() {
        let (_server, services) = LeanTokenMcp::pending();
        let cancellation = CancellationToken::new();
        cancellation.cancel();

        let error = services
            .wait_for_services(
                services.get(),
                cancellation,
                tokio::time::Instant::now() + Duration::from_secs(30),
            )
            .await
            .expect_err("cancelled startup wait must stop");

        assert!(matches!(error, crate::Error::Cancelled));
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

        let selector = into_mcp_error(crate::Error::InvalidJsonSelector {
            stage: "evaluate",
            offset: 6,
            line: 1,
            column: 7,
            reason: "Runtime error: Argument 0 expects type array, given number".into(),
        });
        assert_eq!(selector.code, rmcp::model::ErrorCode::INVALID_PARAMS);
        assert_eq!(
            selector.data,
            Some(serde_json::json!({
                "category": "invalid_json_selector",
                "field": "JMESPath expression",
                "stage": "evaluate",
                "offset": 6,
                "line": 1,
                "column": 7,
                "reason": "Runtime error: Argument 0 expects type array, given number",
            }))
        );

        let syntax = into_mcp_error(crate::Error::InvalidJson {
            syntax_category: "syntax",
            byte_offset: 12,
            line: 1,
            column: 13,
            reason: "trailing comma at line 1 column 13".into(),
        });
        assert_eq!(syntax.code, rmcp::model::ErrorCode::INVALID_PARAMS);
        assert_eq!(
            syntax
                .data
                .as_ref()
                .and_then(|data| data["byte_offset"].as_u64()),
            Some(12)
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
        let (request, _, _, _, _, _, _) = request.into_parts(37);
        assert_eq!(request.token_budget, 37);
        assert!(!request.verbose_diagnostics);
        let null_limit = serde_json::from_value::<ContextMcpRequest>(serde_json::json!({
            "task": "find answer",
            "max_response_tokens": null
        }))
        .expect("null response limit is equivalent to omission");
        let (_, _, _, _, options, _, _) = null_limit.into_parts(37);
        assert_eq!(options.max_response_tokens(), None);

        let request = serde_json::from_value::<ContextMcpRequest>(serde_json::json!({
            "task": "find answer",
            "token_budget": 23,
            "max_response_tokens": 47,
            "focus_paths": ["src/**"],
            "strict_focus_paths": true,
            "minimum_fragments_per_focus_path": 2,
            "required_evidence": [{
                "path": "paper/**",
                "queries": ["failure boundary", "disclosure"],
                "minimum_query_matches": 2
            }],
            "changed_paths": ["src/lib.rs"],
            "strict_changed_paths": true,
            "verbose_diagnostics": true,
            "workflow_evidence": {
                "failure_traces": ["error[E0001]"],
                "symbols": ["answer"],
                "paths": ["src/lib.rs"],
                "test_intents": ["answer regression"]
            }
        }))
        .expect("context request with a budget");
        let (request, _, workflow_evidence, _, options, _, _) = request.into_parts(37);
        assert_eq!(request.token_budget, 23);
        assert_eq!(options.max_response_tokens(), Some(47));
        assert_eq!(request.focus_paths, ["src/**"]);
        assert!(request.strict_focus_paths);
        assert_eq!(request.minimum_fragments_per_focus_path, Some(2));
        assert_eq!(request.required_evidence.len(), 1);
        assert_eq!(request.required_evidence[0].path, "paper/**");
        assert_eq!(
            request.required_evidence[0].queries,
            ["failure boundary", "disclosure"]
        );
        assert_eq!(request.required_evidence[0].minimum_query_matches, 2);
        assert_eq!(request.changed_paths, ["src/lib.rs"]);
        assert!(request.strict_changed_paths);
        assert!(request.verbose_diagnostics);
        assert_eq!(workflow_evidence.failure_traces, ["error[E0001]"]);
        assert_eq!(workflow_evidence.symbols, ["answer"]);
        assert_eq!(workflow_evidence.paths, ["src/lib.rs"]);
        assert_eq!(workflow_evidence.test_intents, ["answer regression"]);

        for invalid in [0, MAX_OUTPUT_TOKENS + 1] {
            let request = serde_json::from_value::<ContextMcpRequest>(serde_json::json!({
                "task": "find answer",
                "max_response_tokens": invalid
            }))
            .expect("syntactically valid context response limit");
            assert!(request.validate_limits(McpLimitPolicy::DEFAULT).is_err());
        }
        assert!(
            serde_json::from_value::<ContextMcpRequest>(serde_json::json!({
                "task": "find answer",
                "max_response_tokens": -1
            }))
            .is_err()
        );
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
        let (_, _, _, _, _, _, handoff) = request.into_parts(37);
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
        assert!(descriptions["savings"].contains("explicitly unobserved task outcomes"));
        assert!(descriptions["savings"].contains("not claims about task success"));
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
            let (request, _, _, _) = request.into_parts();
            assert_eq!(request.start_line, Some(10));
            assert_eq!(request.end_line, Some(20));
        }
        let heading = serde_json::from_value::<ReadMcpRequest>(serde_json::json!({
            "path": "README.md",
            "target": {"kind": "heading", "name": "Installation", "occurrence": 2}
        }))
        .expect("Markdown heading target");
        assert!(heading.validate_limits(McpLimitPolicy::DEFAULT).is_ok());
        let (heading, _, _, _) = heading.into_parts();
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
        let (continuation, _, _, _) = continuation.into_parts();
        assert_eq!(continuation.continuation_cursor.as_deref(), Some("opaque"));
        assert!(continuation.symbol.is_none());
        assert!(continuation.heading.is_none());
        assert!(continuation.heading_occurrence.is_none());
        assert!(continuation.start_line.is_none());
        assert!(continuation.end_line.is_none());
    }

    #[test]
    fn retrieval_response_budget_schemas_are_optional_and_bounded() {
        let tools = LeanTokenMcp::tool_router().list_all();
        for name in [
            "context", "read", "search", "outline", "files", "history", "json",
        ] {
            let tool = tools
                .iter()
                .find(|tool| tool.name == name)
                .unwrap_or_else(|| panic!("{name} tool"));
            let schema = serde_json::Value::Object((*tool.input_schema).clone());
            assert_eq!(
                schema.pointer("/properties/max_response_tokens/minimum"),
                Some(&serde_json::json!(1)),
                "{name}"
            );
            assert_eq!(
                schema.pointer("/properties/max_response_tokens/maximum"),
                Some(&serde_json::json!(32_000)),
                "{name}"
            );
            assert_eq!(
                schema.pointer("/properties/max_response_tokens/default"),
                Some(&serde_json::Value::Null),
                "{name}"
            );
        }
    }

    #[test]
    fn context_focus_candidate_schema_exposes_generation_bounds() {
        let context = LeanTokenMcp::tool_router()
            .list_all()
            .into_iter()
            .find(|tool| tool.name == "context")
            .expect("context tool");
        let schema = serde_json::Value::Object((*context.input_schema).clone());
        assert_eq!(
            schema.pointer("/properties/focus_paths/maxItems"),
            Some(&serde_json::json!(32))
        );
        assert_eq!(
            schema.pointer("/properties/minimum_fragments_per_focus_path/maximum"),
            Some(&serde_json::json!(8))
        );
    }

    #[test]
    fn retrieval_response_budget_limits_are_validated_for_every_tool() {
        macro_rules! assert_invalid {
            ($ty:ty, $base:expr) => {
                for invalid in [0, MAX_OUTPUT_TOKENS + 1] {
                    let mut value = $base;
                    value["max_response_tokens"] = serde_json::json!(invalid);
                    let request =
                        serde_json::from_value::<$ty>(value).expect("deserialize request");
                    assert!(
                        request.validate_limits(McpLimitPolicy::DEFAULT).is_err(),
                        "{} accepted {invalid}",
                        stringify!($ty)
                    );
                }
            };
        }

        assert_invalid!(FilesMcpRequest, serde_json::json!({"operation": "tree"}));
        assert_invalid!(SearchMcpRequest, serde_json::json!({"query": "needle"}));
        assert_invalid!(
            OutlineMcpRequest,
            serde_json::json!({"paths": ["src/lib.rs"]})
        );
        assert_invalid!(
            ReadMcpRequest,
            serde_json::json!({
                "path": "src/lib.rs",
                "target": {"kind": "lines", "start": 1, "end": 1}
            })
        );
        assert_invalid!(
            HistoryMcpRequest,
            serde_json::json!({
                "operation": {
                    "kind": "read_symbol",
                    "path": "src/lib.rs",
                    "symbol": "owner",
                    "revision": "HEAD"
                }
            })
        );
        assert_invalid!(
            JsonMcpRequest,
            serde_json::json!({
                "operation": {"kind": "query", "path": "data.json"}
            })
        );
    }

    #[test]
    fn receipt_id_maps_to_the_service_request() {
        let request = serde_json::from_value::<ReadMcpRequest>(serde_json::json!({
            "path": "README.md",
            "receipt_id": "r0000000000000001",
            "target": {"kind": "lines", "start": 1, "end": 2}
        }))
        .expect("read request with receipt");
        let (request, _, _, _) = request.into_parts();
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
        let (call, _, _) = request.into_parts().expect("history parts");
        let HistoryMcpCall::Single(request) = call else {
            panic!("expected single-symbol history call");
        };
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
    fn diff_symbols_history_maps_targets_cursor_and_response_budget() {
        let request = serde_json::from_value::<HistoryMcpRequest>(serde_json::json!({
            "operation": {
                "kind": "diff_symbols",
                "targets": [
                    {
                        "path": "src/old.rs",
                        "symbol": "old_name",
                        "head_path": "src/new.rs",
                        "head_symbol": "new_name"
                    }
                ],
                "base_revision": "main~1",
                "head_revision": "main"
            },
            "max_results": 1,
            "max_tokens": 500,
            "max_response_tokens": 900,
            "cursor": "history-cursor"
        }))
        .expect("batched history request");
        request
            .validate_limits(McpLimitPolicy::DEFAULT)
            .expect("batched history limits");
        let (call, options, _) = request.into_parts().expect("batched history parts");
        assert_eq!(options.max_response_tokens(), Some(900));
        let HistoryMcpCall::DiffSymbols(request) = call else {
            panic!("expected batched-symbol history call");
        };
        assert_eq!(request.base_revision, "main~1");
        assert_eq!(request.head_revision, "main");
        assert_eq!(request.max_results, Some(1));
        assert_eq!(request.max_tokens, Some(500));
        assert_eq!(request.cursor.as_deref(), Some("history-cursor"));
        assert_eq!(request.targets.len(), 1);
        assert_eq!(request.targets[0].path, "src/old.rs");
        assert_eq!(request.targets[0].symbol, "old_name");
        assert_eq!(request.targets[0].head_path.as_deref(), Some("src/new.rs"));
        assert_eq!(request.targets[0].head_symbol.as_deref(), Some("new_name"));

        let single_with_cursor = serde_json::from_value::<HistoryMcpRequest>(serde_json::json!({
            "operation": {
                "kind": "read_symbol",
                "path": "src/lib.rs",
                "symbol": "item",
                "revision": "main"
            },
            "cursor": "not-valid-here"
        }))
        .expect("single history request");
        assert!(matches!(
            single_with_cursor
                .into_parts()
                .expect_err("cursor is exclusive to diff_symbols"),
            crate::Error::InvalidInput {
                field: "cursor",
                reason: "is only valid for diff_symbols"
            }
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
        let (request, _, execution, _) = request.into_parts();
        assert_eq!(request.max_items, Some(500));
        assert!(request.cursor.is_none());
        assert_eq!(execution, JsonExecutionOptions::mcp(None));
        assert!(matches!(
            request.operation,
            JsonOperation::NumericSummary {
                path,
                selector: Some(JsonSelector::Jmespath { expression }),
            } if path == "artifacts/results.json" && expression == "runs[].score"
        ));

        let request = serde_json::from_value::<JsonMcpRequest>(serde_json::json!({
            "operation": {
                "kind": "query",
                "path": "artifacts/results.json",
                "projection": "keys"
            },
            "depth": 1,
            "cursor": "j2:source:query:2"
        }))
        .expect("paged JSON request");
        let (request, _, execution, _) = request.into_parts();
        assert_eq!(request.cursor.as_deref(), Some("j2:source:query:2"));
        assert_eq!(execution, JsonExecutionOptions::mcp(Some(1)));
        assert!(matches!(
            request.operation,
            JsonOperation::Query {
                projection: JsonProjection::Keys,
                ..
            }
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
        let (request, _, _, _, _) = request.into_parts();

        assert_eq!(
            request.cursor.as_deref(),
            Some("12:outline:34:0000000000000000")
        );
    }

    #[test]
    fn compact_projections_map_to_service_requests() {
        let files = serde_json::from_value::<FilesMcpRequest>(serde_json::json!({
            "operation": "tree"
        }))
        .expect("default files projection");
        let (_, projection, _, _, _) = files.into_parts();
        assert_eq!(projection, FilesMcpProjection::Full);

        let files = serde_json::from_value::<FilesMcpRequest>(serde_json::json!({
            "operation": "find",
            "query": "service",
            "projection": "paths"
        }))
        .expect("path projection");
        let (_, projection, _, _, _) = files.into_parts();
        assert_eq!(projection, FilesMcpProjection::Paths);

        let search = serde_json::from_value::<SearchMcpRequest>(serde_json::json!({
            "query": "Services"
        }))
        .expect("default search projection");
        let (_, projection, _, _, _) = search.into_parts();
        assert_eq!(projection, SearchMcpProjection::Full);

        let search = serde_json::from_value::<SearchMcpRequest>(serde_json::json!({
            "query": "Services",
            "projection": "grouped"
        }))
        .expect("grouped projection");
        let (_, projection, _, _, _) = search.into_parts();
        assert_eq!(projection, SearchMcpProjection::Grouped);

        let outline = serde_json::from_value::<OutlineMcpRequest>(serde_json::json!({
            "paths": ["src/services.rs"]
        }))
        .expect("default outline projection");
        let (_, projection, _, _, _) = outline.into_parts();
        assert_eq!(projection, OutlineMcpProjection::Full);

        let outline = serde_json::from_value::<OutlineMcpRequest>(serde_json::json!({
            "paths": ["src/services.rs"],
            "projection": "signatures"
        }))
        .expect("signature projection");
        let (_, projection, _, _, _) = outline.into_parts();
        assert_eq!(projection, OutlineMcpProjection::Signatures);
    }
}
