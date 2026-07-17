//! Task-shaped context candidate assembly and ranking handoff.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use tokio_util::sync::CancellationToken;

mod facets;

use super::Services;
use super::read::StoredExcerpt;
use super::search::{chunk_search_hit, fts_quote, matching_line};
use super::validation::{
    MAX_INPUT_ITEMS, MAX_PATTERN_BYTES, MAX_QUERY_BYTES, check_cancelled, path_allowed,
    validate_input, validate_patterns,
};
use crate::model::*;
use crate::ranking::{self, Candidate};
use crate::repository::git_changed_paths;
use crate::storage::{FileRecord, ReadSession, SymbolRecord};
use crate::text::{expand_terms, identifier_words};
use crate::{Error, Result};
use facets::{ContextQuery, FacetKind};

const GIT_CHANGED_PATHS_MAX: usize = 512;
/// Maximum context query terms (symbols/refs/FTS fan-out budget).
const MAX_CONTEXT_QUERIES: usize = 12;
/// Per-term symbol/reference candidate cap for context assembly.
const MAX_CONTEXT_HITS_PER_SOURCE: usize = 20;
/// Per-term FTS candidate cap for context assembly.
const MAX_CONTEXT_LEXICAL_HITS: usize = 30;
/// Per-import symbol scan cap for concept-corroborated structural expansion.
const MAX_IMPORT_SYMBOLS: usize = 128;
const MIN_CORROBORATED_QUERY_WEIGHT: f64 = 0.65;
const SYMBOL_CONTEXT_TOKEN_CAP: usize = 768;
const REFERENCE_CONTEXT_TOKEN_CAP: usize = 256;
const TEXT_CONTEXT_TOKEN_CAP: usize = 256;
const IMPORT_SYMBOL_CONTEXT_TOKEN_CAP: usize = 384;

#[derive(Clone, Copy)]
enum ContextExcerptKind {
    Symbol,
    Reference,
    Text,
    ImportSymbol,
}

impl ContextExcerptKind {
    const fn token_cap(self) -> usize {
        match self {
            Self::Symbol => SYMBOL_CONTEXT_TOKEN_CAP,
            Self::Reference => REFERENCE_CONTEXT_TOKEN_CAP,
            Self::Text => TEXT_CONTEXT_TOKEN_CAP,
            Self::ImportSymbol => IMPORT_SYMBOL_CONTEXT_TOKEN_CAP,
        }
    }
}

fn excerpt_budget(request_budget: usize, kind: ContextExcerptKind) -> usize {
    request_budget.min(kind.token_cap())
}

fn cached_file(
    session: &ReadSession,
    cache: &mut HashMap<String, Option<FileRecord>>,
    path: &str,
) -> Result<Option<FileRecord>> {
    if let Some(file) = cache.get(path) {
        return Ok(file.clone());
    }
    let file = session.find_file(path)?;
    cache.insert(path.to_owned(), file.clone());
    Ok(file)
}

