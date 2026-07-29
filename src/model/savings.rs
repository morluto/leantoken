use super::*;

#[derive(
    Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq, PartialOrd, Ord,
)]
#[serde(rename_all = "snake_case")]
/// Source retrieval operation included in token-savings accounting.
pub enum TokenSavingsOperation {
    /// Indexed source search.
    Search,
    /// Structural file outline.
    Outline,
    /// Exact source read.
    Read,
    /// Ranked task context.
    Context,
}

impl TokenSavingsOperation {
    pub(crate) const ALL: [Self; 4] = [Self::Search, Self::Outline, Self::Read, Self::Context];

    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Search => "search",
            Self::Outline => "outline",
            Self::Read => "read",
            Self::Context => "context",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
/// Cumulative source-token estimate for one retrieval operation.
pub struct TokenSavingsByOperation {
    /// Retrieval operation represented by this row.
    pub operation: TokenSavingsOperation,
    /// Number of successful responses included in the estimate.
    pub tracked_requests: u64,
    /// Source tokens in the corresponding direct-read baseline.
    pub baseline_source_tokens: u64,
    /// Source tokens returned by LeanToken.
    pub emitted_source_tokens: u64,
    /// Saturating per-request difference between baseline and emitted source tokens.
    pub estimated_source_tokens_saved: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
/// Cumulative, repository-local estimate of source tokens avoided by retrieval.
pub struct TokenSavingsResponse {
    /// Tokenizer used for the tracked source counts.
    pub tokenizer: String,
    /// Whether the configured tokenizer provides exact local source counts.
    pub token_count_exact: bool,
    /// Stable description of the baseline used by this estimate.
    pub estimate_basis: String,
    /// Number of successful source responses included in the estimate.
    pub tracked_requests: u64,
    /// Source tokens in the corresponding direct-read baselines.
    pub baseline_source_tokens: u64,
    /// Source tokens returned by LeanToken.
    pub emitted_source_tokens: u64,
    /// Sum of saturating per-request baseline reductions.
    pub estimated_source_tokens_saved: u64,
    /// Fixed-shape breakdown for every tracked retrieval operation.
    pub by_operation: Vec<TokenSavingsByOperation>,
}

#[derive(
    Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq, PartialOrd, Ord,
)]
#[serde(rename_all = "snake_case")]
/// Retrieval operation included in full-response token accounting.
pub enum TokenAccountingOperation {
    /// Repository path discovery.
    Files,
    /// Indexed source search.
    Search,
    /// Structural file outline.
    Outline,
    /// Exact source read.
    Read,
    /// Ranked context planning without source materialization.
    ContextPlan,
    /// Ranked task context with source materialization.
    Context,
    /// Structural JSON query.
    Json,
    /// Immutable symbol history.
    History,
    /// Exact-only carry of prior receipt evidence into one current generation.
    ReceiptRebase,
}

impl TokenAccountingOperation {
    pub(crate) const ALL: [Self; 9] = [
        Self::Files,
        Self::Search,
        Self::Outline,
        Self::Read,
        Self::ContextPlan,
        Self::Context,
        Self::Json,
        Self::History,
        Self::ReceiptRebase,
    ];

    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Files => "files",
            Self::Search => "search",
            Self::Outline => "outline",
            Self::Read => "read",
            Self::ContextPlan => "context_plan",
            Self::Context => "context",
            Self::Json => "json",
            Self::History => "history",
            Self::ReceiptRebase => "receipt_rebase",
        }
    }

    pub(crate) fn from_str(value: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|operation| operation.as_str() == value)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
