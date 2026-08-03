use super::*;

#[cfg_attr(not(test), allow(dead_code))]
pub(in crate::mcp) const RECEIPT_RESOURCE_RESPONSE_RESERVE_TOKENS: usize = 128;

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

pub(in crate::mcp) const fn default_result_option() -> Option<usize> {
    Some(DEFAULT_RESULTS)
}

pub(in crate::mcp) const fn default_token_option() -> Option<usize> {
    Some(DEFAULT_READ_TOKENS)
}

pub(in crate::mcp) const fn default_context_line_option() -> Option<usize> {
    Some(DEFAULT_CONTEXT_LINES)
}

pub(in crate::mcp) const fn default_heading_occurrence() -> usize {
    1
}

pub(in crate::mcp) fn result_limit_schema(_: &mut SchemaGenerator) -> Schema {
    schemars::json_schema!({
        "type": ["integer", "null"],
        "format": "uint",
        "minimum": 1,
        "maximum": MAX_RESULTS,
        "default": DEFAULT_RESULTS
    })
}

pub(in crate::mcp) fn token_limit_schema(_: &mut SchemaGenerator) -> Schema {
    schemars::json_schema!({
        "type": ["integer", "null"],
        "format": "uint",
        "minimum": 1,
        "maximum": MAX_OUTPUT_TOKENS,
        "default": DEFAULT_READ_TOKENS
    })
}

pub(in crate::mcp) fn context_token_limit_schema(_: &mut SchemaGenerator) -> Schema {
    schemars::json_schema!({
        "type": ["integer", "null"],
        "format": "uint",
        "minimum": 1,
        "maximum": MAX_OUTPUT_TOKENS,
        "default": DEFAULT_CONTEXT_TOKENS
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
        "type": ["integer", "null"],
        "format": "uint",
        "minimum": 0,
        "maximum": MAX_CONTEXT_LINES,
        "default": DEFAULT_CONTEXT_LINES
    })
}

pub(in crate::mcp) fn expected_repository_id_schema(_: &mut SchemaGenerator) -> Schema {
    schemars::json_schema!({
        "description": "Expected opaque repository identity from an earlier response.",
        "type": ["string", "null"],
        "maxLength": crate::services::MAX_EXPECTED_REPOSITORY_ID_BYTES
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
