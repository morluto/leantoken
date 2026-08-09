use rmcp::{
    ErrorData,
    model::{CallToolResult, ContentBlock},
};

pub(super) fn into_mcp_error(error: crate::Error) -> ErrorData {
    let cause = error.reconciliation_cause();
    match cause {
        crate::Error::Cancelled => {
            ErrorData::invalid_request("request cancelled", mcp_error_data(cause.public_category()))
        }
        crate::Error::PathOutsideRoot(_) => {
            tracing::debug!(%cause, "MCP path rejected outside repository root");
            ErrorData::invalid_params(
                "path must stay within the repository root",
                mcp_error_data(cause.public_category()),
            )
        }
        crate::Error::UnsupportedPathEncoding(_) => ErrorData::invalid_params(
            "repository path is not valid UTF-8",
            mcp_error_data(cause.public_category()),
        ),
        crate::Error::NotIndexed(_) => ErrorData::invalid_params(
            "requested path is not indexed",
            mcp_error_data(cause.public_category()),
        ),
        crate::Error::SymbolNotFound { .. } => ErrorData::invalid_params(
            "requested symbol is not indexed",
            mcp_error_data(cause.public_category()),
        ),
        crate::Error::AmbiguousSymbol { .. } => ErrorData::invalid_params(
            "requested symbol matches multiple definitions",
            mcp_error_data(cause.public_category()),
        ),
        crate::Error::HeadingNotFound { .. } => ErrorData::invalid_params(
            "requested document heading occurrence is not indexed",
            mcp_error_data(cause.public_category()),
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
            mcp_error_data(cause.public_category()),
        ),
        crate::Error::RetrievalLimitExceeded {
            kind,
            observed,
            limit,
        } => ErrorData::invalid_params(
            format!("{cause}; {}", kind.guidance()),
            Some(serde_json::json!({
                "category": cause.public_category(),
                "reason": kind.as_str(),
                "requested": observed,
                "limit": limit,
            })),
        ),
        crate::Error::RegexWorkBudgetExceeded {
            dimension,
            candidate_files,
            candidate_chunks,
            candidate_bytes,
            limit,
        } => ErrorData::invalid_params(
            format!(
                "regex search stopped after exhausting its bounded candidate-work budget; {}",
                dimension.guidance()
            ),
            Some(serde_json::json!({
                "category": cause.public_category(),
                "complete": false,
                "limiting_dimension": dimension,
                "candidate_files": candidate_files,
                "candidate_chunks": candidate_chunks,
                "candidate_bytes": candidate_bytes,
                "limit": limit,
            })),
        ),
        crate::Error::ResponseBudgetExceeded {
            provided_max_response_tokens,
            minimum_required_response_tokens,
            retry_with_at_least,
            breakdown,
        } => ErrorData::invalid_params(
            format!("max_response_tokens is too small; retry with at least {retry_with_at_least}"),
            Some(serde_json::json!({
                "category": cause.public_category(),
                "field": "max_response_tokens",
                "requested": minimum_required_response_tokens,
                "limit": provided_max_response_tokens,
                "provided_max_response_tokens": provided_max_response_tokens,
                "minimum_required_response_tokens": minimum_required_response_tokens,
                "retry_with_at_least": retry_with_at_least,
                "breakdown": breakdown,
            })),
        ),
        crate::Error::RequestLimitExceeded {
            field,
            requested,
            limit,
        } => ErrorData::invalid_params(
            format!("{field} exceeds its configured limit"),
            Some(serde_json::json!({
                "category": cause.public_category(),
                "field": field,
                "requested": requested,
                "limit": limit,
            })),
        ),
        crate::Error::UnsupportedLanguage(_) => ErrorData::invalid_params(
            "requested structured language is unsupported",
            mcp_error_data(cause.public_category()),
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
                "category": cause.public_category(),
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
                "category": cause.public_category(),
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
                "category": cause.public_category(),
                "field": field,
            })),
        ),
        crate::Error::InvalidSearchOptions {
            field,
            allowed_modes,
            conflicting_options,
            ranked_symbol_example,
            exhaustive_text_example,
        } => ErrorData::invalid_params(
            format!(
                "invalid {field}: exhaustive occurrences require text or regex mode; use ranked symbol search or exhaustive text search"
            ),
            Some(serde_json::json!({
                "category": cause.public_category(),
                "field": field,
                "allowed_modes": allowed_modes,
                "conflicting_options": conflicting_options,
                "examples": {
                    "ranked_symbol": ranked_symbol_example,
                    "exhaustive_text": exhaustive_text_example,
                },
            })),
        ),
        crate::Error::InvalidInputConstraints(violations) => ErrorData::invalid_params(
            cause.to_string(),
            Some(serde_json::json!({
                "category": cause.public_category(),
                "violations": violations,
            })),
        ),
        crate::Error::InputTooLong { field, max_bytes } => ErrorData::invalid_params(
            "request input exceeds its byte limit",
            Some(serde_json::json!({
                "category": cause.public_category(),
                "field": field,
                "limit": max_bytes,
            })),
        ),
        crate::Error::Io(error) if error.kind() == std::io::ErrorKind::NotFound => {
            ErrorData::invalid_params("requested path does not exist", mcp_error_data("not_found"))
        }
        crate::Error::InvalidRequest(_) => ErrorData::invalid_params(
            "request parameters are invalid",
            mcp_error_data(cause.public_category()),
        ),
        crate::Error::StaleCursor => ErrorData::invalid_params(
            "cursor is stale or invalid",
            mcp_error_data(cause.public_category()),
        ),
        crate::Error::UnknownReceipt(_) => ErrorData::invalid_params(
            "retrieval receipt is unknown or expired",
            mcp_error_data(cause.public_category()),
        ),
        crate::Error::StaleReceipt { .. } => ErrorData::invalid_params(
            "retrieval receipt belongs to a stale repository generation",
            mcp_error_data(cause.public_category()),
        ),
        crate::Error::UnknownQueryReceipt(_) => ErrorData::invalid_params(
            "query coverage receipt is unknown or expired",
            mcp_error_data(cause.public_category()),
        ),
        crate::Error::QueryReceiptMismatch => ErrorData::invalid_params(
            "query coverage receipt does not cover the requested predicate",
            mcp_error_data(cause.public_category()),
        ),
        crate::Error::StaleQueryReceipt { .. } => ErrorData::invalid_params(
            "relevant indexed content changed after the query coverage receipt was recorded",
            mcp_error_data(cause.public_category()),
        ),
        crate::Error::Regex(_) => ErrorData::invalid_params(
            "regular expression is invalid",
            mcp_error_data(cause.public_category()),
        ),
        crate::Error::Glob(_) => ErrorData::invalid_params(
            "glob pattern is invalid",
            mcp_error_data(cause.public_category()),
        ),
        crate::Error::RootNotFound(_)
        | crate::Error::UnsafeRepositoryRoot(_)
        | crate::Error::RepositoryMismatch { .. }
        | crate::Error::IndexScopeMismatch { .. }
        | crate::Error::InvalidConfiguration(_) => {
            tracing::error!(%cause, "repository configuration is invalid");
            ErrorData::internal_error(
                "repository configuration is invalid",
                mcp_error_data(cause.public_category()),
            )
        }
        crate::Error::IndexLimitExceeded { .. } => {
            tracing::error!(%cause, "repository indexing limit exceeded");
            ErrorData::internal_error(
                "repository indexing limit exceeded",
                mcp_error_data(cause.public_category()),
            )
        }
        crate::Error::RepositoryTraversal(_) => {
            tracing::error!(%cause, "repository traversal failed");
            ErrorData::internal_error(
                "repository traversal failed",
                mcp_error_data(cause.public_category()),
            )
        }
        crate::Error::RuntimeCapabilityUnavailable { .. } => {
            tracing::error!(%cause, "repository runtime is unavailable");
            ErrorData::internal_error(
                "repository runtime is unavailable",
                mcp_error_data(cause.public_category()),
            )
        }
        crate::Error::IndexNotReady => ErrorData::internal_error(
            "repository index is not ready",
            mcp_error_data(cause.public_category()),
        ),
        crate::Error::RetryableConflict(_) => ErrorData::internal_error(
            "repository operation should be retried",
            mcp_error_data(cause.public_category()),
        ),
        crate::Error::SerializationFailure(_)
        | crate::Error::ResponseAccountingInvariant(_)
        | crate::Error::CachePruneFailure(_)
        | crate::Error::SetupFailure(_)
        | crate::Error::OperationFailure(_) => {
            tracing::error!(%cause, category = cause.public_category(), "typed product failure");
            ErrorData::internal_error(
                "repository retrieval failed",
                mcp_error_data(cause.public_category()),
            )
        }
        _ => {
            tracing::error!(%cause, "MCP tool failed");
            ErrorData::internal_error(
                "repository retrieval failed",
                mcp_error_data(cause.public_category()),
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
