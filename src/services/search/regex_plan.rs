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

pub(super) fn regex_candidate_plan(request: &SearchRequest) -> RegexPlanDecision {
    // SQLite's default trigram tokenizer folds ASCII only. Rust regexes use
    // Unicode simple case folding, so a case-insensitive ASCII literal can
    // also match non-ASCII code points (for example, Kelvin sign for `k`).
    // Falling back avoids false negatives until those semantics can be
    // represented by the candidate index.
    if !request.case_sensitive {
        return RegexPlanDecision::Fallback(RegexPlanDiagnostics {
            fallback_reason: RegexPlanFallbackReason::CaseInsensitiveUnicode,
            nodes_visited: 0,
            term_count: 0,
            term_bytes: 0,
        });
    }
    let hir = match regex_syntax::parse(&request.query) {
        Ok(hir) => hir,
        Err(_) => {
            return RegexPlanDecision::Fallback(RegexPlanDiagnostics {
                fallback_reason: RegexPlanFallbackReason::HirParseFailed,
                nodes_visited: 0,
                term_count: 0,
                term_bytes: 0,
            });
        }
    };
    let nodes_visited = match bounded_hir_node_count(&hir) {
        Ok(nodes) => nodes,
        Err(error) => {
            return regex_plan_budget_fallback(
                error,
                RegexPlanBudget {
                    nodes: MAX_REGEX_PLAN_NODES.saturating_add(1),
                    ..RegexPlanBudget::default()
                },
            );
        }
    };
    let mut budget = RegexPlanBudget {
        nodes: nodes_visited,
        ..RegexPlanBudget::default()
    };
    match regex_candidate_expr(&hir, &mut budget) {
        Ok(Some(expression)) => RegexPlanDecision::Planned(RegexCandidatePlan {
            expression,
            source: RegexPlanSource::MandatoryLiterals,
            nodes_visited,
            term_count: budget.terms,
            term_bytes: budget.term_bytes,
            alternative_count: 1,
            min_literal_len: 0,
        }),
        Err(error) => regex_plan_budget_fallback(error, budget),
        Ok(None) => extracted_regex_candidate_plan(&hir, nodes_visited).unwrap_or({
            RegexPlanDecision::Fallback(RegexPlanDiagnostics {
                fallback_reason: RegexPlanFallbackReason::LiteralSequenceUnavailable,
                nodes_visited,
                term_count: budget.terms,
                term_bytes: budget.term_bytes,
            })
        }),
    }
}

pub(super) fn bounded_hir_node_count(
    hir: &Hir,
) -> std::result::Result<usize, RegexPlanBudgetExceeded> {
    fn visit(hir: &Hir, nodes: &mut usize) -> std::result::Result<(), RegexPlanBudgetExceeded> {
        *nodes = nodes.saturating_add(1);
        if *nodes > MAX_REGEX_PLAN_NODES {
            return Err(RegexPlanBudgetExceeded::Nodes);
        }
        match hir.kind() {
            HirKind::Capture(capture) => visit(&capture.sub, nodes)?,
            HirKind::Repetition(repetition) => visit(&repetition.sub, nodes)?,
            HirKind::Concat(expressions) | HirKind::Alternation(expressions) => {
                for expression in expressions {
                    visit(expression, nodes)?;
                }
            }
            HirKind::Empty | HirKind::Literal(_) | HirKind::Class(_) | HirKind::Look(_) => {}
        }
        Ok(())
    }

    let mut nodes = 0usize;
    visit(hir, &mut nodes)?;
    Ok(nodes)
}

