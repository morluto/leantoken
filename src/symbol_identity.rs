//! Shared canonical symbol identity matching and uniqueness resolution.

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

/// Case-insensitive variant used only for ranked search scoring.
pub(crate) fn symbol_identity_matches_ignore_ascii_case(
    requested: &str,
    name: &str,
    parent: Option<&str>,
) -> bool {
    requested.eq_ignore_ascii_case(name)
        || split_qualified_symbol(requested).is_some_and(|(requested_parent, requested_name)| {
            parent.is_some_and(|parent| requested_parent.eq_ignore_ascii_case(parent))
                && requested_name.eq_ignore_ascii_case(name)
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
}
