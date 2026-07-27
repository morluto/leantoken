//! Task-shaped context candidate assembly and ranking handoff.

use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    sync::LazyLock,
    time::Instant,
};

use globset::{GlobBuilder, GlobSet, GlobSetBuilder};
use tokio_util::sync::CancellationToken;

mod facets;

use super::change_receipt::{classify_revision_changes, owner_test_coverage};
use super::handoff::{self, HandoffProvenance};
use super::read::{AdaptiveExcerptRequest, StoredExcerpt, StoredExcerptRequest};
use super::receipts::{ReceiptDecision, ReceiptEvidence};
use super::search::{chunk_search_hit_for_range, compile_literal_regex, fts_quote};
use super::validation::{
    MAX_INPUT_ITEMS, MAX_PATH_BYTES, MAX_PATTERN_BYTES, MAX_QUERY_BYTES, PathFilter, PathMatcher,
    check_cancelled, validate_glob_patterns, validate_input,
};
use super::{ServiceCallOptions, Services, retrieval_primitive_key};
use crate::model::*;
use crate::ranking::{self, Candidate};
use crate::repository::{
    git_diff_hunks_scoped, git_diff_identity, git_diff_paths, git_diff_paths_between,
    git_head_revision, git_working_tree_status, normalize_relative, validate_relative,
};
use crate::storage::ChunkHit;
use crate::storage::{FileRecord, ReadSession, SymbolHit, SymbolRecord};
use crate::text::{byte_to_line, expand_terms, identifier_words, line_starts};
use crate::tokens::ResponseBudget;
use crate::{Error, Result};
use facets::{ContextQuery, FacetKind};
const GIT_CHANGED_PATHS_MAX: usize = 512;
/// Maximum explicit changed paths accepted from a diff-scoped request.
const MAX_DIFF_CHANGED_PATHS: usize = 512;
/// Maximum bytes for a base revision string.
const MAX_BASE_REVISION_BYTES: usize = 256;
/// Maximum context query terms (symbols/refs/FTS fan-out budget).
pub(super) const MAX_CONTEXT_QUERIES: usize = 12;
/// Per-term symbol/reference candidate cap for context assembly.
pub(super) const MAX_CONTEXT_HITS_PER_SOURCE: usize = 20;
/// Per-term FTS candidate cap for context assembly.
pub(super) const MAX_CONTEXT_LEXICAL_HITS: usize = 30;
/// Per-import symbol scan cap for concept-corroborated structural expansion.
const MAX_IMPORT_SYMBOLS: usize = 128;
/// Exact constraint names retained per storage batch.
const MAX_EXACT_SYMBOL_BATCH_NAMES: usize = 32;
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
const OVERSIZED_CHANGE_PATHS: usize = 32;
const MIN_OVERSIZED_PATH_GROUPS: usize = 3;
const MAX_ROUTING_GROUPS: usize = 5;
const MAX_ROUTING_SUGGESTIONS: usize = 3;
const LEXICAL_OCCURRENCE_SATURATION: usize = 25;

fn parse_revision_range(revision: &str) -> Result<Option<(&str, &str)>> {
    let Some((base, head)) = revision.split_once("..") else {
        return Ok(None);
    };
    if base.trim().is_empty()
        || head.trim().is_empty()
        || base.ends_with('.')
        || head.starts_with('.')
        || head.contains("..")
    {
        return Err(Error::InvalidInput {
            field: "base revision",
            reason: "revision range must be BASE..HEAD",
        });
    }
    Ok(Some((base, head)))
}

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
    ContextPathScorer::new(terms, task).score(path)
}

struct ContextPathScorer {
    terms: Vec<String>,
    code_token_parts: Vec<Vec<String>>,
    languages: [bool; 5],
}

impl ContextPathScorer {
    fn new(terms: &[String], task: &str) -> Self {
        let code_token_parts = facets::legacy_code_tokens(task)
            .into_iter()
            .filter(|token| {
                token.contains("::")
                    || token
                        .split('.')
                        .any(|part| part.chars().next().is_some_and(char::is_uppercase))
            })
            .map(|code_token| {
                expand_terms(&code_token)
                    .into_iter()
                    .map(|part| part.to_ascii_lowercase())
                    .filter(|part| part.chars().count() >= 2)
                    .collect::<HashSet<_>>()
                    .into_iter()
                    .collect()
            })
            .collect();
        Self {
            terms: terms.iter().map(|term| term.to_ascii_lowercase()).collect(),
            code_token_parts,
            languages: [
                task_mentions_language(task, "javascript"),
                task_mentions_language(task, "typescript"),
                task_mentions_language(task, "python"),
                task_mentions_language(task, "rust"),
                task_mentions_language(task, "go"),
            ],
        }
    }

    fn score(&self, path: &str) -> f64 {
        let path = path.to_lowercase();
        let mut score = self
            .terms
            .iter()
            .filter(|term| path.contains(term.as_str()))
            .count() as f64;
        for parts in &self.code_token_parts {
            let matched_parts = parts.iter().filter(|part| path.contains(*part)).count();
            if matched_parts >= 2 {
                #[allow(clippy::cast_precision_loss)]
                {
                    score += (matched_parts * matched_parts) as f64;
                }
            }
        }
        for (mentioned, component, extensions) in [
            (self.languages[0], "/js/", &["js", "jsx", "mjs", "cjs"][..]),
            (self.languages[1], "/ts/", &["ts", "tsx"][..]),
            (self.languages[2], "/python/", &["py", "pyw"][..]),
            (self.languages[3], "/rust/", &["rs"][..]),
            (self.languages[4], "/go/", &["go"][..]),
        ] {
            let extension_matches = path
                .rsplit_once('.')
                .is_some_and(|(_, extension)| extensions.contains(&extension));
            let component = component.trim_matches('/');
            let component_matches = path
                .split('/')
                .any(|path_component| path_component == component);
            if mentioned && (extension_matches || component_matches) {
                // An explicit language name in the task is strong repository-scope
                // evidence. Keep this above an exact-name match in another
                // language so common names such as `Point` do not dominate.
                score += 12.0;
            }
        }
        score
    }
}

fn context_path_group(path: &str) -> String {
    let mut components = path.split('/').filter(|component| !component.is_empty());
    let first = components.next().unwrap_or("<root>");
    let second = components.next();
    if second.is_none() {
        return "<root>".into();
    }
    if matches!(
        first,
        "src" | "lib" | "app" | "apps" | "crates" | "packages"
    ) && let Some(second) = second
    {
        format!("{first}/{second}")
    } else {
        first.to_owned()
    }
}

