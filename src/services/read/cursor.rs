use crate::services::validation::is_lower_hex;

impl ReadCursor {
    pub(super) fn encode(&self) -> String {
        let full_hash = self.full_hash.as_deref().unwrap_or("-");
        let prefix_hash = self.prefix_hash.as_deref().unwrap_or("-");
        let modified_ns = self
            .modified_ns
            .map(|value| value.to_string())
            .unwrap_or_else(|| "-".to_string());
        format!(
            "{}:read:v4:{}:{}:{}:{}:{}:{}:{}:{}:{}:{}",
            self.generation,
            self.target_start_line,
            self.target_end_line
                .map_or_else(|| "-".to_string(), |line| line.to_string()),
            self.next_start_line,
            self.next_byte,
            if self.full { "f" } else { "b" },
            full_hash,
            prefix_hash,
            self.path_hash,
            self.file_size,
            modified_ns,
        )
    }
}

pub(super) fn decode_read_cursor(cursor: &str) -> Result<ReadCursor> {
    let fields = cursor.split(':').collect::<Vec<_>>();
    let [
        generation,
        kind,
        version,
        target_start,
        target_end,
        next_start,
        next_byte,
        policy,
        full_hash,
        prefix_hash,
        path_hash,
        file_size,
        modified_ns,
    ] = fields.as_slice()
    else {
        return Err(Error::StaleCursor);
    };
    let full = match *policy {
        "b" => false,
        "f" => true,
        _ => return Err(Error::StaleCursor),
    };
    let target_end_line = (*target_end != "-")
        .then(|| target_end.parse::<usize>().map_err(|_| Error::StaleCursor))
        .transpose()?;
    if *kind != "read"
        || *version != "v4"
        || (*full_hash != "-"
            && (full_hash.len() != crate::text::CONTENT_FINGERPRINT_HEX_LEN
                || !full_hash.bytes().all(is_lower_hex)))
        || (full && *full_hash == "-")
        || (*prefix_hash != "-"
            && (prefix_hash.len() != crate::text::CONTENT_FINGERPRINT_HEX_LEN
                || !prefix_hash.bytes().all(is_lower_hex)))
        || (!full && *prefix_hash == "-")
        || path_hash.len() != 16
        || !path_hash.bytes().all(is_lower_hex)
        || (*modified_ns != "-" && modified_ns.parse::<u128>().is_err())
    {
        return Err(Error::StaleCursor);
    }
    let cursor = ReadCursor {
        generation: generation.parse().map_err(|_| Error::StaleCursor)?,
        target_start_line: target_start.parse().map_err(|_| Error::StaleCursor)?,
        target_end_line,
        next_start_line: next_start.parse().map_err(|_| Error::StaleCursor)?,
        next_byte: next_byte.parse().map_err(|_| Error::StaleCursor)?,
        full_hash: (*full_hash != "-").then(|| (*full_hash).to_string()),
        prefix_hash: (*prefix_hash != "-").then(|| (*prefix_hash).to_string()),
        full,
        file_size: file_size.parse().map_err(|_| Error::StaleCursor)?,
        modified_ns: (*modified_ns != "-").then(|| modified_ns.parse::<u128>().unwrap_or(0)),
        path_hash: (*path_hash).into(),
    };
    if cursor.target_start_line == 0
        || cursor.next_start_line < cursor.target_start_line
        || cursor.target_end_line.is_some_and(|end_line| {
            end_line < cursor.target_start_line || cursor.next_start_line > end_line
        })
        || cursor.next_byte == 0
    {
        return Err(Error::StaleCursor);
    }
    Ok(cursor)
}

pub(super) fn validate_read_cursor(cursor: &ReadCursor, generation: u64, path: &str) -> Result<()> {
    if cursor.generation != generation || cursor.path_hash != read_path_hash(path) {
        return Err(Error::StaleCursor);
    }
    Ok(())
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
