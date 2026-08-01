use super::*;
use crate::model::ReadPolicy;

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(in crate::mcp) struct ReadMcpRequest {
    /// Expected opaque repository identity from an earlier response.
    #[serde(default)]
    #[schemars(schema_with = "expected_repository_id_schema")]
    pub(in crate::mcp) expected_repository_id: Option<String>,
    /// Repository-relative UTF-8 source file.
    #[schemars(length(min = 1, max = 4096))]
    pub(in crate::mcp) path: RepositoryPath,
    /// Exact symbol, document heading, line range, or continuation to read.
    pub(in crate::mcp) target: ReadMcpTarget,
    /// Maximum source tokens to return (default 8000, maximum 32000).
    #[serde(default)]
    #[schemars(schema_with = "token_limit_schema", default = "default_token_option")]
    pub(in crate::mcp) max_tokens: Option<usize>,
    /// Maximum tokens in the final serialized service response.
    #[serde(default)]
    #[schemars(schema_with = "response_token_limit_schema")]
    pub(in crate::mcp) max_response_tokens: Option<usize>,
    /// Hash from the same prior target; matching content returns `not_modified`.
    #[serde(default)]
    #[schemars(schema_with = "expected_repository_id_schema")]
    pub(in crate::mcp) expected_hash: Option<String>,
    /// Record this target and prefer a cheaper follow-up. Without `expected_hash`,
    /// select the latest compatible base for this exact target.
    #[serde(default)]
    pub(in crate::mcp) delta: bool,
    /// Suppress evidence already returned under this server-managed receipt.
    #[serde(default)]
    #[schemars(length(max = 128))]
    pub(in crate::mcp) receipt_id: Option<String>,
    /// Use `reconcile_working_tree` after edits; otherwise `indexed_generation`.
    #[serde(default)]
    #[schemars(schema_with = "index_consistency_schema")]
    pub(in crate::mcp) consistency: IndexConsistency,
    /// I/O and verification policy. `bounded` (default) stops after the
    /// requested page and reports `index_state: unknown`. `full` hashes the
    /// complete live file, reports current/stale with indexed hashes, and is
    /// required for `delta: true`.
    #[serde(default)]
    pub(in crate::mcp) policy: ReadPolicy,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(in crate::mcp) enum ReadMcpTarget {
    /// Read one indexed symbol definition.
    Symbol {
        /// Structured indexed symbol identity.
        identity: SymbolIdentity,
    },
    /// Read one indexed Markdown or LaTeX section by exact title or outline signature.
    Heading {
        /// Exact title or outline signature such as `## Performance` or `\section{Method}`.
        #[schemars(length(min = 1, max = 4096))]
        name: String,
        /// One-based occurrence when the heading text is duplicated.
        #[serde(default = "default_heading_occurrence")]
        #[schemars(default = "default_heading_occurrence", range(min = 1))]
        occurrence: usize,
    },
    /// Read one inclusive one-based line range.
    Lines {
        /// First one-based line.
        #[schemars(range(min = 1))]
        start: usize,
        /// Last one-based line; must be at least `start`.
        #[schemars(range(min = 1))]
        end: usize,
    },
    /// Continue a truncated read without losing a partial final line.
    Continuation {
        /// Opaque cursor from the preceding truncated response.
        #[schemars(length(min = 1, max = 256))]
        cursor: String,
    },
}

impl ReadMcpRequest {
    pub(in crate::mcp) fn validate_limits(&self, limits: McpLimitPolicy) -> crate::Result<()> {
        validate_optional_positive_limit("max_tokens", self.max_tokens, limits.max_output_tokens)?;
        validate_optional_positive_limit(
            "max_response_tokens",
            self.max_response_tokens,
            limits.max_response_tokens,
        )?;
        if matches!(self.target, ReadMcpTarget::Heading { occurrence: 0, .. }) {
            return Err(crate::Error::InvalidInput {
                field: "heading occurrence",
                reason: "must be one-based",
            });
        }
        Ok(())
    }

    pub(in crate::mcp) fn into_parts(
        self,
    ) -> (
        ReadRequest,
        IndexConsistency,
        ServiceCallOptions,
        Option<String>,
    ) {
        let (start_line, end_line, symbol, heading, heading_occurrence, continuation_cursor) =
            match self.target {
                ReadMcpTarget::Symbol { identity } => (
                    None,
                    None,
                    Some(identity.qualified_name()),
                    None,
                    None,
                    None,
                ),
                ReadMcpTarget::Heading { name, occurrence } => {
                    (None, None, None, Some(name), Some(occurrence), None)
                }
                ReadMcpTarget::Lines { start, end } => {
                    (Some(start), Some(end), None, None, None, None)
                }
                ReadMcpTarget::Continuation { cursor } => {
                    (None, None, None, None, None, Some(cursor))
                }
            };
        (
            ReadRequest {
                path: self.path.into_string(),
                start_line,
                end_line,
                symbol,
                heading,
                heading_occurrence,
                continuation_cursor,
                max_tokens: self.max_tokens,
                expected_hash: self.expected_hash,
                delta: self.delta,
                receipt_id: self.receipt_id,
                policy: self.policy,
            },
            self.consistency,
            service_call_options(self.max_response_tokens),
            self.expected_repository_id,
        )
    }
}
