pub(super) fn coverage_count(
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

pub(super) fn search_coverage(
    all: &[CandidateSearchHit],
    returned: &[CandidateSearchHit],
) -> SearchCoverage {
    SearchCoverage {
        definitions: coverage_count(all, returned, |hit| {
            hit_has_kind(hit, SearchHitKind::Symbol)
        }),
        references: coverage_count(all, returned, |hit| {
            hit_has_kind(hit, SearchHitKind::Reference)
        }),
        text_matches: coverage_count(all, returned, |hit| {
            hit_has_kind(hit, SearchHitKind::Text) || hit_has_kind(hit, SearchHitKind::Regex)
        }),
    }
}

pub(super) fn grouped_search_key(hit: &SearchHit) -> String {
    if let Some(symbol) = hit.symbol.as_deref() {
        return format!("symbol:{symbol}");
    }
    if let Some(symbol) = hit.enclosing_symbol.as_deref() {
        return format!("scope:{}:{symbol}", hit.path);
    }
    format!("range:{}:{}:{}", hit.path, hit.start_line, hit.end_line)
}

pub(super) fn compact_search_hit(hit: &SearchHit) -> SearchCompactHit {
    SearchCompactHit {
        path: hit.path.clone(),
        start_line: hit.start_line,
        end_line: hit.end_line,
        match_kind: hit.match_kind.clone(),
        role: hit.role,
        symbol: hit.symbol.clone(),
        enclosing_symbol: hit.enclosing_symbol.clone(),
        occurrence: hit.occurrence.clone(),
    }
}

pub(super) fn grouped_search_evidence(hit: &SearchHit) -> SearchGroupEvidence {
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

pub(super) fn group_search_hits(hits: &[SearchHit]) -> Vec<SearchGroup> {
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
        if hit_has_kind(hit, SearchHitKind::Text) || hit_has_kind(hit, SearchHitKind::Regex) {
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

        if hit.role == Some(ReferenceRole::Reference) || hit_has_kind(hit, SearchHitKind::Reference)
        {
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
pub(super) enum OccurrenceGroupKey {
    Path(String),
    Excerpt {
        path: String,
        start_line: usize,
        end_line: usize,
        content_hash: String,
    },
}

pub(super) fn occurrence_group_key(
    hit: &SearchHit,
    output: SearchOccurrenceOutput,
) -> OccurrenceGroupKey {
    match output {
        SearchOccurrenceOutput::Coordinates => OccurrenceGroupKey::Path(hit.path.clone()),
        SearchOccurrenceOutput::Excerpts => OccurrenceGroupKey::Excerpt {
            path: hit.path.clone(),
            start_line: hit.start_line,
            end_line: hit.end_line,
            content_hash: hit.content_hash.clone(),
        },
    }
}

pub(super) fn group_occurrence_hits(
    hits: &[SearchHit],
    output: SearchOccurrenceOutput,
) -> Result<Vec<SearchOccurrenceGroup>> {
    let mut groups = Vec::<SearchOccurrenceGroup>::new();
    let mut positions = HashMap::<OccurrenceGroupKey, usize>::new();
    for hit in hits {
        let occurrence = hit.occurrence.as_ref().ok_or_else(|| {
            Error::OperationFailure(
                "exhaustive occurrence response omitted exact coordinates".into(),
            )
        })?;
        let key = occurrence_group_key(hit, output);
        let index = *positions.entry(key).or_insert_with(|| {
            let index = groups.len();
            let (start_line, end_line, excerpt, content_hash) = match output {
                SearchOccurrenceOutput::Coordinates => {
                    (occurrence.start_line, occurrence.end_line, None, None)
                }
                SearchOccurrenceOutput::Excerpts => (
                    hit.start_line,
                    hit.end_line,
                    Some(hit.excerpt.clone()),
                    Some(hit.content_hash.clone()),
                ),
            };
            groups.push(SearchOccurrenceGroup {
                path: hit.path.clone(),
                start_line,
                end_line,
                excerpt,
                content_hash,
                occurrences: Vec::new(),
            });
            index
        });
        let group = &mut groups[index];
        if output == SearchOccurrenceOutput::Coordinates {
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

pub(super) fn select_search_page(
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
        let group_key = match output_shape {
            SearchOutputShape::OccurrenceGroups(SearchOccurrenceOutput::Excerpts) => Some(
                occurrence_group_key(&candidate.hit, SearchOccurrenceOutput::Excerpts),
            ),
            SearchOutputShape::Full
            | SearchOutputShape::Compact
            | SearchOutputShape::OccurrenceGroups(SearchOccurrenceOutput::Coordinates) => None,
        };
        let count = match output_shape {
            SearchOutputShape::Full => tokenizer.count(&candidate.hit.excerpt),
            SearchOutputShape::Compact => 0,
            SearchOutputShape::OccurrenceGroups(SearchOccurrenceOutput::Coordinates) => 0,
            SearchOutputShape::OccurrenceGroups(SearchOccurrenceOutput::Excerpts)
                if group_key
                    .as_ref()
                    .is_some_and(|key| charged_occurrence_groups.contains(key)) =>
            {
                0
            }
            SearchOutputShape::OccurrenceGroups(SearchOccurrenceOutput::Excerpts) => {
                tokenizer.count(&candidate.hit.excerpt)
            }
        };
        if emitted_tokens.saturating_add(count) > token_limit {
            if selected.is_empty() {
                // An empty page still makes cursor progress.  The documented
                // contract permits a token budget to filter every candidate
                // on a page; only preserve an unconsumed candidate when a
                // preceding selected hit has exhausted this page's budget.
                consumed += 1;
                continue;
            }
            // Keep the first hit that cannot fit on this page at the cursor
            // boundary. A fresh page has its own source-token allowance; if
            // we consumed this hit here, pagination could skip it forever.
            break;
        }
        consumed += 1;
        emitted_tokens += count;
        if let Some(key) = group_key {
            charged_occurrence_groups.insert(key);
        }
        selected.push(candidate);
    }
    Ok((selected, consumed, emitted_tokens))
}

pub(super) fn selected_search_source_tokens(
    selected: &[CandidateSearchHit],
    output_shape: SearchOutputShape,
    tokenizer: &crate::tokens::Tokenizer,
) -> usize {
    match output_shape {
        SearchOutputShape::Full => selected
            .iter()
            .map(|candidate| tokenizer.count(&candidate.hit.excerpt))
            .sum(),
        SearchOutputShape::Compact => 0,
        SearchOutputShape::OccurrenceGroups(SearchOccurrenceOutput::Coordinates) => 0,
        SearchOutputShape::OccurrenceGroups(SearchOccurrenceOutput::Excerpts) => {
            let mut seen = HashSet::new();
            selected
                .iter()
                .filter(|candidate| {
                    seen.insert(occurrence_group_key(
                        &candidate.hit,
                        SearchOccurrenceOutput::Excerpts,
                    ))
                })
                .map(|candidate| tokenizer.count(&candidate.hit.excerpt))
                .sum()
        }
    }
}
use super::*;
