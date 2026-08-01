use super::*;

pub(crate) const MAX_INDEX_SCOPE_PATTERNS: usize = 64;
pub(crate) const MAX_INDEX_SCOPE_PATTERN_BYTES: usize = 1024;
pub(crate) const MAX_INDEX_SCOPE_TOTAL_BYTES: usize = 16 * 1024;
pub(crate) const INDEX_SCOPE_DIGEST_HEX_CHARS: usize = 16;

#[derive(Debug)]
pub(crate) struct ScopeMatcher {
    pub(crate) matcher: crate::repository::RepositoryPatternSet,
    pub(crate) static_prefixes: Vec<String>,
    pub(crate) excluded_subtree_roots: Vec<String>,
}

impl ScopeMatcher {
    pub(crate) fn compile(patterns: &[String], exclusions: bool) -> Result<Self> {
        let mut static_prefixes = Vec::with_capacity(patterns.len());
        let mut excluded_subtree_roots = Vec::new();
        for pattern in patterns {
            let has_meta = pattern.contains(['*', '?', '[', ']', '{', '}']);
            let prefix = pattern
                .split('/')
                .take_while(|component| !component.contains(['*', '?', '[', ']', '{', '}']))
                .collect::<Vec<_>>()
                .join("/");
            static_prefixes.push(prefix);
            if exclusions {
                if !has_meta {
                    excluded_subtree_roots.push(pattern.clone());
                } else if let Some(root) = pattern.strip_suffix("/**")
                    && !root.contains(['*', '?', '[', ']', '{', '}'])
                {
                    excluded_subtree_roots.push(root.to_owned());
                }
            }
        }
        excluded_subtree_roots.sort_unstable();
        excluded_subtree_roots.dedup();
        Ok(Self {
            matcher: crate::repository::RepositoryPatternSet::new(patterns)?,
            static_prefixes,
            excluded_subtree_roots,
        })
    }

    pub(crate) fn matches(&self, path: &str) -> bool {
        self.matcher.is_match(path)
    }

    pub(crate) fn excludes_directory(&self, path: &str) -> bool {
        self.excluded_subtree_roots
            .iter()
            .any(|root| path_is_within(path, root))
            || self.matches(path)
    }

    pub(crate) fn may_match_descendant(&self, directory: &str) -> bool {
        self.static_prefixes.iter().any(|prefix| {
            prefix.is_empty()
                || path_is_within(directory, prefix)
                || path_is_within(prefix, directory)
        })
    }
}

pub(crate) fn path_is_within(path: &str, root: &str) -> bool {
    path.strip_prefix(root)
        .is_some_and(|suffix| suffix.is_empty() || suffix.starts_with('/'))
}

pub(crate) fn normalize_scope_pattern(pattern: String) -> Result<String> {
    if pattern.len() > MAX_INDEX_SCOPE_PATTERN_BYTES {
        return Err(Error::InvalidConfiguration(format!(
            "index scope pattern exceeds {MAX_INDEX_SCOPE_PATTERN_BYTES} bytes"
        )));
    }
    crate::repository::RepositoryPattern::parse(pattern)
        .map(|pattern| pattern.as_str().to_owned())
        .map_err(|error| Error::InvalidConfiguration(error.to_string()))
}

/// Immutable, normalized repository-relative indexing boundary.
///
/// Includes are optional and default to the whole repository. Excludes always
/// win. Literal paths include or exclude their complete subtree; glob patterns
/// use the same slash-normalized matching semantics as retrieval path filters.
#[derive(Debug, Clone)]
pub struct IndexScope {
    includes: Vec<String>,
    excludes: Vec<String>,
    include_matcher: std::sync::Arc<ScopeMatcher>,
    exclude_matcher: std::sync::Arc<ScopeMatcher>,
    digest: Option<String>,
}

impl IndexScope {
    /// Normalize and compile one bounded indexing scope.
    ///
    /// # Errors
    ///
    /// Returns a typed configuration or glob error for empty, absolute,
    /// parent-traversing, oversized, excessive, or malformed patterns.
    pub fn new(
        includes: impl IntoIterator<Item = String>,
        excludes: impl IntoIterator<Item = String>,
    ) -> Result<Self> {
        let mut includes = includes
            .into_iter()
            .map(normalize_scope_pattern)
            .collect::<Result<Vec<_>>>()?;
        let mut excludes = excludes
            .into_iter()
            .map(normalize_scope_pattern)
            .collect::<Result<Vec<_>>>()?;
        if includes.len().saturating_add(excludes.len()) > MAX_INDEX_SCOPE_PATTERNS {
            return Err(Error::InvalidConfiguration(format!(
                "index scope accepts at most {MAX_INDEX_SCOPE_PATTERNS} patterns"
            )));
        }
        let total_bytes = includes
            .iter()
            .chain(&excludes)
            .map(String::len)
            .sum::<usize>();
        if total_bytes > MAX_INDEX_SCOPE_TOTAL_BYTES {
            return Err(Error::InvalidConfiguration(format!(
                "index scope patterns exceed {MAX_INDEX_SCOPE_TOTAL_BYTES} total bytes"
            )));
        }
        includes.sort_unstable();
        includes.dedup();
        excludes.sort_unstable();
        excludes.dedup();
        let include_matcher = std::sync::Arc::new(ScopeMatcher::compile(&includes, false)?);
        let exclude_matcher = std::sync::Arc::new(ScopeMatcher::compile(&excludes, true)?);
        let digest = if includes.is_empty() && excludes.is_empty() {
            None
        } else {
            let mut hasher = blake3::Hasher::new();
            hasher.update(b"leantoken-index-scope-v2\0");
            for pattern in &includes {
                hasher.update(b"include\0");
                hasher.update(pattern.as_bytes());
                hasher.update(b"\0");
            }
            for pattern in &excludes {
                hasher.update(b"exclude\0");
                hasher.update(pattern.as_bytes());
                hasher.update(b"\0");
            }
            Some(hasher.finalize().to_hex().to_string())
        };
        Ok(Self {
            includes,
            excludes,
            include_matcher,
            exclude_matcher,
            digest,
        })
    }

