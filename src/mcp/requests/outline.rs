use super::*;

#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
/// Response projection for structural outlines.
pub(in crate::mcp) enum OutlineMcpProjection {
    /// Preserve symbols, imports, and byte offsets.
    #[default]
    Full,
    /// Return symbol signatures and line ranges without imports or byte offsets.
    Signatures,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(in crate::mcp) struct OutlineMcpRequest {
    /// Expected opaque repository identity from an earlier response.
    #[serde(default)]
    #[schemars(schema_with = "expected_repository_id_schema")]
    pub(in crate::mcp) expected_repository_id: Option<String>,
    /// One to 256 repository-relative source files to outline.
    #[schemars(length(min = 1, max = 256), inner(length(max = 4096)))]
    pub(in crate::mcp) paths: Vec<RepositoryPath>,
    /// Keep definitions whose names contain this value.
    #[serde(default)]
    #[schemars(length(max = 4096))]
    pub(in crate::mcp) symbol_name: Option<String>,
    /// Keep definitions of this exact syntax kind.
    #[serde(default)]
    #[schemars(length(max = 4096))]
    pub(in crate::mcp) symbol_kind: Option<String>,
    /// Maximum definitions and imports to return (default 20, maximum 100).
    #[serde(default)]
    #[schemars(schema_with = "result_limit_schema")]
    pub(in crate::mcp) max_results: Option<usize>,
    /// Maximum signature and import tokens (default 8000, maximum 32000).
    #[serde(default)]
    #[schemars(schema_with = "token_limit_schema")]
    pub(in crate::mcp) max_tokens: Option<usize>,
    /// Maximum tokens in the final serialized service response.
    #[serde(default)]
    #[schemars(schema_with = "response_token_limit_schema")]
    pub(in crate::mcp) max_response_tokens: Option<usize>,
    /// Suppress evidence already returned under this immutable artifact.
    #[serde(default)]
    #[schemars(length(max = 128))]
    pub(in crate::mcp) receipt_id: Option<String>,
    /// Opaque cursor from a result-limited outline response.
    #[serde(default)]
    #[schemars(length(max = 256))]
    pub(in crate::mcp) cursor: Option<String>,
    /// Response shape: `full` definitions (default) or compact `signatures`.
    #[serde(default)]
    pub(in crate::mcp) projection: OutlineMcpProjection,
}

impl OutlineMcpRequest {
    pub(in crate::mcp) fn validate_limits(&self, limits: McpLimitPolicy) -> crate::Result<()> {
        validate_optional_positive_limit("max_results", self.max_results, limits.max_results)?;
        validate_optional_positive_limit("max_tokens", self.max_tokens, limits.max_output_tokens)?;
        validate_optional_positive_limit(
            "max_response_tokens",
            self.max_response_tokens,
            limits.max_response_tokens,
        )
    }

    pub(in crate::mcp) fn into_parts(
        self,
    ) -> (
        OutlineRequest,
        OutlineMcpProjection,
        ServiceCallOptions,
        Option<String>,
    ) {
        (
            OutlineRequest {
                paths: self
                    .paths
                    .into_iter()
                    .map(RepositoryPath::into_string)
                    .collect(),
                symbol_name: self.symbol_name,
                symbol_kind: self.symbol_kind,
                max_results: self.max_results,
                max_tokens: self.max_tokens,
                receipt_id: self.receipt_id,
                cursor: self.cursor,
            },
            self.projection,
            service_call_options(self.max_response_tokens),
            self.expected_repository_id,
        )
    }
}
