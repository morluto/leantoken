use std::collections::{BTreeSet, HashMap, HashSet};

use crate::WorkflowEvidence;
use crate::text::{expand_identifier, expand_terms, identifier_words};

const MAX_ATOMS: usize = 16;
const MAX_FACET_VARIANTS: usize = 4;
const MAX_QUOTED_PHRASES: usize = 4;
const MAX_BEHAVIOR_TERMS: usize = 6;
const MAX_NATURAL_PHRASES: usize = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum FacetKind {
    ExactAtom,
    PrimaryChange,
    FailureTrace,
    PreserveConstraint,
    Symbol,
    Path,
    Behavior,
    TestIntent,
    Configuration,
}

impl FacetKind {
    pub(super) const fn as_str(self) -> &'static str {
        match self {
            Self::ExactAtom => "exact_atom",
            Self::PrimaryChange => "primary_change",
            Self::FailureTrace => "failure_trace",
            Self::PreserveConstraint => "preserve_constraint",
            Self::Symbol => "symbol",
            Self::Path => "path",
            Self::Behavior => "behavior",
            Self::TestIntent => "test_intent",
            Self::Configuration => "configuration",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(super) struct TaskFacet {
    pub(super) kind: FacetKind,
    pub(super) original: String,
    pub(super) variants: Vec<String>,
    pub(super) weight: f64,
    pub(super) fusion_key: String,
}

#[derive(Debug, Clone, PartialEq)]
pub(super) struct ContextQuery {
    pub(super) value: String,
    pub(super) weight: f64,
    pub(super) concept_weight: f64,
    pub(super) fusion_key: String,
    pub(super) fuse: bool,
    pub(super) facets: BTreeSet<FacetKind>,
    pub(super) exact_variant: bool,
}

impl ContextQuery {
    pub(super) fn has_facet(&self, kind: FacetKind) -> bool {
        self.facets.contains(&kind)
    }

    pub(super) fn facet_names(&self) -> impl Iterator<Item = &'static str> + '_ {
        self.facets.iter().copied().map(FacetKind::as_str)
    }

    pub(super) fn is_generic_test_path_prior(&self) -> bool {
        self.has_facet(FacetKind::TestIntent)
    }
}

#[derive(Debug, Clone)]
pub(super) struct FacetPlan {
    pub(super) queries: Vec<ContextQuery>,
}

#[derive(Clone, Copy)]
struct QuerySpec<'a> {
    value: &'a str,
    fusion_key: &'a str,
    exact_variant: bool,
    fuse: bool,
    weight: f64,
    concept_weight: f64,
}

pub(super) fn plan(task: &str, limit: usize) -> FacetPlan {
    if limit == 0 {
        return FacetPlan {
            queries: Vec::new(),
        };
    }

    let terms = task_terms(task);
    let wants_tests = terms.iter().any(|term| is_test_term(term));
    let atoms = technical_atoms(task);
    let atom_parts = atoms
        .iter()
        .flat_map(|atom| {
            std::iter::once(atom.to_ascii_lowercase()).chain(
                expand_terms(atom)
                    .into_iter()
                    .map(|term| term.to_ascii_lowercase()),
            )
        })
        .collect::<HashSet<_>>();
    let mut facets = Vec::new();

    for atom in &atoms {
        push_facet(
            &mut facets,
            FacetKind::ExactAtom,
            atom,
            vec![atom.clone()],
            1.0,
        );
        let kind = classify_atom(atom);
        push_facet(
            &mut facets,
            kind,
            atom,
            technical_variants(atom),
            match kind {
                FacetKind::Path => 0.95,
                FacetKind::Configuration => 0.9,
                _ => 0.95,
            },
        );
    }

    let behavior_terms = select_behavior_terms(
        terms
            .iter()
            .filter(|term| {
                !is_test_term(term)
                    && !is_stop_word(term)
                    && !atom_parts.contains(&term.to_ascii_lowercase())
            })
            .cloned()
            .collect(),
        MAX_BEHAVIOR_TERMS,
    );
    let quoted_phrases = quoted_phrases(task)
        .into_iter()
        .take(MAX_QUOTED_PHRASES)
        .collect::<Vec<_>>();
    let natural_phrase_limit = if atoms.is_empty() {
        MAX_NATURAL_PHRASES
    } else {
        1
    };
    let primary_task = primary_task_text(task);
    let mut selected_natural_phrases =
        natural_phrases(&primary_task, &atom_parts, natural_phrase_limit);
    if selected_natural_phrases.len() < natural_phrase_limit && primary_task != task {
        for phrase in natural_phrases(task, &atom_parts, natural_phrase_limit) {
            if selected_natural_phrases
                .iter()
                .all(|existing| !existing.eq_ignore_ascii_case(&phrase))
            {
                selected_natural_phrases.push(phrase);
                if selected_natural_phrases.len() == natural_phrase_limit {
                    break;
                }
            }
        }
    }
    let natural_phrases = selected_natural_phrases;

    for phrase in quoted_phrases.iter().chain(&natural_phrases) {
        push_facet(
            &mut facets,
            FacetKind::Behavior,
            phrase,
            phrase_variants(phrase),
            0.85,
        );
    }

    for term in &behavior_terms {
        let kind = if is_configuration_term(term) {
            FacetKind::Configuration
        } else {
            FacetKind::Behavior
        };
        push_facet(
            &mut facets,
            kind,
            term,
            vec![term.clone()],
            prose_weight(term),
        );
    }

    if wants_tests {
        push_facet(
            &mut facets,
            FacetKind::TestIntent,
            "test",
            ["test", "spec", "fixture", "regression"]
                .map(str::to_owned)
                .to_vec(),
            0.65,
        );
    }

    let mut queries = build_queries(
        task,
        &facets,
        &behavior_terms,
        &natural_phrases,
        limit,
        wants_tests,
    );
    annotate_task_roles(task, &mut queries);
    FacetPlan { queries }
}

pub(super) fn plan_with_workflow_evidence(
    task: &str,
    evidence: &WorkflowEvidence,
    limit: usize,
) -> FacetPlan {
    if evidence.is_empty() {
        return plan(task, limit);
    }
    let mut queries = Vec::new();
    let mut seen = HashSet::new();

    for symbol in evidence.symbols.iter().take(4) {
        let Some(value) = normalize_atom(symbol) else {
            continue;
        };
        push_workflow_query(
            &mut queries,
            &mut seen,
            ContextQuery {
                fusion_key: format!("workflow:symbol:{}", value.to_ascii_lowercase()),
                value,
                weight: 1.25,
                concept_weight: 1.0,
                fuse: false,
                facets: [
                    FacetKind::ExactAtom,
                    FacetKind::PrimaryChange,
                    FacetKind::Symbol,
                ]
                .into_iter()
                .collect(),
                exact_variant: true,
            },
            limit,
        );
    }
    for path in evidence.paths.iter().take(2) {
        push_workflow_query(
            &mut queries,
            &mut seen,
            ContextQuery {
                fusion_key: format!("workflow:path:{}", path.to_ascii_lowercase()),
                value: path.clone(),
                weight: 1.1,
                concept_weight: 0.95,
                fuse: false,
                facets: [
                    FacetKind::ExactAtom,
                    FacetKind::PrimaryChange,
                    FacetKind::Path,
                ]
                .into_iter()
                .collect(),
                exact_variant: true,
            },
            limit,
        );
    }
    for trace in evidence.failure_traces.iter().take(1) {
        for mut query in plan(trace, 2).queries {
            query.facets.insert(FacetKind::FailureTrace);
            query.weight += 0.2;
            query.concept_weight = query.concept_weight.max(0.9);
            query.fusion_key = format!("workflow:failure_trace:{}", query.fusion_key);
            push_workflow_query(&mut queries, &mut seen, query, limit);
        }
    }
    for intent in evidence.test_intents.iter().take(1) {
        for mut query in plan(intent, 2).queries {
            query.facets.insert(FacetKind::TestIntent);
            query.weight += 0.1;
            query.concept_weight = query.concept_weight.max(0.75);
            query.fusion_key = format!("workflow:test_intent:{}", query.fusion_key);
            push_workflow_query(&mut queries, &mut seen, query, limit);
        }
    }
    for query in plan(task, limit).queries {
        push_workflow_query(&mut queries, &mut seen, query, limit);
    }
    FacetPlan { queries }
}

fn push_workflow_query(
    queries: &mut Vec<ContextQuery>,
    seen: &mut HashSet<String>,
    query: ContextQuery,
    limit: usize,
) {
    if queries.len() >= limit {
        return;
    }
    let key = query.value.to_ascii_lowercase();
    if seen.insert(key) {
        queries.push(query);
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum TaskRole {
    PrimaryChange,
    FailureTrace,
    PreserveConstraint,
    TestIntent,
}

impl TaskRole {
    const fn facet(self) -> FacetKind {
        match self {
            Self::PrimaryChange => FacetKind::PrimaryChange,
            Self::FailureTrace => FacetKind::FailureTrace,
            Self::PreserveConstraint => FacetKind::PreserveConstraint,
            Self::TestIntent => FacetKind::TestIntent,
        }
    }
}

fn annotate_task_roles(task: &str, queries: &mut [ContextQuery]) {
    let clauses = task_clauses(task);
    for (position, clause) in clauses.iter().enumerate() {
        let roles = clause_roles(clause, position == 0);
        if roles.is_empty() {
            continue;
        }
        let normalized = clause.to_ascii_lowercase();
        let terms = task_terms(clause)
            .into_iter()
            .chain(technical_atoms(clause))
            .flat_map(|term| std::iter::once(term.clone()).chain(expand_terms(&term)))
            .map(|term| term.to_ascii_lowercase())
            .collect::<HashSet<_>>();
        for query in queries.iter_mut().filter(|query| {
            let value = query.value.to_ascii_lowercase();
            normalized.contains(&value)
                || terms.contains(&value)
                || terms.contains(&query.fusion_key.to_ascii_lowercase())
        }) {
            let generic_test_path_prior = query.is_generic_test_path_prior();
            query.facets.extend(
                roles
                    .iter()
                    .filter(|role| **role != TaskRole::TestIntent || generic_test_path_prior)
                    .map(|role| role.facet()),
            );
        }
    }
    if !queries
        .iter()
        .any(|query| query.has_facet(FacetKind::PrimaryChange))
        && let Some(query) = queries.first_mut()
    {
        query.facets.insert(FacetKind::PrimaryChange);
    }
}

fn task_clauses(task: &str) -> Vec<&str> {
    task.split(['\n', ';'])
        .flat_map(|part| part.split(". "))
        .flat_map(split_role_transitions)
        .map(str::trim)
        .filter(|clause| !clause.is_empty())
        .collect()
}

fn split_role_transitions(part: &str) -> Vec<&str> {
    let normalized = part.to_ascii_lowercase();
    let mut boundaries = [" while ", " without ", " but "]
        .into_iter()
        .filter_map(|marker| normalized.find(marker).map(|position| position + 1))
        .collect::<Vec<_>>();
    boundaries.sort_unstable();
    boundaries.dedup();
    let mut start = 0;
    let mut clauses = Vec::with_capacity(boundaries.len() + 1);
    for boundary in boundaries {
        clauses.push(&part[start..boundary]);
        start = boundary;
    }
    clauses.push(&part[start..]);
    clauses
}

fn clause_roles(clause: &str, first: bool) -> Vec<TaskRole> {
    let clause = clause.to_ascii_lowercase();
    let has_any = |markers: &[&str]| markers.iter().any(|marker| clause.contains(marker));
    let words = clause
        .split(|character: char| !character.is_alphanumeric() && character != '_')
        .filter(|word| !word.is_empty())
        .collect::<HashSet<_>>();
    let has_word = |markers: &[&str]| markers.iter().any(|marker| words.contains(marker));
    let preserve = has_word(&[
        "preserve",
        "preserved",
        "preserves",
        "preserving",
        "keep",
        "keeping",
        "retain",
        "retained",
        "retaining",
        "unchanged",
    ]) || has_any(&[
        "without changing",
        "must remain",
        "do not change",
        "while maintaining",
    ]);
    let test = has_word(&["test", "tests", "regression", "spec", "coverage", "assert"]);
    let primary = !preserve
        && !test
        && (first
            || has_word(&[
                "fix",
                "implement",
                "change",
                "add",
                "make",
                "refactor",
                "trace",
                "identify",
                "find",
                "investigate",
                "diagnose",
                "support",
            ]));
    let failure = has_word(&[
        "error",
        "fail",
        "failed",
        "fails",
        "failure",
        "not_ready",
        "panic",
        "timeout",
        "broken",
        "incorrect",
    ]) || clause.contains("instead of");
    [
        primary.then_some(TaskRole::PrimaryChange),
        failure.then_some(TaskRole::FailureTrace),
        preserve.then_some(TaskRole::PreserveConstraint),
        test.then_some(TaskRole::TestIntent),
    ]
    .into_iter()
    .flatten()
    .collect()
}

pub(super) fn primary_task_text(task: &str) -> String {
    let primary = task_clauses(task)
        .into_iter()
        .enumerate()
        .filter(|(position, clause)| {
            clause_roles(clause, *position == 0).contains(&TaskRole::PrimaryChange)
        })
        .map(|(_, clause)| clause)
        .collect::<Vec<_>>()
        .join(" ");
    if primary.is_empty() {
        task.to_owned()
    } else {
        primary
    }
}

fn build_queries(
    task: &str,
    facets: &[TaskFacet],
    behavior_terms: &[String],
    natural_phrases: &[String],
    limit: usize,
    wants_tests: bool,
) -> Vec<ContextQuery> {
    let available = limit.saturating_sub(usize::from(wants_tests));
    let mut queries = Vec::new();
    let mut positions = HashMap::<String, usize>::new();
    let code_terms = code_tokens(task);
    let exact_terms = facets
        .iter()
        .filter(|facet| facet.kind == FacetKind::ExactAtom)
        .map(|facet| facet.original.as_str())
        .collect::<Vec<_>>();
    let exact_limit = if exact_terms.is_empty() {
        0
    } else {
        exact_terms.len().min(available.min(4))
    };
    let remaining = available.saturating_sub(exact_limit);
    let phrase_count = natural_phrases
        .len()
        .min(if code_terms.is_empty() {
            MAX_NATURAL_PHRASES
        } else {
            1
        })
        .min(remaining / 2);
    let phrase_slots = phrase_count.saturating_mul(2);
    let prose_reserve = behavior_terms
        .len()
        .min(available.saturating_sub(exact_limit + phrase_slots));

    for value in exact_terms.into_iter().take(exact_limit) {
        push_fusion_query(
            facets,
            &mut queries,
            &mut positions,
            QuerySpec {
                value,
                fusion_key: &value.to_ascii_lowercase(),
                exact_variant: true,
                fuse: true,
                weight: context_query_weight(value, true),
                concept_weight: context_query_weight(value, true) + 1.0,
            },
            exact_limit,
        );
    }
    for value in behavior_terms.iter().take(prose_reserve) {
        push_fusion_query(
            facets,
            &mut queries,
            &mut positions,
            QuerySpec {
                value,
                fusion_key: &value.to_ascii_lowercase(),
                exact_variant: false,
                fuse: false,
                weight: context_query_weight(value, false),
                concept_weight: context_query_weight(value, false),
            },
            available,
        );
    }

    for phrase in natural_phrases.iter().take(phrase_count) {
        push_fusion_query(
            facets,
            &mut queries,
            &mut positions,
            QuerySpec {
                value: phrase,
                fusion_key: &phrase.to_ascii_lowercase(),
                exact_variant: false,
                fuse: true,
                weight: context_query_weight(phrase, false),
                concept_weight: context_query_weight(phrase, false),
            },
            available,
        );
    }
    // One component per phrase finds code-style occurrences without expanding
    // every phrase into a second unbounded prose lane.
    for phrase in natural_phrases.iter().take(phrase_count) {
        let Some(component) = task_terms(phrase).into_iter().next() else {
            continue;
        };
        push_fusion_query(
            facets,
            &mut queries,
            &mut positions,
            QuerySpec {
                value: &component,
                fusion_key: &phrase.to_ascii_lowercase(),
                exact_variant: false,
                fuse: true,
                weight: context_query_weight(&component, false),
                concept_weight: context_query_weight(&component, false),
            },
            available,
        );
    }

    let mut round = 0usize;
    while queries.len() < available {
        let before = queries.len();
        for code_term in &code_terms {
            let expansions = expand_terms(code_term);
            let Some(value) = expansions.get(round) else {
                continue;
            };
            let weight = context_query_weight(value, true);
            push_fusion_query(
                facets,
                &mut queries,
                &mut positions,
                QuerySpec {
                    value,
                    fusion_key: &code_term.to_ascii_lowercase(),
                    exact_variant: false,
                    fuse: true,
                    weight,
                    concept_weight: weight + 1.0,
                },
                available,
            );
            if queries.len() == available {
                break;
            }
        }
        if queries.len() == before {
            break;
        }
        round += 1;
    }

    if wants_tests {
        let test_facet = facets
            .iter()
            .find(|facet| facet.kind == FacetKind::TestIntent)
            .expect("test intent facet");
        push_query(
            &mut queries,
            &mut positions,
            test_facet,
            QuerySpec {
                value: "test",
                fusion_key: "test",
                exact_variant: false,
                fuse: false,
                weight: 0.2,
                concept_weight: 0.2,
            },
            limit,
        );
    }
    queries
}

fn push_fusion_query(
    facets: &[TaskFacet],
    queries: &mut Vec<ContextQuery>,
    positions: &mut HashMap<String, usize>,
    spec: QuerySpec<'_>,
    limit: usize,
) {
    for facet in facets
        .iter()
        .filter(|facet| facet.fusion_key.eq_ignore_ascii_case(spec.fusion_key))
    {
        push_query(queries, positions, facet, spec, limit);
    }
}

fn push_query(
    queries: &mut Vec<ContextQuery>,
    positions: &mut HashMap<String, usize>,
    facet: &TaskFacet,
    spec: QuerySpec<'_>,
    limit: usize,
) {
    if queries.len() >= limit || spec.value.chars().count() < 2 {
        return;
    }
    if !spec.exact_variant && is_stop_word(spec.value) {
        return;
    }
    let normalized = spec.value.to_ascii_lowercase();
    if let Some(position) = positions.get(&normalized).copied() {
        let query = &mut queries[position];
        query.weight = query.weight.max(spec.weight);
        query.concept_weight = query.concept_weight.max(spec.concept_weight);
        query.exact_variant |= spec.exact_variant;
        query.fuse |= spec.fuse;
        query.facets.insert(facet.kind);
        return;
    }
    positions.insert(normalized, queries.len());
    queries.push(ContextQuery {
        value: spec.value.to_owned(),
        weight: spec.weight,
        concept_weight: spec.concept_weight,
        fusion_key: facet.fusion_key.clone(),
        fuse: spec.fuse,
        facets: BTreeSet::from([facet.kind]),
        exact_variant: spec.exact_variant,
    });
}

fn push_facet(
    facets: &mut Vec<TaskFacet>,
    kind: FacetKind,
    original: &str,
    variants: Vec<String>,
    weight: f64,
) {
    if original.is_empty()
        || facets
            .iter()
            .any(|facet| facet.kind == kind && facet.original.eq_ignore_ascii_case(original))
    {
        return;
    }
    let mut seen = HashSet::new();
    let variants = std::iter::once(original.to_owned())
        .chain(variants)
        .filter(|variant| variant.chars().count() >= 2 && seen.insert(variant.to_ascii_lowercase()))
        .take(MAX_FACET_VARIANTS)
        .collect();
    facets.push(TaskFacet {
        kind,
        original: original.to_owned(),
        variants,
        weight,
        fusion_key: original.to_ascii_lowercase(),
    });
}

pub(super) fn code_tokens(task: &str) -> Vec<String> {
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

pub(super) fn technical_atoms(task: &str) -> Vec<String> {
    let mut atoms = Vec::new();
    let mut seen = HashSet::new();
    for raw in task.split_whitespace() {
        for piece in raw.split('=').take(2) {
            let Some(atom) = normalize_atom(piece) else {
                continue;
            };
            if looks_technical(&atom) && seen.insert(atom.to_ascii_lowercase()) {
                atoms.push(atom);
                if atoms.len() == MAX_ATOMS {
                    return atoms;
                }
            }
        }
    }
    atoms
}

fn normalize_atom(raw: &str) -> Option<String> {
    if raw.starts_with("#[")
        && let Some(end) = raw.find(']')
    {
        return Some(raw[..=end].to_owned());
    }
    let start = raw
        .char_indices()
        .find(|(_, character)| character.is_alphanumeric() || matches!(character, '_' | '#' | '@'))
        .map(|(index, _)| index)?;
    let end = raw
        .char_indices()
        .rev()
        .find(|(_, character)| character.is_alphanumeric() || matches!(character, '_' | ']' | '>'))
        .map(|(index, character)| index + character.len_utf8())?;
    let raw = &raw[start..end];
    if raw.is_empty() {
        return None;
    }
    let raw = if !raw.contains('<')
        && let Some(call) = raw.find('(')
    {
        &raw[..call]
    } else {
        raw
    };
    (!raw.is_empty()).then(|| raw.to_owned())
}

fn looks_technical(value: &str) -> bool {
    value.starts_with("#[")
        || value.starts_with('@')
        || value.contains("::")
        || value.contains('.')
        || value.contains('/')
        || value.contains('_')
        || value.contains('<')
        || value
            .as_bytes()
            .windows(2)
            .any(|pair| pair[0].is_ascii_lowercase() && pair[1].is_ascii_uppercase())
        || (value.contains('-')
            && value
                .split('-')
                .all(|part| !part.is_empty() && part.chars().all(char::is_alphanumeric)))
        || (value.chars().count() >= 3
            && value.chars().any(char::is_alphabetic)
            && value
                .chars()
                .filter(|character| character.is_alphabetic())
                .all(char::is_uppercase))
}

fn classify_atom(atom: &str) -> FacetKind {
    if atom.contains('/') {
        FacetKind::Path
    } else if atom.starts_with("#[") || atom.starts_with('@') || is_configuration_term(atom) {
        FacetKind::Configuration
    } else {
        FacetKind::Symbol
    }
}

fn technical_variants(atom: &str) -> Vec<String> {
    let mut variants = Vec::new();
    if atom.contains('-') {
        variants.push(atom.replace('-', " "));
        variants.extend(
            atom.split('-')
                .filter(|part| part.chars().count() >= 3 && !is_stop_word(part))
                .map(str::to_owned),
        );
    }
    variants.extend(expand_terms(atom));
    variants.extend(identifier_words(atom));
    if atom.contains('/')
        && let Some(name) = atom.rsplit('/').next()
    {
        variants.push(name.to_owned());
        if let Some((stem, _)) = name.rsplit_once('.') {
            variants.push(stem.to_owned());
        }
    }
    if atom.contains(['.', ':'])
        && let Some(member) = atom.rsplit(['.', ':']).find(|part| !part.is_empty())
    {
        variants.extend(expand_identifier(member));
    }
    variants
        .into_iter()
        .filter(|variant| variant.chars().count() >= 3 && !is_stop_word(variant))
        .collect()
}

fn quoted_phrases(task: &str) -> Vec<String> {
    let mut phrases = Vec::new();
    let mut seen = HashSet::new();
    let chars = task.char_indices().collect::<Vec<_>>();
    let mut index = 0usize;
    while index < chars.len() {
        let (start, quote) = chars[index];
        if !matches!(quote, '\'' | '"' | '`') || (index > 0 && chars[index - 1].1.is_alphanumeric())
        {
            index += 1;
            continue;
        }
        let mut closing = index + 1;
        while closing < chars.len()
            && chars[closing].1 != quote
            && chars[closing].0.saturating_sub(start) <= 160
        {
            closing += 1;
        }
        if closing < chars.len() && chars[closing].1 == quote {
            let value = task[start + quote.len_utf8()..chars[closing].0].trim();
            if value.chars().count() >= 2 && seen.insert(value.to_ascii_lowercase()) {
                phrases.push(value.to_owned());
            }
            index = closing + 1;
        } else {
            index += 1;
        }
    }
    phrases
}

fn phrase_variants(phrase: &str) -> Vec<String> {
    std::iter::once(phrase.to_owned())
        .chain(
            task_terms(phrase)
                .into_iter()
                .filter(|term| !is_stop_word(term)),
        )
        .collect()
}

fn select_behavior_terms(terms: Vec<String>, limit: usize) -> Vec<String> {
    let mut seen = HashSet::new();
    let candidates = terms
        .into_iter()
        .enumerate()
        .filter(|(_, term)| seen.insert(term.to_ascii_lowercase()))
        .collect::<Vec<_>>();
    let prefix = candidates.len().min(limit.min(4));
    let mut selected = candidates.iter().take(prefix).cloned().collect::<Vec<_>>();
    let mut remaining = candidates.into_iter().skip(prefix).collect::<Vec<_>>();
    remaining.sort_by(|(left_position, left), (right_position, right)| {
        right
            .chars()
            .count()
            .cmp(&left.chars().count())
            .then_with(|| left_position.cmp(right_position))
            .then_with(|| left.cmp(right))
    });
    selected.extend(remaining.into_iter().take(limit.saturating_sub(prefix)));
    selected.sort_by_key(|(position, _)| *position);
    selected.into_iter().map(|(_, term)| term).collect()
}

fn natural_phrases(task: &str, excluded_terms: &HashSet<String>, limit: usize) -> Vec<String> {
    if limit == 0 {
        return Vec::new();
    }
    let terms = task_terms(task);
    let mut seen = HashSet::new();
    let mut candidates = terms
        .windows(2)
        .enumerate()
        .filter_map(|(position, pair)| {
            let lower = pair
                .iter()
                .map(|term| term.to_ascii_lowercase())
                .collect::<Vec<_>>();
            if pair.iter().any(|term| {
                is_test_term(term)
                    || is_stop_word(term)
                    || excluded_terms.contains(&term.to_ascii_lowercase())
            }) {
                return None;
            }
            let phrase = pair.join(" ");
            let normalized = phrase.to_ascii_lowercase();
            let specificity = lower.iter().map(|term| term.chars().count()).sum::<usize>();
            seen.insert(normalized)
                .then_some((position, specificity, phrase))
        })
        .collect::<Vec<_>>();
    candidates.sort_by(
        |(left_position, left_specificity, left), (right_position, right_specificity, right)| {
            right_specificity
                .cmp(left_specificity)
                .then_with(|| left_position.cmp(right_position))
                .then_with(|| left.cmp(right))
        },
    );
    candidates.truncate(limit);
    candidates.sort_by_key(|candidate| candidate.0);
    candidates
        .into_iter()
        .map(|(_, _, phrase)| phrase)
        .collect()
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
        "test" | "tests" | "testing" | "coverage" | "regression" | "spec"
    )
}

fn is_configuration_term(term: &str) -> bool {
    let lower = term.to_ascii_lowercase();
    lower.contains("config")
        || lower.contains("setting")
        || lower.contains("option")
        || lower.contains("feature")
        || lower.contains("server_name")
        || lower.starts_with("env_")
}

fn prose_weight(term: &str) -> f64 {
    match term.chars().count() {
        10.. => 0.8,
        7..=9 => 0.65,
        4..=6 => 0.45,
        _ => 0.25,
    }
}

fn is_stop_word(term: &str) -> bool {
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
            | "callback"
            | "change"
            | "does"
            | "each"
            | "ensure"
            | "every"
            | "fail"
            | "find"
            | "fix"
            | "for"
            | "from"
            | "how"
            | "if"
            | "in"
            | "into"
            | "is"
            | "it"
            | "its"
            | "keep"
            | "locate"
            | "loudly"
            | "make"
            | "must"
            | "not"
            | "of"
            | "on"
            | "one"
            | "only"
            | "or"
            | "preserve"
            | "same"
            | "so"
            | "than"
            | "then"
            | "the"
            | "this"
            | "to"
            | "trace"
            | "update"
            | "when"
            | "while"
            | "within"
            | "without"
            | "with"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn exact_atoms(task: &str) -> Vec<String> {
        technical_atoms(task)
    }

    fn facet(kind: FacetKind, original: &str, variants: Vec<String>) -> TaskFacet {
        let mut facets = Vec::new();
        push_facet(&mut facets, kind, original, variants, 1.0);
        facets.pop().expect("non-empty facet")
    }

    #[test]
    fn extracts_required_technical_atoms_without_stripping_exact_forms() {
        let task = "Fix Rack::Deflater, _.cloneDeep, res.send, #[serde(untagged)], \
            _pytest.monkeypatch.notset, renameTable, WithRequiredStructEnabled, \
            src/services/context.rs, snake_case, kebab-case, camelCase, PascalCase, \
            ERR_INVALID_CONFIG, and Result<Option<T>,Error>.";
        let atoms = exact_atoms(task);
        for expected in [
            "Rack::Deflater",
            "_.cloneDeep",
            "res.send",
            "#[serde(untagged)]",
            "_pytest.monkeypatch.notset",
            "renameTable",
            "WithRequiredStructEnabled",
            "src/services/context.rs",
            "snake_case",
            "kebab-case",
            "camelCase",
            "PascalCase",
            "ERR_INVALID_CONFIG",
            "Result<Option<T>,Error>",
        ] {
            assert!(
                atoms.iter().any(|atom| atom == expected),
                "missing {expected}: {atoms:?}"
            );
        }
    }

    #[test]
    fn exact_atom_is_the_first_variant_even_when_not_scheduled_as_a_query() {
        for atom in ["CONFIG", "#[serde(untagged)]"] {
            let facet = facet(FacetKind::ExactAtom, atom, vec![atom.to_owned()]);
            assert_eq!(facet.variants.first(), Some(&facet.original));
        }
    }

    #[test]
    fn punctuation_adjacent_calls_and_generic_signatures_keep_atom_boundaries() {
        let atoms =
            exact_atoms("Fix (res.send(payload)), `Rack::Deflater`, and Result<Option<T>,Error>.");

        assert!(atoms.iter().any(|atom| atom == "res.send"));
        assert!(atoms.iter().any(|atom| atom == "Rack::Deflater"));
        assert!(atoms.iter().any(|atom| atom == "Result<Option<T>,Error>"));
    }

    #[test]
    fn quoted_error_text_and_annotations_create_bounded_queries() {
        let task = "Handle @retry and report \"Failed to lookup view\" without changing behavior.";
        let plan = plan(task, 12);
        let annotation = technical_atoms(task)
            .into_iter()
            .find(|atom| atom == "@retry")
            .expect("configuration annotation");
        assert_eq!(classify_atom(&annotation), FacetKind::Configuration);
        let phrase = quoted_phrases(task)
            .into_iter()
            .find(|phrase| phrase == "Failed to lookup view")
            .expect("quoted behavior");
        let annotation = facet(
            FacetKind::Configuration,
            &annotation,
            technical_variants(&annotation),
        );
        let phrase = facet(FacetKind::Behavior, &phrase, phrase_variants(&phrase));
        assert!(plan.queries.len() <= 12);
        assert!(annotation.variants.len() <= MAX_FACET_VARIANTS);
        assert!(phrase.variants.len() <= MAX_FACET_VARIANTS);
    }

    #[test]
    fn expansion_is_deterministic_and_strictly_bounded() {
        let first = plan(
            "Fix Rack::Deflater and WithRequiredStructEnabled with regression coverage",
            8,
        );
        let second = plan(
            "Fix Rack::Deflater and WithRequiredStructEnabled with regression coverage",
            8,
        );
        assert_eq!(first.queries, second.queries);
        assert!(first.queries.len() <= 8);
        assert!(first.queries.len() > 3, "{first:?}");
        assert_eq!(
            first.queries.last().map(|query| query.value.as_str()),
            Some("test")
        );
    }

    #[test]
    fn technical_tasks_keep_exact_and_bounded_natural_query_lanes() {
        let plan = plan(
            "Trace render.AsciiJSON while snapshot consistency protects concurrent readers",
            8,
        );

        assert!(
            plan.queries
                .iter()
                .any(|query| query.value == "render.AsciiJSON" && query.exact_variant)
        );
        assert!(plan.queries.iter().any(|query| {
            query.value == "snapshot consistency" || query.value == "concurrent readers"
        }));
    }

    #[test]
    fn natural_language_terms_are_selected_across_the_complete_task() {
        let plan = plan(
            "Ensure ordinary behavior before bounded candidate generation and deterministic selection",
            12,
        );

        assert!(
            plan.queries
                .iter()
                .any(|query| query.value == "candidate generation")
        );
        assert!(
            plan.queries
                .iter()
                .any(|query| query.value == "deterministic selection")
        );
        assert!(plan.queries.len() <= 12);
    }

    #[test]
    fn qualified_atoms_keep_exact_owner_and_bounded_symbol_expansions() {
        let plan = plan(
            "Fix render.AsciiJSON for non-BMP JSON with UTF-16 while preserving BMP and ASCII behavior",
            10,
        );
        let facet = facet(
            FacetKind::Symbol,
            "render.AsciiJSON",
            technical_variants("render.AsciiJSON"),
        );

        assert_eq!(
            facet.variants.first().map(String::as_str),
            Some("render.AsciiJSON")
        );
        assert!(facet.variants.len() <= MAX_FACET_VARIANTS);
        assert!(plan.queries.iter().any(|query| {
            query.value == "render.AsciiJSON"
                && query.exact_variant
                && query.has_facet(FacetKind::ExactAtom)
        }));
    }

    #[test]
    fn kebab_case_error_atoms_retain_a_bounded_phrase_variant() {
        let plan = plan(
            "Report the failed-to-lookup-view error through the callback",
            8,
        );
        let facet = facet(
            FacetKind::Symbol,
            "failed-to-lookup-view",
            technical_variants("failed-to-lookup-view"),
        );

        assert!(
            facet
                .variants
                .iter()
                .any(|value| value == "failed to lookup view")
        );
        assert!(facet.variants.len() <= MAX_FACET_VARIANTS);
        assert!(plan.queries.len() <= 8);
    }

    #[test]
    fn workflow_evidence_reserves_deterministic_typed_query_lanes() {
        let evidence = WorkflowEvidence::new()
            .with_failure_traces(["error: default_values_if is missing".to_owned()])
            .with_symbols(["default_values_if".to_owned()])
            .with_paths(["tests/builder/default_vals.rs".to_owned()])
            .with_test_intents(["default values regression".to_owned()]);

        let first = plan_with_workflow_evidence("cargo test failed", &evidence, 12);
        let second = plan_with_workflow_evidence("cargo test failed", &evidence, 12);

        assert_eq!(first.queries, second.queries);
        assert!(first.queries.len() <= 12);
        for kind in [
            FacetKind::FailureTrace,
            FacetKind::Symbol,
            FacetKind::Path,
            FacetKind::TestIntent,
        ] {
            assert!(
                first.queries.iter().any(|query| query.has_facet(kind)),
                "missing {kind:?}: {:?}",
                first.queries
            );
        }
        assert_eq!(
            first.queries.first().map(|query| query.value.as_str()),
            Some("default_values_if")
        );
    }

    #[test]
    fn empty_workflow_evidence_preserves_the_existing_plan() {
        let task = "fix Parser::parse for src/parser.rs";

        assert_eq!(
            plan(task, 12).queries,
            plan_with_workflow_evidence(task, &WorkflowEvidence::default(), 12).queries
        );
    }

    #[test]
    fn task_roles_separate_primary_preserve_and_test_queries() {
        let facet_plan = plan(
            "Fix direct context initialization instead of index_not_ready. Preserve MCP startup \
             and snapshot consistency. Add a regression test.",
            16,
        );

        assert!(facet_plan.queries.iter().any(|query| {
            query.has_facet(FacetKind::PrimaryChange)
                && (query.value == "context" || query.value == "initialization")
        }));
        assert!(facet_plan.queries.iter().any(|query| {
            query.has_facet(FacetKind::FailureTrace) && query.value.contains("index_not_ready")
        }));
        assert!(
            facet_plan
                .queries
                .iter()
                .any(|query| query.has_facet(FacetKind::PreserveConstraint))
        );
        assert!(
            facet_plan
                .queries
                .iter()
                .any(|query| query.has_facet(FacetKind::TestIntent))
        );
        assert_eq!(
            primary_task_text("Which test suite verifies the MCP tool schemas?"),
            "Which test suite verifies the MCP tool schemas?"
        );
        assert!(
            plan("Test direct context initialization", 8)
                .queries
                .iter()
                .any(|query| query.has_facet(FacetKind::TestIntent))
        );

        let mixed = plan(
            "Add SearchCompactResponse compact output while preserving full ranking and regression tests",
            16,
        );
        let exact = mixed
            .queries
            .iter()
            .find(|query| query.value == "SearchCompactResponse")
            .expect("exact implementation query");
        assert!(exact.has_facet(FacetKind::PrimaryChange));
        assert!(!exact.has_facet(FacetKind::TestIntent));
        assert!(mixed.queries.iter().any(|query| {
            query.has_facet(FacetKind::PreserveConstraint)
                && !query.has_facet(FacetKind::TestIntent)
        }));
        let primary_phrase = ["search", "projection"].join(" ");
        let natural_task = format!(
            "Add a source-free compact {primary_phrase} to CLI and MCP while preserving full ranking"
        );
        let natural = plan(&natural_task, 16);
        assert!(natural.queries.iter().any(|query| {
            query.value == primary_phrase && query.has_facet(FacetKind::PrimaryChange)
        }));
        assert_eq!(
            primary_task_text(
                "Add SearchCompactResponse compact output while preserving full ranking"
            ),
            "Add SearchCompactResponse compact output"
        );
    }
}
