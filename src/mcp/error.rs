use rmcp::{
    ErrorData,
    model::{CallToolResult, ContentBlock},
};

pub(super) fn into_mcp_error(error: crate::Error) -> ErrorData {
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
            "requested document heading occurrence is not indexed",
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

pub(super) fn mcp_error_data(category: &'static str) -> Option<serde_json::Value> {
    Some(serde_json::json!({ "category": category }))
}

pub(super) fn tool_unavailable(reason: &'static str, message: &'static str) -> CallToolResult {
    let mut result = CallToolResult::error(vec![ContentBlock::text(message)]);
    result.structured_content = Some(serde_json::json!({
        "status": "unavailable",
        "reason": reason,
        "message": message,
    }));
    result
}
