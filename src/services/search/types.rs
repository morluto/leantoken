pub(super) const MAX_REGEX_CANDIDATES: usize = 2_000;
/// Maximum files examined during a regex scan before early exit.
pub(super) const MAX_REGEX_FILES_SCANNED: usize = 10_000;
/// Maximum chunks examined per file during a regex scan.
pub(super) const MAX_REGEX_CHUNKS_PER_FILE: usize = 256;
/// Maximum trigram rows verified before a planned regex search fails explicitly.
pub(super) const MAX_REGEX_CANDIDATE_CHUNKS: usize = 10_000;
/// Maximum lightweight FTS rows inspected while applying path-scoped planning.
pub(super) const MAX_SCOPED_REGEX_ROWS_SCANNED: usize = 100_000;
/// Maximum exact matches materialized by one exhaustive occurrence request.
const MAX_EXHAUSTIVE_OCCURRENCES: usize = 100_000;
const FILTER_SCAN_PAGE_SIZE: usize = 256;
const MAX_FILTER_SCAN_ROWS: usize = 10_000;
const REGEX_CANDIDATE_PAGE_SIZE: usize = 512;
const MAX_REGEX_PLAN_NODES: usize = 256;
const MAX_REGEX_PLAN_TERMS: usize = 32;
const MAX_REGEX_PLAN_TERM_BYTES: usize = 256;
const MAX_REGEX_LITERAL_SEQUENCE: usize = 16;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RegexPlanning {
    Enabled,
    Disabled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SearchDiagnostics {
    Omit,
    Collect,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SearchOutputShape {
    Full,
    OccurrenceGroups { coordinates_only: bool },
}

#[derive(Debug, Clone, Copy)]
struct SearchExecutionOptions {
    output_shape: SearchOutputShape,
    response_options: ServiceCallOptions,
    record_savings: bool,
}

enum QueryReceiptExecution {
    None,
    Pending(QueryReceiptRecord),
    Outcome(QueryReceiptOutcome),
}

struct RegexScan {
    hits: Vec<ChunkHit>,
    phases: SearchPhaseCounters,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum RegexCandidateExpr {
    Term(String),
    All(Vec<RegexCandidateExpr>),
    Any(Vec<RegexCandidateExpr>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RegexCandidatePlan {
    expression: RegexCandidateExpr,
    source: RegexPlanSource,
    nodes_visited: usize,
    term_count: usize,
    term_bytes: usize,
    alternative_count: usize,
    min_literal_len: usize,
}

struct RegexPlanDiagnostics {
    fallback_reason: RegexPlanFallbackReason,
    nodes_visited: usize,
    term_count: usize,
    term_bytes: usize,
}

enum RegexPlanDecision {
    Planned(RegexCandidatePlan),
    Fallback(RegexPlanDiagnostics),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RegexPlanBudgetExceeded {
    Nodes,
    Terms,
    TermBytes,
}

#[derive(Default)]
struct RegexPlanBudget {
    nodes: usize,
    terms: usize,
    term_bytes: usize,
}

impl RegexPlanBudget {
    fn add_term(&mut self, term: &str) -> std::result::Result<(), RegexPlanBudgetExceeded> {
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
