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
    /// Query the latest completed index generation without scanning filesystem changes.
    #[default]
    #[serde(alias = "committed")]
    IndexedGeneration,
    /// Reconcile the current working tree before querying the resulting generation.
    #[serde(alias = "working_tree")]
    ReconcileWorkingTree,
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
    /// Tokens in the compact JSON response envelope after values and result items are removed.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub protocol_tokens: usize,
    /// Tokens attributed to paths, metadata values, and repeated result structure.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub path_and_metadata_tokens: usize,
    /// Tokens in the final serialized service response, including accounting fields.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub total_response_tokens: usize,
    /// Compatibility alias for `total_response_tokens`.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub payload_tokens: usize,
    /// Tokenizer used for source and serialized response accounting.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub tokenizer: String,
    /// Compatibility alias for `source_tokens`.
    pub emitted_tokens: usize,
    /// Whether the configured tokenizer produces exact local counts.
    pub token_count_exact: bool,
    /// Opaque server-managed retrieval receipt for suppressing repeated evidence.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub receipt_id: Option<String>,
    /// Evidence omitted because its content hash was already recorded by the receipt.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub receipt_suppressed_exact: usize,
    /// Evidence omitted because its source range overlaps evidence recorded by the receipt.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub receipt_suppressed_overlap: usize,
    /// Returned evidence that is semantically close to evidence recorded by the receipt.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub receipt_near_duplicates: usize,
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

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
/// Path-only `leantoken.files` response for callers that do not need ranking metadata.
pub struct FilesPathsResponse {
    pub paths: Vec<String>,
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
    /// Server-managed receipt whose previously returned evidence should be suppressed.
    #[serde(default)]
    pub receipt_id: Option<String>,
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
/// Signature-only symbol identity with line coordinates.
pub struct OutlineSignature {
    pub name: String,
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,
    pub start_line: usize,
    pub end_line: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
/// One file in a signature-only outline response.
pub struct OutlineSignaturesFile {
    pub path: String,
    /// Hash of the serialized ordered `signatures` array.
    pub content_hash: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    pub parse_complete: bool,
    pub structurally_complete: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub signatures: Vec<OutlineSignature>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
/// Opt-in outline response that omits imports and symbol byte offsets.
pub struct OutlineSignaturesResponse {
    pub files: Vec<OutlineSignaturesFile>,
    pub parse_complete: bool,
    pub result_complete: bool,
    pub total_symbols: usize,
    pub returned_symbols: usize,
    pub truncated_by_max_results: bool,
    pub truncated_by_max_tokens: bool,
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
    /// Record a bounded base and prefer a cheaper unified diff on a changed follow-up.
    #[serde(default)]
    pub delta: bool,
    /// Server-managed receipt whose previously returned evidence should be suppressed.
    #[serde(default)]
    pub receipt_id: Option<String>,
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
    /// Unified diff from the requested base hash to `content_hash`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delta: Option<String>,
    /// Bounded delta decision and accounting metadata.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delta_receipt: Option<ReadDeltaReceipt>,
    pub content_hash: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub indexed_hash: Option<String>,
    pub index_stale: bool,
    pub meta: ResponseMeta,
}

/// Git-backed symbol history operation.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum HistoryOperation {
    /// Read one parsed symbol from a historical file blob.
    ReadSymbol {
        /// Repository-relative source path.
        path: String,
        /// Exact parsed symbol name, optionally qualified as `parent.name`.
        symbol: String,
        /// Git revision containing the source blob.
        revision: String,
    },
    /// Compare one parsed symbol between two revisions.
    DiffSymbol {
        /// Repository-relative source path at both revisions.
        path: String,
        /// Exact parsed symbol name, optionally qualified as `parent.name`.
        symbol: String,
        /// Older Git revision.
        base_revision: String,
        /// Newer Git revision.
        head_revision: String,
    },
    /// List recent commits that touched the symbol's tracked line history.
    SymbolLog {
        /// Repository-relative source path.
        path: String,
        /// Exact parsed symbol name at `revision`, optionally qualified as `parent.name`.
        symbol: String,
        /// Revision from which line history starts; defaults to `HEAD`.
        #[serde(default)]
        revision: Option<String>,
    },
}

/// Input for Git-backed symbol history retrieval.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct HistoryRequest {
    /// Historical operation and its exact target.
    pub operation: HistoryOperation,
    /// Maximum commits returned by `symbol_log`; defaults to 20.
    #[serde(default)]
    pub max_results: Option<usize>,
    /// Maximum source or diff tokens returned; defaults to 8000.
    #[serde(default)]
    pub max_tokens: Option<usize>,
}

/// One parsed symbol read from an immutable Git revision.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct HistoricalSymbol {
    /// Resolved 12-character revision.
    pub revision: String,
    pub path: String,
    pub name: String,
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    /// First line of the complete historical symbol.
    pub target_start_line: usize,
    /// Last line of the complete historical symbol.
    pub target_end_line: usize,
    /// Last line represented by `content`; omitted from serialized metadata when
    /// source is absent.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub returned_end_line: usize,
    /// Whether source remains after `content`.
    pub truncated: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    pub content_hash: String,
}

