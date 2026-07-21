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

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ResponseMeta {
    pub repository_generation: u64,
    pub freshness: Freshness,
    pub emitted_tokens: usize,
    pub token_count_exact: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
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
/// Input for `leantoken_files`.
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
/// Input for `leantoken_search`.
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
    /// Cursor returned by an earlier response from the same generation.
    #[serde(default)]
    pub cursor: Option<String>,
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
    pub score: f64,
    pub score_reasons: Vec<String>,
    pub content_hash: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SearchResponse {
    pub hits: Vec<SearchHit>,
    pub meta: ResponseMeta,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
/// Input for `leantoken_outline`.
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
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct OutlineFile {
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    pub structurally_complete: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub symbols: Vec<Symbol>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub imports: Vec<Import>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct OutlineResponse {
    pub files: Vec<OutlineFile>,
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
/// Input for `leantoken_read`.
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
    pub start_line: usize,
    pub end_line: usize,
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
    NotModified,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
/// Input for `leantoken_context`.
pub struct ContextRequest {
    /// Natural-language coding task used to retrieve evidence.
    pub task: String,
    /// Maximum source tokens across selected fragments.
    pub token_budget: usize,
    /// Boost matching paths without filtering other candidates.
    #[serde(default)]
    pub focus_paths: Vec<String>,
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

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ContextResponse {
    pub fragments: Vec<ContextFragment>,
    pub receipt: EvidenceReceipt,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub omitted: Vec<OmittedCandidate>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
    pub meta: ResponseMeta,
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

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct StatusResponse {
    pub repository_root: String,
    pub database_path: String,
    pub repository_generation: u64,
    pub freshness: Freshness,
    pub file_count: usize,
    pub chunk_count: usize,
    pub symbol_count: usize,
    pub languages: Vec<LanguageCount>,
    pub warnings: Vec<String>,
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
    fn compact_context_response_round_trips_with_defaults() {
        let response = ContextResponse {
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
            omitted: Vec::new(),
            warnings: Vec::new(),
            meta: ResponseMeta {
                repository_generation: 7,
                freshness: Freshness::Current,
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
        assert_eq!(value["meta"]["token_count_exact"], true);
        assert!(value.get("omitted").is_none());
        assert!(value.get("warnings").is_none());

        let round_trip: ContextResponse =
            serde_json::from_value(value).expect("deserialize compact response");
        assert_eq!(round_trip.fragments[0].representation, "source");
        assert_eq!(round_trip.fragments[0].content_hash, "");
        assert!(round_trip.receipt.task_fingerprint.is_empty());
        assert_eq!(round_trip.meta.freshness, Freshness::Current);
        assert!(round_trip.meta.token_count_exact);
    }

    #[test]
    fn compact_context_response_snapshot() {
        let response = ContextResponse {
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
            omitted: vec![OmittedCandidate {
                path: "src/other.rs".into(),
                start_line: 10,
                end_line: 12,
                reason: "budget or result limit".into(),
            }],
            warnings: vec!["1 omitted".into()],
            meta: ResponseMeta {
                repository_generation: 7,
                freshness: Freshness::Reconciling,
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
