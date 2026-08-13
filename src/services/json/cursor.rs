//! Cursor state and normalized query identity for paginated JSON key projections.

use serde_json::json;

use super::execution::{JsonCursorVersion, JsonExecutionOptions};
use crate::model::JsonOperation;
use crate::services::Services;
use crate::services::cursor::{CursorEnvelope, CursorKind, StreamId, StreamIdentityBuilder};
use crate::text::CONTENT_FINGERPRINT_HEX_LEN;
use crate::{Error, Result};

pub(super) const MAX_JSON_CURSOR_BYTES: usize = 256;
const JSON_CURSOR_GENERATION: u64 = 0;

pub(super) struct JsonCursor {
    envelope: CursorEnvelope,
}

impl JsonCursor {
    pub(super) fn offset_for(&self, source_hash: &str, stream_id: StreamId) -> Result<usize> {
        let payload =
            self.envelope
                .payload_for(CursorKind::JsonKeys, JSON_CURSOR_GENERATION, stream_id)?;
        if payload.len() != CONTENT_FINGERPRINT_HEX_LEN + size_of::<u64>() {
            return Err(Error::StaleCursor);
        }
        let (cursor_source_hash, offset) = payload.split_at(CONTENT_FINGERPRINT_HEX_LEN);
        if cursor_source_hash != source_hash.as_bytes() {
            return Err(Error::StaleCursor);
        }
        let offset = u64::from_le_bytes(offset.try_into().map_err(|_| Error::StaleCursor)?);
        let offset = usize::try_from(offset).map_err(|_| Error::StaleCursor)?;
        if offset == 0 {
            return Err(Error::StaleCursor);
        }
        Ok(offset)
    }
}

fn is_fingerprint(value: &str) -> bool {
    value.len() == CONTENT_FINGERPRINT_HEX_LEN && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

pub(super) fn decode_json_cursor(cursor: &str) -> Result<JsonCursor> {
    CursorEnvelope::parse(cursor, MAX_JSON_CURSOR_BYTES).map(|envelope| JsonCursor { envelope })
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

pub(super) fn json_stream_id(services: &Services, query_hash: &str) -> StreamId {
    let mut stream = StreamIdentityBuilder::for_service(services, CursorKind::JsonKeys);
    stream.field_str("query_hash", query_hash);
    stream.finish()
}

pub(super) fn make_json_cursor(
    stream_id: StreamId,
    source_hash: &str,
    offset: usize,
) -> Result<String> {
    if !is_fingerprint(source_hash) || offset == 0 {
        return Err(Error::OperationFailure(
            "invalid JSON continuation state".into(),
        ));
    }
    let mut payload = Vec::with_capacity(CONTENT_FINGERPRINT_HEX_LEN + size_of::<u64>());
    payload.extend_from_slice(source_hash.as_bytes());
    payload.extend_from_slice(
        &u64::try_from(offset)
            .map_err(|_| Error::OperationFailure("JSON cursor offset overflow".into()))?
            .to_le_bytes(),
    );
    CursorEnvelope::new(
        CursorKind::JsonKeys,
        JSON_CURSOR_GENERATION,
        stream_id,
        payload,
    )
    .map(CursorEnvelope::encode)
}
