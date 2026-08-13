//! Versioned continuation values shared by paginated service operations.
//!
//! A cursor carries the immutable stream identity established by an operation,
//! the relevant snapshot generation, and a bounded operation-specific payload.
//! The binary representation is encoded with URL-safe base64 and includes a
//! checksum so truncated or accidentally edited cursors fail closed. The
//! checksum is an integrity check, not an authorization boundary.

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};

use super::Services;
use crate::{Error, Result};

const CURSOR_VERSION: u8 = 1;
const STREAM_ID_BYTES: usize = 16;
const CHECKSUM_BYTES: usize = 8;
const CURSOR_HEADER_BYTES: usize = 1 + 1 + 8 + STREAM_ID_BYTES;
const MIN_CURSOR_BYTES: usize = CURSOR_HEADER_BYTES + CHECKSUM_BYTES;
const MAX_CURSOR_PAYLOAD_BYTES: usize = 8 * 1024;
const MAX_ENCODED_CURSOR_BYTES: usize = 11_000;
const OFFSET_PAYLOAD_BYTES: usize = 8;
const ENCODED_CURSOR_BYTES: usize = 56;
const CURSOR_CHECKSUM_DOMAIN: &[u8] = b"leantoken-continuation-checksum-v1\0";
const STREAM_ID_DOMAIN: &[u8] = b"leantoken-stream-identity-v1\0";

const _: () = assert!(CURSOR_HEADER_BYTES + OFFSET_PAYLOAD_BYTES + CHECKSUM_BYTES == 42);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub(crate) enum CursorKind {
    Search = 1,
    Files = 2,
    Outline = 3,
    Read = 4,
    JsonKeys = 5,
    HistoryDiffSymbols = 6,
    CacheList = 7,
}

impl CursorKind {
    fn parse(value: u8) -> Result<Self> {
        match value {
            value if value == Self::Search as u8 => Ok(Self::Search),
            value if value == Self::Files as u8 => Ok(Self::Files),
            value if value == Self::Outline as u8 => Ok(Self::Outline),
            value if value == Self::Read as u8 => Ok(Self::Read),
            value if value == Self::JsonKeys as u8 => Ok(Self::JsonKeys),
            value if value == Self::HistoryDiffSymbols as u8 => Ok(Self::HistoryDiffSymbols),
            value if value == Self::CacheList as u8 => Ok(Self::CacheList),
            _ => Err(Error::StaleCursor),
        }
    }
}

/// Opaque identity of one deterministic ordered result stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct StreamId([u8; STREAM_ID_BYTES]);

/// Length-delimited builder used by an operation to define its stream once.
pub(crate) struct StreamIdentityBuilder {
    hasher: blake3::Hasher,
}

impl StreamIdentityBuilder {
    pub(crate) fn new(kind: CursorKind) -> Self {
        let mut hasher = blake3::Hasher::new();
        hasher.update(STREAM_ID_DOMAIN);
        hasher.update(&[kind as u8]);
        Self { hasher }
    }

    /// Start an operation stream with the process-independent repository and
    /// output-semantics fields shared by every retrieval service.
    pub(super) fn for_service(services: &Services, kind: CursorKind) -> Self {
        let mut stream = Self::new(kind);
        stream.field_str("repository_id", &services.repository_id());
        stream.field_optional_str(
            "index_scope_digest",
            services.config.index_scope().full_digest(),
        );
        stream.field_str("implementation_version", env!("CARGO_PKG_VERSION"));
        stream.field_str("tokenizer", services.config.tokenizer.name());
        stream
    }

    pub(crate) fn field_str(&mut self, name: &str, value: &str) {
        self.field_bytes(name, value.as_bytes());
    }

    pub(super) fn field_optional_str(&mut self, name: &str, value: Option<&str>) {
        self.field_name(name);
        match value {
            Some(value) => {
                self.hasher.update(&[1]);
                self.length_prefixed(value.as_bytes());
            }
            None => {
                self.hasher.update(&[0]);
            }
        }
    }

    pub(super) fn field_bool(&mut self, name: &str, value: bool) {
        self.field_name(name);
        self.hasher.update(&[u8::from(value)]);
    }

    pub(super) fn field_usize(&mut self, name: &str, value: usize) {
        self.field_name(name);
        self.hasher.update(&(value as u64).to_le_bytes());
    }

    pub(super) fn field_optional_usize(&mut self, name: &str, value: Option<usize>) {
        self.field_name(name);
        match value {
            Some(value) => {
                self.hasher.update(&[1]);
                self.hasher.update(&(value as u64).to_le_bytes());
            }
            None => {
                self.hasher.update(&[0]);
            }
        }
    }

    pub(super) fn field_strings(&mut self, name: &str, values: &[String]) {
        self.field_name(name);
        self.hasher.update(&(values.len() as u64).to_le_bytes());
        for value in values {
            self.length_prefixed(value.as_bytes());
        }
    }

