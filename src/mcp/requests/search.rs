use super::*;
use crate::model::QueryReceiptAction;

#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
/// Response projection for indexed search.
pub(in crate::mcp) enum SearchMcpProjection {
    /// Select `occurrences` for exhaustive lexical search and `full` otherwise.
    #[default]
    Auto,
    /// Preserve the complete ranked-hit response.
    Full,
    /// Group the selected page into symbol or file summaries.
    Grouped,
    /// Share each exhaustive lexical excerpt across its exact occurrence coordinates.
    Occurrences,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(transform = add_search_option_constraints)]
pub(in crate::mcp) struct SearchMcpRequest {
    /// Expected opaque repository identity from an earlier response.
    #[serde(default)]
    #[schemars(schema_with = "expected_repository_id_schema")]
    pub(in crate::mcp) expected_repository_id: Option<String>,
    /// Non-empty text, identifier, symbol, or Rust regular expression to find.
    #[schemars(length(min = 1, max = 65536))]
    pub(in crate::mcp) query: String,
    /// Candidate source to search (default `auto`).
    #[serde(default)]
    pub(in crate::mcp) mode: SearchMode,
    /// Include only matching repository paths.
    #[serde(default)]
    #[schemars(length(max = 256), inner(length(max = 4096)))]
    pub(in crate::mcp) include_paths: Vec<String>,
    /// Exclude matching repository paths.
    #[serde(default)]
    #[schemars(length(max = 256), inner(length(max = 4096)))]
    pub(in crate::mcp) exclude_paths: Vec<String>,
    /// Boost matching paths without filtering other results.
    #[serde(default)]
    #[schemars(length(max = 256), inner(length(max = 4096)))]
    pub(in crate::mcp) focus_paths: Vec<String>,
    /// Maximum hits to return (default 20, maximum 100).
    #[serde(default, deserialize_with = "deserialize_optional_limit")]
    #[schemars(schema_with = "result_limit_schema", default = "default_result_option")]
    pub(in crate::mcp) max_results: Option<usize>,
    /// Maximum source tokens across excerpts (default 8000, maximum 32000).
    #[serde(default, deserialize_with = "deserialize_optional_limit")]
    #[schemars(schema_with = "token_limit_schema", default = "default_token_option")]
    pub(in crate::mcp) max_tokens: Option<usize>,
    /// Maximum tokens in the final serialized service response.
    #[serde(default)]
    #[schemars(schema_with = "response_token_limit_schema")]
    pub(in crate::mcp) max_response_tokens: Option<usize>,
    /// Lines before and after each match (default 2, maximum 20).
    #[serde(default, deserialize_with = "deserialize_optional_limit")]
    #[schemars(
        schema_with = "context_line_limit_schema",
        default = "default_context_line_option"
    )]
    pub(in crate::mcp) context_lines: Option<usize>,
    /// Preserve query case when matching.
    #[serde(default)]
    pub(in crate::mcp) case_sensitive: bool,
    /// Return every text or regex occurrence with exact coordinates and counts;
    /// requires `mode=text` or `mode=regex`.
    #[serde(default)]
    pub(in crate::mcp) all_occurrences: bool,
    /// Omit excerpts and hashes from an exhaustive occurrence response.
    #[serde(default)]
    pub(in crate::mcp) coordinates_only: bool,
    /// Prefer structural definitions when identifier channels find the same definition.
    #[serde(default)]
    pub(in crate::mcp) prefer_structural: bool,
    /// Suppress evidence already returned under this server-managed receipt.
    #[serde(default)]
    #[schemars(length(max = 128))]
    pub(in crate::mcp) receipt_id: Option<String>,
    /// Explicitly record or reuse complete exhaustive-query coverage.
    #[serde(default)]
    pub(in crate::mcp) query_receipt: Option<QueryReceiptAction>,
    /// Cursor returned by the same search and repository generation.
    #[serde(default)]
    #[schemars(length(max = 4096))]
    pub(in crate::mcp) cursor: Option<String>,
    /// Use `reconcile_working_tree` after edits; otherwise `indexed_generation`.
    #[serde(default)]
    #[schemars(schema_with = "index_consistency_schema")]
    pub(in crate::mcp) consistency: IndexConsistency,
    /// Response shape. Exhaustive searches default to `occurrences`; others default to `full`.
    #[serde(default)]
    pub(in crate::mcp) projection: SearchMcpProjection,
}

