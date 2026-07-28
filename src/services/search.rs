//! Lexical and structural search over a request-scoped snapshot.

use std::collections::{HashMap, HashSet};

use regex_syntax::hir::{Hir, HirKind};
use tokio_util::sync::CancellationToken;

use super::read::{StoredExcerpt, StoredExcerptRequest};
use super::receipts::{ReceiptDecision, ReceiptEvidence};
use super::validation::{
    MAX_QUERY_BYTES, PathFilter, PathMatcher, check_cancelled, make_cursor, parse_cursor,
    validate_cursor, validate_glob_patterns, validate_input,
};
use super::{ServiceCallOptions, Services, retrieval_primitive_key};
use crate::model::*;
use crate::storage::{ChunkHit, ReadSession, ReferenceHit, SymbolHit};
use crate::text::{
    anchored_line_window, byte_range_to_line_range, byte_to_line, excerpt, hash, line_starts,
};
use crate::{Error, Result};

/// Absolute regex scan candidate cap (independent of max_results multiplier).
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
    term_count: usize,
}

#[derive(Default)]
struct RegexPlanBudget {
    nodes: usize,
    terms: usize,
    term_bytes: usize,
}

impl RegexPlanBudget {
    fn visit(&mut self) -> std::result::Result<(), ()> {
        self.nodes = self.nodes.saturating_add(1);
        (self.nodes <= MAX_REGEX_PLAN_NODES).then_some(()).ok_or(())
    }

    fn add_term(&mut self, term: &str) -> std::result::Result<(), ()> {
        self.terms = self.terms.saturating_add(1);
        self.term_bytes = self.term_bytes.saturating_add(term.len());
        (self.terms <= MAX_REGEX_PLAN_TERMS && self.term_bytes <= MAX_REGEX_PLAN_TERM_BYTES)
            .then_some(())
            .ok_or(())
    }
}

impl RegexCandidateExpr {
    fn fts_query(&self) -> String {
        match self {
            Self::Term(term) => fts_quote(term),
            Self::All(expressions) => expressions
                .iter()
                .map(|expression| format!("({})", expression.fts_query()))
                .collect::<Vec<_>>()
                .join(" AND "),
            Self::Any(expressions) => expressions
                .iter()
                .map(|expression| format!("({})", expression.fts_query()))
                .collect::<Vec<_>>()
                .join(" OR "),
        }
    }
}

fn regex_candidate_plan(request: &SearchRequest) -> Option<RegexCandidatePlan> {
    // SQLite's default trigram tokenizer folds ASCII only. Rust regexes use
    // Unicode simple case folding, so a case-insensitive ASCII literal can
    // also match non-ASCII code points (for example, Kelvin sign for `k`).
    // Falling back avoids false negatives until those semantics can be
    // represented by the candidate index.
    if !request.case_sensitive {
        return None;
    }
    let hir = regex_syntax::parse(&request.query).ok()?;
    let mut budget = RegexPlanBudget::default();
    let expression = regex_candidate_expr(&hir, &mut budget).ok()??;
    Some(RegexCandidatePlan {
        expression,
        term_count: budget.terms,
    })
}

fn regex_candidate_expr(
    hir: &Hir,
    budget: &mut RegexPlanBudget,
) -> std::result::Result<Option<RegexCandidateExpr>, ()> {
    budget.visit()?;
    match hir.kind() {
        HirKind::Literal(literal) => literal_candidate_expr(&literal.0, budget),
        HirKind::Capture(capture) => regex_candidate_expr(&capture.sub, budget),
        HirKind::Repetition(repetition) if repetition.min > 0 => {
            regex_candidate_expr(&repetition.sub, budget)
        }
        HirKind::Concat(expressions) => {
            let mut plans = Vec::new();
            for expression in expressions {
                if let Some(plan) = regex_candidate_expr(expression, budget)? {
                    plans.push(plan);
                }
            }
            Ok(combine_candidate_expr(plans, true))
        }
        HirKind::Alternation(expressions) => {
            let mut plans = Vec::with_capacity(expressions.len());
            for expression in expressions {
                let Some(plan) = regex_candidate_expr(expression, budget)? else {
                    return Ok(None);
                };
                plans.push(plan);
            }
            Ok(combine_candidate_expr(plans, false))
        }
        HirKind::Empty | HirKind::Class(_) | HirKind::Look(_) | HirKind::Repetition(_) => Ok(None),
    }
}

fn literal_candidate_expr(
    literal: &[u8],
    budget: &mut RegexPlanBudget,
) -> std::result::Result<Option<RegexCandidateExpr>, ()> {
    let mut terms = Vec::new();
    for bytes in literal.split(|byte| !byte.is_ascii_alphanumeric() && *byte != b'_') {
        if bytes.len() < 3 {
            continue;
        }
        let term = std::str::from_utf8(bytes).map_err(|_| ())?.to_owned();
        budget.add_term(&term)?;
        terms.push(RegexCandidateExpr::Term(term));
    }
    Ok(combine_candidate_expr(terms, true))
}

