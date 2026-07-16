use std::sync::Arc;

use rmcp::{
    ErrorData, RoleServer, ServerHandler, ServiceExt,
    handler::server::wrapper::Parameters,
    model::{CallToolResult, ContentBlock},
    service::RequestContext,
    tool, tool_handler, tool_router,
    transport::stdio,
};
use serde::Serialize;
use tokio_util::sync::CancellationToken;

use crate::model::{ContextRequest, FilesRequest, OutlineRequest, ReadRequest, SearchRequest};
use crate::services::Services;

/// LeanToken MCP server.
#[derive(Clone)]
pub struct LeanTokenMcp {
    services: Arc<Services>,
    result_mode: McpResultMode,
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
            services,
            result_mode: McpResultMode::Dual,
        }
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
}

#[tool_router]
impl LeanTokenMcp {
    #[tool(
        name = "leantoken_files",
        description = "Start here when repository paths are unknown. Returns only a compact tree or path matches: tree for hierarchy, find for fuzzy names, glob for patterns. Then use outline or search; no source bodies are returned."
    )]
    async fn leantoken_files(
        &self,
        Parameters(req): Parameters<FilesRequest>,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, ErrorData> {
        let resp = self
            .services
            .files_cancellable(req, context.ct.clone())
            .await
            .map_err(into_mcp_error)?;
        self.result(resp)
    }

    #[tool(
        name = "leantoken_search",
        description = "Ranked, token-bounded source lookup. Use symbol for definitions, reference for usages, identifier for exact names, text for substrings, regex for patterns, or auto to combine evidence. Follow a hit with leantoken_read for the exact required range."
    )]
    async fn leantoken_search(
        &self,
        Parameters(req): Parameters<SearchRequest>,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, ErrorData> {
        let resp = self
            .services
            .search_cancellable(req, context.ct.clone())
            .await
            .map_err(into_mcp_error)?;
        self.result(resp)
    }

    #[tool(
        name = "leantoken_outline",
        description = "Structural map of definitions, signatures, imports, and ranges, replacing whole-file orientation reads. Use before leantoken_read when the relevant symbol or range is unknown. Omits source bodies."
    )]
    async fn leantoken_outline(
        &self,
        Parameters(req): Parameters<OutlineRequest>,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, ErrorData> {
        let resp = self
            .services
            .outline_cancellable(req, context.ct.clone())
            .await
            .map_err(into_mcp_error)?;
        self.result(resp)
    }

    #[tool(
        name = "leantoken_read",
        description = "Read an exact symbol or narrow line range. Prefer this after files, outline, or search instead of reading a whole file. Reuse content_hash as expected_hash to receive not_modified without duplicate source."
    )]
    async fn leantoken_read(
        &self,
        Parameters(req): Parameters<ReadRequest>,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, ErrorData> {
        let resp = self
            .services
            .read_cancellable(req, context.ct.clone())
            .await
            .map_err(into_mcp_error)?;
        self.result(resp)
    }

    #[tool(
        name = "leantoken_context",
        description = "Use when task scope is still uncertain after narrow discovery, or when one-shot orientation is worth its metadata cost. Finds and ranks evidence within a token budget. Pass receipt fragment_hashes as known_hashes on later calls to avoid resending exact evidence."
    )]
    async fn leantoken_context(
        &self,
        Parameters(req): Parameters<ContextRequest>,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, ErrorData> {
        let resp = self
            .services
            .context_cancellable(req, context.ct.clone())
            .await
            .map_err(into_mcp_error)?;
        self.result(resp)
    }
}

#[tool_handler(
    instructions = "Retrieve progressively: files for paths, outline or search for candidates, then read exact symbols or ranges. Use context only when scope remains uncertain. Reuse hashes to suppress unchanged evidence. Use native tools for edits, commands, and tests."
)]
impl ServerHandler for LeanTokenMcp {}

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
            ErrorData::internal_error("repository retrieval failed", None)
        })
}

fn into_mcp_error(error: crate::Error) -> ErrorData {
    match &error {
        crate::Error::Cancelled => ErrorData::invalid_request("request cancelled", None),
        crate::Error::RootNotFound(_)
        | crate::Error::PathOutsideRoot(_)
        | crate::Error::NotIndexed(_)
        | crate::Error::LimitExceeded
        | crate::Error::UnsupportedLanguage(_)
        | crate::Error::InvalidRequest(_)
        | crate::Error::StaleCursor
        | crate::Error::Regex(_)
        | crate::Error::Glob(_) => ErrorData::invalid_params(error.to_string(), None),
        _ => {
            tracing::error!(%error, "MCP tool failed");
            ErrorData::internal_error("repository retrieval failed", None)
        }
    }
}

/// Return the JSON-serialized tool catalog for token-cost measurements.
pub fn tool_catalog_json() -> String {
    serde_json::to_string(&LeanTokenMcp::tool_router().list_all())
        .expect("tool catalog is serializable")
}

/// Run the MCP server over stdio until the transport closes or SIGINT is received.
pub async fn serve_stdio(services: Arc<Services>, result_mode: McpResultMode) -> crate::Result<()> {
    let server = LeanTokenMcp::new(services).with_result_mode(result_mode);
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
        assert!(descriptions["leantoken_files"].contains("Start here"));
        assert!(descriptions["leantoken_search"].contains("Follow a hit"));
        assert!(descriptions["leantoken_outline"].contains("whole-file"));
        assert!(descriptions["leantoken_read"].contains("expected_hash"));
        assert!(descriptions["leantoken_context"].contains("scope is still uncertain"));
    }

    #[test]
    fn complete_tool_catalog_stays_token_bounded() {
        let tools = LeanTokenMcp::tool_router().list_all();
        let json = serde_json::to_string(&tools).expect("tool catalog JSON");
        let token_count = crate::tokens::count(&json);
        assert!(
            token_count <= 1_600,
            "five-tool catalog grew to {token_count} cl100k tokens"
        );
    }

    #[test]
    fn tool_catalog_schema_snapshot() {
        let tools = LeanTokenMcp::tool_router().list_all();
        insta::assert_json_snapshot!("mcp_tool_catalog", tools);
    }
}
