//! Input validation, cancellation probes, and shared path filters.

use globset::Glob;
use tokio_util::sync::CancellationToken;

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
pub(super) fn path_allowed(path: &str, includes: &[String], excludes: &[String]) -> Result<bool> {
    let mut included = includes.is_empty();
    for pattern in includes {
        included |= path_matches(path, pattern)?;
    }
    let mut excluded = false;
    for pattern in excludes {
        excluded |= path_matches(path, pattern)?;
    }
    Ok(included && !excluded)
}

pub(super) fn path_matches(path: &str, pattern: &str) -> Result<bool> {
    if pattern.contains(['*', '?', '[', ']']) {
        Ok(Glob::new(pattern)?.compile_matcher().is_match(path))
    } else {
        let pattern = pattern.trim_matches('/');
        Ok(path == pattern || path.starts_with(&format!("{pattern}/")))
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

pub(super) fn validate_optional_input(
    value: Option<&str>,
    name: &str,
    max_bytes: usize,
) -> Result<()> {
    if let Some(value) = value {
        validate_input(value, name, max_bytes)?;
    }
    Ok(())
}

pub(super) fn validate_input(value: &str, name: &str, max_bytes: usize) -> Result<()> {
    if value.len() > max_bytes {
        return Err(Error::InvalidRequest(format!(
            "{name} exceeds {max_bytes} bytes"
        )));
    }
    Ok(())
}

pub(super) fn parse_cursor(cursor: Option<&str>, generation: u64) -> Result<usize> {
    let Some(cursor) = cursor else { return Ok(0) };
    let Some((cursor_generation, offset)) = cursor.split_once(':') else {
        return Err(Error::StaleCursor);
    };
    if cursor_generation.parse::<u64>().ok() != Some(generation) {
        return Err(Error::StaleCursor);
    }
    offset.parse().map_err(|_| Error::StaleCursor)
}

pub(super) fn make_cursor(generation: u64, offset: usize) -> String {
    format!("{generation}:{offset}")
}
