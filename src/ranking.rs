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
    ContextCoverageReceipt, ContextFragment, ContextOmissionFacet, ContextOmissionSummary,
    ContextPlanCandidate, ContextPlanFocusCoverage, ContextQueryPlan, ContextRequest,
    ContextResponse, EvidenceReceipt, Freshness, OmittedCandidate, ResponseMeta,
};
use crate::services::validation::{PathMatcher, path_matches};
use crate::tokens;

const FACET_PREFIX: &str = "facet:";
const CHANNEL_PREFIX: &str = "channel:";

/// Overlap ratio above which two candidates in the same file are considered
/// duplicates.  Measured against the smaller candidate's line count.
const OVERLAP_THRESHOLD: f64 = 0.5;

/// Divisor for the per-file diversity cap. A 1,200-token context may include
/// two non-overlapping regions from one file, while tiny budgets still prefer
/// breadth.
const DIVERSITY_DIVISOR: usize = 600;
const MAX_OMITTED_DETAILS: usize = 1;
const MAX_OMISSION_FACETS: usize = 12;
const MIN_RELATIVE_CONTEXT_SCORE: f64 = 0.25;

fn increment_facet(counts: &mut HashMap<String, usize>, value: impl Into<String>) {
    let count = counts.entry(value.into()).or_default();
    *count = count.saturating_add(1);
}

fn bounded_facets(counts: HashMap<String, usize>) -> Vec<ContextOmissionFacet> {
    let mut facets = counts
        .into_iter()
        .map(|(value, count)| ContextOmissionFacet { value, count })
        .collect::<Vec<_>>();
    facets.sort_by(|left, right| {
        right
            .count
            .cmp(&left.count)
            .then_with(|| left.value.cmp(&right.value))
    });
    if facets.len() > MAX_OMISSION_FACETS {
        let other = facets
            .drain(MAX_OMISSION_FACETS - 1..)
            .map(|facet| facet.count)
            .sum();
        facets.push(ContextOmissionFacet {
            value: "[other]".into(),
            count: other,
        });
    }
    facets
}

fn candidate_file_type(path: &str) -> String {
    Path::new(path)
        .extension()
        .and_then(|extension| extension.to_str())
        .filter(|extension| !extension.is_empty())
        .map_or_else(
            || "[no extension]".into(),
            |extension| format!(".{}", extension.to_ascii_lowercase()),
        )
}

fn score_band(score: f64) -> &'static str {
    if score >= 1.0 {
        "score >= 1.0"
    } else if score >= 0.5 {
        "0.5 <= score < 1.0"
    } else if score > 0.0 {
        "0 < score < 0.5"
    } else {
        "score = 0"
    }
}

fn summarize_omissions(
    path_omitted: &[ScoredCandidate],
    known_omitted: &[ScoredCandidate],
    limit_omitted: &[ScoredCandidate],
    prefiltered_path_omissions: &[String],
    focus_paths: &PathMatcher,
    changed_paths: &HashSet<&str>,
) -> ContextOmissionSummary {
    let mut paths = HashMap::new();
    let mut file_types = HashMap::new();
    let mut score_bands = HashMap::new();
    let mut focused = 0usize;
    let mut changed = 0usize;

    let mut record = |path: &str, score: Option<f64>| {
        increment_facet(&mut paths, path);
        increment_facet(&mut file_types, candidate_file_type(path));
        increment_facet(&mut score_bands, score.map_or("not scored", score_band));
        focused = focused.saturating_add(usize::from(focus_paths.is_match(path)));
        changed = changed.saturating_add(usize::from(changed_paths.contains(path)));
    };
    for candidate in path_omitted
        .iter()
        .chain(known_omitted)
        .chain(limit_omitted)
    {
        record(&candidate.candidate.path, Some(candidate.score));
    }
    for path in prefiltered_path_omissions {
        record(path, None);
    }

    let path_excluded = path_omitted
        .len()
        .saturating_add(prefiltered_path_omissions.len());
    let known_hash = known_omitted.len();
    let budget_or_result_limit = limit_omitted.len();
    let total = path_excluded
        .saturating_add(known_hash)
        .saturating_add(budget_or_result_limit);
    let mut reasons = HashMap::new();
    if path_excluded > 0 {
        reasons.insert("path_excluded".into(), path_excluded);
    }
    if known_hash > 0 {
        reasons.insert("known_hash".into(), known_hash);
    }
    if budget_or_result_limit > 0 {
        reasons.insert("budget_or_result_limit".into(), budget_or_result_limit);
    }

    ContextOmissionSummary {
        path_excluded,
        known_hash,
        budget_or_result_limit,
        by_path: bounded_facets(paths),
        by_language_or_file_type: bounded_facets(file_types),
        by_reason: bounded_facets(reasons),
        by_score_band: bounded_facets(score_bands),
        focused,
        not_focused: total.saturating_sub(focused),
        changed,
        not_changed: total.saturating_sub(changed),
    }
}

/// Linear scoring weights for ranking signals.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Weights {
    pub exact: f64,
    pub symbol: f64,
    pub reference: f64,
    pub bm25: f64,
    pub path: f64,
    pub lexical_frequency_penalty: f64,
    pub size: f64,
    pub focus: f64,
    pub import: f64,
    pub change: f64,
}

impl Default for Weights {
    fn default() -> Self {
        Self {
            exact: 1.0,
            symbol: 0.8,
            reference: 0.5,
            bm25: 0.4,
            path: 0.25,
            lexical_frequency_penalty: 0.25,
            size: 0.15,
            focus: 0.35,
            import: 0.25,
            change: 0.2,
        }
    }
}

/// Internal candidate carrying every signal used by the ranker.
#[derive(Debug, Clone)]
#[must_use]
pub struct Candidate {
    pub path: String,
    pub start_line: usize,
    pub end_line: usize,
    pub content: String,
    pub match_kinds: Vec<String>,
    pub concepts: Vec<String>,
    pub concept_weight: f64,
    pub representation: String,
    pub symbol_name: Option<String>,
    pub exact: f64,
    pub symbol: f64,
    pub reference: f64,
    pub bm25: f64,
    pub path_score: f64,
    pub lexical_frequency_penalty: f64,
    pub size_score: f64,
    pub focus_boost: f64,
    pub import_boost: f64,
    pub change_boost: f64,
}

impl Candidate {
    /// Create a candidate with all signals initialized to zero and a default
    /// `representation` of `"source"`.
    pub fn new(
        path: impl Into<String>,
        start_line: usize,
        end_line: usize,
        content: impl Into<String>,
    ) -> Self {
        Self {
            path: path.into(),
            start_line,
            end_line,
            content: content.into(),
            match_kinds: Vec::new(),
            concepts: Vec::new(),
            concept_weight: 0.0,
            representation: "source".into(),
            symbol_name: None,
            exact: 0.0,
            symbol: 0.0,
            reference: 0.0,
            bm25: 0.0,
            path_score: 0.0,
            lexical_frequency_penalty: 0.0,
            size_score: 0.0,
            focus_boost: 0.0,
            import_boost: 0.0,
            change_boost: 0.0,
        }
    }

    pub fn match_kind(mut self, kind: impl Into<String>) -> Self {
        self.match_kinds.push(kind.into());
        self
    }

    pub(crate) fn facet(mut self, kind: &str, fusion_key: &str) -> Self {
        self.push_metadata(format!("{FACET_PREFIX}{kind}:{fusion_key}"));
        self
    }

    pub(crate) fn channel(mut self, channel: &str, rank: usize) -> Self {
        self.push_metadata(format!("{CHANNEL_PREFIX}{channel}:{rank}"));
        self
    }

    fn push_metadata(&mut self, value: String) {
        if !self.match_kinds.contains(&value) {
            self.match_kinds.push(value);
        }
    }

    /// Associate this evidence with an independently extracted task concept.
    pub fn concept(mut self, concept: impl Into<String>, weight: f64) -> Self {
        let concept = concept.into();
        if !concept.is_empty() && !self.concepts.contains(&concept) {
            self.concepts.push(concept);
        }
        self.concept_weight = self.concept_weight.max(weight.clamp(0.0, 2.0));
        self
    }

    pub fn representation(mut self, representation: impl Into<String>) -> Self {
        self.representation = representation.into();
        self
    }

    pub fn symbol_name(mut self, name: impl Into<String>) -> Self {
        self.symbol_name = Some(name.into());
        self
    }

    pub fn exact(mut self, value: f64) -> Self {
        self.exact = value;
        self
    }

    pub fn symbol(mut self, value: f64) -> Self {
        self.symbol = value;
        self
    }

    pub fn reference(mut self, value: f64) -> Self {
        self.reference = value;
        self
    }

    pub fn bm25(mut self, value: f64) -> Self {
        self.bm25 = value;
        self
    }

    pub fn path_score(mut self, value: f64) -> Self {
        self.path_score = value;
        self
    }

