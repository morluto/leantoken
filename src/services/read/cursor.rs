impl ReadCursor {
    pub(super) fn encode(&self) -> String {
        format!(
            "{}:read:{}:{}:{}:{}:{}:{}",
            self.generation,
            self.target_start_line,
            self.target_end_line,
            self.next_start_line,
            self.next_byte,
            self.full_hash,
            self.path_hash,
        )
    }
}

pub(super) fn decode_read_cursor(cursor: &str) -> Result<ReadCursor> {
    let fields = cursor.split(':').collect::<Vec<_>>();
    let [
        generation,
        kind,
        target_start,
        target_end,
        next_start,
        next_byte,
        full_hash,
        path_hash,
    ] = fields.as_slice()
    else {
        return Err(Error::StaleCursor);
    };
    if *kind != "read"
        || full_hash.len() != crate::text::CONTENT_FINGERPRINT_HEX_LEN
        || path_hash.len() != 16
        || !full_hash.bytes().all(is_lower_hex)
        || !path_hash.bytes().all(is_lower_hex)
    {
        return Err(Error::StaleCursor);
    }
    let cursor = ReadCursor {
        generation: generation.parse().map_err(|_| Error::StaleCursor)?,
        target_start_line: target_start.parse().map_err(|_| Error::StaleCursor)?,
        target_end_line: target_end.parse().map_err(|_| Error::StaleCursor)?,
        next_start_line: next_start.parse().map_err(|_| Error::StaleCursor)?,
        next_byte: next_byte.parse().map_err(|_| Error::StaleCursor)?,
        full_hash: (*full_hash).into(),
        path_hash: (*path_hash).into(),
    };
    if cursor.target_start_line == 0
        || cursor.target_end_line < cursor.target_start_line
        || cursor.next_start_line < cursor.target_start_line
        || cursor.next_start_line > cursor.target_end_line
        || cursor.next_byte == 0
    {
        return Err(Error::StaleCursor);
    }
    Ok(cursor)
}

pub(super) fn parse_read_cursor(cursor: &str, generation: u64, path: &str) -> Result<ReadCursor> {
    let cursor = decode_read_cursor(cursor)?;
    if cursor.generation != generation || cursor.path_hash != read_path_hash(path) {
        return Err(Error::StaleCursor);
    }
    Ok(cursor)
}

pub(super) fn read_path_hash(path: &str) -> String {
    blake3::hash(path.as_bytes()).to_hex()[..16].to_string()
}

pub(super) fn returned_end_line(start_line: usize, content: &str) -> usize {
    let newline_count = content.bytes().filter(|byte| *byte == b'\n').count();
    start_line
        .saturating_add(newline_count)
        .saturating_sub(usize::from(content.ends_with('\n') && newline_count > 0))
}
use super::*;
