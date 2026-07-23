//! Input validation, cancellation probes, and shared path filters.

use globset::Glob;
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

pub(crate) fn path_matches(path: &str, pattern: &str) -> Result<bool> {
    let pattern = pattern.replace('\\', "/");
    if pattern.contains(['*', '?', '[', ']', '{', '}']) {
        Ok(Glob::new(&pattern)?.compile_matcher().is_match(path))
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