    pub fn lexical_frequency_penalty(mut self, value: f64) -> Self {
        self.lexical_frequency_penalty = value;
        self
    }

    pub fn size_score(mut self, value: f64) -> Self {
        self.size_score = value;
        self
    }

    pub fn focus_boost(mut self, value: f64) -> Self {
        self.focus_boost = value;
        self
    }

    pub fn import_boost(mut self, value: f64) -> Self {
        self.import_boost = value;
        self
    }

    pub fn change_boost(mut self, value: f64) -> Self {
        self.change_boost = value;
        self
    }

    /// BLAKE3 fingerprint of the candidate content.
    #[must_use]
    pub fn content_hash(&self) -> String {
        crate::text::hash(&self.content)
    }

    /// Exact token count using LeanToken's default tokenizer.
    #[must_use]
    pub fn token_count(&self) -> usize {
        tokens::count(&self.content)
    }

    /// Count this candidate with an explicit tokenizer.
    #[must_use]
    pub fn token_count_with(&self, tokenizer: tokens::Tokenizer) -> usize {
        tokenizer.count(&self.content)
    }

    /// Number of lines covered by the candidate range.
    #[must_use]
    pub fn line_count(&self) -> usize {
        if self.end_line >= self.start_line {
            self.end_line - self.start_line + 1
        } else {
            0
        }
    }

    /// Combined ranking score using the supplied weights and pre-computed
    /// token count.  Deterministic and side-effect free.
    #[must_use]
    #[allow(clippy::cast_precision_loss)]
    pub fn score(&self, weights: &Weights, token_count: usize) -> f64 {
        // BM25 is normalized so a raw score of 0 maps to 0 and very large raw
        // scores saturate near 1.
        let bm25_norm = self.bm25 / (1.0 + self.bm25);

        // If an explicit size score was supplied, use it; otherwise larger
        // fragments receive a small penalty.
        let size = if self.size_score == 0.0 {
            1.0 / (1.0 + token_count as f64 / 150.0)
        } else {
            self.size_score
        };

        let base = self.exact * weights.exact
            + self.symbol * weights.symbol
            + self.reference * weights.reference
            + bm25_norm * weights.bm25
            + self.path_score * weights.path
            + size * weights.size;

        // God-file penalty: files that mention a term everywhere are down-weighted.
        let penalty = self.lexical_frequency_penalty * weights.lexical_frequency_penalty;

        // Focus/import/change boosts are additive.
        let boost = self.focus_boost * weights.focus
            + self.import_boost * weights.import
            + self.change_boost * weights.change;

        (base + boost - penalty).max(0.0)
    }

    /// Short human-readable reason for why the candidate was selected.
    #[must_use]
    pub fn reason(&self) -> String {
        let mut parts: Vec<&str> = self
            .match_kinds
            .iter()
            .map(String::as_str)
            .filter(|kind| !is_internal_metadata(kind))
            .collect();
        if self.focus_boost > 0.0 && !parts.contains(&"focus") {
            parts.push("focus");
        }
        if self.import_boost > 0.0 && !parts.contains(&"import") {
            parts.push("import");
        }
        if self.change_boost > 0.0 && !parts.contains(&"changed") {
            parts.push("changed");
        }
        if parts.is_empty() {
            "context".to_string()
        } else {
            parts.join("; ")
        }
    }
}

fn is_internal_metadata(kind: &str) -> bool {
    kind.starts_with(FACET_PREFIX) || kind.starts_with(CHANNEL_PREFIX)
}

/// A candidate with a fully resolved score, token count, content hash, and
/// score-per-token diagnostic.
#[derive(Debug, Clone)]
#[must_use]
pub struct ScoredCandidate {
    pub candidate: Candidate,
    pub score: f64,
    pub token_count: usize,
    pub content_hash: String,
    pub marginal_score: f64,
}

impl ScoredCandidate {
    #[allow(clippy::cast_precision_loss)]
    pub fn new(candidate: Candidate, weights: &Weights) -> Self {
        Self::new_with_tokenizer(candidate, weights, tokens::Tokenizer::default())
    }

    #[allow(clippy::cast_precision_loss)]
    fn new_with_tokenizer(
        candidate: Candidate,
        weights: &Weights,
        tokenizer: tokens::Tokenizer,
    ) -> Self {
        let token_count = candidate.token_count_with(tokenizer).max(1);
        let content_hash = candidate.content_hash();
        let score = candidate.score(weights, token_count);
        let marginal_score = score / token_count as f64;
        Self {
            candidate,
            score,
            token_count,
            content_hash,
            marginal_score,
        }
    }
}

/// Score all candidates and sort by descending combined score.  Ties are
/// broken by path and then starting line for deterministic ordering.
#[must_use]
pub fn rank(candidates: Vec<Candidate>, weights: &Weights) -> Vec<ScoredCandidate> {
    rank_with_tokenizer(candidates, weights, tokens::Tokenizer::default())
}

fn rank_with_tokenizer(
    candidates: Vec<Candidate>,
    weights: &Weights,
    tokenizer: tokens::Tokenizer,
) -> Vec<ScoredCandidate> {
    let mut scored: Vec<ScoredCandidate> = candidates
        .into_iter()
        .map(|candidate| ScoredCandidate::new_with_tokenizer(candidate, weights, tokenizer))
        .collect();

    scored.sort_by(|a, b| {
        let ord = b.score.total_cmp(&a.score);
        if ord != Ordering::Equal {
            return ord;
        }
        let ord = a.candidate.path.cmp(&b.candidate.path);
        if ord != Ordering::Equal {
            return ord;
        }
        a.candidate.start_line.cmp(&b.candidate.start_line)
    });

    scored
}

/// Remove content-identical candidates and candidates whose line ranges
/// overlap the same file by at least the module's overlap threshold. The higher-scored
/// copy is kept.
#[must_use]
#[allow(clippy::cast_precision_loss)]
pub fn deduplicate(candidates: Vec<ScoredCandidate>) -> Vec<ScoredCandidate> {
    deduplicate_with_options(candidates, &Weights::default())
}

#[allow(clippy::cast_precision_loss)]
fn deduplicate_with_options(
    candidates: Vec<ScoredCandidate>,
    weights: &Weights,
) -> Vec<ScoredCandidate> {
    let mut sorted = candidates;
    sorted.sort_by(|a, b| {
        let ord = b.candidate.exact.total_cmp(&a.candidate.exact);
        if ord != Ordering::Equal {
            return ord;
        }
        let ord = b.score.total_cmp(&a.score);
        if ord != Ordering::Equal {
            return ord;
        }
        let ord = a.candidate.path.cmp(&b.candidate.path);
        if ord != Ordering::Equal {
            return ord;
        }
        a.candidate.start_line.cmp(&b.candidate.start_line)
    });

    let mut kept: Vec<ScoredCandidate> = Vec::with_capacity(sorted.len());
    let mut seen_hashes: HashMap<(String, String), usize> = HashMap::new();
    let mut kept_by_path: HashMap<String, Vec<usize>> = HashMap::new();

    for candidate in sorted {
        let hash_key = (
            candidate.candidate.path.clone(),
            candidate.content_hash.clone(),
        );
        if let Some(existing) = seen_hashes.get(&hash_key).copied() {
            merge_scored_candidate(&mut kept[existing], &candidate, weights);
            continue;
        }

        let candidate_lines = candidate.candidate.line_count();
        let duplicate = kept_by_path
            .get(&candidate.candidate.path)
            .and_then(|indices| {
                indices.iter().copied().find(|&index| {
                    let existing = &kept[index];

                    // Non-overlapping ranges cannot be duplicates.
                    if candidate.candidate.end_line < existing.candidate.start_line
                        || candidate.candidate.start_line > existing.candidate.end_line
                    {
                        return false;
                    }

                    let overlap_start = candidate
                        .candidate
                        .start_line
                        .max(existing.candidate.start_line);
                    let overlap_end = candidate
                        .candidate
                        .end_line
                        .min(existing.candidate.end_line);
                    let overlap_lines = overlap_end - overlap_start + 1;
                    let min_lines = candidate_lines.min(existing.candidate.line_count());

                    overlap_lines as f64 >= OVERLAP_THRESHOLD * min_lines as f64
                })
            });
        if let Some(existing) = duplicate {
            merge_scored_candidate(&mut kept[existing], &candidate, weights);
            continue;
        }

        let kept_index = kept.len();
        seen_hashes.insert(hash_key, kept_index);
        kept_by_path
            .entry(candidate.candidate.path.clone())
            .or_default()
            .push(kept_index);
        kept.push(candidate);
    }

    kept.sort_by(|a, b| {
        b.score
            .total_cmp(&a.score)
            .then_with(|| a.candidate.path.cmp(&b.candidate.path))
            .then_with(|| a.candidate.start_line.cmp(&b.candidate.start_line))
    });
    kept
}

