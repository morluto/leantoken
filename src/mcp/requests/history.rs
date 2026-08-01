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
}

#[derive(Debug, Clone, Default, Deserialize, JsonSchema)]
pub(in crate::mcp) struct HistoryMcpLimits {
    /// Maximum results (default 20; service-specific operations may use a lower bound).
    #[serde(default)]
    #[schemars(schema_with = "result_limit_schema", default = "default_result_option")]
    pub(in crate::mcp) max_results: Option<usize>,
    /// Maximum source or diff tokens to return (default 8000, maximum 32000).
    #[serde(default)]
    #[schemars(schema_with = "token_limit_schema", default = "default_token_option")]
    pub(in crate::mcp) max_tokens: Option<usize>,
    /// Maximum tokens in the final serialized service response.
    #[serde(default)]
    #[schemars(schema_with = "response_token_limit_schema")]
    pub(in crate::mcp) max_response_tokens: Option<usize>,
}

#[derive(Debug, Clone, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(in crate::mcp) enum HistoryMcpOperation {
    /// Read one parsed symbol, optionally qualified as `parent.name`, from an immutable revision.
    ReadSymbol {
        #[schemars(length(min = 1, max = 4096))]
        path: RepositoryPath,
        #[schemars(length(min = 1, max = 4096))]
        symbol: SymbolIdentity,
        #[schemars(length(min = 1, max = 4096))]
        revision: NonEmptyText,
        #[serde(flatten)]
        limits: HistoryMcpLimits,
    },
    /// Compare one parsed symbol across revisions, including added or removed endpoints.
    DiffSymbol {
        #[schemars(length(min = 1, max = 4096))]
        path: RepositoryPath,
        #[schemars(length(min = 1, max = 4096))]
        symbol: SymbolIdentity,
        #[schemars(length(min = 1, max = 4096))]
        base_revision: NonEmptyText,
        #[schemars(length(min = 1, max = 4096))]
        head_revision: NonEmptyText,
        #[serde(flatten)]
        limits: HistoryMcpLimits,
    },
    /// Diff an ordered symbol set with shared revisions, metadata, and bounded Git work.
    DiffSymbols {
        #[schemars(length(min = 1, max = "crate::services::MAX_DIFF_SYMBOL_TARGETS"))]
        targets: Vec<HistoryMcpTarget>,
        #[schemars(length(min = 1, max = 4096))]
        base_revision: NonEmptyText,
        #[schemars(length(min = 1, max = 4096))]
        head_revision: NonEmptyText,
        #[serde(flatten)]
        limits: HistoryMcpLimits,
        /// Opaque cursor returned by `diff_symbols`; reuse the exact operation.
        #[serde(default)]
        #[schemars(length(min = 1, max = 128))]
        cursor: Option<String>,
    },
    /// List commits that touched the symbol's tracked historical lines.
    SymbolLog {
        #[schemars(length(min = 1, max = 4096))]
        path: RepositoryPath,
        #[schemars(length(min = 1, max = 4096))]
        symbol: SymbolIdentity,
        #[serde(default)]
        #[schemars(length(min = 1, max = 4096))]
        revision: Option<NonEmptyText>,
        #[serde(flatten)]
        limits: HistoryMcpLimits,
    },
}

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum HistoryMcpOperationWire {
    ReadSymbol {
        path: RepositoryPath,
        symbol: SymbolIdentity,
        revision: NonEmptyText,
        #[serde(default)]
        max_results: Option<usize>,
        #[serde(default)]
        max_tokens: Option<usize>,
        #[serde(default)]
        max_response_tokens: Option<usize>,
    },
    DiffSymbol {
        path: RepositoryPath,
        symbol: SymbolIdentity,
        base_revision: NonEmptyText,
        head_revision: NonEmptyText,
        #[serde(default)]
        max_results: Option<usize>,
        #[serde(default)]
        max_tokens: Option<usize>,
        #[serde(default)]
        max_response_tokens: Option<usize>,
    },
    DiffSymbols {
        targets: Vec<HistoryMcpTarget>,
        base_revision: NonEmptyText,
        head_revision: NonEmptyText,
        #[serde(default)]
        max_results: Option<usize>,
        #[serde(default)]
        max_tokens: Option<usize>,
        #[serde(default)]
        max_response_tokens: Option<usize>,
        #[serde(default)]
        cursor: Option<String>,
    },
    SymbolLog {
        path: RepositoryPath,
        symbol: SymbolIdentity,
        #[serde(default)]
        revision: Option<NonEmptyText>,
        #[serde(default)]
        max_results: Option<usize>,
        #[serde(default)]
        max_tokens: Option<usize>,
        #[serde(default)]
        max_response_tokens: Option<usize>,
    },
}

fn history_limits(
    max_results: Option<usize>,
    max_tokens: Option<usize>,
    max_response_tokens: Option<usize>,
) -> HistoryMcpLimits {
    HistoryMcpLimits {
        max_results,
        max_tokens,
        max_response_tokens,
    }
}

