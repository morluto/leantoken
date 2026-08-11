//! Unified authenticated continuation cursor for all paginated retrieval operations.
//!
//! Every operation that produces paginated results returns an opaque cursor
//! string to the caller. On the next page, the caller hands it back and
//! LeanToken must validate that the cursor was produced by the same binary,
//! for the same repository, and for the same normalized request—before
//! trusting the embedded offset.
//!
//! ## Design
//!
//! The cursor is structured as:
//!
//! ```text
//! <kind>:<generation>:<binding_hash>:<offset>:<mac>
//! ```
//!
//! - `kind` — a short operation label (e.g. `"search"`, `"read"`, `"outline"`).
//! - `generation` — the SQLite generation the cursor was created under.
//! - `binding_hash` — a 16-hex-char blake3 digest of all request fields
//!   that define the result set. This is operation-specific.
//! - `offset` — the operation-specific position to resume from.
//! - `mac` — a blake3 keyed hash over the cursor payload, keyed by a
//!   process-wide key, making the offset tamper-proof.
//!
//! The MAC prevents a caller from editing the offset to skip entries. The
//! binding hash prevents replaying a cursor against a different request or
//! repository. The generation prevents replaying across index versions.

use std::sync::OnceLock;

use crate::{Error, Result};

/// Maximum encoded cursor length to prevent memory amplification from
/// oversized untrusted input. The old search cursor had a 64-byte limit;
/// the new format is longer but still bounded.
const MAX_CURSOR_BYTES: usize = 512;

/// Domain separator used in the MAC computation. Includes the cursor kind
/// so a cursor from one operation cannot be accepted by another.
const CURSOR_MAC_DOMAIN: &[u8] = b"leantoken-cursor-mac-v1\0";

/// Process-wide key for the blake3 keyed hash. Generated once per process
/// from OS-provided randomness so cursors from one process cannot be replayed
/// in another and the key cannot be guessed from public metadata.
static CURSOR_KEY: OnceLock<[u8; 32]> = OnceLock::new();

fn cursor_key() -> &'static [u8; 32] {
    CURSOR_KEY.get_or_init(|| {
        // Derive the key from OS randomness rather than public process metadata
        // (PID, timestamp) so it cannot be guessed by an untrusted client.
        let mut key = [0u8; 32];
        let mut rng = blake3::Hasher::new();
        rng.update(b"leantoken-cursor-key-v2\0");
        // Mix in OS-provided randomness if available.
        if getrandom::fill(&mut key).is_ok() {
            return key;
        }
        // Fallback: mix process state (less secure but still process-unique)
        rng.update(&std::process::id().to_le_bytes());
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        rng.update(&(nanos as u64).to_le_bytes());
        let digest = rng.finalize();
        key.copy_from_slice(digest.as_bytes());
        key
    })
}

/// Compute the MAC for a cursor payload.
fn compute_mac(kind: &str, generation: u64, binding_hash: &str, offset: usize) -> String {
    let mut hasher = blake3::Hasher::new_keyed(cursor_key());
    hasher.update(CURSOR_MAC_DOMAIN);
    hasher.update(&(kind.len() as u64).to_le_bytes());
    hasher.update(kind.as_bytes());
    hasher.update(&generation.to_le_bytes());
    hasher.update(&(binding_hash.len() as u64).to_le_bytes());
    hasher.update(binding_hash.as_bytes());
    hasher.update(&(offset as u64).to_le_bytes());
    hasher
        .finalize()
        .to_hex()
        .as_str()
        .split_at(16)
        .0
        .to_owned()
}

/// Encode an authenticated continuation cursor.
///
/// The cursor is opaque to the caller. It binds the cursor to the
/// operation `kind`, the snapshot `generation`, a `binding_hash` of all
/// request fields that define the result set, and the `offset` to resume
/// from. The MAC prevents tampering with the offset.
#[must_use]
pub(crate) fn encode_cursor(
    kind: &str,
    generation: u64,
    binding_hash: &str,
    offset: usize,
) -> String {
    let mac = compute_mac(kind, generation, binding_hash, offset);
    format!("{kind}:{generation}:{binding_hash}:{offset}:{mac}")
}

/// Decode and validate an authenticated continuation cursor.
///
/// Returns the decoded `offset` if the cursor is valid (correct kind,
/// matching generation, valid MAC), or an error if the cursor is stale,
/// tampered, or from a different operation.
pub(crate) fn decode_cursor(
    cursor: &str,
    expected_kind: &str,
    expected_generation: u64,
    expected_binding_hash: &str,
) -> Result<usize> {
    // Reject oversized cursors before splitting to prevent memory amplification
    // from untrusted input containing many colons.
    if cursor.len() > MAX_CURSOR_BYTES {
        return Err(Error::StaleCursor);
    }
    let fields = cursor.split(':').collect::<Vec<_>>();
    if fields.len() != 5 {
        return Err(Error::StaleCursor);
    }
    let kind = fields[0];
    let generation = fields[1];
    let binding_hash = fields[2];
    let offset = fields[3];
    let mac = fields[4];

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
    let expected_mac = compute_mac(
        expected_kind,
        expected_generation,
        expected_binding_hash,
        offset,
    );
    if mac != expected_mac.as_str() {
        return Err(Error::StaleCursor);
    }
    Ok(offset)
}

/// Parse a cursor or return offset 0 if no cursor is provided.
///
/// This is the common entry point for paginated operations: if the
/// caller provides no cursor, we start from offset 0; if they provide
/// one, we validate it and return the offset.
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
    fn tampered_offset_is_rejected() {
        let hash = binding_hash(&["query", "text"]);
        let cursor = encode_cursor("search", 42, &hash, 100);
        let tampered = cursor.replacen(":100:", ":200:", 1);
        assert!(decode_cursor(&tampered, "search", 42, &hash).is_err());
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