fn merge_scored_candidate(
    existing: &mut ScoredCandidate,
    duplicate: &ScoredCandidate,
    weights: &Weights,
) {
    merge_candidate_signals(&mut existing.candidate, &duplicate.candidate);
    existing.score = existing.candidate.score(weights, existing.token_count);
    existing.marginal_score = existing.score / existing.token_count as f64;
}

fn merge_candidate_signals(existing: &mut Candidate, duplicate: &Candidate) {
    for kind in &duplicate.match_kinds {
        if !existing.match_kinds.contains(kind) {
            existing.match_kinds.push(kind.clone());
        }
    }
    for concept in &duplicate.concepts {
        if !existing.concepts.contains(concept) {
            existing.concepts.push(concept.clone());
        }
    }
    existing.concept_weight = existing.concept_weight.max(duplicate.concept_weight);
    if existing.symbol_name.is_none() {
        existing.symbol_name.clone_from(&duplicate.symbol_name);
    }
    existing.exact = existing.exact.max(duplicate.exact);
    existing.symbol = existing.symbol.max(duplicate.symbol);
    existing.reference = existing.reference.max(duplicate.reference);
    existing.bm25 = existing.bm25.max(duplicate.bm25);
    existing.path_score = existing.path_score.max(duplicate.path_score);
    existing.lexical_frequency_penalty = existing
        .lexical_frequency_penalty
        .min(duplicate.lexical_frequency_penalty);
    existing.size_score = existing.size_score.max(duplicate.size_score);
    existing.focus_boost = existing.focus_boost.max(duplicate.focus_boost);
    existing.import_boost = existing.import_boost.max(duplicate.import_boost);
    existing.change_boost = existing.change_boost.max(duplicate.change_boost);
}

/// Select the highest-relevance candidates that fit within the token budget
/// while preserving file diversity and bounding protocol metadata.
#[must_use]
pub fn select(
    candidates: Vec<Candidate>,
    request: &ContextRequest,
    repository_generation: u64,
) -> ContextResponse {
    select_with_tokenizer(
        candidates,
        request,
        repository_generation,
        tokens::Tokenizer::default(),
    )
}

/// Select candidates using an explicit tokenizer for budgets and metadata.
#[must_use]
pub fn select_with_tokenizer(
    candidates: Vec<Candidate>,
    request: &ContextRequest,
    repository_generation: u64,
    tokenizer: tokens::Tokenizer,
) -> ContextResponse {
    select_with_tokenizer_and_context_exclusions(
        candidates,
        request,
        repository_generation,
        tokenizer,
        &default_context_exclude_paths(),
        &[],
    )
}

pub(crate) fn select_with_tokenizer_and_context_exclusions(
    candidates: Vec<Candidate>,
    request: &ContextRequest,
    repository_generation: u64,
    tokenizer: tokens::Tokenizer,
    context_exclude_paths: &[String],
    prefiltered_path_omissions: &[String],
) -> ContextResponse {
    select_with_options(
        candidates,
        request,
        repository_generation,
        &Weights::default(),
        tokenizer,
        context_exclude_paths,
        prefiltered_path_omissions,
    )
}

/// Same as [`select`] but with explicit [`Weights`].
#[must_use]
pub fn select_with_weights(
    candidates: Vec<Candidate>,
    request: &ContextRequest,
    repository_generation: u64,
    weights: &Weights,
) -> ContextResponse {
    select_with_weights_and_tokenizer(
        candidates,
        request,
        repository_generation,
        weights,
        tokens::Tokenizer::default(),
    )
}

/// Select candidates with explicit ranking weights and tokenizer.
#[must_use]
pub fn select_with_weights_and_tokenizer(
    candidates: Vec<Candidate>,
    request: &ContextRequest,
    repository_generation: u64,
    weights: &Weights,
    tokenizer: tokens::Tokenizer,
) -> ContextResponse {
    select_with_options(
        candidates,
        request,
        repository_generation,
        weights,
        tokenizer,
        &default_context_exclude_paths(),
        &[],
    )
}

fn select_with_options(
    candidates: Vec<Candidate>,
    request: &ContextRequest,
    repository_generation: u64,
    weights: &Weights,
    tokenizer: tokens::Tokenizer,
    context_exclude_paths: &[String],
    prefiltered_path_omissions: &[String],
) -> ContextResponse {
    let mut candidates = candidates;
    let focus_paths = PathMatcher::new_lossy(&request.focus_paths);
    let include_paths = PathMatcher::new_lossy(&request.include_paths);
    let exclude_paths = PathMatcher::new_lossy(&request.exclude_paths);
    let context_exclude_paths = PathMatcher::new_lossy(context_exclude_paths);
    let changed_paths = request
        .changed_paths
        .iter()
        .map(String::as_str)
        .collect::<HashSet<_>>();
    apply_request_signals(&mut candidates, request, &focus_paths);

    let known_hashes: HashSet<String> = request.known_hashes.iter().cloned().collect();

    let mut path_omitted: Vec<ScoredCandidate> = Vec::new();
    let mut known_omitted: Vec<ScoredCandidate> = Vec::new();
    let mut eligible: Vec<Candidate> = Vec::with_capacity(candidates.len());
    let mut generated_artifact_warning = false;

    for candidate in candidates {
        let explicitly_included =
            !request.include_paths.is_empty() && include_paths.is_match(&candidate.path);
        generated_artifact_warning |= context_exclude_paths.is_match(&candidate.path);
        if (!request.include_paths.is_empty() && !include_paths.is_match(&candidate.path))
            || exclude_paths.is_match(&candidate.path)
            || (context_exclude_paths.is_match(&candidate.path) && !explicitly_included)
            || (request.strict_focus_paths && !focus_paths.is_match(&candidate.path))
            || (request.strict_changed_paths && !changed_paths.contains(candidate.path.as_str()))
        {
            path_omitted.push(ScoredCandidate::new_with_tokenizer(
                candidate, weights, tokenizer,
            ));
            continue;
        }

        let hash = candidate.content_hash();
        if known_hashes.contains(&hash) {
            known_omitted.push(ScoredCandidate::new_with_tokenizer(
                candidate, weights, tokenizer,
            ));
        } else {
            eligible.push(candidate);
        }
    }

    let ranked = rank_with_tokenizer(eligible, weights, tokenizer);
    let deduped = deduplicate_with_options(ranked, weights);
    let candidate_paths_total = deduped
        .iter()
        .map(|candidate| candidate.candidate.path.as_str())
        .collect::<HashSet<_>>()
        .len();

    let budget = request.token_budget;
    let max_per_file = (budget / DIVERSITY_DIVISOR).clamp(1, 3);
    // Candidate excerpts vary from a few tokens to hundreds. A token-derived
    // fragment estimate underfilled budgets when high-value evidence happened
    // to be short. The caller-bounded cap limits metadata; the token budget
    // remains the authoritative content bound.
    let max_fragments = request.max_fragments.unwrap_or(DEFAULT_CONTEXT_FRAGMENTS);
    let (mut selected, remaining) =
        select_required_candidates(deduped, request, budget, max_fragments);
    let required_tokens = selected
        .iter()
        .map(|candidate| candidate.token_count)
        .sum::<usize>();
    let (additional, mut omitted) = greedy_select(
        remaining,
        budget.saturating_sub(required_tokens),
        max_per_file,
        max_fragments.saturating_sub(selected.len()),
    );
    selected.extend(additional);
    let result_complete = omitted.is_empty();

    let covered_candidates = selected.iter().chain(&known_omitted);
    let mut coverage = ContextCoverageReceipt::default();
    for pattern in &request.must_include_paths {
        if covered_candidates
            .clone()
            .any(|candidate| required_path_matches(&candidate.candidate, pattern))
        {
            coverage.covered_must_include_paths.push(pattern.clone());
        } else {
            coverage.uncovered_must_include_paths.push(pattern.clone());
        }
    }
    for symbol in &request.must_include_symbols {
        if covered_candidates
            .clone()
            .any(|candidate| required_symbol_matches(&candidate.candidate, symbol))
        {
            coverage.covered_must_include_symbols.push(symbol.clone());
        } else {
            coverage.uncovered_must_include_symbols.push(symbol.clone());
        }
    }

    let estimated_source_tokens = selected.iter().map(|candidate| candidate.token_count).sum();
    let plan = request.plan_only.then(|| {
        let minimum_fragments = request.minimum_fragments_per_focus_path.unwrap_or(1);
        let focus_coverage = request
            .focus_paths
            .iter()
            .map(|pattern| {
                let matcher = PathMatcher::new_lossy(std::slice::from_ref(pattern));
                let candidate_fragments = selected
                    .iter()
                    .filter(|candidate| matcher.is_match(&candidate.candidate.path))
                    .count();
                ContextPlanFocusCoverage {
                    pattern: pattern.clone(),
                    candidate_fragments,
                    minimum_fragments,
                    satisfied: candidate_fragments >= minimum_fragments,
                }
            })
            .collect();
        let candidates = selected
            .iter()
            .map(|scored| ContextPlanCandidate {
                path: scored.candidate.path.clone(),
                start_line: scored.candidate.start_line,
                end_line: scored.candidate.end_line,
                representation: scored.candidate.representation.clone(),
                score: (scored.score * 10_000.0).round() / 10_000.0,
                reasons: scored
                    .candidate
                    .reason()
                    .split("; ")
                    .map(str::to_owned)
                    .collect(),
                estimated_tokens: scored.token_count,
            })
            .collect();
        ContextQueryPlan {
            candidates,
            candidate_paths_total,
            estimated_source_tokens,
            focus_coverage,
            generated_artifact_warning,
            result_complete,
        }
    });

    // Materialized responses carry source; plans carry only the same selection's metadata.
    let mut fragments = Vec::with_capacity(selected.len());
    let mut fragment_hashes = Vec::with_capacity(selected.len());
    if !request.plan_only {
        for scored in &selected {
            fragments.push(ContextFragment {
                path: scored.candidate.path.clone(),
                start_line: scored.candidate.start_line,
                end_line: scored.candidate.end_line,
                representation: scored.candidate.representation.clone(),
                content: scored.candidate.content.clone(),
                content_hash: scored.content_hash.clone(),
                score: (scored.score * 10_000.0).round() / 10_000.0,
                reason: scored.candidate.reason(),
                token_count: scored.token_count,
            });
            fragment_hashes.push(scored.content_hash.clone());
        }
    }
    let emitted_tokens = if request.plan_only {
        0
    } else {
        estimated_source_tokens
    };

    let omission_summary = summarize_omissions(
        &path_omitted,
        &known_omitted,
        &omitted,
        prefiltered_path_omissions,
        &focus_paths,
        &changed_paths,
    );
    let mut omitted_dto: Vec<OmittedCandidate> = path_omitted
        .into_iter()
        .map(|scored| OmittedCandidate {
            path: scored.candidate.path,
            start_line: scored.candidate.start_line,
            end_line: scored.candidate.end_line,
            reason: "path excluded".to_string(),
        })
        .chain(known_omitted.into_iter().map(|scored| OmittedCandidate {
            path: scored.candidate.path,
            start_line: scored.candidate.start_line,
            end_line: scored.candidate.end_line,
            reason: "known hash".to_string(),
        }))
        .collect();

    omitted_dto.extend(omitted.drain(..).map(|scored| OmittedCandidate {
        path: scored.candidate.path,
        start_line: scored.candidate.start_line,
        end_line: scored.candidate.end_line,
        reason: "budget or result limit".to_string(),
    }));

    let omitted_count = omission_summary
        .path_excluded
        .saturating_add(omission_summary.known_hash)
        .saturating_add(omission_summary.budget_or_result_limit);
    omitted_dto.truncate(MAX_OMITTED_DETAILS);
    let mut warnings = Vec::new();
    if omitted_count > 0 {
        warnings.push(format!("{omitted_count} omitted"));
    }
    if request.plan_only && generated_artifact_warning {
        warnings.push(
            "generated-artifact candidates matched context exclusion defaults; review their explicit inclusion before materializing source"
                .into(),
        );
    }

    let task_hash = blake3::hash(request.task.as_bytes()).to_hex().to_string();
    let task_fingerprint = task_hash[..32].to_string();

    let receipt = EvidenceReceipt {
        task_fingerprint,
        fragment_hashes,
    };

    let meta = ResponseMeta {
        repository_id: String::new(),
        repository_generation,
        freshness: Freshness::Current,
        source_tokens: emitted_tokens,
        protocol_tokens: 0,
        path_and_metadata_tokens: 0,
        total_response_tokens: 0,
        payload_tokens: 0,
        tokenizer: tokenizer.name().into(),
        emitted_tokens,
        token_count_exact: tokenizer.is_exact(),
        receipt_id: None,
        receipt_suppressed_exact: 0,
        receipt_suppressed_overlap: 0,
        receipt_near_duplicates: 0,
        next_cursor: None,
    };

    let mut response = ContextResponse {
        workflow: crate::model::ContextWorkflow::Implementation,
        workflow_receipt: None,
        plan,
        fragments,
        receipt,
        diff_scope: None,
        omitted: omitted_dto,
        omission_summary,
        coverage,
        routing: None,
        handoff_manifest: None,
        warnings,
        meta,
    };
    let accounting = tokens::response_token_accounting(&response, emitted_tokens, &tokenizer)
        .expect("context response metadata is serializable");
    response.meta.protocol_tokens = accounting.protocol_tokens;
    response.meta.path_and_metadata_tokens = accounting.path_and_metadata_tokens;
    response.meta.total_response_tokens = accounting.total_response_tokens;
    response.meta.payload_tokens = accounting.total_response_tokens;
    response
}

