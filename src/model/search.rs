use super::*;

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

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
/// Exhaustive lexical matches without repeated excerpts or ranked-hit metadata.
pub struct SearchOccurrencesResponse {
    pub groups: Vec<SearchOccurrenceGroup>,
    pub groups_returned: usize,
    pub occurrences_returned: usize,
    pub occurrences_total: usize,
    pub coordinates_only: bool,
    pub coverage: SearchCoverage,
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

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
/// Deterministic phase and candidate counts for an evaluation-only search.
pub struct SearchPhaseCounters {
    /// Candidate source selected for a regex request.
    pub regex_candidate_strategy: RegexCandidateStrategy,
    /// Mandatory trigram terms in the selected candidate plan.
    pub regex_plan_terms: usize,
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
