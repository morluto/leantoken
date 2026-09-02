use super::*;
use crate::services::cursor::{CursorEnvelope, CursorKind, StreamId, StreamIdentityBuilder};
use crate::services::validation::is_lower_hex;

const MAX_READ_CURSOR_BYTES: usize = 256;

pub(super) fn read_stream_id(services: &Services, path: &str, policy: ReadPolicy) -> StreamId {
    let mut stream = StreamIdentityBuilder::for_service(services, CursorKind::Read);
    stream.field_str("path", path);
    stream.field_str(
        "policy",
        match policy {
            ReadPolicy::Bounded => "bounded",
            ReadPolicy::Full => "full",
        },
    );
    stream.finish()
}

pub(super) fn encode_read_cursor(
    generation: u64,
    stream_id: StreamId,
    state: ReadCursorState,
) -> Result<String> {
    validate_read_state(&state)?;
    let mut payload = Vec::with_capacity(160);
    payload.push(match state.policy {
        ReadPolicy::Bounded => 1,
        ReadPolicy::Full => 2,
    });
    push_usize(&mut payload, state.target_start_line)?;
    push_optional_usize(&mut payload, state.target_end_line)?;
    push_usize(&mut payload, state.next_start_line)?;
    push_usize(&mut payload, state.next_byte)?;
    push_fingerprint(&mut payload, state.full_hash.as_deref())?;
    push_fingerprint(&mut payload, state.prefix_hash.as_deref())?;
    push_usize(&mut payload, state.file_size)?;
    push_optional_u128(&mut payload, state.modified_ns);
    CursorEnvelope::new(CursorKind::Read, generation, stream_id, payload)
        .map(CursorEnvelope::encode)
}

pub(super) fn decode_read_cursor(cursor: &str) -> Result<ReadCursor> {
    let envelope = CursorEnvelope::parse(cursor, MAX_READ_CURSOR_BYTES)?;
    let (kind, generation, stream_id) = envelope.identity();
    let payload = envelope.payload_for(kind, generation, stream_id)?;
    let mut payload = PayloadReader::new(payload);
    let policy = match payload.byte()? {
        1 => ReadPolicy::Bounded,
        2 => ReadPolicy::Full,
        _ => return Err(Error::StaleCursor),
    };
    let state = ReadCursorState {
        target_start_line: payload.usize()?,
        target_end_line: payload.optional_usize()?,
        next_start_line: payload.usize()?,
        next_byte: payload.usize()?,
        full_hash: payload.fingerprint()?,
        prefix_hash: payload.fingerprint()?,
        policy,
        file_size: payload.usize()?,
        modified_ns: payload.optional_u128()?,
    };
    payload.finish()?;
    validate_read_state(&state).map_err(|_| Error::StaleCursor)?;
    Ok(ReadCursor { envelope, state })
}

pub(super) fn validate_read_cursor(
    cursor: &ReadCursor,
    generation: u64,
    stream_id: StreamId,
) -> Result<()> {
    cursor
        .envelope
        .payload_for(CursorKind::Read, generation, stream_id)
        .map(|_| ())
}

fn validate_read_state(state: &ReadCursorState) -> Result<()> {
    let fingerprints_match_policy = match state.policy {
        ReadPolicy::Bounded => state.full_hash.is_none() && state.prefix_hash.is_some(),
        ReadPolicy::Full => state.full_hash.is_some() && state.prefix_hash.is_none(),
    };
    if state.target_start_line == 0
        || state.next_start_line < state.target_start_line
        || state.target_end_line.is_some_and(|end_line| {
            end_line < state.target_start_line || state.next_start_line > end_line
        })
        || state.next_byte == 0
        || !fingerprints_match_policy
    {
        return Err(Error::StaleCursor);
    }
    Ok(())
}

fn push_usize(payload: &mut Vec<u8>, value: usize) -> Result<()> {
    payload.extend_from_slice(
        &u64::try_from(value)
            .map_err(|_| Error::OperationFailure("read cursor value overflow".into()))?
            .to_le_bytes(),
    );
    Ok(())
}

fn push_optional_usize(payload: &mut Vec<u8>, value: Option<usize>) -> Result<()> {
    match value {
        Some(value) => {
            payload.push(1);
            push_usize(payload, value)
        }
        None => {
            payload.push(0);
            Ok(())
        }
    }
}

fn push_optional_u128(payload: &mut Vec<u8>, value: Option<u128>) {
    match value {
        Some(value) => {
            payload.push(1);
            payload.extend_from_slice(&value.to_le_bytes());
        }
        None => payload.push(0),
    }
}

fn push_fingerprint(payload: &mut Vec<u8>, value: Option<&str>) -> Result<()> {
    match value {
        Some(value)
            if value.len() == crate::text::CONTENT_FINGERPRINT_HEX_LEN
                && value.bytes().all(is_lower_hex) =>
        {
            payload.push(1);
            payload.extend_from_slice(value.as_bytes());
            Ok(())
        }
        Some(_) => Err(Error::OperationFailure(
            "invalid read cursor fingerprint".into(),
        )),
        None => {
            payload.push(0);
            Ok(())
        }
    }
}

struct PayloadReader<'a> {
    remaining: &'a [u8],
}

impl<'a> PayloadReader<'a> {
    const fn new(remaining: &'a [u8]) -> Self {
        Self { remaining }
    }

    fn byte(&mut self) -> Result<u8> {
        self.take::<1>().map(|bytes| bytes[0])
    }

    fn usize(&mut self) -> Result<usize> {
        let value = u64::from_le_bytes(self.take::<8>()?);
        usize::try_from(value).map_err(|_| Error::StaleCursor)
    }

    fn optional_usize(&mut self) -> Result<Option<usize>> {
        match self.byte()? {
            0 => Ok(None),
            1 => self.usize().map(Some),
            _ => Err(Error::StaleCursor),
        }
    }

    fn optional_u128(&mut self) -> Result<Option<u128>> {
        match self.byte()? {
            0 => Ok(None),
            1 => self.take::<16>().map(u128::from_le_bytes).map(Some),
            _ => Err(Error::StaleCursor),
        }
    }

    fn fingerprint(&mut self) -> Result<Option<String>> {
        match self.byte()? {
            0 => Ok(None),
            1 => {
                let bytes = self.take_slice(crate::text::CONTENT_FINGERPRINT_HEX_LEN)?;
                let value = std::str::from_utf8(bytes).map_err(|_| Error::StaleCursor)?;
                if !value.bytes().all(is_lower_hex) {
                    return Err(Error::StaleCursor);
                }
                Ok(Some(value.to_owned()))
            }
            _ => Err(Error::StaleCursor),
        }
    }

    fn finish(self) -> Result<()> {
        if self.remaining.is_empty() {
            Ok(())
        } else {
            Err(Error::StaleCursor)
        }
    }

    fn take<const N: usize>(&mut self) -> Result<[u8; N]> {
        self.take_slice(N)?
            .try_into()
            .map_err(|_| Error::StaleCursor)
    }

    fn take_slice(&mut self, length: usize) -> Result<&'a [u8]> {
        if self.remaining.len() < length {
            return Err(Error::StaleCursor);
        }
        let (value, remaining) = self.remaining.split_at(length);
        self.remaining = remaining;
        Ok(value)
    }
}

pub(super) fn returned_end_line(start_line: usize, content: &str) -> usize {
    let newline_count = content.bytes().filter(|byte| *byte == b'\n').count();
    start_line
        .saturating_add(newline_count)
        .saturating_sub(usize::from(content.ends_with('\n') && newline_count > 0))
}
