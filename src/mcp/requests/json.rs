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
    /// Maximum tokens across selected/projected JSON (default 8000, maximum 32000).
    #[serde(default, deserialize_with = "deserialize_optional_limit")]
    #[schemars(schema_with = "token_limit_schema", default = "default_token_option")]
    pub(in crate::mcp) max_tokens: Option<usize>,
    /// Maximum tokens in the final serialized service response.
    #[serde(default)]
    #[schemars(schema_with = "response_token_limit_schema")]
    pub(in crate::mcp) max_response_tokens: Option<usize>,
    /// Maximum structural items returned (default 1000, maximum 10000).
    #[serde(default, deserialize_with = "deserialize_optional_limit")]
    #[schemars(range(min = 1, max = 10000))]
    pub(in crate::mcp) max_items: Option<usize>,
    /// Array elements sampled by collapsed projections (default 3, maximum 20).
    #[serde(default)]
    #[schemars(range(min = 0, max = 20))]
    pub(in crate::mcp) array_sample_size: Option<usize>,
    /// Maximum keys traversal depth relative to the selected root (root is zero).
    #[serde(default)]
    #[schemars(range(min = 0, max = 64))]
    pub(in crate::mcp) depth: Option<usize>,
    /// Opaque cursor returned by an incomplete keys projection.
    #[serde(default)]
    #[schemars(length(max = 256))]
    pub(in crate::mcp) cursor: Option<String>,
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
        expression: String,
    },
}

impl From<JsonMcpSelector> for JsonSelector {
    fn from(value: JsonMcpSelector) -> Self {
        match value {
            JsonMcpSelector::Pointer { pointer } => Self::Pointer { pointer },
            JsonMcpSelector::Jmespath { expression } => Self::Jmespath { expression },
        }
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(in crate::mcp) enum JsonMcpOperation {
    /// Select and project one JSON value.
    Query {
        #[schemars(length(min = 1, max = 4096))]
        path: String,
        #[serde(default)]
        selector: Option<JsonMcpSelector>,
        #[serde(default)]
        projection: JsonProjection,
    },
    /// Summarize numeric leaves below one JSON selection.
    NumericSummary {
        #[schemars(length(min = 1, max = 4096))]
        path: String,
        #[serde(default)]
        selector: Option<JsonMcpSelector>,
    },
    /// Compare selected fields between two JSON files.
    DiffFields {
        #[schemars(length(min = 1, max = 4096))]
        base_path: String,
        #[schemars(length(min = 1, max = 4096))]
        head_path: String,
        #[schemars(length(min = 1, max = 100))]
        selectors: Vec<JsonMcpSelector>,
        #[serde(default)]
        projection: JsonProjection,
    },
}

impl JsonMcpRequest {
    pub(in crate::mcp) fn validate_limits(&self, limits: McpLimitPolicy) -> crate::Result<()> {
        validate_optional_positive_limit("max_tokens", self.max_tokens, limits.max_output_tokens)?;
        validate_optional_positive_limit(
            "max_response_tokens",
            self.max_response_tokens,
            limits.max_response_tokens,
        )?;
        validate_optional_positive_limit("max_items", self.max_items, 10_000)?;
        if self.array_sample_size.is_some_and(|value| value > 20) {
            return Err(crate::Error::RequestLimitExceeded {
                field: "array_sample_size",
                requested: self.array_sample_size.unwrap_or_default(),
                limit: 20,
            });
        }
        if self.depth.is_some_and(|value| value > MAX_JSON_DEPTH) {
            return Err(crate::Error::RequestLimitExceeded {
                field: "depth",
                requested: self.depth.unwrap_or_default(),
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
        let operation = match self.operation {
            JsonMcpOperation::Query {
                path,
                selector,
                projection,
            } => JsonOperation::Query {
                path,
                selector: selector.map(Into::into),
                projection,
            },
            JsonMcpOperation::NumericSummary { path, selector } => JsonOperation::NumericSummary {
                path,
                selector: selector.map(Into::into),
            },
            JsonMcpOperation::DiffFields {
                base_path,
                head_path,
                selectors,
                projection,
            } => JsonOperation::DiffFields {
                base_path,
                head_path,
                selectors: selectors.into_iter().map(Into::into).collect(),
                projection,
            },
        };
        let execution = JsonExecutionOptions::mcp(self.depth);
        (
            JsonRequest {
                operation,
                max_tokens: self.max_tokens,
                max_items: self.max_items,
                array_sample_size: self.array_sample_size,
                cursor: self.cursor,
            },
            service_call_options(self.max_response_tokens),
            execution,
            self.expected_repository_id,
        )
    }
}
