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

// Ranking stages share private signal and ordering helpers, but each
// stage has a distinct physical owner to keep the pipeline navigable.
include!("ranking/metadata.rs");
include!("ranking/omissions.rs");
include!("ranking/candidate.rs");
include!("ranking/scored.rs");
include!("ranking/dedup.rs");
include!("ranking/focus_diagnostics.rs");
include!("ranking/selection.rs");
include!("ranking/requirements.rs");
include!("ranking/greedy.rs");
include!("ranking/coverage.rs");
include!("ranking/response.rs");

#[cfg(test)]
mod tests;
