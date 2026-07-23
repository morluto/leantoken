//! Task-shaped context candidate assembly and ranking handoff.

use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    sync::LazyLock,
};

use globset::{GlobBuilder, GlobSet, GlobSetBuilder};
use tokio_util::sync::CancellationToken;

mod facets;

use super::Services;
use super::read::{AdaptiveExcerptRequest, StoredExcerpt, StoredExcerptRequest};
use super::search::{chunk_search_hit, compile_literal_regex, fts_quote, matching_line};
use super::validation::{
    MAX_INPUT_ITEMS, MAX_PATH_BYTES, MAX_PATTERN_BYTES, MAX_QUERY_BYTES, check_cancelled,
    path_allowed, validate_glob_patterns, validate_input,
};
use crate::model::*;
use crate::ranking::{self, Candidate};
use crate::repository::{
    git_changed_paths, git_diff_hunks, git_diff_paths, normalize_relative, validate_relative,
};
use crate::storage::{ReadSession, SymbolRecord};
use crate::text::{expand_terms, identifier_words};
use crate::{Error, Result};
use facets::{ContextQuery, FacetKind};
const GIT_CHANGED_PATHS_MAX: usize = 512;
/// Maximum explicit changed paths accepted from a diff-scoped request.
const MAX_DIFF_CHANGED_PATHS: usize = 512;
/// Maximum bytes for a base revision string.
const MAX_BASE_REVISION_BYTES: usize = 256;
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
const MAX_DIFF_EVIDENCE_SYMBOLS: usize = 64;
const MAX_DIFF_EVIDENCE_RELATIONSHIPS: usize = 64;
const MAX_DIFF_EVIDENCE_PATHS: usize = 64;
const MAX_WORKFLOW_SCAN_FILES: usize = 8_192;
const MAX_OWNER_TEST_SCAN_FILES: usize = 4_096;
const MAX_REFERENCES_PER_CHANGED_SYMBOL: usize = 8;

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
    for (language, component, extensions) in [
        ("javascript", "/js/", &["js", "jsx", "mjs", "cjs"][..]),
        ("typescript", "/ts/", &["ts", "tsx"][..]),
        ("python", "/python/", &["py", "pyw"][..]),
        ("rust", "/rust/", &["rs"][..]),
        ("go", "/go/", &["go"][..]),
    ] {
        let extension_matches = path
            .rsplit_once('.')
            .is_some_and(|(_, extension)| extensions.contains(&extension));
        if task_mentions_language(task, language)
            && (extension_matches || format!("/{path}/").contains(component))
        {
            // An explicit language name in the task is strong repository-scope
            // evidence. Keep this above an exact-name match in another
            // language so common names such as `Point` do not dominate.
            score += 12.0;
        }
    }
    score
}

fn resolve_context_workflow(requested: ContextWorkflow, task: &str) -> ContextWorkflow {
    if requested != ContextWorkflow::Auto {
        return requested;
    }
    let task = task.to_ascii_lowercase();
    if ["pull request", "contribution", "contributing"]
        .iter()
        .any(|signal| task.contains(signal))
    {
        ContextWorkflow::Contribution
    } else if ["code review", "review this", "review the changes"]
        .iter()
        .any(|signal| task.contains(signal))
    {
        ContextWorkflow::Review
    } else if ["investigate", "root cause", "diagnose"]
        .iter()
        .any(|signal| task.contains(signal))
    {
        ContextWorkflow::Investigation
    } else {
        ContextWorkflow::Implementation
    }
}

fn workflow_path_role(path: &str, request: &ContextRequest) -> Option<(f64, &'static str)> {
    let normalized = path.replace('\\', "/");
    let lower = normalized.to_ascii_lowercase();
    let mut best = WORKFLOW_PATHS
        .matches(&normalized)
        .into_iter()
        .map(|index| WORKFLOW_PATH_ROLES[index])
        .max_by(|left, right| left.0.total_cmp(&right.0));
    if likely_owner_test(&lower, request) && best.is_none_or(|(score, _)| score < OWNER_TEST_SCORE)
    {
        best = Some((OWNER_TEST_SCORE, "owner_test"));
    }
    best
}

const OWNER_TEST_SCORE: f64 = 3.75;
const WORKFLOW_PATH_RULES: [(&str, f64, &str); 15] = [
    ("AGENTS.md", 5.0, "guidance"),
    ("**/AGENTS.md", 5.0, "guidance"),
    ("CONTRIBUTING*", 5.0, "guidance"),
    ("**/CONTRIBUTING*", 5.0, "guidance"),
    (".github/PULL_REQUEST_TEMPLATE*", 5.0, "guidance"),
    (".github/PULL_REQUEST_TEMPLATE/**", 5.0, "guidance"),
    (".github/ISSUE_TEMPLATE/**", 4.5, "template"),
    ("docs/development*", 4.25, "guidance"),
    ("docs/contributing*", 4.25, "guidance"),
    ("docs/testing*", 4.25, "guidance"),
    (".github/workflows/**", 3.0, "validation"),
    ("Cargo.toml", 3.0, "validation"),
    ("Makefile", 3.0, "validation"),
    ("justfile", 3.0, "validation"),
    ("{package.json,pyproject.toml,go.mod}", 3.0, "validation"),
];
const WORKFLOW_PATH_ROLES: [(f64, &str); WORKFLOW_PATH_RULES.len()] = workflow_path_roles();

