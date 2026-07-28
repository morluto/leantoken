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
    ContextRequest, ContextRequiredEvidence, ContextResponseProfile, ContextWorkflow,
    DiffSymbolsRequest, DiffSymbolsTarget, FileOperation, FilesRequest, HandoffManifestRequest,
    HistoryOperation, HistoryRequest, IndexConsistency, JsonOperation, JsonProjection, JsonRequest,
    JsonSelector, OutlineRequest, ReadRequest, SearchMode, SearchRequest, WorkflowEvidence,
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

pub(crate) fn mcp_schema_fingerprint() -> String {
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

mod requests;

use requests::*;

mod admission;
mod compatibility;
mod result;
mod runtime;
mod server;
mod state;

use admission::RequestAdmission;
use compatibility::McpResultModeState;
pub(crate) use compatibility::resolve_auto_result_mode;
pub use compatibility::{McpResultModeResolution, McpResultModeResolutionReason};
pub use result::{McpResultMode, tool_result};
use result::{RetryableToolResponse, retryable_tool_result};
use runtime::RetrievalPreparation;
#[cfg(test)]
use runtime::retry_after_initial_index_with_policy;
pub use server::LeanTokenMcp;
pub use state::McpServices;
#[cfg(test)]
use state::StartupFailure;
use state::{McpLimitPolicy, McpServiceState};

include!("mcp/tools.rs");

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
    let transport =
        BoundedStdioTransport::new(server.request_dispatch.clone(), server.result_mode.clone());

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
