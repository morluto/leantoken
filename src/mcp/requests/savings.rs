use super::*;

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(in crate::mcp) struct SavingsMcpRequest {
    /// Optional name of an approved repository context.
    #[serde(default)]
    #[schemars(schema_with = "repository_context_schema")]
    pub(in crate::mcp) repository_context: Option<String>,
    /// Opaque snapshot from an earlier savings response; returns aggregate deltas.
    #[serde(default)]
    #[schemars(length(max = 32768))]
    pub(in crate::mcp) snapshot: Option<String>,
}