/// One commit returned by symbol line-history traversal.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct SymbolHistoryCommit {
    /// Full commit object ID.
    pub commit: String,
    /// Commit author date in strict ISO 8601 format.
    pub authored_at: String,
    /// Commit subject.
    pub subject: String,
}

/// Git-backed symbol history response.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct HistoryResponse {
    /// Resolved operation kind.
    pub kind: String,
    /// Historical symbol for `read_symbol`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub symbol: Option<HistoricalSymbol>,
    /// Base-side symbol for `diff_symbol`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before: Option<HistoricalSymbol>,
    /// Head-side symbol for `diff_symbol`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after: Option<HistoricalSymbol>,
    /// Unified symbol diff for `diff_symbol`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diff: Option<String>,
    /// Whether the unified diff was truncated by `max_tokens`.
    #[serde(default)]
    pub diff_truncated: bool,
    /// Deterministic classification for `diff_symbol`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub semantic_change: Option<DiffSymbolChange>,
    /// Recent commits for `symbol_log`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub commits: Vec<SymbolHistoryCommit>,
    /// Whether all matching commits fit `max_results`.
    #[serde(default)]
    pub result_complete: bool,
    pub meta: ResponseMeta,
}

/// One exact symbol pairing for a bounded multi-symbol revision diff.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct DiffSymbolsTarget {
    /// Repository-relative path at the base revision.
    pub path: String,
    /// Exact base-side symbol name, optionally qualified as `parent.name`.
    pub symbol: String,
    /// Different repository-relative head path for an explicit rename or move.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head_path: Option<String>,
    /// Different exact head-side symbol name for an explicit rename.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head_symbol: Option<String>,
}

/// Input for one bounded, batched multi-symbol revision diff.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct DiffSymbolsRequest {
    /// Ordered exact symbol pairings; the order is preserved across pages.
    pub targets: Vec<DiffSymbolsTarget>,
    /// Older Git revision.
    pub base_revision: String,
    /// Newer Git revision.
    pub head_revision: String,
    /// Maximum symbol results returned on one page; defaults to 20.
    #[serde(default)]
    pub max_results: Option<usize>,
    /// Maximum tokens shared by all unified diffs on one page; defaults to 8000.
    #[serde(default)]
    pub max_tokens: Option<usize>,
    /// Opaque cursor returned by an earlier page of this exact immutable request.
    #[serde(default)]
    pub cursor: Option<String>,
}

/// Shared immutable commit metadata for one multi-symbol diff endpoint.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct HistoryRevisionMetadata {
    /// Resolved 12-character commit identity.
    pub revision: String,
    /// Commit author date in strict ISO 8601 format.
    pub authored_at: String,
    /// Commit subject.
    pub subject: String,
}

