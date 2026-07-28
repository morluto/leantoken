use super::*;

#[derive(Debug, Serialize)]
pub(in crate::mcp) struct RetryableToolResponse {
    pub(in crate::mcp) status: &'static str,
    pub(in crate::mcp) reason: &'static str,
    pub(in crate::mcp) message: &'static str,
    pub(in crate::mcp) retry_after_ms: u64,
}

impl RetryableToolResponse {
    pub(in crate::mcp) const fn new(
        reason: &'static str,
        message: &'static str,
        retry_after_ms: u64,
    ) -> Self {
        Self {
            status: "retryable",
            reason,
            message,
            retry_after_ms,
        }
    }
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

pub(in crate::mcp) fn retryable_tool_result(
    response: RetryableToolResponse,
    mode: McpResultMode,
) -> CallToolResult {
    tool_result(response, mode).unwrap_or_else(|error| {
        tracing::error!(%error, "MCP retry response serialization failed");
        tool_unavailable(
            "response_serialization",
            "repository retrieval is temporarily unavailable; retry shortly",
        )
    })
}