    pub(crate) fn finish(self) -> StreamId {
        let digest = self.hasher.finalize();
        let mut identity = [0; STREAM_ID_BYTES];
        identity.copy_from_slice(&digest.as_bytes()[..STREAM_ID_BYTES]);
        StreamId(identity)
    }

    pub(crate) fn field_bytes(&mut self, name: &str, value: &[u8]) {
        self.field_name(name);
        self.length_prefixed(value);
    }

    fn field_name(&mut self, name: &str) {
        self.length_prefixed(name.as_bytes());
    }

    fn length_prefixed(&mut self, value: &[u8]) {
        self.hasher.update(&(value.len() as u64).to_le_bytes());
        self.hasher.update(value);
    }
}

/// Shared versioning, stream binding, snapshot binding, and integrity layer.
///
/// Operations own the interpretation of `payload`; this envelope owns every
/// property that must be checked uniformly at the protocol boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CursorEnvelope {
    kind: CursorKind,
    generation: u64,
    stream_id: StreamId,
    payload: Vec<u8>,
}

impl CursorEnvelope {
    pub(crate) fn new(
        kind: CursorKind,
        generation: u64,
        stream_id: StreamId,
        payload: Vec<u8>,
    ) -> Result<Self> {
        if payload.len() > MAX_CURSOR_PAYLOAD_BYTES {
            return Err(Error::OperationFailure("cursor payload overflow".into()));
        }
        Ok(Self {
            kind,
            generation,
            stream_id,
            payload,
        })
    }

    pub(crate) fn parse(encoded: &str, max_encoded_bytes: usize) -> Result<Self> {
        if encoded.is_empty() || encoded.len() > max_encoded_bytes.min(MAX_ENCODED_CURSOR_BYTES) {
            return Err(Error::StaleCursor);
        }
        let bytes = URL_SAFE_NO_PAD
            .decode(encoded)
            .map_err(|_| Error::StaleCursor)?;
        if bytes.len() < MIN_CURSOR_BYTES
            || bytes.len() > CURSOR_HEADER_BYTES + MAX_CURSOR_PAYLOAD_BYTES + CHECKSUM_BYTES
            || URL_SAFE_NO_PAD.encode(&bytes) != encoded
        {
            return Err(Error::StaleCursor);
        }
        let checksum_start = bytes.len() - CHECKSUM_BYTES;
        let expected_checksum = cursor_checksum(&bytes[..checksum_start]);
        if bytes[checksum_start..] != expected_checksum {
            return Err(Error::StaleCursor);
        }
        if bytes[0] != CURSOR_VERSION {
            return Err(Error::StaleCursor);
        }
        let kind = CursorKind::parse(bytes[1])?;
        let generation =
            u64::from_le_bytes(bytes[2..10].try_into().map_err(|_| Error::StaleCursor)?);
        let mut stream_id = [0; STREAM_ID_BYTES];
        stream_id.copy_from_slice(&bytes[10..10 + STREAM_ID_BYTES]);
        Ok(Self {
            kind,
            generation,
            stream_id: StreamId(stream_id),
            payload: bytes[CURSOR_HEADER_BYTES..checksum_start].to_vec(),
        })
    }

    pub(crate) fn encode(self) -> String {
        let mut bytes =
            Vec::with_capacity(CURSOR_HEADER_BYTES + self.payload.len() + CHECKSUM_BYTES);
        bytes.push(CURSOR_VERSION);
        bytes.push(self.kind as u8);
        bytes.extend_from_slice(&self.generation.to_le_bytes());
        bytes.extend_from_slice(&self.stream_id.0);
        bytes.extend_from_slice(&self.payload);
        let checksum = cursor_checksum(&bytes);
        bytes.extend_from_slice(&checksum);
        URL_SAFE_NO_PAD.encode(bytes)
    }

    pub(crate) fn payload_for(
        &self,
        expected_kind: CursorKind,
        expected_generation: u64,
        expected_stream_id: StreamId,
    ) -> Result<&[u8]> {
        if self.kind != expected_kind
            || self.generation != expected_generation
            || self.stream_id != expected_stream_id
        {
            return Err(Error::StaleCursor);
        }
        Ok(&self.payload)
    }

    /// Return the bounded operation payload after envelope syntax, version,
    /// kind, and checksum validation. Resume paths must still call
    /// [`Self::payload_for`] against their expected snapshot and stream.
    pub(super) fn payload(&self) -> &[u8] {
        &self.payload
    }
}

/// Parsed continuation supplied at the protocol boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct ContinuationCursor {
    kind: CursorKind,
    generation: u64,
    stream_id: StreamId,
    position: u64,
}