/// Per-target result state for a multi-symbol revision diff.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DiffSymbolsStatus {
    Unchanged,
    Added,
    Removed,
    Renamed,
    Modified,
    NotFound,
    Unavailable,
}

/// Why one returned multi-symbol diff is incomplete.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DiffSymbolsIncompleteReason {
    MaxTokens,
    MaxDiffBytes,
    MaxResponseTokens,
}

/// One result from a bounded multi-symbol revision diff.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct DiffSymbolsResult {
    /// Zero-based position in the complete ordered request.
    pub request_index: usize,
    /// Normalized requested symbol pairing.
    pub target: DiffSymbolsTarget,
    pub status: DiffSymbolsStatus,
    /// Base-side parsed symbol when present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before: Option<HistoricalSymbol>,
    /// Head-side parsed symbol when present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after: Option<HistoricalSymbol>,
    /// Unified diff for changed, added, removed, or renamed symbols.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diff: Option<String>,
    /// Whether the unified diff was truncated by a declared request bound.
    #[serde(default)]
    pub diff_truncated: bool,
    /// Deterministic semantic classification when a change was resolved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub semantic_change: Option<DiffSymbolChange>,
    /// Stable reason for an unavailable target or truncated diff.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Typed truncation reason when the result remains otherwise usable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub incomplete_reason: Option<DiffSymbolsIncompleteReason>,
}

/// Directly observed work counters for one bounded multi-symbol diff page.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct DiffSymbolsDiagnostics {
    /// Git subprocesses executed for identity, commit metadata, and batched blobs.
    pub git_subprocesses: usize,
    pub base_paths_requested: usize,
    pub head_paths_requested: usize,
    pub base_blob_bytes: usize,
    pub head_blob_bytes: usize,
    /// Parsed definitions retained across both endpoints.
    pub parsed_symbols: usize,
    /// Unified diff bytes present in the response after all fitting.
    pub retained_diff_bytes: usize,
}

/// Bounded multi-symbol revision diff with shared endpoint metadata.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct DiffSymbolsResponse {
    pub kind: String,
    pub base: HistoryRevisionMetadata,
    pub head: HistoryRevisionMetadata,
    pub results: Vec<DiffSymbolsResult>,
    /// Whether all requested targets and their diff text were returned completely.
    pub result_complete: bool,
    pub diagnostics: DiffSymbolsDiagnostics,
    pub meta: ResponseMeta,
}

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

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ReadStatus {
    Content,
    /// The response contains only part of the resolved target.
    Truncated,
    NotModified,
    /// A complete unified diff is returned instead of full current content.
    Delta,
    /// A server-managed evidence receipt already contained the exact current content.
    ReceiptSuppressed,
}

/// Result selected by an opt-in read delta request.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ReadDeltaOutcome {
    /// Full current content was returned.
    Full,
    /// A complete unified diff was returned.
    Delta,
    /// The requested base hash already identifies the current content.
    NotModified,
    /// A general evidence receipt already contained the exact current content.
    ReceiptSuppressed,
}

/// Why an opt-in read delta attempt returned full content.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ReadDeltaFallback {
    /// No bounded base matched the target and requested hash.
    BaseUnavailable,
    /// The resolved target or returned coordinates changed.
    TargetChanged,
    /// The current response is truncated and cannot be a complete delta target.
    CurrentTruncated,
    /// The current page exceeds the per-entry delta-state bound.
    ContentTooLarge,
    /// The complete delta response was not smaller than a full-content response.
    DeltaNotSmaller,
}

