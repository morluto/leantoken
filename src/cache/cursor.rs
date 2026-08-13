use super::*;
use crate::services::cursor::{CursorEnvelope, CursorKind, StreamId, StreamIdentityBuilder};

#[derive(Clone, Copy)]
pub(super) enum CacheListMode<'a> {
    Summary,
    Page {
        limit: usize,
        cursor: Option<&'a str>,
    },
}

pub(super) fn parse_list_mode(request: &CacheListRequest) -> Result<CacheListMode<'_>> {
    if request.limit == 0 {
        return Err(Error::InvalidInput {
            field: "cache list limit",
            reason: "must be greater than zero",
        });
    }
    if request.limit > MAX_CACHE_LIST_LIMIT {
        return Err(Error::RequestLimitExceeded {
            field: "cache list limit",
            requested: request.limit,
            limit: MAX_CACHE_LIST_LIMIT,
        });
    }
    if request.summary && request.cursor.is_some() {
        return Err(Error::InvalidInput {
            field: "cache list cursor",
            reason: "cannot be combined with summary mode",
        });
    }
    if request.summary {
        Ok(CacheListMode::Summary)
    } else {
        Ok(CacheListMode::Page {
            limit: request.limit,
            cursor: request.cursor.as_deref(),
        })
    }
}

pub(super) fn normalize_repository_root_filter(path: &Path) -> PathBuf {
    let absolute = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    absolute.canonicalize().unwrap_or(absolute)
}

pub(super) fn cache_list_filter_hash(
    request: &CacheListRequest,
    repository_root: Option<&Path>,
) -> String {
    cache_list_filter_hash_in_namespace(b"cache-list-v3\0", request, repository_root)
}

fn previous_cache_list_filter_hash(
    request: &CacheListRequest,
    repository_root: Option<&Path>,
) -> String {
    cache_list_filter_hash_in_namespace(b"cache-list-v2\0", request, repository_root)
}

fn unversioned_cache_list_filter_hash(
    request: &CacheListRequest,
    repository_root: Option<&Path>,
) -> String {
    cache_list_filter_hash_in_namespace(b"", request, repository_root)
}

fn cache_list_filter_hash_in_namespace(
    namespace: &[u8],
    request: &CacheListRequest,
    repository_root: Option<&Path>,
) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(namespace);
    if request.states.is_empty() {
        hasher.update(b"all-states");
    } else {
        for state in CacheState::ALL {
            if request.states.contains(&state) {
                hasher.update(state.label().as_bytes());
                hasher.update(b"\0");
            }
        }
    }
    hasher.update(b"\xff");
    if let Some(root) = repository_root {
        hasher.update(root.as_os_str().as_encoded_bytes());
    }
    hasher.update(b"\xffcompatibility\0");
    if request.compatibilities.is_empty() {
        hasher.update(b"all");
    } else {
        for compatibility in CacheCompatibility::ALL {
            if request.compatibilities.contains(&compatibility) {
                hasher.update(compatibility.label().as_bytes());
                hasher.update(b"\0");
            }
        }
    }
    hasher.update(b"\xffcontent-versions\0");
    let mut versions = request.index_content_versions.clone();
    versions.sort_unstable();
    versions.dedup();
    if versions.is_empty() {
        hasher.update(b"all");
    } else {
        for version in versions {
            hasher.update(&version.to_le_bytes());
        }
    }
    hasher.update(b"\xffincompatible\0");
    hasher.update(&[u8::from(request.incompatible_with_current)]);
    hasher.finalize().to_hex()[..CACHE_LIST_CURSOR_HASH_CHARS].to_owned()
}

pub(super) fn cache_list_stream_id(cache_root: &Path, filter_hash: &str) -> StreamId {
    let cache_root = normalize_repository_root_filter(cache_root);
    let mut stream = StreamIdentityBuilder::new(CursorKind::CacheList);
    stream.field_bytes("cache_root", cache_root.as_os_str().as_encoded_bytes());
    stream.field_str("filter_hash", filter_hash);
    stream.finish()
}

pub(super) fn encode_cache_list_cursor(stream_id: StreamId, after_id: &str) -> Result<String> {
    if !is_cache_id(after_id) {
        return Err(Error::OperationFailure(
            "invalid cache list continuation state".into(),
        ));
    }
    CursorEnvelope::new(
        CursorKind::CacheList,
        0,
        stream_id,
        after_id.as_bytes().to_vec(),
    )
    .map(CursorEnvelope::encode)
}

pub(super) fn decode_cache_list_cursor(
    cursor: &str,
    stream_id: StreamId,
    request: &CacheListRequest,
    repository_root: Option<&Path>,
) -> Result<String> {
    if cursor.len() > MAX_CACHE_LIST_CURSOR_BYTES {
        return Err(Error::InputTooLong {
            field: "cache list cursor",
            max_bytes: MAX_CACHE_LIST_CURSOR_BYTES,
        });
    }
    if cursor.starts_with(LEGACY_CACHE_LIST_CURSOR_PREFIX) {
        let previous_hash = previous_cache_list_filter_hash(request, repository_root);
        return decode_legacy_cache_list_cursor(cursor, &previous_hash).or_else(|error| {
            let unversioned_hash = unversioned_cache_list_filter_hash(request, repository_root);
            decode_legacy_cache_list_cursor(cursor, &unversioned_hash).map_err(|_| error)
        });
    }
    let envelope = CursorEnvelope::parse(cursor, MAX_CACHE_LIST_CURSOR_BYTES)
        .map_err(|_| invalid_cache_cursor())?;
    let payload = envelope
        .payload_for(CursorKind::CacheList, 0, stream_id)
        .map_err(|_| Error::InvalidInput {
            field: "cache list cursor",
            reason: "does not match the active cache filters",
        })?;
    let after_id = std::str::from_utf8(payload)
        .ok()
        .filter(|id| is_cache_id(id))
        .ok_or_else(invalid_cache_cursor)?;
    Ok(after_id.to_owned())
}

fn decode_legacy_cache_list_cursor(cursor: &str, expected_filter_hash: &str) -> Result<String> {
    let mut parts = cursor.splitn(3, ':');
    let prefix = parts.next();
    let filter_hash = parts.next();
    let after_id = parts.next();
    if prefix != Some(LEGACY_CACHE_LIST_CURSOR_PREFIX)
        || filter_hash.is_none_or(|hash| {
            hash.len() != CACHE_LIST_CURSOR_HASH_CHARS
                || !hash.bytes().all(|byte| byte.is_ascii_hexdigit())
        })
    {
        return Err(invalid_cache_cursor());
    }
    if filter_hash != Some(expected_filter_hash) {
        return Err(Error::InvalidInput {
            field: "cache list cursor",
            reason: "does not match the active cache filters",
        });
    }
    after_id
        .filter(|id| is_cache_id(id))
        .map(str::to_owned)
        .ok_or_else(invalid_cache_cursor)
}

#[cfg(test)]
pub(super) fn make_previous_cache_list_cursor(
    request: &CacheListRequest,
    repository_root: Option<&Path>,
    after_id: &str,
) -> String {
    format!(
        "{LEGACY_CACHE_LIST_CURSOR_PREFIX}:{}:{after_id}",
        previous_cache_list_filter_hash(request, repository_root)
    )
}

fn invalid_cache_cursor() -> Error {
    Error::InvalidInput {
        field: "cache list cursor",
        reason: "must be an opaque cursor returned by cache list",
    }
}
