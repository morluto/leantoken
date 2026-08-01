use super::*;
use crate::model::QueryReceiptAction;

/// A search query that preserves significant leading and trailing whitespace.
#[derive(Debug, Clone, Serialize, PartialEq, Eq, Hash)]
#[serde(transparent)]
pub(in crate::mcp) struct SearchMcpQuery(String);

impl JsonSchema for SearchMcpQuery {
    fn inline_schema() -> bool {
        true
    }

    fn schema_name() -> std::borrow::Cow<'static, str> {
        "SearchMcpQuery".into()
    }

    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "description": "Non-empty search query; leading and trailing whitespace is significant.",
            "type": "string",
            "minLength": 1,
            "maxLength": 65536
        })
    }
}

impl<'de> Deserialize<'de> for SearchMcpQuery {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        if value.trim().is_empty() {
            Err(serde::de::Error::custom(
                "must not be empty or whitespace-only",
            ))
        } else {
            Ok(Self(value))
        }
    }
}

impl SearchMcpQuery {
    fn into_string(self) -> String {
        self.0
    }
}

#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(in crate::mcp) enum SearchMcpProjection {
    #[default]
    Auto,
    Full,
    Grouped,
    Occurrences,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(in crate::mcp) struct SearchMcpRequest {
    #[serde(default)]
    #[schemars(schema_with = "expected_repository_id_schema")]
    pub(in crate::mcp) expected_repository_id: Option<String>,
    /// Search semantics and all bounds are owned by the selected tagged operation.
    pub(in crate::mcp) operation: SearchMcpOperation,
}

#[derive(Debug, Clone, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(in crate::mcp) enum SearchMcpOperation {
    Auto {
        #[serde(flatten)]
        options: SearchMcpOptions,
    },
    Text {
        #[serde(flatten)]
        options: SearchMcpOptions,
    },
    Regex {
        #[serde(flatten)]
        options: SearchMcpOptions,
    },
    Identifier {
        #[serde(flatten)]
        options: SearchMcpOptions,
    },
    Symbol {
        #[serde(flatten)]
        options: SearchMcpOptions,
    },
    Reference {
        #[serde(flatten)]
        options: SearchMcpOptions,
    },
}

impl<'de> Deserialize<'de> for SearchMcpOperation {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let mut value = serde_json::Value::deserialize(deserializer)?;
        let object = value
            .as_object_mut()
            .ok_or_else(|| serde::de::Error::custom("search operation must be an object"))?;
        let kind = object
            .get("kind")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| serde::de::Error::missing_field("kind"))?;
        object.remove("kind");
        let options = serde_json::from_value::<SearchMcpOptions>(serde_json::Value::Object(
            std::mem::take(object),
        ))
        .map_err(|error| serde::de::Error::custom(error.to_string()))?;
        match kind.as_str() {
            "auto" => Ok(Self::Auto { options }),
            "text" => Ok(Self::Text { options }),
            "regex" => Ok(Self::Regex { options }),
            "identifier" => Ok(Self::Identifier { options }),
            "symbol" => Ok(Self::Symbol { options }),
            "reference" => Ok(Self::Reference { options }),
            _ => Err(serde::de::Error::unknown_variant(
                &kind,
                &["auto", "text", "regex", "identifier", "symbol", "reference"],
            )),
        }
    }
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(
    description = "Search options are structurally generated here; runtime validation additionally enforces cross-field rules: all_occurrences requires text or regex mode, occurrences projection requires all_occurrences=true, coordinates_only requires all_occurrences=true with auto or occurrences projection, and query_receipt requires occurrence projection."
)]
pub(in crate::mcp) struct SearchMcpOptions {
    pub(in crate::mcp) query: SearchMcpQuery,
    #[serde(default)]
    #[schemars(length(max = 256), inner(length(max = 4096)))]
    pub(in crate::mcp) include_paths: Vec<RepositoryPattern>,
    #[serde(default)]
    #[schemars(length(max = 256), inner(length(max = 4096)))]
    pub(in crate::mcp) exclude_paths: Vec<RepositoryPattern>,
    #[serde(default)]
    #[schemars(length(max = 256), inner(length(max = 4096)))]
    pub(in crate::mcp) focus_paths: Vec<RepositoryPattern>,
    #[serde(default)]
    #[schemars(schema_with = "result_limit_schema", default = "default_result_option")]
    pub(in crate::mcp) max_results: Option<usize>,
    #[serde(default)]
    #[schemars(schema_with = "token_limit_schema", default = "default_token_option")]
    pub(in crate::mcp) max_tokens: Option<usize>,
    #[serde(default)]
    #[schemars(schema_with = "response_token_limit_schema")]
    pub(in crate::mcp) max_response_tokens: Option<usize>,
    #[serde(default)]
    #[schemars(
        schema_with = "context_line_limit_schema",
        default = "default_context_line_option"
    )]
    pub(in crate::mcp) context_lines: Option<usize>,
    #[serde(default)]
    pub(in crate::mcp) case_sensitive: bool,
    #[serde(default)]
    pub(in crate::mcp) all_occurrences: bool,
    #[serde(default)]
    pub(in crate::mcp) coordinates_only: bool,
    #[serde(default)]
    pub(in crate::mcp) prefer_structural: bool,
    #[serde(default)]
    #[schemars(length(max = 128))]
    pub(in crate::mcp) receipt_id: Option<String>,
    #[serde(default)]
    pub(in crate::mcp) query_receipt: Option<QueryReceiptAction>,
    #[serde(default)]
    #[schemars(length(max = 4096))]
    pub(in crate::mcp) cursor: Option<String>,
    #[serde(default)]
    #[schemars(schema_with = "index_consistency_schema")]
    pub(in crate::mcp) consistency: IndexConsistency,
    #[serde(default)]
    pub(in crate::mcp) projection: SearchMcpProjection,
}