fn select_required_candidates(
    mut candidates: Vec<ScoredCandidate>,
    request: &ContextRequest,
    budget: usize,
    max_fragments: usize,
) -> (Vec<ScoredCandidate>, Vec<ScoredCandidate>) {
    let mut selected = Vec::new();
    let mut used_tokens = 0usize;

    for pattern in &request.must_include_paths {
        if selected
            .iter()
            .any(|candidate: &ScoredCandidate| required_path_matches(&candidate.candidate, pattern))
        {
            continue;
        }
        let remaining = budget.saturating_sub(used_tokens);
        let Some(index) = candidates.iter().position(|candidate| {
            required_path_matches(&candidate.candidate, pattern)
                && candidate.token_count <= remaining
        }) else {
            continue;
        };
        if selected.len() == max_fragments {
            break;
        }
        let candidate = candidates.remove(index);
        used_tokens = used_tokens.saturating_add(candidate.token_count);
        selected.push(candidate);
    }

    for symbol in &request.must_include_symbols {
        if selected
            .iter()
            .any(|candidate| required_symbol_matches(&candidate.candidate, symbol))
        {
            continue;
        }
        let remaining = budget.saturating_sub(used_tokens);
        let Some(index) = candidates.iter().position(|candidate| {
            required_symbol_matches(&candidate.candidate, symbol)
                && candidate.token_count <= remaining
        }) else {
            continue;
        };
        if selected.len() == max_fragments {
            break;
        }
        let candidate = candidates.remove(index);
        used_tokens = used_tokens.saturating_add(candidate.token_count);
        selected.push(candidate);
    }

    let minimum_focus_fragments = request
        .minimum_fragments_per_focus_path
        .unwrap_or(usize::from(request.strict_focus_paths));
    if minimum_focus_fragments > 0 {
        for pattern in &request.focus_paths {
            while selected
                .iter()
                .filter(|candidate| required_path_matches(&candidate.candidate, pattern))
                .count()
                < minimum_focus_fragments
            {
                if selected.len() == max_fragments {
                    break;
                }
                let remaining = budget.saturating_sub(used_tokens);
                let Some(index) = candidates.iter().position(|candidate| {
                    required_path_matches(&candidate.candidate, pattern)
                        && candidate.token_count <= remaining
                }) else {
                    break;
                };
                let candidate = candidates.remove(index);
                used_tokens = used_tokens.saturating_add(candidate.token_count);
                selected.push(candidate);
            }
        }
    }

    (selected, candidates)
}

fn required_path_matches(candidate: &Candidate, pattern: &str) -> bool {
    path_matches(&candidate.path, pattern).unwrap_or(false)
}

fn required_symbol_matches(candidate: &Candidate, symbol: &str) -> bool {
    candidate
        .symbol_name
        .as_deref()
        .is_some_and(|name| name == symbol)
}

fn apply_request_signals(
    candidates: &mut [Candidate],
    request: &ContextRequest,
    focus_paths: &PathMatcher,
) {
    for candidate in candidates {
        if focus_paths.is_match(&candidate.path) {
            candidate.focus_boost += 1.0;
        }

        if let Some(ref name) = candidate.symbol_name {
            for focus_symbol in &request.focus_symbols {
                if focus_symbol == name {
                    candidate.focus_boost += 1.0;
                    break;
                }
            }
        }
    }
}