impl SearchMcpRequest {
    pub(in crate::mcp) fn validate_limits(&self, limits: McpLimitPolicy) -> crate::Result<()> {
        validate_optional_positive_limit("max_results", self.max_results, limits.max_results)?;
        validate_optional_positive_limit("max_tokens", self.max_tokens, limits.max_output_tokens)?;
        validate_optional_positive_limit(
            "max_response_tokens",
            self.max_response_tokens,
            limits.max_response_tokens,
        )?;
        validate_optional_limit(
            "context_lines",
            self.context_lines,
            limits.max_context_lines,
        )?;
        if self.all_occurrences && !self.mode.supports_all_occurrences() {
            return Err(crate::Error::InvalidInput {
                field: "all_occurrences",
                reason: "requires text or regex mode",
            });
        }
        if self.coordinates_only && !self.all_occurrences {
            return Err(crate::Error::InvalidInput {
                field: "coordinates_only",
                reason: "requires all_occurrences=true",
            });
        }
        if self.coordinates_only
            && !matches!(
                self.projection,
                SearchMcpProjection::Auto | SearchMcpProjection::Occurrences
            )
        {
            return Err(crate::Error::InvalidInput {
                field: "coordinates_only",
                reason: "requires the occurrences projection",
            });
        }
        if self.projection == SearchMcpProjection::Occurrences && !self.all_occurrences {
            return Err(crate::Error::InvalidInput {
                field: "projection",
                reason: "occurrences requires all_occurrences=true",
            });
        }
        if self.query_receipt.is_some()
            && !matches!(
                self.projection,
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
        let projection = match self.projection {
            SearchMcpProjection::Auto if self.all_occurrences => SearchMcpProjection::Occurrences,
            SearchMcpProjection::Auto => SearchMcpProjection::Full,
            projection => projection,
        };
        (
            SearchRequest {
                query: self.query,
                mode: self.mode,
                include_paths: self.include_paths,
                exclude_paths: self.exclude_paths,
                focus_paths: self.focus_paths,
                max_results: self.max_results,
                max_tokens: self.max_tokens,
                context_lines: self.context_lines,
                case_sensitive: self.case_sensitive,
                all_occurrences: self.all_occurrences,
                prefer_structural: self.prefer_structural,
                receipt_id: self.receipt_id,
                query_receipt: self.query_receipt,
                cursor: self.cursor,
            },
            projection,
            self.coordinates_only,
            self.consistency,
            service_call_options(self.max_response_tokens),
            self.expected_repository_id,
        )
    }
}

pub(in crate::mcp) fn add_search_option_constraints(schema: &mut Schema) {
    let exhaustive_modes = SearchMode::EXHAUSTIVE_MODES.map(SearchMode::wire_name);
    schema.insert(
        "allOf".into(),
        serde_json::json!([
            {
                "if": {
                    "properties": {"all_occurrences": {"const": true}},
                    "required": ["all_occurrences"]
                },
                "then": {
                    "properties": {
                        "mode": {"enum": exhaustive_modes}
                    },
                    "required": ["mode"]
                }
            },
            {
                "if": {
                    "properties": {"projection": {"const": "occurrences"}},
                    "required": ["projection"]
                },
                "then": {
                    "properties": {"all_occurrences": {"const": true}},
                    "required": ["all_occurrences"]
                }
            }
        ]),
    );
}
