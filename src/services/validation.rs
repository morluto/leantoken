//! Input validation, cancellation probes, and shared path filters.

use tokio_util::sync::CancellationToken;

pub(crate) use crate::repository::RepositoryPatternSet as PathMatcher;
use crate::{Error, Result};

pub(super) const MAX_QUERY_BYTES: usize = 64 * 1024;
pub(super) const MAX_PATTERN_BYTES: usize = 4 * 1024;
pub(super) const MAX_PATH_BYTES: usize = 4 * 1024;
pub(super) const MAX_INPUT_ITEMS: usize = 256;
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
    PathMatcher::new(patterns)?;
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

pub(super) fn is_lower_hex(byte: u8) -> bool {
    byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)
}
