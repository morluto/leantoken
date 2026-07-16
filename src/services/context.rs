//! Task-shaped context candidate assembly and ranking handoff.

use std::collections::{BTreeSet, HashMap, HashSet};

use tokio_util::sync::CancellationToken;

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
use crate::storage::{FileRecord, ReadSession};
use crate::text::{expand_terms, identifier_words};
use crate::{Error, Result};

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
    for code_token in code_tokens(task).into_iter().filter(|token| {
        token.contains("::")
            || token
                .split('.')
                .any(|part| part.chars().next().is_some_and(char::is_uppercase))
    }) {
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

#[derive(Debug, Clone, PartialEq)]
struct ContextQuery {
    value: String,
    weight: f64,
    fusion_key: Option<String>,
}

fn context_queries(task: &str, limit: usize) -> Vec<ContextQuery> {
    if limit == 0 {
        return Vec::new();
    }
    let wants_tests = task_terms(task).iter().any(|term| is_test_term(term));
    let available = limit.saturating_sub(usize::from(wants_tests));
    let code_terms = code_tokens(task);
    let code_parts = code_terms
        .iter()
        .flat_map(|term| std::iter::once(term.clone()).chain(expand_terms(term)))
        .map(|term| term.to_ascii_lowercase())
        .collect::<HashSet<_>>();
    let mut prose = task_terms(task)
        .into_iter()
        .filter(|value| {
            !is_test_term(value)
                && !is_context_stop_word(value)
                && !code_parts.contains(&value.to_ascii_lowercase())
        })
        .enumerate()
        .collect::<Vec<_>>();
    prose.sort_by(|(left_index, left), (right_index, right)| {
        context_query_weight(right, false)
            .total_cmp(&context_query_weight(left, false))
            .then_with(|| left_index.cmp(right_index))
    });

    let prose_reserve = prose.len().min(4).min(available);
    let exact_limit = available.saturating_sub(prose_reserve);
    let mut seen = HashSet::new();
    let mut terms = Vec::new();
    for code_term in code_terms.iter().take(exact_limit) {
        push_context_query(
            &mut terms,
            &mut seen,
            code_term.clone(),
            true,
            Some(code_term.to_ascii_lowercase()),
        );
    }
    for (_, value) in prose.iter().take(prose_reserve) {
        push_context_query(&mut terms, &mut seen, value.clone(), false, None);
    }

    let mut expansion_round = 0usize;
    while terms.len() < available {
        let before = terms.len();
        for code_term in &code_terms {
            let expansions = expand_terms(code_term);
            if let Some(value) = expansions.get(expansion_round) {
                push_context_query(
                    &mut terms,
                    &mut seen,
                    value.clone(),
                    true,
                    Some(code_term.to_ascii_lowercase()),
                );
                if terms.len() == available {
                    break;
                }
            }
        }
        if terms.len() == before {
            break;
        }
        expansion_round += 1;
    }
    if wants_tests {
        terms.push(ContextQuery {
            value: "test".into(),
            weight: 0.2,
            fusion_key: None,
        });
    }
    terms
}

fn push_context_query(
    terms: &mut Vec<ContextQuery>,
    seen: &mut HashSet<String>,
    value: String,
    explicit_code_token: bool,
    fusion_key: Option<String>,
) {
    if value.chars().count() < 2
        || is_context_stop_word(&value)
        || !seen.insert(value.to_ascii_lowercase())
    {
        return;
    }
    terms.push(ContextQuery {
        weight: context_query_weight(&value, explicit_code_token),
        value,
        fusion_key,
    });
}

fn task_terms(task: &str) -> Vec<String> {
    task.split(|character: char| !character.is_alphanumeric() && character != '_')
        .filter(|value| value.chars().count() >= 2)
        .map(str::to_owned)
        .collect()
}

fn is_test_term(term: &str) -> bool {
    matches!(
        term.to_ascii_lowercase().as_str(),
        "test" | "tests" | "testing" | "coverage" | "regression"
    )
}

fn context_query_weight(term: &str, explicit_code_token: bool) -> f64 {
    if explicit_code_token {
        return if term.contains(['_', ':', '.', '-']) {
            1.0
        } else {
            0.95
        };
    }
    if term.contains(['_', ':', '.', '-']) {
        return 0.9;
    }
    match term.chars().count() {
        10.. => 0.8,
        7..=9 => 0.65,
        4..=6 => 0.45,
        _ => 0.25,
    }
}

fn record_query_hit(
    fusion: &mut HashMap<String, HashMap<String, f64>>,
    path: &str,
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

fn code_tokens(task: &str) -> Vec<String> {
    task.split_whitespace()
        .map(|token| {
            token.trim_matches(|character: char| !character.is_alphanumeric() && character != '_')
        })
        .filter(|token| {
            token.contains('_')
                || token.contains("::")
                || token.contains('.')
                || (token.contains('-') && token.chars().any(char::is_uppercase))
        })
        .map(str::to_owned)
        .collect()
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

fn is_context_stop_word(term: &str) -> bool {
    matches!(
        term.to_ascii_lowercase().as_str(),
        "a" | "an"
            | "and"
            | "add"
            | "adding"
            | "are"
            | "as"
            | "be"
            | "before"
            | "both"
            | "but"
            | "by"
            | "calling"
            | "can"
            | "change"
            | "does"
            | "each"
            | "fix"
            | "for"
            | "from"
            | "if"
            | "in"
            | "into"
            | "is"
            | "it"
            | "its"
            | "make"
            | "not"
            | "of"
            | "on"
            | "one"
            | "only"
            | "or"
            | "same"
            | "so"
            | "than"
            | "then"
            | "the"
            | "this"
            | "to"
            | "update"
            | "when"
            | "while"
            | "within"
            | "without"
            | "with"
    )
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

    pub(super) fn context_sync(
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
            let queries = context_queries(&request.task, MAX_CONTEXT_QUERIES);
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
            for query in queries.iter().filter(|query| query.value != "test") {
                let term = &query.value;
                let concept = query.fusion_key.as_deref().unwrap_or(term);
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
                        request.token_budget,
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
                    if let Some(fusion_key) = &query.fusion_key {
                        record_query_hit(
                            &mut query_fusion,
                            &hit.path,
                            fusion_key,
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
                    candidates.push(
                        Candidate::new(
                            &hit.path,
                            excerpt.start_line,
                            excerpt.end_line,
                            excerpt.content,
                        )
                        .match_kind("symbol")
                        .concept(
                            concept,
                            query.weight + f64::from(query.fusion_key.is_some()),
                        )
                        .representation("symbol")
                        .symbol_name(hit.symbol.name)
                        .exact(exact + qualified * 1.5)
                        .symbol(1.0)
                        .path_score(context_path_score(&hit.path, &terms, &request.task))
                        .change_boost(change_boost),
                    );
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
                            request.token_budget,
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
                    if let Some(fusion_key) = &query.fusion_key {
                        record_query_hit(
                            &mut query_fusion,
                            &hit.path,
                            fusion_key,
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
                    candidates.push(
                        Candidate::new(
                            &hit.path,
                            excerpt.start_line,
                            excerpt.end_line,
                            excerpt.content,
                        )
                        .match_kind("reference")
                        .concept(
                            concept,
                            query.weight + f64::from(query.fusion_key.is_some()),
                        )
                        .symbol_name(hit.reference.name)
                        .reference(1.0)
                        .path_score(context_path_score(&hit.path, &terms, &request.task))
                        .change_boost(change_boost),
                    );
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
                            request.token_budget,
                        )?
                    } else {
                        None
                    }
                    .unwrap_or(StoredExcerpt {
                        content: search_hit.excerpt.clone(),
                        start_line: search_hit.start_line,
                        end_line: search_hit.end_line,
                    });
                    if let Some(fusion_key) = &query.fusion_key {
                        record_query_hit(
                            &mut query_fusion,
                            &search_hit.path,
                            fusion_key,
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
                    .concept(
                        concept,
                        query.weight + f64::from(query.fusion_key.is_some()),
                    )
                    .exact(query.weight)
                    .bm25((-hit.score).max(0.0) * 1_000_000.0)
                    .path_score(context_path_score(&search_hit.path, &terms, &request.task))
                    .lexical_frequency_penalty(
                        (occurrences.saturating_sub(5) as f64 / 20.0).min(1.0),
                    )
                    .change_boost(change_boost);
                    candidates.push(candidate);
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
                    let end_line = chunk.end_line.min(chunk.start_line + 29);
                    let content = crate::text::excerpt(
                        &chunk.content,
                        1,
                        end_line.saturating_sub(chunk.start_line) + 1,
                    );
                    let change_boost = Self::file_change_boost(
                        Some(&target_file),
                        &target_path,
                        &changed_paths,
                        request.prior_repository_generation,
                    );
                    candidates.push(
                        Candidate::new(&target_path, chunk.start_line, end_line, content)
                            .match_kind("import")
                            .concept(seed_path, 0.2)
                            .representation("import_neighbor")
                            .path_score(context_path_score(&target_path, &terms, &request.task))
                            .import_boost(1.0)
                            .change_boost(change_boost),
                    );
                    neighbor_count += 1;
                    if neighbor_count >= 24 {
                        break;
                    }
                }
                if neighbor_count >= 24 {
                    break;
                }
            }

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
        assert!(qualified.fusion_key.is_some());
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
