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

impl Services {
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
