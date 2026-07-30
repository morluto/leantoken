use super::*;

pub(super) fn validate_list_request(request: &CacheListRequest) -> Result<()> {
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
    Ok(())
}

pub(super) fn normalize_repository_root_filter(path: &Path) -> PathBuf {
    let absolute = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    absolute.canonicalize().unwrap_or(absolute)
}

pub(super) fn cache_list_filter_hash(
    request: &CacheListRequest,
    repository_root: Option<&Path>,
) -> String {
    let mut hasher = blake3::Hasher::new();
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
    hasher.finalize().to_hex()[..CACHE_LIST_CURSOR_HASH_CHARS].to_owned()
}

pub(super) fn cache_list_v2_filter_hash(
    request: &CacheListV2Request,
    repository_root: Option<&Path>,
) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(cache_list_filter_hash(&request.request, repository_root).as_bytes());
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

pub(super) fn encode_cache_list_cursor(filter_hash: &str, after_id: &str) -> String {
    encode_cache_list_cursor_with_prefix(CACHE_LIST_CURSOR_PREFIX, filter_hash, after_id)
}

pub(super) fn encode_cache_list_cursor_with_prefix(
    prefix: &str,
    filter_hash: &str,
    after_id: &str,
) -> String {
    format!("{prefix}:{filter_hash}:{after_id}")
}

pub(super) fn decode_cache_list_cursor(cursor: &str, expected_filter_hash: &str) -> Result<String> {
    decode_cache_list_cursor_with_prefix(cursor, CACHE_LIST_CURSOR_PREFIX, expected_filter_hash)
}

pub(super) fn decode_cache_list_cursor_with_prefix(
    cursor: &str,
    expected_prefix: &str,
    expected_filter_hash: &str,
) -> Result<String> {
    if cursor.len() > MAX_CACHE_LIST_CURSOR_BYTES {
        return Err(Error::InputTooLong {
            field: "cache list cursor",
            max_bytes: MAX_CACHE_LIST_CURSOR_BYTES,
        });
    }
    let mut parts = cursor.splitn(3, ':');
    let prefix = parts.next();
    let filter_hash = parts.next();
    let after_id = parts.next();
    if prefix != Some(expected_prefix)
        || filter_hash.is_none_or(|hash| {
            hash.len() != CACHE_LIST_CURSOR_HASH_CHARS
                || !hash.bytes().all(|byte| byte.is_ascii_hexdigit())
        })
        || after_id.is_none_or(|id| !is_cache_id(id))
    {
        return Err(Error::InvalidInput {
            field: "cache list cursor",
            reason: "must be an opaque cursor returned by cache list",
        });
    }
    if filter_hash != Some(expected_filter_hash) {
        return Err(Error::InvalidInput {
            field: "cache list cursor",
            reason: "does not match the active cache filters",
        });
    }
    Ok(after_id.expect("validated cache cursor id").to_owned())
}
