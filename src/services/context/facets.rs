use std::collections::{BTreeSet, HashMap, HashSet};

use crate::text::{expand_identifier, expand_terms, identifier_words};

const MAX_ATOMS: usize = 16;
const MAX_FACET_VARIANTS: usize = 4;
const MAX_QUOTED_PHRASES: usize = 4;
const MAX_BEHAVIOR_TERMS: usize = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum FacetKind {
    ExactAtom,
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
}

#[derive(Debug, Clone)]
pub(super) struct FacetPlan {
    #[cfg(test)]
    pub(super) facets: Vec<TaskFacet>,
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
            #[cfg(test)]
            facets: Vec::new(),
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

    for phrase in quoted_phrases(task).into_iter().take(MAX_QUOTED_PHRASES) {
        push_facet(
            &mut facets,
            FacetKind::Behavior,
            &phrase,
            phrase_variants(&phrase),
            0.85,
        );
    }

    for term in terms
        .iter()
        .filter(|term| {
            !is_test_term(term)
                && !is_stop_word(term)
                && !atom_parts.contains(&term.to_ascii_lowercase())
        })
        .take(MAX_BEHAVIOR_TERMS)
    {
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

    let queries = build_queries(task, &facets, limit, wants_tests);
    FacetPlan {
        #[cfg(test)]
        facets,
        queries,
    }
}

fn build_queries(
    task: &str,
    facets: &[TaskFacet],
    limit: usize,
    wants_tests: bool,
) -> Vec<ContextQuery> {
    let available = limit.saturating_sub(usize::from(wants_tests));
    let mut queries = Vec::new();
    let mut positions = HashMap::<String, usize>::new();
    let code_terms = legacy_code_tokens(task);
    let code_parts = code_terms
        .iter()
        .flat_map(|term| std::iter::once(term.clone()).chain(expand_terms(term)))
        .map(|term| term.to_ascii_lowercase())
        .collect::<HashSet<_>>();
    let prose = task_terms(task)
        .into_iter()
        .filter(|value| {
            !is_test_term(value)
                && !is_stop_word(value)
                && !code_parts.contains(&value.to_ascii_lowercase())
        })
        .collect::<Vec<_>>();
    let prose_reserve = prose.len().min(MAX_BEHAVIOR_TERMS).min(available);
    let exact_limit = available.saturating_sub(prose_reserve);

    for value in code_terms.iter().take(exact_limit) {
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
    for value in prose.iter().take(prose_reserve) {
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

pub(super) fn legacy_code_tokens(task: &str) -> Vec<String> {
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
            | "change"
            | "does"
            | "each"
            | "find"
            | "fix"
            | "for"
            | "from"
            | "if"
            | "in"
            | "into"
            | "is"
            | "it"
            | "its"
            | "locate"
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

#[cfg(test)]
mod tests {
    use super::*;

    fn exact_atoms(task: &str) -> Vec<String> {
        plan(task, 64)
            .facets
            .into_iter()
            .filter(|facet| facet.kind == FacetKind::ExactAtom)
            .map(|facet| facet.original)
            .collect()
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
        let plan = plan("Fix CONFIG and #[serde(untagged)].", 12);
        for facet in plan
            .facets
            .iter()
            .filter(|facet| facet.kind == FacetKind::ExactAtom)
        {
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
    fn quoted_error_text_and_annotations_create_bounded_facets() {
        let plan = plan(
            "Handle @retry and report \"Failed to lookup view\" without changing behavior.",
            12,
        );
        assert!(
            plan.facets.iter().any(|facet| {
                facet.kind == FacetKind::Configuration && facet.original == "@retry"
            })
        );
        assert!(plan.facets.iter().any(|facet| {
            facet.kind == FacetKind::Behavior && facet.original == "Failed to lookup view"
        }));
        assert!(plan.queries.len() <= 12);
        assert!(
            plan.facets
                .iter()
                .all(|facet| facet.variants.len() <= MAX_FACET_VARIANTS)
        );
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
        assert!(first.queries.len() > 3);
        assert_eq!(
            first.queries.last().map(|query| query.value.as_str()),
            Some("test")
        );
    }

    #[test]
    fn qualified_atoms_keep_exact_owner_and_bounded_symbol_expansions() {
        let plan = plan(
            "Fix render.AsciiJSON for non-BMP JSON with UTF-16 while preserving BMP and ASCII behavior",
            10,
        );
        let facet = plan
            .facets
            .iter()
            .find(|facet| facet.kind == FacetKind::Symbol && facet.original == "render.AsciiJSON")
            .expect("qualified symbol facet");

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
        let facet = plan
            .facets
            .iter()
            .find(|facet| {
                facet.kind == FacetKind::Symbol && facet.original == "failed-to-lookup-view"
            })
            .expect("kebab-case facet");

        assert!(
            facet
                .variants
                .iter()
                .any(|value| value == "failed to lookup view")
        );
        assert!(facet.variants.len() <= MAX_FACET_VARIANTS);
        assert!(plan.queries.len() <= 8);
    }
}
