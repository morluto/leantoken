//! Cursor encoding, decoding, and query-hash derivation for paginated JSON
//! keys projections.

use serde_json::json;

use super::execution::{JsonCursorVersion, JsonExecutionOptions};
use crate::model::JsonOperation;
use crate::text::CONTENT_FINGERPRINT_HEX_LEN;
use crate::{Error, Result};

pub(super) const MAX_JSON_CURSOR_BYTES: usize = 256;

pub(super) struct JsonCursor {
    source_hash: String,
    query_hash: String,
    offset: usize,
}

impl JsonCursor {
    pub(super) fn offset(&self) -> usize {
        self.offset
    }

    pub(super) fn matches(&self, source_hash: &str, query_hash: &str) -> bool {
        self.source_hash == source_hash && self.query_hash == query_hash
    }
}

fn is_fingerprint(value: &str) -> bool {
    value.len() == CONTENT_FINGERPRINT_HEX_LEN && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

pub(super) fn decode_json_cursor(
    cursor: &str,
    expected_version: JsonCursorVersion,
) -> Result<JsonCursor> {
    if cursor.len() > MAX_JSON_CURSOR_BYTES {
        return Err(Error::StaleCursor);
    }
    let mut fields = cursor.split(':');
    let version = fields.next();
    let source_hash = fields.next();
    let query_hash = fields.next();
    let offset = fields.next();
    if version != Some(expected_version.prefix()) || fields.next().is_some() {
        return Err(Error::StaleCursor);
    }
    let (Some(source_hash), Some(query_hash), Some(offset)) = (source_hash, query_hash, offset)
    else {
        return Err(Error::StaleCursor);
    };
    if !is_fingerprint(source_hash) || !is_fingerprint(query_hash) {
        return Err(Error::StaleCursor);
    }
    let offset = offset.parse::<usize>().map_err(|_| Error::StaleCursor)?;
    if offset == 0 {
        return Err(Error::StaleCursor);
    }
    Ok(JsonCursor {
        source_hash: source_hash.to_owned(),
        query_hash: query_hash.to_owned(),
        offset,
    })
}

pub(super) fn json_query_hash(
    operation: &JsonOperation,
    execution: JsonExecutionOptions,
) -> Result<String> {
    let serialized = if execution.cursor_version() == JsonCursorVersion::V1 {
        serde_json::to_string(operation)
    } else {
        serde_json::to_string(&json!({
            "operation": operation,
            "depth": execution.depth(),
            "order": "depth_then_pointer",
        }))
    }
    .map_err(|error| Error::SerializationFailure(error.to_string()))?;
    Ok(crate::text::hash(&serialized))
}

pub(super) fn make_json_cursor(
    version: JsonCursorVersion,
    source_hash: &str,
    query_hash: &str,
    offset: usize,
) -> String {
    format!("{}:{source_hash}:{query_hash}:{offset}", version.prefix())
}
