//! Pure, deterministic ranking and selection of source evidence.
//!
//! This module contains no DB access, network calls, async runtime, agent
//! orchestration, or MCP protocol code.  All ranking, deduplication, and
//! budget-aware selection are deterministic functions of the inputs.
//!
//! Public API:
//!
//! * [`crate::ranking::Candidate`] – internal source fragment with ranking signals.
//! * [`crate::ranking::ScoredCandidate`] – a [`crate::ranking::Candidate`] combined with its token count,
//!   content hash, score, and score-per-token diagnostic.
//! * [`crate::ranking::Weights`] – tunable linear weights for each ranking signal.
//! * [`crate::ranking::rank`] – score and sort candidates.
//! * [`crate::ranking::deduplicate`] – remove content-identical and strongly overlapping
//!   candidates, keeping the higher-scored copy.
//! * [`crate::ranking::select`] / [`crate::ranking::select_with_weights_and_tokenizer`] – turn a candidate set and a
//!   [`ContextRequest`] into a [`ContextResponse`], including fragments,
//!   evidence receipt, and omitted candidates.

use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::path::Path;

use crate::config::{DEFAULT_CONTEXT_FRAGMENTS, default_context_exclude_paths};
use crate::model::{
    ContextCoverageReceipt, ContextFocusCapacityBlocker, ContextFocusPathCoverage,
    ContextFocusPathDiagnostics, ContextFocusSuppression, ContextFocusSuppressionBoundary,
    ContextFragment, ContextOmissionFacet, ContextOmissionSummary, ContextPlanCandidate,
    ContextPlanFocusCoverage, ContextQueryPlan, ContextRequest, ContextRequiredEvidenceCoverage,
    ContextResponse, ContextResponseProfile, EvidenceReceipt, Freshness, OmittedCandidate,
    ResponseMeta,
};
use crate::services::validation::{PathMatcher, path_matches};
use crate::tokens;

mod candidate;
mod coverage;
mod dedup;
mod focus_diagnostics;
mod greedy;
mod metadata;
mod omissions;
mod requirements;
mod response;
mod scored;
mod selection;

pub use candidate::{Candidate, Weights};
use coverage::*;
pub use dedup::deduplicate;
pub(crate) use dedup::deduplicate_with_options;
use focus_diagnostics::*;
use greedy::*;
pub(crate) use metadata::required_evidence_marker;
use metadata::*;
use omissions::*;
use requirements::*;
use response::*;
use scored::rank_with_tokenizer;
pub use scored::{ScoredCandidate, rank};
pub(crate) use selection::select_with_tokenizer_and_context_exclusions;
pub use selection::{
    select, select_with_tokenizer, select_with_weights, select_with_weights_and_tokenizer,
};

fn bounded_count_f64(value: usize) -> f64 {
    // `usize` is already the bounded count type used by the tokenizer and
    // selection budget. Converting directly preserves monotonic ordering for
    // counts above `u32::MAX`; saturating through `u32` made every larger
    // fragment indistinguishable to the ranker.
    value as f64
}

#[cfg(test)]
mod tests;
