use std::sync::Arc;

use rmcp::{
    ErrorData, RoleServer, ServerHandler, ServiceExt, handler::server::wrapper::Parameters,
    model::CallToolResult, service::RequestContext, tool, tool_handler, tool_router,
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
}

impl LeanTokenMcp {
    #[must_use]
    pub fn new(services: Arc<Services>) -> Self {
        Self { services }
    }
}

#[tool_router]
impl LeanTokenMcp {
    #[tool(
        name = "leantoken_files",
        description = "Path discovery replacing find and rg --files. Use tree for hierarchy, find for fuzzy paths, and glob for known patterns. Does not search contents; use leantoken_search."
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
        structured(resp)
    }

    #[tool(
        name = "leantoken_search",
        description = "Ranked source lookup replacing grep and rg. Use symbol for definitions, reference for usages, identifier for exact names, text for substrings, regex for patterns, or auto to combine evidence. Use leantoken_files for filenames."
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
        structured(resp)
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
        structured(resp)
    }

    #[tool(
        name = "leantoken_read",
        description = "Bounded source retrieval replacing cat, head, and tail. Prefer a symbol or narrow range; run leantoken_outline first for unfamiliar files. Pass expected_hash to suppress unchanged content."
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
        structured(resp)
    }

    #[tool(
        name = "leantoken_context",
        description = "First tool for a new non-trivial repository task. Composes ranked evidence within a token budget, replacing several broad searches and reads. Use narrower tools for follow-up questions."
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
        structured(resp)
    }
}

#[tool_handler(
    instructions = "Use LeanToken for repository discovery: context for a new task, files for paths, search for code, outline before read, and read only needed ranges. Use native tools for edits, commands, and tests."
)]
impl ServerHandler for LeanTokenMcp {}

fn structured<T: Serialize>(value: T) -> Result<CallToolResult, ErrorData> {
    serde_json::to_value(value)
        .map(CallToolResult::structured)
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
pub async fn serve_stdio(services: Arc<Services>) -> crate::Result<()> {
    let server = LeanTokenMcp::new(services);
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
        assert!(descriptions["leantoken_files"].contains("find and rg --files"));
        assert!(descriptions["leantoken_search"].contains("grep and rg"));
        assert!(descriptions["leantoken_outline"].contains("whole-file"));
        assert!(descriptions["leantoken_read"].contains("cat, head, and tail"));
        assert!(descriptions["leantoken_context"].contains("First tool"));
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
