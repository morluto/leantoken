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

#[derive(Clone, Copy)]
struct OccurrenceMaterializationLimit {
    existing_hits: usize,
    max_hits: usize,
}

fn chunk_search_hits(
    hit: &ChunkHit,
    query: &str,
    case_sensitive: bool,
    context: usize,
    compiled_matcher: Option<&regex::Regex>,
    regex_match: bool,
    occurrence_limit: OccurrenceMaterializationLimit,
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
        if occurrence_limit.existing_hits.saturating_add(hits.len())
            == occurrence_limit.max_hits
        {
            return Err(Error::RetrievalLimitExceeded {
                kind: RetrievalLimitKind::ExhaustiveOccurrences,
                observed: occurrence_limit
                    .existing_hits
                    .saturating_add(hits.len())
                    .saturating_add(1),
                limit: occurrence_limit.max_hits,
            });
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

impl Services {
    fn symbol_search_hit(&self, hit: SymbolHit, query: &str, excerpt: StoredExcerpt) -> SearchHit {
        let exact = crate::symbol_identity::symbol_identity_matches_ignore_ascii_case(
            query,
            &hit.symbol.name,
            hit.symbol.parent.as_deref(),
        );
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
}
