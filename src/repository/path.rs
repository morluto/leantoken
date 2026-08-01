use super::*;

pub fn resolve_existing(root: &Path, requested: &str) -> Result<PathBuf> {
    let relative = validate_relative(requested)?;
    let canonical = root.join(relative).canonicalize()?;
    if !canonical.starts_with(root) {
        return Err(Error::PathOutsideRoot(canonical));
    }
    Ok(canonical)
}

pub fn validate_relative(requested: &str) -> Result<PathBuf> {
    Ok(PathBuf::from(normalize_relative(requested)?))
}

/// Validate and normalize a repository-relative request path.
///
/// Repository keys always use forward slashes, independent of the host
/// platform. This helper therefore recognizes both separator styles before
/// applying the relative-path contract.
pub fn normalize_relative(requested: &str) -> Result<String> {
    RepositoryPath::parse(requested).map(RepositoryPath::into_string)
}

pub fn slash_path(path: &Path) -> String {
    path.components()
        .filter_map(|component| match component {
            Component::Normal(value) => Some(value.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("/")
}

pub(crate) fn checked_slash_path(path: &Path) -> Result<String> {
    path.components()
        .filter_map(|component| match component {
            Component::Normal(value) => Some(
                value
                    .to_str()
                    .map(str::to_owned)
                    .ok_or_else(|| Error::UnsupportedPathEncoding(path.to_path_buf())),
            ),
            _ => None,
        })
        .collect::<Result<Vec<_>>>()
        .map(|components| components.join("/"))
}

#[cfg(test)]
mod typed_path_tests {
    use super::*;

    #[test]
    fn canonicalization_is_cross_platform_and_idempotent() {
        for (input, expected) in [
            (r"src\\services//./read.rs", "src/services/read.rs"),
            ("./tests///unit.rs", "tests/unit.rs"),
            (".", "."),
        ] {
            let canonical = RepositoryPath::parse(input).expect("valid path");
            assert_eq!(canonical.as_str(), expected);
            assert_eq!(
                RepositoryPath::parse(canonical.as_str())
                    .expect("canonical path")
                    .as_str(),
                expected
            );
        }
    }

    #[test]
    fn paths_and_patterns_fail_closed_on_escape_or_empty_values() {
        for value in ["", "   ", "/src", r"C:\\src", "../src", r"..\\src"] {
            assert!(
                RepositoryPath::parse(value).is_err(),
                "accepted path {value:?}"
            );
        }
        for value in ["", "/", ".", "./", "../**", "["] {
            assert!(
                RepositoryPattern::parse(value).is_err(),
                "accepted pattern {value:?}"
            );
        }
    }

    #[test]
    fn compiled_patterns_preserve_literal_subtrees_and_case_sensitive_globs() {
        let matcher = RepositoryPatternSet::new(&[
            "src".into(),
            r"tests\\**\\*.rs".into(),
            "README.*".into(),
        ])
        .expect("patterns");
        assert!(matcher.is_match("src/lib.rs"));
        assert!(matcher.is_match("tests/unit/path.rs"));
        assert!(matcher.is_match("README.md"));
        assert!(!matcher.is_match("readme.md"));
        assert!(!matcher.is_match("source/lib.rs"));
    }

    #[test]
    fn import_relative_join_cannot_escape_repository() {
        let source = RepositoryPath::parse("src/nested/module.rs").expect("source");
        assert_eq!(
            source
                .join_relative("../shared.rs")
                .expect("bounded parent")
                .as_str(),
            "src/shared.rs"
        );
        assert!(
            RepositoryPath::parse("module.rs")
                .expect("root source")
                .join_relative("../outside.rs")
                .is_err()
        );
    }

    #[test]
    fn transparent_deserialization_still_enforces_boundary_invariants() {
        let path =
            serde_json::from_str::<RepositoryPath>(r#""src\\lib.rs""#).expect("canonical path");
        assert_eq!(path.as_str(), "src/lib.rs");
        assert!(serde_json::from_str::<RepositoryPath>(r#""../outside.rs""#).is_err());

        let pattern = serde_json::from_str::<RepositoryPattern>(r#""src\\**\\*.rs""#)
            .expect("canonical pattern");
        assert_eq!(pattern.as_str(), "src/**/*.rs");
        assert!(serde_json::from_str::<RepositoryPattern>(r#""[""#).is_err());
    }
}
use globset::{Candidate, Glob, GlobSet, GlobSetBuilder};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub(crate) const MAX_REPOSITORY_PATTERNS: usize = 256;
pub(crate) const MAX_REPOSITORY_PATH_BYTES: usize = 4 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, JsonSchema)]
#[serde(transparent)]
pub struct RepositoryPath(String);

impl RepositoryPath {
    pub fn parse(value: impl AsRef<str>) -> Result<Self> {
        normalize_repository_value(value.as_ref(), "path", true, MAX_REPOSITORY_PATH_BYTES)
            .map(Self)
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    #[must_use]
    pub fn into_string(self) -> String {
        self.0
    }

    /// Join an import-relative path without interpreting glob syntax.
    pub fn join_relative(&self, relative: &str) -> Result<Self> {
        if relative.starts_with('/')
            || relative.starts_with('\\')
            || relative.as_bytes().get(1).is_some_and(|byte| *byte == b':')
        {
            return Err(Error::PathOutsideRoot(PathBuf::from(relative)));
        }
        if relative.contains('\0') {
            return Err(Error::InvalidInput {
                field: "import path",
                reason: "must not contain NUL bytes",
            });
        }
        let mut components = self.0.rsplit_once('/').map_or(Vec::new(), |(parent, _)| {
            parent.split('/').map(str::to_owned).collect()
        });
        let normalized = relative.replace('\\', "/");
        for component in normalized.split('/') {
            match component {
                "" | "." => {}
                ".." => {
                    if components.pop().is_none() {
                        return Err(Error::PathOutsideRoot(PathBuf::from(relative)));
                    }
                }
                component => components.push(component.to_owned()),
            }
        }
        if components.is_empty() {
            return Err(Error::InvalidInput {
                field: "import path",
                reason: "must identify a repository-relative path",
            });
        }
        Self::parse(components.join("/"))
    }
}

impl AsRef<str> for RepositoryPath {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl std::fmt::Display for RepositoryPath {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for RepositoryPath {
    fn deserialize<Deserializer>(
        deserializer: Deserializer,
    ) -> std::result::Result<Self, Deserializer::Error>
    where
        Deserializer: serde::Deserializer<'de>,
    {
        Self::parse(String::deserialize(deserializer)?).map_err(|error| match error {
            Error::PathOutsideRoot(_) => {
                serde::de::Error::custom("path must stay within the repository root")
            }
            error => serde::de::Error::custom(error.to_string()),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, JsonSchema)]
#[serde(transparent)]
pub struct RepositoryPattern(String);

impl RepositoryPattern {
    pub fn parse(value: impl AsRef<str>) -> Result<Self> {
        let canonical = normalize_repository_value(
            value.as_ref(),
            "path pattern",
            false,
            MAX_REPOSITORY_PATH_BYTES,
        )?;
        if has_glob_meta(&canonical) {
            Glob::new(&canonical)?;
        }
        Ok(Self(canonical))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    #[must_use]
    pub fn is_glob(&self) -> bool {
        has_glob_meta(&self.0)
    }
}

impl<'de> Deserialize<'de> for RepositoryPattern {
    fn deserialize<Deserializer>(
        deserializer: Deserializer,
    ) -> std::result::Result<Self, Deserializer::Error>
    where
        Deserializer: serde::Deserializer<'de>,
    {
        Self::parse(String::deserialize(deserializer)?).map_err(|error| match error {
            Error::PathOutsideRoot(_) => {
                serde::de::Error::custom("path pattern must stay within the repository root")
            }
            error => serde::de::Error::custom(error.to_string()),
        })
    }
}

#[derive(Debug, Clone)]
pub struct RepositoryPatternSet {
    patterns: Vec<RepositoryPattern>,
    literals: Vec<String>,
    globs: GlobSet,
}

impl RepositoryPatternSet {
    pub fn new(patterns: &[String]) -> Result<Self> {
        if patterns.len() > MAX_REPOSITORY_PATTERNS {
            return Err(Error::LimitExceeded);
        }
        let patterns = patterns
            .iter()
            .map(RepositoryPattern::parse)
            .collect::<Result<Vec<_>>>()?;
        Self::compile(patterns)
    }

    pub fn compile(patterns: Vec<RepositoryPattern>) -> Result<Self> {
        if patterns.len() > MAX_REPOSITORY_PATTERNS {
            return Err(Error::LimitExceeded);
        }
        let mut literals = Vec::new();
        let mut globs = GlobSetBuilder::new();
        for pattern in &patterns {
            if pattern.is_glob() {
                globs.add(Glob::new(pattern.as_str())?);
            } else {
                literals.push(pattern.0.clone());
            }
        }
        Ok(Self {
            patterns,
            literals,
            globs: globs.build()?,
        })
    }

    pub fn empty() -> Self {
        Self {
            patterns: Vec::new(),
            literals: Vec::new(),
            globs: GlobSet::empty(),
        }
    }

    #[must_use]
    pub fn is_match(&self, path: &str) -> bool {
        let candidate = Candidate::new(path);
        self.literals.iter().any(|literal| {
            path.strip_prefix(literal)
                .is_some_and(|suffix| suffix.is_empty() || suffix.starts_with('/'))
        }) || self.globs.is_match_candidate(&candidate)
    }

    #[must_use]
    pub fn canonical_strings(&self) -> Vec<String> {
        self.patterns
            .iter()
            .map(|pattern| pattern.0.clone())
            .collect()
    }
}

fn has_glob_meta(value: &str) -> bool {
    value.contains(['*', '?', '[', ']', '{', '}'])
}

fn normalize_repository_value(
    requested: &str,
    field: &'static str,
    allow_root: bool,
    max_bytes: usize,
) -> Result<String> {
    if requested.len() > max_bytes {
        return Err(Error::InputTooLong { field, max_bytes });
    }
    if requested.trim().is_empty() || requested.contains('\0') {
        return Err(Error::InvalidInput {
            field,
            reason: "must be a non-empty repository-relative value",
        });
    }
    let bytes = requested.as_bytes();
    if requested.starts_with('/')
        || requested.starts_with('\\')
        || (bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':')
    {
        return Err(Error::PathOutsideRoot(PathBuf::from(requested)));
    }
    let normalized = requested.replace('\\', "/");
    let mut components = Vec::new();
    for component in normalized.split('/') {
        match component {
            "" | "." => {}
            ".." => return Err(Error::PathOutsideRoot(PathBuf::from(requested))),
            component => components.push(component),
        }
    }
    if components.is_empty() {
        if allow_root {
            return Ok(".".into());
        }
        return Err(Error::InvalidInput {
            field,
            reason: "must not resolve to the repository root",
        });
    }
    Ok(components.join("/"))
}
