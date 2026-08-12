use std::sync::Arc;

use rmcp::{
    ErrorData, RoleServer, ServerHandler, ServiceExt,
    handler::server::wrapper::Parameters,
    model::{CallToolResult, ProtocolVersion},
    service::RequestContext,
    tool, tool_handler, tool_router,
};
use schemars::{JsonSchema, Schema, SchemaGenerator};
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

use crate::Config;
use crate::config::{DEFAULT_CONTEXT_FRAGMENTS, MAX_CONTEXT_LINES, MAX_OUTPUT_TOKENS, MAX_RESULTS};
use crate::model::{
    ContextRequest, ContextRequiredEvidence, ContextResponseProfile, ContextWorkflow,
    IndexConsistency, IndexProgressSnapshot, NonEmptyText, OutlineRequest, ReadRequest, SearchMode,
    SearchRequest, SymbolIdentity, WorkflowEvidence,
};
use crate::repository::{RepositoryPath, RepositoryPattern};
use crate::services::{
    MAX_CONTEXT_FOCUS_CANDIDATES_PER_PATTERN, ServiceCallOptions, Services,
    validate_positive_request_limit, validate_request_limit,
};

const DEFAULT_ACTIVE_TOOL_CALL_CAPACITY: usize = 16;
const MCP_INSTRUCTIONS: &str = "One LeanToken server owns one repository. Call refresh explicitly to acquire the working tree and atomically publish a complete immutable generation. Search, outline, read, and context always use one published generation and never reopen repository files. Use native tools for edits, builds, tests, live dirty reads, and Git history. A continuation cursor is valid only for the repository, request, and generation that produced it.";

fn serialized_response<T: Serialize>(response: T) -> crate::Result<serde_json::Value> {
    serde_json::to_value(response)
        .map_err(|error| crate::Error::SerializationFailure(error.to_string()))
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

use error::{into_tool_error, mcp_error_data, tool_unavailable, visible_mcp_error};

mod requests;

use requests::*;

mod admission;
mod result;
mod runtime;
mod server;
mod state;

use admission::RequestAdmission;
pub use result::{McpResultMode, tool_result};
use result::{RetryableToolResponse, retryable_tool_result, tool_result_with_limit};
use runtime::RetrievalPreparation;
pub use server::LeanTokenMcp;
use state::McpLimitPolicy;

mod tools;

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
    let transport = rmcp::transport::stdio();

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