fn context_path_score(path: &str, terms: &[String], task: &str) -> f64 {
    let path = path.to_lowercase();
    let mut score = terms
        .iter()
        .filter(|term| path.contains(term.to_ascii_lowercase().as_str()))
        .count() as f64;
    for code_token in facets::legacy_code_tokens(task)
        .into_iter()
        .filter(|token| {
            token.contains("::")
                || token
                    .split('.')
                    .any(|part| part.chars().next().is_some_and(char::is_uppercase))
        })
    {
        let matched_parts = expand_terms(&code_token)
            .into_iter()
            .map(|part| part.to_ascii_lowercase())
            .filter(|part| part.chars().count() >= 2 && path.contains(part))
            .collect::<HashSet<_>>()
            .len();
        if matched_parts >= 2 {
            #[allow(clippy::cast_precision_loss)]
            {
                score += (matched_parts * matched_parts) as f64;
            }
        }
    }
    for (language, component) in [
        ("javascript", "/js/"),
        ("typescript", "/ts/"),
        ("python", "/python/"),
        ("rust", "/rust/"),
        ("go", "/go/"),
    ] {
        if task_mentions_language(task, language) && format!("/{path}/").contains(component) {
            // An explicit language name in the task is strong repository-scope
            // evidence. Keep this above an exact-name match in another
            // language so common names such as `Point` do not dominate.
            score += 12.0;
        }
    }
    score
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum CandidateDiagnostics {
    Omit,
    Collect,
}

fn qualified_symbol_match(
    concept: &str,
    name: &str,
    parent: Option<&str>,
    signature: Option<&str>,
) -> f64 {
    if !concept.contains(['.', ':']) {
        return 0.0;
    }
    let parts = concept
        .split(|character: char| !character.is_alphanumeric() && character != '_')
        .flat_map(identifier_words)
        .map(|part| part.to_ascii_lowercase())
        .filter(|part| part.chars().count() >= 2)
        .collect::<HashSet<_>>();
    if parts.len() < 2 {
        return 0.0;
    }
    let haystack = format!(
        "{} {} {}",
        name,
        parent.unwrap_or_default(),
        signature.unwrap_or_default()
    )
    .to_ascii_lowercase();
    f64::from(parts.iter().all(|part| haystack.contains(part)))
}

fn record_query_hit(
    fusion: &mut HashMap<String, HashMap<String, f64>>,
    path: &str,
    fusion_key: &str,
    weight: f64,
    rank: usize,
) {
    if weight < MIN_CORROBORATED_QUERY_WEIGHT {
        return;
    }
    const RRF_K: f64 = 60.0;
    #[allow(clippy::cast_precision_loss)]
    let score = weight * RRF_K / (RRF_K + rank as f64 + 1.0);
    fusion
        .entry(path.to_owned())
        .or_default()
        .entry(fusion_key.to_owned())
        .and_modify(|current| *current = current.max(score))
        .or_insert(score);
}

fn apply_query_fusion(
    candidates: &mut [Candidate],
    fusion: &HashMap<String, HashMap<String, f64>>,
) {
    for candidate in candidates {
        let Some(matches) = fusion.get(&candidate.path) else {
            continue;
        };
        if matches.len() > 1 {
            let total = matches.values().sum::<f64>();
            let strongest = matches.values().copied().fold(0.0, f64::max);
            candidate.path_score += (total - strongest).min(0.2);
            if !candidate
                .match_kinds
                .iter()
                .any(|kind| kind == "multi-query")
            {
                candidate.match_kinds.push("multi-query".into());
            }
        }
    }
}

fn annotate_candidate(
    mut candidate: Candidate,
    query: &ContextQuery,
    channel: &str,
    rank: usize,
) -> Candidate {
    for facet in query.facet_names() {
        candidate = candidate.facet(facet, &query.fusion_key);
    }
    candidate.channel(channel, rank)
}

fn low_cardinality_exact_query(queries: &[ContextQuery]) -> bool {
    queries
        .iter()
        .filter(|query| query.has_facet(FacetKind::ExactAtom))
        .map(|query| query.fusion_key.as_str())
        .collect::<BTreeSet<_>>()
        .len()
        == 1
}

fn corroborated_import_symbol<'a>(
    symbols: Vec<SymbolRecord>,
    queries: &'a [ContextQuery],
    seed_concepts: &BTreeSet<String>,
) -> Option<(SymbolRecord, &'a ContextQuery, f64)> {
    let mut best: Option<(usize, usize, usize, SymbolRecord, &ContextQuery, f64)> = None;
    for (query_rank, query) in queries.iter().enumerate() {
        if query.concept_weight < MIN_CORROBORATED_QUERY_WEIGHT
            || !seed_concepts.contains(&query.fusion_key)
            || !(query.has_facet(FacetKind::ExactAtom)
                || query.has_facet(FacetKind::Symbol)
                || query.has_facet(FacetKind::Configuration))
        {
            continue;
        }
        for symbol in &symbols {
            let exact = symbol.name.eq_ignore_ascii_case(&query.value);
            let qualified = qualified_symbol_match(
                &query.fusion_key,
                &symbol.name,
                symbol.parent.as_deref(),
                symbol.signature.as_deref(),
            ) > 0.0;
            if !exact && !qualified {
                continue;
            }
            let class = usize::from(qualified) * 2 + usize::from(exact);
            let evidence = f64::from(exact) + f64::from(qualified) * 1.5;
            let candidate = (
                class,
                usize::MAX - query_rank,
                usize::MAX - symbol.start_line,
                symbol.clone(),
                query,
                evidence,
            );
            if best.as_ref().is_none_or(|current| {
                (candidate.0, candidate.1, candidate.2) > (current.0, current.1, current.2)
            }) {
                best = Some(candidate);
            }
        }
    }
    best.map(|(_, _, _, symbol, query, evidence)| (symbol, query, evidence))
}

