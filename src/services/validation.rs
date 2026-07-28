//! Input validation, cancellation probes, and shared path filters.

use globset::{Candidate, Glob, GlobMatcher, GlobSet, GlobSetBuilder};
use tokio_util::sync::CancellationToken;

use crate::{Error, Result};

pub(super) const MAX_QUERY_BYTES: usize = 64 * 1024;
pub(super) const MAX_PATTERN_BYTES: usize = 4 * 1024;
pub(super) const MAX_PATH_BYTES: usize = 4 * 1024;
pub(super) const MAX_INPUT_ITEMS: usize = 256;
const MAX_CURSOR_BYTES: usize = 64;
pub(super) fn check_cancelled(cancellation: &CancellationToken) -> Result<()> {
    if cancellation.is_cancelled() {
        Err(Error::Cancelled)
    } else {
        Ok(())
    }
}
pub(crate) fn path_matches(path: &str, pattern: &str) -> Result<bool> {
    Ok(PathMatcher::new(std::slice::from_ref(&pattern.to_owned()))?.is_match(path))
}

pub(crate) struct PathMatcher {
    literals: Vec<String>,
    globs: GlobSet,
    fallback_globs: Vec<GlobMatcher>,
}

impl PathMatcher {
    pub(crate) fn new(patterns: &[String]) -> Result<Self> {
        let mut literals = Vec::new();
        let mut globs = GlobSetBuilder::new();
        for pattern in patterns {
            let pattern = pattern.replace('\\', "/");
            if pattern.contains(['*', '?', '[', ']', '{', '}']) {
                globs.add(Glob::new(&pattern)?);
            } else {
                literals.push(pattern.trim_matches('/').to_owned());
            }
        }
        Ok(Self {
            literals,
            globs: globs.build()?,
            fallback_globs: Vec::new(),
        })
    }

    pub(crate) fn new_lossy(patterns: &[String]) -> Self {
        let mut literals = Vec::new();
        let mut globs = GlobSetBuilder::new();
        let mut fallback_globs = Vec::new();
        for pattern in patterns {
            let pattern = pattern.replace('\\', "/");
            if pattern.contains(['*', '?', '[', ']', '{', '}']) {
                if let Ok(glob) = Glob::new(&pattern) {
                    fallback_globs.push(glob.compile_matcher());
                    globs.add(glob);
                }
            } else {
                literals.push(pattern.trim_matches('/').to_owned());
            }
        }
        let (globs, fallback_globs) = match globs.build() {
            Ok(globs) => (globs, Vec::new()),
            Err(_) => (GlobSet::empty(), fallback_globs),
        };
        Self {
            literals,
            globs,
            fallback_globs,
        }
    }

    pub(crate) fn is_match(&self, path: &str) -> bool {
        let candidate = Candidate::new(path);
        self.literals.iter().any(|literal| {
            path.strip_prefix(literal)
                .is_some_and(|suffix| suffix.is_empty() || suffix.starts_with('/'))
        }) || self.globs.is_match_candidate(&candidate)
            || self
                .fallback_globs
                .iter()
                .any(|glob| glob.is_match_candidate(&candidate))
    }
}

pub(super) struct PathFilter {
    includes: PathMatcher,
    excludes: PathMatcher,
    include_all: bool,
}

impl PathFilter {
    pub(super) fn new(includes: &[String], excludes: &[String]) -> Result<Self> {
        Ok(Self {
            includes: PathMatcher::new(includes)?,
            excludes: PathMatcher::new(excludes)?,
            include_all: includes.is_empty(),
        })
    }

    pub(super) fn allows(&self, path: &str) -> bool {
        (self.include_all || self.includes.is_match(path)) && !self.excludes.is_match(path)
    }
}

pub(super) fn validate_patterns(patterns: &[String]) -> Result<()> {
    if patterns.len() > MAX_INPUT_ITEMS {
        return Err(Error::LimitExceeded);
    }
    for pattern in patterns {
        validate_input(pattern, "path pattern", MAX_PATTERN_BYTES)?;
    }
    Ok(())
}

pub(super) fn validate_glob_patterns(patterns: &[String]) -> Result<()> {
    validate_patterns(patterns)?;
    for pattern in patterns {
        let pattern = pattern.replace('\\', "/");
        if pattern.contains(['*', '?', '[', ']', '{', '}']) {
            Glob::new(&pattern)?;
        }
    }
    Ok(())
}

pub(super) fn validate_optional_input(
    value: Option<&str>,
    name: &'static str,
    max_bytes: usize,
) -> Result<()> {
    if let Some(value) = value {
        validate_input(value, name, max_bytes)?;
    }
    Ok(())
}

pub(super) fn validate_input(value: &str, name: &'static str, max_bytes: usize) -> Result<()> {
    if value.len() > max_bytes {
        return Err(Error::InputTooLong {
            field: name,
            max_bytes,
        });
    }
    Ok(())
}

fn decode_cursor(cursor: Option<&str>) -> Result<Option<(u64, usize)>> {
    let Some(cursor) = cursor else {
        return Ok(None);
    };
    if cursor.len() > MAX_CURSOR_BYTES {
        return Err(Error::StaleCursor);
    }
    let Some((cursor_generation, offset)) = cursor.split_once(':') else {
        return Err(Error::StaleCursor);
    };
    let cursor_generation = cursor_generation
        .parse::<u64>()
        .map_err(|_| Error::StaleCursor)?;
    let offset = offset.parse::<usize>().map_err(|_| Error::StaleCursor)?;
    Ok(Some((cursor_generation, offset)))
}

pub(super) fn validate_cursor(cursor: Option<&str>) -> Result<()> {
    decode_cursor(cursor).map(drop)
}

pub(super) fn parse_cursor(cursor: Option<&str>, generation: u64) -> Result<usize> {
    let Some((cursor_generation, offset)) = decode_cursor(cursor)? else {
        return Ok(0);
    };
    if cursor_generation != generation {
        return Err(Error::StaleCursor);
    }
    Ok(offset)
}

pub(super) fn make_cursor(generation: u64, offset: usize) -> String {
    format!("{generation}:{offset}")
}

pub(super) fn is_lower_hex(byte: u8) -> bool {
    byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compiled_path_filter_preserves_literal_glob_and_exclusion_semantics() {
        let filter = PathFilter::new(
            &["src".into(), "tests/**/*.rs".into()],
            &["src/generated".into(), "**/snapshots/**".into()],
        )
        .expect("valid filters");

        assert!(filter.allows("src/lib.rs"));
        assert!(filter.allows("tests/unit/parser.rs"));
        assert!(!filter.allows("src/generated/schema.rs"));
        assert!(!filter.allows("tests/snapshots/parser.rs"));
        assert!(!filter.allows("docs/usage.md"));
    }

    #[test]
    fn compiled_path_matcher_normalizes_separators_and_directory_prefixes() {
        let matcher =
            PathMatcher::new(&["src\\services".into(), "**/*.md".into()]).expect("valid matcher");

        assert!(matcher.is_match("src/services/context.rs"));
        assert!(matcher.is_match("docs/usage.md"));
        assert!(!matcher.is_match("src/service.rs"));
    }

    #[test]
    fn lossy_path_matcher_keeps_valid_patterns_beside_an_invalid_glob() {
        let matcher = PathMatcher::new_lossy(&["[".into(), "src".into()]);

        assert!(matcher.is_match("src/lib.rs"));
        assert!(!matcher.is_match("tests/lib.rs"));
    }
}
