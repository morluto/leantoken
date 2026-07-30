use super::*;

pub(crate) enum ScopedRegexPathAtom {
    /// Matches `path` or any descendant (`src` → `src`, `src/...`).
    Prefix(String),
    /// Matches descendants only (`src/**` → `src/...`).
    Children(String),
}

pub(crate) struct ScopedRegexPathSql {
    pub(crate) clause: String,
    pub(crate) params: Vec<String>,
}

pub(crate) fn scoped_regex_path_has_glob_meta(pattern: &str) -> bool {
    pattern.contains(['*', '?', '[', ']', '{', '}'])
}

pub(crate) fn expressible_scoped_regex_path(pattern: &str) -> Option<ScopedRegexPathAtom> {
    let pattern = pattern.replace('\\', "/");
    let pattern = pattern.trim_matches('/');
    if pattern.is_empty() {
        return None;
    }
    if !scoped_regex_path_has_glob_meta(pattern) {
        return Some(ScopedRegexPathAtom::Prefix(pattern.to_owned()));
    }
    let prefix = pattern.strip_suffix("/**")?;
    let prefix = prefix.trim_matches('/');
    if prefix.is_empty() || scoped_regex_path_has_glob_meta(prefix) {
        return None;
    }
    Some(ScopedRegexPathAtom::Children(prefix.to_owned()))
}

pub(crate) fn scoped_regex_path_atom_sql(
    column: &str,
    param_index: usize,
    atom: &ScopedRegexPathAtom,
) -> String {
    match atom {
        ScopedRegexPathAtom::Prefix(_) => format!(
            "({column} = ?{param_index} OR substr({column}, 1, length(?{param_index}) + 1) = ?{param_index} || '/')"
        ),
        ScopedRegexPathAtom::Children(_) => {
            format!("substr({column}, 1, length(?{param_index}) + 1) = ?{param_index} || '/'")
        }
    }
}

pub(crate) fn scoped_regex_path_atom_param(atom: ScopedRegexPathAtom) -> String {
    match atom {
        ScopedRegexPathAtom::Prefix(value) | ScopedRegexPathAtom::Children(value) => value,
    }
}

/// Push simple include/exclude path predicates into scoped-regex candidate SQL.
///
/// Include predicates are emitted only when every include pattern is expressible,
/// so SQL never under-selects. Expressible excludes are always pushed; patterns
/// SQL cannot express remain filtered by the Rust `PathFilter` callback.
pub(crate) fn scoped_regex_path_sql(
    include_paths: &[String],
    exclude_paths: &[String],
) -> ScopedRegexPathSql {
    let mut clause = String::new();
    let mut params = Vec::new();
    let mut next_index = 3usize;

    if !include_paths.is_empty() {
        let includes = include_paths
            .iter()
            .map(|pattern| expressible_scoped_regex_path(pattern))
            .collect::<Option<Vec<_>>>();
        if let Some(includes) = includes {
            let mut parts = Vec::with_capacity(includes.len());
            for atom in includes {
                parts.push(scoped_regex_path_atom_sql("f.path", next_index, &atom));
                params.push(scoped_regex_path_atom_param(atom));
                next_index = next_index.saturating_add(1);
            }
            if !parts.is_empty() {
                clause.push_str(" AND (");
                clause.push_str(&parts.join(" OR "));
                clause.push(')');
            }
        }
    }

    for pattern in exclude_paths {
        let Some(atom) = expressible_scoped_regex_path(pattern) else {
            continue;
        };
        clause.push_str(" AND NOT ");
        clause.push_str(&scoped_regex_path_atom_sql("f.path", next_index, &atom));
        params.push(scoped_regex_path_atom_param(atom));
        next_index = next_index.saturating_add(1);
    }

    ScopedRegexPathSql { clause, params }
}

pub(crate) fn bounded_limit(limit: usize) -> i64 {
    let capped = limit.clamp(1, HARD_MAX_RESULTS);
    i64::try_from(capped).unwrap_or(i64::MAX)
}

pub(crate) fn quoted_fts_phrase(query: &str) -> String {
    format!("\"{}\"", query.replace('"', "\"\""))
}

#[cfg(test)]
mod scoped_regex_path_sql_tests {
    use super::{ScopedRegexPathAtom, expressible_scoped_regex_path, scoped_regex_path_sql};

    #[test]
    fn expressible_patterns_cover_literals_and_recursive_globs() {
        assert!(matches!(
            expressible_scoped_regex_path("src"),
            Some(ScopedRegexPathAtom::Prefix(value)) if value == "src"
        ));
        assert!(matches!(
            expressible_scoped_regex_path("included/**"),
            Some(ScopedRegexPathAtom::Children(value)) if value == "included"
        ));
        assert!(expressible_scoped_regex_path("**/*.rs").is_none());
        assert!(expressible_scoped_regex_path("src/{a,b}").is_none());
    }

    #[test]
    fn include_sql_requires_every_pattern_to_be_expressible() {
        let mixed = scoped_regex_path_sql(&["src".into(), "**/*.rs".into()], &[]);
        assert!(mixed.clause.is_empty());
        assert!(mixed.params.is_empty());

        let ready = scoped_regex_path_sql(&["src".into(), "included/**".into()], &[]);
        assert!(ready.clause.contains("f.path = ?3"));
        assert!(
            ready
                .clause
                .contains("substr(f.path, 1, length(?4) + 1) = ?4 || '/'")
        );
        assert_eq!(
            ready.params,
            vec!["src".to_string(), "included".to_string()]
        );
    }

    #[test]
    fn exclude_sql_pushes_only_expressible_patterns() {
        let sql = scoped_regex_path_sql(&[], &["tests".into(), "**/*.snap".into()]);
        assert!(sql.clause.starts_with(" AND NOT "));
        assert!(sql.clause.contains("f.path = ?3"));
        assert!(!sql.clause.contains("?4"));
        assert_eq!(sql.params, vec!["tests".to_string()]);
    }
}
