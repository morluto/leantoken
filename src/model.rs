use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
/// State of the committed index while a response is produced.
pub enum Freshness {
    /// No reconciliation is active.
    Current,
    /// A query used the last committed generation during reconciliation.
    Reconciling,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
/// Readiness of the repository index for retrieval.
pub enum IndexState {
    /// No index generation has completed.
    Uninitialized,
    /// At least one committed generation is available.
    Ready,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
/// Consistency boundary applied before repository retrieval.
pub enum IndexConsistency {
    /// Query the latest committed index generation without waiting for filesystem changes.
    #[default]
    Committed,
    /// Reconcile the current working tree before querying the resulting generation.
    WorkingTree,
}

/// Requested or resolved evidence workflow for context retrieval.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ContextWorkflow {
    /// Infer a workflow only from high-confidence task language.
    #[default]
    Auto,
    /// General feature, fix, and refactor implementation evidence.
    Implementation,
    /// Repository guidance, templates, validation, changed files, and owner tests.
    Contribution,
    /// Changed code, repository guidance, validation, and review evidence.
    Review,
    /// Diagnostic evidence for tracing behavior and root causes.
    Investigation,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ResponseMeta {
    /// Stable opaque identity for the canonical repository root.
    pub repository_id: String,
    pub repository_generation: u64,
    pub freshness: Freshness,
    /// Tokens in source content selected for the response.
    #[serde(default)]
    pub source_tokens: usize,
    /// Tokens in the compact serialized response, excluding this field itself.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub payload_tokens: usize,
    /// Tokenizer used for source and payload accounting.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub tokenizer: String,
    /// Compatibility alias for `source_tokens`.
    pub emitted_tokens: usize,
    /// Whether the configured tokenizer produces exact local counts.
    pub token_count_exact: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

fn is_zero(value: &usize) -> bool {
    *value == 0
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
/// Repository path-discovery operation.
pub enum FileOperation {
    /// Return a compact hierarchy.
    Tree,
    /// Fuzzy-match paths and basenames.
    Find,
    /// Match indexed paths with a glob.
    Glob,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
/// Input for `leantoken.files`.
pub struct FilesRequest {
    /// Discovery operation to perform.
    pub operation: FileOperation,
    /// Optional repository-relative tree root.
    #[serde(default)]
    pub path: Option<String>,
    /// Fuzzy path query used by `find`.
    #[serde(default)]
    pub query: Option<String>,
    /// Glob pattern used by `glob`.
    #[serde(default)]
    pub pattern: Option<String>,
    /// Maximum entries to return.
    #[serde(default)]
    pub max_results: Option<usize>,
    /// Cursor returned by an earlier response from the same generation.
    #[serde(default)]
    pub cursor: Option<String>,
    /// Maximum hierarchy depth below `path` for `tree`.
    #[serde(default)]
    pub depth: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct FileEntry {
    pub path: String,
    pub kind: FileEntryKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub score: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FileEntryKind {
    File,
    Directory,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct FilesResponse {
    pub entries: Vec<FileEntry>,
    pub meta: ResponseMeta,
}

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
    pub score_reasons: Vec<String>,
    pub content_hash: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SearchResponse {
    pub hits: Vec<SearchHit>,
    /// Occurrences returned in this response page after token limits.
    #[serde(default)]
    pub occurrences_returned: usize,
    /// Exact filtered occurrence count when `all_occurrences` is enabled.
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
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
/// Input for `leantoken.outline`.
pub struct OutlineRequest {
    /// Repository-relative files to outline.
    pub paths: Vec<String>,
    /// Keep definitions whose names contain this value.
    #[serde(default)]
    pub symbol_name: Option<String>,
    /// Keep definitions of this exact syntax kind.
    #[serde(default)]
    pub symbol_kind: Option<String>,
    /// Maximum definitions and imports to return.
    #[serde(default)]
    pub max_results: Option<usize>,
    /// Maximum tokens across signatures and import targets.
    #[serde(default)]
    pub max_tokens: Option<usize>,
    /// Opaque cursor returned when `max_results` leaves outline entries unread.
    #[serde(default)]
    pub cursor: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct OutlineFile {
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    /// Whether structural parsing covered the complete indexed file.
    #[serde(default)]
    pub parse_complete: bool,
    /// Compatibility alias for `parse_complete`.
    pub structurally_complete: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub symbols: Vec<Symbol>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub imports: Vec<Import>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct OutlineResponse {
    pub files: Vec<OutlineFile>,
    /// Whether every requested file was parsed completely.
    #[serde(default)]
    pub parse_complete: bool,
    /// Whether this response contains every filtered symbol and import.
    #[serde(default)]
    pub result_complete: bool,
    /// Exact filtered symbol count across all requested files.
    #[serde(default)]
    pub total_symbols: usize,
    /// Symbols returned in this response.
    #[serde(default)]
    pub returned_symbols: usize,
    /// Exact import count across all requested files.
    #[serde(default)]
    pub total_imports: usize,
    /// Imports returned in this response.
    #[serde(default)]
    pub returned_imports: usize,
    /// Whether the result cap left outline entries for another page.
    #[serde(default)]
    pub truncated_by_max_results: bool,
    /// Whether signatures or imports were omitted by the token budget.
    #[serde(default)]
    pub truncated_by_max_tokens: bool,
    /// Exact filtered symbol counts grouped by syntax kind.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub symbol_counts_by_kind: BTreeMap<String, usize>,
    pub meta: ResponseMeta,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Symbol {
    pub name: String,
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,
    pub start_line: usize,
    pub end_line: usize,
    pub start_byte: usize,
    pub end_byte: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Import {
    pub raw_target: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolved_path: Option<String>,
    pub line: usize,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ReferenceRole {
    Definition,
    Reference,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Reference {
    pub name: String,
    pub kind: String,
    pub role: ReferenceRole,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enclosing_symbol: Option<String>,
    pub start_line: usize,
    pub end_line: usize,
    pub start_byte: usize,
    pub end_byte: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
/// Input for `leantoken.read`.
pub struct ReadRequest {
    /// Repository-relative file path.
    pub path: String,
    /// First one-based line; defaults to the start of the file.
    #[serde(default)]
    pub start_line: Option<usize>,
    /// Last one-based line; defaults to the end of the file.
    #[serde(default)]
    pub end_line: Option<usize>,
    /// Indexed symbol to read; cannot be combined with line fields.
    #[serde(default)]
    pub symbol: Option<String>,
    /// Indexed Markdown heading title or outline signature to read.
    #[serde(default)]
    pub heading: Option<String>,
    /// One-based occurrence of a duplicate Markdown heading; defaults to 1.
    #[serde(default)]
    pub heading_occurrence: Option<usize>,
    /// Opaque cursor returned by a truncated read; cannot be combined with a new target.
    #[serde(default)]
    pub continuation_cursor: Option<String>,
    /// Maximum source tokens to return.
    #[serde(default)]
    pub max_tokens: Option<usize>,
    /// Hash from the same prior range; matching content returns `not_modified`.
    #[serde(default)]
    pub expected_hash: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ReadResponse {
    pub path: String,
    pub status: ReadStatus,
    /// First line in the complete resolved target.
    #[serde(default)]
    pub target_start_line: usize,
    /// Last line in the complete resolved target.
    #[serde(default)]
    pub target_end_line: usize,
    /// First line represented by this response page.
    #[serde(default)]
    pub returned_start_line: usize,
    /// Last line represented by this response page.
    #[serde(default)]
    pub returned_end_line: usize,
    /// Compatibility alias for `returned_start_line`.
    pub start_line: usize,
    /// Compatibility alias for `returned_end_line`.
    pub end_line: usize,
    /// Whether source remains after this response page.
    #[serde(default)]
    pub truncated: bool,
    /// First line represented by the next response page.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_start_line: Option<usize>,
    /// Opaque continuation bound to this repository generation and live file content.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub continuation_cursor: Option<String>,
    /// Whether `expected_hash` matched this response page.
    #[serde(default)]
    pub not_modified: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    pub content_hash: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub indexed_hash: Option<String>,
    pub index_stale: bool,
    pub meta: ResponseMeta,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ReadStatus {
    Content,
    /// The response contains only part of the resolved target.
    Truncated,
    NotModified,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
/// Input for `leantoken.context`.
pub struct ContextRequest {
    /// Natural-language coding task used to retrieve evidence.
    pub task: String,
    /// Maximum source tokens across selected fragments.
    pub token_budget: usize,
    /// Require returned source fragments to match at least one path pattern.
    #[serde(default)]
    pub include_paths: Vec<String>,
    /// Require evidence matching each path pattern when indexed and within budget.
    #[serde(default)]
    pub must_include_paths: Vec<String>,
    /// Require evidence for each exact symbol when indexed and within budget.
    #[serde(default)]
    pub must_include_symbols: Vec<String>,
    /// Maximum number of returned fragments.
    #[serde(default)]
    pub max_fragments: Option<usize>,
    /// Boost matching paths without filtering other candidates.
    #[serde(default)]
    pub focus_paths: Vec<String>,
    /// Require every returned fragment to match at least one focus path.
    #[serde(default)]
    pub strict_focus_paths: bool,
    /// Minimum returned fragments required for each focus path pattern.
    #[serde(default)]
    pub minimum_fragments_per_focus_path: Option<usize>,
    /// Boost candidates for these exact symbol names.
    #[serde(default)]
    pub focus_symbols: Vec<String>,
    /// Exclude matching repository paths.
    #[serde(default)]
    pub exclude_paths: Vec<String>,
    /// Fragment hashes already held by the caller and not to resend.
    #[serde(default)]
    pub known_hashes: Vec<String>,
    /// Earlier generation used to boost files indexed since that response.
    #[serde(default)]
    pub prior_repository_generation: Option<u64>,
    /// Base revision for diff-scoped context; resolved against the repository.
    #[serde(default)]
    pub base_revision: Option<String>,
    /// Explicit changed paths for diff-scoped context; bounded and validated.
    #[serde(default)]
    pub changed_paths: Vec<String>,
    /// Require every returned fragment to belong to the resolved changed paths.
    #[serde(default)]
    pub strict_changed_paths: bool,
}

/// Selected coverage for one caller-supplied focus path pattern.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct ContextFocusPathCoverage {
    /// Original focus path pattern.
    pub pattern: String,
    /// Indexed files matched by the pattern.
    pub indexed_paths: usize,
    /// Minimum fragments required by this request.
    pub minimum_fragments: usize,
    /// Returned fragments matched by the pattern.
    pub selected_fragments: usize,
    /// Whether indexed and selected evidence met the requested minimum.
    pub satisfied: bool,
}

/// Selected coverage for a strict resolved changed-path scope.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct ContextChangedPathCoverage {
    /// Resolved changed paths in the hard scope.
    pub resolved_paths: usize,
    /// Resolved changed paths present in the index.
    pub indexed_paths: usize,
    /// Returned fragments belonging to resolved changed paths.
    pub selected_fragments: usize,
    /// Whether the strict scope produced indexed selected evidence.
    pub satisfied: bool,
}

/// Indexed and selected evidence coverage for caller-supplied context constraints.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct ContextCoverageReceipt {
    /// Focus path patterns that matched no indexed path.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unmatched_focus_paths: Vec<String>,
    /// Focus symbols that matched no exact indexed symbol.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unmatched_focus_symbols: Vec<String>,
    /// Per-pattern selection coverage when focus paths are strict or carry a minimum.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub focus_path_coverage: Vec<ContextFocusPathCoverage>,
    /// Coverage of the resolved changed-path boundary when it is strict.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub changed_path_coverage: Option<ContextChangedPathCoverage>,
    /// Whether every requested strict or minimum focus/changed scope was satisfied.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub strict_scope_satisfied: Option<bool>,
    /// Hard include patterns that matched no indexed path.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unmatched_include_paths: Vec<String>,
    /// Required path patterns represented by returned or already-held evidence.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub covered_must_include_paths: Vec<String>,
    /// Required exact symbols represented by returned or already-held evidence.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub covered_must_include_symbols: Vec<String>,
    /// Required path patterns that matched no indexed path.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unmatched_must_include_paths: Vec<String>,
    /// Required exact symbols that matched no indexed symbol.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unmatched_must_include_symbols: Vec<String>,
    /// Indexed required path patterns omitted by path, token, or result constraints.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub uncovered_must_include_paths: Vec<String>,
    /// Indexed required symbols omitted by path, token, or result constraints.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub uncovered_must_include_symbols: Vec<String>,
}

impl ContextCoverageReceipt {
    fn is_empty(&self) -> bool {
        self.unmatched_focus_paths.is_empty()
            && self.unmatched_focus_symbols.is_empty()
            && self.focus_path_coverage.is_empty()
            && self.changed_path_coverage.is_none()
            && self.strict_scope_satisfied.is_none()
            && self.unmatched_include_paths.is_empty()
            && self.covered_must_include_paths.is_empty()
            && self.covered_must_include_symbols.is_empty()
            && self.unmatched_must_include_paths.is_empty()
            && self.unmatched_must_include_symbols.is_empty()
            && self.uncovered_must_include_paths.is_empty()
            && self.uncovered_must_include_symbols.is_empty()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ContextFragment {
    pub path: String,
    pub start_line: usize,
    pub end_line: usize,
    #[serde(
        default = "source_representation",
        skip_serializing_if = "is_source_representation"
    )]
    pub representation: String,
    pub content: String,
    #[serde(default, skip_serializing)]
    pub content_hash: String,
    #[serde(default, skip_serializing)]
    pub score: f64,
    pub reason: String,
    #[serde(default, skip_serializing)]
    pub token_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct EvidenceReceipt {
    /// Internal task identity used by evaluation; the originating request already carries the task.
    #[serde(default, skip_serializing)]
    pub task_fingerprint: String,
    /// Content hashes aligned by index with `ContextResponse.fragments`.
    pub fragment_hashes: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct OmittedCandidate {
    pub path: String,
    pub start_line: usize,
    pub end_line: usize,
    pub reason: String,
}

/// Counts of generated context candidates omitted at caller or budget boundaries.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct ContextOmissionSummary {
    /// Candidates rejected by `include_paths` or `exclude_paths`.
    pub path_excluded: usize,
    /// Candidates suppressed because the caller already holds their content hash.
    pub known_hash: usize,
    /// Ranked candidates that did not fit the token or result limit.
    pub budget_or_result_limit: usize,
}

impl ContextOmissionSummary {
    fn is_empty(&self) -> bool {
        self.path_excluded == 0 && self.known_hash == 0 && self.budget_or_result_limit == 0
    }
}

/// One deterministic path group inferred from an oversized diff scope.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct ContextPathGroup {
    /// Repository-relative directory prefix represented by this group.
    pub prefix: String,
    /// Number of changed paths represented by the prefix.
    pub changed_paths: usize,
}

/// A bounded follow-up context scope that preserves the original request invariants.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct ContextRoutingSuggestion {
    /// Hard path scope recommended for the next `context` request.
    pub include_paths: Vec<String>,
}

/// Breadth and decomposition guidance for a context request spanning many changed paths.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct ContextRoutingReceipt {
    /// Distinct candidate paths considered before ranking.
    pub candidate_paths: usize,
    /// Changed paths represented by the diff scope.
    pub changed_paths: usize,
    /// Distinct paths selected into the bounded response.
    pub selected_paths: usize,
    /// Whether selected evidence is spread across multiple inferred groups.
    pub weakly_concentrated: bool,
    /// Consistency boundary to reuse with every suggested scope.
    pub consistency: IndexConsistency,
    /// Base revision to reuse with every suggested scope.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_revision: Option<String>,
    /// Held hashes to reuse with every suggested scope.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub known_hashes: Vec<String>,
    /// Total deterministic path groups represented by the changed paths.
    pub path_groups_total: usize,
    /// Largest deterministic changed-path groups, in descending size order.
    pub path_groups: Vec<ContextPathGroup>,
    /// Bounded hard-scope suggestions for follow-up context calls.
    pub suggestions: Vec<ContextRoutingSuggestion>,
}

/// Receipt describing the resolved diff scope, if one was supplied.
///
/// When the caller provides a `base_revision` or `changed_paths`, this
/// records the resolved base and head identities, the changed paths used
/// as ranking seeds, and how many of those paths were found in the index.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct DiffScopeReceipt {
    /// Resolved base revision short SHA, or `None` when paths were explicit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_revision: Option<String>,
    /// Resolved head revision short SHA, or `None` for the working tree.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head_revision: Option<String>,
    /// Changed paths used as ranking seeds.
    pub changed_paths: Vec<String>,
    /// Number of changed paths found in the committed index.
    pub indexed_changed_paths: usize,
    /// Bounded symbol and relationship evidence derived from changed paths.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence: Option<DiffEvidenceReceipt>,
}

/// Bounded evidence mapping a diff scope to indexed definitions and neighbors.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct DiffEvidenceReceipt {
    /// Target-side changed line ranges parsed from Git.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub changed_hunks: Vec<DiffHunkEvidence>,
    /// Definitions owned by indexed changed files.
    pub changed_symbols: Vec<DiffSymbolEvidence>,
    /// Direct reference, import, and likely owner-test relationships.
    pub related_paths: Vec<DiffRelatedPath>,
    /// Coverage gaps or truncation reasons; absence never means no relationship.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub gaps: Vec<String>,
}

/// One target-side changed line range.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct DiffHunkEvidence {
    pub path: String,
    pub start_line: usize,
    pub end_line: usize,
}

/// One indexed definition within diff scope.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct DiffSymbolEvidence {
    pub path: String,
    pub name: String,
    pub kind: String,
    pub start_line: usize,
    pub end_line: usize,
}

/// One path related to diff scope by an observed or explicitly labeled heuristic signal.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct DiffRelatedPath {
    pub changed_path: String,
    pub related_path: String,
    /// `reference`, `importer`, or `test_name_match`.
    pub signal: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ContextResponse {
    /// Workflow selected by the context router.
    pub workflow: ContextWorkflow,
    /// Bounded routing evidence for specialized workflows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow_receipt: Option<WorkflowReceipt>,
    pub fragments: Vec<ContextFragment>,
    pub receipt: EvidenceReceipt,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diff_scope: Option<DiffScopeReceipt>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub omitted: Vec<OmittedCandidate>,
    /// Aggregate omission causes, including details truncated from `omitted`.
    #[serde(default, skip_serializing_if = "ContextOmissionSummary::is_empty")]
    pub omission_summary: ContextOmissionSummary,
    /// Coverage of caller-supplied focus, hard-scope, and must-cover constraints.
    #[serde(default, skip_serializing_if = "ContextCoverageReceipt::is_empty")]
    pub coverage: ContextCoverageReceipt,
    /// Decomposition guidance for oversized, multi-area diff scopes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub routing: Option<ContextRoutingReceipt>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
    pub meta: ResponseMeta,
}

/// Evidence-family coverage produced by specialized context routing.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct WorkflowReceipt {
    /// Number of repository-guidance candidates discovered.
    pub guidance_candidates: usize,
    /// Number of issue or pull-request template candidates discovered.
    pub template_candidates: usize,
    /// Number of validation-configuration candidates discovered.
    pub validation_candidates: usize,
    /// Number of changed/focused-path owner-test candidates discovered.
    pub owner_test_candidates: usize,
    /// Evidence families absent from the indexed repository.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub missing_families: Vec<String>,
}

#[derive(Debug, Clone)]
/// Evaluation-only context result with the paths seen before ranking and selection.
///
/// This is not part of the MCP surface. It lets retrieval benchmarks distinguish
/// candidate-generation misses from ranking or token-allocation misses without
/// inflating normal responses with diagnostic metadata.
pub struct ContextEvaluation {
    /// Normal token-bounded context response.
    pub response: ContextResponse,
    /// Sorted unique paths represented by candidates before ranking and selection.
    pub generated_candidate_paths: Vec<String>,
    /// Candidate signal summaries before deduplication and selection.
    pub generated_candidates: Vec<ContextCandidateEvaluation>,
    /// Request-local counts for candidate generation and excerpt hydration.
    pub phases: ContextPhaseCounters,
    /// Evaluation-only wall-time breakdown without behavioral assertions.
    pub timings: ContextPhaseTimings,
    /// Privacy-safe generation-scoped storage primitive identities in call order.
    pub primitive_keys: Vec<RetrievalPrimitiveKey>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
/// Diagnostic-only context wall-time phases in milliseconds.
///
/// Candidate generation includes its nested storage phases, so it should not be
/// added to those fields. Timings are measurements, never correctness gates.
pub struct ContextPhaseTimings {
    /// Complete pinned-snapshot context request.
    pub total_ms: f64,
    /// Candidate generation, including nested lookup and hydration phases.
    pub candidate_generation_ms: f64,
    /// Batched exact-symbol constraint lookups.
    pub exact_symbol_lookup_ms: f64,
    /// General symbol searches.
    pub symbol_search_ms: f64,
    /// Reference searches.
    pub reference_search_ms: f64,
    /// Word or trigram lexical candidate queries.
    pub lexical_search_ms: f64,
    /// Full literal verification and occurrence analysis over lexical candidates.
    pub lexical_verify_ms: f64,
    /// Batched enclosing-symbol lookups.
    pub enclosing_lookup_ms: f64,
    /// Adaptive declaration excerpt hydration.
    pub adaptive_excerpt_ms: f64,
    /// Stored line-window hydration.
    pub stored_excerpt_ms: f64,
    /// Workflow, import-neighbor, and reverse-dependency candidate generation.
    pub workflow_generation_ms: f64,
    /// Candidate scoring, selection, coverage finalization, and response assembly.
    pub ranking_finalize_ms: f64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
/// Deterministic phase and candidate counts for an evaluation-only context run.
pub struct ContextPhaseCounters {
    /// Facet queries retained by the bounded planner.
    pub queries_planned: usize,
    /// Non-test-intent queries used for candidate generation.
    pub queries_executed: usize,
    /// Exact symbol names submitted in one constraint lookup.
    pub exact_symbol_names: usize,
    /// Non-empty bounded exact-symbol storage batches.
    pub exact_symbol_batches: usize,
    /// Exact symbol rows returned for focus and must-cover constraints.
    pub exact_symbol_hits: usize,
    /// Symbol rows returned before path filtering and excerpt hydration.
    pub symbol_candidates: usize,
    /// Reference rows returned before path filtering and excerpt hydration.
    pub reference_candidates: usize,
    /// FTS chunk rows returned before lexical verification.
    pub lexical_candidate_chunks: usize,
    /// FTS chunks verified with the compiled literal matcher.
    pub lexical_chunks_verified: usize,
    /// Verified lexical chunks retained for excerpt generation.
    pub lexical_matches: usize,
    /// Enclosing-symbol locations submitted to storage.
    pub enclosing_location_requests: usize,
    /// Non-empty enclosing-symbol storage batches.
    pub enclosing_location_batches: usize,
    /// Distinct enclosing-symbol locations across the complete request.
    pub unique_enclosing_locations: usize,
    /// Adaptive declaration excerpt requests submitted to storage.
    pub adaptive_excerpt_requests: usize,
    /// Non-empty adaptive excerpt storage batches.
    pub adaptive_excerpt_batches: usize,
    /// Distinct adaptive excerpt requests across the complete request.
    pub unique_adaptive_excerpt_requests: usize,
    /// Stored line-window requests submitted to storage.
    pub stored_excerpt_requests: usize,
    /// Non-empty stored excerpt storage batches.
    pub stored_excerpt_batches: usize,
    /// Distinct stored line-window requests across the complete request.
    pub unique_stored_excerpt_requests: usize,
    /// Candidate representations handed to ranking before selection.
    pub generated_candidates: usize,
}

/// Graph-signal policy used only by frozen context-retrieval evaluations.
///
/// Production adapters do not accept this value. Each variant keeps the same
/// lexical and syntax candidates, then enables at most one additional signal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContextSignalPolicy {
    /// Symbol and full-text candidates without dependency or caller signals.
    LexicalSyntax,
    /// Add concept-corroborated symbols from files imported by seed candidates.
    ImportNeighbor,
    /// Add a ranking boost to existing candidates that import seed files.
    ReverseDependency,
    /// Add parsed reference candidates as high-confidence caller evidence.
    HighConfidenceCaller,
}

#[derive(Debug, Clone)]
/// Evaluation-only summary of a generated context candidate.
pub struct ContextCandidateEvaluation {
    /// Repository-relative candidate path.
    pub path: String,
    /// Inclusive first line of the candidate range.
    pub start_line: usize,
    /// Inclusive last line of the candidate range.
    pub end_line: usize,
    /// Candidate representation selected during generation.
    pub representation: String,
    /// Retrieval signals that produced the candidate.
    pub match_kinds: Vec<String>,
    /// Query concepts matched by the candidate.
    pub concepts: Vec<String>,
    /// Aggregate weight of matched concepts.
    pub concept_weight: f64,
    /// Candidate score before final selection.
    pub score: f64,
    /// Candidate token count used by selection.
    pub token_count: usize,
}

/// Bounded aggregate counts for files skipped during index preparation.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct IndexSkipReasonCounts {
    /// Files detected as binary during preparation.
    pub binary: usize,
    /// Files admitted by discovery that exceeded the byte limit before reading completed.
    pub oversized_during_read: usize,
    /// Files whose preparation failed before searchable content could be produced.
    pub failed: usize,
}

impl IndexSkipReasonCounts {
    /// Return the total number of preparation skips across every public reason.
    #[must_use]
    pub fn total(&self) -> usize {
        self.binary
            .saturating_add(self.oversized_during_read)
            .saturating_add(self.failed)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct IndexResponse {
    pub repository_generation: u64,
    pub files_seen: usize,
    pub files_indexed: usize,
    pub files_unchanged: usize,
    pub files_removed: usize,
    pub files_skipped: usize,
    pub warnings: Vec<String>,
}

/// Additive index details serialized beside the compatible response fields.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct IndexReport {
    /// Source-compatible index response retained for existing Rust consumers.
    #[serde(flatten)]
    pub response: IndexResponse,
    /// Known aggregate preparation skip counts whose sum equals `files_skipped`.
    ///
    /// Legacy deserialized responses omit this field because their reason
    /// breakdown is unknown. Responses produced by this version always include
    /// the fixed-shape object.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skip_reasons: Option<IndexSkipReasonCounts>,
}

impl IndexReport {
    /// Attach a known preparation breakdown to a compatible index response.
    #[must_use]
    pub fn with_skip_reasons(response: IndexResponse, skip_reasons: IndexSkipReasonCounts) -> Self {
        Self {
            response,
            skip_reasons: Some(skip_reasons),
        }
    }

    /// Discard additive details and return the compatible index response.
    #[must_use]
    pub fn into_response(self) -> IndexResponse {
        self.response
    }
}

impl std::ops::Deref for IndexReport {
    type Target = IndexResponse;

    fn deref(&self) -> &Self::Target {
        &self.response
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct StatusResponse {
    pub repository_root: String,
    pub database_path: String,
    pub repository_generation: u64,
    /// Whether a committed generation is available for retrieval.
    pub index_state: IndexState,
    pub freshness: Freshness,
    pub file_count: usize,
    pub chunk_count: usize,
    pub symbol_count: usize,
    pub languages: Vec<LanguageCount>,
    pub warnings: Vec<String>,
}

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

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct LanguageCount {
    pub language: String,
    pub files: usize,
}

fn is_source_representation(value: &String) -> bool {
    value == "source"
}

fn source_representation() -> String {
    "source".to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn index_report_preserves_unknown_legacy_skip_reasons_and_serializes_known_counts() {
        let legacy: IndexReport = serde_json::from_value(serde_json::json!({
            "repository_generation": 1,
            "files_seen": 2,
            "files_indexed": 1,
            "files_unchanged": 0,
            "files_removed": 0,
            "files_skipped": 1,
            "warnings": []
        }))
        .expect("deserialize legacy index report");
        assert_eq!(legacy.skip_reasons, None);
        let legacy_value = serde_json::to_value(&legacy).expect("reserialize legacy report");
        assert!(legacy_value.get("skip_reasons").is_none());

        let skip_reasons = IndexSkipReasonCounts {
            binary: 1,
            oversized_during_read: 2,
            failed: 3,
        };
        let response = IndexResponse {
            repository_generation: 2,
            files_seen: 7,
            files_indexed: 1,
            files_unchanged: 0,
            files_removed: 2,
            files_skipped: skip_reasons.total(),
            warnings: vec!["failed preparation".into()],
        };
        let report = IndexReport::with_skip_reasons(response, skip_reasons);
        let value = serde_json::to_value(report).expect("serialize index report");

        assert_eq!(value["files_skipped"], 6);
        assert_eq!(
            value["skip_reasons"],
            serde_json::json!({
                "binary": 1,
                "oversized_during_read": 2,
                "failed": 3
            })
        );
        let round_trip: IndexReport =
            serde_json::from_value(value).expect("deserialize current index report");
        assert_eq!(
            round_trip.skip_reasons,
            Some(IndexSkipReasonCounts {
                binary: 1,
                oversized_during_read: 2,
                failed: 3,
            })
        );
        assert_eq!(round_trip.files_skipped, 6);
    }

    #[test]
    fn status_response_serializes_readiness_independently_from_freshness() {
        for (repository_generation, index_state, freshness) in [
            (0, IndexState::Uninitialized, Freshness::Current),
            (0, IndexState::Uninitialized, Freshness::Reconciling),
            (4, IndexState::Ready, Freshness::Current),
            (4, IndexState::Ready, Freshness::Reconciling),
        ] {
            let response = StatusResponse {
                repository_root: "/repository".into(),
                database_path: "/cache/index.sqlite".into(),
                repository_generation,
                index_state,
                freshness: freshness.clone(),
                file_count: 0,
                chunk_count: 0,
                symbol_count: 0,
                languages: Vec::new(),
                warnings: Vec::new(),
            };

            let value = serde_json::to_value(response).expect("serialize status");
            assert_eq!(
                value["index_state"],
                match index_state {
                    IndexState::Uninitialized => "uninitialized",
                    IndexState::Ready => "ready",
                }
            );
            assert_eq!(
                value["freshness"],
                match freshness {
                    Freshness::Current => "current",
                    Freshness::Reconciling => "reconciling",
                }
            );
        }
    }

    #[test]
    fn compact_context_response_round_trips_with_defaults() {
        let response = ContextResponse {
            workflow: ContextWorkflow::Implementation,
            workflow_receipt: None,
            fragments: vec![ContextFragment {
                path: "src/lib.rs".into(),
                start_line: 1,
                end_line: 2,
                representation: "source".into(),
                content: "fn answer() {}".into(),
                content_hash: "receipt-hash".into(),
                score: 2.0,
                reason: "symbol".into(),
                token_count: 4,
            }],
            receipt: EvidenceReceipt {
                task_fingerprint: "task".into(),
                fragment_hashes: vec!["receipt-hash".into()],
            },
            diff_scope: None,
            omitted: Vec::new(),
            omission_summary: ContextOmissionSummary::default(),
            coverage: ContextCoverageReceipt::default(),
            routing: None,
            warnings: Vec::new(),
            meta: ResponseMeta {
                repository_id: "repository".into(),
                repository_generation: 7,
                freshness: Freshness::Current,
                source_tokens: 4,
                payload_tokens: 0,
                tokenizer: "cl100k_base".into(),
                emitted_tokens: 4,
                token_count_exact: true,
                next_cursor: None,
            },
        };

        let value = serde_json::to_value(&response).expect("serialize response");
        assert!(value["fragments"][0].get("representation").is_none());
        assert!(value["fragments"][0].get("content_hash").is_none());
        assert!(value["receipt"].get("task_fingerprint").is_none());
        assert_eq!(value["meta"]["freshness"], "current");
        assert_eq!(value["meta"]["source_tokens"], 4);
        assert_eq!(value["meta"]["tokenizer"], "cl100k_base");
        assert_eq!(value["meta"]["token_count_exact"], true);
        assert!(value.get("omitted").is_none());
        assert!(value.get("warnings").is_none());

        let round_trip: ContextResponse =
            serde_json::from_value(value).expect("deserialize compact response");
        assert_eq!(round_trip.fragments[0].representation, "source");
        assert_eq!(round_trip.fragments[0].content_hash, "");
        assert!(round_trip.receipt.task_fingerprint.is_empty());
        assert_eq!(round_trip.meta.freshness, Freshness::Current);
        assert_eq!(round_trip.meta.source_tokens, 4);
        assert_eq!(round_trip.meta.tokenizer, "cl100k_base");
        assert!(round_trip.meta.token_count_exact);

        let mut legacy_value = serde_json::to_value(response).expect("serialize legacy response");
        let legacy_meta = legacy_value["meta"]
            .as_object_mut()
            .expect("response metadata object");
        legacy_meta.remove("source_tokens");
        legacy_meta.remove("payload_tokens");
        legacy_meta.remove("tokenizer");
        let legacy: ContextResponse =
            serde_json::from_value(legacy_value).expect("deserialize legacy response");
        assert_eq!(legacy.meta.source_tokens, 0);
        assert_eq!(legacy.meta.payload_tokens, 0);
        assert!(legacy.meta.tokenizer.is_empty());
    }

    #[test]
    fn compact_context_response_snapshot() {
        let response = ContextResponse {
            workflow: ContextWorkflow::Implementation,
            workflow_receipt: None,
            fragments: vec![ContextFragment {
                path: "src/lib.rs".into(),
                start_line: 4,
                end_line: 6,
                representation: "source".into(),
                content: "pub fn answer() -> u8 { 42 }".into(),
                content_hash: "fragment-hash".into(),
                score: 1.25,
                reason: "symbol; focus".into(),
                token_count: 9,
            }],
            receipt: EvidenceReceipt {
                task_fingerprint: "internal-task-fingerprint".into(),
                fragment_hashes: vec!["fragment-hash".into()],
            },
            diff_scope: None,
            omitted: vec![OmittedCandidate {
                path: "src/other.rs".into(),
                start_line: 10,
                end_line: 12,
                reason: "budget or result limit".into(),
            }],
            omission_summary: ContextOmissionSummary {
                budget_or_result_limit: 1,
                ..ContextOmissionSummary::default()
            },
            coverage: ContextCoverageReceipt::default(),
            routing: None,
            warnings: vec!["1 omitted".into()],
            meta: ResponseMeta {
                repository_id: "repository".into(),
                repository_generation: 7,
                freshness: Freshness::Reconciling,
                source_tokens: 9,
                payload_tokens: 123,
                tokenizer: "cl100k_base".into(),
                emitted_tokens: 9,
                token_count_exact: true,
                next_cursor: None,
            },
        };

        insta::assert_json_snapshot!(response);
    }

    #[test]
    fn compact_empty_outline_round_trips_with_defaults() {
        let file = OutlineFile {
            path: "README.md".into(),
            language: None,
            parse_complete: true,
            structurally_complete: true,
            symbols: Vec::new(),
            imports: Vec::new(),
        };

        let value = serde_json::to_value(&file).expect("serialize outline");
        assert!(value.get("symbols").is_none());
        assert!(value.get("imports").is_none());

        let round_trip: OutlineFile =
            serde_json::from_value(value).expect("deserialize compact outline");
        assert!(round_trip.symbols.is_empty());
        assert!(round_trip.imports.is_empty());
    }
}