fn build_context_routing(
    request: &ContextRequest,
    scope: &DiffScopeReceipt,
    candidate_paths: usize,
    selected_paths: &[String],
) -> Option<ContextRoutingReceipt> {
    if scope.changed_paths.len() < OVERSIZED_CHANGE_PATHS {
        return None;
    }
    let mut grouped_paths = BTreeMap::<String, Vec<String>>::new();
    for path in &scope.changed_paths {
        grouped_paths
            .entry(context_path_group(path))
            .or_default()
            .push(path.clone());
    }
    if grouped_paths.len() < MIN_OVERSIZED_PATH_GROUPS {
        return None;
    }
    let path_groups_total = grouped_paths.len();
    let mut groups = grouped_paths.into_iter().collect::<Vec<_>>();
    groups.sort_by(|left, right| {
        right
            .1
            .len()
            .cmp(&left.1.len())
            .then_with(|| left.0.cmp(&right.0))
    });
    let selected_groups = selected_paths.iter().fold(
        BTreeMap::<String, BTreeSet<&str>>::new(),
        |mut paths, path| {
            paths
                .entry(context_path_group(path))
                .or_default()
                .insert(path);
            paths
        },
    );
    let selected_path_count = selected_paths
        .iter()
        .map(String::as_str)
        .collect::<BTreeSet<_>>()
        .len();
    let strongest_selected_group = selected_groups
        .values()
        .map(BTreeSet::len)
        .max()
        .unwrap_or(0);
    let weakly_concentrated = selected_path_count > 1
        && strongest_selected_group.saturating_mul(2) <= selected_path_count;

    let suggestions = groups
        .iter()
        .take(MAX_ROUTING_SUGGESTIONS)
        .map(|(prefix, _)| ContextRoutingSuggestion {
            include_paths: vec![if prefix == "<root>" {
                "*".into()
            } else {
                format!("{prefix}/**")
            }],
        })
        .collect();
    let path_groups = groups
        .into_iter()
        .take(MAX_ROUTING_GROUPS)
        .map(|(prefix, paths)| ContextPathGroup {
            prefix,
            changed_paths: paths.len(),
        })
        .collect();
    Some(ContextRoutingReceipt {
        candidate_paths,
        changed_paths: scope.changed_paths.len(),
        selected_paths: selected_path_count,
        weakly_concentrated,
        consistency: IndexConsistency::IndexedGeneration,
        base_revision: request.base_revision.clone(),
        known_hashes: request.known_hashes.clone(),
        path_groups_total,
        path_groups,
        suggestions,
    })
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

#[derive(Default)]
struct ContextPhaseTracker {
    enabled: bool,
    generation: u64,
    counters: ContextPhaseCounters,
    timings: ContextPhaseTimings,
    started: Option<Instant>,
    enclosing_locations: BTreeSet<(i64, usize)>,
    adaptive_excerpts: BTreeSet<(i64, usize, usize, usize, usize)>,
    stored_excerpts: BTreeSet<(i64, usize, usize, usize, usize, usize)>,
    primitive_keys: Vec<RetrievalPrimitiveKey>,
}

#[derive(Clone, Copy)]
enum ContextTimedPhase {
    ExactSymbolLookup,
    SymbolSearch,
    ReferenceSearch,
    LexicalSearch,
    LexicalVerify,
    EnclosingLookup,
    AdaptiveExcerpt,
    StoredExcerpt,
}

impl ContextPhaseTracker {
    fn new(diagnostics: CandidateDiagnostics, generation: u64) -> Self {
        Self {
            enabled: diagnostics == CandidateDiagnostics::Collect,
            generation,
            started: (diagnostics == CandidateDiagnostics::Collect).then(Instant::now),
            ..Self::default()
        }
    }

    fn measure<T>(
        &mut self,
        phase: ContextTimedPhase,
        operation: impl FnOnce() -> Result<T>,
    ) -> Result<T> {
        if !self.enabled {
            return operation();
        }
        let started = Instant::now();
        let output = operation()?;
        self.record_elapsed(phase, Some(started));
        Ok(output)
    }

    fn record_elapsed(&mut self, phase: ContextTimedPhase, started: Option<Instant>) {
        let Some(started) = started else { return };
        let elapsed_ms = started.elapsed().as_secs_f64() * 1_000.0;
        let target = match phase {
            ContextTimedPhase::ExactSymbolLookup => &mut self.timings.exact_symbol_lookup_ms,
            ContextTimedPhase::SymbolSearch => &mut self.timings.symbol_search_ms,
            ContextTimedPhase::ReferenceSearch => &mut self.timings.reference_search_ms,
            ContextTimedPhase::LexicalSearch => &mut self.timings.lexical_search_ms,
            ContextTimedPhase::LexicalVerify => &mut self.timings.lexical_verify_ms,
            ContextTimedPhase::EnclosingLookup => &mut self.timings.enclosing_lookup_ms,
            ContextTimedPhase::AdaptiveExcerpt => &mut self.timings.adaptive_excerpt_ms,
            ContextTimedPhase::StoredExcerpt => &mut self.timings.stored_excerpt_ms,
        };
        *target += elapsed_ms;
    }

    fn timer(&self) -> Option<Instant> {
        self.enabled.then(Instant::now)
    }

    fn record_primitive(&mut self, kind: &str, normalized_inputs: impl FnOnce() -> String) {
        if self.enabled {
            self.primitive_keys.push(retrieval_primitive_key(
                self.generation,
                kind,
                &normalized_inputs(),
            ));
        }
    }

    fn record_enclosing_locations(&mut self, locations: &[(i64, usize)]) {
        if !self.enabled {
            return;
        }
        self.counters.enclosing_location_batches = self
            .counters
            .enclosing_location_batches
            .saturating_add(usize::from(!locations.is_empty()));
        self.counters.enclosing_location_requests = self
            .counters
            .enclosing_location_requests
            .saturating_add(locations.len());
        self.enclosing_locations.extend(locations.iter().copied());
        for (file_id, line) in locations {
            self.record_primitive("enclosing_symbol", || format!("{file_id}:{line}"));
        }
    }

    fn record_adaptive_excerpts(&mut self, requests: &[AdaptiveExcerptRequest]) {
        if !self.enabled {
            return;
        }
        self.counters.adaptive_excerpt_batches = self
            .counters
            .adaptive_excerpt_batches
            .saturating_add(usize::from(!requests.is_empty()));
        self.counters.adaptive_excerpt_requests = self
            .counters
            .adaptive_excerpt_requests
            .saturating_add(requests.len());
        self.adaptive_excerpts
            .extend(requests.iter().map(|request| {
                (
                    request.file_id,
                    request.declaration_start,
                    request.declaration_end,
                    request.matched_line,
                    request.token_budget,
                )
            }));
        for request in requests {
            self.record_primitive("adaptive_excerpt", || {
                format!(
                    "{}:{}:{}:{}:{}",
                    request.file_id,
                    request.declaration_start,
                    request.declaration_end,
                    request.matched_line,
                    request.token_budget
                )
            });
        }
    }

    fn record_stored_excerpts(&mut self, requests: &[StoredExcerptRequest]) {
        if !self.enabled {
            return;
        }
        self.counters.stored_excerpt_batches = self
            .counters
            .stored_excerpt_batches
            .saturating_add(usize::from(!requests.is_empty()));
        self.counters.stored_excerpt_requests = self
            .counters
            .stored_excerpt_requests
            .saturating_add(requests.len());
        self.stored_excerpts.extend(requests.iter().map(|request| {
            (
                request.file_id,
                request.desired_start_line,
                request.desired_end_line,
                request.required_start_line,
                request.required_end_line,
                request.max_lines,
            )
        }));
        for request in requests {
            self.record_primitive("stored_excerpt", || {
                format!(
                    "{}:{}:{}:{}:{}:{}",
                    request.file_id,
                    request.desired_start_line,
                    request.desired_end_line,
                    request.required_start_line,
                    request.required_end_line,
                    request.max_lines
                )
            });
        }
    }

    fn finish(
        mut self,
        generated_candidates: usize,
    ) -> (
        ContextPhaseCounters,
        ContextPhaseTimings,
        Vec<RetrievalPrimitiveKey>,
    ) {
        if !self.enabled {
            return (
                ContextPhaseCounters::default(),
                ContextPhaseTimings::default(),
                Vec::new(),
            );
        }
        self.counters.unique_enclosing_locations = self.enclosing_locations.len();
        self.counters.unique_adaptive_excerpt_requests = self.adaptive_excerpts.len();
        self.counters.unique_stored_excerpt_requests = self.stored_excerpts.len();
        self.counters.generated_candidates = generated_candidates;
        self.timings.total_ms = self
            .started
            .map(|started| started.elapsed().as_secs_f64() * 1_000.0)
            .unwrap_or(0.0);
        (self.counters, self.timings, self.primitive_keys)
    }
}

struct LexicalMatchFacts {
    search_hit: SearchHit,
    matched_line: usize,
    occurrences: usize,
}

fn analyze_lexical_match(
    hit: &ChunkHit,
    matcher: &regex::Regex,
    context_lines: usize,
) -> Option<LexicalMatchFacts> {
    let mut matches = matcher.find_iter(&hit.content);
    let first = matches.next()?;
    let occurrences = 1usize.saturating_add(
        matches
            .take(LEXICAL_OCCURRENCE_SATURATION.saturating_sub(1))
            .count(),
    );
    let starts = line_starts(&hit.content);
    let search_hit = chunk_search_hit_for_range(
        hit,
        first.start(),
        first.end(),
        context_lines,
        false,
        false,
        &starts,
    );
    let local_start = byte_to_line(&starts, hit.content.len(), first.start());
    Some(LexicalMatchFacts {
        search_hit,
        matched_line: hit.start_line + local_start - 1,
        occurrences,
    })
}

#[derive(Clone, Copy)]
struct ContextSignals {
    import_neighbor: bool,
    reverse_dependency: bool,
    caller: bool,
}

struct AccountedContextResponse {
    response: ContextResponse,
    baseline_source_tokens: Option<usize>,
    operation: TokenAccountingOperation,
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
        if let Some(max_fragments) = request.max_fragments {
            self.result_limit(Some(max_fragments))?;
        }
        if let Some(minimum) = request.minimum_fragments_per_focus_path {
            self.result_limit(Some(minimum))?;
        }
        if request.plan_only && request.receipt_id.is_some() {
            return Err(Error::InvalidInput {
                field: "receipt_id",
                reason: "must be omitted when plan_only is true",
            });
        }
        validate_input(&request.task, "task", MAX_QUERY_BYTES)?;
        validate_glob_patterns(&request.include_paths)?;
        if request
            .include_paths
            .iter()
            .any(|pattern| pattern.trim().is_empty())
        {
            return Err(Error::InvalidInput {
                field: "include paths",
                reason: "must not contain empty patterns",
            });
        }
        validate_glob_patterns(&request.must_include_paths)?;
        if request
            .must_include_paths
            .iter()
            .any(|pattern| pattern.trim().is_empty())
        {
            return Err(Error::InvalidInput {
                field: "must include paths",
                reason: "must not contain empty patterns",
            });
        }
        validate_glob_patterns(&request.focus_paths)?;
        if request
            .focus_paths
            .iter()
            .any(|pattern| pattern.trim().is_empty())
        {
            return Err(Error::InvalidInput {
                field: "focus paths",
                reason: "must not contain empty patterns",
            });
        }
        if (request.strict_focus_paths || request.minimum_fragments_per_focus_path.is_some())
            && request.focus_paths.is_empty()
        {
            return Err(Error::InvalidInput {
                field: "focus paths",
                reason: "must not be empty when focus path constraints are enabled",
            });
        }
        validate_glob_patterns(&request.exclude_paths)?;
        if request.focus_symbols.len() > MAX_INPUT_ITEMS {
            return Err(Error::LimitExceeded);
        }
        for symbol in &request.focus_symbols {
            validate_input(symbol, "focus symbol", MAX_PATTERN_BYTES)?;
        }
        if request.must_include_symbols.len() > MAX_INPUT_ITEMS {
            return Err(Error::LimitExceeded);
        }
        for symbol in &request.must_include_symbols {
            validate_input(symbol, "must include symbol", MAX_PATTERN_BYTES)?;
            if symbol.trim().is_empty() {
                return Err(Error::InvalidInput {
                    field: "must include symbols",
                    reason: "must not contain empty symbols",
                });
            }
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
            parse_revision_range(revision)?;
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

    fn append_constraint_candidates(
        &self,
        session: &ReadSession,
        request: &ContextRequest,
        cancellation: &CancellationToken,
        candidates: &mut Vec<Candidate>,
        phases: &mut ContextPhaseTracker,
    ) -> Result<ContextCoverageReceipt> {
        let mut coverage = ContextCoverageReceipt::default();
        let mut focus_path_matches = vec![0usize; request.focus_paths.len()];
        let mut include_path_matches = vec![false; request.include_paths.len()];
        let mut required_path_matches = vec![false; request.must_include_paths.len()];
        let mut required_path_files = vec![None::<FileRecord>; request.must_include_paths.len()];
        let focus_matchers = request
            .focus_paths
            .iter()
            .map(|pattern| PathMatcher::new(std::slice::from_ref(pattern)))
            .collect::<Result<Vec<_>>>()?;
        let include_matchers = request
            .include_paths
            .iter()
            .map(|pattern| PathMatcher::new(std::slice::from_ref(pattern)))
            .collect::<Result<Vec<_>>>()?;
        let required_matchers = request
            .must_include_paths
            .iter()
            .map(|pattern| PathMatcher::new(std::slice::from_ref(pattern)))
            .collect::<Result<Vec<_>>>()?;
        let path_filter = PathFilter::new(&request.include_paths, &request.exclude_paths)?;

        if !request.focus_paths.is_empty()
            || !request.include_paths.is_empty()
            || !request.must_include_paths.is_empty()
        {
            let mut cursor = None;
            loop {
                check_cancelled(cancellation)?;
                let page = session.list_files(512, cursor)?;
                let Some(last) = page.last() else {
                    break;
                };
                cursor = Some(last.id);
                for file in page {
                    for (index, matcher) in focus_matchers.iter().enumerate() {
                        if matcher.is_match(&file.path) {
                            focus_path_matches[index] = focus_path_matches[index].saturating_add(1);
                        }
                    }
                    for (index, matcher) in include_matchers.iter().enumerate() {
                        include_path_matches[index] |= matcher.is_match(&file.path);
                    }
                    for (index, matcher) in required_matchers.iter().enumerate() {
                        if !matcher.is_match(&file.path) {
                            continue;
                        }
                        required_path_matches[index] = true;
                        if required_path_files[index].is_none() && path_filter.allows(&file.path) {
                            required_path_files[index] = Some(file.clone());
                        }
                    }
                }
            }
        }

        coverage.unmatched_focus_paths = request
            .focus_paths
            .iter()
            .zip(&focus_path_matches)
            .filter(|(_, matched)| **matched == 0)
            .map(|(pattern, _)| pattern.clone())
            .collect();
        let minimum_focus_fragments = request.minimum_fragments_per_focus_path.unwrap_or(1);
        if !request.focus_paths.is_empty() {
            coverage.focus_path_coverage = request
                .focus_paths
                .iter()
                .zip(focus_path_matches)
                .map(|(pattern, indexed_paths)| ContextFocusPathCoverage {
                    pattern: pattern.clone(),
                    indexed_paths,
                    minimum_fragments: minimum_focus_fragments,
                    selected_fragments: 0,
                    satisfied: false,
                })
                .collect();
        }
        coverage.unmatched_include_paths = request
            .include_paths
            .iter()
            .zip(include_path_matches)
            .filter(|(_, matched)| !matched)
            .map(|(pattern, _)| pattern.clone())
            .collect();
        coverage.unmatched_must_include_paths = request
            .must_include_paths
            .iter()
            .zip(&required_path_matches)
            .filter(|(_, matched)| !**matched)
            .map(|(pattern, _)| pattern.clone())
            .collect();

        let path_excerpt_requests = required_path_files
            .iter()
            .flatten()
            .map(|file| StoredExcerptRequest {
                file_id: file.id,
                desired_start_line: 1,
                desired_end_line: 40,
                required_start_line: 1,
                required_end_line: 1,
                max_lines: 40,
            })
            .collect::<Vec<_>>();
        phases.record_stored_excerpts(&path_excerpt_requests);
        let path_excerpts = phases.measure(ContextTimedPhase::StoredExcerpt, || {
            self.stored_excerpts(session, &path_excerpt_requests)
        })?;
        for ((pattern, file), excerpt) in request
            .must_include_paths
            .iter()
            .zip(required_path_files)
            .filter_map(|(pattern, file)| file.map(|file| (pattern, file)))
            .zip(path_excerpts)
        {
            let Some(excerpt) = excerpt else { continue };
            candidates.push(
                Candidate::new(
                    file.path,
                    excerpt.start_line,
                    excerpt.end_line,
                    excerpt.content,
                )
                .match_kind("must_path")
                .concept(format!("must:path:{pattern}"), 2.0)
                .representation("required_path")
                .exact(2.0)
                .focus_boost(2.0),
            );
        }

        let mut exact_names = Vec::new();
        let mut seen_exact_names = HashSet::new();
        for name in request
            .focus_symbols
            .iter()
            .chain(&request.must_include_symbols)
        {
            if seen_exact_names.insert(name.clone()) {
                exact_names.push(name.clone());
            }
        }
        phases.counters.exact_symbol_names = exact_names.len();
        let required_names = request
            .must_include_symbols
            .iter()
            .map(String::as_str)
            .collect::<HashSet<_>>();
        let mut exact_presence = HashSet::new();
        let mut allowed_required_hits = HashMap::<String, SymbolHit>::new();
        for names in exact_names.chunks(MAX_EXACT_SYMBOL_BATCH_NAMES) {
            check_cancelled(cancellation)?;
            phases.counters.exact_symbol_batches =
                phases.counters.exact_symbol_batches.saturating_add(1);
            let results = phases.measure(ContextTimedPhase::ExactSymbolLookup, || {
                session.find_symbols_exact_batch(names, MAX_IMPORT_SYMBOLS)
            })?;
            for (name, hits) in names.iter().zip(results) {
                phases.record_primitive("exact_symbol", || {
                    format!("case_sensitive:true:limit:{MAX_IMPORT_SYMBOLS}:name:{name}")
                });
                phases.counters.exact_symbol_hits =
                    phases.counters.exact_symbol_hits.saturating_add(hits.len());
                if hits.is_empty() {
                    continue;
                }
                exact_presence.insert(name.clone());
                if required_names.contains(name.as_str())
                    && let Some(hit) = hits.into_iter().find(|hit| path_filter.allows(&hit.path))
                {
                    allowed_required_hits.insert(name.clone(), hit);
                }
            }
        }
        for symbol in &request.focus_symbols {
            check_cancelled(cancellation)?;
            if !exact_presence.contains(symbol) {
                coverage.unmatched_focus_symbols.push(symbol.clone());
            }
        }
        let mut required_symbol_hits = Vec::<(String, SymbolHit)>::new();
        for symbol in &request.must_include_symbols {
            check_cancelled(cancellation)?;
            if !exact_presence.contains(symbol) {
                coverage.unmatched_must_include_symbols.push(symbol.clone());
                continue;
            }
            if let Some(hit) = allowed_required_hits.get(symbol).cloned() {
                required_symbol_hits.push((symbol.clone(), hit));
            }
        }
        let required_symbol_budget = request
            .token_budget
            .saturating_div(required_symbol_hits.len().max(1))
            .max(1);
        let symbol_excerpt_requests = required_symbol_hits
            .iter()
            .map(|(_, hit)| AdaptiveExcerptRequest {
                file_id: hit.symbol.file_id,
                declaration_start: hit.symbol.start_line,
                declaration_end: hit.symbol.end_line,
                matched_line: hit.symbol.start_line,
                token_budget: required_symbol_budget,
            })
            .collect::<Vec<_>>();
        phases.record_adaptive_excerpts(&symbol_excerpt_requests);
        let symbol_excerpts = phases.measure(ContextTimedPhase::AdaptiveExcerpt, || {
            self.adaptive_context_excerpts(session, &symbol_excerpt_requests)
        })?;
        for (((symbol, hit), excerpt), rank) in required_symbol_hits
            .into_iter()
            .zip(symbol_excerpts)
            .zip(0usize..)
        {
            let Some(excerpt) = excerpt else { continue };
            candidates.push(
                Candidate::new(
                    hit.path,
                    excerpt.start_line,
                    excerpt.end_line,
                    excerpt.content,
                )
                .match_kind("must_symbol")
                .concept(format!("must:symbol:{symbol}"), 2.0)
                .representation("required_symbol")
                .symbol_name(hit.symbol.name)
                .target_range(hit.symbol.start_line, hit.symbol.end_line)
                .exact(2.0)
                .symbol(2.0)
                .focus_boost(2.0)
                .channel("must_symbol", rank),
            );
        }

        Ok(coverage)
    }

    fn finalize_strict_scope_coverage(
        &self,
        session: &ReadSession,
        request: &ContextRequest,
        selected_paths: &[String],
        coverage: &mut ContextCoverageReceipt,
    ) -> Result<()> {
        for focus in &mut coverage.focus_path_coverage {
            let matcher = PathMatcher::new(std::slice::from_ref(&focus.pattern))?;
            focus.selected_fragments = selected_paths
                .iter()
                .filter(|path| matcher.is_match(path))
                .count();
            focus.satisfied =
                focus.indexed_paths > 0 && focus.selected_fragments >= focus.minimum_fragments;
        }

        if request.strict_changed_paths {
            let changed_paths = request
                .changed_paths
                .iter()
                .map(String::as_str)
                .collect::<HashSet<_>>();
            let mut indexed_paths = 0usize;
            for path in &request.changed_paths {
                if session.find_file(path)?.is_some() {
                    indexed_paths = indexed_paths.saturating_add(1);
                }
            }
            let selected_fragments = selected_paths
                .iter()
                .filter(|path| changed_paths.contains(path.as_str()))
                .count();
            coverage.changed_path_coverage = Some(ContextChangedPathCoverage {
                resolved_paths: changed_paths.len(),
                indexed_paths,
                selected_fragments,
                satisfied: !changed_paths.is_empty() && indexed_paths > 0 && selected_fragments > 0,
            });
        }

        let focus_coverage_is_required =
            request.strict_focus_paths || request.minimum_fragments_per_focus_path.is_some();
        if focus_coverage_is_required || request.strict_changed_paths {
            coverage.strict_scope_satisfied = Some(
                (!focus_coverage_is_required
                    || coverage
                        .focus_path_coverage
                        .iter()
                        .all(|focus| focus.satisfied))
                    && coverage
                        .changed_path_coverage
                        .as_ref()
                        .is_none_or(|changed| changed.satisfied),
            );
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
        let path_filter = PathFilter::new(
            &expansion.request.include_paths,
            &expansion.request.exclude_paths,
        )?;
        let mut pending = Vec::new();
        for target in targets {
            check_cancelled(expansion.cancellation)?;
            let Some((_, seed_concepts)) = seed_paths.get(target.seed_index) else {
                continue;
            };
            let target_path = &target.target_file.path;
            if !path_filter.allows(target_path) {
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
    /// A single `base_revision` resolves committed and working-tree paths since
    /// that revision, including untracked files. `BASE..HEAD` instead resolves
    /// an immutable revision range. Explicit `changed_paths` are merged with
    /// either result. When neither input is supplied, strict changed-path
    /// either result unless strict changed-path scope makes the explicit paths
    /// authoritative. When neither input is supplied, strict changed-path
    /// requests use the current working tree; otherwise `None` preserves
    /// task-only behavior.
    fn resolve_diff_scope(
        &self,
        request: &ContextRequest,
    ) -> Result<(Option<DiffScopeReceipt>, HashSet<String>, bool)> {
        let has_base = request
            .base_revision
            .as_deref()
            .is_some_and(|rev| !rev.trim().is_empty());
        let has_paths = !request.changed_paths.is_empty();
        let revision = request
            .base_revision
            .as_deref()
            .filter(|revision| !revision.trim().is_empty());
        let immutable_range = revision.map(parse_revision_range).transpose()?.flatten();
        let explicit_hard_scope = has_paths && request.strict_changed_paths;
        let git_result = match (revision, immutable_range) {
            (Some(_), Some((base, head))) if explicit_hard_scope => {
                Some(git_diff_identity(&self.config.root, base, Some(head))?)
            }
            (Some(revision), None) if explicit_hard_scope => {
                Some(git_diff_identity(&self.config.root, revision, None)?)
            }
            (Some(_), Some((base, head))) => Some(git_diff_paths_between(
                &self.config.root,
                base,
                head,
                MAX_DIFF_CHANGED_PATHS,
            )?),
            (Some(revision), None) => Some(git_diff_paths(
                &self.config.root,
                revision,
                MAX_DIFF_CHANGED_PATHS,
            )?),
            (None, None) => None,
            (None, Some(_)) => unreachable!("a range comes from a revision"),
        };
        let working_tree_status = git_working_tree_status(&self.config.root, GIT_CHANGED_PATHS_MAX);
        if !working_tree_status.available {
            tracing::debug!("working-tree signal unavailable");
        }
        let working_tree_paths = working_tree_status.changed_paths;
        let working_tree_state_available = working_tree_status.available;
        if !has_base && !has_paths && !request.strict_changed_paths {
            return Ok((None, working_tree_paths, working_tree_state_available));
        }
        if let Some(git_result) = git_result {
            let mut changed_paths = request.changed_paths.clone();
            if !explicit_hard_scope {
                let mut resolved_paths = git_result.changed_paths;
                if immutable_range.is_none() {
                    resolved_paths.extend(working_tree_paths.iter().cloned());
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
            }
            changed_paths.sort();
            changed_paths.dedup();
            return Ok((
                Some(DiffScopeReceipt {
                    base_revision: Some(git_result.base_revision),
                    head_revision: Some(git_result.head_revision),
                    changed_paths,
                    indexed_changed_paths: 0,
                    evidence: None,
                }),
                working_tree_paths,
                working_tree_state_available,
            ));
        }
        let mut resolved_paths = if has_paths {
            request.changed_paths.clone()
        } else {
            working_tree_paths.iter().cloned().collect::<Vec<_>>()
        };
        resolved_paths.sort();
        resolved_paths.dedup();
        Ok((
            Some(DiffScopeReceipt {
                base_revision: None,
                head_revision: None,
                changed_paths: resolved_paths,
                indexed_changed_paths: 0,
                evidence: None,
            }),
            working_tree_paths,
            working_tree_state_available,
        ))
    }

    /// Select ranked task evidence within an exact source-token budget.
    pub async fn context(&self, request: ContextRequest) -> Result<ContextResponse> {
        self.context_with_options(request, ServiceCallOptions::default())
            .await
    }

    /// Select ranked task evidence under explicit serialized-response controls.
    pub async fn context_with_options(
        &self,
        request: ContextRequest,
        options: ServiceCallOptions,
    ) -> Result<ContextResponse> {
        let accounted = self
            .context_cancellable_with_workflow(
                request,
                ContextWorkflow::Auto,
                options,
                CancellationToken::new(),
            )
            .await?;
        Ok(self.record_context_response(accounted))
    }

    /// Select context and attach compact provenance for a host-triggered handoff.
    pub async fn context_with_handoff(
        &self,
        request: ContextRequest,
        handoff: HandoffManifestRequest,
    ) -> Result<ContextResponse> {
        let accounted = self
            .context_cancellable_with_workflow_and_handoff(
                request,
                ContextWorkflow::Auto,
                Some(handoff),
                ServiceCallOptions::default(),
                CancellationToken::new(),
            )
            .await?;
        Ok(self.record_context_response(accounted))
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
        let accounted = self
            .context_cancellable_with_workflow(
                request,
                ContextWorkflow::Auto,
                ServiceCallOptions::default(),
                cancellation,
            )
            .await?;
        let mut response = accounted.response;
        set_routing_consistency(&mut response, consistency);
        self.finalize_response(&mut response)?;
        self.record_token_savings(
            accounted.operation,
            accounted.baseline_source_tokens,
            &response.meta,
        );
        Ok(response)
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
        let accounted = self
            .context_cancellable_with_workflow(
                request,
                workflow,
                ServiceCallOptions::default(),
                cancellation,
            )
            .await?;
        let mut response = accounted.response;
        set_routing_consistency(&mut response, consistency);
        self.finalize_response(&mut response)?;
        self.record_token_savings(
            accounted.operation,
            accounted.baseline_source_tokens,
            &response.meta,
        );
        Ok(response)
    }

    /// Retrieve context with an opt-in handoff manifest under explicit adapter policy.
    pub async fn context_with_handoff_workflow_consistency_cancellable(
        &self,
        request: ContextRequest,
        handoff: HandoffManifestRequest,
        workflow: ContextWorkflow,
        consistency: IndexConsistency,
        cancellation: CancellationToken,
    ) -> Result<ContextResponse> {
        self.validate_context_request(&request)?;
        validate_handoff_context_request(&request, &handoff)?;
        self.apply_consistency(consistency, cancellation.clone())
            .await?;
        let accounted = self
            .context_cancellable_with_workflow_and_handoff(
                request,
                workflow,
                Some(handoff),
                ServiceCallOptions::default(),
                cancellation,
            )
            .await?;
        let mut response = accounted.response;
        set_routing_consistency(&mut response, consistency);
        self.finalize_response(&mut response)?;
        self.record_token_savings(
            accounted.operation,
            accounted.baseline_source_tokens,
            &response.meta,
        );
        Ok(response)
    }

    pub async fn context_cancellable(
        &self,
        request: ContextRequest,
        cancellation: CancellationToken,
    ) -> Result<ContextResponse> {
        let accounted = self
            .context_cancellable_with_workflow(
                request,
                ContextWorkflow::Auto,
                ServiceCallOptions::default(),
                cancellation,
            )
            .await?;
        Ok(self.record_context_response(accounted))
    }

    /// Retrieve context under adapter policy and explicit response controls.
    #[allow(clippy::too_many_arguments)]
    pub async fn context_with_options_workflow_consistency_cancellable(
        &self,
        request: ContextRequest,
        handoff: Option<HandoffManifestRequest>,
        workflow: ContextWorkflow,
        consistency: IndexConsistency,
        options: ServiceCallOptions,
        cancellation: CancellationToken,
    ) -> Result<ContextResponse> {
        if options.max_response_tokens() == Some(0) {
            return Err(Error::InvalidInput {
                field: "max_response_tokens",
                reason: "must be greater than zero",
            });
        }
        self.validate_context_request(&request)?;
        if let Some(handoff) = &handoff {
            validate_handoff_context_request(&request, handoff)?;
        }
        self.apply_consistency(consistency, cancellation.clone())
            .await?;
        let accounted = self
            .context_cancellable_with_workflow_and_handoff(
                request,
                workflow,
                handoff,
                options,
                cancellation,
            )
            .await?;
        let mut response = accounted.response;
        set_routing_consistency(&mut response, consistency);
        self.finalize_response(&mut response)?;
        if let Some(max_response_tokens) = options.max_response_tokens()
            && response.meta.total_response_tokens > max_response_tokens
        {
            return Err(Error::RequestLimitExceeded {
                field: "max_response_tokens",
                requested: response.meta.total_response_tokens,
                limit: max_response_tokens,
            });
        }
        self.record_token_savings(
            accounted.operation,
            accounted.baseline_source_tokens,
            &response.meta,
        );
        Ok(response)
    }

    async fn context_cancellable_with_workflow(
        &self,
        request: ContextRequest,
        workflow: ContextWorkflow,
        options: ServiceCallOptions,
        cancellation: CancellationToken,
    ) -> Result<AccountedContextResponse> {
        self.context_cancellable_with_workflow_and_handoff(
            request,
            workflow,
            None,
            options,
            cancellation,
        )
        .await
    }

    async fn context_cancellable_with_workflow_and_handoff(
        &self,
        request: ContextRequest,
        workflow: ContextWorkflow,
        handoff: Option<HandoffManifestRequest>,
        options: ServiceCallOptions,
        cancellation: CancellationToken,
    ) -> Result<AccountedContextResponse> {
        let this = self.clone();
        self.blocking_executor
            .run(cancellation, move |cancellation| {
                let operation = if request.plan_only {
                    TokenAccountingOperation::ContextPlan
                } else {
                    TokenAccountingOperation::Context
                };
                let (evaluation, baseline_source_tokens) = this.context_sync(
                    request,
                    workflow,
                    handoff,
                    options,
                    cancellation,
                    CandidateDiagnostics::Omit,
                    ContextSignals::PRODUCTION,
                )?;
                Ok(AccountedContextResponse {
                    response: evaluation.response,
                    baseline_source_tokens,
                    operation,
                })
            })
            .await
    }

    fn record_context_response(&self, accounted: AccountedContextResponse) -> ContextResponse {
        self.record_token_savings(
            accounted.operation,
            accounted.baseline_source_tokens,
            &accounted.response.meta,
        );
        accounted.response
    }

    /// Retrieve context and expose pre-selection candidate paths for evaluation.
    ///
    /// Production adapters should use [`Self::context`]. This method exists for
    /// frozen retrieval benchmarks and does not alter the MCP response schema.
    pub async fn context_evaluation(&self, request: ContextRequest) -> Result<ContextEvaluation> {
        let this = self.clone();
        self.blocking_executor
            .run(CancellationToken::new(), move |cancellation| {
                this.context_sync(
                    request,
                    ContextWorkflow::Implementation,
                    None,
                    ServiceCallOptions::default(),
                    cancellation,
                    CandidateDiagnostics::Collect,
                    ContextSignals::PRODUCTION,
                )
                .map(|(evaluation, _)| evaluation)
            })
            .await
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
        self.blocking_executor
            .run(CancellationToken::new(), move |cancellation| {
                this.context_sync(
                    request,
                    ContextWorkflow::Implementation,
                    None,
                    ServiceCallOptions::default(),
                    cancellation,
                    CandidateDiagnostics::Collect,
                    ContextSignals::evaluation(policy),
                )
                .map(|(evaluation, _)| evaluation)
            })
            .await
    }

    fn append_workflow_candidates(
        &self,
        session: &ReadSession,
        request: &ContextRequest,
        workflow: ContextWorkflow,
        cancellation: &CancellationToken,
        candidates: &mut Vec<Candidate>,
    ) -> Result<(Option<WorkflowReceipt>, Vec<String>)> {
        if !matches!(
            workflow,
            ContextWorkflow::Contribution | ContextWorkflow::Review
        ) {
            return Ok((None, Vec::new()));
        }

        let mut matches = Vec::new();
        let path_filter = PathFilter::new(&request.include_paths, &request.exclude_paths)?;
        let mut path_excluded = Vec::new();
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
                    if path_filter.allows(&file.path) {
                        matches.push((file, score, family));
                    } else {
                        path_excluded.push(file.path);
                    }
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
        Ok((
            Some(WorkflowReceipt {
                guidance_candidates,
                template_candidates,
                validation_candidates,
                owner_test_candidates,
                missing_families,
            }),
            path_excluded,
        ))
    }

    fn build_diff_evidence(
        &self,
        session: &ReadSession,
        request: &ContextRequest,
        scope: &DiffScopeReceipt,
        workflow: ContextWorkflow,
        cancellation: &CancellationToken,
    ) -> Result<DiffEvidenceReceipt> {
        let mut changed_symbols = Vec::new();
        let mut relationships = BTreeSet::new();
        let mut gaps = Vec::new();
        let immutable_range = request
            .base_revision
            .as_deref()
            .and_then(|revision| parse_revision_range(revision).ok().flatten())
            .is_some();
        let changed_hunks = if let Some(base_revision) = &scope.base_revision {
            let head_revision = if immutable_range {
                Some(scope.head_revision.as_deref().ok_or_else(|| {
                    Error::InternalFailure("immutable diff scope has no head revision".into())
                })?)
            } else {
                None
            };
            let mut hunks = git_diff_hunks_scoped(
                &self.config.root,
                base_revision,
                head_revision,
                &scope.changed_paths,
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
                            hunk.start_line <= hunk.end_line
                                && symbol.start_line <= hunk.end_line
                                && symbol.end_line >= hunk.start_line
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

        let semantic_change = if workflow == ContextWorkflow::Review && !request.plan_only {
            let semantic_paths = scoped_paths
                .iter()
                .map(|path| (*path).clone())
                .collect::<Vec<_>>();
            let mut semantic = if immutable_range {
                let base_revision = scope.base_revision.as_deref().ok_or_else(|| {
                    Error::InternalFailure("immutable diff scope has no base revision".into())
                })?;
                let head_revision = scope.head_revision.as_deref().ok_or_else(|| {
                    Error::InternalFailure("immutable diff scope has no head revision".into())
                })?;
                classify_revision_changes(
                    &self.config.root,
                    base_revision,
                    head_revision,
                    &semantic_paths,
                    usize::try_from(self.config.max_file_bytes).unwrap_or(usize::MAX),
                    MAX_DIFF_EVIDENCE_SYMBOLS,
                )
            } else {
                DiffSemanticChangeReceipt {
                    symbol_changes: Vec::new(),
                    configuration_changes: Vec::new(),
                    owner_tests: Vec::new(),
                    gaps: vec!["semantic_change_requires_immutable_range".into()],
                }
            };
            if scope.changed_paths.len() > semantic_paths.len() {
                semantic
                    .gaps
                    .push("semantic_changed_paths_truncated".into());
            }
            if owner_test_scan_truncated {
                semantic.gaps.push("owner_test_scan_truncated".into());
            }
            semantic.owner_tests = owner_test_coverage(
                &scoped_paths,
                &relationships,
                owner_test_scan_truncated,
                &mut semantic.gaps,
            );
            semantic.gaps.sort();
            semantic.gaps.dedup();
            Some(semantic)
        } else {
            None
        };
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
            semantic_change,
            gaps,
        })
    }

    fn context_response_with_receipt_reserve(
        &self,
        response: &ContextResponse,
        request: &ContextRequest,
    ) -> Result<ContextResponse> {
        let mut sized = response.clone();
        if !request.plan_only {
            let receipt_id = request
                .receipt_id
                .clone()
                .unwrap_or_else(|| "rffffffffffffffff".into());
            let selected = sized.fragments.len();
            sized.meta.receipt_id = Some(receipt_id.clone());
            sized.meta.receipt_suppressed_exact = selected;
            sized.meta.receipt_suppressed_overlap = selected;
            sized.meta.receipt_near_duplicates = selected;
            sized.warnings.push(format!(
                "{selected} returned fragments are semantic near-duplicates of prior receipt evidence"
            ));
            sized
                .warnings
                .push("all selected evidence was already covered by the receipt".into());
            if let Some(manifest) = &mut sized.handoff_manifest {
                manifest.receipt_id = Some(receipt_id);
            }
        }
        set_routing_consistency(&mut sized, IndexConsistency::ReconcileWorkingTree);
        self.finalize_response(&mut sized)?;
        Ok(sized)
    }

    fn context_response_tokens_with_receipt_reserve(
        &self,
        response: &ContextResponse,
        request: &ContextRequest,
    ) -> Result<usize> {
        let sized = self.context_response_with_receipt_reserve(response, request)?;
        let budget = ResponseBudget::new(&self.config.tokenizer, usize::MAX);
        let serialized_tokens = budget.serialized_tokens(&sized)?;
        debug_assert_eq!(serialized_tokens, sized.meta.total_response_tokens);
        Ok(serialized_tokens)
    }

    fn context_response_fits(
        &self,
        response: &ContextResponse,
        request: &ContextRequest,
        max_response_tokens: usize,
    ) -> Result<bool> {
        let sized = self.context_response_with_receipt_reserve(response, request)?;
        ResponseBudget::new(&self.config.tokenizer, max_response_tokens)
            .fits(&sized)
            .map_err(Into::into)
    }

    fn refresh_context_omission_warning(response: &mut ContextResponse) {
        response.warnings.retain(|warning| {
            warning
                .strip_suffix(" omitted")
                .is_none_or(|count| count.parse::<usize>().is_err())
        });
        let omitted = response
            .omission_summary
            .path_excluded
            .saturating_add(response.omission_summary.known_hash)
            .saturating_add(response.omission_summary.budget_or_result_limit);
        if omitted > 0 {
            response.warnings.insert(0, format!("{omitted} omitted"));
        }
    }

    fn trim_context_selection(response: &mut ContextResponse, keep: usize) {
        let (removed, removed_tokens) = if let Some(plan) = &mut response.plan {
            let removed = plan.candidates.len().saturating_sub(keep);
            let removed_tokens = plan
                .candidates
                .iter()
                .skip(keep)
                .map(|candidate| candidate.estimated_tokens)
                .sum::<usize>();
            plan.candidates.truncate(keep);
            plan.estimated_source_tokens =
                plan.estimated_source_tokens.saturating_sub(removed_tokens);
            plan.result_complete &= removed == 0;
            (removed, removed_tokens)
        } else {
            let removed = response.fragments.len().saturating_sub(keep);
            let removed_tokens = response
                .fragments
                .iter()
                .skip(keep)
                .map(|fragment| fragment.token_count)
                .sum::<usize>();
            response.fragments.truncate(keep);
            response.receipt.fragment_hashes.truncate(keep);
            (removed, removed_tokens)
        };
        response.meta.source_tokens = response.meta.source_tokens.saturating_sub(removed_tokens);
        response.meta.emitted_tokens = response.meta.source_tokens;
        response.omission_summary.budget_or_result_limit = response
            .omission_summary
            .budget_or_result_limit
            .saturating_add(removed);
        Self::refresh_context_omission_warning(response);
    }

    fn fit_context_response(
        &self,
        response: &mut ContextResponse,
        request: &ContextRequest,
        max_response_tokens: usize,
    ) -> Result<()> {
        self.finalize_response(response)?;
        if self.context_response_fits(response, request, max_response_tokens)? {
            return Ok(());
        }

        response.omitted.clear();
        response.omission_summary.by_path.clear();
        response.omission_summary.by_language_or_file_type.clear();
        response.omission_summary.by_reason.clear();
        response.omission_summary.by_score_band.clear();
        if self.context_response_fits(response, request, max_response_tokens)? {
            self.finalize_response(response)?;
            return Ok(());
        }

        if let Some(scope) = &mut response.diff_scope {
            scope.evidence = None;
        }
        if self.context_response_fits(response, request, max_response_tokens)? {
            self.finalize_response(response)?;
            return Ok(());
        }

        response.routing = None;
        if self.context_response_fits(response, request, max_response_tokens)? {
            self.finalize_response(response)?;
            return Ok(());
        }

        if let Some(plan) = &mut response.plan {
            for candidate in &mut plan.candidates {
                candidate.reasons.clear();
            }
        }
        for fragment in &mut response.fragments {
            fragment.reason.clear();
        }
        if self.context_response_fits(response, request, max_response_tokens)? {
            self.finalize_response(response)?;
            return Ok(());
        }

        let can_reduce_selected = request.include_paths.is_empty()
            && request.must_include_paths.is_empty()
            && request.must_include_symbols.is_empty()
            && request.focus_paths.is_empty()
            && request.focus_symbols.is_empty()
            && !request.strict_focus_paths
            && request.minimum_fragments_per_focus_path.is_none()
            && request.base_revision.is_none()
            && request.changed_paths.is_empty()
            && !request.strict_changed_paths
            && response.handoff_manifest.is_none();
        if can_reduce_selected {
            let selected = response
                .plan
                .as_ref()
                .map_or(response.fragments.len(), |plan| plan.candidates.len());
            let omission_reserve = response
                .omission_summary
                .budget_or_result_limit
                .saturating_add(selected);
            let budget = ResponseBudget::new(&self.config.tokenizer, max_response_tokens);
            let keep = budget.largest_fitting_prefix(selected, |keep| {
                let mut candidate = response.clone();
                Self::trim_context_selection(&mut candidate, keep);
                candidate.omission_summary.budget_or_result_limit = omission_reserve;
                Self::refresh_context_omission_warning(&mut candidate);
                self.context_response_tokens_with_receipt_reserve(&candidate, request)
            })?;
            if let Some(keep) = keep {
                Self::trim_context_selection(response, keep);
                self.finalize_response(response)?;
                if self.context_response_fits(response, request, max_response_tokens)? {
                    return Ok(());
                }
                return Err(Error::InternalFailure(
                    "context prefix fitting violated its monotonic sizing reserve".into(),
                ));
            }
            Self::trim_context_selection(response, 0);
        }

        let minimum = self.context_response_tokens_with_receipt_reserve(response, request)?;
        Err(Error::RequestLimitExceeded {
            field: "max_response_tokens",
            requested: minimum,
            limit: max_response_tokens,
        })
    }

    #[allow(clippy::cognitive_complexity, clippy::too_many_arguments)]
    fn context_sync(
        &self,
        mut request: ContextRequest,
        workflow: ContextWorkflow,
        handoff: Option<HandoffManifestRequest>,
        options: ServiceCallOptions,
        cancellation: &CancellationToken,
        diagnostics: CandidateDiagnostics,
        signals: ContextSignals,
    ) -> Result<(ContextEvaluation, Option<usize>)> {
        check_cancelled(cancellation)?;
        if options.max_response_tokens() == Some(0) {
            return Err(Error::InvalidInput {
                field: "max_response_tokens",
                reason: "must be greater than zero",
            });
        }
        self.validate_context_request(&request)?;
        if let Some(handoff) = &handoff {
            validate_handoff_context_request(&request, handoff)?;
        }
        request.changed_paths = request
            .changed_paths
            .iter()
            .map(|path| normalize_relative(path))
            .collect::<Result<Vec<_>>>()?;
        let (diff_scope, mut changed_paths, working_tree_state_available) =
            self.resolve_diff_scope(&request)?;
        let working_tree_state = if !working_tree_state_available {
            HandoffWorkingTreeState::Unknown
        } else if changed_paths.is_empty() {
            HandoffWorkingTreeState::Clean
        } else {
            HandoffWorkingTreeState::Dirty
        };
        let working_tree_paths = changed_paths.iter().cloned().collect::<Vec<_>>();
        let mut scoped_request = request.clone();
        if let Some(scope) = &diff_scope {
            scoped_request.changed_paths = scope.changed_paths.clone();
        }
        if let Some(ref scope) = diff_scope {
            changed_paths.extend(scope.changed_paths.iter().cloned());
        }
        let path_filter = PathFilter::new(&request.include_paths, &request.exclude_paths)?;
        let strict_changed_paths = request.strict_changed_paths.then(|| {
            scoped_request
                .changed_paths
                .iter()
                .map(String::as_str)
                .collect::<HashSet<_>>()
        });
        self.consistent(|session, generation| {
            let facet_plan = facets::plan(&request.task, MAX_CONTEXT_QUERIES);
            let queries = facet_plan.queries;
            let mut phases = ContextPhaseTracker::new(diagnostics, generation);
            let candidate_generation_started = phases.timer();
            phases.counters.queries_planned = queries.len();
            phases.counters.queries_executed = queries
                .iter()
                .filter(|query| !query.has_facet(FacetKind::TestIntent))
                .count();
            let terms = queries
                .iter()
                .map(|query| query.value.clone())
                .collect::<Vec<_>>();
            let path_scorer = ContextPathScorer::new(&terms, &request.task);
            let mut candidates = Vec::new();
            let mut path_excluded_candidates = Vec::new();
            let mut query_fusion = HashMap::<String, HashMap<String, f64>>::new();
            let mut coverage = self.append_constraint_candidates(
                session,
                &request,
                cancellation,
                &mut candidates,
                &mut phases,
            )?;

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
                let symbol_results = phases.measure(ContextTimedPhase::SymbolSearch, || {
                    session.search_symbols(term, false, MAX_CONTEXT_HITS_PER_SOURCE)
                })?;
                phases.record_primitive(
                    "symbol_search",
                    || format!(
                        "case_sensitive:false:limit:{MAX_CONTEXT_HITS_PER_SOURCE}:query:{term}"
                    ),
                );
                phases.counters.symbol_candidates = phases
                    .counters
                    .symbol_candidates
                    .saturating_add(symbol_results.len());
                let mut symbol_hits = Vec::new();
                for (rank, hit) in symbol_results
                    .into_iter()
                    .enumerate()
                {
                    check_cancelled(cancellation)?;
                    if path_filter.allows(&hit.path)
                        && strict_changed_paths
                            .as_ref()
                            .is_none_or(|paths| paths.contains(hit.path.as_str()))
                    {
                        symbol_hits.push((rank, hit));
                    } else {
                        path_excluded_candidates.push(hit.path);
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
                phases.record_adaptive_excerpts(&symbol_excerpt_requests);
                let symbol_excerpts = phases.measure(ContextTimedPhase::AdaptiveExcerpt, || {
                    self.adaptive_context_excerpts(session, &symbol_excerpt_requests)
                })?;
                for ((rank, hit), excerpt) in symbol_hits
                    .into_iter()
                    .zip(symbol_excerpts)
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
                    .path_score(path_scorer.score(&hit.path))
                    .change_boost(change_boost);
                    candidates.push(annotate_candidate(candidate, query, "symbol", rank));
                }
                let reference_results = if signals.caller {
                    phases.measure(ContextTimedPhase::ReferenceSearch, || {
                        session.search_references(term, false, MAX_CONTEXT_HITS_PER_SOURCE)
                    })?
                } else {
                    Vec::new()
                };
                if signals.caller {
                    phases.record_primitive(
                        "reference_search",
                        || format!(
                            "case_sensitive:false:limit:{MAX_CONTEXT_HITS_PER_SOURCE}:query:{term}"
                        ),
                    );
                }
                phases.counters.reference_candidates = phases
                    .counters
                    .reference_candidates
                    .saturating_add(reference_results.len());
                let mut reference_hits = Vec::new();
                for (rank, hit) in reference_results.into_iter().enumerate() {
                    check_cancelled(cancellation)?;
                    if path_filter.allows(&hit.path)
                        && strict_changed_paths
                            .as_ref()
                            .is_none_or(|paths| paths.contains(hit.path.as_str()))
                    {
                        reference_hits.push((rank, hit));
                    } else {
                        path_excluded_candidates.push(hit.path);
                    }
                }
                let reference_locations = reference_hits
                    .iter()
                    .map(|(_, hit)| (hit.reference.file_id, hit.reference.start_line))
                    .collect::<Vec<_>>();
                phases.record_enclosing_locations(&reference_locations);
                let enclosing = phases.measure(ContextTimedPhase::EnclosingLookup, || {
                    session.find_enclosing_symbols_batch(&reference_locations)
                })?;
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
                phases.record_adaptive_excerpts(&adaptive_requests);
                let mut adaptive_excerpts = vec![None; reference_hits.len()];
                let hydrated_adaptive =
                    phases.measure(ContextTimedPhase::AdaptiveExcerpt, || {
                        self.adaptive_context_excerpts(session, &adaptive_requests)
                    })?;
                for (index, excerpt) in adaptive_indices
                    .into_iter()
                    .zip(hydrated_adaptive)
                {
                    adaptive_excerpts[index] = excerpt;
                }
                let mut fallback_indices = Vec::new();
                let mut fallback_requests = Vec::new();
                for (index, ((_, hit), adaptive)) in
                    reference_hits.iter().zip(&adaptive_excerpts).enumerate()
                {
                    if adaptive.is_some() {
                        continue;
                    }
                    fallback_indices.push(index);
                    fallback_requests.push(StoredExcerptRequest {
                        file_id: hit.reference.file_id,
                        desired_start_line: hit.reference.start_line.saturating_sub(2).max(1),
                        desired_end_line: hit.reference.end_line.saturating_add(2),
                        required_start_line: hit.reference.start_line,
                        required_end_line: hit.reference.end_line,
                        max_lines: 12,
                    });
                }
                phases.record_stored_excerpts(&fallback_requests);
                let mut fallback_excerpts = vec![None; reference_hits.len()];
                let hydrated_fallback = phases.measure(ContextTimedPhase::StoredExcerpt, || {
                    self.stored_excerpts(session, &fallback_requests)
                })?;
                for (index, excerpt) in fallback_indices
                    .into_iter()
                    .zip(hydrated_fallback)
                {
                    fallback_excerpts[index] = excerpt;
                }
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
                    .path_score(path_scorer.score(&hit.path))
                    .change_boost(change_boost);
                    candidates.push(annotate_candidate(candidate, query, "reference", rank));
                }
                let term_regex = compile_literal_regex(term, false)?;
                let lexical = phases.measure(ContextTimedPhase::LexicalSearch, || {
                    if term.chars().count() >= 3 {
                        session.search_trigram(term, MAX_CONTEXT_LEXICAL_HITS)
                    } else {
                        session.search_word(&fts_quote(term), MAX_CONTEXT_LEXICAL_HITS)
                    }
                })?;
                let lexical_kind = if term.chars().count() >= 3 {
                    "trigram"
                } else {
                    "word"
                };
                phases.record_primitive(
                    lexical_kind,
                    || format!("limit:{MAX_CONTEXT_LEXICAL_HITS}:query:{term}"),
                );
                phases.counters.lexical_candidate_chunks = phases
                    .counters
                    .lexical_candidate_chunks
                    .saturating_add(lexical.len());
                let mut lexical_hits = Vec::new();
                let lexical_verify_started = phases.timer();
                for (rank, hit) in lexical.into_iter().enumerate() {
                    check_cancelled(cancellation)?;
                    if !path_filter.allows(&hit.path)
                        || strict_changed_paths
                            .as_ref()
                            .is_some_and(|paths| !paths.contains(hit.path.as_str()))
                    {
                        path_excluded_candidates.push(hit.path);
                        continue;
                    }
                    phases.counters.lexical_chunks_verified =
                        phases.counters.lexical_chunks_verified.saturating_add(1);
                    let Some(facts) =
                        term_regex
                            .as_ref()
                            .and_then(|matcher| analyze_lexical_match(&hit, matcher, 2))
                    else {
                        continue;
                    };
                    phases.counters.lexical_matches =
                        phases.counters.lexical_matches.saturating_add(1);
                    lexical_hits.push((rank, hit, facts));
                }
                phases.record_elapsed(
                    ContextTimedPhase::LexicalVerify,
                    lexical_verify_started,
                );
                let lexical_locations = lexical_hits
                    .iter()
                    .map(|(_, hit, facts)| (hit.file_id, facts.matched_line))
                    .collect::<Vec<_>>();
                phases.record_enclosing_locations(&lexical_locations);
                let enclosing = phases.measure(ContextTimedPhase::EnclosingLookup, || {
                    session.find_enclosing_symbols_batch(&lexical_locations)
                })?;
                let mut adaptive_indices = Vec::new();
                let mut adaptive_requests = Vec::new();
                for (index, ((_, hit, facts), symbol)) in
                    lexical_hits.iter().zip(enclosing).enumerate()
                {
                    if let Some(symbol) = symbol {
                        adaptive_indices.push(index);
                        adaptive_requests.push(AdaptiveExcerptRequest {
                            file_id: hit.file_id,
                            declaration_start: symbol.start_line,
                            declaration_end: symbol.end_line,
                            matched_line: facts.matched_line,
                            token_budget: excerpt_budget(
                                request.token_budget,
                                ContextExcerptKind::Text,
                            ),
                        });
                    }
                }
                phases.record_adaptive_excerpts(&adaptive_requests);
                let mut adaptive_excerpts = vec![None; lexical_hits.len()];
                let hydrated_adaptive =
                    phases.measure(ContextTimedPhase::AdaptiveExcerpt, || {
                        self.adaptive_context_excerpts(session, &adaptive_requests)
                    })?;
                for (index, excerpt) in adaptive_indices
                    .into_iter()
                    .zip(hydrated_adaptive)
                {
                    adaptive_excerpts[index] = excerpt;
                }
                for ((rank, hit, facts), adaptive) in
                    lexical_hits.into_iter().zip(adaptive_excerpts)
                {
                    check_cancelled(cancellation)?;
                    let excerpt = adaptive.unwrap_or(StoredExcerpt {
                        content: facts.search_hit.excerpt.clone(),
                        start_line: facts.search_hit.start_line,
                        end_line: facts.search_hit.end_line,
                    });
                    if query.fuse {
                        record_query_hit(
                            &mut query_fusion,
                            &facts.search_hit.path,
                            &query.fusion_key,
                            query.weight,
                            rank,
                        );
                    }
                    let change_boost = Self::file_change_boost(
                        Some(hit.generation),
                        &facts.search_hit.path,
                        &changed_paths,
                        request.prior_repository_generation,
                    );
                    let candidate = Candidate::new(
                        &facts.search_hit.path,
                        excerpt.start_line,
                        excerpt.end_line,
                        excerpt.content,
                    )
                    .match_kind("text")
                    .concept(concept, query.concept_weight)
                    .exact(query.weight)
                    .bm25((-hit.score).max(0.0) * 1_000_000.0)
                    .path_score(path_scorer.score(&facts.search_hit.path))
                    .lexical_frequency_penalty(
                        (facts.occurrences.saturating_sub(5) as f64 / 20.0).min(1.0),
                    )
                    .change_boost(change_boost);
                    candidates.push(annotate_candidate(candidate, query, "text", rank));
                }
            }

            apply_query_fusion(&mut candidates, &query_fusion);
            let resolved_workflow = resolve_context_workflow(workflow, &request.task);
            let workflow_started = phases.timer();
            let (workflow_receipt, workflow_path_excluded) = self.append_workflow_candidates(
                session,
                &scoped_request,
                resolved_workflow,
                cancellation,
                &mut candidates,
            )?;
            path_excluded_candidates.extend(workflow_path_excluded);

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
            if let Some(paths) = &strict_changed_paths {
                candidates.retain(|candidate| paths.contains(candidate.path.as_str()));
                path_excluded_candidates.retain(|path| paths.contains(path.as_str()));
            }
            if let Some(started) = workflow_started {
                phases.timings.workflow_generation_ms =
                    started.elapsed().as_secs_f64() * 1_000.0;
            }
            if let Some(started) = candidate_generation_started {
                phases.timings.candidate_generation_ms =
                    started.elapsed().as_secs_f64() * 1_000.0;
            }

            let ranking_started = phases.timer();
            let candidate_path_count = candidates
                .iter()
                .map(|candidate| candidate.path.as_str())
                .collect::<BTreeSet<_>>()
                .len();
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
            let mut response = ranking::select_with_tokenizer_and_context_exclusions(
                candidates,
                &scoped_request,
                generation,
                self.config.tokenizer,
                &self.config.context_exclude_paths,
                &path_excluded_candidates,
            );
            coverage.covered_must_include_paths =
                std::mem::take(&mut response.coverage.covered_must_include_paths);
            coverage.covered_must_include_symbols =
                std::mem::take(&mut response.coverage.covered_must_include_symbols);
            coverage.partial_must_include_symbols =
                std::mem::take(&mut response.coverage.partial_must_include_symbols);
            coverage.uncovered_must_include_paths =
                std::mem::take(&mut response.coverage.uncovered_must_include_paths);
            coverage.uncovered_must_include_symbols =
                std::mem::take(&mut response.coverage.uncovered_must_include_symbols);
            coverage
                .uncovered_must_include_paths
                .retain(|pattern| !coverage.unmatched_must_include_paths.contains(pattern));
            coverage
                .uncovered_must_include_symbols
                .retain(|symbol| !coverage.unmatched_must_include_symbols.contains(symbol));
            let selected_paths: Vec<String> = response.plan.as_ref().map_or_else(
                || {
                    response
                        .fragments
                        .iter()
                        .map(|fragment| fragment.path.clone())
                        .collect()
                },
                |plan| {
                    plan.candidates
                        .iter()
                        .map(|candidate| candidate.path.clone())
                        .collect()
                },
            );
            self.finalize_strict_scope_coverage(
                session,
                &scoped_request,
                &selected_paths,
                &mut coverage,
            )?;
            response.coverage = coverage;
            let uncovered = response
                .coverage
                .uncovered_must_include_paths
                .len()
                .saturating_add(response.coverage.uncovered_must_include_symbols.len());
            if uncovered > 0 {
                response.warnings.push(format!(
                    "{uncovered} indexed must-cover requirements were not selected"
                ));
            }
            let partial = response.coverage.partial_must_include_symbols.len();
            if partial > 0 {
                let subject = if partial == 1 {
                    "1 required symbol was".to_owned()
                } else {
                    format!("{partial} required symbols were")
                };
                response.warnings.push(format!(
                    "{subject} returned only partially; inspect target ranges and truncated fragments"
                ));
            }
            let unmatched = response
                .coverage
                .unmatched_must_include_paths
                .len()
                .saturating_add(response.coverage.unmatched_must_include_symbols.len());
            if unmatched > 0 {
                response.warnings.push(format!(
                    "{unmatched} must-cover requirements matched no indexed evidence"
                ));
            }
            let unmatched_hints = response
                .coverage
                .unmatched_focus_paths
                .len()
                .saturating_add(response.coverage.unmatched_focus_symbols.len())
                .saturating_add(response.coverage.unmatched_include_paths.len());
            if unmatched_hints > 0 {
                response.warnings.push(format!(
                    "{unmatched_hints} focus or include constraints matched no indexed evidence"
                ));
            }
            let underfilled_focus_paths = response
                .coverage
                .focus_path_coverage
                .iter()
                .filter(|focus| !focus.satisfied)
                .count();
            if underfilled_focus_paths > 0 {
                response.warnings.push(format!(
                    "{underfilled_focus_paths} focus path constraints did not meet minimum fragment coverage"
                ));
            }
            if response
                .coverage
                .changed_path_coverage
                .as_ref()
                .is_some_and(|changed| !changed.satisfied)
            {
                response.warnings.push(
                    "strict changed-path scope produced no indexed selected evidence".into(),
                );
            }
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
                scope.evidence = (!request.plan_only || request.verbose_diagnostics)
                    .then(|| {
                        self.build_diff_evidence(
                            session,
                            &scoped_request,
                            &scope,
                            resolved_workflow,
                            cancellation,
                        )
                    })
                    .transpose()?;
                response.routing = build_context_routing(
                    &request,
                    &scope,
                    candidate_path_count,
                    &selected_paths,
                );
                if let Some(routing) = &response.routing {
                    let concentration = if routing.weakly_concentrated {
                        "; selected evidence is weakly concentrated"
                    } else {
                        ""
                    };
                    response.warnings.push(format!(
                        "context spans {} changed paths across {} path groups{concentration}",
                        routing.changed_paths, routing.path_groups_total
                    ));
                }
                response.diff_scope = Some(scope);
            }
            if let Some(handoff) = &handoff {
                let evidence = response
                    .fragments
                    .iter()
                    .map(|fragment| HandoffEvidence {
                        path: fragment.path.clone(),
                        start_line: fragment.start_line,
                        end_line: fragment.end_line,
                        content_hash: fragment.content_hash.clone(),
                    })
                    .collect::<Vec<_>>();
                let resolved_head = response
                    .diff_scope
                    .as_ref()
                    .and_then(|scope| scope.head_revision.clone());
                let (commit_revision, commit_revision_available) =
                    if let Some(head) = resolved_head {
                        (Some(head), true)
                    } else {
                        match git_head_revision(&self.config.root) {
                            Ok(head) => (Some(head), true),
                            Err(error) => {
                                tracing::debug!(%error, "handoff Git identity unavailable");
                                (None, false)
                            }
                        }
                    };
                response.handoff_manifest = Some(handoff::build(
                    &request,
                    handoff,
                    &response,
                    evidence,
                    HandoffProvenance {
                        commit_revision,
                        commit_revision_available,
                        working_tree_state: if commit_revision_available {
                            working_tree_state
                        } else {
                            HandoffWorkingTreeState::Unknown
                        },
                        working_tree_paths: working_tree_paths.clone(),
                    },
                ));
            }
            if let Some(max_response_tokens) = options.max_response_tokens() {
                self.fit_context_response(&mut response, &request, max_response_tokens)?;
            }
            if !request.plan_only {
                let receipt_candidates = response
                    .fragments
                    .iter()
                    .map(|fragment| {
                        ReceiptEvidence::new(
                            fragment.path.clone(),
                            fragment.start_line,
                            fragment.end_line,
                            fragment.content_hash.clone(),
                            Some(&fragment.content),
                        )
                    })
                    .collect::<Vec<_>>();
                let receipt = self.evaluate_receipt(
                    request.receipt_id.as_deref(),
                    generation,
                    &receipt_candidates,
                )?;
                response.fragments = response
                    .fragments
                    .into_iter()
                    .zip(&receipt.decisions)
                    .filter_map(|(fragment, decision)| {
                        matches!(
                            decision,
                            ReceiptDecision::Return | ReceiptDecision::ReturnNearDuplicate
                        )
                        .then_some(fragment)
                    })
                    .collect();
                response.receipt.fragment_hashes = response
                    .fragments
                    .iter()
                    .map(|fragment| fragment.content_hash.clone())
                    .collect();
                response.meta.source_tokens = response
                    .fragments
                    .iter()
                    .map(|fragment| self.config.tokenizer.count(&fragment.content))
                    .sum();
                response.meta.emitted_tokens = response.meta.source_tokens;
                receipt.apply_meta(&mut response.meta);
                if response.meta.receipt_near_duplicates > 0 {
                    response.warnings.push(format!(
                        "{} returned fragments are semantic near-duplicates of prior receipt evidence",
                        response.meta.receipt_near_duplicates
                    ));
                }
                if response.fragments.is_empty() {
                    if response.meta.receipt_suppressed_exact
                        + response.meta.receipt_suppressed_overlap
                        > 0
                    {
                        response
                            .warnings
                            .push("all selected evidence was already covered by the receipt".into());
                    } else if response.omission_summary.budget_or_result_limit == 0 {
                        response
                            .warnings
                            .push("no relevant indexed evidence found".into());
                    }
                }
            }
            if let Some(manifest) = &mut response.handoff_manifest {
                manifest.receipt_id.clone_from(&response.meta.receipt_id);
            }
            self.finalize_response(&mut response)?;
            if let Some(max_response_tokens) = options.max_response_tokens()
                && response.meta.total_response_tokens > max_response_tokens
            {
                return Err(Error::InternalFailure(
                    "context response exceeded its fitted serialized-response budget".into(),
                ));
            }
            let baseline_source_tokens = if request.plan_only {
                None
            } else {
                let paths = response
                    .fragments
                    .iter()
                    .map(|fragment| fragment.path.clone())
                    .collect::<BTreeSet<_>>()
                    .into_iter()
                    .collect::<Vec<_>>();
                session.whole_file_source_tokens(&paths, self.config.tokenizer.name())?
            };
            if let Some(started) = ranking_started {
                phases.timings.ranking_finalize_ms =
                    started.elapsed().as_secs_f64() * 1_000.0;
            }
            let (phases, timings, primitive_keys) =
                phases.finish(generated_candidates.len());
            Ok((
                ContextEvaluation {
                    response,
                    generated_candidate_paths,
                    generated_candidates,
                    phases,
                    timings,
                    primitive_keys,
                },
                baseline_source_tokens,
            ))
        })
    }
}

fn validate_handoff_context_request(
    request: &ContextRequest,
    handoff: &HandoffManifestRequest,
) -> Result<()> {
    handoff::validate_request(handoff)?;
    if request.plan_only {
        return Err(Error::InvalidInput {
            field: "plan_only",
            reason: "cannot be combined with a handoff manifest",
        });
    }
    Ok(())
}

fn set_routing_consistency(response: &mut ContextResponse, consistency: IndexConsistency) {
    if let Some(routing) = &mut response.routing {
        routing.consistency = consistency;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lexical_match_facts_share_first_match_and_saturate_frequency_count() {
        let content = (0..100)
            .map(|index| format!("needle_{index}"))
            .collect::<Vec<_>>()
            .join("\n");
        let hit = ChunkHit {
            chunk_id: 1,
            file_id: 1,
            path: "src/lib.rs".into(),
            content,
            start_line: 10,
            end_line: 109,
            start_byte: 100,
            end_byte: 1_000,
            token_count: 100,
            generation: 1,
            score: 0.0,
        };
        let matcher = regex::RegexBuilder::new("needle")
            .case_insensitive(true)
            .build()
            .expect("matcher");

        let facts = analyze_lexical_match(&hit, &matcher, 2).expect("match facts");

        assert_eq!(facts.matched_line, 10);
        assert_eq!(facts.search_hit.start_line, 10);
        assert_eq!(facts.occurrences, LEXICAL_OCCURRENCE_SATURATION);
    }

    #[test]
    fn revision_ranges_require_two_explicit_endpoints() {
        assert_eq!(
            parse_revision_range("main~1..main").expect("valid range"),
            Some(("main~1", "main"))
        );
        assert_eq!(
            parse_revision_range("origin/main").expect("single revision"),
            None
        );
        for invalid in ["..main", "main..", "main...head"] {
            assert!(parse_revision_range(invalid).is_err(), "{invalid}");
        }
    }

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
            include_paths: Vec::new(),
            must_include_paths: Vec::new(),
            must_include_symbols: Vec::new(),
            max_fragments: None,
            plan_only: false,
            focus_paths: Vec::new(),
            strict_focus_paths: false,
            minimum_fragments_per_focus_path: None,
            focus_symbols: Vec::new(),
            exclude_paths: Vec::new(),
            known_hashes: Vec::new(),
            receipt_id: None,
            prior_repository_generation: None,
            base_revision: None,
            changed_paths: vec!["src/core.rs".into()],
            strict_changed_paths: false,
            verbose_diagnostics: false,
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
    fn context_queries_cover_early_domain_tail_intent_and_natural_phrases() {
        let terms = context_queries(
            "Trace how index generations are published atomically and how request snapshot consistency is preserved for concurrent readers",
            12,
        );

        assert!(terms.len() <= 8);
        assert!(terms.iter().any(|term| term.value == "index"));
        assert!(terms.iter().any(|term| term.value == "snapshot"));
        assert!(terms.iter().any(|term| term.value == "concurrent"));
        assert!(
            terms
                .iter()
                .any(|term| term.value == "snapshot consistency")
        );
        assert!(terms.iter().any(|term| term.value == "concurrent readers"));
        assert!(!terms.iter().any(|term| term.value == "Trace"));
        assert!(!terms.iter().any(|term| term.value == "how"));
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
    fn oversized_diff_routing_is_bounded_deterministic_and_preserves_retry_inputs() {
        assert_eq!(context_path_group("README.md"), "<root>");
        let request = ContextRequest {
            task: "review the changed implementation".into(),
            token_budget: 4_000,
            include_paths: Vec::new(),
            must_include_paths: Vec::new(),
            must_include_symbols: Vec::new(),
            max_fragments: None,
            plan_only: false,
            focus_paths: Vec::new(),
            strict_focus_paths: false,
            minimum_fragments_per_focus_path: None,
            focus_symbols: Vec::new(),
            exclude_paths: Vec::new(),
            known_hashes: vec!["held".into()],
            receipt_id: None,
            prior_repository_generation: None,
            base_revision: Some("origin/main".into()),
            changed_paths: Vec::new(),
            strict_changed_paths: false,
            verbose_diagnostics: false,
        };
        let changed_paths = (0..12)
            .flat_map(|index| {
                [
                    format!("src/browser/file_{index}.rs"),
                    format!("src/runtime/file_{index}.rs"),
                    format!("tests/scenario_{index}.rs"),
                ]
            })
            .collect::<Vec<_>>();
        let scope = DiffScopeReceipt {
            base_revision: Some("base".into()),
            head_revision: Some("head".into()),
            changed_paths,
            indexed_changed_paths: 36,
            evidence: None,
        };
        let fragments = [
            ContextFragment {
                path: "src/browser/file_0.rs".into(),
                start_line: 1,
                end_line: 1,
                target_start_line: None,
                target_end_line: None,
                truncated: false,
                representation: "source".into(),
                content: "browser".into(),
                content_hash: "browser-hash".into(),
                score: 1.0,
                reason: "text".into(),
                token_count: 1,
            },
            ContextFragment {
                path: "src/runtime/file_0.rs".into(),
                start_line: 1,
                end_line: 1,
                target_start_line: None,
                target_end_line: None,
                truncated: false,
                representation: "source".into(),
                content: "runtime".into(),
                content_hash: "runtime-hash".into(),
                score: 1.0,
                reason: "text".into(),
                token_count: 1,
            },
        ];

        let selected_paths = fragments
            .iter()
            .map(|fragment| fragment.path.clone())
            .collect::<Vec<_>>();
        let routing = build_context_routing(&request, &scope, 24, &selected_paths)
            .expect("oversized routing");

        assert_eq!(routing.changed_paths, 36);
        assert_eq!(routing.path_groups_total, 3);
        assert!(routing.weakly_concentrated);
        assert_eq!(
            routing
                .path_groups
                .iter()
                .map(|group| group.prefix.as_str())
                .collect::<Vec<_>>(),
            vec!["src/browser", "src/runtime", "tests"]
        );
        assert_eq!(routing.suggestions.len(), 3);
        assert_eq!(routing.suggestions[0].include_paths, vec!["src/browser/**"]);
        assert_eq!(routing.base_revision.as_deref(), Some("origin/main"));
        assert_eq!(routing.known_hashes, vec!["held"]);
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
