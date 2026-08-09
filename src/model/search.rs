use super::*;
use serde::ser::SerializeStruct;

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
/// Search candidate source.
pub enum SearchMode {
    /// Combine structural and lexical candidates.
    #[default]
    Auto,
    /// Match a literal substring.
    Text,
    /// Verify a Rust regular expression over indexed chunks.
    Regex,
    /// Search identifier tokens and structural names.
    Identifier,
    /// Search definitions only.
    Symbol,
    /// Search syntactic references only.
    Reference,
}

impl SearchMode {
    /// Modes that can produce an exhaustive occurrence result.
    pub(crate) const EXHAUSTIVE_MODES: [Self; 2] = [Self::Text, Self::Regex];

    pub(crate) fn supports_all_occurrences(self) -> bool {
        Self::EXHAUSTIVE_MODES.contains(&self)
    }

    pub(crate) const fn wire_name(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Text => "text",
            Self::Regex => "regex",
            Self::Identifier => "identifier",
            Self::Symbol => "symbol",
            Self::Reference => "reference",
        }
    }
}

pub(crate) const EXHAUSTIVE_SEARCH_MODES: &[&str] = &["text", "regex"];
pub(crate) const RANKED_SYMBOL_SEARCH_EXAMPLE: &str =
    r#"{"operation":{"kind":"symbol","query":"Services"}}"#;
pub(crate) const EXHAUSTIVE_TEXT_SEARCH_EXAMPLE: &str = r#"{"operation":{"kind":"text","query":"Services","all_occurrences":true,"projection":"occurrences"}}"#;

