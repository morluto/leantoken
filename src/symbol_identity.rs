//! Shared canonical symbol identity matching and uniqueness resolution.

use std::collections::{BTreeSet, HashMap};

/// Maximum number of index literals generated for one Unicode simple-fold query.
///
/// Most queries have one representation. The bound prevents repeated characters
/// such as `s`/long-s or `k`/Kelvin-sign from creating an exponential FTS query.
pub(crate) const MAX_CASE_FOLD_LITERAL_VARIANTS: usize = 32;

/// Complete, bounded index literals for one Unicode simple-case-folded value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CaseFoldLiteralVariants {
    pub(crate) values: Vec<String>,
    pub(crate) expanded: bool,
}

/// Expand a literal into the scalar alternatives accepted by Rust's regex engine.
///
/// SQLite's bundled FTS and `lower()` behavior cannot be used as the correctness
/// oracle for Unicode case-insensitive matching. Returning `None` means the
/// complete Cartesian product exceeded the hard bound and the caller must use a
/// bounded full-scan verifier instead of an incomplete index query.
pub(crate) fn case_fold_literal_variants(value: &str) -> Option<CaseFoldLiteralVariants> {
    let unique = value.chars().collect::<BTreeSet<_>>();
    let mut alternatives = HashMap::with_capacity(unique.len());
    for character in unique {
        let mut class =
            regex_syntax::hir::ClassUnicode::new([regex_syntax::hir::ClassUnicodeRange::new(
                character, character,
            )]);
        class.try_case_fold_simple().ok()?;
        let mut choices = BTreeSet::new();
        for range in class.iter() {
            for codepoint in u32::from(range.start())..=u32::from(range.end()) {
                choices.insert(ascii_fold(char::from_u32(codepoint)?));
            }
        }
        alternatives.insert(character, choices.into_iter().collect::<Vec<_>>());
    }

    let mut values = vec![String::new()];
    for character in value.chars() {
        let choices = alternatives
            .get(&character)
            .expect("query character alternatives were prepared");
        if values.len().checked_mul(choices.len())? > MAX_CASE_FOLD_LITERAL_VARIANTS {
            return None;
        }
        let mut expanded = Vec::with_capacity(values.len() * choices.len());
        for prefix in &values {
            for choice in choices {
                let mut variant = String::with_capacity(prefix.len() + choice.len_utf8());
                variant.push_str(prefix);
                variant.push(*choice);
                expanded.push(variant);
            }
        }
        values = expanded;
    }
    values.sort();
    values.dedup();
    let ascii_canonical = value.chars().map(ascii_fold).collect::<String>();
    let expanded = values.len() != 1 || values.first() != Some(&ascii_canonical);
    Some(CaseFoldLiteralVariants { values, expanded })
}

/// Build one FTS expression that admits every complete case-folded literal.
pub(crate) fn case_fold_fts_query(variants: &CaseFoldLiteralVariants) -> String {
    variants
        .values
        .iter()
        .map(|value| format!("\"{}\"", value.replace('"', "\"\"")))
        .collect::<Vec<_>>()
        .join(" OR ")
}

fn ascii_fold(character: char) -> char {
    if character.is_ascii() {
        character.to_ascii_lowercase()
    } else {
        character
    }
}

/// Whether a literal regex match covers the complete candidate identity.
pub(crate) fn literal_match_is_entire(matcher: &regex::Regex, candidate: &str) -> bool {
    matcher
        .find(candidate)
        .is_some_and(|matched| matched.start() == 0 && matched.end() == candidate.len())
}

/// Match a bare or qualified symbol identity with a compiled literal matcher.
pub(crate) fn symbol_identity_matches_case_fold(
    matcher: &regex::Regex,
    name: &str,
    parent: Option<&str>,
) -> bool {
    literal_match_is_entire(matcher, name)
        || parent.is_some_and(|parent| {
            let mut qualified = String::with_capacity(parent.len() + name.len() + 1);
            qualified.push_str(parent);
            qualified.push('.');
            qualified.push_str(name);
            literal_match_is_entire(matcher, &qualified)
        })
}

/// Search a bare name or, for a qualified request, its canonical identity.
pub(crate) fn symbol_identity_contains_case_fold(
    requested: &str,
    matcher: &regex::Regex,
    name: &str,
    parent: Option<&str>,
) -> bool {
    matcher.is_match(name)
        || split_qualified_symbol(requested).is_some()
            && parent.is_some_and(|parent| {
                let mut qualified = String::with_capacity(parent.len() + name.len() + 1);
                qualified.push_str(parent);
                qualified.push('.');
                qualified.push_str(name);
                matcher.is_match(&qualified)
            })
}

/// Result of resolving one symbol identity within an already bounded scope.
pub(crate) enum SymbolResolution<T> {
    NotFound,
    Unique(T),
    Ambiguous,
}

/// Return the qualified owner/name parts when `requested` uses `parent.name`.
pub(crate) fn split_qualified_symbol(requested: &str) -> Option<(&str, &str)> {
    let (parent, name) = requested.rsplit_once('.')?;
    (!parent.is_empty() && !name.is_empty()).then_some((parent, name))
}

/// Match either a bare parsed name or its canonical `parent.name` identity.
pub(crate) fn symbol_identity_matches(requested: &str, name: &str, parent: Option<&str>) -> bool {
    requested == name
        || parent.is_some_and(|parent| {
            requested
                .strip_prefix(parent)
                .and_then(|suffix| suffix.strip_prefix('.'))
                == Some(name)
        })
}

/// Resolve zero, one, or multiple matches without silently choosing the first.
pub(crate) fn resolve_symbol_matches<T>(
    mut matches: impl Iterator<Item = T>,
) -> SymbolResolution<T> {
    let Some(first) = matches.next() else {
        return SymbolResolution::NotFound;
    };
    if matches.next().is_some() {
        SymbolResolution::Ambiguous
    } else {
        SymbolResolution::Unique(first)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_symbol_identity_requires_an_exact_owner_boundary() {
        assert!(symbol_identity_matches("run", "run", Some("Service")));
        assert!(symbol_identity_matches(
            "Service.run",
            "run",
            Some("Service")
        ));
        assert!(!symbol_identity_matches(
            "Other.run",
            "run",
            Some("Service")
        ));
        assert!(!symbol_identity_matches(
            "MyService.run",
            "run",
            Some("Service")
        ));
        assert_eq!(
            split_qualified_symbol("module.Service.run"),
            Some(("module.Service", "run"))
        );
        assert_eq!(split_qualified_symbol("run"), None);
    }

    #[test]
    fn case_fold_literals_expand_only_the_unicode_equivalence_classes() {
        let ordinary = case_fold_literal_variants("Alpha").expect("ordinary variants");
        assert_eq!(ordinary.values, ["alpha"]);
        assert!(!ordinary.expanded);

        let kelvin = case_fold_literal_variants("k").expect("Kelvin variants");
        assert_eq!(kelvin.values, ["k", "K"]);
        assert!(kelvin.expanded);

        let georgian = case_fold_literal_variants("აbc").expect("Georgian variants");
        assert_eq!(georgian.values, ["აbc", "Აbc"]);
        assert!(georgian.expanded);

        let uncased = case_fold_literal_variants("🦀").expect("uncased variants");
        assert_eq!(uncased.values, ["🦀"]);
        assert!(!uncased.expanded);

        assert!(case_fold_literal_variants(&"s".repeat(6)).is_none());
    }
}
