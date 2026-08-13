use super::*;

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(in crate::mcp) struct SavingsMcpRequest {
    /// Opaque snapshot from an earlier savings response; returns aggregate deltas.
    #[serde(default)]
    #[schemars(length(max = 32768))]
    pub(in crate::mcp) snapshot: Option<String>,
}