/// Full-response token accounting for one retrieval operation.
pub struct ResponseTokenAccountingByOperation {
    /// Retrieval operation represented by this row.
    pub operation: TokenAccountingOperation,
    /// Number of successful structured responses included in the row.
    pub tracked_requests: u64,
    /// Responses with a represented-source baseline.
    pub baseline_requests: u64,
    /// Source tokens in represented direct-read baselines.
    pub baseline_source_tokens: u64,
    /// Source tokens selected into LeanToken responses.
    pub response_source_tokens: u64,
    /// Tokens attributed to response paths, metadata, and repeated result structure.
    pub path_and_metadata_tokens: u64,
    /// Tokens attributed to the compact response envelope.
    pub protocol_tokens: u64,
    /// Tokens in complete serialized responses, including accounting fields.
    pub total_response_tokens: u64,
    /// Baseline tokens minus complete response tokens; negative values are net cost.
    pub estimated_net_tokens_saved: i64,
    /// Evidence items omitted by exact receipt suppression.
    pub receipt_suppressed_exact: u64,
    /// Evidence items omitted by overlapping-range receipt suppression.
    pub receipt_suppressed_overlap: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
/// Repository-local accounting for complete successful retrieval responses.
pub struct ResponseTokenAccounting {
    /// Stable description of which responses and costs are included.
    pub accounting_scope: String,
    /// Stable description of the net-savings calculation.
    pub estimate_basis: String,
    /// Number of successful structured responses included.
    pub tracked_requests: u64,
    /// Responses with a represented-source baseline.
    pub baseline_requests: u64,
    /// Source tokens in represented direct-read baselines.
    pub baseline_source_tokens: u64,
    /// Source tokens selected into LeanToken responses.
    pub response_source_tokens: u64,
    /// Tokens attributed to response paths, metadata, and repeated result structure.
    pub path_and_metadata_tokens: u64,
    /// Tokens attributed to compact response envelopes.
    pub protocol_tokens: u64,
    /// Tokens in complete serialized responses, including accounting fields.
    pub total_response_tokens: u64,
    /// Baseline tokens minus complete response tokens; negative values are net cost.
    pub estimated_net_tokens_saved: i64,
    /// Evidence items omitted by exact receipt suppression.
    pub receipt_suppressed_exact: u64,
    /// Evidence items omitted by overlapping-range receipt suppression.
    pub receipt_suppressed_overlap: u64,
    /// Fixed-shape breakdown for every accounted retrieval operation.
    pub by_operation: Vec<ResponseTokenAccountingByOperation>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
/// Source-only savings plus complete successful-response token accounting.
pub struct TokenSavingsReport {
    /// Backward-compatible source-only savings fields.
    #[serde(flatten)]
    pub source_savings: TokenSavingsResponse,
    /// Full-response costs and net estimate for every retrieval operation.
    pub response_accounting: ResponseTokenAccounting,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
/// Count of observed failed service requests for one operation and error category.
pub struct ServiceFailureObservation {
    /// Retrieval operation that returned the error.
    pub operation: TokenAccountingOperation,
    /// Stable, non-sensitive error variant category.
    pub error_category: String,
    /// Number of best-effort failure records persisted for this category.
    pub failed_requests: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
/// Directly observed request and suppression counters.
pub struct TokenSavingsObservations {
    /// Stable description of where records are captured and when they may be skipped.
    pub observation_scope: String,
    /// Successful service response records persisted after final accounting.
    pub successful_response_records: u64,
    /// Successful responses with a represented-source baseline.
    pub responses_with_baseline: u64,
    /// Backward-compatible source-compression comparisons.
    pub source_compression_requests: u64,
    /// Failed service requests persisted at an instrumented operation boundary.
    pub failed_service_requests: u64,
    /// Exact `expected_hash` matches that returned `not_modified`.
    pub expected_hash_not_modified_responses: u64,
    /// Requested source tokens omitted by exact `expected_hash` matches.
    pub expected_hash_suppressed_source_tokens: u64,
    /// Mutually exclusive observed request outcome classes.
    pub request_classification: TokenSavingsRequestClassification,
    /// Fixed-order breakdown of observed failures by operation and category.
    pub failed_by_operation_and_category: Vec<ServiceFailureObservation>,
    /// Outcomes that cannot be inferred without a host task/outcome identity.
    pub unobserved: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
/// Backward-compatible savings report plus directly observed service outcomes.
pub struct ObservedTokenSavingsReport {
    /// Existing source and full-response accounting fields.
    #[serde(flatten)]
    pub report: TokenSavingsReport,
    /// Additive counters whose observation boundaries are explicitly documented.
    pub observations: TokenSavingsObservations,
}

#[derive(
    Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq, PartialOrd, Ord,
)]
#[serde(rename_all = "snake_case")]
/// Outcome assigned to one successful retrieval for aggregate savings accounting.
pub enum TokenSavingsRequestClass {
    /// Returned supported evidence or useful negative evidence.
    Useful,
    /// Returned a bounded or explicitly partial result.
    Incomplete,
    /// Could not provide the requested structural retrieval contract.
    Unsupported,
    /// Suppressed unchanged source because an explicit or automatic base hash matched.
    HashSuppressed,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
/// Aggregate mutually exclusive retrieval outcome counts.
pub struct TokenSavingsRequestClassification {
    /// Successful complete supported retrievals.
    pub useful: u64,
    /// Successful bounded or explicitly partial retrievals.
    pub incomplete: u64,
    /// Successful unsupported results plus typed unsupported-language errors.
    pub unsupported: u64,
    /// Successful exact `expected_hash` suppression.
    pub hash_suppressed: u64,
    /// Successful records created before outcome classification was available.
    pub legacy_unclassified: u64,
    /// Observed errors other than typed unsupported-language outcomes.
    pub failed: u64,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
/// Time window represented by a savings snapshot response.
pub enum TokenSavingsWindow {
    /// All persisted aggregate records.
    Lifetime,
    /// Aggregate counters added after the supplied opaque snapshot.
    Delta,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
/// Observed accounting plus a caller-carried opaque aggregate snapshot.
pub struct TokenSavingsSnapshotReport {
    #[serde(flatten)]
    pub observed: ObservedTokenSavingsReport,
    /// Opaque state that can be supplied to a later savings request.
    pub snapshot: String,
    /// Whether the report covers lifetime totals or a snapshot delta.
    pub window: TokenSavingsWindow,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct LanguageCount {
    pub language: String,
    pub files: usize,
}
