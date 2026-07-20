//! Lexical and structural search over a request-scoped snapshot.

use std::collections::HashSet;

use tokio_util::sync::CancellationToken;

use super::Services;
use super::files::FILE_LIST_PAGE_SIZE;
use super::read::{StoredExcerpt, StoredExcerptRequest};
use super::validation::{
    MAX_QUERY_BYTES, check_cancelled, make_cursor, parse_cursor, path_allowed, path_matches,
    validate_input, validate_patterns,
};
use crate::model::*;
use crate::storage::{ChunkHit, ReadSession, ReferenceHit, SymbolHit};
use crate::text::{byte_range_to_line_range, excerpt, hash};
use crate::{Error, Result};

/// Absolute regex scan candidate cap (independent of max_results multiplier).
const MAX_REGEX_CANDIDATES: usize = 2_000;
/// Maximum files examined during a regex scan before early exit.
const MAX_REGEX_FILES_SCANNED: usize = 10_000;
/// Maximum chunks examined per file during a regex scan.
const MAX_REGEX_CHUNKS_PER_FILE: usize = 256;
pub(super) fn chunk_search_hit(
    hit: ChunkHit,
    query: &str,
    case_sensitive: bool,
    context: usize,
    compiled_regex: Option<&regex::Regex>,
) -> Result<Option<SearchHit>> {
    let byte_range = if let Some(regex) = compiled_regex {
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
    let (local_start, local_end) = byte_range_to_line_range(&hit.content, start, end);
    let excerpt_start = local_start.saturating_sub(context).max(1);
    let available_lines = hit.content.lines().count().max(1);
    let mut excerpt_end = local_end.saturating_add(context).min(available_lines);
    if excerpt_end.saturating_sub(excerpt_start).saturating_add(1) > 20 {
        excerpt_end = excerpt_start + 19;
    }
    let excerpt = excerpt(&hit.content, excerpt_start, excerpt_end);
    Ok(Some(SearchHit {
        path: hit.path,
        start_line: hit.start_line + excerpt_start - 1,
        end_line: hit.start_line + excerpt_end - 1,
        content_hash: hash(&excerpt),
        excerpt,
        match_kind: if compiled_regex.is_some() {
            "regex".into()
        } else {
            "text".into()
        },
        role: None,
        symbol: None,
        enclosing_symbol: None,
        score: 3.0 + (-hit.score).max(0.0) * 1_000_000.0,
        score_reasons: vec![if compiled_regex.is_some() {
            "regex match".into()
        } else {
            "text match".into()
        }],
    }))
}

pub(super) fn matching_line(hit: &ChunkHit, query: &str, case_sensitive: bool) -> Option<usize> {
    hit.content
        .lines()
        .position(|line| {
            if case_sensitive {
                line.contains(query)
            } else {
                line.to_lowercase().contains(&query.to_lowercase())
            }
        })
        .map(|offset| hit.start_line + offset)
}

fn matching_line_for_search(
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
    matching_line(hit, query, case_sensitive)
}

fn apply_focus(hits: &mut [SearchHit], focus_paths: &[String]) -> Result<()> {
    for hit in hits {
        let mut focused = false;
        for focus in focus_paths {
            focused |= path_matches(&hit.path, focus)?;
        }
        if focused {
            hit.score += 2.0;
            hit.score_reasons.push("focus path".into());
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
        if request.query.trim().is_empty() {
            return Err(Error::InvalidRequest(
                "search query must not be empty".into(),
            ));
        }
        validate_input(&request.query, "search query", MAX_QUERY_BYTES)?;
        validate_patterns(&request.include_paths)?;
        validate_patterns(&request.exclude_paths)?;
        validate_patterns(&request.focus_paths)?;
        let regex = matches!(request.mode, SearchMode::Regex)
            .then(|| compile_regex(&request))
            .transpose()?;
        self.consistent(|session, generation| {
            let limit = self.result_limit(request.max_results);
            let token_limit = self.token_limit(request.max_tokens, self.config.default_read_tokens);
            let offset = parse_cursor(request.cursor.as_deref(), generation)?;
            let context_lines = request
                .context_lines
                .unwrap_or(self.config.context_lines)
                .min(20);
            let mut hits = Vec::new();
            if matches!(
                request.mode,
                SearchMode::Auto | SearchMode::Identifier | SearchMode::Symbol
            ) {
                let mut symbol_hits = Vec::new();
                for hit in
                    session.search_symbols(&request.query, request.case_sensitive, limit * 4)?
                {
                    check_cancelled(cancellation)?;
                    if path_allowed(&hit.path, &request.include_paths, &request.exclude_paths)? {
                        symbol_hits.push(hit);
                    }
                }
                let excerpt_requests = symbol_hits
                    .iter()
                    .map(|hit| StoredExcerptRequest {
                        file_id: hit.symbol.file_id,
                        start_line: hit.symbol.start_line,
                        end_line: hit.symbol.end_line,
                        context: context_lines,
                        max_lines: 30,
                    })
                    .collect::<Vec<_>>();
                for (hit, excerpt) in symbol_hits
                    .into_iter()
                    .zip(self.stored_excerpts(session, &excerpt_requests)?)
                {
                    if let Some(excerpt) = excerpt {
                        hits.push(self.symbol_search_hit(hit, &request.query, excerpt));
                    }
                }
            }
            if matches!(
                request.mode,
                SearchMode::Auto | SearchMode::Identifier | SearchMode::Reference
            ) {
                let mut reference_hits = Vec::new();
                for hit in
                    session.search_references(&request.query, request.case_sensitive, limit * 4)?
                {
                    check_cancelled(cancellation)?;
                    if path_allowed(&hit.path, &request.include_paths, &request.exclude_paths)? {
                        reference_hits.push(hit);
                    }
                }
                let excerpt_requests = reference_hits
                    .iter()
                    .map(|hit| StoredExcerptRequest {
                        file_id: hit.reference.file_id,
                        start_line: hit.reference.start_line,
                        end_line: hit.reference.end_line,
                        context: context_lines,
                        max_lines: 12,
                    })
                    .collect::<Vec<_>>();
                for (hit, excerpt) in reference_hits
                    .into_iter()
                    .zip(self.stored_excerpts(session, &excerpt_requests)?)
                {
                    if let Some(excerpt) = excerpt {
                        hits.push(self.reference_search_hit(hit, &request.query, excerpt));
                    }
                }
            }

            let lexical = match request.mode {
                SearchMode::Regex => self.regex_hits(
                    session,
                    &request,
                    regex.as_ref().expect("regex mode compiles a pattern"),
                    limit * 20,
                    cancellation,
                )?,
                SearchMode::Text | SearchMode::Auto => {
                    if request.query.chars().count() >= 3 {
                        session.search_trigram(&request.query, limit * 8)?
                    } else {
                        session.search_word(&fts_quote(&request.query), limit * 8)?
                    }
                }
                SearchMode::Identifier => {
                    session.search_word(&fts_quote(&request.query), limit * 8)?
                }
                SearchMode::Symbol | SearchMode::Reference => Vec::new(),
            };
            let mut lexical_hits = Vec::new();
            for hit in lexical {
                check_cancelled(cancellation)?;
                if path_allowed(&hit.path, &request.include_paths, &request.exclude_paths)?
                    && let Some(search_hit) = chunk_search_hit(
                        hit.clone(),
                        &request.query,
                        request.case_sensitive,
                        context_lines,
                        regex.as_ref(),
                    )?
                {
                    let matched_line = matching_line_for_search(
                        &hit,
                        &request.query,
                        request.case_sensitive,
                        regex.as_ref(),
                    )
                    .unwrap_or(search_hit.start_line);
                    lexical_hits.push((hit, search_hit, matched_line));
                }
            }
            let lexical_locations = lexical_hits
                .iter()
                .map(|(hit, _, matched_line)| (hit.file_id, *matched_line))
                .collect::<Vec<_>>();
            let enclosing = session.find_enclosing_symbols_batch(&lexical_locations)?;
            for ((_, mut hit, _), symbol) in lexical_hits.into_iter().zip(enclosing) {
                hit.enclosing_symbol = symbol.map(|symbol| symbol.name);
                hits.push(hit);
            }

            apply_focus(&mut hits, &request.focus_paths)?;
            hits.sort_by(|left, right| {
                right
                    .score
                    .total_cmp(&left.score)
                    .then_with(|| left.path.cmp(&right.path))
                    .then_with(|| left.start_line.cmp(&right.start_line))
            });
            let mut seen = HashSet::new();
            hits.retain(|hit| {
                seen.insert((
                    hit.path.clone(),
                    hit.start_line,
                    hit.end_line,
                    hit.content_hash.clone(),
                ))
            });

            let mut emitted_tokens = 0usize;
            let mut selected = Vec::new();
            let remaining = hits.len().saturating_sub(offset);
            let mut consumed = 0usize;
            for hit in hits.into_iter().skip(offset) {
                check_cancelled(cancellation)?;
                if selected.len() >= limit {
                    break;
                }
                consumed += 1;
                let count = self.config.tokenizer.count(&hit.excerpt);
                if emitted_tokens.saturating_add(count) > token_limit {
                    continue;
                }
                emitted_tokens += count;
                selected.push(hit);
            }
            let has_more = consumed < remaining;
            Ok(SearchResponse {
                hits: selected,
                meta: self.meta(
                    generation,
                    emitted_tokens,
                    has_more.then(|| make_cursor(generation, offset + consumed)),
                ),
            })
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
            role: Some(ReferenceRole::Definition),
            symbol: Some(hit.symbol.name),
            enclosing_symbol: hit.symbol.parent,
            score: if exact { 10.0 } else { 7.0 },
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
            role: Some(hit.reference.role),
            symbol: Some(hit.reference.name),
            enclosing_symbol: hit.reference.enclosing_symbol,
            score: if exact { 8.0 } else { 5.0 },
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
        max_candidates: usize,
        cancellation: &CancellationToken,
    ) -> Result<Vec<ChunkHit>> {
        // Hard caps prevent repository-wide regex work from running unbounded.
        // Prefer FTS/trigram prefilters in other modes; regex fails explicitly
        // rather than returning an incomplete snapshot scan.
        let max_candidates = max_candidates.min(MAX_REGEX_CANDIDATES);
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
                if !path_allowed(&file.path, &request.include_paths, &request.exclude_paths)? {
                    continue;
                }
                let chunks = session
                    .get_chunks_for_file(file.id, MAX_REGEX_CHUNKS_PER_FILE.saturating_add(1))?;
                let chunks_truncated = chunks.len() > MAX_REGEX_CHUNKS_PER_FILE;
                for chunk in chunks.into_iter().take(MAX_REGEX_CHUNKS_PER_FILE) {
                    check_cancelled(cancellation)?;
                    if regex.is_match(&chunk.content) {
                        if hits.len() == max_candidates {
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