/// Provenance and token accounting for one opt-in read delta decision.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ReadDeltaReceipt {
    /// Stable hash of the repository and caller-selected target.
    pub target_key: String,
    /// Requested prior content hash, when supplied.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_hash: Option<String>,
    /// Hash of the complete current response page.
    pub head_hash: String,
    /// Repository generation observed when the bounded base was captured.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_generation: Option<u64>,
    /// Repository generation used to resolve the current target.
    pub head_generation: u64,
    /// Selected response form.
    pub outcome: ReadDeltaOutcome,
    /// Tokens required by full current content.
    pub full_tokens: usize,
    /// Tokens in the returned delta, or zero for `not_modified`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delta_tokens: Option<usize>,
    /// Full-content tokens avoided by the selected response.
    pub avoided_tokens: usize,
    /// Explicit reason full content was retained after a delta attempt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fallback_reason: Option<ReadDeltaFallback>,
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
    /// Return a bounded ranked query plan without source fragments.
    #[serde(default)]
    pub plan_only: bool,
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
    /// Server-managed receipt whose previously returned evidence should be suppressed.
    #[serde(default)]
    pub receipt_id: Option<String>,
    /// Earlier generation used to boost files indexed since that response.
    #[serde(default)]
    pub prior_repository_generation: Option<u64>,
    /// Base revision or `BASE..HEAD` range for diff-scoped context.
    #[serde(default)]
    pub base_revision: Option<String>,
    /// Explicit changed paths for diff-scoped context; bounded and validated.
    #[serde(default)]
    pub changed_paths: Vec<String>,
    /// Require every returned fragment to belong to the resolved changed paths.
    #[serde(default)]
    pub strict_changed_paths: bool,
    /// Include full omission facets instead of compact aggregate diagnostics.
    #[serde(default, skip_serializing_if = "is_false")]
    pub verbose_diagnostics: bool,
}

/// Optional host-supplied state carried into a compact context handoff manifest.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct HandoffManifestRequest {
    /// Compact task summary; the context task is used when omitted.
    #[serde(default)]
    #[schemars(length(min = 1, max = 512))]
    pub summary: Option<String>,
    /// Commands and checks reported by the caller.
    #[serde(default)]
    #[schemars(length(max = 16))]
    pub validations: Vec<HandoffValidation>,
    /// Assumptions the next executor must preserve or verify.
    #[serde(default)]
    #[schemars(length(max = 16), inner(length(min = 1, max = 512)))]
    pub assumptions: Vec<String>,
    /// Questions that remain unresolved at handoff time.
    #[serde(default)]
    #[schemars(length(max = 16), inner(length(min = 1, max = 512)))]
    pub open_questions: Vec<String>,
    /// Searches or checks that produced no supporting evidence.
    #[serde(default)]
    #[schemars(length(max = 16), inner(length(min = 1, max = 512)))]
    pub negative_evidence: Vec<String>,
    /// Explicit constraints describing approaches or paths to avoid.
    #[serde(default)]
    #[schemars(length(max = 16), inner(length(min = 1, max = 512)))]
    pub avoid_rules: Vec<String>,
}

/// Caller-reported validation retained in a handoff manifest.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct HandoffValidation {
    /// Exact command or check identifier.
    #[schemars(length(min = 1, max = 1024))]
    pub command: String,
    /// Caller-reported outcome; LeanToken does not execute this command.
    pub status: HandoffValidationStatus,
    /// Optional compact result detail.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(length(min = 1, max = 512))]
    pub summary: Option<String>,
}

/// Caller-reported outcome of one handoff validation.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum HandoffValidationStatus {
    /// The caller reports that the check passed.
    Passed,
    /// The caller reports that the check failed.
    Failed,
}

/// Git working-tree state observed while a handoff manifest was assembled.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum HandoffWorkingTreeState {
    /// Git reported no changed or untracked paths.
    Clean,
    /// Git reported at least one changed or untracked path.
    Dirty,
    /// Git working-tree state could not be determined.
    Unknown,
}

/// Source coordinate and content identity retained without copying source text.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct HandoffEvidence {
    /// Repository-relative source path.
    pub path: String,
    /// Inclusive one-based start line.
    pub start_line: usize,
    /// Inclusive one-based end line.
    pub end_line: usize,
    /// BLAKE3 identity of the selected source fragment.
    pub content_hash: String,
}