    /// Return whether this scope indexes the complete ignore-visible repository.
    #[must_use]
    pub fn is_full(&self) -> bool {
        self.digest.is_none()
    }

    /// Return canonical include patterns in deterministic order.
    #[must_use]
    pub fn includes(&self) -> &[String] {
        &self.includes
    }

    /// Return canonical exclude patterns in deterministic order.
    #[must_use]
    pub fn excludes(&self) -> &[String] {
        &self.excludes
    }

    /// Return the compact opaque identity disclosed in public provenance.
    #[must_use]
    pub fn digest(&self) -> Option<&str> {
        self.digest
            .as_deref()
            .map(|digest| &digest[..INDEX_SCOPE_DIGEST_HEX_CHARS])
    }

    pub(crate) fn full_digest(&self) -> Option<&str> {
        self.digest.as_deref()
    }

    pub(crate) fn includes_path(&self, path: &str, is_directory: bool) -> bool {
        if path.is_empty() {
            return true;
        }
        if is_directory && self.exclude_matcher.excludes_directory(path)
            || !is_directory && self.exclude_matcher.matches(path)
        {
            return false;
        }
        if self.includes.is_empty() {
            return true;
        }
        if is_directory {
            self.include_matcher.matches(path) || self.include_matcher.may_match_descendant(path)
        } else {
            self.include_matcher.matches(path)
        }
    }

    pub(crate) fn may_include_descendant(&self, directory: &str) -> bool {
        if !directory.is_empty() && self.exclude_matcher.excludes_directory(directory) {
            return false;
        }
        self.includes.is_empty()
            || directory.is_empty()
            || self.include_matcher.matches(directory)
            || self.include_matcher.may_match_descendant(directory)
    }

    pub(crate) fn identity_material(&self) -> String {
        self.full_digest().unwrap_or("full").to_owned()
    }
}

impl Default for IndexScope {
    fn default() -> Self {
        Self::new(Vec::new(), Vec::new()).expect("empty index scope is valid")
    }
}

impl PartialEq for IndexScope {
    fn eq(&self, other: &Self) -> bool {
        self.includes == other.includes && self.excludes == other.excludes
    }
}

impl Eq for IndexScope {}

#[cfg(test)]
mod index_scope_tests {
    use super::*;

    #[test]
    fn normalized_scope_is_deterministic_and_prunes_literal_subtrees() {
        let scope = IndexScope::new(
            vec!["./src/**".into(), "tests\\**\\*.rs".into()],
            vec!["src/generated/**".into()],
        )
        .expect("scope");
        let equivalent = IndexScope::new(
            vec!["tests/**/*.rs".into(), "src//**".into()],
            vec!["./src/generated/**".into()],
        )
        .expect("equivalent scope");

        assert_eq!(scope, equivalent);
        assert_eq!(scope.digest(), equivalent.digest());
        assert!(scope.includes_path("src", true));
        assert!(!scope.includes_path("src/generated", true));
        assert!(scope.includes_path("src/lib.rs", false));
        assert!(scope.includes_path("tests/unit/parser.rs", false));
        assert!(!scope.includes_path("tests/unit/parser.md", false));
        assert!(!scope.includes_path("third_party", true));
    }

    #[test]
    fn scope_rejects_unbounded_or_non_relative_patterns() {
        assert!(IndexScope::new(vec!["../src/**".into()], Vec::new()).is_err());
        assert!(IndexScope::new(vec!["/src/**".into()], Vec::new()).is_err());
        assert!(IndexScope::new(vec![String::new()], Vec::new()).is_err());
        assert!(
            IndexScope::new(
                vec!["x".repeat(MAX_INDEX_SCOPE_PATTERN_BYTES + 1)],
                Vec::new(),
            )
            .is_err()
        );
        assert!(
            IndexScope::new(
                (0..=MAX_INDEX_SCOPE_PATTERNS).map(|index| format!("src/{index}")),
                Vec::new(),
            )
            .is_err()
        );
    }

    #[test]
    fn exclude_only_scope_admits_every_other_path_and_matching_is_case_sensitive() {
        let scope =
            IndexScope::new(Vec::new(), vec!["third_party".into()]).expect("exclude-only scope");

        assert!(scope.includes_path("src/lib.rs", false));
        assert!(!scope.includes_path("third_party", true));
        assert!(!scope.includes_path("third_party/lib.rs", false));

        let cased = IndexScope::new(vec!["Src/**".into()], Vec::new()).expect("cased scope");
        assert!(!cased.includes_path("src/lib.rs", false));
    }
}