fn import_seed_paths(
    candidates: &[Candidate],
    queries: &[ContextQuery],
    tokenizer: crate::tokens::Tokenizer,
) -> Vec<(String, BTreeSet<String>)> {
    if low_cardinality_exact_query(queries) {
        return Vec::new();
    }
    let mut paths = BTreeMap::<String, (f64, BTreeSet<String>)>::new();
    for candidate in candidates {
        if candidate.concept_weight < MIN_CORROBORATED_QUERY_WEIGHT || candidate.concepts.is_empty()
        {
            continue;
        }
        let token_count = candidate.token_count_with(tokenizer).max(1);
        let score = candidate.score(&ranking::Weights::default(), token_count);
        let entry = paths
            .entry(candidate.path.clone())
            .or_insert_with(|| (score, BTreeSet::new()));
        entry.0 = entry.0.max(score);
        entry.1.extend(candidate.concepts.iter().cloned());
    }
    let mut paths = paths.into_iter().collect::<Vec<_>>();
    paths.sort_by(|left, right| {
        right
            .1
            .0
            .total_cmp(&left.1.0)
            .then_with(|| left.0.cmp(&right.0))
    });
    paths
        .into_iter()
        .map(|(path, (_, concepts))| (path, concepts))
        .collect()
}

struct ImportExpansion<'a> {
    session: &'a ReadSession,
    request: &'a ContextRequest,
    queries: &'a [ContextQuery],
    terms: &'a [String],
    changed_paths: &'a HashSet<String>,
    cancellation: &'a CancellationToken,
}

