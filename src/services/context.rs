//! Task-shaped context candidate assembly and ranking handoff.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use tokio_util::sync::CancellationToken;

mod facets;

use super::Services;
use super::search::{chunk_search_hit, fts_quote, matching_line};
use super::validation::{
    MAX_INPUT_ITEMS, MAX_PATTERN_BYTES, MAX_QUERY_BYTES, check_cancelled, path_allowed,
    validate_input, validate_patterns,
};
use crate::model::*;
use crate::ranking::{self, Candidate, EvidenceRole};
use crate::repository::git_changed_paths;
use crate::storage::{FileRecord, ReadSession};
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
    for code_token in terms.iter().filter(|token| {
        token.contains("::")
            || token
                .split('.')
                .any(|part| part.chars().next().is_some_and(char::is_uppercase))
    }) {
        let matched_parts = expand_terms(code_token)
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

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct CandidateRange {
    path: String,
    start_line: usize,
    end_line: usize,
}

impl CandidateRange {
    fn new(path: &str, start_line: usize, end_line: usize) -> Self {
        Self {
            path: path.to_owned(),
            start_line,
            end_line,
        }
    }

    fn from_candidate(candidate: &Candidate) -> Self {
        Self::new(&candidate.path, candidate.start_line, candidate.end_line)
    }
}

fn record_query_hit(
    fusion: &mut HashMap<CandidateRange, HashMap<String, f64>>,
    range: CandidateRange,
    fusion_key: &str,
    weight: f64,
    rank: usize,
) {
    if weight < 0.65 {
        return;
    }
    const RRF_K: f64 = 60.0;
    #[allow(clippy::cast_precision_loss)]
    let score = weight * RRF_K / (RRF_K + rank as f64 + 1.0);
    fusion
        .entry(range)
        .or_default()
        .entry(fusion_key.to_owned())
        .and_modify(|current| *current = current.max(score))
        .or_insert(score);
}

fn apply_query_fusion(
    candidates: &mut [Candidate],
    fusion: &HashMap<CandidateRange, HashMap<String, f64>>,
) {
    for candidate in candidates {
        let Some(matches) = fusion.get(&CandidateRange::from_candidate(candidate)) else {
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
    allow_role_diversity: bool,
) -> Candidate {
    for facet in query.facet_names() {
        candidate = candidate.facet(facet, &query.fusion_key);
    }
    candidate = candidate.channel(channel, rank);
    for role in evidence_roles(&candidate.path, channel, query) {
        candidate = candidate.role(role);
    }
    if allow_role_diversity {
        candidate = candidate.enable_role_diversity();
    }
    if is_uncertain_path(&candidate.path) {
        candidate = candidate.role(EvidenceRole::Uncertainty);
    }
    if is_generated_snapshot(&candidate.path) {
        candidate.lexical_frequency_penalty = candidate.lexical_frequency_penalty.max(1.0);
        candidate = candidate.role(EvidenceRole::Uncertainty);
    }
    candidate
}

fn evidence_roles(path: &str, channel: &str, query: &ContextQuery) -> Vec<EvidenceRole> {
    if is_test_path(path) {
        return vec![EvidenceRole::Test];
    }
    let mut roles = Vec::new();
    if query.has_facet(FacetKind::Configuration) || is_contract_path(path) {
        roles.push(EvidenceRole::Contract);
    }
    let channel_role = match channel {
        "symbol" | "path" => EvidenceRole::Implementation,
        "reference" => EvidenceRole::Caller,
        "import" => EvidenceRole::Contract,
        _ => EvidenceRole::Implementation,
    };
    if !roles.contains(&channel_role) {
        roles.push(channel_role);
    }
    roles
}

fn is_test_path(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    let framed = format!("/{lower}");
    framed.contains("/test/")
        || framed.contains("/tests/")
        || framed.contains("/spec/")
        || framed.contains("/specs/")
        || lower.ends_with("_test.go")
        || lower.contains(".test.")
        || lower.contains(".spec.")
        || lower.starts_with("test_")
        || lower.contains("/test_")
}

fn is_contract_path(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    lower.contains("config")
        || lower.ends_with("cargo.toml")
        || lower.ends_with("package.json")
        || lower.ends_with("pyproject.toml")
        || lower.ends_with("go.mod")
}

fn is_generated_snapshot(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    lower.contains("/snapshots/")
        || lower.contains("__snapshots__")
        || lower.ends_with(".snap")
        || lower.contains("/generated/")
        || lower.ends_with(".generated.rs")
}

fn is_uncertain_path(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    let framed = format!("/{lower}");
    framed.contains("/docs/")
        || framed.contains("/doc/")
        || framed.contains("/examples/")
        || framed.contains("/example/")
        || framed.contains("/fixtures/")
        || lower == "readme.md"
        || lower.ends_with("/readme.md")
        || lower == "history.md"
        || lower.ends_with("/history.md")
        || lower.starts_with("changelog")
        || lower.contains("/changelog")
}

fn candidate_token_budget(total: usize, allow_role_diversity: bool) -> usize {
    if allow_role_diversity {
        (total / 3).clamp(128, 600).min(total)
    } else {
        total
    }
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

    /// Select ranked task evidence within an exact source-token budget.
    pub async fn context(&self, request: ContextRequest) -> Result<ContextResponse> {
        self.context_cancellable(request, CancellationToken::new())
            .await
    }

    pub async fn context_cancellable(
        &self,
        request: ContextRequest,
        cancellation: CancellationToken,
    ) -> Result<ContextResponse> {
        let this = self.clone();
        tokio::task::spawn_blocking(move || this.context_sync(request, &cancellation)).await?
    }

    fn context_sync(
        &self,
        request: ContextRequest,
        cancellation: &CancellationToken,
    ) -> Result<ContextResponse> {
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
            let mut facet_plan = facets::plan(&request.task, MAX_CONTEXT_QUERIES);
            let queries = facet_plan.queries.clone();
            let terms = queries
                .iter()
                .map(|query| query.value.clone())
                .collect::<Vec<_>>();
            let mut file_cache = HashMap::<String, Option<FileRecord>>::new();
            let mut candidates = Vec::new();
            let candidate_budget =
                candidate_token_budget(request.token_budget, facet_plan.allow_role_diversity);
            let mut query_fusion = HashMap::<CandidateRange, HashMap<String, f64>>::new();
            let mut resolved_facets = HashSet::new();
            let mut candidate_counts = BTreeMap::<String, usize>::new();

            for path_facet in facet_plan
                .facets
                .iter()
                .filter(|facet| facet.kind == FacetKind::Path)
            {
                let Some(query) = queries
                    .iter()
                    .find(|query| query.fusion_key == path_facet.fusion_key)
                else {
                    continue;
                };
                let path = path_facet.original.trim_start_matches("./");
                if !path_allowed(path, &[], &request.exclude_paths)? {
                    continue;
                }
                let Some(file) = cached_file(session, &mut file_cache, path)? else {
                    continue;
                };
                let Some(chunk) = session.get_chunks_for_file(file.id, 1)?.into_iter().next()
                else {
                    continue;
                };
                let Some(excerpt) = self.adaptive_context_excerpt(
                    session,
                    file.id,
                    chunk.start_line,
                    chunk.end_line,
                    chunk.start_line,
                    candidate_budget,
                )?
                else {
                    continue;
                };
                let change_boost = Self::file_change_boost(
                    Some(&file),
                    path,
                    &changed_paths,
                    request.prior_repository_generation,
                );
                let candidate =
                    Candidate::new(path, excerpt.start_line, excerpt.end_line, excerpt.content)
                        .match_kind("path")
                        .concept(&query.fusion_key, query.concept_weight)
                        .exact(1.0)
                        .path_score(context_path_score(path, &terms, &request.task) + 1.0)
                        .change_boost(change_boost);
                candidates.push(annotate_candidate(
                    candidate,
                    query,
                    "path",
                    0,
                    facet_plan.allow_role_diversity,
                ));
                resolved_facets.insert(query.fusion_key.clone());
                *candidate_counts
                    .entry(format!("{}:path", query.fusion_key))
                    .or_default() += 1;
            }

            // Workflow words such as `test` are useful path priors but terrible
            // retrieval queries: nearly every test function becomes a high-
            // scoring symbol candidate. Keep them out of candidate generation.
            for query in queries
                .iter()
                .filter(|query| !query.has_facet(FacetKind::TestIntent))
            {
                let term = &query.value;
                let concept = query.fusion_key.as_str();
                let mut query_hit = false;
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
                        candidate_budget,
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
                    record_query_hit(
                        &mut query_fusion,
                        CandidateRange::new(&hit.path, excerpt.start_line, excerpt.end_line),
                        &query.fusion_key,
                        query.weight,
                        rank,
                    );
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
                    candidates.push(annotate_candidate(
                        candidate,
                        query,
                        "symbol",
                        rank,
                        facet_plan.allow_role_diversity,
                    ));
                    query_hit = true;
                    *candidate_counts
                        .entry(format!("{}:symbol", query.fusion_key))
                        .or_default() += 1;
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
                            candidate_budget,
                        )?
                    } else {
                        None
                    };
                    let excerpt = if excerpt.is_some() {
                        excerpt
                    } else {
                        self.adaptive_context_excerpt(
                            session,
                            hit.reference.file_id,
                            hit.reference.start_line.saturating_sub(2).max(1),
                            hit.reference.end_line.saturating_add(2),
                            hit.reference.start_line,
                            candidate_budget,
                        )?
                    };
                    let Some(excerpt) = excerpt else {
                        continue;
                    };
                    record_query_hit(
                        &mut query_fusion,
                        CandidateRange::new(&hit.path, excerpt.start_line, excerpt.end_line),
                        &query.fusion_key,
                        query.weight,
                        rank,
                    );
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
                    candidates.push(annotate_candidate(
                        candidate,
                        query,
                        "reference",
                        rank,
                        facet_plan.allow_role_diversity,
                    ));
                    query_hit = true;
                    *candidate_counts
                        .entry(format!("{}:reference", query.fusion_key))
                        .or_default() += 1;
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
                            candidate_budget,
                        )?
                    } else {
                        self.adaptive_context_excerpt(
                            session,
                            hit.file_id,
                            search_hit.start_line,
                            search_hit.end_line,
                            matched_line,
                            candidate_budget,
                        )?
                    };
                    let Some(excerpt) = excerpt else {
                        continue;
                    };
                    record_query_hit(
                        &mut query_fusion,
                        CandidateRange::new(&search_hit.path, excerpt.start_line, excerpt.end_line),
                        &query.fusion_key,
                        query.weight,
                        rank,
                    );
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
                    candidates.push(annotate_candidate(
                        candidate,
                        query,
                        "text",
                        rank,
                        facet_plan.allow_role_diversity,
                    ));
                    query_hit = true;
                    *candidate_counts
                        .entry(format!("{}:text", query.fusion_key))
                        .or_default() += 1;
                }
                if query_hit {
                    resolved_facets.insert(query.fusion_key.clone());
                }
            }

            apply_query_fusion(&mut candidates, &query_fusion);

            let seed_paths = candidates
                .iter()
                .map(|candidate| candidate.path.clone())
                .collect::<BTreeSet<_>>();
            let mut neighbor_count = 0usize;
            for seed_path in seed_paths.iter().take(24) {
                check_cancelled(cancellation)?;
                let Some(seed_file) = cached_file(session, &mut file_cache, seed_path)? else {
                    continue;
                };
                for import in session.get_imports_for_file(seed_file.id, 32)? {
                    check_cancelled(cancellation)?;
                    let Some(target_path) = import.resolved_path else {
                        continue;
                    };
                    if !path_allowed(&target_path, &[], &request.exclude_paths)? {
                        continue;
                    }
                    let Some(target_file) = cached_file(session, &mut file_cache, &target_path)?
                    else {
                        continue;
                    };
                    let Some(chunk) = session
                        .get_chunks_for_file(target_file.id, 1)?
                        .into_iter()
                        .next()
                    else {
                        continue;
                    };
                    let target_path_score = context_path_score(&target_path, &terms, &request.task);
                    let import_haystack =
                        format!("{}\n{}\n{}", import.raw_target, target_path, chunk.content)
                            .to_ascii_lowercase();
                    let Some(query) = queries.iter().find(|query| {
                        !query.has_facet(FacetKind::TestIntent)
                            && query.value.chars().count() >= 3
                            && import_haystack.contains(&query.value.to_ascii_lowercase())
                    }) else {
                        continue;
                    };
                    let Some(excerpt) = self.adaptive_context_excerpt(
                        session,
                        target_file.id,
                        chunk.start_line,
                        chunk.end_line,
                        chunk.start_line,
                        candidate_budget,
                    )?
                    else {
                        continue;
                    };
                    let change_boost = Self::file_change_boost(
                        Some(&target_file),
                        &target_path,
                        &changed_paths,
                        request.prior_repository_generation,
                    );
                    let candidate = Candidate::new(
                        &target_path,
                        excerpt.start_line,
                        excerpt.end_line,
                        excerpt.content,
                    )
                    .match_kind("import")
                    .concept(&query.fusion_key, query.concept_weight.min(1.0))
                    .representation("import_neighbor")
                    .path_score(target_path_score)
                    .import_boost(1.0)
                    .change_boost(change_boost);
                    candidates.push(annotate_candidate(
                        candidate,
                        query,
                        "import",
                        neighbor_count,
                        facet_plan.allow_role_diversity,
                    ));
                    *candidate_counts
                        .entry(format!("{}:import", query.fusion_key))
                        .or_default() += 1;
                    neighbor_count += 1;
                    if neighbor_count >= 24 {
                        break;
                    }
                }
                if neighbor_count >= 24 {
                    break;
                }
            }

            for fusion_key in resolved_facets {
                facet_plan.mark_resolved(&fusion_key);
            }
            let extracted_facets = facet_plan
                .facets
                .iter()
                .map(|facet| format!("{}:{}", facet.kind.as_str(), facet.original))
                .collect::<Vec<_>>();
            let unresolved_facets = facet_plan
                .unresolved()
                .map(|facet| format!("{}:{}", facet.kind.as_str(), facet.original))
                .collect::<Vec<_>>();
            let candidate_provenance_count = candidates
                .iter()
                .map(|candidate| candidate.provenance().count())
                .sum::<usize>();
            let mut roles_by_range = HashMap::<CandidateRange, BTreeSet<String>>::new();
            for candidate in &candidates {
                roles_by_range
                    .entry(CandidateRange::from_candidate(candidate))
                    .or_default()
                    .extend(candidate.role_names().map(str::to_owned));
            }
            let mut response = ranking::select_with_tokenizer(
                candidates,
                &request,
                generation,
                self.config.tokenizer,
            );
            let selected_roles = response
                .fragments
                .iter()
                .flat_map(|fragment| {
                    roles_by_range
                        .get(&CandidateRange::new(
                            &fragment.path,
                            fragment.start_line,
                            fragment.end_line,
                        ))
                        .into_iter()
                        .flatten()
                })
                .cloned()
                .collect::<BTreeSet<_>>();
            tracing::debug!(
                ?extracted_facets,
                ?unresolved_facets,
                wants_tests = facet_plan.wants_tests,
                role_diversity = facet_plan.allow_role_diversity,
                ?candidate_counts,
                candidate_provenance_count,
                ?selected_roles,
                selected_fragments = response.fragments.len(),
                omitted_candidates = response.omitted.len(),
                "assembled task evidence portfolio"
            );
            response.meta.freshness = self.freshness();
            if response.fragments.is_empty() {
                response
                    .warnings
                    .push("no relevant indexed evidence found".into());
            }
            Ok(response)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let terms = facets::plan(
            "copy_current_request_context reuses one copied request context so calling the decorated function concurrently can corrupt state; add a regression test",
            12,
        )
        .queries;

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
        let terms = facets::plan(
            "Fix res.send adding Content-Length when Transfer-Encoding is present and add coverage",
            12,
        )
        .queries;

        assert!(terms.iter().any(|term| term.value == "res.send"));
        assert!(terms.iter().any(|term| term.value == "Content-Length"));
        assert!(terms.iter().any(|term| term.value == "Transfer-Encoding"));
        assert_eq!(terms.last().map(|term| term.value.as_str()), Some("test"));
    }

    #[test]
    fn context_queries_keep_early_domain_nouns_over_later_long_words() {
        let terms = facets::plan(
            "Fix app.render and res.render for a view name ending in a dot. The callback must report the normal lookup error.",
            12,
        )
        .queries;

        assert!(terms.iter().any(|term| term.value == "view"));
        assert!(terms.iter().any(|term| term.value == "name"));
        assert!(terms.iter().any(|term| term.value == "ending"));
        assert!(terms.iter().any(|term| term.value == "dot"));
        assert!(!terms.iter().any(|term| term.value == "callback"));
    }

    #[test]
    fn context_queries_reserve_space_for_task_intent() {
        let terms = facets::plan(
            "Fix Alpha::first_long_identifier Beta::second_long_identifier while preserving idempotency",
            12,
        )
        .queries;

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
        let terms = facets::plan(
            "Fix GlobSet::matches_all when one compiled strategy matches",
            12,
        )
        .queries;
        let qualified = terms
            .iter()
            .find(|term| term.value == "GlobSet::matches_all")
            .expect("qualified query");
        let expansion = terms
            .iter()
            .find(|term| term.value != qualified.value && term.fusion_key == qualified.fusion_key)
            .expect("expanded query");

        assert_eq!(qualified.fusion_key, expansion.fusion_key);
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
        assert!(
            context_path_score(
                "render/json.go",
                &["render.AsciiJSON".into()],
                "Fix render.AsciiJSON escaping",
            ) > 0.0
        );
        assert!(
            context_path_score(
                "tokio/src/fs/file.rs",
                &["tokio::fs::File".into()],
                "Fix tokio::fs::File poll_write",
            ) > 0.0
        );
    }

    #[test]
    fn fusion_requires_two_independent_query_concepts() {
        let mut fusion = HashMap::new();
        record_query_hit(
            &mut fusion,
            CandidateRange::new("one.rs", 1, 1),
            "globset::matches_all",
            1.0,
            0,
        );
        record_query_hit(
            &mut fusion,
            CandidateRange::new("one.rs", 1, 1),
            "globset::matches_all",
            0.95,
            1,
        );
        record_query_hit(
            &mut fusion,
            CandidateRange::new("two.rs", 1, 1),
            "content-length",
            1.0,
            0,
        );
        record_query_hit(
            &mut fusion,
            CandidateRange::new("two.rs", 1, 1),
            "transfer-encoding",
            1.0,
            1,
        );
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

    #[test]
    fn fusion_is_scoped_to_the_matching_range_not_the_whole_path() {
        let mut fusion = HashMap::new();
        record_query_hit(
            &mut fusion,
            CandidateRange::new("shared.rs", 1, 3),
            "alpha",
            1.0,
            0,
        );
        record_query_hit(
            &mut fusion,
            CandidateRange::new("shared.rs", 1, 3),
            "beta",
            1.0,
            0,
        );
        record_query_hit(
            &mut fusion,
            CandidateRange::new("shared.rs", 20, 22),
            "alpha",
            1.0,
            0,
        );
        let mut candidates = vec![
            Candidate::new("shared.rs", 1, 3, "first"),
            Candidate::new("shared.rs", 20, 22, "second"),
        ];

        apply_query_fusion(&mut candidates, &fusion);

        assert!(candidates[0].path_score > 0.0);
        assert_eq!(candidates[1].path_score, 0.0);
    }
}