pub(crate) fn incompatible_occurrence_options(
    mode: SearchMode,
    mut conflicting_options: Vec<String>,
) -> crate::Error {
    conflicting_options.insert(0, format!("mode={}", mode.wire_name()));
    crate::Error::InvalidSearchOptions {
        field: "all_occurrences",
        allowed_modes: EXHAUSTIVE_SEARCH_MODES,
        conflicting_options,
        ranked_symbol_example: RANKED_SYMBOL_SEARCH_EXAMPLE,
        exhaustive_text_example: EXHAUSTIVE_TEXT_SEARCH_EXAMPLE,
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
/// Explicit lifecycle action for one exhaustive-query coverage receipt.
pub enum QueryReceiptAction {
    /// Record a receipt only when the exhaustive result is returned completely.
    Record,
    /// Reuse a prior complete receipt instead of repeating the exhaustive scan.
    Reuse {
        /// Opaque server-managed query receipt.
        #[schemars(length(max = 128))]
        receipt_id: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
/// Input for `leantoken.search`.
pub struct SearchRequest {
    /// Text, identifier, symbol, or regular expression to find.
    pub query: String,
    /// Candidate source to search.
    #[serde(default)]
    pub mode: SearchMode,
    /// Include only matching repository paths.
    #[serde(default)]
    pub include_paths: Vec<String>,
    /// Exclude matching repository paths.
    #[serde(default)]
    pub exclude_paths: Vec<String>,
    /// Boost matching repository paths without filtering other results.
    #[serde(default)]
    pub focus_paths: Vec<String>,
    /// Maximum hits to return.
    #[serde(default)]
    pub max_results: Option<usize>,
    /// Maximum source tokens across returned excerpts.
    #[serde(default)]
    pub max_tokens: Option<usize>,
    /// Lines included before and after each match.
    #[serde(default)]
    pub context_lines: Option<usize>,
    /// Preserve query case when matching.
    #[serde(default)]
    pub case_sensitive: bool,
    /// Return every text or regex occurrence instead of one hit per indexed chunk.
    #[serde(default)]
    pub all_occurrences: bool,
    /// Prefer a structural definition when lexical and structural channels find the same definition.
    #[serde(default)]
    pub prefer_structural: bool,
    /// Server-managed receipt whose previously returned evidence should be suppressed.
    #[serde(default)]
    pub receipt_id: Option<String>,
    /// Explicit record or reuse action for an exhaustive text or regex query.
    #[serde(default)]
    pub query_receipt: Option<QueryReceiptAction>,
    /// Cursor returned by an earlier response from the same generation.
    #[serde(default)]
    pub cursor: Option<String>,
}

/// Exact source coordinates for one lexical search occurrence.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct SearchOccurrence {
    /// One-based line containing the start of the match.
    pub start_line: usize,
    /// One-based line containing the end of the match.
    pub end_line: usize,
    /// Zero-based UTF-8 byte column of the match start on `start_line`.
    #[serde(default)]
    pub start_column: usize,
    /// Zero-based exclusive UTF-8 byte column of the match end on `end_line`.
    #[serde(default)]
    pub end_column: usize,
    /// Zero-based UTF-8 byte offset of the match start in the indexed file.
    pub start_byte: usize,
    /// Zero-based exclusive UTF-8 byte offset of the match end in the indexed file.
    pub end_byte: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SearchHit {
    pub path: String,
    pub start_line: usize,
    pub end_line: usize,
    pub excerpt: String,
    pub match_kind: String,
    /// Search channels represented by this possibly merged hit.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub match_kinds: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<ReferenceRole>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub symbol: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enclosing_symbol: Option<String>,
    /// Exact match coordinates when exhaustive occurrence search is enabled.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub occurrence: Option<SearchOccurrence>,
    pub score: f64,
    /// Score normalized against the strongest candidate in this response query.
    #[serde(default)]
    pub normalized_score: f64,
    pub score_reasons: Vec<String>,
    pub content_hash: String,
}

/// Returned and omitted hit counts for one search channel.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct SearchCoverageCount {
    /// Deduplicated hits available before pagination and token limits.
    pub total: usize,
    /// Hits from this channel returned in the current response.
    pub returned: usize,
    /// Available hits from this channel not returned in the current response.
    pub truncated: usize,
}

/// Search coverage separated by evidence channel.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct SearchCoverage {
    /// Structural definition hits.
    pub definitions: SearchCoverageCount,
    /// Structural reference hits.
    pub references: SearchCoverageCount,
    /// Lexical text and regex hits.
    pub text_matches: SearchCoverageCount,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SearchResponse {
    pub hits: Vec<SearchHit>,
    /// Per-channel availability and current-page coverage.
    #[serde(default)]
    pub coverage: SearchCoverage,
    /// Occurrences returned in this response page after token limits.
    #[serde(default)]
    pub occurrences_returned: usize,
    /// Exact filtered occurrence count when `all_occurrences` is enabled.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub occurrences_total: Option<usize>,
    pub meta: ResponseMeta,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
/// Compact line/column coordinates for one exhaustive lexical occurrence.
pub struct SearchOccurrenceCoordinate {
    /// One-based line containing the start of the match.
    pub line: usize,
    /// One-based end line when a regular expression spans multiple lines.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_line: Option<usize>,
    /// Zero-based UTF-8 byte column on `line`.
    pub start_column: usize,
    /// Zero-based exclusive UTF-8 byte column on `end_line`, or `line` for a single-line match.
    pub end_column: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
/// One excerpt shared by every exhaustive occurrence it contains.
pub struct SearchOccurrenceGroup {
    pub path: String,
    pub start_line: usize,
    pub end_line: usize,
    /// Omitted by `coordinates_only` responses.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub excerpt: Option<String>,
    /// Hash of `excerpt`; omitted by `coordinates_only` responses.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_hash: Option<String>,
    pub occurrences: Vec<SearchOccurrenceCoordinate>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
/// Persistence outcome for an explicit exhaustive-query receipt action.
pub enum QueryReceiptStatus {
    /// A complete result was persisted after response fitting succeeded.
    Recorded,
    /// A prior complete result proved the requested query without rescanning source chunks.
    AlreadyCovered,
    /// The scan completed, but pagination or token selection omitted occurrences.
    NotRecordedIncompleteResponse,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
/// Relationship between the requested path scope and the recorded proof scope.
pub enum QueryReceiptScopeRelation {
    /// The normalized include/exclude scopes are identical.
    Exact,
    /// A zero-match proof over a conservative syntactic superset covers this scope.
    Subset,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
/// Bounded proof metadata for one explicit exhaustive-query receipt action.
pub struct QueryReceiptOutcome {
    pub status: QueryReceiptStatus,
    /// Present only for persisted or reused complete receipts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub receipt_id: Option<String>,
    /// Whether this outcome is a complete reusable coverage proof.
    pub complete: bool,
    /// Exact match count for the requested predicate.
    pub match_count: usize,
    /// BLAKE3 commitment to the normalized requested predicate.
    pub requested_predicate_blake3: String,
    /// BLAKE3 commitment to the predicate whose proof was recorded.
    pub covered_predicate_blake3: String,
    /// BLAKE3 commitment to the complete ordered result set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result_blake3: Option<String>,
    /// Repository generation where the persisted proof was originally recorded.
    pub receipt_generation: u64,
    /// Whether unchanged relevant indexed partitions allowed reuse in a later generation.
    #[serde(default)]
    pub reused_across_generation: bool,
    /// Exact scope reuse or a conservative zero-result subset proof.
    pub scope_relation: QueryReceiptScopeRelation,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
/// Exhaustive lexical matches without repeated excerpts or ranked-hit metadata.
pub struct SearchOccurrencesResponse {
    pub groups: Vec<SearchOccurrenceGroup>,
    pub groups_returned: usize,
    pub occurrences_returned: usize,
    pub occurrences_total: usize,
    pub coordinates_only: bool,
    pub coverage: SearchCoverage,
    /// Present only when the caller explicitly records or reuses query coverage.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub query_receipt: Option<QueryReceiptOutcome>,
    pub meta: ResponseMeta,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
/// One verifiable source excerpt retained by grouped search.
pub struct SearchGroupEvidence {
    pub path: String,
    pub start_line: usize,
    pub end_line: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub excerpt: Option<String>,
    pub content_hash: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub match_kinds: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<ReferenceRole>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
/// Reference locations summarized by file without repeating excerpts or scores.
pub struct SearchReferenceGroup {
    pub path: String,
    pub count: usize,
    pub start_line: usize,
    pub end_line: usize,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub roles: Vec<ReferenceRole>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
/// Search hits grouped by their matched symbol or enclosing structural scope.
pub struct SearchGroup {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub symbol: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub definition: Option<SearchGroupEvidence>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub representative: Option<SearchGroupEvidence>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub references: Vec<SearchReferenceGroup>,
    pub text_matches: usize,
    pub total_hits: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
/// Opt-in grouped search response with the same coverage and freshness contract.
pub struct SearchGroupedResponse {
    pub groups: Vec<SearchGroup>,
    pub coverage: SearchCoverage,
    pub hits_returned: usize,
    pub groups_returned: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub occurrences_total: Option<usize>,
    pub meta: ResponseMeta,
}

#[derive(Debug, Clone)]
/// Evaluation-only search result with deterministic execution counters.
///
/// This is not part of the MCP surface. Benchmarks use it to distinguish
/// candidate reduction from response behavior without adding timing assertions
/// or diagnostics to normal responses.
pub struct SearchEvaluation {
    /// Normal token-bounded search response.
    pub response: SearchResponse,
    /// Request-local counts for the lexical execution phases.
    pub phases: SearchPhaseCounters,
    /// Privacy-safe generation-scoped storage primitive identities.
    pub primitive_keys: Vec<RetrievalPrimitiveKey>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
/// Evaluation-only identity for one logical retrieval primitive.
///
/// The digest includes the pinned repository generation and normalized
/// primitive inputs. Raw queries, paths, symbols, and file identifiers are not
/// exposed.
pub struct RetrievalPrimitiveKey {
    /// Primitive family, such as `trigram` or `adaptive_excerpt`.
    pub kind: String,
    /// BLAKE3 digest of the versioned generation-scoped normalized inputs.
    pub key_blake3: String,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
/// Candidate source used to verify a regex request.
pub enum RegexCandidateStrategy {
    /// The regex had no sound bounded trigram plan and scanned indexed chunks.
    #[default]
    FullScan,
    /// A sound trigram expression selected candidate chunks before verification.
    Trigram,
}

/// Regex candidate-planning decision with mutually exclusive diagnostics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegexPlanningOutcome {
    /// No sound bounded candidate plan was selected.
    FullScan {
        /// Stable reason planning fell back, absent before a regex scan runs.
        fallback_reason: Option<RegexPlanFallbackReason>,
    },
    /// A sound trigram candidate expression was selected.
    Trigram {
        /// HIR analysis that produced the plan.
        source: RegexPlanSource,
    },
}

impl Default for RegexPlanningOutcome {
    fn default() -> Self {
        Self::FullScan {
            fallback_reason: None,
        }
    }
}

impl RegexPlanningOutcome {
    /// Candidate strategy implied by this planning outcome.
    #[must_use]
    pub const fn strategy(self) -> RegexCandidateStrategy {
        match self {
            Self::FullScan { .. } => RegexCandidateStrategy::FullScan,
            Self::Trigram { .. } => RegexCandidateStrategy::Trigram,
        }
    }

    /// Planner source, present only for a selected trigram plan.
    #[must_use]
    pub const fn source(self) -> Option<RegexPlanSource> {
        match self {
            Self::Trigram { source } => Some(source),
            Self::FullScan { .. } => None,
        }
    }

    /// Fallback reason, present only for a diagnosed full scan.
    #[must_use]
    pub const fn fallback_reason(self) -> Option<RegexPlanFallbackReason> {
        match self {
            Self::FullScan { fallback_reason } => fallback_reason,
            Self::Trigram { .. } => None,
        }
    }
}

#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
/// HIR analysis that produced a sound trigram candidate plan.
pub enum RegexPlanSource {
    /// Existing recursive analysis found mandatory inner literal terms.
    MandatoryLiterals,
    /// Bounded extraction found a finite set of required match prefixes.
    PrefixLiterals,
    /// Bounded extraction found a finite set of required match suffixes.
    SuffixLiterals,
}

#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
/// Privacy-safe reason a regex request retained the bounded full-scan path.
pub enum RegexPlanFallbackReason {
    /// The evaluation explicitly forced the full-scan oracle.
    PlanningDisabled,
    /// Unicode case folding cannot be represented by SQLite's ASCII folding.
    CaseInsensitiveUnicode,
    /// The HIR parser unexpectedly rejected a regex accepted by the matcher.
    HirParseFailed,
    /// Recursive analysis crossed its bounded HIR-node limit.
    PlanNodeLimit,
    /// Candidate translation crossed its bounded term-count limit.
    PlanTermLimit,
    /// Candidate translation crossed its bounded aggregate term-byte limit.
    PlanTermBytesLimit,
    /// Prefix and suffix literal sequences were infinite or not indexable.
    LiteralSequenceUnavailable,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
/// Deterministic phase and candidate counts for an evaluation-only search.
pub struct SearchPhaseCounters {
    /// Candidate strategy and its applicable diagnostic payload.
    pub regex_planning: RegexPlanningOutcome,
    /// HIR nodes visited before selecting or rejecting a bounded plan.
    pub regex_plan_nodes: usize,
    /// Trigram terms in the selected candidate plan.
    pub regex_plan_terms: usize,
    /// Aggregate bytes in selected word-trigram terms.
    pub regex_plan_term_bytes: usize,
    /// Indexed files in the pinned repository snapshot.
    ///
    /// Candidate plans report corpus scale without scanning these files. The
    /// full-scan fallback checks each file against its structural bounds.
    pub regex_files_considered: usize,
    /// Chunks loaded by the full-scan fallback.
    pub regex_chunks_loaded: usize,
    /// Rows returned by the trigram candidate query.
    pub regex_candidate_chunks: usize,
    /// Candidate chunks verified with the compiled regex.
    pub regex_chunks_verified: usize,
    /// Matching chunks retained for occurrence hydration.
    pub regex_retained_chunks: usize,
}

impl Serialize for SearchPhaseCounters {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut state = serializer.serialize_struct("SearchPhaseCounters", 11)?;
        state.serialize_field("regex_candidate_strategy", &self.regex_planning.strategy())?;
        state.serialize_field("regex_plan_source", &self.regex_planning.source())?;
        state.serialize_field(
            "regex_plan_fallback_reason",
            &self.regex_planning.fallback_reason(),
        )?;
        state.serialize_field("regex_plan_nodes", &self.regex_plan_nodes)?;
        state.serialize_field("regex_plan_terms", &self.regex_plan_terms)?;
        state.serialize_field("regex_plan_term_bytes", &self.regex_plan_term_bytes)?;
        state.serialize_field("regex_files_considered", &self.regex_files_considered)?;
        state.serialize_field("regex_chunks_loaded", &self.regex_chunks_loaded)?;
        state.serialize_field("regex_candidate_chunks", &self.regex_candidate_chunks)?;
        state.serialize_field("regex_chunks_verified", &self.regex_chunks_verified)?;
        state.serialize_field("regex_retained_chunks", &self.regex_retained_chunks)?;
        state.end()
    }
}
