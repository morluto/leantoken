pub(in crate::services) const MAX_REGEX_CANDIDATES: usize = 2_000;
/// Maximum files examined during a regex scan before early exit.
pub(in crate::services) const MAX_REGEX_FILES_SCANNED: usize = 10_000;
/// Maximum chunks examined per file during a regex scan.
pub(in crate::services) const MAX_REGEX_CHUNKS_PER_FILE: usize = 256;
/// Maximum trigram rows verified before a planned regex search fails explicitly.
pub(in crate::services) const MAX_REGEX_CANDIDATE_CHUNKS: usize = 10_000;
/// Maximum lightweight FTS rows inspected while applying path-scoped planning.
pub(in crate::services) const MAX_SCOPED_REGEX_ROWS_SCANNED: usize = 100_000;
/// Maximum exact matches materialized by one exhaustive occurrence request.
pub(super) const MAX_EXHAUSTIVE_OCCURRENCES: usize = 100_000;
pub(super) const FILTER_SCAN_PAGE_SIZE: usize = 256;
pub(super) const MAX_FILTER_SCAN_ROWS: usize = 10_000;
pub(super) const REGEX_CANDIDATE_PAGE_SIZE: usize = 512;
pub(super) const MAX_REGEX_PLAN_NODES: usize = 256;
pub(super) const MAX_REGEX_PLAN_TERMS: usize = 32;
pub(super) const MAX_REGEX_PLAN_TERM_BYTES: usize = 256;
pub(super) const MAX_REGEX_LITERAL_SEQUENCE: usize = 16;

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SearchOutputShape {
    Full,
    OccurrenceGroups { coordinates_only: bool },
}

#[derive(Debug, Clone, Copy)]
pub(super) struct SearchExecutionOptions {
    pub(super) output_shape: SearchOutputShape,
    pub(super) response_options: ServiceCallOptions,
    pub(super) record_savings: bool,
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
use super::*;
