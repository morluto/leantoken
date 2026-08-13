use super::*;
use crate::config::{DEFAULT_READ_TOKENS, DEFAULT_RESULTS};

pub(in crate::services) const MAX_REGEX_CANDIDATES: usize = 2_000;
/// Maximum files examined during a regex scan before early exit.
pub(in crate::services) const MAX_REGEX_FILES_SCANNED: usize = 10_000;
/// Maximum chunks examined per file during a regex scan.
pub(in crate::services) const MAX_REGEX_CHUNKS_PER_FILE: usize = 256;
/// Maximum trigram rows verified before a planned regex search fails explicitly.
pub(in crate::services) const MAX_REGEX_CANDIDATE_CHUNKS: usize = 10_000;
/// Maximum lightweight FTS rows inspected while applying path-scoped planning.
pub(in crate::services) const MAX_SCOPED_REGEX_ROWS_SCANNED: usize = 100_000;
/// File budget clamped to the pre-existing emergency full-scan ceiling.
pub(in crate::services) const DEFAULT_REGEX_WORK_FILES: usize = MAX_REGEX_FILES_SCANNED;
/// Twice the largest legitimate fallback scan observed by the boundary profile.
pub(in crate::services) const DEFAULT_REGEX_WORK_CHUNKS: usize = 20_510;
/// Twice the largest representative indexed corpus, rounded up below the 2 GiB index ceiling.
pub(in crate::services) const DEFAULT_REGEX_WORK_BYTES: usize = 1024 * 1024 * 1024;
/// Verification-byte floor for sound long-identifier candidate plans.
pub(in crate::services) const LITERAL_IDENTIFIER_WORK_BYTES: usize = 4 * 1024 * 1024;
/// Maximum delay between cooperative cancellation probes during candidate verification.
pub(in crate::services) const REGEX_CANCELLATION_CHECK_INTERVAL: usize = 64;
/// Maximum exact matches materialized by one exhaustive occurrence request.
pub(super) const MAX_EXHAUSTIVE_OCCURRENCES: usize = 100_000;
pub(super) const FILTER_SCAN_PAGE_SIZE: usize = 256;
pub(super) const MAX_FILTER_SCAN_ROWS: usize = 10_000;
pub(super) const REGEX_CANDIDATE_PAGE_SIZE: usize = 512;
pub(super) const MAX_REGEX_PLAN_NODES: usize = 256;
pub(super) const MAX_REGEX_PLAN_TERMS: usize = 32;
pub(super) const MAX_REGEX_PLAN_TERM_BYTES: usize = 256;
pub(super) const MAX_LITERAL_IDENTIFIER_PLAN_BYTES: usize = 4 * 1024;
pub(super) const MAX_REGEX_LITERAL_SEQUENCE: usize = 16;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct RegexWorkLimits {
    pub(super) files: usize,
    pub(super) chunks: usize,
    pub(super) bytes: usize,
}

impl Default for RegexWorkLimits {
    fn default() -> Self {
        Self {
            files: DEFAULT_REGEX_WORK_FILES,
            chunks: DEFAULT_REGEX_WORK_CHUNKS,
            bytes: DEFAULT_REGEX_WORK_BYTES,
        }
    }
}

impl RegexWorkLimits {
    /// Derive bounded scan work from caller-visible result and token budgets.
    /// Each dimension remains below the repository-wide emergency ceilings.
    /// Scan ceilings are independent of page-sized result bounds so cursor
    /// continuation can reach later matches. The configured chunk size is
    /// always affordable so a small output budget cannot reject a valid
    /// candidate before result filtering.
    pub(super) fn for_request(
        max_results: Option<usize>,
        max_tokens: Option<usize>,
        minimum_chunk_bytes: usize,
    ) -> Self {
        // The public schema advertises these values as defaults. Treating an
        // explicit default as a tighter scan ceiling made equivalent requests
        // do different work and could reject a valid late match.
        let max_results = max_results.filter(|value| *value != DEFAULT_RESULTS);
        let max_tokens = max_tokens.filter(|value| *value != DEFAULT_READ_TOKENS);
        let result_work_bytes = max_results.map(|value| value.max(1).saturating_mul(64));
        let token_work_bytes = max_tokens.map(|tokens| tokens.max(1).saturating_mul(64));
        Self {
            files: DEFAULT_REGEX_WORK_FILES,
            chunks: DEFAULT_REGEX_WORK_CHUNKS,
            bytes: match (token_work_bytes, result_work_bytes) {
                (None, None) => DEFAULT_REGEX_WORK_BYTES,
                (token_bytes, result_bytes) => token_bytes
                    .into_iter()
                    .chain(result_bytes)
                    .max()
                    .unwrap_or(0)
                    .max(minimum_chunk_bytes)
                    .clamp(1024, DEFAULT_REGEX_WORK_BYTES),
            },
        }
    }

