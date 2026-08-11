//! Unified continuation cursor for all paginated retrieval operations.
//!
//! Every operation that produces paginated results returns an opaque cursor
//! string to the caller. On the next page, the caller hands it back and
//! LeanToken must validate that the cursor was produced for the same request
//! and snapshot generation before trusting the embedded offset.
//!
//! ## Design
//!
//! The cursor is structured as:
//!
//! ```text
//! <kind>:<generation>:<binding_hash>:<offset>
//! ```
//!
//! - `kind` — a short operation label (e.g. `"search"`, `"read"`, `"outline"`).
//! - `generation` — the SQLite generation the cursor was created under.
//! - `binding_hash` — a 16-hex-char blake3 digest of all request fields
//!   that define the result set. This is operation-specific.
//! - `offset` — the operation-specific position to resume from.
//!
//! The binding hash prevents replaying a cursor against a different request.
//! The generation prevents replaying across index versions. No MAC is needed
//! — this is a local tool, and the cursor's integrity comes from binding the
//! request fields, not from cryptographic authentication of the offset.

use crate::{Error, Result};

/// Maximum encoded cursor length to prevent memory amplification from
/// oversized untrusted input.
const MAX_CURSOR_BYTES: usize = 512;

/// Encode a continuation cursor.
#[must_use]
pub(crate) fn encode_cursor(
    kind: &str,
    generation: u64,
    binding_hash: &str,
    offset: usize,
) -> String {
    format!("{kind}:{generation}:{binding_hash}:{offset}")
}

/// Decode and validate a continuation cursor.
///
/// Returns the decoded `offset` if the cursor is valid (correct kind,
/// matching generation, matching binding hash), or an error if the cursor
/// is stale, from a different operation, or malformed.
pub(crate) fn decode_cursor(
    cursor: &str,
    expected_kind: &str,
    expected_generation: u64,
    expected_binding_hash: &str,
) -> Result<usize> {
    if cursor.len() > MAX_CURSOR_BYTES {
        return Err(Error::StaleCursor);
    }
    let fields = cursor.split(':').collect::<Vec<_>>();
    if fields.len() != 4 {
        return Err(Error::StaleCursor);
    }
    let kind = fields[0];
    let generation = fields[1];
    let binding_hash = fields[2];
    let offset = fields[3];

    if kind != expected_kind {
        return Err(Error::StaleCursor);
    }
    let cursor_generation = generation.parse::<u64>().map_err(|_| Error::StaleCursor)?;
    if cursor_generation != expected_generation {
        return Err(Error::StaleCursor);
    }
    if binding_hash.len() != 16
        || !binding_hash.bytes().all(|b| b.is_ascii_hexdigit())
        || binding_hash != expected_binding_hash
    {
        return Err(Error::StaleCursor);
    }
    let offset = offset.parse::<usize>().map_err(|_| Error::StaleCursor)?;
    Ok(offset)
}

/// Parse a cursor or return offset 0 if no cursor is provided.
pub(crate) fn parse_cursor(
    cursor: Option<&str>,
    expected_kind: &str,
    expected_generation: u64,
    expected_binding_hash: &str,
) -> Result<usize> {
    match cursor {
        None => Ok(0),
        Some(c) => decode_cursor(c, expected_kind, expected_generation, expected_binding_hash),
    }
}

/// Compute a binding hash from a set of string fields.
///
/// Each field is length-prefixed and concatenated into the hasher. The
/// resulting 16-hex-char digest uniquely identifies the request that
/// produced this cursor.
#[must_use]
pub(crate) fn binding_hash(fields: &[&str]) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"leantoken-cursor-binding-v1\0");
    for field in fields {
        hasher.update(&(field.len() as u64).to_le_bytes());
        hasher.update(field.as_bytes());
    }
    hasher
        .finalize()
        .to_hex()
        .as_str()
        .split_at(16)
        .0
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_preserves_offset() {
        let hash = binding_hash(&["query", "text"]);
        let cursor = encode_cursor("search", 42, &hash, 100);
        let offset = decode_cursor(&cursor, "search", 42, &hash).expect("decode should succeed");
        assert_eq!(offset, 100);
    }

    #[test]
    fn wrong_kind_is_rejected() {
        let hash = binding_hash(&["query", "text"]);
        let cursor = encode_cursor("search", 42, &hash, 0);
        assert!(decode_cursor(&cursor, "read", 42, &hash).is_err());
    }

    #[test]
    fn wrong_generation_is_rejected() {
        let hash = binding_hash(&["query", "text"]);
        let cursor = encode_cursor("search", 42, &hash, 0);
        assert!(decode_cursor(&cursor, "search", 43, &hash).is_err());
    }

    #[test]
    fn wrong_binding_hash_is_rejected() {
        let hash = binding_hash(&["query", "text"]);
        let cursor = encode_cursor("search", 42, &hash, 0);
        let wrong_hash = binding_hash(&["different", "query"]);
        assert!(decode_cursor(&cursor, "search", 42, &wrong_hash).is_err());
    }

    #[test]
    fn parse_cursor_returns_zero_for_none() {
        let hash = binding_hash(&["query"]);
        assert_eq!(parse_cursor(None, "search", 42, &hash).unwrap(), 0);
    }

    #[test]
    fn oversized_cursor_is_rejected() {
        let hash = binding_hash(&["query"]);
        let cursor = "x".repeat(600);
        assert!(decode_cursor(&cursor, "search", 42, &hash).is_err());
    }
}
