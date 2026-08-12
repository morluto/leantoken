use super::*;

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(in crate::mcp) struct ReceiptRebaseMcpRequest {
    /// Immutable evidence artifact from an earlier repository generation.
    #[schemars(length(min = 1, max = 128))]
    pub(in crate::mcp) receipt_id: String,
    /// Maximum source-free examples per outcome; complete counts and digest are always returned.
    #[serde(default)]
    #[schemars(range(min = 0, max = 16))]
    pub(in crate::mcp) max_samples_per_outcome: Option<usize>,
    /// Maximum tokens in the final serialized service response.
    #[serde(default)]
    #[schemars(schema_with = "response_token_limit_schema")]
    pub(in crate::mcp) max_response_tokens: Option<usize>,
    /// Use `reconcile_working_tree` after edits; otherwise `indexed_generation`.
    #[serde(default)]
    #[schemars(schema_with = "index_consistency_schema")]
    pub(in crate::mcp) consistency: IndexConsistency,
    /// Expected opaque repository identity from an earlier response.
    #[serde(default)]
    #[schemars(schema_with = "expected_repository_id_schema")]
    pub(in crate::mcp) expected_repository_id: Option<String>,
}

impl ReceiptRebaseMcpRequest {
    pub(in crate::mcp) fn validate_limits(&self, limits: McpLimitPolicy) -> crate::Result<()> {
        validate_optional_limit(
            "max_samples_per_outcome",
            self.max_samples_per_outcome,
            MAX_RECEIPT_REBASE_SAMPLES_PER_OUTCOME,
        )?;
        validate_optional_positive_limit(
            "max_response_tokens",
            self.max_response_tokens,
            limits.max_response_tokens,
        )
    }

    pub(in crate::mcp) fn into_parts(
        self,
    ) -> (
        ReceiptRebaseRequest,
        IndexConsistency,
        ServiceCallOptions,
        Option<String>,
    ) {
        (
            ReceiptRebaseRequest {
                receipt_id: self.receipt_id,
                max_samples_per_outcome: self.max_samples_per_outcome,
            },
            self.consistency,
            service_call_options_with_receipt(self.max_response_tokens),
            self.expected_repository_id,
        )
    }
}