pub(super) fn regex_plan_budget_fallback(
    error: RegexPlanBudgetExceeded,
    budget: RegexPlanBudget,
) -> RegexPlanDecision {
    let fallback_reason = match error {
        RegexPlanBudgetExceeded::Nodes => RegexPlanFallbackReason::PlanNodeLimit,
        RegexPlanBudgetExceeded::Terms => RegexPlanFallbackReason::PlanTermLimit,
        RegexPlanBudgetExceeded::TermBytes => RegexPlanFallbackReason::PlanTermBytesLimit,
    };
    RegexPlanDecision::Fallback(RegexPlanDiagnostics {
        fallback_reason,
        nodes_visited: budget.nodes,
        term_count: budget.terms,
        term_bytes: budget.term_bytes,
    })
}

pub(super) fn regex_candidate_expr(
    hir: &Hir,
    budget: &mut RegexPlanBudget,
) -> std::result::Result<Option<RegexCandidateExpr>, RegexPlanBudgetExceeded> {
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

pub(super) fn literal_candidate_expr(
    literal: &[u8],
    budget: &mut RegexPlanBudget,
) -> std::result::Result<Option<RegexCandidateExpr>, RegexPlanBudgetExceeded> {
    let mut terms = Vec::new();
    for bytes in literal.split(|byte| !byte.is_ascii_alphanumeric() && *byte != b'_') {
        if bytes.len() < 3 {
            continue;
        }
        let Some(term) = std::str::from_utf8(bytes).ok().map(str::to_owned) else {
            continue;
        };
        budget.add_term(&term)?;
        terms.push(RegexCandidateExpr::Term(term));
    }
    Ok(combine_candidate_expr(terms, true))
}

pub(super) fn extracted_regex_candidate_plan(
    hir: &Hir,
    nodes_visited: usize,
) -> Option<RegexPlanDecision> {
    let mut plans = Vec::new();
    let mut first_limit = None;
    for (kind, source) in [
        (ExtractKind::Prefix, RegexPlanSource::PrefixLiterals),
        (ExtractKind::Suffix, RegexPlanSource::SuffixLiterals),
    ] {
        match extracted_literal_plan(hir, nodes_visited, kind, source) {
            Ok(Some(plan)) => plans.push(plan),
            Ok(None) => {}
            Err(error) => {
                first_limit.get_or_insert(error);
            }
        }
    }
    if plans.is_empty() {
        return first_limit.map(|(error, budget)| regex_plan_budget_fallback(error, budget));
    }
    plans.sort_by(|left, right| {
        left.alternative_count
            .cmp(&right.alternative_count)
            .then_with(|| right.min_literal_len.cmp(&left.min_literal_len))
            .then_with(|| right.term_bytes.cmp(&left.term_bytes))
            .then_with(|| {
                regex_plan_source_order(left.source).cmp(&regex_plan_source_order(right.source))
            })
    });
    Some(RegexPlanDecision::Planned(plans.remove(0)))
}

pub(super) fn extracted_literal_plan(
    hir: &Hir,
    nodes_visited: usize,
    kind: ExtractKind,
    source: RegexPlanSource,
) -> std::result::Result<Option<RegexCandidatePlan>, (RegexPlanBudgetExceeded, RegexPlanBudget)> {
    let mut extractor = Extractor::new();
    extractor
        .kind(kind)
        .limit_class(MAX_REGEX_LITERAL_SEQUENCE)
        .limit_repeat(MAX_REGEX_LITERAL_SEQUENCE)
        .limit_literal_len(MAX_REGEX_PLAN_TERM_BYTES)
        .limit_total(MAX_REGEX_LITERAL_SEQUENCE);
    let sequence = extractor.extract(hir);
    let min_literal_len = sequence.min_literal_len().unwrap_or(0);
    let Some(literals) = sequence.literals() else {
        return Ok(None);
    };
    if literals.is_empty() {
        return Ok(None);
    }

    let mut budget = RegexPlanBudget {
        nodes: nodes_visited,
        ..RegexPlanBudget::default()
    };
    let mut alternatives = Vec::with_capacity(literals.len());
    for literal in literals {
        let expression = match literal_candidate_expr(literal.as_bytes(), &mut budget) {
            Ok(Some(expression)) => expression,
            Ok(None) => return Ok(None),
            Err(error) => return Err((error, budget)),
        };
        alternatives.push(expression);
    }
    let alternative_count = alternatives.len();
    let Some(expression) = combine_candidate_expr(alternatives, false) else {
        return Ok(None);
    };
    Ok(Some(RegexCandidatePlan {
        expression,
        source,
        nodes_visited,
        term_count: budget.terms,
        term_bytes: budget.term_bytes,
        alternative_count,
        min_literal_len,
    }))
}

pub(super) const fn regex_plan_source_order(source: RegexPlanSource) -> u8 {
    match source {
        RegexPlanSource::MandatoryLiterals => 0,
        RegexPlanSource::PrefixLiterals => 1,
        RegexPlanSource::SuffixLiterals => 2,
    }
}

pub(super) fn combine_candidate_expr(
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

pub(super) fn compile_regex(request: &SearchRequest) -> Result<regex::Regex> {
    Ok(regex::RegexBuilder::new(&request.query)
        .case_insensitive(!request.case_sensitive)
        .size_limit(1 << 20)
        .dfa_size_limit(1 << 20)
        .build()?)
}

pub(super) fn compile_occurrence_literal_regex(request: &SearchRequest) -> Result<regex::Regex> {
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
pub(in crate::services) fn compile_literal_regex(
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

pub(in crate::services) struct LiteralFullScan<'a> {
    pub(in crate::services) session: &'a IndexReadSnapshot,
    pub(in crate::services) query: &'a str,
    pub(in crate::services) matcher: &'a regex::Regex,
    pub(in crate::services) include_paths: &'a [String],
    pub(in crate::services) exclude_paths: &'a [String],
    pub(in crate::services) max_candidates: usize,
    pub(in crate::services) max_tokens: usize,
    pub(in crate::services) cancellation: &'a CancellationToken,
}

impl Services {
    pub(in crate::services) fn full_scan_literal_hits(
        &self,
        scan: LiteralFullScan<'_>,
    ) -> Result<Vec<ChunkHit>> {
        let LiteralFullScan {
            session,
            query,
            matcher,
            include_paths,
            exclude_paths,
            max_candidates,
            max_tokens,
            cancellation,
        } = scan;
        let request = SearchRequest {
            query: query.to_owned(),
            mode: SearchMode::Text,
            include_paths: include_paths.to_vec(),
            exclude_paths: exclude_paths.to_vec(),
            focus_paths: Vec::new(),
            max_results: Some(max_candidates),
            max_tokens: Some(max_tokens),
            context_lines: Some(2),
            case_sensitive: false,
            all_occurrences: false,
            prefer_structural: false,
            receipt_id: None,
            query_receipt: None,
            cursor: None,
        };
        Ok(self
            .regex_hits(
                session,
                &request,
                matcher,
                Some(max_candidates),
                cancellation,
                RegexPlanning::Disabled,
            )?
            .hits)
    }

    pub(super) fn regex_hits(
        &self,
        session: &IndexReadSnapshot,
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
        let decision = if planning == RegexPlanning::Enabled {
            regex_candidate_plan(request)
        } else {
            RegexPlanDecision::Fallback(RegexPlanDiagnostics {
                fallback_reason: RegexPlanFallbackReason::PlanningDisabled,
                nodes_visited: 0,
                term_count: 0,
                term_bytes: 0,
            })
        };
        let fallback = match decision {
            RegexPlanDecision::Planned(plan) => {
                return self.regex_candidate_hits(RegexCandidateParams {
                    session,
                    regex,
                    max_candidates,
                    cancellation,
                    path_filter,
                    has_path_filters,
                    include_paths: &request.include_paths,
                    exclude_paths: &request.exclude_paths,
                    max_results: request.max_results,
                    max_tokens: request.max_tokens,
                    minimum_chunk_bytes: self.config.chunk_bytes,
                    files_considered: file_count,
                    plan,
                });
            }
            RegexPlanDecision::Fallback(diagnostics) => diagnostics,
        };

        if file_count > MAX_REGEX_FILES_SCANNED {
            return Err(Error::RetrievalLimitExceeded {
                kind: RetrievalLimitKind::RegexFullScanFiles,
                observed: file_count,
                limit: MAX_REGEX_FILES_SCANNED,
            });
        }
        let files = session.regex_scan_files(MAX_REGEX_FILES_SCANNED)?;
        for (file, chunk_count) in &files {
            check_cancelled(cancellation)?;
            if path_filter.allows(&file.path) && *chunk_count > MAX_REGEX_CHUNKS_PER_FILE {
                return Err(Error::RetrievalPathLimitExceeded {
                    kind: RetrievalLimitKind::RegexChunksPerFile,
                    path: file.path.clone(),
                    observed: *chunk_count,
                    limit: MAX_REGEX_CHUNKS_PER_FILE,
                });
            }
        }
        let mut hits = Vec::new();
        let mut work = RegexWorkBudget::for_request(
            request.max_results,
            request.max_tokens,
            self.config.chunk_bytes,
        );
        let mut phases = SearchPhaseCounters {
            regex_planning: RegexPlanningOutcome::FullScan {
                fallback_reason: Some(fallback.fallback_reason),
            },
            regex_plan_nodes: fallback.nodes_visited,
            regex_plan_terms: fallback.term_count,
            regex_plan_term_bytes: fallback.term_bytes,
            regex_files_considered: files.len(),
            ..SearchPhaseCounters::default()
        };
        for (file, _) in files {
            check_cancelled(cancellation)?;
            if !path_filter.allows(&file.path) {
                continue;
            }
            work.charge_file(cancellation)?;
            let chunks = session.get_chunks_for_file(file.id, MAX_REGEX_CHUNKS_PER_FILE)?;
            for chunk in chunks {
                if max_candidates.is_some_and(|limit| hits.len() == limit)
                    && regex.is_match(&chunk.content)
                {
                    return Err(Error::RetrievalLimitExceeded {
                        kind: RetrievalLimitKind::RegexRetainedChunks,
                        observed: hits.len().saturating_add(1),
                        limit: max_candidates.unwrap_or(MAX_REGEX_CANDIDATES),
                    });
                }
                work.charge_chunk(chunk.content.len(), cancellation)?;
                phases.regex_chunks_loaded = phases.regex_chunks_loaded.saturating_add(1);
                phases.regex_chunks_verified = phases.regex_chunks_verified.saturating_add(1);
                if regex.is_match(&chunk.content) {
                    if max_candidates.is_some_and(|limit| hits.len() == limit) {
                        return Err(Error::RetrievalLimitExceeded {
                            kind: RetrievalLimitKind::RegexRetainedChunks,
                            observed: hits.len().saturating_add(1),
                            limit: max_candidates.unwrap_or(MAX_REGEX_CANDIDATES),
                        });
                    }
                    phases.regex_retained_chunks = phases.regex_retained_chunks.saturating_add(1);
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

    pub(super) fn regex_candidate_hits(
        &self,
        params: RegexCandidateParams<'_, '_>,
    ) -> Result<RegexScan> {
        let RegexCandidateParams {
            session,
            regex,
            max_candidates,
            cancellation,
            path_filter,
            has_path_filters,
            include_paths,
            exclude_paths,
            max_results,
            max_tokens,
            minimum_chunk_bytes,
            files_considered,
            plan,
        } = params;
        let mut phases = SearchPhaseCounters {
            regex_planning: RegexPlanningOutcome::Trigram {
                source: plan.source,
            },
            regex_plan_nodes: plan.nodes_visited,
            regex_plan_terms: plan.term_count,
            regex_plan_term_bytes: plan.term_bytes,
            regex_files_considered: files_considered,
            ..SearchPhaseCounters::default()
        };
        let query = plan.expression.fts_query();
        let mut work = RegexWorkBudget::for_request(max_results, max_tokens, minimum_chunk_bytes);
        let mut charged_files = HashSet::new();
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
                    if charged_files.insert(hit.file_id) {
                        work.charge_file(cancellation)?;
                    }
                    if max_candidates.is_some_and(|limit| hits.len() == limit)
                        && regex.is_match(&hit.content)
                    {
                        return Err(Error::RetrievalLimitExceeded {
                            kind: RetrievalLimitKind::RegexRetainedChunks,
                            observed: hits.len().saturating_add(1),
                            limit: max_candidates.unwrap_or(MAX_REGEX_CANDIDATES),
                        });
                    }
                    work.charge_chunk(hit.content.len(), cancellation)?;
                    phases.regex_chunks_verified = phases.regex_chunks_verified.saturating_add(1);
                    if regex.is_match(&hit.content) {
                        if max_candidates.is_some_and(|limit| hits.len() == limit) {
                            return Err(Error::RetrievalLimitExceeded {
                                kind: RetrievalLimitKind::RegexRetainedChunks,
                                observed: hits.len().saturating_add(1),
                                limit: max_candidates.unwrap_or(MAX_REGEX_CANDIDATES),
                            });
                        }
                        phases.regex_retained_chunks =
                            phases.regex_retained_chunks.saturating_add(1);
                        hits.push(hit);
                    }
                }
            }
            return Ok(RegexScan { hits, phases });
        }
        let candidate_count = session
            .regex_candidate_count_up_to(&query, MAX_REGEX_CANDIDATE_CHUNKS.saturating_add(1))?;
        if candidate_count > MAX_REGEX_CANDIDATE_CHUNKS {
            return Err(Error::RetrievalLimitExceeded {
                kind: RetrievalLimitKind::RegexCandidateChunks,
                observed: candidate_count,
                limit: MAX_REGEX_CANDIDATE_CHUNKS,
            });
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
                if !path_filter.allows(&hit.path) {
                    continue;
                }
                if charged_files.insert(hit.file_id) {
                    work.charge_file(cancellation)?;
                }
                if max_candidates.is_some_and(|limit| hits.len() == limit)
                    && regex.is_match(&hit.content)
                {
                    return Err(Error::RetrievalLimitExceeded {
                        kind: RetrievalLimitKind::RegexRetainedChunks,
                        observed: hits.len().saturating_add(1),
                        limit: max_candidates.unwrap_or(MAX_REGEX_CANDIDATES),
                    });
                }
                work.charge_chunk(hit.content.len(), cancellation)?;
                phases.regex_chunks_verified = phases.regex_chunks_verified.saturating_add(1);
                if regex.is_match(&hit.content) {
                    if max_candidates.is_some_and(|limit| hits.len() == limit) {
                        return Err(Error::RetrievalLimitExceeded {
                            kind: RetrievalLimitKind::RegexRetainedChunks,
                            observed: hits.len().saturating_add(1),
                            limit: max_candidates.unwrap_or(MAX_REGEX_CANDIDATES),
                        });
                    }
                    phases.regex_retained_chunks = phases.regex_retained_chunks.saturating_add(1);
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
use super::*;
pub(super) struct RegexCandidateParams<'a, 'b> {
    pub session: &'a IndexReadSnapshot,
    pub regex: &'a regex::Regex,
    pub max_candidates: Option<usize>,
    pub cancellation: &'a CancellationToken,
    pub path_filter: PathFilter,
    pub has_path_filters: bool,
    pub include_paths: &'b [String],
    pub exclude_paths: &'b [String],
    pub max_results: Option<usize>,
    pub max_tokens: Option<usize>,
    pub minimum_chunk_bytes: usize,
    pub files_considered: usize,
    pub plan: RegexCandidatePlan,
}