/// Compact, provenance-bearing state for a host-triggered executor handoff.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct HandoffManifest {
    /// Manifest contract version.
    pub schema_version: u32,
    /// Compact task summary.
    pub summary: String,
    /// Stable fingerprint of the complete context task.
    pub task_fingerprint: String,
    /// Stable opaque identity of the canonical repository root.
    pub repository_id: String,
    /// Atomic repository generation used to select evidence.
    pub repository_generation: u64,
    /// Index freshness observed for the context response.
    pub freshness: Freshness,
    /// Resolved Git commit at the handoff boundary, when available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit_revision: Option<String>,
    /// Working-tree state observed while the response was assembled.
    pub working_tree_state: HandoffWorkingTreeState,
    /// Resolved diff base, when the context request supplied one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_revision: Option<String>,
    /// Resolved diff head, when the context request used an immutable range.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head_revision: Option<String>,
    /// Same-process receipt that can suppress already-returned evidence.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub receipt_id: Option<String>,
    /// Selected evidence coordinates and hashes before receipt suppression.
    pub evidence: Vec<HandoffEvidence>,
    /// Fragment hashes the requesting host already holds.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub held_fragment_hashes: Vec<String>,
    /// Caller-supplied focus path patterns.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub focus_paths: Vec<String>,
    /// Caller-supplied focus symbols.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub focus_symbols: Vec<String>,
    /// Resolved or explicitly supplied changed paths.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub changed_paths: Vec<String>,
    /// Paths related by bounded diff evidence.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub related_paths: Vec<String>,
    /// Likely owner-test paths derived from bounded diff evidence.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub test_paths: Vec<String>,
    /// Commands and checks reported by the caller.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub validations: Vec<HandoffValidation>,
    /// Caller-supplied assumptions.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub assumptions: Vec<String>,
    /// Caller-supplied unresolved questions.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub open_questions: Vec<String>,
    /// Caller-supplied negative evidence.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub negative_evidence: Vec<String>,
    /// Caller-supplied constraints on approaches to avoid.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub avoid_rules: Vec<String>,
    /// Explicit provenance, coverage, or bounded-output limitations.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub gaps: Vec<String>,
}

/// Selected or planned coverage for one caller-supplied focus path pattern.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct ContextFocusPathCoverage {
    /// Original focus path pattern.
    pub pattern: String,
    /// Indexed files matched by the pattern.
    pub indexed_paths: usize,
    /// Minimum fragments required by this request.
    pub minimum_fragments: usize,
    /// Returned or planned fragments matched by the pattern.
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

/// Indexed and selected or planned evidence coverage for context constraints.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct ContextCoverageReceipt {
    /// Focus path patterns that matched no indexed path.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unmatched_focus_paths: Vec<String>,
    /// Focus symbols that matched no exact indexed symbol.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unmatched_focus_symbols: Vec<String>,
    /// Per-pattern selection or plan coverage; ordinary focus paths require one fragment.
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
    /// Required path patterns represented by returned, planned, or already-held evidence.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub covered_must_include_paths: Vec<String>,
    /// Required exact symbols completely represented by returned, planned, or already-held evidence.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub covered_must_include_symbols: Vec<String>,
    /// Required exact symbols represented only by explicitly truncated evidence.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub partial_must_include_symbols: Vec<String>,
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
            && self.partial_must_include_symbols.is_empty()
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
    /// First line of the complete required symbol, when this is required-symbol evidence.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_start_line: Option<usize>,
    /// Last line of the complete required symbol, when this is required-symbol evidence.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_end_line: Option<usize>,
    /// Whether this fragment contains only part of its required symbol target.
    #[serde(default, skip_serializing_if = "is_false")]
    pub truncated: bool,
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

/// One ranked source candidate in a metadata-only context query plan.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct ContextPlanCandidate {
    /// Repository-relative candidate path.
    pub path: String,
    /// First one-based source line that materialization would return.
    pub start_line: usize,
    /// Last one-based source line that materialization would return.
    pub end_line: usize,
    /// First line of the complete required symbol, when this is required-symbol evidence.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_start_line: Option<usize>,
    /// Last line of the complete required symbol, when this is required-symbol evidence.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_end_line: Option<usize>,
    /// Whether materialization would contain only part of its required symbol target.
    #[serde(default, skip_serializing_if = "is_false")]
    pub truncated: bool,
    /// Source representation selected by the retrieval pipeline.
    pub representation: String,
    /// Deterministic final ranking score.
    pub score: f64,
    /// Bounded human-readable ranking signals.
    pub reasons: Vec<String>,
    /// Exact source-token estimate for this candidate.
    pub estimated_tokens: usize,
}