fn task_mentions_language(task: &str, language: &str) -> bool {
    task.split(|character: char| !character.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .any(|word| {
            if language == "go" {
                word == "Go" || word.eq_ignore_ascii_case("golang")
            } else {
                word.eq_ignore_ascii_case(language)
            }
        })
}

impl Services {
    fn file_change_boost(
        file: Option<&FileRecord>,
        path: &str,
        changed_paths: &HashSet<String>,
        prior_generation: Option<u64>,
    ) -> f64 {
        let mut boost = 0.0;

        if let Some(prior) = prior_generation
            && file.is_some_and(|f| f.generation > prior)
        {
            boost += 1.0;
        }

        if changed_paths.contains(path) {
            boost += 1.0;
        }

        boost
    }

    fn append_import_symbol_candidates(
        &self,
        expansion: ImportExpansion<'_>,
        file_cache: &mut HashMap<String, Option<FileRecord>>,
        candidates: &mut Vec<Candidate>,
    ) -> Result<()> {
        let seed_paths = import_seed_paths(candidates, expansion.queries, self.config.tokenizer);
        let mut neighbor_count = 0usize;
        let mut neighbor_ranges = BTreeSet::new();
        for (seed_path, seed_concepts) in seed_paths.iter().take(24) {
            check_cancelled(expansion.cancellation)?;
            let Some(seed_file) = cached_file(expansion.session, file_cache, seed_path)? else {
                continue;
            };
            for import in expansion.session.get_imports_for_file(seed_file.id, 32)? {
                check_cancelled(expansion.cancellation)?;
                let Some(target_path) = import.resolved_path else {
                    continue;
                };
                if !path_allowed(&target_path, &[], &expansion.request.exclude_paths)? {
                    continue;
                }
                let Some(target_file) = cached_file(expansion.session, file_cache, &target_path)?
                else {
                    continue;
                };
                let Some((symbol, query, exact)) = corroborated_import_symbol(
                    expansion
                        .session
                        .get_symbols_for_file(target_file.id, MAX_IMPORT_SYMBOLS)?,
                    expansion.queries,
                    seed_concepts,
                ) else {
                    continue;
                };
                let Some(excerpt) = self.adaptive_context_excerpt(
                    expansion.session,
                    target_file.id,
                    symbol.start_line,
                    symbol.end_line,
                    symbol.start_line,
                    excerpt_budget(
                        expansion.request.token_budget,
                        ContextExcerptKind::ImportSymbol,
                    ),
                )?
                else {
                    continue;
                };
                if !neighbor_ranges.insert((
                    target_path.clone(),
                    excerpt.start_line,
                    excerpt.end_line,
                )) {
                    continue;
                }
                let change_boost = Self::file_change_boost(
                    Some(&target_file),
                    &target_path,
                    expansion.changed_paths,
                    expansion.request.prior_repository_generation,
                );
                let candidate = Candidate::new(
                    &target_path,
                    excerpt.start_line,
                    excerpt.end_line,
                    excerpt.content,
                )
                .match_kind("import")
                .match_kind("symbol")
                .concept(&query.fusion_key, query.concept_weight)
                .representation("import_symbol")
                .symbol_name(symbol.name)
                .exact(exact)
                .symbol(1.0)
                .path_score(context_path_score(
                    &target_path,
                    expansion.terms,
                    &expansion.request.task,
                ))
                .import_boost(1.0)
                .change_boost(change_boost);
                candidates.push(annotate_candidate(
                    candidate,
                    query,
                    "import_symbol",
                    neighbor_count,
                ));
                neighbor_count += 1;
                if neighbor_count >= 24 {
                    break;
                }
            }
            if neighbor_count >= 24 {
                break;
            }
        }
        Ok(())
    }

    /// Select ranked task evidence within an exact source-token budget.
    pub async fn context(&self, request: ContextRequest) -> Result<ContextResponse> {
        self.context_cancellable(request, CancellationToken::new())
            .await
    }

    /// Retrieve context after applying the requested index consistency boundary.
    pub async fn context_with_consistency_cancellable(
        &self,
        request: ContextRequest,
        consistency: IndexConsistency,
        cancellation: CancellationToken,
    ) -> Result<ContextResponse> {
        self.apply_consistency(consistency, cancellation.clone())
            .await?;
        self.context_cancellable(request, cancellation).await
    }

    pub async fn context_cancellable(
        &self,
        request: ContextRequest,
        cancellation: CancellationToken,
    ) -> Result<ContextResponse> {
        let this = self.clone();
        tokio::task::spawn_blocking(move || {
            this.context_sync(request, &cancellation, CandidateDiagnostics::Omit)
                .map(|evaluation| evaluation.response)
        })
        .await?
    }

    /// Retrieve context and expose pre-selection candidate paths for evaluation.
    ///
    /// Production adapters should use [`Self::context`]. This method exists for
    /// frozen retrieval benchmarks and does not alter the MCP response schema.
    pub async fn context_evaluation(&self, request: ContextRequest) -> Result<ContextEvaluation> {
        let this = self.clone();
        let cancellation = CancellationToken::new();
        tokio::task::spawn_blocking(move || {
            this.context_sync(request, &cancellation, CandidateDiagnostics::Collect)
        })
        .await?
    }

    fn context_sync(
        &self,
        request: ContextRequest,
        cancellation: &CancellationToken,
        diagnostics: CandidateDiagnostics,
    ) -> Result<ContextEvaluation> {
        check_cancelled(cancellation)?;
        if request.task.trim().is_empty() || request.token_budget == 0 {
            return Err(Error::InvalidRequest(
                "context requires a task and positive token budget".into(),
            ));
        }
        validate_input(&request.task, "task", MAX_QUERY_BYTES)?;
        validate_patterns(&request.focus_paths)?;
        validate_patterns(&request.exclude_paths)?;
        if request.focus_symbols.len() > MAX_INPUT_ITEMS {
            return Err(Error::LimitExceeded);
        }
        for symbol in &request.focus_symbols {
            validate_input(symbol, "focus symbol", MAX_PATTERN_BYTES)?;
        }
        if request.token_budget > self.config.max_output_tokens {
            return Err(Error::LimitExceeded);
        }
        if request.known_hashes.len() > MAX_INPUT_ITEMS {
            return Err(Error::LimitExceeded);
        }
        for hash in &request.known_hashes {
            validate_input(hash, "known hash", 128)?;
        }
        let changed_paths = git_changed_paths(&self.config.root, GIT_CHANGED_PATHS_MAX)
            .unwrap_or_else(|error| {
                tracing::debug!(%error, "working-tree signal unavailable");
                HashSet::new()
            });
        self.consistent(|session, generation| {
            let facet_plan = facets::plan(&request.task, MAX_CONTEXT_QUERIES);
            let queries = facet_plan.queries;
            let terms = queries
                .iter()
                .map(|query| query.value.clone())
                .collect::<Vec<_>>();
            let mut file_cache = HashMap::<String, Option<FileRecord>>::new();
            let mut candidates = Vec::new();
            let mut query_fusion = HashMap::<String, HashMap<String, f64>>::new();

            // Workflow words such as `test` are useful path priors but terrible
            // retrieval queries: nearly every test function becomes a high-
            // scoring symbol candidate. Keep them out of candidate generation.
            for query in queries
                .iter()
                .filter(|query| !query.has_facet(FacetKind::TestIntent))
            {
                let term = &query.value;
                let concept = query.fusion_key.as_str();
                check_cancelled(cancellation)?;
                for (rank, hit) in session
                    .search_symbols(term, false, MAX_CONTEXT_HITS_PER_SOURCE)?
                    .into_iter()
                    .enumerate()
                {
                    check_cancelled(cancellation)?;
                    if !path_allowed(&hit.path, &[], &request.exclude_paths)? {
                        continue;
                    }
                    let Some(excerpt) = self.adaptive_context_excerpt(
                        session,
                        hit.symbol.file_id,
                        hit.symbol.start_line,
                        hit.symbol.end_line,
                        hit.symbol.start_line,
                        excerpt_budget(request.token_budget, ContextExcerptKind::Symbol),
                    )?
                    else {
                        continue;
                    };
                    let exact = f64::from(hit.symbol.name.eq_ignore_ascii_case(term));
                    let qualified = qualified_symbol_match(
                        concept,
                        &hit.symbol.name,
                        hit.symbol.parent.as_deref(),
                        hit.symbol.signature.as_deref(),
                    );
                    if query.fuse {
                        record_query_hit(
                            &mut query_fusion,
                            &hit.path,
                            &query.fusion_key,
                            query.weight,
                            rank,
                        );
                    }
                    let file = cached_file(session, &mut file_cache, &hit.path)?;
                    let change_boost = Self::file_change_boost(
                        file.as_ref(),
                        &hit.path,
                        &changed_paths,
                        request.prior_repository_generation,
                    );
                    let candidate = Candidate::new(
                        &hit.path,
                        excerpt.start_line,
                        excerpt.end_line,
                        excerpt.content,
                    )
                    .match_kind("symbol")
                    .concept(concept, query.concept_weight)
                    .representation("symbol")
                    .symbol_name(hit.symbol.name)
                    .exact(exact + qualified * 1.5)
                    .symbol(1.0)
                    .path_score(context_path_score(&hit.path, &terms, &request.task))
                    .change_boost(change_boost);
                    candidates.push(annotate_candidate(candidate, query, "symbol", rank));
                }
                for (rank, hit) in session
                    .search_references(term, false, MAX_CONTEXT_HITS_PER_SOURCE)?
                    .into_iter()
                    .enumerate()
                {
                    check_cancelled(cancellation)?;
                    if !path_allowed(&hit.path, &[], &request.exclude_paths)? {
                        continue;
                    }
                    let excerpt = if let Some(symbol) = session
                        .find_enclosing_symbol(hit.reference.file_id, hit.reference.start_line)?
                    {
                        self.adaptive_context_excerpt(
                            session,
                            hit.reference.file_id,
                            symbol.start_line,
                            symbol.end_line,
                            hit.reference.start_line,
                            excerpt_budget(request.token_budget, ContextExcerptKind::Reference),
                        )?
                    } else {
                        None
                    };
                    let excerpt = if excerpt.is_some() {
                        excerpt
                    } else {
                        self.stored_excerpt(
                            session,
                            hit.reference.file_id,
                            hit.reference.start_line,
                            hit.reference.end_line,
                            2,
                            12,
                        )?
                    };
                    let Some(excerpt) = excerpt else {
                        continue;
                    };
                    if query.fuse {
                        record_query_hit(
                            &mut query_fusion,
                            &hit.path,
                            &query.fusion_key,
                            query.weight,
                            rank,
                        );
                    }
                    let file = cached_file(session, &mut file_cache, &hit.path)?;
                    let change_boost = Self::file_change_boost(
                        file.as_ref(),
                        &hit.path,
                        &changed_paths,
                        request.prior_repository_generation,
                    );
                    let candidate = Candidate::new(
                        &hit.path,
                        excerpt.start_line,
                        excerpt.end_line,
                        excerpt.content,
                    )
                    .match_kind("reference")
                    .concept(concept, query.concept_weight)
                    .symbol_name(hit.reference.name)
                    .reference(1.0)
                    .path_score(context_path_score(&hit.path, &terms, &request.task))
                    .change_boost(change_boost);
                    candidates.push(annotate_candidate(candidate, query, "reference", rank));
                }
                let lexical = if term.chars().count() >= 3 {
                    session.search_trigram(term, MAX_CONTEXT_LEXICAL_HITS)?
                } else {
                    session.search_word(&fts_quote(term), MAX_CONTEXT_LEXICAL_HITS)?
                };
                for (rank, hit) in lexical.into_iter().enumerate() {
                    check_cancelled(cancellation)?;
                    if !path_allowed(&hit.path, &[], &request.exclude_paths)? {
                        continue;
                    }
                    let Some(search_hit) = chunk_search_hit(hit.clone(), term, false, 2, None)?
                    else {
                        continue;
                    };
                    let matched_line =
                        matching_line(&hit, term, false).unwrap_or(search_hit.start_line);
                    let excerpt = if let Some(symbol) =
                        session.find_enclosing_symbol(hit.file_id, matched_line)?
                    {
                        self.adaptive_context_excerpt(
                            session,
                            hit.file_id,
                            symbol.start_line,
                            symbol.end_line,
                            matched_line,
                            excerpt_budget(request.token_budget, ContextExcerptKind::Text),
                        )?
                    } else {
                        None
                    }
                    .unwrap_or(StoredExcerpt {
                        content: search_hit.excerpt.clone(),
                        start_line: search_hit.start_line,
                        end_line: search_hit.end_line,
                    });
                    if query.fuse {
                        record_query_hit(
                            &mut query_fusion,
                            &search_hit.path,
                            &query.fusion_key,
                            query.weight,
                            rank,
                        );
                    }
                    let occurrences = hit
                        .content
                        .to_lowercase()
                        .matches(&term.to_lowercase())
                        .count();
                    let file = cached_file(session, &mut file_cache, &search_hit.path)?;
                    let change_boost = Self::file_change_boost(
                        file.as_ref(),
                        &search_hit.path,
                        &changed_paths,
                        request.prior_repository_generation,
                    );
                    let candidate = Candidate::new(
                        &search_hit.path,
                        excerpt.start_line,
                        excerpt.end_line,
                        excerpt.content,
                    )
                    .match_kind("text")
                    .concept(concept, query.concept_weight)
                    .exact(query.weight)
                    .bm25((-hit.score).max(0.0) * 1_000_000.0)
                    .path_score(context_path_score(&search_hit.path, &terms, &request.task))
                    .lexical_frequency_penalty(
                        (occurrences.saturating_sub(5) as f64 / 20.0).min(1.0),
                    )
                    .change_boost(change_boost);
                    candidates.push(annotate_candidate(candidate, query, "text", rank));
                }
            }

            apply_query_fusion(&mut candidates, &query_fusion);

            self.append_import_symbol_candidates(
                ImportExpansion {
                    session,
                    request: &request,
                    queries: &queries,
                    terms: &terms,
                    changed_paths: &changed_paths,
                    cancellation,
                },
                &mut file_cache,
                &mut candidates,
            )?;

            let generated_candidate_paths = if diagnostics == CandidateDiagnostics::Collect {
                candidates
                    .iter()
                    .map(|candidate| candidate.path.clone())
                    .collect::<BTreeSet<_>>()
                    .into_iter()
                    .collect()
            } else {
                Vec::new()
            };
            let generated_candidates = if diagnostics == CandidateDiagnostics::Collect {
                candidates
                    .iter()
                    .map(|candidate| {
                        let token_count = candidate.token_count_with(self.config.tokenizer).max(1);
                        ContextCandidateEvaluation {
                            path: candidate.path.clone(),
                            start_line: candidate.start_line,
                            end_line: candidate.end_line,
                            representation: candidate.representation.clone(),
                            match_kinds: candidate.match_kinds.clone(),
                            concepts: candidate.concepts.clone(),
                            concept_weight: candidate.concept_weight,
                            score: candidate.score(&ranking::Weights::default(), token_count),
                            token_count,
                        }
                    })
                    .collect()
            } else {
                Vec::new()
            };
            let mut response = ranking::select_with_tokenizer(
                candidates,
                &request,
                generation,
                self.config.tokenizer,
            );
            response.meta.freshness = self.freshness();
            if response.fragments.is_empty() {
                response
                    .warnings
                    .push("no relevant indexed evidence found".into());
            }
            Ok(ContextEvaluation {
                response,
                generated_candidate_paths,
                generated_candidates,
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn context_queries(task: &str, limit: usize) -> Vec<ContextQuery> {
        facets::plan(task, limit).queries
    }

    #[test]
    fn language_scope_does_not_treat_lowercase_go_as_golang() {
        assert!(!task_mentions_language("go fix the parser", "go"));
        assert!(task_mentions_language("fix the Go parser", "go"));
        assert!(task_mentions_language("fix the golang parser", "go"));
        assert!(task_mentions_language(
            "fix TypeScript parsing",
            "typescript"
        ));
    }

    #[test]
    fn context_queries_keep_identifiers_and_late_test_signals() {
        let terms = context_queries(
            "copy_current_request_context reuses one copied request context so calling the decorated function concurrently can corrupt state; add a regression test",
            12,
        );

        assert!(
            terms
                .iter()
                .any(|term| term.value == "copy_current_request_context")
        );
        assert!(terms.iter().any(|term| term.value == "test"));
        assert!(!terms.iter().any(|term| term.value == "one"));
    }

    #[test]
    fn context_queries_preserve_dotted_and_header_tokens() {
        let terms = context_queries(
            "Fix res.send adding Content-Length when Transfer-Encoding is present and add coverage",
            12,
        );

        assert!(terms.iter().any(|term| term.value == "res.send"));
        assert!(terms.iter().any(|term| term.value == "Content-Length"));
        assert!(terms.iter().any(|term| term.value == "Transfer-Encoding"));
        assert_eq!(terms.last().map(|term| term.value.as_str()), Some("test"));
    }

    #[test]
    fn context_queries_keep_early_domain_nouns_over_later_long_words() {
        let terms = context_queries(
            "Fix app.render and res.render for a view name ending in a dot. The callback must report the normal lookup error.",
            12,
        );

        assert!(terms.iter().any(|term| term.value == "view"));
        assert!(terms.iter().any(|term| term.value == "name"));
        assert!(terms.iter().any(|term| term.value == "ending"));
        assert!(terms.iter().any(|term| term.value == "dot"));
        assert!(!terms.iter().any(|term| term.value == "callback"));
    }

    #[test]
    fn context_queries_reserve_space_for_task_intent() {
        let terms = context_queries(
            "Fix Alpha::first_long_identifier Beta::second_long_identifier while preserving idempotency",
            12,
        );

        assert!(
            terms
                .iter()
                .any(|term| term.value == "Alpha::first_long_identifier")
        );
        assert!(
            terms
                .iter()
                .any(|term| term.value == "Beta::second_long_identifier")
        );
        assert!(terms.iter().any(|term| term.value == "idempotency"));
    }

    #[test]
    fn context_query_expansions_share_one_fusion_concept() {
        let terms = context_queries(
            "Fix GlobSet::matches_all when one compiled strategy matches",
            12,
        );
        let qualified = terms
            .iter()
            .find(|term| term.value == "GlobSet::matches_all")
            .expect("qualified query");
        let expansion = terms
            .iter()
            .find(|term| term.value != qualified.value && term.fusion_key == qualified.fusion_key)
            .expect("expanded query");

        assert_eq!(qualified.fusion_key, expansion.fusion_key);
        assert!(!qualified.fusion_key.is_empty());
    }

    #[test]
    fn candidate_diagnostics_retain_facet_and_ranked_channel_provenance() {
        let query = context_queries("Fix Rack::Deflater behavior", 12)
            .into_iter()
            .find(|query| query.value == "Rack::Deflater")
            .expect("exact technical query");
        let candidate = annotate_candidate(
            Candidate::new("src/lib.rs", 1, 1, "target").match_kind("symbol"),
            &query,
            "symbol",
            2,
        );

        assert!(
            candidate
                .match_kinds
                .iter()
                .any(|kind| kind == "facet:exact_atom:rack::deflater")
        );
        assert!(
            candidate
                .match_kinds
                .iter()
                .any(|kind| kind == "channel:symbol:2")
        );
        assert_eq!(candidate.reason(), "symbol");
    }

    #[test]
    fn low_cardinality_exact_query_disables_neighbor_expansion() {
        let exact = context_queries("Fix Rack::Deflater", 12);
        let multi = context_queries("Fix Rack::Deflater and Compression::Writer", 12);

        assert!(low_cardinality_exact_query(&exact));
        assert!(!low_cardinality_exact_query(&multi));
    }

    #[test]
    fn import_symbol_requires_the_same_seed_and_target_concept() {
        let queries = context_queries("Fix Rack::Deflater and Compression::Writer", 12);
        let deflater_query = queries
            .iter()
            .find(|query| query.fusion_key == "rack::deflater")
            .expect("deflater query");
        let symbol = SymbolRecord {
            id: 1,
            file_id: 2,
            name: "Deflater".into(),
            kind: "class".into(),
            parent: Some("Rack".into()),
            signature: Some("class Rack::Deflater".into()),
            start_line: 10,
            end_line: 20,
            start_byte: 100,
            end_byte: 200,
        };

        assert!(
            corroborated_import_symbol(vec![symbol.clone()], &queries, &BTreeSet::new()).is_none()
        );
        let matched = corroborated_import_symbol(
            vec![symbol],
            &queries,
            &BTreeSet::from([deflater_query.fusion_key.clone()]),
        )
        .expect("same-concept import symbol");
        assert_eq!(matched.0.name, "Deflater");
        assert_eq!(matched.1.fusion_key, "rack::deflater");
        assert!(matched.2 > 0.0);
    }

    #[test]
    fn qualified_symbol_match_requires_all_owner_and_name_parts() {
        assert_eq!(
            qualified_symbol_match(
                "render.AsciiJSON",
                "Render",
                None,
                Some("func (r AsciiJSON) Render() error"),
            ),
            1.0
        );
        assert_eq!(
            qualified_symbol_match(
                "render.AsciiJSON",
                "AsciiJSON",
                None,
                Some("type AsciiJSON")
            ),
            0.0
        );
        assert_eq!(
            qualified_symbol_match("Flask.run", "run", Some("Flask"), Some("def run()")),
            1.0
        );
    }

    #[test]
    fn qualified_path_evidence_excludes_dynamic_lowercase_receivers() {
        assert_eq!(
            context_path_score(
                "test/app.render.js",
                &[],
                "Fix app.render for a trailing dot",
            ),
            0.0
        );
        assert!(context_path_score("render/json.go", &[], "Fix render.AsciiJSON escaping",) > 0.0);
        assert!(
            context_path_score(
                "tokio/src/fs/file.rs",
                &[],
                "Fix tokio::fs::File poll_write",
            ) > 0.0
        );
    }

    #[test]
    fn fusion_requires_two_independent_query_concepts() {
        let mut fusion = HashMap::new();
        record_query_hit(&mut fusion, "one.rs", "globset::matches_all", 1.0, 0);
        record_query_hit(&mut fusion, "one.rs", "globset::matches_all", 0.95, 1);
        record_query_hit(&mut fusion, "two.rs", "content-length", 1.0, 0);
        record_query_hit(&mut fusion, "two.rs", "transfer-encoding", 1.0, 1);
        let mut candidates = vec![
            Candidate::new("one.rs", 1, 1, "one"),
            Candidate::new("two.rs", 1, 1, "two"),
        ];

        apply_query_fusion(&mut candidates, &fusion);

        assert_eq!(candidates[0].path_score, 0.0);
        assert!(
            !candidates[0]
                .match_kinds
                .iter()
                .any(|kind| kind == "multi-query")
        );
        assert!(candidates[1].path_score > 0.0);
        assert!(
            candidates[1]
                .match_kinds
                .iter()
                .any(|kind| kind == "multi-query")
        );
    }
}