fn greedy_select(
    candidates: Vec<ScoredCandidate>,
    budget: usize,
    max_per_file: usize,
    max_fragments: usize,
) -> (Vec<ScoredCandidate>, Vec<ScoredCandidate>) {
    let mut pool = candidates;
    pool.sort_by(compare_utility);
    let confidence_floor = pool.first().map_or(0.0, |candidate| {
        candidate.score * MIN_RELATIVE_CONTEXT_SCORE
    });

    let mut selected = Vec::new();
    let mut deferred = Vec::with_capacity(pool.len());
    let mut omitted: Vec<ScoredCandidate> = Vec::with_capacity(pool.len());
    let mut used_tokens = 0usize;
    let mut file_counts: HashMap<String, usize> = HashMap::new();
    let mut covered_concepts = HashSet::new();
    let mut concept_representations = HashSet::new();
    let mut concept_paths = HashMap::new();

    for candidate in pool {
        let adds_concept = candidate
            .candidate
            .concepts
            .iter()
            .any(|concept| !covered_concepts.contains(concept));
        if !adds_concept || candidate.candidate.concept_weight < 1.0 {
            deferred.push(candidate);
            continue;
        }
        let file_count = *file_counts.get(&candidate.candidate.path).unwrap_or(&0);
        let remaining = budget.saturating_sub(used_tokens);

        if candidate_fits(
            &candidate,
            remaining,
            file_count,
            max_per_file,
            selected.len(),
            max_fragments,
        ) {
            covered_concepts.extend(candidate.candidate.concepts.iter().cloned());
            concept_representations.extend(
                candidate
                    .candidate
                    .concepts
                    .iter()
                    .map(|concept| (concept.clone(), candidate.candidate.representation.clone())),
            );
            for concept in &candidate.candidate.concepts {
                concept_paths
                    .entry(concept.clone())
                    .or_insert_with(|| candidate.candidate.path.clone());
            }
            push_selected(candidate, &mut selected, &mut used_tokens, &mut file_counts);
        } else {
            deferred.push(candidate);
        }
    }

    deferred.sort_by(|left, right| {
        let left_same_path = left.candidate.concepts.iter().any(|concept| {
            concept_paths
                .get(concept)
                .is_some_and(|path| path == &left.candidate.path)
        });
        let right_same_path = right.candidate.concepts.iter().any(|concept| {
            concept_paths
                .get(concept)
                .is_some_and(|path| path == &right.candidate.path)
        });
        right_same_path
            .cmp(&left_same_path)
            .then_with(|| compare_utility(left, right))
    });
    let mut remaining = Vec::with_capacity(deferred.len());
    for candidate in deferred {
        let adds_decisive_view = candidate.candidate.concept_weight >= 1.8
            && candidate.candidate.concepts.iter().any(|concept| {
                covered_concepts.contains(concept)
                    && !concept_representations
                        .contains(&(concept.clone(), candidate.candidate.representation.clone()))
            });
        let file_count = *file_counts.get(&candidate.candidate.path).unwrap_or(&0);
        let remaining_tokens = budget.saturating_sub(used_tokens);
        if adds_decisive_view
            && candidate_fits(
                &candidate,
                remaining_tokens,
                file_count,
                max_per_file,
                selected.len(),
                max_fragments,
            )
        {
            concept_representations.extend(
                candidate
                    .candidate
                    .concepts
                    .iter()
                    .map(|concept| (concept.clone(), candidate.candidate.representation.clone())),
            );
            push_selected(candidate, &mut selected, &mut used_tokens, &mut file_counts);
        } else {
            remaining.push(candidate);
        }
    }

    let mut fill = Vec::with_capacity(remaining.len());
    for candidate in remaining {
        let adds_concept = candidate
            .candidate
            .concepts
            .iter()
            .any(|concept| !covered_concepts.contains(concept));
        let file_count = *file_counts.get(&candidate.candidate.path).unwrap_or(&0);
        let remaining_tokens = budget.saturating_sub(used_tokens);
        let confident =
            candidate.candidate.concept_weight >= 1.0 || candidate.score >= confidence_floor;
        if adds_concept
            && confident
            && candidate_fits(
                &candidate,
                remaining_tokens,
                file_count,
                max_per_file,
                selected.len(),
                max_fragments,
            )
        {
            covered_concepts.extend(candidate.candidate.concepts.iter().cloned());
            push_selected(candidate, &mut selected, &mut used_tokens, &mut file_counts);
        } else {
            fill.push(candidate);
        }
    }

    for candidate in fill {
        if candidate.candidate.concept_weight < 1.0 && candidate.score < confidence_floor {
            omitted.push(candidate);
            continue;
        }
        let file_count = *file_counts.get(&candidate.candidate.path).unwrap_or(&0);
        let remaining = budget.saturating_sub(used_tokens);
        if candidate_fits(
            &candidate,
            remaining,
            file_count,
            max_per_file,
            selected.len(),
            max_fragments,
        ) {
            push_selected(candidate, &mut selected, &mut used_tokens, &mut file_counts);
        } else {
            omitted.push(candidate);
        }
    }

    (selected, omitted)
}

fn candidate_fits(
    candidate: &ScoredCandidate,
    remaining_tokens: usize,
    file_count: usize,
    max_per_file: usize,
    selected_count: usize,
    max_fragments: usize,
) -> bool {
    candidate.token_count <= remaining_tokens
        && file_count < max_per_file
        && selected_count < max_fragments
}

fn push_selected(
    candidate: ScoredCandidate,
    selected: &mut Vec<ScoredCandidate>,
    used_tokens: &mut usize,
    file_counts: &mut HashMap<String, usize>,
) {
    *used_tokens += candidate.token_count;
    *file_counts
        .entry(candidate.candidate.path.clone())
        .or_insert(0) += 1;
    selected.push(candidate);
}