/// Planned evidence coverage for one caller-supplied focus path.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct ContextPlanFocusCoverage {
    /// Original focus path pattern.
    pub pattern: String,
    /// Planned candidate fragments matched by the pattern.
    pub candidate_fragments: usize,
    /// Minimum candidate fragments requested for the pattern.
    pub minimum_fragments: usize,
    /// Whether the planned candidates meet the requested minimum.
    pub satisfied: bool,
}

/// Bounded metadata-only preview of context source materialization.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct ContextQueryPlan {
    /// Ranked candidates that the same materialized request would select.
    pub candidates: Vec<ContextPlanCandidate>,
    /// Distinct eligible candidate paths considered before source selection.
    pub candidate_paths_total: usize,
    /// Exact source tokens the planned candidates would materialize.
    pub estimated_source_tokens: usize,
    /// Planned coverage for each requested focus path pattern.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub focus_coverage: Vec<ContextPlanFocusCoverage>,
    /// Whether generated-artifact defaults matched any generated candidate.
    pub generated_artifact_warning: bool,
    /// Whether every eligible candidate fit the token and fragment limits.
    pub result_complete: bool,
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
    #[serde(default, skip_serializing_if = "is_zero")]
    pub path_excluded: usize,
    /// Candidates suppressed because the caller already holds their content hash.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub known_hash: usize,
    /// Ranked candidates that did not fit the token or result limit.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub budget_or_result_limit: usize,
    /// Highest-frequency omitted paths, bounded with an `[other]` bucket.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub by_path: Vec<ContextOmissionFacet>,
    /// Omitted candidates grouped by language or file extension.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub by_language_or_file_type: Vec<ContextOmissionFacet>,
    /// Omitted candidates grouped by the boundary that rejected them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub by_reason: Vec<ContextOmissionFacet>,
    /// Omitted candidates grouped by deterministic final-score ranges.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub by_score_band: Vec<ContextOmissionFacet>,
    /// Omitted candidates matching at least one requested focus path.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub focused: usize,
    /// Omitted candidates outside every requested focus path.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub not_focused: usize,
    /// Omitted candidates belonging to an explicitly resolved changed path.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub changed: usize,
    /// Omitted candidates outside the explicitly resolved changed paths.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub not_changed: usize,
}

impl ContextOmissionSummary {
    fn is_empty(&self) -> bool {
        self.path_excluded == 0 && self.known_hash == 0 && self.budget_or_result_limit == 0
    }
}

/// One value and count in a bounded context omission breakdown.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct ContextOmissionFacet {
    /// Stable facet value such as a path, file type, reason, or score range.
    pub value: String,
    /// Number of omitted candidates represented by this value.
    pub count: usize,
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
    /// Deterministic revision-to-revision change classification for review scopes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub semantic_change: Option<DiffSemanticChangeReceipt>,
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

/// Deterministic semantic classification for one bounded immutable diff.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct DiffSemanticChangeReceipt {
    /// Added, removed, renamed, or modified parsed definitions.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub symbol_changes: Vec<DiffSymbolChange>,
    /// Changed key paths in recognized JSON configuration files.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub configuration_changes: Vec<DiffConfigurationChange>,
    /// Owner-test discovery status for each bounded changed path.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub owner_tests: Vec<DiffOwnerTestCoverage>,
    /// Explicit reasons why semantic coverage is incomplete.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub gaps: Vec<String>,
}