    pub(super) fn for_literal_identifier_request(
        max_results: Option<usize>,
        max_tokens: Option<usize>,
        minimum_chunk_bytes: usize,
    ) -> Self {
        let mut limits = Self::for_request(max_results, max_tokens, minimum_chunk_bytes);
        limits.bytes = limits
            .bytes
            .clamp(LITERAL_IDENTIFIER_WORK_BYTES, DEFAULT_REGEX_WORK_BYTES);
        limits
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RegexPlanning {
    Enabled,
    Disabled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SearchDiagnostics {
    Omit,
    Collect,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) enum DefinitionPreference {
    #[default]
    Ranked,
    Structural,
}

impl DefinitionPreference {
    pub(super) const fn from_prefer_structural(prefer_structural: bool) -> Self {
        if prefer_structural {
            Self::Structural
        } else {
            Self::Ranked
        }
    }

    pub(super) const fn prefers_structural(self) -> bool {
        matches!(self, Self::Structural)
    }
}

pub(super) enum PreparedQueryReceipt {
    None,
    Record(ExactQueryPredicate),
    Reuse {
        receipt_id: String,
        predicate: ExactQueryPredicate,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ExhaustiveSearchMode {
    Text,
    Regex,
}

pub(super) enum SearchKind {
    Auto(DefinitionPreference),
    Text,
    Regex,
    Identifier(DefinitionPreference),
    Symbol,
    Reference,
    Exhaustive {
        mode: ExhaustiveSearchMode,
        query_receipt: PreparedQueryReceipt,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::services) enum LexicalMatchKind {
    Text,
    Regex,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SearchHitKind {
    Symbol,
    Reference,
    Text,
    Regex,
}

impl SearchHitKind {
    pub(super) const fn label(self) -> &'static str {
        match self {
            Self::Symbol => "symbol",
            Self::Reference => "reference",
            Self::Text => "text",
            Self::Regex => "regex",
        }
    }
}

impl LexicalMatchKind {
    pub(in crate::services) const fn label(self) -> &'static str {
        self.hit_kind().label()
    }

    pub(in crate::services) const fn reason(self) -> &'static str {
        match self {
            Self::Text => "text match",
            Self::Regex => "regex match",
        }
    }

    const fn hit_kind(self) -> SearchHitKind {
        match self {
            Self::Text => SearchHitKind::Text,
            Self::Regex => SearchHitKind::Regex,
        }
    }
}

impl SearchKind {
    pub(super) const fn mode(&self) -> SearchMode {
        match self {
            Self::Auto(_) => SearchMode::Auto,
            Self::Text
            | Self::Exhaustive {
                mode: ExhaustiveSearchMode::Text,
                ..
            } => SearchMode::Text,
            Self::Regex
            | Self::Exhaustive {
                mode: ExhaustiveSearchMode::Regex,
                ..
            } => SearchMode::Regex,
            Self::Identifier(_) => SearchMode::Identifier,
            Self::Symbol => SearchMode::Symbol,
            Self::Reference => SearchMode::Reference,
        }
    }

    pub(super) const fn is_exhaustive(&self) -> bool {
        matches!(self, Self::Exhaustive { .. })
    }

    pub(super) const fn is_exhaustive_text(&self) -> bool {
        matches!(
            self,
            Self::Exhaustive {
                mode: ExhaustiveSearchMode::Text,
                ..
            }
        )
    }

    pub(super) const fn is_regex(&self) -> bool {
        matches!(
            self,
            Self::Regex
                | Self::Exhaustive {
                    mode: ExhaustiveSearchMode::Regex,
                    ..
                }
        )
    }

    pub(super) const fn lexical_match_kind(&self) -> LexicalMatchKind {
        if self.is_regex() {
            LexicalMatchKind::Regex
        } else {
            LexicalMatchKind::Text
        }
    }

    pub(super) const fn definition_preference(&self) -> DefinitionPreference {
        match self {
            Self::Auto(preference) | Self::Identifier(preference) => *preference,
            Self::Text | Self::Regex | Self::Symbol | Self::Reference | Self::Exhaustive { .. } => {
                DefinitionPreference::Ranked
            }
        }
    }

    pub(super) const fn query_receipt(&self) -> Option<&PreparedQueryReceipt> {
        match self {
            Self::Exhaustive { query_receipt, .. } => Some(query_receipt),
            Self::Auto(_)
            | Self::Text
            | Self::Regex
            | Self::Identifier(_)
            | Self::Symbol
            | Self::Reference => None,
        }
    }
}

pub(super) struct SearchInput {
    pub(super) query: String,
    pub(super) kind: SearchKind,
    pub(super) include_paths: Vec<String>,
    pub(super) exclude_paths: Vec<String>,
    pub(super) focus_paths: Vec<String>,
    pub(super) max_results: Option<usize>,
    pub(super) max_tokens: Option<usize>,
    pub(super) case_sensitive: bool,
    pub(super) receipt_id: Option<String>,
    pub(super) cursor: Option<String>,
}

#[derive(serde::Serialize, serde::Deserialize)]
pub(super) struct SearchPosition {
    #[serde(rename = "o")]
    pub(super) offset: usize,
}

impl SearchInput {
    pub(super) fn from_request(request: SearchRequest, kind: SearchKind) -> Self {
        let SearchRequest {
            query,
            mode: _,
            include_paths,
            exclude_paths,
            focus_paths,
            max_results,
            max_tokens,
            context_lines: _,
            case_sensitive,
            all_occurrences: _,
            prefer_structural: _,
            receipt_id,
            query_receipt: _,
            cursor,
        } = request;
        Self {
            query,
            kind,
            include_paths,
            exclude_paths,
            focus_paths,
            max_results,
            max_tokens,
            case_sensitive,
            receipt_id,
            cursor,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SearchOutputShape {
    Full,
    Compact,
    OccurrenceGroups(SearchOccurrenceOutput),
}

#[derive(Debug, Clone, Copy)]
pub(super) struct SearchExecutionOptions {
    pub(super) response_options: ServiceCallOptions,
    pub(super) accounting: SearchAccounting,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SearchAccounting {
    Omit,
    Record,
}

pub(super) enum QueryReceiptExecution {
    None,
    Pending(QueryReceiptRecord),
    Outcome(QueryReceiptOutcome),
}

pub(super) struct RegexScan {
    pub(super) hits: Vec<ChunkHit>,
    pub(super) phases: SearchPhaseCounters,
}

#[derive(Default)]
pub(super) struct RegexWorkBudget {
    limits: RegexWorkLimits,
    candidate_files: usize,
    candidate_chunks: usize,
    candidate_bytes: usize,
}

impl RegexWorkBudget {
    pub(super) fn for_request(
        max_results: Option<usize>,
        max_tokens: Option<usize>,
        minimum_chunk_bytes: usize,
    ) -> Self {
        Self {
            limits: RegexWorkLimits::for_request(max_results, max_tokens, minimum_chunk_bytes),
            ..Self::default()
        }
    }

    pub(super) fn for_literal_identifier_request(
        max_results: Option<usize>,
        max_tokens: Option<usize>,
        minimum_chunk_bytes: usize,
    ) -> Self {
        Self {
            limits: RegexWorkLimits::for_literal_identifier_request(
                max_results,
                max_tokens,
                minimum_chunk_bytes,
            ),
            ..Self::default()
        }
    }

    pub(super) fn charge_file(&mut self, cancellation: &CancellationToken) -> Result<()> {
        check_cancelled(cancellation)?;
        self.candidate_files = self.candidate_files.saturating_add(1);
        self.enforce(
            RegexWorkDimension::CandidateFiles,
            self.candidate_files,
            self.limits.files,
        )
    }

    pub(super) fn charge_chunk(
        &mut self,
        bytes: usize,
        cancellation: &CancellationToken,
    ) -> Result<()> {
        self.candidate_chunks = self.candidate_chunks.saturating_add(1);
        self.candidate_bytes = self.candidate_bytes.saturating_add(bytes);
        self.enforce(
            RegexWorkDimension::CandidateChunks,
            self.candidate_chunks,
            self.limits.chunks,
        )?;
        self.enforce(
            RegexWorkDimension::CandidateBytes,
            self.candidate_bytes,
            self.limits.bytes,
        )?;
        if self
            .candidate_chunks
            .is_multiple_of(REGEX_CANCELLATION_CHECK_INTERVAL)
        {
            check_cancelled(cancellation)?;
        }
        Ok(())
    }

    fn enforce(&self, dimension: RegexWorkDimension, observed: usize, limit: usize) -> Result<()> {
        if observed <= limit {
            return Ok(());
        }
        Err(Error::RegexWorkBudgetExceeded {
            dimension,
            candidate_files: self.candidate_files,
            candidate_chunks: self.candidate_chunks,
            candidate_bytes: self.candidate_bytes,
            limit,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum RegexCandidateExpr {
    Term(String),
    All(Vec<RegexCandidateExpr>),
    Any(Vec<RegexCandidateExpr>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct RegexCandidatePlan {
    pub(super) expression: RegexCandidateExpr,
    pub(super) source: RegexPlanSource,
    pub(super) nodes_visited: usize,
    pub(super) term_count: usize,
    pub(super) term_bytes: usize,
    pub(super) alternative_count: usize,
    pub(super) min_literal_len: usize,
}

pub(super) struct RegexPlanDiagnostics {
    pub(super) fallback_reason: RegexPlanFallbackReason,
    pub(super) nodes_visited: usize,
    pub(super) term_count: usize,
    pub(super) term_bytes: usize,
}

pub(super) enum RegexPlanDecision {
    Planned(RegexCandidatePlan),
    Fallback(RegexPlanDiagnostics),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RegexPlanBudgetExceeded {
    Nodes,
    Terms,
    TermBytes,
}

#[derive(Default)]
pub(super) struct RegexPlanBudget {
    pub(super) nodes: usize,
    pub(super) terms: usize,
    pub(super) term_bytes: usize,
}

impl RegexPlanBudget {
    pub(super) fn add_term(
        &mut self,
        term: &str,
    ) -> std::result::Result<(), RegexPlanBudgetExceeded> {
        self.terms = self.terms.saturating_add(1);
        self.term_bytes = self.term_bytes.saturating_add(term.len());
        if self.terms > MAX_REGEX_PLAN_TERMS {
            return Err(RegexPlanBudgetExceeded::Terms);
        }
        if self.term_bytes > MAX_REGEX_PLAN_TERM_BYTES {
            return Err(RegexPlanBudgetExceeded::TermBytes);
        }
        Ok(())
    }
}

#[cfg(test)]
mod regex_work_budget_tests {
    use super::*;

    #[test]
    fn aggregate_chunk_budget_reports_complete_bounded_counters() {
        let cancellation = CancellationToken::new();
        let mut budget = RegexWorkBudget {
            limits: RegexWorkLimits::default(),
            candidate_files: 7,
            candidate_chunks: DEFAULT_REGEX_WORK_CHUNKS,
            candidate_bytes: 11_000,
        };
        let error = budget
            .charge_chunk(100, &cancellation)
            .expect_err("next chunk exhausts budget");
        assert!(matches!(
            error,
            Error::RegexWorkBudgetExceeded {
                dimension: RegexWorkDimension::CandidateChunks,
                candidate_files: 7,
                candidate_chunks,
                candidate_bytes: 11_100,
                limit: DEFAULT_REGEX_WORK_CHUNKS,
            } if candidate_chunks == DEFAULT_REGEX_WORK_CHUNKS + 1
        ));
    }

    #[test]
    fn cancellation_is_checked_at_the_documented_interval() {
        let cancellation = CancellationToken::new();
        cancellation.cancel();
        let mut budget = RegexWorkBudget {
            limits: RegexWorkLimits::default(),
            candidate_chunks: REGEX_CANCELLATION_CHECK_INTERVAL - 1,
            ..RegexWorkBudget::default()
        };
        assert!(matches!(
            budget.charge_chunk(1, &cancellation),
            Err(Error::Cancelled)
        ));
    }

    #[test]
    fn request_work_limits_are_monotonic_and_globally_bounded() {
        let small = RegexWorkLimits::for_request(Some(1), Some(1), 32 * 1024);
        let large = RegexWorkLimits::for_request(Some(100), Some(32_000), 32 * 1024);
        assert_eq!(small.files, large.files);
        assert_eq!(small.chunks, large.chunks);
        assert!(small.bytes <= large.bytes);
        assert!(large.files <= DEFAULT_REGEX_WORK_FILES);
        assert!(large.chunks <= DEFAULT_REGEX_WORK_CHUNKS);
        assert!(large.bytes <= DEFAULT_REGEX_WORK_BYTES);
    }

    #[test]
    fn omitted_request_dimensions_keep_their_independent_global_ceiling() {
        let token_limited = RegexWorkLimits::for_request(None, Some(1), 32 * 1024);
        assert_eq!(token_limited.files, DEFAULT_REGEX_WORK_FILES);
        assert_eq!(token_limited.chunks, DEFAULT_REGEX_WORK_CHUNKS);
        assert_eq!(token_limited.bytes, 32 * 1024);

        let result_limited = RegexWorkLimits::for_request(Some(1), None, 32 * 1024);
        assert_eq!(result_limited.files, DEFAULT_REGEX_WORK_FILES);
        assert_eq!(result_limited.chunks, DEFAULT_REGEX_WORK_CHUNKS);
        assert_eq!(result_limited.bytes, 32 * 1024);
    }

    #[test]
    fn literal_identifier_work_keeps_a_bounded_verification_floor() {
        let small = RegexWorkLimits::for_literal_identifier_request(Some(1), Some(1), 32 * 1024);
        let large =
            RegexWorkLimits::for_literal_identifier_request(Some(100), Some(128_000), 32 * 1024);
        assert_eq!(small.bytes, LITERAL_IDENTIFIER_WORK_BYTES);
        assert!(small.bytes <= large.bytes);
        assert!(large.bytes <= DEFAULT_REGEX_WORK_BYTES);
    }
}
