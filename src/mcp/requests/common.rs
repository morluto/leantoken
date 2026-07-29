use super::*;

use std::borrow::Cow;

use serde::de::DeserializeOwned;

pub(in crate::mcp) const RECEIPT_RESOURCE_RESPONSE_RESERVE_TOKENS: usize = 128;

/// Project-owned parameter extractor that preserves generated RMCP schemas but
/// returns deserialization failures as ordinary JSON-RPC invalid-params errors.
#[derive(Debug, Deserialize)]
#[serde(transparent)]
pub(in crate::mcp) struct Parameters<T>(pub(in crate::mcp) T);

impl<T: JsonSchema> JsonSchema for Parameters<T> {
    fn inline_schema() -> bool {
        T::inline_schema()
    }

    fn schema_name() -> Cow<'static, str> {
        T::schema_name()
    }

    fn schema_id() -> Cow<'static, str> {
        T::schema_id()
    }

    fn json_schema(generator: &mut SchemaGenerator) -> Schema {
        T::json_schema(generator)
    }
}

impl<S, T>
    rmcp::handler::server::common::FromContextPart<
        rmcp::handler::server::tool::ToolCallContext<'_, S>,
    > for Parameters<T>
where
    T: DeserializeOwned,
{
    fn from_context_part(
        context: &mut rmcp::handler::server::tool::ToolCallContext<S>,
    ) -> Result<Self, ErrorData> {
        let arguments = context.arguments.take().unwrap_or_default();
        serde_json::from_value(serde_json::Value::Object(arguments))
            .map(Self)
            .map_err(|error| {
                ErrorData::invalid_params(
                    format!("invalid parameters for {}", context.name()),
                    Some(serde_json::json!({
                        "category": "invalid_input",
                        "field": "parameters",
                        "reason": error.to_string(),
                    })),
                )
            })
    }
}

pub(in crate::mcp) fn service_call_options(
    max_response_tokens: Option<usize>,
) -> ServiceCallOptions {
    max_response_tokens.map_or_else(ServiceCallOptions::new, |limit| {
        ServiceCallOptions::new().with_max_response_tokens(
            limit
                .saturating_sub(RECEIPT_RESOURCE_RESPONSE_RESERVE_TOKENS)
                .max(1),
        )
    })
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
        "type": "integer",
        "format": "uint",
        "minimum": 1,
        "maximum": MAX_RESULTS,
        "default": DEFAULT_RESULTS
    })
}

pub(in crate::mcp) fn token_limit_schema(_: &mut SchemaGenerator) -> Schema {
    schemars::json_schema!({
        "type": "integer",
        "format": "uint",
        "minimum": 1,
        "maximum": MAX_OUTPUT_TOKENS,
        "default": DEFAULT_READ_TOKENS
    })
}

pub(in crate::mcp) fn context_token_limit_schema(_: &mut SchemaGenerator) -> Schema {
    schemars::json_schema!({
        "type": "integer",
        "format": "uint",
        "minimum": 1,
        "maximum": MAX_OUTPUT_TOKENS,
        "default": DEFAULT_CONTEXT_TOKENS
    })
}

pub(in crate::mcp) fn response_token_limit_schema(_: &mut SchemaGenerator) -> Schema {
    schemars::json_schema!({
        "type": "integer",
        "format": "uint",
        "minimum": 1,
        "maximum": MAX_OUTPUT_TOKENS
    })
}

pub(in crate::mcp) fn context_line_limit_schema(_: &mut SchemaGenerator) -> Schema {
    schemars::json_schema!({
        "type": "integer",
        "format": "uint",
        "minimum": 0,
        "maximum": MAX_CONTEXT_LINES,
        "default": DEFAULT_CONTEXT_LINES
    })
}

pub(in crate::mcp) fn expected_repository_id_schema(_: &mut SchemaGenerator) -> Schema {
    schemars::json_schema!({
        "type": ["string", "null"],
        "maxLength": crate::services::MAX_EXPECTED_REPOSITORY_ID_BYTES
    })
}

pub(in crate::mcp) fn file_operation_schema(generator: &mut SchemaGenerator) -> Schema {
    generator.subschema_for::<FileOperation>()
}

pub(in crate::mcp) fn add_files_operation_constraints(schema: &mut Schema) {
    schema.insert(
        "oneOf".into(),
        serde_json::json!([
            {
                "properties": {"operation": {"const": "tree"}},
                "not": {"anyOf": [
                    {"required": ["query"]},
                    {"required": ["pattern"]}
                ]}
            },
            {
                "properties": {
                    "operation": {"const": "find"},
                    "query": {"type": "string"}
                },
                "required": ["query"],
                "not": {"anyOf": [
                    {"required": ["path"]},
                    {"required": ["pattern"]},
                    {"required": ["depth"]}
                ]}
            },
            {
                "properties": {
                    "operation": {"const": "glob"},
                    "pattern": {"type": "string"}
                },
                "required": ["pattern"],
                "not": {"anyOf": [
                    {"required": ["path"]},
                    {"required": ["query"]},
                    {"required": ["depth"]}
                ]}
            }
        ]),
    );
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

pub(in crate::mcp) fn deserialize_optional_limit<'de, D>(
    deserializer: D,
) -> std::result::Result<Option<usize>, D::Error>
where
    D: Deserializer<'de>,
{
    usize::deserialize(deserializer).map(Some)
}

pub(in crate::mcp) fn index_consistency_schema(_: &mut SchemaGenerator) -> Schema {
    schemars::json_schema!({
        "type": "string",
        "enum": ["indexed_generation", "reconcile_working_tree"]
    })
}