/// How one parsed definition changed across two revisions.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DiffSymbolChangeKind {
    /// The definition exists only at the head revision.
    Added,
    /// The definition exists only at the base revision.
    Removed,
    /// A unique body fingerprint links differently named definitions.
    Renamed,
    /// The same definition exists at both revisions with changed content.
    Modified,
}

/// Which part of a matched definition changed.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DiffSymbolModification {
    /// The parser-provided signature changed.
    SignatureChanged,
    /// Content changed while the normalized parser signature stayed equal.
    BodyOnly,
}

/// One deterministic parsed-definition change.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct DiffSymbolChange {
    /// Change classification.
    pub kind: DiffSymbolChangeKind,
    /// Base-side definition when present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before: Option<DiffSymbolEvidence>,
    /// Head-side definition when present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after: Option<DiffSymbolEvidence>,
    /// Signature or body-only detail for matched definitions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub modification: Option<DiffSymbolModification>,
    /// Whether this change deterministically alters an explicitly public contract.
    pub public_contract_changed: bool,
}

/// How one JSON configuration key path changed.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DiffConfigurationChangeKind {
    /// The key path exists only at the head revision.
    Added,
    /// The key path exists only at the base revision.
    Removed,
    /// The key path exists at both revisions with a different value fingerprint.
    Modified,
}

/// One configuration key-path change without configuration values.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct DiffConfigurationChange {
    /// Repository-relative configuration file.
    pub path: String,
    /// RFC 6901 JSON Pointer identifying the changed key.
    pub key_path: String,
    /// Key-path change classification.
    pub kind: DiffConfigurationChangeKind,
}

/// Owner-test discovery status for one changed path.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DiffOwnerTestStatus {
    /// At least one likely owner test was found.
    Found,
    /// A complete bounded scan found no likely owner test.
    Missing,
    /// A truncated scan found no likely owner test.
    Unknown,
}

/// Bounded likely owner-test coverage for one changed path.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct DiffOwnerTestCoverage {
    /// Repository-relative changed path.
    pub changed_path: String,
    /// Whether likely owner tests were found.
    pub status: DiffOwnerTestStatus,
    /// Bounded likely owner-test paths.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub paths: Vec<String>,
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
    /// Metadata-only selection preview when `plan_only` was requested.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan: Option<ContextQueryPlan>,
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
    /// Opt-in compact state for a host-triggered executor handoff.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handoff_manifest: Option<HandoffManifest>,
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
    /// Index-content compatibility version used by this binary.
    #[serde(default)]
    pub index_content_version: u32,
    pub repository_generation: u64,
    /// Whether a committed generation is available for retrieval.
    pub index_state: IndexState,
    /// Whether this read-only status request checked the live working tree.
    #[serde(default)]
    pub working_tree_checked: bool,
    pub freshness: Freshness,
    pub file_count: usize,
    pub chunk_count: usize,
    pub symbol_count: usize,
    /// Bytes occupied by the SQLite main, WAL, and shared-memory files.
    pub index_storage_bytes: u64,
    /// Sum of complete source bytes represented by indexed files.
    pub indexed_source_bytes: u64,
    /// Index storage divided by indexed source bytes, when source is non-empty.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub index_amplification_ratio: Option<f64>,
    /// Resident memory for the current LeanToken process when the platform exposes it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub process_rss_bytes: Option<u64>,
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
}