impl<'de> Deserialize<'de> for HistoryMcpOperation {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        Ok(match HistoryMcpOperationWire::deserialize(deserializer)? {
            HistoryMcpOperationWire::ReadSymbol {
                path,
                symbol,
                revision,
                max_results,
                max_tokens,
                max_response_tokens,
            } => Self::ReadSymbol {
                path,
                symbol,
                revision,
                limits: history_limits(max_results, max_tokens, max_response_tokens),
            },
            HistoryMcpOperationWire::DiffSymbol {
                path,
                symbol,
                base_revision,
                head_revision,
                max_results,
                max_tokens,
                max_response_tokens,
            } => Self::DiffSymbol {
                path,
                symbol,
                base_revision,
                head_revision,
                limits: history_limits(max_results, max_tokens, max_response_tokens),
            },
            HistoryMcpOperationWire::DiffSymbols {
                targets,
                base_revision,
                head_revision,
                max_results,
                max_tokens,
                max_response_tokens,
                cursor,
            } => Self::DiffSymbols {
                targets,
                base_revision,
                head_revision,
                limits: history_limits(max_results, max_tokens, max_response_tokens),
                cursor,
            },
            HistoryMcpOperationWire::SymbolLog {
                path,
                symbol,
                revision,
                max_results,
                max_tokens,
                max_response_tokens,
            } => Self::SymbolLog {
                path,
                symbol,
                revision,
                limits: history_limits(max_results, max_tokens, max_response_tokens),
            },
        })
    }
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(in crate::mcp) struct HistoryMcpTarget {
    #[schemars(length(min = 1, max = 4096))]
    pub(in crate::mcp) path: RepositoryPath,
    #[schemars(length(min = 1, max = 4096))]
    pub(in crate::mcp) symbol: SymbolIdentity,
    #[serde(default)]
    #[schemars(length(min = 1, max = 4096))]
    pub(in crate::mcp) head_path: Option<RepositoryPath>,
    #[serde(default)]
    #[schemars(length(min = 1, max = 4096))]
    pub(in crate::mcp) head_symbol: Option<SymbolIdentity>,
}

#[derive(Debug, Clone)]
pub(in crate::mcp) enum HistoryMcpCall {
    Single(HistoryRequest),
    DiffSymbols(DiffSymbolsRequest),
}

impl HistoryMcpRequest {
    pub(in crate::mcp) fn max_response_tokens(&self) -> Option<usize> {
        match &self.operation {
            HistoryMcpOperation::ReadSymbol { limits, .. }
            | HistoryMcpOperation::DiffSymbol { limits, .. }
            | HistoryMcpOperation::SymbolLog { limits, .. } => limits.max_response_tokens,
            HistoryMcpOperation::DiffSymbols { limits, .. } => limits.max_response_tokens,
        }
    }

    pub(in crate::mcp) fn validate_limits(&self, limits: McpLimitPolicy) -> crate::Result<()> {
        let options = match &self.operation {
            HistoryMcpOperation::ReadSymbol { limits, .. }
            | HistoryMcpOperation::DiffSymbol { limits, .. }
            | HistoryMcpOperation::SymbolLog { limits, .. } => limits,
            HistoryMcpOperation::DiffSymbols { limits, .. } => limits,
        };
        validate_optional_positive_limit("max_results", options.max_results, MAX_RESULTS)?;
        validate_optional_positive_limit(
            "max_tokens",
            options.max_tokens,
            limits.max_output_tokens,
        )?;
        validate_optional_positive_limit(
            "max_response_tokens",
            options.max_response_tokens,
            limits.max_response_tokens,
        )
    }

    pub(in crate::mcp) fn into_parts(
        self,
    ) -> crate::Result<(HistoryMcpCall, ServiceCallOptions, Option<String>)> {
        let expected_repository_id = self.expected_repository_id;
        let (call, max_response_tokens) = match self.operation {
            HistoryMcpOperation::ReadSymbol {
                path,
                symbol,
                revision,
                limits,
            } => (
                HistoryMcpCall::Single(HistoryRequest {
                    operation: HistoryOperation::ReadSymbol {
                        path: path.into_string(),
                        symbol: symbol.qualified_name(),
                        revision: revision.into_string(),
                    },
                    max_results: limits.max_results,
                    max_tokens: limits.max_tokens,
                }),
                limits.max_response_tokens,
            ),
            HistoryMcpOperation::DiffSymbol {
                path,
                symbol,
                base_revision,
                head_revision,
                limits,
            } => (
                HistoryMcpCall::Single(HistoryRequest {
                    operation: HistoryOperation::DiffSymbol {
                        path: path.into_string(),
                        symbol: symbol.qualified_name(),
                        base_revision: base_revision.into_string(),
                        head_revision: head_revision.into_string(),
                    },
                    max_results: limits.max_results,
                    max_tokens: limits.max_tokens,
                }),
                limits.max_response_tokens,
            ),
            HistoryMcpOperation::DiffSymbols {
                targets,
                base_revision,
                head_revision,
                limits,
                cursor,
            } => (
                HistoryMcpCall::DiffSymbols(DiffSymbolsRequest {
                    targets: targets
                        .into_iter()
                        .map(|target| DiffSymbolsTarget {
                            path: target.path.into_string(),
                            symbol: target.symbol.qualified_name(),
                            head_path: target.head_path.map(RepositoryPath::into_string),
                            head_symbol: target.head_symbol.map(|symbol| symbol.qualified_name()),
                        })
                        .collect(),
                    base_revision: base_revision.into_string(),
                    head_revision: head_revision.into_string(),
                    max_results: limits.max_results,
                    max_tokens: limits.max_tokens,
                    cursor,
                }),
                limits.max_response_tokens,
            ),
            HistoryMcpOperation::SymbolLog {
                path,
                symbol,
                revision,
                limits,
            } => (
                HistoryMcpCall::Single(HistoryRequest {
                    operation: HistoryOperation::SymbolLog {
                        path: path.into_string(),
                        symbol: symbol.qualified_name(),
                        revision: revision.map(NonEmptyText::into_string),
                    },
                    max_results: limits.max_results,
                    max_tokens: limits.max_tokens,
                }),
                limits.max_response_tokens,
            ),
        };
        Ok((
            call,
            service_call_options(max_response_tokens),
            expected_repository_id,
        ))
    }
}