impl ContinuationCursor {
    pub(super) fn at(
        kind: CursorKind,
        generation: u64,
        stream_id: StreamId,
        position: usize,
    ) -> Result<Self> {
        Ok(Self {
            kind,
            generation,
            stream_id,
            position: u64::try_from(position)
                .map_err(|_| Error::OperationFailure("cursor position overflow".into()))?,
        })
    }

    pub(super) fn parse_optional(encoded: Option<&str>) -> Result<Option<Self>> {
        encoded.map(Self::parse).transpose()
    }

    pub(super) fn parse(encoded: &str) -> Result<Self> {
        if encoded.len() != ENCODED_CURSOR_BYTES {
            return Err(Error::StaleCursor);
        }
        let envelope = CursorEnvelope::parse(encoded, ENCODED_CURSOR_BYTES)?;
        let position = u64::from_le_bytes(
            envelope
                .payload
                .as_slice()
                .try_into()
                .map_err(|_| Error::StaleCursor)?,
        );
        Ok(Self {
            kind: envelope.kind,
            generation: envelope.generation,
            stream_id: envelope.stream_id,
            position,
        })
    }

    pub(super) fn encode(self) -> String {
        CursorEnvelope::new(
            self.kind,
            self.generation,
            self.stream_id,
            self.position.to_le_bytes().to_vec(),
        )
        .expect("fixed-size cursor payload is bounded")
        .encode()
    }

    pub(super) fn position_for(
        self,
        expected_kind: CursorKind,
        expected_generation: u64,
        expected_stream_id: StreamId,
    ) -> Result<usize> {
        if self.kind != expected_kind
            || self.generation != expected_generation
            || self.stream_id != expected_stream_id
        {
            return Err(Error::StaleCursor);
        }
        usize::try_from(self.position).map_err(|_| Error::StaleCursor)
    }
}

fn cursor_checksum(payload: &[u8]) -> [u8; CHECKSUM_BYTES] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(CURSOR_CHECKSUM_DOMAIN);
    hasher.update(payload);
    let digest = hasher.finalize();
    let mut checksum = [0; CHECKSUM_BYTES];
    checksum.copy_from_slice(&digest.as_bytes()[..CHECKSUM_BYTES]);
    checksum
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stream(value: &str) -> StreamId {
        let mut builder = StreamIdentityBuilder::new(CursorKind::Search);
        builder.field_str("query", value);
        builder.finish()
    }

    #[test]
    fn cursor_round_trip_preserves_its_typed_identity() {
        let stream_id = stream("needle");
        for position in [0, 1, 42, usize::MAX] {
            let encoded = ContinuationCursor::at(CursorKind::Search, 7, stream_id, position)
                .expect("bounded cursor")
                .encode();
            assert_eq!(encoded.len(), ENCODED_CURSOR_BYTES);
            let decoded = ContinuationCursor::parse(&encoded).expect("parse cursor");
            assert_eq!(
                decoded
                    .position_for(CursorKind::Search, 7, stream_id)
                    .expect("resume cursor"),
                position
            );
        }
    }

    #[test]
    fn cursor_rejects_generation_stream_and_integrity_mismatches() {
        let stream_id = stream("needle");
        let encoded = ContinuationCursor::at(CursorKind::Search, 7, stream_id, 42)
            .expect("bounded cursor")
            .encode();
        let decoded = ContinuationCursor::parse(&encoded).expect("parse cursor");
        assert!(
            decoded
                .position_for(CursorKind::Search, 8, stream_id)
                .is_err()
        );
        assert!(
            decoded
                .position_for(CursorKind::Search, 7, stream("other"))
                .is_err()
        );

        let mut corrupted = encoded.into_bytes();
        corrupted[12] = if corrupted[12] == b'A' { b'B' } else { b'A' };
        let corrupted = String::from_utf8(corrupted).expect("base64 text");
        assert!(ContinuationCursor::parse(&corrupted).is_err());
    }

    #[test]
    fn stream_fields_are_length_delimited_and_named() {
        let mut combined = StreamIdentityBuilder::new(CursorKind::Search);
        combined.field_str("left", "ab");
        combined.field_str("right", "c");

        let mut split = StreamIdentityBuilder::new(CursorKind::Search);
        split.field_str("left", "a");
        split.field_str("right", "bc");

        let mut renamed = StreamIdentityBuilder::new(CursorKind::Search);
        renamed.field_str("first", "ab");
        renamed.field_str("right", "c");

        assert_ne!(combined.finish(), split.finish());
        assert_ne!(stream("ab"), renamed.finish());
    }

    #[test]
    fn malformed_and_legacy_cursors_fail_at_the_boundary() {
        for cursor in ["", "7:1", "not-base64", &"A".repeat(ENCODED_CURSOR_BYTES)] {
            assert!(ContinuationCursor::parse(cursor).is_err(), "{cursor}");
        }
    }
}
