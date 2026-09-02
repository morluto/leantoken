use super::*;

#[derive(Debug, Serialize)]
pub(in crate::mcp) struct RetryableToolResponse {
    pub(in crate::mcp) status: &'static str,
    pub(in crate::mcp) reason: &'static str,
    pub(in crate::mcp) message: &'static str,
    pub(in crate::mcp) retry_after_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(in crate::mcp) index_progress: Option<IndexProgressSnapshot>,
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
            index_progress: None,
        }
    }

    pub(in crate::mcp) fn with_index_progress(
        mut self,
        index_progress: Option<IndexProgressSnapshot>,
    ) -> Self {
        self.index_progress = index_progress;
        self
    }
}

/// Wire representation used for successful MCP tool results.
#[derive(Debug, Clone, Copy, Default, clap::ValueEnum, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum McpResultMode {
    /// Send JSON as both text and structured content for hosts that require both.
    Dual,
    /// Send JSON only as text content for hosts that ignore structured content.
    Text,
    /// Send only structured content for hosts verified to support it.
    #[default]
    Structured,
}

impl McpResultMode {
    pub(in crate::mcp) fn response_shape(
        self,
        protocol: Option<&ProtocolVersion>,
    ) -> crate::tokens::McpResponseShape {
        let mode = match self {
            Self::Dual => crate::tokens::McpResponseMode::Dual,
            Self::Text => crate::tokens::McpResponseMode::Text,
            Self::Structured => crate::tokens::McpResponseMode::Structured,
        };
        crate::tokens::McpResponseShape {
            mode,
            protocol: crate::tokens::McpProtocolShape::negotiated(protocol),
        }
    }
}

/// Serialize a successful tool value using an explicit wire representation.
pub fn tool_result<T: Serialize>(
    value: T,
    mode: McpResultMode,
) -> Result<CallToolResult, ErrorData> {
    tool_result_with_limit(value, mode, None, Some(&ProtocolVersion::V_2026_07_28))
}

pub(in crate::mcp) fn tool_result_with_limit<T: Serialize>(
    value: T,
    mode: McpResultMode,
    max_response_tokens: Option<usize>,
    protocol: Option<&ProtocolVersion>,
) -> Result<CallToolResult, ErrorData> {
    let mut value = serde_json::to_value(value).map_err(|error| {
        tracing::error!(%error, "MCP response serialization failed");
        ErrorData::internal_error(
            "repository retrieval failed",
            mcp_error_data("response_serialization"),
        )
    })?;
    let shape = mode.response_shape(protocol);
    recalculate_mcp_accounting(&mut value, shape).map_err(|error| {
        tracing::error!(%error, "MCP response accounting failed");
        ErrorData::internal_error(
            "repository retrieval failed",
            mcp_error_data("response_accounting"),
        )
    })?;
    if let Some(limit) = max_response_tokens
        && let Some(total) = value
            .pointer("/meta/total_response_tokens")
            .and_then(serde_json::Value::as_u64)
            .and_then(|total| usize::try_from(total).ok())
        && total > limit
    {
        let source_tokens = value
            .pointer("/meta/source_tokens")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or_default();
        let protocol_tokens = value
            .pointer("/meta/protocol_tokens")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or_default();
        let path_and_metadata_tokens = value
            .pointer("/meta/path_and_metadata_tokens")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or_default();
        return Err(ErrorData::invalid_params(
            format!("max_response_tokens is too small; retry with at least {total}"),
            Some(serde_json::json!({
                "category": "request_limit_exceeded",
                "field": "max_response_tokens",
                "requested": total,
                "limit": limit,
                "provided_max_response_tokens": limit,
                "minimum_required_response_tokens": total,
                "retry_with_at_least": total,
                "breakdown": {
                    "mandatory_response_tokens": total,
                    "source_tokens": source_tokens,
                    "protocol_tokens": protocol_tokens,
                    "path_and_metadata_tokens": path_and_metadata_tokens,
                    "receipt_reserve_tokens": 0,
                },
            })),
        ));
    }
    Ok(crate::tokens::model_visible_mcp_tool_result(
        value, shape.mode,
    ))
}

fn recalculate_mcp_accounting(
    value: &mut serde_json::Value,
    shape: crate::tokens::McpResponseShape,
) -> serde_json::Result<()> {
    let Some(tokenizer_name) = value
        .pointer("/meta/tokenizer")
        .and_then(serde_json::Value::as_str)
    else {
        return Ok(());
    };
    let Some(tokenizer) = tokenizer_by_name(tokenizer_name) else {
        return Ok(());
    };
    let source_tokens = value
        .pointer("/meta/source_tokens")
        .and_then(serde_json::Value::as_u64)
        .and_then(|value| usize::try_from(value).ok())
        .unwrap_or_default();
    for _ in 0..32 {
        let result = crate::tokens::model_visible_mcp_result(value.clone(), shape);
        let accounting =
            crate::tokens::response_token_accounting(&result, source_tokens, &tokenizer)?;
        let meta = value
            .get_mut("meta")
            .and_then(serde_json::Value::as_object_mut)
            .expect("receipt-bearing response has object metadata");
        let stable = meta
            .get("protocol_tokens")
            .and_then(serde_json::Value::as_u64)
            == Some(accounting.protocol_tokens as u64)
            && meta
                .get("path_and_metadata_tokens")
                .and_then(serde_json::Value::as_u64)
                == Some(accounting.path_and_metadata_tokens as u64)
            && meta
                .get("total_response_tokens")
                .and_then(serde_json::Value::as_u64)
                == Some(accounting.total_response_tokens as u64);
        if stable {
            return Ok(());
        }
        meta.insert("protocol_tokens".into(), accounting.protocol_tokens.into());
        meta.insert(
            "path_and_metadata_tokens".into(),
            accounting.path_and_metadata_tokens.into(),
        );
        meta.insert(
            "total_response_tokens".into(),
            accounting.total_response_tokens.into(),
        );
    }
    Err(serde_json::Error::io(std::io::Error::other(
        "MCP tool-result accounting did not reach a fixed point",
    )))
}

fn tokenizer_by_name(name: &str) -> Option<crate::tokens::Tokenizer> {
    use crate::tokens::Tokenizer;
    match name {
        "cl100k_base" => Some(Tokenizer::Cl100kBase),
        "o200k_base" => Some(Tokenizer::O200kBase),
        "o200k_harmony" => Some(Tokenizer::O200kHarmony),
        "p50k_base" => Some(Tokenizer::P50kBase),
        "r50k_base" => Some(Tokenizer::R50kBase),
        "gpt2" => Some(Tokenizer::Gpt2),
        "p50k_edit" => Some(Tokenizer::P50kEdit),
        "estimate" => Some(Tokenizer::Estimate),
        _ => None,
    }
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
            mode,
        )
    })
}
