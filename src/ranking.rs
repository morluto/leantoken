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

use crate::model::{
    ContextFragment, ContextRequest, ContextResponse, EvidenceReceipt, Freshness, OmittedCandidate,
    ResponseMeta,
};
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
const MAX_CONTEXT_FRAGMENTS: usize = 8;
const MAX_OMITTED_DETAILS: usize = 1;
const MIN_RELATIVE_CONTEXT_SCORE: f64 = 0.25;

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
    deduplicate_with_options(
        candidates,
        &Weights::default(),
        tokens::Tokenizer::default(),
    )
}

#[allow(clippy::cast_precision_loss)]
fn deduplicate_with_options(
    candidates: Vec<ScoredCandidate>,
    weights: &Weights,
    tokenizer: tokens::Tokenizer,
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
            merge_scored_candidate(&mut kept[existing], &candidate, weights, tokenizer);
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
            merge_scored_candidate(&mut kept[existing], &candidate, weights, tokenizer);
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
    tokenizer: tokens::Tokenizer,
) {
    merge_candidate_signals(&mut existing.candidate, &duplicate.candidate);
    *existing = ScoredCandidate::new_with_tokenizer(existing.candidate.clone(), weights, tokenizer);
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
    select_with_options(
        candidates,
        request,
        repository_generation,
        &Weights::default(),
        tokenizer,
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
    )
}

fn select_with_options(
    candidates: Vec<Candidate>,
    request: &ContextRequest,
    repository_generation: u64,
    weights: &Weights,
    tokenizer: tokens::Tokenizer,
) -> ContextResponse {
    let mut candidates = candidates;
    apply_request_signals(&mut candidates, request);

    let known_hashes: HashSet<String> = request.known_hashes.iter().cloned().collect();

    let mut known_omitted: Vec<ScoredCandidate> = Vec::new();
    let mut eligible: Vec<Candidate> = Vec::with_capacity(candidates.len());

    for candidate in candidates {
        if is_excluded(&candidate.path, &request.exclude_paths) {
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
    let deduped = deduplicate_with_options(ranked, weights, tokenizer);

    let budget = request.token_budget;
    let max_per_file = (budget / DIVERSITY_DIVISOR).clamp(1, 3);
    // Candidate excerpts vary from a few tokens to hundreds. A token-derived
    // fragment estimate underfilled budgets when high-value evidence happened
    // to be short. The fixed cap bounds metadata; the token budget remains the
    // authoritative content bound.
    let max_fragments = MAX_CONTEXT_FRAGMENTS;
    let (selected, mut omitted) = greedy_select(deduped, budget, max_per_file, max_fragments);

    // Build DTOs.
    let mut fragments: Vec<ContextFragment> = Vec::with_capacity(selected.len());
    let mut fragment_hashes = Vec::with_capacity(selected.len());
    let mut emitted_tokens = 0usize;

    for scored in selected {
        emitted_tokens += scored.token_count;
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
        fragment_hashes.push(scored.content_hash);
    }

    let mut omitted_dto: Vec<OmittedCandidate> = known_omitted
        .into_iter()
        .map(|scored| OmittedCandidate {
            path: scored.candidate.path,
            start_line: scored.candidate.start_line,
            end_line: scored.candidate.end_line,
            reason: "known hash".to_string(),
        })
        .collect();

    omitted_dto.extend(omitted.drain(..).map(|scored| OmittedCandidate {
        path: scored.candidate.path,
        start_line: scored.candidate.start_line,
        end_line: scored.candidate.end_line,
        reason: "budget or result limit".to_string(),
    }));

    let omitted_count = omitted_dto.len();
    omitted_dto.truncate(MAX_OMITTED_DETAILS);
    let mut warnings = Vec::new();
    if omitted_count > 0 {
        warnings.push(format!("{omitted_count} omitted"));
    }

    let task_hash = blake3::hash(request.task.as_bytes()).to_hex().to_string();
    let task_fingerprint = task_hash[..32].to_string();

    let receipt = EvidenceReceipt {
        task_fingerprint,
        fragment_hashes,
    };

    let meta = ResponseMeta {
        repository_generation,
        freshness: Freshness::Current,
        emitted_tokens,
        token_count_exact: tokenizer.is_exact(),
        next_cursor: None,
    };

    ContextResponse {
        fragments,
        receipt,
        omitted: omitted_dto,
        warnings,
        meta,
    }
}

fn apply_request_signals(candidates: &mut [Candidate], request: &ContextRequest) {
    for candidate in candidates {
        for focus_path in &request.focus_paths {
            if focus_path.is_empty() {
                continue;
            }
            if focus_matches(&candidate.path, focus_path) {
                candidate.focus_boost += 1.0;
                break;
            }
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

fn focus_matches(path: &str, pattern: &str) -> bool {
    path == pattern
        || path.contains(pattern)
        || path.starts_with(&format!("{pattern}/"))
        || path.ends_with(&format!("/{pattern}"))
}

fn is_excluded(path: &str, patterns: &[String]) -> bool {
    patterns.iter().any(|pattern| {
        if pattern.is_empty() {
            return false;
        }
        path == pattern || path.starts_with(&format!("{pattern}/"))
    })
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
            focus_paths: Vec::new(),
            focus_symbols: Vec::new(),
            exclude_paths: Vec::new(),
            known_hashes: Vec::new(),
            prior_repository_generation: None,
        }
    }

    fn request_focused(budget: usize, focus_path: &str) -> ContextRequest {
        ContextRequest {
            task: "focus path test".into(),
            token_budget: budget,
            focus_paths: vec![focus_path.into()],
            focus_symbols: Vec::new(),
            exclude_paths: Vec::new(),
            known_hashes: Vec::new(),
            prior_repository_generation: None,
        }
    }

    fn request_excluding(budget: usize, exclude: &str) -> ContextRequest {
        ContextRequest {
            task: "exclude path test".into(),
            token_budget: budget,
            focus_paths: Vec::new(),
            focus_symbols: Vec::new(),
            exclude_paths: vec![exclude.into()],
            known_hashes: Vec::new(),
            prior_repository_generation: None,
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

        assert_eq!(response.fragments.len(), MAX_CONTEXT_FRAGMENTS);
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
