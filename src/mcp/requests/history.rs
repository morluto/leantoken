use super::*;

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(in crate::mcp) struct HistoryMcpRequest {
    /// Expected opaque repository identity from an earlier response.
    #[serde(default)]
    #[schemars(schema_with = "expected_repository_id_schema")]
    pub(in crate::mcp) expected_repository_id: Option<String>,
    /// Git-backed symbol history operation.
    pub(in crate::mcp) operation: HistoryMcpOperation,
    /// Maximum results (default 20): 32 for `diff_symbols`, 100 for `symbol_log`.
    #[serde(default, deserialize_with = "deserialize_optional_limit")]
    #[schemars(schema_with = "result_limit_schema", default = "default_result_option")]
    pub(in crate::mcp) max_results: Option<usize>,
    /// Maximum source or diff tokens to return (default 8000, maximum 32000).
    #[serde(default, deserialize_with = "deserialize_optional_limit")]
    #[schemars(schema_with = "token_limit_schema", default = "default_token_option")]
    pub(in crate::mcp) max_tokens: Option<usize>,
    /// Maximum tokens in the final serialized service response.
    #[serde(default)]
    #[schemars(schema_with = "response_token_limit_schema")]
    pub(in crate::mcp) max_response_tokens: Option<usize>,
    /// Opaque cursor returned by `diff_symbols`; reuse the exact operation.
    #[serde(default)]
    #[schemars(length(min = 1, max = 128))]
    pub(in crate::mcp) cursor: Option<String>,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(in crate::mcp) enum HistoryMcpOperation {
    /// Read one parsed symbol, optionally qualified as `parent.name`, from an immutable revision.
    ReadSymbol {
        #[schemars(length(min = 1, max = 4096))]
        path: String,
        #[schemars(length(min = 1, max = 4096))]
        symbol: String,
        #[schemars(length(min = 1, max = 4096))]
        revision: String,
    },
    /// Compare one parsed symbol across revisions, including added or removed endpoints.
    DiffSymbol {
        #[schemars(length(min = 1, max = 4096))]
        path: String,
        #[schemars(length(min = 1, max = 4096))]
        symbol: String,
        #[schemars(length(min = 1, max = 4096))]
        base_revision: String,
        #[schemars(length(min = 1, max = 4096))]
        head_revision: String,
    },
    /// Diff an ordered symbol set with shared revisions, metadata, and bounded Git work.
    DiffSymbols {
        #[schemars(length(min = 1, max = "crate::services::MAX_DIFF_SYMBOL_TARGETS"))]
        targets: Vec<HistoryMcpTarget>,
        #[schemars(length(min = 1, max = 4096))]
        base_revision: String,
        #[schemars(length(min = 1, max = 4096))]
        head_revision: String,
    },
    /// List commits that touched the symbol's tracked historical lines.
    SymbolLog {
        #[schemars(length(min = 1, max = 4096))]
        path: String,
        #[schemars(length(min = 1, max = 4096))]
        symbol: String,
        #[serde(default)]
        #[schemars(length(min = 1, max = 4096))]
        revision: Option<String>,
    },
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(in crate::mcp) struct HistoryMcpTarget {
    #[schemars(length(min = 1, max = 4096))]
    pub(in crate::mcp) path: String,
    #[schemars(length(min = 1, max = 4096))]
    pub(in crate::mcp) symbol: String,
    #[serde(default)]
    #[schemars(length(min = 1, max = 4096))]
    pub(in crate::mcp) head_path: Option<String>,
    #[serde(default)]
    #[schemars(length(min = 1, max = 4096))]
    pub(in crate::mcp) head_symbol: Option<String>,
}

#[derive(Debug, Clone)]
pub(in crate::mcp) enum HistoryMcpCall {
    Single(HistoryRequest),
    DiffSymbols(DiffSymbolsRequest),
}

impl HistoryMcpRequest {
    pub(in crate::mcp) fn validate_limits(&self, limits: McpLimitPolicy) -> crate::Result<()> {
        validate_optional_positive_limit("max_results", self.max_results, MAX_RESULTS)?;
        validate_optional_positive_limit("max_tokens", self.max_tokens, limits.max_output_tokens)?;
        validate_optional_positive_limit(
            "max_response_tokens",
            self.max_response_tokens,
            limits.max_response_tokens,
        )
    }

    pub(in crate::mcp) fn into_parts(
        self,
    ) -> crate::Result<(HistoryMcpCall, ServiceCallOptions, Option<String>)> {
        let has_cursor = self.cursor.is_some();
        let cursor = self.cursor.clone();
        let call = match self.operation {
            HistoryMcpOperation::ReadSymbol {
                path,
                symbol,
                revision,
            } => HistoryMcpCall::Single(HistoryRequest {
                operation: HistoryOperation::ReadSymbol {
                    path,
                    symbol,
                    revision,
                },
                max_results: self.max_results,
                max_tokens: self.max_tokens,
            }),
            HistoryMcpOperation::DiffSymbol {
                path,
                symbol,
                base_revision,
                head_revision,
            } => HistoryMcpCall::Single(HistoryRequest {
                operation: HistoryOperation::DiffSymbol {
                    path,
                    symbol,
                    base_revision,
                    head_revision,
                },
                max_results: self.max_results,
                max_tokens: self.max_tokens,
            }),
            HistoryMcpOperation::DiffSymbols {
                targets,
                base_revision,
                head_revision,
            } => HistoryMcpCall::DiffSymbols(DiffSymbolsRequest {
                targets: targets
                    .into_iter()
                    .map(|target| DiffSymbolsTarget {
                        path: target.path,
                        symbol: target.symbol,
                        head_path: target.head_path,
                        head_symbol: target.head_symbol,
                    })
                    .collect(),
                base_revision,
                head_revision,
                max_results: self.max_results,
                max_tokens: self.max_tokens,
                cursor,
            }),
            HistoryMcpOperation::SymbolLog {
                path,
                symbol,
                revision,
            } => HistoryMcpCall::Single(HistoryRequest {
                operation: HistoryOperation::SymbolLog {
                    path,
                    symbol,
                    revision,
                },
                max_results: self.max_results,
                max_tokens: self.max_tokens,
            }),
        };
        if has_cursor && matches!(call, HistoryMcpCall::Single(_)) {
            return Err(crate::Error::InvalidInput {
                field: "cursor",
                reason: "is only valid for diff_symbols",
            });
        }
        Ok((
            call,
            service_call_options(self.max_response_tokens),
            self.expected_repository_id,
        ))
    }
}
