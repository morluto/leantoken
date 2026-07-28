use super::*;

/// Selector used by structural JSON operations.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum JsonSelector {
    /// RFC 6901 JSON Pointer.
    Pointer {
        /// Empty for the root or a slash-prefixed JSON Pointer.
        pointer: String,
    },
    /// Standard JMESPath expression.
    Jmespath {
        /// Expression evaluated against the complete JSON document.
        expression: String,
    },
}

/// Structural projection applied after JSON selection.
#[derive(Debug, Default, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum JsonProjection {
    /// Preserve the selected JSON value.
    #[default]
    Value,
    /// Replace arrays with count and bounded sample summaries.
    Collapsed,
    /// Return JSON Pointer-shaped key paths and value types only.
    Keys,
    /// Return an inferred structural schema without leaf values.
    Schema,
}

/// Structural JSON retrieval operation.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum JsonOperation {
    /// Select and structurally project one JSON value.
    Query {
        /// Repository-relative JSON file.
        path: String,
        /// Optional root-relative selector.
        #[serde(default)]
        selector: Option<JsonSelector>,
        /// Projection applied to the selected value.
        #[serde(default)]
        projection: JsonProjection,
    },
    /// Summarize every numeric leaf below one selected value.
    NumericSummary {
        /// Repository-relative JSON file.
        path: String,
        /// Optional root-relative selector.
        #[serde(default)]
        selector: Option<JsonSelector>,
    },
    /// Compare selected fields between two live JSON files.
    DiffFields {
        /// Repository-relative base JSON file.
        base_path: String,
        /// Repository-relative comparison JSON file.
        head_path: String,
        /// Non-empty selectors evaluated independently against both files.
        selectors: Vec<JsonSelector>,
        /// Projection applied to each present selected value.
        #[serde(default)]
        projection: JsonProjection,
    },
}

/// Input for bounded structural JSON retrieval.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct JsonRequest {
    /// Structural operation and its file targets.
    pub operation: JsonOperation,
    /// Maximum tokens across returned selected/projected JSON; defaults to 8000.
    #[serde(default)]
    pub max_tokens: Option<usize>,
    /// Maximum structural items returned; defaults to 1000.
    #[serde(default)]
    pub max_items: Option<usize>,
    /// Array elements sampled by `collapsed`; defaults to 3.
    #[serde(default)]
    pub array_sample_size: Option<usize>,
    /// Opaque cursor returned by an incomplete `keys` projection.
    #[serde(default)]
    pub cursor: Option<String>,
}

/// Descriptive statistics for numeric JSON leaves.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct JsonNumericSummary {
    /// Finite numeric leaves included in the statistics.
    pub count: usize,
    /// Non-numeric scalar leaves ignored below the selection.
    pub non_numeric_count: usize,
    /// Minimum numeric value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min: Option<f64>,
    /// Median numeric value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub median: Option<f64>,
    /// Nearest-rank 95th percentile.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub p95: Option<f64>,
    /// Maximum numeric value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max: Option<f64>,
}

/// One selector comparison between two JSON files.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct JsonFieldDiff {
    /// Selector evaluated against both documents.
    pub selector: JsonSelector,
    /// Whether the selector exists in the base document.
    pub before_present: bool,
    /// Projected base value when present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before: Option<serde_json::Value>,
    /// Whether the selector exists in the comparison document.
    pub after_present: bool,
    /// Projected comparison value when present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after: Option<serde_json::Value>,
    /// Whether presence or the selected value changed.
    pub changed: bool,
}

/// Exact live JSON source represented by a structural response.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct JsonSource {
    /// Repository-relative file path.
    pub path: String,
    /// Hash of the complete UTF-8 file contents.
    pub content_hash: String,
    /// Complete source byte length.
    pub bytes: usize,
}

/// Bound that prevented a structural JSON response from being complete.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum JsonIncompleteReason {
    /// The structural item page limit was reached.
    MaxItems,
    /// The projected JSON token page limit was reached.
    MaxTokens,
}

/// Bounded structural JSON response.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct JsonResponse {
    /// Resolved operation kind.
    pub kind: String,
    /// Selected/projected value for `query`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<serde_json::Value>,
    /// Statistics for `numeric_summary`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub numeric_summary: Option<JsonNumericSummary>,
    /// Selector comparisons for `diff_fields`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub differences: Vec<JsonFieldDiff>,
    /// Exact live files represented by this response.
    pub sources: Vec<JsonSource>,
    /// Whether structural item and token caps omitted no requested output.
    pub result_complete: bool,
    /// Exact structural items in the selected projection when diagnostics apply.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_items: Option<usize>,
    /// Structural items emitted in this response page when diagnostics apply.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub returned_items: Option<usize>,
    /// Structural items still unread after this response page when diagnostics apply.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remaining_items: Option<usize>,
    /// Bound responsible for an incomplete structural projection.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub incomplete_reason: Option<JsonIncompleteReason>,
    pub meta: ResponseMeta,
}
