//! Lexical and structural search over a request-scoped snapshot.

use std::collections::{HashMap, HashSet};

use tokio_util::sync::CancellationToken;

use super::Services;
use super::files::FILE_LIST_PAGE_SIZE;
use super::read::{StoredExcerpt, StoredExcerptRequest};
use super::receipts::{ReceiptDecision, ReceiptEvidence};
use super::validation::{
    MAX_QUERY_BYTES, PathFilter, PathMatcher, check_cancelled, make_cursor, parse_cursor,
    validate_cursor, validate_glob_patterns, validate_input,
};
use crate::model::*;
use crate::storage::{ChunkHit, ReadSession, ReferenceHit, SymbolHit};
use crate::text::{
    anchored_line_window, byte_range_to_line_range, byte_to_line, excerpt, hash, line_starts,
};
use crate::{Error, Result};

/// Absolute regex scan candidate cap (independent of max_results multiplier).
const MAX_REGEX_CANDIDATES: usize = 2_000;
/// Maximum files examined during a regex scan before early exit.
const MAX_REGEX_FILES_SCANNED: usize = 10_000;
/// Maximum chunks examined per file during a regex scan.
const MAX_REGEX_CHUNKS_PER_FILE: usize = 256;
/// Maximum exact matches materialized by one exhaustive occurrence request.
const MAX_EXHAUSTIVE_OCCURRENCES: usize = 100_000;
const FILTER_SCAN_PAGE_SIZE: usize = 256;
const MAX_FILTER_SCAN_ROWS: usize = 10_000;

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

fn chunk_search_hit_for_range(
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

impl Services {
    /// Search indexed lexical and structural evidence.
    pub async fn search(&self, request: SearchRequest) -> Result<SearchResponse> {
        self.search_cancellable(request, CancellationToken::new())
            .await
    }

    /// Search after applying a cancellable index consistency boundary.
    pub async fn search_with_consistency_cancellable(
        &self,
        request: SearchRequest,
        consistency: IndexConsistency,
        cancellation: CancellationToken,
    ) -> Result<SearchResponse> {
        validate_search_input(&request)?;
        self.result_limit(request.max_results)?;
        self.token_limit(request.max_tokens, self.config.default_read_tokens)?;
        self.context_line_limit(request.context_lines)?;
        self.apply_consistency(consistency, cancellation.clone())
            .await?;
        self.search_cancellable(request, cancellation).await
    }

    pub async fn search_cancellable(
        &self,
        request: SearchRequest,
        cancellation: CancellationToken,
    ) -> Result<SearchResponse> {
        let this = self.clone();
        tokio::task::spawn_blocking(move || this.search_sync(request, &cancellation)).await?
    }

    fn search_sync(
        &self,
        request: SearchRequest,
        cancellation: &CancellationToken,
    ) -> Result<SearchResponse> {
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
        let (mut response, baseline_source_tokens) = self.consistent(|session, generation| {
            let offset = parse_cursor(request.cursor.as_deref(), generation)?;
            let mut hits = Vec::new();
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
                SearchMode::Regex => self.regex_hits(
                    session,
                    &request,
                    regex.as_ref().expect("regex mode compiles a pattern"),
                    (!request.all_occurrences).then_some(limit.saturating_mul(20)),
                    cancellation,
                )?,
                SearchMode::Text if request.all_occurrences => self.regex_hits(
                    session,
                    &request,
                    occurrence_literal_regex
                        .as_ref()
                        .expect("exhaustive text mode compiles a literal pattern"),
                    None,
                    cancellation,
                )?,
                SearchMode::Text | SearchMode::Auto | SearchMode::Identifier => {
                    let fetch_page = |offset, page_limit| {
                        if matches!(request.mode, SearchMode::Identifier)
                            || request.query.chars().count() < 3
                        {
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

            let mut emitted_tokens = 0usize;
            let mut selected = Vec::new();
            let total_candidates = hits.len();
            let page = hits.iter().skip(offset).take(limit).cloned();
            let mut consumed = 0usize;
            for candidate in page {
                check_cancelled(cancellation)?;
                consumed += 1;
                let count = self.config.tokenizer.count(&candidate.hit.excerpt);
                if emitted_tokens.saturating_add(count) > token_limit {
                    continue;
                }
                emitted_tokens += count;
                selected.push(candidate);
            }
            let has_more = offset.saturating_add(consumed) < total_candidates;
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
                request.receipt_id.as_deref(),
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
            emitted_tokens = selected
                .iter()
                .map(|candidate| self.config.tokenizer.count(&candidate.hit.excerpt))
                .sum();
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
            Ok((response, baseline_source_tokens))
        })?;
        if let Some(baseline_source_tokens) = baseline_source_tokens {
            self.record_token_savings(
                TokenSavingsOperation::Search,
                baseline_source_tokens,
                response.meta.emitted_tokens,
            );
        }
        self.finalize_response(&mut response)?;
        Ok(response)
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
    ) -> Result<Vec<ChunkHit>> {
        // Hard caps prevent repository-wide lexical scans from running
        // unbounded. Exhaustive modes lift only the candidate-chunk cap and
        // fail explicitly if another cap would make the result incomplete.
        let max_candidates = max_candidates.map(|limit| limit.min(MAX_REGEX_CANDIDATES));
        let path_filter = PathFilter::new(&request.include_paths, &request.exclude_paths)?;
        let mut hits = Vec::new();
        let mut files_scanned = 0usize;
        let mut cursor = None;
        loop {
            check_cancelled(cancellation)?;
            let page = session.list_files(FILE_LIST_PAGE_SIZE, cursor)?;
            if page.is_empty() {
                break;
            }
            cursor = page.last().map(|file| file.id);
            for file in page {
                check_cancelled(cancellation)?;
                if files_scanned == MAX_REGEX_FILES_SCANNED {
                    return Err(Error::LimitExceeded);
                }
                files_scanned += 1;
                if !path_filter.allows(&file.path) {
                    continue;
                }
                let chunks = session
                    .get_chunks_for_file(file.id, MAX_REGEX_CHUNKS_PER_FILE.saturating_add(1))?;
                let chunks_truncated = chunks.len() > MAX_REGEX_CHUNKS_PER_FILE;
                for chunk in chunks.into_iter().take(MAX_REGEX_CHUNKS_PER_FILE) {
                    check_cancelled(cancellation)?;
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
                if chunks_truncated {
                    return Err(Error::LimitExceeded);
                }
            }
        }
        Ok(hits)
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