impl TokenAccountingOperation {
    pub(crate) const ALL: [Self; 8] = [
        Self::Files,
        Self::Search,
        Self::Outline,
        Self::Read,
        Self::ContextPlan,
        Self::Context,
        Self::Json,
        Self::History,
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

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct LanguageCount {
    pub language: String,
    pub files: usize,
}

fn is_source_representation(value: &String) -> bool {
    value == "source"
}

fn is_false(value: &bool) -> bool {
    !value
}

fn source_representation() -> String {
    "source".to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn consistency_names_are_explicit_and_legacy_inputs_remain_readable() {
        assert_eq!(
            serde_json::to_string(&IndexConsistency::IndexedGeneration)
                .expect("serialize indexed generation"),
            "\"indexed_generation\""
        );
        assert_eq!(
            serde_json::to_string(&IndexConsistency::ReconcileWorkingTree)
                .expect("serialize working-tree reconciliation"),
            "\"reconcile_working_tree\""
        );
        assert_eq!(
            serde_json::from_str::<IndexConsistency>("\"committed\"")
                .expect("legacy committed alias"),
            IndexConsistency::IndexedGeneration
        );
        assert_eq!(
            serde_json::from_str::<IndexConsistency>("\"working_tree\"")
                .expect("legacy working-tree alias"),
            IndexConsistency::ReconcileWorkingTree
        );
    }

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
                index_content_version: 12,
                repository_generation,
                index_state,
                working_tree_checked: false,
                freshness: freshness.clone(),
                file_count: 0,
                chunk_count: 0,
                symbol_count: 0,
                index_storage_bytes: 0,
                indexed_source_bytes: 0,
                index_amplification_ratio: None,
                process_rss_bytes: None,
                languages: Vec::new(),
                warnings: Vec::new(),
            };

            let value = serde_json::to_value(response).expect("serialize status");
            assert_eq!(value["index_content_version"], 12);
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
            assert_eq!(value["working_tree_checked"], false);
        }
    }

    #[test]
    fn compact_context_response_round_trips_with_defaults() {
        let response = ContextResponse {
            workflow: ContextWorkflow::Implementation,
            workflow_receipt: None,
            plan: None,
            fragments: vec![ContextFragment {
                path: "src/lib.rs".into(),
                start_line: 1,
                end_line: 2,
                target_start_line: None,
                target_end_line: None,
                truncated: false,
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
            handoff_manifest: None,
            warnings: Vec::new(),
            meta: ResponseMeta {
                repository_id: "repository".into(),
                repository_generation: 7,
                freshness: Freshness::Current,
                source_tokens: 4,
                protocol_tokens: 0,
                path_and_metadata_tokens: 0,
                total_response_tokens: 0,
                payload_tokens: 0,
                tokenizer: "cl100k_base".into(),
                emitted_tokens: 4,
                token_count_exact: true,
                receipt_id: None,
                receipt_suppressed_exact: 0,
                receipt_suppressed_overlap: 0,
                receipt_near_duplicates: 0,
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
        legacy_meta.remove("protocol_tokens");
        legacy_meta.remove("path_and_metadata_tokens");
        legacy_meta.remove("total_response_tokens");
        legacy_meta.remove("payload_tokens");
        legacy_meta.remove("tokenizer");
        let legacy: ContextResponse =
            serde_json::from_value(legacy_value).expect("deserialize legacy response");
        assert_eq!(legacy.meta.source_tokens, 0);
        assert_eq!(legacy.meta.protocol_tokens, 0);
        assert_eq!(legacy.meta.path_and_metadata_tokens, 0);
        assert_eq!(legacy.meta.total_response_tokens, 0);
        assert_eq!(legacy.meta.payload_tokens, 0);
        assert!(legacy.meta.tokenizer.is_empty());
    }

    #[test]
    fn compact_context_response_snapshot() {
        let response = ContextResponse {
            workflow: ContextWorkflow::Implementation,
            workflow_receipt: None,
            plan: None,
            fragments: vec![ContextFragment {
                path: "src/lib.rs".into(),
                start_line: 4,
                end_line: 6,
                target_start_line: None,
                target_end_line: None,
                truncated: false,
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
            handoff_manifest: None,
            warnings: vec!["1 omitted".into()],
            meta: ResponseMeta {
                repository_id: "repository".into(),
                repository_generation: 7,
                freshness: Freshness::Reconciling,
                source_tokens: 9,
                protocol_tokens: 17,
                path_and_metadata_tokens: 97,
                total_response_tokens: 123,
                payload_tokens: 123,
                tokenizer: "cl100k_base".into(),
                emitted_tokens: 9,
                token_count_exact: true,
                receipt_id: None,
                receipt_suppressed_exact: 0,
                receipt_suppressed_overlap: 0,
                receipt_near_duplicates: 0,
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
