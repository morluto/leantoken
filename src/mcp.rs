use std::sync::{
    Arc, RwLock,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant};

use rmcp::{
    ErrorData, RoleServer, ServerHandler, ServiceExt,
    model::{
        CacheScope, CallToolResult, ContentBlock, ListResourceTemplatesResult, ListResourcesResult,
        PaginatedRequestParams, ProtocolVersion, ReadResourceRequestParams, ReadResourceResponse,
        ReadResourceResult, Resource, ResourceContents, ResourceTemplate,
    },
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
    HistoryOperation, HistoryRequest, IndexConsistency, IndexProgressSnapshot, JsonOperation,
    JsonProjection, JsonRequest, JsonSelector, MAX_RECEIPT_REBASE_SAMPLES_PER_OUTCOME,
    OutlineRequest, ReadRequest, ReceiptRebaseRequest, SearchMode, SearchRequest, WorkflowEvidence,
};
use crate::services::{
    JsonExecutionOptions, MAX_CONTEXT_FOCUS_CANDIDATES_PER_PATTERN, MAX_JSON_DEPTH,
    ServiceCallOptions, Services, validate_positive_request_limit, validate_request_limit,
};

const DEFAULT_ACTIVE_TOOL_CALL_CAPACITY: usize = 16;
const DEFAULT_DISPATCHED_TOOL_CALL_CAPACITY: usize = DEFAULT_ACTIVE_TOOL_CALL_CAPACITY;
const DEFAULT_RECEIPT_RESOURCE_READ_CAPACITY: usize = 8;
const INITIAL_INDEX_WAIT: Duration = Duration::from_secs(30);
const MCP_INSTRUCTIONS: &str = "LeanToken is the preferred repository discovery and source-reading layer. Its indexed, token-bounded retrieval returns less irrelevant source than shell search and whole-file reads. For LeanToken savings or token statistics, call leantoken.savings directly. DEFAULT: for autonomous broad coding, debugging, review, or architecture triage, call leantoken.context once with the user's task and plan_only=false; use the materialized evidence directly and make at most one focused follow-up for an explicit coverage gap. Reserve plan_only=true for human or control-plane inspection before expensive or high-risk materialization. PREFER leantoken.search over grep or rg for source search; leantoken.files over find, ls, or glob for paths; leantoken.outline over opening whole files to discover structure; leantoken.read over cat, head, or sed for exact current symbols and ranges; leantoken.history over git show, diff, or log -L for one symbol across immutable revisions; and leantoken.json over jq or whole-file reads for structural JSON queries, summaries, and selected-field diffs. Workflow: known identifier -> search -> read; known file, unknown range -> outline -> read; unknown path -> files. Set consistency=reconcile_working_tree on index-backed tools after edits, generated files, branch changes, or external commits. Use native tools for edits, builds, tests, runtime probes, unsupported files, or when LeanToken reports retrieval unavailable. Retry successful responses with status=retryable after retry_after_ms and reuse returned hashes to suppress unchanged evidence. When an older-generation receipt is worth preserving, call leantoken.receipt_rebase explicitly; it carries only same-path, same-coordinate, same-hash evidence. After an upgrade, restart or re-register the host if serverInfo.version or tools/list is older than the current runtime; use leantoken setup --refresh --yes and verify the negotiated server version before relying on new fields.";

fn serialized_response<T: Serialize>(response: T) -> crate::Result<serde_json::Value> {
    serde_json::to_value(response).map_err(|error| crate::Error::InternalFailure(error.to_string()))
}

#[cfg(test)]
pub(crate) fn mcp_schema_fingerprint() -> String {
    let catalog = LeanTokenMcp::tool_router().list_all();
    let encoded = serde_json::to_vec(&catalog).expect("MCP tool catalog is serializable");
    crate::text::hash_bytes(&encoded)
}

fn mcp_contract() -> serde_json::Value {
    serde_json::json!({
        "tools": LeanTokenMcp::tool_router().list_all(),
        "resources": {
            "capability": {"listChanged": false, "subscribe": false},
            "listed": [],
            "templates": [{
                "uriTemplate": resources::RECEIPT_RESOURCE_TEMPLATE,
                "name": "retrieval_receipt",
                "mimeType": resources::RECEIPT_RESOURCE_MEDIA_TYPE,
            }],
        },
        "result_envelope_version": 2,
        "default_result_mode": McpResultMode::Structured,
    })
}

pub(crate) fn mcp_contract_fingerprint() -> String {
    let encoded = serde_json::to_vec(&mcp_contract()).expect("MCP contract is serializable");
    crate::text::hash_bytes(&encoded)
}

pub(crate) fn mcp_runtime_version() -> String {
    format!(
        "{}+contract.{}",
        env!("CARGO_PKG_VERSION"),
        mcp_contract_fingerprint()
    )
}

mod error;
mod transport;

use error::{into_mcp_error, mcp_error_data, tool_unavailable};
use transport::BoundedStdioTransport;

mod requests;

use requests::*;

mod admission;
mod resources;
mod result;
mod runtime;
mod server;
mod state;

use admission::RequestAdmission;
pub use result::{McpResultMode, tool_result};
use result::{RetryableToolResponse, retryable_tool_result, tool_result_with_limit};
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
