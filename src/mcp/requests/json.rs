use super::*;

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(in crate::mcp) struct JsonMcpRequest {
    /// Expected opaque repository identity from an earlier response.
    #[serde(default)]
    #[schemars(schema_with = "expected_repository_id_schema")]
    pub(in crate::mcp) expected_repository_id: Option<String>,
    /// Structural JSON operation.
    pub(in crate::mcp) operation: JsonMcpOperation,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub(in crate::mcp) struct JsonMcpLimits {
    /// Maximum tokens across selected/projected JSON (default 8000, maximum 32000).
    #[serde(default)]
    #[schemars(schema_with = "token_limit_schema", default = "default_token_option")]
    pub(in crate::mcp) max_tokens: Option<usize>,
    /// Maximum tokens in the final serialized service response.
    #[serde(default)]
    #[schemars(schema_with = "response_token_limit_schema")]
    pub(in crate::mcp) max_response_tokens: Option<usize>,
    /// Maximum structural items returned (default 1000, maximum 10000).
    #[serde(default)]
    #[schemars(range(min = 1, max = 10000))]
    pub(in crate::mcp) max_items: Option<usize>,
    /// Array elements sampled by collapsed projections (default 3, maximum 20).
    #[serde(default)]
    #[schemars(range(min = 0, max = 20))]
    pub(in crate::mcp) array_sample_size: Option<usize>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(in crate::mcp) enum JsonMcpSelector {
    /// RFC 6901 JSON Pointer.
    Pointer {
        #[schemars(length(max = 4096))]
        pointer: String,
    },
    /// Standard JMESPath expression.
    Jmespath {
        #[schemars(length(min = 1, max = 4096))]
        expression: NonEmptyText,
    },
}

impl From<JsonMcpSelector> for JsonSelector {
    fn from(value: JsonMcpSelector) -> Self {
        match value {
            JsonMcpSelector::Pointer { pointer } => Self::Pointer { pointer },
            JsonMcpSelector::Jmespath { expression } => Self::Jmespath {
                expression: expression.into_string(),
            },
        }
    }
}

#[derive(Debug, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(in crate::mcp) enum JsonMcpOperation {
    /// Select and project one JSON value.
    Query {
        #[schemars(length(min = 1, max = 4096))]
        path: RepositoryPath,
        #[serde(default)]
        selector: Option<JsonMcpSelector>,
        #[serde(default)]
        projection: JsonProjection,
        #[serde(flatten)]
        limits: JsonMcpLimits,
        /// Maximum keys traversal depth relative to the selected root (root is zero).
        #[serde(default)]
        #[schemars(range(min = 0, max = 64))]
        depth: Option<usize>,
        /// Opaque cursor returned by an incomplete keys projection.
        #[serde(default)]
        #[schemars(length(max = 256))]
        cursor: Option<String>,
    },
    /// Summarize numeric leaves below one JSON selection.
    NumericSummary {
        #[schemars(length(min = 1, max = 4096))]
        path: RepositoryPath,
        #[serde(default)]
        selector: Option<JsonMcpSelector>,
        #[serde(flatten)]
        limits: JsonMcpLimits,
    },
    /// Compare selected fields between two JSON files.
    DiffFields {
        #[schemars(length(min = 1, max = 4096))]
        base_path: RepositoryPath,
        #[schemars(length(min = 1, max = 4096))]
        head_path: RepositoryPath,
        #[schemars(length(min = 1, max = 100))]
        selectors: Vec<JsonMcpSelector>,
        #[serde(default)]
        projection: JsonProjection,
        #[serde(flatten)]
        limits: JsonMcpLimits,
    },
}

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum JsonMcpOperationWire {
    Query {
        path: RepositoryPath,
        #[serde(default)]
        selector: Option<JsonMcpSelector>,
        #[serde(default)]
        projection: JsonProjection,
        #[serde(default)]
        max_tokens: Option<usize>,
        #[serde(default)]
        max_response_tokens: Option<usize>,
        #[serde(default)]
        max_items: Option<usize>,
        #[serde(default)]
        array_sample_size: Option<usize>,
        #[serde(default)]
        depth: Option<usize>,
        #[serde(default)]
        cursor: Option<String>,
    },
    NumericSummary {
        path: RepositoryPath,
        #[serde(default)]
        selector: Option<JsonMcpSelector>,
        #[serde(default)]
        max_tokens: Option<usize>,
        #[serde(default)]
        max_response_tokens: Option<usize>,
        #[serde(default)]
        max_items: Option<usize>,
        #[serde(default)]
        array_sample_size: Option<usize>,
    },
    DiffFields {
        base_path: RepositoryPath,
        head_path: RepositoryPath,
        selectors: Vec<JsonMcpSelector>,
        #[serde(default)]
        projection: JsonProjection,
        #[serde(default)]
        max_tokens: Option<usize>,
        #[serde(default)]
        max_response_tokens: Option<usize>,
        #[serde(default)]
        max_items: Option<usize>,
        #[serde(default)]
        array_sample_size: Option<usize>,
    },
}

fn json_limits(
    max_tokens: Option<usize>,
    max_response_tokens: Option<usize>,
    max_items: Option<usize>,
    array_sample_size: Option<usize>,
) -> JsonMcpLimits {
    JsonMcpLimits {
        max_tokens,
        max_response_tokens,
        max_items,
        array_sample_size,
    }
}

impl<'de> Deserialize<'de> for JsonMcpOperation {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        Ok(match JsonMcpOperationWire::deserialize(deserializer)? {
            JsonMcpOperationWire::Query {
                path,
                selector,
                projection,
                max_tokens,
                max_response_tokens,
                max_items,
                array_sample_size,
                depth,
                cursor,
            } => Self::Query {
                path,
                selector,
                projection,
                limits: json_limits(
                    max_tokens,
                    max_response_tokens,
                    max_items,
                    array_sample_size,
                ),
                depth,
                cursor,
            },
            JsonMcpOperationWire::NumericSummary {
                path,
                selector,
                max_tokens,
                max_response_tokens,
                max_items,
                array_sample_size,
            } => Self::NumericSummary {
                path,
                selector,
                limits: json_limits(
                    max_tokens,
                    max_response_tokens,
                    max_items,
                    array_sample_size,
                ),
            },
            JsonMcpOperationWire::DiffFields {
                base_path,
                head_path,
                selectors,
                projection,
                max_tokens,
                max_response_tokens,
                max_items,
                array_sample_size,
            } => Self::DiffFields {
                base_path,
                head_path,
                selectors,
                projection,
                limits: json_limits(
                    max_tokens,
                    max_response_tokens,
                    max_items,
                    array_sample_size,
                ),
            },
        })
    }
}

