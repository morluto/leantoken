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
    /// Send JSON as both text and structured content for broad host compatibility.
    Dual,
    /// Send JSON only as text content for hosts that ignore structured content.
    Text,
    /// Send only structured content for hosts verified to support it.
    #[default]
    Structured,
}

/// Serialize a successful tool value using an explicit wire representation.
pub fn tool_result<T: Serialize>(
    value: T,
    mode: McpResultMode,
) -> Result<CallToolResult, ErrorData> {
    tool_result_with_limit(value, mode, None)
}

pub(in crate::mcp) fn tool_result_with_limit<T: Serialize>(
    value: T,
    mode: McpResultMode,
    max_response_tokens: Option<usize>,
) -> Result<CallToolResult, ErrorData> {
    let (value, receipt_id) = serde_json::to_value(value)
        .and_then(decorate_receipt_result)
        .map_err(|error| {
            tracing::error!(%error, "MCP response serialization failed");
            ErrorData::internal_error(
                "repository retrieval failed",
                mcp_error_data("response_serialization"),
            )
        })?;
    if let Some(limit) = max_response_tokens
        && let Some(total) = value
            .pointer("/meta/total_response_tokens")
            .and_then(serde_json::Value::as_u64)
            .and_then(|total| usize::try_from(total).ok())
        && total > limit
    {
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
            })),
        ));
    }
    let mut result = match mode {
        McpResultMode::Dual => CallToolResult::structured(value.clone()),
        McpResultMode::Text => CallToolResult::success(vec![ContentBlock::text(value.to_string())]),
        McpResultMode::Structured => {
            let mut result = CallToolResult::default();
            result.structured_content = Some(value);
            result.is_error = Some(false);
            result
        }
    };
    if let Some(receipt_id) = receipt_id {
        let uri = resources::receipt_uri(&receipt_id);
        result.content.push(ContentBlock::text(format!(
            "Copy the opaque URI exactly and call MCP resources/read: {uri}. In Codex, call read_mcp_resource with the LeanToken server and this exact URI."
        )));
        result.content.push(ContentBlock::resource_link(
            resources::receipt_resource_link(&receipt_id),
        ));
    }
    Ok(result)
}

fn decorate_receipt_result(
    mut value: serde_json::Value,
) -> serde_json::Result<(serde_json::Value, Option<String>)> {
    let receipt_id = value
        .pointer("/meta/receipt_id")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned);
    let Some(receipt_id) = receipt_id else {
        return Ok((value, None));
    };
    let uri = resources::receipt_uri(&receipt_id);
    let Some(object) = value.as_object_mut() else {
        return Ok((value, None));
    };
    object.insert(
        "receipt_resource".into(),
        serde_json::json!({
            "kind": "retrieval_receipt",
            "id": receipt_id,
            "uri": uri,
        }),
    );
    recalculate_structured_accounting(&mut value)?;
    Ok((value, Some(receipt_id)))
}

fn recalculate_structured_accounting(value: &mut serde_json::Value) -> serde_json::Result<()> {
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
        let accounting =
            crate::tokens::response_token_accounting(value, source_tokens, &tokenizer)?;
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
        "structured MCP response accounting did not reach a fixed point",
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
        )
    })
}
