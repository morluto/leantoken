use super::*;

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
/// One path-scoped evidence requirement for `leantoken.context`.
pub struct ContextRequiredEvidence {
    /// Repository-relative path pattern whose evidence must be represented.
    #[schemars(length(min = 1, max = 4096))]
    pub path: String,
    /// Alternative literal queries; distinct matches contribute to the minimum.
    #[schemars(length(min = 1, max = 16), inner(length(min = 1, max = 4096)))]
    pub queries: Vec<String>,
    /// Minimum number of distinct queries that selected evidence must match.
    #[serde(default = "one")]
    #[schemars(range(min = 1, max = 16), default = "one")]
    pub minimum_query_matches: usize,
}

const fn one() -> usize {
    1
}

/// Presentation depth for a context response.
///
/// Profiles never change candidate generation, ranking, selected fragment
/// membership or order, source-token budgets, hard constraints, or receipt
/// suppression.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ContextResponseProfile {
    /// Preserve fail-loud coverage and routing while omitting optional detail.
    Compact,
    /// Preserve the historical default response shape and diagnostics.
    #[default]
    Balanced,
    /// Include the complete bounded omission and diff diagnostics.
    Explain,
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
    /// Require task-relevant evidence for each path-scoped query contract.
    #[serde(default)]
    pub required_evidence: Vec<ContextRequiredEvidence>,
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
    /// Internal presentation flag derived from the selected response profile.
    #[serde(skip)]
    #[schemars(skip)]
    pub explain_diagnostics: bool,
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
    /// Bounded allocation details for plans and explain-profile responses.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diagnostics: Option<ContextFocusPathDiagnostics>,
}

/// Why one generated focus candidate was not selected.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ContextFocusSuppressionBoundary {
    /// Request path policy removed the candidate before ranking.
    PathPolicy,
    /// A caller-provided content hash suppressed the candidate.
    KnownHash,
    /// Overlap or exact-content deduplication retained another candidate.
    Deduplicated,
    /// The source-token budget could not admit the candidate.
    TokenBudget,
    /// Hard required focus minima exceeded the fragment limit.
    MaxFragments,
    /// The per-file diversity cap rejected another region from the same file.
    FileDiversity,
    /// Higher-utility evidence displaced a soft focus candidate.
    GlobalRanking,
}

/// Count at one deterministic focus-candidate suppression boundary.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct ContextFocusSuppression {
    /// Selection boundary that rejected these candidates.
    pub boundary: ContextFocusSuppressionBoundary,
    /// Distinct focus-matching candidate ranges rejected at this boundary.
    pub fragments: usize,
}

/// Primary reason an unsatisfied focus path could not reach its minimum.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ContextFocusCapacityBlocker {
    /// The pattern matched no file in the pinned index generation.
    NoIndexedPaths,
    /// Path policy left no matched file eligible for candidate generation.
    PathPolicy,
    /// Eligible indexed files produced no bounded candidate evidence.
    CandidateGeneration,
    /// The per-pattern file or candidate fan-out bound prevented the minimum.
    CandidateFanoutLimit,
    /// Caller-held hashes suppressed evidence needed for returned coverage.
    KnownHash,
    /// Candidate deduplication retained fewer distinct focus regions than required.
    Deduplicated,
    /// The source-token budget could not admit enough matching evidence.
    TokenBudget,
    /// Required focus minima exceeded the request fragment capacity.
    MaxFragments,
    /// The per-file diversity cap prevented another matching region.
    FileDiversity,
    /// Higher-utility evidence displaced a soft focus candidate.
    GlobalRanking,
}

/// Bounded allocation facts for one focus path pattern.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct ContextFocusPathDiagnostics {
    /// Indexed files remaining after include, exclude, generated-artifact, and
    /// strict changed-path policy.
    pub eligible_paths: usize,
    /// Distinct generated candidate ranges matching this focus pattern.
    pub generated_fragments: usize,
    /// Generated candidates carrying an indexed symbol target.
    pub generated_symbol_fragments: usize,
    /// Selected fragments occupying an enforced per-focus reservation.
    pub reserved_fragments: usize,
    /// Exact source tokens selected for this focus pattern before delivery-time
    /// receipt suppression.
    pub selected_source_tokens: usize,
    /// Bounded non-zero candidate counts grouped by selection boundary.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub suppressed_by: Vec<ContextFocusSuppression>,
    /// Primary reason this pattern did not meet its requested minimum.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capacity_blocker: Option<ContextFocusCapacityBlocker>,
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

/// Selected or planned coverage for one path-scoped evidence requirement.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct ContextRequiredEvidenceCoverage {
    /// Original repository-relative path pattern.
    pub path: String,
    /// Indexed files matched by the path pattern.
    pub indexed_paths: usize,
    /// Bounded indexed files inspected for matching evidence.
    pub inspected_paths: usize,
    /// Minimum number of distinct query matches required.
    pub minimum_query_matches: usize,
    /// Distinct caller queries matched by selected or already-held evidence.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub matched_queries: Vec<String>,
    /// Caller queries not represented by selected or already-held evidence.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unmatched_queries: Vec<String>,
    /// Selected or already-held fragments carrying matching evidence.
    pub selected_fragments: usize,
    /// Whether indexed evidence met the explicit query contract.
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
    /// Whether every requested strict or minimum path scope was satisfied.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path_scope_satisfied: Option<bool>,
    /// Per-contract coverage for explicit path-scoped evidence requirements.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub required_evidence: Vec<ContextRequiredEvidenceCoverage>,
    /// Whether every explicit path-scoped evidence contract was satisfied.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence_scope_satisfied: Option<bool>,
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
            && self.path_scope_satisfied.is_none()
            && self.required_evidence.is_empty()
            && self.evidence_scope_satisfied.is_none()
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
    /// Effective presentation profile after service-option normalization.
    #[serde(default)]
    pub effective_response_profile: ContextResponseProfile,
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