impl JsonMcpRequest {
    pub(in crate::mcp) fn validate_limits(&self, limits: McpLimitPolicy) -> crate::Result<()> {
        let options = match &self.operation {
            JsonMcpOperation::Query { limits, .. } => limits,
            JsonMcpOperation::NumericSummary { limits, .. }
            | JsonMcpOperation::DiffFields { limits, .. } => limits,
        };
        validate_optional_positive_limit(
            "max_tokens",
            options.max_tokens,
            limits.max_output_tokens,
        )?;
        validate_optional_positive_limit(
            "max_response_tokens",
            options.max_response_tokens,
            limits.max_response_tokens,
        )?;
        validate_optional_positive_limit("max_items", options.max_items, 10_000)?;
        if options.array_sample_size.is_some_and(|value| value > 20) {
            return Err(crate::Error::RequestLimitExceeded {
                field: "array_sample_size",
                requested: options.array_sample_size.unwrap_or_default(),
                limit: 20,
            });
        }
        let depth = match &self.operation {
            JsonMcpOperation::Query { depth, .. } => *depth,
            JsonMcpOperation::NumericSummary { .. } | JsonMcpOperation::DiffFields { .. } => None,
        };
        if depth.is_some_and(|value| value > MAX_JSON_DEPTH) {
            return Err(crate::Error::RequestLimitExceeded {
                field: "depth",
                requested: depth.unwrap_or_default(),
                limit: MAX_JSON_DEPTH,
            });
        }
        Ok(())
    }

    pub(in crate::mcp) fn into_parts(
        self,
    ) -> (
        JsonRequest,
        ServiceCallOptions,
        JsonExecutionOptions,
        Option<String>,
    ) {
        let expected_repository_id = self.expected_repository_id;
        let (operation, limits, depth, cursor) = match self.operation {
            JsonMcpOperation::Query {
                path,
                selector,
                projection,
                limits,
                depth,
                cursor,
            } => (
                JsonOperation::Query {
                    path: path.into_string(),
                    selector: selector.map(Into::into),
                    projection,
                },
                limits,
                depth,
                cursor,
            ),
            JsonMcpOperation::NumericSummary {
                path,
                selector,
                limits,
            } => (
                JsonOperation::NumericSummary {
                    path: path.into_string(),
                    selector: selector.map(Into::into),
                },
                limits,
                None,
                None,
            ),
            JsonMcpOperation::DiffFields {
                base_path,
                head_path,
                selectors,
                projection,
                limits,
            } => (
                JsonOperation::DiffFields {
                    base_path: base_path.into_string(),
                    head_path: head_path.into_string(),
                    selectors: selectors.into_iter().map(Into::into).collect(),
                    projection,
                },
                limits,
                None,
                None,
            ),
        };
        let execution = JsonExecutionOptions::mcp(depth);
        (
            JsonRequest {
                operation,
                max_tokens: limits.max_tokens,
                max_items: limits.max_items,
                array_sample_size: limits.array_sample_size,
                cursor,
            },
            service_call_options(limits.max_response_tokens),
            execution,
            expected_repository_id,
        )
    }

    pub(in crate::mcp) fn max_response_tokens(&self) -> Option<usize> {
        match &self.operation {
            JsonMcpOperation::Query { limits, .. } => limits.max_response_tokens,
            JsonMcpOperation::NumericSummary { limits, .. }
            | JsonMcpOperation::DiffFields { limits, .. } => limits.max_response_tokens,
        }
    }
}
