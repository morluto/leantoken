//! Task-shaped context candidate assembly and ranking handoff.

use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    sync::LazyLock,
    time::Instant,
};

use globset::{GlobBuilder, GlobSet, GlobSetBuilder};
use tokio_util::sync::CancellationToken;

mod facets;
mod response;

use super::change_receipt::{classify_revision_changes, owner_test_coverage};
use super::handoff::{self, HandoffProvenance};
use super::read::{AdaptiveExcerptRequest, StoredExcerpt, StoredExcerptRequest};
use super::search::{chunk_search_hit_for_range, compile_literal_regex, fts_quote};
use super::validation::{
    MAX_INPUT_ITEMS, MAX_PATH_BYTES, MAX_PATTERN_BYTES, MAX_QUERY_BYTES, PathFilter, PathMatcher,
    check_cancelled, validate_glob_patterns, validate_input,
};
use super::{ServiceCallOptions, Services, retrieval_primitive_key};
use crate::model::*;
use crate::ranking::{self, Candidate};
use crate::repository::{
    git_diff_hunks_scoped, git_diff_identity, git_diff_paths, git_diff_paths_between,
    git_head_revision, git_working_tree_status, normalize_relative, validate_relative,
};
use crate::storage::ChunkHit;
use crate::storage::{FileRecord, ReadSession, SymbolHit, SymbolRecord};
use crate::text::{byte_to_line, expand_terms, identifier_words, line_starts};
use crate::tokens::ResponseBudget;
use crate::{Error, Result};
use facets::{ContextQuery, FacetKind};

// Keep each pipeline owner in a separate source file while sharing the private
// context implementation namespace. This avoids widening internal visibility
// solely to satisfy Rust module boundaries.
include!("context/constants.rs");
include!("context/constraint_types.rs");
include!("context/scope_helpers.rs");
include!("context/diagnostics.rs");
include!("context/pipeline.rs");
include!("context/validation.rs");
include!("context/candidates/constraints.rs");
include!("context/candidates/focus.rs");
include!("context/candidates/graph.rs");
include!("context/scope.rs");
include!("context/api.rs");
include!("context/candidates/lexical.rs");
include!("context/candidates/workflow.rs");
include!("context/diff_evidence.rs");
include!("context/finalize.rs");
include!("context/execution.rs");

#[cfg(test)]
mod tests;