const fn workflow_path_roles() -> [(f64, &'static str); WORKFLOW_PATH_RULES.len()] {
    let mut roles = [(0.0, ""); WORKFLOW_PATH_RULES.len()];
    let mut index = 0;
    while index < WORKFLOW_PATH_RULES.len() {
        roles[index] = (WORKFLOW_PATH_RULES[index].1, WORKFLOW_PATH_RULES[index].2);
        index += 1;
    }
    roles
}

static WORKFLOW_PATHS: LazyLock<GlobSet> = LazyLock::new(|| {
    let mut builder = GlobSetBuilder::new();
    for (pattern, _, _) in WORKFLOW_PATH_RULES {
        builder.add(
            GlobBuilder::new(pattern)
                .case_insensitive(true)
                .literal_separator(true)
                .build()
                .expect("static workflow glob"),
        );
    }
    builder.build().expect("static workflow glob set")
});

fn likely_owner_test(path: &str, request: &ContextRequest) -> bool {
    owner_test_changed_path(path, request).is_some()
}

fn owner_test_changed_path(path: &str, request: &ContextRequest) -> Option<String> {
    let path = path.replace('\\', "/").to_ascii_lowercase();
    let is_test = path.contains("/test")
        || path.starts_with("test")
        || path.contains("/spec")
        || path.starts_with("spec");
    if !is_test {
        return None;
    }
    let test_name = path.rsplit('/').next().unwrap_or(&path);
    let test_stem = test_name.split('.').next().unwrap_or(test_name);
    request
        .changed_paths
        .iter()
        .chain(&request.focus_paths)
        .find_map(|changed| {
            let normalized = changed.replace('\\', "/");
            let stem = normalized
                .rsplit('/')
                .next()
                .and_then(|name| name.split('.').next())?
                .to_ascii_lowercase();
            (stem.len() >= 3 && contains_filename_token(test_stem, &stem)).then(|| changed.clone())
        })
}

fn contains_filename_token(path: &str, token: &str) -> bool {
    path.match_indices(token).any(|(start, matched)| {
        let end = start + matched.len();
        let boundary =
            |character: Option<char>| character.is_none_or(|value| !value.is_alphanumeric());
        boundary(path[..start].chars().next_back()) && boundary(path[end..].chars().next())
    })
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum CandidateDiagnostics {
    Omit,
    Collect,
}

#[derive(Clone, Copy)]
struct ContextSignals {
    import_neighbor: bool,
    reverse_dependency: bool,
    caller: bool,
}

impl ContextSignals {
    const PRODUCTION: Self = Self {
        import_neighbor: true,
        reverse_dependency: false,
        caller: true,
    };

    const fn evaluation(policy: ContextSignalPolicy) -> Self {
        match policy {
            ContextSignalPolicy::LexicalSyntax => Self {
                import_neighbor: false,
                reverse_dependency: false,
                caller: false,
            },
            ContextSignalPolicy::ImportNeighbor => Self {
                import_neighbor: true,
                reverse_dependency: false,
                caller: false,
            },
            ContextSignalPolicy::ReverseDependency => Self {
                import_neighbor: false,
                reverse_dependency: true,
                caller: false,
            },
            ContextSignalPolicy::HighConfidenceCaller => Self {
                import_neighbor: false,
                reverse_dependency: false,
                caller: true,
            },
        }
    }
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
    fn validate_context_request(&self, request: &ContextRequest) -> Result<()> {
        if request.task.trim().is_empty() {
            return Err(Error::InvalidInput {
                field: "task",
                reason: "must not be empty",
            });
        }
        self.token_budget_limit(request.token_budget)?;
        validate_input(&request.task, "task", MAX_QUERY_BYTES)?;
        validate_glob_patterns(&request.focus_paths)?;
        validate_glob_patterns(&request.exclude_paths)?;
        if request.focus_symbols.len() > MAX_INPUT_ITEMS {
            return Err(Error::LimitExceeded);
        }
        for symbol in &request.focus_symbols {
            validate_input(symbol, "focus symbol", MAX_PATTERN_BYTES)?;
        }
        if request.known_hashes.len() > MAX_INPUT_ITEMS {
            return Err(Error::LimitExceeded);
        }
        for hash in &request.known_hashes {
            validate_input(hash, "known hash", 128)?;
        }
        if request.changed_paths.len() > MAX_DIFF_CHANGED_PATHS {
            return Err(Error::LimitExceeded);
        }
        for path in &request.changed_paths {
            validate_input(path, "changed path", MAX_PATH_BYTES)?;
            validate_relative(path)?;
        }
        if let Some(revision) = request
            .base_revision
            .as_deref()
            .filter(|revision| !revision.trim().is_empty())
        {
            validate_input(revision, "base revision", MAX_BASE_REVISION_BYTES)?;
        }
        for query in facets::plan(&request.task, MAX_CONTEXT_QUERIES)
            .queries
            .iter()
            .filter(|query| !query.has_facet(FacetKind::TestIntent))
        {
            compile_literal_regex(&query.value, false)?;
        }
        Ok(())
    }

    fn file_change_boost(
        file_generation: Option<u64>,
        path: &str,
        changed_paths: &HashSet<String>,
        prior_generation: Option<u64>,
    ) -> f64 {
        let mut boost = 0.0;

        if let Some(prior) = prior_generation
            && file_generation.is_some_and(|generation| generation > prior)
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
        candidates: &mut Vec<Candidate>,
    ) -> Result<()> {
        let seed_paths = import_seed_paths(candidates, expansion.queries, self.config.tokenizer);
        let requested_paths = seed_paths
            .iter()
            .take(24)
            .map(|(path, _)| path.clone())
            .collect::<Vec<_>>();
        let targets =
            expansion
                .session
                .import_symbol_targets(&requested_paths, 32, MAX_IMPORT_SYMBOLS)?;
        let mut pending = Vec::new();
        for target in targets {
            check_cancelled(expansion.cancellation)?;
            let Some((_, seed_concepts)) = seed_paths.get(target.seed_index) else {
                continue;
            };
            let target_path = &target.target_file.path;
            if !path_allowed(target_path, &[], &expansion.request.exclude_paths)? {
                continue;
            }
            let Some((symbol, query, exact)) =
                corroborated_import_symbol(target.symbols, expansion.queries, seed_concepts)
            else {
                continue;
            };
            pending.push((target.target_file, symbol, query.clone(), exact));
        }
        let excerpt_requests = pending
            .iter()
            .map(|(target_file, symbol, _, _)| AdaptiveExcerptRequest {
                file_id: target_file.id,
                declaration_start: symbol.start_line,
                declaration_end: symbol.end_line,
                matched_line: symbol.start_line,
                token_budget: excerpt_budget(
                    expansion.request.token_budget,
                    ContextExcerptKind::ImportSymbol,
                ),
            })
            .collect::<Vec<_>>();
        let excerpts = self.adaptive_context_excerpts(expansion.session, &excerpt_requests)?;
        let mut neighbor_count = 0usize;
        let mut neighbor_ranges = BTreeSet::new();
        for ((target_file, symbol, query, exact), excerpt) in pending.into_iter().zip(excerpts) {
            check_cancelled(expansion.cancellation)?;
            let Some(excerpt) = excerpt else { continue };
            let target_path = target_file.path;
            if !neighbor_ranges.insert((target_path.clone(), excerpt.start_line, excerpt.end_line))
            {
                continue;
            }
            let change_boost = Self::file_change_boost(
                Some(target_file.generation),
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
                &query,
                "import_symbol",
                neighbor_count,
            ));
            neighbor_count += 1;
            if neighbor_count >= 24 {
                break;
            }
        }
        Ok(())
    }

    fn apply_reverse_dependency_boost(
        &self,
        session: &ReadSession,
        queries: &[ContextQuery],
        candidates: &mut [Candidate],
    ) -> Result<()> {
        let seed_paths = import_seed_paths(candidates, queries, self.config.tokenizer)
            .into_iter()
            .take(24)
            .map(|(path, _)| path)
            .collect::<Vec<_>>();
        let importers = session
            .affected_importers(&seed_paths)?
            .into_iter()
            .collect::<HashSet<_>>();
        for candidate in candidates {
            if importers.contains(&candidate.path) {
                if !candidate
                    .match_kinds
                    .iter()
                    .any(|kind| kind == "reverse-import")
                {
                    candidate.match_kinds.push("reverse-import".into());
                }
                candidate.import_boost = candidate.import_boost.max(1.0);
            }
        }
        Ok(())
    }

    /// Resolve a diff scope from the request into a receipt, if one is supplied.
    ///
    /// When `base_revision` is set, committed and working-tree paths since that
    /// revision are resolved from the repository, including untracked files.
    /// Explicit `changed_paths` are merged with that result. When neither input
    /// is supplied, `None` is returned and task-only behavior is preserved.
    fn resolve_diff_scope(&self, request: &ContextRequest) -> Result<Option<DiffScopeReceipt>> {
        let has_base = request
            .base_revision
            .as_deref()
            .is_some_and(|rev| !rev.trim().is_empty());
        let has_paths = !request.changed_paths.is_empty();
        if !has_base && !has_paths {
            return Ok(None);
        }
        if let Some(ref revision) = request.base_revision
            && !revision.trim().is_empty()
        {
            let git_result = git_diff_paths(&self.config.root, revision, MAX_DIFF_CHANGED_PATHS)?;
            let mut changed_paths = request.changed_paths.clone();
            let mut resolved_paths = git_result.changed_paths;
            match git_changed_paths(&self.config.root, MAX_DIFF_CHANGED_PATHS) {
                Ok(working_tree_paths) => resolved_paths.extend(working_tree_paths),
                Err(error) => {
                    tracing::debug!(%error, "working-tree diff scope unavailable");
                }
            }
            resolved_paths.sort();
            resolved_paths.dedup();
            for path in resolved_paths {
                if changed_paths.len() == MAX_DIFF_CHANGED_PATHS {
                    break;
                }
                if !changed_paths.contains(&path) {
                    changed_paths.push(path);
                }
            }
            changed_paths.sort();
            changed_paths.dedup();
            return Ok(Some(DiffScopeReceipt {
                base_revision: Some(git_result.base_revision),
                head_revision: Some(git_result.head_revision),
                changed_paths,
                indexed_changed_paths: 0,
                evidence: None,
            }));
        }
        Ok(Some(DiffScopeReceipt {
            base_revision: None,
            head_revision: None,
            changed_paths: request.changed_paths.clone(),
            indexed_changed_paths: 0,
            evidence: None,
        }))
    }

    /// Select ranked task evidence within an exact source-token budget.
    pub async fn context(&self, request: ContextRequest) -> Result<ContextResponse> {
        self.context_cancellable_with_workflow(
            request,
            ContextWorkflow::Auto,
            CancellationToken::new(),
        )
        .await
    }

    /// Retrieve context after applying the requested index consistency boundary.
    pub async fn context_with_consistency_cancellable(
        &self,
        request: ContextRequest,
        consistency: IndexConsistency,
        cancellation: CancellationToken,
    ) -> Result<ContextResponse> {
        self.validate_context_request(&request)?;
        self.apply_consistency(consistency, cancellation.clone())
            .await?;
        self.context_cancellable_with_workflow(request, ContextWorkflow::Auto, cancellation)
            .await
    }

    /// Retrieve context under an explicit or auto-detected workflow.
    pub async fn context_with_workflow_consistency_cancellable(
        &self,
        request: ContextRequest,
        workflow: ContextWorkflow,
        consistency: IndexConsistency,
        cancellation: CancellationToken,
    ) -> Result<ContextResponse> {
        self.validate_context_request(&request)?;
        self.apply_consistency(consistency, cancellation.clone())
            .await?;
        self.context_cancellable_with_workflow(request, workflow, cancellation)
            .await
    }

    pub async fn context_cancellable(
        &self,
        request: ContextRequest,
        cancellation: CancellationToken,
    ) -> Result<ContextResponse> {
        self.context_cancellable_with_workflow(request, ContextWorkflow::Auto, cancellation)
            .await
    }

    async fn context_cancellable_with_workflow(
        &self,
        request: ContextRequest,
        workflow: ContextWorkflow,
        cancellation: CancellationToken,
    ) -> Result<ContextResponse> {
        let this = self.clone();
        tokio::task::spawn_blocking(move || {
            let (evaluation, baseline_source_tokens) = this.context_sync(
                request,
                workflow,
                &cancellation,
                CandidateDiagnostics::Omit,
                ContextSignals::PRODUCTION,
            )?;
            if let Some(baseline_source_tokens) = baseline_source_tokens {
                this.record_token_savings(
                    TokenSavingsOperation::Context,
                    baseline_source_tokens,
                    evaluation.response.meta.emitted_tokens,
                );
            }
            Ok(evaluation.response)
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
            this.context_sync(
                request,
                ContextWorkflow::Implementation,
                &cancellation,
                CandidateDiagnostics::Collect,
                ContextSignals::PRODUCTION,
            )
            .map(|(evaluation, _)| evaluation)
        })
        .await?
    }

    /// Retrieve context under one evaluation-only dependency or caller policy.
    ///
    /// This API is not exposed through CLI or MCP adapters. It exists so frozen
    /// ablations can compare additive signals without approximating selection.
    pub async fn context_signal_evaluation(
        &self,
        request: ContextRequest,
        policy: ContextSignalPolicy,
    ) -> Result<ContextEvaluation> {
        let this = self.clone();
        let cancellation = CancellationToken::new();
        tokio::task::spawn_blocking(move || {
            this.context_sync(
                request,
                ContextWorkflow::Implementation,
                &cancellation,
                CandidateDiagnostics::Collect,
                ContextSignals::evaluation(policy),
            )
            .map(|(evaluation, _)| evaluation)
        })
        .await?
    }

    fn append_workflow_candidates(
        &self,
        session: &ReadSession,
        request: &ContextRequest,
        workflow: ContextWorkflow,
        cancellation: &CancellationToken,
        candidates: &mut Vec<Candidate>,
    ) -> Result<Option<WorkflowReceipt>> {
        if !matches!(
            workflow,
            ContextWorkflow::Contribution | ContextWorkflow::Review
        ) {
            return Ok(None);
        }

        let mut matches = Vec::new();
        let mut cursor = None;
        let mut scanned_files = 0;
        let mut scan_truncated = false;
        loop {
            check_cancelled(cancellation)?;
            let page = session.list_files(512, cursor)?;
            let Some(last) = page.last() else {
                break;
            };
            cursor = Some(last.id);
            for file in page {
                if scanned_files == MAX_WORKFLOW_SCAN_FILES {
                    scan_truncated = true;
                    break;
                }
                scanned_files += 1;
                if let Some((score, family)) = workflow_path_role(&file.path, request) {
                    matches.push((file, score, family));
                }
            }
            if scan_truncated {
                break;
            }
        }
        let count = |family| {
            matches
                .iter()
                .filter(|(_, _, candidate_family)| *candidate_family == family)
                .count()
        };
        let guidance_candidates = count("guidance");
        let template_candidates = count("template");
        let validation_candidates = count("validation");
        let owner_test_candidates = count("owner_test");
        let mut missing_families = Vec::new();
        for (family, candidates) in [
            ("guidance", guidance_candidates),
            ("template", template_candidates),
            ("validation", validation_candidates),
            ("owner_test", owner_test_candidates),
        ] {
            if candidates == 0 {
                missing_families.push(family.to_owned());
            }
        }
        if scan_truncated {
            missing_families.push("repository_scan_truncated".to_owned());
        }
        matches.sort_by(|left, right| {
            right
                .1
                .total_cmp(&left.1)
                .then_with(|| left.0.path.cmp(&right.0.path))
        });
        matches.truncate(24);

        let excerpt_requests = matches
            .iter()
            .map(|(file, _, _)| StoredExcerptRequest {
                file_id: file.id,
                desired_start_line: 1,
                desired_end_line: 80,
                required_start_line: 1,
                required_end_line: 1,
                max_lines: 80,
            })
            .collect::<Vec<_>>();
        for ((file, score, family), excerpt) in matches
            .into_iter()
            .zip(self.stored_excerpts(session, &excerpt_requests)?)
        {
            let Some(excerpt) = excerpt else { continue };
            candidates.push(
                Candidate::new(
                    file.path,
                    excerpt.start_line,
                    excerpt.end_line,
                    excerpt.content,
                )
                .match_kind(format!("workflow_{family}"))
                .representation("workflow")
                .path_score(score)
                .focus_boost(score),
            );
        }
        Ok(Some(WorkflowReceipt {
            guidance_candidates,
            template_candidates,
            validation_candidates,
            owner_test_candidates,
            missing_families,
        }))
    }

    fn build_diff_evidence(
        &self,
        session: &ReadSession,
        request: &ContextRequest,
        scope: &DiffScopeReceipt,
        cancellation: &CancellationToken,
    ) -> Result<DiffEvidenceReceipt> {
        let mut changed_symbols = Vec::new();
        let mut relationships = BTreeSet::new();
        let mut gaps = Vec::new();
        let changed_hunks = if let Some(base_revision) = &scope.base_revision {
            let mut hunks = git_diff_hunks(
                &self.config.root,
                base_revision,
                MAX_DIFF_EVIDENCE_SYMBOLS + 1,
            )?;
            if hunks.len() > MAX_DIFF_EVIDENCE_SYMBOLS {
                gaps.push("changed_hunk_evidence_truncated".into());
                hunks.truncate(MAX_DIFF_EVIDENCE_SYMBOLS);
            }
            hunks
                .into_iter()
                .map(|hunk| DiffHunkEvidence {
                    path: hunk.path,
                    start_line: hunk.start_line,
                    end_line: hunk.end_line,
                })
                .collect::<Vec<_>>()
        } else {
            gaps.push("hunk_ranges_unavailable_for_explicit_paths".into());
            Vec::new()
        };
        let scoped_paths = scope
            .changed_paths
            .iter()
            .take(MAX_DIFF_EVIDENCE_PATHS)
            .collect::<Vec<_>>();
        if scope.changed_paths.len() > scoped_paths.len() {
            gaps.push("changed_path_evidence_truncated".into());
        }

        for path in &scoped_paths {
            check_cancelled(cancellation)?;
            let Some(file) = session.find_file(path)? else {
                gaps.push(format!("{path}:not_indexed_or_deleted"));
                continue;
            };
            if !file.structurally_complete {
                gaps.push(format!("{path}:structural_coverage_incomplete"));
            }
            let symbols = session.get_symbols_for_file(file.id, MAX_DIFF_EVIDENCE_SYMBOLS)?;
            let path_hunks = changed_hunks
                .iter()
                .filter(|hunk| hunk.path == **path)
                .collect::<Vec<_>>();
            let symbols = symbols
                .into_iter()
                .filter(|symbol| {
                    path_hunks.is_empty()
                        || path_hunks.iter().any(|hunk| {
                            symbol.start_line <= hunk.end_line && symbol.end_line >= hunk.start_line
                        })
                })
                .collect::<Vec<_>>();
            if symbols.is_empty() && file.structurally_complete {
                gaps.push(format!("{path}:no_indexed_definitions"));
            }
            for symbol in symbols {
                if changed_symbols.len() == MAX_DIFF_EVIDENCE_SYMBOLS {
                    gaps.push("changed_symbol_evidence_truncated".into());
                    break;
                }
                let mut references = session.search_references(
                    &symbol.name,
                    true,
                    MAX_REFERENCES_PER_CHANGED_SYMBOL + 1,
                )?;
                if references.len() > MAX_REFERENCES_PER_CHANGED_SYMBOL {
                    gaps.push(format!(
                        "{path}:{}:reference_evidence_truncated",
                        symbol.name
                    ));
                    references.truncate(MAX_REFERENCES_PER_CHANGED_SYMBOL);
                }
                for reference in references {
                    if reference.path != **path {
                        relationships.insert((
                            (**path).clone(),
                            reference.path,
                            "reference".to_owned(),
                        ));
                    }
                }
                changed_symbols.push(DiffSymbolEvidence {
                    path: (**path).clone(),
                    name: symbol.name,
                    kind: symbol.kind,
                    start_line: symbol.start_line,
                    end_line: symbol.end_line,
                });
            }
            for importer in session.affected_importers(&[(**path).clone()])? {
                if importer != **path {
                    relationships.insert(((**path).clone(), importer, "importer".to_owned()));
                }
            }
        }

        let mut cursor = None;
        let mut scanned_owner_test_files = 0;
        let mut owner_test_scan_truncated = false;
        loop {
            check_cancelled(cancellation)?;
            let page = session.list_files(512, cursor)?;
            let Some(last) = page.last() else {
                break;
            };
            cursor = Some(last.id);
            for file in page {
                if scanned_owner_test_files == MAX_OWNER_TEST_SCAN_FILES {
                    owner_test_scan_truncated = true;
                    break;
                }
                scanned_owner_test_files += 1;
                if let Some(changed_path) = owner_test_changed_path(&file.path, request) {
                    relationships.insert((changed_path, file.path, "test_name_match".to_owned()));
                }
            }
            if owner_test_scan_truncated {
                break;
            }
        }
        if owner_test_scan_truncated {
            gaps.push("owner_test_scan_truncated".into());
        }

        let relationship_count = relationships.len();
        let related_paths = relationships
            .into_iter()
            .take(MAX_DIFF_EVIDENCE_RELATIONSHIPS)
            .map(|(changed_path, related_path, signal)| DiffRelatedPath {
                changed_path,
                related_path,
                signal,
            })
            .collect();
        if relationship_count > MAX_DIFF_EVIDENCE_RELATIONSHIPS {
            gaps.push("related_path_evidence_truncated".into());
        }
        gaps.sort();
        gaps.dedup();

        Ok(DiffEvidenceReceipt {
            changed_hunks,
            changed_symbols,
            related_paths,
            gaps,
        })
    }

    #[allow(clippy::cognitive_complexity)]
    fn context_sync(
        &self,
        mut request: ContextRequest,
        workflow: ContextWorkflow,
        cancellation: &CancellationToken,
        diagnostics: CandidateDiagnostics,
        signals: ContextSignals,
    ) -> Result<(ContextEvaluation, Option<usize>)> {
        check_cancelled(cancellation)?;
        self.validate_context_request(&request)?;
        request.changed_paths = request
            .changed_paths
            .iter()
            .map(|path| normalize_relative(path))
            .collect::<Result<Vec<_>>>()?;
        let diff_scope = self.resolve_diff_scope(&request)?;
        let mut scoped_request = request.clone();
        if let Some(scope) = &diff_scope {
            scoped_request.changed_paths = scope.changed_paths.clone();
        }

        let mut changed_paths = git_changed_paths(&self.config.root, GIT_CHANGED_PATHS_MAX)
            .unwrap_or_else(|error| {
                tracing::debug!(%error, "working-tree signal unavailable");
                HashSet::new()
            });
        if let Some(ref scope) = diff_scope {
            changed_paths.extend(scope.changed_paths.iter().cloned());
        }
        self.consistent(|session, generation| {
            let facet_plan = facets::plan(&request.task, MAX_CONTEXT_QUERIES);
            let queries = facet_plan.queries;
            let terms = queries
                .iter()
                .map(|query| query.value.clone())
                .collect::<Vec<_>>();
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
                let mut symbol_hits = Vec::new();
                for (rank, hit) in session
                    .search_symbols(term, false, MAX_CONTEXT_HITS_PER_SOURCE)?
                    .into_iter()
                    .enumerate()
                {
                    check_cancelled(cancellation)?;
                    if path_allowed(&hit.path, &[], &request.exclude_paths)? {
                        symbol_hits.push((rank, hit));
                    }
                }
                let symbol_excerpt_requests = symbol_hits
                    .iter()
                    .map(|(_, hit)| AdaptiveExcerptRequest {
                        file_id: hit.symbol.file_id,
                        declaration_start: hit.symbol.start_line,
                        declaration_end: hit.symbol.end_line,
                        matched_line: hit.symbol.start_line,
                        token_budget: excerpt_budget(
                            request.token_budget,
                            ContextExcerptKind::Symbol,
                        ),
                    })
                    .collect::<Vec<_>>();
                for ((rank, hit), excerpt) in symbol_hits
                    .into_iter()
                    .zip(self.adaptive_context_excerpts(session, &symbol_excerpt_requests)?)
                {
                    check_cancelled(cancellation)?;
                    let Some(excerpt) = excerpt else { continue };
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
                    let change_boost = Self::file_change_boost(
                        Some(hit.generation),
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
                let reference_results = signals
                    .caller
                    .then(|| session.search_references(term, false, MAX_CONTEXT_HITS_PER_SOURCE))
                    .transpose()?
                    .unwrap_or_default();
                let mut reference_hits = Vec::new();
                for (rank, hit) in reference_results.into_iter().enumerate() {
                    check_cancelled(cancellation)?;
                    if path_allowed(&hit.path, &[], &request.exclude_paths)? {
                        reference_hits.push((rank, hit));
                    }
                }
                let reference_locations = reference_hits
                    .iter()
                    .map(|(_, hit)| (hit.reference.file_id, hit.reference.start_line))
                    .collect::<Vec<_>>();
                let enclosing = session.find_enclosing_symbols_batch(&reference_locations)?;
                let mut adaptive_indices = Vec::new();
                let mut adaptive_requests = Vec::new();
                for (index, ((_, hit), symbol)) in reference_hits.iter().zip(enclosing).enumerate()
                {
                    if let Some(symbol) = symbol {
                        adaptive_indices.push(index);
                        adaptive_requests.push(AdaptiveExcerptRequest {
                            file_id: hit.reference.file_id,
                            declaration_start: symbol.start_line,
                            declaration_end: symbol.end_line,
                            matched_line: hit.reference.start_line,
                            token_budget: excerpt_budget(
                                request.token_budget,
                                ContextExcerptKind::Reference,
                            ),
                        });
                    }
                }
                let mut adaptive_excerpts = vec![None; reference_hits.len()];
                for (index, excerpt) in adaptive_indices
                    .into_iter()
                    .zip(self.adaptive_context_excerpts(session, &adaptive_requests)?)
                {
                    adaptive_excerpts[index] = excerpt;
                }
                let fallback_requests = reference_hits
                    .iter()
                    .map(|(_, hit)| StoredExcerptRequest {
                        file_id: hit.reference.file_id,
                        desired_start_line: hit.reference.start_line.saturating_sub(2).max(1),
                        desired_end_line: hit.reference.end_line.saturating_add(2),
                        required_start_line: hit.reference.start_line,
                        required_end_line: hit.reference.end_line,
                        max_lines: 12,
                    })
                    .collect::<Vec<_>>();
                let fallback_excerpts = self.stored_excerpts(session, &fallback_requests)?;
                for (((rank, hit), adaptive), fallback) in reference_hits
                    .into_iter()
                    .zip(adaptive_excerpts)
                    .zip(fallback_excerpts)
                {
                    check_cancelled(cancellation)?;
                    let excerpt = adaptive.or(fallback);
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
                    let change_boost = Self::file_change_boost(
                        Some(hit.generation),
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
                let term_regex = compile_literal_regex(term, false)?;
                let lexical = if term.chars().count() >= 3 {
                    session.search_trigram(term, MAX_CONTEXT_LEXICAL_HITS)?
                } else {
                    session.search_word(&fts_quote(term), MAX_CONTEXT_LEXICAL_HITS)?
                };
                let mut lexical_hits = Vec::new();
                for (rank, hit) in lexical.into_iter().enumerate() {
                    check_cancelled(cancellation)?;
                    if !path_allowed(&hit.path, &[], &request.exclude_paths)? {
                        continue;
                    }
                    let Some(search_hit) =
                        chunk_search_hit(hit.clone(), term, false, 2, term_regex.as_ref(), false)?
                    else {
                        continue;
                    };
                    let matched_line = matching_line(&hit, term, false, term_regex.as_ref())
                        .unwrap_or(search_hit.start_line);
                    lexical_hits.push((rank, hit, search_hit, matched_line));
                }
                let lexical_locations = lexical_hits
                    .iter()
                    .map(|(_, hit, _, matched_line)| (hit.file_id, *matched_line))
                    .collect::<Vec<_>>();
                let enclosing = session.find_enclosing_symbols_batch(&lexical_locations)?;
                let mut adaptive_indices = Vec::new();
                let mut adaptive_requests = Vec::new();
                for (index, ((_, hit, _, matched_line), symbol)) in
                    lexical_hits.iter().zip(enclosing).enumerate()
                {
                    if let Some(symbol) = symbol {
                        adaptive_indices.push(index);
                        adaptive_requests.push(AdaptiveExcerptRequest {
                            file_id: hit.file_id,
                            declaration_start: symbol.start_line,
                            declaration_end: symbol.end_line,
                            matched_line: *matched_line,
                            token_budget: excerpt_budget(
                                request.token_budget,
                                ContextExcerptKind::Text,
                            ),
                        });
                    }
                }
                let mut adaptive_excerpts = vec![None; lexical_hits.len()];
                for (index, excerpt) in adaptive_indices
                    .into_iter()
                    .zip(self.adaptive_context_excerpts(session, &adaptive_requests)?)
                {
                    adaptive_excerpts[index] = excerpt;
                }
                for ((rank, hit, search_hit, _), adaptive) in
                    lexical_hits.into_iter().zip(adaptive_excerpts)
                {
                    check_cancelled(cancellation)?;
                    let excerpt = adaptive.unwrap_or(StoredExcerpt {
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
                    let occurrences = term_regex
                        .as_ref()
                        .map_or(0, |matcher| matcher.find_iter(&hit.content).count());
                    let change_boost = Self::file_change_boost(
                        Some(hit.generation),
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
            let resolved_workflow = resolve_context_workflow(workflow, &request.task);
            let workflow_receipt = self.append_workflow_candidates(
                session,
                &scoped_request,
                resolved_workflow,
                cancellation,
                &mut candidates,
            )?;

            signals
                .import_neighbor
                .then(|| {
                    self.append_import_symbol_candidates(
                        ImportExpansion {
                            session,
                            request: &request,
                            queries: &queries,
                            terms: &terms,
                            changed_paths: &changed_paths,
                            cancellation,
                        },
                        &mut candidates,
                    )
                })
                .transpose()?;
            signals
                .reverse_dependency
                .then(|| self.apply_reverse_dependency_boost(session, &queries, &mut candidates))
                .transpose()?;

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
            response.workflow = resolved_workflow;
            response.workflow_receipt = workflow_receipt;
            response.meta.freshness = self.freshness();
            response.meta.repository_id = self.repository_id();
            if let Some(mut scope) = diff_scope.clone() {
                let mut indexed = 0usize;
                for path in &scope.changed_paths {
                    if session.find_file(path)?.is_some() {
                        indexed += 1;
                    }
                }
                scope.indexed_changed_paths = indexed;
                scope.evidence = Some(self.build_diff_evidence(
                    session,
                    &scoped_request,
                    &scope,
                    cancellation,
                )?);
                response.diff_scope = Some(scope);
            }
            if response.fragments.is_empty() {
                response
                    .warnings
                    .push("no relevant indexed evidence found".into());
            }
            let paths = response
                .fragments
                .iter()
                .map(|fragment| fragment.path.clone())
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect::<Vec<_>>();
            let baseline_source_tokens =
                session.whole_file_source_tokens(&paths, self.config.tokenizer.name())?;
            Ok((
                ContextEvaluation {
                    response,
                    generated_candidate_paths,
                    generated_candidates,
                },
                baseline_source_tokens,
            ))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workflow_auto_detection_requires_high_confidence_language() {
        assert_eq!(
            resolve_context_workflow(ContextWorkflow::Auto, "prepare this pull request"),
            ContextWorkflow::Contribution
        );
        assert_eq!(
            resolve_context_workflow(ContextWorkflow::Auto, "review this parser change"),
            ContextWorkflow::Review
        );
        assert_eq!(
            resolve_context_workflow(ContextWorkflow::Auto, "implement parser review comments"),
            ContextWorkflow::Implementation
        );
    }

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
    fn language_scope_boosts_common_source_file_extensions() {
        assert_eq!(
            context_path_score("src/main.rs", &[], "Fix this Rust bug"),
            12.0
        );
        assert_eq!(
            context_path_score("lib/parser.py", &[], "Fix this Python parser"),
            12.0
        );
        assert_eq!(
            context_path_score("src/main.rs", &[], "Fix this Python parser"),
            0.0
        );
    }

    #[test]
    fn owner_test_matching_requires_filename_token_boundaries() {
        let mut request = ContextRequest {
            task: "fix core".into(),
            token_budget: 100,
            focus_paths: Vec::new(),
            focus_symbols: Vec::new(),
            exclude_paths: Vec::new(),
            known_hashes: Vec::new(),
            prior_repository_generation: None,
            base_revision: None,
            changed_paths: vec!["src/core.rs".into()],
        };

        assert_eq!(
            owner_test_changed_path("tests/core_tests.rs", &request),
            Some("src/core.rs".into())
        );
        assert_eq!(
            owner_test_changed_path("tests/hardcore_tests.rs", &request),
            None
        );
        assert_eq!(
            owner_test_changed_path("tests/core/unrelated_tests.rs", &request),
            None
        );
        request.changed_paths = vec!["src/my_core.rs".into()];
        assert_eq!(
            owner_test_changed_path("tests/my_core_spec.rs", &request),
            Some("src/my_core.rs".into())
        );
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
