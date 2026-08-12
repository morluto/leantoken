//! Task-shaped context candidate assembly and ranking handoff.

pub(super) use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    sync::LazyLock,
    time::Instant,
};

pub(super) use globset::{GlobBuilder, GlobSet, GlobSetBuilder};
pub(super) use tokio_util::sync::CancellationToken;

mod api;
mod candidates {
    use super::*;

    mod constraints;
    mod focus;
    mod graph;
    mod lexical;
    mod workflow;
}
mod constants;
mod constraint_types;
mod diagnostics;
mod diff_evidence;
mod execution;
mod facets;
mod finalize;
mod pipeline;
mod response;
mod scope;
mod scope_helpers;
mod validation;

pub(crate) use constants::MAX_CONTEXT_FOCUS_CANDIDATES_PER_PATTERN;
use constants::*;
pub(crate) use constants::{
    MAX_CONTEXT_HITS_PER_SOURCE, MAX_CONTEXT_LEXICAL_HITS, MAX_CONTEXT_QUERIES,
};
use constraint_types::*;
use diagnostics::*;
use diff_evidence::{DiffEvidenceInput, DiffEvidenceMode};
use pipeline::*;
use scope_helpers::*;
use validation::*;

pub(super) use super::change_receipt::{classify_revision_changes, owner_test_coverage};
pub(super) use super::execution_options::RetrievalExecution;
pub(super) use super::handoff::{self, HandoffProvenance};
pub(super) use super::index_read::{
    ChunkHit, FileRecord, RepositoryGeneration, SymbolHit, SymbolRecord,
};
pub(super) use super::read::{AdaptiveExcerptRequest, StoredExcerpt, StoredExcerptRequest};
pub(super) use super::search::{
    LexicalMatchKind, LiteralFullScan, OccurrenceMetadata, chunk_search_hit_for_range,
    compile_literal_regex, fts_quote,
};
pub(super) use super::validation::{
    MAX_INPUT_ITEMS, MAX_PATH_BYTES, MAX_PATTERN_BYTES, MAX_QUERY_BYTES, PathFilter, PathMatcher,
    check_cancelled, validate_glob_patterns, validate_input,
};
pub(super) use super::{ServiceCallOptions, Services, retrieval_primitive_key};
pub(super) use crate::model::*;
pub(super) use crate::ranking::{self, Candidate, CandidateTargetRange};
pub(super) use crate::repository::{
    GitWorkingTreeStatus, git_branch_name, git_diff_hunks_scoped, git_diff_identity,
    git_diff_paths, git_diff_paths_between, git_head_revision, git_working_tree_status,
    normalize_relative, validate_relative,
};
pub(super) use crate::text::{byte_to_line, expand_terms, identifier_words, line_starts};
pub(super) use crate::tokens::ResponseBudget;
pub(super) use crate::{Error, Result};
use facets::{ContextQuery, FacetKind};
pub use pipeline::ContextWorkflowOptions;

#[cfg(test)]
mod tests;
