use super::*;

pub(in crate::mcp) fn service_call_options(
    max_response_tokens: Option<usize>,
) -> ServiceCallOptions {
    service_call_options_with_receipt(max_response_tokens, false)
}

pub(in crate::mcp) fn service_call_options_with_receipt(
    max_response_tokens: Option<usize>,
    receipt: bool,
) -> ServiceCallOptions {
    let options = max_response_tokens.map_or_else(ServiceCallOptions::new, |limit| {
        ServiceCallOptions::new().with_max_response_tokens(limit)
    });
    options.with_receipt_resource_reserve(receipt)
}

pub(in crate::mcp) const fn default_heading_occurrence() -> usize {
    1
}

pub(in crate::mcp) fn result_limit_schema(_: &mut SchemaGenerator) -> Schema {
    schemars::json_schema!({
        "description": "The server's configured max_results cap may be lower than the protocol ceiling; omitted values use config.default_results.",
        "type": ["integer", "null"],
        "format": "uint",
        "minimum": 1,
        "maximum": MAX_RESULTS,
    })
}

pub(in crate::mcp) fn token_limit_schema(_: &mut SchemaGenerator) -> Schema {
    schemars::json_schema!({
        "description": "The server's configured default_read_tokens value applies when this field is omitted.",
        "type": ["integer", "null"],
        "format": "uint",
        "minimum": 1,
        "maximum": MAX_OUTPUT_TOKENS
    })
}

pub(in crate::mcp) fn context_token_limit_schema(_: &mut SchemaGenerator) -> Schema {
    schemars::json_schema!({
        "description": "The server's configured default_context_tokens value applies when this field is omitted.",
        "type": ["integer", "null"],
        "format": "uint",
        "minimum": 1,
        "maximum": MAX_OUTPUT_TOKENS
    })
}

pub(in crate::mcp) fn response_token_limit_schema(_: &mut SchemaGenerator) -> Schema {
    schemars::json_schema!({
        "type": ["integer", "null"],
        "format": "uint",
        "minimum": 1,
        "maximum": MAX_OUTPUT_TOKENS
    })
}

pub(in crate::mcp) fn context_line_limit_schema(_: &mut SchemaGenerator) -> Schema {
    schemars::json_schema!({
        "description": "The server's configured context_lines value applies when this field is omitted.",
        "type": ["integer", "null"],
        "format": "uint",
        "minimum": 0,
        "maximum": MAX_CONTEXT_LINES
    })
}

pub(in crate::mcp) fn expected_repository_id_schema(_: &mut SchemaGenerator) -> Schema {
    schemars::json_schema!({
        "description": "Expected opaque repository identity from an earlier response.",
        "type": ["string", "null"],
        "maxLength": crate::services::MAX_EXPECTED_REPOSITORY_ID_BYTES
    })
}

pub(in crate::mcp) fn repository_context_schema(_: &mut SchemaGenerator) -> Schema {
    schemars::json_schema!({
        "description": "Optional name of an approved repository context; omitted values use the primary workspace.",
        "type": ["string", "null"],
        "maxLength": 64
    })
}

pub(in crate::mcp) fn validate_optional_positive_limit(
    field: &'static str,
    requested: Option<usize>,
    limit: usize,
) -> crate::Result<()> {
    requested.map_or(Ok(()), |requested| {
        validate_positive_request_limit(field, requested, limit).map(drop)
    })
}

pub(in crate::mcp) fn validate_optional_limit(
    field: &'static str,
    requested: Option<usize>,
    limit: usize,
) -> crate::Result<()> {
    requested.map_or(Ok(()), |requested| {
        validate_request_limit(field, requested, limit).map(drop)
    })
}

pub(in crate::mcp) fn index_consistency_schema(_: &mut SchemaGenerator) -> Schema {
    schemars::json_schema!({
        "description": "Use `reconcile_working_tree` after edits; otherwise use the indexed generation.",
        "type": "string",
        "enum": ["indexed_generation", "reconcile_working_tree"]
    })
}