impl SearchMcpOperation {
    fn mode(&self) -> SearchMode {
        match self {
            Self::Auto { .. } => SearchMode::Auto,
            Self::Text { .. } => SearchMode::Text,
            Self::Regex { .. } => SearchMode::Regex,
            Self::Identifier { .. } => SearchMode::Identifier,
            Self::Symbol { .. } => SearchMode::Symbol,
            Self::Reference { .. } => SearchMode::Reference,
        }
    }

    fn options(&self) -> &SearchMcpOptions {
        match self {
            Self::Auto { options }
            | Self::Text { options }
            | Self::Regex { options }
            | Self::Identifier { options }
            | Self::Symbol { options }
            | Self::Reference { options } => options,
        }
    }

    fn into_parts(self) -> (SearchMode, SearchMcpOptions) {
        match self {
            Self::Auto { options } => (SearchMode::Auto, options),
            Self::Text { options } => (SearchMode::Text, options),
            Self::Regex { options } => (SearchMode::Regex, options),
            Self::Identifier { options } => (SearchMode::Identifier, options),
            Self::Symbol { options } => (SearchMode::Symbol, options),
            Self::Reference { options } => (SearchMode::Reference, options),
        }
    }
}

impl SearchMcpRequest {
    pub(in crate::mcp) fn validate_limits(&self, limits: McpLimitPolicy) -> crate::Result<()> {
        let options = self.operation.options();
        validate_optional_positive_limit("max_results", options.max_results, limits.max_results)?;
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
        validate_optional_limit(
            "context_lines",
            options.context_lines,
            limits.max_context_lines,
        )?;
        if options.all_occurrences && !self.operation.mode().supports_all_occurrences() {
            let mut conflicts = vec!["all_occurrences=true".into()];
            if options.projection == SearchMcpProjection::Occurrences {
                conflicts.push("projection=occurrences".into());
            }
            if options.coordinates_only {
                conflicts.push("coordinates_only=true".into());
            }
            return Err(crate::incompatible_occurrence_options(
                self.operation.mode(),
                conflicts,
            ));
        }
        if options.coordinates_only && !options.all_occurrences {
            return Err(crate::Error::InvalidInput {
                field: "coordinates_only",
                reason: "requires all_occurrences=true",
            });
        }
        if options.coordinates_only
            && !matches!(
                options.projection,
                SearchMcpProjection::Auto | SearchMcpProjection::Occurrences
            )
        {
            return Err(crate::Error::InvalidInput {
                field: "coordinates_only",
                reason: "requires the occurrences projection",
            });
        }
        if options.projection == SearchMcpProjection::Occurrences && !options.all_occurrences {
            return Err(crate::Error::InvalidInput {
                field: "projection",
                reason: "occurrences requires all_occurrences=true",
            });
        }
        if options.query_receipt.is_some()
            && !matches!(
                options.projection,
                SearchMcpProjection::Auto | SearchMcpProjection::Occurrences
            )
        {
            return Err(crate::Error::InvalidInput {
                field: "query_receipt",
                reason: "requires the occurrences projection",
            });
        }
        Ok(())
    }

    pub(in crate::mcp) fn max_response_tokens(&self) -> Option<usize> {
        self.operation.options().max_response_tokens
    }

    pub(in crate::mcp) fn into_parts(
        self,
    ) -> (
        SearchRequest,
        SearchMcpProjection,
        bool,
        IndexConsistency,
        ServiceCallOptions,
        Option<String>,
    ) {
        let (mode, options) = self.operation.into_parts();
        let projection = match options.projection {
            SearchMcpProjection::Auto if options.all_occurrences => {
                SearchMcpProjection::Occurrences
            }
            SearchMcpProjection::Auto => SearchMcpProjection::Full,
            projection => projection,
        };
        (
            SearchRequest {
                query: options.query.into_string(),
                mode,
                include_paths: options
                    .include_paths
                    .iter()
                    .map(|pattern| pattern.as_str().to_owned())
                    .collect(),
                exclude_paths: options
                    .exclude_paths
                    .iter()
                    .map(|pattern| pattern.as_str().to_owned())
                    .collect(),
                focus_paths: options
                    .focus_paths
                    .iter()
                    .map(|pattern| pattern.as_str().to_owned())
                    .collect(),
                max_results: options.max_results,
                max_tokens: options.max_tokens,
                context_lines: options.context_lines,
                case_sensitive: options.case_sensitive,
                all_occurrences: options.all_occurrences,
                prefer_structural: options.prefer_structural,
                receipt_id: options.receipt_id,
                query_receipt: options.query_receipt,
                cursor: options.cursor,
            },
            projection,
            options.coordinates_only,
            options.consistency,
            service_call_options(options.max_response_tokens),
            self.expected_repository_id,
        )
    }
}