fn compare_utility(a: &ScoredCandidate, b: &ScoredCandidate) -> Ordering {
    let ord = b.score.total_cmp(&a.score);
    if ord != Ordering::Equal {
        return ord;
    }

    let ord = b.marginal_score.total_cmp(&a.marginal_score);
    if ord != Ordering::Equal {
        return ord;
    }

    let ord = a.token_count.cmp(&b.token_count);
    if ord != Ordering::Equal {
        return ord;
    }

    let ord = a.candidate.path.cmp(&b.candidate.path);
    if ord != Ordering::Equal {
        return ord;
    }

    a.candidate.start_line.cmp(&b.candidate.start_line)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request_with_budget(budget: usize) -> ContextRequest {
        ContextRequest {
            task: "rank source evidence for a task".into(),
            token_budget: budget,
            include_paths: Vec::new(),
            must_include_paths: Vec::new(),
            must_include_symbols: Vec::new(),
            max_fragments: None,
            plan_only: false,
            focus_paths: Vec::new(),
            strict_focus_paths: false,
            minimum_fragments_per_focus_path: None,
            focus_symbols: Vec::new(),
            exclude_paths: Vec::new(),
            known_hashes: Vec::new(),
            receipt_id: None,
            prior_repository_generation: None,
            base_revision: None,
            changed_paths: Vec::new(),
            strict_changed_paths: false,
        }
    }

    fn request_focused(budget: usize, focus_path: &str) -> ContextRequest {
        ContextRequest {
            task: "focus path test".into(),
            token_budget: budget,
            include_paths: Vec::new(),
            must_include_paths: Vec::new(),
            must_include_symbols: Vec::new(),
            max_fragments: None,
            plan_only: false,
            focus_paths: vec![focus_path.into()],
            strict_focus_paths: false,
            minimum_fragments_per_focus_path: None,
            focus_symbols: Vec::new(),
            exclude_paths: Vec::new(),
            known_hashes: Vec::new(),
            receipt_id: None,
            prior_repository_generation: None,
            base_revision: None,
            changed_paths: Vec::new(),
            strict_changed_paths: false,
        }
    }

    fn request_excluding(budget: usize, exclude: &str) -> ContextRequest {
        ContextRequest {
            task: "exclude path test".into(),
            token_budget: budget,
            include_paths: Vec::new(),
            must_include_paths: Vec::new(),
            must_include_symbols: Vec::new(),
            max_fragments: None,
            plan_only: false,
            focus_paths: Vec::new(),
            strict_focus_paths: false,
            minimum_fragments_per_focus_path: None,
            focus_symbols: Vec::new(),
            exclude_paths: vec![exclude.into()],
            known_hashes: Vec::new(),
            receipt_id: None,
            prior_repository_generation: None,
            base_revision: None,
            changed_paths: Vec::new(),
            strict_changed_paths: false,
        }
    }

    #[test]
    fn score_is_finite_and_non_negative() {
        let candidate = Candidate::new("a.rs", 1, 2, "fn main() {}")
            .exact(1.0)
            .symbol(1.0)
            .reference(1.0)
            .bm25(10.0)
            .path_score(0.8)
            .focus_boost(0.5)
            .import_boost(0.5)
            .change_boost(0.5)
            .lexical_frequency_penalty(0.2);

        let weights = Weights::default();
        let score = candidate.score(&weights, candidate.token_count());
        assert!(score.is_finite());
        assert!(score >= 0.0);
    }

    #[test]
    fn internal_facet_provenance_does_not_expand_response_reasons() {
        let candidate = Candidate::new("src/lib.rs", 1, 1, "target")
            .match_kind("symbol")
            .facet("exact_atom", "target")
            .channel("symbol", 3);

        assert_eq!(candidate.reason(), "symbol");
        assert!(
            candidate
                .match_kinds
                .iter()
                .any(|kind| kind == "facet:exact_atom:target")
        );
        assert!(
            candidate
                .match_kinds
                .iter()
                .any(|kind| kind == "channel:symbol:3")
        );
    }

    #[test]
    fn bm25_normalizes_and_saturates() {
        let w = Weights::default();
        let low = Candidate::new("a.rs", 1, 1, "x").bm25(0.1);
        let high = Candidate::new("a.rs", 1, 1, "x").bm25(1_000_000.0);

        let low_score = low.score(&w, low.token_count());
        let high_score = high.score(&w, high.token_count());

        assert!(high_score > low_score);
        // Saturated BM25 contribution should be bounded.
        assert!(high_score < low_score + w.bm25 * 2.0 + 1.0);
    }

    #[test]
    fn lexical_frequency_penalty_reduces_score() {
        let w = Weights::default();
        let base = Candidate::new("a.rs", 1, 1, "x").exact(1.0);
        let penalized = Candidate::new("a.rs", 1, 1, "x")
            .exact(1.0)
            .lexical_frequency_penalty(1.0);

        let base_score = base.score(&w, base.token_count());
        let penalized_score = penalized.score(&w, penalized.token_count());

        assert!(penalized_score < base_score);
    }

    #[test]
    fn larger_implicit_size_score_is_smaller() {
        let w = Weights::default();
        let small = Candidate::new("a.rs", 1, 1, "x").exact(1.0);
        let large = Candidate::new("a.rs", 1, 1, "word ".repeat(50)).exact(1.0);

        let small_score = small.score(&w, small.token_count());
        let large_score = large.score(&w, large.token_count());

        // Both exact, but the larger content gets an implicit size penalty.
        assert!(large_score < small_score || large.token_count() == small.token_count());
    }

    #[test]
    fn content_hash_is_deterministic() {
        let a = Candidate::new("a.rs", 1, 2, "same content");
        let b = Candidate::new("b.rs", 3, 4, "same content");
        assert_eq!(a.content_hash(), b.content_hash());
        assert_ne!(
            a.content_hash(),
            Candidate::new("a.rs", 1, 2, "different").content_hash()
        );
    }

    #[test]
    fn dedup_keeps_content_identical_highest_score() {
        let a = Candidate::new("a.rs", 1, 2, "same body")
            .exact(1.0)
            .match_kind("exact");
        let b = Candidate::new("a.rs", 10, 11, "same body")
            .exact(0.5)
            .match_kind("reference");

        let ranked = rank(vec![a, b], &Weights::default());
        let deduped = deduplicate(ranked);

        assert_eq!(deduped.len(), 1);
        assert!(deduped[0].score > 0.9);
    }

    #[test]
    fn dedup_keeps_content_identical_candidates_at_distinct_paths() {
        let implementation = Candidate::new("src/lib.rs", 1, 1, "same body").exact(1.0);
        let contract = Candidate::new("examples/lib.rs", 1, 1, "same body").exact(0.5);

        let deduped = deduplicate(rank(vec![implementation, contract], &Weights::default()));

        assert_eq!(deduped.len(), 2);
    }

    #[test]
    fn dedup_merges_multi_channel_provenance_and_recomputes_score() {
        let symbol = Candidate::new("src/lib.rs", 1, 2, "fn target() {}")
            .concept("target", 2.0)
            .match_kind("symbol")
            .facet("exact_atom", "target")
            .channel("symbol", 0)
            .symbol(1.0);
        let text = Candidate::new("src/lib.rs", 1, 2, "fn target() {}")
            .concept("behavior", 0.8)
            .match_kind("text")
            .facet("behavior", "behavior")
            .channel("text", 2)
            .exact(1.0)
            .bm25(1_000_000.0);
        let best_single = rank(vec![symbol.clone(), text.clone()], &Weights::default())
            .into_iter()
            .map(|candidate| candidate.score)
            .fold(0.0, f64::max);

        let deduped = deduplicate(rank(vec![symbol, text], &Weights::default()));

        assert_eq!(deduped.len(), 1);
        let merged = &deduped[0];
        assert!(merged.score > best_single);
        assert!(
            merged
                .candidate
                .concepts
                .iter()
                .any(|value| value == "target")
        );
        assert!(
            merged
                .candidate
                .concepts
                .iter()
                .any(|value| value == "behavior")
        );
        assert!(
            merged
                .candidate
                .match_kinds
                .iter()
                .any(|value| value == "symbol")
        );
        assert!(
            merged
                .candidate
                .match_kinds
                .iter()
                .any(|value| value == "text")
        );
    }

    #[test]
    fn dedup_preserves_exact_range_when_a_broader_candidate_overlaps() {
        let broad = Candidate::new("a.rs", 1, 10, "broad")
            .bm25(1_000_000.0)
            .path_score(2.0);
        let exact = Candidate::new("a.rs", 5, 15, "exact").exact(1.0);

        let deduped = deduplicate(rank(vec![broad, exact], &Weights::default()));

        assert_eq!(deduped.len(), 1);
        assert_eq!(deduped[0].candidate.start_line, 5);
    }

    #[test]
    fn dedup_keeps_overlapping_highest_score() {
        let a = Candidate::new("a.rs", 1, 10, "first").exact(1.0);
        let b = Candidate::new("a.rs", 5, 15, "second").exact(0.5);

        let ranked = rank(vec![a, b], &Weights::default());
        let deduped = deduplicate(ranked);

        // 6 of 10 lines overlap, exceeding the 0.5 threshold.
        assert_eq!(deduped.len(), 1);
    }

    #[test]
    fn dedup_keeps_non_overlapping_same_file() {
        let a = Candidate::new("a.rs", 1, 5, "first").exact(1.0);
        let b = Candidate::new("a.rs", 7, 10, "second").exact(0.9);

        let ranked = rank(vec![a, b], &Weights::default());
        let deduped = deduplicate(ranked);

        assert_eq!(deduped.len(), 2);
    }

    #[test]
    fn rank_orders_by_score() {
        let a = Candidate::new("a.rs", 1, 1, "x").exact(1.0);
        let b = Candidate::new("b.rs", 1, 1, "x").exact(0.5);
        let c = Candidate::new("c.rs", 1, 1, "x").exact(0.0);

        let ranked = rank(vec![c, b, a], &Weights::default());

        assert!(ranked[0].score > ranked[1].score);
        assert!(ranked[1].score > ranked[2].score);
    }

    #[test]
    fn selection_skips_a_higher_scored_candidate_that_does_not_fit() {
        let cheap = Candidate::new("cheap.rs", 1, 1, "alpha").exact(0.5);
        let expensive = Candidate::new("expensive.rs", 1, 1, "alpha ".repeat(20)).exact(1.0);

        let req = request_with_budget(1);
        let resp = select(vec![expensive, cheap], &req, 1);

        assert_eq!(resp.fragments.len(), 1);
        assert_eq!(resp.fragments[0].path, "cheap.rs");
    }

    #[test]
    fn file_diversity_caps_same_file_selection() {
        let a1 = Candidate::new("a.rs", 1, 2, "alpha beta").exact(1.0);
        let a2 = Candidate::new("a.rs", 10, 11, "gamma delta").exact(0.95);
        let b1 = Candidate::new("b.rs", 1, 2, "epsilon zeta").exact(0.9);

        // Budget is enough for two 2-token fragments.
        let req = request_with_budget(10);
        let resp = select(vec![a1, a2, b1], &req, 1);

        let a_count = resp.fragments.iter().filter(|f| f.path == "a.rs").count();
        let b_count = resp.fragments.iter().filter(|f| f.path == "b.rs").count();

        assert_eq!(a_count, 1);
        assert_eq!(b_count, 1);
    }

    #[test]
    fn context_uses_short_fragments_without_underfilling_result_cap() {
        let mut candidates = (0..8)
            .map(|index| {
                Candidate::new(format!("file{index}.rs"), 1, 1, format!("evidence_{index}"))
                    .exact(1.0)
            })
            .collect::<Vec<_>>();
        candidates.push(Candidate::new("file0.rs", 20, 20, "second_region").exact(2.0));

        let response = select(candidates, &request_with_budget(1_200), 1);

        assert_eq!(response.fragments.len(), DEFAULT_CONTEXT_FRAGMENTS);
        assert_eq!(
            response
                .fragments
                .iter()
                .filter(|fragment| fragment.path == "file0.rs")
                .count(),
            2
        );
        assert!(response.meta.emitted_tokens < 1_200);
    }

    #[test]
    fn context_honors_caller_fragment_limit_above_the_default() {
        let candidates = (0..12)
            .map(|index| {
                Candidate::new(format!("file{index}.rs"), 1, 1, format!("evidence_{index}"))
                    .concept(format!("concept_{index}"), 1.0)
                    .exact(1.0)
            })
            .collect::<Vec<_>>();
        let mut request = request_with_budget(1_200);
        request.max_fragments = Some(12);

        let response = select(candidates, &request, 1);

        assert_eq!(response.fragments.len(), 12);
    }

    #[test]
    fn must_cover_candidate_precedes_higher_scored_general_evidence() {
        let required = Candidate::new("src/required.rs", 1, 1, "required")
            .symbol_name("required_symbol")
            .exact(0.1);
        let general = Candidate::new("src/general.rs", 1, 1, "general").exact(10.0);
        let mut request = request_with_budget(100);
        request.must_include_paths = vec!["src/required.rs".into()];
        request.must_include_symbols = vec!["required_symbol".into()];
        request.max_fragments = Some(1);

        let response = select(vec![general, required], &request, 1);

        assert_eq!(response.fragments[0].path, "src/required.rs");
        assert_eq!(
            response.coverage.covered_must_include_paths,
            vec!["src/required.rs"]
        );
        assert_eq!(
            response.coverage.covered_must_include_symbols,
            vec!["required_symbol"]
        );
        assert!(response.coverage.uncovered_must_include_paths.is_empty());
        assert!(response.coverage.uncovered_must_include_symbols.is_empty());
    }

    #[test]
    fn uncovered_must_cover_requirements_are_explicit() {
        let mut request = request_with_budget(100);
        request.must_include_paths = vec!["src/missing.rs".into()];
        request.must_include_symbols = vec!["missing_symbol".into()];

        let response = select(
            vec![Candidate::new("src/general.rs", 1, 1, "general").exact(1.0)],
            &request,
            1,
        );

        assert_eq!(
            response.coverage.uncovered_must_include_paths,
            vec!["src/missing.rs"]
        );
        assert_eq!(
            response.coverage.uncovered_must_include_symbols,
            vec!["missing_symbol"]
        );
    }

    #[test]
    fn known_hash_satisfies_must_cover_without_resending_source() {
        let required = Candidate::new("src/required.rs", 1, 1, "required")
            .symbol_name("required_symbol")
            .exact(1.0);
        let known_hash = required.content_hash();
        let mut request = request_with_budget(100);
        request.must_include_paths = vec!["src/required.rs".into()];
        request.must_include_symbols = vec!["required_symbol".into()];
        request.known_hashes = vec![known_hash];

        let response = select(vec![required], &request, 1);

        assert!(response.fragments.is_empty());
        assert_eq!(response.omission_summary.known_hash, 1);
        assert_eq!(
            response.coverage.covered_must_include_paths,
            vec!["src/required.rs"]
        );
        assert_eq!(
            response.coverage.covered_must_include_symbols,
            vec!["required_symbol"]
        );
        assert!(response.coverage.uncovered_must_include_paths.is_empty());
        assert!(response.coverage.uncovered_must_include_symbols.is_empty());
    }

    #[test]
    fn concept_allocation_keeps_independent_task_evidence() {
        let alpha_best = Candidate::new("alpha.rs", 1, 1, "alpha evidence")
            .concept("alpha", 1.0)
            .exact(2.0);
        let alpha_duplicate = Candidate::new("alpha_other.rs", 1, 1, "more alpha")
            .concept("alpha", 1.0)
            .exact(1.5);
        let beta = Candidate::new("beta.rs", 1, 1, "beta evidence")
            .concept("beta", 1.0)
            .exact(0.1);

        let response = select(
            vec![alpha_duplicate, beta, alpha_best],
            &request_with_budget(6),
            1,
        );

        assert!(
            response
                .fragments
                .iter()
                .any(|fragment| fragment.path == "alpha.rs")
        );
        assert!(
            response
                .fragments
                .iter()
                .any(|fragment| fragment.path == "beta.rs")
        );
    }

    #[test]
    fn decisive_second_view_prefers_the_definition_path() {
        let definition = Candidate::new("owner.rs", 1, 1, "definition")
            .concept("handle", 2.0)
            .representation("symbol")
            .exact(10.0);
        let owner_source = Candidate::new("owner.rs", 10, 10, "owner_source")
            .concept("handle", 2.0)
            .exact(0.5);
        let unrelated_source = Candidate::new("other.rs", 1, 1, "other ".repeat(3_000))
            .concept("handle", 2.0)
            .exact(1.0);

        let response = select(
            vec![unrelated_source, owner_source, definition],
            &request_with_budget(1_200),
            1,
        );

        assert_eq!(response.fragments.len(), 2);
        assert_eq!(response.fragments[0].path, "owner.rs");
        assert_eq!(response.fragments[1].path, "owner.rs");
    }

    #[test]
    fn weak_non_code_fill_is_omitted_by_relative_confidence() {
        let strong = Candidate::new("strong.rs", 1, 1, "strong")
            .concept("explicit", 1.0)
            .exact(10.0);
        let weak = Candidate::new("weak.rs", 1, 1, "weak").exact(0.0);

        let response = select(vec![weak, strong], &request_with_budget(100), 1);

        assert_eq!(response.fragments.len(), 1);
        assert_eq!(response.fragments[0].path, "strong.rs");
        assert!(
            response
                .warnings
                .iter()
                .any(|warning| warning.contains("omitted"))
        );
    }

    #[test]
    fn known_hash_omitted_and_reported() {
        let c = Candidate::new("known.rs", 1, 2, "alpha beta").exact(1.0);
        let hash = c.content_hash();

        let mut req = request_with_budget(10);
        req.known_hashes.push(hash);

        let resp = select(vec![c], &req, 1);

        assert!(resp.fragments.is_empty());
        assert_eq!(resp.omitted.len(), 1);
        assert_eq!(resp.omitted[0].reason, "known hash");
    }

    #[test]
    fn exclude_paths_filter_candidates() {
        let kept = Candidate::new("src/lib.rs", 1, 2, "alpha").exact(1.0);
        let excluded = Candidate::new("test/ranking.rs", 1, 2, "beta").exact(1.0);

        let req = request_excluding(10, "test");
        let resp = select(vec![kept, excluded], &req, 1);

        assert_eq!(resp.fragments.len(), 1);
        assert_eq!(resp.fragments[0].path, "src/lib.rs");
        assert_eq!(resp.omission_summary.path_excluded, 1);
        assert_eq!(resp.omitted[0].reason, "path excluded");
    }

    #[test]
    fn omission_summary_reports_bounded_coverage_facets() {
        let selected = Candidate::new("src/selected.rs", 1, 2, "selected").exact(3.0);
        let limited = Candidate::new("src/changed.rs", 1, 2, "limited").exact(2.0);
        let known = Candidate::new("src/known.md", 1, 2, "known").exact(1.0);
        let known_hash = known.content_hash();
        let excluded = Candidate::new("tests/helper.ts", 1, 2, "excluded").exact(1.0);
        let mut request = request_with_budget(100);
        request.max_fragments = Some(1);
        request.focus_paths = vec!["src/**".into()];
        request.changed_paths = vec!["src/changed.rs".into()];
        request.exclude_paths = vec!["tests/**".into()];
        request.known_hashes = vec![known_hash];

        let response = select_with_tokenizer_and_context_exclusions(
            vec![selected, limited, known, excluded],
            &request,
            1,
            tokens::Tokenizer::default(),
            &[],
            &["generated/tool.js".into()],
        );

        let summary = response.omission_summary;
        assert_eq!(summary.path_excluded, 2);
        assert_eq!(summary.known_hash, 1);
        assert_eq!(summary.budget_or_result_limit, 1);
        assert_eq!(summary.focused, 2);
        assert_eq!(summary.not_focused, 2);
        assert_eq!(summary.changed, 1);
        assert_eq!(summary.not_changed, 3);
        assert!(summary.by_reason.contains(&ContextOmissionFacet {
            value: "path_excluded".into(),
            count: 2,
        }));
        assert!(
            summary
                .by_language_or_file_type
                .iter()
                .any(|facet| facet.value == ".js" && facet.count == 1)
        );
        assert!(
            summary
                .by_score_band
                .iter()
                .any(|facet| facet.value == "not scored" && facet.count == 1)
        );
        assert_eq!(
            summary
                .by_path
                .iter()
                .map(|facet| facet.count)
                .sum::<usize>(),
            4
        );
    }

    #[test]
    fn omission_facets_fold_long_tails_into_other() {
        let counts = (0..20)
            .map(|index| (format!("path-{index:02}"), 1))
            .collect();

        let facets = bounded_facets(counts);

        assert_eq!(facets.len(), MAX_OMISSION_FACETS);
        assert_eq!(facets.last().expect("other").value, "[other]");
        assert_eq!(facets.last().expect("other").count, 9);
        assert_eq!(facets.iter().map(|facet| facet.count).sum::<usize>(), 20);
    }

    #[test]
    fn include_paths_are_a_hard_fragment_boundary() {
        let included = Candidate::new("src/browser/capture.rs", 1, 2, "alpha").exact(0.5);
        let unrelated = Candidate::new("src/managed/evidence.rs", 1, 2, "beta").exact(10.0);
        let mut request = request_with_budget(10);
        request.include_paths = vec!["src/browser/**".into()];

        let response = select(vec![unrelated, included], &request, 1);

        assert_eq!(response.fragments.len(), 1);
        assert_eq!(response.fragments[0].path, "src/browser/capture.rs");
        assert_eq!(response.omission_summary.path_excluded, 1);
        assert_eq!(response.omitted[0].reason, "path excluded");
    }

    #[test]
    fn generated_context_defaults_require_an_explicit_include() {
        let generated =
            Candidate::new("artifacts/runtime_reports/latest.json", 1, 2, "generated").exact(10.0);
        let source = Candidate::new("src/runtime.rs", 1, 2, "source").exact(0.5);
        let request = request_with_budget(20);

        let response = select(vec![generated.clone(), source], &request, 1);

        assert_eq!(response.fragments.len(), 1);
        assert_eq!(response.fragments[0].path, "src/runtime.rs");
        assert_eq!(response.omission_summary.path_excluded, 1);

        let mut included_request = request_with_budget(20);
        included_request.include_paths = vec!["artifacts/runtime_reports/**".into()];
        let included = select(vec![generated], &included_request, 1);

        assert_eq!(included.fragments.len(), 1);
        assert_eq!(
            included.fragments[0].path,
            "artifacts/runtime_reports/latest.json"
        );
    }

    #[test]
    fn context_plan_matches_materialized_selection_without_source() {
        let focused = Candidate::new("src/ranking.rs", 10, 12, "focused evidence")
            .match_kind("symbol")
            .exact(2.0);
        let other = Candidate::new("src/other.rs", 20, 21, "other evidence").match_kind("text");
        let candidates = vec![other, focused];
        let mut request = request_focused(100, "src/ranking.rs");
        request.max_fragments = Some(1);
        request.plan_only = true;

        let preview = select(candidates.clone(), &request, 7);
        let plan = preview.plan.as_ref().expect("query plan");

        assert!(preview.fragments.is_empty());
        assert!(preview.receipt.fragment_hashes.is_empty());
        assert_eq!(preview.meta.source_tokens, 0);
        assert_eq!(preview.meta.emitted_tokens, 0);
        assert!(!plan.candidates.is_empty());
        assert_eq!(plan.candidates.len(), 1);
        assert!(!plan.result_complete);
        assert!(
            plan.candidates
                .iter()
                .all(|candidate| candidate.score >= 0.0)
        );
        assert!(
            plan.candidates
                .iter()
                .all(|candidate| !candidate.reasons.is_empty())
        );
        assert_eq!(
            plan.estimated_source_tokens,
            plan.candidates
                .iter()
                .map(|candidate| candidate.estimated_tokens)
                .sum::<usize>()
        );
        assert_eq!(plan.focus_coverage.len(), 1);
        assert!(plan.focus_coverage[0].satisfied);

        request.plan_only = false;
        let materialized = select(candidates, &request, 7);
        assert!(materialized.plan.is_none());
        assert_eq!(
            plan.candidates
                .iter()
                .map(|candidate| (&candidate.path, candidate.start_line, candidate.end_line))
                .collect::<Vec<_>>(),
            materialized
                .fragments
                .iter()
                .map(|fragment| (&fragment.path, fragment.start_line, fragment.end_line))
                .collect::<Vec<_>>()
        );
        assert_eq!(
            plan.estimated_source_tokens,
            materialized.meta.source_tokens
        );
    }

    #[test]
    fn context_plan_warns_when_generated_defaults_match() {
        let generated =
            Candidate::new("artifacts/runtime_reports/latest.json", 1, 2, "generated").exact(10.0);
        let source = Candidate::new("src/runtime.rs", 1, 2, "source").exact(0.5);
        let mut request = request_with_budget(20);
        request.plan_only = true;

        let response = select(vec![generated, source], &request, 1);
        let plan = response.plan.expect("query plan");

        assert!(plan.generated_artifact_warning);
        assert!(
            response
                .warnings
                .iter()
                .any(|warning| warning.contains("generated-artifact"))
        );
        assert!(
            plan.candidates
                .iter()
                .all(|candidate| candidate.path != "artifacts/runtime_reports/latest.json")
        );
    }

    #[test]
    fn focus_path_boosts_selection() {
        let focus = Candidate::new("src/ranking.rs", 1, 2, "alpha").exact(0.5);
        let other = Candidate::new("src/other.rs", 1, 2, "beta").exact(0.5);

        let req = request_focused(10, "src/ranking.rs");
        let resp = select(vec![other, focus], &req, 1);

        assert_eq!(resp.fragments.len(), 2);
        // Higher combined score should place the focus candidate first.
        assert_eq!(resp.fragments[0].path, "src/ranking.rs");
    }

    #[test]
    fn focus_symbol_boosts_selection() {
        let focus = Candidate::new("a.rs", 1, 2, "alpha")
            .exact(0.5)
            .symbol_name("rank_items");
        let other = Candidate::new("b.rs", 1, 2, "beta")
            .exact(0.5)
            .symbol_name("other");

        let mut req = request_with_budget(10);
        req.focus_symbols.push("rank_items".into());

        let resp = select(vec![other, focus], &req, 1);

        assert_eq!(resp.fragments[0].path, "a.rs");
    }

    #[test]
    fn budget_omits_low_value_candidates() {
        let tiny = Candidate::new("tiny.rs", 1, 1, "alpha").exact(1.0);
        let huge = Candidate::new(
            "huge.rs",
            1,
            1,
            (0..200).map(|i| format!("token{i} ")).collect::<String>(),
        )
        .exact(0.9);

        let req = request_with_budget(5);
        let resp = select(vec![huge, tiny], &req, 1);

        // tiny should be selected; huge should not fit in a budget of 5 tokens.
        assert_eq!(resp.fragments.len(), 1);
        assert_eq!(resp.fragments[0].path, "tiny.rs");
        assert!(!resp.omitted.is_empty());
    }

    #[test]
    fn evidence_receipt_populated() {
        let c = Candidate::new("a.rs", 1, 2, "alpha beta").exact(1.0);

        let req = request_with_budget(10);
        let resp = select(vec![c], &req, 42);

        assert_eq!(resp.meta.repository_generation, 42);
        assert!(!resp.receipt.task_fingerprint.is_empty());
        assert_eq!(resp.receipt.fragment_hashes.len(), resp.fragments.len());
        assert_eq!(
            resp.meta.emitted_tokens,
            resp.fragments.iter().map(|f| f.token_count).sum::<usize>()
        );
        assert_eq!(resp.meta.source_tokens, resp.meta.emitted_tokens);
        assert_eq!(resp.meta.tokenizer, tokens::Tokenizer::default().name());
        let mut countable = resp.clone();
        countable.meta.protocol_tokens = 0;
        countable.meta.path_and_metadata_tokens = 0;
        countable.meta.total_response_tokens = 0;
        countable.meta.payload_tokens = 0;
        let payload = serde_json::to_string(&countable).expect("serialize context response");
        assert_eq!(
            resp.meta.total_response_tokens,
            tokens::Tokenizer::default().count(&payload)
        );
        assert_eq!(resp.meta.payload_tokens, resp.meta.total_response_tokens);
        assert_eq!(
            resp.meta.total_response_tokens,
            resp.meta.source_tokens
                + resp.meta.protocol_tokens
                + resp.meta.path_and_metadata_tokens
        );
        assert!(resp.meta.token_count_exact);
    }

    #[test]
    fn explicit_weights_and_tokenizer_control_budget_metadata() {
        let candidate = Candidate::new("a.rs", 1, 1, "alpha beta gamma").exact(1.0);
        let request = request_with_budget(20);
        let response = select_with_weights_and_tokenizer(
            vec![candidate],
            &request,
            7,
            &Weights::default(),
            tokens::Tokenizer::Estimate,
        );

        assert!(!response.meta.token_count_exact);
        assert_eq!(response.meta.source_tokens, response.meta.emitted_tokens);
        assert_eq!(response.meta.tokenizer, tokens::Tokenizer::Estimate.name());
        assert_eq!(response.meta.emitted_tokens, 4);
    }

    #[test]
    fn empty_pool_returns_empty_response() {
        let req = request_with_budget(100);
        let resp = select(Vec::new(), &req, 1);

        assert!(resp.fragments.is_empty());
        assert!(resp.omitted.is_empty());
        assert!(resp.receipt.fragment_hashes.is_empty());
    }

    #[test]
    fn change_boost_increases_score() {
        let w = Weights::default();
        let base = Candidate::new("a.rs", 1, 1, "x").exact(1.0);
        let changed = Candidate::new("a.rs", 1, 1, "x")
            .exact(1.0)
            .change_boost(1.0);

        assert!(changed.score(&w, changed.token_count()) > base.score(&w, base.token_count()));
    }

    #[test]
    fn import_boost_increases_score() {
        let w = Weights::default();
        let base = Candidate::new("a.rs", 1, 1, "x").exact(1.0);
        let imported = Candidate::new("a.rs", 1, 1, "x")
            .exact(1.0)
            .import_boost(1.0);

        assert!(imported.score(&w, imported.token_count()) > base.score(&w, base.token_count()));
    }
}
