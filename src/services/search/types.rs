use super::*;

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

#[derive(Default)]
pub(super) struct RegexWorkBudget {
    candidate_files: usize,
    candidate_chunks: usize,
    candidate_bytes: usize,
}

impl RegexWorkBudget {
    pub(super) fn charge_file(&mut self, cancellation: &CancellationToken) -> Result<()> {
        check_cancelled(cancellation)?;
        self.candidate_files = self.candidate_files.saturating_add(1);
        self.enforce(
            RegexWorkDimension::CandidateFiles,
            self.candidate_files,
            DEFAULT_REGEX_WORK_FILES,
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
            DEFAULT_REGEX_WORK_CHUNKS,
        )?;
        self.enforce(
            RegexWorkDimension::CandidateBytes,
            self.candidate_bytes,
            DEFAULT_REGEX_WORK_BYTES,
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
            candidate_chunks: REGEX_CANCELLATION_CHECK_INTERVAL - 1,
            ..RegexWorkBudget::default()
        };
        assert!(matches!(
            budget.charge_chunk(1, &cancellation),
            Err(Error::Cancelled)
        ));
    }
}