fn combine_candidate_expr(
    mut expressions: Vec<RegexCandidateExpr>,
    all: bool,
) -> Option<RegexCandidateExpr> {
    match expressions.len() {
        0 => None,
        1 => expressions.pop(),
        _ if all => Some(RegexCandidateExpr::All(expressions)),
        _ => Some(RegexCandidateExpr::Any(expressions)),
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct DefinitionIdentity {
    path: String,
    start_line: usize,
    end_line: usize,
}

#[derive(Clone)]
struct CandidateSearchHit {
    hit: SearchHit,
    definition: Option<DefinitionIdentity>,
}

struct SearchResponseShape<'a> {
    all: &'a [CandidateSearchHit],
    request: &'a SearchRequest,
    generation: u64,
    total_candidates: usize,
    offset: usize,
    consumed: usize,
    has_more: bool,
}

fn hit_has_kind(hit: &SearchHit, kind: &str) -> bool {
    hit.match_kind == kind || hit.match_kinds.iter().any(|candidate| candidate == kind)
}

fn merge_search_hits(primary: &mut SearchHit, secondary: SearchHit) {
    primary.score = primary.score.max(secondary.score);
    for kind in secondary.match_kinds {
        if !primary.match_kinds.contains(&kind) {
            primary.match_kinds.push(kind);
        }
    }
    for reason in secondary.score_reasons {
        if !primary.score_reasons.contains(&reason) {
            primary.score_reasons.push(reason);
        }
    }
    if primary.role.is_none() {
        primary.role = secondary.role;
    }
    if primary.symbol.is_none() {
        primary.symbol = secondary.symbol;
    }
    if primary.enclosing_symbol.is_none() {
        primary.enclosing_symbol = secondary.enclosing_symbol;
    }
}

fn deduplicate_definition_channels(
    hits: Vec<CandidateSearchHit>,
    prefer_structural: bool,
) -> Vec<CandidateSearchHit> {
    let mut deduplicated: Vec<CandidateSearchHit> = Vec::with_capacity(hits.len());
    for mut candidate in hits {
        let candidate_structural = hit_has_kind(&candidate.hit, "symbol");
        let candidate_lexical =
            hit_has_kind(&candidate.hit, "text") || hit_has_kind(&candidate.hit, "regex");
        let duplicate = candidate.definition.as_ref().and_then(|identity| {
            deduplicated.iter().position(|existing| {
                existing.definition.as_ref() == Some(identity)
                    && ((candidate_structural
                        && (hit_has_kind(&existing.hit, "text")
                            || hit_has_kind(&existing.hit, "regex")))
                        || (candidate_lexical && hit_has_kind(&existing.hit, "symbol")))
            })
        });
        let Some(index) = duplicate else {
            deduplicated.push(candidate);
            continue;
        };
        if prefer_structural
            && candidate_structural
            && !hit_has_kind(&deduplicated[index].hit, "symbol")
        {
            std::mem::swap(&mut candidate, &mut deduplicated[index]);
        }
        merge_search_hits(&mut deduplicated[index].hit, candidate.hit);
    }
    deduplicated
}

fn deduplicate_exact_hits(
    hits: Vec<CandidateSearchHit>,
    prefer_structural: bool,
) -> Vec<CandidateSearchHit> {
    let mut deduplicated: Vec<CandidateSearchHit> = Vec::with_capacity(hits.len());
    let mut positions = HashMap::new();
    for mut candidate in hits {
        let key = (
            candidate.hit.path.clone(),
            candidate.hit.start_line,
            candidate.hit.end_line,
            candidate.hit.content_hash.clone(),
            candidate
                .hit
                .occurrence
                .as_ref()
                .map(|occurrence| (occurrence.start_byte, occurrence.end_byte)),
        );
        let Some(&index) = positions.get(&key) else {
            positions.insert(key, deduplicated.len());
            deduplicated.push(candidate);
            continue;
        };
        if prefer_structural
            && hit_has_kind(&candidate.hit, "symbol")
            && !hit_has_kind(&deduplicated[index].hit, "symbol")
        {
            std::mem::swap(&mut candidate, &mut deduplicated[index]);
        }
        if deduplicated[index].definition.is_none() {
            deduplicated[index].definition = candidate.definition.clone();
        }
        merge_search_hits(&mut deduplicated[index].hit, candidate.hit);
    }
    deduplicated
}

fn normalize_search_scores(hits: &mut [CandidateSearchHit]) {
    let max_score = hits
        .iter()
        .map(|candidate| candidate.hit.score)
        .filter(|score| score.is_finite())
        .fold(0.0_f64, f64::max);
    for candidate in hits {
        candidate.hit.normalized_score = if max_score > 0.0 && candidate.hit.score.is_finite() {
            (candidate.hit.score / max_score).clamp(0.0, 1.0)
        } else {
            0.0
        };
    }
}

fn coverage_count(
    all: &[CandidateSearchHit],
    returned: &[CandidateSearchHit],
    matches: impl Fn(&SearchHit) -> bool,
) -> SearchCoverageCount {
    let total = all
        .iter()
        .filter(|candidate| matches(&candidate.hit))
        .count();
    let returned = returned
        .iter()
        .filter(|candidate| matches(&candidate.hit))
        .count();
    SearchCoverageCount {
        total,
        returned,
        truncated: total.saturating_sub(returned),
    }
}

fn search_coverage(all: &[CandidateSearchHit], returned: &[CandidateSearchHit]) -> SearchCoverage {
    SearchCoverage {
        definitions: coverage_count(all, returned, |hit| hit_has_kind(hit, "symbol")),
        references: coverage_count(all, returned, |hit| hit_has_kind(hit, "reference")),
        text_matches: coverage_count(all, returned, |hit| {
            hit_has_kind(hit, "text") || hit_has_kind(hit, "regex")
        }),
    }
}

fn grouped_search_key(hit: &SearchHit) -> String {
    if let Some(symbol) = hit.symbol.as_deref() {
        return format!("symbol:{symbol}");
    }
    if let Some(symbol) = hit.enclosing_symbol.as_deref() {
        return format!("scope:{}:{symbol}", hit.path);
    }
    format!("range:{}:{}:{}", hit.path, hit.start_line, hit.end_line)
}

fn grouped_search_evidence(hit: &SearchHit) -> SearchGroupEvidence {
    SearchGroupEvidence {
        path: hit.path.clone(),
        start_line: hit.start_line,
        end_line: hit.end_line,
        excerpt: Some(hit.excerpt.clone()),
        content_hash: hit.content_hash.clone(),
        match_kinds: hit.match_kinds.clone(),
        role: hit.role,
    }
}

fn group_search_hits(hits: &[SearchHit]) -> Vec<SearchGroup> {
    let mut groups = Vec::<SearchGroup>::new();
    let mut positions = HashMap::<String, usize>::new();
    for hit in hits {
        let key = grouped_search_key(hit);
        let index = *positions.entry(key).or_insert_with(|| {
            let index = groups.len();
            groups.push(SearchGroup {
                symbol: hit.symbol.clone().or_else(|| hit.enclosing_symbol.clone()),
                definition: None,
                representative: None,
                references: Vec::new(),
                text_matches: 0,
                total_hits: 0,
            });
            index
        });
        let group = &mut groups[index];
        group.total_hits = group.total_hits.saturating_add(1);
        if hit_has_kind(hit, "text") || hit_has_kind(hit, "regex") {
            group.text_matches = group.text_matches.saturating_add(1);
        }

        if hit.role == Some(ReferenceRole::Definition) {
            if group.definition.is_none() {
                group.definition = Some(grouped_search_evidence(hit));
                group.representative = None;
            }
        } else if group.definition.is_none() && group.representative.is_none() {
            group.representative = Some(grouped_search_evidence(hit));
        }

        if hit.role == Some(ReferenceRole::Reference) || hit_has_kind(hit, "reference") {
            if let Some(reference) = group
                .references
                .iter_mut()
                .find(|reference| reference.path == hit.path)
            {
                reference.count = reference.count.saturating_add(1);
                reference.start_line = reference.start_line.min(hit.start_line);
                reference.end_line = reference.end_line.max(hit.end_line);
                if let Some(role) = hit.role
                    && !reference.roles.contains(&role)
                {
                    reference.roles.push(role);
                }
            } else {
                group.references.push(SearchReferenceGroup {
                    path: hit.path.clone(),
                    count: 1,
                    start_line: hit.start_line,
                    end_line: hit.end_line,
                    roles: hit.role.into_iter().collect(),
                });
            }
        }
    }
    groups
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum OccurrenceGroupKey {
    Path(String),
    Excerpt {
        path: String,
        start_line: usize,
        end_line: usize,
        content_hash: String,
    },
}

fn occurrence_group_key(hit: &SearchHit, coordinates_only: bool) -> OccurrenceGroupKey {
    if coordinates_only {
        OccurrenceGroupKey::Path(hit.path.clone())
    } else {
        OccurrenceGroupKey::Excerpt {
            path: hit.path.clone(),
            start_line: hit.start_line,
            end_line: hit.end_line,
            content_hash: hit.content_hash.clone(),
        }
    }
}

fn group_occurrence_hits(
    hits: &[SearchHit],
    coordinates_only: bool,
) -> Result<Vec<SearchOccurrenceGroup>> {
    let mut groups = Vec::<SearchOccurrenceGroup>::new();
    let mut positions = HashMap::<OccurrenceGroupKey, usize>::new();
    for hit in hits {
        let occurrence = hit.occurrence.as_ref().ok_or_else(|| {
            Error::InternalFailure(
                "exhaustive occurrence response omitted exact coordinates".into(),
            )
        })?;
        let key = occurrence_group_key(hit, coordinates_only);
        let index = *positions.entry(key).or_insert_with(|| {
            let index = groups.len();
            groups.push(SearchOccurrenceGroup {
                path: hit.path.clone(),
                start_line: if coordinates_only {
                    occurrence.start_line
                } else {
                    hit.start_line
                },
                end_line: if coordinates_only {
                    occurrence.end_line
                } else {
                    hit.end_line
                },
                excerpt: (!coordinates_only).then(|| hit.excerpt.clone()),
                content_hash: (!coordinates_only).then(|| hit.content_hash.clone()),
                occurrences: Vec::new(),
            });
            index
        });
        let group = &mut groups[index];
        if coordinates_only {
            group.start_line = group.start_line.min(occurrence.start_line);
            group.end_line = group.end_line.max(occurrence.end_line);
        }
        group.occurrences.push(SearchOccurrenceCoordinate {
            line: occurrence.start_line,
            end_line: (occurrence.end_line != occurrence.start_line).then_some(occurrence.end_line),
            start_column: occurrence.start_column,
            end_column: occurrence.end_column,
        });
    }
    Ok(groups)
}

fn select_search_page(
    hits: &[CandidateSearchHit],
    offset: usize,
    limit: usize,
    token_limit: usize,
    output_shape: SearchOutputShape,
    tokenizer: &crate::tokens::Tokenizer,
    cancellation: &CancellationToken,
) -> Result<(Vec<CandidateSearchHit>, usize, usize)> {
    let mut emitted_tokens = 0usize;
    let mut selected = Vec::new();
    let mut consumed = 0usize;
    let mut charged_occurrence_groups = HashSet::new();
    for candidate in hits.iter().skip(offset).take(limit).cloned() {
        check_cancelled(cancellation)?;
        consumed += 1;
        let group_key = match output_shape {
            SearchOutputShape::OccurrenceGroups {
                coordinates_only: false,
            } => Some(occurrence_group_key(&candidate.hit, false)),
            SearchOutputShape::Full
            | SearchOutputShape::OccurrenceGroups {
                coordinates_only: true,
            } => None,
        };
        let count = match output_shape {
            SearchOutputShape::Full => tokenizer.count(&candidate.hit.excerpt),
            SearchOutputShape::OccurrenceGroups {
                coordinates_only: true,
            } => 0,
            SearchOutputShape::OccurrenceGroups {
                coordinates_only: false,
            } if group_key
                .as_ref()
                .is_some_and(|key| charged_occurrence_groups.contains(key)) =>
            {
                0
            }
            SearchOutputShape::OccurrenceGroups {
                coordinates_only: false,
            } => tokenizer.count(&candidate.hit.excerpt),
        };
        if emitted_tokens.saturating_add(count) > token_limit {
            continue;
        }
        emitted_tokens += count;
        if let Some(key) = group_key {
            charged_occurrence_groups.insert(key);
        }
        selected.push(candidate);
    }
    Ok((selected, consumed, emitted_tokens))
}

fn selected_search_source_tokens(
    selected: &[CandidateSearchHit],
    output_shape: SearchOutputShape,
    tokenizer: &crate::tokens::Tokenizer,
) -> usize {
    match output_shape {
        SearchOutputShape::Full => selected
            .iter()
            .map(|candidate| tokenizer.count(&candidate.hit.excerpt))
            .sum(),
        SearchOutputShape::OccurrenceGroups {
            coordinates_only: true,
        } => 0,
        SearchOutputShape::OccurrenceGroups {
            coordinates_only: false,
        } => {
            let mut seen = HashSet::new();
            selected
                .iter()
                .filter(|candidate| seen.insert(occurrence_group_key(&candidate.hit, false)))
                .map(|candidate| tokenizer.count(&candidate.hit.excerpt))
                .sum()
        }
    }
}

fn collect_filtered_hits<T>(
    request: &SearchRequest,
    max_candidates: usize,
    cancellation: &CancellationToken,
    mut fetch_page: impl FnMut(usize, usize) -> Result<Vec<T>>,
    path: impl Fn(&T) -> &str,
) -> Result<Vec<T>> {
    let path_filter = PathFilter::new(&request.include_paths, &request.exclude_paths)?;
    let mut selected = Vec::new();
    let mut offset = 0usize;
    while selected.len() < max_candidates && offset < MAX_FILTER_SCAN_ROWS {
        check_cancelled(cancellation)?;
        let page_limit = FILTER_SCAN_PAGE_SIZE.min(MAX_FILTER_SCAN_ROWS - offset);
        let page = fetch_page(offset, page_limit)?;
        let page_len = page.len();
        for hit in page {
            check_cancelled(cancellation)?;
            if path_filter.allows(path(&hit)) {
                selected.push(hit);
                if selected.len() == max_candidates {
                    break;
                }
            }
        }
        offset = offset.saturating_add(page_len);
        if page_len < page_limit {
            break;
        }
    }
    Ok(selected)
}

pub(super) fn chunk_search_hit(
    hit: &ChunkHit,
    query: &str,
    case_sensitive: bool,
    context: usize,
    compiled_matcher: Option<&regex::Regex>,
    regex_match: bool,
) -> Result<Option<SearchHit>> {
    let byte_range = if let Some(regex) = compiled_matcher {
        regex
            .find(&hit.content)
            .map(|matched| (matched.start(), matched.end()))
    } else if case_sensitive {
        hit.content
            .find(query)
            .map(|start| (start, start + query.len()))
    } else {
        regex::RegexBuilder::new(&regex::escape(query))
            .case_insensitive(true)
            .build()?
            .find(&hit.content)
            .map(|matched| (matched.start(), matched.end()))
    };
    let Some((start, end)) = byte_range else {
        return Ok(None);
    };
    let starts = line_starts(&hit.content);
    Ok(Some(chunk_search_hit_for_range(
        hit,
        start,
        end,
        context,
        regex_match,
        false,
        &starts,
    )))
}

fn chunk_search_hits(
    hit: &ChunkHit,
    query: &str,
    case_sensitive: bool,
    context: usize,
    compiled_matcher: Option<&regex::Regex>,
    regex_match: bool,
    max_hits: usize,
) -> Result<Vec<SearchHit>> {
    let ranges: Box<dyn Iterator<Item = (usize, usize)> + '_> =
        if let Some(regex) = compiled_matcher {
            Box::new(
                regex
                    .find_iter(&hit.content)
                    .map(|matched| (matched.start(), matched.end())),
            )
        } else if case_sensitive {
            Box::new(
                hit.content
                    .match_indices(query)
                    .map(|(start, matched)| (start, start + matched.len())),
            )
        } else {
            Box::new(std::iter::empty())
        };
    let starts = line_starts(&hit.content);
    let mut hits = Vec::new();
    for (start, end) in ranges {
        if hits.len() == max_hits {
            return Err(Error::LimitExceeded);
        }
        hits.push(chunk_search_hit_for_range(
            hit,
            start,
            end,
            context,
            regex_match,
            true,
            &starts,
        ));
    }
    Ok(hits)
}

pub(super) fn chunk_search_hit_for_range(
    hit: &ChunkHit,
    start: usize,
    end: usize,
    context: usize,
    regex_match: bool,
    include_occurrence: bool,
    line_starts: &[usize],
) -> SearchHit {
    let text_len = hit.content.len();
    let local_start = byte_to_line(line_starts, text_len, start);
    let local_end = if end == 0 || end == start {
        local_start
    } else {
        byte_to_line(line_starts, text_len, end.saturating_sub(1))
    };
    let available_lines = line_starts.len().max(1);
    let desired_start = local_start.saturating_sub(context).max(1);
    let desired_end = local_end.saturating_add(context).min(available_lines);
    let (excerpt_start, excerpt_end) =
        anchored_line_window(desired_start, desired_end, local_start, local_end, 20);
    let excerpt = excerpt(&hit.content, excerpt_start, excerpt_end);
    SearchHit {
        path: hit.path.clone(),
        start_line: hit.start_line + excerpt_start - 1,
        end_line: hit.start_line + excerpt_end - 1,
        content_hash: hash(&excerpt),
        excerpt,
        match_kind: if regex_match {
            "regex".into()
        } else {
            "text".into()
        },
        match_kinds: vec![if regex_match {
            "regex".into()
        } else {
            "text".into()
        }],
        role: None,
        symbol: None,
        enclosing_symbol: None,
        occurrence: include_occurrence.then_some(SearchOccurrence {
            start_line: hit.start_line + local_start - 1,
            end_line: hit.start_line + local_end - 1,
            start_column: start.saturating_sub(line_starts[local_start.saturating_sub(1)]),
            end_column: end.saturating_sub(line_starts[local_end.saturating_sub(1)]),
            start_byte: hit.start_byte + start,
            end_byte: hit.start_byte + end,
        }),
        score: 3.0 + (-hit.score).max(0.0) * 1_000_000.0,
        normalized_score: 0.0,
        score_reasons: vec![if regex_match {
            "regex match".into()
        } else {
            "text match".into()
        }],
    }
}

pub(super) fn matching_line(
    hit: &ChunkHit,
    query: &str,
    case_sensitive: bool,
    compiled_regex: Option<&regex::Regex>,
) -> Option<usize> {
    if let Some(regex) = compiled_regex {
        return regex.find(&hit.content).map(|matched| {
            let (local_start, _) =
                byte_range_to_line_range(&hit.content, matched.start(), matched.end());
            hit.start_line + local_start - 1
        });
    }
    if !case_sensitive {
        return None;
    }
    hit.content
        .lines()
        .position(|line| line.contains(query))
        .map(|offset| hit.start_line + offset)
}

fn apply_focus(hits: &mut [CandidateSearchHit], focus_paths: &[String]) -> Result<()> {
    let focus_paths = PathMatcher::new(focus_paths)?;
    for candidate in hits {
        if focus_paths.is_match(&candidate.hit.path) {
            candidate.hit.score += 2.0;
            candidate.hit.score_reasons.push("focus path".into());
        }
    }
    Ok(())
}

pub(super) fn fts_quote(value: &str) -> String {
    format!("\"{}\"", value.replace('"', "\"\""))
}

fn compile_regex(request: &SearchRequest) -> Result<regex::Regex> {
    Ok(regex::RegexBuilder::new(&request.query)
        .case_insensitive(!request.case_sensitive)
        .size_limit(1 << 20)
        .dfa_size_limit(1 << 20)
        .build()?)
}

fn compile_occurrence_literal_regex(request: &SearchRequest) -> Result<regex::Regex> {
    Ok(regex::RegexBuilder::new(&regex::escape(&request.query))
        .case_insensitive(!request.case_sensitive)
        .size_limit(1 << 20)
        .dfa_size_limit(1 << 20)
        .build()?)
}

/// Compile a case-insensitive literal matcher for non-regex search modes.
///
/// In Auto/Text/Identifier modes the query is a literal string, not a regex
/// pattern. Compile it once per request so every lexical hit reuses the same
/// matcher. Case-sensitive search uses `str::find` without compilation.
pub(super) fn compile_literal_regex(
    query: &str,
    case_sensitive: bool,
) -> Result<Option<regex::Regex>> {
    if case_sensitive {
        return Ok(None);
    }
    Ok(Some(
        regex::RegexBuilder::new(&regex::escape(query))
            .case_insensitive(true)
            .size_limit(1 << 20)
            .dfa_size_limit(1 << 20)
            .build()?,
    ))
}

fn validate_search_input(request: &SearchRequest) -> Result<()> {
    if request.query.trim().is_empty() {
        return Err(Error::InvalidInput {
            field: "search query",
            reason: "must not be empty",
        });
    }
    validate_input(&request.query, "search query", MAX_QUERY_BYTES)?;
    validate_glob_patterns(&request.include_paths)?;
    validate_glob_patterns(&request.exclude_paths)?;
    validate_glob_patterns(&request.focus_paths)?;
    validate_cursor(request.cursor.as_deref())?;
    if request.all_occurrences && !matches!(request.mode, SearchMode::Text | SearchMode::Regex) {
        return Err(Error::InvalidInput {
            field: "all occurrences",
            reason: "requires text or regex mode",
        });
    }
    if request.prefer_structural
        && !matches!(request.mode, SearchMode::Auto | SearchMode::Identifier)
    {
        return Err(Error::InvalidInput {
            field: "prefer structural",
            reason: "requires auto or identifier mode",
        });
    }
    if matches!(request.mode, SearchMode::Regex) {
        compile_regex(request)?;
    } else {
        compile_literal_regex(&request.query, request.case_sensitive)?;
    }
    Ok(())
}

fn validate_occurrence_group_input(request: &SearchRequest) -> Result<()> {
    validate_search_input(request)?;
    if !request.all_occurrences {
        return Err(Error::InvalidInput {
            field: "occurrence projection",
            reason: "requires all_occurrences=true",
        });
    }
    Ok(())
}

impl Services {
    fn ensure_search_page_fits(
        &self,
        selected: &mut [CandidateSearchHit],
        shape: SearchResponseShape<'_>,
        options: ServiceCallOptions,
    ) -> Result<()> {
        let provisional = |selected: &[CandidateSearchHit]| SearchResponse {
            hits: selected
                .iter()
                .map(|candidate| candidate.hit.clone())
                .collect(),
            coverage: search_coverage(shape.all, selected),
            occurrences_returned: selected.len(),
            occurrences_total: shape
                .request
                .all_occurrences
                .then_some(shape.total_candidates),
            meta: self.meta(
                shape.generation,
                selected
                    .iter()
                    .map(|candidate| self.config.tokenizer.count(&candidate.hit.excerpt))
                    .sum(),
                shape
                    .has_more
                    .then(|| make_cursor(shape.generation, shape.offset + shape.consumed)),
            ),
        };
        let mut sized = provisional(selected);
        if self.response_fits_with_receipt_reserve(&sized, selected.len(), options)? {
            return Ok(());
        }
        for candidate in selected.iter_mut() {
            candidate.hit.score_reasons.clear();
        }
        sized = provisional(selected);
        if self.response_fits_with_receipt_reserve(&sized, selected.len(), options)? {
            return Ok(());
        }
        Err(Error::RequestLimitExceeded {
            field: "max_response_tokens",
            requested: self
                .finalized_response_tokens_with_receipt_reserve(&sized, selected.len())?,
            limit: options
                .max_response_tokens()
                .expect("fitting only runs with a response limit"),
        })
    }

    /// Search indexed lexical and structural evidence.
    pub async fn search(&self, request: SearchRequest) -> Result<SearchResponse> {
        self.search_with_options(request, ServiceCallOptions::new())
            .await
    }

    /// Search under explicit serialized-response controls.
    pub async fn search_with_options(
        &self,
        request: SearchRequest,
        options: ServiceCallOptions,
    ) -> Result<SearchResponse> {
        self.search_cancellable_with_options(request, options, CancellationToken::new())
            .await
    }

    /// Search after applying a cancellable index consistency boundary.
    pub async fn search_with_consistency_cancellable(
        &self,
        request: SearchRequest,
        consistency: IndexConsistency,
        cancellation: CancellationToken,
    ) -> Result<SearchResponse> {
        self.search_with_options_consistency_cancellable(
            request,
            consistency,
            ServiceCallOptions::new(),
            cancellation,
        )
        .await
    }

    /// Search under consistency and serialized-response controls.
    pub async fn search_with_options_consistency_cancellable(
        &self,
        request: SearchRequest,
        consistency: IndexConsistency,
        options: ServiceCallOptions,
        cancellation: CancellationToken,
    ) -> Result<SearchResponse> {
        let operation = TokenAccountingOperation::Search;
        self.observe_service_result(operation, self.validate_call_options(options))?;
        self.observe_service_result(operation, validate_search_input(&request))?;
        self.observe_service_result(operation, self.result_limit(request.max_results))?;
        self.observe_service_result(
            operation,
            self.token_limit(request.max_tokens, self.config.default_read_tokens),
        )?;
        self.observe_service_result(operation, self.context_line_limit(request.context_lines))?;
        let consistency_result = self
            .apply_consistency(consistency, cancellation.clone())
            .await;
        self.observe_service_result(operation, consistency_result)?;
        self.search_cancellable_with_options(request, options, cancellation)
            .await
    }

    pub async fn search_cancellable(
        &self,
        request: SearchRequest,
        cancellation: CancellationToken,
    ) -> Result<SearchResponse> {
        self.search_cancellable_with_options(request, ServiceCallOptions::new(), cancellation)
            .await
    }

    async fn search_cancellable_with_options(
        &self,
        request: SearchRequest,
        options: ServiceCallOptions,
        cancellation: CancellationToken,
    ) -> Result<SearchResponse> {
        let operation = TokenAccountingOperation::Search;
        self.observe_service_result(operation, self.validate_call_options(options))?;
        let this = self.clone();
        let result = self
            .blocking_executor
            .run(cancellation, move |cancellation| {
                this.search_sync(
                    request,
                    cancellation,
                    RegexPlanning::Enabled,
                    SearchDiagnostics::Omit,
                    SearchExecutionOptions {
                        output_shape: SearchOutputShape::Full,
                        response_options: options,
                        record_savings: true,
                    },
                )
                .map(|evaluation| evaluation.response)
            })
            .await;
        self.observe_service_result(operation, result)
    }

    /// Search with hits grouped by matched symbol or enclosing scope.
    pub async fn search_grouped(&self, request: SearchRequest) -> Result<SearchGroupedResponse> {
        self.search_grouped_with_options(request, ServiceCallOptions::new())
            .await
    }

    /// Search with grouped output under an exact serialized-response bound.
    pub async fn search_grouped_with_options(
        &self,
        request: SearchRequest,
        options: ServiceCallOptions,
    ) -> Result<SearchGroupedResponse> {
        self.search_grouped_cancellable_with_options(request, options, CancellationToken::new())
            .await
    }

    /// Search with grouped output after applying the requested consistency boundary.
    pub async fn search_grouped_with_options_consistency_cancellable(
        &self,
        request: SearchRequest,
        consistency: IndexConsistency,
        options: ServiceCallOptions,
        cancellation: CancellationToken,
    ) -> Result<SearchGroupedResponse> {
        let operation = TokenAccountingOperation::Search;
        self.observe_service_result(operation, self.validate_call_options(options))?;
        self.observe_service_result(operation, validate_search_input(&request))?;
        self.observe_service_result(operation, self.result_limit(request.max_results))?;
        self.observe_service_result(
            operation,
            self.token_limit(request.max_tokens, self.config.default_read_tokens),
        )?;
        self.observe_service_result(operation, self.context_line_limit(request.context_lines))?;
        let consistency_result = self
            .apply_consistency(consistency, cancellation.clone())
            .await;
        self.observe_service_result(operation, consistency_result)?;
        self.search_grouped_cancellable_with_options(request, options, cancellation)
            .await
    }

    async fn search_grouped_cancellable_with_options(
        &self,
        request: SearchRequest,
        options: ServiceCallOptions,
        cancellation: CancellationToken,
    ) -> Result<SearchGroupedResponse> {
        let operation = TokenAccountingOperation::Search;
        self.observe_service_result(operation, self.validate_call_options(options))?;
        let this = self.clone();
        let result = self
            .blocking_executor
            .run(cancellation, move |cancellation| {
                let response = this
                    .search_sync(
                        request,
                        cancellation,
                        RegexPlanning::Enabled,
                        SearchDiagnostics::Omit,
                        SearchExecutionOptions {
                            output_shape: SearchOutputShape::Full,
                            response_options: ServiceCallOptions::new(),
                            record_savings: false,
                        },
                    )?
                    .response;
                let hits_returned = response.hits.len();
                let groups = group_search_hits(&response.hits);
                let source_tokens = groups
                    .iter()
                    .filter_map(|group| group.definition.as_ref().or(group.representative.as_ref()))
                    .filter_map(|evidence| evidence.excerpt.as_deref())
                    .map(|excerpt| this.config.tokenizer.count(excerpt))
                    .sum();
                let mut meta = response.meta;
                meta.source_tokens = source_tokens;
                meta.emitted_tokens = source_tokens;
                let mut compact = SearchGroupedResponse {
                    groups_returned: groups.len(),
                    groups,
                    coverage: response.coverage,
                    hits_returned,
                    occurrences_total: response.occurrences_total,
                    meta,
                };
                this.finalize_bounded_response(&mut compact, options)?;
                this.record_token_savings(TokenAccountingOperation::Search, None, &compact.meta);
                Ok(compact)
            })
            .await;
        self.observe_service_result(operation, result)
    }

    /// Search every lexical occurrence while sharing repeated excerpts.
    pub async fn search_occurrences(
        &self,
        request: SearchRequest,
        coordinates_only: bool,
    ) -> Result<SearchOccurrencesResponse> {
        self.search_occurrences_with_options(request, coordinates_only, ServiceCallOptions::new())
            .await
    }

    /// Search every lexical occurrence under an exact serialized-response bound.
    pub async fn search_occurrences_with_options(
        &self,
        request: SearchRequest,
        coordinates_only: bool,
        options: ServiceCallOptions,
    ) -> Result<SearchOccurrencesResponse> {
        self.search_occurrences_cancellable_with_options(
            request,
            coordinates_only,
            options,
            CancellationToken::new(),
        )
        .await
    }

    /// Search grouped occurrences after applying the requested consistency boundary.
    pub async fn search_occurrences_with_options_consistency_cancellable(
        &self,
        request: SearchRequest,
        coordinates_only: bool,
        consistency: IndexConsistency,
        options: ServiceCallOptions,
        cancellation: CancellationToken,
    ) -> Result<SearchOccurrencesResponse> {
        let operation = TokenAccountingOperation::Search;
        self.observe_service_result(operation, self.validate_call_options(options))?;
        self.observe_service_result(operation, validate_occurrence_group_input(&request))?;
        self.observe_service_result(operation, self.result_limit(request.max_results))?;
        self.observe_service_result(
            operation,
            self.token_limit(request.max_tokens, self.config.default_read_tokens),
        )?;
        self.observe_service_result(operation, self.context_line_limit(request.context_lines))?;
        let consistency_result = self
            .apply_consistency(consistency, cancellation.clone())
            .await;
        self.observe_service_result(operation, consistency_result)?;
        self.search_occurrences_cancellable_with_options(
            request,
            coordinates_only,
            options,
            cancellation,
        )
        .await
    }

    async fn search_occurrences_cancellable_with_options(
        &self,
        request: SearchRequest,
        coordinates_only: bool,
        options: ServiceCallOptions,
        cancellation: CancellationToken,
    ) -> Result<SearchOccurrencesResponse> {
        let operation = TokenAccountingOperation::Search;
        self.observe_service_result(operation, self.validate_call_options(options))?;
        self.observe_service_result(operation, validate_occurrence_group_input(&request))?;
        let this = self.clone();
        let result = self
            .blocking_executor
            .run(cancellation, move |cancellation| {
                let response = this
                    .search_sync(
                        request,
                        cancellation,
                        RegexPlanning::Enabled,
                        SearchDiagnostics::Omit,
                        SearchExecutionOptions {
                            output_shape: SearchOutputShape::OccurrenceGroups { coordinates_only },
                            response_options: ServiceCallOptions::new(),
                            record_savings: false,
                        },
                    )?
                    .response;
                let occurrences_total = response.occurrences_total.ok_or_else(|| {
                    Error::InternalFailure(
                        "grouped occurrence search omitted its exact total".into(),
                    )
                })?;
                let groups = group_occurrence_hits(&response.hits, coordinates_only)?;
                let mut compact = SearchOccurrencesResponse {
                    groups_returned: groups.len(),
                    groups,
                    occurrences_returned: response.occurrences_returned,
                    occurrences_total,
                    coordinates_only,
                    coverage: response.coverage,
                    meta: response.meta,
                };
                this.finalize_bounded_response(&mut compact, options)?;
                this.record_token_savings(TokenAccountingOperation::Search, None, &compact.meta);
                Ok(compact)
            })
            .await;
        self.observe_service_result(operation, result)
    }

    /// Search and expose deterministic candidate-phase counts for evaluation.
    ///
    /// Production adapters should use [`Self::search`]. This method does not
    /// alter the normal response or MCP schemas.
    pub async fn search_evaluation(&self, request: SearchRequest) -> Result<SearchEvaluation> {
        let this = self.clone();
        self.blocking_executor
            .run(CancellationToken::new(), move |cancellation| {
                this.search_sync(
                    request,
                    cancellation,
                    RegexPlanning::Enabled,
                    SearchDiagnostics::Collect,
                    SearchExecutionOptions {
                        output_shape: SearchOutputShape::Full,
                        response_options: ServiceCallOptions::new(),
                        record_savings: true,
                    },
                )
            })
            .await
    }

    /// Search with regex candidate planning disabled for differential evaluation.
    ///
    /// This API is not exposed through CLI or MCP adapters. It retains the
    /// bounded legacy scan so tests and benchmarks can prove optimized parity.
    pub async fn search_full_scan_evaluation(
        &self,
        request: SearchRequest,
    ) -> Result<SearchEvaluation> {
        let this = self.clone();
        self.blocking_executor
            .run(CancellationToken::new(), move |cancellation| {
                this.search_sync(
                    request,
                    cancellation,
                    RegexPlanning::Disabled,
                    SearchDiagnostics::Collect,
                    SearchExecutionOptions {
                        output_shape: SearchOutputShape::Full,
                        response_options: ServiceCallOptions::new(),
                        record_savings: true,
                    },
                )
            })
            .await
    }

    fn search_sync(
        &self,
        request: SearchRequest,
        cancellation: &CancellationToken,
        regex_planning: RegexPlanning,
        diagnostics: SearchDiagnostics,
        execution: SearchExecutionOptions,
    ) -> Result<SearchEvaluation> {
        check_cancelled(cancellation)?;
        validate_search_input(&request)?;
        let regex = matches!(request.mode, SearchMode::Regex)
            .then(|| compile_regex(&request))
            .transpose()?;
        let literal_regex = if !matches!(request.mode, SearchMode::Regex) {
            compile_literal_regex(&request.query, request.case_sensitive)?
        } else {
            None
        };
        let occurrence_literal_regex = (request.all_occurrences
            && matches!(request.mode, SearchMode::Text))
        .then(|| compile_occurrence_literal_regex(&request))
        .transpose()?;
        let limit = self.result_limit(request.max_results)?;
        let token_limit = self.token_limit(request.max_tokens, self.config.default_read_tokens)?;
        let context_lines = self.context_line_limit(request.context_lines)?;
        let search_result = self.consistent(|session, generation| {
            let offset = parse_cursor(request.cursor.as_deref(), generation)?;
            let mut hits = Vec::new();
            let mut phases = SearchPhaseCounters::default();
            let mut primitive_keys = Vec::new();
            if matches!(
                request.mode,
                SearchMode::Auto | SearchMode::Identifier | SearchMode::Symbol
            ) {
                let symbol_hits = collect_filtered_hits(
                    &request,
                    limit.saturating_mul(4),
                    cancellation,
                    |offset, page_limit| {
                        session.search_symbols_page(
                            &request.query,
                            request.case_sensitive,
                            page_limit,
                            offset,
                        )
                    },
                    |hit: &SymbolHit| &hit.path,
                )?;
                let excerpt_requests = symbol_hits
                    .iter()
                    .map(|hit| StoredExcerptRequest {
                        file_id: hit.symbol.file_id,
                        desired_start_line: hit
                            .symbol
                            .start_line
                            .saturating_sub(context_lines)
                            .max(1),
                        desired_end_line: hit.symbol.end_line.saturating_add(context_lines),
                        required_start_line: hit.symbol.start_line,
                        required_end_line: hit.symbol.start_line,
                        max_lines: 30,
                    })
                    .collect::<Vec<_>>();
                for (hit, excerpt) in symbol_hits
                    .into_iter()
                    .zip(self.stored_excerpts(session, &excerpt_requests)?)
                {
                    if let Some(excerpt) = excerpt {
                        let definition = DefinitionIdentity {
                            path: hit.path.clone(),
                            start_line: hit.symbol.start_line,
                            end_line: hit.symbol.end_line,
                        };
                        hits.push(CandidateSearchHit {
                            hit: self.symbol_search_hit(hit, &request.query, excerpt),
                            definition: Some(definition),
                        });
                    }
                }
            }
            if matches!(
                request.mode,
                SearchMode::Auto | SearchMode::Identifier | SearchMode::Reference
            ) {
                let reference_hits = collect_filtered_hits(
                    &request,
                    limit.saturating_mul(4),
                    cancellation,
                    |offset, page_limit| {
                        session.search_references_page(
                            &request.query,
                            request.case_sensitive,
                            page_limit,
                            offset,
                        )
                    },
                    |hit: &ReferenceHit| &hit.path,
                )?;
                let excerpt_requests = reference_hits
                    .iter()
                    .map(|hit| StoredExcerptRequest {
                        file_id: hit.reference.file_id,
                        desired_start_line: hit
                            .reference
                            .start_line
                            .saturating_sub(context_lines)
                            .max(1),
                        desired_end_line: hit.reference.end_line.saturating_add(context_lines),
                        required_start_line: hit.reference.start_line,
                        required_end_line: hit.reference.end_line,
                        max_lines: 12,
                    })
                    .collect::<Vec<_>>();
                for (hit, excerpt) in reference_hits
                    .into_iter()
                    .zip(self.stored_excerpts(session, &excerpt_requests)?)
                {
                    if let Some(excerpt) = excerpt {
                        hits.push(CandidateSearchHit {
                            hit: self.reference_search_hit(hit, &request.query, excerpt),
                            definition: None,
                        });
                    }
                }
            }

            let lexical = match request.mode {
                SearchMode::Regex => {
                    let scan = self.regex_hits(
                        session,
                        &request,
                        regex.as_ref().expect("regex mode compiles a pattern"),
                        (!request.all_occurrences).then_some(limit.saturating_mul(20)),
                        cancellation,
                        regex_planning,
                    )?;
                    phases = scan.phases;
                    let primitive_kind = match phases.regex_candidate_strategy {
                        RegexCandidateStrategy::Trigram => "regex_trigram_candidates",
                        RegexCandidateStrategy::FullScan => "regex_full_scan",
                    };
                    if diagnostics == SearchDiagnostics::Collect {
                        primitive_keys.push(retrieval_primitive_key(
                            generation,
                            primitive_kind,
                            &format!(
                                "case_sensitive:{}:include:{:?}:exclude:{:?}:query:{}",
                                request.case_sensitive,
                                request.include_paths,
                                request.exclude_paths,
                                request.query
                            ),
                        ));
                    }
                    scan.hits
                }
                SearchMode::Text if request.all_occurrences => {
                    let scan = self.regex_hits(
                        session,
                        &request,
                        occurrence_literal_regex
                            .as_ref()
                            .expect("exhaustive text mode compiles a literal pattern"),
                        None,
                        cancellation,
                        RegexPlanning::Disabled,
                    )?;
                    phases = scan.phases;
                    scan.hits
                }
                SearchMode::Text | SearchMode::Auto | SearchMode::Identifier
                    if request.query.chars().count() < 3 =>
                {
                    // Word FTS cannot match substrings shorter than three
                    // characters; reuse the literal regex lexical path so
                    // Identifier stays aligned with Text/Auto short queries.
                    let short_literal_regex = compile_occurrence_literal_regex(&request)?;
                    let scan = self.regex_hits(
                        session,
                        &request,
                        &short_literal_regex,
                        Some(limit.saturating_mul(20)),
                        cancellation,
                        RegexPlanning::Disabled,
                    )?;
                    phases = scan.phases;
                    scan.hits
                }
                SearchMode::Text | SearchMode::Auto | SearchMode::Identifier => {
                    let fetch_page = |offset, page_limit| {
                        if matches!(request.mode, SearchMode::Identifier) {
                            session.search_word_page(&fts_quote(&request.query), page_limit, offset)
                        } else {
                            session.search_trigram_page(&request.query, page_limit, offset)
                        }
                    };
                    collect_filtered_hits(
                        &request,
                        limit.saturating_mul(8),
                        cancellation,
                        fetch_page,
                        |hit: &ChunkHit| &hit.path,
                    )?
                }
                SearchMode::Symbol | SearchMode::Reference => Vec::new(),
            };
            let mut lexical_hits = Vec::new();
            for hit in lexical {
                check_cancelled(cancellation)?;
                let chunk_hits = if request.all_occurrences {
                    chunk_search_hits(
                        &hit,
                        &request.query,
                        request.case_sensitive,
                        context_lines,
                        regex
                            .as_ref()
                            .or(occurrence_literal_regex.as_ref())
                            .or(literal_regex.as_ref()),
                        matches!(request.mode, SearchMode::Regex),
                        MAX_EXHAUSTIVE_OCCURRENCES.saturating_sub(lexical_hits.len()),
                    )?
                } else {
                    chunk_search_hit(
                        &hit,
                        &request.query,
                        request.case_sensitive,
                        context_lines,
                        regex.as_ref().or(literal_regex.as_ref()),
                        matches!(request.mode, SearchMode::Regex),
                    )?
                    .into_iter()
                    .collect()
                };
                for search_hit in chunk_hits {
                    if request.all_occurrences && lexical_hits.len() == MAX_EXHAUSTIVE_OCCURRENCES {
                        return Err(Error::LimitExceeded);
                    }
                    let matched_line = search_hit
                        .occurrence
                        .as_ref()
                        .map(|occurrence| occurrence.start_line)
                        .or_else(|| {
                            matching_line(
                                &hit,
                                &request.query,
                                request.case_sensitive,
                                regex.as_ref().or(literal_regex.as_ref()),
                            )
                        })
                        .unwrap_or(search_hit.start_line);
                    lexical_hits.push((hit.file_id, search_hit, matched_line));
                }
            }
            let lexical_locations = lexical_hits
                .iter()
                .map(|(file_id, _, matched_line)| (*file_id, *matched_line))
                .collect::<Vec<_>>();
            let mut enclosing = Vec::with_capacity(lexical_locations.len());
            for locations in lexical_locations.chunks(512) {
                check_cancelled(cancellation)?;
                enclosing.extend(session.find_enclosing_symbols_batch(locations)?);
            }
            for ((_, mut hit, _), symbol) in lexical_hits.into_iter().zip(enclosing) {
                let definition = symbol.map(|symbol| {
                    hit.enclosing_symbol = Some(symbol.name);
                    DefinitionIdentity {
                        path: hit.path.clone(),
                        start_line: symbol.start_line,
                        end_line: symbol.end_line,
                    }
                });
                hits.push(CandidateSearchHit { hit, definition });
            }

            apply_focus(&mut hits, &request.focus_paths)?;
            hits.sort_by(|left, right| {
                right
                    .hit
                    .score
                    .total_cmp(&left.hit.score)
                    .then_with(|| left.hit.path.cmp(&right.hit.path))
                    .then_with(|| left.hit.start_line.cmp(&right.hit.start_line))
                    .then_with(|| {
                        left.hit
                            .occurrence
                            .as_ref()
                            .map(|occurrence| occurrence.start_byte)
                            .cmp(
                                &right
                                    .hit
                                    .occurrence
                                    .as_ref()
                                    .map(|occurrence| occurrence.start_byte),
                            )
                    })
            });
            hits = deduplicate_exact_hits(hits, request.prefer_structural);
            if matches!(request.mode, SearchMode::Auto | SearchMode::Identifier) {
                hits = deduplicate_definition_channels(hits, request.prefer_structural);
            }
            normalize_search_scores(&mut hits);

            let total_candidates = hits.len();
            let (mut selected, consumed, _) = select_search_page(
                &hits,
                offset,
                limit,
                token_limit,
                execution.output_shape,
                &self.config.tokenizer,
                cancellation,
            )?;
            let has_more = offset.saturating_add(consumed) < total_candidates;
            self.ensure_search_page_fits(
                &mut selected,
                SearchResponseShape {
                    all: &hits,
                    request: &request,
                    generation,
                    total_candidates,
                    offset,
                    consumed,
                    has_more,
                },
                execution.response_options,
            )?;
            let receipt_candidates = selected
                .iter()
                .map(|candidate| {
                    ReceiptEvidence::new(
                        candidate.hit.path.clone(),
                        candidate.hit.start_line,
                        candidate.hit.end_line,
                        candidate.hit.content_hash.clone(),
                        Some(&candidate.hit.excerpt),
                    )
                })
                .collect::<Vec<_>>();
            let receipt = self.evaluate_receipt(
                matches!(execution.output_shape, SearchOutputShape::Full)
                    .then_some(request.receipt_id.as_deref())
                    .flatten(),
                generation,
                &receipt_candidates,
            )?;
            selected = selected
                .into_iter()
                .zip(&receipt.decisions)
                .filter_map(|(candidate, decision)| {
                    matches!(
                        decision,
                        ReceiptDecision::Return | ReceiptDecision::ReturnNearDuplicate
                    )
                    .then_some(candidate)
                })
                .collect();
            let emitted_tokens = selected_search_source_tokens(
                &selected,
                execution.output_shape,
                &self.config.tokenizer,
            );
            let paths = selected
                .iter()
                .map(|candidate| candidate.hit.path.clone())
                .collect::<HashSet<_>>()
                .into_iter()
                .collect::<Vec<_>>();
            let occurrences_returned = selected.len();
            let coverage = search_coverage(&hits, &selected);
            let selected = selected
                .into_iter()
                .map(|candidate| candidate.hit)
                .collect();
            let baseline_source_tokens =
                session.whole_file_source_tokens(&paths, self.config.tokenizer.name())?;
            let mut response = SearchResponse {
                hits: selected,
                coverage,
                occurrences_returned,
                occurrences_total: request.all_occurrences.then_some(total_candidates),
                meta: self.meta(
                    generation,
                    emitted_tokens,
                    has_more.then(|| make_cursor(generation, offset + consumed)),
                ),
            };
            receipt.apply_meta(&mut response.meta);
            Ok((response, baseline_source_tokens, phases, primitive_keys))
        });
        let (mut response, baseline_source_tokens, phases, primitive_keys) = search_result?;
        self.finalize_bounded_response(&mut response, execution.response_options)?;
        if execution.record_savings {
            self.record_token_savings(
                TokenAccountingOperation::Search,
                baseline_source_tokens,
                &response.meta,
            );
        }
        Ok(SearchEvaluation {
            response,
            phases,
            primitive_keys,
        })
    }

    fn symbol_search_hit(&self, hit: SymbolHit, query: &str, excerpt: StoredExcerpt) -> SearchHit {
        let exact = hit.symbol.name == query || hit.symbol.name.eq_ignore_ascii_case(query);
        SearchHit {
            path: hit.path,
            start_line: excerpt.start_line,
            end_line: excerpt.end_line,
            content_hash: hash(&excerpt.content),
            excerpt: excerpt.content,
            match_kind: "symbol".into(),
            match_kinds: vec!["symbol".into()],
            role: Some(ReferenceRole::Definition),
            symbol: Some(hit.symbol.name),
            enclosing_symbol: hit.symbol.parent,
            occurrence: None,
            score: if exact { 10.0 } else { 7.0 },
            normalized_score: 0.0,
            score_reasons: vec![if exact {
                "exact symbol".into()
            } else {
                "symbol".into()
            }],
        }
    }

    fn reference_search_hit(
        &self,
        hit: ReferenceHit,
        query: &str,
        excerpt: StoredExcerpt,
    ) -> SearchHit {
        let exact = hit.reference.name == query || hit.reference.name.eq_ignore_ascii_case(query);
        SearchHit {
            path: hit.path,
            start_line: excerpt.start_line,
            end_line: excerpt.end_line,
            content_hash: hash(&excerpt.content),
            excerpt: excerpt.content,
            match_kind: "reference".into(),
            match_kinds: vec!["reference".into()],
            role: Some(hit.reference.role),
            symbol: Some(hit.reference.name),
            enclosing_symbol: hit.reference.enclosing_symbol,
            occurrence: None,
            score: if exact { 8.0 } else { 5.0 },
            normalized_score: 0.0,
            score_reasons: vec![if exact {
                "exact reference".into()
            } else {
                "reference".into()
            }],
        }
    }

    fn regex_hits(
        &self,
        session: &ReadSession,
        request: &SearchRequest,
        regex: &regex::Regex,
        max_candidates: Option<usize>,
        cancellation: &CancellationToken,
        planning: RegexPlanning,
    ) -> Result<RegexScan> {
        // Hard caps prevent repository-wide lexical scans from running
        // unbounded. Exhaustive modes lift only the candidate-chunk cap and
        // fail explicitly if another cap would make the result incomplete.
        let max_candidates = max_candidates.map(|limit| limit.min(MAX_REGEX_CANDIDATES));
        let path_filter = PathFilter::new(&request.include_paths, &request.exclude_paths)?;
        let has_path_filters =
            !request.include_paths.is_empty() || !request.exclude_paths.is_empty();
        let file_count = session.file_count()?;
        let plan = (planning == RegexPlanning::Enabled)
            .then(|| regex_candidate_plan(request))
            .flatten();
        if let Some(plan) = plan {
            return self.regex_candidate_hits(
                session,
                regex,
                max_candidates,
                cancellation,
                path_filter,
                has_path_filters,
                &request.include_paths,
                &request.exclude_paths,
                file_count,
                plan,
            );
        }

        if file_count > MAX_REGEX_FILES_SCANNED {
            return Err(Error::LimitExceeded);
        }
        let files = session.regex_scan_files(MAX_REGEX_FILES_SCANNED)?;
        for (file, chunk_count) in &files {
            check_cancelled(cancellation)?;
            if path_filter.allows(&file.path) && *chunk_count > MAX_REGEX_CHUNKS_PER_FILE {
                return Err(Error::LimitExceeded);
            }
        }
        let mut hits = Vec::new();
        let mut phases = SearchPhaseCounters {
            regex_candidate_strategy: RegexCandidateStrategy::FullScan,
            regex_files_considered: files.len(),
            ..SearchPhaseCounters::default()
        };
        for (file, _) in files {
            check_cancelled(cancellation)?;
            if !path_filter.allows(&file.path) {
                continue;
            }
            let chunks = session.get_chunks_for_file(file.id, MAX_REGEX_CHUNKS_PER_FILE)?;
            for chunk in chunks {
                check_cancelled(cancellation)?;
                phases.regex_chunks_loaded = phases.regex_chunks_loaded.saturating_add(1);
                phases.regex_chunks_verified = phases.regex_chunks_verified.saturating_add(1);
                if regex.is_match(&chunk.content) {
                    if max_candidates.is_some_and(|limit| hits.len() == limit) {
                        return Err(Error::LimitExceeded);
                    }
                    hits.push(ChunkHit {
                        chunk_id: chunk.id,
                        file_id: chunk.file_id,
                        path: file.path.clone(),
                        content: chunk.content,
                        start_line: chunk.start_line,
                        end_line: chunk.end_line,
                        start_byte: chunk.start_byte,
                        end_byte: chunk.end_byte,
                        token_count: chunk.token_count,
                        generation: file.generation,
                        score: 0.0,
                    });
                }
            }
        }
        Ok(RegexScan { hits, phases })
    }

    #[allow(clippy::too_many_arguments)]
    fn regex_candidate_hits(
        &self,
        session: &ReadSession,
        regex: &regex::Regex,
        max_candidates: Option<usize>,
        cancellation: &CancellationToken,
        path_filter: PathFilter,
        has_path_filters: bool,
        include_paths: &[String],
        exclude_paths: &[String],
        files_considered: usize,
        plan: RegexCandidatePlan,
    ) -> Result<RegexScan> {
        let mut phases = SearchPhaseCounters {
            regex_candidate_strategy: RegexCandidateStrategy::Trigram,
            regex_plan_terms: plan.term_count,
            regex_files_considered: files_considered,
            ..SearchPhaseCounters::default()
        };
        let query = plan.expression.fts_query();
        if has_path_filters {
            let candidate_ids = session.select_scoped_regex_candidate_ids(
                &query,
                MAX_SCOPED_REGEX_ROWS_SCANNED,
                MAX_REGEX_CANDIDATE_CHUNKS,
                include_paths,
                exclude_paths,
                |path| path_filter.allows(path),
            )?;
            phases.regex_candidate_chunks = candidate_ids.len();
            let mut hits = Vec::new();
            for candidate_batch in candidate_ids.chunks(REGEX_CANDIDATE_PAGE_SIZE) {
                check_cancelled(cancellation)?;
                for hit in session.regex_candidates_by_ids(candidate_batch)? {
                    check_cancelled(cancellation)?;
                    phases.regex_chunks_verified = phases.regex_chunks_verified.saturating_add(1);
                    if regex.is_match(&hit.content) {
                        if max_candidates.is_some_and(|limit| hits.len() == limit) {
                            return Err(Error::LimitExceeded);
                        }
                        hits.push(hit);
                    }
                }
            }
            return Ok(RegexScan { hits, phases });
        }
        let candidate_count = session
            .regex_candidate_count_up_to(&query, MAX_REGEX_CANDIDATE_CHUNKS.saturating_add(1))?;
        if candidate_count > MAX_REGEX_CANDIDATE_CHUNKS {
            return Err(Error::LimitExceeded);
        }
        phases.regex_candidate_chunks = candidate_count;
        let mut hits = Vec::new();
        let mut offset = 0usize;
        loop {
            check_cancelled(cancellation)?;
            let page =
                session.search_regex_candidates_page(&query, REGEX_CANDIDATE_PAGE_SIZE, offset)?;
            let page_len = page.len();
            if page_len == 0 {
                break;
            }
            for hit in page {
                check_cancelled(cancellation)?;
                if !path_filter.allows(&hit.path) {
                    continue;
                }
                phases.regex_chunks_verified = phases.regex_chunks_verified.saturating_add(1);
                if regex.is_match(&hit.content) {
                    if max_candidates.is_some_and(|limit| hits.len() == limit) {
                        return Err(Error::LimitExceeded);
                    }
                    hits.push(hit);
                }
            }
            offset = offset.saturating_add(page_len);
            if page_len < REGEX_CANDIDATE_PAGE_SIZE {
                break;
            }
        }
        Ok(RegexScan { hits, phases })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exhaustive_chunk_hits_fail_before_exceeding_the_materialization_limit() {
        let hit = ChunkHit {
            chunk_id: 1,
            file_id: 1,
            path: "src/lib.rs".into(),
            content: "key key key".into(),
            start_line: 1,
            end_line: 1,
            start_byte: 0,
            end_byte: 11,
            token_count: 3,
            generation: 1,
            score: 0.0,
        };

        let error = chunk_search_hits(&hit, "key", true, 0, None, false, 2)
            .expect_err("third occurrence exceeds the materialization limit");

        assert!(matches!(error, Error::LimitExceeded));
    }
}
